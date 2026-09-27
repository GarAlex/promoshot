//! One loudness for every host (review 2026-09-27, P2-30).
//!
//! A resource's `normalize` effect planned its gain two ways. The apps took
//! the RMS of the whole file minus 0.7 dB, so the pauses in a narration
//! pulled the estimate down and the voice came out loud. Headless ran
//! ffmpeg's loudnorm, a DYNAMIC filter that rides the level through the
//! file. Narration with pauses landed several dB apart. Now loudness is
//! ITU-R BS.1770-4 — K-weighting, 400 ms blocks, and the two gates that
//! leave silence out — measured here, and both hosts apply the one static
//! gain [`LoudnessMeter::gain_db`] plans, at the end of the resource's
//! chain: the whole resource, through its other effects, is measured, so a
//! compressor before or after it in the list lands on the same target.

/// The true-peak ceiling loudnorm used (`TP=-1.5`), kept as a sample-peak
/// ceiling: a quiet narration raised to the target never clips.
pub const PEAK_CEILING_DB: f64 = -1.5;
/// The most a normalize will move a file, either way.
pub const MAX_GAIN_DB: f64 = 24.0;

/// One biquad section's coefficients (a0 normalised to 1).
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

/// One channel's running state through a biquad, direct form I.
#[derive(Clone, Copy, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    fn step(&mut self, filter: &Biquad, x: f64) -> f64 {
        let y = filter.b[0] * x + filter.b[1] * self.x1 + filter.b[2] * self.x2
            - filter.a[0] * self.y1
            - filter.a[1] * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The K-weighting pair for `sample_rate`: the head's high shelf, then the
/// revised low-frequency B-curve high pass — BS.1770's filters, derived for
/// any rate (the standard tabulates 48 kHz only).
fn k_weighting(sample_rate: f64) -> (Biquad, Biquad) {
    let shelf = {
        let f0 = 1_681.974_450_955_533;
        let gain = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let vh = 10f64.powf(gain / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        }
    };
    let high_pass = {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        }
    };
    (shelf, high_pass)
}

/// BS.1770 integrated loudness, fed in pieces: an hour of narration is
/// measured chunk by chunk and holds only its 100 ms energies (36 000 of
/// them), never the samples. The 400 ms gating blocks, overlapping by 75%,
/// are four consecutive 100 ms sums.
pub struct LoudnessMeter {
    channels: usize,
    shelf: Biquad,
    high_pass: Biquad,
    state: Vec<(BiquadState, BiquadState)>,
    step: usize,
    partial_energy: f64,
    partial_frames: usize,
    sub_blocks: Vec<f64>,
    total_energy: f64,
    total_frames: usize,
    peak: f32,
}

impl LoudnessMeter {
    /// A meter for interleaved samples of `channels` at `sample_rate`;
    /// `None` for a shape no audio has.
    pub fn new(channels: usize, sample_rate: f64) -> Option<Self> {
        if channels == 0 || !(sample_rate.is_finite() && sample_rate >= 1000.0) {
            return None;
        }
        let (shelf, high_pass) = k_weighting(sample_rate);
        Some(Self {
            channels,
            shelf,
            high_pass,
            state: vec![Default::default(); channels],
            step: ((0.1 * sample_rate).round() as usize).max(1),
            partial_energy: 0.0,
            partial_frames: 0,
            sub_blocks: Vec::new(),
            total_energy: 0.0,
            total_frames: 0,
            peak: 0.0,
        })
    }

    /// Feeds interleaved samples; a trailing partial frame is ignored.
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            let mut energy = 0.0;
            for (value, (shelf, high_pass)) in frame.iter().zip(self.state.iter_mut()) {
                self.peak = self.peak.max(value.abs());
                let weighted =
                    high_pass.step(&self.high_pass, shelf.step(&self.shelf, *value as f64));
                energy += weighted * weighted;
            }
            self.partial_energy += energy;
            self.partial_frames += 1;
            self.total_energy += energy;
            self.total_frames += 1;
            if self.partial_frames == self.step {
                self.sub_blocks.push(self.partial_energy);
                self.partial_energy = 0.0;
                self.partial_frames = 0;
            }
        }
    }

    /// The largest absolute sample so far.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Integrated loudness in LUFS so far — `None` for silence (nothing
    /// passes the absolute gate). Every channel weighs 1, as the standard
    /// does for L, R and C; a clip shorter than one block is one block.
    pub fn integrated_lufs(&self) -> Option<f64> {
        let block_frames = (4 * self.step) as f64;
        let powers: Vec<f64> = if self.sub_blocks.len() >= 4 {
            self.sub_blocks
                .windows(4)
                .map(|w| w.iter().sum::<f64>() / block_frames)
                .collect()
        } else if self.total_frames > 0 {
            vec![self.total_energy / self.total_frames as f64]
        } else {
            return None;
        };
        let loudness = |power: f64| -0.691 + 10.0 * power.log10();
        let above_absolute: Vec<f64> = powers
            .into_iter()
            .filter(|p| *p > 0.0 && loudness(*p) > -70.0)
            .collect();
        if above_absolute.is_empty() {
            return None;
        }
        let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
        let relative = loudness(mean(&above_absolute)) - 10.0;
        let gated: Vec<f64> = above_absolute
            .into_iter()
            .filter(|p| loudness(*p) > relative)
            .collect();
        (!gated.is_empty()).then(|| loudness(mean(&gated)))
    }

    /// The gain, in dB, that takes what was fed to `target_lufs` — held
    /// under the peak ceiling and within ±24 dB; 0 for silence. What the
    /// apps and the headless mixer both apply to a normalized resource, as
    /// one static gain at the end of its chain.
    pub fn gain_db(&self, target_lufs: f64) -> f64 {
        let Some(loudness) = self.integrated_lufs() else {
            return 0.0;
        };
        let mut gain = target_lufs - loudness;
        if self.peak > 0.0 {
            gain = gain.min(PEAK_CEILING_DB - 20.0 * (self.peak as f64).log10());
        }
        gain.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    }
}

/// Integrated loudness in LUFS of interleaved samples, in one go.
pub fn integrated_lufs(samples: &[f32], channels: usize, sample_rate: f64) -> Option<f64> {
    let mut meter = LoudnessMeter::new(channels, sample_rate)?;
    meter.push(samples);
    meter.integrated_lufs()
}

/// The largest absolute sample.
pub fn sample_peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
}

/// [`LoudnessMeter::gain_db`] of interleaved samples, in one go.
pub fn normalize_gain_db(
    samples: &[f32],
    channels: usize,
    sample_rate: f64,
    target_lufs: f64,
) -> f64 {
    match LoudnessMeter::new(channels, sample_rate) {
        Some(mut meter) => {
            meter.push(samples);
            meter.gain_db(target_lufs)
        }
        None => 0.0,
    }
}

/// The target a normalize without one aims for.
pub const DEFAULT_TARGET_LUFS: f64 = -16.0;

/// A normalize's target as every host reads it: defaulted, then clamped
/// to -70…-5 LUFS.
pub fn target_lufs(requested: Option<f64>) -> f64 {
    requested
        .filter(|t| t.is_finite())
        .unwrap_or(DEFAULT_TARGET_LUFS)
        .clamp(-70.0, -5.0)
}

/// The target a resource's effects ask for, when they normalize.
pub fn normalize_target(effects: &[promo_model::AudioEffect]) -> Option<f64> {
    effects
        .iter()
        .find(|e| e.kind == promo_model::AudioEffectKind::Normalize)
        .map(|e| target_lufs(e.target_lufs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine of `peak_db` dBFS at `hz`, `seconds` long, on every channel.
    fn sine(hz: f64, peak_db: f64, seconds: f64, channels: usize, rate: f64) -> Vec<f32> {
        let amplitude = 10f64.powf(peak_db / 20.0);
        let frames = (seconds * rate) as usize;
        let mut out = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let value =
                (amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / rate).sin()) as f32;
            for _ in 0..channels {
                out.push(value);
            }
        }
        out
    }

    /// EBU Tech 3341's first case: a stereo 1 kHz sine at -23 dBFS reads
    /// -23 LUFS; one channel of it reads 3 dB less. At 44.1 kHz too — the
    /// filters are derived for the rate, not tabulated for 48 kHz.
    #[test]
    fn a_reference_tone_reads_its_level() {
        for rate in [48_000.0, 44_100.0] {
            let stereo = integrated_lufs(&sine(1000.0, -23.0, 10.0, 2, rate), 2, rate).unwrap();
            assert!((stereo + 23.0).abs() < 0.1, "stereo at {rate}: {stereo}");
            let mono = integrated_lufs(&sine(1000.0, -23.0, 10.0, 1, rate), 1, rate).unwrap();
            assert!((mono + 26.01).abs() < 0.1, "mono at {rate}: {mono}");
        }
        assert_eq!(
            integrated_lufs(&vec![0.0; 96_000], 2, 48_000.0),
            None,
            "silence"
        );
    }

    /// Pauses do not count: a voice speaking a third of the time reads
    /// within about a decibel of its speaking level, where the whole
    /// file's RMS — the apps' old estimate — read it 4.8 dB quieter and
    /// planned that much too much gain. The decibel is the standard's own:
    /// 400 ms blocks straddling a word's edge count at 1/4, 1/2 and 3/4
    /// power — 75 blocks in all, summing to 58.5 full ones here.
    #[test]
    fn pauses_are_gated_out() {
        let rate = 48_000.0;
        let spoken = sine(440.0, -20.0, 1.0, 1, rate);
        let pause = vec![0.0f32; 2 * rate as usize];
        let mut narration = Vec::new();
        for _ in 0..6 {
            narration.extend_from_slice(&spoken);
            narration.extend_from_slice(&pause);
        }
        let continuous = integrated_lufs(&sine(440.0, -20.0, 18.0, 1, rate), 1, rate).unwrap();
        let gated = integrated_lufs(&narration, 1, rate).unwrap();
        let edges = 10.0 * (58.5f64 / 75.0).log10();
        assert!(
            (gated - continuous - edges).abs() < 0.02,
            "{gated} vs {continuous}"
        );
        let rms_db = {
            let mean =
                narration.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / narration.len() as f64;
            10.0 * mean.log10()
        };
        let continuous_rms = 10.0 * (10f64.powf(-20.0 / 10.0) / 2.0).log10();
        assert!(
            continuous_rms - rms_db > 4.0,
            "the old estimate was pulled down by the pauses"
        );
    }

    /// Chunks of any size read the same as one piece: the app feeds its
    /// stem in reader-sized buffers, the headless mixer in one.
    #[test]
    fn chunked_reads_match_one_piece() {
        let rate = 44_100.0;
        let mut signal = sine(300.0, -18.0, 3.0, 2, rate);
        signal.extend(vec![0.0f32; 44_100 * 2]);
        signal.extend(sine(2000.0, -30.0, 2.0, 2, rate));
        let whole = integrated_lufs(&signal, 2, rate).unwrap();
        let mut meter = LoudnessMeter::new(2, rate).unwrap();
        for chunk in signal.chunks(2 * 1237) {
            meter.push(chunk);
        }
        assert!((meter.integrated_lufs().unwrap() - whole).abs() < 1e-9);
        assert_eq!(meter.peak(), sample_peak(&signal));
        // Shorter than one block: measured as one block, not silence.
        let blip = sine(1000.0, -23.0, 0.2, 2, rate);
        let short = integrated_lufs(&blip, 2, rate).unwrap();
        assert!((short + 23.0).abs() < 0.3, "{short}");
        assert!(LoudnessMeter::new(0, rate).is_none());
        assert!(LoudnessMeter::new(2, f64::NAN).is_none());
    }

    /// The gain meets the target, stops at the peak ceiling, and stays
    /// within ±24 dB.
    #[test]
    fn the_gain_meets_the_target_under_the_ceiling() {
        let rate = 48_000.0;
        let quiet = sine(1000.0, -23.0, 5.0, 2, rate);
        assert!((normalize_gain_db(&quiet, 2, rate, -16.0) - 7.0).abs() < 0.1);
        // A peaky signal raised to -9 LUFS would pass -1.5 dBFS: capped.
        let peaky = sine(1000.0, -6.0, 5.0, 2, rate);
        let capped = normalize_gain_db(&peaky, 2, rate, -1.0);
        assert!((capped - 4.5).abs() < 0.1, "{capped}");
        let whisper = sine(1000.0, -80.0, 5.0, 2, rate);
        assert!(normalize_gain_db(&whisper, 2, rate, -5.0) <= MAX_GAIN_DB);
        assert_eq!(normalize_gain_db(&[0.0; 1000], 1, rate, -16.0), 0.0);
    }
}
