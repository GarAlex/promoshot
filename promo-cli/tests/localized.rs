//! A project in several languages through the built binary
//! (core-private docs/LOCALIZATION.md, slice 1), read back as pixels: the
//! language must change the picture, and no language must change nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// One `promo` at a time: each render starts a GPU device.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn promo(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A 640x360 project on black with one white caption held on a resource.
fn project(tag: &str, resource_extra: Value, languages: Option<Value>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-lang-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (ok, text) = promo(&[
        "init",
        dir.to_str().unwrap(),
        "--args",
        r#"{"canvas":"640x360"}"#,
    ]);
    assert!(ok, "{text}");
    let path = dir.join("metadata.json");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["compositionSettings"]["backgroundColorHex"] = json!("000000");
    let mut resource = json!({"id": "c", "kind": "caption", "filename": "", "displayName": "c",
        "addedAt": 0, "captionText": "Hi",
        "captionStyle": {"fontSize": 40, "textColorHex": "FFFFFF", "alignment": "center"}});
    for (key, value) in resource_extra.as_object().unwrap() {
        resource[key] = value.clone();
    }
    doc["resources"].as_array_mut().unwrap().push(resource);
    doc["layers"].as_array_mut().unwrap().push(json!({
        "id": "l", "name": "l", "sortIndex": 5, "kind": "caption", "isEnabled": true,
        "startTime": 0, "duration": 2, "resourceID": "c",
        "keyframes": [{"id": "k", "time": 0, "transitionDuration": 0,
                       "placement": {"anchor": "center"}}]}));
    if let Some(languages) = languages {
        doc["languages"] = languages;
    }
    std::fs::write(&path, doc.to_string()).unwrap();
    dir
}

fn still(dir: &Path, language: Option<&str>) -> Result<Vec<u8>, String> {
    let out = dir.with_extension(format!("{}.png", language.unwrap_or("none")));
    let mut args = vec![
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        "1",
    ];
    if let Some(language) = language {
        args.extend(["--language", language]);
    }
    let (ok, text) = promo(&args);
    if !ok {
        return Err(text);
    }
    Ok(std::fs::read(out).unwrap())
}

/// Width and height of what is lit, from the PNG's pixels.
fn lit_box(png: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory(png).unwrap().to_rgba8();
    let (mut left, mut right, mut top, mut bottom) = (u32::MAX, 0, u32::MAX, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[0] > 128 {
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
    }
    assert!(left != u32::MAX, "nothing lit");
    (right - left + 1, bottom - top + 1)
}

fn languages() -> Value {
    json!({"source": "en", "targets": ["de", "ja"]})
}

#[test]
fn a_language_changes_the_picture_and_the_source_does_not() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let plain = project("plain", json!({}), None);
    let localized = project(
        "text",
        json!({"localized": {"de": {"captionText": "Hi there, world", "by": "person"}}}),
        Some(languages()),
    );
    let plain_png = still(&plain, None).unwrap();
    let source = still(&localized, None).unwrap();
    assert_eq!(source, plain_png, "languages on the file change no render");
    assert_eq!(
        still(&localized, Some("en")).unwrap(),
        plain_png,
        "the source named is the source"
    );
    assert_eq!(
        still(&localized, Some("ja")).unwrap(),
        plain_png,
        "a language with no entry shows the source"
    );
    let (source_width, _) = lit_box(&source);
    let (german_width, _) = lit_box(&still(&localized, Some("de")).unwrap());
    assert!(
        german_width > source_width * 2,
        "the German text is drawn: {german_width} vs {source_width}"
    );
}

#[test]
fn a_language_can_set_its_own_text_size() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project(
        "size",
        json!({"localized": {"de": {"captionStyle": {"fontSize": 80}}}}),
        Some(languages()),
    );
    let (_, source_height) = lit_box(&still(&dir, None).unwrap());
    let german = still(&dir, Some("de")).unwrap();
    let (_, german_height) = lit_box(&german);
    assert!(
        (german_height as f64 / source_height as f64 - 2.0).abs() < 0.3,
        "twice the size: {german_height} vs {source_height}"
    );
}

#[test]
fn a_bad_language_is_refused_and_validate_names_identity() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project(
        "bad",
        json!({"localized": {"de": {"id": "other"}}}),
        Some(languages()),
    );
    let why = still(&dir, Some("fr")).unwrap_err();
    assert!(why.contains("en, de, ja"), "{why}");
    let why = still(&dir, Some("de")).unwrap_err();
    assert!(why.contains("cannot differ per language"), "{why}");
    let (ok, text) = promo(&["validate", dir.to_str().unwrap(), "--strict"]);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot differ per language"), "{text}");
}

#[test]
fn validate_reads_languages_as_part_of_the_format() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project(
        "valid",
        json!({"localized": {"de": {"captionText": "Hallo", "by": "person"}}}),
        Some(languages()),
    );
    let (_, text) = promo(&["validate", dir.to_str().unwrap(), "--json"]);
    let report: Value = serde_json::from_str(&text).expect(&text);
    assert_eq!(report["breaks"], json!([]), "{text}");
    assert_eq!(report["renders"], json!(true), "{text}");
}

#[test]
fn only_renders_take_a_language() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project("verbs", json!({}), Some(languages()));
    let (ok, text) = promo(&["validate", dir.to_str().unwrap(), "--language", "de"]);
    assert!(!ok && text.contains("--language"), "{text}");
}
