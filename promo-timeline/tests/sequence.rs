//! `sequence` and `after`, slice 3 (docs/BEAT-SYNC.md §5): layers back to
//! back by list, or one after another by id, joined into the one timing
//! pass with musical values and attachments.

use promo_model::ProjectMetadata;
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

fn project(settings: Value, layers: Value) -> Value {
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 30, "videoDuration": 30, "subtitles": [],
        "compositionSettings": settings, "layers": layers,
        "resources": [{"id": "song", "kind": "audio", "filename": "song.m4a",
                       "displayName": "Song", "addedAt": 0, "duration": 60,
                       "beatGrid": {"bpm": 120}}]
    })
}

fn image(id: &str, sort: i64, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": sort, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 1,
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn clock() -> Value {
    json!({"resourceID": "song", "startTime": 0})
}

fn resolved(value: &Value) -> (ProjectMetadata, TimingReport) {
    let mut meta = ProjectMetadata::from_json(&value.to_string()).expect("decodes");
    let report = resolve_timing(&mut meta, &BeatGrids::default());
    (meta, report)
}

fn window(meta: &ProjectMetadata, id: &str) -> (f64, f64) {
    let layer = promo_model::nesting::all_layers(meta)
        .into_iter()
        .find(|l| l.id == id)
        .expect("layer");
    (layer.start_time, layer.duration.unwrap_or(f64::NAN))
}

/// The list lays its layers back to back, each for its length — beats,
/// bars, seconds, a time value, or its own duration — from the first
/// member's own start.
#[test]
fn sequence_lays_layers_back_to_back() {
    let mut settings = json!({"beatClock": clock()});
    settings["sequence"] = json!([
        {"layer": "c", "beats": 4},
        {"layer": "a", "bars": 1},
        {"layer": "b", "duration": {"beats": 1}},
        {"layer": "d"}
    ]);
    let file = project(
        settings,
        json!([
            image("a", 0, json!({})),
            image("b", 1, json!({})),
            image("c", 2, json!({"startTime": {"beat": 2}})),
            image("d", 3, json!({"duration": 0.75})),
        ]),
    );
    let (meta, report) = resolved(&file);
    // Edges set in seconds off the beat are said, and nothing else.
    assert!(
        report.problems.iter().all(|p| matches!(
            p,
            TimingProblem::OffGrid { .. } | TimingProblem::ShorterThanABeat { .. }
        )),
        "{:?}",
        report.problems
    );
    assert_eq!(window(&meta, "c"), (0.5, 2.0));
    assert_eq!(window(&meta, "a"), (2.5, 2.0));
    assert_eq!(window(&meta, "b"), (4.5, 0.5));
    assert_eq!(window(&meta, "d"), (5.0, 0.75), "its own duration");
    assert!(report
        .resolved
        .iter()
        .any(|r| r.at == "layers[a].startTime" && r.spec == "sequence"));
}

/// With a clock, a cut given in seconds snaps to the nearest beat, and so
/// does the first member's own start; without one, seconds are seconds.
#[test]
fn sequence_seconds_cuts_snap_to_the_grid() {
    let layers = json!([
        image("a", 0, json!({"startTime": 0.2})),
        image("b", 1, json!({})),
    ]);
    let sequence = json!([{"layer": "a", "seconds": 1.3}, {"layer": "b", "seconds": 0.6}]);
    let (meta, _) = resolved(&project(
        json!({"beatClock": clock(), "sequence": sequence}),
        layers.clone(),
    ));
    assert_eq!(window(&meta, "a"), (0.0, 1.5));
    assert_eq!(window(&meta, "b"), (1.5, 0.5));
    let (meta, _) = resolved(&project(json!({"sequence": sequence}), layers));
    assert_eq!(window(&meta, "a"), (0.2, 1.3));
    assert!((window(&meta, "b").0 - 1.5).abs() < 1e-12);
    assert!((window(&meta, "b").1 - 0.6).abs() < 1e-12);
}

/// `after` names a layer by id, wherever it sits in the stack, plus an
/// offset in seconds or beats; a chain of them follows through.
#[test]
fn after_names_a_layer_by_id_across_the_stack() {
    let file = project(
        json!({"beatClock": clock()}),
        json!([
            image("top", 9, json!({"after": "mid"})),
            image("low", 0, json!({"startTime": 1.0, "duration": 2.0})),
            image(
                "mid",
                4,
                json!({"after": {"layer": "low", "offset": {"beats": 1}}})
            ),
            image(
                "off",
                5,
                json!({"after": {"layer": "low", "offset": -0.25}})
            ),
        ]),
    );
    let (meta, report) = resolved(&file);
    // Edges set in seconds off the beat are said, and nothing else.
    assert!(
        report.problems.iter().all(|p| matches!(
            p,
            TimingProblem::OffGrid { .. } | TimingProblem::ShorterThanABeat { .. }
        )),
        "{:?}",
        report.problems
    );
    assert_eq!(window(&meta, "mid").0, 3.5);
    assert_eq!(window(&meta, "top").0, 4.5);
    assert_eq!(window(&meta, "off").0, 2.75);
}

/// An `after` layer can follow a layer an attachment placed, and an
/// attachment can anchor to an `after` layer.
#[test]
fn after_can_follow_an_attached_layer() {
    let file = project(
        json!({}),
        json!([
            image("a", 0, json!({"startTime": 1.0, "duration": 1.0})),
            image(
                "b",
                1,
                json!({"timing": {"start": {"from": "previousEnd", "offset": 0.5}},
                                 "duration": 1.0})
            ),
            image("c", 5, json!({"after": "b", "duration": 2.0})),
            image(
                "d",
                6,
                json!({"timing": {"start": {"from": "previousEnd", "offset": 0}}})
            ),
        ]),
    );
    let (meta, report) = resolved(&file);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(window(&meta, "b").0, 2.5);
    assert_eq!(window(&meta, "c").0, 3.5);
    assert_eq!(
        window(&meta, "d").0,
        5.5,
        "an anchor on an after layer's end"
    );
}

/// A circle of `after` places nobody: each layer is named and keeps its
/// stored start; the rest of the project still resolves.
#[test]
fn after_cycle_is_reported_and_keeps_numbers() {
    let file = project(
        json!({}),
        json!([
            image("a", 0, json!({"startTime": 1.0, "after": "c"})),
            image("b", 1, json!({"startTime": 2.0, "after": "a"})),
            image("c", 2, json!({"startTime": 3.0, "after": "b"})),
            image("d", 3, json!({"after": "a"})),
            image("e", 4, json!({"after": "ghost"})),
        ]),
    );
    let (meta, report) = resolved(&file);
    for (id, start) in [("a", 1.0), ("b", 2.0), ("c", 3.0)] {
        assert_eq!(window(&meta, id).0, start, "{id} moved");
    }
    let circular = report
        .problems
        .iter()
        .filter(|p| matches!(p, TimingProblem::AfterCircular { .. }))
        .count();
    assert_eq!(circular, 3, "{:?}", report.problems);
    assert!(report.problems.iter().all(TimingProblem::is_break));
    assert_eq!(
        window(&meta, "d").0,
        2.0,
        "following a stuck layer still works"
    );
    assert!(report
        .problems
        .iter()
        .any(|p| matches!(p, TimingProblem::UnknownLayer { id, .. } if id == "ghost")));
}

/// One layer in the sequence and also with `after` or `timing.start`: the
/// sequence places it, and validate says so as a break; `after` beats a
/// start anchor the same way. A layer listed twice counts once.
#[test]
fn sequence_wins_over_after_and_timing_and_says_so() {
    let file = project(
        json!({"sequence": [{"layer": "a", "seconds": 1}, {"layer": "b", "seconds": 1},
                            {"layer": "a", "seconds": 5}]}),
        json!([
            image("a", 0, json!({"startTime": 2.0})),
            image(
                "b",
                1,
                json!({"after": "c",
                                 "timing": {"start": {"from": "previousEnd", "offset": 4}}})
            ),
            image("c", 2, json!({"startTime": 10.0, "duration": 1.0})),
            image(
                "d",
                3,
                json!({"after": "c",
                                 "timing": {"start": {"from": "previousStart", "offset": 0}}})
            ),
        ]),
    );
    let (meta, report) = resolved(&file);
    assert_eq!(window(&meta, "a"), (2.0, 1.0));
    assert_eq!(window(&meta, "b"), (3.0, 1.0), "the sequence places b");
    assert_eq!(window(&meta, "d").0, 11.0, "after places d");
    let said: Vec<String> = report.problems.iter().map(|p| p.to_string()).collect();
    for expected in [
        "\"b\" is in the sequence and also has after",
        "\"b\" is in the sequence and also has timing.start",
        "\"d\" has both after and timing.start — after places it",
        "\"a\" is in the sequence twice",
    ] {
        assert!(
            said.iter().any(|s| s.contains(expected)),
            "{expected}: {said:?}"
        );
    }
    assert!(report.problems.iter().all(TimingProblem::is_break));
    let text = format!(
        "{:?}",
        promo_timeline::report::file_report(&file.to_string(), None).unwrap()
    );
    assert!(text.contains("the sequence places it"), "{text}");
}

fn sequenced() -> Value {
    project(
        json!({"beatClock": clock(),
               "sequence": [{"layer": "a", "beats": 2}, {"layer": "b", "beats": 2},
                            {"layer": "c", "beats": 2}]}),
        json!([
            image("a", 0, json!({})),
            image("b", 1, json!({})),
            image("c", 2, json!({}))
        ]),
    )
}

fn assert_sequence(file: &Value, expected: Value) {
    let found = &file["compositionSettings"]["sequence"];
    assert!(
        promo_model::json_semantically_equal(found, &expected),
        "{found} is not {expected}"
    );
}

/// A sequenced layer someone moves leaves the sequence when the file is
/// written; the rest stay in it.
#[test]
fn moved_sequenced_layer_leaves_the_sequence_on_save() {
    let (mut meta, _) = resolved(&sequenced());
    meta.layers.as_mut().unwrap()[1].start_time = 7.0;
    let file: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_sequence(
        &file,
        json!([{"layer": "a", "beats": 2}, {"layer": "c", "beats": 2}]),
    );
    assert_eq!(file["layers"][1]["startTime"], 7.0);
    // Resolution says the same, before any save: the moved layer stays
    // where it was put, and the next member closes the gap.
    let (mut meta, _) = resolved(&sequenced());
    meta.layers.as_mut().unwrap()[1].start_time = 7.0;
    resolve_timing(&mut meta, &BeatGrids::default());
    assert_eq!(window(&meta, "b").0, 7.0);
    assert_eq!(window(&meta, "c").0, 1.0);
}

/// One whose length someone changes stays in the sequence, its entry
/// rewritten as that many seconds.
#[test]
fn resized_sequenced_layer_rewrites_its_entry() {
    let (mut meta, _) = resolved(&sequenced());
    meta.layers.as_mut().unwrap()[1].duration = Some(1.5);
    let file: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_sequence(
        &file,
        json!([{"layer": "a", "beats": 2}, {"layer": "b", "seconds": 1.5},
               {"layer": "c", "beats": 2}]),
    );
    // A moved `after` layer loses its `after` on save.
    let (mut meta, _) = resolved(&project(
        json!({}),
        json!([
            image("a", 0, json!({})),
            image("b", 1, json!({"after": "a"}))
        ]),
    ));
    meta.layers.as_mut().unwrap()[1].start_time = 4.0;
    let file: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert!(file["layers"][1].get("after").is_none(), "{file}");
    // ...and a re-resolve before the save lets it stay where it was put.
    resolve_timing(&mut meta, &BeatGrids::default());
    assert_eq!(window(&meta, "b").0, 4.0);
    assert!(meta.layers.as_ref().unwrap()[1].after.is_none());
    // An untouched one keeps it, and the rung says the musical reader.
    let (meta, _) = resolved(&project(
        json!({}),
        json!([
            image("a", 0, json!({})),
            image("b", 1, json!({"after": "a"}))
        ]),
    ));
    let file: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_eq!(file["layers"][1]["after"], "a");
    assert_eq!(meta.minimum_reader_version(), 49);
}

/// The pass is still idempotent with sequence and after in it, and the
/// file form survives the trip.
#[test]
fn sequence_and_after_resolve_idempotently() {
    let mut file = sequenced();
    file["layers"].as_array_mut().unwrap().push(image(
        "z",
        9,
        json!({"after": {"layer": "c", "offset": {"beats": 1}}}),
    ));
    let (mut meta, _) = resolved(&file);
    let once = meta.clone();
    resolve_timing(&mut meta, &BeatGrids::default());
    assert_eq!(meta, once);
    let mut again = ProjectMetadata::from_json(&once.to_json().unwrap()).unwrap();
    resolve_timing(&mut again, &BeatGrids::default());
    assert_eq!(again, once);
    assert_eq!(window(&once, "z").0, 3.5);
}
