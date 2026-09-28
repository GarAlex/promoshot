//! Where a model's slots land on the canvas, MEASURED by the renderer
//! (review 2026-09-27, P2-36).
//!
//! The agents' 3D runs spent their turns finding where a screen lands: C11
//! built nine calibration projects and fitted a law to them, C12 bound
//! magenta to a screen and read the corners off renders. The renderer knows
//! — so explain asks it the same way, without a second copy of the camera
//! code: the frame as it is, then the frame with one bound slot painted a
//! probe colour, and the pixels that changed are where that slot shows —
//! occlusion, glass, the camera's floor, a flown route and every host
//! composition included, because it is the render.

use std::path::Path;

use promo_model::{MaterialBinding, ProjectResourceKind};
use serde_json::{json, Value};

use crate::project::Project;
use crate::render;

/// One bound slot of one model resource, on the canvas at a moment.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotPlacement {
    pub model: String,
    pub slot: String,
    /// x, y, width, height in canvas pixels; `None` when no pixel of it
    /// shows (off frame, turned away, behind something, not on yet).
    pub rect: Option<[f64; 4]>,
    /// The share of the canvas it covers, 0…1.
    pub covers: f64,
}

/// The longest side a probe render is drawn at: enough for a rect to a
/// couple of canvas pixels, cheap enough for eight slots.
const PROBE_EDGE: f64 = 640.0;
/// The most slots one explain measures.
const MOST_SLOTS: usize = 8;

/// Every slot that shows a picture, video or composition — the screens —
/// placed on the canvas at `time`, by probe renders.
pub fn slot_placements(project: &Project, time: f64) -> Result<Vec<SlotPlacement>, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for resource in project.meta.resources.iter().flatten() {
        if resource.kind != ProjectResourceKind::Model {
            continue;
        }
        for (slot, binding) in resource.materials.iter().flatten() {
            if binding.resource_id().is_some() {
                pairs.push((resource.id.clone(), slot.clone()));
            }
        }
    }
    pairs.truncate(MOST_SLOTS);
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let settings = &project.meta.composition_settings;
    let (cw, ch) = (
        settings.canvas_width.max(1.0),
        settings.canvas_height.max(1.0),
    );
    let scale = (PROBE_EDGE / cw.max(ch)).min(1.0);
    let even = |v: f64| (((v * scale).round() as u32).max(2) + 1) & !1;
    let (w, h) = (even(cw), even(ch));
    let (sx, sy) = (cw / w as f64, ch / h as f64);

    let plain = render::Renderer::new(project, w, h)?.frame_rgba(time)?;
    let mut out = Vec::new();
    for (model, slot) in pairs {
        let mut meta = project.meta.clone();
        if let Some(materials) = meta
            .resources
            .as_mut()
            .and_then(|rs| rs.iter_mut().find(|r| r.id == model))
            .and_then(|r| r.materials.as_mut())
        {
            materials.insert(slot.clone(), MaterialBinding::Color("FF00FF".into()));
        }
        // A fresh renderer per probe: a cached frame of the model must not
        // answer for the probe.
        let probe = render::Renderer::new(&project.with_meta(meta), w, h)?.frame_rgba(time)?;
        let (mut x0, mut y0, mut x1, mut y1, mut count) = (u32::MAX, u32::MAX, 0u32, 0u32, 0usize);
        for (i, (a, b)) in plain.chunks_exact(4).zip(probe.chunks_exact(4)).enumerate() {
            let changed = (0..3)
                .map(|c| (a[c] as i32 - b[c] as i32).abs())
                .sum::<i32>()
                > 36;
            if changed {
                let (x, y) = (i as u32 % w, i as u32 / w);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                count += 1;
            }
        }
        let rect = (count > 0).then(|| {
            [
                x0 as f64 * sx,
                y0 as f64 * sy,
                (x1 - x0 + 1) as f64 * sx,
                (y1 - y0 + 1) as f64 * sy,
            ]
        });
        out.push(SlotPlacement {
            model,
            slot,
            rect,
            covers: count as f64 / (w as f64 * h as f64),
        });
    }
    Ok(out)
}

/// `promo_explain` with the renderer's measurements: the project-only
/// answer from `promo_author::explain`, and — when a model shows something
/// on a slot — where each such slot lands on the canvas at that moment and
/// how much of the frame it covers, written into every slot entry of every
/// layer showing that model, nested ones included. The CLI's `explain` and
/// the app's server answer with this; the headless server asks the CLI.
pub fn explain(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let text = promo_author::explain(args, root)?;
    let mut answer: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let slots = shows_on_a_slot(&answer["layers"]);
    let aims = aims_or_flies(&answer["layers"]);
    if !slots && !aims {
        return Ok(text);
    }
    let raw = args
        .get("project")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut dir = std::fs::canonicalize(raw).map_err(|e| format!("project: {e}"))?;
    if dir.is_file() {
        dir = dir.parent().map(Path::to_path_buf).unwrap_or(dir);
    }
    let time = answer["time"].as_f64().unwrap_or(0.0);
    let project = Project::open(&dir);
    if slots {
        match project
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|project| slot_placements(project, time))
        {
            Ok(placements) => place_slots(&mut answer["layers"], &placements),
            Err(why) => answer["slotPlacement"] = json!(format!("not measured: {why}")),
        }
    }
    // A stage whose camera frames its own shot: whether each member is in
    // frame NOW, from the engine's framing probe — the moment `validate`
    // looks at across the whole film (3D plan §6½, R1).
    if aims {
        match project
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|project| crate::framing::in_frame(project, time))
        {
            Ok(framed) => mark_in_frame(&mut answer["layers"], &framed),
            Err(why) => answer["inFrame"] = json!(format!("not measured: {why}")),
        }
    }
    serde_json::to_string_pretty(&answer).map_err(|e| e.to_string())
}

/// Does any stage here (compositions included) have a camera that aims or
/// flies a route — one that frames its own shot?
fn aims_or_flies(layers: &Value) -> bool {
    layers.as_array().is_some_and(|layers| {
        layers.iter().any(|layer| {
            let camera = &layer["camera"];
            (layer["members"].is_array()
                && (camera.get("target").is_some() || camera.get("route").is_some()))
                || aims_or_flies(&layer["inside"]["layers"])
        })
    })
}

fn mark_in_frame(layers: &mut Value, framed: &std::collections::BTreeMap<String, Value>) {
    let Some(layers) = layers.as_array_mut() else {
        return;
    };
    for layer in layers {
        if let Some(members) = layer["members"].as_array_mut() {
            for member in members {
                if let Some(doc) = member["id"].as_str().and_then(|id| framed.get(id)) {
                    member["inFrame"] = doc.clone();
                }
            }
        }
        mark_in_frame(&mut layer["inside"]["layers"], framed);
    }
}

/// Does any layer here (members and compositions included) show something
/// on a model's slot?
fn shows_on_a_slot(layers: &Value) -> bool {
    layers.as_array().is_some_and(|layers| {
        layers.iter().any(|layer| {
            layer["slots"]
                .as_object()
                .is_some_and(|slots| slots.values().any(|s| s.get("shows").is_some()))
                || shows_on_a_slot(&layer["members"])
                || shows_on_a_slot(&layer["inside"]["layers"])
        })
    })
}

fn place_slots(layers: &mut Value, placements: &[SlotPlacement]) {
    let Some(layers) = layers.as_array_mut() else {
        return;
    };
    for layer in layers {
        let showing = layer["showing"].as_str().map(String::from);
        if let (Some(model), Some(slots)) = (showing, layer["slots"].as_object_mut()) {
            for (slot, entry) in slots.iter_mut() {
                let Some(placed) = placements
                    .iter()
                    .find(|p| p.model == model && &p.slot == slot)
                else {
                    continue;
                };
                entry["onCanvas"] = match placed.rect {
                    Some([x, y, w, h]) => json!({ "x": x.round(), "y": y.round(),
                                                  "width": w.round(), "height": h.round() }),
                    None => Value::Null,
                };
                entry["covers"] = json!((placed.covers * 1000.0).round() / 1000.0);
                if placed.rect.is_none() {
                    entry["why"] = json!(
                        "no pixel of it shows now — off frame, turned away, behind \
                         something, or not on yet"
                    );
                }
            }
        }
        place_slots(&mut layer["members"], placements);
        place_slots(&mut layer["inside"]["layers"], placements);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone with a picture on its Screen, looked at from the front: the
    /// render places the screen inside the model's square and says how
    /// much of the frame it covers. Turned round, no pixel of the screen
    /// shows and the answer says so — the probe sees what the camera sees.
    #[test]
    fn a_bound_screen_is_placed_by_the_render() {
        if promo_gpu::gpu_for_test().is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-placement-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        crate::render::write_png(
            &dir.join("Resources/shot.png"),
            &[40, 200, 90, 255].repeat(64),
            8,
            8,
        )
        .unwrap();
        let project = |yaw: f64| {
            std::fs::write(
                dir.join("metadata.json"),
                json!({
                    "id": "P", "name": "Probe", "createdAt": 0, "state": "recorded",
                    "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                    "compositionSettings": { "canvasWidth": 640, "canvasHeight": 360,
                                             "backgroundColorHex": "202020" },
                    "resources": [
                        { "id": "shot", "kind": "image", "filename": "shot.png", "displayName": "s",
                          "addedAt": 0, "pixelWidth": 8, "pixelHeight": 8 },
                        { "id": "phone", "kind": "model", "filename": "", "displayName": "Phone",
                          "addedAt": 0, "recipe": { "device": { "kind": "phone" } },
                          "materials": { "Screen": { "resourceID": "shot" } } }
                    ],
                    "layers": [{ "id": "m", "name": "m", "sortIndex": 0, "kind": "model",
                                 "isEnabled": true, "startTime": 0, "duration": 2,
                                 "resourceID": "phone",
                                 "keyframes": [{ "id": "k", "time": 0,
                                                 "camera": { "yaw": yaw, "pitch": 0 } }] }]
                })
                .to_string(),
            )
            .unwrap();
            Project::open(&dir).expect("project")
        };
        let front = slot_placements(&project(0.0), 0.5).expect("measured");
        assert_eq!(front.len(), 1);
        let [x, y, w, h] = front[0].rect.expect("the screen faces the camera");
        // Inside the square the model is drawn in, as explain lays the
        // layer out without rendering — the layout and the render agree.
        let answer: Value = serde_json::from_str(
            &explain(
                &json!({ "project": dir.to_string_lossy(), "time": 0.5 }),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        let square = &answer["layers"][0]["rect"];
        let (sx, sy) = (square["x"].as_f64().unwrap(), square["y"].as_f64().unwrap());
        let (sw, sh) = (
            square["width"].as_f64().unwrap(),
            square["height"].as_f64().unwrap(),
        );
        assert!(
            x >= sx - 2.0 && y >= sy - 2.0 && x + w <= sx + sw + 2.0 && y + h <= sy + sh + 2.0,
            "the screen {:?} inside the model's square {square}",
            front[0]
        );
        assert!(h > w, "a phone's screen is tall: {:?}", front[0]);
        assert!(front[0].covers > 0.02, "{:?}", front[0]);
        let back = slot_placements(&project(180.0), 0.5).expect("measured");
        assert_eq!(
            back[0].rect, None,
            "turned away, the screen shows nothing: {:?}",
            back[0]
        );

        // And explain carries it into the slot entry.
        project(0.0);
        let answer: Value = serde_json::from_str(
            &explain(
                &json!({ "project": dir.to_string_lossy(), "time": 0.5 }),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        let screen = &answer["layers"][0]["slots"]["Screen"];
        assert!(
            screen["onCanvas"]["height"].as_f64().unwrap() > 0.0,
            "{screen}"
        );
        assert!(screen["covers"].as_f64().unwrap() > 0.02, "{screen}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
