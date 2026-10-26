//! The flow-matching sampler ACE-Step's base model generates with: an
//! Euler ODE from noise (t = 1) to data (t = 0) on a shifted linear
//! schedule, the velocity guided by APG (adaptive projected guidance,
//! `apg_guidance.py`) between the conditioned and the null-conditioned
//! prediction. DCW is off for base models (the reference's default), and
//! lego's repaint range is the whole take, which makes the repaint
//! injection and blend no-ops; neither is here.

use candle_core::{DType, Device, Result, Tensor};

use crate::dit::{CrossKv, Dit};

/// The reference's schedule: `linspace(1, 0, steps + 1)` in f32, then
/// `shift·t / (1 + (shift − 1)·t)`.
pub fn timesteps(steps: usize, shift: f32) -> Vec<f32> {
    (0..=steps)
        .map(|i| {
            // torch.linspace: start + i·step for the first half, end − (n−1−i)·step after.
            let n = steps + 1;
            let step = (0.0f32 - 1.0) / steps as f32;
            let t = if i < n / 2 { 1.0 + step * i as f32 } else { 0.0 - step * (steps - i) as f32 };
            if shift != 1.0 {
                shift * t / (1.0 + (shift - 1.0) * t)
            } else {
                t
            }
        })
        .collect()
}

/// APG with the reference's constants: momentum −0.75, norm threshold
/// 2.5, η = 0, norms and projection along time (`dims=[1]` of (B, T, C)).
pub struct Apg {
    momentum: f32,
    running: Option<Vec<f32>>,
    norm_threshold: f32,
}

impl Default for Apg {
    fn default() -> Self {
        Apg {
            momentum: -0.75,
            running: None,
            norm_threshold: 2.5,
        }
    }
}

impl Apg {
    /// `cond`, `uncond`: (T, C) row-major. Returns the guided velocity.
    pub fn guide(&mut self, cond: &[f32], uncond: &[f32], frames: usize, channels: usize, scale: f64) -> Vec<f32> {
        let n = frames * channels;
        // In f32 as the reference does: diff, the momentum and the clip.
        let mut diff: Vec<f32> = (0..n).map(|i| cond[i] - uncond[i]).collect();
        // MomentumBuffer.update: running = diff + momentum · running.
        if let Some(prev) = &self.running {
            for (d, p) in diff.iter_mut().zip(prev) {
                *d += self.momentum * p;
            }
        }
        self.running = Some(diff.clone());
        for c in 0..channels {
            let norm = (0..frames).map(|t| (diff[t * channels + c] as f64).powi(2)).sum::<f64>().sqrt() as f32;
            let f = (self.norm_threshold / norm).min(1.0);
            for t in 0..frames {
                diff[t * channels + c] *= f;
            }
        }
        // project(): in f64, v1 = normalize(cond) along time; orthogonal part.
        let mut out = vec![0f32; n];
        for c in 0..channels {
            let norm = (0..frames).map(|t| (cond[t * channels + c] as f64).powi(2)).sum::<f64>().sqrt().max(1e-12);
            let dot: f64 = (0..frames).map(|t| diff[t * channels + c] as f64 * (cond[t * channels + c] as f64 / norm)).sum();
            for t in 0..frames {
                let i = t * channels + c;
                let v1 = cond[i] as f64 / norm;
                let orth = (diff[i] as f64 - dot * v1) as f32;
                out[i] = cond[i] + (scale as f32 - 1.0) * orth;
            }
        }
        out
    }
}

/// What one generation needs from the conditioning.
pub struct Conditioning<'a> {
    pub dit: &'a Dit,
    /// (1, T, 128): source latents and chunk mask.
    pub context: &'a Tensor,
    /// Cross-attention K/V of the conditioned and the null branch, stacked
    /// as batch 2 (conditioned first).
    pub kv: &'a CrossKv,
}

/// Runs the Euler loop from `noise` (1, T, 64, on the CPU, f32). `step`
/// is told each finished step (1-based) and answers false to stop.
pub fn sample(
    c: &Conditioning,
    noise: &Tensor,
    steps: usize,
    guidance: f64,
    shift: f32,
    step: &mut dyn FnMut(usize) -> bool,
) -> Result<Option<Tensor>> {
    let (_, frames, channels) = noise.dims3()?;
    let dev = c.dit.device().clone();
    let dtype = c.dit.dtype();
    let ts = timesteps(steps, shift);
    let context2 = Tensor::cat(&[c.context, c.context], 0)?;
    let mut xt: Vec<f32> = noise.flatten_all()?.to_vec1()?;
    let mut apg = Apg::default();
    for i in 0..steps {
        let (t_curr, t_prev) = (ts[i], ts[i + 1]);
        let x = Tensor::from_slice(&xt, (1, frames, channels), &Device::Cpu)?.to_dtype(dtype)?.to_device(&dev)?;
        let x2 = Tensor::cat(&[&x, &x], 0)?;
        let v = c.dit.forward(&x2, &context2, &[t_curr, t_curr], c.kv)?;
        let v = v.to_dtype(DType::F32)?.to_device(&Device::Cpu)?;
        let cond: Vec<f32> = v.get(0)?.flatten_all()?.to_vec1()?;
        let uncond: Vec<f32> = v.get(1)?.flatten_all()?.to_vec1()?;
        let vt = if guidance > 1.0 {
            apg.guide(&cond, &uncond, frames, channels, guidance)
        } else {
            cond
        };
        let dt = t_curr - t_prev;
        for (x, v) in xt.iter_mut().zip(&vt) {
            *x -= v * dt;
        }
        if !step(i + 1) {
            return Ok(None);
        }
    }
    Ok(Some(Tensor::from_vec(xt, (1, frames, channels), &Device::Cpu)?))
}
