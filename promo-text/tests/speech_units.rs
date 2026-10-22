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
        // Scripts written without spaces reveal by character; their words
        // are characters, not units (§1.2).
        if text
            .chars()
            .any(|c| (0x3040..=0x9FFF).contains(&(c as u32)))
        {
            continue;
        }
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
