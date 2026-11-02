//! A number the core computed reads back as the same number. Without
//! serde_json's `float_roundtrip` the parser landed `2 - 1/30` (a beat
//! flash's off key) one ulp low, so the apps' whole-document replace of an
//! unchanged project was a change, and the test hosts stopped on "a partial
//! core sync left … behind the Swift model".

use promo_model::ProjectMetadata;
use serde_json::json;

#[test]
fn a_computed_keyframe_time_survives_the_file() {
    let time: f64 = 2.0 - 1.0 / 30.0;
    let file = json!({"id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {}, "resources": [],
        "layers": [{"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
                    "isEnabled": true, "startTime": 0, "duration": 4,
                    "keyframes": [{"id": "k", "time": time}]}]})
    .to_string();
    let meta = ProjectMetadata::from_json(&file).unwrap();
    let read = meta.layers.as_ref().unwrap()[0].keyframes[0].time;
    assert_eq!(read.to_bits(), time.to_bits(), "{read:?} is not {time:?}");
    let again = ProjectMetadata::from_json(&meta.to_json().unwrap()).unwrap();
    assert_eq!(again, meta);
}
