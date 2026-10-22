//! Speech values resolved to seconds, slice 6 (docs/SPEECH-SYNC.md §6): a
//! time field naming a word of a take lands where the layer playing that
//! take says it — through its start, trims, speed and cuts — inside the one
//! timing pass, beside the beats.

use promo_model::speech::{TakeTiming, TimingSource};
use promo_model::ProjectMetadata;
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem, TimingReport};
use serde_json::{json, Value};

const SCRIPT: &str = "Euclid proved it. Primes never end, said Euclid. Then he rested.";

/// The take's word times: the estimate's layout over 6 s, as a provider's
/// record (deterministic, and every word apart from its neighbours).
fn timing(script: &str) -> TakeTiming {
    let mut t = promo_model::estimate::estimate_words(script, 6.0, None, 1.0);
    t.source = TimingSource::Provider("elevenlabs".into());
    t.estimated = false;
    t.confidence = 1.0;
    t.aligner_version = None;
    t
}

fn inputs() -> BeatGrids {
    let mut grids = BeatGrids::default();
    grids
        .words
        .by_resource
        .insert("intro".into(), timing(SCRIPT));
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

fn voice(extra: Value) -> Value {
    let mut layer = json!({"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio",
                           "isEnabled": true, "startTime": 2, "duration": 6,
                           "resourceID": "intro", "keyframes": []});
    for (k, v) in extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    layer
}

fn project(settings: Value, layers: Value, intro: Value) -> Value {
    let mut take = json!({"id": "intro", "kind": "audio", "filename": "intro.mp3",
                          "displayName": "Intro", "addedAt": 0, "duration": 6,
                          "speech": {"text": SCRIPT, "provider": "elevenlabs"}});
    for (k, v) in intro.as_object().unwrap() {
        take[k] = v.clone();
    }
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 12, "videoDuration": 12, "subtitles": [],
        "compositionSettings": settings, "layers": layers,
        "resources": [take,
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

fn resolved(value: &Value) -> (ProjectMetadata, TimingReport) {
    resolved_with(value, &inputs())
}

fn layer<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a promo_model::ProjectLayer {
    promo_model::nesting::all_layers(meta)
        .into_iter()
        .find(|l| l.id == id)
        .expect("layer")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn breaks(report: &TimingReport) -> Vec<String> {
    report
        .problems
        .iter()
        .filter(|p| p.is_break())
        .map(|p| p.to_string())
        .collect()
}

/// A word by index, by its text and the nth time it is said, a sentence by
/// number — through a resource id, a layer id and the scope's speech clock.
#[test]
fn word_by_index_text_nth_and_sentence() {
    let t = timing(SCRIPT);
    let file = project(
        json!({"speechClock": {"layer": "vo"}}),
        json!([
            voice(json!({})),
            image("a", json!({"startTime": {"word": 4, "of": "intro"}})),
            image("b", json!({"startTime": {"word": "euclid", "nth": 2}})),
            image(
                "c",
                json!({"startTime": {"sentence": 2, "of": "vo"},
                               "duration": {"sentence": 2, "of": "intro"}})
            ),
            image(
                "d",
                json!({"startTime": 1, "duration": 8,
                               "keyframes": [{"id": "k", "time": {"word": "rested"}}]})
            ),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    assert!(close(layer(&meta, "a").start_time, 2.0 + t.words[3].start));
    assert!(close(layer(&meta, "b").start_time, 2.0 + t.words[7].start));
    let c = layer(&meta, "c");
    assert!(close(c.start_time, 2.0 + t.sentences[1].start));
    assert!(close(
        c.duration.unwrap(),
        t.sentences[1].end - t.sentences[1].start
    ));
    assert!(close(
        layer(&meta, "d").keyframes[0].time,
        2.0 + t.words[10].start - 1.0
    ));
    // Listed with the word each landed on, and the timing's source.
    let row = report
        .speech
        .iter()
        .find(|r| r.at == "layers[a].startTime")
        .expect("listed");
    assert_eq!(row.word, Some((4, "Primes".to_string())));
    assert_eq!(row.source, "provider:elevenlabs");
    assert_eq!(report.speech.len(), 5, "{:?}", report.speech);
    // A word not said, an index or an nth past the end: breaks, numbers stand.
    for (spec, says) in [
        (
            json!({"word": "Eucld", "of": "intro"}),
            "no word \"Eucld\" in \"intro\" (closest: word 1 \"Euclid\")",
        ),
        (
            json!({"word": 40, "of": "intro"}),
            "word 40 is past the end of \"intro\" (11 words)",
        ),
        (
            json!({"word": "Euclid", "nth": 3, "of": "intro"}),
            "says \"Euclid\" 2 times, not 3",
        ),
        (
            json!({"sentence": 4, "of": "intro"}),
            "sentence 4 is past the end",
        ),
        (
            json!({"word": 2, "of": "nobody"}),
            "\"nobody\" is no take here",
        ),
        (json!({"word": 2, "of": "song"}), "\"song\" says nothing"),
    ] {
        let file = project(
            json!({}),
            json!([
                voice(json!({})),
                {"id": "music", "name": "Music", "sortIndex": 2, "kind": "audio",
                 "isEnabled": true, "startTime": 0, "resourceID": "song", "keyframes": []},
                image("x", json!({"startTime": spec}))
            ]),
            json!({}),
        );
        let (meta, report) = resolved(&file);
        let said = breaks(&report);
        assert!(said.iter().any(|m| m.contains(says)), "{spec}: {said:?}");
        assert_eq!(layer(&meta, "x").start_time, 0.0);
    }
}

/// The end edge, a seconds offset and a beats offset on the scope's grid.
#[test]
fn edges_and_offsets_including_beats() {
    let t = timing(SCRIPT);
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            voice(json!({})),
            image(
                "a",
                json!({"startTime": {"word": 2, "of": "intro", "edge": "end"}})
            ),
            image(
                "b",
                json!({"startTime": {"word": 2, "of": "intro", "offset": -0.1}})
            ),
            image(
                "c",
                json!({"startTime": {"word": 2, "of": "intro", "offset": {"beats": 1}}})
            ),
            image(
                "d",
                json!({"startTime": {"word": 2, "of": "intro"},
                               "duration": {"word": 5, "of": "intro", "edge": "start"}})
            ),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    let w = 2.0 + t.words[1].start;
    assert!(close(layer(&meta, "a").start_time, 2.0 + t.words[1].end));
    assert!(close(layer(&meta, "b").start_time, w - 0.1));
    // 120 BPM from 0: one beat is half a second wherever it starts.
    assert!(close(layer(&meta, "c").start_time, w + 0.5));
    assert!(close(
        layer(&meta, "d").duration.unwrap(),
        t.words[4].start - t.words[1].start
    ));
}

/// `quantize` puts the word's time on the nearest beat, downbeat or bar.
#[test]
fn quantize_lands_on_the_beat_grid() {
    let t = timing(SCRIPT);
    let file = project(
        json!({"beatClock": {"resourceID": "song", "startTime": 0}}),
        json!([
            voice(json!({})),
            image(
                "a",
                json!({"startTime": {"word": 6, "of": "intro", "quantize": "beat"}})
            ),
            image(
                "b",
                json!({"startTime": {"word": 6, "of": "intro", "quantize": "bar"}})
            ),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    let w = 2.0 + t.words[5].start;
    assert!(close(layer(&meta, "a").start_time, (w / 0.5).round() * 0.5));
    assert!(close(layer(&meta, "b").start_time, (w / 2.0).round() * 2.0));
    assert!(
        !close(w, (w / 0.5).round() * 0.5),
        "the word sits off the beat"
    );
}

/// The take's second maps through the layer: its start, the resource's
/// trim and speed, and a media cut's own trim.
#[test]
fn clock_follows_trim_speed_and_cuts() {
    let t = timing(SCRIPT);
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image("a", json!({"startTime": {"word": 6, "of": "intro"}}))
        ]),
        json!({"trimStart": 1.0, "speed": 2.0}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    assert!(close(
        layer(&meta, "a").start_time,
        2.0 + (t.words[5].start - 1.0) / 2.0
    ));
    // A cut of the take: its own trim, not the resource's.
    let file = project(
        json!({}),
        json!([
            voice(json!({"mediaCutID": "cut"})),
            image("a", json!({"startTime": {"word": 6, "of": "vo"}}))
        ]),
        json!({"trimStart": 1.0, "mediaCuts": [{"id": "cut", "trimStart": 0.5}]}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    assert!(close(
        layer(&meta, "a").start_time,
        2.0 + t.words[5].start - 0.5
    ));
}

/// A word the layer never plays is a break that names it, and the start
/// keeps its stored number.
#[test]
fn trimmed_out_word_is_a_break() {
    let t = timing(SCRIPT);
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image("a", json!({"startTime": {"word": 1, "of": "intro"}}))
        ]),
        json!({"trimStart": t.words[2].start}),
    );
    let (meta, report) = resolved(&file);
    let said = breaks(&report);
    assert!(
        said.iter()
            .any(|m| m.contains("word 1 \"Euclid\" is trimmed out of layer \"vo\"")),
        "{said:?}"
    );
    assert_eq!(layer(&meta, "a").start_time, 0.0);
}

/// The voice-over is itself sequenced: the loop places it, then its words.
#[test]
fn clock_layer_that_is_itself_sequenced_resolves_in_the_loop() {
    let t = timing(SCRIPT);
    let mut settings = json!({});
    settings["sequence"] = json!([{"layer": "lead", "seconds": 1.5}, {"layer": "vo"}]);
    let file = project(
        settings,
        json!([
            image("lead", json!({"startTime": 0.5})),
            voice(json!({"startTime": 0})),
            image("a", json!({"startTime": {"word": 3, "of": "vo"}})),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    assert!(breaks(&report).is_empty(), "{:?}", report.problems);
    assert!(close(layer(&meta, "vo").start_time, 2.0));
    assert!(close(layer(&meta, "a").start_time, 2.0 + t.words[2].start));
    // Resolving the resolved project changes nothing.
    let mut again = meta.clone();
    resolve_timing(&mut again, &inputs());
    assert_eq!(again, meta);
}

/// The music enters on a word while the voice starts on a beat of that
/// music: neither can settle. Both keep their stored numbers, and both are
/// named — a break.
#[test]
fn speech_beat_cycle_is_circular() {
    let file = project(
        json!({"beatClock": {"resourceID": "song"}}),
        json!([
            voice(json!({"startTime": {"beat": 5}})),
            {"id": "music", "name": "Music", "sortIndex": 2, "kind": "audio",
             "isEnabled": true, "startTime": {"word": 3, "of": "vo"}, "duration": 10,
             "resourceID": "song", "keyframes": []},
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    let circular: Vec<&TimingProblem> = report
        .problems
        .iter()
        .filter(|p| matches!(p, TimingProblem::Circular { .. }))
        .collect();
    assert_eq!(circular.len(), 2, "{:?}", report.problems);
    assert!(circular.iter().all(|p| p.is_break()));
    assert_eq!(layer(&meta, "vo").start_time, 0.0);
    assert_eq!(layer(&meta, "music").start_time, 0.0);
    // And the file still says what it said.
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert_eq!(written["layers"][0]["startTime"], json!({"beat": 5}));
    assert_eq!(
        written["layers"][1]["startTime"],
        json!({"of": "vo", "word": 3})
    );
}

/// An index whose guard word moved follows the word, and says so; a guard
/// the take no longer says is a break.
#[test]
fn guard_follows_a_moved_word_and_warns() {
    let t = timing(SCRIPT);
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image(
                "a",
                json!({"startTime": {"word": 2, "of": "intro", "text": "Primes"}})
            ),
            image(
                "b",
                json!({"startTime": {"word": 4, "of": "intro", "text": "primes!"}})
            ),
            image(
                "c",
                json!({"startTime": {"word": 2, "of": "intro", "text": "Archimedes"}})
            ),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    assert!(close(layer(&meta, "a").start_time, 2.0 + t.words[3].start));
    assert!(report.problems.iter().any(|p| matches!(p,
        TimingProblem::GuardMoved { layer, word: 2, to: 4, .. } if layer == "a")));
    assert!(report.problems.iter().any(|p| p
        .to_string()
        .contains("word 2 is now \"proved\"; followed \"Primes\" to word 4")));
    // The guard held: no warning for b.
    assert!(close(layer(&meta, "b").start_time, 2.0 + t.words[3].start));
    assert!(!report.problems.iter().any(|p| matches!(p,
        TimingProblem::GuardMoved { layer, .. } if layer == "b")));
    let said = breaks(&report);
    assert!(
        said.iter()
            .any(|m| m.contains("layer \"c\"") && m.contains("no \"Archimedes\"")),
        "{said:?}"
    );
}

/// A take with no record lays out on the estimate, says so, and is listed
/// in `missing_words` — what holds a render.
#[test]
fn missing_timing_lays_out_on_estimate_and_holds_render() {
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image("a", json!({"startTime": {"word": 4, "of": "intro"}}))
        ]),
        json!({}),
    );
    let (meta, report) = resolved_with(&file, &BeatGrids::default());
    let estimate = promo_model::estimate::estimate_words(SCRIPT, 6.0, None, 1.0);
    assert!(close(
        layer(&meta, "a").start_time,
        2.0 + estimate.words[3].start
    ));
    assert_eq!(report.missing_words, vec!["intro".to_string()]);
    assert!(report
        .problems
        .iter()
        .any(|p| matches!(p, TimingProblem::SpeechEstimated { take } if take == "intro")));
    assert!(!report.problems.iter().any(TimingProblem::is_break));
    assert_eq!(report.speech[0].source, "estimate (no timing yet)");
    // With timing, nothing is missing.
    let (_, report) = resolved(&file);
    assert!(report.missing_words.is_empty());
    assert_eq!(report.speech_clocks.len(), 1);
    assert_eq!(report.speech_clocks[0].layer.as_deref(), Some("vo"));
    assert_eq!(report.speech_clocks[0].words, 11);
}

/// A visual edge inside a word of a playing take warns, with the nearest
/// gap; an edge between words does not.
#[test]
fn cut_mid_word_warns_with_the_gap() {
    let t = timing(SCRIPT);
    let inside = 2.0 + (t.words[4].start + t.words[4].end) / 2.0;
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image(
                "a",
                json!({"startTime": {"word": 2, "of": "intro"}, "duration": inside - 2.0 - t.words[1].start})
            ),
            image(
                "b",
                json!({"startTime": 2.0 + t.words[6].start, "duration": 0.5})
            ),
        ]),
        json!({}),
    );
    let (_, report) = resolved(&file);
    let cuts: Vec<String> = report
        .problems
        .iter()
        .filter(|p| matches!(p, TimingProblem::CutMidWord { .. }))
        .map(|p| p.to_string())
        .collect();
    assert!(
        cuts.iter().any(|m| m.contains("layer \"a\"")
            && m.contains("cuts mid-word \"never\"")
            && m.contains("the nearest gap is")),
        "{cuts:?}"
    );
    // b starts on a word's start: not mid-word.
    assert!(
        !cuts.iter().any(|m| m.contains("layer \"b\" at")
            && m.contains(&format!("{:.2}s", 2.0 + t.words[6].start))),
        "{cuts:?}"
    );
}

/// `during` places start and length on a span, with lead and hold; the
/// sequence wins over it, it over `after`; a hand move drops it.
#[test]
fn during_places_a_span_and_yields_to_the_sequence() {
    let t = timing(SCRIPT);
    let mut settings = json!({});
    settings["sequence"] = json!([{"layer": "q", "seconds": 1}]);
    let file = project(
        settings,
        json!([
            voice(json!({})),
            image(
                "a",
                json!({"during": {"sentence": 2, "of": "intro", "lead": 0.15, "hold": 0.25}})
            ),
            image(
                "b",
                json!({"during": {"words": [4, 6], "of": "intro"}, "after": "a"})
            ),
            image("q", json!({"during": {"word": 1, "of": "intro"}})),
        ]),
        json!({}),
    );
    let (meta, report) = resolved(&file);
    let a = layer(&meta, "a");
    assert!(close(a.start_time, 2.0 + t.sentences[1].start - 0.15));
    assert!(close(
        a.start_time + a.duration.unwrap(),
        2.0 + t.sentences[1].end + 0.25
    ));
    let b = layer(&meta, "b");
    assert!(close(b.start_time, 2.0 + t.words[3].start));
    assert!(report.problems.iter().any(|p| matches!(p,
        TimingProblem::DuringWins { layer, over } if layer == "b" && over == "after")));
    assert!(report.problems.iter().any(|p| matches!(p,
        TimingProblem::SequenceWins { layer, over } if layer == "q" && over == "during")));
    // The file keeps `during`; a hand move drops it and the numbers stand.
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert!(written["layers"][1].get("during").is_some());
    let mut moved = meta.clone();
    moved.layers.as_mut().unwrap()[1].start_time = 0.25;
    let written: Value = serde_json::from_str(&moved.to_json().unwrap()).unwrap();
    assert!(written["layers"][1].get("during").is_none());
    assert_eq!(written["layers"][1]["startTime"], json!(0.25));
    resolve_timing(&mut moved, &inputs());
    assert_eq!(layer(&moved, "a").start_time, 0.25);
    assert!(layer(&moved, "a").during.is_none());
}

/// A speech value where a length is required is a break.
#[test]
fn speech_in_a_length_field_is_a_break() {
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image("a", json!({"fadeIn": {"word": 3, "of": "intro"}}))
        ]),
        json!({}),
    );
    let (_, report) = resolved(&file);
    assert!(breaks(&report)
        .iter()
        .any(|m| m.contains("fadeIn") && m.contains("a length is required")));
}

/// Every repository project — fixtures, demos, examples — says nothing
/// about speech, and word timing beside it changes not one number.
#[test]
fn seconds_only_and_beat_projects_are_unchanged() {
    use std::path::{Path, PathBuf};
    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    files(&path, out);
                }
            } else if name == "metadata.json" {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut all = Vec::new();
    for dir in ["examples", "demos", "fixtures"] {
        files(&root.join(dir), &mut all);
    }
    let mut checked = 0;
    for file in all {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(meta) = ProjectMetadata::from_json(&text) else {
            continue;
        };
        // The narrated recipe is timed on speech by design (S10); every
        // other project is seconds or beats.
        if promo_model::speech::needs_speech_reader(&meta) {
            assert!(
                file.to_string_lossy().contains("Narrated.promo"),
                "{}",
                file.display()
            );
            continue;
        }
        assert!(meta.minimum_reader_version() < 50, "{}", file.display());
        let mut plain = meta.clone();
        let report = resolve_timing(&mut plain, &BeatGrids::default());
        assert!(report.speech.is_empty() && report.speech_clocks.is_empty());
        assert!(report.missing_words.is_empty());
        // Word timing for every take beside it: the same answer.
        let mut timed = meta.clone();
        let mut grids = BeatGrids::default();
        for r in meta.resources.iter().flatten() {
            if let Some(script) = promo_model::speech::take_script(r) {
                grids
                    .words
                    .by_resource
                    .insert(r.id.clone(), timing(&script));
            }
        }
        let again = resolve_timing(&mut timed, &grids);
        assert_eq!(timed, plain, "{}", file.display());
        assert_eq!(again, report, "{}", file.display());
        assert_eq!(
            plain.minimum_reader_version(),
            meta.minimum_reader_version(),
            "{}",
            file.display()
        );
        checked += 1;
    }
    assert!(checked > 100, "only {checked} projects");
}

/// `validate` lists each speech value with its seconds, word and source,
/// in text and in JSON, and names the take it counted on.
#[test]
fn validate_lists_speech_times() {
    let t = timing(SCRIPT);
    let file = project(
        json!({}),
        json!([
            voice(json!({})),
            image(
                "s3",
                json!({"startTime": {"word": 4, "of": "intro"},
                                "duration": {"sentence": 2, "of": "intro"}})
            ),
        ]),
        json!({}),
    );
    let report =
        promo_timeline::report::file_report_with(&file.to_string(), None, &inputs()).unwrap();
    let text = report.text();
    assert!(text.contains("\n\nSpeech times\n"), "{text}");
    let line = format!(
        "layers[s3].startTime   {{\"of\":\"intro\",\"word\":4}}      → word 4 \u{201C}Primes\u{201D} at \
         {:.3} s  (provider:elevenlabs)",
        2.0 + t.words[3].start
    );
    assert!(text.contains(&line), "{text}");
    let line = format!(
        "layers[s3].duration    {{\"of\":\"intro\",\"sentence\":2}}  → ends with sentence 2 at \
         {:.3} s  (provider:elevenlabs)",
        2.0 + t.sentences[1].end
    );
    assert!(text.contains(&line), "{text}");
    assert!(!text.contains("Musical times"), "{text}");
    let json = report.json();
    let rows = json["resolvedTimes"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["at"] == "layers[s3].startTime")
        .expect("a row");
    assert_eq!(row["clock"], "speech");
    assert_eq!(row["word"], json!({"index": 4, "text": "Primes"}));
    assert_eq!(row["source"], "provider:elevenlabs");
    assert_eq!(row["value"], json!({"word": 4, "of": "intro"}));
    let clocks = json["speechClocks"].as_array().unwrap();
    assert_eq!(clocks.len(), 1);
    assert_eq!(clocks[0]["take"], "intro");
    assert_eq!(clocks[0]["layer"], "vo");
    assert_eq!(clocks[0]["words"], 11);
    assert_eq!(clocks[0]["sentences"], 3);
}
