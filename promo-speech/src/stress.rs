//! The friendly stress spellings — стои'т and стоИт — converted to the
//! combining acute the dictionaries print, at the wire; and Google's SSML
//! door. Ported from the Swift, held by the same test vectors.

use serde_json::{json, Value};

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
}
