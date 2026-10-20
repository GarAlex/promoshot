//! Musical time through every loader (docs/BEAT-SYNC.md, slice 2): the CLI's
//! open, `explain`, the editor document and `validate` all read the same
//! resolved seconds — one pass behind all of them — and a render waits for a
//! beat clock whose audio has no grid.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn folder(name: &str, metadata: &Value) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-beats-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    std::fs::write(dir.join("metadata.json"), metadata.to_string()).unwrap();
    dir
}

fn beat_project(grid: Option<Value>) -> Value {
    let mut song = json!({"id": "song", "kind": "audio", "filename": "song.m4a",
                          "displayName": "Song", "addedAt": 0, "duration": 30});
    if let Some(grid) = grid {
        song["beatGrid"] = grid;
    }
    json!({
        "id": "P", "name": "Beats", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 8, "videoDuration": 8, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90,
                                "beatClock": {"resourceID": "song", "startTime": 0.25}},
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 8,
             "keyframes": [{"id": "g", "time": 0, "colorHex": "202030"}]},
            {"id": "s1", "name": "One", "sortIndex": 1, "kind": "caption", "isEnabled": true,
             "startTime": {"beat": 3}, "duration": {"bars": 1}, "captionText": "One",
             "keyframes": []},
            {"id": "s2", "name": "Two", "sortIndex": 2, "kind": "caption", "isEnabled": true,
             "startTime": 0, "timing": {"start": {"from": "previousEnd", "offset": 0}},
             "duration": {"beats": 2}, "captionText": "Two", "keyframes": []}
        ],
        "resources": [song]
    })
}

fn life(dir: &Path, id: &str) -> (f64, f64) {
    let out = promo_author::explain(&json!({"project": dir, "time": 0.1, "layer": id}), None)
        .expect("explains");
    let out: Value = serde_json::from_str(&out).unwrap();
    let life = &out["layers"][0]["life"];
    (
        life["start"].as_f64().unwrap(),
        life["end"].as_f64().unwrap(),
    )
}

/// The CLI's open, `explain` and the editor document give the same numbers
/// for the same file — bit for bit where no JSON text sits between — and
/// they are the grid's.
#[test]
fn every_loader_gives_the_same_numbers() {
    let file = beat_project(Some(json!({"bpm": 100, "offset": 0.05})));
    let dir = folder("loaders", &file);
    let beat = 60.0 / 100.0;
    let start = 0.25 + (0.05 + 2.0 * beat);

    let cli = promo_cli::project::Project::open(&dir).expect("opens");
    let layer = |meta: &promo_model::ProjectMetadata, id: &str| {
        let l = meta
            .layers
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .clone();
        (l.start_time, l.start_time + l.duration.unwrap())
    };
    let (s1, s2) = (layer(&cli.meta, "s1"), layer(&cli.meta, "s2"));
    assert_eq!(s1.0, start);
    assert!((s1.1 - (0.25 + (0.05 + 6.0 * beat))).abs() < 1e-12);
    assert_eq!(s2.0, s1.1, "the attachment follows the musical end");

    // explain answers in JSON text, and serde_json's default float parse
    // may land one ulp off the shortest round-trip it printed — so here, and
    // only here, to a picosecond.
    for (id, expected) in [("s1", s1), ("s2", s2)] {
        let (start, end) = life(&dir, id);
        assert!(
            (start - expected.0).abs() < 1e-12 && (end - expected.1).abs() < 1e-12,
            "explain disagrees on {id}: {:?} vs {expected:?}",
            (start, end)
        );
    }

    let mut doc = promo_editor::document::Document::open(&file.to_string()).unwrap();
    doc.apply(&serde_json::from_value(json!({"kind": "renameProject", "name": "Beats"})).unwrap())
        .unwrap();
    assert_eq!(layer(doc.meta(), "s1"), s1, "the document disagrees");
    assert_eq!(layer(doc.meta(), "s2"), s2, "the document disagrees");

    // validate sees a clean file: the grid is stated.
    let report = promo_timeline::report::file_report(&file.to_string(), None).unwrap();
    assert!(
        !format!("{report:?}").contains("beat"),
        "a stated grid draws no beat finding: {report:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clock whose audio has no grid lays out on 120 BPM — and a render
/// refuses to start until one is stated, saying how.
#[test]
fn a_render_waits_for_a_missing_grid() {
    let dir = folder("waits", &beat_project(None));
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("still")
        .arg(&dir)
        .output()
        .expect("run promo");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("has no beat grid yet"), "{said}");
    assert!(said.contains("beatGrid"), "{said}");

    // validate names it, as a warning.
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(&dir)
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("\"song\" has no beat grid yet"), "{said}");

    // Stated, the render is no longer held for it.
    std::fs::write(
        dir.join("metadata.json"),
        beat_project(Some(json!({"bpm": 120}))).to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("still")
        .arg(&dir)
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!said.contains("no beat grid"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[path = "../../promo-media/tests/support/click_track.rs"]
mod click_track;

/// `promo beats` answers the tracker's JSON for a file, and for a project
/// analyses the beat clock's song once, caches it and answers from the
/// cache after that.
#[test]
fn promo_beats_json_shape() {
    let (samples, _) = click_track::click_track(128.0, 12.0, 44_100.0, None);
    let dir = folder("beats-cli", &beat_project(None));
    let song = dir.join("Resources/song.m4a");
    // A WAV under the resource's name: the tracker reads it by content.
    std::fs::write(&song, click_track::wav(&samples, 1, 44_100)).unwrap();
    let promo = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_promo"))
            .arg("beats")
            .args(args)
            .output()
            .expect("run promo");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let file: Value = serde_json::from_str(&promo(&[song.to_str().unwrap(), "--json"])).unwrap();
    for key in [
        "trackerVersion",
        "source",
        "duration",
        "bpm",
        "offset",
        "timeSignature",
        "grid",
        "confidence",
        "beats",
        "downbeats",
        "sections",
    ] {
        assert!(file.get(key).is_some(), "no `{key}`: {file}");
    }
    assert!((file["bpm"].as_f64().unwrap() - 128.0).abs() < 0.05);
    assert_eq!(file["source"]["sha256"].as_str().unwrap().len(), 32);
    assert!(file.get("fromCache").is_none(), "a bare file has no cache");

    let first: Value = serde_json::from_str(&promo(&[dir.to_str().unwrap(), "--json"])).unwrap();
    assert_eq!(first["fromCache"], false);
    let key = first["source"]["sha256"].as_str().unwrap();
    assert!(dir.join(format!("Resources/.beats/{key}.json")).exists());
    let again: Value = serde_json::from_str(&promo(&[dir.to_str().unwrap(), "--json"])).unwrap();
    assert_eq!(again["fromCache"], true);
    assert_eq!(again["beats"], first["beats"]);
    let text = promo(&[dir.to_str().unwrap(), "--resource", "song"]);
    assert!(
        text.starts_with("128.0") && text.contains(" BPM, 4/4, "),
        "{text}"
    );
    assert!(text.contains("from the project's cache"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn has_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// Sixteen colours far apart: the six hues at two brightnesses, then four
/// more mixes.
const SLIDES: [[u8; 3]; 16] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 0],
    [0, 255, 255],
    [255, 0, 255],
    [128, 0, 0],
    [0, 128, 0],
    [0, 0, 128],
    [128, 128, 0],
    [0, 128, 128],
    [128, 0, 128],
    [255, 255, 255],
    [255, 128, 0],
    [128, 255, 0],
    [0, 128, 255],
];

/// A beat-cut slideshow: sixteen stills, each two beats of `song`, a
/// sequence on its beat clock — and nothing in the file says a second.
fn beat_slideshow(name: &str, bpm: f64) -> (PathBuf, Value) {
    let dir = folder(name, &json!({}));
    let mut resources = vec![
        json!({"id": "song", "kind": "audio", "filename": "song.wav",
                                    "displayName": "Song", "addedAt": 0, "duration": 30}),
    ];
    let mut layers = vec![
        json!({"id": "music", "name": "Music", "sortIndex": 0, "kind": "audio",
                                 "isEnabled": true, "startTime": 0, "resourceID": "song",
                                 "keyframes": []}),
    ];
    let mut sequence = Vec::new();
    for (k, rgb) in SLIDES.iter().enumerate() {
        let picture = image::RgbImage::from_pixel(160, 90, image::Rgb(*rgb));
        picture
            .save(dir.join(format!("Resources/slide{k}.png")))
            .unwrap();
        resources.push(json!({"id": format!("r{k}"), "kind": "image",
                              "filename": format!("slide{k}.png"), "displayName": format!("{k}"),
                              "addedAt": 0, "pixelWidth": 160, "pixelHeight": 90}));
        layers.push(json!({"id": format!("s{k}"), "name": format!("Slide {k}"),
                           "sortIndex": k + 1, "kind": "image", "isEnabled": true,
                           "startTime": 0, "duration": 1, "resourceID": format!("r{k}"),
                           "keyframes": [{"id": format!("k{k}"), "time": 0,
                                          "placement": {"mode": "fill"}}]}));
        sequence.push(json!({"layer": format!("s{k}"), "beats": 2}));
    }
    let metadata = json!({
        "id": "P", "name": "Beat cut", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 31, "videoDuration": 31, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90, "fps": 30,
                                "beatClock": {"resourceID": "song"}, "sequence": sequence},
        "layers": layers, "resources": resources
    });
    std::fs::write(
        dir.join("metadata.json"),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let (samples, _) = click_track::click_track(bpm, 30.0, 44_100.0, None);
    std::fs::write(
        dir.join("Resources/song.wav"),
        click_track::wav(&samples, 1, 44_100),
    )
    .unwrap();
    (dir, metadata)
}

/// The rendered video's frames, as which slide each one shows (by the
/// colour at its centre), at the project's 30 fps.
fn slides_shown(dir: &Path) -> Vec<Option<usize>> {
    let out = dir.join("cut.mp4");
    let status = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(["video"])
        .arg(dir)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run promo");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args([
            "-vf",
            "scale=16:9",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .expect("ffmpeg decodes the render");
    raw.stdout
        .chunks_exact(16 * 9 * 3)
        .map(|frame| {
            let at = (4 * 16 + 8) * 3;
            let pixel = [frame[at], frame[at + 1], frame[at + 2]];
            let distance = |c: &[u8; 3]| {
                (0..3)
                    .map(|i| (f64::from(c[i]) - f64::from(pixel[i])).powi(2))
                    .sum::<f64>()
            };
            if distance(&[0, 0, 0]) < 40.0 * 40.0 {
                return None;
            }
            (0..SLIDES.len()).min_by(|&a, &b| distance(&SLIDES[a]).total_cmp(&distance(&SLIDES[b])))
        })
        .collect()
}

/// Every cut — the first frame of each slide — within one frame of the
/// beat it was meant for, both the tracker's (from the project's cache)
/// and the one the fixture put there.
fn assert_cuts_on_beats(dir: &Path, bpm: f64) {
    let shown = slides_shown(dir);
    let cache = std::fs::read_dir(dir.join("Resources/.beats"))
        .expect("the open analysed the song")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|v| (v["bpm"].as_f64().unwrap_or(0.0) - bpm).abs() < 0.05)
        })
        .expect("an analysis at this tempo");
    let analysis: Value = serde_json::from_str(&std::fs::read_to_string(cache).unwrap()).unwrap();
    let detected: Vec<f64> = analysis["beats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_f64().unwrap())
        .collect();
    for k in 0..SLIDES.len() {
        let first = shown
            .iter()
            .position(|s| *s == Some(k))
            .unwrap_or_else(|| panic!("slide {k} never shows at {bpm} BPM"));
        let truth = click_track::OFFSET + 2.0 * k as f64 * 60.0 / bpm;
        for (name, beat) in [("detected", detected[2 * k]), ("true", truth)] {
            let off = first as f64 - beat * 30.0;
            assert!(
                off.abs() <= 1.0,
                "{bpm} BPM: slide {k} first shows at frame {first}, {off:.2} frames from the \
                 {name} beat at {beat:.4} s"
            );
        }
    }
}

/// AC1: sixteen stills, a sequence of two beats each on the 64 BPM song
/// (thirty-two beats, 30 s): the video's every cut lands within a frame of
/// the beat — the song analysed on open, nothing stated in seconds.
#[test]
fn beat_cut_slideshow_cuts_land_on_beats() {
    if !has_ffmpeg() {
        eprintln!("no ffmpeg — the render cannot be written or read back");
        return;
    }
    let (dir, _) = beat_slideshow("ac1", 64.0);
    assert_cuts_on_beats(&dir, 64.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC2: the same file with only the song's `filename` changed to a 96 BPM
/// track: every cut moves onto the new beats, and the JSON is otherwise the
/// same bytes.
#[test]
fn swapping_the_track_retimes_the_edit() {
    if !has_ffmpeg() {
        eprintln!("no ffmpeg — the render cannot be written or read back");
        return;
    }
    let (dir, _) = beat_slideshow("ac2", 64.0);
    let (samples, _) = click_track::click_track(96.0, 30.0, 44_100.0, None);
    std::fs::write(
        dir.join("Resources/song96.wav"),
        click_track::wav(&samples, 1, 44_100),
    )
    .unwrap();
    let text = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
    assert_eq!(text.matches("\"song.wav\"").count(), 1);
    let swapped = text.replace("\"song.wav\"", "\"song96.wav\"");
    std::fs::write(dir.join("metadata.json"), &swapped).unwrap();
    assert_cuts_on_beats(&dir, 96.0);
    assert_eq!(
        std::fs::read_to_string(dir.join("metadata.json")).unwrap(),
        swapped,
        "the render wrote the file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clock whose song is a compressed file and no ffmpeg to read it: the
/// beats cannot be measured, which validate names as a break.
#[test]
fn missing_ffmpeg_reports_a_break() {
    let dir = folder("noffmpeg", &beat_project(None));
    std::fs::write(dir.join("Resources/song.m4a"), b"not a wav, needs ffmpeg").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(&dir)
        .env("PATH", "")
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.starts_with("NOT OK"), "{said}");
    assert!(said.contains("could not be measured"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A white flash on every downbeat, rendered by the CLI: the frame on a
/// downbeat is lit, the frame on an off-beat is not — read off the pixels.
#[test]
fn flash_brightens_each_downbeat_frame() {
    let file = json!({
        "id": "P", "name": "Flash", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90, "fps": 30,
                                "beatClock": {"resourceID": "song", "startTime": 0},
                                "flash": {"on": "downbeat", "colorHex": "FFFFFF",
                                          "opacity": 0.8, "duration": {"beats": 0.5}}},
        "layers": [{"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
                    "isEnabled": true, "startTime": 0, "duration": 4,
                    "keyframes": [{"id": "g", "time": 0, "colorHex": "000000"}]}],
        "resources": [{"id": "song", "kind": "audio", "filename": "song.wav",
                       "displayName": "Song", "addedAt": 0, "beatGrid": {"bpm": 120}}]
    });
    let dir = folder("flash", &file);
    let brightness = |time: f64| {
        let out = dir.join(format!("still-{time}.png"));
        let run = Command::new(env!("CARGO_BIN_EXE_promo"))
            .args(["still"])
            .arg(&dir)
            .args(["--out"])
            .arg(&out)
            .args(["--time", &time.to_string()])
            .output()
            .expect("run promo");
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let picture = image::open(&out).expect("a still").to_rgb8();
        let pixel = picture.get_pixel(80, 45);
        (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3
    };
    for downbeat in [0.0, 2.0] {
        let lit = brightness(downbeat);
        assert!(
            (190..=215).contains(&lit),
            "the downbeat at {downbeat} s is {lit}, not lit"
        );
    }
    for off in [1.0, 1.5, 3.0] {
        assert!(brightness(off) < 10, "the off-beat at {off} s is lit");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
