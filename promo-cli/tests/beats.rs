//! Musical time through every loader (docs/BEAT-SYNC.md, slice 2): the CLI's
//! open, `explain`, the editor document and `validate` all read the same
//! resolved seconds — one pass behind all of them — and a render waits for a
//! beat clock whose audio has no grid.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn folder(name: &str, metadata: &Value) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-beats-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    std::fs::write(dir.join("metadata.json"), metadata.to_string()).unwrap();
    dir
}

fn beat_project(grid: Option<Value>) -> Value {
    let mut song = json!({"id": "song", "kind": "audio", "filename": "song.m4a",
                          "displayName": "Song", "addedAt": 0, "duration": 30});
    if let Some(grid) = grid {
        song["beatGrid"] = grid;
    }
    json!({
        "id": "P", "name": "Beats", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 8, "videoDuration": 8, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90,
                                "beatClock": {"resourceID": "song", "startTime": 0.25}},
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 8,
             "keyframes": [{"id": "g", "time": 0, "colorHex": "202030"}]},
            {"id": "s1", "name": "One", "sortIndex": 1, "kind": "caption", "isEnabled": true,
             "startTime": {"beat": 3}, "duration": {"bars": 1}, "captionText": "One",
             "keyframes": []},
            {"id": "s2", "name": "Two", "sortIndex": 2, "kind": "caption", "isEnabled": true,
             "startTime": 0, "timing": {"start": {"from": "previousEnd", "offset": 0}},
             "duration": {"beats": 2}, "captionText": "Two", "keyframes": []}
        ],
        "resources": [song]
    })
}

fn life(dir: &Path, id: &str) -> (f64, f64) {
    let out = promo_author::explain(&json!({"project": dir, "time": 0.1, "layer": id}), None)
        .expect("explains");
    let out: Value = serde_json::from_str(&out).unwrap();
    let life = &out["layers"][0]["life"];
    (
        life["start"].as_f64().unwrap(),
        life["end"].as_f64().unwrap(),
    )
}

/// The CLI's open, `explain` and the editor document give the same numbers
/// for the same file — bit for bit where no JSON text sits between — and
/// they are the grid's.
#[test]
fn every_loader_gives_the_same_numbers() {
    let file = beat_project(Some(json!({"bpm": 100, "offset": 0.05})));
    let dir = folder("loaders", &file);
    let beat = 60.0 / 100.0;
    let start = 0.25 + (0.05 + 2.0 * beat);

    let cli = promo_cli::project::Project::open(&dir).expect("opens");
    let layer = |meta: &promo_model::ProjectMetadata, id: &str| {
        let l = meta
            .layers
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .clone();
        (l.start_time, l.start_time + l.duration.unwrap())
    };
    let (s1, s2) = (layer(&cli.meta, "s1"), layer(&cli.meta, "s2"));
    assert_eq!(s1.0, start);
    assert!((s1.1 - (0.25 + (0.05 + 6.0 * beat))).abs() < 1e-12);
    assert_eq!(s2.0, s1.1, "the attachment follows the musical end");

    // explain answers in JSON text, and serde_json's default float parse
    // may land one ulp off the shortest round-trip it printed — so here, and
    // only here, to a picosecond.
    for (id, expected) in [("s1", s1), ("s2", s2)] {
        let (start, end) = life(&dir, id);
        assert!(
            (start - expected.0).abs() < 1e-12 && (end - expected.1).abs() < 1e-12,
            "explain disagrees on {id}: {:?} vs {expected:?}",
            (start, end)
        );
    }

    let mut doc = promo_editor::document::Document::open(&file.to_string()).unwrap();
    doc.apply(&serde_json::from_value(json!({"kind": "renameProject", "name": "Beats"})).unwrap())
        .unwrap();
    assert_eq!(layer(doc.meta(), "s1"), s1, "the document disagrees");
    assert_eq!(layer(doc.meta(), "s2"), s2, "the document disagrees");

    // validate sees a clean file: the grid is stated.
    let report = promo_timeline::report::file_report(&file.to_string(), None).unwrap();
    assert!(
        !format!("{report:?}").contains("beat"),
        "a stated grid draws no beat finding: {report:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clock whose audio has no grid lays out on 120 BPM — and a render
/// refuses to start until one is stated, saying how.
#[test]
fn a_render_waits_for_a_missing_grid() {
    let dir = folder("waits", &beat_project(None));
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("still")
        .arg(&dir)
        .output()
        .expect("run promo");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("has no beat grid yet"), "{said}");
    assert!(said.contains("beatGrid"), "{said}");

    // validate names it, as a warning.
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(&dir)
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("\"song\" has no beat grid yet"), "{said}");

    // Stated, the render is no longer held for it.
    std::fs::write(
        dir.join("metadata.json"),
        beat_project(Some(json!({"bpm": 120}))).to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("still")
        .arg(&dir)
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!said.contains("no beat grid"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
