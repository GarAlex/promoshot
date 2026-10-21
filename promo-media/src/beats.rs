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

use std::path::Path;

use promo_model::musical::{BeatAnalysis, BeatSection};

/// Bumped whenever the analysis of the same samples could change (it
/// lives with the cache, which re-measures an older one's answers).
/// 2: tempo from the autocorrelation's first four multiples, one steady
/// grid fitted to the whole envelope, a confidence against the off-beat
/// sixteenths, timbre in the sections. 3: confidence from the beats alone
/// (alignment against every phase, times steadiness), a jittery free track
/// yields to a grid that explains half as much.
pub use promo_model::musical::TRACKER_VERSION;

/// The analysis rate: every input is resampled to it.
pub const RATE: f64 = 22_050.0;
const N_FFT: usize = 2048;
const HOP: usize = 512;
const BANDS: usize = 40;

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
            self.mono.push(crate::dsp::mix(&frame));
        }
        let mut frames = rest.chunks_exact(self.channels);
        for frame in &mut frames {
            self.mono.push(crate::dsp::mix(frame));
        }
        self.carry.extend_from_slice(frames.remainder());
    }

    /// The analysis. `source` is left empty — the cache fills it.
    pub fn finish(self) -> BeatAnalysis {
        let duration = self.mono.len() as f64 / self.rate;
        let samples = crate::dsp::resample(&self.mono, self.rate, RATE);
        analyse(&samples, duration, self.options)
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
    let fft = crate::dsp::Fft::new(N_FFT);
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

/// The envelope's autocorrelation at every lag up to `max_lag` frames,
/// each a mean over the overlap so long lags are not penalised for being
/// long.
fn autocorrelation(onsets: &[f64], max_lag: usize) -> Vec<f64> {
    let n = onsets.len();
    (0..=max_lag)
        .map(|lag| {
            if lag >= n {
                return 0.0;
            }
            let mut sum = 0.0;
            for t in 0..n - lag {
                sum += onsets[t] * onsets[t + lag];
            }
            sum / (n - lag) as f64
        })
        .collect()
}

/// How strongly a tempo is preferred before the music is heard: a
/// log-normal an octave wide around 120 BPM — or, with a hint, a narrow
/// window (±5%) around it, its half and its double, the hint itself first.
fn prior(bpm: f64, hint: Option<f64>) -> f64 {
    match hint {
        None => {
            let octaves = libm::log2(bpm / 120.0);
            libm::exp(-0.5 * octaves * octaves)
        }
        Some(h) => [(h, 1.0), (h / 2.0, 0.5), (h * 2.0, 0.5)]
            .iter()
            .map(|&(centre, weight)| {
                let off = libm::log2(bpm / centre) / 0.07;
                weight * libm::exp(-0.5 * off * off)
            })
            .fold(0.0, f64::max),
    }
}

/// The beat period in frames. Each candidate period scores the
/// autocorrelation at its first four multiples — a beat is also a half bar
/// and a bar, so the strong bar-length periodicity of a loop votes for the
/// beat inside it instead of drowning it — weighted by [`prior`]; the
/// winner is then refined on its fourth multiple, where a frame of error is
/// a quarter of a frame of period.
fn tempo(onsets: &[f64], hint: Option<f64>) -> Option<f64> {
    let rate = RATE / HOP as f64;
    let hint = hint.filter(|h| h.is_finite() && *h >= 20.0 && *h <= 400.0);
    let (lo, hi) = (0.25 * rate, 1.5 * rate);
    let max_lag = (4.0 * hi).ceil() as usize + 2;
    let acf = autocorrelation(onsets, max_lag);
    let at = |lag: f64| {
        let i = lag.floor() as usize;
        if i + 1 >= acf.len() {
            return 0.0;
        }
        let f = lag - i as f64;
        acf[i] * (1.0 - f) + acf[i + 1] * f
    };
    let mut best: Option<(f64, f64)> = None;
    let steps = ((hi - lo) / 0.05).floor() as usize;
    for step in 0..=steps {
        let period = lo + step as f64 * 0.05;
        let mut score = 0.0;
        for k in [1.0, 2.0, 4.0] {
            score += at(period * k);
        }
        let value = score * prior(60.0 * rate / period, hint);
        if best.is_none_or(|(_, b)| value > b) {
            best = Some((period, value));
        }
    }
    let (period, value) = best?;
    if value <= 0.0 {
        return None;
    }
    // The fourth multiple's own peak, within a frame of where the comb put
    // it, through a parabola.
    let target = 4.0 * period;
    let (a, z) = (
        (target - 1.5).floor().max(1.0) as usize,
        (target + 1.5).ceil() as usize,
    );
    let mut peak = target.round() as usize;
    for lag in a..=z.min(acf.len() - 2) {
        if acf[lag] > acf[peak.min(acf.len() - 2)] {
            peak = lag;
        }
    }
    if peak == 0 || peak + 1 >= acf.len() {
        return Some(period);
    }
    let (l, c, r) = (acf[peak - 1], acf[peak], acf[peak + 1]);
    let d = l - 2.0 * c + r;
    let shift = if d < 0.0 {
        (0.5 * (l - r) / d).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let refined = (peak as f64 + shift) / 4.0;
    // A refinement that wandered off the comb's choice is not one.
    Some(if (refined - period).abs() <= 0.5 {
        refined
    } else {
        period
    })
}

/// The envelope at a time in seconds, between frames linearly.
fn onset_at(onsets: &[f64], t: f64) -> f64 {
    let f = t * RATE / HOP as f64;
    if f < 0.0 {
        return 0.0;
    }
    let i = f.floor() as usize;
    if i + 1 >= onsets.len() {
        return 0.0;
    }
    let w = f - i as f64;
    onsets[i] * (1.0 - w) + onsets[i + 1] * w
}

/// The mean envelope on a grid of `period` from `phase` across [from, to).
fn grid_score(onsets: &[f64], period: f64, phase: f64, from: f64, to: f64) -> f64 {
    let first = ((from - phase) / period).ceil();
    let (mut sum, mut count) = (0.0, 0.0f64);
    let mut k = first;
    loop {
        let t = phase + k * period;
        if t >= to {
            break;
        }
        sum += onset_at(onsets, t);
        count += 1.0;
        k += 1.0;
    }
    sum / count.max(1.0)
}

/// The one steady grid that best fits the whole envelope: the period
/// within 1% of the tempo's and the phase that put the most onset on its
/// beats, searched coarse then fine. Returns (period, phase, score).
fn fit_grid(onsets: &[f64], period: f64, from: f64, to: f64) -> (f64, f64, f64) {
    let frame = HOP as f64 / RATE;
    // Judged on the envelope smoothed over ±2 frames, so the grid that
    // sits near every hit beats the one that lands exactly on a few loud
    // ones — over a short excerpt those few would choose the period.
    let smooth: Vec<f64> = (0..onsets.len())
        .map(|t| {
            let mut sum = 0.0;
            for (j, w) in [(-2i64, 0.25), (-1, 0.75), (0, 1.0), (1, 0.75), (2, 0.25)] {
                let k = t as i64 + j;
                if k >= 0 && (k as usize) < onsets.len() {
                    sum += w * onsets[k as usize];
                }
            }
            sum / 3.0
        })
        .collect();
    let onsets = &smooth[..];
    let search = |centre: f64, span: f64, steps: usize, phase_step: f64| {
        let mut best = (centre, 0.0, f64::NEG_INFINITY);
        for i in 0..=steps {
            let p = centre - span + 2.0 * span * i as f64 / steps as f64;
            let phases = (p / phase_step).floor() as usize;
            for j in 0..phases {
                let phase = j as f64 * phase_step;
                let score = grid_score(onsets, p, phase, from, to);
                if score > best.2 {
                    best = (p, phase, score);
                }
            }
        }
        best
    };
    let coarse = search(period, 0.01 * period, 80, frame / 2.0);
    let fine = search(coarse.0, 0.0006 * period, 24, frame / 8.0);
    if fine.2 >= coarse.2 {
        fine
    } else {
        coarse
    }
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

/// How much a beat list's gaps jump from one beat to the next, as a share
/// of the median gap: 0 for one steady grid, a hundredth or so for a player
/// gliding between tempos, a tenth and more for a track chasing hats.
fn jitter(beats: &[f64]) -> f64 {
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.len() < 2 {
        return 0.0;
    }
    let mut sorted = gaps.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    if median <= 0.0 {
        return 0.0;
    }
    let mut sum = 0.0;
    for w in gaps.windows(2) {
        sum += (w[1] - w[0]).abs() / median;
    }
    sum / (gaps.len() - 1) as f64
}

/// How sure the beats are, from the beats and the music alone — never the
/// hint that steered the search. Three parts, multiplied:
/// - alignment: how much more onset sits on the beats than on the same grid
///   at every other phase of the beat (an aligned pulse: twice the average
///   or more; a misplaced one: the average);
/// - steadiness: a list jumping by a tenth of a beat gap to gap is a track
///   chasing the hats, not a pulse;
/// - support: how strongly the music repeats at the beats' tempo (beat,
///   half bar, bar) against the strongest periodicity it has at any tempo —
///   a tempo forced by a wrong hint can line its sparse beats up with loud
///   hits, but the song does not repeat there (right tempos measured
///   0.83–0.97 of the best on the fixtures and real loops; forced wrong
///   ones 0.66–0.71).
fn confidence_of(onsets: &[f64], beats: &[f64]) -> f64 {
    if beats.len() < 3 {
        return 0.0;
    }
    let gaps: Vec<f64> = {
        let mut g: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        g.sort_by(f64::total_cmp);
        g
    };
    let rate = RATE / HOP as f64;
    let period = gaps[gaps.len() / 2] * rate;
    let acf = autocorrelation(onsets, (4.0 * 1.5 * rate).ceil() as usize + 2);
    let lag_at = |lag: f64| {
        let i = lag.floor() as usize;
        if i + 1 >= acf.len() {
            return 0.0;
        }
        let f = lag - i as f64;
        acf[i] * (1.0 - f) + acf[i + 1] * f
    };
    let comb = |p: f64| lag_at(p) + lag_at(2.0 * p) + lag_at(4.0 * p);
    let mut strongest: f64 = 0.0;
    let mut p = 0.25 * rate;
    while p <= 1.5 * rate {
        strongest = strongest.max(comb(p));
        p += 0.05;
    }
    let mut here: f64 = 0.0;
    let mut q = period - 1.0;
    while q <= period + 1.0 {
        here = here.max(comb(q));
        q += 0.05;
    }
    let support = if strongest > 0.0 {
        ((here / strongest - 0.7) / 0.15).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let at = |t: f64| {
        let f = frame_of(t);
        let (a, b) = (f.saturating_sub(1), (f + 1).min(onsets.len() - 1));
        onsets[a..=b].iter().copied().fold(0.0, f64::max)
    };
    let (mut on, mut every) = (0.0, 0.0);
    for w in beats.windows(2) {
        let gap = w[1] - w[0];
        on += at(w[0]);
        for k in 0..16 {
            every += at(w[0] + gap * k as f64 / 16.0) / 16.0;
        }
    }
    if every <= 0.0 {
        return 0.0;
    }
    let alignment = ((on / every - 1.0) / 0.8).clamp(0.0, 1.0);
    let steadiness = (1.0 - jitter(beats) / 0.12).clamp(0.0, 1.0);
    alignment * steadiness * support
}

/// A grid moved onto the music's attacks: each grid beat's attack within
/// 60 ms (see [`attacks`]), the ones within 30 ms of it kept. When they lie
/// on one line to within 4 ms RMS — a click, a drum machine — the
/// least-squares line through them is the grid, to the sample; otherwise
/// (attacks smeared by a busy mix) the fitted period stands and only the
/// phase moves, by the attacks' median lead or lag. `None` when fewer than
/// eight (or than half) of the beats found an attack.
fn attack_line(beats: &[f64], placed: &[f64], step: f64) -> Option<(f64, f64)> {
    let kept: Vec<(f64, f64)> = beats
        .iter()
        .zip(placed)
        .enumerate()
        .filter(|(_, (g, a))| (*a - *g).abs() <= 0.030)
        .map(|(k, (_, a))| (k as f64, *a))
        .collect();
    if kept.len() < 8 || 2 * kept.len() < beats.len() {
        return None;
    }
    let n = kept.len() as f64;
    let (mut ks, mut ts) = (0.0, 0.0);
    for &(k, t) in &kept {
        ks += k;
        ts += t;
    }
    let (k_mean, t_mean) = (ks / n, ts / n);
    let (mut num, mut den) = (0.0, 0.0);
    for &(k, t) in &kept {
        num += (k - k_mean) * (t - t_mean);
        den += (k - k_mean) * (k - k_mean);
    }
    if den <= 0.0 {
        return None;
    }
    let period = num / den;
    if period <= 0.0 {
        return None;
    }
    let start = t_mean - period * k_mean;
    let mut square = 0.0;
    for &(k, t) in &kept {
        let r = t - (start + k * period);
        square += r * r;
    }
    if (square / n).sqrt() < 0.004 {
        return Some((start, period));
    }
    let mut leads: Vec<f64> = kept
        .iter()
        .map(|&(k, t)| t - (beats[0] + k * step))
        .collect();
    leads.sort_by(f64::total_cmp);
    Some((beats[0] + leads[leads.len() / 2], step))
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
    let hint = options
        .bpm_hint
        .filter(|h| h.is_finite() && *h >= 20.0 && *h <= 400.0);
    let empty = BeatAnalysis {
        tracker_version: TRACKER_VERSION,
        source: None,
        bpm_hint: hint,
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
    // The music's span: where the dynamic programme heard beats.
    let (from, to) = (
        found[0] - 0.25 * period / frames_per_second,
        found[found.len() - 1] + 0.25 * period / frames_per_second,
    );
    let tracked = attacks(x, &found);

    // One steady grid, fitted to the whole envelope at once — a generated
    // loop keeps one exactly, and a beat-by-beat track wanders after its
    // hi-hats and arpeggios. It stands when it explains the onsets nearly
    // as well as the free track does (above the envelope's own mean level);
    // music whose tempo truly moves keeps its tracked beats.
    let (step, phase, _) = fit_grid(&onsets, period / frames_per_second, from, to);
    let first = ((from - phase) / step).ceil();
    let grid: Vec<f64> = (0..)
        .map(|k| phase + (first + k as f64) * step)
        .take_while(|&t| t < to)
        .collect();
    let mut level = 0.0;
    for v in &onsets {
        level += v;
    }
    let level = level / onsets.len().max(1) as f64;
    // Both judged alike: each beat by the strongest onset within two
    // frames of it — a free track sits on its peaks by construction, and a
    // grid a frame off a peak is still on the beat.
    let near = |t: f64| {
        let f = frame_of(t);
        let (a, b) = (f.saturating_sub(2), (f + 2).min(onsets.len() - 1));
        onsets[a..=b].iter().copied().fold(0.0, f64::max)
    };
    let mean_near = |beats: &[f64]| {
        let mut sum = 0.0;
        for &t in beats {
            sum += near(t);
        }
        sum / beats.len().max(1) as f64
    };
    let tracked_score = mean_near(&tracked);
    let uniform_score = mean_near(&grid);
    // A free track whose gaps jump about from beat to beat is following
    // hats and fills, not a tempo that moves (a player speeding up changes
    // gap by gap by a hair): then a grid that explains even half as much
    // stands.
    let ratio = if tracked_score > level {
        (uniform_score - level) / (tracked_score - level)
    } else {
        f64::INFINITY
    };
    let steady = grid.len() >= 8
        && uniform_score > level
        && (ratio >= 0.75 || (jitter(&tracked) > 0.06 && ratio >= 0.5));
    let (beats, grid_kind, beat_period) = if steady {
        let placed = attacks(x, &grid);
        let (start, step) = attack_line(&grid, &placed, step).unwrap_or((grid[0], step));
        (
            (0..grid.len())
                .map(|k| start + k as f64 * step)
                .collect::<Vec<f64>>(),
            "uniform",
            step,
        )
    } else {
        let mut gaps: Vec<f64> = tracked.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(f64::total_cmp);
        (tracked.clone(), "tracked", gaps[gaps.len() / 2])
    };
    let grid = grid_kind;

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

    let sections = sections(
        x,
        &spectra,
        &onsets,
        &downbeats,
        beat_period * m as f64,
        duration,
    );

    let confidence = confidence_of(&onsets, &beats);

    BeatAnalysis {
        tracker_version: TRACKER_VERSION,
        source: None,
        bpm_hint: hint,
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
/// of bass, its spectral centroid and its onset density, standardised, and
/// its timbre — the shape of its log-band spectrum; a boundary where the
/// bars after differ most from the bars before (four each side, fewer near
/// the ends; a novelty peak above its mean plus one deviation) — so a new
/// instrument or a drop at the same loudness is a boundary too. Sections
/// are at least four bars, two in a song shorter than sixteen.
fn sections(
    x: &[f64],
    spectra: &Spectra,
    onsets: &[f64],
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
    const SCALARS: usize = 4;
    let mut features: Vec<[f64; SCALARS]> = Vec::with_capacity(count);
    let mut timbre: Vec<[f64; BANDS]> = Vec::with_capacity(count);
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
        let (mut bass, mut total, mut centroid, mut density) = (0.0, 0.0, 0.0, 0.0);
        let mut shape = [0.0; BANDS];
        for f in fa..fb {
            bass += spectra.bass_energy[f];
            total += spectra.total_energy[f];
            centroid += spectra.centroid[f];
            density += onsets.get(f).copied().unwrap_or(0.0);
            for (s, v) in shape.iter_mut().zip(&spectra.bands[f]) {
                *s += v;
            }
        }
        let frames = (fb - fa).max(1) as f64;
        features.push([
            db,
            if total > 0.0 { bass / total } else { 0.0 },
            centroid / frames,
            density / frames,
        ]);
        timbre.push(shape.map(|v| v / frames));
        loudness.push(db);
    }
    for f in 0..SCALARS {
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
        // hears — 1 dB, 5% of the energy in the bass, 100 Hz of centroid, a
        // tenth of the onset envelope's spread — so a feature that barely
        // moves does not turn its noise into change.
        let spread = (square / count as f64)
            .sqrt()
            .max([1.0, 0.05, 100.0, 0.1][f]);
        for row in &mut features {
            row[f] = (row[f] - mean) / spread;
        }
    }
    let mean_of = |a: usize, b: usize| {
        let mut out = [0.0; SCALARS];
        for row in &features[a..b] {
            for f in 0..SCALARS {
                out[f] += row[f];
            }
        }
        out.map(|v| v / (b - a) as f64)
    };
    let shape_of = |a: usize, b: usize| {
        let mut out = [0.0; BANDS];
        for row in &timbre[a..b] {
            for (o, v) in out.iter_mut().zip(row) {
                *o += v;
            }
        }
        out
    };
    // The timbre change between two spans: one minus the cosine of their
    // mean log-band spectra, in units of 1% — a new voice in the mix
    // moves it by several.
    let timbre_change = |p: &[f64; BANDS], q: &[f64; BANDS]| {
        let (mut dot, mut pp, mut qq) = (0.0, 0.0, 0.0);
        for b in 0..BANDS {
            dot += p[b] * q[b];
            pp += p[b] * p[b];
            qq += q[b] * q[b];
        }
        if pp > 0.0 && qq > 0.0 {
            (1.0 - dot / (pp.sqrt() * qq.sqrt())) / 0.01
        } else {
            0.0
        }
    };
    let novelty: Vec<f64> = (0..count)
        .map(|i| {
            // Two bars a side at least: one bar against one is a fill,
            // not a section.
            if i < 2 || count - i < 2 {
                return 0.0;
            }
            let (a, z) = (i.saturating_sub(4), (i + 4).min(count));
            let (p, q) = (mean_of(a, i), mean_of(i, z));
            let mut square = 0.0;
            for f in 0..SCALARS {
                square += (p[f] - q[f]) * (p[f] - q[f]);
            }
            let t = timbre_change(&shape_of(a, i), &shape_of(i, z));
            (square + t * t).sqrt()
        })
        .collect();
    let judged: Vec<f64> = (2..count.saturating_sub(1)).map(|i| novelty[i]).collect();
    let mut sum = 0.0;
    for v in &judged {
        sum += v;
    }
    let inner = judged.len().max(1) as f64;
    let mean = sum / inner;
    let mut square = 0.0;
    for v in &judged {
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
    // At least `least` bars apart — the ends counting as boundaries too, so
    // a song's last, fading bar is not a section of its own.
    let least = if count >= 16 { 4 } else { 2 };
    for c in candidates {
        if c >= least && count - c >= least && cuts.iter().all(|&k| k.abs_diff(c) >= least) {
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
            // An intro or outro is a quieter stretch at an end — and not
            // most of the song.
            let short = 2 * (w[1] - w[0]) <= count;
            let label = if Some(i) == drop {
                "drop"
            } else if i == 0 && last > 0 && level[i] < 0.5 && short {
                "intro"
            } else if i == last && i > 0 && level[i] < 0.5 && short {
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

/// PCM reading lives in `crate::dsp`, shared with the word aligner.
pub use crate::dsp::{read_pcm, read_wav, Pcm};

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

// The cache's naming lives in `promo_model::beat_cache`, so a reader that
// does not analyse (explain, the apps) finds the same files.
pub use promo_model::beat_cache::{cache_path, files_hashed, load_cached, source_of, store};

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
        if let Some(cached) = load_cached(project, audio, options.bpm_hint) {
            return Ok((cached, true));
        }
    }
    let fresh = analyse_file(audio, options)?;
    Ok((store(project, audio, fresh)?, false))
}
