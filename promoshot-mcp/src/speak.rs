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
            return Ok(key_report(
                keys,
                promo_speech::PROVIDERS
                    .iter()
                    .map(|p| p.to_string())
                    .collect(),
            ));
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
            format!("blocked — no key for {}", missing.join(", "))
        };
        return Ok(format!(
            "{}\n{}: {} narration(s), {} settled, {} pending — {verdict}",
            key_report(keys, plan.providers()),
            dir.display(),
            plan.settled + plan.pending.len(),
            plan.settled,
            plan.pending.len()
        ));
    }
    if !missing.is_empty() {
        return Err(format!(
            "nothing was synthesized: {} narration(s) pending, but no key for {} — {}",
            plan.pending.len(),
            missing.join(", "),
            promo_speech::keys::missing_key_message(&missing[0])
        ));
    }

    let mut persist = |doc: &Value| {
        std::fs::write(&meta_path, doc.to_string()).map_err(|e| format!("write: {e}"))
    };
    let report = promo_speech::settle(&mut doc, &resources_dir, synth, measure, &mut persist)?;
    if report.is_empty() {
        return Ok("no narration scripts with text — nothing to do".into());
    }
    Ok(report.join("\n"))
}

/// Where each provider's key comes from — never the key. One line.
fn key_report(keys: &dyn promo_speech::KeyStore, providers: Vec<String>) -> String {
    if providers.is_empty() {
        return "narration keys: none needed".into();
    }
    let parts: Vec<String> = providers
        .iter()
        .map(|provider| match keys.key(provider) {
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

    /// `check: true` tells an agent whether narration is possible before
    /// it plans one — per provider, never the key — and a walk that
    /// would need a missing key refuses BEFORE buying anything.
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
            blocked.contains("1 pending — blocked — no key for openai"),
            "{blocked}"
        );
        let err = speak(
            &json!({ "project": dir.to_string_lossy() }),
            Some(&root),
            &Never,
            &no_keys,
            &|_| Ok(1.0),
        )
        .unwrap_err();
        assert!(err.starts_with("nothing was synthesized"), "{err}");
        assert!(err.contains("key set openai"), "{err}");
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
