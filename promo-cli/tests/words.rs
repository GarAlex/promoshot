//! `promo words` (docs/SPEECH-SYNC.md §9, slice 1): a take's word timing
//! as the record's JSON — the project's cached record when there is one,
//! else a syllable estimate over the take's length, flagged as one.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn folder(name: &str, metadata: &Value) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-words-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    std::fs::write(dir.join("metadata.json"), metadata.to_string()).unwrap();
    dir
}

const SCRIPT: &str = "Meet Euclid. Geometry, proved step by step.";

fn project(script: &str) -> Value {
    json!({
        "id": "P", "name": "Words", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90},
        "layers": [{"id": "a", "name": "VO", "sortIndex": 0, "kind": "audio",
                    "isEnabled": true, "startTime": 0, "resourceID": "intro",
                    "keyframes": []}],
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 3.9,
                       "speech": {"text": script, "provider": "openai", "voiceID": "alloy",
                                  "wordFixes": [{"word": 1, "start": 0.25}]}}]
    })
}

fn promo(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("words")
        .args(args)
        .output()
        .expect("run promo");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn record_keys(record: &Value) {
    for key in [
        "timingVersion",
        "audio",
        "script",
        "scriptHash",
        "duration",
        "source",
        "estimated",
        "confidence",
        "words",
        "sentences",
    ] {
        assert!(record.get(key).is_some(), "no `{key}`: {record}");
    }
}

#[test]
fn promo_words_json_shape() {
    let dir = folder("shape", &project(SCRIPT));
    let take = dir.join("Resources/intro.mp3");
    std::fs::write(&take, promo_speech::silent_mp3(3.9)).unwrap();

    // A project's take with no record: an estimate, said to be one, the
    // take's hand fix applied, nothing stored.
    let first: Value = serde_json::from_str(&promo(&[dir.to_str().unwrap(), "--json"])).unwrap();
    record_keys(&first);
    assert_eq!(first["fromCache"], false);
    assert_eq!(first["source"], "estimate");
    assert_eq!(first["estimated"], true);
    assert_eq!(first["audio"]["sha256"].as_str().unwrap().len(), 32);
    assert_eq!(first["words"].as_array().unwrap().len(), 7);
    assert_eq!(first["words"][0]["start"], 0.25, "the fix is applied");
    assert_eq!(first["words"][1]["chars"], json!([5, 11]));
    assert_eq!(first["sentences"][1]["words"], json!([2, 6]));
    assert!(
        !dir.join("Resources/.words").exists(),
        "an estimate is not stored"
    );

    // A stored record is the answer from then on…
    let mut timed = promo_speech::estimate_words(SCRIPT, 3.9, None, 1.0);
    timed.source = promo_model::speech::TimingSource::Provider("elevenlabs".into());
    timed.estimated = false;
    timed.confidence = 1.0;
    timed.words[3].start = 2.5;
    promo_model::word_cache::store(&dir, &take, timed).unwrap();
    let cached: Value = serde_json::from_str(&promo(&[dir.to_str().unwrap(), "--json"])).unwrap();
    assert_eq!(cached["fromCache"], true);
    assert_eq!(cached["source"], "provider:elevenlabs");
    assert_eq!(cached["words"][3]["start"], 2.5);
    assert!(cached.get("stale").is_none());

    // …and still, marked stale, after the text is edited with no new take.
    std::fs::write(
        dir.join("metadata.json"),
        project("Meet Euclid. Geometry, proved again.").to_string(),
    )
    .unwrap();
    let stale: Value = serde_json::from_str(&promo(&[dir.to_str().unwrap(), "--json"])).unwrap();
    assert_eq!(stale["stale"], true);
    assert_eq!(
        stale["script"], SCRIPT,
        "the record says what the take says"
    );
    let text = promo(&[dir.to_str().unwrap()]);
    assert!(text.contains("timed by elevenlabs"), "{text}");
    assert!(text.contains("these times follow the old take"), "{text}");
    assert!(text.contains("   1     0.250"), "{text}");

    // A bare file with its script: an estimate over its length.
    let file: Value = serde_json::from_str(&promo(&[
        take.to_str().unwrap(),
        "--script",
        SCRIPT,
        "--json",
    ]))
    .unwrap();
    record_keys(&file);
    assert!(file.get("fromCache").is_none(), "a bare file has no cache");
    let length = file["duration"].as_f64().unwrap();
    assert!((length - 3.9).abs() < 0.1, "{length}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn promo_words_names_what_it_needs() {
    let dir = folder("needs", &project(SCRIPT));
    let fail = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_promo"))
            .arg("words")
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let bare = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    assert!(fail(&[bare.to_str().unwrap()]).contains("needs its script"));
    assert!(fail(&[dir.to_str().unwrap(), "--resource", "nope"]).contains("no resource `nope`"));
    assert!(fail(&[dir.to_str().unwrap(), "--script", "x"]).contains("its own script"));
    let _ = std::fs::remove_dir_all(&dir);
}
