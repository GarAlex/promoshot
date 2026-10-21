//! Provider transcription (docs/SPEECH-SYNC.md §4.6): when a person asks
//! for it — never on an open, never on its own — a take's words timed by a
//! provider's recogniser, on the person's own key. Allowed because it is
//! opt-in: it spends money (whisper-1 about $0.006 a minute, Scribe about
//! $0.40 an hour), it is not deterministic, and sync never needs it.
//! What comes back is mapped onto the script by the same word alignment
//! as every other source (`timing::record`) and stored as
//! `transcribed:<provider>`, authoritative like provider timing.

use promo_model::speech::{SpokenWord, TimingSource};
use serde_json::Value;

use crate::timing::Spoken;

/// The providers that transcribe.
pub const TRANSCRIBERS: &[&str] = &["openai", "elevenlabs"];

/// What transcribing `seconds` of audio costs with `provider`, in USD, and
/// how that was worked out — said before anything is spent.
pub fn cost(provider: &str, seconds: f64) -> Result<(f64, String), String> {
    let minutes = seconds.max(0.0) / 60.0;
    match provider {
        "openai" => Ok((
            minutes * 0.006,
            format!("OpenAI whisper-1 at $0.006 a minute × {minutes:.2} min"),
        )),
        "elevenlabs" => Ok((
            minutes / 60.0 * 0.40,
            format!("ElevenLabs Scribe at $0.40 an hour × {minutes:.2} min"),
        )),
        other => Err(format!("transcription by `{other}` — openai or elevenlabs")),
    }
}

/// One multipart/form-data body: text fields, then the file. Answers the
/// content type (with its boundary) and the bytes.
pub fn multipart(fields: &[(&str, &str)], file: (&str, &str, &[u8])) -> (String, Vec<u8>) {
    // A boundary no field holds: fixed text plus the body's own hash.
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for (name, value) in fields {
        hasher.update(name.as_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(file.2);
    let digest: String = hasher.finalize()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let boundary = format!("promo-{digest}");
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    let (name, filename, bytes) = file;
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n",
            filename.replace('"', "")
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// OpenAI's request: whisper-1 (the model that returns word timestamps),
/// verbose JSON, word granularity, the script as the prompt so names are
/// spelled as written.
pub fn openai_request(audio: &[u8], filename: &str, script: &str) -> (String, Vec<u8>) {
    multipart(
        &[
            ("model", "whisper-1"),
            ("response_format", "verbose_json"),
            ("timestamp_granularities[]", "word"),
            ("prompt", script),
        ],
        ("file", filename, audio),
    )
}

/// ElevenLabs' request: Scribe.
pub fn elevenlabs_request(audio: &[u8], filename: &str) -> (String, Vec<u8>) {
    multipart(&[("model_id", "scribe_v1")], ("file", filename, audio))
}

/// The words of OpenAI's verbose answer: `words[] {word, start, end}`.
pub fn parse_openai(answer: &Value) -> Result<Vec<SpokenWord>, String> {
    let words = answer
        .get("words")
        .and_then(Value::as_array)
        .ok_or("OpenAI returned no words (was timestamp_granularities[]=word sent?)")?;
    words
        .iter()
        .map(|w| {
            Ok(SpokenWord {
                text: w
                    .get("word")
                    .and_then(Value::as_str)
                    .ok_or("a word with no text")?
                    .trim()
                    .to_string(),
                start: w
                    .get("start")
                    .and_then(Value::as_f64)
                    .ok_or("a word with no start")?,
                end: w.get("end").and_then(Value::as_f64),
                chars: None,
            })
        })
        .collect()
}

/// The words of ElevenLabs' answer: `words[] {text, start, end, type}`,
/// the `spacing` entries skipped.
pub fn parse_elevenlabs(answer: &Value) -> Result<Vec<SpokenWord>, String> {
    let words = answer
        .get("words")
        .and_then(Value::as_array)
        .ok_or("ElevenLabs returned no words")?;
    words
        .iter()
        .filter(|w| w.get("type").and_then(Value::as_str) != Some("spacing"))
        .map(|w| {
            Ok(SpokenWord {
                text: w
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("a word with no text")?
                    .trim()
                    .to_string(),
                start: w
                    .get("start")
                    .and_then(Value::as_f64)
                    .ok_or("a word with no start")?,
                end: w.get("end").and_then(Value::as_f64),
                chars: None,
            })
        })
        .collect()
}

/// What a transcription answer says, as a source the mapper reads.
pub fn spoken(provider: &str, answer: &Value, script: &str) -> Result<Spoken, String> {
    let words = match provider {
        "openai" => parse_openai(answer)?,
        "elevenlabs" => parse_elevenlabs(answer)?,
        other => return Err(format!("transcription by `{other}` — openai or elevenlabs")),
    };
    Ok(Spoken {
        words,
        source: TimingSource::Transcribed(provider.to_string()),
        confidence: 0.9,
        against: script.to_string(),
    })
}

/// One transcription with the person's key: the request goes out, the
/// answer comes back as a [`Spoken`] for `timing::record`. The key travels
/// in a header, never a URL; the error names the endpoint and the answer.
pub fn transcribe_with_key(
    provider: &str,
    audio: &[u8],
    filename: &str,
    script: &str,
    key: &str,
) -> Result<Spoken, String> {
    let (url, header, value, (content_type, body)) = match provider {
        "openai" => (
            "https://api.openai.com/v1/audio/transcriptions",
            "Authorization",
            format!("Bearer {key}"),
            openai_request(audio, filename, script),
        ),
        "elevenlabs" => (
            "https://api.elevenlabs.io/v1/speech-to-text",
            "xi-api-key",
            key.to_string(),
            elevenlabs_request(audio, filename),
        ),
        other => return Err(format!("transcription by `{other}` — openai or elevenlabs")),
    };
    let response = ureq::post(url)
        .set(header, &value)
        .set("Content-Type", &content_type)
        .send_bytes(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, response) => format!(
                "{url} answered {code}: {}",
                response.into_string().unwrap_or_default()
            ),
            other => other.to_string(),
        })?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))?;
    spoken(provider, &answer, script)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/speech")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Both answers map onto the script's words, the provider's own
    /// spellings ("Euclid's", "two thousand") merged and matched.
    #[test]
    fn transcriptions_map_onto_the_script() {
        let script = "Meet Euclid. Geometry, proved step by step.";
        for (provider, name) in [
            ("openai", "openai-transcription.json"),
            ("elevenlabs", "elevenlabs-scribe.json"),
        ] {
            let spoken = spoken(provider, &fixture(name), script).unwrap();
            assert_eq!(spoken.source.as_text(), format!("transcribed:{provider}"));
            let timing = crate::timing::record(script, &spoken, 3.7);
            let starts: Vec<f64> = timing.words.iter().map(|w| w.start).collect();
            assert_eq!(
                starts,
                [0.12, 0.42, 1.4, 2.16, 2.56, 2.84, 3.02],
                "{provider}"
            );
            assert!(timing.words.iter().all(|w| !w.interpolated), "{provider}");
        }
    }

    /// The request is what each API documents: whisper-1 with word
    /// timestamps and the script as the prompt; Scribe; the audio as the
    /// file field, the boundary absent from the parts.
    #[test]
    fn requests_are_multipart_as_documented() {
        let (kind, body) = openai_request(b"ID3audio", "vo.mp3", "Meet Euclid.");
        let boundary = kind.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"model\"\r\n\r\nwhisper-1\r\n"));
        assert!(text.contains("name=\"timestamp_granularities[]\"\r\n\r\nword\r\n"));
        assert!(text.contains("name=\"prompt\"\r\n\r\nMeet Euclid.\r\n"));
        assert!(text.contains("name=\"file\"; filename=\"vo.mp3\""));
        assert!(text.ends_with(&format!("\r\n--{boundary}--\r\n")));
        assert_eq!(text.matches(&format!("--{boundary}\r\n")).count(), 5);
        let (_, body) = elevenlabs_request(b"ID3", "vo.mp3");
        assert!(String::from_utf8_lossy(&body).contains("name=\"model_id\"\r\n\r\nscribe_v1"));
    }

    #[test]
    fn the_cost_is_said_before_anything_is_spent() {
        let (usd, how) = cost("openai", 90.0).unwrap();
        assert!((usd - 0.009).abs() < 1e-9, "{usd}");
        assert!(how.contains("$0.006 a minute"));
        let (usd, _) = cost("elevenlabs", 3600.0).unwrap();
        assert!((usd - 0.40).abs() < 1e-9);
        assert!(cost("google", 1.0).is_err());
    }
}
