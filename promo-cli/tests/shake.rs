//! `shake` moves every kind of layer by exactly its `amplitude`, in canvas
//! pixels, on each hit, and brings it back (docs/BEAT-SYNC.md §4) —
//! measured on the rendered frames: the white subject's centre on a beat
//! against the same layer off it.
//!
//! The bug these hold down (v0.3.11, demo 36): a caption was thrown to the
//! top of the canvas (its shifts are its margins), a placed caption did not
//! move and validate blamed a verticalShift nobody wrote, and a placed
//! picture jumped ~228 px on every hit (its base resolved against a square
//! stand-in for its real aspect).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const AMPLITUDE: f64 = 20.0;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-shake-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    dir
}

/// 320×180, black, 120 BPM from 0 s (a bar every 2 s): `layer`, from
/// 0.5 s, shakes on every downbeat by `AMPLITUDE` px.
fn project(dir: &Path, mut layer: Value, resources: Value) -> Value {
    layer["shake"] = json!({"on": "downbeat", "amplitude": AMPLITUDE});
    // From half a second in: the first hit is the downbeat at 2 s, so the
    // frames before it show whether the layer waits at home for it.
    layer["startTime"] = json!(0.5);
    layer["duration"] = json!(3.5);
    let mut all = vec![
        json!({"id": "song", "kind": "audio", "filename": "song.wav",
                              "displayName": "Song", "addedAt": 0, "duration": 30,
                              "beatGrid": {"bpm": 120}}),
    ];
    all.extend(resources.as_array().unwrap().iter().cloned());
    let file = json!({
        "id": "P", "name": "Shake", "createdAt": 0, "state": "recorded", "minReaderVersion": 49,
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {"canvasWidth": 320, "canvasHeight": 180, "fps": 30,
                                "backgroundColorHex": "000000",
                                "subtitleColorHex": "FFFFFF", "subtitleFontSize": 40,
                                "subtitleBackgroundOpacity": 0,
                                "subtitleLeftMargin": 20, "subtitleRightMargin": 20,
                                "subtitleVerticalMargin": 60,
                                "beatClock": {"resourceID": "song", "startTime": 0}},
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 4,
             "keyframes": [{"id": "g", "time": 0, "colorHex": "000000"}]},
            layer
        ],
        "resources": all
    });
    std::fs::write(dir.join("metadata.json"), file.to_string()).unwrap();
    file
}

fn white_png(dir: &Path, name: &str, w: u32, h: u32) {
    image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]))
        .save(dir.join("Resources").join(name))
        .unwrap();
}

fn promo(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo")
}

/// The centre of the bright pixels of the frame at `time`, in canvas px.
fn centre(dir: &Path, time: f64) -> (f64, f64) {
    let out = dir.join(format!("still-{time}.png"));
    let run = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        &time.to_string(),
    ]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let picture = image::open(&out).expect("a still").to_luma8();
    let (w, h) = picture.dimensions();
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (x, y, p) in picture.enumerate_pixels() {
        if p[0] > 128 {
            sx += f64::from(x);
            sy += f64::from(y);
            n += 1.0;
        }
    }
    assert!(n > 20.0, "nothing bright at {time} s");
    // Back to canvas px when the still renders at another size.
    let (kx, ky) = (320.0 / f64::from(w), 180.0 / f64::from(h));
    (sx / n * kx, sy / n * ky)
}

/// On the downbeat at 2 s the subject stands `AMPLITUDE` px from where it
/// stands between beats (1.5 s), and is back there after the decay (2.6 s);
/// validate blames nothing.
fn assert_shakes(dir: &Path, what: &str) {
    let validate = promo(&["validate", dir.to_str().unwrap()]);
    let said = String::from_utf8_lossy(&validate.stdout).into_owned();
    assert!(validate.status.success(), "{what}: {said}");
    assert!(
        said.starts_with("ok — nothing"),
        "{what}: validate blames something: {said}"
    );
    let base = centre(dir, 1.5);
    let hit = centre(dir, 2.0);
    let after = centre(dir, 2.6);
    let moved = ((hit.0 - base.0).powi(2) + (hit.1 - base.1).powi(2)).sqrt();
    assert!(
        (moved - AMPLITUDE).abs() <= 1.5,
        "{what}: the hit moved {moved:.1} px ({base:?} → {hit:?}), not {AMPLITUDE}"
    );
    let back = ((after.0 - base.0).powi(2) + (after.1 - base.1).powi(2)).sqrt();
    assert!(
        back <= 1.0,
        "{what}: {back:.1} px from home after the decay"
    );
    // Before the first hit it stands at home too, not at the hit's offset.
    let early = centre(dir, 1.0);
    let off = ((early.0 - base.0).powi(2) + (early.1 - base.1).powi(2)).sqrt();
    assert!(off <= 1.0, "{what}: {off:.1} px from home before a hit");
}

fn caption(extra: Value) -> Value {
    let mut layer = json!({"id": "words", "name": "Words", "sortIndex": 1, "kind": "caption",
                           "isEnabled": true, "startTime": 0, "duration": 4,
                           "captionText": "HI", "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

#[test]
fn shake_moves_a_caption_placed_by_its_margins() {
    let dir = folder("caption-margins");
    project(&dir, caption(json!({})), json!([]));
    assert_shakes(&dir, "caption by margins");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shake_moves_a_placed_caption() {
    let dir = folder("caption-placed");
    project(
        &dir,
        caption(json!({"captionStyle": {"placement": {"anchor": "center"}}})),
        json!([]),
    );
    assert_shakes(&dir, "placed caption");
    let _ = std::fs::remove_dir_all(&dir);
}

fn picture(extra: Value) -> Value {
    let mut layer = json!({"id": "pic", "name": "Picture", "sortIndex": 1, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 4,
                           "resourceID": "img", "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn picture_resource() -> Value {
    json!([{"id": "img", "kind": "image", "filename": "white.png", "displayName": "White",
            "addedAt": 0, "pixelWidth": 64, "pixelHeight": 36}])
}

#[test]
fn shake_moves_a_placed_image_by_its_amplitude() {
    let dir = folder("image-placed");
    white_png(&dir, "white.png", 64, 36);
    project(
        &dir,
        picture(json!({"keyframes": [{"id": "p", "time": 0,
                                       "placement": {"height": 54, "anchor": "center"}}]})),
        picture_resource(),
    );
    assert_shakes(&dir, "placed image");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shake_moves_an_image_placed_by_numbers() {
    let dir = folder("image-raw");
    white_png(&dir, "white.png", 64, 36);
    project(
        &dir,
        picture(json!({"keyframes": [{"id": "p", "time": 0, "zoom": 0.3,
                                       "horizontalShift": 120, "verticalShift": 60}]})),
        picture_resource(),
    );
    assert_shakes(&dir, "image by numbers");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shake_moves_a_drawing() {
    let dir = folder("drawing");
    project(
        &dir,
        json!({"id": "ink", "name": "Ink", "sortIndex": 1, "kind": "drawing",
               "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "box",
               // A drawing fits its content to the canvas; a quarter of
               // that keeps the box clear of the edges a hit could clip.
               "keyframes": [{"id": "z", "time": 0, "zoom": 0.25}]}),
        json!([{"id": "box", "kind": "drawing", "filename": "", "displayName": "Box",
                "addedAt": 0,
                "drawing": {"shapes": [{"id": "r", "kind": "rect",
                                        "points": [[130, 60], [190, 120]],
                                        "strokeColorHex": "FFFFFF", "strokeWidth": 0,
                                        "fillColorHex": "FFFFFF",
                                        "arrowStart": false, "arrowEnd": false}]}}]),
    );
    assert_shakes(&dir, "drawing");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shake_moves_a_placed_video() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg missing: the video case needs a clip");
        return;
    }
    let dir = folder("video-placed");
    let clip = dir.join("Resources/white.mp4");
    let made = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=white:s=64x36:d=4:r=30",
        ])
        .args(["-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(made.success());
    project(
        &dir,
        json!({"id": "clip", "name": "Clip", "sortIndex": 1, "kind": "video",
               "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "vid",
               "keyframes": [{"id": "p", "time": 0,
                              "placement": {"height": 54, "anchor": "center"}}]}),
        json!([{"id": "vid", "kind": "video", "filename": "white.mp4", "displayName": "White",
                "addedAt": 0, "duration": 4, "trimStart": 0, "trimEnd": 4,
                "videoNaturalWidth": 64, "videoNaturalHeight": 36}]),
    );
    assert_shakes(&dir, "placed video");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bright area of the frame at `time`, in canvas px².
fn area(dir: &Path, time: f64) -> f64 {
    let out = dir.join(format!("area-{time}.png"));
    let run = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        &time.to_string(),
    ]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let picture = image::open(&out).expect("a still").to_luma8();
    let (w, h) = picture.dimensions();
    let bright = picture.pixels().filter(|p| p[0] > 128).count() as f64;
    bright * (320.0 / f64::from(w)) * (180.0 / f64::from(h))
}

/// A `pulse` on a placed picture's size grows its box by `amount` on the
/// beat and nowhere else — not from the layer's start to its first beat —
/// and on a caption it grows the words (its fontSize), never shrinks them
/// to a point.
#[test]
fn pulse_grows_a_placed_picture_and_a_caption_on_the_beat_only() {
    for (name, layer, resources) in [
        (
            "pulse-image",
            picture(json!({"keyframes": [{"id": "p", "time": 0,
                                           "placement": {"height": 54, "anchor": "center"}}]})),
            picture_resource(),
        ),
        ("pulse-caption", caption(json!({})), json!([])),
    ] {
        let dir = folder(name);
        white_png(&dir, "white.png", 64, 36);
        let mut file = project(&dir, layer, resources);
        let layers = file["layers"].as_array_mut().unwrap();
        let shaken = layers.last_mut().unwrap().as_object_mut().unwrap();
        shaken.remove("shake");
        shaken.insert(
            "pulse".into(),
            json!({"every": "downbeat", "property": "placement", "amount": 0.5}),
        );
        std::fs::write(dir.join("metadata.json"), file.to_string()).unwrap();
        let (early, between, peak) = (area(&dir, 1.0), area(&dir, 1.5), area(&dir, 2.0));
        // Between beats it is its full size: "HI" at 40 points, the
        // 96×54 picture — not text shrunk to a point.
        assert!(between > 300.0, "{name}: only {between} px² between beats");
        assert!(
            (early - between).abs() <= between * 0.03,
            "{name}: {early} before the first beat, {between} between beats"
        );
        let grown = peak / between;
        assert!(
            (1.8..2.7).contains(&grown),
            "{name}: the peak's area is ×{grown:.2} of its base, not about ×2.25"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
