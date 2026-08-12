//! promo-media: codec-backend traits (the backend contract), caches, proxy
//! manager and decoder-pool scheduling. No codec library is linked here —
//! backends register through these traits (VideoToolbox host-side on Apple,
//! Media Foundation on Windows, ffmpeg on Linux/fallback).
//!
//! P0 ships the trait shapes so the FFI and engine compile against them; the
//! conformance suite and real scheduling are Phase 1+.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("asset unsupported by this backend: {0}")]
    Unsupported(String),
    #[error("backend failure: {0}")]
    Backend(String),
}

/// What a codec backend can do — the engine negotiates per asset.
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub name: &'static str,
    pub hardware_accelerated: bool,
    pub max_width: u32,
    pub max_height: u32,
}

/// Stream info returned by open().
#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_s: f64,
    pub nominal_fps: f64,
    /// Display rotation in degrees (the preferredTransform lesson: this must
    /// be honored by every backend and is covered by the conformance suite).
    pub rotation_degrees: i32,
}

/// Pull-driven decode session. Implementations run their own threads; the
/// engine's scheduler calls these from its decode pool.
pub trait VideoDecoder: Send {
    fn info(&self) -> &VideoInfo;
    /// Seeks (keyframe-aligned) so the next `next_frame` is at/before `pts`.
    fn seek(&mut self, pts_s: f64) -> Result<(), MediaError>;
    /// Next frame in presentation order, as a GPU surface when possible.
    fn next_frame(&mut self) -> Result<Option<(promo_gpu_surface::Frame,)>, MediaError>;
}

/// Placeholder module so the trait signature names a frame type without a
/// circular dependency; replaced by the real promo-gpu surface hand-off in P1
/// when the FFI carries live frames.
pub mod promo_gpu_surface {
    #[derive(Debug)]
    pub struct Frame {
        pub pts_s: f64,
    }
}
