//! AC1 (docs/SPEECH-SYNC.md §7.1, slice 7): a karaoke caption synced to the
//! ElevenLabs fixture take lights each word within one frame of the
//! provider's word start — read from the rendered pixels, the renderer's
//! own answer, against the provider's own times.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const SCRIPT: &str = "Meet Euclid. Geometry, proved step by step.";
/// The provider's word starts in the fixture (`elevenlabs-with-timestamps`).
const STARTS: [f64; 7] = [0.12, 0.42, 1.4, 2.16, 2.56, 2.84, 3.02];
const FPS: f64 = 30.0;
/// Where the voice-over starts on the timeline.
const AT: f64 = 0.5;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech")
}

/// The project: a voice-over of the fixture take at 0.5 s, and its line as
/// a white caption whose active word turns pure red.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-ac1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    let audio = dir.join("Resources/intro.mp3");
    std::fs::copy(fixtures().join("elevenlabs-with-timestamps.mp3"), &audio).unwrap();
    // The provider's answer, as the settle stores it when a take is bought.
    let answer: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("elevenlabs-with-timestamps.json")).unwrap(),
    )
    .unwrap();
    let take = promo_speech::timing::elevenlabs_take(SCRIPT, &answer).unwrap();
    let record = promo_speech::timing::record(SCRIPT, &take.spoken.unwrap(), 3.7);
    promo_model::word_cache::store(&dir, &audio, record).unwrap();
    let meta = json!({
        "id": "P", "name": "AC1", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4.4, "videoDuration": 4.4, "subtitles": [],
        "minReaderVersion": 50,
        "compositionSettings": {"canvasWidth": 960, "canvasHeight": 240, "fps": 30},
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 4.4,
             "keyframes": [{"id": "g", "time": 0, "colorHex": "101010"}]},
            {"id": "vo", "name": "VO", "sortIndex": 1, "kind": "audio", "isEnabled": true,
             "startTime": AT, "duration": 3.7, "resourceID": "intro", "keyframes": []},
            {"id": "cap", "name": "Line", "sortIndex": 2, "kind": "caption", "isEnabled": true,
             "startTime": 0.25, "duration": 4.1, "captionText": SCRIPT,
             "captionStyle": {"fontSize": 40, "textColorHex": "FFFFFF",
                              "backgroundOpacity": 0, "shadowOpacity": 0, "strokeWidth": 0,
                              "placement": {"anchor": "center"},
                              "reveal": {"by": "word", "mode": "highlight",
                                         "highlightColorHex": "FF0000",
                                         "sync": {"of": "intro"}}},
             "keyframes": []}
        ],
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 3.7,
                       "imageCuts": [], "disabledAudioTrackIndices": [],
                       "speech": {"text": SCRIPT, "provider": "elevenlabs",
                                  "voiceID": "fixture"}}]
    });
    std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
    dir
}

/// Red ink: the highlight, never the white text or the ground.
fn red(p: &image::Rgba<u8>) -> bool {
    p[0] > 150 && p[1] < 90 && p[2] < 90
}

/// AC1: each word's highlight first appears within ±1 frame of the frame
/// its provider start falls on.
#[test]
fn captions_reveal_each_word_within_one_frame() {
    let dir = folder("own");
    let out = dir.join("frames");
    let run = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("frames")
        .arg(&dir)
        .args([
            "--out",
            out.to_str().unwrap(),
            "--fps",
            "30",
            "--from",
            "0",
            "--to",
            "4.0",
        ])
        .output()
        .expect("run promo");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let mut frames: Vec<PathBuf> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    frames.sort();
    assert!(frames.len() >= 115, "{} frames", frames.len());
    // Per frame, the red ink's centre: which word is lit, if any.
    let lit: Vec<Option<(f64, f64)>> = frames
        .iter()
        .map(|path| {
            let img = image::open(path).unwrap().to_rgba8();
            let mut xs = Vec::new();
            for (x, y, p) in img.enumerate_pixels() {
                if red(p) {
                    xs.push((x as f64, y as f64));
                }
            }
            let n = xs.len() as f64;
            (xs.len() > 20).then(|| {
                (
                    xs.iter().map(|p| p.0).sum::<f64>() / n,
                    xs.iter().map(|p| p.1).sum::<f64>() / n,
                )
            })
        })
        .collect();
    // The words, in reading order, by where a lit word sits: a new word is
    // a jump of its centre by more than 10 px.
    let mut words: Vec<(usize, (f64, f64))> = Vec::new(); // (first frame, centre)
    for (frame, centre) in lit.iter().enumerate() {
        let Some(c) = centre else { continue };
        match words.last() {
            Some((_, last)) if (c.0 - last.0).abs() < 10.0 && (c.1 - last.1).abs() < 10.0 => {}
            _ => words.push((frame, *c)),
        }
    }
    assert_eq!(words.len(), STARTS.len(), "lit words: {words:?}");
    assert!(
        words.windows(2).all(|w| w[1].1 .1 > w[0].1 .1 + 10.0
            || ((w[1].1 .1 - w[0].1 .1).abs() < 10.0 && w[1].1 .0 > w[0].1 .0)),
        "in reading order: {words:?}"
    );
    for (k, ((frame, _), start)) in words.iter().zip(STARTS).enumerate() {
        let expected = ((AT + start) * FPS - 1e-9).ceil();
        assert!(
            (*frame as f64 - expected).abs() <= 1.0,
            "word {} lit at frame {frame}, its start is frame {expected}",
            k + 1
        );
    }
    // Between "Euclid." and "Geometry," the voice pauses: nothing is lit.
    let pause = ((AT + 1.1) * FPS) as usize;
    assert_eq!(lit[pause], None, "a highlight goes out between words");
    // explain shows how the caption's words matched the take.
    let out = promo_author::explain(&json!({"project": dir, "time": 1.0, "layer": "cap"}), None)
        .expect("explains");
    let out: Value = serde_json::from_str(&out).unwrap();
    let reveal = &out["layers"][0]["reveal"];
    assert_eq!(reveal["units"], 7, "{reveal}");
    assert_eq!(reveal["matched"], 7, "{reveal}");
    assert_eq!(reveal["words"], "words 1\u{2013}7", "{reveal}");
    assert_eq!(reveal["sync"], json!({"of": "intro"}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same karaoke line with its reveal on the project-wide caption style
/// (`compositionSettings.subtitleReveal`): rendered, the word being said
/// is lit and the pause after "Euclid." is dark — read from the pixels.
#[test]
fn a_shared_synced_reveal_lights_the_word_being_said() {
    let dir = folder("shared");
    let file = dir.join("metadata.json");
    let mut meta: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let reveal = meta["layers"][2]["captionStyle"]
        .as_object_mut()
        .unwrap()
        .remove("reveal")
        .unwrap();
    meta["compositionSettings"]["subtitleReveal"] = reveal;
    std::fs::write(&file, meta.to_string()).unwrap();
    let lit = |time: f64| -> usize {
        let out = dir.join(format!("shared-{time}.png"));
        let run = Command::new(env!("CARGO_BIN_EXE_promo"))
            .arg("still")
            .arg(&dir)
            .arg("--out")
            .arg(&out)
            .args(["--time", &time.to_string()])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        image::open(&out)
            .unwrap()
            .to_rgba8()
            .pixels()
            .filter(|p| red(p))
            .count()
    };
    assert!(lit(AT + 0.6) > 20, "\"Euclid\" is lit while it is said");
    assert_eq!(lit(AT + 1.15), 0, "the pause after it is dark");
    let _ = std::fs::remove_dir_all(&dir);
}

/// By character, a synced highlight types across the word as it is said:
/// the lit letter of "Geometry," moves right through the word's span
/// rather than every letter arriving with the word — read from the pixels.
#[test]
fn a_character_reveal_walks_inside_the_word() {
    let dir = folder("chars");
    let file = dir.join("metadata.json");
    let mut meta: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    meta["layers"][2]["captionStyle"]["reveal"]["by"] = json!("character");
    std::fs::write(&file, meta.to_string()).unwrap();
    // Word 3's own span, from the record the folder stored.
    let answer: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("elevenlabs-with-timestamps.json")).unwrap(),
    )
    .unwrap();
    let take = promo_speech::timing::elevenlabs_take(SCRIPT, &answer).unwrap();
    let record = promo_speech::timing::record(SCRIPT, &take.spoken.unwrap(), 3.7);
    let word = &record.words[2];
    assert!(word.text.starts_with("Geometry"), "{}", word.text);
    let centre = |time: f64| -> Option<f64> {
        let out = dir.join(format!("chars-{time}.png"));
        let run = Command::new(env!("CARGO_BIN_EXE_promo"))
            .arg("still")
            .arg(&dir)
            .arg("--out")
            .arg(&out)
            .args(["--time", &time.to_string()])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let img = image::open(&out).unwrap().to_rgba8();
        let xs: Vec<f64> = img
            .enumerate_pixels()
            .filter(|(_, _, p)| red(p))
            .map(|(x, _, _)| x as f64)
            .collect();
        (xs.len() > 5).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    };
    let span = word.end - word.start;
    let early = centre(AT + word.start + 0.1 * span).expect("a letter is lit early in the word");
    let late = centre(AT + word.start + 0.8 * span).expect("a letter is lit late in the word");
    // "Geometry," at 40 px is some 180 px wide: seven letters apart is far
    // more than a letter's width.
    assert!(late - early > 60.0, "early {early}, late {late}");
    let _ = std::fs::remove_dir_all(&dir);
}
