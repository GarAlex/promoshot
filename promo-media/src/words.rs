//! Offline forced alignment (docs/SPEECH-SYNC.md §4): when each word of a
//! script is said in a recording, with no model and no download — for
//! recordings, OpenAI and Chirp takes, and anything timed before timing
//! existed.
//!
//! The method: the audio becomes 10 ms frames (25 ms Hann windows at
//! 16 kHz) of log energy, voicing (the autocorrelation peak at 70–400 Hz)
//! and a 30 ms envelope; a voice-activity detector with hysteresis and
//! syllable nuclei after de Jong & Wempe (2009) come from those. The
//! script's words get the estimate's syllable counts and punctuation
//! pauses (`promo_speech::word_weights`), their expected lengths fitted to
//! the speech the detector found. A dynamic programme places every word
//! over the frames — a log-normal length prior, the nuclei it holds against
//! its syllables, edges on dips and pauses rather than through vowels, no
//! silence inside — with pauses allowed only where nothing is said. Each
//! word's edges are then put on the attack (or release) in the waveform.
//!
//! Determinism, as for the beat tracker: `libm` transcendentals, the
//! shared FFT and resampler (`crate::dsp`), sequential `f64` reductions,
//! ties to the lowest index, times rounded to 1 µs.

use promo_model::speech::{
    round_us, script_hash, sentences_of, tokenize, TakeTiming, TimingSource, Word, TIMING_VERSION,
};

/// Bumped whenever the alignment of the same samples could change: a
/// record from an older aligner is aligned again (§1.3).
pub use promo_model::speech::ALIGNER_VERSION;

/// The analysis rate.
pub const RATE: f64 = 16_000.0;
/// 10 ms.
const HOP: usize = 160;
/// 25 ms.
const WIN: usize = 400;
const FFT: usize = 512;
/// What each frame of a pause between two words the script runs together
/// costs, and what each frame of one at its punctuation earns (issue #10).
const PAUSE_FRAME_COST: f64 = 0.08;
const PAUSE_FRAME_REWARD: f64 = 0.02;
/// A pause this many frames long between words the script runs together
/// is a stray: with a comma within `MISPLACED_REACH` words that got no
/// pause, the words between read `DOUBTFUL` at most.
const STRAY_PAUSE: usize = 12;
/// A quiet run this long is a pause, and every frame of one inside a word
/// costs this much.
const HELD_PAUSE: usize = 8;
const SWALLOWED_FRAME_COST: f64 = 0.12;
/// How much quiet a word may hold, free, at each inner joint — a hyphen
/// between letters or digits ("small-team", "GPT-Rosalind"): a voice may
/// pause there (edge-tts does) where it never pauses inside a plain word
/// (issue #16).
const JOINT_PAUSE: usize = 40;
/// What each of those frames costs: less than a swallowed pause, more
/// than nothing, so a joint holds a pause the voice made, not one it
/// borrowed from the words around it.
const JOINT_FRAME_COST: f64 = 0.08;
const MISPLACED_REACH: usize = 3;
const DOUBTFUL: f64 = 0.3;

/// What the aligner reads the script as.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlignOptions {
    /// The voice's language ("en-US"), when known; else by its script.
    pub language: Option<String>,
}

/// Fed native-rate interleaved PCM in chunks of any size, like the beat
/// meter; [`WordAligner::finish`] aligns.
pub struct WordAligner {
    channels: usize,
    rate: f64,
    script: String,
    options: AlignOptions,
    mono: Vec<f32>,
    carry: Vec<f32>,
}

impl WordAligner {
    /// `None` for no channels, a rate that is not a positive number, or a
    /// script with no words.
    pub fn new(
        channels: usize,
        sample_rate: f64,
        script: &str,
        options: AlignOptions,
    ) -> Option<Self> {
        if channels == 0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
            return None;
        }
        if tokenize(script).words.is_empty() {
            return None;
        }
        Some(WordAligner {
            channels,
            rate: sample_rate,
            script: script.to_string(),
            options,
            mono: Vec::new(),
            carry: Vec::new(),
        })
    }

    /// Downmixes as it goes, so the chunking never changes a sample.
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

    /// The record, `source: "aligned"`, each word's prominence read from
    /// the same audio (§7.3); its `audio` is filled when stored.
    pub fn finish(self) -> TakeTiming {
        let duration = self.mono.len() as f64 / self.rate;
        let x = crate::dsp::resample(&self.mono, self.rate, RATE);
        let mut timing = align(&x, duration, &self.script, &self.options);
        let scores = prominence(&x, &timing);
        for (w, p) in timing.words.iter_mut().zip(scores) {
            w.prominence = Some(p);
        }
        timing
    }

    /// `timing` — a record of this audio another source timed (a provider,
    /// a local voice) — with each word's prominence read from the samples
    /// pushed, its times untouched.
    pub fn score(self, mut timing: TakeTiming) -> TakeTiming {
        let x = crate::dsp::resample(&self.mono, self.rate, RATE);
        let scores = prominence(&x, &timing);
        for (w, p) in timing.words.iter_mut().zip(scores) {
            w.prominence = Some(p);
        }
        timing
    }
}

/// The aligner over one audio file (a WAV directly, anything else through
/// ffmpeg), `audio` filled in.
pub fn align_file(
    path: &std::path::Path,
    script: &str,
    options: &AlignOptions,
) -> Result<TakeTiming, String> {
    let pcm = crate::dsp::read_pcm(path)?;
    let mut aligner = WordAligner::new(pcm.channels, pcm.rate, script, options.clone())
        .ok_or_else(|| {
            format!(
                "{}: nothing to align (no audio, or a script with no words)",
                path.display()
            )
        })?;
    aligner.push(&pcm.samples);
    let mut timing = aligner.finish();
    timing.audio = promo_model::media_cache::source_of(path)?;
    Ok(timing)
}

/// A take's word timing for a project, by the cache's rules (§1.3): an
/// exact record of a provider, a local voice or a transcription is
/// authoritative and never replaced; an aligned or estimated record from
/// this aligner (or estimate) stands; an older one is aligned again; a
/// record of the take under an older script (stale) is what the take says,
/// so it stands too; with none, the take is aligned and the record stored
/// (or only used, when the folder cannot be written). `refresh` aligns
/// again — refused for authoritative timing, which only a new take
/// replaces.
pub fn timing_for(
    project: &std::path::Path,
    take: &promo_model::word_cache::Take,
    options: &AlignOptions,
    refresh: bool,
) -> Result<Timed, String> {
    use promo_model::word_cache;
    let found = word_cache::lookup(project, &take.file, &take.script);
    // Every record carries its words' prominence (§7.3), read from the
    // audio once — whoever timed the take — and kept with the record.
    let with_prominence = |mut timing: TakeTiming| -> TakeTiming {
        if timing.words.iter().any(|w| w.prominence.is_none())
            && add_prominence(&take.file, &mut timing).is_ok()
        {
            if let Ok(stored) = word_cache::store(project, &take.file, timing.clone()) {
                return stored;
            }
        }
        timing
    };
    if let Some(found) = &found {
        if word_cache::usable(found, refresh, &take.id)? {
            return Ok(Timed {
                timing: with_prominence(found.timing.clone()),
                from_cache: true,
                stale: found.stale,
                realigned: false,
            });
        }
    }
    let fresh = align_file(&take.file, &take.script, options)?;
    let realigned = found.is_some();
    let timing = word_cache::store(project, &take.file, fresh.clone()).unwrap_or(fresh);
    let timing = with_prominence(timing);
    Ok(Timed {
        timing,
        from_cache: false,
        stale: false,
        realigned,
    })
}

/// Each word's prominence (docs/SPEECH-SYNC.md §7.3), 0–1: a z-score over
/// the take's words of three things read from the audio — the word's
/// loudness (its frames' mean dB), its pitch range (semitones between its
/// voiced frames' lowest and highest pitch) and how long it is against the
/// estimate's length for it — averaged and squashed. A cue for
/// generators, never a timing. `x` is mono at the aligner's rate.
pub fn prominence(x: &[f64], timing: &TakeTiming) -> Vec<f64> {
    let words = &timing.words;
    if words.is_empty() {
        return Vec::new();
    }
    let f = frames(x);
    let count = f.db.len();
    let frame_of =
        |t: f64| ((t * RATE / HOP as f64).max(0.0) as usize).min(count.saturating_sub(1));
    let expected =
        promo_model::estimate::estimate_words(&timing.script, timing.duration, None, 1.0);
    let mut loud = Vec::new();
    let mut range = Vec::new();
    let mut length = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let (a, b) = (
            frame_of(w.start),
            frame_of(w.end).max(frame_of(w.start) + 1).min(count),
        );
        let span = &f.db[a..b.max(a + 1).min(count)];
        loud.push(span.iter().sum::<f64>() / span.len().max(1) as f64);
        let pitches: Vec<f64> = f.pitch[a..b.max(a + 1).min(count)]
            .iter()
            .copied()
            .filter(|p| *p > 0.0)
            .collect();
        range.push(
            match (
                pitches.iter().copied().fold(f64::INFINITY, f64::min),
                pitches.iter().copied().fold(0.0f64, f64::max),
            ) {
                (lo, hi) if hi > 0.0 && lo.is_finite() => 12.0 * libm::log2(hi / lo),
                _ => 0.0,
            },
        );
        let said = (w.end - w.start).max(0.01);
        let guess = expected
            .words
            .get(i)
            .map(|e| (e.end - e.start).max(0.01))
            .unwrap_or(said);
        length.push(libm::log(said / guess));
    }
    let z = |v: &[f64]| -> Vec<f64> {
        let n = v.len() as f64;
        let mean = v.iter().sum::<f64>() / n;
        let sd = (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n).sqrt();
        v.iter()
            .map(|x| if sd > 1e-9 { (x - mean) / sd } else { 0.0 })
            .collect()
    };
    let (zl, zr, zd) = (z(&loud), z(&range), z(&length));
    (0..words.len())
        .map(|i| {
            let score = (zl[i] + zr[i] + zd[i]) / 3.0;
            let p = 1.0 / (1.0 + libm::exp(-1.5 * score));
            (p * 1000.0).round() / 1000.0
        })
        .collect()
}

/// [`prominence`] over a take's file, written onto its record's words.
pub fn add_prominence(path: &std::path::Path, timing: &mut TakeTiming) -> Result<(), String> {
    let pcm = crate::dsp::read_pcm(path)?;
    let channels = pcm.channels.max(1);
    let mono: Vec<f32> = pcm
        .samples
        .chunks_exact(channels)
        .map(crate::dsp::mix)
        .collect();
    let x = crate::dsp::resample(&mono, pcm.rate, RATE);
    let scores = prominence(&x, timing);
    for (w, p) in timing.words.iter_mut().zip(scores) {
        w.prominence = Some(p);
    }
    Ok(())
}

/// [`timing_for`]'s answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Timed {
    pub timing: TakeTiming,
    pub from_cache: bool,
    /// The record follows an older script.
    pub stale: bool,
    /// An older aligner's (or estimate's) record was aligned again.
    pub realigned: bool,
}

/// Per-frame features.
struct Frames {
    /// Log energy, dB.
    db: Vec<f64>,
    /// The envelope: dB smoothed over 30 ms.
    env: Vec<f64>,
    voiced: Vec<bool>,
    /// The pitch of a voiced frame, Hz; 0 when unvoiced.
    pitch: Vec<f64>,
    speech: Vec<bool>,
    /// The 10th percentile of `db`.
    floor: f64,
    /// Frame indices of syllable nuclei.
    nuclei: Vec<usize>,
}

fn hann() -> Vec<f64> {
    (0..WIN)
        .map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / WIN as f64))
        .collect()
}

fn frames(x: &[f64]) -> Frames {
    let count = if x.len() >= WIN {
        (x.len() - WIN) / HOP + 1
    } else {
        1
    };
    let window = hann();
    let wsum: f64 = window.iter().map(|w| w * w).sum();
    let fft = crate::dsp::Fft::new(FFT);
    let mut db = Vec::with_capacity(count);
    let mut voiced = Vec::with_capacity(count);
    let mut pitch = Vec::with_capacity(count);
    let (min_lag, max_lag) = ((RATE / 400.0) as usize, (RATE / 70.0) as usize);
    for t in 0..count {
        let start = t * HOP;
        let mut re = vec![0.0f64; FFT];
        let mut im = vec![0.0f64; FFT];
        let mut energy = 0.0;
        for i in 0..WIN {
            let v = x.get(start + i).copied().unwrap_or(0.0) * window[i];
            re[i] = v;
            energy += v * v;
        }
        db.push(10.0 * libm::log10(energy / wsum + 1e-10));
        // Voicing: the normalised autocorrelation's peak over the pitch
        // lags, from the power spectrum (zero-padded, so not circular
        // within the lags read).
        fft.run(&mut re, &mut im);
        for k in 0..FFT {
            re[k] = re[k] * re[k] + im[k] * im[k];
            im[k] = 0.0;
        }
        fft.run(&mut re, &mut im);
        let zero = re[0];
        let mut peak = 0.0f64;
        let mut best = 0usize;
        if zero > 1e-9 {
            for (lag, power) in re.iter().enumerate().take(max_lag + 1).skip(min_lag) {
                // Unbias by the overlap the window leaves at this lag.
                let r = power / zero * WIN as f64 / (WIN - lag) as f64;
                if r > peak {
                    peak = r;
                    best = lag;
                }
            }
        }
        voiced.push(peak > 0.5);
        pitch.push(if peak > 0.5 && best > 0 {
            RATE / best as f64
        } else {
            0.0
        });
    }
    let env: Vec<f64> = (0..count)
        .map(|t| {
            let a = t.saturating_sub(1);
            let b = (t + 1).min(count - 1);
            let mut sum = 0.0;
            for v in &db[a..=b] {
                sum += v;
            }
            sum / (b - a + 1) as f64
        })
        .collect();
    let mut sorted = db.clone();
    sorted.sort_by(f64::total_cmp);
    // The floor is the room's noise — or, for a synthesized take, digital
    // silence far below anything heard; the speech level is the 90th
    // percentile. Thresholds hold to both, so a breath or a pause 50 dB
    // under the voice is never speech just because the floor is -120 dB.
    let floor = sorted[sorted.len() / 10].max(sorted[sorted.len() * 9 / 10] - 50.0);
    // Voice activity: loud enough, or quieter and voiced; a quiet run
    // under 80 ms between speech is a dip, not a pause (the hysteresis); a
    // speech run under 30 ms is a click. No hangover: a pause is where the
    // word ends, and a hangover would hold every word 50 ms past its end.
    // Held to the voice's own level as well: a synthesized take's breaths
    // and pauses are noise only 15–30 dB under its speech, far above any
    // floor — loud enough, or nearer the voice and voiced.
    let level = sorted[sorted.len() * 9 / 10];
    let mut speech: Vec<bool> = (0..count)
        .map(|t| {
            (db[t] > floor + 12.0 && db[t] > level - 28.0)
                || (db[t] > floor + 6.0 && db[t] > level - 34.0 && voiced[t])
        })
        .collect();
    fill_runs(&mut speech, false, 8);
    fill_runs(&mut speech, true, 3);
    // Nuclei: envelope peaks at least 2 dB above the dip on either side,
    // voiced, in speech, at least 60 ms apart (the higher kept).
    let mut nuclei: Vec<usize> = Vec::new();
    for t in 1..count.saturating_sub(1) {
        if !(env[t] >= env[t - 1] && env[t] > env[t + 1] && speech[t] && voiced[t]) {
            continue;
        }
        if env[t] < floor + 9.0 {
            continue;
        }
        match nuclei.last().copied() {
            None => nuclei.push(t),
            Some(last) => {
                let dip = env[last..=t].iter().copied().fold(f64::INFINITY, f64::min);
                if t - last >= 6 && env[t] - dip >= 2.0 && env[last] - dip >= 2.0 {
                    nuclei.push(t);
                } else if env[t] > env[last] {
                    *nuclei.last_mut().unwrap() = t;
                }
            }
        }
    }
    Frames {
        db,
        env,
        voiced,
        pitch,
        speech,
        floor,
        nuclei,
    }
}

/// Turns runs of `value` shorter than `shorter` frames that sit between
/// runs of the other value into the other value.
fn fill_runs(flags: &mut [bool], value: bool, shorter: usize) {
    let n = flags.len();
    let mut t = 0;
    while t < n {
        if flags[t] != value {
            t += 1;
            continue;
        }
        let start = t;
        while t < n && flags[t] == value {
            t += 1;
        }
        if start > 0 && t < n && t - start < shorter {
            for f in &mut flags[start..t] {
                *f = !value;
            }
        }
    }
}

/// Seconds at a frame boundary `b` (between frames b−1 and b): halfway
/// between their centres.
fn boundary_seconds(b: usize) -> f64 {
    (b as f64 * HOP as f64 + WIN as f64 / 2.0 - HOP as f64 / 2.0) / RATE
}

const SIGMA: f64 = 0.35;

/// English words a speaker says quickly and without stress.
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "at", "by", "for", "with", "and", "or", "but", "is",
    "it", "as", "be", "you", "your", "we", "our", "its", "it's", "that", "this", "from", "then",
    "than", "so", "if", "not", "are", "was", "has", "had", "do", "can",
];

/// The alignment of 16 kHz mono `x` (the take is `duration` seconds) to
/// `script`.
pub fn align(x: &[f64], duration: f64, script: &str, options: &AlignOptions) -> TakeTiming {
    let tokens = tokenize(script);
    let weights = promo_speech::word_weights(&tokens, script, options.language.as_deref());
    let n = tokens.words.len();
    let f = frames(x);
    let count = f.db.len();
    let speech_frames = f.speech.iter().filter(|s| **s).count().max(1);
    let syllables: Vec<f64> = weights.iter().map(|w| w.syllables.max(1.0)).collect();
    let total_syllables: f64 = syllables.iter().sum();
    // A word's expected length: its syllables, and its letters (a proxy for
    // its phones — "strengths" outlasts "a" though both are one syllable),
    // with the short function words read short; fitted so the words fill
    // the speech the detector heard.
    let units: Vec<f64> = tokens
        .words
        .iter()
        .zip(&syllables)
        .map(|(w, s)| {
            let letters = w.text.chars().filter(|c| c.is_alphanumeric()).count() as f64;
            let base = s + 0.25 * letters;
            if FUNCTION_WORDS.contains(&w.text.to_lowercase().as_str()) {
                base * 0.6
            } else {
                base
            }
        })
        .collect();
    let total_units: f64 = units.iter().sum();
    // Quiet each word may hold at its inner joints, in frames.
    let joint_allowance: Vec<usize> = tokens
        .words
        .iter()
        .map(|w| joints(&w.text) * JOINT_PAUSE)
        .collect();
    let expected: Vec<f64> = units
        .iter()
        .map(|u| (u * speech_frames as f64 / total_units).max(3.0))
        .collect();

    // Prefix sums: nuclei and non-speech frames.
    let mut nuc = vec![0usize; count + 1];
    let mut quiet = vec![0usize; count + 1];
    let is_nucleus: Vec<bool> = {
        let mut v = vec![false; count];
        for &t in &f.nuclei {
            v[t] = true;
        }
        v
    };
    for t in 0..count {
        nuc[t + 1] = nuc[t] + usize::from(is_nucleus[t]);
        quiet[t + 1] = quiet[t] + usize::from(!f.speech[t]);
    }
    // Runs of non-speech: how many frames from t on (and up to t) are
    // quiet — a word never begins or ends inside a pause.
    let mut quiet_ahead = vec![0usize; count + 1];
    for t in (0..count).rev() {
        quiet_ahead[t] = if f.speech[t] {
            0
        } else {
            quiet_ahead[t + 1] + 1
        };
    }
    let mut quiet_behind = vec![0usize; count + 1];
    for t in 1..=count {
        quiet_behind[t] = if f.speech[t - 1] {
            0
        } else {
            quiet_behind[t - 1] + 1
        };
    }
    // Frames inside a real pause (a quiet run the detector kept, not a
    // dip it filled): a word that swallows one pays for every frame, more
    // than the pause would cost between words — or a long pause the script
    // puts nowhere hides inside a word (issue #10).
    let mut held = vec![0usize; count + 1];
    for t in 0..count {
        let run = quiet_ahead[t] + quiet_behind[t];
        held[t + 1] = held[t] + usize::from(!f.speech[t] && run >= HELD_PAUSE);
    }
    // Edge quality at each boundary.
    let edge: Vec<f64> = (0..=count)
        .map(|b| {
            if b == 0 || b == count {
                return -0.3;
            }
            if f.speech[b - 1] != f.speech[b] {
                return -0.3;
            }
            let here = f.env[b].min(f.env[b - 1]);
            let left = f.env[b.saturating_sub(15)..b]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            let right = f.env[b..(b + 15).min(count)]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            let local_min = here
                <= f.env[b.saturating_sub(2)..(b + 2).min(count)]
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min)
                    + 1e-9;
            if local_min && here <= left.min(right) - 3.0 {
                -0.3
            } else if f.voiced[b] && f.voiced[b - 1] && here > f.floor + 18.0 {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    let word_cost = |i: usize, s: usize, b: usize| -> f64 {
        let len = (b - s) as f64;
        let ratio = libm::log(len / expected[i]);
        let mut cost = ratio * ratio / (2.0 * SIGMA * SIGMA);
        cost += 0.5 * ((nuc[b] - nuc[s]) as f64 - syllables[i]).abs();
        let allowed = joint_allowance[i];
        let held_in = held[b] - held[s];
        let free = held_in.min(allowed);
        cost += 2.0 * (quiet[b] - quiet[s] - free) as f64 / len;
        cost += SWALLOWED_FRAME_COST * (held_in - free) as f64 + JOINT_FRAME_COST * free as f64;
        let span = b - s;
        cost += 0.15 * (quiet_ahead[s].min(span) + quiet_behind[b].min(span)) as f64;
        cost += edge[s] + edge[b];
        cost
    };
    // A pause after word i: where the script has punctuation it is
    // expected — the longer the better, up to the length the punctuation
    // asks for; where it has none, every frame of silence is paid for, so
    // a long pause is pulled to the comma it belongs to rather than left
    // between two words that run on (issue #10).
    let pause_cost = |i: usize| {
        if weights[i].pause_after > 0.0 {
            -1.0
        } else {
            1.0
        }
    };
    let pause_frame_cost = |i: usize| {
        if weights[i].pause_after > 0.0 {
            -PAUSE_FRAME_REWARD
        } else {
            PAUSE_FRAME_COST
        }
    };

    // d[i][b]: word i ends at b. g[i][s]: word i ended, then a pause of
    // non-speech frames up to s. Back-pointers: the start of word i, and
    // the start of the pause.
    const NONE: usize = usize::MAX;
    let width = count + 1;
    let mut d = vec![f64::INFINITY; n * width];
    let mut from = vec![NONE; n * width];
    let mut via_pause = vec![false; n * width];
    let mut g = vec![f64::INFINITY; n * width];
    let mut pause_from = vec![NONE; n * width];
    // Where the first word may start: anywhere, every speech frame before
    // it a cost (something said that the script does not hold).
    let lead: Vec<f64> = (0..=count).map(|s| 0.1 * (s - quiet[s]) as f64).collect();
    for i in 0..n {
        let min_len = ((expected[i] / 4.0).floor() as usize).max(3);
        let max_len = ((expected[i] * 4.0).ceil() as usize + 10).max(min_len + 1);
        for b in min_len..=count {
            let mut best = f64::INFINITY;
            let mut best_s = NONE;
            let mut best_pause = false;
            let first = b.saturating_sub(max_len);
            for s in first..=b - min_len {
                let (before, paused) = if i == 0 {
                    (lead[s], false)
                } else {
                    let plain = d[(i - 1) * width + s];
                    let paused = g[(i - 1) * width + s];
                    if paused < plain {
                        (paused, true)
                    } else {
                        (plain, false)
                    }
                };
                if !before.is_finite() {
                    continue;
                }
                let cost = before + word_cost(i, s, b);
                if cost < best {
                    best = cost;
                    best_s = s;
                    best_pause = paused;
                }
            }
            d[i * width + b] = best;
            from[i * width + b] = best_s;
            via_pause[i * width + b] = best_pause;
        }
        // Pauses after word i: runs of non-speech frames.
        for s in 1..=count {
            if f.speech[s - 1] {
                continue;
            }
            let start_here = d[i * width + s - 1] + pause_cost(i);
            let extend = g[i * width + s - 1] + pause_frame_cost(i);
            if start_here < extend {
                g[i * width + s] = start_here;
                pause_from[i * width + s] = s - 1;
            } else if extend.is_finite() {
                g[i * width + s] = extend;
                pause_from[i * width + s] = pause_from[i * width + s - 1];
            }
        }
    }
    // The last word ends where what follows is cheapest: speech after it
    // costs as speech before the first word does.
    let mut end_best = f64::INFINITY;
    let mut end_at = count;
    for b in 0..=count {
        let cost = d[(n - 1) * width + b] + 0.1 * ((count - b) - (quiet[count] - quiet[b])) as f64;
        if cost < end_best {
            end_best = cost;
            end_at = b;
        }
    }
    // Backtrack.
    let mut spans = vec![(0usize, 0usize); n];
    let mut b = end_at;
    let mut ok = end_best.is_finite();
    for i in (0..n).rev() {
        if !ok {
            break;
        }
        let s = from[i * width + b];
        if s == NONE {
            ok = false;
            break;
        }
        spans[i] = (s, b);
        if i > 0 {
            b = if via_pause[i * width + b] {
                pause_from[(i - 1) * width + s]
            } else {
                s
            };
        }
    }
    if !ok {
        // Too short for its words: spread them evenly, at the floor of
        // confidence.
        return spread(&tokens, script, duration);
    }

    // Edge confidence: how much worse the best other boundary at least
    // 30 ms away is.
    let margin = |i: usize, at: usize, start_edge: bool| -> f64 {
        let (s, b) = spans[i];
        let mut best = f64::INFINITY;
        let lo = at.saturating_sub(12);
        let hi = (at + 12).min(count);
        for alt in lo..=hi {
            if alt.abs_diff(at) < 3 {
                continue;
            }
            let delta = if start_edge {
                if alt + 2 > b {
                    continue;
                }
                let mut c = word_cost(i, alt, b) - word_cost(i, s, b);
                if i > 0 && spans[i - 1].1 == s && alt > spans[i - 1].0 + 2 {
                    let (ps, _) = spans[i - 1];
                    c += word_cost(i - 1, ps, alt) - word_cost(i - 1, ps, s);
                }
                c
            } else {
                if alt < s + 2 {
                    continue;
                }
                let mut c = word_cost(i, s, alt) - word_cost(i, s, b);
                if i + 1 < n && spans[i + 1].0 == b && alt + 2 < spans[i + 1].1 {
                    let (_, nb) = spans[i + 1];
                    c += word_cost(i + 1, alt, nb) - word_cost(i + 1, b, nb);
                }
                c
            };
            best = best.min(delta);
        }
        if best.is_finite() {
            1.0 - libm::exp(-best.max(0.0))
        } else {
            1.0
        }
    };
    // A pause the programme had to put where the script runs on, while a
    // comma near it went without one: the words between are as likely on
    // the wrong side of it as the right one, whatever their edges look
    // like (issue #10). They read as doubtful.
    let pause_after: Vec<usize> = (0..n)
        .map(|i| {
            if i + 1 < n {
                spans[i + 1].0.saturating_sub(spans[i].1)
            } else {
                0
            }
        })
        .collect();
    let punctuated = |i: usize| weights[i].pause_after > 0.0;
    let mut doubtful = vec![false; n];
    for i in 0..n.saturating_sub(1) {
        if punctuated(i) || pause_after[i] < STRAY_PAUSE {
            continue;
        }
        let lo = i.saturating_sub(MISPLACED_REACH);
        let hi = (i + MISPLACED_REACH).min(n - 2);
        for (j, after) in pause_after.iter().enumerate().take(hi + 1).skip(lo) {
            if j != i && punctuated(j) && *after < STRAY_PAUSE / 2 {
                for flag in doubtful.iter_mut().take(i.max(j) + 2).skip(i.min(j)) {
                    *flag = true;
                }
            }
        }
    }
    let mut words: Vec<Word> = Vec::with_capacity(n);
    let mut confidences = Vec::with_capacity(n);
    for (i, w) in tokens.words.iter().enumerate() {
        let (s, b) = spans[i];
        let (start, rise) = refine_with_rise(x, boundary_seconds(s), true);
        let (end, fall) = refine_with_rise(x, boundary_seconds(b), false);
        let end = end.max(start);
        // An edge the waveform shows plainly (a 15 dB attack or release)
        // is trusted; one inside connected speech is as sure as the
        // programme's margin over the next best boundary.
        let sure = |plain: f64, margin: f64| {
            if plain >= 15.0 {
                0.95f64.max(margin)
            } else {
                margin
            }
        };
        let mut c = sure(rise, margin(i, s, true)).min(sure(fall, margin(i, b, false)));
        if doubtful[i] {
            c = c.min(DOUBTFUL);
        }
        confidences.push(c);
        words.push(Word {
            text: w.text.clone(),
            start: round_us(start.min(duration)),
            end: round_us(end.min(duration)),
            chars: w.chars,
            confidence: Some((c * 100.0).round() / 100.0),
            interpolated: false,
            prominence: None,
        });
    }
    // Keep them in order after refinement.
    for i in 1..n {
        if words[i].start < words[i - 1].end {
            let mid = round_us((words[i].start + words[i - 1].end) / 2.0);
            words[i - 1].end = mid.max(words[i - 1].start);
            words[i].start = mid.min(words[i].end);
        }
    }
    let mean = confidences.iter().sum::<f64>() / n as f64;
    let found = f.nuclei.len() as f64;
    let fit = found.min(total_syllables) / found.max(total_syllables).max(1.0);
    let mut confidence = mean * fit;
    let mut extra = serde_json::Map::new();
    // Does the recording say this script at all?
    let speech_seconds = speech_frames as f64 * HOP as f64 / RATE;
    let reads = promo_speech::estimate(script, options.language.as_deref(), 1.0);
    let spoken = (reads.seconds - reads.pauses - 0.4).max(0.1);
    let ratio = speech_seconds / spoken;
    if !(0.6..=1.7).contains(&ratio) {
        confidence = confidence.min(0.3);
        extra.insert(
            "warnings".into(),
            serde_json::json!([format!(
                "the recording may not say this script (speech {speech_seconds:.1} s, the script reads about {spoken:.1} s)"
            )]),
        );
    }
    let confidence = (confidence * 100.0).round() / 100.0;
    TakeTiming {
        timing_version: TIMING_VERSION,
        audio: Default::default(),
        script: script.to_string(),
        script_hash: script_hash(script),
        duration: round_us(duration),
        source: TimingSource::Aligned,
        estimated: false,
        confidence,
        sentences: sentences_of(&tokens, &words),
        words,
        aligner_version: Some(ALIGNER_VERSION),
        extra,
    }
}

/// Inner joints a voice may pause at: hyphens (or slashes) with a letter
/// or digit on each side — "small-team" has one, "-" alone none.
fn joints(word: &str) -> usize {
    let chars: Vec<char> = word.chars().collect();
    (1..chars.len().saturating_sub(1))
        .filter(|&i| {
            matches!(chars[i], '-' | '\u{2010}' | '\u{2011}' | '/')
                && chars[i - 1].is_alphanumeric()
                && chars[i + 1].is_alphanumeric()
        })
        .count()
}

/// A take too short for its words: even shares, confidence 0.1.
fn spread(tokens: &promo_model::speech::Script, script: &str, duration: f64) -> TakeTiming {
    let n = tokens.words.len().max(1) as f64;
    let words: Vec<Word> = tokens
        .words
        .iter()
        .enumerate()
        .map(|(i, w)| Word {
            text: w.text.clone(),
            start: round_us(duration * i as f64 / n),
            end: round_us(duration * (i + 1) as f64 / n),
            chars: w.chars,
            confidence: None,
            interpolated: true,
            prominence: None,
        })
        .collect();
    let mut extra = serde_json::Map::new();
    extra.insert(
        "warnings".into(),
        serde_json::json!(["the take is too short to align its words"]),
    );
    TakeTiming {
        timing_version: TIMING_VERSION,
        audio: Default::default(),
        script: script.to_string(),
        script_hash: script_hash(script),
        duration: round_us(duration),
        source: TimingSource::Aligned,
        estimated: false,
        confidence: 0.1,
        sentences: sentences_of(tokens, &words),
        words,
        aligner_version: Some(ALIGNER_VERSION),
        extra,
    }
}

/// An edge put on the waveform: within ±45 ms of `at`, where a 10 ms
/// centred RMS (longer than a pitch period, so a voice's own ripple never
/// crosses; 1 ms hop) crosses 6 dB under the loudest it reaches there —
/// the crossing going up nearest `at` for a start, going down for an end.
/// With no rise to speak of (connected speech) the edge stays.
/// Answers the edge and how many dB the voice rose (or fell) there — 0
/// when it found none.
fn refine_with_rise(x: &[f64], at: f64, start: bool) -> (f64, f64) {
    const STEP: i64 = 16;
    const HALF: i64 = 80;
    let centre = (at * RATE) as i64;
    let reach = (0.045 * RATE) as i64;
    let level = |p: i64| -> f64 {
        let mut e = 0.0;
        let mut k = 0;
        for i in (p - HALF).max(0)..(p + HALF).min(x.len() as i64) {
            e += x[i as usize] * x[i as usize];
            k += 1;
        }
        10.0 * libm::log10(e / k.max(1) as f64 + 1e-10)
    };
    let points: Vec<(i64, f64)> = ((centre - reach)..=(centre + reach))
        .step_by(STEP as usize)
        .filter(|p| *p >= 0 && (*p as usize) < x.len())
        .map(|p| (p, level(p)))
        .collect();
    if points.len() < 3 {
        return (at, 0.0);
    }
    let lo = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let hi = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    if hi - lo < 10.0 {
        return (at, 0.0);
    }
    let threshold = (hi - 18.0).max(lo + 3.0);
    let crossing = points
        .windows(2)
        .filter_map(|w| {
            let rising = w[0].1 < threshold && w[1].1 >= threshold;
            let falling = w[0].1 >= threshold && w[1].1 < threshold;
            ((start && rising) || (!start && falling)).then_some(w[1].0)
        })
        .min_by_key(|p| (p - centre).abs());
    match crossing {
        Some(p) => (p as f64 / RATE, hi - lo),
        None => (at, 0.0),
    }
}
