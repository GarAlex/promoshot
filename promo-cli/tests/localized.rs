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
    let exports = dir.with_extension("stills");
    let out = exports.join("shot.png");
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
    // A language other than the source lands in its own folder.
    let written = match language.filter(|l| *l != "en") {
        Some(language) => exports.join(language).join("shot.png"),
        None => out,
    };
    Ok(std::fs::read(written).unwrap())
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

fn with_three_languages(tag: &str) -> PathBuf {
    project(
        tag,
        json!({"localized": {"de": {"captionText": "Hallo zusammen", "by": "person"}}}),
        Some(languages()),
    )
}

#[test]
fn a_list_renders_each_language_beside_the_source() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = with_three_languages("list");
    let exports = dir.with_extension("exports");
    let _ = std::fs::remove_dir_all(&exports);
    let out = exports.join("shot.png");
    let (ok, text) = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        "1",
        "--language",
        "all",
    ]);
    assert!(ok, "{text}");
    let source = std::fs::read(&out).expect("the source stays where it was");
    let german = std::fs::read(exports.join("de/shot.png")).expect("German in its folder");
    let japanese = std::fs::read(exports.join("ja/shot.png")).expect("Japanese in its folder");
    assert_ne!(german, source);
    assert_eq!(japanese, source, "no entry: the source's words");
    assert!(!exports.join("en").exists(), "the source has no folder");
}

#[test]
fn a_language_that_fails_is_named_and_the_others_finish() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = with_three_languages("partial");
    let exports = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&exports);
    let out = exports.join("shot.png");
    let (ok, text) = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        "1",
        "--language",
        "fr,de",
        "--json",
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("fr: \"fr\" is not one"), "{text}");
    assert!(
        exports.join("de/shot.png").is_file(),
        "German still rendered"
    );
    assert!(!exports.join("fr").join("shot.png").exists());
}

#[test]
fn all_on_a_project_with_no_languages_is_the_source() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project("none", json!({}), None);
    let out = dir.with_extension("none.png");
    let (ok, text) = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        "1",
        "--language",
        "all",
    ]);
    assert!(ok, "{text}");
    assert!(out.is_file());
}

/// promo apply: setLanguages and setTranslation written, read by strings,
/// a caption kept on a layer moved, rendered in that language, and the file
/// stamped for a reader that keeps what it does not read.
#[test]
fn translating_through_the_tools_end_to_end() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project("tools", json!({}), None);
    let d = dir.to_str().unwrap();
    let path = dir.join("metadata.json");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["layers"].as_array_mut().unwrap().push(json!({
        "id": "inline", "name": "inline", "sortIndex": 6, "kind": "caption", "isEnabled": true,
        "startTime": 0, "duration": 2, "captionText": "Inline",
        "captionStyle": {"fontSize": 30, "textColorHex": "FFFFFF"},
        "keyframes": [{"id": "ki", "time": 0, "transitionDuration": 0,
                       "placement": {"anchor": "top"}}]}));
    std::fs::write(&path, doc.to_string()).unwrap();

    let apply = |commands: Value| {
        promo(&[
            "apply",
            d,
            "--args",
            &json!({ "commands": commands }).to_string(),
        ])
    };
    let (ok, text) = apply(json!([
        {"kind": "setTranslation", "resourceID": "c", "language": "de",
         "patch": {"captionText": "x"}}]));
    assert!(!ok && text.contains("setLanguages first"), "{text}");

    let (ok, text) = apply(json!([
        {"kind": "setLanguages", "source": "en", "targets": ["de"]},
        {"kind": "setTranslation", "resourceID": "c", "language": "de",
         "patch": {"captionText": "Hallo zusammen"}, "by": "machine"},
        {"kind": "setTranslation", "layerID": "inline", "language": "de",
         "patch": {"captionText": "Oben"}, "by": "machine"}]));
    assert!(ok, "{text}");

    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(doc["minReaderVersion"].as_i64().unwrap() >= 48, "stamped");

    let (ok, text) = promo(&["strings", d, "--json"]);
    assert!(ok, "{text}");
    let answer: Value = serde_json::from_str(&text).expect(&text);
    assert_eq!(answer["source"], json!("en"));
    let rows = answer["strings"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{text}");
    for row in rows {
        assert_eq!(row["translations"][0]["state"], json!("current"), "{row}");
        assert_eq!(row["translations"][0]["by"], json!("machine"), "{row}");
    }
    assert!(
        rows.iter().any(|r| r["source"] == json!("Inline")),
        "{text}"
    );

    let (_, report) = promo(&["validate", d, "--json"]);
    let report: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["languages"][0]["language"], json!("de"));
    assert_eq!(report["languages"][0]["missing"], json!([]));
    assert_eq!(report["breaks"], json!([]), "{report}");

    // The person edits the source: stale, and validate names it.
    let (ok, text) = apply(json!([
        {"kind": "patchResource", "resourceID": "c", "patch": {"captionText": "Hello all"}}]));
    assert!(ok, "{text}");
    let (ok, text) = promo(&["validate", d, "--strict"]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("Languages\n  de  2 strings: 0 missing, 1 stale"),
        "{text}"
    );
    assert!(text.contains("c captionText"), "{text}");
    let (_, inspected) = promo(&["inspect", d]);
    assert!(
        inspected.contains("languages: en source, de (0 missing, 1 stale)"),
        "{inspected}"
    );
}

/// A translation that runs off the canvas where the source fit is named
/// for that language. The source's own words raise nothing.
#[test]
fn validate_names_a_translation_that_no_longer_fits() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project(
        "fit",
        json!({"localized": {"de": {"captionStyle": {"fontSize": 400}}}}),
        Some(languages()),
    );
    let (_, text) = promo(&["validate", dir.to_str().unwrap()]);
    assert!(
        text.lines()
            .any(|l| l.contains("- de: ") && l.contains("past")),
        "{text}"
    );
    assert!(
        !text
            .lines()
            .any(|l| l.contains("- ja: ") && l.contains("past")),
        "{text}"
    );
}

/// Frames and their contact sheet go in the language's folder together:
/// the sheet is what an agent looks at.
#[test]
fn frames_and_the_sheet_land_in_the_languages_folder() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = with_three_languages("frames");
    let exports = dir.with_extension("frames");
    let _ = std::fs::remove_dir_all(&exports);
    let (ok, text) = promo(&[
        "frames",
        dir.to_str().unwrap(),
        "--out",
        exports.join("frames").to_str().unwrap(),
        "--sheet",
        exports.join("sheet.png").to_str().unwrap(),
        "--times",
        "0.5,1",
        "--language",
        "de",
    ]);
    assert!(ok, "{text}");
    assert!(exports.join("de/sheet.png").is_file(), "{text}");
    assert!(exports.join("de/frames").is_dir(), "{text}");
    assert!(!exports.join("frames").exists(), "{text}");
}

/// Seconds of a sound, tone or silence, as ffmpeg writes it.
fn sound(path: &Path, seconds: f64, tone: bool) {
    let source = if tone {
        format!("sine=frequency=880:duration={seconds}")
    } else {
        format!("anullsrc=r=44100:cl=mono:d={seconds}")
    };
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &source])
        .arg(path)
        .status()
        .expect("ffmpeg");
    assert!(status.success());
}

/// The peak of a render's sound, in dB, and how long it runs.
fn peak_and_length(path: &Path) -> (f64, f64) {
    let out = Command::new("ffmpeg")
        .args([
            "-i",
            path.to_str().unwrap(),
            "-af",
            "volumedetect",
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("ffmpeg");
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let peak = text
        .split("max_volume: ")
        .nth(1)
        .and_then(|rest| rest.split(" dB").next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(f64::NEG_INFINITY);
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    let length = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .unwrap();
    (peak, length)
}

/// A silent source and a German take of a tone, three times as long: the
/// render plays the German one, runs its length, and what is anchored on the
/// speech moves with it, while a literal time does not and validate says so.
#[test]
fn a_languages_take_is_what_the_render_plays() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("no ffmpeg — the render cannot be written or read back");
        return;
    }
    let dir = project(
        "take",
        json!({}),
        Some(json!({"source": "en", "targets": ["de"]})),
    );
    let resources = dir.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    sound(&resources.join("r1.wav"), 1.0, false);
    sound(&resources.join("r1.de.wav"), 3.0, true);
    let path = dir.join("metadata.json");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["resources"].as_array_mut().unwrap().push(json!({
        "id": "r1", "kind": "audio", "filename": "r1.wav", "displayName": "Intro",
        "addedAt": 0, "duration": 1.0,
        "speech": {"text": "One one one. Two two two.", "provider": "openai", "voiceID": "alloy"},
        "localized": {"de": {
            "filename": "r1.de.wav", "duration": 3.0,
            "speech": {"text": "Eins eins eins. Zwei zwei zwei.", "provider": "openai",
                       "voiceID": "nova"}}}}));
    let layers = doc["layers"].as_array_mut().unwrap();
    layers.push(
        json!({"id": "voice", "name": "voice", "sortIndex": 6, "kind": "audio",
        "isEnabled": true, "startTime": 0, "duration": 1.0, "resourceID": "r1", "keyframes": []}),
    );
    // The caption is said in the second sentence; a literal one sits past the
    // source's end, inside the German take.
    let caption = layers.iter_mut().find(|l| l["id"] == "l").unwrap();
    caption["during"] = json!({"sentence": 2, "of": "voice"});
    layers.push(
        json!({"id": "fixed", "name": "fixed", "sortIndex": 7, "kind": "caption",
        "isEnabled": true, "startTime": 2.5, "duration": 0.2, "resourceID": "c",
        "keyframes": [{"id": "k2", "time": 0, "transitionDuration": 0,
                       "placement": {"anchor": "center"}}]}),
    );
    std::fs::write(&path, doc.to_string()).unwrap();

    let out = dir.with_extension("videos").join("film.mp4");
    let render = |language: Option<&str>| {
        let mut args = vec![
            "video",
            dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ];
        if let Some(language) = language {
            args.extend(["--language", language]);
        }
        let (ok, text) = promo(&args);
        assert!(ok, "{text}");
    };
    render(None);
    let (source_peak, source_length) = peak_and_length(&out);
    assert!(
        source_peak < -60.0,
        "the source is silent: {source_peak} dB"
    );
    render(Some("de"));
    let german = dir.with_extension("videos").join("de").join("film.mp4");
    let (german_peak, german_length) = peak_and_length(&german);
    assert!(
        german_peak > -30.0,
        "the German take plays: {german_peak} dB"
    );
    assert!(
        german_length > source_length,
        "{german_length} vs {source_length}"
    );
    assert!((german_length - 3.0).abs() < 0.2, "{german_length}");

    // The spoken caption is not on screen yet in German when the source's is.
    let lit = |png: Vec<u8>| {
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        image.pixels().any(|p| p[0] > 128)
    };
    let shot = |time: &str, language: Option<&str>| {
        let exports = dir.with_extension("stills");
        let written = exports.join("shot.png");
        let mut args = vec![
            "still",
            dir.to_str().unwrap(),
            "--out",
            written.to_str().unwrap(),
            "--time",
            time,
        ];
        if let Some(language) = language {
            args.extend(["--language", language]);
        }
        let (ok, text) = promo(&args);
        assert!(ok, "{text}");
        let file = match language {
            Some(language) => exports.join(language).join("shot.png"),
            None => written,
        };
        std::fs::read(file).unwrap()
    };
    // (A silent source aligns its second sentence late: at 0.9 of its 1 s.)
    assert!(lit(shot("0.95", None)), "the source says it by then");
    assert!(!lit(shot("0.95", Some("de"))), "German has not said it yet");
    assert!(lit(shot("2.95", Some("de"))), "German is saying it");

    let (_, report) = promo(&["validate", dir.to_str().unwrap()]);
    assert!(
        report.contains("runs 2.0 s longer than the source"),
        "{report}"
    );
    assert!(report.contains("1 layer(s) at literal times"), "{report}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("stills"));
    let _ = std::fs::remove_dir_all(dir.with_extension("videos"));
}
