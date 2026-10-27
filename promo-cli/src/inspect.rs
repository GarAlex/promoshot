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
    // SVG pictures (vector resources): the file, its size, whether its
    // text draws (from outlines the import made) and who shows it — the
    // layers naming it, the swap keyframes that bring it in, the layers it
    // masks.
    let vectors: Vec<serde_json::Value> = resources
        .iter()
        .filter(|r| r.kind == promo_model::ProjectResourceKind::Vector)
        .map(|r| {
            let id = r.id.as_str();
            let placed_by: Vec<String> = all_layers
                .iter()
                .filter(|l| l.resource_id.as_deref() == Some(id))
                .map(|l| l.id.clone())
                .collect();
            let swapped_in_by: Vec<serde_json::Value> = all_layers
                .iter()
                .flat_map(|l| {
                    l.keyframes
                        .iter()
                        .filter(move |k| k.resource_id.as_deref() == Some(id))
                        .map(move |k| serde_json::json!({ "layer": l.id, "time": k.time }))
                })
                .collect();
            let masks: Vec<String> = all_layers
                .iter()
                .filter(|l| l.mask_resource_id.as_deref() == Some(id))
                .map(|l| l.id.clone())
                .collect();
            let dir = project.dir.join("Resources");
            let picture = promo_svg::load(&dir, r);
            let text = match promo_svg::render_source(&dir, r) {
                Ok((_, true)) => "outlines",
                _ => match &picture {
                    Ok(p) if p.text_elements() > 0 => "no outlines — text draws nothing",
                    _ => "none",
                },
            };
            let block = promo_model::vector::block(r);
            // The element tree (SVG-PLAN.md slice 2): each element's tier
            // and reason, as drawn with the resource's overrides, and
            // which ones an override changes.
            let overrides = block
                .as_ref()
                .map(|b| b.overrides.clone())
                .unwrap_or_default();
            let model = picture.as_ref().ok().and_then(|p| p.model().ok());
            let nodes: Vec<serde_json::Value> = model
                .as_ref()
                .map(|m| {
                    m.nodes()
                        .iter()
                        .map(|n| {
                            let mut row = serde_json::to_value(n).unwrap_or_default();
                            if let Some(o) = overrides
                                .iter()
                                .find(|o| m.resolve(&o.node).ok() == Some(n.node.as_str()))
                            {
                                let mut keys = serde_json::to_value(o).unwrap_or_default();
                                if let Some(map) = keys.as_object_mut() {
                                    map.remove("node");
                                }
                                row["override"] = keys;
                            }
                            row
                        })
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({
                "nodes": nodes,
                "editable": model.as_ref().map(|m| m.editable()),
                "id": r.id, "name": r.display_name, "file": r.filename,
                "size": promo_svg::natural_size(r)
                    .or_else(|| picture.as_ref().ok().map(|p| p.size()))
                    .map(|(w, h)| [w, h]),
                "elements": picture.as_ref().ok().map(|p| p.elements()),
                "text": text,
                "missingFonts": block.and_then(|b| b.missing_fonts).unwrap_or_default(),
                "problem": picture.as_ref().err(),
                "placedBy": placed_by, "swappedInBy": swapped_in_by, "masks": masks,
            })
        })
        .collect();
    // Beat clocks and their grids, each musical value in both forms, and
    // the credits of what the project uses (docs/BEAT-SYNC.md §7–8).
    let beats: Vec<serde_json::Value> = project
        .timing
        .clocks
        .iter()
        .map(|c| {
            let analysis = project.analyses.get(&c.resource);
            let source = match c.source.as_str() {
                "beatGrid" => "override".to_string(),
                "analysis" => analysis
                    .map(|(_, how)| how.clone())
                    .unwrap_or_else(|| "analysis".into()),
                _ => "none — 120 BPM stand-in".into(),
            };
            // A hand grid's measure: the tempo the song itself keeps.
            let measured = (c.source == "beatGrid")
                .then(|| analysis.map(|(a, _)| a.bpm))
                .flatten();
            serde_json::json!({
                "scope": c.scope, "resource": c.resource, "source": source,
                "bpm": c.bpm, "meter": c.meter, "offset": c.offset, "clock": c.clock,
                "measuredBpm": measured,
                "bpmHint": analysis.and_then(|(a, _)| a.bpm_hint),
                "grid": analysis.map(|(a, _)| a.grid.clone()),
                "confidence": analysis.map(|(a, _)| a.confidence),
                "beats": analysis.map(|(a, _)| a.beats.len()),
                "bars": analysis.map(|(a, _)| a.downbeats.len()),
                "sections": analysis.map(|(a, _)| a.sections.iter().map(|s| serde_json::json!({
                    "index": s.index, "label": s.label, "startBar": s.start_bar,
                    "bars": s.bars, "energy": s.energy,
                })).collect::<Vec<_>>()),
            })
        })
        .collect();
    let musical_of = |layer: &promo_model::ProjectLayer| -> Vec<(String, String, f64)> {
        let mut out: Vec<(String, String, f64)> = layer
            .time_specs
            .iter()
            .flatten()
            .map(|(key, entry)| {
                let shown = entry
                    .spec
                    .clone()
                    .or_else(|| entry.rule.clone())
                    .unwrap_or_default();
                let seconds =
                    promo_model::musical::layer_number(layer, key).unwrap_or(entry.seconds);
                (key.clone(), shown, seconds)
            })
            .collect();
        for keyframe in &layer.keyframes {
            for (key, entry) in keyframe.time_specs.iter().flatten() {
                if let Some(spec) = &entry.spec {
                    let seconds = promo_model::musical::keyframe_number(keyframe, key)
                        .unwrap_or(entry.seconds);
                    out.push((
                        format!("keyframes[{}].{key}", keyframe.id),
                        spec.clone(),
                        seconds,
                    ));
                }
            }
        }
        out
    };
    // Each take's word timing (docs/SPEECH-SYNC.md §9): what speech values
    // count on.
    let words: Vec<serde_json::Value> = promo_model::word_cache::takes(&project.dir, &project.meta)
        .into_iter()
        .map(|take| {
            let record = project.grids.words.by_resource.get(&take.id);
            let fixes = project
                .meta
                .resources
                .iter()
                .flatten()
                .find(|r| r.id == take.id)
                .and_then(|r| promo_model::speech::word_fixes(r).ok())
                .map_or(0, |f| f.len());
            let file = take
                .file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match record {
                Some(t) => {
                    let spoken: f64 = t.words.iter().map(|w| (w.end - w.start).max(0.0)).sum();
                    serde_json::json!({
                        "take": take.id, "file": file, "source": t.source.as_text(),
                        "confidence": t.confidence, "estimated": t.estimated,
                        "words": t.words.len(), "sentences": t.sentences.len(),
                        "coverage": if t.duration > 0.0 { spoken / t.duration } else { 0.0 },
                        "first": t.words.first().map(|w| w.text.clone()),
                        "last": t.words.last().map(|w| w.text.clone()),
                        "stale": project.grids.words.stale.contains(&take.id),
                        "fixes": fixes,
                    })
                }
                None => serde_json::json!({
                    "take": take.id, "file": file, "source": null, "fixes": fixes,
                    "missing": project.missing_words.iter().find(|(id, _)| *id == take.id)
                        .map(|(_, why)| why.clone())
                        .unwrap_or_else(|| "no word timing yet".into()),
                }),
            }
        })
        .collect();
    let credits = crate::credits::credits(&project.meta, None);
    if json {
        return serde_json::json!({
            "beats": beats,
            "words": words,
            "credits": crate::credits::json(&credits),
            "provenance": resources.iter().filter_map(|r| r.provenance.as_ref().map(|p|
                serde_json::json!({ "resource": r.id, "provenance": p }))).collect::<Vec<_>>(),
            "name": project.meta.name,
            "folder": project.dir.display().to_string(),
            "canvas": { "width": w, "height": h },
            "duration": project.duration(),
            "updated": project.meta.updated_at,
            "compositions": compositions,
            "vectors": vectors,
            "layers": layers
                .iter()
                .map(|l| serde_json::json!({
                    "id": l.id, "name": l.name, "kind": l.kind,
                    "startTime": l.start_time, "duration": l.duration,
                    "handle": handle(&l.id),
                    "musical": musical_of(l).into_iter().map(|(at, value, seconds)| {
                        serde_json::json!({ "at": at,
                            "value": serde_json::from_str::<serde_json::Value>(&value)
                                .unwrap_or(serde_json::Value::String(value)),
                            "seconds": seconds })
                    }).collect::<Vec<_>>(),
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
        for (at, value, seconds) in musical_of(layer) {
            out.push_str(&format!("      {at}  {value} → {seconds:.3} s\n"));
        }
    }
    for clock in &beats {
        out.push_str(&format!(
            "beats:     \"{}\" ({}) — {} BPM, {}/4, first downbeat {:.3} s, from the {}{}\n",
            clock["resource"].as_str().unwrap_or(""),
            clock["scope"].as_str().unwrap_or(""),
            clock["bpm"],
            clock["meter"],
            clock["offset"].as_f64().unwrap_or(0.0),
            clock["source"].as_str().unwrap_or(""),
            match (
                clock["confidence"].as_f64(),
                clock["beats"].as_u64(),
                clock["bars"].as_u64()
            ) {
                (Some(c), Some(b), Some(n)) => format!(", confidence {c:.2}, {b} beats, {n} bars"),
                _ => String::new(),
            }
        ));
        if let Some(h) = clock["bpmHint"].as_f64() {
            out.push_str(&format!("      measured with the song's hint, {h} BPM\n"));
        }
        if let Some(m) = clock["measuredBpm"].as_f64() {
            if (m - clock["bpm"].as_f64().unwrap_or(m)).abs() > 1e-6 {
                out.push_str(&format!(
                    "      the song itself measures {m} BPM — the hand grid differs\n"
                ));
            }
        }
        for section in clock["sections"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "      {} — bars {}–{}, energy {:.2}\n",
                section["label"].as_str().unwrap_or(""),
                section["startBar"],
                section["startBar"].as_u64().unwrap_or(1) + section["bars"].as_u64().unwrap_or(1)
                    - 1,
                section["energy"].as_f64().unwrap_or(0.0)
            ));
        }
    }
    for take in &words {
        let name = take["take"].as_str().unwrap_or("");
        let file = take["file"].as_str().unwrap_or("");
        match take["source"].as_str() {
            Some(source) => out.push_str(&format!(
                "words:     \"{name}\" ({file}) — {} words, {} sentences, from {source}, \
                 confidence {:.2}, covers {:.0}% — \"{}\" … \"{}\"{}{}\n",
                take["words"],
                take["sentences"],
                take["confidence"].as_f64().unwrap_or(0.0),
                take["coverage"].as_f64().unwrap_or(0.0) * 100.0,
                take["first"].as_str().unwrap_or(""),
                take["last"].as_str().unwrap_or(""),
                if take["stale"].as_bool() == Some(true) {
                    ", stale (the text changed)"
                } else {
                    ""
                },
                match take["fixes"].as_u64() {
                    Some(0) | None => String::new(),
                    Some(n) => format!(", {n} hand fix(es)"),
                }
            )),
            None => out.push_str(&format!(
                "words:     \"{name}\" ({file}) — {}\n",
                take["missing"].as_str().unwrap_or("no word timing yet")
            )),
        }
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
    if !vectors.is_empty() {
        out.push_str(&format!("vectors:   {}\n", vectors.len()));
        for v in &vectors {
            let count = |key: &str| v[key].as_array().map_or(0, |a| a.len());
            let mut uses = format!(
                "placed by {} layer{}",
                count("placedBy"),
                plural(count("placedBy"))
            );
            if count("swappedInBy") > 0 {
                uses.push_str(&format!(
                    ", swapped in by {} keyframe{}",
                    count("swappedInBy"),
                    plural(count("swappedInBy"))
                ));
            }
            if count("masks") > 0 {
                uses.push_str(&format!(
                    ", masks {} layer{}",
                    count("masks"),
                    plural(count("masks"))
                ));
            }
            let size = v["size"]
                .as_array()
                .map(|s| {
                    format!(
                        "{}x{}",
                        s[0].as_f64().unwrap_or(0.0),
                        s[1].as_f64().unwrap_or(0.0)
                    )
                })
                .unwrap_or_else(|| "no size".into());
            let mut notes = Vec::new();
            match v["text"].as_str() {
                Some("outlines") => notes.push("text as outlines".to_string()),
                Some("none") | None => {}
                Some(other) => notes.push(other.to_string()),
            }
            for font in v["missingFonts"].as_array().into_iter().flatten() {
                notes.push(format!("font {font} was missing"));
            }
            if let Some(problem) = v["problem"].as_str() {
                notes.push(problem.to_string());
            }
            out.push_str(&format!(
                "  {}  \"{}\"  {}  {size}  {uses}{}\n",
                v["id"].as_str().unwrap_or(""),
                v["name"].as_str().unwrap_or(""),
                v["file"].as_str().unwrap_or(""),
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", notes.join("; "))
                }
            ));
            // The element tree: address, the author's name, tier and
            // reason; ● marks an element an override changes, with its keys.
            let nodes = v["nodes"].as_array().cloned().unwrap_or_default();
            if !nodes.is_empty() {
                let tally = |tier: &str| nodes.iter().filter(|n| n["tier"] == tier).count();
                let changed = nodes.iter().filter(|n| n.get("override").is_some()).count();
                out.push_str(&format!(
                    "    elements: {} (A {}, B {}, C {}, locked {}){}{}\n",
                    nodes.len(),
                    tally("A"),
                    tally("B"),
                    tally("C"),
                    tally("locked"),
                    if changed > 0 {
                        format!(" · {changed} overridden")
                    } else {
                        String::new()
                    },
                    if v["editable"] == false {
                        " · edits need a fresh import"
                    } else {
                        ""
                    }
                ));
                const SHOWN: usize = 80;
                for n in nodes.iter().take(SHOWN) {
                    let depth = n["depth"].as_u64().unwrap_or(0) as usize;
                    let name = n["name"]
                        .as_str()
                        .map(|s| format!(" ← {s}"))
                        .unwrap_or_default();
                    let mark = n
                        .get("override")
                        .and_then(|o| o.as_object())
                        .map(|o| format!(" ● {}", o.keys().cloned().collect::<Vec<_>>().join(", ")))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "      {}{}{name}  {}  {} — {}{mark}\n",
                        "  ".repeat(depth),
                        n["node"].as_str().unwrap_or(""),
                        n["tag"].as_str().unwrap_or(""),
                        n["tier"].as_str().unwrap_or(""),
                        n["reason"].as_str().unwrap_or(""),
                    ));
                }
                if nodes.len() > SHOWN {
                    out.push_str(&format!(
                        "      … {} more (the JSON form lists every element)\n",
                        nodes.len() - SHOWN
                    ));
                }
            }
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
    let made: Vec<String> = resources
        .iter()
        .filter_map(|r| {
            let p = r.provenance.as_ref()?;
            Some(format!(
                "\n  - \"{}\": {}{}{}",
                r.display_name,
                p.tool.as_deref().unwrap_or("an unnamed tool"),
                p.model
                    .as_ref()
                    .map(|m| format!(" {m}"))
                    .unwrap_or_default(),
                p.license
                    .as_ref()
                    .map(|l| format!(", {l}"))
                    .unwrap_or_default(),
            ))
        })
        .collect();
    if !made.is_empty() {
        out.push_str("\n\nprovenance:");
        for line in made {
            out.push_str(&line);
        }
    }
    if !credits.is_empty() {
        out.push_str(&format!("\n\ncredits:\n{}", crate::credits::text(&credits)));
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
pub(crate) fn pointed_at(node: &Value, parent: Option<&str>, out: &mut HashSet<String>) {
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
