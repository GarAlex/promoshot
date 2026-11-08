//! Machine translation of a project's strings (core-private/docs/LOCALIZATION.md,
//! slice 9): what a translator is handed, and a batch asked of OpenAI with the
//! person's own key. Apple's on-device translator runs in the apps; both are
//! handed [`plain`]'s text, so the two never disagree about what was sent.
//!
//! Nothing here writes a project. A translation lands through
//! `setTranslation` with `by: "machine"`, which never overwrites a person's.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// What a translator is handed for one string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Plain {
    pub text: String,
    /// Every letter was a capital: put the answer back in capitals.
    pub upper: bool,
}

/// `text` as a translator reads it best: stress marks gone (стои'т, стоИт,
/// стои́т), stylized capitals back to plain case (акУлА, ДоБРЫй), and a
/// shouted line in sentence case. Translators read an odd capital as a name
/// or an acronym and leave the word untranslated or garble it; Apple's
/// turned a Barmaley line written that way into nonsense.
///
/// Deliberately narrow: a word written in capitals inside an ordinary line
/// keeps them (США, BAR-MA-LEI), as do brands written in camel case
/// (PromoShot, iPhone, YouTube, macOS, ВКонтакте). A line counts as shouted
/// when most of its words are capitals or stylized; then every such word is
/// lowered and the sentences start with a capital again.
pub fn plain(text: &str) -> Plain {
    plain_named(text, &HashMap::new())
}

/// [`plain`] for every string of a batch, keeping the capital of a name the
/// batch writes capitalized mid-sentence and never in lowercase: a shouted
/// БАРМАЛЕЙ comes out Бармалей, not бармалей, when another line has
/// "злодей Бармалей".
pub fn plain_all(texts: &[&str]) -> Vec<Plain> {
    let names = names(texts);
    texts.iter().map(|t| plain_named(t, &names)).collect()
}

/// Lowercase form → the form written mid-sentence.
fn names(texts: &[&str]) -> HashMap<String, String> {
    let mut titled = HashMap::new();
    let mut lowered = HashSet::new();
    for text in texts {
        walk(&unstressed(text), |word, at_start| {
            let low: String = word.iter().flat_map(|c| c.to_lowercase()).collect();
            if word.iter().all(|c| c.is_lowercase()) {
                lowered.insert(low);
            } else if !at_start
                && word.len() >= 2
                && word[0].is_uppercase()
                && word[1..].iter().all(|c| c.is_lowercase())
            {
                titled.insert(low, word.iter().collect::<String>());
            }
        });
    }
    titled.retain(|low, _| !lowered.contains(low));
    titled
}

/// Each word of `text` and whether it starts a sentence.
fn walk(text: &str, mut each: impl FnMut(&[char], bool)) {
    let mut word = Vec::new();
    let mut at_start = true;
    let mut started = true;
    for c in text.chars() {
        if c.is_alphabetic() {
            if word.is_empty() {
                started = at_start;
            }
            word.push(c);
            at_start = false;
            continue;
        }
        if !word.is_empty() {
            each(&word, started);
            word.clear();
        }
        if c.is_numeric() {
            at_start = false;
        } else if ".!?…".contains(c) {
            at_start = true;
        }
    }
    if !word.is_empty() {
        each(&word, started);
    }
}

fn plain_named(text: &str, names: &HashMap<String, String>) -> Plain {
    let text = unstressed(text);
    // Counted by what spaces separate, so БАР-МА-ЛЕЙ is one word.
    let mut counted = 0;
    let mut loud = 0;
    for token in text.split_whitespace() {
        let parts = words(token);
        if parts.iter().map(Vec::len).sum::<usize>() < 2 {
            continue;
        }
        counted += 1;
        if parts
            .iter()
            .any(|w| all_caps(w) || (stylized(w) && !stress_capital(w)))
        {
            loud += 1;
        }
    }
    let upper = text.chars().any(char::is_uppercase) && !text.chars().any(char::is_lowercase);
    let shouted = upper || (counted > 0 && loud * 2 > counted);

    let mut out = String::with_capacity(text.len());
    let mut word: Vec<char> = Vec::new();
    let flush = |word: &mut Vec<char>, out: &mut String| {
        let lowered = if shouted {
            all_caps(word) || word.iter().skip(1).any(|c| c.is_uppercase())
        } else {
            false
        };
        if lowered {
            let low: String = word.iter().flat_map(|c| c.to_lowercase()).collect();
            out.push_str(names.get(&low).unwrap_or(&low));
        } else if stylized(word) {
            out.push(word[0]);
            out.extend(word[1..].iter().flat_map(|c| c.to_lowercase()));
        } else {
            out.extend(word.iter());
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    let text = if shouted { sentence_case(&out) } else { out };
    Plain { text, upper }
}

/// The answer to a string [`plain`] marked `upper`, in capitals again.
pub fn recased(plain: &Plain, translated: &str) -> String {
    if plain.upper {
        translated.to_uppercase()
    } else {
        translated.to_string()
    }
}

const STRESS_VOWELS: &str = "аеёиоуыэюяіїє";

/// The combining acute, and an apostrophe right after a lowercase Cyrillic
/// vowel, removed: the spellings `stress::marking_stress` reads.
fn unstressed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<char> = None;
    for c in text.chars() {
        let after_vowel = previous.is_some_and(|p| STRESS_VOWELS.contains(p));
        previous = Some(c);
        if c == '\u{0301}' || ((c == '\'' || c == '’') && after_vowel) {
            continue;
        }
        out.push(c);
    }
    out
}

fn words(text: &str) -> Vec<Vec<char>> {
    let mut all = Vec::new();
    let mut word = Vec::new();
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else if !word.is_empty() {
            all.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        all.push(word);
    }
    all
}

fn all_caps(word: &[char]) -> bool {
    word.iter().any(|c| c.is_uppercase()) && !word.iter().any(|c| c.is_lowercase())
}

/// A word whose capitals are neither a name's, an acronym's nor a brand's:
/// a stress mark (стоИт), or capitals scattered for effect (акУлА, ДоБРЫй,
/// кАРАКУЛА).
fn stylized(word: &[char]) -> bool {
    if !word.iter().any(|c| c.is_lowercase())
        || word.iter().any(|c| !c.is_uppercase() && !c.is_lowercase())
    {
        return false;
    }
    stress_capital(word) || !camel(word)
}

/// The app's capital-stress rule: one capital, not the first letter, a
/// Cyrillic vowel, in an all-Cyrillic word.
fn stress_capital(word: &[char]) -> bool {
    let uppers: Vec<usize> = (0..word.len())
        .filter(|&i| word[i].is_uppercase())
        .collect();
    uppers.len() == 1
        && uppers[0] > 0
        && word.iter().all(|c| ('\u{0400}'..='\u{04FF}').contains(c))
        && word[uppers[0]]
            .to_lowercase()
            .next()
            .is_some_and(|c| STRESS_VOWELS.contains(c))
}

/// Capitals where names and brands put them: a first capital (or two, as in
/// ВКонтакте), a capital starting each later part of at least two letters
/// (PromoShot, iPhone, YouTube), and an acronym of up to three at the end
/// (macOS, OpenAI, ChatGPT).
fn camel(word: &[char]) -> bool {
    // Runs of one case: (uppercase, length).
    let mut runs: Vec<(bool, usize)> = Vec::new();
    for c in word {
        let upper = c.is_uppercase();
        match runs.last_mut() {
            Some((u, n)) if *u == upper => *n += 1,
            _ => runs.push((upper, 1)),
        }
    }
    let last = runs.len() - 1;
    runs.iter().enumerate().all(|(i, &(upper, n))| {
        if !upper {
            return true;
        }
        let next_lower = runs.get(i + 1).map_or(0, |&(_, m)| m);
        if i == last {
            // A trailing acronym; a lone trailing capital (молокО) is not one.
            return i == 0 || (2..=3).contains(&n);
        }
        if i == 0 {
            return n == 1 || (n == 2 && next_lower >= 2);
        }
        n == 1 && next_lower >= 2
    })
}

/// A capital at the start and after each sentence's end, past any opening
/// quote or bracket.
fn sentence_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_start = true;
    for c in text.chars() {
        if c.is_alphabetic() {
            if at_start {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            at_start = false;
            continue;
        }
        if c.is_numeric() {
            at_start = false;
        } else if ".!?…".contains(c) {
            at_start = true;
        }
        out.push(c);
    }
    out
}

/// One batch for a translator: what the apps hand over.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// BCP 47 codes.
    pub source: String,
    pub target: String,
    /// The languages' names, for the prompt ("Russian").
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub target_name: Option<String>,
    pub strings: Vec<Item>,
    /// What a person already wrote in the target language: the names and
    /// wording to keep.
    #[serde(default)]
    pub glossary: Vec<Example>,
    /// The OpenAI model; [`OPENAI_MODEL`] when absent.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Item {
    pub id: String,
    pub text: String,
    /// The strings report's key: `captionText`, `recipe`, `speech`, `svg:<node>`.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Example {
    pub source: String,
    pub translation: String,
}

/// A model that follows a schema and is cheap per character.
pub const OPENAI_MODEL: &str = "gpt-5-mini";

/// Strings per request, and the characters they may add up to: a long
/// project goes in several requests, each well inside any output limit.
const BATCH_STRINGS: usize = 60;
const BATCH_CHARS: usize = 6000;

fn kind(key: Option<&str>) -> &'static str {
    match key {
        Some("speech") => "narration",
        Some("recipe") => "title",
        Some(k) if k.starts_with("svg:") => "picture text",
        _ => "caption",
    }
}

fn named(code: &str, name: Option<&str>) -> String {
    match name {
        Some(name) if !name.is_empty() => format!("{name} ({code})"),
        _ => code.to_string(),
    }
}

/// The chat request for one batch. `strings` already [`plain`]ed.
pub fn openai_body(request: &Request) -> Value {
    let source = named(&request.source, request.source_name.as_deref());
    let target = named(&request.target, request.target_name.as_deref());
    let instructions = format!(
        "You translate the strings of a video project from {source} to {target}. Each string \
         is a caption (text on screen that must fit where it is), a title, text inside a \
         picture, or a narration (a script a voice reads aloud).\n\
         - Translate the meaning naturally. A narration must read as natural speech.\n\
         - Keep the same number of sentences as the source: narration timing is anchored to \
         sentences.\n\
         - Keep each translation about as long as its source. Captions and titles stay short.\n\
         - Give every name one spelling in {target} and use it in every string. Where the \
         glossary has a name or a phrase, use its translation.\n\
         - Leave product and brand names, URLs, numbers, emoji, and markup such as {{...}}, \
         <...> and [...] as they are. Keep line breaks.\n\
         - Answer every id exactly once, with the translation alone."
    );
    let strings: Vec<Value> = request
        .strings
        .iter()
        .map(|s| json!({ "id": s.id, "kind": kind(s.key.as_deref()), "text": s.text }))
        .collect();
    let mut input = json!({ "source": source, "target": target, "strings": strings });
    if !request.glossary.is_empty() {
        input["glossary"] = json!(request
            .glossary
            .iter()
            .map(|e| json!({ "source": e.source, "translation": e.translation }))
            .collect::<Vec<_>>());
    }
    json!({
        "model": request.model.as_deref().unwrap_or(OPENAI_MODEL),
        "messages": [
            { "role": "system", "content": instructions },
            { "role": "user", "content": input.to_string() },
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "translations",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "translations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "text": { "type": "string" },
                                },
                                "required": ["id", "text"],
                                "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["translations"],
                    "additionalProperties": false,
                },
            },
        },
    })
}

/// The translations in an answer, in the order of `ids`. Every id must be
/// answered: a half-translated batch is refused, not written.
pub fn parse_openai(answer: &Value, ids: &[String]) -> Result<Vec<String>, String> {
    let message = &answer["choices"][0]["message"];
    if let Some(refusal) = message["refusal"].as_str() {
        return Err(format!("OpenAI declined to translate: {refusal}"));
    }
    let content = message["content"]
        .as_str()
        .ok_or("OpenAI's answer has no content")?;
    let parsed: Value = serde_json::from_str(content)
        .map_err(|e| format!("OpenAI's answer is not the JSON asked for: {e}"))?;
    let mut by_id = std::collections::HashMap::new();
    for row in parsed["translations"].as_array().into_iter().flatten() {
        if let (Some(id), Some(text)) = (row["id"].as_str(), row["text"].as_str()) {
            by_id.insert(id.to_string(), text.to_string());
        }
    }
    let answered: Vec<String> = ids.iter().filter_map(|id| by_id.remove(id)).collect();
    if answered.len() != ids.len() {
        return Err(format!(
            "OpenAI answered {} of {} strings",
            answered.len(),
            ids.len()
        ));
    }
    Ok(answered)
}

/// `request.strings` in batches small enough for one answer each.
fn batches(strings: &[Item]) -> Vec<&[Item]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = 0;
    for (i, item) in strings.iter().enumerate() {
        let n = item.text.chars().count();
        if i > start && (i - start >= BATCH_STRINGS || chars + n > BATCH_CHARS) {
            out.push(&strings[start..i]);
            start = i;
            chars = 0;
        }
        chars += n;
    }
    if start < strings.len() {
        out.push(&strings[start..]);
    }
    out
}

/// Every string of `request` translated by OpenAI with the person's key, in
/// order. The error names the endpoint and OpenAI's answer, never the key.
pub fn translate_openai(request: &Request, key: &str) -> Result<Vec<String>, String> {
    let texts: Vec<&str> = request.strings.iter().map(|s| s.text.as_str()).collect();
    let names = names(&texts);
    let mut out = Vec::with_capacity(request.strings.len());
    for batch in batches(&request.strings) {
        let plains: Vec<Plain> = batch.iter().map(|s| plain_named(&s.text, &names)).collect();
        let sent = Request {
            strings: batch
                .iter()
                .zip(&plains)
                .map(|(s, p)| Item {
                    text: p.text.clone(),
                    ..s.clone()
                })
                .collect(),
            ..request.clone()
        };
        let answer = crate::post_bytes(
            "https://api.openai.com/v1/chat/completions",
            &[("Authorization", &format!("Bearer {key}"))],
            &openai_body(&sent),
        )?;
        let answer: Value = serde_json::from_slice(&answer)
            .map_err(|e| format!("OpenAI's answer did not read: {e}"))?;
        let ids: Vec<String> = batch.iter().map(|s| s.id.clone()).collect();
        let texts = parse_openai(&answer, &ids)?;
        out.extend(plains.iter().zip(texts).map(|(p, t)| recased(p, &t)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> String {
        plain(text).text
    }

    #[test]
    fn ordinary_text_passes_untouched() {
        for text in [
            "Download PromoShot for iPhone and macOS today.",
            "Watch it on YouTube, ask OpenAI or ChatGPT.",
            "Москва, США и ВКонтакте.",
            "В Африке ужасный БАР-МА-ЛЕЙ!",
            "Hello there",
            "東京へようこそ",
        ] {
            assert_eq!(p(text), text);
        }
    }

    #[test]
    fn stress_marks_go() {
        assert_eq!(p("Это стоИт денег"), "Это стоит денег");
        assert_eq!(p("Это стои'т денег"), "Это стоит денег");
        assert_eq!(p("Это стои\u{0301}т денег"), "Это стоит денег");
        assert_eq!(p("молокО"), "молоко");
        // An apostrophe that is not after a vowel stays.
        assert_eq!(p("don't, О'Брайен"), "don't, О'Брайен");
    }

    #[test]
    fn stylized_capitals_go_back_to_plain_case() {
        assert_eq!(
            p("Подело́м тебе, АкулА, поделом!"),
            "Поделом тебе, Акула, поделом!"
        );
        assert_eq!(p("Добрый кАРАКУЛА пришёл"), "Добрый каракула пришёл");
    }

    #[test]
    fn a_shouted_line_is_lowered_into_sentences() {
        // A Barmaley line, capitals scattered for effect.
        let line = "\"НАм акУлА кАРАКУЛА НИПОЧЕМ, НИПОЧЕМ, МЫ аКулу КАРАКУЛУ Кирпичом, \
                    кирпичом!\" КаБЛУКОМ, КАБЛУком!";
        assert_eq!(
            p(line),
            "\"Нам акула каракула нипочем, нипочем, мы акулу каракулу Кирпичом, \
             кирпичом!\" Каблуком, каблуком!"
        );
        assert!(!plain(line).upper);
    }

    #[test]
    fn an_all_caps_string_comes_back_in_capitals() {
        let title = plain("КОНЕЦ");
        assert_eq!(
            title,
            Plain {
                text: "Конец".into(),
                upper: true
            }
        );
        assert_eq!(recased(&title, "The end"), "THE END");
        let ordinary = plain("Конец");
        assert_eq!(recased(&ordinary, "The end"), "The end");
    }

    #[test]
    fn a_shouted_name_keeps_its_capital_when_another_line_writes_it() {
        let texts = [
            "Он идёт в Африку, жадный Бармалей!",
            "ЛЮБИТ БАРМАЛЕЙ МАЛЕНЬКИХ ДЕТЕЙ, ЛЮБИТ ДЕТЕЙ!",
            "Не ходите, дети. Детям не надо.",
            "НЕ ХОДИТЕ В АФРИКУ, ДЕТИ!",
        ];
        let plains = plain_all(&texts);
        assert_eq!(
            plains[1].text,
            "Любит Бармалей маленьких детей, любит детей!"
        );
        // "Не" starts a sentence and "не" is written lowercase too: no name.
        assert_eq!(plains[3].text, "Не ходите в Африку, дети!");
    }

    fn request(n: usize) -> Request {
        Request {
            source: "ru".into(),
            target: "en".into(),
            source_name: Some("Russian".into()),
            target_name: Some("English".into()),
            strings: (0..n)
                .map(|i| Item {
                    id: format!("s{i}"),
                    text: format!("строка {i}"),
                    key: Some(if i == 0 { "speech" } else { "captionText" }.into()),
                })
                .collect(),
            glossary: vec![Example {
                source: "Бармалей".into(),
                translation: "Barmaley".into(),
            }],
            model: None,
        }
    }

    #[test]
    fn the_body_asks_for_every_id_against_a_schema() {
        let body = openai_body(&request(2));
        assert_eq!(body["model"], OPENAI_MODEL);
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Russian (ru) to English (en)"));
        assert!(system.contains("same number of sentences"));
        let input: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["strings"][0]["kind"], "narration");
        assert_eq!(input["strings"][1]["kind"], "caption");
        assert_eq!(input["strings"][1]["id"], "s1");
        assert_eq!(input["glossary"][0]["translation"], "Barmaley");
    }

    fn answer(rows: Value) -> Value {
        json!({ "choices": [{ "message": {
            "content": json!({ "translations": rows }).to_string(), "refusal": null } }] })
    }

    #[test]
    fn an_answer_reads_in_the_order_asked() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let rows = json!([{ "id": "b", "text": "two" }, { "id": "a", "text": "one" }]);
        assert_eq!(
            parse_openai(&answer(rows), &ids).unwrap(),
            vec!["one", "two"]
        );
    }

    #[test]
    fn a_half_answer_or_a_refusal_is_refused() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let half = parse_openai(&answer(json!([{ "id": "a", "text": "one" }])), &ids);
        assert_eq!(half.unwrap_err(), "OpenAI answered 1 of 2 strings");
        let refused = json!({ "choices": [{ "message": { "content": null, "refusal": "no" } }] });
        assert!(parse_openai(&refused, &ids)
            .unwrap_err()
            .contains("declined"));
        let garbled = json!({ "choices": [{ "message": { "content": "not json" } }] });
        assert!(parse_openai(&garbled, &ids).is_err());
    }

    #[test]
    fn a_long_project_goes_in_batches() {
        let r = request(130);
        let sizes: Vec<usize> = batches(&r.strings).iter().map(|b| b.len()).collect();
        assert_eq!(sizes, vec![60, 60, 10]);
        let long = vec![
            Item {
                id: "a".into(),
                text: "x".repeat(5000),
                key: None,
            },
            Item {
                id: "b".into(),
                text: "x".repeat(5000),
                key: None,
            },
        ];
        assert_eq!(batches(&long).len(), 2);
    }
}
