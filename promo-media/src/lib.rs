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
pub mod proxy;

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
/// Which of an asset's audio tracks to sum. The apps insert every kept
/// track of a multi-track recording into the mix at unity (AVFoundation's
/// mixer adds them), so summing is the parity-correct reading, not a
/// per-track average.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackSelection {
    /// Every audio track, summed.
    All,
    /// Only the first audio track (sound layers, narration).
    First,
    /// Every track except these zero-based indices.
    Except(Vec<i64>),
}

impl TrackSelection {
    /// Which zero-based tracks out of `count` this selection keeps, in order.
    pub fn kept(&self, count: usize) -> Vec<usize> {
        match self {
            TrackSelection::All => (0..count).collect(),
            TrackSelection::First => (0..count.min(1)).collect(),
            TrackSelection::Except(disabled) => (0..count)
                .filter(|i| !disabled.contains(&(*i as i64)))
                .collect(),
        }
    }
}

pub trait AudioReader: Send + Sync {
    /// `None` when the asset carries no audio track — which is not an error,
    /// it is most screen recordings.
    fn read(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Option<AudioBuffer>, MediaError> {
        self.read_at_speed(path, sample_rate, channels, 1.0)
    }

    /// `read_at_speed` over a subset of the asset's audio tracks, summed.
    /// `None` when nothing is kept — a recording whose every track the
    /// person switched off is silent, not an error. Backends that cannot
    /// pick tracks refuse anything but `All` rather than quietly summing
    /// everything.
    fn read_tracks(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        speed: f64,
        tracks: &TrackSelection,
    ) -> Result<Option<AudioBuffer>, MediaError> {
        match tracks {
            TrackSelection::All => self.read_at_speed(path, sample_rate, channels, speed),
            other => Err(MediaError::Backend(format!(
                "this audio reader cannot select tracks ({other:?})"
            ))),
        }
    }

    /// Reads the asset time-stretched by `speed`, PITCH PRESERVED — 1.5 is
    /// half again as fast and the voice is unchanged. Naive resampling would
    /// be one line shorter and turn narration into a chipmunk.
    fn read_at_speed(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        speed: f64,
    ) -> Result<Option<AudioBuffer>, MediaError>;

    /// `read_tracks`, with an extra filter chain (see [`effects_chain`])
    /// applied after the tempo chain. A reader that cannot filter refuses
    /// a chain rather than quietly playing the resource dry.
    fn read_tracks_with(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        speed: f64,
        tracks: &TrackSelection,
        extra_filter: Option<&str>,
    ) -> Result<Option<AudioBuffer>, MediaError> {
        match extra_filter {
            None => self.read_tracks(path, sample_rate, channels, speed, tracks),
            Some(chain) => Err(MediaError::Backend(format!(
                "this audio reader cannot apply effects ({chain})"
            ))),
        }
    }
}

/// `atempo` handles 0.5–2.0 in one pass, so anything outside that is chained:
/// 3.0 becomes 2.0 then 1.5. Returns None for a rate near enough to 1 that
/// the filter would only cost a resample.
/// The ffmpeg filter chain for a resource's audio effects, in order —
/// `None` when nothing applies. Numbers are formatted plainly so the chain
/// is the same string on every host.
pub fn effects_chain(effects: &[promo_model::AudioEffect]) -> Option<String> {
    use promo_model::AudioEffectKind;
    let mut stages: Vec<String> = Vec::new();
    for effect in effects {
        match effect.kind {
            AudioEffectKind::None => {}
            AudioEffectKind::Normalize => {
                let target = effect.target_lufs.unwrap_or(-16.0).clamp(-70.0, -5.0);
                stages.push(format!("loudnorm=I={target}:TP=-1.5:LRA=11"));
            }
            AudioEffectKind::Compressor => {
                let threshold = effect.threshold_db.unwrap_or(-18.0).clamp(-60.0, 0.0);
                let ratio = effect.ratio.unwrap_or(3.0).clamp(1.0, 20.0);
                let attack = effect.attack_ms.unwrap_or(20.0).clamp(0.01, 2000.0);
                let release = effect.release_ms.unwrap_or(250.0).clamp(0.01, 9000.0);
                stages.push(format!(
                    "acompressor=threshold={threshold}dB:ratio={ratio}:attack={attack}:release={release}"
                ));
            }
            AudioEffectKind::Eq => {
                let Some(frequency) = effect.frequency_hz else {
                    continue;
                };
                let frequency = frequency.clamp(20.0, 20_000.0);
                let width = effect.width_octaves.unwrap_or(1.0).clamp(0.05, 10.0);
                let gain = effect.gain_db.unwrap_or(0.0).clamp(-30.0, 30.0);
                stages.push(format!("equalizer=f={frequency}:t=o:w={width}:g={gain}"));
            }
        }
    }
    if stages.is_empty() {
        None
    } else {
        Some(stages.join(","))
    }
}

pub fn atempo_chain(speed: f64) -> Option<String> {
    if !(speed.is_finite()) || (speed - 1.0).abs() < 1e-6 || speed <= 0.0 {
        return None;
    }
    let mut remaining = speed.clamp(0.1, 10.0);
    let mut stages = Vec::new();
    while remaining > 2.0 {
        stages.push(2.0);
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        stages.push(0.5);
        remaining /= 0.5;
    }
    stages.push(remaining);
    Some(
        stages
            .iter()
            .map(|s| format!("atempo={s:.6}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// What to write.
#[derive(Debug, Clone)]
pub struct EncodeSpec {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Chapter starts (seconds, title) the container carries — a player's
    /// chapter menu. Each runs to the next, the last to the end.
    pub chapters: Vec<(f64, String)>,
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

#[cfg(test)]
mod effects_tests {
    use super::*;
    use promo_model::{AudioEffect, AudioEffectKind};

    fn effect(kind: AudioEffectKind) -> AudioEffect {
        AudioEffect {
            kind,
            target_lufs: None,
            threshold_db: None,
            ratio: None,
            attack_ms: None,
            release_ms: None,
            frequency_hz: None,
            width_octaves: None,
            gain_db: None,
        }
    }

    /// The chain is the effects in order, defaults filled, unknowns
    /// skipped, and nothing at all when nothing applies.
    #[test]
    fn effects_chain_spells_each_stage_in_order() {
        assert_eq!(effects_chain(&[]), None);
        assert_eq!(effects_chain(&[effect(AudioEffectKind::None)]), None);
        let mut eq = effect(AudioEffectKind::Eq);
        eq.frequency_hz = Some(1000.0);
        eq.gain_db = Some(3.0);
        let mut comp = effect(AudioEffectKind::Compressor);
        comp.ratio = Some(4.0);
        assert_eq!(
            effects_chain(&[effect(AudioEffectKind::Normalize), comp, eq]).as_deref(),
            Some(
                "loudnorm=I=-16:TP=-1.5:LRA=11,\
                 acompressor=threshold=-18dB:ratio=4:attack=20:release=250,\
                 equalizer=f=1000:t=o:w=1:g=3"
            )
        );
        assert_eq!(effects_chain(&[effect(AudioEffectKind::Eq)]), None);
    }
}

#[cfg(test)]
mod speed_tests {
    use super::atempo_chain;

    #[test]
    fn ordinary_rates_are_a_single_stage() {
        // 0.5-2.0 is what one atempo instance accepts, and covers every rate
        // a person actually reaches for.
        assert_eq!(atempo_chain(1.2).unwrap(), "atempo=1.200000");
        assert_eq!(atempo_chain(0.8).unwrap(), "atempo=0.800000");
    }

    #[test]
    fn unity_asks_for_no_filter_at_all() {
        assert!(atempo_chain(1.0).is_none(), "1x must not cost a resample");
        assert!(atempo_chain(1.0000001).is_none());
    }

    #[test]
    fn extreme_rates_chain_to_stay_inside_the_filter_limits() {
        // 3x is outside one instance's range, so it becomes 2x then 1.5x —
        // whose product is 3. A single atempo=3 would be rejected by ffmpeg.
        let chain = atempo_chain(3.0).unwrap();
        assert_eq!(chain.matches("atempo").count(), 2, "{chain}");
        let product: f64 = chain
            .split(',')
            .map(|s| s.trim_start_matches("atempo=").parse::<f64>().unwrap())
            .product();
        assert!(
            (product - 3.0).abs() < 1e-6,
            "{chain} multiplies to {product}"
        );

        let slow = atempo_chain(0.25).unwrap();
        let product: f64 = slow
            .split(',')
            .map(|s| s.trim_start_matches("atempo=").parse::<f64>().unwrap())
            .product();
        assert!(
            (product - 0.25).abs() < 1e-6,
            "{slow} multiplies to {product}"
        );
    }

    #[test]
    fn nonsense_rates_are_refused_rather_than_dividing_by_zero() {
        assert!(atempo_chain(0.0).is_none());
        assert!(atempo_chain(-1.0).is_none());
        assert!(atempo_chain(f64::NAN).is_none());
    }
}
