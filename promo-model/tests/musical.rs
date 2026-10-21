//! Musical time values and the beat-sync fields, slice 1 (docs/BEAT-SYNC.md
//! §1, §9 S1): the format and its lossless plumbing. A musical value lifts
//! aside on load and folds back on save; an edited value flattens alone;
//! the rung says who may open the file; and every seconds-only project in
//! the repository encodes exactly as it did before musical time existed.

use std::path::{Path, PathBuf};

use promo_model::{musical, ProjectMetadata};
use serde_json::{json, Value};

/// A project using every musical form, on every field that takes one, at
/// every depth a layer lives: the project's layers, a stage's members and a
/// composition's layers — plus every beat-sync field slice 1 parses.
fn musical_project() -> Value {
    json!({
        "id": "P", "name": "Beats", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 8, "videoDuration": 8, "subtitles": [],
        "compositionSettings": {
            "canvasWidth": 1920, "canvasHeight": 1080,
            "beatClock": {"resourceID": "song", "startTime": 0},
            "sequence": [{"layer": "s1", "beats": 4}, {"layer": "s2", "duration": {"bars": 1}}],
            "flash": {"on": "downbeat", "colorHex": "FFFFFF", "opacity": 0.6,
                      "duration": {"beats": 0.25}}
        },
        "layers": [
            {"id": "s1", "name": "One", "sortIndex": 0, "kind": "image", "isEnabled": true,
             "startTime": {"beat": 1}, "duration": {"beats": 2},
             "fadeIn": {"beats": 0.5}, "fadeOut": {"bars": 1},
             "transitionIn": {"kind": "wipe", "duration": {"beats": 1}},
             "transitionOut": {"kind": "fade", "duration": 0.25},
             "pulse": {"every": "downbeat", "property": "viewport", "amount": 0.04,
                       "decay": {"beats": 0.5}},
             "keyframes": [
                 {"id": "k1", "time": {"beats": 1}, "transitionDuration": {"beats": 0.25}},
                 {"id": "k2", "time": 2.5, "transitionDuration": 0.5}
             ]},
            {"id": "s2", "name": "Two", "sortIndex": 1, "kind": "image", "isEnabled": true,
             "startTime": 3, "duration": 2,
             "after": {"layer": "s1", "offset": {"beats": 1}},
             "shake": [{"on": "beat", "amplitude": 8}]},
            {"id": "st", "name": "Stage", "sortIndex": 2, "kind": "stage", "isEnabled": true,
             "startTime": {"snap": "bar", "near": 4.1}, "keyframes": [],
             "members": [
                 {"id": "m1", "name": "Member", "sortIndex": 0, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": {"bars": 2},
                  "keyframes": []}
             ]},
            {"id": "nest", "name": "Nest", "sortIndex": 3, "kind": "image", "isEnabled": true,
             "startTime": 0, "duration": 4, "resourceID": "comp", "keyframes": []},
            {"id": "music", "name": "Music", "sortIndex": 4, "kind": "audio", "isEnabled": true,
             "startTime": 0, "resourceID": "song", "keyframes": []}
        ],
        "resources": [
            {"id": "song", "kind": "audio", "filename": "song.m4a", "displayName": "Song",
             "addedAt": 0, "beatGrid": {"bpm": 128, "offset": 0.469, "timeSignature": [4, 4]},
             "provenance": {"tool": "ace-step", "license": "Apache-2.0", "cost": 0}},
            {"id": "comp", "kind": "composition", "filename": "", "displayName": "Comp",
             "addedAt": 0,
             "composition": {"canvasWidth": 1920, "canvasHeight": 1080,
                             "beatClock": {"resourceID": "song"},
                             "layers": [
                                 {"id": "n1", "name": "Inner", "sortIndex": 0, "kind": "image",
                                  "isEnabled": true, "startTime": {"bar": 2},
                                  "duration": {"beats": 3}, "keyframes": []}
                             ]}}
        ]
    })
}

/// Where the musical values sit in `musical_project`, as JSON pointers.
const MUSICAL_AT: &[&str] = &[
    "/layers/0/startTime",
    "/layers/0/duration",
    "/layers/0/fadeIn",
    "/layers/0/fadeOut",
    "/layers/0/transitionIn/duration",
    "/layers/0/keyframes/0/time",
    "/layers/0/keyframes/0/transitionDuration",
    "/layers/2/startTime",
    "/layers/2/members/0/duration",
    "/resources/1/composition/layers/0/startTime",
    "/resources/1/composition/layers/0/duration",
];

fn decode(value: &Value) -> ProjectMetadata {
    ProjectMetadata::from_json(&value.to_string()).expect("decodes")
}

/// The file the core writes holds every musical value exactly as the
/// source said it, and writing it again changes not one byte.
#[test]
fn musical_values_round_trip_byte_for_byte() {
    let source = musical_project();
    let first = decode(&source).to_json().expect("encodes");
    let written: Value = serde_json::from_str(&first).unwrap();
    for at in MUSICAL_AT {
        assert_eq!(
            written.pointer(at),
            source.pointer(at),
            "{at} did not come back as written"
        );
    }
    assert!(
        !first.contains("timeSpecs"),
        "the side map reached the file"
    );
    // Seconds stay seconds beside them.
    assert_eq!(
        written.pointer("/layers/0/transitionOut/duration"),
        Some(&json!(0.25))
    );
    assert_eq!(
        written.pointer("/layers/0/keyframes/1/time"),
        Some(&json!(2.5))
    );
    // The fields slice 1 only carries come back as written too.
    for at in [
        "/compositionSettings/beatClock",
        "/compositionSettings/sequence",
        "/compositionSettings/flash",
        "/layers/0/pulse",
        "/layers/1/after",
        "/layers/1/shake",
        "/resources/0/beatGrid",
        "/resources/0/provenance",
        "/resources/1/composition/beatClock",
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
    // A fixed point: the written file reads and writes as itself.
    let again = ProjectMetadata::from_json(&first).unwrap();
    assert_eq!(again, decode(&source));
    assert_eq!(again.to_json().unwrap(), first);
}

/// The app form holds numbers in every time field, each spec beside its
/// number with the core's label, so a host decoding numbers can open it.
#[test]
fn the_app_form_holds_numbers_and_the_specs_beside_them() {
    let meta = decode(&musical_project());
    let app: Value = serde_json::from_str(&meta.to_app_json().unwrap()).unwrap();
    for at in MUSICAL_AT {
        assert!(app.pointer(at).unwrap().is_number(), "{at} is not a number");
    }
    assert_eq!(
        app.pointer("/layers/0/timeSpecs/duration"),
        Some(&json!({"spec": "{\"beats\":2}", "seconds": 0.0, "label": "2 beats"}))
    );
    assert_eq!(
        app.pointer("/layers/0/timeSpecs/transitionIn.duration/label"),
        Some(&json!("1 beat"))
    );
    assert_eq!(
        app.pointer("/layers/0/keyframes/0/timeSpecs/time/spec"),
        Some(&json!("{\"beats\":1}"))
    );
    assert_eq!(
        app.pointer("/layers/2/timeSpecs/startTime/label"),
        Some(&json!("nearest bar to 4.1 s"))
    );
    // The app form decodes as itself and still writes the musical file.
    let back = ProjectMetadata::from_json(&app.to_string()).unwrap();
    assert_eq!(back, meta);
    assert_eq!(back.to_json().unwrap(), meta.to_json().unwrap());
}

/// A malformed musical value is refused, and the message says where.
#[test]
fn malformed_musical_value_names_its_path() {
    let mut project = musical_project();
    project["layers"][0]["duration"] = json!({"beets": 2});
    let err = ProjectMetadata::from_json(&project.to_string()).unwrap_err();
    assert!(
        err.to_string()
            .contains("layers[s1].duration: unknown key \"beets\""),
        "{err}"
    );
    let mut project = musical_project();
    project["resources"][1]["composition"]["layers"][0]["startTime"] =
        json!({"bar": 2, "beats": 1});
    let err = ProjectMetadata::from_json(&project.to_string()).unwrap_err();
    assert!(
        err.to_string()
            .contains("resources[comp].composition.layers[n1].startTime: needs exactly one"),
        "{err}"
    );
    let mut project = musical_project();
    project["layers"][2]["members"][0]["duration"] = json!({"bars": -1});
    let err = ProjectMetadata::from_json(&project.to_string()).unwrap_err();
    assert!(
        err.to_string()
            .contains("layers[st].members[m1].duration: \"bars\" must be at least 0"),
        "{err}"
    );
    // The validator's own walk says the same, rather than panicking.
    assert!(ProjectMetadata::unread_keys(&project.to_string()).is_err());
}

/// A person who edits one musical value's seconds flattens that value and
/// no other: the number stands at its field, every other spec comes back.
#[test]
fn edited_value_flattens_only_itself() {
    let mut meta = decode(&musical_project());
    let layers = meta.layers.as_mut().unwrap();
    layers[0].duration = Some(1.75);
    layers[0].keyframes[0].transition_duration = 0.3;
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_eq!(written.pointer("/layers/0/duration"), Some(&json!(1.75)));
    assert_eq!(
        written.pointer("/layers/0/keyframes/0/transitionDuration"),
        Some(&json!(0.3))
    );
    let source = musical_project();
    for at in MUSICAL_AT.iter().filter(|at| {
        **at != "/layers/0/duration" && **at != "/layers/0/keyframes/0/transitionDuration"
    }) {
        assert_eq!(
            written.pointer(at),
            source.pointer(at),
            "{at} flattened too"
        );
    }
    // A number within the epsilon is no edit.
    let mut meta = decode(&musical_project());
    meta.layers.as_mut().unwrap()[0].start_time = 0.00005;
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_eq!(
        written.pointer("/layers/0/startTime"),
        Some(&json!({"beat": 1}))
    );
}

/// A file with a musical value a save keeps needs the musical reader; one
/// whose only musical value was edited to seconds is back to what its
/// other content needs.
#[test]
fn musical_file_stamps_49() {
    assert_eq!(promo_model::READER_VERSION, 49);
    assert_eq!(promo_model::LOSSLESS_READER, 48);
    let plain = |layer: Value| {
        json!({
            "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
            "compositionSettings": {}, "layers": [layer]
        })
    };
    let image = |extra: Value| {
        let mut layer = json!({"id": "a", "name": "A", "sortIndex": 0, "kind": "image",
                               "isEnabled": true, "startTime": 0, "duration": 2,
                               "keyframes": []});
        for (k, v) in extra.as_object().unwrap() {
            layer[k] = v.clone();
        }
        layer
    };
    assert!(decode(&plain(image(json!({})))).minimum_reader_version() < 48);
    for (what, extra) in [
        ("a musical duration", json!({"duration": {"beats": 2}})),
        (
            "a musical keyframe",
            json!({"keyframes": [{"id": "k", "time": {"beat": 3}}]}),
        ),
        ("after", json!({"after": "b"})),
        ("a pulse", json!({"pulse": {"every": "beat"}})),
        ("a shake", json!({"shake": {"on": "beat"}})),
    ] {
        assert_eq!(
            decode(&plain(image(extra))).minimum_reader_version(),
            49,
            "{what}"
        );
    }
    let mut sequenced = plain(image(json!({})));
    sequenced["compositionSettings"]["sequence"] = json!([{"layer": "a", "beats": 4}]);
    assert_eq!(
        decode(&sequenced).minimum_reader_version(),
        49,
        "a sequence"
    );
    let mut flashed = plain(image(json!({})));
    flashed["compositionSettings"]["flash"] = json!({"on": "downbeat"});
    assert_eq!(decode(&flashed).minimum_reader_version(), 49, "a flash");
    assert_eq!(decode(&musical_project()).minimum_reader_version(), 49);

    // Edited to seconds, the value folds flat and the rung comes down.
    let mut meta = decode(&plain(image(json!({"duration": {"beats": 2}}))));
    meta.layers.as_mut().unwrap()[0].duration = Some(1.5);
    assert!(meta.minimum_reader_version() < 48);
}

/// A clock, a stated grid and provenance render nothing differently: a
/// lossless reader keeps them, so they stamp 48, not a rung of their own.
#[test]
fn ignorable_beat_fields_stamp_48() {
    let base = || {
        json!({
            "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
            "compositionSettings": {},
            "layers": [{"id": "a", "name": "A", "sortIndex": 0, "kind": "audio",
                        "isEnabled": true, "startTime": 0, "resourceID": "song",
                        "keyframes": []}],
            "resources": [{"id": "song", "kind": "audio", "filename": "s.m4a",
                           "displayName": "S", "addedAt": 0}]
        })
    };
    assert!(decode(&base()).minimum_reader_version() < 48);
    let mut clock = base();
    clock["compositionSettings"]["beatClock"] = json!({"resourceID": "song"});
    let mut layer_clock = base();
    layer_clock["layers"][0]["beatClock"] = json!({"resourceID": "song", "startTime": 1});
    let mut grid = base();
    grid["resources"][0]["beatGrid"] = json!({"bpm": 120});
    let mut provenance = base();
    provenance["resources"][0]["provenance"] = json!({"tool": "suno", "cost": 0.1});
    for (what, project) in [
        ("a beat clock", clock),
        ("a layer's beat clock", layer_clock),
        ("a beat grid", grid),
        ("provenance", provenance),
    ] {
        assert_eq!(decode(&project).minimum_reader_version(), 48, "{what}");
    }
}

/// What a lossy copy cannot read — the apps' model has no `after`,
/// `sequence`, generators, a composition's clock or provenance — comes back
/// from the document it was made from; its specs are its own. The project's
/// beat clock and a resource's beat grid the apps read and edit: a copy
/// without them has cleared them, and that stands.
#[test]
fn a_lossy_copy_gets_the_beat_fields_back() {
    let original = decode(&musical_project());
    let mut copy: Value = serde_json::from_str(&original.to_app_json().unwrap()).unwrap();
    let settings = copy["compositionSettings"].as_object_mut().unwrap();
    for key in ["sequence", "flash"] {
        settings.remove(key);
    }
    copy["layers"][0].as_object_mut().unwrap().remove("pulse");
    copy["layers"][1].as_object_mut().unwrap().remove("after");
    copy["layers"][1].as_object_mut().unwrap().remove("shake");
    copy["resources"][0]
        .as_object_mut()
        .unwrap()
        .remove("provenance");
    copy["resources"][1]["composition"]
        .as_object_mut()
        .unwrap()
        .remove("beatClock");
    let mut carried = decode(&copy);
    assert_ne!(carried, original);
    carried.carry_unread_from(&original);
    assert_eq!(carried, original);

    // Cleared by the apps: the clock and the grid stay cleared.
    let mut cleared = copy.clone();
    cleared["compositionSettings"]
        .as_object_mut()
        .unwrap()
        .remove("beatClock");
    cleared["resources"][0]
        .as_object_mut()
        .unwrap()
        .remove("beatGrid");
    let mut carried = decode(&cleared);
    carried.carry_unread_from(&original);
    assert!(carried.composition_settings.beat_clock.is_none());
    assert!(carried.resources.as_ref().unwrap()[0].beat_grid.is_none());
}

fn project_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && name != "runs" && !name.starts_with('.') {
                project_files(&path, out);
            }
        } else if name.ends_with(".json") {
            out.push(path);
        }
    }
}

/// AC4: every seconds-only project in the repository — the fixtures, the
/// demos, the examples, the recipes — decodes exactly as before (straight
/// from the text, no side map), encodes to exactly the bytes the encoder
/// wrote before musical time existed, and asks no new reader.
#[test]
fn seconds_only_projects_are_unchanged() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    project_files(&root, &mut files);
    let mut texts: Vec<(String, String)> = files
        .iter()
        .filter_map(|f| Some((f.display().to_string(), std::fs::read_to_string(f).ok()?)))
        .collect();
    texts.extend(
        promo_model::quick_schema_recipes()
            .into_iter()
            .enumerate()
            .map(|(i, r)| (format!("recipe {i}"), r.to_string())),
    );
    let mut checked = 0;
    for (name, text) in &texts {
        let Ok(meta) = ProjectMetadata::from_json(text) else {
            continue;
        };
        // The beat-cut recipe and its example are musical on purpose.
        if meta.id == "beatcut" {
            continue;
        }
        checked += 1;
        // Nothing to lift: the file holds no musical value...
        let mut value: Value = serde_json::from_str(text).unwrap();
        assert!(
            !musical::lift(&mut value).unwrap(),
            "{name} holds a musical value"
        );
        // ...so the model is what serde alone reads, with no side map...
        let direct: ProjectMetadata = serde_json::from_str(text).unwrap();
        assert_eq!(meta, direct, "{name}: decode changed");
        assert!(
            !musical::carries_specs(&meta),
            "{name}: a side map appeared"
        );
        // ...the file form is the pre-feature encoding, byte for byte...
        let written = meta.to_json().unwrap();
        assert_eq!(
            written,
            serde_json::to_string(&meta).unwrap(),
            "{name}: encode changed"
        );
        assert_eq!(written, meta.to_app_json().unwrap(), "{name}: two forms");
        // ...folding finds nothing, and no musical reader is asked for.
        let mut folded: Value = serde_json::from_str(&written).unwrap();
        assert!(!musical::fold(&mut folded), "{name}: fold changed it");
        assert!(
            meta.minimum_reader_version() <= promo_model::LOSSLESS_READER,
            "{name}: stamped {}",
            meta.minimum_reader_version()
        );
    }
    assert!(checked > 20, "decoded only {checked}");
}

/// Provenance on a resource — every field, and one this build does not
/// name — comes back as written, and asks only the lossless reader.
#[test]
fn provenance_round_trips_and_stamps_48() {
    let provenance = json!({"tool": "ace-step", "model": "v1.5", "license": "Apache-2.0",
                            "prompt": "a bright synth loop", "cost": 0.25, "currency": "EUR",
                            "createdAt": "2026-10-20T10:00:00Z", "url": "https://example.com/x",
                            "seed": 7});
    let file = json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 1, "videoDuration": 1, "subtitles": [],
        "compositionSettings": {},
        "resources": [{"id": "song", "kind": "audio", "filename": "s.wav", "displayName": "S",
                       "addedAt": 0, "provenance": provenance.clone()}]
    });
    let meta = decode(&file);
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert!(promo_model::json_semantically_equal(
        &written["resources"][0]["provenance"],
        &provenance
    ));
    assert_eq!(meta.minimum_reader_version(), 48);
}
