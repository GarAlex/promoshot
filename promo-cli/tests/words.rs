//! `promo words` and the CLI's open (docs/SPEECH-SYNC.md §4.5, §9, slice
//! 5): a take's record by the cache's rules — authoritative provider
//! timing as it is, the take aligned offline and stored when it has none,
//! an older aligner's record aligned again — and the record's JSON shape.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/speech")
}

/// The synthetic take (known truth) and the script it says.
fn synthetic() -> (Vec<u8>, String) {
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("synthetic-60w.json")).unwrap(),
    )
    .unwrap();
    (
        std::fs::read(fixtures().join("synthetic-60w.wav")).unwrap(),
        golden["script"].as_str().unwrap().to_string(),
    )
}

fn folder(name: &str, script: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-words-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    write_project(&dir, script);
    std::fs::write(dir.join("Resources/intro.wav"), synthetic().0).unwrap();
    dir
}

fn write_project(dir: &Path, script: &str) {
    let meta = json!({
        "id": "P", "name": "Words", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 30, "videoDuration": 30, "subtitles": [],
        "compositionSettings": {"canvasWidth": 160, "canvasHeight": 90},
        "layers": [{"id": "vo", "name": "VO", "sortIndex": 0, "kind": "audio",
                    "isEnabled": true, "startTime": 2.0, "resourceID": "intro",
                    "keyframes": []}],
        "resources": [{"id": "intro", "kind": "audio", "filename": "intro.wav",
                       "displayName": "Intro", "addedAt": 0,
                       "speech": {"text": script, "provider": "openai", "voiceID": "alloy",
                                  "renderedHash": "x"}}]
    });
    std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
}

fn promo(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fail(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn words(dir: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["words", dir.to_str().unwrap(), "--json"];
    args.extend_from_slice(extra);
    serde_json::from_str(&promo(&args)).unwrap()
}

fn records(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("Resources/.words"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// AC7 (the CLI half): the record's keys, `fromCache`, and the take
/// aligned and stored on the first ask.
#[test]
fn promo_words_json_shape() {
    let (_, script) = synthetic();
    let dir = folder("shape", &script);
    let first = words(&dir, &[]);
    for key in [
        "timingVersion",
        "audio",
        "script",
        "scriptHash",
        "duration",
        "source",
        "estimated",
        "confidence",
        "words",
        "sentences",
        "alignerVersion",
        "fromCache",
    ] {
        assert!(first.get(key).is_some(), "no `{key}`: {first}");
    }
    assert_eq!(first["fromCache"], false);
    assert_eq!(first["source"], "aligned");
    assert_eq!(first["estimated"], false);
    assert_eq!(first["words"].as_array().unwrap().len(), 60);
    assert_eq!(first["audio"]["filename"], "intro.wav");
    assert_eq!(records(&dir), 1, "stored");
    let again = words(&dir, &[]);
    assert_eq!(again["fromCache"], true);
    assert_eq!(again["words"], first["words"]);

    // The text table: 1-based, the sentences after.
    let text = promo(&["words", dir.to_str().unwrap()]);
    assert!(text.starts_with("aligned to the audio"), "{text}");
    assert!(text.contains("   1     0.35"), "{text}");
    assert!(text.contains("sentences"), "{text}");

    // --timeline: through the layer that plays it, 2 s in.
    let mapped = words(&dir, &["--timeline"]);
    assert_eq!(mapped["timeline"]["layer"], "vo");
    let shifted = mapped["timeline"]["words"][0]["start"].as_f64().unwrap();
    assert!((shifted - first["words"][0]["start"].as_f64().unwrap() - 2.0).abs() < 1e-6);

    // An audio file with its script: aligned, not stored, no fromCache.
    let file = words_file(&dir.join("Resources/intro.wav"), &script);
    assert_eq!(file["source"], "aligned");
    assert!(file.get("fromCache").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

fn words_file(file: &Path, script: &str) -> Value {
    serde_json::from_str(&promo(&[
        "words",
        file.to_str().unwrap(),
        "--script",
        script,
        "--json",
    ]))
    .unwrap()
}

/// Provider (and local, transcribed) timing is never replaced but by a new
/// take: `--refresh` refuses it; an aligned record realigns.
#[test]
fn cache_is_authoritative_for_provider_records() {
    let (_, script) = synthetic();
    let dir = folder("authority", &script);
    let file = dir.join("Resources/intro.wav");
    let mut provider = promo_speech::estimate_words(&script, 25.0, None, 1.0);
    provider.source = promo_model::speech::TimingSource::Provider("elevenlabs".into());
    provider.estimated = false;
    provider.confidence = 1.0;
    provider.aligner_version = None;
    promo_model::word_cache::store(&dir, &file, provider).unwrap();
    let kept = words(&dir, &[]);
    assert_eq!(kept["source"], "provider:elevenlabs");
    assert_eq!(kept["fromCache"], true);
    let err = fail(&["words", dir.to_str().unwrap(), "--refresh"]);
    assert!(err.contains("provider timing is authoritative"), "{err}");
    // Opening the project leaves it as it is.
    promo(&["validate", dir.to_str().unwrap()]);
    assert_eq!(words(&dir, &[])["source"], "provider:elevenlabs");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record an older aligner made is aligned again — on the open and on
/// `promo words` — and `--refresh` aligns an aligned record again.
#[test]
fn older_aligner_records_are_realigned() {
    let (_, script) = synthetic();
    let dir = folder("older", &script);
    let file = dir.join("Resources/intro.wav");
    let mut old = promo_speech::estimate_words(&script, 25.0, None, 1.0);
    old.source = promo_model::speech::TimingSource::Aligned;
    old.estimated = false;
    old.aligner_version = Some(0);
    promo_model::word_cache::store(&dir, &file, old).unwrap();
    let fresh = words(&dir, &[]);
    assert_eq!(fresh["alignerVersion"], promo_media::words::ALIGNER_VERSION);
    assert_eq!(fresh["fromCache"], false);
    assert!(
        fresh["notes"].to_string().contains("aligned again"),
        "{fresh}"
    );
    assert_eq!(
        records(&dir),
        1,
        "the new record replaced the old under one key"
    );
    let refreshed = words(&dir, &["--refresh"]);
    assert_eq!(refreshed["fromCache"], false);
    assert_eq!(refreshed["words"], fresh["words"], "deterministic");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The CLI's open aligns every take with no timing and stores it, as it
/// measures a beat clock's song; `--no-analyse` reads the cache only.
#[test]
fn the_open_aligns_takes_with_no_timing() {
    let (_, script) = synthetic();
    let dir = folder("open", &script);
    promo(&["validate", dir.to_str().unwrap(), "--no-analyse"]);
    assert_eq!(records(&dir), 0, "cache only");
    let estimate = words(&dir, &["--no-analyse"]);
    assert_eq!(estimate["source"], "estimate");
    assert_eq!(estimate["fromCache"], false);
    assert_eq!(records(&dir), 0, "an estimate is not stored");
    promo(&["validate", dir.to_str().unwrap()]);
    assert_eq!(records(&dir), 1, "the open aligned and stored it");
    assert_eq!(words(&dir, &[])["fromCache"], true);

    // An edited script with no new take: the old record, stale.
    write_project(&dir, "Something else entirely now.");
    let stale = words(&dir, &[]);
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["script"], script);
    // A record whose audio no file has goes with --prune.
    std::fs::write(dir.join("Resources/intro.wav"), b"RIFF re-made").unwrap();
    assert!(promo(&["words", dir.to_str().unwrap(), "--prune"]).contains("removed 1"));
    assert_eq!(records(&dir), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Transcription spends the person's key, so it happens only when asked:
/// `--check` says what it would cost and spends nothing; no key is a
/// refusal that says where a key goes; nothing else ever transcribes.
#[test]
fn transcribe_is_never_automatic() {
    let (_, script) = synthetic();
    let dir = folder("transcribe", &script);
    let checked = promo(&[
        "words",
        dir.to_str().unwrap(),
        "--transcribe",
        "openai",
        "--check",
    ]);
    assert!(checked.contains("would cost about $0.00"), "{checked}");
    assert!(checked.contains("nothing was spent"), "{checked}");
    assert_eq!(records(&dir), 0, "a check stores nothing");
    // This machine has no key headlessly (the keyring is not linked into
    // the CLI): the refusal names where one goes, and nothing is stored.
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(["words", dir.to_str().unwrap(), "--transcribe", "elevenlabs"])
        .env("ELEVENLABS_API_KEY_FILE", dir.join("no-such-key"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no key for elevenlabs"));
    // The open, a plain ask and a refresh are offline: aligned, never
    // transcribed.
    promo(&["validate", dir.to_str().unwrap()]);
    assert_eq!(words(&dir, &["--refresh"])["source"], "aligned");
    assert!(fail(&["words", dir.to_str().unwrap(), "--check"]).contains("--transcribe"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn promo_words_names_what_it_needs() {
    let (_, script) = synthetic();
    let dir = folder("needs", &script);
    let bare = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    assert!(fail(&["words", bare.to_str().unwrap()]).contains("needs its script"));
    assert!(
        fail(&["words", dir.to_str().unwrap(), "--resource", "nope"])
            .contains("no resource `nope`")
    );
    assert!(fail(&["words", dir.to_str().unwrap(), "--script", "x"]).contains("its own script"));
    assert!(
        fail(&["words", dir.to_str().unwrap(), "--transcribe", "google"])
            .contains("openai or elevenlabs")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
