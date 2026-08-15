//! promo-media: codec I/O — the contract, the registry, and the backends.
//!
//! Every front end needs to read and write video, so this is the one place
//! that knows how. A backend implements [`DecoderBackend`] / [`EncoderBackend`]
//! and registers; callers ask the [`Registry`] and never name a codec library.
//!
//! Decoders hand frames over as [`GpuSurface`](promo_gpu::GpuSurface), which
//! `Compositor::import` already dispatches on — so a backend can return CPU
//! pixels today and a zero-copy platform surface later without touching a
//! single caller.

pub mod conformance;
pub mod ffmpeg;

use promo_gpu::GpuSurface;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("asset unsupported by this backend: {0}")]
    Unsupported(String),
    #[error("backend failure: {0}")]
    Backend(String),
    #[error("no backend can open {0}")]
    NoBackend(String),
    #[error("{0} is not available: {1}")]
    ToolMissing(&'static str, String),
}

/// What a codec backend can do — the engine negotiates per asset.
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub name: &'static str,
    pub hardware_accelerated: bool,
    pub max_width: u32,
    pub max_height: u32,
}

/// Stream info returned by `open`.
#[derive(Debug, Clone, Default)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_s: f64,
    pub nominal_fps: f64,
    /// Display rotation in degrees. **Every backend must honour this** — a
    /// portrait phone capture is stored landscape with a rotation tag, and a
    /// backend that ignores it renders every such clip on its side.
    pub rotation_degrees: i32,
}

/// A decode session over one asset.
///
/// Rendering walks time forward, so implementations may assume mostly
/// monotonic requests and treat a backwards jump as the expensive case.
pub trait VideoDecoder: Send {
    fn info(&self) -> &VideoInfo;

    /// The frame to display at `pts_s`, or `None` past the end.
    fn frame_at(&mut self, pts_s: f64) -> Result<Option<GpuSurface>, MediaError>;
}

pub trait DecoderBackend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    /// Can this backend open the asset at all? Cheap check — the real answer
    /// comes from `open`.
    fn probe(&self, path: &Path) -> bool;
    fn open(&self, path: &Path) -> Result<Box<dyn VideoDecoder>, MediaError>;
}

/// Interleaved f32 PCM, and what it is.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl AudioBuffer {
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration_s(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }
}

/// Reads an asset's audio as PCM. Whole-buffer rather than streaming: a
/// composition's audio is minutes of f32 at most, and the mixer wants random
/// access across overlapping layers anyway.
pub trait AudioReader: Send + Sync {
    /// `None` when the asset carries no audio track — which is not an error,
    /// it is most screen recordings.
    fn read(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Option<AudioBuffer>, MediaError>;
}

/// What to write.
#[derive(Debug, Clone)]
pub struct EncodeSpec {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Lower is better quality; backends map it to their own scale.
    pub quality: u32,
    /// Optional mixed soundtrack, muxed alongside the video.
    pub audio: Option<AudioBuffer>,
}

/// An encode session. Frames arrive as tightly packed BGRA rows.
pub trait VideoEncoder {
    fn write_frame(&mut self, bgra: &[u8]) -> Result<(), MediaError>;
    /// Flushes and finishes the file. Consumes the encoder because a
    /// half-finished video file is not a useful object to keep holding.
    fn finish(self: Box<Self>) -> Result<(), MediaError>;
}

pub trait EncoderBackend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    fn open(&self, path: &Path, spec: &EncodeSpec) -> Result<Box<dyn VideoEncoder>, MediaError>;
}

/// The backends available to this build, in preference order.
pub struct Registry {
    decoders: Vec<Box<dyn DecoderBackend>>,
    encoders: Vec<Box<dyn EncoderBackend>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            decoders: Vec::new(),
            encoders: Vec::new(),
        }
    }

    /// The audio reader for this build.
    pub fn audio_reader(&self) -> &dyn AudioReader {
        &ffmpeg::FfmpegAudioReader
    }

    /// Everything this build can offer. Today that is the ffmpeg backend;
    /// VideoToolbox is registered by the Apple host, Media Foundation later.
    pub fn with_defaults() -> Self {
        let mut registry = Self::empty();
        registry.register_decoder(Box::new(ffmpeg::FfmpegDecoderBackend));
        registry.register_encoder(Box::new(ffmpeg::FfmpegEncoderBackend));
        registry
    }

    pub fn register_decoder(&mut self, backend: Box<dyn DecoderBackend>) {
        self.decoders.push(backend);
    }

    pub fn register_encoder(&mut self, backend: Box<dyn EncoderBackend>) {
        self.encoders.push(backend);
    }

    pub fn decoder_names(&self) -> Vec<&'static str> {
        self.decoders
            .iter()
            .map(|b| b.capabilities().name)
            .collect()
    }

    /// First backend that will take the asset.
    pub fn open_decoder(&self, path: &Path) -> Result<Box<dyn VideoDecoder>, MediaError> {
        let mut last: Option<MediaError> = None;
        for backend in &self.decoders {
            if !backend.probe(path) {
                continue;
            }
            match backend.open(path) {
                Ok(decoder) => return Ok(decoder),
                // Keep trying: one backend refusing an asset is not the
                // answer, it is one opinion.
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| MediaError::NoBackend(path.display().to_string())))
    }

    pub fn open_encoder(
        &self,
        path: &Path,
        spec: &EncodeSpec,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        let mut last: Option<MediaError> = None;
        for backend in &self.encoders {
            match backend.open(path, spec) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| MediaError::NoBackend(path.display().to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_registry_offers_ffmpeg() {
        let registry = Registry::with_defaults();
        assert!(registry.decoder_names().contains(&"ffmpeg"));
    }

    #[test]
    fn an_unopenable_asset_reports_which_path_failed() {
        let registry = Registry::with_defaults();
        let Err(err) = registry.open_decoder(Path::new("/nonexistent/clip.mp4")) else {
            panic!("a missing file must not open");
        };
        assert!(
            err.to_string().contains("clip.mp4"),
            "the message must name the file: {err}"
        );
    }
}
