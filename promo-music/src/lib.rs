//! Generated music layers for PromoShot (core-private docs/MIDI.md §7):
//! ACE-Step 1.5's `lego` task in native Rust on candle — Metal on the Mac,
//! the CPU (or CUDA) headless — loading the published Hugging Face
//! safetensors as they are. No Python, no server.
//!
//! The pieces: [`vae`] (Oobleck, audio ↔ latents), [`text`] (the
//! Qwen3-Embedding text encoder and its tokenizer), [`dit`] (the condition
//! encoder and the 2B diffusion transformer), [`sampler`] (the flow-matching
//! Euler loop with APG guidance), [`pipeline`] (a lego request end to end),
//! and [`store`] (where the weights live and their pinned install).

pub mod dit;
pub mod nn;
pub mod pipeline;
pub mod rng;
pub mod sampler;
pub mod text;
pub mod vae;

use candle_core::{DType, Device};

/// The compute device this build prefers: the first Metal GPU when the
/// `metal` feature is on (and `PROMOSHOT_MUSIC_DEVICE` is not `cpu`), else
/// the first CUDA GPU with `cuda`, else the CPU.
pub fn default_device() -> Device {
    let want_cpu = std::env::var("PROMOSHOT_MUSIC_DEVICE").is_ok_and(|v| v == "cpu");
    if !want_cpu {
        #[cfg(feature = "metal")]
        if let Ok(d) = Device::new_metal(0) {
            return d;
        }
        #[cfg(feature = "cuda")]
        if let Ok(d) = Device::new_cuda(0) {
            return d;
        }
    }
    Device::Cpu
}

/// The dtype weights and activations use on `device`: f32 on the CPU (the
/// fast path there, and the reference's), `PROMOSHOT_MUSIC_DTYPE`
/// (`f32`, `f16`, `bf16`) or f32 on a GPU.
pub fn default_dtype(device: &Device) -> DType {
    match std::env::var("PROMOSHOT_MUSIC_DTYPE").as_deref() {
        Ok("f16") if !device.is_cpu() => DType::F16,
        Ok("bf16") if !device.is_cpu() => DType::BF16,
        _ => DType::F32,
    }
}
