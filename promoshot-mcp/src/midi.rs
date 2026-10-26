//! `promo_midi`, headless: a MIDI-sequenced audio resource rendered and
//! settled with the installed sound bank (core-private docs/MIDI.md §3a).
//! The expansion, the render, the receipt and the settle are
//! `promo-midi`'s, shared with the apps over the FFI; this is the tool's
//! host half — the fence, the document's file, an imported `.mid`, the
//! bank's folder — and `promoshot-mcp sounds …`, the person's door to it.

use std::path::Path;

use serde_json::{json, Value};

/// The tool: optionally set the resource's block (`midi`) or import a
/// song (`file` / `midBase64`), then render and settle — or, with
/// `check`, only say whether the take is current.
pub fn midi(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .ok_or("`project` is required")?;
    let id = args
        .get("resource")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("`resource` is required — the id of the audio resource that holds the midi block")?;
    let dir = std::fs::canonicalize(project).map_err(|e| format!("project: {e}"))?;
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
    let resources = dir.join("Resources");
    if let Some(spec) = args.get("generate") {
        return generate(spec, &dir, id);
    }
    let check = args.get("check").and_then(Value::as_bool) == Some(true);
    if check {
        let found = promo_midi::settle::midi_resources(&doc)
            .iter()
            .any(|r| r == id);
        if !found {
            return Err(format!("no MIDI resource `{id}` in the project"));
        }
        let (bank_sha, bank_note) = match promo_midi::bank(None) {
            Ok(bank) => (bank.sha256.clone(), format!("bank: {}", bank.name)),
            Err(e) => (
                doc["resources"]
                    .as_array()
                    .and_then(|l| l.iter().find(|r| r["id"] == id))
                    .and_then(|r| r["midi"]["bank"]["sha256"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                e,
            ),
        };
        let why = promo_midi::stale(&doc, &resources, id, &bank_sha)?;
        return Ok(
            json!({ "resource": id, "current": why.is_none(), "why": why, "bank": bank_note })
                .to_string(),
        );
    }

    let before = doc.clone();
    let index = match doc["resources"]
        .as_array()
        .and_then(|l| l.iter().position(|r| r["id"] == id))
    {
        Some(index) => index,
        None => {
            if args.get("midi").is_none()
                && args.get("file").is_none()
                && args.get("midBase64").is_none()
            {
                return Err(format!(
                    "no resource `{id}` in the project — pass `midi` to create it"
                ));
            }
            if !doc["resources"].is_array() {
                doc["resources"] = json!([]);
            }
            let list = doc["resources"]
                .as_array_mut()
                .ok_or("resources is not a list")?;
            list.push(
                json!({ "id": id, "kind": "audio", "filename": "", "displayName": id,
                "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [] }),
            );
            list.len() - 1
        }
    };
    if let Some(block) = args.get("midi") {
        if !block.is_object() {
            return Err("`midi` must be an object — the resource's midi block".into());
        }
        doc["resources"][index]["midi"] = block.clone();
    }
    let bytes = match (
        args.get("file").and_then(Value::as_str),
        args.get("midBase64").and_then(Value::as_str),
    ) {
        (Some(_), Some(_)) => return Err("pass `file` or `midBase64`, not both".into()),
        (Some(path), None) => {
            let path = Path::new(path);
            let bytes = std::fs::read(path).map_err(|e| format!("file {}: {e}", path.display()))?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Imported.mid".into());
            Some((name, bytes))
        }
        (None, Some(text)) => Some((format!("{id}.mid"), base64(text)?)),
        (None, None) => None,
    };
    if let Some((name, bytes)) = bytes {
        let imported = promo_midi::import_mid(&resources, &name, &bytes)?;
        // The song becomes editable notes; what `midi` says on top wins.
        let mut block = imported.block();
        if let (Some(given), Some(map)) = (
            args.get("midi").and_then(Value::as_object),
            block.as_object_mut(),
        ) {
            for (k, v) in given {
                map.insert(k.clone(), v.clone());
            }
        }
        doc["resources"][index]["midi"] = block;
    }
    let bank = match promo_midi::bank(None) {
        Ok(bank) => bank,
        Err(e) => {
            if doc != before {
                promo_midi::settle::stamp(&mut doc);
                write(&meta_path, &doc)?;
                return Err(format!(
                    "{e}. The midi block is saved; promo_validate says \"MIDI not rendered\" until \
                     it is rendered."
                ));
            }
            return Err(e);
        }
    };
    let settled = promo_midi::settle_doc(&mut doc, &resources, id, &bank);
    // What was set or imported stays, rendered or not.
    if doc != before {
        promo_midi::settle::stamp(&mut doc);
        write(&meta_path, &doc)?;
    }
    Ok(settled?.to_json().to_string())
}

/// `generate: {track, caption?, seed?, range?: {from, to}, trackName?}`: a
/// generated layer from the installed music model (MIDI.md §7). Blocks
/// until it is made or refused; without the model it says so.
fn generate(spec: &Value, dir: &Path, id: &str) -> Result<String, String> {
    use promo_midi::generate::{self, Request};
    let track = spec
        .get("track")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or("`generate.track` names the layer: a new track, or a generated one to redo")?;
    let mut request = Request::new(track);
    request.caption = spec
        .get("caption")
        .and_then(Value::as_str)
        .map(str::to_string);
    request.seed = spec.get("seed").and_then(Value::as_i64);
    request.track_name = spec
        .get("trackName")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(range) = spec.get("range") {
        match (range["from"].as_f64(), range["to"].as_f64()) {
            (Some(from), Some(to)) => request.range = Some((from, to)),
            _ => {
                return Err(
                    "`generate.range` is {from, to} in bars (1-based, `to` exclusive)".into(),
                )
            }
        }
    }
    let bank = promo_midi::bank(None)?;
    let (made, settled) = generate::generate(
        dir,
        id,
        &request,
        generate::generator()?.as_ref(),
        &bank,
        &mut generate::Quiet,
    )?;
    Ok(json!({ "generated": made.to_json(), "result": settled.to_json() }).to_string())
}

fn write(path: &Path, doc: &Value) -> Result<(), String> {
    let text = serde_json::to_string(doc).map_err(|e| e.to_string())?;
    let temporary = path.with_file_name(format!(".metadata.json.partial-{}", std::process::id()));
    std::fs::write(&temporary, text).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("write: {e}"))
}

/// Standard base64 (padding optional, whitespace ignored).
fn base64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err("midBase64 is not base64".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// `promoshot-mcp sounds list | install <bank>` — the person's door to
/// the sound banks, like `voices` is to the voice packages.
pub fn sounds_command(args: &[String]) -> Result<String, String> {
    let usage = "usage: promoshot-mcp sounds list | install generaluser";
    let dir = promo_midi::sounds::sounds_dir();
    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("list") | None, None) => {
            let mut lines = vec![format!("sound banks in {}:", dir.display())];
            for bank in promo_midi::sounds::BANKS {
                let installed = std::fs::metadata(promo_midi::sounds::bank_path(&bank, &dir))
                    .is_ok_and(|m| m.len() == bank.bytes);
                lines.push(format!(
                    "  {:<12} {:<2} {:>4.0} MB  {}  {}",
                    bank.id,
                    if installed { "✓" } else { "-" },
                    bank.bytes as f64 / 1e6,
                    bank.title,
                    bank.license
                ));
            }
            lines.push("install: `promoshot-mcp sounds install generaluser`".into());
            Ok(lines.join("\n"))
        }
        (Some("install"), Some(id)) => {
            let mut shown = 0u64;
            let path = promo_midi::sounds::install(id, &dir, &mut |done, total| {
                let percent = (done * 100).checked_div(total).unwrap_or(0).min(100);
                if percent >= shown + 10 {
                    shown = percent;
                    eprintln!("{id}: {percent}% of {:.0} MB", total as f64 / 1e6);
                }
            })?;
            Ok(format!(
                "{id}: installed and verified at {}",
                path.display()
            ))
        }
        _ => Err(usage.into()),
    }
}

#[cfg(test)]
#[path = "../../promo-midi/tests/support/fake_lego.rs"]
mod fake_lego;

#[cfg(test)]
mod tests {
    use super::*;

    /// `generate` through the tool: a layer from the installed model, kept
    /// and mixed; a missing track is refused; no model, it says so.
    #[test]
    fn the_tool_generates_a_layer() {
        let dir = std::env::temp_dir().join(format!("mcp-midi-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let block = json!({ "tempo": 120, "bars": 2, "loop": { "from": 1, "to": 3 },
            "tracks": [{ "name": "Drums", "bank": "drums", "steps": { "resolution": 16,
                "length": 16, "lanes": [{ "note": 36, "on": [0, 4, 8, 12] }] } }] });
        std::fs::write(dir.join("metadata.json"), json!({ "resources": [{ "id": "M",
            "kind": "audio", "filename": "", "displayName": "Beat", "addedAt": 0, "midi": block }] })
            .to_string()).unwrap();
        let project = dir.to_string_lossy().into_owned();
        let missing = midi(
            &json!({ "project": project, "resource": "M", "generate": {} }),
            None,
        );
        assert!(missing.unwrap_err().contains("generate.track"));
        if promo_midi::sounds::installed_default().is_none() {
            eprintln!("SKIPPED: {}", promo_midi::sounds::missing_bank_message());
            return;
        }
        let ask = json!({ "project": project, "resource": "M",
            "generate": { "track": "Shaker", "caption": "shaker 16ths", "seed": 7 } });
        promo_midi::generate::set_generator(None);
        let absent = midi(&ask, None).unwrap_err();
        assert!(absent.contains("music model is not installed"), "{absent}");
        promo_midi::generate::set_generator(Some(std::sync::Arc::new(
            super::fake_lego::FakeLego::new(),
        )));
        let out = midi(&ask, None);
        promo_midi::generate::set_generator(None);
        let out: Value = serde_json::from_str(&out.unwrap()).unwrap();
        assert_eq!(out["generated"]["file"], "Beat · Shaker.wav", "{out}");
        assert_eq!(out["result"]["layers"][0]["status"], "current");
        assert!(dir.join("Resources/.midi/Beat · Shaker.wav").is_file());
        assert!(dir.join("Resources/Beat.wav").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("TVRoZA==").unwrap(), b"MThd");
        assert_eq!(base64("TVRoZA").unwrap(), b"MThd");
        assert!(base64("!!").is_err());
    }
}
