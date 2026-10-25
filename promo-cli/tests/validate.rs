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

/// `promo video` writes the project's export size (review 2026-09-27,
/// P2-31): a 16:9 canvas stated to export at 320x320 comes out square, the
/// canvas fitted inside bars of the background's colour — as the apps
/// export it, through the same engine. Headless used to write the canvas
/// size whatever the project said.
#[test]
fn a_video_is_written_at_the_projects_export_size() {
    let has_ffmpeg = Command::new("ffmpeg").arg("-version").output().is_ok();
    if !has_ffmpeg {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("promo-exportsize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    // A red picture exactly the canvas's size over a black ground: the
    // canvas is where the red is.
    image::RgbaImage::from_pixel(320, 180, image::Rgba([255, 0, 0, 255]))
        .save(dir.join("Resources/red.png"))
        .unwrap();
    std::fs::write(
        dir.join("metadata.json"),
        r#"{"id":"P","name":"Size","createdAt":0,"state":"recorded","minReaderVersion":18,
            "trimStart":0,"trimEnd":1,"videoDuration":1,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":180,"backgroundColorHex":"000000",
              "videoExportWidth":320,"videoExportHeight":320},
            "resources":[{"id":"R","kind":"image","filename":"red.png","displayName":"r","addedAt":0,
              "pixelWidth":320,"pixelHeight":180,"imageCuts":[],"disabledAudioTrackIndices":[]}],
            "layers":[{"id":"L","name":"red","sortIndex":0,"kind":"image","isEnabled":true,
              "startTime":0,"duration":1,"resourceID":"R","keyframes":[]}]}"#,
    )
    .unwrap();
    let out = dir.join("out.mp4");
    let status = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("video")
        .arg(&dir)
        .args(["--out", out.to_str().unwrap(), "--fps", "10"])
        .output()
        .expect("run promo");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
        ])
        .args(["-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("ffprobe");
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "320,320");
    // One frame as raw RGB: a bar at the top, the red canvas in the middle.
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("ffmpeg");
    let rgb = frame.stdout;
    assert_eq!(rgb.len(), 320 * 320 * 3);
    let at = |x: usize, y: usize| &rgb[(y * 320 + x) * 3..(y * 320 + x) * 3 + 3];
    assert!(at(160, 20)[0] < 40, "a bar above: {:?}", at(160, 20));
    assert!(
        at(160, 160)[0] > 200,
        "the canvas in the middle: {:?}",
        at(160, 160)
    );
    assert!(at(160, 300)[0] < 40, "a bar below: {:?}", at(160, 300));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #17: an audio layer on a video plays its sound; `validate` names
/// the one it cannot — a video with no audio track, or every track
/// disabled — and an audio layer on a resource with no sound at all (an
/// image) is a break. A video with sound is clean.
#[test]
fn an_audio_layer_on_a_silent_video_is_named() {
    let dir = std::env::temp_dir().join(format!("promo-silent-video-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    let encode = |name: &str, with_audio: bool| {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=64x64:r=10:d=2",
        ]);
        if with_audio {
            command.args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:a",
                "aac",
            ]);
        }
        command
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"])
            .arg(dir.join("Resources").join(name))
            .status()
            .is_ok_and(|s| s.success())
    };
    if !encode("loud.mp4", true) || !encode("mute.mp4", false) {
        eprintln!("no ffmpeg with libx264; skipping");
        return;
    }
    image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255]))
        .save(dir.join("Resources/still.png"))
        .unwrap();
    let write = |resource: &str, disabled: &str| {
        std::fs::write(
            dir.join("metadata.json"),
            format!(
                r#"{{"id":"P","name":"Silent","createdAt":0,"state":"recorded",
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":64,"canvasHeight":64,"backgroundColorHex":"000000"}},
                "resources":[
                  {{"id":"loud","kind":"video","filename":"loud.mp4","displayName":"loud","addedAt":0,
                    "duration":2,"imageCuts":[],"disabledAudioTrackIndices":[{disabled}]}},
                  {{"id":"mute","kind":"video","filename":"mute.mp4","displayName":"mute","addedAt":0,
                    "duration":2,"imageCuts":[],"disabledAudioTrackIndices":[]}},
                  {{"id":"still","kind":"image","filename":"still.png","displayName":"still","addedAt":0,
                    "pixelWidth":8,"pixelHeight":8,"imageCuts":[],"disabledAudioTrackIndices":[]}}],
                "layers":[{{"id":"bg","name":"Ground","sortIndex":0,"kind":"background","isEnabled":true,
                    "startTime":0,"duration":2,"keyframes":[]}},
                  {{"id":"S","name":"sound","sortIndex":1,"kind":"audio","isEnabled":true,
                    "startTime":0,"duration":2,"resourceID":"{resource}","keyframes":[]}}]}}"#
            ),
        )
        .unwrap();
    };
    write("loud", "");
    let (_, text) = validate(&dir, &["--strict"]);
    assert!(
        text.starts_with("ok — nothing"),
        "a video with sound is clean: {text}"
    );
    write("mute", "");
    let (_, text) = validate(&dir, &[]);
    assert!(
        text.contains(
            r#"layer "sound" plays the sound of the video "mute", which has no audio track"#
        ),
        "{text}"
    );
    write("loud", "0");
    let (_, text) = validate(&dir, &[]);
    assert!(
        text.contains(r#"the video "loud", which has no audio track"#),
        "every track disabled: {text}"
    );
    write("still", "");
    let (_, text) = validate(&dir, &[]);
    assert!(text.starts_with("NOT OK"), "{text}");
    assert!(
        text.contains(r#"is an audio layer on "still" (kind image), which has no sound"#),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
