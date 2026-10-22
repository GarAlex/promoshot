//! Synthetic speech with a known truth (docs/SPEECH-SYNC.md §10, S5): each
//! syllable a voiced burst — a 140 Hz harmonic stack under two formant
//! bumps, with an attack and a release — syllables of a word back to back,
//! 0–40 ms between words, 300 ms at a comma and 600 ms at a sentence end.
//! Every transcendental is `libm`'s, every choice a splitmix64 of the
//! position, so the samples are the same bits on every machine.

// Shared by every test binary in this folder; only the word tests call it.
#![allow(dead_code)]

/// The fixture's script: sixty words of promo narration.
pub const SCRIPT: &str = "Meet Euclid, the patient geometry tutor. It proves every step, \
explains each idea, and never rushes you. Draw a triangle on the board. Euclid measures the \
angles, checks the sides, and shows why the sum is always the same. Ask a question at any \
time. The answer arrives in plain words, with a picture beside it. Try it today. Now.";

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A number in [0, 1) for position `k` of stream `stream`.
fn unit(stream: u64, k: u64) -> f64 {
    (splitmix(stream.wrapping_mul(1_000_003).wrapping_add(k)) >> 11) as f64 / (1u64 << 53) as f64
}

/// What follows a word: its own gap, from the punctuation after it.
fn gap_after(script: &str, word: usize, total: usize) -> f64 {
    if word + 1 == total {
        return 0.0;
    }
    let t = promo_model::speech::tokenize(script);
    let (a, b) = (t.words[word].chars[1], t.words[word + 1].chars[0]);
    let between: String = script
        .chars()
        .skip(a as usize)
        .take((b - a) as usize)
        .collect();
    if between.contains(['.', '!', '?']) {
        0.6
    } else if between.contains([',', ';', ':']) {
        0.3
    } else {
        [0.0, 0.005, 0.015, 0.025, 0.04][(unit(7, word as u64) * 5.0) as usize]
    }
}

/// The samples at `rate` (mono, in [-1, 1]) and each word's true
/// (start, end) in seconds, for `script` with `syllables[i]` bursts in
/// word i.
pub fn synthetic_speech(
    script: &str,
    syllables: &[usize],
    rate: f64,
) -> (Vec<f32>, Vec<(f64, f64)>) {
    synthetic_speech_pausing(script, syllables, rate, &[])
}

/// The same, with a pause INSIDE some words: `(word, after_syllable,
/// seconds)` — a voice that stops at "small-team"'s hyphen.
pub fn synthetic_speech_pausing(
    script: &str,
    syllables: &[usize],
    rate: f64,
    inner: &[(usize, usize, f64)],
) -> (Vec<f32>, Vec<(f64, f64)>) {
    let words = syllables.len();
    let lead = 0.35;
    // Plan the bursts first: (start, length, f1, f2, amplitude).
    let mut bursts: Vec<(f64, f64, f64, f64, f64)> = Vec::new();
    let mut truth = Vec::with_capacity(words);
    let mut t = lead;
    let mut k = 0u64;
    for (w, &count) in syllables.iter().enumerate() {
        let start = t;
        for syllable in 0..count.max(1) {
            if let Some(&(_, _, pause)) = inner
                .iter()
                .find(|(word, after, _)| *word == w && *after == syllable)
            {
                t += pause;
            }
            let length = 0.14 + 0.08 * unit(1, k);
            let f1 = 500.0 + 300.0 * unit(2, k);
            let f2 = 1100.0 + 800.0 * unit(3, k);
            let amplitude = 0.25 + 0.2 * unit(4, k);
            bursts.push((t, length, f1, f2, amplitude));
            t += length;
            k += 1;
        }
        truth.push((start, t));
        t += gap_after(script, w, words);
    }
    let total = t + 0.4;
    let n = (total * rate).ceil() as usize;
    let mut out = vec![0.0f32; n];
    // A low noise floor, as a quiet room has.
    let mut seed = 0x1234_5678u64;
    for s in out.iter_mut() {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        *s = ((seed >> 40) as f64 / (1u64 << 24) as f64 - 0.5) as f32 * 0.001;
    }
    let attack = 0.012;
    let release = 0.03;
    for &(start, length, f1, f2, amplitude) in &bursts {
        let first = (start * rate).floor() as usize;
        let last = (((start + length) * rate).ceil() as usize).min(n);
        // Harmonic weights under the two formants, normalised.
        let f0 = 140.0;
        let mut weights = Vec::new();
        let mut h = 1;
        while f0 * h as f64 <= 4000.0 {
            let f = f0 * h as f64;
            let bump = |centre: f64, width: f64| libm::exp(-((f - centre) / width).powi(2));
            weights.push(bump(f1, 150.0) + 0.6 * bump(f2, 250.0) + 0.05);
            h += 1;
        }
        let norm: f64 = weights.iter().sum();
        for (i, slot) in out.iter_mut().enumerate().take(last).skip(first) {
            let at = i as f64 / rate - start;
            if !(0.0..length).contains(&at) {
                continue;
            }
            let envelope = if at < attack {
                0.5 - 0.5 * libm::cos(std::f64::consts::PI * at / attack)
            } else if at > length - release {
                0.5 - 0.5 * libm::cos(std::f64::consts::PI * (length - at) / release)
            } else {
                1.0
            };
            let mut value = 0.0;
            for (j, wgt) in weights.iter().enumerate() {
                value += wgt * libm::sin(2.0 * std::f64::consts::PI * f0 * (j + 1) as f64 * at);
            }
            *slot += (amplitude * envelope * value / norm * 3.0) as f32;
        }
    }
    (out, truth)
}
