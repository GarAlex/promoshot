//! Speech values through every loader (docs/SPEECH-SYNC.md §6, slice 6):
//! re-voicing a line re-times the cuts that name it and nothing else
//! (AC2), the CLI's open, `explain`, the editor document and `validate`
//! read the same seconds, and a render waits for a take with no timing.

use std::path::{Path, PathBuf};
use std::process::Command;

use promo_model::speech::{SpokenWord, TimingSource};
use promo_model::ProjectMetadata;
use serde_json::{json, Value};

/// A deterministic synthesizer that says when: each word lasts by its
/// length, so a line's text decides every time after the first change.
struct FakeSynth;

fn fake_words(text: &str) -> Vec<(f64, f64)> {
    let mut t = 0.2;
    promo_model::speech::tokenize(text)
        .words
        .iter()
        .map(|w| {
            let start = t;
            t += 0.12 + 0.06 * w.text.chars().count() as f64;
            let end = t;
            t += 0.08;
            (start, end)
        })
        .collect()
}

impl promo_speech::Synth for FakeSynth {
    fn synthesize(&self, _: &str, _: &str, text: &str) -> Result<Vec<u8>, String> {
        Ok(format!("FAKE-AUDIO {text}").into_bytes())
    }

    fn synthesize_timed(
        &self,
        provider: &str,
        voice: &str,
        text: &str,
    ) -> Result<promo_speech::Take, String> {
        let words = promo_model::speech::tokenize(text).words;
        Ok(promo_speech::Take {
            audio: self.synthesize(provider, voice, text)?,
            spoken: Some(promo_speech::Spoken {
                words: words
                    .iter()
                    .zip(fake_words(text))
                    .map(|(w, (start, end))| SpokenWord {
                        text: w.text.clone(),
                        start,
                        end: Some(end),
                        chars: None,
                    })
                    .collect(),
                source: TimingSource::Provider("fake".into()),
                confidence: 1.0,
                against: text.to_string(),
            }),
            note: None,
        })
    }
}

fn length_of(text: &str) -> f64 {
    fake_words(text).last().map_or(0.5, |(_, end)| end + 0.3)
}

const INTRO: &str = "Euclid proved it. Primes never end.";
const OUTRO: &str = "Then he rested, and so can you.";

fn image(id: &str, sort: i64, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": sort, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 1,
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn take(id: &str, text: &str) -> Value {
    json!({"id": id, "kind": "audio", "filename": format!("{id}.mp3"), "displayName": id,
           "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
           "speech": {"text": text, "provider": "openai", "voiceID": "alloy"}})
}

fn document(intro: &str) -> Value {
    json!({
        "id": "P", "name": "Speech", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 12, "videoDuration": 12, "subtitles": [],
        "minReaderVersion": 50,
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90},
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 12,
             "keyframes": [{"id": "g", "time": 0, "colorHex": "202030"}]},
            {"id": "vo1", "name": "VO 1", "sortIndex": 1, "kind": "audio", "isEnabled": true,
             "startTime": 0.5, "duration": 5, "resourceID": "intro", "keyframes": []},
            {"id": "vo2", "name": "VO 2", "sortIndex": 2, "kind": "audio", "isEnabled": true,
             "startTime": 6, "duration": 5, "resourceID": "outro", "keyframes": []},
            image("a", 3, json!({"startTime": {"word": 5, "of": "intro"}})),
            image("b", 4, json!({"startTime": {"word": 2, "of": "outro"}})),
            image("c", 5, json!({"startTime": {"sentence": 2, "of": "vo1"},
                                  "duration": {"sentence": 2, "of": "vo1"}})),
            image("d", 6, json!({"startTime": 3})),
        ],
        "resources": [take("intro", intro), take("outro", OUTRO)]
    })
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-speech-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    dir
}

/// Voices what needs voicing (the receipts hold the rest) and writes the
/// file; answers the settle report.
fn settle(dir: &Path, doc: &mut Value) -> Vec<String> {
    let measure = |path: &Path| -> Result<f64, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(length_of(text.trim_start_matches("FAKE-AUDIO ")))
    };
    let mut nop = |_: &Value| Ok(());
    let said = promo_speech::settle(doc, &dir.join("Resources"), &FakeSynth, &measure, &mut nop)
        .expect("settles");
    std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();
    said
}

/// Within the record's rounding (1 µs).
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 2e-6
}

fn start_of(meta: &ProjectMetadata, id: &str) -> (f64, Option<f64>) {
    let l = promo_model::nesting::all_layers(meta)
        .into_iter()
        .find(|l| l.id == id)
        .expect("layer");
    (l.start_time, l.duration)
}

/// The fake take's word `n` (1-based) of `text`, on the layer at `at`.
fn word_at(text: &str, n: usize, at: f64) -> f64 {
    at + fake_words(text)[n - 1].0
}

/// AC2: one narration's text changes and the settle re-voices it. Only the
/// layers that name that take move, each to its word's new time; the
/// project is otherwise the same, number for number.
#[test]
fn editing_a_line_retimes_dependent_cuts() {
    let dir = folder("ac2");
    let mut doc = document(INTRO);
    let said = settle(&dir, &mut doc);
    assert!(
        said.iter()
            .filter(|l| l.starts_with("intro:") || l.starts_with("outro:"))
            .all(|l| l.contains("timed by provider:fake")),
        "{said:?}"
    );
    let before = promo_cli::project::Project::open(&dir).expect("opens");
    assert!(
        before.timing.missing_words.is_empty(),
        "{:?}",
        before.timing.problems
    );
    assert!(
        !before.timing.problems.iter().any(|p| p.is_break()),
        "{:?}",
        before.timing.problems
    );
    assert!(near(start_of(&before.meta, "a").0, word_at(INTRO, 5, 0.5)));
    assert!(near(start_of(&before.meta, "b").0, word_at(OUTRO, 2, 6.0)));

    // The line is rewritten: two words up front, one changed.
    let edited = "So, Euclid proved it. Primes never ever end.";
    doc["resources"][0]["speech"]["text"] = json!(edited);
    let said = settle(&dir, &mut doc);
    assert!(
        said.iter()
            .any(|l| l.starts_with("intro:") && l.contains("timed by")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|l| l.starts_with("outro:") && l.contains("reused")),
        "{said:?}"
    );
    let after = promo_cli::project::Project::open(&dir).expect("opens");
    assert!(after.timing.missing_words.is_empty());

    // Word 5 of the new line is "Primes"; sentence 2 starts later and runs longer.
    assert!(near(start_of(&after.meta, "a").0, word_at(edited, 5, 0.5)));
    assert_ne!(start_of(&after.meta, "a"), start_of(&before.meta, "a"));
    let words = fake_words(edited);
    let (c, c_length) = start_of(&after.meta, "c");
    assert!(near(c, 0.5 + words[4].0));
    assert!(near(c_length.unwrap(), words[7].1 - words[4].0));
    // Nothing else moved: the layers that do not name the take, number for
    // number, and every other resource.
    let rest = |meta: &ProjectMetadata| -> Value {
        let mut v: Value = serde_json::from_str(&meta.to_app_json().unwrap()).unwrap();
        let layers = v["layers"].as_array_mut().unwrap();
        layers.retain(|l| l["id"] != "a" && l["id"] != "c");
        let resources = v["resources"].as_array_mut().unwrap();
        resources.retain(|r| r["id"] != "intro");
        v
    };
    assert_eq!(rest(&after.meta), rest(&before.meta));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The CLI's open, `explain`, the editor document and `validate` give the
/// same numbers for a speech-timed file.
#[test]
fn every_loader_gives_the_same_numbers() {
    let dir = folder("loaders");
    let mut doc = document(INTRO);
    settle(&dir, &mut doc);
    let cli = promo_cli::project::Project::open(&dir).expect("opens");
    let expected = start_of(&cli.meta, "a").0;
    assert!(near(expected, word_at(INTRO, 5, 0.5)));
    let (c, c_length) = start_of(&cli.meta, "c");

    // explain, through its JSON text: to a picosecond.
    let out = promo_author::explain(&json!({"project": dir, "time": 0.1, "layer": "a"}), None)
        .expect("explains");
    let out: Value = serde_json::from_str(&out).unwrap();
    let life = &out["layers"][0]["life"];
    assert!(
        (life["start"].as_f64().unwrap() - expected).abs() < 1e-12,
        "{life}"
    );
    let times = &out["layers"][0]["times"];
    assert_eq!(times["startTime"]["clock"], "speech", "{times}");
    assert_eq!(
        times["startTime"]["label"], "word 5 \u{201C}never\u{201D}",
        "{times}"
    );

    // The editor document, on the words the host hands it.
    let text = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
    let mut document = promo_editor::document::Document::open(&text).unwrap();
    let meta = ProjectMetadata::from_json(&text).unwrap();
    document.set_beat_grids(promo_timeline::BeatGrids::default().with_words(&dir, &meta));
    assert_eq!(
        start_of(document.meta(), "a").0,
        expected,
        "the document disagrees"
    );
    assert_eq!(
        start_of(document.meta(), "c"),
        (c, c_length),
        "the document disagrees"
    );

    // validate lists it, on the same grids the open measured.
    let report =
        promo_timeline::report::file_report_with(&text, None, &cli.grids).expect("validates");
    let row = report
        .speech_times
        .iter()
        .find(|r| r.at == "layers[a].startTime")
        .expect("listed");
    assert_eq!(row.seconds, expected);
    assert_eq!(row.source, "provider:fake");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A take a speech value counts on with no timing: laid out on an
/// estimate, validate says so, and a render waits.
#[test]
fn a_render_waits_for_missing_words() {
    let dir = folder("waits");
    let doc = document(INTRO);
    std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();
    for id in ["intro", "outro"] {
        std::fs::write(dir.join(format!("Resources/{id}.mp3")), b"not audio").unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("still")
        .arg(&dir)
        .arg("--no-analyse")
        .output()
        .expect("run promo");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("not rendering: speech values count on"),
        "{said}"
    );
    assert!(said.contains("\"intro\""), "{said}");

    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .arg("validate")
        .arg(&dir)
        .arg("--no-analyse")
        .output()
        .expect("run promo");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("\"intro\" has no word timing yet"), "{said}");
    assert!(said.contains("Speech times"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
