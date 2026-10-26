//! Generated layers (MIDI.md §7) against a mock ACE-Step: the request the
//! client sends, the file it keeps (aligned, a take long, 48 kHz stereo),
//! the receipt that follows its bytes, `contextChanged` after a MIDI edit,
//! the drift refusal, cancel, the server's errors and validate's words.
//! The context take needs the bank (`PROMOSHOT_SOUNDS`); without it a
//! test says so and passes.

#[path = "support/mock_ace.rs"]
mod mock_ace;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mock_ace::MockAce;
use promo_midi::generate::{self, Endpoint, Progress, Request};
use promo_midi::{settle, sounds, Bank};
use serde_json::{json, Value};

fn bank() -> Option<std::sync::Arc<Bank>> {
    match sounds::installed_default() {
        Some(path) => Some(Bank::open(&path).expect("the installed bank opens")),
        None => {
            eprintln!("SKIPPED: {}", sounds::missing_bank_message());
            None
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "promo-midi-gen-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    dir
}

fn groove() -> Value {
    json!({
        "tempo": 120, "timeSignature": [4, 4], "bars": 2, "loop": { "from": 1, "to": 3 },
        "tracks": [
            { "name": "Drums", "bank": "drums", "program": 0,
              "steps": { "resolution": 16, "length": 16, "lanes": [
                { "note": 36, "on": [0, 4, 8, 12], "velocity": 110 },
                { "note": 42, "on": [2, 6, 10, 14], "velocity": 80 } ] } },
            { "name": "Bass", "program": 33, "notes": [
                { "t": 0, "d": 1, "n": 45 }, { "t": 2, "d": 1, "n": 48 },
                { "t": 4, "d": 1, "n": 52 }, { "t": 6, "d": 1, "n": 45 } ] }
        ]
    })
}

fn project(dir: &Path, midi: Value) -> Value {
    let doc = json!({
        "id": "P", "name": "T", "createdAt": 0, "state": "recorded", "subtitles": [],
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "compositionSettings": {},
        "layers": [{ "id": "L", "kind": "audio", "resourceID": "M", "startTime": 0, "duration": 4 }],
        "resources": [{ "id": "M", "kind": "audio", "filename": "", "displayName": "Groove",
            "addedAt": 0, "midi": midi }],
    });
    std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();
    doc
}

/// Tones that follow the groove's bass: A and C in bar 1, E and A in bar 2.
fn harmony() -> (f64, Vec<Vec<f64>>) {
    (
        2.0,
        vec![vec![220.0, 261.63, 440.0], vec![329.63, 220.0, 659.26]],
    )
}

fn request(track: &str, caption: Option<&str>) -> Request {
    let mut r = Request::new(track);
    r.caption = caption.map(str::to_string);
    r.seed = Some(1234);
    r.poll = Duration::from_millis(5);
    r.timeout = Duration::from_secs(30);
    r
}

fn endpoint(mock: &MockAce) -> Endpoint {
    Endpoint::resolve(Some(&mock.url), None)
}

fn read(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap()
}

fn track<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["resources"][0]["midi"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap()
}

struct Phases(Vec<String>);
impl Progress for Phases {
    fn phase(&mut self, phase: &str) {
        self.0.push(phase.to_string());
    }
}

/// Generate: the lego request over the context (the 4 s loop tiled to
/// 32 s), the third period kept in Resources/.midi/ a take long, on the
/// chords and normalised, the track recorded, and the take re-rendered
/// with it — a new receipt, current.
#[test]
fn generate_writes_an_aligned_layer_and_a_new_take() {
    let Some(bank) = bank() else { return };
    let dir = scratch("gen");
    project(&dir, groove());
    let resources = dir.join("Resources");
    let first = settle::settle(&dir, "M", &bank).unwrap();
    let before_take = std::fs::read(resources.join("Groove.wav")).unwrap();

    let mock = MockAce::start();
    mock.with(|s| {
        s.polls_before_done = 2;
        s.answer_rate = Some(44_100);
        s.tones = Some(harmony());
    });
    let mut phases = Phases(Vec::new());
    let (made, settled) = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("warm string pad")),
        &endpoint(&mock),
        &bank,
        &mut phases,
    )
    .unwrap();
    assert!(made.created);
    assert_eq!(made.file, "Groove · Pad.wav");
    assert_eq!(made.seed, 1234);
    assert_eq!(made.model.as_deref(), Some("acestep-v15-base"));
    assert_eq!(made.track_name, "strings");
    assert_eq!(made.gate.kind, "tonal");
    assert!(made.gate.measured, "{:?}", made.gate);
    assert!(made.gate.chord_fit.unwrap() > 0.7, "{:?}", made.gate);
    assert_eq!(
        phases.0,
        [
            "rendering",
            "uploading",
            "queued",
            "generating",
            "downloading",
            "aligning",
            "mixing"
        ]
    );

    // What went to ACE-Step.
    let (fields, frames) = mock.with(|s| (s.releases[0].clone(), s.context_frames[0]));
    assert_eq!(fields["task_type"], "lego");
    assert_eq!(fields["prompt"], "warm string pad");
    assert_eq!(fields["track_name"], "strings");
    assert_eq!(fields["bpm"], "120");
    assert_eq!(fields["timesignature"], "4");
    assert_eq!(fields["keyscale"], "A minor");
    assert_eq!(fields["seed"], "1234");
    assert_eq!(fields["use_random_seed"], "false");
    assert_eq!(fields["model"], "acestep-v15-base");
    assert_eq!(fields["thinking"], "false");
    assert_eq!(fields["inference_steps"], "50");
    assert_eq!(fields["audio_format"], "wav");
    assert_eq!(fields["repainting_start"], "0");
    assert_eq!(fields["repainting_end"], "-1");
    assert!(!fields.contains_key("instruction") && !fields.contains_key("audio_duration"));
    assert_eq!(frames, 8 * 192_000, "the 4 s loop tiled to 32 s");
    assert_eq!(frames % 1920, 0, "a whole number of latents");

    // The layer file: 48 kHz stereo, exactly the take.
    let layer = std::fs::read(resources.join(".midi/Groove · Pad.wav")).unwrap();
    let audio = promo_midi::render::read_stereo(&layer).unwrap();
    assert_eq!(audio.frames(), 192_000);
    let pcm = promo_media::dsp::read_wav(&layer).unwrap();
    assert_eq!((pcm.rate, pcm.channels), (48_000.0, 2));
    let lufs = promo_media::loudness::integrated_lufs(&audio.interleaved(), 2, 48_000.0).unwrap();
    assert!(
        (lufs - promo_midi::layer::PART_LUFS).abs() < 0.5,
        "normalised: {lufs}"
    );

    // The track's record, and the take with the layer in it.
    let doc = read(&dir);
    let pad = track(&doc, "Pad");
    let g = &pad["generated"];
    assert_eq!(g["provider"], "ace-step");
    assert_eq!(g["task"], "lego");
    assert_eq!(g["caption"], "warm string pad");
    assert_eq!(g["seed"], 1234);
    assert_eq!(g["status"], "current");
    assert_eq!(g["file"], "Groove · Pad.wav");
    assert_eq!(
        g["contextHash"],
        promo_model::midi::context_hash(&groove(), None).as_str()
    );
    assert_eq!(g["trackName"], "strings");
    assert_eq!(g["keyscale"], "A minor");
    assert!(g["gate"]["chordFit"].as_f64().unwrap() > 0.7, "{g}");
    assert!(pad.get("volume").is_none(), "no defaults written");
    assert_ne!(settled.rendered_hash, first.rendered_hash);
    assert!(!settled.reused);
    assert_eq!(settled.layers.len(), 1);
    assert!(settled.layers[0].present);
    let after_take = std::fs::read(resources.join("Groove.wav")).unwrap();
    assert_ne!(after_take, before_take, "the layer is in the mix");
    assert_eq!(
        after_take.len(),
        before_take.len(),
        "the loop keeps its length"
    );
    assert_eq!(
        settle::stale(&doc, &resources, "M", &bank.sha256).unwrap(),
        None
    );
    assert!(
        settle::findings(&doc, &resources).is_empty(),
        "{:?}",
        settle::findings(&doc, &resources)
    );

    // Regenerate keeps the caption and the file, takes a new seed.
    let mut again = request("Pad", None);
    again.seed = Some(99);
    let (redo, _) = generate::generate(
        &dir,
        "M",
        &again,
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap();
    assert!(!redo.created);
    assert_eq!(redo.caption, "warm string pad");
    assert_eq!(redo.file, "Groove · Pad.wav");
    assert_eq!(track(&read(&dir), "Pad")["generated"]["seed"], 99);
    assert_eq!(
        read(&dir)["resources"][0]["midi"]["tracks"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An edit to the music marks the layer `contextChanged` on the next
/// settle (validate says so); a mix change does not; undoing the edit
/// makes it current again. Nothing regenerates.
#[test]
fn a_midi_edit_marks_the_context_changed() {
    let Some(bank) = bank() else { return };
    let dir = scratch("ctx");
    project(&dir, groove());
    let mock = MockAce::start();
    mock.with(|s| s.tones = Some(harmony()));
    generate::generate(
        &dir,
        "M",
        &request("Pad", Some("pad")),
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap();
    let resources = dir.join("Resources");
    let mut doc = read(&dir);
    let layer = std::fs::read(resources.join(".midi/Groove · Pad.wav")).unwrap();

    doc["resources"][0]["midi"]["tracks"][0]["volume"] = json!(0.5);
    settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    assert_eq!(
        track(&doc, "Pad")["generated"]["status"],
        "current",
        "the mix is not context"
    );

    doc["resources"][0]["midi"]["tracks"][1]["notes"][0]["n"] = json!(47);
    let settled = settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    assert_eq!(track(&doc, "Pad")["generated"]["status"], "contextChanged");
    assert_eq!(settled.layers[0].status.as_deref(), Some("contextChanged"));
    assert!(
        settled.report().contains("context changed"),
        "{}",
        settled.report()
    );
    let words = settle::findings(&doc, &resources);
    assert!(
        words
            .iter()
            .any(|w| w.starts_with("Generated layer context changed: \"Pad\"")),
        "{words:?}"
    );
    assert_eq!(
        std::fs::read(resources.join(".midi/Groove · Pad.wav")).unwrap(),
        layer
    );
    assert_eq!(
        mock.with(|s| s.releases.len()),
        1,
        "never regenerated silently"
    );

    doc["resources"][0]["midi"]["tracks"][1]["notes"][0]["n"] = json!(45);
    settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    assert_eq!(track(&doc, "Pad")["generated"]["status"], "current");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The gate, percussive: clicks on the beats are kept (their drift
/// reported); 80 ms late is refused, and nothing changes — no layer file,
/// the same metadata.json.
#[test]
fn a_percussive_layer_off_the_grid_is_refused() {
    let Some(bank) = bank() else { return };
    let dir = scratch("drift");
    project(&dir, groove());
    let before = std::fs::read(dir.join("metadata.json")).unwrap();
    let mock = MockAce::start();
    mock.with(|s| s.offset_ms = 80.0);
    let err = generate::generate(
        &dir,
        "M",
        &request("Shaker", Some("shaker sixteenths")),
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap_err();
    assert!(
        err.contains("off the MIDI grid") && err.contains("limit is 30 ms"),
        "{err}"
    );
    assert!(!dir.join("Resources/.midi/Groove · Shaker.wav").exists());
    assert_eq!(std::fs::read(dir.join("metadata.json")).unwrap(), before);
    assert_eq!(
        mock.with(|s| s.releases[0]["track_name"].clone()),
        "percussion"
    );

    mock.with(|s| s.offset_ms = 0.0);
    let (made, _) = generate::generate(
        &dir,
        "M",
        &request("Shaker", Some("shaker sixteenths")),
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap();
    assert_eq!(made.gate.kind, "percussive");
    assert!(made.gate.drift_ms.unwrap().abs() < 10.0, "{:?}", made.gate);
    assert!(
        made.gate.kick_p95_ms.is_none_or(|p| p < 50.0),
        "{:?}",
        made.gate
    );
    assert_eq!(
        track(&read(&dir), "Shaker")["generated"]["gate"]["kind"],
        "percussive"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The gate, tonal: a layer in another key, or one that sits out a bar,
/// is refused with its numbers.
#[test]
fn a_tonal_layer_off_the_chords_is_refused() {
    let Some(bank) = bank() else { return };
    let dir = scratch("chords");
    project(&dir, groove());
    let mock = MockAce::start();
    mock.with(|s| s.tones = Some((2.0, vec![vec![185.0, 277.18], vec![233.08, 311.13]])));
    let off = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("warm pad")),
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap_err();
    assert!(off.contains("does not follow the MIDI's chords"), "{off}");
    mock.with(|s| {
        s.tones = Some(harmony());
        s.silent_bars = vec![1];
    });
    let out = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("warm pad")),
        &endpoint(&mock),
        &bank,
        &mut generate::Quiet,
    )
    .unwrap_err();
    assert!(out.contains("sits out part of the take"), "{out}");
    assert!(!dir.join("Resources/.midi/Groove · Pad.wav").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

struct CancelAfter<'a>(&'a str, &'a AtomicBool);
impl Progress for CancelAfter<'_> {
    fn phase(&mut self, phase: &str) {
        if phase == self.0 {
            self.1.store(true, Ordering::Relaxed);
        }
    }
    fn cancelled(&self) -> bool {
        self.1.load(Ordering::Relaxed)
    }
}

/// Cancel while the server works: the client stops polling, keeps
/// nothing, and says it was cancelled.
#[test]
fn cancel_stops_and_keeps_nothing() {
    let Some(bank) = bank() else { return };
    let dir = scratch("cancel");
    project(&dir, groove());
    let before = std::fs::read(dir.join("metadata.json")).unwrap();
    let mock = MockAce::start();
    mock.with(|s| s.polls_before_done = 1_000_000);
    let flag = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let err = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("pad")),
        &endpoint(&mock),
        &bank,
        &mut CancelAfter("generating", &flag),
    )
    .unwrap_err();
    assert_eq!(err, generate::CANCELLED);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(mock.with(|s| s.polls) < 5, "stopped polling");
    assert_eq!(std::fs::read(dir.join("metadata.json")).unwrap(), before);
    assert!(!dir
        .join("Resources/.midi")
        .join("Groove · Pad.wav")
        .exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The server's own words come through: a failed task, a missing key
/// (401, with where to set it), a server that is not there (how to run
/// it); a new layer needs a caption and a MIDI track is not a layer.
#[test]
fn errors_say_what_happened() {
    let Some(bank) = bank() else { return };
    let dir = scratch("errors");
    project(&dir, groove());
    let mock = MockAce::start();
    mock.with(|s| s.tones = Some(harmony()));
    let quiet = &mut generate::Quiet;
    let none = generate::generate(
        &dir,
        "M",
        &request("Pad", None),
        &endpoint(&mock),
        &bank,
        quiet,
    )
    .unwrap_err();
    assert!(none.contains("needs a caption"), "{none}");
    let midi = generate::generate(
        &dir,
        "M",
        &request("Bass", Some("x")),
        &endpoint(&mock),
        &bank,
        quiet,
    )
    .unwrap_err();
    assert!(midi.contains("is a MIDI track"), "{midi}");
    mock.with(|s| s.fail = Some("out of memory".into()));
    let failed = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("x")),
        &endpoint(&mock),
        &bank,
        quiet,
    )
    .unwrap_err();
    assert!(failed.contains("out of memory"), "{failed}");
    mock.with(|s| {
        s.fail = None;
        s.require_key = Some("sesame".into());
    });
    let locked = generate::generate(
        &dir,
        "M",
        &request("Pad", Some("x")),
        &endpoint(&mock),
        &bank,
        quiet,
    )
    .unwrap_err();
    assert!(
        locked.contains("401") && locked.contains("PROMOSHOT_ACESTEP_KEY"),
        "{locked}"
    );
    assert!(!locked.contains("sesame"));
    let keyed = Endpoint::resolve(Some(&mock.url), Some("sesame"));
    generate::generate(&dir, "M", &request("Pad", Some("x")), &keyed, &bank, quiet).unwrap();
    let gone = Endpoint::resolve(Some("http://127.0.0.1:9"), None);
    let err =
        generate::generate(&dir, "M", &request("Pad", Some("x")), &gone, &bank, quiet).unwrap_err();
    assert!(
        err.contains("not answering") && err.contains("ACE-Step-1.5"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The endpoint's status: health and models, or how to start it.
#[test]
fn status_reports_the_endpoint() {
    let mock = MockAce::start();
    let status = generate::status(&endpoint(&mock), Duration::from_secs(5)).unwrap();
    assert_eq!(status["ok"], true);
    assert_eq!(status["service"], "ACE-Step API");
    assert_eq!(status["models"], json!(["acestep-v15-base"]));
    let gone = generate::status(
        &Endpoint::resolve(Some("http://127.0.0.1:9"), None),
        Duration::from_secs(2),
    )
    .unwrap_err();
    assert!(gone.contains("does not install it"), "{gone}");
}

/// The stored file is mixed like a track on every host — no ACE-Step
/// needed: muted it leaves the take byte for byte the MIDI's; soloed it
/// is all you hear; gone, the take plays without it, says so, and
/// validate warns.
#[test]
fn a_stored_layer_mixes_with_gain_mute_and_solo() {
    let Some(bank) = bank() else { return };
    let dir = scratch("mix");
    project(&dir, groove());
    let resources = dir.join("Resources");
    let plain = settle::settle(&dir, "M", &bank).unwrap();
    let midi_only = std::fs::read(resources.join("Groove.wav")).unwrap();
    // A hand-made layer: a 440 Hz tone a take long.
    let tone = promo_midi::render::Stereo {
        left: (0..192_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.2)
            .collect(),
        right: vec![0.0; 192_000],
    };
    std::fs::create_dir_all(resources.join(".midi")).unwrap();
    std::fs::write(
        resources.join(".midi/Tone.wav"),
        promo_midi::render::stereo_wav(&tone),
    )
    .unwrap();
    let mut doc = read(&dir);
    doc["resources"][0]["midi"]["tracks"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "Tone", "mute": true, "generated": { "file": "Tone.wav" } }));
    let muted = settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    assert_ne!(muted.rendered_hash, plain.rendered_hash);
    assert_eq!(
        std::fs::read(resources.join("Groove.wav")).unwrap(),
        midi_only,
        "muted: the MIDI alone"
    );

    doc["resources"][0]["midi"]["tracks"][2]["mute"] = json!(false);
    doc["resources"][0]["midi"]["tracks"][2]["solo"] = json!(true);
    settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    let solo =
        promo_midi::render::read_stereo(&std::fs::read(resources.join("Groove.wav")).unwrap())
            .unwrap();
    assert!(
        solo.right.iter().all(|x| x.abs() < 1e-6),
        "soloed: only the tone, left only"
    );
    assert!(solo.left.iter().any(|x| x.abs() > 0.1));

    std::fs::remove_file(resources.join(".midi/Tone.wav")).unwrap();
    assert!(
        settle::stale(&doc, &resources, "M", &bank.sha256)
            .unwrap()
            .is_some(),
        "its bytes are in the receipt"
    );
    let gone = settle::settle_doc(&mut doc, &resources, "M", &bank).unwrap();
    assert!(
        gone.notes.iter().any(|n| n.contains("Tone.wav is missing")),
        "{:?}",
        gone.notes
    );
    assert!(!gone.layers[0].present);
    let words = settle::findings(&doc, &resources);
    assert!(
        words
            .iter()
            .any(|w| w.starts_with("Generated layer missing: \"Tone\"")),
        "{words:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Not in the suite: the gate on real lego takes (the experiment's
/// outputs), `PROMO_ACE_TAKES` a list of 32 s loop takes and
/// `PROMO_ACE_PROJECT` the loop project they were made over.
#[test]
#[ignore]
fn the_gate_on_real_takes() {
    let project: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(&std::env::var("PROMO_ACE_PROJECT").unwrap()).join("metadata.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let block = project["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r.get("midi").is_some())
        .unwrap()["midi"]
        .clone();
    let block = promo_model::midi::MidiBlock::from_value(&block).unwrap();
    let score = promo_midi::score::expand(&block, None).unwrap();
    let frames = (score.length * 48_000.0).round() as usize;
    for path in std::env::var("PROMO_ACE_TAKES").unwrap().split(':') {
        let made = promo_midi::render::read_stereo(&std::fs::read(path).unwrap()).unwrap();
        let file = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = if file.contains("pad") {
            "synth"
        } else if file.contains("rums") || file.contains("perc") {
            "drums"
        } else {
            "strings"
        };
        let kept = if made.frames() >= 3 * frames { 2 } else { 0 };
        let layer = generate::assemble(&made, frames, kept, true);
        let wide = promo_midi::render::Stereo {
            left: made.left[..made.frames().min(3 * frames)].to_vec(),
            right: made.right[..made.frames().min(3 * frames)].to_vec(),
        };
        let n = wide.frames() as f64 / 48_000.0;
        let gate = promo_midi::layer::gate(
            &layer,
            &wide,
            (0.0, n),
            &score,
            &block,
            name,
            (0.0, score.length),
        );
        println!("{path}: {}", gate.to_json());
    }
}
