//! The speech-timing wire fields (docs/SPEECH-SYNC.md §1.3, slice 1): a
//! recording's `transcript` and a take's `speech.wordFixes` — ignorable,
//! so rung 48, and kept through a writer that cannot read them.

use promo_model::ProjectMetadata;
use serde_json::{json, Value};

fn decode(value: &Value) -> ProjectMetadata {
    ProjectMetadata::from_json(&value.to_string()).expect("decodes")
}

fn base() -> Value {
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {},
        "layers": [{"id": "a", "name": "A", "sortIndex": 0, "kind": "audio",
                    "isEnabled": true, "startTime": 0, "resourceID": "vo",
                    "keyframes": []}],
        "resources": [{"id": "vo", "kind": "audio", "filename": "vo.m4a",
                       "displayName": "VO", "addedAt": 0,
                       "speech": {"text": "Meet Euclid.", "provider": "openai",
                                  "voiceID": "alloy"}}]
    })
}

/// Neither field renders anything differently: a lossless reader keeps
/// them, so they stamp 48 and no rung of their own.
#[test]
fn transcript_and_word_fixes_stamp_48() {
    assert!(decode(&base()).minimum_reader_version() < 48);
    let mut transcript = base();
    transcript["resources"][0]["transcript"] = json!("What the recording says.");
    let mut fixes = base();
    fixes["resources"][0]["speech"]["wordFixes"] =
        json!([{"word": 2, "start": 0.41, "scriptHash": "0badf00d"}]);
    for (what, project) in [("a transcript", transcript), ("word fixes", fixes)] {
        let meta = decode(&project);
        assert_eq!(meta.minimum_reader_version(), 48, "{what}");
        // Both are read, so validate never calls them "no effect".
        let unread = ProjectMetadata::unread_keys(&project.to_string()).unwrap();
        assert!(
            !unread.iter().any(|k| k.key == "transcript"),
            "{what}: {unread:?}"
        );
        // And a file holding them encodes them back as written.
        let back: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
        for key in ["transcript", "speech"] {
            assert_eq!(
                back["resources"][0].get(key),
                project["resources"][0].get(key),
                "{what}: {key}"
            );
        }
    }
}

/// The apps' model has no `transcript`: their copy gets it back from the
/// file it was made from.
#[test]
fn a_lossy_copy_gets_the_transcript_back() {
    let mut file = base();
    file["resources"][0]["transcript"] = json!("What the recording says.");
    let original = decode(&file);
    let mut copy = file.clone();
    copy["resources"][0]
        .as_object_mut()
        .unwrap()
        .remove("transcript");
    let mut carried = decode(&copy);
    assert_ne!(carried, original);
    carried.carry_unread_from(&original);
    assert_eq!(carried, original);
}

/// Fixes parse from the resource, name their script, and a malformed list
/// is a sentence, not a panic.
#[test]
fn word_fixes_read_from_the_take() {
    let mut project = base();
    project["resources"][0]["speech"]["wordFixes"] =
        json!([{"word": 2, "end": 1.2}, {"word": 1, "start": 0.1, "scriptHash": "9c1e04aa"}]);
    let meta = decode(&project);
    let resource = &meta.resources.as_ref().unwrap()[0];
    let fixes = promo_model::speech::word_fixes(resource).unwrap();
    assert_eq!(fixes.len(), 2);
    assert_eq!(fixes[0].word, 2);
    assert_eq!(fixes[0].end, Some(1.2));
    assert_eq!(fixes[1].script_hash.as_deref(), Some("9c1e04aa"));
    assert_eq!(
        promo_model::speech::take_script(resource).as_deref(),
        Some("Meet Euclid.")
    );

    project["resources"][0]["speech"]["wordFixes"] = json!([{"word": "two"}]);
    let meta = decode(&project);
    let err = promo_model::speech::word_fixes(&meta.resources.as_ref().unwrap()[0]).unwrap_err();
    assert!(err.contains("speech.wordFixes"), "{err}");
}
