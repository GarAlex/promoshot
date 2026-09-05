//! The voice rosters, as the apps show them: id, name, a line of detail,
//! and every attribute the provider offers as label → value, so a person
//! can search "russian", "narration" or "professional" and find a voice.

use crate::{get_json, language_name};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// Provider-scoped id, e.g. "alloy"; qualified as "openai:alloy" when
    /// it travels in a project.
    pub id: String,
    pub name: String,
    pub language: Option<String>,
    pub detail: Option<String>,
    pub attributes: BTreeMap<String, String>,
}

impl Voice {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "language": self.language,
            "detail": self.detail,
            "attributes": self.attributes,
        })
    }
}

/// Can this provider's roster be answered without the person's key?
///
/// OpenAI's is a fixed list compiled into this binary — there is no
/// listing endpoint — so asking for a key first refused a question the
/// binary could already answer. It was the only tool in 600 logged calls
/// that failed every time it was called.
pub fn roster_needs_key(provider: &str) -> bool {
    provider != "openai"
}

/// OpenAI's fixed roster (no listing endpoint).
pub fn openai_roster() -> Vec<Voice> {
    [
        ("alloy", "Alloy", "Neutral, even"),
        ("ash", "Ash", "Warm, low"),
        ("coral", "Coral", "Bright"),
        ("echo", "Echo", "Calm"),
        ("fable", "Fable", "Expressive"),
        ("nova", "Nova", "Crisp, quick"),
        ("onyx", "Onyx", "Deep"),
        ("sage", "Sage", "Measured"),
        ("shimmer", "Shimmer", "Light"),
    ]
    .iter()
    .map(|(id, name, detail)| Voice {
        id: id.to_string(),
        name: name.to_string(),
        language: Some("en".into()),
        detail: Some(detail.to_string()),
        attributes: BTreeMap::new(),
    })
    .collect()
}

/// A provider's roster with an explicit key.
pub fn voices_with_key(provider: &str, key: &str) -> Result<Vec<Voice>, String> {
    match provider {
        "openai" => Ok(openai_roster()),
        "elevenlabs" => {
            let answer = get_json(
                "https://api.elevenlabs.io/v1/voices",
                &[("xi-api-key", key)],
            )
            .map_err(|e| {
                if e.contains("answered 401") {
                    // The commonest 401 here is not a wrong key — it is a
                    // RESTRICTED key scoped to Text to Speech only.
                    format!(
                        "{e} — if this key is restricted, it also needs the \"Voices: Read\" \
                             permission to list voices (Text to Speech alone is not enough)"
                    )
                } else {
                    e
                }
            })?;
            Ok(elevenlabs_voices(&answer))
        }
        "google" => {
            let answer = get_json(
                "https://texttospeech.googleapis.com/v1/voices",
                &[("X-Goog-Api-Key", key)],
            )?;
            Ok(google_voices(&answer))
        }
        other => Err(format!("provider `{other}` — openai, elevenlabs or google")),
    }
}

/// One ElevenLabs library entry into a Voice, metadata included: the
/// API's labels (accent, age, gender, use case), the category and the
/// verified languages all become searchable attributes.
pub fn elevenlabs_voices(answer: &Value) -> Vec<Voice> {
    let Some(entries) = answer.get("voices").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("voice_id")?.as_str()?.to_string();
            let name = entry.get("name")?.as_str()?.to_string();
            let labels = entry.get("labels").and_then(Value::as_object);
            let category = entry
                .get("category")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty());
            let detail = labels
                .and_then(|l| l.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| category.map(str::to_string));
            let mut attributes = BTreeMap::new();
            if let Some(labels) = labels {
                for (key, value) in labels {
                    if let Some(value) = value.as_str().filter(|v| !v.is_empty()) {
                        attributes.insert(capitalized(&key.replace('_', " ")), value.to_string());
                    }
                }
            }
            if let Some(category) = category {
                attributes.insert("Category".into(), category.to_string());
            }
            if let Some(verified) = entry.get("verified_languages").and_then(Value::as_array) {
                let mut names: Vec<String> = verified
                    .iter()
                    .filter_map(|item| item.get("language").and_then(Value::as_str))
                    .map(language_name)
                    .collect();
                names.sort();
                names.dedup();
                if !names.is_empty() {
                    attributes.insert("Languages".into(), names.join(", "));
                }
            }
            Some(Voice {
                id,
                name,
                language: None,
                detail,
                attributes,
            })
        })
        .collect()
}

/// One Google voice into a Voice. The name IS the id ("ru-RU-Wavenet-A"),
/// and it encodes the locale and the voice class — both become
/// attributes the free-text filter can find.
pub fn google_voices(answer: &Value) -> Vec<Voice> {
    let Some(entries) = answer.get("voices").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            let mut attributes = BTreeMap::new();
            if let Some(codes) = entry.get("languageCodes").and_then(Value::as_array) {
                let mut names: Vec<String> = codes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(language_name)
                    .collect();
                names.sort();
                names.dedup();
                if !names.is_empty() {
                    attributes.insert("Languages".into(), names.join(", "));
                }
            }
            if let Some(gender) = entry.get("ssmlGender").and_then(Value::as_str) {
                if gender != "SSML_VOICE_GENDER_UNSPECIFIED" {
                    attributes.insert("Gender".into(), gender.to_lowercase());
                }
            }
            let parts: Vec<&str> = name.split('-').collect();
            if parts.len() >= 3 {
                attributes.insert("Class".into(), parts[2..parts.len() - 1].join(" "));
            } else {
                // A voice named alone is a Gemini voice: served by a
                // Gemini TTS model through Vertex AI, which the Google
                // project has to have enabled — a 403 otherwise. The same
                // voice is also listed as `<locale>-Chirp3-HD-<Name>`,
                // which the Text-to-Speech API serves on its own; an agent
                // reading the roster should know which it is picking.
                attributes.insert(
                    "Class".into(),
                    "Gemini TTS (needs Vertex AI enabled)".into(),
                );
            }
            let detail: Vec<&str> = ["Class", "Languages"]
                .iter()
                .filter_map(|k| attributes.get(*k).map(String::as_str))
                .collect();
            Some(Voice {
                id: name.clone(),
                name,
                language: None,
                detail: (!detail.is_empty()).then(|| detail.join(", ")),
                attributes,
            })
        })
        .collect()
}

/// Swift `capitalized`: the first letter of every word up, the rest down.
fn capitalized(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_elevenlabs_roster_keeps_every_label_as_a_searchable_attribute() {
        let answer = json!({ "voices": [
            { "voice_id": "v1", "name": "Anna", "category": "professional",
              "labels": { "accent": "native", "use_case": "narration", "description": "warm", "age": "" },
              "verified_languages": [ { "language": "ru" }, { "language": "en" }, { "language": "ru" } ] },
            { "voice_id": "v2", "name": "Bare" },
            { "name": "no id" }
        ] });
        let voices = elevenlabs_voices(&answer);
        assert_eq!(voices.len(), 2);
        let anna = &voices[0];
        assert_eq!(anna.detail.as_deref(), Some("warm"));
        assert_eq!(
            anna.attributes.get("Accent").map(String::as_str),
            Some("native")
        );
        assert_eq!(
            anna.attributes.get("Use Case").map(String::as_str),
            Some("narration")
        );
        assert_eq!(
            anna.attributes.get("Category").map(String::as_str),
            Some("professional")
        );
        assert_eq!(
            anna.attributes.get("Languages").map(String::as_str),
            Some("English, Russian")
        );
        assert!(
            !anna.attributes.contains_key("Age"),
            "empty labels are dropped"
        );
        assert!(voices[1].attributes.is_empty() && voices[1].detail.is_none());
    }

    #[test]
    fn a_google_voice_names_its_locale_class_and_gender() {
        let answer = json!({ "voices": [
            { "name": "ru-RU-Wavenet-A", "languageCodes": ["ru-RU"], "ssmlGender": "FEMALE" },
            { "name": "en-US-Chirp3-HD-Aoede", "languageCodes": ["en-US"], "ssmlGender": "SSML_VOICE_GENDER_UNSPECIFIED" },
            { "name": "Aoede", "languageCodes": ["en-US", "de-DE"], "ssmlGender": "FEMALE" }
        ] });
        let voices = google_voices(&answer);
        assert_eq!(voices[0].id, "ru-RU-Wavenet-A");
        assert_eq!(
            voices[0].attributes.get("Class").map(String::as_str),
            Some("Wavenet")
        );
        assert_eq!(
            voices[0].attributes.get("Languages").map(String::as_str),
            Some("Russian")
        );
        assert_eq!(
            voices[0].attributes.get("Gender").map(String::as_str),
            Some("female")
        );
        assert_eq!(voices[0].detail.as_deref(), Some("Wavenet, Russian"));
        assert_eq!(
            voices[1].attributes.get("Class").map(String::as_str),
            Some("Chirp3 HD")
        );
        assert!(!voices[1].attributes.contains_key("Gender"));
        // The bare name is the Gemini form of the same voice, and says so.
        assert_eq!(voices[2].id, "Aoede");
        assert_eq!(
            voices[2].detail.as_deref(),
            Some("Gemini TTS (needs Vertex AI enabled), English, German")
        );
        assert_eq!(openai_roster().len(), 9);
        assert!(voices_with_key("nope", "k").is_err());
    }
}
