//! Narration, shared by the headless server and (over the FFI) the apps:
//! the providers spoken to, the receipt that keeps unchanged text from
//! being bought twice, the stress spellings converted at the wire, the
//! voice rosters — and where the PERSON'S OWN key comes from.
//!
//! Keys: the OS keyring first (macOS Keychain, the Secret Service on
//! Linux, the Credential Manager on Windows — service `promoshot`, one
//! entry per provider), then a secrets file (`OPENAI_API_KEY_FILE`, else
//! `/run/secrets/OPENAI_API_KEY`) for a container or a CI box that has
//! no keyring — never an environment variable holding the key. A key
//! travels in a request header, never a URL; nothing here stores or logs
//! one. Without a key an agent CANNOT
//! narrate: the honest fallback is a recorded file dropped into
//! Resources/ and referenced like any audio.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub mod keys;
pub mod stress;
pub mod voices;

pub use keys::{
    secrets_path, FixedKeys, KeySource, KeyStatus, KeyStore, SystemKeys, PROVIDERS, SECRETS,
    SERVICE,
};
pub use stress::{google_input, marking_stress};
pub use voices::{roster_needs_key, voices_with_key, Voice};

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
            let mut spoken = json!({
                "languageCode": google_locale(voice),
                "name": google_voice_name(voice),
            });
            if let Some(model) = google_model(voice) {
                spoken["modelName"] = json!(model);
            }
            let body = json!({
                "input": google_input(&text),
                "voice": spoken,
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

/// The `languageCode` a Google voice is asked for in.
///
/// Google's classic voices carry their locale in the name —
/// `en-US-Neural2-A`, `fr-FR-Wavenet-B` — and the first two segments are
/// it. Its Gemini voices are named alone (`Achernar`, `Kore`): they
/// speak any language, and the request still has to name one. Taking
/// "the first two segments" of a bare name handed the voice name over
/// as the language, which Google refuses ("Requested language code
/// 'Achernar' is not supported for Gemini voices"). A name with no
/// locale prefix is asked for in `en-US`, the roster's own default. A
/// voice may also state its locale explicitly as `Achernar@de-DE`.
pub fn google_locale(voice: &str) -> String {
    if let Some((_, locale)) = voice.split_once('@') {
        return locale.to_string();
    }
    let parts: Vec<&str> = voice.split('-').collect();
    let looks_like_locale = parts.len() >= 3
        && (2..=3).contains(&parts[0].len())
        && parts[0].chars().all(|c| c.is_ascii_lowercase())
        && (2..=3).contains(&parts[1].len())
        && parts[1]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if looks_like_locale {
        format!("{}-{}", parts[0], parts[1])
    } else {
        "en-US".to_string()
    }
}

/// The model a Google voice is spoken by, where one has to be named.
///
/// A Gemini voice — one named alone, with no locale prefix — is served
/// by a Gemini TTS model and Google refuses the request without one
/// ("This voice requires a model name to be specified"). The classic
/// voices carry their model in the name (`Neural2`, `Wavenet`) and take
/// none. Flash is the one named here: the same voices as Pro, a fraction
/// of the price, and the difference is not what a narration track shows.
pub fn google_model(voice: &str) -> Option<&'static str> {
    let name = google_voice_name(voice);
    (google_locale(name) == "en-US" && !name.contains('-')).then_some("gemini-2.5-flash-tts")
}

/// The voice NAME Google is sent: the id with any explicit locale
/// suffix removed.
pub fn google_voice_name(voice: &str) -> &str {
    voice.split_once('@').map_or(voice, |(name, _)| name)
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

/// One narration the walk would still have to buy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub provider: String,
}

/// What a walk over `doc` would find: how many narrations are already
/// settled (receipt holds and the file is there) and which are pending,
/// with their providers — so keys can be checked BEFORE anything is
/// bought, and a person can be told what a call would spend.
pub struct Plan {
    pub settled: usize,
    pub pending: Vec<Pending>,
}

impl Plan {
    /// The providers the pending narrations need, each once, in order.
    pub fn providers(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for p in &self.pending {
            if !seen.contains(&p.provider) {
                seen.push(p.provider.clone());
            }
        }
        seen
    }
}

/// A narration resource's words, provider, voice, receipt, id and file —
/// or None when it has nothing to say.
struct Script {
    id: String,
    words: String,
    provider: String,
    voice: String,
    receipt: Option<String>,
    filename: String,
}

fn script(index: usize, resource: &Value) -> Option<Script> {
    let speech = resource.get("speech")?;
    let words = speech
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if words.trim().is_empty() {
        return None;
    }
    let id = resource
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("R{index}"));
    let filename = match resource.get("filename").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!("narration-{id}.mp3"),
    };
    Some(Script {
        id,
        words,
        provider: speech
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("openai")
            .to_string(),
        voice: speech
            .get("voiceID")
            .and_then(Value::as_str)
            .unwrap_or("alloy")
            .to_string(),
        receipt: speech
            .get("renderedHash")
            .and_then(Value::as_str)
            .map(str::to_string),
        filename,
    })
}

fn is_settled(
    script: &Script,
    resources_dir: &Path,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
) -> Option<f64> {
    let destination = resources_dir.join(&script.filename);
    let print = fingerprint(&script.provider, &script.voice, &script.words);
    if script.receipt.as_deref() == Some(print.as_str()) && destination.exists() {
        return measure(&destination).ok();
    }
    None
}

/// The plan for `doc` without touching anything.
pub fn plan(
    doc: &Value,
    resources_dir: &Path,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
) -> Plan {
    let mut settled = 0;
    let mut pending = Vec::new();
    for (index, resource) in doc
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let Some(script) = script(index, resource) else {
            continue;
        };
        if is_settled(&script, resources_dir, measure).is_some() {
            settled += 1;
        } else {
            pending.push(Pending {
                id: script.id,
                provider: script.provider,
            });
        }
    }
    Plan { settled, pending }
}

/// The providers a plan needs that `keys` has no key for — checked BEFORE
/// a single request goes out, so a walk never buys two narrations and
/// then stops at a third for want of a key.
pub fn missing_keys(plan: &Plan, keys: &dyn KeyStore) -> Vec<String> {
    plan.providers()
        .into_iter()
        .filter(|provider| keys.key(provider).is_none())
        .collect()
}

/// The receipt walk, the app's rule mirrored field for field: every
/// resource whose `speech.text` says something is settled — reused when
/// the receipt (`renderedHash`) still matches the fingerprint of
/// provider|voice|text and the file is there (reopening costs nothing,
/// editing the words spends again); a filename that is not one plain
/// component is refused BEFORE paying; a fresh take voids old trims; the
/// receipt is written back beside what it paid for. `measure` answers a
/// written file's duration in seconds (ffprobe in the server, a literal
/// in tests). `persist` is called with the document after EVERY newly
/// bought narration — a receipt is worth money the moment it exists, and
/// a later failure must not lose it. Answers the report lines; on an
/// error, everything settled before it has already been persisted.
pub fn settle(
    doc: &mut Value,
    resources_dir: &Path,
    synth: &dyn Synth,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
    persist: &mut dyn FnMut(&Value) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(resources_dir).map_err(|e| e.to_string())?;
    let count = doc
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut report: Vec<String> = Vec::new();
    let mut spent = 0usize;
    for index in 0..count {
        let script = {
            let resource = &doc["resources"][index];
            match script(index, resource) {
                Some(script) => script,
                None => continue,
            }
        };
        let id = script.id.clone();
        if script.filename.contains('/')
            || script.filename.contains('\\')
            || script.filename.starts_with('.')
        {
            return Err(format!(
                "{id}: filename \"{}\" must be a plain file name inside Resources/",
                script.filename
            ));
        }
        if let Some(seconds) = is_settled(&script, resources_dir, measure) {
            report.push(format!("{id}: reused ({seconds:.2}s) — receipt holds"));
            continue;
        }
        let destination = resources_dir.join(&script.filename);
        let print = fingerprint(&script.provider, &script.voice, &script.words);
        let audio = synth.synthesize(&script.provider, &script.voice, &script.words)?;
        std::fs::write(&destination, &audio).map_err(|e| format!("{id}: write: {e}"))?;
        let seconds = measure(&destination)?;
        spent += 1;
        {
            let resource = &mut doc["resources"][index];
            resource["filename"] = json!(script.filename);
            resource["duration"] = json!(seconds);
            resource["kind"] = json!("audio");
            if let Some(object) = resource.as_object_mut() {
                object.remove("trimStart");
                object.remove("trimEnd");
                object.remove("trimKeyframes");
            }
            resource["speech"]["renderedHash"] = json!(print);
            resource["speech"]["provider"] = json!(script.provider);
            resource["speech"]["voiceID"] = json!(script.voice);
        }
        persist(doc)?;
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
    #[test]
    fn a_google_voice_is_asked_for_in_its_own_locale_or_english() {
        assert_eq!(google_locale("en-US-Neural2-A"), "en-US");
        assert_eq!(google_locale("fr-FR-Wavenet-B"), "fr-FR");
        assert_eq!(google_locale("cmn-CN-Standard-A"), "cmn-CN");
        // A Gemini voice is named alone and speaks any language: the
        // request names one rather than handing the voice name over.
        assert_eq!(google_locale("Achernar"), "en-US");
        assert_eq!(google_locale("Kore"), "en-US");
        assert_eq!(google_locale("Achernar@de-DE"), "de-DE");
        assert_eq!(google_voice_name("Achernar@de-DE"), "Achernar");
        assert_eq!(google_voice_name("en-US-Neural2-A"), "en-US-Neural2-A");
        // …and is served by a Gemini model, which the request must name;
        // a classic voice names its own in its name and takes none.
        assert_eq!(google_model("Achernar"), Some("gemini-2.5-flash-tts"));
        assert_eq!(google_model("Achernar@de-DE"), Some("gemini-2.5-flash-tts"));
        assert_eq!(google_model("en-US-Neural2-A"), None);
        assert_eq!(google_model("cmn-CN-Standard-A"), None);
    }

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
        let mut nop = |_: &Value| Ok(());
        let first = settle(&mut doc, &resources, &Fake, &measure, &mut nop).unwrap();
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

        let second = settle(&mut doc, &resources, &Fake, &measure, &mut nop).unwrap();
        assert!(second[0].contains("reused"), "{second:?}");
        assert!(second.last().unwrap().contains("0 newly synthesized"));

        doc["resources"][0]["speech"]["text"] = json!("Hello again");
        let third = settle(&mut doc, &resources, &Fake, &measure, &mut nop).unwrap();
        assert!(
            third[0].contains("generated"),
            "editing the words spends again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The plan names what a walk would buy, keys are checked against it
    /// before anything is bought, and every bought receipt is persisted
    /// at once — so a failure on the third narration cannot make the next
    /// call pay for the first two again.
    #[test]
    fn keys_are_checked_first_and_receipts_survive_a_failure_mid_walk() {
        let dir = std::env::temp_dir().join(format!("promo-speech-midwalk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resources = dir.join("Resources");
        let measure = |_: &Path| Ok(1.0);
        let mut doc = json!({ "resources": [
            { "id": "A", "kind": "audio", "filename": "a.mp3", "displayName": "a", "addedAt": 0,
              "speech": { "text": "first", "provider": "openai", "voiceID": "alloy" } },
            { "id": "B", "kind": "audio", "filename": "b.mp3", "displayName": "b", "addedAt": 0,
              "speech": { "text": "second", "provider": "elevenlabs", "voiceID": "v" } },
            { "id": "C", "kind": "audio", "filename": "c.mp3", "displayName": "c", "addedAt": 0,
              "speech": { "text": "third", "provider": "openai", "voiceID": "alloy" } }
        ] });
        let first = plan(&doc, &resources, &measure);
        assert_eq!(first.settled, 0);
        assert_eq!(
            first.providers(),
            vec!["openai".to_string(), "elevenlabs".to_string()]
        );
        let keys = FixedKeys(vec![("openai".into(), "k".into())]);
        assert_eq!(missing_keys(&first, &keys), vec!["elevenlabs".to_string()]);

        // A synth that refuses the second provider: the first receipt is
        // persisted before the refusal surfaces.
        struct Picky;
        impl Synth for Picky {
            fn synthesize(&self, provider: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
                if provider == "elevenlabs" {
                    Err("elevenlabs said no".into())
                } else {
                    Ok(b"ID3ok".to_vec())
                }
            }
        }
        let mut persisted: Vec<Value> = Vec::new();
        let err = settle(&mut doc, &resources, &Picky, &measure, &mut |d| {
            persisted.push(d.clone());
            Ok(())
        })
        .unwrap_err();
        assert!(err.contains("elevenlabs said no"), "{err}");
        assert_eq!(persisted.len(), 1, "the one bought narration was persisted");
        assert_eq!(
            persisted[0]["resources"][0]["speech"]["renderedHash"],
            json!(fingerprint("openai", "alloy", "first"))
        );
        // Re-planning finds A settled and only B and C pending.
        let again = plan(&doc, &resources, &measure);
        assert_eq!(again.settled, 1);
        assert_eq!(
            again
                .pending
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "C"]
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
        let err = settle(
            &mut doc,
            &dir.join("Resources"),
            &Never,
            &|_| Ok(1.0),
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.contains("plain file name"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
