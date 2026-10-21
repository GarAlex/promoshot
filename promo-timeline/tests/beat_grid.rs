//! The beat grid and the one timing pass, slice 2 (docs/BEAT-SYNC.md §2,
//! §9 S2): musical values resolve to seconds against the clock's grid, in
//! the order the design gives, bit-identically for a stated grid.

use promo_model::ProjectMetadata;
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

fn project(settings: Value, layers: Value, resources: Value) -> Value {
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 30, "videoDuration": 30, "subtitles": [],
        "compositionSettings": settings, "layers": layers, "resources": resources
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

fn song(grid: Value) -> Value {
    let mut resource = json!({"id": "song", "kind": "audio", "filename": "song.m4a",
                              "displayName": "Song", "addedAt": 0, "duration": 60});
    if !grid.is_null() {
        resource["beatGrid"] = grid;
    }
    resource
}

fn resolved(value: &Value) -> (ProjectMetadata, TimingReport) {
    let mut meta = ProjectMetadata::from_json(&value.to_string()).expect("decodes");
    let report = resolve_timing(&mut meta, &BeatGrids::default());
    (meta, report)
}

fn layer<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a promo_model::ProjectLayer {
    promo_model::nesting::all_layers(meta)
        .into_iter()
        .find(|l| l.id == id)
        .expect("layer")
}

/// AC3: a stated grid times the edit bit-identically — every number pinned
/// as its IEEE bits, computed once from `clock + (offset + k·60/bpm)/speed`.
#[test]
fn override_grid_is_bit_identical() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 1.5}}),
        json!([image(
            "s1",
            0,
            json!({
                "startTime": {"beat": 5}, "duration": {"bars": 2},
                "fadeIn": {"beats": 0.5}, "fadeOut": {"beats": 1},
                "keyframes": [{"id": "k", "time": {"beats": 2},
                               "transitionDuration": {"beats": 0.5}}]
            })
        )]),
        json!([song(
            json!({"bpm": 131.2, "offset": 0.42, "timeSignature": [4, 4]})
        )]),
    );
    let (meta, report) = resolved(&file);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(report.missing_grids.is_empty());
    let s1 = layer(&meta, "s1");
    let bits = |v: f64| v.to_bits();
    assert_eq!(
        bits(s1.start_time),
        0x400dfe805fe805ff,
        "start {}",
        s1.start_time
    );
    assert_eq!(bits(s1.duration.unwrap()), 0x400d44aed44aed45, "duration");
    assert_eq!(bits(s1.fade_in.unwrap()), 0x3fcd44aed44aed40, "fadeIn");
    assert_eq!(bits(s1.fade_out.unwrap()), 0x3fdd44aed44aed50, "fadeOut");
    assert_eq!(
        bits(s1.keyframes[0].time),
        0x3fed44aed44aed44,
        "keyframe time"
    );
    assert_eq!(
        bits(s1.keyframes[0].transition_duration),
        0x3fcd44aed44aed40,
        "keyframe ramp"
    );
    // The side map holds the same numbers, and the report lists them.
    let specs = s1.time_specs.as_ref().unwrap();
    assert_eq!(specs["duration"].seconds.to_bits(), 0x400d44aed44aed45);
    assert_eq!(report.resolved.len(), 6);
    assert!(report
        .resolved
        .iter()
        .any(|r| r.at == "layers[s1].startTime" && r.spec == r#"{"beat":5}"#));
}

/// Beats and bars are counted along the grid from the resolved start, and a
/// bar is as many beats as the time signature says.
#[test]
fn beats_and_bars_measure_along_the_grid() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            image(
                "a",
                0,
                json!({"startTime": {"beat": 3}, "duration": {"beats": 4}})
            ),
            image("b", 1, json!({"startTime": 1.25, "duration": {"bars": 1}})),
            image(
                "c",
                2,
                json!({"startTime": {"bar": 2}, "duration": {"bar": 4}})
            ),
        ]),
        json!([song(json!({"bpm": 90, "timeSignature": [3, 4]}))]),
    );
    let (meta, report) = resolved(&file);
    // A layer placed on seconds off the beat is said, and nothing else.
    assert!(
        report
            .problems
            .iter()
            .all(|p| matches!(p, TimingProblem::OffGrid { .. })),
        "{:?}",
        report.problems
    );
    let beat = 60.0 / 90.0;
    let a = layer(&meta, "a");
    assert!((a.start_time - 2.0 * beat).abs() < 1e-12);
    assert!((a.duration.unwrap() - 4.0 * beat).abs() < 1e-12);
    // A seconds start, a bar of three beats from wherever it is.
    let b = layer(&meta, "b");
    assert_eq!(b.start_time, 1.25);
    assert!((b.duration.unwrap() - 3.0 * beat).abs() < 1e-12);
    // A position as the duration is the END: bar 2 to bar 4 is two bars.
    let c = layer(&meta, "c");
    assert!((c.start_time - 3.0 * beat).abs() < 1e-12);
    assert!((c.duration.unwrap() - 6.0 * beat).abs() < 1e-12);
}

/// Positions are 1-based from the first downbeat; a beat before it is a
/// pickup; `{"beats": n}` as a start is `{"beat": n + 1}`.
#[test]
fn absolute_positions_are_one_based_with_pickups() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            image("one", 0, json!({"startTime": {"beat": 1}})),
            image("pickup", 1, json!({"startTime": {"beat": 0}})),
            image("bar2", 2, json!({"startTime": {"bar": 2}})),
            image("counted", 3, json!({"startTime": {"beats": 4}})),
            image(
                "snapped",
                4,
                json!({"startTime": {"snap": "beat", "near": 2.0}})
            ),
        ]),
        json!([song(json!({"bpm": 120, "offset": 1.0}))]),
    );
    let (meta, _) = resolved(&file);
    let start = |id: &str| layer(&meta, id).start_time;
    assert_eq!(start("one"), 1.0);
    assert_eq!(start("pickup"), 0.5);
    assert_eq!(start("bar2"), 3.0);
    assert_eq!(
        start("counted"),
        start("bar2"),
        "{{beats: 4}} is {{beat: 5}}"
    );
    assert_eq!(start("snapped"), 2.0);
}

/// A length field refuses a position: a break, and the stored seconds stand.
#[test]
fn lengths_reject_positions() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([image(
            "a",
            0,
            json!({"fadeIn": {"bar": 3}, "fadeOut": {"beats": 1}})
        )]),
        json!([song(json!({"bpm": 120}))]),
    );
    let (meta, report) = resolved(&file);
    let problem = report
        .problems
        .iter()
        .find(|p| matches!(p, TimingProblem::PositionForLength { .. }))
        .expect("refused");
    assert!(problem.is_break());
    assert!(
        problem
            .to_string()
            .contains("fadeIn {\"bar\":3} is a position"),
        "{problem}"
    );
    let a = layer(&meta, "a");
    assert_eq!(a.fade_in, Some(0.0), "the stored number stands");
    assert_eq!(a.fade_out, Some(0.5), "the length beside it resolves");
}

/// With no `startTime`, the clock is where the first enabled audio layer
/// playing the resource puts its source second 0: through its trim and
/// its speed.
#[test]
fn clock_follows_its_audio_layer_trim_and_speed() {
    let mut music = song(json!({"bpm": 120, "offset": 0.5}));
    music["trimStart"] = json!(1.0);
    music["speed"] = json!(2.0);
    let file = project(
        json!({"beatClock": {"resourceID": "song"}}),
        json!([
            {"id": "m", "name": "Music", "sortIndex": 0, "kind": "audio", "isEnabled": true,
             "startTime": 3.0, "resourceID": "song", "keyframes": []},
            image("a", 1, json!({"startTime": {"beat": 1}, "duration": {"beats": 2}})),
        ]),
        json!([music]),
    );
    let (meta, report) = resolved(&file);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    // Source 1.0 plays at 3.0, at 2x: source 0 is at 2.5; the downbeat at
    // source 0.5 lands at 2.75, and a beat (0.5 s of source) lasts 0.25 s.
    let a = layer(&meta, "a");
    assert_eq!(a.start_time, 2.75);
    assert_eq!(a.duration, Some(0.5));
    // A clock layer that itself starts on its beat is refused.
    let mut circular = file.clone();
    circular["layers"][0]["startTime"] = json!({"beat": 2});
    let (_, report) = resolved(&circular);
    assert!(report
        .problems
        .iter()
        .any(|p| matches!(p, TimingProblem::ClockOnItsOwnBeat { .. }) && p.is_break()));
}

/// A nested composition counts its own clock's beats — never its parent's
/// — and a layer's own clock overrides the scope's.
#[test]
fn nested_composition_uses_its_own_clock() {
    let mut fast = song(json!({"bpm": 240}));
    fast["id"] = json!("fast");
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            image("top", 0, json!({"duration": {"beats": 1}})),
            image(
                "own",
                1,
                json!({"duration": {"beats": 1},
                                   "beatClock": {"resourceID": "fast", "startTime": 0}})
            ),
        ]),
        json!([
            song(json!({"bpm": 60})),
            fast,
            {"id": "comp", "kind": "composition", "filename": "", "displayName": "C",
             "addedAt": 0,
             "composition": {"canvasWidth": 100, "canvasHeight": 100,
                             "beatClock": {"resourceID": "fast", "startTime": 0},
                             "layers": [image("inner", 0, json!({"duration": {"beats": 1}}))]}}
        ]),
    );
    let (meta, report) = resolved(&file);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(layer(&meta, "top").duration, Some(1.0));
    assert_eq!(layer(&meta, "own").duration, Some(0.25));
    assert_eq!(layer(&meta, "inner").duration, Some(0.25));
    // Without a clock of its own the composition is clockless, not the
    // parent's: the nominal grid, with a warning.
    let mut orphan = file.clone();
    orphan["resources"][2]["composition"]
        .as_object_mut()
        .unwrap()
        .remove("beatClock");
    let (meta, report) = resolved(&orphan);
    assert_eq!(layer(&meta, "inner").duration, Some(0.5));
    assert!(report
        .problems
        .iter()
        .any(|p| matches!(p, TimingProblem::NoBeatClock { layer, .. } if layer == "inner")));
}

/// No clock: musical values still lay out, on 120 BPM from 0 s, and each
/// one is named. A clock whose audio has no grid lays out the same way and
/// is listed as waiting for one.
#[test]
fn no_beat_clock_uses_the_nominal_grid_and_warns() {
    let file = project(
        json!({}),
        json!([image(
            "a",
            0,
            json!({"startTime": {"bar": 2}, "duration": {"beats": 3}})
        )]),
        json!([]),
    );
    let (meta, report) = resolved(&file);
    let a = layer(&meta, "a");
    assert_eq!(a.start_time, 2.0);
    assert_eq!(a.duration, Some(1.5));
    let warned: Vec<String> = report.problems.iter().map(|p| p.to_string()).collect();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(warned[0].contains("has no beat clock — resolved against 120 BPM from 0 s"));
    assert!(report.problems.iter().all(|p| !p.is_break()));
    // validate says so too, as a warning.
    let text = promo_timeline::report::file_report(&file.to_string(), None).unwrap();
    let all = format!("{text:?}");
    assert!(all.contains("has no beat clock"), "{all}");

    let waiting = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([image("a", 0, json!({"duration": {"beats": 3}}))]),
        json!([song(Value::Null)]),
    );
    let (meta, report) = resolved(&waiting);
    assert_eq!(layer(&meta, "a").duration, Some(1.5));
    assert_eq!(report.missing_grids, vec!["song".to_string()]);
    let all = format!(
        "{:?}",
        promo_timeline::report::file_report(&waiting.to_string(), None).unwrap()
    );
    assert!(all.contains("has no beat grid yet"), "{all}");
}

/// Resolving a resolved project changes nothing, and neither does the trip
/// through the file form and back.
#[test]
fn resolve_timing_is_idempotent() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0.3}}),
        json!([
            image(
                "a",
                0,
                json!({"startTime": {"beat": 2}, "duration": {"bars": 1},
                                 "fadeOut": {"beats": 0.5},
                                 "keyframes": [{"id": "k", "time": {"beats": 1}}]})
            ),
            image(
                "b",
                1,
                json!({"timing": {"start": {"from": "previousEnd", "offset": 0}},
                                 "duration": {"beats": 2}})
            ),
        ]),
        json!([song(json!({"bpm": 128, "offset": 0.1}))]),
    );
    let (mut meta, _) = resolved(&file);
    let once = meta.clone();
    resolve_timing(&mut meta, &BeatGrids::default());
    assert_eq!(meta, once, "a second pass moved something");
    let mut again = ProjectMetadata::from_json(&once.to_json().unwrap()).unwrap();
    resolve_timing(&mut again, &BeatGrids::default());
    assert_eq!(again, once, "fold, lift and resolve moved something");
}

/// A musical length and an attachment compose: a layer attached to the end
/// of a musical one starts there, an attached layer's musical end is
/// measured from where the attachment put it, and the next layer follows.
#[test]
fn musical_lengths_and_attachments_compose() {
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            image(
                "a",
                0,
                json!({"startTime": {"beat": 2}, "duration": {"beats": 4}})
            ),
            image(
                "b",
                1,
                json!({"timing": {"start": {"from": "previousEnd", "offset": 0}},
                                 "duration": {"bar": 3}})
            ),
            image(
                "c",
                2,
                json!({"timing": {"start": {"from": "previousEnd", "offset": 0.1}},
                                 "duration": 1})
            ),
        ]),
        json!([song(json!({"bpm": 120}))]),
    );
    let (meta, report) = resolved(&file);
    // A layer placed on seconds off the beat is said, and nothing else.
    assert!(
        report
            .problems
            .iter()
            .all(|p| matches!(p, TimingProblem::OffGrid { .. })),
        "{:?}",
        report.problems
    );
    assert_eq!(layer(&meta, "a").start_time, 0.5);
    assert_eq!(layer(&meta, "a").duration, Some(2.0));
    // b starts where a ends and lasts until bar 3 (4 s): its musical END
    // depends on where the attachment put its start — one more round.
    assert_eq!(layer(&meta, "b").start_time, 2.5);
    assert_eq!(layer(&meta, "b").duration, Some(1.5));
    assert_eq!(layer(&meta, "c").start_time, 4.1);
}

/// A value someone moved stays where they put it: its spec goes, and the
/// pass leaves the number alone.
#[test]
fn an_edited_value_is_not_resolved_back() {
    let file = project(
        json!({}),
        json!([image("a", 0, json!({"duration": {"beats": 4}}))]),
        json!([]),
    );
    let (mut meta, _) = resolved(&file);
    assert_eq!(layer(&meta, "a").duration, Some(2.0));
    meta.layers.as_mut().unwrap()[0].duration = Some(1.25);
    resolve_timing(&mut meta, &BeatGrids::default());
    let a = layer(&meta, "a");
    assert_eq!(a.duration, Some(1.25));
    assert!(a.time_specs.is_none(), "the edit flattened the value");
}
