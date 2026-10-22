//! Speech values on a looping take (docs/SPEECH-SYNC.md §6): a layer that
//! loops its take says each word once per play, so a value lands in the
//! play it names (`"play": n`), else the play nearest the field's number —
//! with a warning, never silently the first — and the editors' snaps,
//! ticks and synced reveals see every play.

use promo_model::speech::{TakeTiming, TimingSource};
use promo_model::{ProjectLayer, ProjectMetadata};
use promo_timeline::musical::Edge;
use promo_timeline::speech_time::{snap_speech, word_ticks, SpeechUnit};
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

const SCRIPT: &str = "Euclid proved it. Primes never end, said Euclid. Then he rested.";
/// The voice-over starts here and loops its 6 s take three times.
const AT: f64 = 2.0;
const PERIOD: f64 = 6.0;

fn timing() -> TakeTiming {
    let mut t = promo_model::estimate::estimate_words(SCRIPT, PERIOD, None, 1.0);
    t.source = TimingSource::Provider("elevenlabs".into());
    t.estimated = false;
    t.confidence = 1.0;
    t.aligner_version = None;
    t
}

fn inputs() -> BeatGrids {
    let mut grids = BeatGrids::default();
    grids.words.by_resource.insert("intro".into(), timing());
    grids
}

fn image(id: &str, extra: Value) -> Value {
    let mut layer = json!({"id": id, "name": id, "sortIndex": 1, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 1,
                           "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn project(voice: Value, layers: Value) -> Value {
    let mut vo = json!({"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio",
                        "isEnabled": true, "startTime": AT, "duration": 3.0 * PERIOD,
                        "resourceID": "intro", "beyondEnd": "loop", "keyframes": []});
    for (k, v) in voice.as_object().unwrap() {
        vo[k] = v.clone();
    }
    let mut all = vec![vo];
    all.extend(layers.as_array().unwrap().iter().cloned());
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 24, "videoDuration": 24, "subtitles": [],
        "compositionSettings": {}, "layers": all,
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.mp3",
                       "displayName": "Intro", "addedAt": 0, "duration": PERIOD,
                       "speech": {"text": SCRIPT, "provider": "elevenlabs"}}]
    })
}

fn resolved(value: &Value) -> (ProjectMetadata, TimingReport) {
    let mut meta = ProjectMetadata::from_json(&value.to_string()).expect("decodes");
    let report = resolve_timing(&mut meta, &inputs());
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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn ambiguous<'a>(report: &'a TimingReport, id: &str) -> Vec<&'a TimingProblem> {
    report
        .problems
        .iter()
        .filter(|p| matches!(p, TimingProblem::SpeechLoopAmbiguous { layer, .. } if layer == id))
        .collect()
}

/// A named play lands in it, quietly; a value with no play lands in the
/// first and says the take is said three times; a play past the layer's
/// end is a break.
#[test]
fn a_value_lands_in_the_play_it_names() {
    let t = timing();
    let word4 = |play: f64| AT + play * PERIOD + t.words[3].start;
    let file = project(
        json!({}),
        json!([
            image(
                "named",
                json!({"startTime": {"word": 4, "of": "intro", "play": 2}})
            ),
            image("plain", json!({"startTime": {"word": 4, "of": "intro"}})),
            image(
                "past",
                json!({"startTime": {"word": 4, "of": "intro", "play": 4}})
            ),
        ]),
    );
    let (meta, report) = resolved(&file);
    assert!(close(layer(&meta, "named").start_time, word4(1.0)));
    assert!(
        ambiguous(&report, "named").is_empty(),
        "{:?}",
        report.problems
    );
    let row = report
        .speech
        .iter()
        .find(|r| r.at.contains("[named]"))
        .expect("a row");
    assert!(row.label.ends_with("(play 2)"), "{}", row.label);

    assert!(close(layer(&meta, "plain").start_time, word4(0.0)));
    let warned = ambiguous(&report, "plain");
    assert_eq!(warned.len(), 1, "{:?}", report.problems);
    assert!(matches!(
        warned[0],
        TimingProblem::SpeechLoopAmbiguous {
            plays: 3,
            used: 1,
            ..
        }
    ));
    let text = warned[0].to_string();
    assert!(
        text.contains("said 3 times") && text.contains("\"play\""),
        "{text}"
    );
    assert!(!warned[0].is_break());

    assert!(report.problems.iter().any(|p| p.is_break()
        && p.to_string().contains("past")
        && p.to_string().contains("play 4")));
}

/// A keyframe on a word falls in the play its layer is showing: settled,
/// so nothing is said; a layer that lives across several plays is
/// ambiguous again, and says so.
#[test]
fn a_keyframe_falls_in_the_play_its_layer_shows() {
    let t = timing();
    let rested = |play: f64| AT + play * PERIOD + t.words[10].start;
    let key = json!([{"id": "k", "time": {"word": "rested", "of": "intro"}}]);
    let file = project(
        json!({}),
        json!([
            image(
                "third",
                json!({"startTime": 15, "duration": 6, "keyframes": key})
            ),
            image(
                "long",
                json!({"startTime": 2, "duration": 20, "keyframes": key})
            ),
        ]),
    );
    let (meta, report) = resolved(&file);
    assert!(
        close(layer(&meta, "third").keyframes[0].time, rested(2.0) - 15.0),
        "{}",
        layer(&meta, "third").keyframes[0].time
    );
    assert!(
        ambiguous(&report, "third").is_empty(),
        "{:?}",
        report.problems
    );
    assert!(close(
        layer(&meta, "long").keyframes[0].time,
        rested(0.0) - 2.0
    ));
    assert!(matches!(
        ambiguous(&report, "long")[..],
        [TimingProblem::SpeechLoopAmbiguous {
            used: 1,
            plays: 3,
            ..
        }]
    ));
}

/// A layer that holds its take, or loops it with no set end, plays it once:
/// nothing to choose, nothing said.
#[test]
fn a_take_that_plays_once_needs_no_play() {
    for voice in [json!({"beyondEnd": "hold"}), json!({"duration": null})] {
        let file = project(
            voice.clone(),
            json!([image("a", json!({"startTime": {"word": 4, "of": "intro"}}))]),
        );
        let (meta, report) = resolved(&file);
        assert!(close(
            layer(&meta, "a").start_time,
            AT + timing().words[3].start
        ));
        assert!(
            ambiguous(&report, "a").is_empty(),
            "{voice}: {:?}",
            report.problems
        );
    }
}

/// The editors see every play: a snap near the second play's word writes
/// that play, and the word lane ticks each word once per play.
#[test]
fn snaps_and_ticks_see_every_play() {
    let t = timing();
    let near = AT + PERIOD + t.words[3].start + 0.02;
    let file = project(json!({}), json!([image("s", json!({"startTime": near}))]));
    let meta = ProjectMetadata::from_json(&file.to_string()).unwrap();
    let snapped = snap_speech(&meta, &inputs(), "s", Edge::Start, SpeechUnit::Word).unwrap();
    assert_eq!(
        snapped.value,
        json!({"word": 4, "of": "intro", "text": "Primes", "play": 2})
    );
    assert!(close(snapped.seconds, AT + PERIOD + t.words[3].start));
    assert!(snapped.label.ends_with("(play 2)"), "{}", snapped.label);

    let ticks = word_ticks(&meta, &inputs(), "vo").unwrap();
    assert_eq!(ticks.len(), 3 * t.words.len());
    assert!(close(
        ticks[t.words.len()].start,
        AT + PERIOD + t.words[0].start
    ));
}

/// A synced caption in the second play arrives on the second play's words.
#[test]
fn a_synced_caption_follows_the_play_it_shows_in() {
    let t = timing();
    let start = AT + PERIOD + 0.5;
    let file = project(
        json!({}),
        json!([{"id": "cap", "name": "cap", "sortIndex": 1, "kind": "caption",
                "isEnabled": true, "startTime": start, "duration": 4,
                "captionText": "Primes never end, said Euclid.",
                "captionStyle": {"reveal": {"by": "word", "sync": {"of": "intro"}}},
                "keyframes": []}]),
    );
    let (meta, _) = resolved(&file);
    let times = layer(&meta, "cap")
        .caption_style
        .as_ref()
        .and_then(|s| s.reveal.as_ref())
        .and_then(|r| r.unit_times.clone())
        .expect("unit times");
    assert!(
        close(times[0], AT + PERIOD + t.words[3].start - start),
        "{times:?}"
    );
}
