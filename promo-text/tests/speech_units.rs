//! The speech tokenizer's units are the caption reveal's word units
//! (docs/SPEECH-SYNC.md §1.2): a caption synced to a take maps unit for
//! unit, so the two cuts must never disagree. Held over every caption the
//! repo's projects carry.

use std::path::{Path, PathBuf};

use promo_text::{reveal_layout, RevealBy, TextStyle};

fn metadata_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // A demo's runs/ are git-ignored agent output: walking them made
            // the sweep pass here and find a handful of projects on CI.
            if name != "target" && name != "runs" && !name.starts_with('.') {
                metadata_files(&path, out);
            }
        } else if name == "metadata.json"
            || (name.starts_with("reference") && name.ends_with(".json"))
        {
            out.push(path);
        }
    }
}

fn captions(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                if key == "captionText" {
                    if let Some(text) = v.as_str() {
                        out.push(text.to_string());
                    }
                }
                captions(v, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| captions(v, out)),
        _ => {}
    }
}

#[test]
fn tokenizer_words_are_reveal_units() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for dir in ["examples", "demos", "fixtures"] {
        metadata_files(&root.join(dir), &mut files);
    }
    let mut texts = Vec::new();
    for file in &files {
        if let Ok(value) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(file).unwrap())
        {
            captions(&value, &mut texts);
        }
    }
    texts.sort();
    texts.dedup();
    texts.retain(|t| !t.trim().is_empty());
    // Guard the guard: a walk that found nothing would pass by looking at
    // nothing.
    assert!(texts.len() > 50, "only {} captions found", texts.len());
    let style = TextStyle {
        font_size: 48.0,
        padding: 10.0,
        left_margin: 40.0,
        right_margin: 40.0,
        ..TextStyle::default()
    };
    let mut checked = 0;
    for text in &texts {
        let script = promo_model::speech::tokenize(text);
        let Some(layout) = reveal_layout(text, 1920.0, 1080.0, &style, RevealBy::Word) else {
            continue;
        };
        assert_eq!(
            layout.units.len(),
            script.units.len(),
            "{text:?}: reveal units {} vs tokenizer units {:?}",
            layout.units.len(),
            script.units
        );
        // Every word sits in a unit, in order, and inside its span.
        let mut last = 0;
        for word in &script.words {
            assert!(word.unit >= last, "{text:?}");
            last = word.unit;
            let unit = script.units[word.unit as usize];
            assert!(
                unit[0] <= word.chars[0] && word.chars[1] <= unit[1],
                "{text:?}"
            );
        }
        checked += 1;
    }
    assert!(checked > 50, "only {checked} captions laid out");
}

/// Scripts written without spaces: a word reveal walks real words, the
/// tokenizer's, or a synced reveal lights the wrong ones. No project in the
/// repo says any of these, so the sweep above cannot see them.
#[test]
fn a_reveal_by_word_walks_words_without_spaces() {
    let style = TextStyle {
        font_size: 48.0,
        padding: 10.0,
        left_margin: 40.0,
        right_margin: 40.0,
        ..TextStyle::default()
    };
    for text in [
        "東京のチームにも同じ画面が見えています。",
        "您在东京的团队也能看到。",
        "ทีมของคุณในโตเกียว",
        "iPadで見る「東京」",
    ] {
        let script = promo_model::speech::tokenize(text);
        let layout = reveal_layout(text, 1920.0, 1080.0, &style, RevealBy::Word).expect("layout");
        let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        let starts: Vec<usize> = script
            .units
            .iter()
            .map(|unit| offsets[unit[0] as usize])
            .collect();
        let got: Vec<usize> = layout.units.iter().map(|u| u.byte).collect();
        assert_eq!(got, starts, "{text}");
    }
}

/// One caption, one look: a machine with a Japanese or a Chinese family draws
/// all of a caption's Han and kana in it. Left to the shaper, 画 came out in a
/// thin serif in the middle of a bold sans line.
#[test]
fn a_caption_without_spaces_is_one_face() {
    let style = TextStyle {
        font_size: 48.0,
        bold: true,
        padding: 10.0,
        left_margin: 40.0,
        right_margin: 40.0,
        ..TextStyle::default()
    };
    for text in [
        "売上から注文まで、必要なものは画面に見えています。",
        "您在东京的团队也能看到同样的画面。",
    ] {
        // A machine with no such font (the Linux CI) says so: `undrawable`.
        if promo_text::undrawable(text).is_none() {
            let families = promo_text::families_drawing(text, &style);
            assert_eq!(families.len(), 1, "{text}: {families:?}");
        }
    }
    assert_eq!(promo_text::undrawable("plain English, with an é"), None);
}
