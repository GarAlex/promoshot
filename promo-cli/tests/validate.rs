//! `promo validate` through the built binary, the way the headless MCP
//! server and a CLI-only agent run it: the first word is the verdict, and
//! `--strict` makes the exit code say the same.

use std::process::Command;

fn project(dir: &std::path::Path, layer_extra: &str) {
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    std::fs::write(
        dir.join("metadata.json"),
        format!(
            r#"{{"id":"P","name":"Strict","createdAt":0,"state":"recorded","minReaderVersion":18,
                "trimStart":0,"trimEnd":3,"videoDuration":3,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":180}},
                "layers":[{{"id":"bg","name":"Ground","sortIndex":0,"kind":"background",
                  "isEnabled":true,"startTime":0,"duration":3{layer_extra},"keyframes":[
                    {{"id":"k","time":0,"colorHex":"101014","transitionDuration":0}}]}}]}}"#
        ),
    )
    .unwrap();
}

fn validate(dir: &std::path::Path, flags: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(dir)
        .args(flags)
        .output()
        .expect("run promo");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A field nothing reads used to come back "ok — the project decodes";
/// now the answer starts NOT OK and names it, the exit code stays 0 for
/// scripts that only gate on decoding, and `--strict` turns it into 1.
#[test]
fn a_field_nothing_reads_is_not_ok_and_strict_fails_on_it() {
    let dir = std::env::temp_dir().join(format!("promo-strict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    project(&dir, "");
    let (success, text) = validate(&dir, &[]);
    assert!(success, "{text}");
    assert!(text.starts_with("ok"), "{text}");
    let (success, _) = validate(&dir, &["--strict"]);
    assert!(success, "a clean project passes --strict");

    project(&dir, r#","opacity":0.5"#);
    let (success, text) = validate(&dir, &[]);
    assert!(
        success,
        "without --strict a finding is not a failure: {text}"
    );
    assert!(text.starts_with("NOT OK"), "{text}");
    assert!(text.contains(r#""opacity" is not read here"#), "{text}");

    let (success, json) = validate(&dir, &["--strict", "--json"]);
    assert!(!success, "--strict fails on a break");
    let value: serde_json::Value = serde_json::from_str(json.trim()).expect("one JSON object");
    assert_eq!(value["ok"], false);
    assert_eq!(value["renders"], false);
    assert_eq!(value["breaks"].as_array().map(Vec::len), Some(1));
    assert!(value["text"].as_str().unwrap().starts_with("NOT OK"));

    let _ = std::fs::remove_dir_all(&dir);
}
