//! ACE-Step 1.5's 2B diffusion transformer (`acestep-v15-base`) and the
//! condition encoder in front of it, as `modeling_acestep_v15_base.py`
//! defines them. Only what `lego` runs is here: the audio tokenizer and
//! detokenizer (cover hints) are not loaded — lego's `is_covers` is false,
//! so the reference computes them and throws them away.

use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::VarBuilder;

use crate::nn::{window_mask, Attention, AttnShape, EncoderLayer, Linear, Mlp, RmsNorm, Rope};

pub const HIDDEN: usize = 2048;
const INTER: usize = 6144;
const LAYERS: usize = 24;
const LYRIC_LAYERS: usize = 8;
const TIMBRE_LAYERS: usize = 4;
pub const TEXT_DIM: usize = 1024;
pub const ACOUSTIC: usize = 64;
const IN_CHANNELS: usize = 192;
const PATCH: usize = 2;
pub const SLIDING_WINDOW: usize = 128;
const SHAPE: AttnShape = AttnShape {
    hidden: HIDDEN,
    heads: 16,
    kv_heads: 8,
    head_dim: 128,
    eps: 1e-6,
};
/// The timbre reference length the silence latent gives (30 s at 25 Hz).
pub const TIMBRE_FRAMES: usize = 750;

/// Layer `i` attends within the sliding window when even, fully when odd
/// (the config's `layer_types`).
fn sliding(i: usize) -> bool {
    i % 2 == 0
}

/// The masks one sequence length needs: (sliding, full).
fn masks(len: usize, dtype: DType, dev: &Device) -> Result<(Option<Tensor>, Option<Tensor>)> {
    Ok((window_mask(len, Some(SLIDING_WINDOW), false, dtype, dev)?, None))
}

struct Stack {
    embed: Linear,
    layers: Vec<EncoderLayer>,
    norm: RmsNorm,
}

impl Stack {
    fn load(vb: &VarBuilder, in_dim: usize, layers: usize) -> Result<Stack> {
        Ok(Stack {
            embed: Linear::load(&vb.pp("embed_tokens"), in_dim, HIDDEN, true)?,
            layers: (0..layers)
                .map(|i| EncoderLayer::load(&vb.pp("layers").pp(i), SHAPE, INTER))
                .collect::<Result<Vec<_>>>()?,
            norm: RmsNorm::load(&vb.pp("norm"), HIDDEN, SHAPE.eps)?,
        })
    }

    fn forward(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        let mut h = self.embed.forward(x)?;
        let (slide, full) = masks(h.dim(1)?, h.dtype(), h.device())?;
        for (i, layer) in self.layers.iter().enumerate() {
            let m = if sliding(i) { slide.as_ref() } else { full.as_ref() };
            h = layer.forward(&h, rope, m)?;
        }
        self.norm.forward(&h)
    }
}

/// Text, lyrics and timbre packed into the DiT's cross-attention sequence.
pub struct ConditionEncoder {
    text_projector: Linear,
    lyric: Stack,
    timbre: Stack,
    rope: Rope,
}

impl ConditionEncoder {
    /// `vb` at the checkpoint root (keys `encoder.*`).
    pub fn load(vb: &VarBuilder) -> Result<ConditionEncoder> {
        let vb = vb.pp("encoder");
        Ok(ConditionEncoder {
            text_projector: Linear::load(&vb.pp("text_projector"), TEXT_DIM, HIDDEN, false)?,
            lyric: Stack::load(&vb.pp("lyric_encoder"), TEXT_DIM, LYRIC_LAYERS)?,
            timbre: Stack::load(&vb.pp("timbre_encoder"), ACOUSTIC, TIMBRE_LAYERS)?,
            rope: Rope::new(SHAPE.head_dim, 1_000_000.0, 4096, vb.dtype(), vb.device())?,
        })
    }

    /// `text` (1, Lt, 1024) — the prompt's hidden state; `lyric` (1, Ll,
    /// 1024) — the lyric token embeddings; `timbre` (1, 750, 64) — the
    /// reference latents (the silence latent when there is none). Every
    /// mask is all ones at batch 1, so packing is a concatenation in the
    /// reference's order: lyrics, the timbre token, text.
    pub fn forward(&self, text: &Tensor, lyric: &Tensor, timbre: &Tensor) -> Result<Tensor> {
        let text = self.text_projector.forward(text)?;
        let lyric = self.lyric.forward(lyric, &self.rope)?;
        let timbre = self.timbre.forward(timbre, &self.rope)?.narrow(1, 0, 1)?;
        Tensor::cat(&[lyric, timbre, text], 1)
    }
}

struct TimestepEmbedding {
    linear_1: Linear,
    linear_2: Linear,
    time_proj: Linear,
}

impl TimestepEmbedding {
    fn load(vb: &VarBuilder) -> Result<TimestepEmbedding> {
        Ok(TimestepEmbedding {
            linear_1: Linear::load(&vb.pp("linear_1"), 256, HIDDEN, true)?,
            linear_2: Linear::load(&vb.pp("linear_2"), HIDDEN, HIDDEN, true)?,
            time_proj: Linear::load(&vb.pp("time_proj"), HIDDEN, HIDDEN * 6, true)?,
        })
    }

    /// `t` per batch item → (temb (B, 2048), proj (B, 6, 2048)).
    fn forward(&self, t: &[f32], dtype: DType, dev: &Device) -> Result<(Tensor, Tensor)> {
        let half = 128;
        let mut rows = Vec::with_capacity(t.len() * 256);
        for &tv in t {
            let tv = tv * 1000.0;
            let mut cos = Vec::with_capacity(half);
            let mut sin = Vec::with_capacity(half);
            for i in 0..half {
                let f = (-(10000f32).ln() * i as f32 / half as f32).exp();
                let a = tv * f;
                cos.push(a.cos());
                sin.push(a.sin());
            }
            rows.extend(cos);
            rows.extend(sin);
        }
        let freq = Tensor::from_vec(rows, (t.len(), 256), dev)?.to_dtype(dtype)?;
        let temb = self.linear_2.forward(&candle_nn::ops::silu(&self.linear_1.forward(&freq)?)?)?;
        let proj = self.time_proj.forward(&candle_nn::ops::silu(&temb)?)?.reshape((t.len(), 6, HIDDEN))?;
        Ok((temb, proj))
    }
}

struct DitLayer {
    self_attn_norm: RmsNorm,
    self_attn: Attention,
    cross_attn_norm: RmsNorm,
    cross_attn: Attention,
    mlp_norm: RmsNorm,
    mlp: Mlp,
    scale_shift: Tensor,
}

/// Per-layer cross-attention keys and values for one conditioning (they
/// do not change over the steps).
pub struct CrossKv(Vec<(Tensor, Tensor)>);

pub struct Dit {
    layers: Vec<DitLayer>,
    proj_in: Linear,
    proj_out: Linear,
    proj_out_bias: Tensor,
    time_embed: TimestepEmbedding,
    time_embed_r: TimestepEmbedding,
    condition_embedder: Linear,
    norm_out: RmsNorm,
    scale_shift: Tensor,
    null_condition: Tensor,
    rope: Rope,
    device: Device,
    dtype: DType,
}

impl Dit {
    /// `vb` at the checkpoint root (keys `decoder.*`, `null_condition_emb`).
    pub fn load(vb: &VarBuilder) -> Result<Dit> {
        let null_condition = vb.get((1, 1, HIDDEN), "null_condition_emb")?;
        let vb = vb.pp("decoder");
        let layers = (0..LAYERS)
            .map(|i| {
                let l = vb.pp("layers").pp(i);
                Ok(DitLayer {
                    self_attn_norm: RmsNorm::load(&l.pp("self_attn_norm"), HIDDEN, SHAPE.eps)?,
                    self_attn: Attention::load(&l.pp("self_attn"), SHAPE)?,
                    cross_attn_norm: RmsNorm::load(&l.pp("cross_attn_norm"), HIDDEN, SHAPE.eps)?,
                    cross_attn: Attention::load(&l.pp("cross_attn"), SHAPE)?,
                    mlp_norm: RmsNorm::load(&l.pp("mlp_norm"), HIDDEN, SHAPE.eps)?,
                    mlp: Mlp::load(&l.pp("mlp"), HIDDEN, INTER)?,
                    scale_shift: l.get((1, 6, HIDDEN), "scale_shift_table")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // proj_in: Conv1d(192 → 2048, k = stride = 2) is a linear map of each
        // pair of frames: weight (out, in, k) → (out, k·in), frames (k, in).
        let w_in = vb.get((HIDDEN, IN_CHANNELS, PATCH), "proj_in.1.weight")?;
        let w_in = w_in.permute((0, 2, 1))?.reshape((HIDDEN, PATCH * IN_CHANNELS))?;
        let proj_in = Linear::new(w_in, Some(vb.get(HIDDEN, "proj_in.1.bias")?))?;
        // proj_out: ConvTranspose1d(2048 → 64, k = stride = 2): each token
        // makes two frames; weight (in, out, k) → (k·out, in).
        let w_out = vb.get((HIDDEN, ACOUSTIC, PATCH), "proj_out.1.weight")?;
        let w_out = w_out.permute((2, 1, 0))?.reshape((PATCH * ACOUSTIC, HIDDEN))?;
        let proj_out = Linear::new(w_out, None)?;
        Ok(Dit {
            layers,
            proj_in,
            proj_out,
            proj_out_bias: vb.get(ACOUSTIC, "proj_out.1.bias")?,
            time_embed: TimestepEmbedding::load(&vb.pp("time_embed"))?,
            time_embed_r: TimestepEmbedding::load(&vb.pp("time_embed_r"))?,
            condition_embedder: Linear::load(&vb.pp("condition_embedder"), HIDDEN, HIDDEN, true)?,
            norm_out: RmsNorm::load(&vb.pp("norm_out"), HIDDEN, SHAPE.eps)?,
            scale_shift: vb.get((1, 2, HIDDEN), "scale_shift_table")?,
            null_condition,
            rope: Rope::new(SHAPE.head_dim, 1_000_000.0, 32_768, vb.dtype(), vb.device())?,
            device: vb.device().clone(),
            dtype: vb.dtype(),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn dtype(&self) -> DType {
        self.dtype
    }

    /// The unconditional branch's encoder states: the null embedding over
    /// the conditioning's length.
    pub fn null_condition(&self, cond: &Tensor) -> Result<Tensor> {
        self.null_condition.broadcast_as(cond.shape())?.contiguous()
    }

    /// The cross-attention K/V of encoder states `enc` (B, S, 2048).
    pub fn cross_kv(&self, enc: &Tensor) -> Result<CrossKv> {
        let enc = self.condition_embedder.forward(enc)?;
        Ok(CrossKv(
            self.layers.iter().map(|l| l.cross_attn.kv(&enc)).collect::<Result<Vec<_>>>()?,
        ))
    }

    /// One velocity prediction. `x` (B, T, 64) the noisy latents,
    /// `context` (B, T, 128) the source latents and chunk mask, `t` the
    /// timestep per batch item (`timestep_r` = `t`, so the r-embedding sees
    /// zero), `kv` from [`Dit::cross_kv`].
    pub fn forward(&self, x: &Tensor, context: &Tensor, t: &[f32], kv: &CrossKv) -> Result<Tensor> {
        let (b, frames, _) = x.dims3()?;
        let (temb_t, proj_t) = self.time_embed.forward(t, self.dtype, &self.device)?;
        let zeros = vec![0f32; t.len()];
        let (temb_r, proj_r) = self.time_embed_r.forward(&zeros, self.dtype, &self.device)?;
        let temb = (temb_t + temb_r)?;
        let tproj = (proj_t + proj_r)?;

        let mut h = Tensor::cat(&[context, x], D::Minus1)?;
        let pad = frames % PATCH;
        if pad != 0 {
            let z = Tensor::zeros((b, PATCH - pad, IN_CHANNELS), h.dtype(), h.device())?;
            h = Tensor::cat(&[h, z], 1)?;
        }
        let tokens = h.dim(1)? / PATCH;
        let mut h = self.proj_in.forward(&h.reshape((b, tokens, PATCH * IN_CHANNELS))?)?;
        let (slide, full) = masks(tokens, self.dtype, &self.device)?;

        for (i, layer) in self.layers.iter().enumerate() {
            let mods = layer.scale_shift.broadcast_add(&tproj)?;
            let m = |k: usize| mods.narrow(1, k, 1);
            let (shift, scale, gate) = (m(0)?, m(1)?, m(2)?);
            let (c_shift, c_scale, c_gate) = (m(3)?, m(4)?, m(5)?);
            let n = layer.self_attn_norm.forward(&h)?;
            let n = n.broadcast_mul(&(scale + 1.0)?)?.broadcast_add(&shift)?;
            let mask = if sliding(i) { slide.as_ref() } else { full.as_ref() };
            let a = layer.self_attn.forward_self(&n, Some(&self.rope), mask)?;
            h = (h + a.broadcast_mul(&gate)?)?;
            let n = layer.cross_attn_norm.forward(&h)?;
            h = (&h + layer.cross_attn.forward_cross(&n, &kv.0[i])?)?;
            let n = layer.mlp_norm.forward(&h)?;
            let n = n.broadcast_mul(&(c_scale + 1.0)?)?.broadcast_add(&c_shift)?;
            h = (&h + layer.mlp.forward(&n)?.broadcast_mul(&c_gate)?)?;
        }

        let mods = self.scale_shift.broadcast_add(&temb.unsqueeze(1)?)?;
        let (shift, scale) = (mods.narrow(1, 0, 1)?, mods.narrow(1, 1, 1)?);
        let h = self.norm_out.forward(&h)?.broadcast_mul(&(scale + 1.0)?)?.broadcast_add(&shift)?;
        let y = self.proj_out.forward(&h)?.reshape((b, tokens * PATCH, ACOUSTIC))?;
        y.broadcast_add(&self.proj_out_bias)?.narrow(1, 0, frames)
    }
}
