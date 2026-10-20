//! The beat tracker, slice 4 (docs/BEAT-SYNC.md §3): synthesized click
//! tracks at 64, 96 and 128 BPM, whose truth is known to the sample.

mod support;

use promo_media::beats::{self, BeatMeter, BeatOptions};
use promo_model::musical::BeatAnalysis;
use support::click_track::{click_track, wav, OFFSET};

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
    assert_eq!(beats::load_cached(&dir, &song).unwrap().bpm, fresh.bpm);
    let _ = std::fs::remove_dir_all(&dir);
}
