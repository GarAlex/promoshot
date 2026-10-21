//! AC8 (docs/SPEECH-SYNC.md §7.3, slice 8): four stills `per` sentence of
//! the synthetic take, rendered, cut where each sentence starts less the
//! lead — read from the rendered pixels, against the take's own record.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const FPS: f64 = 30.0;
/// Where the voice-over starts on the timeline.
const AT: f64 = 1.0;
const LEAD: f64 = 0.1;
const COLOURS: [[u8; 3]; 4] = [[220, 30, 30], [30, 200, 60], [40, 60, 230], [230, 210, 30]];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech")
}

fn folder(golden: &Value) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-ac8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    let audio = dir.join("Resources/intro.wav");
    std::fs::copy(fixtures().join("synthetic-60w.wav"), &audio).unwrap();
    let record: promo_model::speech::TakeTiming = serde_json::from_value(golden.clone()).unwrap();
    promo_model::word_cache::store(&dir, &audio, record).unwrap();
    for (k, colour) in COLOURS.iter().enumerate() {
        let picture = image::RgbImage::from_pixel(64, 36, image::Rgb(*colour));
        picture
            .save(dir.join(format!("Resources/s{k}.png")))
            .unwrap();
    }
    let slide = |k: usize| {
        json!({"id": format!("s{k}"), "name": format!("Slide {}", k + 1), "sortIndex": k + 1,
               "kind": "image", "isEnabled": true, "startTime": 0, "duration": 1,
               "resourceID": format!("p{k}"),
               "keyframes": [{"id": format!("k{k}"), "time": 0, "transitionDuration": 0,
                              "placement": {"mode": "fill"}}]})
    };
    let mut layers = vec![
        json!({"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio", "isEnabled": true,
               "startTime": AT, "resourceID": "intro", "keyframes": []}),
    ];
    layers.extend((0..4).map(slide));
    layers[1]["startTime"] = json!({"sentence": 1, "of": "intro", "offset": -LEAD});
    let mut resources = vec![
        json!({"id": "intro", "kind": "audio", "filename": "intro.wav",
        "displayName": "Intro", "addedAt": 0, "duration": golden["duration"],
        "imageCuts": [], "disabledAudioTrackIndices": [], "transcript": golden["script"]}),
    ];
    resources.extend((0..4).map(|k| {
        json!({"id": format!("p{k}"), "kind": "image", "filename": format!("s{k}.png"),
               "displayName": format!("Still {}", k + 1), "addedAt": 0,
               "pixelWidth": 64, "pixelHeight": 36, "imageCuts": [],
               "disabledAudioTrackIndices": []})
    }));
    let meta = json!({
        "id": "P", "name": "AC8", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 12, "videoDuration": 12, "subtitles": [],
        "minReaderVersion": 50,
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90, "fps": 30,
                                "backgroundColorHex": "000000",
                                "sequence": [{"layers": ["s0", "s1", "s2", "s3"], "per": "sentence",
                                              "of": "intro", "lead": LEAD}]},
        "layers": layers,
        "resources": resources
    });
    std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
    dir
}

/// AC8: each cut first shows on the frame of its sentence's start less the
/// lead, ±1 frame.
#[test]
fn one_slide_per_sentence() {
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("synthetic-60w.json")).unwrap(),
    )
    .unwrap();
    let dir = folder(&golden);
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
            "11.5",
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
    // Which slide each frame shows, by the colour at its centre.
    let shown: Vec<Option<usize>> = frames
        .iter()
        .map(|path| {
            let img = image::open(path).unwrap().to_rgb8();
            let p = img.get_pixel(img.width() / 2, img.height() / 2);
            COLOURS
                .iter()
                .position(|c| (0..3).all(|i| (i32::from(c[i]) - i32::from(p[i])).abs() < 24))
        })
        .collect();
    let first = |k: usize| shown.iter().position(|s| *s == Some(k));
    let sentences = golden["sentences"].as_array().unwrap();
    for (k, sentence) in sentences.iter().enumerate().take(4) {
        let start = sentence["start"].as_f64().unwrap();
        let expected = ((AT + start - LEAD) * FPS - 1e-9).ceil();
        let got = first(k).unwrap_or_else(|| panic!("slide {k} never shows: {shown:?}"));
        assert!(
            (got as f64 - expected).abs() <= 1.0,
            "slide {} first shows at frame {got}, its sentence less the lead is frame {expected}",
            k + 1
        );
    }
    // Before the first cut: nothing; the slides in order, each held.
    assert_eq!(shown[0], None, "no slide before the voice");
    let order: Vec<usize> = shown
        .iter()
        .flatten()
        .copied()
        .fold(Vec::new(), |mut v, s| {
            if v.last() != Some(&s) {
                v.push(s);
            }
            v
        });
    assert_eq!(order, [0, 1, 2, 3]);
    // Four slides for eight sentences: the last holds the rest, and validate
    // says so.
    let said = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(&dir)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&said.stdout);
    assert!(said.contains("4 slides for the 8 sentences"), "{said}");
    // The open read each word's prominence from the audio, and kept it.
    let record = promo_model::word_cache::lookup(
        &dir,
        &dir.join("Resources/intro.wav"),
        golden["script"].as_str().unwrap(),
    )
    .expect("the record");
    assert!(record.timing.words.iter().all(|w| w.prominence.is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}
