//! The beat tracker (docs/BEAT-SYNC.md §3): tempo, beats, downbeats,
//! sections and a confidence for one audio file, deterministic to the bit.
//!
//! The method is Ellis 2007 (librosa's `beat_track`): an onset-strength
//! envelope from a log-band spectral flux, the tempo from its weighted
//! autocorrelation, the beats from a dynamic programme that trades onset
//! strength against tempo steadiness. Each beat is then placed on its
//! attack in the waveform itself, and a steady track — generated music, a
//! click — is fitted to one exact arithmetic grid.
//!
//! Determinism: every transcendental is `libm`'s (macOS and glibc differ in
//! the last ulp), the FFT is this file's own radix-2 with `libm` twiddles,
//! reductions run in one fixed order in `f64`, ties go to the lowest index,
//! the host never resamples (this file does, with one fixed kernel), and
//! the output is rounded to 1 µs (times) and 0.001 (bpm).

use std::path::{Path, PathBuf};

use promo_model::musical::{BeatAnalysis, BeatSection, BeatSource};

/// Bumped whenever the analysis of the same samples could change.
pub const TRACKER_VERSION: u32 = 1;

/// The analysis rate: every input is resampled to it.
pub const RATE: f64 = 22_050.0;
const N_FFT: usize = 2048;
const HOP: usize = 512;
const BANDS: usize = 40;
const PHASES: usize = 256;
const TAPS: usize = 64;

/// What the caller knows about the music.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatOptions {
    /// Beats per bar.
    pub meter: u32,
    /// A tempo to prefer, centring the prior instead of 120 BPM.
    pub bpm_hint: Option<f64>,
}

impl Default for BeatOptions {
    fn default() -> Self {
        BeatOptions {
            meter: 4,
            bpm_hint: None,
        }
    }
}

/// Fed native-rate interleaved PCM in chunks of any size, like
/// `loudness::LoudnessMeter`; [`BeatMeter::finish`] analyses it.
pub struct BeatMeter {
    channels: usize,
    rate: f64,
    options: BeatOptions,
    mono: Vec<f32>,
    /// The start of a frame a chunk ended inside.
    carry: Vec<f32>,
}

impl BeatMeter {
    /// `None` for no channels or a rate that is not a positive number.
    pub fn new(channels: usize, sample_rate: f64, options: BeatOptions) -> Option<Self> {
        if channels == 0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
            return None;
        }
        Some(BeatMeter {
            channels,
            rate: sample_rate,
            options,
            mono: Vec::new(),
            carry: Vec::new(),
        })
    }

    /// Downmixes as it goes: the mean of each frame's channels, summed in
    /// channel order — so the chunking never changes a sample.
    pub fn push(&mut self, interleaved: &[f32]) {
        let mut rest = interleaved;
        if !self.carry.is_empty() {
            let need = self.channels - self.carry.len();
            let take = need.min(rest.len());
            self.carry.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.carry.len() < self.channels {
                return;
            }
            let frame = std::mem::take(&mut self.carry);
            self.mono.push(mix(&frame));
        }
        let mut frames = rest.chunks_exact(self.channels);
        for frame in &mut frames {
            self.mono.push(mix(frame));
        }
        self.carry.extend_from_slice(frames.remainder());
    }

    /// The analysis. `source` is left empty — the cache fills it.
    pub fn finish(self) -> BeatAnalysis {
        let duration = self.mono.len() as f64 / self.rate;
        let samples = resample(&self.mono, self.rate);
        analyse(&samples, duration, self.options)
    }
}

fn mix(frame: &[f32]) -> f32 {
    let mut sum = 0.0f64;
    for &s in frame {
        sum += f64::from(s);
    }
    (sum / frame.len() as f64) as f32
}

// ---------------------------------------------------------------- resampling

fn sinc(t: f64) -> f64 {
    if t == 0.0 {
        1.0
    } else {
        let x = std::f64::consts::PI * t;
        libm::sin(x) / x
    }
}

/// To [`RATE`] with one fixed windowed-sinc kernel: 64 taps, Blackman,
/// cut off at the lower Nyquist, at 256 fractional phases, each phase's taps
/// summing to 1.
fn resample(input: &[f32], rate: f64) -> Vec<f64> {
    if rate == RATE {
        return input.iter().map(|&s| f64::from(s)).collect();
    }
    let ratio = rate / RATE;
    let cutoff = (RATE / rate).min(1.0);
    let half = (TAPS / 2) as f64;
    let mut kernel = vec![[0.0f64; TAPS]; PHASES];
    for (q, taps) in kernel.iter_mut().enumerate() {
        let frac = q as f64 / PHASES as f64;
        let mut sum = 0.0;
        for (j, tap) in taps.iter_mut().enumerate() {
            let d = j as f64 - (half - 1.0) - frac;
            let w = if d.abs() < half {
                let a = std::f64::consts::PI * d / half;
                0.42 + 0.5 * libm::cos(a) + 0.08 * libm::cos(2.0 * a)
            } else {
                0.0
            };
            *tap = cutoff * sinc(cutoff * d) * w;
            sum += *tap;
        }
        if sum != 0.0 {
            for tap in taps.iter_mut() {
                *tap /= sum;
            }
        }
    }
    let count = (input.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let x = n as f64 * ratio;
        let mut base = x.floor() as i64;
        let mut q = ((x - x.floor()) * PHASES as f64).round() as usize;
        if q == PHASES {
            q = 0;
            base += 1;
        }
        let first = base - (half as i64 - 1);
        let mut acc = 0.0;
        for (j, tap) in kernel[q].iter().enumerate() {
            let k = first + j as i64;
            if k >= 0 && (k as usize) < input.len() {
                acc += f64::from(input[k as usize]) * tap;
            }
        }
        out.push(acc);
    }
    out
}

// ---------------------------------------------------------------- the FFT

struct Fft {
    cos: Vec<f64>,
    sin: Vec<f64>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let cos = (0..n / 2)
            .map(|k| libm::cos(-2.0 * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        let sin = (0..n / 2)
            .map(|k| libm::sin(-2.0 * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        let reversed = (0..n)
            .map(|i| (i.reverse_bits() >> (usize::BITS - bits)) & (n - 1))
            .collect();
        Fft { cos, sin, reversed }
    }

    /// In place, iterative radix-2, decimation in time.
    fn run(&self, re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        for i in 0..n {
            let j = self.reversed[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= n {
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..size / 2 {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + size / 2);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

// ---------------------------------------------------------------- the analysis

/// What the spectrum says, frame by frame (frame `t` centred on sample
/// `t × HOP`).
struct Spectra {
    /// Log band energies, `BANDS` per frame.
    bands: Vec<[f64; BANDS]>,
    /// The onset envelope: mean positive log-band rise.
    flux: Vec<f64>,
    /// The same for the bass band alone (30–150 Hz).
    bass_flux: Vec<f64>,
    /// Raw energy in the bass band, and in all, per frame.
    bass_energy: Vec<f64>,
    total_energy: Vec<f64>,
    /// Spectral centroid in Hz per frame.
    centroid: Vec<f64>,
}

fn spectra(x: &[f64]) -> Spectra {
    let frames = 1 + x.len() / HOP;
    let fft = Fft::new(N_FFT);
    let window: Vec<f64> = (0..N_FFT)
        .map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64))
        .collect();
    let bins = N_FFT / 2 + 1;
    let freq: Vec<f64> = (0..bins).map(|k| k as f64 * RATE / N_FFT as f64).collect();
    // 40 triangles, log-spaced from 30 Hz to 11 kHz.
    let edges: Vec<f64> = (0..BANDS + 2)
        .map(|i| 30.0 * libm::pow(11_000.0 / 30.0, i as f64 / (BANDS + 1) as f64))
        .collect();
    let weights: Vec<Vec<(usize, f64)>> = (0..BANDS)
        .map(|b| {
            let (lo, mid, hi) = (edges[b], edges[b + 1], edges[b + 2]);
            (0..bins)
                .filter_map(|k| {
                    let f = freq[k];
                    let w = if f > lo && f <= mid {
                        (f - lo) / (mid - lo)
                    } else if f > mid && f < hi {
                        (hi - f) / (hi - mid)
                    } else {
                        0.0
                    };
                    (w > 0.0).then_some((k, w))
                })
                .collect()
        })
        .collect();
    let scale = (N_FFT as f64 / 2.0) * (N_FFT as f64 / 2.0);
    let mut out = Spectra {
        bands: Vec::with_capacity(frames),
        flux: Vec::with_capacity(frames),
        bass_flux: Vec::with_capacity(frames),
        bass_energy: Vec::with_capacity(frames),
        total_energy: Vec::with_capacity(frames),
        centroid: Vec::with_capacity(frames),
    };
    let mut re = vec![0.0; N_FFT];
    let mut im = vec![0.0; N_FFT];
    let mut previous_bass = 0.0;
    for t in 0..frames {
        let first = t as i64 * HOP as i64 - (N_FFT / 2) as i64;
        for i in 0..N_FFT {
            let k = first + i as i64;
            re[i] = if k >= 0 && (k as usize) < x.len() {
                x[k as usize] * window[i]
            } else {
                0.0
            };
            im[i] = 0.0;
        }
        fft.run(&mut re, &mut im);
        let power: Vec<f64> = (0..bins)
            .map(|k| (re[k] * re[k] + im[k] * im[k]) / scale)
            .collect();
        let mut bands = [0.0; BANDS];
        for (b, band) in bands.iter_mut().enumerate() {
            let mut e = 0.0;
            for &(k, w) in &weights[b] {
                e += power[k] * w;
            }
            *band = libm::log(1.0 + 1000.0 * e);
        }
        let (mut bass, mut total, mut moment) = (0.0, 0.0, 0.0);
        for k in 0..bins {
            total += power[k];
            moment += power[k] * freq[k];
            if freq[k] >= 30.0 && freq[k] <= 150.0 {
                bass += power[k];
            }
        }
        let bass_log = libm::log(1.0 + 1000.0 * bass);
        let flux = match out.bands.last() {
            Some(before) => {
                let mut sum = 0.0;
                for b in 0..BANDS {
                    sum += (bands[b] - before[b]).max(0.0);
                }
                sum / BANDS as f64
            }
            None => 0.0,
        };
        out.bass_flux.push(if t == 0 {
            0.0
        } else {
            (bass_log - previous_bass).max(0.0)
        });
        previous_bass = bass_log;
        out.flux.push(flux);
        out.bands.push(bands);
        out.bass_energy.push(bass);
        out.total_energy.push(total);
        out.centroid
            .push(if total > 0.0 { moment / total } else { 0.0 });
    }
    out
}

/// The envelope less its one-second moving mean, over its spread, and
/// floored at 0.
fn envelope(flux: &[f64]) -> Vec<f64> {
    let half = ((RATE / HOP as f64) / 2.0).round() as usize;
    let n = flux.len();
    let mut detrended = Vec::with_capacity(n);
    for t in 0..n {
        let (a, b) = (t.saturating_sub(half), (t + half + 1).min(n));
        let mut sum = 0.0;
        for v in &flux[a..b] {
            sum += v;
        }
        detrended.push(flux[t] - sum / (b - a) as f64);
    }
    let mut square = 0.0;
    for v in &detrended {
        square += v * v;
    }
    let spread = (square / n.max(1) as f64).sqrt();
    detrended
        .into_iter()
        .map(|v| {
            if spread > 0.0 {
                (v / spread).max(0.0)
            } else {
                0.0
            }
        })
        .collect()
}

/// The beat period in frames: the autocorrelation over 0.2–2 s, weighted
/// by a log-normal prior an octave wide around 120 BPM (or the hint), its
/// peak refined by a parabola.
fn tempo(onsets: &[f64], hint: Option<f64>) -> Option<f64> {
    let rate = RATE / HOP as f64;
    let (lo, hi) = ((0.2 * rate).ceil() as usize, (2.0 * rate).floor() as usize);
    let centre = hint.filter(|h| h.is_finite() && *h > 0.0).unwrap_or(120.0);
    let weighted: Vec<f64> = (0..=hi + 1)
        .map(|lag| {
            if lag < lo.saturating_sub(1) || lag == 0 {
                return 0.0;
            }
            let mut sum = 0.0;
            for t in 0..onsets.len().saturating_sub(lag) {
                sum += onsets[t] * onsets[t + lag];
            }
            let bpm = 60.0 * rate / lag as f64;
            let octaves = libm::log2(bpm / centre);
            sum * libm::exp(-0.5 * octaves * octaves)
        })
        .collect();
    let mut best = lo;
    for lag in lo..=hi {
        if weighted[lag] > weighted[best] {
            best = lag;
        }
    }
    if weighted[best] <= 0.0 {
        return None;
    }
    let (a, b, c) = (weighted[best - 1], weighted[best], weighted[best + 1]);
    let denominator = a - 2.0 * b + c;
    let shift = if denominator < 0.0 {
        (0.5 * (a - c) / denominator).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some(best as f64 + shift)
}

/// The beats, as frames: Ellis's dynamic programme on the envelope.
fn track(onsets: &[f64], period: f64) -> Vec<f64> {
    let n = onsets.len();
    let reach = period.round() as i64;
    // The envelope smoothed by a Gaussian of width period/32.
    let local: Vec<f64> = (0..n as i64)
        .map(|t| {
            let mut sum = 0.0;
            for j in -reach..=reach {
                let k = t + j;
                if k >= 0 && (k as usize) < n {
                    let z = j as f64 * 32.0 / period;
                    sum += onsets[k as usize] * libm::exp(-0.5 * z * z);
                }
            }
            sum
        })
        .collect();
    let (near, far) = (
        (period / 2.0).round() as usize,
        (2.0 * period).round() as usize,
    );
    let mut score = vec![0.0; n];
    let mut back: Vec<Option<usize>> = vec![None; n];
    for t in 0..n {
        let mut best: Option<(f64, usize)> = None;
        if t >= near {
            let first = t.saturating_sub(far);
            for (tau, &prior) in score.iter().enumerate().take(t - near + 1).skip(first) {
                let gap = libm::log((t - tau) as f64 / period);
                let candidate = prior - 100.0 * gap * gap;
                if best.is_none_or(|(b, _)| candidate > b) {
                    best = Some((candidate, tau));
                }
            }
        }
        match best {
            Some((value, tau)) => {
                score[t] = local[t] + value;
                back[t] = Some(tau);
            }
            None => score[t] = local[t],
        }
    }
    // The last strong peak of the score.
    let peaks: Vec<usize> = (1..n.saturating_sub(1))
        .filter(|&t| score[t] > score[t - 1] && score[t] >= score[t + 1])
        .collect();
    if peaks.is_empty() {
        return Vec::new();
    }
    let mut values: Vec<f64> = peaks.iter().map(|&t| score[t]).collect();
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let Some(&last) = peaks.iter().rev().find(|&&t| score[t] >= 0.5 * median) else {
        return Vec::new();
    };
    let mut beats = vec![last];
    let mut at = last;
    while let Some(tau) = back[at] {
        beats.push(tau);
        at = tau;
    }
    beats.reverse();
    // Weak beats at either edge are not the music's: a beat's strength is
    // the smoothed score's peak within two frames of it, and weak is under
    // half the median beat's — a median, so accented downbeats do not make
    // every plain beat look weak.
    let strength = |b: usize| {
        let (a, z) = (b.saturating_sub(2), (b + 2).min(n - 1));
        local[a..=z].iter().copied().fold(0.0, f64::max)
    };
    let mut strengths: Vec<f64> = beats.iter().map(|&b| strength(b)).collect();
    strengths.sort_by(f64::total_cmp);
    let floor = 0.5 * strengths[strengths.len() / 2];
    while beats.first().is_some_and(|&b| strength(b) < floor) {
        beats.remove(0);
    }
    while beats.last().is_some_and(|&b| strength(b) < floor) {
        beats.pop();
    }
    // To a fraction of a frame: the parabola through the envelope's peak
    // within two frames.
    beats
        .into_iter()
        .map(|b| {
            let (a, z) = (b.saturating_sub(2), (b + 2).min(n - 1));
            let mut peak = b;
            for t in a..=z {
                if onsets[t] > onsets[peak] {
                    peak = t;
                }
            }
            if peak == 0 || peak + 1 >= n {
                return peak as f64;
            }
            let (l, c, r) = (onsets[peak - 1], onsets[peak], onsets[peak + 1]);
            let d = l - 2.0 * c + r;
            if d < 0.0 {
                peak as f64 + (0.5 * (l - r) / d).clamp(-0.5, 0.5)
            } else {
                peak as f64
            }
        })
        .collect()
}

/// Moves each beat onto its attack in the waveform: within 60 ms, the
/// sample where the energy of the next 5 ms most outweighs the 20 ms
/// before it. A frame of the spectrum is 23 ms and a band's rise starts
/// before its centre; the waveform knows to the sample.
fn attacks(x: &[f64], beats: &[f64]) -> Vec<f64> {
    let mut prefix = Vec::with_capacity(x.len() + 1);
    let mut sum = 0.0;
    prefix.push(0.0);
    for &s in x {
        sum += s * s;
        prefix.push(sum);
    }
    let energy = |a: usize, b: usize| prefix[b] - prefix[a];
    let (after, before) = ((0.005 * RATE) as usize, (0.020 * RATE) as usize);
    let floor = 1e-9 * (sum / x.len().max(1) as f64) * before as f64 + 1e-12;
    let reach = (0.060 * RATE) as i64;
    beats
        .iter()
        .map(|&t| {
            let centre = (t * RATE).round() as i64;
            let mut best: Option<(f64, usize)> = None;
            for n in (centre - reach)..=(centre + reach) {
                if n < before as i64 || n as usize + after > x.len() {
                    continue;
                }
                let n = n as usize;
                let ratio = energy(n, n + after) / (energy(n - before, n) + floor);
                if best.is_none_or(|(b, _)| ratio > b) {
                    best = Some((ratio, n));
                }
            }
            best.map_or(t, |(_, n)| n as f64 / RATE)
        })
        .collect()
}

/// One steady tempo, when the beats keep one: at least eight, every gap
/// within 4% of the median, and a least-squares line through them that
/// misses none by more than 10 ms RMS. Returns the line's start and period.
fn uniform_fit(beats: &[f64]) -> Option<(f64, f64)> {
    let n = beats.len();
    if n < 8 {
        return None;
    }
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(f64::total_cmp);
    let median = gaps[gaps.len() / 2];
    if beats
        .windows(2)
        .any(|w| ((w[1] - w[0]) - median).abs() > 0.04 * median)
    {
        return None;
    }
    let k_mean = (n - 1) as f64 / 2.0;
    let mut t_sum = 0.0;
    for &t in beats {
        t_sum += t;
    }
    let t_mean = t_sum / n as f64;
    let (mut num, mut den) = (0.0, 0.0);
    for (k, &t) in beats.iter().enumerate() {
        let dk = k as f64 - k_mean;
        num += dk * (t - t_mean);
        den += dk * dk;
    }
    let period = num / den;
    let start = t_mean - period * k_mean;
    let mut square = 0.0;
    for (k, &t) in beats.iter().enumerate() {
        let r = t - (start + k as f64 * period);
        square += r * r;
    }
    ((square / n as f64).sqrt() < 0.010 && period > 0.0).then_some((start, period))
}

fn round_us(t: f64) -> f64 {
    (t * 1e6).round() / 1e6
}

fn frame_of(t: f64) -> usize {
    (t * RATE / HOP as f64).round().max(0.0) as usize
}

/// The analysis of mono samples at [`RATE`] lasting `duration` seconds.
pub fn analyse(x: &[f64], duration: f64, options: BeatOptions) -> BeatAnalysis {
    let meter = options.meter.max(1);
    let empty = BeatAnalysis {
        tracker_version: TRACKER_VERSION,
        source: None,
        duration: round_us(duration),
        bpm: 0.0,
        offset: 0.0,
        time_signature: [meter, 4],
        grid: "tracked".into(),
        confidence: 0.0,
        beats: Vec::new(),
        downbeats: Vec::new(),
        sections: Vec::new(),
    };
    let spectra = spectra(x);
    let onsets = envelope(&spectra.flux);
    let Some(period) = tempo(&onsets, options.bpm_hint) else {
        return empty;
    };
    let frames_per_second = RATE / HOP as f64;
    let found: Vec<f64> = track(&onsets, period)
        .into_iter()
        .map(|f| f / frames_per_second)
        .collect();
    if found.len() < 2 {
        return empty;
    }
    let placed = attacks(x, &found);
    let (beats, grid, beat_period) = match uniform_fit(&placed) {
        Some((start, step)) => (
            (0..placed.len())
                .map(|k| start + k as f64 * step)
                .collect::<Vec<f64>>(),
            "uniform",
            step,
        ),
        None => {
            let mut gaps: Vec<f64> = placed.windows(2).map(|w| w[1] - w[0]).collect();
            gaps.sort_by(f64::total_cmp);
            (placed.clone(), "tracked", gaps[gaps.len() / 2])
        }
    };

    // Downbeats: the phase whose beats carry the most bass onset, plus half
    // the spectral change across them. Ties go to the lowest phase.
    let bass: Vec<f64> = beats
        .iter()
        .map(|&t| {
            let f = frame_of(t);
            let (a, b) = (
                f.saturating_sub(2),
                (f + 2).min(spectra.bass_flux.len() - 1),
            );
            spectra.bass_flux[a..=b].iter().copied().fold(0.0, f64::max)
        })
        .collect();
    let means: Vec<[f64; BANDS]> = (0..beats.len())
        .map(|k| {
            let a = frame_of(beats[k]).min(spectra.bands.len() - 1);
            let b = beats
                .get(k + 1)
                .map_or(spectra.bands.len(), |&t| {
                    frame_of(t).min(spectra.bands.len())
                })
                .max(a + 1);
            let mut mean = [0.0; BANDS];
            for frame in &spectra.bands[a..b] {
                for (m, v) in mean.iter_mut().zip(frame) {
                    *m += v;
                }
            }
            for m in &mut mean {
                *m /= (b - a) as f64;
            }
            mean
        })
        .collect();
    let change: Vec<f64> = (0..beats.len())
        .map(|k| {
            if k == 0 {
                return 0.0;
            }
            let (p, q) = (&means[k - 1], &means[k]);
            let (mut dot, mut pp, mut qq) = (0.0, 0.0, 0.0);
            for b in 0..BANDS {
                dot += p[b] * q[b];
                pp += p[b] * p[b];
                qq += q[b] * q[b];
            }
            if pp > 0.0 && qq > 0.0 {
                1.0 - dot / (pp.sqrt() * qq.sqrt())
            } else {
                0.0
            }
        })
        .collect();
    let m = meter as usize;
    let mut phase = 0;
    let mut best = f64::NEG_INFINITY;
    for p in 0..m.min(beats.len()) {
        let (mut sum, mut count) = (0.0, 0.0);
        for k in (p..beats.len()).step_by(m) {
            sum += bass[k] + 0.5 * change[k];
            count += 1.0;
        }
        let value = sum / count;
        if value > best {
            best = value;
            phase = p;
        }
    }
    let downbeats: Vec<f64> = beats.iter().skip(phase).step_by(m).copied().collect();

    let sections = sections(x, &spectra, &downbeats, beat_period * m as f64, duration);

    // How much more onset sits on the beats than between them.
    let at = |t: f64| {
        let f = frame_of(t);
        let (a, b) = (f.saturating_sub(1), (f + 1).min(onsets.len() - 1));
        onsets[a..=b].iter().copied().fold(0.0, f64::max)
    };
    let (mut on, mut off) = (0.0, 0.0);
    for w in beats.windows(2) {
        on += at(w[0]);
        off += at((w[0] + w[1]) / 2.0);
    }
    let ratio = if on + off > 0.0 { on / (on + off) } else { 0.5 };
    let confidence = ((ratio - 0.5) * 2.0).clamp(0.0, 1.0);

    BeatAnalysis {
        tracker_version: TRACKER_VERSION,
        source: None,
        duration: round_us(duration),
        bpm: (60.0 / beat_period * 1000.0).round() / 1000.0,
        offset: round_us(downbeats.first().copied().unwrap_or(0.0)),
        time_signature: [meter, 4],
        grid: grid.into(),
        confidence: (confidence * 1000.0).round() / 1000.0,
        beats: beats.iter().map(|&t| round_us(t)).collect(),
        downbeats: downbeats.iter().map(|&t| round_us(t)).collect(),
        sections,
    }
}

/// Bars grouped where the music changes: per bar its loudness, its share
/// of bass and its spectral centroid, standardised; a boundary where the
/// four bars after differ most from the four before (a novelty peak above
/// its mean plus one deviation, at least four bars from the next).
fn sections(
    x: &[f64],
    spectra: &Spectra,
    downbeats: &[f64],
    bar: f64,
    duration: f64,
) -> Vec<BeatSection> {
    let count = downbeats.len();
    if count == 0 {
        return Vec::new();
    }
    let end_of = |i: usize| {
        downbeats
            .get(i + 1)
            .copied()
            .unwrap_or((downbeats[i] + bar).min(duration))
    };
    let mut features: Vec<[f64; 3]> = Vec::with_capacity(count);
    let mut loudness = Vec::with_capacity(count);
    for (i, &a) in downbeats.iter().enumerate() {
        let b = end_of(i);
        let (sa, sb) = (
            ((a * RATE) as usize).min(x.len()),
            ((b * RATE) as usize).min(x.len()),
        );
        let mut square = 0.0;
        for &s in &x[sa..sb] {
            square += s * s;
        }
        let db = 10.0 * libm::log10(square / (sb - sa).max(1) as f64 + 1e-12);
        let (fa, fb) = (
            frame_of(a).min(spectra.total_energy.len()),
            frame_of(b).min(spectra.total_energy.len()),
        );
        let (mut bass, mut total, mut centroid) = (0.0, 0.0, 0.0);
        for f in fa..fb {
            bass += spectra.bass_energy[f];
            total += spectra.total_energy[f];
            centroid += spectra.centroid[f];
        }
        let frames = (fb - fa).max(1) as f64;
        features.push([
            db,
            if total > 0.0 { bass / total } else { 0.0 },
            centroid / frames,
        ]);
        loudness.push(db);
    }
    for f in 0..3 {
        let mut sum = 0.0;
        for row in &features {
            sum += row[f];
        }
        let mean = sum / count as f64;
        let mut square = 0.0;
        for row in &features {
            square += (row[f] - mean) * (row[f] - mean);
        }
        // Standardised over a spread no smaller than a change a listener
        // hears — 1 dB, 5% of the energy in the bass, 100 Hz of centroid —
        // so a feature that barely moves does not turn its noise into
        // change.
        let spread = (square / count as f64).sqrt().max([1.0, 0.05, 100.0][f]);
        for row in &mut features {
            row[f] = (row[f] - mean) / spread;
        }
    }
    let mean_of = |a: usize, b: usize| {
        let mut out = [0.0; 3];
        for row in &features[a..b] {
            for f in 0..3 {
                out[f] += row[f];
            }
        }
        out.map(|v| v / (b - a) as f64)
    };
    let novelty: Vec<f64> = (0..count)
        .map(|i| {
            if i == 0 {
                return 0.0;
            }
            let (p, q) = (
                mean_of(i.saturating_sub(4), i),
                mean_of(i, (i + 4).min(count)),
            );
            let mut square = 0.0;
            for f in 0..3 {
                square += (p[f] - q[f]) * (p[f] - q[f]);
            }
            square.sqrt()
        })
        .collect();
    let mut sum = 0.0;
    for v in &novelty[1..] {
        sum += v;
    }
    let inner = (count - 1).max(1) as f64;
    let mean = sum / inner;
    let mut square = 0.0;
    for v in &novelty[1..] {
        square += (v - mean) * (v - mean);
    }
    let threshold = mean + (square / inner).sqrt();
    let mut candidates: Vec<usize> = (1..count)
        .filter(|&i| {
            novelty[i] > threshold
                && novelty[i] >= novelty[i - 1]
                && (i + 1 >= count || novelty[i] > novelty[i + 1])
        })
        .collect();
    candidates.sort_by(|&a, &b| novelty[b].total_cmp(&novelty[a]).then(a.cmp(&b)));
    let mut cuts: Vec<usize> = Vec::new();
    // At least four bars apart — the ends counting as boundaries too, so a
    // song's last, fading bar is not a section of its own.
    for c in candidates {
        if c >= 4 && count - c >= 4 && cuts.iter().all(|&k| k.abs_diff(c) >= 4) {
            cuts.push(c);
        }
    }
    cuts.sort_unstable();
    let mut bounds = vec![0];
    bounds.extend(cuts);
    bounds.push(count);
    let energy: Vec<f64> = bounds
        .windows(2)
        .map(|w| {
            let mut sum = 0.0;
            for v in &loudness[w[0]..w[1]] {
                sum += v;
            }
            sum / (w[1] - w[0]) as f64
        })
        .collect();
    let (lo, hi) = energy
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &e| {
            (lo.min(e), hi.max(e))
        });
    let level: Vec<f64> = energy
        .iter()
        .map(|&e| {
            if hi - lo > 1e-9 {
                (e - lo) / (hi - lo)
            } else {
                1.0
            }
        })
        .collect();
    let drop = (1..level.len())
        .filter(|&i| level[i] > level[i - 1])
        .max_by(|&a, &b| {
            (level[a] - level[a - 1])
                .total_cmp(&(level[b] - level[b - 1]))
                .then(b.cmp(&a))
        });
    let last = level.len() - 1;
    bounds
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let label = if Some(i) == drop {
                "drop"
            } else if i == 0 && last > 0 && level[i] < 0.5 {
                "intro"
            } else if i == last && i > 0 && level[i] < 0.5 {
                "outro"
            } else {
                "section"
            };
            BeatSection {
                index: i as u32,
                label: label.into(),
                start: round_us(downbeats[w[0]]),
                end: round_us(end_of(w[1] - 1)),
                start_bar: w[0] as u32 + 1,
                bars: (w[1] - w[0]) as u32,
                energy: (level[i] * 1000.0).round() / 1000.0,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- files

/// What went wrong reading or caching an analysis, in words.
pub type BeatError = String;

/// Interleaved PCM, its channels and rate.
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: f64,
}

/// A WAV file's PCM (16/24/32-bit integer or 32-bit float), read without
/// ffmpeg. `None` when it is not a WAV this reads.
pub fn read_wav(bytes: &[u8]) -> Option<Pcm> {
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut format: Option<(u16, usize, f64, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            let tag = u16_at(body)?;
            let channels = u16_at(body + 2)? as usize;
            let rate = f64::from(u32_at(body + 4)?);
            let bits = u16_at(body + 14)?;
            // WAVE_FORMAT_EXTENSIBLE carries the real tag in its sub-format.
            let tag = if tag == 0xFFFE {
                u16_at(body + 24)?
            } else {
                tag
            };
            format = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = format?;
            let data = bytes.get(body..(body + size).min(bytes.len()))?;
            let samples: Vec<f32> = match (tag, bits) {
                (1, 16) => data
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect(),
                (1, 24) => data
                    .chunks_exact(3)
                    .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0)
                    .collect(),
                (1, 32) => data
                    .chunks_exact(4)
                    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
                    .collect(),
                (3, 32) => data
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
                _ => return None,
            };
            if channels == 0 {
                return None;
            }
            return Some(Pcm {
                samples,
                channels,
                rate,
            });
        }
        at = body + size + (size & 1);
    }
    None
}

/// The file's PCM at its own rate and channels: a WAV directly, anything
/// else through ffmpeg (f32le, no resampling — the tracker resamples).
pub fn read_pcm(path: &Path) -> Result<Pcm, BeatError> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(pcm) = read_wav(&bytes) {
        return Ok(pcm);
    }
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels",
            "-of",
            "csv=p=0",
        ])
        .arg(crate::tool_path(path))
        .output()
        .map_err(|e| format!("ffprobe is needed to read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let mut fields = text.trim().split(',');
    let rate: f64 = fields
        .next()
        .and_then(|f| f.trim().parse().ok())
        .ok_or_else(|| format!("{}: no audio stream", path.display()))?;
    let channels: usize = fields
        .next()
        .and_then(|f| f.trim().parse().ok())
        .ok_or_else(|| format!("{}: no audio stream", path.display()))?;
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(crate::tool_path(path))
        .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .map_err(|e| format!("ffmpeg is needed to read {}: {e}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{}: ffmpeg could not read its audio",
            path.display()
        ));
    }
    let samples = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(Pcm {
        samples,
        channels,
        rate,
    })
}

/// The analysis of one audio file, its `source` filled in.
pub fn analyse_file(path: &Path, options: BeatOptions) -> Result<BeatAnalysis, BeatError> {
    let pcm = read_pcm(path)?;
    let mut meter = BeatMeter::new(pcm.channels, pcm.rate, options)
        .ok_or_else(|| format!("{}: no samples to analyse", path.display()))?;
    meter.push(&pcm.samples);
    let mut analysis = meter.finish();
    analysis.source = Some(source_of(path)?);
    Ok(analysis)
}

/// The file's name, the first 32 hex digits of the SHA-256 of its bytes,
/// and its size.
pub fn source_of(path: &Path) -> Result<BeatSource, BeatError> {
    use sha2::Digest;
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest = sha2::Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(BeatSource {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sha256: hex[..32].to_string(),
        bytes: bytes.len() as u64,
    })
}

/// Where an analysis of content `key` lives in a project:
/// `Resources/.beats/<key>.json`.
pub fn cache_path(project: &Path, key: &str) -> PathBuf {
    project
        .join("Resources")
        .join(".beats")
        .join(format!("{key}.json"))
}

/// The project's cached analysis of `audio`, by content, when there is one.
pub fn load_cached(project: &Path, audio: &Path) -> Option<BeatAnalysis> {
    let key = source_of(audio).ok()?.sha256;
    let text = std::fs::read_to_string(cache_path(project, &key)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `analysis` (of `audio`) into the project's cache, `source` filled
/// in, and returns what was written.
pub fn store(
    project: &Path,
    audio: &Path,
    mut analysis: BeatAnalysis,
) -> Result<BeatAnalysis, BeatError> {
    let source = source_of(audio)?;
    let path = cache_path(project, &source.sha256);
    analysis.source = Some(source);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&analysis).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(analysis)
}

/// The project's analysis of `audio`: the cache when it holds one — it is
/// authoritative, so an upgraded tracker never re-times an edit — else a
/// fresh one, stored. `refresh` analyses again and replaces the cache.
/// The flag says whether it came from the cache.
pub fn analysis_for(
    project: &Path,
    audio: &Path,
    options: BeatOptions,
    refresh: bool,
) -> Result<(BeatAnalysis, bool), BeatError> {
    if !refresh {
        if let Some(cached) = load_cached(project, audio) {
            return Ok((cached, true));
        }
    }
    let fresh = analyse_file(audio, options)?;
    Ok((store(project, audio, fresh)?, false))
}
