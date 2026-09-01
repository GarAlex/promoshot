//! Narration, shared by the headless server and (over the FFI) the apps:
//! the providers spoken to, the receipt that keeps unchanged text from
//! being bought twice, the stress spellings converted at the wire, the
//! voice rosters — and where the PERSON'S OWN key comes from.
//!
//! Keys: the OS keyring first (macOS Keychain, the Secret Service on
//! Linux, the Credential Manager on Windows — service `promoshot`, one
//! entry per provider), then the environment (`OPENAI_API_KEY`,
//! `ELEVENLABS_API_KEY`, `GOOGLE_API_KEY`) for a container or a CI box
//! that has no keyring. A key travels in a request header, never a URL;
//! nothing here stores or logs one. Without a key an agent CANNOT
//! narrate: the honest fallback is a recorded file dropped into
//! Resources/ and referenced like any audio.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub mod keys;
pub mod stress;
pub mod voices;

pub use keys::{
    FixedKeys, KeySource, KeyStatus, KeyStore, SystemKeys, ENV_VARS, PROVIDERS, SERVICE,
};
pub use stress::{google_input, marking_stress};
pub use voices::{voices_with_key, Voice};

/// sha256("provider|voiceID|text"), lowercase hex — the Swift
/// `SpeechSpec.fingerprint`, exactly. A receipt written by either side
/// holds on the other.
pub fn fingerprint(provider: &str, voice: &str, text: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(format!("{provider}|{voice}|{text}").as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Something that turns words into audio bytes for a provider and voice.
pub trait Synth {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String>;
}

/// The live providers, spending the key a [`KeyStore`] answers with.
pub struct LiveSynth<K: KeyStore> {
    pub keys: K,
}

impl<K: KeyStore> Synth for LiveSynth<K> {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String> {
        let (key, _) = self
            .keys
            .key(provider)
            .ok_or_else(|| keys::missing_key_message(provider))?;
        synthesize_with_key(provider, voice, text, &key)
    }
}

/// One synthesis with an explicit key — the client itself, for a host
/// that keeps its own key store (the apps, over the FFI). Stress
/// spellings are converted here, at the wire, so the script stays what
/// the author typed and the receipt with it.
pub fn synthesize_with_key(
    provider: &str,
    voice: &str,
    text: &str,
    key: &str,
) -> Result<Vec<u8>, String> {
    let text = marking_stress(text);
    match provider {
        "openai" => post_bytes(
            "https://api.openai.com/v1/audio/speech",
            &[("Authorization", &format!("Bearer {key}"))],
            &json!({
                "model": "gpt-4o-mini-tts", "input": text,
                "voice": voice, "response_format": "mp3",
            }),
        ),
        "elevenlabs" => {
            let url = format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"
            );
            post_bytes(
                &url,
                &[("xi-api-key", key)],
                &json!({ "text": text, "model_id": "eleven_multilingual_v2" }),
            )
        }
        "google" => {
            let locale: String = voice.split('-').take(2).collect::<Vec<_>>().join("-");
            let body = json!({
                "input": google_input(&text),
                "voice": { "languageCode": locale, "name": voice },
                "audioConfig": { "audioEncoding": "MP3" },
            });
            let answer = post_bytes(
                "https://texttospeech.googleapis.com/v1/text:synthesize",
                &[("X-Goog-Api-Key", key)],
                &body,
            )?;
            let parsed: Value = serde_json::from_slice(&answer).map_err(|e| e.to_string())?;
            let base64 = parsed
                .get("audioContent")
                .and_then(Value::as_str)
                .ok_or("Google returned no audio content")?;
            base64_decode(base64)
        }
        other => Err(format!("provider `{other}` — openai, elevenlabs or google")),
    }
}

/// POSTs JSON, answers the body bytes. The error names the endpoint and
/// the provider's answer — never the request, which carried the key.
pub(crate) fn post_bytes(
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<Vec<u8>, String> {
    let mut request = ureq::post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| match e {
            ureq::Error::Status(code, response) => format!(
                "{url} answered {code}: {}",
                response.into_string().unwrap_or_default()
            ),
            other => other.to_string(),
        })?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// GETs JSON with headers, answers the parsed body.
pub(crate) fn get_json(url: &str, headers: &[(&str, &str)]) -> Result<Value, String> {
    let mut request = ureq::get(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.call().map_err(|e| match e {
        ureq::Error::Status(code, response) => format!(
            "{url} answered {code}: {}",
            response.into_string().unwrap_or_default()
        ),
        other => other.to_string(),
    })?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    // Standard alphabet, padding tolerated — small enough to own rather
    // than pull a crate for.
    let table: Vec<i16> = {
        let mut t = vec![-1i16; 256];
        for (i, c) in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
            .bytes()
            .enumerate()
        {
            t[c as usize] = i as i16;
        }
        t
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = table[byte as usize];
        if value < 0 {
            return Err("audio content is not base64".into());
        }
        accumulator = (accumulator << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Google's wire conversions, ported from the Swift and held to the same
// vectors: the SSML door, and the friendly stress spellings.

/// The receipt walk, the app's rule mirrored field for field: every
/// resource whose `speech.text` says something is settled — reused when
/// the receipt (`renderedHash`) still matches the fingerprint of
/// provider|voice|text and the file is there (reopening costs nothing,
/// editing the words spends again); a filename that is not one plain
/// component is refused BEFORE paying; a fresh take voids old trims; the
/// receipt is written back beside what it paid for. `measure` answers a
/// written file's duration in seconds (ffprobe in the server, a literal
/// in tests). Answers the report lines; the caller owns the document's
/// file.
pub fn settle(
    doc: &mut Value,
    resources_dir: &Path,
    synth: &dyn Synth,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(resources_dir).map_err(|e| e.to_string())?;
    let Some(resources) = doc.get_mut("resources").and_then(Value::as_array_mut) else {
        return Ok(Vec::new());
    };
    let mut report: Vec<String> = Vec::new();
    let mut spent = 0usize;
    for (index, resource) in resources.iter_mut().enumerate() {
        let Some(speech) = resource.get("speech") else {
            continue;
        };
        let words = speech
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if words.trim().is_empty() {
            continue;
        }
        let provider = speech
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("openai")
            .to_string();
        let voice = speech
            .get("voiceID")
            .and_then(Value::as_str)
            .unwrap_or("alloy")
            .to_string();
        let receipt = speech
            .get("renderedHash")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = resource
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("R{index}"));
        let filename = match resource.get("filename").and_then(Value::as_str) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => format!("narration-{id}.mp3"),
        };
        if filename.contains('/') || filename.contains('\\') || filename.starts_with('.') {
            return Err(format!(
                "{id}: filename \"{filename}\" must be a plain file name inside Resources/"
            ));
        }
        let destination = resources_dir.join(&filename);
        let print = fingerprint(&provider, &voice, &words);

        if receipt.as_deref() == Some(print.as_str()) && destination.exists() {
            if let Ok(seconds) = measure(&destination) {
                report.push(format!("{id}: reused ({seconds:.2}s) — receipt holds"));
                continue;
            }
        }

        let audio = synth.synthesize(&provider, &voice, &words)?;
        std::fs::write(&destination, &audio).map_err(|e| format!("{id}: write: {e}"))?;
        let seconds = measure(&destination)?;
        spent += 1;

        resource["filename"] = json!(filename);
        resource["duration"] = json!(seconds);
        resource["kind"] = json!("audio");
        if let Some(object) = resource.as_object_mut() {
            object.remove("trimStart");
            object.remove("trimEnd");
            object.remove("trimKeyframes");
        }
        resource["speech"]["renderedHash"] = json!(print);
        resource["speech"]["provider"] = json!(provider);
        resource["speech"]["voiceID"] = json!(voice);
        report.push(format!("{id}: generated ({seconds:.2}s)"));
    }
    if !report.is_empty() {
        report.push(format!(
            "{} narration(s), {spent} newly synthesized",
            report.len()
        ));
    }
    Ok(report)
}

/// English names for the language codes providers hand back, so a
/// person searching "russian" finds a voice whose roster says "ru" — the
/// Swift side asks Foundation; this table covers the codes the rosters
/// actually carry and falls back to the code itself.
pub fn language_name(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let base = lower.split(['-', '_']).next().unwrap_or(&lower);
    let table: BTreeMap<&str, &str> = [
        ("ar", "Arabic"),
        ("bg", "Bulgarian"),
        ("bn", "Bengali"),
        ("ca", "Catalan"),
        ("cs", "Czech"),
        ("da", "Danish"),
        ("de", "German"),
        ("el", "Greek"),
        ("en", "English"),
        ("es", "Spanish"),
        ("et", "Estonian"),
        ("fa", "Persian"),
        ("fi", "Finnish"),
        ("fil", "Filipino"),
        ("fr", "French"),
        ("gu", "Gujarati"),
        ("he", "Hebrew"),
        ("hi", "Hindi"),
        ("hr", "Croatian"),
        ("hu", "Hungarian"),
        ("id", "Indonesian"),
        ("is", "Icelandic"),
        ("it", "Italian"),
        ("ja", "Japanese"),
        ("kn", "Kannada"),
        ("ko", "Korean"),
        ("lt", "Lithuanian"),
        ("lv", "Latvian"),
        ("ml", "Malayalam"),
        ("mr", "Marathi"),
        ("ms", "Malay"),
        ("nb", "Norwegian"),
        ("nl", "Dutch"),
        ("no", "Norwegian"),
        ("pa", "Punjabi"),
        ("pl", "Polish"),
        ("pt", "Portuguese"),
        ("ro", "Romanian"),
        ("ru", "Russian"),
        ("sk", "Slovak"),
        ("sl", "Slovenian"),
        ("sr", "Serbian"),
        ("sv", "Swedish"),
        ("sw", "Swahili"),
        ("ta", "Tamil"),
        ("te", "Telugu"),
        ("th", "Thai"),
        ("tr", "Turkish"),
        ("uk", "Ukrainian"),
        ("ur", "Urdu"),
        ("vi", "Vietnamese"),
        ("yue", "Cantonese"),
        ("zh", "Chinese"),
        ("cmn", "Chinese"),
    ]
    .into_iter()
    .collect();
    table
        .get(base)
        .map(|s| s.to_string())
        .unwrap_or_else(|| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The receipt hash must be the Swift one, byte for byte — receipts
    /// written by the app hold here and vice versa. The vector is
    /// sha256("openai|alloy|Built for large worksheets").
    #[test]
    fn the_fingerprint_matches_the_apps() {
        assert_eq!(
            fingerprint("openai", "alloy", "Built for large worksheets"),
            "4192c5b5627c4125a718a4c8dd644730a409166b28c045cc35c6f0cc073cca3b"
        );
    }

    #[test]
    fn language_names_cover_the_rosters_and_fall_back_to_the_code() {
        assert_eq!(language_name("ru"), "Russian");
        assert_eq!(language_name("ru-RU"), "Russian");
        assert_eq!(language_name("EN"), "English");
        assert_eq!(language_name("xx"), "xx");
    }

    struct Fake;
    impl Synth for Fake {
        fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"ID3fake".to_vec())
        }
    }

    fn script() -> Value {
        json!({ "resources": [
            { "id": "V1", "kind": "audio", "filename": "voice.mp3", "displayName": "V",
              "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
              "trimStart": 0.5, "trimEnd": 1.0,
              "speech": { "text": "Hello there", "provider": "openai", "voiceID": "alloy" } },
            { "id": "Q", "kind": "caption", "filename": "", "displayName": "quiet", "addedAt": 0,
              "speech": { "text": "   " } }
        ] })
    }

    #[test]
    fn a_receipt_holds_and_unchanged_text_never_spends_twice() {
        let dir = std::env::temp_dir().join(format!("promo-speech-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resources = dir.join("Resources");
        let measure = |_: &Path| Ok(2.5);
        let mut doc = script();
        let first = settle(&mut doc, &resources, &Fake, &measure).unwrap();
        assert!(first[0].contains("generated"), "{first:?}");
        assert!(resources.join("voice.mp3").is_file());
        let v1 = &doc["resources"][0];
        assert_eq!(v1["duration"], json!(2.5));
        assert_eq!(v1["kind"], json!("audio"));
        assert!(
            v1.get("trimStart").is_none(),
            "a fresh take voids old trims"
        );
        assert_eq!(
            v1["speech"]["renderedHash"],
            json!(fingerprint("openai", "alloy", "Hello there"))
        );

        let second = settle(&mut doc, &resources, &Fake, &measure).unwrap();
        assert!(second[0].contains("reused"), "{second:?}");
        assert!(second.last().unwrap().contains("0 newly synthesized"));

        doc["resources"][0]["speech"]["text"] = json!("Hello again");
        let third = settle(&mut doc, &resources, &Fake, &measure).unwrap();
        assert!(
            third[0].contains("generated"),
            "editing the words spends again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_traversal_filename_is_refused_before_spending() {
        struct Never;
        impl Synth for Never {
            fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
                panic!("must not be asked")
            }
        }
        let dir = std::env::temp_dir().join(format!("promo-speech-refuse-{}", std::process::id()));
        let mut doc = script();
        doc["resources"][0]["filename"] = json!("../escape.mp3");
        let err = settle(&mut doc, &dir.join("Resources"), &Never, &|_| Ok(1.0)).unwrap_err();
        assert!(err.contains("plain file name"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
