//! The authoring verbs through the built binary (review 2026-09-27,
//! P1-22): the MCP tools' own functions, with the tool's arguments as one
//! JSON object — so a CLI-only agent meets the same defaults and checks.

use std::io::Write;
use std::process::{Command, Stdio};

fn promo(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A project built one verb at a time: init, a caption, two keyframes —
/// the second, created without a ramp, ramps the whole gap from the first
/// (the tools' default; a hand-written keyframe holds until 0.5 s before
/// it, which read as "keyframes never animate") — a batch through apply,
/// and a strict validate that passes.
#[test]
fn a_project_built_through_the_authoring_verbs_validates() {
    let root = std::env::temp_dir().join(format!("promo-author-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dir = root.join("Built.promo");
    let project = dir.to_str().unwrap();

    let (ok, out, err) = promo(&["init", project, "--args", r#"{"canvas":"640x360"}"#]);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = promo(&[
        "upsert-layer",
        project,
        "--args",
        r#"{"kind":"caption","id":"title","captionText":"Hello","startTime":0,"duration":3}"#,
    ]);
    assert!(ok, "{out}{err}");
    for keyframe in [
        r#"{"layer":"title","id":"k0","time":0,"placement":{"anchor":"topLeft","offset":[20,20]}}"#,
        r#"{"layer":"title","id":"k1","time":2,"placement":{"anchor":"bottomRight","offset":[-20,-20]}}"#,
    ] {
        let (ok, out, err) = promo(&["upsert-keyframe", project, "--args", keyframe]);
        assert!(ok, "{out}{err}");
    }
    // --args from a file.
    let batch = root.join("batch.json");
    std::fs::write(
        &batch,
        r#"{"commands":[{"kind":"renameLayer","layerID":"title","name":"Headline"}]}"#,
    )
    .unwrap();
    let (ok, out, err) = promo(&["apply", project, "--args", &format!("@{}", batch.display())]);
    assert!(ok && out.contains("renameLayer"), "{out}{err}");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    let title = doc["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "title")
        .expect("the caption");
    assert_eq!(title["name"], "Headline");
    assert_eq!(
        title["keyframes"][1]["transitionDuration"], 2.0,
        "the created keyframe ramps from the previous one"
    );

    let (ok, out, err) = promo(&["validate", project, "--strict"]);
    assert!(ok && out.starts_with("ok"), "{out}{err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// `--args -` reads stdin; a flag the verb does not take names where the
/// arguments go instead of being guessed at.
#[test]
fn arguments_come_from_stdin_and_a_stray_flag_says_where_they_go() {
    let root = std::env::temp_dir().join(format!("promo-author-stdin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let project = root.join("Piped.promo");
    let mut child = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(["init", project.to_str().unwrap(), "--args", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run promo");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"canvas":"320x180","name":"Piped"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(project.join("metadata.json").exists());

    let (ok, _, err) = promo(&[
        "upsert-keyframe",
        project.to_str().unwrap(),
        "--layer",
        "bg",
    ]);
    assert!(!ok);
    assert!(
        err.contains("--args") && err.contains("promo_upsert_keyframe"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A picture that needs normalising — stored sideways with an EXIF
/// orientation, in Display P3 — is staged as an upright sRGB PNG, and the
/// resource is stamped with the upright size (review 2026-09-27, P1-23):
/// the file in Resources/ then reads the same on every host.
#[test]
fn a_rotated_p3_picture_is_staged_upright_in_srgb() {
    use image::ImageEncoder;
    let root = std::env::temp_dir().join(format!("promo-author-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("photo.jpg.png");
    let mut stored = Vec::new();
    for _y in 0..20 {
        for x in 0..40 {
            stored.extend_from_slice(if x < 20 {
                &[180, 60, 60, 255]
            } else {
                &[0, 0, 0, 255]
            });
        }
    }
    let exif: Vec<u8> = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder.set_exif_metadata(exif).unwrap();
    encoder
        .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
        .unwrap();
    encoder
        .write_image(&stored, 40, 20, image::ExtendedColorType::Rgba8)
        .unwrap();
    std::fs::write(&source, &bytes).unwrap();

    let dir = root.join("Staged.promo");
    let project = dir.to_str().unwrap();
    let (ok, out, err) = promo(&["init", project, "--args", r#"{"canvas":"400x400"}"#]);
    assert!(ok, "{out}{err}");
    let args = format!(
        r#"{{"kind":"image","id":"pic","file":"{}","placement":{{"height":200}}}}"#,
        source.display()
    );
    let (ok, out, err) = promo(&["upsert-layer", project, "--args", &args]);
    assert!(ok, "{out}{err}");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    let resource = &doc["resources"][0];
    assert_eq!(resource["pixelWidth"], 20.0, "the upright width");
    assert_eq!(resource["pixelHeight"], 40.0, "the upright height");
    let staged = dir
        .join("Resources")
        .join(resource["filename"].as_str().unwrap());
    let picture = image::open(&staged).unwrap().to_rgba8();
    assert_eq!(picture.dimensions(), (20, 40), "stored upright");
    let [r, g, _, _] = picture.get_pixel(10, 5).0;
    assert!(r > 188 && g < 54, "stored in sRGB: {r} {g}");
    let _ = std::fs::remove_dir_all(&root);
}
