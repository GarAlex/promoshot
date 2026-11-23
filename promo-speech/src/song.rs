//! Songs: Google's Lyria through the Gemini API (`generateContent`), on
//! the person's Google key. The request is the one prompt
//! [`promo_model::speech::song_request`] writes — Lyria has no fields for
//! lyrics, style, length or language — and the answer carries the audio
//! inline, base64, beside a text part (the lyrics it sang, or the song's
//! structure). One request, one song, billed per song.

use serde_json::{json, Value};

use crate::{base64_decode, post_bytes, Take};

/// Where a model is asked.
pub fn endpoint(model: &str) -> String {
    format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent")
}

/// The request for `prompt`: audio and text back, MP3 (Lyria's default).
pub fn body(prompt: &str) -> Value {
    json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": { "responseModalities": ["AUDIO", "TEXT"] },
    })
}

/// The song in an answer: its audio bytes and the words that came with it.
/// Parts come in any order. An answer with no audio says why when it can
/// (a refused prompt names its reason), else that it held none.
pub fn parse(answer: &Value) -> Result<(Vec<u8>, String), String> {
    if let Some(reason) = answer
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(format!(
            "Lyria refused the prompt ({reason}) — it blocks artists' voices and existing lyrics"
        ));
    }
    let candidate = answer.pointer("/candidates/0");
    let parts = candidate
        .and_then(|c| c.pointer("/content/parts"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut audio: Option<Vec<u8>> = None;
    let mut words: Vec<&str> = Vec::new();
    for part in parts {
        let inline = part.get("inlineData").or_else(|| part.get("inline_data"));
        if let Some(data) = inline.and_then(|i| i.get("data")).and_then(Value::as_str) {
            if audio.is_none() {
                audio = Some(base64_decode(data)?);
            }
        } else if let Some(text) = part.get("text").and_then(Value::as_str) {
            words.push(text);
        }
    }
    match audio {
        Some(bytes) if !bytes.is_empty() => Ok((bytes, words.join("\n").trim().to_string())),
        _ => {
            let finish = candidate
                .and_then(|c| c.get("finishReason"))
                .and_then(Value::as_str)
                .map(|r| format!(" (finish reason {r})"))
                .unwrap_or_default();
            Err(format!("Lyria answered without audio{finish}"))
        }
    }
}

/// A refusal of the key itself, said in what to change: the Gemini API is
/// a separate API in the key's Google Cloud project, and Lyria has no free
/// tier.
pub fn explain(error: &str) -> String {
    let hint = if error.contains("SERVICE_DISABLED") || error.contains("has not been used") {
        Some("enable the Generative Language API in this key's Google Cloud project")
    } else if error.contains("API_KEY_SERVICE_BLOCKED") || error.contains("are blocked") {
        Some("this key is restricted to other APIs; allow the Generative Language API on it")
    } else if error.contains("API_KEY_INVALID") {
        Some("Google does not accept this key")
    } else if error.contains("answered 429") {
        Some("over quota, or the project has no billing — Lyria has no free tier")
    } else {
        None
    };
    match hint {
        Some(hint) => format!("{hint}: {error}"),
        None => error.to_string(),
    }
}

/// One song from `model`, asked `prompt`, on `key`. Lyria reports no word
/// times, so the take is aligned later, as an OpenAI take is.
pub fn generate(model: &str, prompt: &str, key: &str) -> Result<Take, String> {
    let bytes = post_bytes(&endpoint(model), &[("x-goog-api-key", key)], &body(prompt))
        .map_err(|e| explain(&e))?;
    let answer: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let (audio, sung) = parse(&answer)?;
    Ok(Take {
        audio,
        sung: Some(sung).filter(|s| !s.is_empty()),
        ..Take::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_the_prompt_with_audio_and_text_back() {
        let body = body("upbeat pop\nInstrumental only, no vocals.");
        assert_eq!(
            body.pointer("/contents/0/parts/0/text")
                .and_then(Value::as_str),
            Some("upbeat pop\nInstrumental only, no vocals.")
        );
        assert_eq!(
            body["generationConfig"]["responseModalities"],
            json!(["AUDIO", "TEXT"])
        );
        assert_eq!(
            endpoint("lyria-3.5"),
            "https://generativelanguage.googleapis.com/v1beta/models/lyria-3.5:generateContent"
        );
    }

    #[test]
    fn the_audio_is_found_whatever_order_the_parts_come_in() {
        // "ID3" base64: the start of an MP3.
        let answer = json!({ "candidates": [{ "content": { "parts": [
            { "text": "[Verse]\nla la" },
            { "inlineData": { "mimeType": "audio/mpeg", "data": "SUQz" } },
        ] }, "finishReason": "STOP" }] });
        let (audio, words) = parse(&answer).unwrap();
        assert_eq!(audio, b"ID3");
        assert_eq!(words, "[Verse]\nla la");
    }

    #[test]
    fn a_refusal_or_an_empty_answer_says_so() {
        let blocked = json!({ "promptFeedback": { "blockReason": "PROHIBITED_CONTENT" } });
        assert!(parse(&blocked).unwrap_err().contains("PROHIBITED_CONTENT"));
        let empty = json!({ "candidates": [{ "content": { "parts": [{ "text": "no" }] },
                                             "finishReason": "SAFETY" }] });
        let error = parse(&empty).unwrap_err();
        assert!(
            error.contains("without audio") && error.contains("SAFETY"),
            "{error}"
        );
    }

    #[test]
    fn a_refused_key_says_what_to_change() {
        let disabled = "https://… answered 403: {\"error\":{\"status\":\"PERMISSION_DENIED\",\"details\":[{\"reason\":\"SERVICE_DISABLED\"}]}}";
        assert!(explain(disabled).starts_with("enable the Generative Language API"));
        let blocked = "https://… answered 403: {\"error\":{\"details\":[{\"reason\":\"API_KEY_SERVICE_BLOCKED\"}]}}";
        assert!(explain(blocked).starts_with("this key is restricted"));
        assert_eq!(explain("timeout"), "timeout");
    }
}
