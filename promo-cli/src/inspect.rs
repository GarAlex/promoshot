//! What a project is, in one answer (`promo inspect`, both servers'
//! `promo_inspect`). The CLI's own listing, which the apps re-implemented
//! in Swift — the folder, layers by kind, undeclared files, missing media,
//! unused resources, palette references — now answered once, here (review
//! 2026-09-27, P3-46).

use std::collections::{BTreeSet, HashSet};

use promo_model::ProjectMetadata;
use serde_json::Value;

use crate::project::{Project, Unsupported};

pub fn inspect(project: &Project, json: bool, decoders: bool) -> String {
    let layers = project.meta.layers.as_deref().unwrap_or(&[]);
    // The author's spellings the app kept when it minted UUIDs (`handles`,
    // minted id → spelling): shown beside each id, because every tool
    // takes either, and "deck" is what the person who wrote it remembers.
    let handles = project
        .meta
        .extra
        .get("handles")
        .and_then(serde_json::Value::as_object);
    let handle = |id: &str| {
        handles
            .and_then(|h| h.get(id))
            .and_then(serde_json::Value::as_str)
    };
    let (w, h) = (
        project.meta.composition_settings.canvas_width,
        project.meta.composition_settings.canvas_height,
    );
    let mut renderable = 0;
    let mut skipped: Vec<(&str, Unsupported)> = Vec::new();
    let mut missing: Vec<(&str, Unsupported)> = Vec::new();
    for layer in layers {
        match project.unsupported_with(layer, decoders) {
            None => renderable += 1,
            Some(why) if is_missing_media(&why) => missing.push((layer.name.as_str(), why)),
            Some(why) => skipped.push((layer.name.as_str(), why)),
        }
    }
    let undeclared = project.undeclared_resources();
    let mut kinds: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for layer in layers {
        *kinds
            .entry(word(serde_json::to_value(layer.kind), layer.kind))
            .or_default() += 1;
    }
    let unused = unused_resources(&project.meta);
    let palette = palette_use(&project.meta);
    // Nested compositions: what each holds, and who places it — the
    // layers naming it at rest, the swap keyframes that take it over
    // (rung 47: a video layer swapped to a composition, the takeover;
    // the same walk sees an image swapped in on an image layer), and
    // the material slots a body binds it to (rung 43: a screen that
    // plays a document). A composition on a device's Screen is named by
    // no layer, and "placed by 0" read as an unused resource to the
    // agent that authored the three-devices demo; the carousel demo's
    // scenes 2–4, reached only by swaps on its one card layer, read the
    // same way. The layer walk is every layer — top level, stage
    // members and nested. Bindings sit on the project's own resources
    // — a nested composition carries layers, not resources — so one
    // walk over them sees a slot on a body that a stage member or a
    // nested layer places.
    let resources = project.resources();
    let all_layers = promo_model::nesting::all_layers(&project.meta);
    let compositions: Vec<serde_json::Value> = resources
        .iter()
        .filter(|r| r.kind == promo_model::ProjectResourceKind::Composition)
        .map(|r| {
            let nested = r.composition.as_ref().map(|c| c.layers.len()).unwrap_or(0);
            let id = r.id.as_str();
            let placed_by: Vec<String> = all_layers
                .iter()
                .filter(|l| l.resource_id.as_deref() == Some(id))
                .map(|l| l.id.clone())
                .collect();
            // A takeover is a keyframe on whichever layer; the layer's id
            // and the keyframe's layer-local time are the handle
            // promo_upsert_keyframe takes, so that pair is what is listed.
            let taken_over_by: Vec<serde_json::Value> = all_layers
                .iter()
                .flat_map(|l| {
                    l.keyframes
                        .iter()
                        .filter(move |k| k.resource_id.as_deref() == Some(id))
                        .map(move |k| serde_json::json!({ "layer": l.id, "time": k.time }))
                })
                .collect();
            let bound_to: Vec<serde_json::Value> = resources
                .iter()
                .flat_map(|body| {
                    body.materials
                        .iter()
                        .flat_map(|slots| slots.iter())
                        .filter(|(_, binding)| binding.resource_id() == Some(r.id.as_str()))
                        .map(move |(slot, _)| {
                            serde_json::json!({ "resource": body.id, "slot": slot })
                        })
                })
                .collect();
            serde_json::json!({
                "id": r.id, "name": r.display_name, "duration": r.duration,
                "layers": nested, "placedBy": placed_by, "takenOverBy": taken_over_by,
                "boundTo": bound_to,
            })
        })
        .collect();
    if json {
        return serde_json::json!({
            "name": project.meta.name,
            "folder": project.dir.display().to_string(),
            "canvas": { "width": w, "height": h },
            "duration": project.duration(),
            "updated": project.meta.updated_at,
            "compositions": compositions,
            "layers": layers
                .iter()
                .map(|l| serde_json::json!({
                    "id": l.id, "name": l.name, "kind": l.kind,
                    "startTime": l.start_time, "duration": l.duration,
                    "handle": handle(&l.id),
                }))
                .collect::<Vec<_>>(),
            "resources": resources.len(),
            "undeclared": undeclared,
            "kinds": kinds,
            "renderable": renderable,
            "markers": project.meta.markers.as_deref().unwrap_or(&[]).iter().map(|m| serde_json::json!({
                "id": m.id, "time": m.time, "name": m.name, "kind": m.kind,
            })).collect::<Vec<_>>(),
            "skipped": skipped
                .iter()
                .map(|(name, why)| serde_json::json!({
                    "layer": name, "reason": why.to_string()
                }))
                .collect::<Vec<_>>(),
            "missingMedia": missing
                .iter()
                .map(|(name, why)| serde_json::json!({
                    "layer": name, "reason": why.to_string()
                }))
                .collect::<Vec<_>>(),
            "unused": unused
                .iter()
                .map(|u| serde_json::json!({
                    "id": u.id, "kind": u.kind, "name": u.name, "why": u.why,
                }))
                .collect::<Vec<_>>(),
            "palette": {
                "named": palette.named,
                "references": palette.references,
                "undefined": palette.undefined,
            },
        })
        .to_string();
    }
    let mut out = String::new();
    out.push_str(&format!("project:   {}\n", project.meta.name));
    out.push_str(&format!("folder:    {}\n", project.dir.display()));
    out.push_str(&format!("canvas:    {w:.0}x{h:.0}\n"));
    out.push_str(&format!("duration:  {:.2}s\n", project.duration()));
    // The turn signal (SPECS D5): a raw change marker, not a calendar date —
    // compare it with the last inspect to learn whether someone else edited.
    if let Some(stamp) = project.meta.updated_at {
        out.push_str(&format!("updated:   {stamp}\n"));
    }
    out.push_str(&format!("layers:    {}\n", layers.len()));
    if !kinds.is_empty() {
        let each: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
        out.push_str(&format!("kinds:     {}\n", each.join(", ")));
    }
    // Each layer with its ID — the handle promo_upsert_keyframe takes.
    // The trial that validated the keyframe tool (issue #1) had to read
    // metadata.json to learn a layer's id; this listing is the fix.
    for layer in layers {
        let end = layer
            .duration
            .map(|d| format!("{:.2}", layer.start_time + d))
            .unwrap_or_else(|| "…".into());
        let kind = serde_json::to_value(layer.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", layer.kind));
        let spelling = handle(&layer.id)
            .map(|h| format!("  ← {h}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {}  {kind}  {:.2}–{end}s  \"{}\"{spelling}\n",
            layer.id, layer.start_time, layer.name
        ));
    }
    if let Some(kept) = handles.filter(|h| !h.is_empty()) {
        out.push_str(&format!(
            "handles:   {} author ids kept — every tool takes either\n",
            kept.len()
        ));
    }
    // A file in Resources/ a layer names by its derived id renders as well
    // as a declared one: counted, and said to have been found, not missing.
    if undeclared > 0 {
        out.push_str(&format!(
            "resources: {} ({undeclared} undeclared, from Resources/)\n",
            resources.len()
        ));
    } else {
        out.push_str(&format!("resources: {}\n", resources.len()));
    }
    if !compositions.is_empty() {
        out.push_str(&format!("compositions: {}\n", compositions.len()));
        // Each way a composition is placed, said in its own words — the
        // layers naming it at rest; the swap keyframes that take it over,
        // by layer and time; the slots, by body and name — so "0 layers"
        // on a device's screen, or on a scene one card swaps to, does not
        // read as unused.
        fn each(
            list: &serde_json::Value,
            say: impl Fn(&serde_json::Value) -> String,
        ) -> Vec<String> {
            list.as_array()
                .map(|items| items.iter().map(say).collect())
                .unwrap_or_default()
        }
        for c in &compositions {
            let layers = c["placedBy"].as_array().map(|p| p.len()).unwrap_or(0);
            let mut placed = format!("placed by {layers} layer{}", plural(layers));
            let takeovers = each(&c["takenOverBy"], |k| {
                format!(
                    "{} {:.2}s",
                    k["layer"].as_str().unwrap_or(""),
                    k["time"].as_f64().unwrap_or(0.0)
                )
            });
            let slots = each(&c["boundTo"], |b| {
                format!(
                    "{} {}",
                    b["resource"].as_str().unwrap_or(""),
                    b["slot"].as_str().unwrap_or("")
                )
            });
            for (how, what, uses) in [
                ("taken over by", "keyframe", takeovers),
                ("on", "slot", slots),
            ] {
                if !uses.is_empty() {
                    placed.push_str(&format!(
                        ", {how} {} {what}{} ({})",
                        uses.len(),
                        plural(uses.len()),
                        uses.join(", ")
                    ));
                }
            }
            out.push_str(&format!(
                "  {}  \"{}\"  {:.2}s  {} layers  {placed}\n",
                c["id"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or(""),
                c["duration"].as_f64().unwrap_or(0.0),
                c["layers"],
            ));
        }
    }
    if let Some(markers) = project.meta.markers.as_deref().filter(|m| !m.is_empty()) {
        out.push_str(&format!("markers:   {}\n", markers.len()));
        for m in markers {
            let kind = serde_json::to_value(m.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            out.push_str(&format!("  {:.2}s  {kind}  \"{}\"\n", m.time, m.name));
        }
    }
    out.push_str(&format!("\nrenderable: {renderable} of {}", layers.len()));
    // What will draw nothing because its media is not there, by layer, so
    // the fix is named; then what this host cannot draw for other reasons.
    if !missing.is_empty() {
        out.push_str("\nmissing media:");
        for (name, why) in &missing {
            out.push_str(&format!("\n  - {name}: {why}"));
        }
    }
    if !skipped.is_empty() {
        out.push_str("\nskipped:");
        for (name, why) in &skipped {
            out.push_str(&format!("\n  - {name}: {why}"));
        }
    }
    if renderable == 0 && !layers.is_empty() {
        out.push_str(
            "\n\nNothing in this project renders yet. Video decoding on this platform\n\
             and text rasterization are the two gaps.",
        );
    }
    if !unused.is_empty() {
        out.push_str("\n\nunused resources:");
        for u in &unused {
            out.push_str(&format!("\n  - {} \"{}\": {}", u.kind, u.name, u.why));
        }
    }
    if palette.named > 0 || !palette.undefined.is_empty() {
        out.push_str(&format!(
            "\n\npalette: {} named, {} references",
            palette.named, palette.references
        ));
    }
    if !palette.undefined.is_empty() {
        out.push_str("\nundefined colours (these fall back to a default):");
        for name in &palette.undefined {
            out.push_str(&format!("\n  - @{name}"));
        }
    }
    out
}

/// "1 layer", "0 layers": the suffix a count takes.
fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// The wire word of an enum value (`image`, `video`, …), from its JSON.
fn word(json: serde_json::Result<Value>, fallback: impl std::fmt::Debug) -> String {
    json.ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{fallback:?}"))
}

/// A layer that draws nothing because its media is not there — the fix is
/// to supply it — as distinct from what this host cannot draw.
fn is_missing_media(why: &Unsupported) -> bool {
    match why {
        Unsupported::MissingFile(_) => true,
        Unsupported::MissingResource(why) => {
            why.contains("not in Resources/")
                || why.contains("names no resource")
                || why.contains("names no model resource")
                || why.contains("names no file")
        }
        _ => false,
    }
}

/// A declared resource nothing uses.
pub struct Unused {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub why: String,
}

/// Every id a pointer in the document names: a layer's or keyframe's
/// `resourceID`, any `…ResourceID` (mask, path, palette, LUT, a speech's
/// source), a theme plate, a slot binding, a morph's two bodies — found by
/// walking the whole file, so a use in a stage member, a nested layer, a
/// camera's route or the environment is a use (the Swift walk counted
/// neither the environment's panorama nor the theme plate).
fn pointed_at(node: &Value, parent: Option<&str>, out: &mut HashSet<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                let pointer = key == "resourceID"
                    || key.ends_with("ResourceID")
                    || key == "themePlateID"
                    || (parent == Some("morph") && (key == "from" || key == "to"));
                if pointer {
                    if let Some(id) = value.as_str() {
                        out.insert(id.to_string());
                    }
                }
                pointed_at(value, Some(key), out);
            }
        }
        Value::Array(items) => {
            for item in items {
                pointed_at(item, parent, out);
            }
        }
        _ => {}
    }
}

/// The declared resources nothing in the document points at.
pub fn unused_resources(meta: &ProjectMetadata) -> Vec<Unused> {
    let Ok(document) = serde_json::to_value(meta) else {
        return Vec::new();
    };
    let mut used = HashSet::new();
    pointed_at(&document, None, &mut used);
    use promo_model::ProjectResourceKind as Kind;
    meta.resources
        .iter()
        .flatten()
        .filter(|r| !used.contains(&r.id))
        .map(|r| {
            // What a material slot can wear: a picture, never a body or a
            // sound.
            let why = if r.extra.contains_key("speech") {
                "speech that nothing plays — add an audio layer pointing at it"
            } else if matches!(r.kind, Kind::Image | Kind::Video | Kind::Composition) {
                "not used by any layer or slot"
            } else {
                "not used by any layer"
            };
            Unused {
                id: r.id.clone(),
                kind: word(serde_json::to_value(r.kind), r.kind),
                name: r.display_name.clone(),
                why: why.into(),
            }
        })
        .collect()
}

/// How the document uses its palette: names defined, `@name` uses of them,
/// and the names used that no palette entry defines (they fall back to the
/// field's default).
pub struct PaletteUse {
    pub named: usize,
    pub references: usize,
    pub undefined: Vec<String>,
}

fn colour_uses(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                // Definitions, not uses — on the settings and on a palette
                // resource alike.
                if key == "palette" {
                    continue;
                }
                if key.ends_with("Hex") {
                    if let Some(name) = value.as_str().and_then(|s| s.strip_prefix('@')) {
                        out.push(name.to_lowercase());
                    }
                } else {
                    colour_uses(value, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                colour_uses(item, out);
            }
        }
        _ => {}
    }
}

pub fn palette_use(meta: &ProjectMetadata) -> PaletteUse {
    let named: HashSet<String> = meta
        .composition_settings
        .palette
        .iter()
        .flatten()
        .map(|c| c.name.to_lowercase())
        .collect();
    let mut uses = Vec::new();
    if let Ok(document) = serde_json::to_value(meta) {
        colour_uses(&document, &mut uses);
    }
    let references = uses.iter().filter(|u| named.contains(*u)).count();
    let undefined: BTreeSet<String> = uses.into_iter().filter(|u| !named.contains(u)).collect();
    PaletteUse {
        named: named.len(),
        references,
        undefined: undefined.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(dir: &std::path::Path, meta: Value) -> Project {
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
        Project::open(dir).expect("opens")
    }

    fn base(layers: Value, resources: Value, settings: Value) -> Value {
        json!({ "id": "P", "name": "Look", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "compositionSettings": settings, "layers": layers, "resources": resources })
    }

    /// The answers the apps' inspect gave, now the core's: a file found in
    /// Resources/ is undeclared, not missing; a declared file that is gone
    /// is missing media, by layer; a use is a use wherever it sits — a
    /// stage member, a slot binding, a nested layer, the theme plate — and
    /// the picture nothing shows is the one unused; `@name` colours are
    /// counted against the palette (review 2026-09-27, P3-46).
    #[test]
    fn inspect_says_what_the_apps_said() {
        let dir = std::env::temp_dir().join(format!("promo-inspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(dir.join("Resources/extra.png"), b"not really").unwrap();
        let derived = promo_model::inventory::derived_id("P", "extra.png");
        let meta = base(
            json!([
                { "id": "bench", "name": "Bench", "sortIndex": 0, "kind": "stage",
                  "isEnabled": true, "startTime": 0, "duration": 3, "keyframes": [],
                  "members": [{ "id": "dev", "name": "Device", "sortIndex": 0, "kind": "model",
                                "isEnabled": true, "startTime": 0, "duration": 3,
                                "resourceID": "phone", "keyframes": [] }] },
                { "id": "found", "name": "Found clip", "sortIndex": 1, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 3, "resourceID": derived,
                  "keyframes": [] },
                { "id": "gone", "name": "Vanished clip", "sortIndex": 2, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 3, "resourceID": "lost",
                  "keyframes": [] },
                { "id": "t", "name": "Title", "sortIndex": 3, "kind": "caption",
                  "isEnabled": true, "startTime": 0, "duration": 3, "captionText": "Hi",
                  "captionStyle": { "textColorHex": "@ink", "strokeColorHex": "@nowhere" },
                  "keyframes": [] }
            ]),
            json!([
                { "id": "shot", "kind": "image", "filename": "shot.png", "displayName": "Shot",
                  "addedAt": 0 },
                { "id": "reel", "kind": "composition", "filename": "", "displayName": "Reel",
                  "addedAt": 0, "composition": { "canvasWidth": 400, "canvasHeight": 250,
                    "layers": [{ "id": "inner", "name": "Inner", "sortIndex": 0, "kind": "image",
                                 "isEnabled": true, "startTime": 0, "duration": 3,
                                 "resourceID": "shot", "keyframes": [] }] } },
                { "id": "phone", "kind": "model", "filename": "", "displayName": "Phone",
                  "addedAt": 0, "recipe": { "device": { "kind": "phone" } },
                  "materials": { "Screen": { "resourceID": "reel" } },
                  "themePlateID": "plate" },
                { "id": "plate", "kind": "image", "filename": "plate.png",
                  "displayName": "Plate", "addedAt": 0 },
                { "id": "lost", "kind": "image", "filename": "lost.png", "displayName": "Lost",
                  "addedAt": 0 },
                { "id": "spare", "kind": "image", "filename": "spare.png", "displayName": "Spare",
                  "addedAt": 0 },
                { "id": "voice", "kind": "audio", "filename": "", "displayName": "Voice",
                  "addedAt": 0, "speech": { "text": "Hello", "provider": "openai",
                                            "voiceID": "alloy" } }
            ]),
            json!({ "canvasWidth": 640, "canvasHeight": 360,
                    "palette": [{ "name": "ink", "colorHex": "111111" }] }),
        );
        let project = project(&dir, meta);
        let text = inspect(&project, false, false);
        assert!(
            text.contains(&format!("folder:    {}", dir.display())),
            "{text}"
        );
        assert!(
            text.contains("kinds:     caption 1, image 2, stage 1"),
            "{text}"
        );
        assert!(text.contains("(1 undeclared, from Resources/)"), "{text}");
        let missing = text
            .split("missing media:")
            .nth(1)
            .expect("a missing section");
        assert!(missing.contains("Vanished clip"), "{text}");
        assert!(
            !missing.contains("Found clip"),
            "a found file is not missing: {text}"
        );
        let unused = text
            .split("unused resources:")
            .nth(1)
            .and_then(|s| s.split("\n\n").next())
            .expect("an unused section");
        assert!(
            unused.contains("image \"Spare\": not used by any layer or slot"),
            "{unused}"
        );
        assert!(
            unused.contains("audio \"Voice\": speech that nothing plays"),
            "{unused}"
        );
        for used in ["\"Shot\"", "\"Reel\"", "\"Phone\"", "\"Plate\"", "\"Lost\""] {
            assert!(!unused.contains(used), "{used} is in use: {unused}");
        }
        assert!(text.contains("palette: 1 named, 1 references"), "{text}");
        assert!(text.contains("  - @nowhere"), "{text}");

        let json: Value = serde_json::from_str(&inspect(&project, true, false)).unwrap();
        assert_eq!(json["undeclared"], 1);
        assert_eq!(json["kinds"]["image"], 2);
        assert_eq!(json["missingMedia"][0]["layer"], "Vanished clip");
        assert_eq!(json["unused"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["palette"]["undefined"], json!(["nowhere"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
