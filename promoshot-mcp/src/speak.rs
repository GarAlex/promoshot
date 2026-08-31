//! `promo_speak`, headless: narration synthesized with the PERSON'S OWN
//! key, or not at all.
//!
//! The contract is the app's, mirrored field for field: walk the resources
//! whose `speech.text` says something; reuse when the receipt
//! (`renderedHash`) still matches the fingerprint of provider|voice|text —
//! reopening costs nothing, editing the words spends again; refuse a
//! filename that is not a plain component BEFORE paying; a fresh take
//! voids old trims; write the receipt back beside what it paid for. The
//! fingerprint is byte-identical to the Swift one, pinned by a vector.
//!
//! Keys come from the environment — OPENAI_API_KEY, ELEVENLABS_API_KEY,
//! GOOGLE_API_KEY — and an agent without one CANNOT narrate: the honest
//! fallback is a recorded file dropped into Resources/ and referenced like
//! any audio. Nothing here pretends otherwise.
//!
//! Google gets the same two conversions the app applies at the wire: the
//! SSML door (a script beginning with <speak>), and the friendly stress
//! spellings — стои'т and стоИт — converted to the combining acute. Ported
//! from the Swift, held by the same test vectors.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::Digest;

/// What a provider does: bytes of mp3 for money. Injected so the tests
/// spend nothing and count every call.
pub trait Synth {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String>;
}

/// sha256("provider|voiceID|text"), lowercase hex — the Swift
/// `SpeechSpec.fingerprint`, exactly.
pub fn fingerprint(provider: &str, voice: &str, text: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(format!("{provider}|{voice}|{text}").as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The whole tool: read the document raw (every unknown key preserved),
/// settle each narration, write it back. `measure` answers a written
/// file's duration in seconds (ffprobe in production, a literal in tests).
pub fn speak(
    args: &Value,
    root: Option<&Path>,
    synth: &dyn Synth,
    measure: &dyn Fn(&Path) -> Result<f64, String>,
) -> Result<String, String> {
    let dir = PathBuf::from(
        args.get("project")
            .and_then(Value::as_str)
            .ok_or("`project` is required")?,
    );
    let dir = std::fs::canonicalize(&dir).map_err(|e| format!("project: {e}"))?;
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
    let resources_dir = dir.join("Resources");
    std::fs::create_dir_all(&resources_dir).map_err(|e| e.to_string())?;

    let Some(resources) = doc.get_mut("resources").and_then(Value::as_array_mut) else {
        return Ok("no resources — nothing to narrate".into());
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
        // Paid-for-first protection, the app's rule verbatim: the filename
        // comes from authored JSON and must be one plain component.
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
        // A new take voids old trims: they index a recording that no
        // longer exists, and a stale trimEnd cut the tail off every longer
        // regeneration.
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

    if report.is_empty() {
        return Ok("no narration scripts with text — nothing to do".into());
    }
    std::fs::write(&meta_path, doc.to_string()).map_err(|e| format!("write: {e}"))?;
    report.push(format!(
        "{} narration(s), {spent} newly synthesized",
        report.len()
    ));
    Ok(report.join("\n"))
}

// ---------------------------------------------------------------------------
// The live providers: the person's own key, from the environment, spent on
// their behalf — in headers, never URLs, never logged.

pub struct LiveSynth;

impl Synth for LiveSynth {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String> {
        match provider {
            "openai" => {
                let key = env_key("OPENAI_API_KEY", provider)?;
                post_bytes(
                    "https://api.openai.com/v1/audio/speech",
                    &[("Authorization", &format!("Bearer {key}"))],
                    &json!({
                        "model": "gpt-4o-mini-tts", "input": text,
                        "voice": voice, "response_format": "mp3",
                    }),
                )
            }
            "elevenlabs" => {
                let key = env_key("ELEVENLABS_API_KEY", provider)?;
                let url = format!(
                    "https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"
                );
                post_bytes(
                    &url,
                    &[("xi-api-key", &key)],
                    &json!({ "text": text, "model_id": "eleven_multilingual_v2" }),
                )
            }
            "google" => {
                let key = env_key("GOOGLE_API_KEY", provider)?;
                let locale: String = voice.split('-').take(2).collect::<Vec<_>>().join("-");
                let spoken = google_input(text);
                let body = json!({
                    "input": spoken,
                    "voice": { "languageCode": locale, "name": voice },
                    "audioConfig": { "audioEncoding": "MP3" },
                });
                let answer = post_bytes(
                    "https://texttospeech.googleapis.com/v1/text:synthesize",
                    &[("X-Goog-Api-Key", &key)],
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
}

fn env_key(var: &str, provider: &str) -> Result<String, String> {
    std::env::var(var).map_err(|_| {
        format!(
            "no key for {provider}: set {var} in the server's environment. \
             Without a key an agent cannot narrate — record a voice file \
             into Resources/ and reference it instead."
        )
    })
}

fn post_bytes(url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Vec<u8>, String> {
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

/// A script that BEGINS with <speak> is SSML; anything else is text with
/// the stress spellings converted.
pub fn google_input(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.to_lowercase().starts_with("<speak") {
        json!({ "ssml": trimmed })
    } else {
        json!({ "text": marking_stress(text) })
    }
}

const STRESS_VOWELS: &str = "аеёиоуыэюяіїє";

/// The two friendly spellings of Russian stress — an apostrophe after the
/// vowel (стои'т) and the vowel capitalized (стоИт) — converted to the
/// combining acute services read. Both rules deliberately narrow, exactly
/// as the app's: don't, l'été, О'Брайен, Москва, США and ВКонтакте all
/// keep themselves.
pub fn marking_stress(text: &str) -> String {
    if text.trim().to_lowercase().starts_with("<speak") {
        return text.to_string();
    }
    capital_stress(&apostrophe_stress(text))
}

fn is_stress_vowel(c: char) -> bool {
    STRESS_VOWELS.chars().any(|v| v == c)
}

fn apostrophe_stress(text: &str) -> String {
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for character in text.chars() {
        if (character == '\'' || character == '’') && previous.map(is_stress_vowel).unwrap_or(false)
        {
            out.push('\u{0301}');
            previous = Some(character);
            continue;
        }
        out.push(character);
        previous = Some(character);
    }
    out
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

fn capital_stress(text: &str) -> String {
    let mut out = String::new();
    let mut word: Vec<char> = Vec::new();
    let flush = |word: &mut Vec<char>, out: &mut String| {
        let uppers: Vec<usize> = word
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_uppercase())
            .map(|(i, _)| i)
            .collect();
        let convertible = uppers.len() == 1
            && uppers[0] > 0
            && word.iter().all(|c| is_cyrillic(*c))
            && word[uppers[0]]
                .to_lowercase()
                .next()
                .map(is_stress_vowel)
                .unwrap_or(false);
        if convertible {
            let at = uppers[0];
            for c in &word[..at] {
                out.push(*c);
            }
            out.extend(word[at].to_lowercase());
            out.push('\u{0301}');
            for c in &word[at + 1..] {
                out.push(*c);
            }
        } else {
            for c in word.iter() {
                out.push(*c);
            }
        }
        word.clear();
    };
    for character in text.chars() {
        if character.is_alphabetic() {
            word.push(character);
        } else {
            flush(&mut word, &mut out);
            out.push(character);
        }
    }
    flush(&mut word, &mut out);
    out
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

    /// The stress conversions, held to the Swift tests' own vectors.
    #[test]
    fn the_stress_spellings_convert_and_the_innocent_survive() {
        assert_eq!(
            marking_stress("Приложение стои'т три"),
            "Приложение стои\u{0301}т три"
        );
        assert_eq!(marking_stress("сто’ит"), "сто\u{0301}ит");
        assert_eq!(marking_stress("don't touch l'été"), "don't touch l'été");
        assert_eq!(marking_stress("О'Брайен согласен"), "О'Брайен согласен");
        assert_eq!(marking_stress("Это стоИт денег"), "Это стои\u{0301}т денег");
        assert_eq!(
            marking_stress("зАмок и замОк"),
            "за\u{0301}мок и замо\u{0301}к"
        );
        for untouched in [
            "Москва слезам не верит",
            "США остаются США",
            "ВКонтакте",
            "ОКНА МОЮТ ВЕСНОЙ",
            "iPhone стоит",
        ] {
            assert_eq!(marking_stress(untouched), untouched);
        }
        let ssml = "<speak>ай'ти <sub alias='сто'>100</sub></speak>";
        assert_eq!(google_input(ssml), json!({ "ssml": ssml }));
    }

    struct Counting(std::cell::Cell<usize>);
    impl Synth for Counting {
        fn synthesize(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
            self.0.set(self.0.get() + 1);
            Ok(vec![0u8; 64])
        }
    }

    fn project_with_script(dir: &Path) {
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            serde_json::json!({
                "id": "P", "name": "n", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
                "compositionSettings": {"canvasWidth": 1280, "canvasHeight": 720},
                "resources": [{
                    "id": "voice", "kind": "audio", "filename": "",
                    "displayName": "Narration 1", "addedAt": 0,
                    "imageCuts": [], "disabledAudioTrackIndices": [],
                    "trimStart": 0.2, "trimEnd": 0.9,
                    "speech": {"text": "Built for large worksheets",
                                "provider": "openai", "voiceID": "alloy"}
                }],
                "layers": []
            })
            .to_string(),
        )
        .unwrap();
    }

    /// The receipt discipline, the trim voiding, and reuse-never-spends —
    /// the same promises the app's own speak tests pin.
    #[test]
    fn a_receipt_holds_and_unchanged_text_never_spends_twice() {
        let dir = std::env::temp_dir().join(format!("speak-{}", uuid::Uuid::new_v4()));
        project_with_script(&dir);
        let synth = Counting(std::cell::Cell::new(0));
        let measure = |_: &Path| Ok(0.5);

        let args = json!({ "project": dir.to_string_lossy() });
        let first = speak(&args, None, &synth, &measure).unwrap();
        assert!(first.contains("voice: generated"), "{first}");
        assert_eq!(synth.0.get(), 1);

        let text = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        let resource = &doc["resources"][0];
        assert_eq!(
            resource["speech"]["renderedHash"],
            json!("4192c5b5627c4125a718a4c8dd644730a409166b28c045cc35c6f0cc073cca3b"),
            "the receipt is the Swift fingerprint"
        );
        assert_eq!(resource["filename"], json!("narration-voice.mp3"));
        assert!(
            resource.get("trimStart").is_none(),
            "a new take voids trims"
        );
        assert!(dir.join("Resources/narration-voice.mp3").exists());

        let second = speak(&args, None, &synth, &measure).unwrap();
        assert!(second.contains("reused"), "{second}");
        assert_eq!(synth.0.get(), 1, "unchanged text must not spend again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The paid-for-first guard: a traversal filename is refused BEFORE any
    /// synthesis happens.
    #[test]
    fn a_traversal_filename_is_refused_before_spending() {
        let dir = std::env::temp_dir().join(format!("speak-{}", uuid::Uuid::new_v4()));
        project_with_script(&dir);
        let text = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
        let mut doc: Value = serde_json::from_str(&text).unwrap();
        doc["resources"][0]["filename"] = json!("../elsewhere/x.mp3");
        std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();

        let synth = Counting(std::cell::Cell::new(0));
        let err = speak(
            &json!({ "project": dir.to_string_lossy() }),
            None,
            &synth,
            &|_| Ok(0.5),
        )
        .unwrap_err();
        assert!(err.contains("plain file name"), "{err}");
        assert_eq!(synth.0.get(), 0, "refused before paying");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
