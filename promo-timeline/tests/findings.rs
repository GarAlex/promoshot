//! What validate says about a beat-cut edit, slice 7 (docs/BEAT-SYNC.md §7):
//! every warning the request names, and the resolved times beside their
//! musical forms.

use promo_timeline::report::file_report_with;
use promo_timeline::BeatGrids;
use serde_json::{json, Value};

/// 120 BPM from 0 s, 30 fps; `layers` over a background.
fn project(layers: Value, song: Value, settings: Value) -> String {
    let mut all = vec![
        json!({"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
                              "isEnabled": true, "startTime": 0, "duration": 4,
                              "keyframes": []}),
    ];
    all.extend(layers.as_array().unwrap().iter().cloned());
    let mut compositions = json!({"canvasWidth": 160, "canvasHeight": 90, "fps": 30,
                                  "beatClock": {"resourceID": "song", "startTime": 0}});
    for (k, v) in settings.as_object().unwrap() {
        compositions[k] = v.clone();
    }
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": compositions,
        "layers": all,
        "resources": [song]
    })
    .to_string()
}

fn song(extra: Value) -> Value {
    let mut song = json!({"id": "song", "kind": "audio", "filename": "s.wav",
                          "displayName": "S", "addedAt": 0, "duration": 30,
                          "beatGrid": {"bpm": 120}});
    for (k, v) in extra.as_object().unwrap() {
        song[k] = v.clone();
    }
    song
}

fn caption(id: &str, start: f64, duration: f64) -> Value {
    json!({"id": id, "name": id, "sortIndex": 1, "kind": "caption", "isEnabled": true,
           "startTime": start, "duration": duration, "captionText": "x", "keyframes": []})
}

fn warnings(file: &str, grids: &BeatGrids) -> Vec<String> {
    let report = file_report_with(file, None, grids).unwrap();
    report.messages()
}

/// A cut off the grid by more than half a frame names the nearest beat;
/// one within half a frame, and every layer without a clock, says nothing.
#[test]
fn off_grid_cut_names_the_nearest_beat() {
    let file = project(
        json!([caption("a", 1.1, 1.0), caption("b", 2.01, 1.0)]),
        song(json!({})),
        json!({}),
    );
    let said = warnings(&file, &BeatGrids::default());
    assert!(
        said.contains(
            &"layer \"a\" at 1.10s: starts 0.10 s off the beat grid — nearest is \
                        beat 3 at 1.00 s"
                .to_string()
        ),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|m| m.contains("\"a\" at 2.10s: ends 0.10 s off")),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|m| m.contains("\"b\" at")),
        "within half a frame: {said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|m| m.contains("Ground") && m.contains("off the beat grid")),
        "a background is not a cut"
    );
    let clockless = project(json!([caption("a", 1.1, 1.0)]), song(json!({})), json!({}))
        .replace(r#""beatClock":{"resourceID":"song","startTime":0},"#, "");
    assert!(!warnings(&clockless, &BeatGrids::default())
        .iter()
        .any(|m| m.contains("off the beat grid")));
}

/// A slide shorter than one beat is said, with the beat's length.
#[test]
fn short_slide_warning() {
    let file = project(json!([caption("s9", 1.0, 0.3)]), song(json!({})), json!({}));
    let said = warnings(&file, &BeatGrids::default());
    assert!(
        said.contains(
            &"layer \"s9\": lasts 0.30 s, shorter than one beat (0.50 s at 120 BPM)".to_string()
        ),
        "{said:?}"
    );
}

/// A beat-relative value with no clock is resolved on the stand-in grid,
/// and said.
#[test]
fn no_clock_warning() {
    let file = project(
        json!([{"id": "s1", "name": "s1", "sortIndex": 1, "kind": "caption", "isEnabled": true,
                "startTime": 0, "duration": {"beats": 2}, "captionText": "x", "keyframes": []}]),
        song(json!({})),
        json!({}),
    )
    .replace(r#""beatClock":{"resourceID":"song","startTime":0},"#, "");
    let said = warnings(&file, &BeatGrids::default());
    assert!(
        said.contains(
            &"layer \"s1\": duration {\"beats\":2} has no beat clock — resolved \
                        against 120 BPM from 0 s; set compositionSettings.beatClock to the music"
                .to_string()
        ),
        "{said:?}"
    );
}

/// The clock's song ending before the timeline is said.
#[test]
fn audio_shorter_than_timeline_warning() {
    let file = project(
        json!([caption("a", 0.0, 4.0)]),
        song(json!({"duration": 3.0})),
        json!({}),
    );
    let said = warnings(&file, &BeatGrids::default());
    assert!(
        said.contains(
            &"the beat clock's audio \"song\" ends at 3.0 s but the timeline runs to \
                        4.0 s — the grid after 3.0 s is extrapolated"
                .to_string()
        ),
        "{said:?}"
    );
}

/// A detection the tracker was unsure of is said — unless the grid is
/// stated, which overrides it.
#[test]
fn low_confidence_warning() {
    let measured = song(json!({}));
    let mut measured = measured.as_object().unwrap().clone();
    measured.remove("beatGrid");
    let file = project(
        json!([caption("a", 0.0, 4.0)]),
        Value::Object(measured),
        json!({}),
    );
    let mut grids = BeatGrids::default();
    grids.by_resource.insert(
        "song".into(),
        serde_json::from_value(json!({"bpm": 120})).unwrap(),
    );
    grids.confidence.insert("song".into(), 0.32);
    assert!(warnings(&file, &grids).contains(
        &"beat detection for \"song\" is unsure (confidence 0.32) — set beatGrid by hand"
            .to_string()
    ));
    let stated = project(json!([caption("a", 0.0, 4.0)]), song(json!({})), json!({}));
    assert!(!warnings(&stated, &grids)
        .iter()
        .any(|m| m.contains("unsure")));
}

/// Validate lists every musical value beside the seconds it resolved to,
/// in its text and its JSON, with the clocks.
#[test]
fn validate_lists_resolved_times() {
    let file = project(
        json!([{"id": "s1", "name": "s1", "sortIndex": 1, "kind": "caption", "isEnabled": true,
                "startTime": {"beat": 2}, "duration": {"beats": 4}, "captionText": "x",
                "keyframes": []},
               {"id": "s2", "name": "s2", "sortIndex": 2, "kind": "caption", "isEnabled": true,
                "startTime": 0, "duration": 1, "after": "s1", "captionText": "y",
                "keyframes": []}]),
        song(json!({})),
        json!({}),
    );
    let report = file_report_with(&file, None, &BeatGrids::default()).unwrap();
    let text = report.text();
    assert!(text.contains("\n\nMusical times\n"), "{text}");
    assert!(
        text.contains("layers[s1].duration    {\"beats\":4}  → 2.000 s"),
        "{text}"
    );
    assert!(
        text.contains("layers[s1].startTime   {\"beat\":2}   → 0.500 s"),
        "{text}"
    );
    assert!(
        text.contains("layers[s2].startTime   after        → 2.500 s"),
        "{text}"
    );
    let json = report.json();
    let rows = json["resolvedTimes"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["at"] == "layers[s1].duration"
        && r["value"] == json!({"beats": 4})
        && r["seconds"] == 2.0));
    assert_eq!(json["beatClocks"][0]["resource"], "song");
    assert_eq!(json["beatClocks"][0]["source"], "beatGrid");
    assert_eq!(json["beatClocks"][0]["bpm"], 120.0);
}

/// A pulse over a keyed track says it reshapes it at beat resolution; a
/// file carrying the tools' in-memory keys is told they are ignored.
#[test]
fn pulse_and_internal_key_warnings() {
    let file = project(
        json!([{"id": "a", "name": "A", "sortIndex": 1, "kind": "caption", "isEnabled": true,
                "startTime": 0, "duration": 4, "captionText": "x",
                "pulse": {"property": "opacity"},
                "timeSpecs": {"duration": {"spec": "{\"beats\":8}", "seconds": 4, "label": "8 beats"}},
                "keyframes": [{"id": "o", "time": 0, "opacity": 1}]}]),
        song(json!({})),
        json!({}),
    );
    let said = warnings(&file, &BeatGrids::default());
    assert!(said.contains(
        &"pulse on layer \"A\" reshapes keyed opacity keyframes at beat resolution".to_string()
    ));
    assert!(
        said.iter()
            .any(|m| m.starts_with("the file holds `timeSpecs`")),
        "{said:?}"
    );
}

/// A grid point before beat 1 is named from the music, never as a negative
/// beat: inside the song, the pickup so many beats before beat 1. An edge
/// before the song plays — or at the composition's start — is not judged.
#[test]
fn points_before_beat_one_are_named_from_the_music() {
    // The song plays from 1.5 s; 120 BPM, beat 1 a second into it (2.5 s).
    let file = project(
        json!([caption("early", 0.2, 3.0), caption("pickup", 1.9, 1.0)]),
        song(json!({"beatGrid": {"bpm": 120, "offset": 1.0}})),
        json!({"beatClock": {"resourceID": "song", "startTime": 1.5}}),
    );
    let said = warnings(&file, &BeatGrids::default());
    assert!(said.iter().all(|m| !m.contains("beat -")), "{said:?}");
    // Before the song plays there is no beat to be off.
    assert!(
        !said.iter().any(|m| m.contains("\"early\" at 0.20s")),
        "{said:?}"
    );
    let pickup = said
        .iter()
        .find(|m| m.contains("\"pickup\" at 1.90s"))
        .expect("the pickup edge is named");
    assert!(
        pickup.contains("the pickup 1 beat before beat 1, at 2.00 s"),
        "{pickup}"
    );
}

/// A layer opening the composition at 0 s is not a cut: an intro there is
/// never "off the beat", wherever the song's first beat falls.
#[test]
fn the_composition_start_is_not_a_cut() {
    let file = project(
        json!([caption("intro", 0.0, 2.0), caption("late", 1.1, 1.0)]),
        song(json!({"beatGrid": {"bpm": 120, "offset": 0.3}})),
        json!({}),
    );
    let said = warnings(&file, &BeatGrids::default());
    assert!(
        !said.iter().any(|m| m.contains("\"intro\" at 0.00s")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|m| m.contains("\"late\" at 1.10s")),
        "a cut inside the song is still judged: {said:?}"
    );
}
