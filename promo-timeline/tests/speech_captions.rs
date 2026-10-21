//! Captions on a take's words, slice 7 (docs/SPEECH-SYNC.md §7.1–7.2): a
//! synced reveal's units arrive when their words are said, and `captions`
//! makes one caption per sentence or phrase, owned like the flash.

use promo_model::speech::{TakeTiming, TimingSource};
use promo_model::{ProjectLayer, ProjectMetadata, TextReveal};
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

const SCRIPT: &str = "Euclid proved it. Primes never end, said Euclid. Then he rested.";

fn timing(script: &str, seconds: f64) -> TakeTiming {
    let mut t = promo_model::estimate::estimate_words(script, seconds, None, 1.0);
    t.source = TimingSource::Provider("elevenlabs".into());
    t.estimated = false;
    t.confidence = 1.0;
    t.aligner_version = None;
    t
}

fn inputs_with(t: TakeTiming) -> BeatGrids {
    let mut grids = BeatGrids::default();
    grids.words.by_resource.insert("intro".into(), t);
    grids
}

fn inputs() -> BeatGrids {
    inputs_with(timing(SCRIPT, 6.0))
}

fn caption(id: &str, text: &str, reveal: Value, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": 1, "kind": "caption",
                           "isEnabled": true, "startTime": 2.5, "duration": 4,
                           "captionText": text, "captionStyle": {"reveal": reveal},
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn project(layers: Value) -> Value {
    let mut all = vec![
        json!({"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio",
                              "isEnabled": true, "startTime": 2, "duration": 6,
                              "resourceID": "intro", "keyframes": []}),
    ];
    all.extend(layers.as_array().unwrap().iter().cloned());
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 10, "videoDuration": 10, "subtitles": [],
        "compositionSettings": {}, "layers": all,
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 6,
                       "speech": {"text": SCRIPT, "provider": "elevenlabs"}}]
    })
}

fn resolved_with(value: &Value, grids: &BeatGrids) -> (ProjectMetadata, TimingReport) {
    let mut meta = ProjectMetadata::from_json(&value.to_string()).expect("decodes");
    let report = resolve_timing(&mut meta, grids);
    (meta, report)
}

fn layer<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a ProjectLayer {
    meta.layers
        .as_ref()
        .unwrap()
        .iter()
        .find(|l| l.id == id)
        .unwrap_or_else(|| panic!("no layer {id}"))
}

fn reveal(meta: &ProjectMetadata, id: &str) -> TextReveal {
    layer(meta, id)
        .caption_style
        .as_ref()
        .and_then(|s| s.reveal.clone())
        .expect("a reveal")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 2e-6
}

/// Each caption word arrives at its word's start in the take (layer-local),
/// a punctuation unit with the next word, a caption that is one sentence
/// of the narration finds that sentence, and a pinned span is used as
/// given.
#[test]
fn synced_reveal_maps_caption_units_to_take_words() {
    let t = timing(SCRIPT, 6.0);
    let at = |w: usize| 2.0 + t.words[w - 1].start - 2.5;
    let file = project(json!([
        caption(
            "a",
            "Primes never end, said Euclid.",
            json!({"by": "word", "mode": "highlight", "sync": {"of": "intro"}}),
            json!({})
        ),
        caption(
            "b",
            "Primes \u{2014} never end",
            json!({"by": "word", "sync": {"of": "vo"}}),
            json!({})
        ),
        caption(
            "c",
            "Euclid",
            json!({"by": "word", "sync": {"of": "intro", "words": [8, 8]}}),
            json!({})
        ),
        caption(
            "d",
            "Euclid",
            json!({"by": "word", "sync": {"of": "intro"}}),
            json!({})
        ),
    ]));
    let (meta, report) = resolved_with(&file, &inputs());
    assert!(
        !report.problems.iter().any(|p| matches!(
            p,
            TimingProblem::RevealSync { .. } | TimingProblem::RevealUnmatched { .. }
        )),
        "{:?}",
        report.problems
    );
    let a = reveal(&meta, "a");
    let times = a.unit_times.clone().expect("unit times");
    let expected: Vec<f64> = (4..=8).map(at).collect();
    assert_eq!(times.len(), 5);
    assert!(
        times.iter().zip(&expected).all(|(x, y)| close(*x, *y)),
        "{times:?} vs {expected:?}"
    );
    let ends = a.unit_ends.expect("unit ends");
    assert!(close(ends[4], 2.0 + t.words[7].end - 2.5));
    let b = reveal(&meta, "b").unit_times.unwrap();
    assert!(close(b[1], at(5)), "the dash arrives with \"never\": {b:?}");
    assert!(close(b[0], at(4)) && close(b[2], at(5)) && close(b[3], at(6)));
    assert!(
        close(reveal(&meta, "c").unit_times.unwrap()[0], at(8)),
        "the pinned span"
    );
    assert!(
        close(reveal(&meta, "d").unit_times.unwrap()[0], at(1)),
        "unpinned: the first"
    );

    // Words the take does not say are placed between their neighbours, and
    // said; a caption none of whose words are said walks at its own pace.
    let file = project(json!([
        caption(
            "e",
            "Primes truly never end",
            json!({"by": "word", "sync": {"of": "intro"}}),
            json!({})
        ),
        caption(
            "f",
            "Nothing here matches",
            json!({"by": "word", "sync": {"of": "intro"}}),
            json!({})
        ),
    ]));
    let (meta, report) = resolved_with(&file, &inputs());
    let e = reveal(&meta, "e").unit_times.unwrap();
    assert!(e[1] > e[0] && e[1] < e[2], "{e:?}");
    assert!(report.problems.iter().any(|p| matches!(p,
        TimingProblem::RevealUnmatched { layer, matched: 3, units: 4, .. } if layer == "e")));
    assert!(reveal(&meta, "f").unit_times.is_none());
    assert!(report.problems.iter().any(|p| matches!(p,
        TimingProblem::RevealSync { layer, .. } if layer == "f")));
}

/// The active word is lit while it sounds: dark between words and after
/// the last one, and nothing is lit before the first.
#[test]
fn highlight_goes_out_between_words() {
    let rule: TextReveal = serde_json::from_value(json!({
        "by": "word", "mode": "highlight",
        "unitTimes": [0.5, 1.0, 2.0], "unitEnds": [0.8, 1.6, 2.4]
    }))
    .unwrap();
    let mut caption: ProjectLayer = serde_json::from_value(json!({
        "id": "c", "name": "c", "sortIndex": 0, "kind": "caption", "isEnabled": true,
        "startTime": 3, "duration": 5, "keyframes": []}))
    .unwrap();
    caption.caption_style = None;
    let at = |local: f64| promo_timeline::reveal::progress(&rule, &caption, 3.0 + local, 3);
    assert_eq!(at(0.2).active, None, "before the first word");
    assert_eq!(at(0.2).shown, 1, "never empty");
    assert_eq!(at(0.6).active, Some(0));
    assert_eq!(at(0.9).active, None, "between words");
    assert_eq!(at(1.2).active, Some(1));
    assert_eq!(at(1.2).shown, 2);
    assert_eq!(at(2.1).active, Some(2));
    assert_eq!(at(2.1).shown, 3);
    assert_eq!(at(3.0).active, None, "after the last");
    assert_eq!(at(3.0).fraction, 1.0);
}

/// The unsynced walk is the walk it always was: the same progress for every
/// reveal the repository's projects carry, at every instant sampled, with
/// unit times absent — the slice's only engine change sits behind them.
#[test]
fn reveal_without_unit_times_is_unchanged() {
    let layer: ProjectLayer = serde_json::from_value(json!({
        "id": "c", "name": "c", "sortIndex": 0, "kind": "caption", "isEnabled": true,
        "startTime": 1, "duration": 4, "keyframes": []}))
    .unwrap();
    for rule in [
        json!({"by": "word"}),
        json!({"by": "character", "secondsPer": 0.05}),
        json!({"by": "line", "mode": "rise", "seconds": 1.5, "easing": "easeInOut"}),
        json!({"by": "word", "mode": "highlight"}),
    ] {
        let plain: TextReveal = serde_json::from_value(rule.clone()).unwrap();
        let mut synced_off = plain.clone();
        synced_off.sync = Some(serde_json::from_value(json!({"of": "intro"})).unwrap());
        for k in 0..60 {
            let t = k as f64 * 0.1;
            let a = promo_timeline::reveal::progress(&plain, &layer, t, 7);
            let b = promo_timeline::reveal::progress(&synced_off, &layer, t, 7);
            assert_eq!(
                a, b,
                "{rule} at {t}: sync with no unit times changes nothing"
            );
            // The old formula, stated: units arrive across the walk.
            let total = plain.total_seconds(7, Some(4.0));
            let raw = if total > 0.0 {
                ((t - 1.0).max(0.0) / total).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let fraction = plain
                .easing
                .unwrap_or(promo_model::Easing::Linear)
                .apply(raw);
            assert_eq!(a.fraction, fraction, "{rule} at {t}");
            assert_eq!(a.shown, ((fraction * 7.0).ceil() as usize).clamp(1, 7));
        }
    }
}

fn narrated(captions: Value) -> Value {
    let mut file = project(json!([]));
    file["layers"][0]["captions"] = captions;
    file
}

fn generated(meta: &ProjectMetadata) -> Vec<&ProjectLayer> {
    meta.layers
        .as_ref()
        .unwrap()
        .iter()
        .filter(|l| l.id.starts_with("speech-captions-"))
        .collect()
}

/// One caption per sentence; a sentence over `maxWords` splits at its
/// comma, then at the gap nearest its middle; each starts at its first word
/// less the lead and ends at its last plus the hold, never past the next.
#[test]
fn generated_captions_split_long_sentences() {
    let t = timing(SCRIPT, 6.0);
    let (meta, report) = resolved_with(&narrated(json!({})), &inputs());
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let texts: Vec<String> = generated(&meta)
        .iter()
        .map(|l| l.caption_text.clone().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "Euclid proved it.",
            "Primes never end, said Euclid.",
            "Then he rested."
        ]
    );

    let (meta, _) = resolved_with(
        &narrated(json!({"maxWords": 3, "lead": 0.2, "hold": 0.3})),
        &inputs(),
    );
    let pieces = generated(&meta);
    let texts: Vec<String> = pieces
        .iter()
        .map(|l| l.caption_text.clone().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "Euclid proved it.",
            "Primes never end,",
            "said Euclid.",
            "Then he rested."
        ]
    );
    let second = pieces[1];
    assert!(close(second.start_time, 2.0 + t.words[3].start - 0.2));
    let end = (2.0 + t.words[5].end + 0.3).min(2.0 + t.words[6].start - 0.2);
    assert!(close(second.start_time + second.duration.unwrap(), end));
    // Each carries its reveal, synced to its words, with their times.
    let r = second
        .caption_style
        .as_ref()
        .unwrap()
        .reveal
        .as_ref()
        .unwrap();
    assert_eq!(r.sync.as_ref().unwrap().words, Some([4, 6]));
    assert_eq!(r.unit_times.as_ref().unwrap().len(), 3);
    assert!(second
        .generated_by
        .as_deref()
        .unwrap()
        .starts_with("captions:vo:"));
    // The file holds none of them, and no derived numbers.
    let written = meta.to_json().unwrap();
    assert!(!written.contains("speech-captions-"), "{written}");
    assert!(
        !written.contains("unitTimes") && !written.contains("\"made\""),
        "{written}"
    );
    assert_eq!(meta.minimum_reader_version(), 50);
    // Resolving the resolved project changes nothing.
    let mut again = meta.clone();
    resolve_timing(&mut again, &inputs());
    assert_eq!(again, meta);

    // By phrase: every comma splits.
    let (meta, _) = resolved_with(&narrated(json!({"by": "phrase"})), &inputs());
    let texts: Vec<String> = generated(&meta)
        .iter()
        .map(|l| l.caption_text.clone().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "Euclid proved it.",
            "Primes never end,",
            "said Euclid.",
            "Then he rested."
        ]
    );
}

/// A generated caption someone edits is theirs: the save keeps it without
/// its tag, and the generator leaves its piece alone.
#[test]
fn edited_generated_caption_becomes_the_persons() {
    let (mut meta, _) = resolved_with(&narrated(json!({})), &inputs());
    let edited = meta
        .layers
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|l| l.id == "speech-captions-vo-1")
        .unwrap();
    edited.caption_text = Some("Primes never end — Euclid.".into());
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    let kept: Vec<&Value> = written["layers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["id"].as_str().unwrap().starts_with("speech-captions-"))
        .collect();
    assert_eq!(kept.len(), 1, "only the edited one is saved");
    assert_eq!(kept[0]["id"], "speech-captions-vo-1");
    assert!(kept[0].get("generatedBy").is_none());
    let (again, _) = resolved_with(&written, &inputs());
    let ids: Vec<&str> = generated(&again).iter().map(|l| l.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "speech-captions-vo-1",
            "speech-captions-vo-0",
            "speech-captions-vo-2"
        ]
    );
    assert_eq!(
        layer(&again, "speech-captions-vo-1")
            .caption_text
            .as_deref(),
        Some("Primes never end — Euclid.")
    );
    assert!(layer(&again, "speech-captions-vo-1").generated_by.is_none());
}

/// A generated caption someone deletes stays deleted: the save records its
/// piece in `skip`, and it is not made again.
#[test]
fn deleted_generated_caption_stays_deleted() {
    let (mut meta, _) = resolved_with(&narrated(json!({})), &inputs());
    meta.layers
        .as_mut()
        .unwrap()
        .retain(|l| l.id != "speech-captions-vo-0");
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_eq!(written["layers"][0]["captions"]["skip"], json!([0]));
    let (again, _) = resolved_with(&written, &inputs());
    let ids: Vec<&str> = generated(&again).iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["speech-captions-vo-1", "speech-captions-vo-2"]);
}

/// A new take re-times every generated caption: they are made from the
/// words on each load, so nothing in the file holds the old times.
#[test]
fn generated_captions_retime_with_a_new_take() {
    let file = narrated(json!({}));
    let (before, _) = resolved_with(&file, &inputs());
    let quicker = timing(SCRIPT, 4.5);
    let (after, _) = resolved_with(&file, &inputs_with(quicker.clone()));
    let a = generated(&before);
    let b = generated(&after);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.caption_text, y.caption_text);
    }
    assert!(close(b[2].start_time, 2.0 + quicker.words[8].start - 0.1));
    assert!(b[2].start_time < a[2].start_time);
    assert_eq!(
        before.to_json().unwrap(),
        after.to_json().unwrap(),
        "the file is the same"
    );
}
