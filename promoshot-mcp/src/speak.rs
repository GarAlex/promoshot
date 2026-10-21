//! `promo_speak` and `promo_voices`, headless: narration synthesized with
//! the PERSON'S OWN key, or not at all. The providers, the receipt walk,
//! the stress spellings and the rosters live in `promo-speech`, shared
//! with the apps over the FFI; this module is the tool's host half — the
//! project fence, the document's file, the duration probe — and the key
//! store: the OS keyring first, the environment behind it.

use serde_json::Value;
use std::path::Path;

pub use promo_speech::Synth;

/// The live providers over the system key store.
pub type LiveSynth = promo_speech::LiveSynth<promo_speech::SystemKeys>;

pub fn live() -> LiveSynth {
    promo_speech::LiveSynth {
        keys: promo_speech::SystemKeys,
    }
}

/// A walk's synth for one project: local takes read the project's own
/// pronunciations (`Resources/pronunciations.dict`); every other provider
/// goes to `inner` unchanged.
struct ProjectSynth<'a> {
    inner: &'a dyn Synth,
    lexicon: std::path::PathBuf,
}

impl Synth for ProjectSynth<'_> {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String> {
        if promo_speech::is_keyless(provider) {
            promo_speech::synthesize_local(voice, text, Some(&self.lexicon))
        } else {
            self.inner.synthesize(provider, voice, text)
        }
    }

    /// The provider's word timing passes through: without this the trait's
    /// default would drop it, and a bought take's timing is irreproducible.
    fn synthesize_timed(
        &self,
        provider: &str,
        voice: &str,
        text: &str,
    ) -> Result<promo_speech::Take, String> {
        if promo_speech::is_keyless(provider) {
            promo_speech::synthesize_local_timed(voice, text, Some(&self.lexicon))
        } else {
            self.inner.synthesize_timed(provider, voice, text)
        }
    }
}

/// The whole tool: read the document raw (every unknown key preserved),
/// check that every pending narration's provider has a key BEFORE a
/// request goes out, settle each narration writing the document back
/// after every one bought, and report. `measure` answers a written
/// file's duration in seconds (ffprobe in production, a literal in tests).
/// `check: true` spends nothing: it reports where each needed key comes
/// from (never the key) and what a real call would synthesize; with no
/// `project` it reports the keys alone.
pub fn speak(
    args: &Value,
    root: Option<&Path>,
    synth: &dyn Synth,
    keys: &dyn promo_speech::KeyStore,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
) -> Result<String, String> {
    let check = args.get("check").and_then(Value::as_bool) == Some(true);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        if check {
            let mut providers: Vec<String> = promo_speech::PROVIDERS
                .iter()
                .map(|p| p.to_string())
                .collect();
            providers.push(promo_speech::LOCAL.into());
            return Ok(key_report(keys, providers, &local_installed_note()));
        }
        return Err("`project` is required (or `check: true` alone, for the keys)".into());
    };
    let dir = std::fs::canonicalize(project).map_err(|e| format!("project: {e}"))?;
    if let Some(root) = root {
        let root = std::fs::canonicalize(root).map_err(|e| format!("--root: {e}"))?;
        if !dir.starts_with(&root) {
            return Err(format!(
                "project is outside the served root {}",
                root.display()
            ));
        }
    }
    let meta_path = dir.join("metadata.json");
    let text = std::fs::read_to_string(&meta_path)
        .map_err(|_| format!("no metadata.json in {}", dir.display()))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("decode: {e}"))?;
    if doc.get("resources").and_then(Value::as_array).is_none() {
        return Ok("no resources — nothing to narrate".into());
    }
    let resources_dir = dir.join("Resources");

    let plan = promo_speech::plan(&doc, &resources_dir, measure);
    let missing = promo_speech::missing_keys(&plan, keys);
    let blockers = promo_speech::local_blockers(&plan);
    // The first reason a provider cannot speak: a missing key, or for
    // `local` the voice package that is not installed.
    let why = |provider: &str| {
        if promo_speech::is_keyless(provider) {
            blockers.join("; ")
        } else {
            promo_speech::keys::missing_key_message(provider)
        }
    };
    if check {
        let verdict = if plan.pending.is_empty() {
            "nothing to synthesize".to_string()
        } else if missing.is_empty() {
            format!(
                "ready — would synthesize {} with {}",
                plan.pending.len(),
                plan.providers().join(", ")
            )
        } else {
            format!(
                "{} — those would get silent placeholders of their estimated length",
                blocked_phrase(&missing)
            )
        };
        return Ok(format!(
            "{}\n{}: {} narration(s), {} settled, {} pending — {verdict}",
            key_report(keys, plan.providers(), &blockers),
            dir.display(),
            plan.settled + plan.pending.len(),
            plan.settled,
            plan.pending.len()
        ));
    }
    // No key for a provider: unless asked not to, its narrations get silent
    // placeholders of their estimated length — the film times right today
    // and is voiced on the next call with a key.
    let placeholders = args.get("placeholder").and_then(Value::as_bool) != Some(false);
    if !missing.is_empty() && !placeholders {
        return Err(format!(
            "nothing was synthesized: {} narration(s) pending, but {} — {}",
            plan.pending.len(),
            blocked_phrase(&missing),
            why(&missing[0])
        ));
    }

    let mut persist = |doc: &Value| {
        std::fs::write(&meta_path, doc.to_string()).map_err(|e| format!("write: {e}"))
    };
    let mut report = promo_speech::placehold(&mut doc, &resources_dir, &missing, &mut persist)?;
    let synth = ProjectSynth {
        inner: synth,
        lexicon: resources_dir.join("pronunciations.dict"),
    };
    report.extend(promo_speech::settle_except(
        &mut doc,
        &resources_dir,
        &synth,
        measure,
        &mut persist,
        &missing,
    )?);
    if report.is_empty() {
        return Ok("no narration scripts with text — nothing to do".into());
    }
    let mut guesses = promo_speech::take_local_guesses();
    if !guesses.is_empty() {
        // Unsure guesses first: they are wrong most of the time.
        guesses.sort_by_key(|g| !g.unsure);
        let unsure = guesses.iter().filter(|g| g.unsure).count();
        report.push(format!(
            "guessed {} word(s) the dictionary lacks{} — listen, and pin or correct any in \
             Resources/pronunciations.dict (these lines, as is or edited), then promo_speak again:",
            guesses.len(),
            if unsure > 0 {
                format!(", {unsure} unsure (check those by ear first)")
            } else {
                String::new()
            }
        ));
        for g in guesses {
            let mark = if g.unsure {
                "   # unsure: check by ear"
            } else {
                ""
            };
            report.push(format!("  {}  {}{mark}", g.word.to_uppercase(), g.arpabet));
        }
    }
    if !missing.is_empty() {
        report.push(format!("silent until voiced: {}", why(&missing[0])));
    }
    Ok(report.join("\n"))
}

/// Where each provider's key comes from — never the key. One line.
fn key_report(
    keys: &dyn promo_speech::KeyStore,
    providers: Vec<String>,
    local_blockers: &[String],
) -> String {
    if providers.is_empty() {
        return "narration keys: none needed".into();
    }
    let parts: Vec<String> = providers
        .iter()
        .map(|provider| match keys.key(provider) {
            _ if promo_speech::is_keyless(provider) => {
                if local_blockers.is_empty() {
                    format!("{provider} — no key needed, voices installed")
                } else {
                    format!("{provider} — no key needed; {}", local_blockers.join("; "))
                }
            }
            Some((_, promo_speech::KeySource::Keyring)) => format!("{provider} — OS keyring"),
            Some((_, promo_speech::KeySource::SecretsFile)) => {
                format!("{provider} — secrets file")
            }
            Some((_, promo_speech::KeySource::Given)) => format!("{provider} — given"),
            None => format!("{provider} — NO KEY (`promoshot-mcp key set {provider}`)"),
        })
        .collect();
    format!("narration keys: {}", parts.join("; "))
}

/// "no key for openai, google; local voice not installed" — the providers
/// a walk cannot voice, keyed ones by their missing key.
fn blocked_phrase(missing: &[String]) -> String {
    let keyed: Vec<&str> = missing
        .iter()
        .map(String::as_str)
        .filter(|p| !promo_speech::is_keyless(p))
        .collect();
    let mut parts = Vec::new();
    if !keyed.is_empty() {
        parts.push(format!("no key for {}", keyed.join(", ")));
    }
    if missing.iter().any(|p| promo_speech::is_keyless(p)) {
        parts.push("local voice not installed".to_string());
    }
    parts.join("; ")
}

/// For a keys-only report: nothing to say when a local voice is
/// installed, else how to install one.
fn local_installed_note() -> Vec<String> {
    let dir = promo_speech::local::voices_dir();
    if promo_speech::local::catalog::installed(&dir).is_empty() {
        vec![format!(
            "no voice package installed in {} (`promoshot-mcp voices install kokoro-en-q8`)",
            dir.display()
        )]
    } else {
        Vec::new()
    }
}

/// `promoshot-mcp voices list|install|remove [package]` — the person's
/// door to the local voice packages, like `key` is to the keyring. An
/// install fetches every file from its pinned upstream URL and checks its
/// SHA-256 before the package appears; `PROMOSHOT_VOICES` picks the folder.
pub fn voices_command(args: &[String]) -> Result<String, String> {
    use promo_speech::local::{catalog, voices_dir};
    let usage = "usage: promoshot-mcp voices list | install <package> | remove <package>";
    let dir = voices_dir();
    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("list") | None, None) => {
            let installed = catalog::installed(&dir);
            let outdated = catalog::outdated(&dir);
            let mut lines = vec![format!("voice packages in {}:", dir.display())];
            for package in catalog::catalog() {
                lines.push(format!(
                    "  {:<16} {:<4} {:>4.0} MB  {}  {}",
                    package.id,
                    if outdated.contains(&package.id) {
                        "↑"
                    } else if installed.contains(&package.id) {
                        "✓"
                    } else {
                        "-"
                    },
                    package.download_bytes() as f64 / 1e6,
                    package.title,
                    package.license
                ));
            }
            if !outdated.is_empty() {
                lines.push(format!(
                    "↑ = installed, with new voices to add: `promoshot-mcp voices install {}`",
                    outdated.join(" / ")
                ));
            }
            lines.push("voices: `promo_voices` with provider local; install: `promoshot-mcp voices install <package>`".into());
            Ok(lines.join("\n"))
        }
        (Some("install"), Some(id)) => {
            let mirror = std::env::var("PROMOSHOT_VOICES_MIRROR").ok();
            let mut shown = 0u64;
            let path = catalog::install(
                id,
                &dir,
                mirror.as_deref().filter(|m| !m.is_empty()),
                &mut |done, total| {
                    let percent = (done * 100).checked_div(total).unwrap_or(0).min(100);
                    if percent >= shown + 10 {
                        shown = percent;
                        eprintln!("{id}: {percent}% of {:.0} MB", total as f64 / 1e6);
                    }
                },
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            Ok(format!(
                "{id}: installed and verified in {}",
                path.display()
            ))
        }
        (Some("remove"), Some(id)) => Ok(if catalog::remove(id, &dir)? {
            format!("{id}: removed")
        } else {
            format!("{id}: not installed")
        }),
        _ => Err(usage.into()),
    }
}

/// `promo_voices`: a provider's roster, one line per voice — id, name,
/// detail — with the person's key from the system store. Without one,
/// the honest refusal names how to register it.
pub fn voices(args: &Value, keys: &dyn promo_speech::KeyStore) -> Result<String, String> {
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    // A key only where the roster is a network call. OpenAI's is compiled
    // in, and demanding a key for it refused a question this binary could
    // already answer.
    let key = match keys.key(provider) {
        Some((key, _)) => key,
        None if promo_speech::roster_needs_key(provider) => {
            return Err(promo_speech::keys::missing_key_message(provider))
        }
        None => String::new(),
    };
    let voices = promo_speech::voices_with_key(provider, &key)?;
    if voices.is_empty() {
        return Ok(format!("{provider}: no voices listed"));
    }
    Ok(voices
        .iter()
        .map(|v| {
            let mut line = format!("{provider}:{}  {}", v.id, v.name);
            if let Some(detail) = &v.detail {
                line.push_str(&format!(" — {detail}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `promoshot-mcp key set|remove|status [provider]` — the person's door
/// to the keyring. `set` reads the key from stdin (a pipe or a paste and
/// Ctrl-D), so it never appears in a shell history or an argv; nothing
/// here prints one.
pub fn key_command(args: &[String], stdin: &mut dyn std::io::Read) -> Result<String, String> {
    let usage = "usage: promoshot-mcp key set <provider> | remove <provider> | status [provider]  (providers: openai, elevenlabs, google)";
    let action = args.first().map(String::as_str).ok_or(usage)?;
    let provider = args.get(1).map(String::as_str);
    match (action, provider) {
        ("set", Some(provider)) => {
            let mut key = String::new();
            stdin
                .read_to_string(&mut key)
                .map_err(|e| format!("reading the key from stdin: {e}"))?;
            promo_speech::keys::keyring_set(provider, &key)?;
            Ok(format!(
                "{provider}: key stored in the OS keyring (service `{}`)",
                promo_speech::SERVICE
            ))
        }
        ("remove", Some(provider)) => Ok(if promo_speech::keys::keyring_remove(provider)? {
            format!("{provider}: key removed from the OS keyring")
        } else {
            format!("{provider}: no key in the OS keyring")
        }),
        ("status", provider) => {
            let providers: Vec<&str> = match provider {
                Some(p) => vec![p],
                None => promo_speech::PROVIDERS.to_vec(),
            };
            let mut lines = Vec::new();
            for p in providers {
                let status = match promo_speech::keys::status(p)? {
                    promo_speech::KeyStatus::Keyring => "OS keyring".to_string(),
                    promo_speech::KeyStatus::SecretsFile(path) => {
                        format!("secrets file ({})", path.display())
                    }
                    promo_speech::KeyStatus::Missing => "none".to_string(),
                };
                lines.push(format!("{p}: {status}"));
            }
            Ok(lines.join("\n"))
        }
        _ => Err(usage.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_speech::{fingerprint, FixedKeys};
    use serde_json::json;

    struct Fake;
    impl Synth for Fake {
        fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"ID3fake".to_vec())
        }
    }

    fn project_with_script(dir: &Path) {
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            json!({ "id": "P", "name": "n", "createdAt": 0, "state": "recorded",
                    "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                    "compositionSettings": { "canvasWidth": 320, "canvasHeight": 180, "backgroundColorHex": "000000" },
                    "resources": [ { "id": "V1", "kind": "audio", "filename": "voice.mp3", "displayName": "V",
                        "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
                        "speech": { "text": "Hello there", "provider": "openai", "voiceID": "alloy" } } ],
                    "layers": [], "unknownTopLevel": true })
            .to_string(),
        )
        .unwrap();
    }

    /// The tool keeps the fence, the file and every unknown key; the walk
    /// itself is promo-speech's and tested there.
    #[test]
    fn the_tool_settles_the_document_and_keeps_unknown_keys() {
        let root = std::env::temp_dir().join(format!("mcp-speak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Talk.promo");
        project_with_script(&dir);
        let answer = speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &Fake,
            &FixedKeys(vec![("openai".into(), "k".into())]),
            &|_| Ok(1.5),
        )
        .unwrap();
        assert!(answer.contains("V1: generated (1.50s)"), "{answer}");
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(doc["unknownTopLevel"], json!(true));
        assert_eq!(
            doc["resources"][0]["speech"]["renderedHash"],
            json!(fingerprint("openai", "alloy", "Hello there"))
        );
        let outside =
            std::env::temp_dir().join(format!("mcp-speak-outside-{}", std::process::id()));
        project_with_script(&outside);
        let err = speak(
            &json!({ "project": outside.to_string_lossy() }),
            Some(&root),
            &Fake,
            &FixedKeys(vec![("openai".into(), "k".into())]),
            &|_| Ok(1.0),
        )
        .unwrap_err();
        assert!(err.contains("outside the served root"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A provider's word timing survives the tool's own synth wrapper and
    /// is in the project's cache when the tool answers: the committed
    /// ElevenLabs answer, through the walk the tool runs.
    #[test]
    fn the_tool_keeps_a_providers_word_timing() {
        struct Eleven;
        impl Synth for Eleven {
            fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
                Err("the timed door must be used".into())
            }
            fn synthesize_timed(
                &self,
                _: &str,
                _: &str,
                text: &str,
            ) -> Result<promo_speech::Take, String> {
                let answer = std::fs::read_to_string(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../fixtures/speech/elevenlabs-with-timestamps.json"),
                )
                .unwrap();
                promo_speech::timing::elevenlabs_take(text, &serde_json::from_str(&answer).unwrap())
            }
        }
        let root = std::env::temp_dir().join(format!("mcp-speak-timed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Timed.promo");
        project_with_script(&dir);
        let path = dir.join("metadata.json");
        let mut doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["resources"][0]["speech"] = json!({ "text": "Meet Euclid. Geometry, proved step by step.",
            "provider": "elevenlabs", "voiceID": "v" });
        std::fs::write(&path, doc.to_string()).unwrap();
        let answer = speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &Eleven,
            &FixedKeys(vec![("elevenlabs".into(), "k".into())]),
            &|_| Ok(3.7),
        )
        .unwrap();
        assert!(
            answer.contains("timed by provider:elevenlabs (7 words)"),
            "{answer}"
        );
        let records: Vec<_> = std::fs::read_dir(dir.join("Resources/.words"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(records.len(), 1);
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(records[0].path()).unwrap()).unwrap();
        assert_eq!(record["source"], "provider:elevenlabs");
        assert_eq!(record["words"][2]["start"], 1.4);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Seconds of a mono 16-bit WAV, from its header — the local takes'
    /// measure in these tests (ffprobe in production).
    fn wav_seconds(path: &Path) -> Result<f64, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as f64;
        let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as f64;
        Ok(data / 2.0 / rate)
    }

    /// The local voices these tests need, or None (and a note) when this
    /// machine has none installed: `PROMOSHOT_VOICES`, else the default
    /// folder, must hold kokoro-en-q8.
    fn local_voices() -> Option<std::path::PathBuf> {
        let dir = promo_speech::local::voices_dir();
        let ok = promo_speech::local::catalog::installed(&dir).contains(&"kokoro-en-q8");
        if !ok {
            eprintln!(
                "skipped: kokoro-en-q8 is not installed in {}",
                dir.display()
            );
        }
        ok.then_some(dir)
    }

    fn local_project(dir: &Path, text: &str) {
        project_with_script(dir);
        let path = dir.join("metadata.json");
        let mut doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["resources"][0]["filename"] = json!("voice.wav");
        doc["resources"][0]["speech"] =
            json!({ "text": text, "provider": "local", "voiceID": "kokoro-en-q8/af_heart" });
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    fn the_record(dir: &Path) -> Value {
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        let file = dir
            .join("Resources")
            .join(doc["resources"][0]["filename"].as_str().unwrap());
        let text = doc["resources"][0]["speech"]["text"].as_str().unwrap();
        let found = promo_model::word_cache::lookup(dir, &file, text).expect("a record");
        assert!(!found.stale);
        serde_json::to_value(found.timing).unwrap()
    }

    /// A local take is timed by its voice's own predictor (local-tts 0.7):
    /// the record is the take's, mapped onto the script — "$49.99" one
    /// script word over the six the voice said.
    #[test]
    fn local_take_writes_local_timing() {
        if local_voices().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-local-timed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Local.promo");
        local_project(&dir, "Save $49.99 today. Then relax.");
        let answer = speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &live(),
            &FixedKeys(vec![]),
            &wav_seconds,
        )
        .unwrap();
        assert!(
            answer.contains("timed by local:kokoro-en-q8/af_heart (5 words)"),
            "{answer}"
        );
        let record = the_record(&dir);
        assert_eq!(record["source"], "local:kokoro-en-q8/af_heart");
        assert_eq!(record["confidence"], 0.97);
        let words = record["words"].as_array().unwrap();
        assert_eq!(words[1]["text"], "$49.99");
        assert!(
            words.iter().all(|w| w.get("interpolated").is_none()),
            "{words:?}"
        );
        // "$49.99" spans the six words said for it: longer than "Save".
        let span = |w: &Value| w["end"].as_f64().unwrap() - w["start"].as_f64().unwrap();
        assert!(span(&words[1]) > 3.0 * span(&words[0]), "{words:?}");
        assert_eq!(record["sentences"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pronunciation edit is part of a local take's receipt: the take is
    /// re-voiced, so its bytes and its timing are new, and the old
    /// record goes with the old bytes.
    #[test]
    fn lexicon_edit_retimes_local_take() {
        if local_voices().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("mcp-local-lexicon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Lexicon.promo");
        local_project(&dir, "Meet Zorblat today.");
        let call = || {
            speak(
                &json!({ "project": dir.to_string_lossy() }),
                Some(&root),
                &live(),
                &FixedKeys(vec![]),
                &wav_seconds,
            )
            .unwrap()
        };
        call();
        let first = the_record(&dir);
        // A long, slow spelling of the name: the word after it moves.
        std::fs::write(
            dir.join("Resources/pronunciations.dict"),
            "ZORBLAT  Z AO1 R B L AE2 T AH0 N AH0 M AH0 R AH0\n",
        )
        .unwrap();
        let second_answer = call();
        assert!(second_answer.contains("V1: generated"), "{second_answer}");
        let second = the_record(&dir);
        assert_ne!(first["audio"]["sha256"], second["audio"]["sha256"]);
        let start = |r: &Value| r["words"][2]["start"].as_f64().unwrap();
        assert!(start(&second) > start(&first) + 0.2, "{first} {second}");
        let left: Vec<_> = std::fs::read_dir(dir.join("Resources/.words"))
            .unwrap()
            .collect();
        assert_eq!(left.len(), 1, "the old take's record went with its bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `check: true` tells an agent whether narration is possible before
    /// it plans one — per provider, never the key. With no key the walk
    /// spends nothing: it writes a silent placeholder of the estimated
    /// length (or, asked `placeholder: false`, refuses), and the next walk
    /// with a key voices it and clears the mark.
    #[test]
    fn check_answers_readiness_and_a_missing_key_refuses_before_spending() {
        let root = std::env::temp_dir().join(format!("mcp-speak-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Talk.promo");
        project_with_script(&dir);
        struct Never;
        impl Synth for Never {
            fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
                panic!("must not spend")
            }
        }
        let no_keys = FixedKeys(vec![]);
        let alone = speak(
            &json!({ "check": true }),
            Some(&root),
            &Never,
            &no_keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        assert!(
            alone.contains("openai — NO KEY (`promoshot-mcp key set openai`)"),
            "{alone}"
        );
        let blocked = speak(
            &json!({ "project": dir.to_string_lossy(), "check": true }),
            Some(&root),
            &Never,
            &no_keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        assert!(
            blocked.contains("1 pending — no key for openai — those would get silent placeholders"),
            "{blocked}"
        );
        let err = speak(
            &json!({ "project": dir.to_string_lossy(), "placeholder": false }),
            Some(&root),
            &Never,
            &no_keys,
            &|_| Ok(1.0),
        )
        .unwrap_err();
        assert!(err.starts_with("nothing was synthesized"), "{err}");
        assert!(err.contains("key set openai"), "{err}");
        // The default: a silent stand-in, nothing spent, still owed.
        let placed = speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &Never,
            &no_keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        assert!(placed.contains("V1: silent placeholder"), "{placed}");
        assert!(placed.contains("key set openai"), "{placed}");
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        let resource = &doc["resources"][0];
        assert_eq!(resource["speech"]["placeholder"], true);
        assert!(
            resource["speech"].get("renderedHash").is_none(),
            "no receipt: still owed"
        );
        let seconds = resource["duration"].as_f64().unwrap();
        assert!(
            seconds > 0.8 && seconds < 2.0,
            "\"Hello there\" ≈ 1.1 s, got {seconds}"
        );
        let bytes = std::fs::read(dir.join("Resources/voice.mp3")).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xFB], "a real (silent) mp3");
        let keys = FixedKeys(vec![("openai".into(), "sk-secret".into())]);
        let ready = speak(
            &json!({ "project": dir.to_string_lossy(), "check": true }),
            Some(&root),
            &Never,
            &keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        assert!(
            ready.contains("ready — would synthesize 1 with openai"),
            "{ready}"
        );
        assert!(!ready.contains("sk-secret"), "never the key");
        speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &Fake,
            &keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        let settled = speak(
            &json!({ "project": dir.to_string_lossy(), "check": true }),
            Some(&root),
            &Never,
            &keys,
            &|_| Ok(1.0),
        )
        .unwrap();
        assert!(
            settled.contains("1 settled, 0 pending — nothing to synthesize"),
            "{settled}"
        );
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert!(
            doc["resources"][0]["speech"].get("placeholder").is_none(),
            "the real take clears the mark"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Voices need a key and say how to register one; OpenAI's roster
    /// needs no network.
    #[test]
    fn voices_answer_the_roster_or_the_way_to_a_key() {
        let keys = FixedKeys(vec![("openai".into(), "sk-test".into())]);
        let answer = voices(&json!({}), &keys).unwrap();
        assert!(
            answer.lines().count() == 9 && answer.contains("openai:alloy  Alloy — Neutral, even"),
            "{answer}"
        );
        let err = voices(&json!({ "provider": "google" }), &keys).unwrap_err();
        assert!(err.contains("key set google"), "{err}");
    }

    #[test]
    fn the_key_command_reads_stdin_and_never_echoes_a_key() {
        let mut empty = std::io::Cursor::new("   \n");
        let err = key_command(&["set".into(), "openai".into()], &mut empty).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let mut none = std::io::empty();
        assert!(key_command(&["set".into()], &mut none)
            .unwrap_err()
            .contains("usage"));
        assert!(key_command(&["dance".into(), "openai".into()], &mut none)
            .unwrap_err()
            .contains("usage"));
        let status = key_command(&["status".into()], &mut none).unwrap();
        assert!(
            status.lines().count() == 3 && status.contains("openai:"),
            "{status}"
        );
    }
}
