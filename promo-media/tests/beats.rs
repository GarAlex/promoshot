//! The beat tracker, slice 4 (docs/BEAT-SYNC.md §3): synthesized click
//! tracks at 64, 96 and 128 BPM, whose truth is known to the sample.

mod support;

use promo_media::beats::{self, BeatMeter, BeatOptions};
use promo_model::musical::BeatAnalysis;
use support::click_track::{click_track, wav, wav16, OFFSET};

fn analyse(samples: &[f32], channels: usize, rate: f64) -> BeatAnalysis {
    let mut meter = BeatMeter::new(channels, rate, BeatOptions::default()).unwrap();
    meter.push(samples);
    meter.finish()
}

fn nearest(truth: &[f64], t: f64) -> f64 {
    truth
        .iter()
        .map(|b| (b - t).abs())
        .fold(f64::INFINITY, f64::min)
}

/// The tempo within 0.05 BPM, a steady track fitted to one exact grid, and
/// every beat within 5 ms of where it was put.
#[test]
fn tracker_finds_fixture_tempo_and_beats() {
    for bpm in [64.0, 96.0, 128.0] {
        let (samples, truth) = click_track(bpm, 30.0, 44_100.0, None);
        let found = analyse(&samples, 1, 44_100.0);
        assert!((found.bpm - bpm).abs() < 0.05, "{bpm}: found {}", found.bpm);
        assert_eq!(found.grid, "uniform", "{bpm}");
        assert!(
            found.beats.len() + 1 >= truth.len(),
            "{bpm}: {} beats",
            found.beats.len()
        );
        for &t in &found.beats {
            let off = nearest(&truth, t);
            assert!(
                off < 0.005,
                "{bpm}: a beat at {t} is {off} s from the truth"
            );
            // Placed on its attack in the waveform, not a 23 ms frame: to
            // within a millisecond.
            assert!(
                off < 0.001,
                "{bpm}: a beat at {t} is {off} s off its attack"
            );
        }
        assert!(
            found.confidence > 0.5,
            "{bpm}: confidence {}",
            found.confidence
        );
        assert_eq!(found.time_signature, [4, 4]);
    }
}

/// The downbeats are the bars' first beats — the bass note — and the
/// offset is the first of them.
#[test]
fn tracker_finds_downbeats() {
    for bpm in [64.0, 96.0, 128.0] {
        let (samples, truth) = click_track(bpm, 30.0, 44_100.0, None);
        let found = analyse(&samples, 1, 44_100.0);
        let bars: Vec<f64> = truth.iter().step_by(4).copied().collect();
        assert!(
            (found.offset - OFFSET).abs() < 0.005,
            "{bpm}: offset {}",
            found.offset
        );
        for &t in &found.downbeats {
            assert!(
                nearest(&bars, t) < 0.005,
                "{bpm}: downbeat {t} is no bar start"
            );
        }
        assert!(found.downbeats.len() + 1 >= bars.len());
    }
}

/// The 128 BPM track's second half, bars 9 to 16, plays three times louder:
/// a section starts at bar 9, the drop.
#[test]
fn tracker_finds_the_loud_section_at_bar_9() {
    let (samples, truth) = click_track(128.0, 30.0, 44_100.0, Some(9));
    let found = analyse(&samples, 1, 44_100.0);
    let drop = found
        .sections
        .iter()
        .find(|s| s.start_bar == 9)
        .unwrap_or_else(|| panic!("no section at bar 9: {:?}", found.sections));
    assert_eq!(drop.label, "drop");
    assert!((drop.start - truth[32]).abs() < 0.005);
    assert_eq!(drop.energy, 1.0);
    assert_eq!(found.sections[0].start_bar, 1);
    assert_eq!(found.sections[0].label, "intro");
}

/// Chunks of any size, frames split across them, give the same answer as
/// one push — at a rate the resampler cannot divide evenly, in stereo.
#[test]
fn chunked_push_equals_one_push() {
    let (mono, _) = click_track(96.0, 12.0, 48_000.0, None);
    let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s * 0.5]).collect();
    let whole = analyse(&stereo, 2, 48_000.0);
    let mut meter = BeatMeter::new(2, 48_000.0, BeatOptions::default()).unwrap();
    for chunk in stereo.chunks(1001) {
        meter.push(chunk);
    }
    assert_eq!(meter.finish(), whole);
    assert!((whole.bpm - 96.0).abs() < 0.05, "{}", whole.bpm);
}

/// The analysis of the 128 BPM fixture is the committed golden, byte for
/// byte — on every machine (the same test runs in the Linux image).
/// Regenerate with PROMO_UPDATE_BEATS_GOLDEN=1.
#[test]
fn tracker_matches_its_golden() {
    let (samples, _) = click_track(128.0, 30.0, 44_100.0, Some(9));
    let text = serde_json::to_string_pretty(&analyse(&samples, 1, 44_100.0)).unwrap() + "\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/click-128.json");
    if std::env::var_os("PROMO_UPDATE_BEATS_GOLDEN").is_some() {
        std::fs::write(path, &text).unwrap();
    }
    let golden = std::fs::read_to_string(path).expect("the golden is committed");
    assert_eq!(text, golden, "the tracker's answer moved");
}

/// The fixture the apps analyse through AVFoundation
/// (`core/fixtures/beats/`): a 16-bit 22.05 kHz mono WAV of the 120 BPM
/// click track, committed so another host can read the same bytes, and the
/// analysis this tracker gives of that FILE — what the app's
/// AVAudioFile → beat meter path must answer too. Regenerate both with
/// PROMO_UPDATE_BEATS_GOLDEN=1.
#[test]
fn committed_fixture_is_the_synthesizer_and_its_golden() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/beats");
    let wav_path = std::path::Path::new(dir).join("click-120.wav");
    let json_path = std::path::Path::new(dir).join("click-120.json");
    let (samples, _) = click_track(120.0, 8.0, 22_050.0, Some(2));
    let bytes = wav16(&samples, 1, 22_050);
    if std::env::var_os("PROMO_UPDATE_BEATS_GOLDEN").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&wav_path, &bytes).unwrap();
    }
    assert_eq!(
        std::fs::read(&wav_path).expect("the fixture is committed"),
        bytes,
        "the committed fixture is the synthesizer's"
    );
    let mut analysis = promo_media::beats::analyse_file(&wav_path, BeatOptions::default()).unwrap();
    analysis.source = None;
    let text = serde_json::to_string_pretty(&analysis).unwrap() + "\n";
    if std::env::var_os("PROMO_UPDATE_BEATS_GOLDEN").is_some() {
        std::fs::write(&json_path, &text).unwrap();
    }
    assert_eq!(
        text,
        std::fs::read_to_string(&json_path).expect("the golden is committed"),
        "the tracker's answer for the committed fixture moved"
    );
    assert!((analysis.bpm - 120.0).abs() < 0.05, "{}", analysis.bpm);
}

fn project(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-beats-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    dir
}

/// The cache is the file's content, not its name: a renamed copy finds the
/// same analysis, under `Resources/.beats/<32 hex digits>.json`.
#[test]
fn cache_is_keyed_by_content_not_name() {
    let dir = project("key");
    let (samples, _) = click_track(128.0, 10.0, 44_100.0, None);
    let song = dir.join("Resources/song.wav");
    std::fs::write(&song, wav(&samples, 1, 44_100)).unwrap();
    let (stored, cached) = beats::analysis_for(&dir, &song, BeatOptions::default(), false).unwrap();
    assert!(!cached);
    let source = stored.source.clone().unwrap();
    assert_eq!(source.sha256.len(), 32);
    assert_eq!(source.filename, "song.wav");
    assert!(beats::cache_path(&dir, &source.sha256).exists());
    let renamed = dir.join("Resources/renamed.wav");
    std::fs::copy(&song, &renamed).unwrap();
    let (again, cached) =
        beats::analysis_for(&dir, &renamed, BeatOptions::default(), false).unwrap();
    assert!(cached, "a renamed copy is the same content");
    assert_eq!(again, stored);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Once written, the cache is the answer — a later tracker never re-times
/// an edit on its own; only a refresh analyses again.
#[test]
fn cache_is_never_silently_reanalysed() {
    let dir = project("authority");
    let (samples, _) = click_track(96.0, 10.0, 44_100.0, None);
    let song = dir.join("Resources/song.wav");
    std::fs::write(&song, wav(&samples, 1, 44_100)).unwrap();
    let (mut stored, _) = beats::analysis_for(&dir, &song, BeatOptions::default(), false).unwrap();
    let key = stored.source.clone().unwrap().sha256;
    stored.bpm = 99.0;
    std::fs::write(
        beats::cache_path(&dir, &key),
        serde_json::to_string(&stored).unwrap(),
    )
    .unwrap();
    let (cached, from_cache) =
        beats::analysis_for(&dir, &song, BeatOptions::default(), false).unwrap();
    assert!(from_cache);
    assert_eq!(cached.bpm, 99.0, "the cache was overruled");
    let (fresh, from_cache) =
        beats::analysis_for(&dir, &song, BeatOptions::default(), true).unwrap();
    assert!(!from_cache);
    assert!((fresh.bpm - 96.0).abs() < 0.05);
    assert_eq!(
        beats::load_cached(&dir, &song, None).unwrap().bpm,
        fresh.bpm
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn analyse_with(samples: &[f32], rate: f64, options: BeatOptions) -> BeatAnalysis {
    let mut meter = BeatMeter::new(1, rate, options).unwrap();
    meter.push(samples);
    meter.finish()
}

/// A loop busy with eighth-note arpeggios and sixteenth-note hats still
/// fits one exact grid on its kicks, at its tempo, with confidence (the
/// first tracker followed the hats and arpeggios: a wandering track and a
/// confidence of 0.2 on real generated chiptune).
#[test]
fn a_busy_loop_fits_one_grid_on_its_pulse() {
    let (samples, truth) = support::click_track::busy_loop(124.0, 24.0, 44_100.0);
    let found = analyse(&samples, 1, 44_100.0);
    assert!((found.bpm - 124.0).abs() < 0.1, "found {}", found.bpm);
    assert_eq!(found.grid, "uniform");
    assert!(found.confidence > 0.5, "confidence {}", found.confidence);
    for &t in &found.beats {
        let off = nearest(&truth, t);
        assert!(off < 0.010, "a beat at {t} is {off} s from a kick");
    }
}

/// The hint constrains the search: eighth notes all alike read as 140 BPM
/// (the faster pulse, nearer the default 120); told 70, the tracker counts
/// the beat at 70 — and a hint of 140 keeps 140.
#[test]
fn the_hint_moves_the_tempo_to_the_pulse_it_names() {
    let samples = support::click_track::even_eighths(70.0, 24.0, 44_100.0);
    let plain = analyse(&samples, 1, 44_100.0);
    assert!((plain.bpm - 140.0).abs() < 0.2, "unhinted: {}", plain.bpm);
    let hinted = analyse_with(
        &samples,
        44_100.0,
        BeatOptions {
            meter: 4,
            bpm_hint: Some(70.0),
        },
    );
    assert!((hinted.bpm - 70.0).abs() < 0.2, "hinted 70: {}", hinted.bpm);
    let hinted = analyse_with(
        &samples,
        44_100.0,
        BeatOptions {
            meter: 4,
            bpm_hint: Some(140.0),
        },
    );
    assert!(
        (hinted.bpm - 140.0).abs() < 0.2,
        "hinted 140: {}",
        hinted.bpm
    );
}

/// Music whose tempo really moves keeps its tracked beats — one grid would
/// drift off them — and they follow the player.
#[test]
fn a_gliding_tempo_keeps_its_tracked_beats() {
    let (samples, truth) = support::click_track::gliding_kicks(110.0, 130.0, 24.0, 44_100.0);
    let found = analyse(&samples, 1, 44_100.0);
    assert_eq!(found.grid, "tracked", "bpm {}", found.bpm);
    let on = found
        .beats
        .iter()
        .filter(|&&t| nearest(&truth, t) < 0.015)
        .count();
    assert!(
        on * 10 >= found.beats.len() * 9,
        "{on} of {} beats on a kick",
        found.beats.len()
    );
}

/// An arrangement that changes at one loudness, one centroid, one bass and
/// one onset density — two tones becoming one at bar 9 — is a section
/// boundary: the timbre alone says so.
#[test]
fn an_arrangement_change_at_one_loudness_is_a_section() {
    let samples = support::click_track::arrangement_change(120.0, 44_100.0);
    let found = analyse(&samples, 1, 44_100.0);
    let starts: Vec<u32> = found.sections.iter().map(|s| s.start_bar).collect();
    assert!(
        starts.contains(&9),
        "sections start at bars {starts:?}: {:?} downbeats {:?}",
        found.sections,
        found.downbeats
    );
    assert_eq!(found.sections.len(), 2, "{:?}", found.sections);
}

/// A real generated loop (ACE-Step, Apache-2.0; asked for 124 BPM): the
/// first tracker heard 121 BPM on a wandering track with confidence 0.22.
/// It is one steady grid near 124.6 BPM — the kicks in the low band agree —
/// with confidence, and the printed beats are that grid.
#[test]
fn the_real_generated_loop_fits_its_tempo() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fixtures/beats/milo-loop.wav"
    );
    let found = beats::analyse_file(std::path::Path::new(path), BeatOptions::default()).unwrap();
    assert_eq!(found.grid, "uniform");
    assert!((124.0..125.2).contains(&found.bpm), "bpm {}", found.bpm);
    assert!(found.confidence >= 0.55, "confidence {}", found.confidence);
    let step = 60.0 / found.bpm;
    for (k, &t) in found.beats.iter().enumerate() {
        let on_grid = found.beats[0] + k as f64 * step;
        assert!(
            (t - on_grid).abs() < 0.001,
            "beat {k} at {t} is off its grid"
        );
    }
    // The hint agrees, and its half is the half.
    let hinted = beats::analyse_file(
        std::path::Path::new(path),
        BeatOptions {
            meter: 4,
            bpm_hint: Some(124.0),
        },
    )
    .unwrap();
    assert!(
        (hinted.bpm - found.bpm).abs() < 0.3,
        "hinted {}",
        hinted.bpm
    );
}
