//! The offline aligner (docs/SPEECH-SYNC.md §4, slice 5) against speech
//! whose truth is known, deterministic to the bit.

mod support;

use promo_media::words::{AlignOptions, WordAligner};
use support::speech_synth::{synthetic_speech, SCRIPT};

fn syllables(script: &str) -> Vec<usize> {
    let tokens = promo_model::speech::tokenize(script);
    promo_speech::word_weights(&tokens, script, None)
        .iter()
        .map(|w| w.syllables.max(1.0).round() as usize)
        .collect()
}

fn aligned(script: &str, rate: f64) -> (promo_model::speech::TakeTiming, Vec<(f64, f64)>) {
    let (samples, truth) = synthetic_speech(script, &syllables(script), rate);
    let mut aligner = WordAligner::new(1, rate, script, AlignOptions::default()).unwrap();
    aligner.push(&samples);
    (aligner.finish(), truth)
}

/// AC5: every word start within 20 ms of the truth, every end within
/// 30 ms, on the synthetic take at a TTS rate.
#[test]
fn aligner_finds_synthetic_words() {
    let (timing, truth) = aligned(SCRIPT, 24_000.0);
    assert_eq!(timing.words.len(), 60);
    let mut worst = 0.0f64;
    let mut errors = Vec::new();
    for (w, (start, end)) in timing.words.iter().zip(&truth) {
        let e = (w.start - start).abs();
        errors.push(e);
        worst = worst.max(e);
        assert!(
            e <= 0.020,
            "{} starts {:.3}, truly {start:.3}",
            w.text,
            w.start
        );
        assert!(
            (w.end - end).abs() <= 0.030,
            "{} ends {:.3}, truly {end:.3}",
            w.text,
            w.end
        );
    }
    errors.sort_by(f64::total_cmp);
    eprintln!(
        "synthetic: median {:.1} ms, p95 {:.1} ms, worst {:.1} ms, confidence {}",
        errors[errors.len() / 2] * 1000.0,
        errors[errors.len() * 95 / 100] * 1000.0,
        worst * 1000.0,
        timing.confidence
    );
    assert_eq!(timing.sentences.len(), 8);
    assert!(timing.confidence > 0.6, "{}", timing.confidence);
    assert!(timing.extra.get("warnings").is_none());
}

fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// The committed fixture (`core/fixtures/speech/synthetic-60w.wav`, 16-bit
/// 24 kHz mono) is the synthesizer's, and the aligner's answer for that
/// FILE is the committed golden — what the apps' AVFoundation path must
/// answer too, and the same bits on macOS and Linux. Regenerate both with
/// PROMO_UPDATE_WORDS_GOLDEN=1.
#[test]
fn aligner_matches_its_golden() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech");
    let wav_path = dir.join("synthetic-60w.wav");
    let json_path = dir.join("synthetic-60w.json");
    let (samples, _) = synthetic_speech(SCRIPT, &syllables(SCRIPT), 24_000.0);
    let bytes = wav16(&samples, 24_000);
    let update = std::env::var_os("PROMO_UPDATE_WORDS_GOLDEN").is_some();
    if update {
        std::fs::write(&wav_path, &bytes).unwrap();
    }
    assert_eq!(
        std::fs::read(&wav_path).expect("the fixture is committed"),
        bytes,
        "the committed fixture is the synthesizer's"
    );
    let mut timing =
        promo_media::words::align_file(&wav_path, SCRIPT, &AlignOptions::default()).unwrap();
    timing.audio.filename = String::new();
    let text = serde_json::to_string_pretty(&timing).unwrap() + "\n";
    if update {
        std::fs::write(&json_path, &text).unwrap();
    }
    assert_eq!(
        text,
        std::fs::read_to_string(&json_path).expect("the golden is committed"),
        "the aligner's answer for the committed fixture moved"
    );
}

/// Chunks of any size give the same record: the downmix and every later
/// step see the same samples.
#[test]
fn chunked_push_equals_one_push() {
    let (samples, _) = synthetic_speech("Meet Euclid. Geometry, proved.", &[1, 2, 4, 1], 22_050.0);
    let stereo: Vec<f32> = samples.iter().flat_map(|s| [*s, *s * 0.5]).collect();
    let whole = {
        let mut a = WordAligner::new(
            2,
            22_050.0,
            "Meet Euclid. Geometry, proved.",
            AlignOptions::default(),
        )
        .unwrap();
        a.push(&stereo);
        a.finish()
    };
    for chunk in [1usize, 7, 1000, 4097] {
        let mut a = WordAligner::new(
            2,
            22_050.0,
            "Meet Euclid. Geometry, proved.",
            AlignOptions::default(),
        )
        .unwrap();
        for piece in stereo.chunks(chunk) {
            a.push(piece);
        }
        assert_eq!(a.finish(), whole, "chunk {chunk}");
    }
}

/// Audio that does not say its script is said to: a warning, and the
/// confidence held to 0.3.
#[test]
fn aligner_warns_when_audio_does_not_say_script() {
    // Twenty seconds of speech said to be two words.
    let (samples, _) = synthetic_speech(SCRIPT, &syllables(SCRIPT), 24_000.0);
    let mut aligner =
        WordAligner::new(1, 24_000.0, "Meet Euclid.", AlignOptions::default()).unwrap();
    aligner.push(&samples);
    let timing = aligner.finish();
    assert!(timing.confidence <= 0.3, "{}", timing.confidence);
    let warnings = timing.extra.get("warnings").expect("a warning").to_string();
    assert!(warnings.contains("may not say this script"), "{warnings}");
    // Two words of speech said to be sixty: too short to place them.
    let (samples, _) = synthetic_speech("Meet Euclid.", &[1, 2], 24_000.0);
    let mut aligner = WordAligner::new(1, 24_000.0, SCRIPT, AlignOptions::default()).unwrap();
    aligner.push(&samples);
    let timing = aligner.finish();
    assert!(timing.confidence <= 0.3, "{}", timing.confidence);
    let warnings = timing.extra.get("warnings").expect("a warning").to_string();
    assert!(
        warnings.contains("too short") || warnings.contains("may not say this script"),
        "{warnings}"
    );
    // A script with no words cannot be aligned at all.
    assert!(WordAligner::new(1, 24_000.0, "— !", AlignOptions::default()).is_none());
}

/// The provider fixtures' MP3s (a tone where each word is) through ffmpeg:
/// the aligner finds the words the fixtures say, within 20 ms.
#[test]
fn aligner_finds_the_provider_fixtures_words() {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech");
    let script = "Meet Euclid. Geometry, proved step by step.";
    let answer: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("google-timepoints.json")).unwrap())
            .unwrap();
    let truth: Vec<f64> = answer["timepoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["timeSeconds"].as_f64().unwrap())
        .collect();
    let timing = promo_media::words::align_file(
        &dir.join("google-timepoints.mp3"),
        script,
        &AlignOptions::default(),
    )
    .unwrap();
    let errors: Vec<f64> = timing
        .words
        .iter()
        .zip(&truth)
        .map(|(w, t)| (w.start - t).abs())
        .collect();
    eprintln!(
        "provider fixture: starts off by {:?} ms",
        errors
            .iter()
            .map(|e| (e * 1000.0).round())
            .collect::<Vec<_>>()
    );
    for (w, e) in timing.words.iter().zip(&errors) {
        assert!(*e <= 0.020, "{} off by {e}", w.text);
    }
}

/// A word said louder, longer and with a wider pitch move than its
/// neighbours is the take's most prominent; the scores are 0–1 and the same
/// every time (S8's `emphasised`).
#[test]
fn prominence_marks_the_stressed_word() {
    let rate = promo_media::words::RATE;
    let script = "one two three four five six.";
    let mut x: Vec<f64> = Vec::new();
    let mut words = Vec::new();
    let mut t = 0.2;
    x.resize((0.2 * rate) as usize, 0.0);
    for i in 0..6 {
        let stressed = i == 3;
        let length = if stressed { 0.45 } else { 0.25 };
        let amplitude = if stressed { 0.6 } else { 0.2 };
        let n = (length * rate) as usize;
        let mut phase = 0.0f64;
        for k in 0..n {
            let f = if stressed {
                180.0 + 120.0 * k as f64 / n as f64
            } else {
                200.0
            };
            phase += 2.0 * std::f64::consts::PI * f / rate;
            // A voice-like buzz: a few harmonics, an attack and release.
            let env = (k as f64 / (0.02 * rate))
                .min(1.0)
                .min((n - k) as f64 / (0.02 * rate));
            x.push(
                amplitude
                    * env
                    * (phase.sin() + 0.5 * (2.0 * phase).sin() + 0.25 * (3.0 * phase).sin()),
            );
        }
        words.push((t, t + length));
        t += length + 0.12;
        x.resize(x.len() + (0.12 * rate) as usize, 0.0);
    }
    let tokens = promo_model::speech::tokenize(script);
    let mut timing = promo_model::estimate::estimate_words(script, t + 0.2, None, 1.0);
    for (w, (a, b)) in timing.words.iter_mut().zip(&words) {
        w.start = *a;
        w.end = *b;
    }
    assert_eq!(tokens.words.len(), 6);
    let scores = promo_media::words::prominence(&x, &timing);
    assert_eq!(scores.len(), 6);
    assert!(scores.iter().all(|p| (0.0..=1.0).contains(p)), "{scores:?}");
    let top = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i);
    assert_eq!(top, Some(3), "{scores:?}");
    assert!(scores[3] > 0.8, "{scores:?}");
    assert_eq!(
        scores,
        promo_media::words::prominence(&x, &timing),
        "deterministic"
    );
}

/// Issue #10: a real TTS take (Kitten's Luna saying the reporter's line)
/// whose comma pause, after "light", the 0.4.0 aligner put before "of" —
/// "of light caught" 0.3–0.6 s late at 0.95 confidence. A pause now
/// belongs at the script's punctuation: the words around the comma land
/// within 120 ms of the voice's own timing, and the take within 60 ms at
/// the median.
#[test]
fn a_comma_pause_lands_at_its_comma() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech");
    let truth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("kitten-comma.json")).unwrap())
            .unwrap();
    let script = truth["script"].as_str().unwrap();
    let timing = promo_media::words::align_file(
        &dir.join("kitten-comma.wav"),
        script,
        &AlignOptions::default(),
    )
    .unwrap();
    let starts: Vec<(String, f64)> = truth["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w[0].as_str().unwrap().to_string(), w[1].as_f64().unwrap()))
        .collect();
    let mut errors: Vec<f64> = Vec::new();
    for ((text, start), word) in starts.iter().zip(&timing.words) {
        assert_eq!(*text, word.text);
        let error = (word.start - start).abs();
        if ["of", "light", "caught"].contains(&text.as_str()) {
            assert!(
                error <= 0.12,
                "{text}: {:.3} against {start:.3}",
                word.start
            );
        }
        errors.push(error);
    }
    errors.sort_by(f64::total_cmp);
    let median = errors[errors.len() / 2];
    eprintln!(
        "kitten comma: median {:.1} ms, worst {:.1} ms",
        median * 1000.0,
        errors[errors.len() - 1] * 1000.0
    );
    assert!(median <= 0.060, "median {median}");
}

/// A pause that sits away from the script's comma — the speaker paused
/// after "bit", the script's comma is after "light" — leaves the words
/// between doubtful: whichever side of the pause they land on, they read
/// at most 0.3, never 0.95.
#[test]
fn a_pause_away_from_its_comma_reads_doubtful() {
    let said = "Every photo is a little, bit of light caught and kept.";
    let script = "Every photo is a little bit of light, caught and kept.";
    let (samples, _) = synthetic_speech(said, &syllables(said), 24_000.0);
    let mut aligner = WordAligner::new(1, 24_000.0, script, AlignOptions::default()).unwrap();
    aligner.push(&samples);
    let timing = aligner.finish();
    let conf = |text: &str| {
        timing
            .words
            .iter()
            .find(|w| w.text == text)
            .and_then(|w| w.confidence)
            .unwrap()
    };
    assert!(
        conf("of") <= 0.3 && conf("light") <= 0.3,
        "{:?}",
        timing.words
    );
    assert!(
        conf("Every") > 0.5,
        "words far from it stay sure: {}",
        conf("Every")
    );
    // Where the script and the take agree, nothing is doubted.
    let (samples, _) = synthetic_speech(script, &syllables(script), 24_000.0);
    let mut aligner = WordAligner::new(1, 24_000.0, script, AlignOptions::default()).unwrap();
    aligner.push(&samples);
    let agreed = aligner.finish();
    assert!(
        agreed.words.iter().all(|w| w.confidence.unwrap() > 0.3),
        "{:?}",
        agreed.words
    );
}
