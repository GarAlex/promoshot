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
    marking_stress_mapped(text).0
}

/// [`marking_stress`], and for each character of what it answers (a
/// Unicode scalar), the scalar index in `text` it came from: an
/// apostrophe becomes the acute one for one, a capital vowel becomes two
/// (the vowel and the acute), both from the capital. A provider's
/// character offsets go back to the script through it.
pub fn marking_stress_mapped(text: &str) -> (String, Vec<u32>) {
    if text.trim().to_lowercase().starts_with("<speak") {
        return (text.to_string(), (0..text.chars().count() as u32).collect());
    }
    let (apostrophes, first) = apostrophe_stress(text);
    let (out, second) = capital_stress(&apostrophes);
    let map = second.into_iter().map(|i| first[i as usize]).collect();
    (out, map)
}

fn is_stress_vowel(c: char) -> bool {
    STRESS_VOWELS.chars().any(|v| v == c)
}

fn apostrophe_stress(text: &str) -> (String, Vec<u32>) {
    let mut out = String::new();
    let mut map = Vec::new();
    let mut previous: Option<char> = None;
    for (index, character) in text.chars().enumerate() {
        map.push(index as u32);
        if (character == '\'' || character == '’') && previous.map(is_stress_vowel).unwrap_or(false)
        {
            out.push('\u{0301}');
            previous = Some(character);
            continue;
        }
        out.push(character);
        previous = Some(character);
    }
    (out, map)
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

fn capital_stress(text: &str) -> (String, Vec<u32>) {
    let mut out = String::new();
    let mut map: Vec<u32> = Vec::new();
    // The word's letters, each with its index in `text`.
    let mut word: Vec<(char, u32)> = Vec::new();
    let flush = |word: &mut Vec<(char, u32)>, out: &mut String, map: &mut Vec<u32>| {
        let uppers: Vec<usize> = word
            .iter()
            .enumerate()
            .filter(|(_, (c, _))| c.is_uppercase())
            .map(|(i, _)| i)
            .collect();
        let convertible = uppers.len() == 1
            && uppers[0] > 0
            && word.iter().all(|(c, _)| is_cyrillic(*c))
            && word[uppers[0]]
                .0
                .to_lowercase()
                .next()
                .map(is_stress_vowel)
                .unwrap_or(false);
        for (i, &(c, from)) in word.iter().enumerate() {
            if convertible && i == uppers[0] {
                for lower in c.to_lowercase() {
                    out.push(lower);
                    map.push(from);
                }
                out.push('\u{0301}');
                map.push(from);
            } else {
                out.push(c);
                map.push(from);
            }
        }
        word.clear();
    };
    for (index, character) in text.chars().enumerate() {
        if character.is_alphabetic() {
            word.push((character, index as u32));
        } else {
            flush(&mut word, &mut out, &mut map);
            out.push(character);
            map.push(index as u32);
        }
    }
    flush(&mut word, &mut out, &mut map);
    (out, map)
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

    /// The map's text is `marking_stress` exactly, over the same vectors,
    /// and each of its characters points at the script character it came
    /// from — the apostrophe at itself, both halves of a capital at it.
    #[test]
    fn stress_map_matches_marking_stress() {
        for text in [
            "Приложение стои'т три",
            "сто’ит",
            "don't touch l'été",
            "О'Брайен согласен",
            "Это стоИт денег",
            "зАмок и замОк",
            "Москва слезам не верит",
            "ВКонтакте",
            "iPhone стоит",
            "<speak>ай'ти</speak>",
            "",
        ] {
            let (mapped, map) = marking_stress_mapped(text);
            assert_eq!(mapped, marking_stress(text), "{text}");
            assert_eq!(map.len(), mapped.chars().count(), "{text}");
            // Monotonic, onto every script character.
            assert!(map.windows(2).all(|w| w[0] <= w[1]), "{text}");
            let n = text.chars().count() as u32;
            assert!((0..n).all(|i| map.contains(&i)), "{text}");
        }
        let (mapped, map) = marking_stress_mapped("стоИт и сто'ит");
        assert_eq!(mapped, "стои\u{0301}т и сто\u{0301}ит");
        assert_eq!(&map[..6], &[0, 1, 2, 3, 3, 4]);
        assert_eq!(&map[9..], &[8, 9, 10, 11, 12, 13]);
    }
}
