//! A file the model will not read says WHERE (issue #15): a minified
//! metadata.json's "line 1 column 790" pointed nowhere a person could find.

use promo_model::ProjectMetadata;
use serde_json::json;

fn project(layers: serde_json::Value) -> String {
    json!({"id": "P", "name": "P", "createdAt": 0, "state": "recorded",
           "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
           "compositionSettings": {}, "layers": layers, "resources": []})
    .to_string()
}

#[test]
fn a_keyframe_without_an_id_is_named_by_its_path() {
    let file = project(json!([
        {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background", "isEnabled": true,
         "startTime": 0, "duration": 4, "keyframes": []},
        {"id": "a", "name": "A", "sortIndex": 1, "kind": "background", "isEnabled": true,
         "startTime": 0, "duration": 4,
         "keyframes": [{"id": "k", "time": 0}, {"time": 1, "opacity": 1}]}
    ]));
    let error = ProjectMetadata::from_json(&file).unwrap_err().to_string();
    assert!(
        error.starts_with("layers[1].keyframes[1]: missing field `id`"),
        "{error}"
    );
    // A file that is not JSON at all keeps serde's own words.
    let broken = ProjectMetadata::from_json("{\"id\": ")
        .unwrap_err()
        .to_string();
    assert!(broken.contains("EOF"), "{broken}");
    // And one that reads is untouched by the second look.
    let fine = project(json!([]));
    assert!(ProjectMetadata::from_json(&fine).is_ok());
}
