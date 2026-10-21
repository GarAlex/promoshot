//! Snapping a layer edge to the beat grid, and naming where a time falls on
//! it (docs/BEAT-SYNC.md §6.3, slice 9): the apps' editor writes what these
//! answer, so the rounding is the core's.

use promo_model::musical::{MusicalTime, SnapUnit};
use promo_model::ProjectMetadata;
use promo_timeline::musical::{bars_beats, snap_edge, Edge};
use promo_timeline::BeatGrids;
use serde_json::json;

/// 120 BPM, the first downbeat at 0.25 s: a beat is 0.5 s, a bar 2 s.
fn project(clock: bool) -> ProjectMetadata {
    let mut settings = json!({"canvasWidth": 160, "canvasHeight": 90, "fps": 30});
    if clock {
        settings["beatClock"] = json!({"resourceID": "song", "startTime": 0});
    }
    let file = json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 8, "videoDuration": 8, "subtitles": [],
        "compositionSettings": settings,
        "resources": [{"id": "song", "kind": "audio", "filename": "s.wav", "displayName": "S",
                       "addedAt": 0, "duration": 30,
                       "beatGrid": {"bpm": 120, "offset": 0.25}}],
        "layers": [{"id": "a", "name": "A", "sortIndex": 0, "kind": "caption", "isEnabled": true,
                    "startTime": 1.1, "duration": 1.3, "captionText": "x", "keyframes": []}]
    });
    ProjectMetadata::from_json(&file.to_string()).unwrap()
}

#[test]
fn a_start_snaps_to_the_nearest_beat_or_bar() {
    let meta = project(true);
    let grids = BeatGrids::default();
    let beat = snap_edge(&meta, &grids, "a", Edge::Start, SnapUnit::Beat).unwrap();
    assert_eq!(
        beat.value,
        MusicalTime::Beat(3.0),
        "1.1 s is nearest beat 3 (1.25 s)"
    );
    assert_eq!(beat.seconds, 1.25);
    assert_eq!(beat.label, "beat 3");
    let bar = snap_edge(&meta, &grids, "a", Edge::Start, SnapUnit::Downbeat).unwrap();
    assert_eq!(
        bar.value,
        MusicalTime::Bar(1.0),
        "bar 1 (0.25 s) is nearer than bar 2"
    );
    assert_eq!(bar.seconds, 0.25);
    assert_eq!(bar.label, "bar 1");
}

#[test]
fn an_end_snaps_to_a_position_after_the_start() {
    let meta = project(true);
    let grids = BeatGrids::default();
    // The end at 2.4 s: nearest beat 5 (2.25 s), a length of 1.15 s.
    let beat = snap_edge(&meta, &grids, "a", Edge::End, SnapUnit::Beat).unwrap();
    assert_eq!(beat.value, MusicalTime::Beat(5.0));
    assert!((beat.seconds - 1.15).abs() < 1e-9, "{}", beat.seconds);
    // Nearest bar start to 2.4 s is bar 2 (2.25 s).
    let bar = snap_edge(&meta, &grids, "a", Edge::End, SnapUnit::Bar).unwrap();
    assert_eq!(bar.value, MusicalTime::Bar(2.0));
    // A layer too short to reach a grid point goes to the next one, never
    // onto or before its start.
    let mut short = project(true);
    short.layers.as_mut().unwrap()[0].start_time = 2.25;
    short.layers.as_mut().unwrap()[0].duration = Some(0.1);
    let next = snap_edge(&short, &grids, "a", Edge::End, SnapUnit::Bar).unwrap();
    assert_eq!(next.value, MusicalTime::Bar(3.0));
    assert!((next.seconds - 2.0).abs() < 1e-9);
}

#[test]
fn without_a_clock_there_is_nothing_to_snap_to() {
    let meta = project(false);
    let err = snap_edge(
        &meta,
        &BeatGrids::default(),
        "a",
        Edge::Start,
        SnapUnit::Beat,
    )
    .unwrap_err();
    assert!(err.contains("no beat clock"), "{err}");
    assert!(snap_edge(
        &project(true),
        &BeatGrids::default(),
        "zz",
        Edge::Start,
        SnapUnit::Beat
    )
    .is_err());
    assert_eq!(bars_beats(&meta, &BeatGrids::default(), "a", 1.0), None);
}

#[test]
fn bars_beats_names_a_time_on_the_grid() {
    let meta = project(true);
    let grids = BeatGrids::default();
    assert_eq!(
        bars_beats(&meta, &grids, "a", 0.25).as_deref(),
        Some("bar 1 · beat 1")
    );
    assert_eq!(
        bars_beats(&meta, &grids, "a", 2.75).as_deref(),
        Some("bar 2 · beat 2")
    );
    assert_eq!(
        bars_beats(&meta, &grids, "a", 1.0).as_deref(),
        Some("bar 1 · beat 2.5")
    );
    assert_eq!(
        bars_beats(&meta, &grids, "a", 0.0).as_deref(),
        Some("0.5 beats before bar 1")
    );
}

/// A snapped value written into the file resolves to the seconds the snap
/// said — the editor's door and the resolver agree.
#[test]
fn a_snapped_value_resolves_to_its_seconds() {
    let meta = project(true);
    let grids = BeatGrids::default();
    let start = snap_edge(&meta, &grids, "a", Edge::Start, SnapUnit::Beat).unwrap();
    let end = snap_edge(&meta, &grids, "a", Edge::End, SnapUnit::Bar).unwrap();
    let mut file: serde_json::Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    file["layers"][0]["startTime"] = start.value.to_value();
    file["layers"][0]["duration"] = end.value.to_value();
    let mut written = ProjectMetadata::from_json(&file.to_string()).unwrap();
    promo_timeline::resolve_timing(&mut written, &grids);
    let layer = &written.layers.as_ref().unwrap()[0];
    assert_eq!(layer.start_time, start.seconds);
    // The end snapped from the old start; it stays on bar 2 from the new one.
    let bar_two = 2.25;
    assert!((layer.start_time + layer.duration.unwrap() - bar_two).abs() < 1e-9);
}
