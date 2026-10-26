//! The Oobleck VAE (Stable Audio's, as diffusers' `AutoencoderOobleck`
//! with ACE-Step 1.5's config): 48 kHz stereo ↔ 64-channel latents at
//! 25 Hz (one latent per 1920 samples). Weight-normalised convolutions are
//! folded at load (`w = g · v / ‖v‖`), the Snake activations' `exp(α)` and
//! `1 / (exp(β) + 1e-9)` precomputed.
//!
//! Encode and decode tile long signals the way ACE-Step's PyTorch path
//! does (overlap, then discard the overlap), so a long context costs
//! bounded memory and the output matches the reference.

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::VarBuilder;

pub const HOP: usize = 1920;
pub const LATENT_DIM: usize = 64;

/// Encode tiling: 30 s windows, 2 s overlap each side (the reference's
/// CPU and large-GPU choice).
pub const ENCODE_CHUNK: usize = 48_000 * 30;
pub const ENCODE_OVERLAP: usize = 48_000 * 2;
/// Decode tiling in latents (the reference's CPU choice).
pub const DECODE_CHUNK: usize = 256;
pub const DECODE_OVERLAP: usize = 64;

#[derive(Debug, Clone)]
struct Snake {
    alpha: Tensor,
    inv_beta: Tensor,
}

impl Snake {
    fn load(vb: &VarBuilder, dim: usize) -> Result<Snake> {
        let a = vb.get((1, dim, 1), "alpha")?.to_dtype(DType::F32)?.exp()?;
        let b = vb.get((1, dim, 1), "beta")?.to_dtype(DType::F32)?.exp()?;
        let inv_beta = (b + 1e-9)?.recip()?;
        let dt = vb.dtype();
        Ok(Snake {
            alpha: a.to_dtype(dt)?,
            inv_beta: inv_beta.to_dtype(dt)?,
        })
    }
}

impl Module for Snake {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let s = x.broadcast_mul(&self.alpha)?.sin()?.sqr()?;
        x + s.broadcast_mul(&self.inv_beta)?
    }
}

/// Folds a weight-normalised kernel stored as `weight_g`/`weight_v`
/// (norm over every dim but 0, PyTorch's default).
fn weight_norm(vb: &VarBuilder, shape: (usize, usize, usize)) -> Result<Tensor> {
    let v = vb.get(shape, "weight_v")?.to_dtype(DType::F32)?;
    let g = vb.get((shape.0, 1, 1), "weight_g")?.to_dtype(DType::F32)?;
    let norm = v.sqr()?.sum_keepdim(2)?.sum_keepdim(1)?.sqrt()?;
    v.broadcast_mul(&g.broadcast_div(&norm)?)?.to_dtype(vb.dtype())
}

#[derive(Debug, Clone)]
struct Conv {
    w: Tensor,
    b: Option<Tensor>,
    padding: usize,
    stride: usize,
    dilation: usize,
}

impl Conv {
    #[allow(clippy::too_many_arguments)]
    fn load(vb: &VarBuilder, cin: usize, cout: usize, k: usize, padding: usize, stride: usize, dilation: usize, bias: bool) -> Result<Conv> {
        Ok(Conv {
            w: weight_norm(vb, (cout, cin, k))?,
            b: if bias { Some(vb.get(cout, "bias")?.reshape((1, cout, 1))?) } else { None },
            padding,
            stride,
            dilation,
        })
    }
}

impl Module for Conv {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.conv1d(&self.w, self.padding, self.stride, self.dilation, 1)?;
        match &self.b {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}

#[derive(Debug, Clone)]
struct ConvT {
    w: Tensor,
    b: Tensor,
    padding: usize,
    stride: usize,
}

/// The decoder's transposed convolutions have kernel 2·stride and padding
/// stride/2 (every stride is even), so each input frame writes two
/// stride-long blocks: one matmul and an overlap-add. candle's generic
/// `conv_transpose1d` kernel is an order of magnitude slower on Metal.
pub fn conv_transpose_k2s(x: &Tensor, w: &Tensor, stride: usize, padding: usize) -> Result<Tensor> {
    let (b, cin, len) = x.dims3()?;
    let (_, cout, k) = w.dims3()?;
    assert_eq!(k, 2 * stride);
    let wr = w.reshape((cin, cout * k))?;
    let mut outs = Vec::with_capacity(b);
    for i in 0..b {
        let xt = x.get(i)?.t()?.contiguous()?; // (len, cin)
        let z = xt.matmul(&wr)?.reshape((len, cout, k))?;
        let a = z.narrow(2, 0, stride)?.permute((1, 0, 2))?.reshape((cout, len * stride))?;
        let bb = z.narrow(2, stride, stride)?.permute((1, 0, 2))?.reshape((cout, len * stride))?;
        let zeros = Tensor::zeros((cout, stride), x.dtype(), x.device())?;
        let full = (Tensor::cat(&[&a, &zeros], 1)? + Tensor::cat(&[&zeros, &bb], 1)?)?;
        let out_len = (len - 1) * stride + k - 2 * padding;
        outs.push(full.narrow(1, padding, out_len)?);
    }
    Tensor::stack(&outs, 0)
}

impl Module for ConvT {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        conv_transpose_k2s(x, &self.w, self.stride, self.padding)?.broadcast_add(&self.b)
    }
}

#[derive(Debug, Clone)]
struct ResUnit {
    snake1: Snake,
    conv1: Conv,
    snake2: Snake,
    conv2: Conv,
}

impl ResUnit {
    fn load(vb: &VarBuilder, dim: usize, dilation: usize) -> Result<ResUnit> {
        let pad = (6 * dilation) / 2;
        Ok(ResUnit {
            snake1: Snake::load(&vb.pp("snake1"), dim)?,
            conv1: Conv::load(&vb.pp("conv1"), dim, dim, 7, pad, 1, dilation, true)?,
            snake2: Snake::load(&vb.pp("snake2"), dim)?,
            conv2: Conv::load(&vb.pp("conv2"), dim, dim, 1, 0, 1, 1, true)?,
        })
    }
}

impl Module for ResUnit {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = self.conv1.forward(&self.snake1.forward(x)?)?;
        let y = self.conv2.forward(&self.snake2.forward(&y)?)?;
        // The convolutions keep the length ("same" padding), so no crop.
        x + y
    }
}

#[derive(Debug, Clone)]
struct EncBlock {
    res: [ResUnit; 3],
    snake: Snake,
    conv: Conv,
}

#[derive(Debug, Clone)]
struct DecBlock {
    snake: Snake,
    conv_t: ConvT,
    res: [ResUnit; 3],
}

/// The VAE's encoder or decoder half, loaded on its own so a host can keep
/// only the half it needs.
#[derive(Debug, Clone)]
pub struct Encoder {
    conv1: Conv,
    blocks: Vec<EncBlock>,
    snake: Snake,
    conv2: Conv,
}

#[derive(Debug, Clone)]
pub struct Decoder {
    conv1: Conv,
    blocks: Vec<DecBlock>,
    snake: Snake,
    conv2: Conv,
}

const HIDDEN: usize = 128;
const RATIOS: [usize; 5] = [2, 4, 4, 6, 10];
const MULTS: [usize; 6] = [1, 1, 2, 4, 8, 16];

fn res3(vb: &VarBuilder, dim: usize) -> Result<[ResUnit; 3]> {
    Ok([
        ResUnit::load(&vb.pp("res_unit1"), dim, 1)?,
        ResUnit::load(&vb.pp("res_unit2"), dim, 3)?,
        ResUnit::load(&vb.pp("res_unit3"), dim, 9)?,
    ])
}

impl Encoder {
    /// `vb` at the checkpoint root (keys `encoder.*`).
    pub fn load(vb: &VarBuilder) -> Result<Encoder> {
        let vb = vb.pp("encoder");
        let mut blocks = Vec::new();
        for (i, &stride) in RATIOS.iter().enumerate() {
            let (cin, cout) = (HIDDEN * MULTS[i], HIDDEN * MULTS[i + 1]);
            let b = vb.pp("block").pp(i);
            blocks.push(EncBlock {
                res: res3(&b, cin)?,
                snake: Snake::load(&b.pp("snake1"), cin)?,
                conv: Conv::load(&b.pp("conv1"), cin, cout, 2 * stride, stride.div_ceil(2), stride, 1, true)?,
            });
        }
        let d = HIDDEN * MULTS[5];
        Ok(Encoder {
            conv1: Conv::load(&vb.pp("conv1"), 2, HIDDEN, 7, 3, 1, 1, true)?,
            blocks,
            snake: Snake::load(&vb.pp("snake1"), d)?,
            conv2: Conv::load(&vb.pp("conv2"), d, 2 * LATENT_DIM, 3, 1, 1, 1, true)?,
        })
    }

    /// (B, 2, N) audio → (mean, std), each (B, 64, N / 1920).
    pub fn moments(&self, x: &Tensor) -> Result<(Tensor, Tensor)> {
        let mut h = self.conv1.forward(x)?;
        for b in &self.blocks {
            for r in &b.res {
                h = r.forward(&h)?;
            }
            h = b.conv.forward(&b.snake.forward(&h)?)?;
        }
        let h = self.conv2.forward(&self.snake.forward(&h)?)?;
        let mean = h.narrow(1, 0, LATENT_DIM)?;
        let scale = h.narrow(1, LATENT_DIM, LATENT_DIM)?.to_dtype(DType::F32)?;
        // softplus(x) + 1e-4, the stable way: max(x, 0) + ln(1 + e^-|x|).
        let sp = (scale.relu()? + (scale.abs()?.neg()?.exp()? + 1.0)?.log()?)?;
        let std = (sp + 1e-4)?.to_dtype(mean.dtype())?;
        Ok((mean, std))
    }

    /// Tiled encode of (1, 2, N) audio into the posterior sample
    /// `mean + std · noise`. `noise(len)` gives the standard normals for one
    /// window's (64, len) posterior, in order — seeded noise in production,
    /// the reference's dumped noise in the parity tests.
    pub fn encode_tiled(&self, audio: &Tensor, noise: &mut dyn FnMut(usize) -> Result<Tensor>) -> Result<Tensor> {
        let samples = audio.dim(2)?;
        let sample = |x: &Tensor, noise: &mut dyn FnMut(usize) -> Result<Tensor>| -> Result<Tensor> {
            let (m, s) = self.moments(x)?;
            let n = noise(m.dim(2)?)?.to_dtype(m.dtype())?.to_device(m.device())?.reshape(m.shape())?;
            let z = (m + s.mul(&n)?)?;
            // Finish this window before its inputs drop (see pipeline.rs:
            // candle's Metal buffer reuse).
            z.device().synchronize()?;
            Ok(z)
        };
        if samples <= ENCODE_CHUNK {
            return sample(audio, noise);
        }
        let stride = ENCODE_CHUNK - 2 * ENCODE_OVERLAP;
        let steps = samples.div_ceil(stride);
        let mut parts = Vec::with_capacity(steps);
        for i in 0..steps {
            let core_start = i * stride;
            let core_end = (core_start + stride).min(samples);
            let win_start = core_start.saturating_sub(ENCODE_OVERLAP);
            let win_end = (core_end + ENCODE_OVERLAP).min(samples);
            let chunk = audio.narrow(2, win_start, win_end - win_start)?;
            let lat = sample(&chunk, noise)?;
            let factor = (win_end - win_start) as f64 / lat.dim(2)? as f64;
            let trim_start = ((core_start - win_start) as f64 / factor).round() as usize;
            let trim_end = ((win_end - core_end) as f64 / factor).round() as usize;
            let len = lat.dim(2)?;
            parts.push(lat.narrow(2, trim_start, len - trim_end - trim_start)?);
        }
        Tensor::cat(&parts, 2)
    }
}

impl Decoder {
    pub fn load(vb: &VarBuilder) -> Result<Decoder> {
        let vb = vb.pp("decoder");
        let n = RATIOS.len();
        let mut blocks = Vec::new();
        for (i, &stride) in RATIOS.iter().rev().enumerate() {
            let (cin, cout) = (HIDDEN * MULTS[n - i], HIDDEN * MULTS[n - i - 1]);
            let b = vb.pp("block").pp(i);
            let cvb = b.pp("conv_t1");
            let w = weight_norm(&cvb, (cin, cout, 2 * stride))?;
            blocks.push(DecBlock {
                snake: Snake::load(&b.pp("snake1"), cin)?,
                conv_t: ConvT {
                    w,
                    b: cvb.get(cout, "bias")?.reshape((1, cout, 1))?,
                    padding: stride.div_ceil(2),
                    stride,
                },
                res: res3(&b, cout)?,
            });
        }
        Ok(Decoder {
            conv1: Conv::load(&vb.pp("conv1"), LATENT_DIM, HIDDEN * 16, 7, 3, 1, 1, true)?,
            blocks,
            snake: Snake::load(&vb.pp("snake1"), HIDDEN)?,
            conv2: Conv::load(&vb.pp("conv2"), HIDDEN, 2, 7, 3, 1, 1, false)?,
        })
    }

    /// (B, 64, T) latents → (B, 2, T · 1920) audio.
    pub fn decode(&self, z: &Tensor) -> Result<Tensor> {
        let mut h = self.conv1.forward(z)?;
        for b in &self.blocks {
            h = b.conv_t.forward(&b.snake.forward(&h)?)?;
            for r in &b.res {
                h = r.forward(&h)?;
            }
        }
        self.conv2.forward(&self.snake.forward(&h)?)
    }

    /// Tiled decode, `chunk`/`overlap` in latents; each window's audio is
    /// moved to the CPU as f32 as soon as it is made.
    pub fn decode_tiled(&self, z: &Tensor, chunk: usize, overlap: usize) -> Result<Tensor> {
        let frames = z.dim(2)?;
        let cpu = Device::Cpu;
        if frames <= chunk {
            return self.decode(z)?.to_dtype(DType::F32)?.to_device(&cpu);
        }
        let mut overlap = overlap;
        while chunk <= 2 * overlap && overlap > 4 {
            overlap /= 2;
        }
        let stride = chunk - 2 * overlap;
        let steps = frames.div_ceil(stride);
        let mut parts = Vec::with_capacity(steps);
        for i in 0..steps {
            let core_start = i * stride;
            let core_end = (core_start + stride).min(frames);
            let win_start = core_start.saturating_sub(overlap);
            let win_end = (core_end + overlap).min(frames);
            let a = self.decode(&z.narrow(2, win_start, win_end - win_start)?)?;
            let up = a.dim(2)? / (win_end - win_start);
            let trim_start = (core_start - win_start) * up;
            let trim_end = (win_end - core_end) * up;
            let len = a.dim(2)?;
            parts.push(a.narrow(2, trim_start, len - trim_start - trim_end)?.to_dtype(DType::F32)?.to_device(&cpu)?);
        }
        Tensor::cat(&parts, 2)
    }
}
