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

/// A synced reveal on the project-wide caption style
/// (`compositionSettings.subtitleReveal`) times each caption on its own
/// words: the times live on each layer (`revealTimes`), the renderer reads
/// them as the reveal's, and a save writes none of them.
#[test]
fn a_shared_synced_reveal_times_each_caption() {
    let t = timing(SCRIPT, 6.0);
    let mut file = project(json!([
        {"id": "a", "name": "a", "sortIndex": 1, "kind": "caption", "isEnabled": true,
         "startTime": 2.5, "duration": 2, "captionText": "Euclid proved it.", "keyframes": []},
        {"id": "b", "name": "b", "sortIndex": 2, "kind": "caption", "isEnabled": true,
         "startTime": 4.0, "duration": 3, "captionText": "Primes never end", "keyframes": []},
    ]));
    file["compositionSettings"]["subtitleReveal"] =
        json!({"by": "word", "mode": "highlight", "sync": {"of": "intro"}});
    let (meta, report) = resolved_with(&file, &inputs());
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let times = |id: &str| {
        layer(&meta, id)
            .reveal_times
            .clone()
            .expect("its own times")
    };
    let a = times("a");
    assert_eq!(a.times.len(), 3);
    assert!(close(a.times[0], 2.0 + t.words[0].start - 2.5));
    let b = times("b");
    assert_eq!(b.times.len(), 3);
    assert!(close(b.times[0], 2.0 + t.words[3].start - 4.0), "{b:?}");
    // The renderer's view: the shared rule with this caption's times.
    let shared = meta.composition_settings.subtitle_reveal.clone().unwrap();
    let walked = promo_timeline::reveal::effective(shared, layer(&meta, "b"));
    assert_eq!(walked.unit_times.as_deref(), Some(b.times.as_slice()));
    let written = meta.to_json().unwrap();
    assert!(!written.contains("revealTimes"), "{written}");
    assert_eq!(meta.minimum_reader_version(), 50);

    // A caption resource's synced style shown by two layers: each its own.
    let mut file = project(json!([
        {"id": "c", "name": "c", "sortIndex": 1, "kind": "caption", "isEnabled": true,
         "startTime": 2.5, "duration": 2, "resourceID": "line", "keyframes": []},
        {"id": "d", "name": "d", "sortIndex": 2, "kind": "caption", "isEnabled": true,
         "startTime": 3.0, "duration": 2, "resourceID": "line", "keyframes": []},
    ]));
    file["resources"].as_array_mut().unwrap().push(json!({
        "id": "line", "kind": "caption", "filename": "", "displayName": "Line", "addedAt": 0,
        "captionText": "Euclid proved it.",
        "captionStyle": {"reveal": {"by": "word", "sync": {"of": "intro"}}}}));
    let (meta, _) = resolved_with(&file, &inputs());
    let c = layer(&meta, "c").reveal_times.clone().unwrap();
    let d = layer(&meta, "d").reveal_times.clone().unwrap();
    assert!(
        close(c.times[0] - d.times[0], 0.5),
        "layer-local, each its own: {c:?} {d:?}"
    );
}

/// Inside a composition, a caption shows the project's shared reveal on a
/// take the composition plays.
#[test]
fn a_composition_caption_syncs_on_its_own_take() {
    let t = timing(SCRIPT, 6.0);
    let mut file = project(json!([]));
    file["compositionSettings"]["subtitleReveal"] =
        json!({"by": "word", "mode": "highlight", "sync": {"of": "intro"}});
    file["resources"].as_array_mut().unwrap().push(json!({
        "id": "card", "kind": "composition", "filename": "", "displayName": "Card", "addedAt": 0,
        "composition": {"canvasWidth": 800, "canvasHeight": 400, "layers": [
            {"id": "cv", "name": "cv", "sortIndex": 0, "kind": "audio", "isEnabled": true,
             "startTime": 1, "duration": 6, "resourceID": "intro", "keyframes": []},
            {"id": "cc", "name": "cc", "sortIndex": 1, "kind": "caption", "isEnabled": true,
             "startTime": 1, "duration": 3, "captionText": "Euclid proved it.", "keyframes": []}]}}));
    let (meta, report) = resolved_with(&file, &inputs());
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let comp = meta
        .resources
        .as_ref()
        .unwrap()
        .iter()
        .find(|r| r.id == "card")
        .unwrap()
        .composition
        .clone()
        .unwrap();
    let cc = comp.layers.iter().find(|l| l.id == "cc").unwrap();
    let times = cc
        .reveal_times
        .clone()
        .expect("synced inside the composition");
    assert!(
        close(times.times[1], 1.0 + t.words[1].start - 1.0),
        "{times:?}"
    );
}

// ---- A language's own words (core-private docs/LOCALIZATION.md §3.4–3.5) ----

const GERMAN: &str = "Euklid bewies es. Primzahlen enden nie, sagte Euklid. Dann ruhte er.";

/// `file` with German on its take: its own file and its own script.
fn in_german(mut file: Value) -> Value {
    file["languages"] = json!({"source": "en", "targets": ["de"]});
    file["resources"][0]["localized"] = json!({"de": {
        "filename": "intro.de.mp3", "duration": 9,
        "speech": {"text": GERMAN, "voiceID": "anna"}}});
    file["resources"].as_array_mut().unwrap().push(json!({
        "id": "pic", "kind": "image", "filename": "pic.png", "displayName": "Pic", "addedAt": 0}));
    file
}

fn german_inputs() -> BeatGrids {
    inputs_with(timing(GERMAN, 9.0))
}

fn flat(file: &Value) -> (ProjectMetadata, Vec<String>) {
    let mut meta = ProjectMetadata::from_json(&file.to_string()).expect("decodes");
    let notes = promo_model::localized::flatten_noting(&mut meta, "de").expect("flattens");
    (meta, notes)
}

/// Captions regenerate from the language's script, on its take's times, and
/// a copy of the app's resolved form makes the same ones: its English
/// generated layers are not the person's.
#[test]
fn generated_captions_say_the_languages_words() {
    let file = in_german(narrated(json!({"by": "sentence", "maxSeconds": 30})));
    let (mut meta, _) = flat(&file);
    let report = resolve_timing(&mut meta, &german_inputs());
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let shown: Vec<String> = generated(&meta)
        .iter()
        .map(|c| c.caption_text.clone().unwrap())
        .collect();
    assert_eq!(
        shown,
        [
            "Euklid bewies es.",
            "Primzahlen enden nie, sagte Euklid.",
            "Dann ruhte er."
        ]
    );
    let german = timing(GERMAN, 9.0);
    let first = generated(&meta)[1];
    assert!(
        close(first.start_time, 2.0 + german.words[3].start - 0.1),
        "{}",
        first.start_time
    );

    // Their words are the script's: nothing to translate on the layer.
    let report =
        promo_timeline::report::file_report_with(&file.to_string(), None, &german_inputs())
            .unwrap();
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.message.contains("keeps its text on the layer")),
        "{:#?}",
        report.findings
    );

    // The app hands the core its resolved source: English pieces in the layers.
    let (app_form, _) = resolved_with(&file, &inputs());
    let app_json = serde_json::to_string(&app_form).unwrap();
    let mut meta = ProjectMetadata::from_json(&app_json).unwrap();
    assert!(generated(&meta)
        .iter()
        .any(|c| c.caption_text.as_deref() == Some("Euclid proved it.")));
    promo_model::localized::flatten(&mut meta, "de").unwrap();
    resolve_timing(&mut meta, &german_inputs());
    let shown: Vec<String> = generated(&meta)
        .iter()
        .map(|c| c.caption_text.clone().unwrap())
        .collect();
    assert_eq!(shown[0], "Euklid bewies es.", "{shown:?}");
    assert_eq!(shown.len(), 3, "{shown:?}");
}

/// A script with no take of its own: the English file plays, so English is
/// what captions and anchors count, whatever record the file has.
#[test]
fn a_script_without_its_file_captions_what_plays() {
    let mut file = in_german(narrated(json!({"by": "sentence", "maxSeconds": 30})));
    file["resources"][0]["localized"] = json!({"de": {"speech": {"text": GERMAN}}});
    let (mut meta, notes) = flat(&file);
    assert!(notes.is_empty(), "{notes:?}");
    resolve_timing(&mut meta, &inputs());
    let first = generated(&meta)[0].caption_text.clone().unwrap();
    assert_eq!(first, "Euclid proved it.");
}

/// A pinned reveal and a `during` land in the same sentence, by place; the
/// name said in that sentence is found exactly and not listed.
#[test]
fn pinned_words_follow_into_the_translation() {
    let file = in_german(project(json!([
        caption("pinned", "Euklid bewies es.", json!({"by": "word", "sync": {"of": "intro", "words": [1, 3]}}), json!({})),
        {"id": "seen", "name": "seen", "sortIndex": 3, "kind": "image", "isEnabled": true,
         "startTime": 0, "resourceID": "pic", "during": {"words": [4, 5], "of": "intro"}, "keyframes": []},
        {"id": "said", "name": "said", "sortIndex": 4, "kind": "image", "isEnabled": true,
         "startTime": {"word": "Euclid", "nth": 2, "of": "intro"}, "duration": 1, "resourceID": "pic",
         "keyframes": []},
        {"id": "last", "name": "last", "sortIndex": 5, "kind": "image", "isEnabled": true,
         "startTime": {"word": 10, "of": "vo", "text": "Then"}, "duration": 1, "resourceID": "pic",
         "keyframes": []},
    ])));
    let (mut meta, notes) = flat(&file);
    resolve_timing(&mut meta, &german_inputs());
    let t = timing(GERMAN, 9.0);
    // "Euclid" (2nd) is "Euklid", another word: by its place, the sentence's end.
    assert!(
        close(layer(&meta, "said").start_time, 2.0 + t.words[7].start),
        "{notes:?}"
    );
    assert!(
        close(layer(&meta, "last").start_time, 2.0 + t.words[8].start),
        "{notes:?}"
    );
    let seen = layer(&meta, "seen");
    assert!(
        close(seen.start_time, 2.0 + t.words[3].start),
        "{}",
        seen.start_time
    );
    assert!(close(
        seen.start_time + seen.duration.unwrap(),
        2.0 + t.words[4].end
    ));
    assert_eq!(reveal(&meta, "pinned").sync.unwrap().words, Some([1, 3]));
    assert!(
        notes
            .iter()
            .any(|n| n.contains("\u{201C}Euclid\u{201D} lands on word 8")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("\u{201C}Then\u{201D}")),
        "{notes:?}"
    );
}

/// A translation with fewer sentences still lands in the take, a sentence
/// value included, rather than breaking past the end.
#[test]
fn another_sentence_count_lands_by_place_and_says_so() {
    let mut file = in_german(project(json!([
        {"id": "third", "name": "third", "sortIndex": 3, "kind": "image", "isEnabled": true,
         "startTime": {"sentence": 3, "of": "intro"}, "duration": 1, "resourceID": "pic",
         "keyframes": []},
    ])));
    let short = "Euklid bewies es, Primzahlen enden nie und er ruhte.";
    file["resources"][0]["localized"]["de"]["speech"]["text"] = json!(short);
    let (mut meta, notes) = flat(&file);
    let report = resolve_timing(&mut meta, &inputs_with(timing(short, 9.0)));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("sentence 3 lands on sentence 1")),
        "{notes:?}"
    );
}

/// A generator's named words follow; a nested composition counts on its own
/// clock; validate lists what is approximate, breaks nothing and says the
/// sentence count.
#[test]
fn generators_and_validate_follow_the_language() {
    let mut file = in_german(project(json!([
        {"id": "s", "name": "s", "sortIndex": 3, "kind": "image", "isEnabled": true,
         "startTime": 0, "duration": 12, "resourceID": "pic", "keyframes": [],
         "pulse": {"on": {"words": ["Euclid", 5], "of": "intro"}, "amount": 0.05, "decay": 0.25}},
    ])));
    let (mut meta, notes) = flat(&file);
    let report = resolve_timing(&mut meta, &german_inputs());
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let peaks = layer(&meta, "s")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-peak"))
        .count();
    assert_eq!(peaks, 3, "{notes:?}");

    file["resources"][0]["localized"]["de"]["speech"]["text"] = json!("Nur ein Satz hier.");
    let text = file.to_string();
    let out = promo_timeline::report::file_report_with(
        &text,
        None,
        &inputs_with(timing("Nur ein Satz hier.", 9.0)),
    )
    .unwrap();
    let lines: Vec<&str> = out.findings.iter().map(|f| f.message.as_str()).collect();
    assert!(
        lines.iter().any(|l| l.contains("1 sentence(s) here and 3")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("de: ") && l.contains("by its place")),
        "{lines:#?}"
    );
}

const JA: &str = "新しいダッシュボードをご紹介します。売上から注文まで、必要なものはすべてワンタップで見つかります。東京のチームにも同じ画面が見えています。";

fn japanese(captions: Value, extra: Vec<Value>) -> (ProjectMetadata, TimingReport) {
    let mut file = narrated(captions);
    file["resources"][0]["speech"]["text"] = json!(JA);
    file["resources"][0]["duration"] = json!(12.0);
    file["layers"][0]["duration"] = json!(12.0);
    file["layers"].as_array_mut().unwrap().extend(extra);
    resolved_with(&file, &inputs_with(timing(JA, 12.0)))
}

/// A script with no spaces: each piece says its own words, none runs on
/// through the rest of the narration, and every word is said once.
#[test]
fn captions_without_spaces_say_their_own_words() {
    let (meta, _) = japanese(json!({"by": "sentence"}), Vec::new());
    let pieces: Vec<String> = generated(&meta)
        .iter()
        .map(|l| l.caption_text.clone().unwrap())
        .collect();
    assert!(pieces.len() > 3, "{pieces:?}");
    assert_eq!(pieces.concat(), JA, "nothing lost, nothing said twice");
    for piece in &pieces {
        assert!(piece.chars().count() < 40, "ran on: {piece}");
        // Not "ます", not "を": a piece starts on a word of its own.
        let first = promo_model::speech::tokenize(piece).words[0].text.clone();
        assert!(
            !first
                .chars()
                .all(|c| ('\u{3040}'..='\u{309F}').contains(&c)),
            "starts on a particle: {piece}"
        );
    }
    assert_eq!(pieces[0], "新しいダッシュボードをご紹介します。");
    // The comma is where the second sentence divides, when it has to.
    assert!(
        pieces.iter().any(|p| p == "売上から注文まで、"),
        "{pieces:?}"
    );
}

/// A name in a script with no spaces is found however the dictionary cuts it.
#[test]
fn a_name_without_spaces_is_found_as_written() {
    let at = |name: &str| {
        let mut anchored = caption("anchor", "x", json!({"by": "word"}), json!({}));
        anchored["startTime"] = json!({"word": name, "of": "intro"});
        let (meta, report) = japanese(json!({"by": "sentence"}), vec![anchored]);
        (layer(&meta, "anchor").start_time, report)
    };
    let timed = timing(JA, 12.0);
    // The layer playing the take starts at 2.
    let word = |text: &str| 2.0 + timed.words.iter().find(|w| w.text == text).unwrap().start;
    let (start, report) = at("東京");
    assert!(close(start, word("東京")), "{start} {report:?}");
    // Two of the take's words, as one name.
    let (start, _) = at("同じ画面");
    assert!(close(start, word("同じ")), "{start}");
}

/// A reveal on a generated piece arrives word by word on the take's own words.
#[test]
fn a_synced_reveal_without_spaces_walks_its_words() {
    let (meta, report) = japanese(
        json!({"by": "sentence",
               "style": {"reveal": {"by": "word", "sync": {"mode": "words", "of": "intro"}}}}),
        Vec::new(),
    );
    let first = generated(&meta)[0];
    let reveal = first
        .caption_style
        .as_ref()
        .and_then(|s| s.reveal.clone())
        .expect("a reveal");
    let times = reveal.unit_times.expect("synced");
    let units = promo_model::speech::unit_texts(first.caption_text.as_deref().unwrap());
    assert_eq!(times.len(), units.len(), "{units:?} {report:?}");
    assert!(times.windows(2).all(|t| t[0] < t[1]), "{times:?}");
}
