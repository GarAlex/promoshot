//! Generators and sequences on a take's words, slice 8 (docs/SPEECH-SYNC.md
//! §7.3): `pulse`, `shake` and `flash` fire on words or sentences, a
//! sequence entry runs `until` a word, and `per` lays one slide per
//! sentence.

use promo_model::speech::{TakeTiming, TimingSource};
use promo_model::ProjectMetadata;
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

const SCRIPT: &str = "Euclid proved it. Primes never end, said Euclid. Then he rested.";

fn timing() -> TakeTiming {
    let mut t = promo_model::estimate::estimate_words(SCRIPT, 6.0, None, 1.0);
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

fn image(id: &str, sort: i64, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": sort, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 9,
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn project(settings: Value, layers: Value) -> Value {
    let mut all = vec![
        json!({"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio",
                              "isEnabled": true, "startTime": 2, "duration": 6,
                              "resourceID": "intro", "keyframes": []}),
    ];
    all.extend(layers.as_array().unwrap().iter().cloned());
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 10, "videoDuration": 10, "subtitles": [],
        "compositionSettings": settings, "layers": all,
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": 6,
                       "speech": {"text": SCRIPT, "provider": "elevenlabs"}},
                      {"id": "song", "kind": "audio", "filename": "song.m4a",
                       "displayName": "Song", "addedAt": 0, "duration": 60,
                       "beatGrid": {"bpm": 120}}]
    })
}

fn resolved_with(value: &Value, grids: &BeatGrids) -> (ProjectMetadata, TimingReport) {
    let mut meta = ProjectMetadata::from_json(&value.to_string()).expect("decodes");
    let report = resolve_timing(&mut meta, grids);
    (meta, report)
}

fn layer<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a promo_model::ProjectLayer {
    meta.layers
        .as_ref()
        .unwrap()
        .iter()
        .find(|l| l.id == id)
        .unwrap_or_else(|| panic!("no layer {id}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// A pulse on named words peaks at every time each is said, on keyframes
/// tagged as speech's, and none of it reaches the file.
#[test]
fn pulse_on_named_words() {
    let t = timing();
    let file = project(
        json!({}),
        json!([image(
            "s",
            1,
            json!({"pulse": {"on": {"words": ["Euclid", 5], "of": "intro"},
                                              "amount": 0.05, "decay": 0.25}})
        )]),
    );
    let (meta, report) = resolved_with(&file, &inputs_with(t.clone()));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let peaks: Vec<f64> = layer(&meta, "s")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-peak"))
        .map(|k| k.time)
        .collect();
    let expected = [t.words[0].start, t.words[4].start, t.words[7].start].map(|s| 2.0 + s);
    assert_eq!(peaks.len(), 3, "{peaks:?}");
    assert!(
        peaks.iter().zip(expected).all(|(a, b)| close(*a, b)),
        "{peaks:?} vs {expected:?}"
    );
    let first = layer(&meta, "s")
        .keyframes
        .iter()
        .find(|k| k.id.ends_with("-peak"))
        .unwrap();
    assert!(first.id.starts_with("speech-s-pulse0-"), "{}", first.id);
    assert!(first.auto.as_deref().unwrap().starts_with("speech:"));
    let written = meta.to_json().unwrap();
    assert!(!written.contains("speech-s-pulse"), "{written}");
    assert_eq!(meta.minimum_reader_version(), 50);
    // A word the take never says is a break.
    let file = project(
        json!({}),
        json!([image(
            "s",
            1,
            json!({"pulse": {"on": {"words": ["Archimedes"], "of": "intro"}}})
        )]),
    );
    let (_, report) = resolved_with(&file, &inputs_with(t));
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.is_break() && p.to_string().contains("no word \"Archimedes\"")),
        "{:?}",
        report.problems
    );
}

/// A flash on sentence starts lights at each, and a range of words limits
/// it.
#[test]
fn flash_on_sentence_starts() {
    let t = timing();
    let file = project(
        json!({"flash": {"on": {"sentences": "start", "of": "intro"}, "opacity": 0.4, "duration": 0.12}}),
        json!([]),
    );
    let (meta, report) = resolved_with(&file, &inputs_with(t.clone()));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let flash = layer(&meta, "beat-flash-0");
    let on: Vec<f64> = flash
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-on"))
        .map(|k| k.time)
        .collect();
    assert_eq!(on.len(), 3);
    for (a, s) in on.iter().zip(&t.sentences) {
        assert!(close(*a, 2.0 + s.start), "{on:?}");
    }
    assert!(flash.name.contains("sentences"), "{}", flash.name);
    // Limited to sentence 2.
    let file = project(
        json!({"flash": {"on": {"words": "all", "of": "intro"}, "range": {"sentence": 2, "of": "intro"}}}),
        json!([]),
    );
    let (meta, _) = resolved_with(&file, &inputs_with(t.clone()));
    let on = layer(&meta, "beat-flash-0")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-on"))
        .count();
    let (a, b) = (t.sentences[1].words[0], t.sentences[1].words[1]);
    assert_eq!(on, (b - a + 1) as usize, "one flash per word of sentence 2");
}

/// `emphasised` takes the most prominent words, `top` of them — the same
/// ones every time, a tie going to the earlier word — or those `above` a
/// threshold; with no prominence measured it says so.
#[test]
fn emphasised_top_n_is_deterministic() {
    let mut t = timing();
    let scores = [0.2, 0.9, 0.1, 0.7, 0.3, 0.7, 0.4, 0.95, 0.1, 0.2, 0.5];
    for (w, p) in t.words.iter_mut().zip(scores) {
        w.prominence = Some(p);
    }
    let shake = |on: Value| {
        project(
            json!({}),
            json!([image("s", 1, json!({"shake": {"on": on, "amplitude": 6}}))]),
        )
    };
    let hits = |meta: &ProjectMetadata| -> Vec<f64> {
        layer(meta, "s")
            .keyframes
            .iter()
            .filter(|k| k.id.ends_with("-hit"))
            .map(|k| k.time)
            .collect()
    };
    let file = shake(json!({"words": "emphasised", "of": "intro", "top": 3}));
    let (first, _) = resolved_with(&file, &inputs_with(t.clone()));
    let (second, _) = resolved_with(&file, &inputs_with(t.clone()));
    assert_eq!(first, second, "deterministic");
    // 0.95 (word 8), 0.9 (word 2), then the tie 0.7: word 4 before word 6.
    let expected = [1, 3, 7].map(|i| 2.0 + t.words[i].start);
    let got = hits(&first);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(
        got.iter().zip(expected).all(|(a, b)| close(*a, b)),
        "{got:?} vs {expected:?}"
    );
    let (above, _) = resolved_with(
        &shake(json!({"words": "emphasised", "of": "intro", "above": 0.8})),
        &inputs_with(t.clone()),
    );
    assert_eq!(hits(&above).len(), 2);
    // No prominence: said, as a break, and nothing made.
    let (none, report) = resolved_with(&file, &inputs_with(timing()));
    assert!(hits(&none).is_empty());
    assert!(report
        .problems
        .iter()
        .any(|p| p.to_string().contains("no word prominence yet")));
}

/// `until` ends a sequence entry on a word, and the next starts there.
#[test]
fn sequence_until_ends_on_a_word() {
    let t = timing();
    let mut settings = json!({});
    settings["sequence"] = json!([
        {"layer": "a", "until": {"word": 4, "of": "intro", "offset": -0.1}},
        {"layer": "b", "until": {"sentence": 3, "of": "intro"}},
        {"layer": "c", "seconds": 1}
    ]);
    let file = project(
        settings,
        json!([
            image("a", 1, json!({"startTime": 0.5})),
            image("b", 2, json!({})),
            image("c", 3, json!({}))
        ]),
    );
    let (meta, report) = resolved_with(&file, &inputs_with(t.clone()));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    let a = layer(&meta, "a");
    assert!(close(
        a.start_time + a.duration.unwrap(),
        2.0 + t.words[3].start - 0.1
    ));
    let b = layer(&meta, "b");
    assert!(close(b.start_time, a.start_time + a.duration.unwrap()));
    assert!(close(
        b.start_time + b.duration.unwrap(),
        2.0 + t.sentences[2].start
    ));
    assert!(close(
        layer(&meta, "c").start_time,
        2.0 + t.sentences[2].start
    ));
    assert_eq!(meta.minimum_reader_version(), 50);
}

fn per(layers: &[&str], extra: Value) -> Value {
    let mut entry = json!({"layers": layers, "per": "sentence", "of": "intro", "lead": 0.1});
    for (k, v) in extra.as_object().unwrap() {
        entry[k] = v.clone();
    }
    entry
}

/// One slide per sentence: each lasts until the next sentence starts, less
/// the lead; fewer slides than sentences, and the last holds through the
/// rest (with a warning); more, and the extras get no slot.
#[test]
fn per_with_fewer_layers_holds_the_last() {
    let t = timing();
    let slides = |n: usize| -> Value {
        Value::Array(
            (0..n)
                .map(|k| image(&format!("s{k}"), k as i64 + 1, json!({"startTime": 0})))
                .collect(),
        )
    };
    let mut settings = json!({});
    settings["sequence"] = json!([per(&["s0", "s1", "s2"], json!({}))]);
    let mut file = project(settings.clone(), slides(3));
    file["layers"][1]["startTime"] = json!({"sentence": 1, "of": "intro", "offset": -0.1});
    let (meta, report) = resolved_with(&file, &inputs_with(t.clone()));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    for k in 0..3 {
        let s = layer(&meta, &format!("s{k}"));
        assert!(
            close(s.start_time, 2.0 + t.sentences[k].start - 0.1),
            "s{k} starts {}",
            s.start_time
        );
    }
    let last = layer(&meta, "s2");
    assert!(close(
        last.start_time + last.duration.unwrap(),
        2.0 + t.sentences[2].end + 0.5
    ));

    settings["sequence"] = json!([per(&["s0", "s1"], json!({}))]);
    let (meta, report) = resolved_with(
        &project(settings.clone(), slides(2)),
        &inputs_with(t.clone()),
    );
    let s1 = layer(&meta, "s1");
    assert!(close(s1.start_time, 2.0 + t.sentences[1].start - 0.1));
    assert!(close(
        s1.start_time + s1.duration.unwrap(),
        2.0 + t.sentences[2].end + 0.5
    ));
    assert!(report.problems.iter().any(|p| matches!(
        p,
        TimingProblem::PerSlots {
            layers: 2,
            sentences: 3,
            ..
        }
    )));
    assert!(report
        .problems
        .iter()
        .any(|p| p.to_string().contains("the last slide holds")));

    settings["sequence"] = json!([per(&["s0", "s1", "s2", "s3"], json!({}))]);
    let (meta, report) = resolved_with(&project(settings, slides(4)), &inputs_with(t));
    assert_eq!(
        layer(&meta, "s3").start_time,
        0.0,
        "no slot: its own numbers"
    );
    assert!(report
        .problems
        .iter()
        .any(|p| p.to_string().contains("the last 1 get no slot")));
}

/// With `quantize`, every sentence cut lands on the nearest beat — music
/// and voice together.
#[test]
fn quantized_sentence_cuts_land_on_beats() {
    let t = timing();
    let mut settings = json!({"beatClock": {"resourceID": "song", "startTime": 0}});
    settings["sequence"] = json!([per(
        &["s0", "s1", "s2"],
        json!({"quantize": "beat", "lead": 0})
    )]);
    let file = project(
        settings,
        json!([
            image("s0", 1, json!({"startTime": {"beat": 1}})),
            image("s1", 2, json!({})),
            image("s2", 3, json!({}))
        ]),
    );
    let (meta, report) = resolved_with(&file, &inputs_with(t.clone()));
    assert!(
        !report.problems.iter().any(TimingProblem::is_break),
        "{:?}",
        report.problems
    );
    for (k, sentence) in [(1, 1usize), (2, 2)] {
        let s = layer(&meta, &format!("s{k}"));
        let at = 2.0 + t.sentences[sentence].start;
        assert!(
            close(s.start_time, (at / 0.5).round() * 0.5),
            "s{k} at {} for {at}",
            s.start_time
        );
        assert!(
            ((s.start_time / 0.5).round() * 0.5 - s.start_time).abs() < 1e-9,
            "on a beat"
        );
    }
}
