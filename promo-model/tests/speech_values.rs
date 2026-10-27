//! Speech values in time fields, slice 6 (docs/SPEECH-SYNC.md §6): the
//! format. A speech value lifts aside on load and folds back on save
//! exactly as written, a malformed one names its path, and a file with one
//! needs the speech reader (rung 50) while `speechClock` alone stays 48.

use promo_model::ProjectMetadata;
use serde_json::{json, Value};

fn image(id: &str, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": 0, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 1,
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

/// Every speech form, on every field that takes one, at every depth.
fn speech_project() -> Value {
    json!({
        "id": "P", "name": "Speech", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 8, "videoDuration": 8, "subtitles": [],
        "compositionSettings": {"speechClock": {"layer": "vo"}},
        "layers": [
            image("s1", json!({"startTime": {"word": 3, "of": "intro", "text": "primes"},
                                "duration": {"sentence": 1, "edge": "end"}})),
            image("s2", json!({"startTime": {"word": "Euclid", "nth": 2, "offset": -0.1},
                                "keyframes": [{"id": "k1", "time": {"word": 5, "quantize": "beat"}}]})),
            image("s3", json!({"startTime": {"snap": "gap", "near": 4.2, "offset": {"beats": -0.5}}})),
            image("s4", json!({"during": {"sentence": 2, "of": "intro", "lead": 0.15, "hold": 0.25},
                                "speechClock": {"resourceID": "intro", "startTime": 0}})),
            {"id": "st", "name": "Stage", "sortIndex": 1, "kind": "stage", "isEnabled": true,
             "startTime": 0, "keyframes": [],
             "members": [image("m1", json!({"startTime": {"sentence": 2}}))]},
            {"id": "vo", "name": "VO", "sortIndex": 2, "kind": "audio", "isEnabled": true,
             "startTime": 0, "resourceID": "intro", "keyframes": []}
        ],
        "resources": [
            {"id": "intro", "kind": "audio", "filename": "intro.mp3", "displayName": "Intro",
             "addedAt": 0, "duration": 6,
             "speech": {"text": "Euclid proved it. Primes never end, said Euclid."}},
            {"id": "comp", "kind": "composition", "filename": "", "displayName": "Comp",
             "addedAt": 0,
             "composition": {"canvasWidth": 1920, "canvasHeight": 1080,
                             "speechClock": {"resourceID": "intro", "startTime": 1},
                             "layers": [image("n1", json!({"duration": {"word": 4, "edge": "start"}}))]}}
        ]
    })
}

const SPEECH_AT: &[&str] = &[
    "/layers/0/startTime",
    "/layers/0/duration",
    "/layers/1/startTime",
    "/layers/1/keyframes/0/time",
    "/layers/2/startTime",
    "/layers/4/members/0/startTime",
    "/resources/1/composition/layers/0/duration",
];

fn decode(value: &Value) -> ProjectMetadata {
    ProjectMetadata::from_json(&value.to_string()).expect("decodes")
}

/// The file the core writes holds every speech value exactly as written;
/// writing it again changes not one byte; the app form holds numbers.
#[test]
fn speech_values_round_trip_byte_for_byte() {
    let source = speech_project();
    let meta = decode(&source);
    let first = meta.to_json().expect("encodes");
    let written: Value = serde_json::from_str(&first).unwrap();
    for at in SPEECH_AT {
        assert!(
            promo_model::json_semantically_equal(
                written.pointer(at).expect(at),
                source.pointer(at).unwrap()
            ),
            "{at} did not come back as written: {:?}",
            written.pointer(at)
        );
    }
    for at in [
        "/compositionSettings/speechClock",
        "/layers/3/during",
        "/layers/3/speechClock",
        "/resources/1/composition/speechClock",
    ] {
        assert!(
            promo_model::json_semantically_equal(
                written.pointer(at).expect(at),
                source.pointer(at).unwrap()
            ),
            "{at} changed: {:?}",
            written.pointer(at)
        );
    }
    assert!(
        !first.contains("timeSpecs"),
        "the side map reached the file"
    );
    let again = ProjectMetadata::from_json(&first).unwrap();
    assert_eq!(again, meta);
    assert_eq!(again.to_json().unwrap(), first);
    // The app form: numbers, each spec beside its number.
    let app: Value = serde_json::from_str(&meta.to_app_json().unwrap()).unwrap();
    for at in SPEECH_AT {
        assert!(app.pointer(at).unwrap().is_number(), "{at} is not a number");
    }
    assert_eq!(
        app.pointer("/layers/0/timeSpecs/startTime/spec"),
        Some(&json!("{\"of\":\"intro\",\"text\":\"primes\",\"word\":3}"))
    );
    let back = ProjectMetadata::from_json(&app.to_string()).unwrap();
    assert_eq!(back.to_json().unwrap(), first);
}

/// A malformed speech value is refused, and the message says where.
#[test]
fn malformed_speech_value_names_its_path() {
    for (at, value, says) in [
        (
            "/layers/0/startTime",
            json!({"word": 3, "sentence": 1}),
            "layers[s1].startTime: needs exactly one",
        ),
        (
            "/layers/0/startTime",
            json!({"word": 0}),
            "layers[s1].startTime: \"word\" must be a whole number from 1",
        ),
        (
            "/layers/1/startTime",
            json!({"word": 3, "nth": 2}),
            "layers[s2].startTime: \"nth\" goes with a word given by its text",
        ),
        (
            "/layers/4/members/0/startTime",
            json!({"sentence": 2, "of": "intro", "edje": "end"}),
            "layers[st].members[m1].startTime: unknown key \"edje\"",
        ),
        (
            "/resources/1/composition/layers/0/duration",
            json!({"word": 4, "quantize": "beats"}),
            "resources[comp].composition.layers[n1].duration: \"quantize\" must be",
        ),
        (
            "/layers/1/keyframes/0/time",
            json!({"word": "Euclid", "text": "Euclid"}),
            "layers[s2].keyframes[k1].time: \"text\" guards an index",
        ),
    ] {
        let mut project = speech_project();
        *project.pointer_mut(at).unwrap() = value;
        let err = ProjectMetadata::from_json(&project.to_string()).unwrap_err();
        assert!(err.to_string().contains(says), "{at}: {err}");
        assert!(ProjectMetadata::unread_keys(&project.to_string()).is_err());
    }
}

/// A speech value or `during` needs the speech reader; `speechClock` alone
/// is ignorable (48); edited to seconds, the rung comes down.
#[test]
fn speech_file_stamps_50() {
    assert_eq!(promo_model::speech::SPEECH_READER, 50);
    // 51 since the vector kind (SVG-PLAN.md); speech files stay 50.
    assert_eq!(promo_model::READER_VERSION, 51);
    let plain = |layer: Value| {
        json!({
            "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
            "compositionSettings": {}, "layers": [layer]
        })
    };
    assert!(decode(&plain(image("a", json!({})))).minimum_reader_version() < 48);
    for (what, extra) in [
        (
            "a speech start",
            json!({"startTime": {"word": 2, "of": "vo"}}),
        ),
        ("a speech end", json!({"duration": {"sentence": 1}})),
        ("a snap", json!({"startTime": {"snap": "gap", "near": 1}})),
        (
            "a speech keyframe",
            json!({"keyframes": [{"id": "k", "time": {"word": "go"}}]}),
        ),
        ("during", json!({"during": {"sentence": 1, "of": "vo"}})),
    ] {
        assert_eq!(
            decode(&plain(image("a", extra))).minimum_reader_version(),
            50,
            "{what}"
        );
    }
    for (what, extra) in [
        (
            "a pulse on words",
            json!({"pulse": {"on": {"words": ["Euclid"], "of": "vo"}}}),
        ),
        (
            "a shake over a sentence",
            json!({"shake": {"on": "beat", "range": {"sentence": 2, "of": "vo"}}}),
        ),
    ] {
        assert_eq!(
            decode(&plain(image("a", extra))).minimum_reader_version(),
            50,
            "{what}"
        );
    }
    for sequence in [
        json!([{"layer": "a", "until": {"sentence": 2, "of": "vo"}}]),
        json!([{"layers": ["a"], "per": "sentence", "of": "vo"}]),
    ] {
        let mut file = plain(image("a", json!({})));
        file["compositionSettings"]["sequence"] = sequence.clone();
        assert_eq!(decode(&file).minimum_reader_version(), 50, "{sequence}");
    }
    let mut flashed = plain(image("a", json!({})));
    flashed["compositionSettings"]["flash"] = json!({"on": {"sentences": "start", "of": "vo"}});
    assert_eq!(
        decode(&flashed).minimum_reader_version(),
        50,
        "a flash on sentences"
    );
    let mut clocked = plain(image("a", json!({})));
    clocked["compositionSettings"]["speechClock"] = json!({"layer": "vo"});
    assert_eq!(
        decode(&clocked).minimum_reader_version(),
        48,
        "speechClock alone"
    );
    assert_eq!(
        decode(&plain(image("a", json!({"speechClock": {"layer": "vo"}}))))
            .minimum_reader_version(),
        48
    );
    assert_eq!(decode(&speech_project()).minimum_reader_version(), 50);
    // A beat-only file stays 49.
    assert_eq!(
        decode(&plain(image("a", json!({"duration": {"beats": 2}})))).minimum_reader_version(),
        49
    );
    // Edited to seconds, the value folds flat and the rung comes down.
    let mut meta = decode(&plain(image("a", json!({"startTime": {"word": 2}}))));
    meta.layers.as_mut().unwrap()[0].start_time = 1.5;
    assert!(meta.minimum_reader_version() < 48);
}

/// A synced reveal and `captions` need the speech reader; what speech sync
/// derives — `unitTimes`, `unitEnds`, a generator's `made` — never reaches
/// the file, and `sync` and `captions` come back as written.
#[test]
fn synced_reveal_and_captions_stamp_50_and_save_no_derived_numbers() {
    let file = json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {},
        "layers": [
            {"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio", "isEnabled": true,
             "startTime": 0, "resourceID": "intro", "keyframes": [],
             "captions": {"maxWords": 6, "skip": [1]}},
            {"id": "c", "name": "C", "sortIndex": 1, "kind": "caption", "isEnabled": true,
             "startTime": 0, "duration": 2, "captionText": "Hello there",
             "captionStyle": {"reveal": {"by": "word", "mode": "highlight",
                                         "sync": {"of": "intro", "words": [1, 2]}}},
             "keyframes": []}
        ]
    });
    let plain = |f: &Value| decode(f).minimum_reader_version();
    assert_eq!(plain(&file), 50);
    let mut no_captions = file.clone();
    no_captions["layers"][0]
        .as_object_mut()
        .unwrap()
        .remove("captions");
    assert_eq!(plain(&no_captions), 50, "a synced reveal alone");
    let mut no_sync = no_captions.clone();
    no_sync["layers"][1]["captionStyle"]["reveal"]
        .as_object_mut()
        .unwrap()
        .remove("sync");
    assert!(plain(&no_sync) < 50);

    let mut meta = decode(&file);
    let reveal = meta.layers.as_mut().unwrap()[1]
        .caption_style
        .as_mut()
        .unwrap()
        .reveal
        .as_mut()
        .unwrap();
    reveal.unit_times = Some(vec![0.1, 0.5]);
    reveal.unit_ends = Some(vec![0.4, 0.9]);
    meta.layers.as_mut().unwrap()[0]
        .captions
        .as_mut()
        .unwrap()
        .made = Some(3);
    let app = meta.to_app_json().unwrap();
    assert!(
        app.contains("unitTimes") && app.contains("\"made\":3"),
        "{app}"
    );
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    let text = written.to_string();
    assert!(
        !text.contains("unitTimes") && !text.contains("unitEnds"),
        "{text}"
    );
    assert!(
        written["layers"][0]["captions"].get("made").is_none(),
        "{text}"
    );
    assert!(promo_model::json_semantically_equal(
        &written["layers"][1]["captionStyle"]["reveal"]["sync"],
        &file["layers"][1]["captionStyle"]["reveal"]["sync"]
    ));
    // Pieces 0 and 2 of the 3 made are gone: a person deleted them.
    assert_eq!(
        written["layers"][0]["captions"]["skip"],
        json!([0, 1, 2]),
        "{text}"
    );
}
