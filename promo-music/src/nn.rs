//! The transformer pieces ACE-Step's DiT, its condition encoder and the
//! Qwen3 text encoder share: Qwen3's RMSNorm, rotary embedding, GQA
//! attention with per-head q/k norms, and the SwiGLU MLP. Written here
//! rather than taken from candle-transformers so one implementation serves
//! the causal text encoder, the bidirectional sliding-window encoders and
//! the cross-attention of the DiT, under one dtype policy.

use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::VarBuilder;

/// A dense layer, `y = x Wᵀ + b`, weights kept in the compute dtype.
#[derive(Debug, Clone)]
pub struct Linear {
    weight_t: Tensor,
    bias: Option<Tensor>,
}

impl Linear {
    pub fn load(vb: &VarBuilder, in_dim: usize, out_dim: usize, bias: bool) -> Result<Linear> {
        let weight = vb.get((out_dim, in_dim), "weight")?;
        let bias = if bias {
            Some(vb.get(out_dim, "bias")?)
        } else {
            None
        };
        Linear::new(weight, bias)
    }

    /// `weight` is (out, in), as PyTorch stores it.
    pub fn new(weight: Tensor, bias: Option<Tensor>) -> Result<Linear> {
        Ok(Linear {
            weight_t: weight.t()?.contiguous()?,
            bias,
        })
    }
}

impl Module for Linear {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let dims = x.dims().to_vec();
        let in_dim = dims[dims.len() - 1];
        let rows: usize = dims[..dims.len() - 1].iter().product();
        let y = x.reshape((rows, in_dim))?.matmul(&self.weight_t)?;
        let y = match &self.bias {
            Some(b) => y.broadcast_add(b)?,
            None => y,
        };
        let mut out = dims;
        let last = out.len() - 1;
        out[last] = self.weight_t.dim(1)?;
        y.reshape(out)
    }
}

/// Qwen3's RMSNorm: in f32, then scaled by the weight.
#[derive(Debug, Clone)]
pub struct RmsNorm {
    weight: Tensor,
    eps: f32,
}

impl RmsNorm {
    pub fn load(vb: &VarBuilder, dim: usize, eps: f64) -> Result<RmsNorm> {
        Ok(RmsNorm {
            weight: vb.get(dim, "weight")?,
            eps: eps as f32,
        })
    }
}

impl Module for RmsNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        candle_nn::ops::rms_norm(&x.contiguous()?, &self.weight, self.eps)
    }
}

/// Rotary position embedding tables (the HF "rotate half" layout).
#[derive(Debug, Clone)]
pub struct Rope {
    cos: Tensor,
    sin: Tensor,
}

impl Rope {
    pub fn new(
        head_dim: usize,
        theta: f64,
        max_len: usize,
        dtype: DType,
        dev: &Device,
    ) -> Result<Rope> {
        // inv_freq as PyTorch computes it: 1 / theta^(arange(0, d, 2) / d) in f32.
        let inv: Vec<f32> = (0..head_dim)
            .step_by(2)
            .map(|i| 1.0f32 / (theta as f32).powf(i as f32 / head_dim as f32))
            .collect();
        let half = inv.len();
        let mut cos = Vec::with_capacity(max_len * half);
        let mut sin = Vec::with_capacity(max_len * half);
        for p in 0..max_len {
            for f in &inv {
                let a = p as f32 * f;
                cos.push(a.cos());
                sin.push(a.sin());
            }
        }
        Ok(Rope {
            cos: Tensor::from_vec(cos, (max_len, half), dev)?.to_dtype(dtype)?,
            sin: Tensor::from_vec(sin, (max_len, half), dev)?.to_dtype(dtype)?,
        })
    }

    /// `x`: (B, H, L, D).
    pub fn apply(&self, x: &Tensor) -> Result<Tensor> {
        let l = x.dim(2)?;
        let cos = self.cos.narrow(0, 0, l)?;
        let sin = self.sin.narrow(0, 0, l)?;
        candle_nn::rotary_emb::rope(&x.contiguous()?, &cos, &sin)
    }
}

/// The attention mask shapes the encoders use, additive, (1, 1, L, L).
pub fn window_mask(
    len: usize,
    window: Option<usize>,
    causal: bool,
    dtype: DType,
    dev: &Device,
) -> Result<Option<Tensor>> {
    if window.is_none() && !causal {
        return Ok(None);
    }
    let mut m = vec![0f32; len * len];
    for i in 0..len {
        for j in 0..len {
            let d = i as i64 - j as i64;
            let ok = (!causal || d >= 0) && window.is_none_or(|w| d.unsigned_abs() as usize <= w);
            if !ok {
                m[i * len + j] = f32::NEG_INFINITY;
            }
        }
    }
    if let Some(w) = window {
        if !causal && w + 1 >= len {
            return Ok(None);
        }
    }
    Ok(Some(
        Tensor::from_vec(m, (1, 1, len, len), dev)?.to_dtype(dtype)?,
    ))
}

/// Multi-head attention with Qwen3's per-head q/k RMSNorm, grouped KV
/// heads, optional rotary embedding (self-attention) and optional mask.
#[derive(Debug, Clone)]
pub struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct AttnShape {
    pub hidden: usize,
    pub heads: usize,
    pub kv_heads: usize,
    pub head_dim: usize,
    pub eps: f64,
}

impl Attention {
    pub fn load(vb: &VarBuilder, s: AttnShape) -> Result<Attention> {
        Ok(Attention {
            q: Linear::load(&vb.pp("q_proj"), s.hidden, s.heads * s.head_dim, false)?,
            k: Linear::load(&vb.pp("k_proj"), s.hidden, s.kv_heads * s.head_dim, false)?,
            v: Linear::load(&vb.pp("v_proj"), s.hidden, s.kv_heads * s.head_dim, false)?,
            o: Linear::load(&vb.pp("o_proj"), s.heads * s.head_dim, s.hidden, false)?,
            q_norm: RmsNorm::load(&vb.pp("q_norm"), s.head_dim, s.eps)?,
            k_norm: RmsNorm::load(&vb.pp("k_norm"), s.head_dim, s.eps)?,
            heads: s.heads,
            kv_heads: s.kv_heads,
            head_dim: s.head_dim,
        })
    }

    /// The cross-attention keys and values of `enc` (B, S, hidden), which
    /// stay the same over every diffusion step.
    pub fn kv(&self, enc: &Tensor) -> Result<(Tensor, Tensor)> {
        let (b, s, _) = enc.dims3()?;
        let k = self
            .k
            .forward(enc)?
            .reshape((b, s, self.kv_heads, self.head_dim))?;
        let k = self.k_norm.forward(&k)?.transpose(1, 2)?;
        let v = self
            .v
            .forward(enc)?
            .reshape((b, s, self.kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        Ok((
            repeat_kv(&k, self.heads / self.kv_heads)?,
            repeat_kv(&v, self.heads / self.kv_heads)?,
        ))
    }

    fn query(&self, x: &Tensor) -> Result<Tensor> {
        let (b, l, _) = x.dims3()?;
        let q = self
            .q
            .forward(x)?
            .reshape((b, l, self.heads, self.head_dim))?;
        self.q_norm.forward(&q)?.transpose(1, 2)
    }

    /// Self-attention over `x` (B, L, hidden).
    pub fn forward_self(
        &self,
        x: &Tensor,
        rope: Option<&Rope>,
        mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let mut q = self.query(x)?;
        let (mut k, v) = {
            let (b, l, _) = x.dims3()?;
            let k = self
                .k
                .forward(x)?
                .reshape((b, l, self.kv_heads, self.head_dim))?;
            let k = self.k_norm.forward(&k)?.transpose(1, 2)?;
            let v = self
                .v
                .forward(x)?
                .reshape((b, l, self.kv_heads, self.head_dim))?
                .transpose(1, 2)?;
            (k, v)
        };
        if let Some(r) = rope {
            q = r.apply(&q)?;
            k = r.apply(&k)?;
        }
        let rep = self.heads / self.kv_heads;
        let out = attend(
            &q,
            &repeat_kv(&k, rep)?,
            &repeat_kv(&v, rep)?,
            mask,
            self.head_dim,
        )?;
        self.o.forward(&out)
    }

    /// Cross-attention from `x` to precomputed `(k, v)`.
    pub fn forward_cross(&self, x: &Tensor, kv: &(Tensor, Tensor)) -> Result<Tensor> {
        let q = self.query(x)?;
        let out = attend(&q, &kv.0, &kv.1, None, self.head_dim)?;
        self.o.forward(&out)
    }
}

fn repeat_kv(x: &Tensor, rep: usize) -> Result<Tensor> {
    if rep == 1 {
        return x.contiguous();
    }
    let (b, h, s, d) = x.dims4()?;
    x.unsqueeze(2)?
        .expand((b, h, rep, s, d))?
        .reshape((b, h * rep, s, d))
}

/// softmax(q kᵀ / √d + mask) v, in f32 for the softmax; returns (B, L, H·D).
fn attend(
    q: &Tensor,
    k: &Tensor,
    v: &Tensor,
    mask: Option<&Tensor>,
    head_dim: usize,
) -> Result<Tensor> {
    let (b, h, l, d) = q.dims4()?;
    let scale = 1.0 / (head_dim as f64).sqrt();
    let q = q.contiguous()?;
    let kt = k.transpose(2, 3)?.contiguous()?;
    let mut scores = (q.matmul(&kt)? * scale)?;
    if let Some(m) = mask {
        scores = scores.broadcast_add(m)?;
    }
    let dtype = scores.dtype();
    let probs = candle_nn::ops::softmax_last_dim(&scores.to_dtype(DType::F32)?)?.to_dtype(dtype)?;
    let out = probs.matmul(&v.contiguous()?)?;
    out.transpose(1, 2)?.reshape((b, l, h * d))
}

/// Qwen3's MLP: down(silu(gate(x)) · up(x)).
#[derive(Debug, Clone)]
pub struct Mlp {
    gate: Linear,
    up: Linear,
    down: Linear,
}

impl Mlp {
    pub fn load(vb: &VarBuilder, hidden: usize, inter: usize) -> Result<Mlp> {
        Ok(Mlp {
            gate: Linear::load(&vb.pp("gate_proj"), hidden, inter, false)?,
            up: Linear::load(&vb.pp("up_proj"), hidden, inter, false)?,
            down: Linear::load(&vb.pp("down_proj"), inter, hidden, false)?,
        })
    }
}

impl Module for Mlp {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let g = candle_nn::ops::silu(&self.gate.forward(x)?)?;
        self.down.forward(&(g * self.up.forward(x)?)?)
    }
}

/// A pre-norm transformer layer (Qwen3's decoder layer; ACE-Step's
/// encoder layer is the same with bidirectional masks).
#[derive(Debug, Clone)]
pub struct EncoderLayer {
    ln1: RmsNorm,
    attn: Attention,
    ln2: RmsNorm,
    mlp: Mlp,
}

impl EncoderLayer {
    pub fn load(vb: &VarBuilder, s: AttnShape, inter: usize) -> Result<EncoderLayer> {
        Ok(EncoderLayer {
            ln1: RmsNorm::load(&vb.pp("input_layernorm"), s.hidden, s.eps)?,
            attn: Attention::load(&vb.pp("self_attn"), s)?,
            ln2: RmsNorm::load(&vb.pp("post_attention_layernorm"), s.hidden, s.eps)?,
            mlp: Mlp::load(&vb.pp("mlp"), s.hidden, inter)?,
        })
    }

    pub fn forward(&self, x: &Tensor, rope: &Rope, mask: Option<&Tensor>) -> Result<Tensor> {
        let h = self
            .attn
            .forward_self(&self.ln1.forward(x)?, Some(rope), mask)?;
        let x = (x + h)?;
        let h = self.mlp.forward(&self.ln2.forward(&x)?)?;
        x + h
    }
}

/// Mean over the last dim, used by tests and metrics.
pub fn max_abs(t: &Tensor) -> Result<f32> {
    t.abs()?
        .flatten_all()?
        .max(D::Minus1)?
        .to_dtype(DType::F32)?
        .to_scalar::<f32>()
}
