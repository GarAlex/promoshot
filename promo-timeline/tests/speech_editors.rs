//! The editors' questions about speech, slice 9 (docs/SPEECH-SYNC.md
//! §8.2): a layer edge snapped to a word, a gap or a sentence — the value
//! to write, its seconds and the core's words — and the words a lane under
//! a voice-over draws.

use promo_model::speech::{TakeTiming, TimingSource};
use promo_model::ProjectMetadata;
use promo_timeline::musical::Edge;
use promo_timeline::speech_time::{snap_speech, snap_speech_time, word_ticks, SpeechUnit};
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem};
use serde_json::json;

const SCRIPT: &str = "Euclid proved it. Primes never end, said Euclid. Then he rested.";

fn timing() -> TakeTiming {
    let mut t = promo_model::estimate::estimate_words(SCRIPT, 6.0, None, 1.0);
    t.source = TimingSource::Provider("elevenlabs".into());
    t.estimated = false;
    t.confidence = 1.0;
    t.aligner_version = None;
    t
}

fn inputs(t: TakeTiming) -> BeatGrids {
    let mut grids = BeatGrids::default();
    grids.words.by_resource.insert("intro".into(), t);
    grids
}

fn project(slide: serde_json::Value) -> ProjectMetadata {
    let file = json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 10, "videoDuration": 10, "subtitles": [],
        "compositionSettings": {},
        "layers": [
            {"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio", "isEnabled": true,
             "startTime": 2, "duration": 6, "resourceID": "intro", "keyframes": []},
            slide
        ],
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 6,
                       "speech": {"text": SCRIPT, "provider": "elevenlabs"}}]
    });
    ProjectMetadata::from_json(&file.to_string()).unwrap()
}

fn slide(start: f64, duration: f64) -> serde_json::Value {
    json!({"id": "s", "name": "S", "sortIndex": 1, "kind": "image", "isEnabled": true,
           "startTime": start, "duration": duration, "keyframes": []})
}

/// A start snaps to the nearest word's start, an end to the nearest word's
/// end; a gap to the middle of the nearest pause; a sentence to its start
/// or end — each written as the value that keeps it there.
#[test]
fn snap_to_word_gap_and_sentence() {
    let t = timing();
    let grids = inputs(t.clone());
    let near_word4 = 2.0 + t.words[3].start + 0.02;
    let meta = project(slide(near_word4, 1.0));
    let snapped = snap_speech(&meta, &grids, "s", Edge::Start, SpeechUnit::Word).unwrap();
    assert_eq!(
        snapped.value,
        json!({"word": 4, "of": "intro", "text": "Primes"})
    );
    assert!((snapped.seconds - (2.0 + t.words[3].start)).abs() < 1e-9);
    assert_eq!(snapped.label, "word 4 \u{201C}Primes\u{201D}");

    let end = snap_speech(&meta, &grids, "s", Edge::End, SpeechUnit::Word).unwrap();
    assert_eq!(end.value["edge"], "end");
    let w = end.value["word"].as_u64().unwrap() as usize - 1;
    assert!(
        (end.seconds - (2.0 + t.words[w].end - near_word4)).abs() < 1e-9,
        "a length"
    );
    assert!(end.label.starts_with("ends with word"));

    let gap = snap_speech(&meta, &grids, "s", Edge::Start, SpeechUnit::Gap).unwrap();
    assert_eq!(gap.value["snap"], "gap");
    assert!(gap.label.starts_with("the gap after word"));

    let sentence = snap_speech(&meta, &grids, "s", Edge::Start, SpeechUnit::Sentence).unwrap();
    assert_eq!(sentence.value, json!({"sentence": 2, "of": "intro"}));
    assert!((sentence.seconds - (2.0 + t.sentences[1].start)).abs() < 1e-9);

    // The value written resolves where the snap said.
    let mut file: serde_json::Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    file["layers"][1]["startTime"] = snapped.value.clone();
    let mut written = ProjectMetadata::from_json(&file.to_string()).unwrap();
    resolve_timing(&mut written, &grids);
    let s = written
        .layers
        .as_ref()
        .unwrap()
        .iter()
        .find(|l| l.id == "s")
        .unwrap();
    assert!((s.start_time - snapped.seconds).abs() < 1e-9);

    // A drag snaps to the nearest word start.
    let at = snap_speech_time(
        &meta,
        &grids,
        "s",
        2.0 + t.words[6].start + 0.03,
        SpeechUnit::Word,
    );
    assert!((at.unwrap() - (2.0 + t.words[6].start)).abs() < 1e-9);
    // No take anywhere: said.
    let mut silent = meta.clone();
    silent.layers.as_mut().unwrap().remove(0);
    assert!(snap_speech(&silent, &grids, "s", Edge::Start, SpeechUnit::Word).is_err());
}

/// The lane under a voice-over: every word it plays, on the output
/// timeline, with sentence starts marked.
#[test]
fn word_ticks_follow_the_layer() {
    let t = timing();
    let meta = project(slide(0.0, 1.0));
    let ticks = word_ticks(&meta, &inputs(t.clone()), "vo").unwrap();
    assert_eq!(ticks.len(), 11);
    assert_eq!(ticks[3].text, "Primes");
    assert!((ticks[3].start - (2.0 + t.words[3].start)).abs() < 1e-9);
    let starts: Vec<u32> = ticks
        .iter()
        .filter(|w| w.sentence_start)
        .map(|w| w.index)
        .collect();
    assert_eq!(starts, [1, 4, 9]);
    assert!(
        word_ticks(&meta, &inputs(t), "s").is_err(),
        "an image plays no take"
    );
}

/// A value that lands on a word the timing doubts says so; a sure word
/// says nothing.
#[test]
fn a_value_on_an_unsure_word_warns() {
    let mut t = timing();
    t.words[3].confidence = Some(0.3);
    let file = json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 10, "videoDuration": 10, "subtitles": [],
        "compositionSettings": {},
        "layers": [
            {"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio", "isEnabled": true,
             "startTime": 2, "duration": 6, "resourceID": "intro", "keyframes": []},
            {"id": "a", "name": "a", "sortIndex": 1, "kind": "image", "isEnabled": true,
             "startTime": {"word": 4, "of": "intro"}, "duration": 1, "keyframes": []},
            {"id": "b", "name": "b", "sortIndex": 2, "kind": "image", "isEnabled": true,
             "startTime": {"word": 5, "of": "intro"}, "duration": 1, "keyframes": []}
        ],
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 6,
                       "speech": {"text": SCRIPT, "provider": "elevenlabs"}}]
    });
    let mut meta = ProjectMetadata::from_json(&file.to_string()).unwrap();
    let report = resolve_timing(&mut meta, &inputs(t));
    let unsure: Vec<String> = report
        .problems
        .iter()
        .filter(|p| matches!(p, TimingProblem::SpeechWordUnsure { .. }))
        .map(|p| p.to_string())
        .collect();
    assert_eq!(unsure.len(), 1, "{unsure:?}");
    assert!(
        unsure[0].contains("layer \"a\" lands on word 4 \"Primes\""),
        "{unsure:?}"
    );
    assert!(!report.problems.iter().any(TimingProblem::is_break));
}
