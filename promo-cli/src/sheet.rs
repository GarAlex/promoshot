//! Contact sheets the servers answer with: the model turntable, and the
//! grid and blit every sheet is tiled with.
//!
//! In the library rather than the CLI binary so the Mac app's server runs
//! the same turntable over the C ABI (review 2026-09-27, P2-32) — it was
//! advertised in the app's skill copy and existed only headless.

use std::path::Path;

use crate::project::Project;
use crate::render;

/// The model seen from around: `count` yaws evenly round the circle (6 by
/// default, at most 64), each rendered on a square `cell` (320 px by
/// default, 32…1024) by the engine's own pass under the default light,
/// tiled into one contact sheet at `out` — what an agent looks at before
/// choosing a camera. Answers `{wrote, cells: [{yaw, column, row}],
/// columns, rows, cell}`.
pub fn turntable(
    file: &Path,
    out: &Path,
    count: Option<usize>,
    cell: Option<u32>,
) -> Result<serde_json::Value, String> {
    let count = count.unwrap_or(6).clamp(1, 64);
    let cell = cell.unwrap_or(320).clamp(32, 1024);
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    promo_engine::model::Model::from_glb(&bytes).map_err(|e| e.to_string())?;

    // A throwaway project round the file: a model layer keyed once per
    // cell, a step between yaws, rendered at whole seconds.
    let dir = std::env::temp_dir().join(format!(
        "promo-turntable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(dir.join("Resources")).map_err(|e| e.to_string())?;
    let answer = render_turntable(file, out, count, cell, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    answer
}

fn render_turntable(
    file: &Path,
    out: &Path,
    count: usize,
    cell: u32,
    dir: &Path,
) -> Result<serde_json::Value, String> {
    let filename = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model.glb")
        .to_string();
    std::fs::copy(file, dir.join("Resources").join(&filename)).map_err(|e| e.to_string())?;
    let yaws: Vec<f64> = (0..count)
        .map(|i| -180.0 + 360.0 * i as f64 / count as f64)
        .collect();
    let keyframes: Vec<serde_json::Value> = yaws
        .iter()
        .enumerate()
        .map(|(i, yaw)| {
            serde_json::json!({
                "id": format!("K{i}"), "time": i as f64,
                "camera": { "yaw": yaw, "pitch": 12.0 },
                "transitionDuration": 0
            })
        })
        .collect();
    let doc = serde_json::json!({
        "id": "turntable", "name": "Turntable", "createdAt": 0, "state": "recorded",
        "minReaderVersion": 29,
        "trimStart": 0, "trimEnd": count as f64, "videoDuration": count as f64, "subtitles": [],
        "compositionSettings": { "canvasWidth": cell, "canvasHeight": cell, "backgroundColorHex": "1A1F2B" },
        "resources": [{ "id": "M", "kind": "model", "filename": filename, "displayName": "Model", "addedAt": 0 }],
        "layers": [{ "id": "L", "name": "model", "sortIndex": 0, "kind": "model", "isEnabled": true,
                     "startTime": 0, "duration": count as f64, "resourceID": "M", "keyframes": keyframes }]
    });
    std::fs::write(dir.join("metadata.json"), doc.to_string()).map_err(|e| e.to_string())?;
    let project = Project::open(dir)?;
    let mut renderer = render::Renderer::new(&project, cell, cell)?;
    let (columns, rows) = grid_for(count);
    let (sheet_w, sheet_h) = (columns as u32 * cell, rows as u32 * cell);
    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];
    let mut cells = Vec::new();
    for (i, yaw) in yaws.iter().enumerate() {
        let rgba = renderer.frame_rgba(i as f64 + 0.5)?;
        let (cx, cy) = ((i % columns) as u32, (i / columns) as u32);
        blit(&mut sheet, sheet_w, &rgba, cell, cell, cx * cell, cy * cell);
        cells.push(serde_json::json!({ "yaw": yaw, "column": cx, "row": cy }));
    }
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    render::write_png(out, &sheet, sheet_w, sheet_h)?;
    Ok(serde_json::json!({
        "wrote": out.display().to_string(),
        "cells": cells, "columns": columns, "rows": rows, "cell": cell,
    }))
}

/// A model's facts, as `promo_media_probe` answers them on a .glb on both
/// servers: its material slots (what a `materials` binding may name),
/// clips, bounds, meshes and triangles.
pub fn model_facts(file: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let model = promo_engine::model::Model::from_glb(&bytes).map_err(|e| e.to_string())?;
    let slots: Vec<serde_json::Value> = model
        .materials
        .iter()
        .filter(|m| !m.name.is_empty())
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "baseColor": m.base_color,
                "metallic": m.metallic,
                "roughness": m.roughness,
                "textured": m.base_texture.is_some(),
                "doubleSided": m.double_sided,
            })
        })
        .collect();
    let clips: Vec<serde_json::Value> = model
        .clip_summary()
        .into_iter()
        .map(|(name, duration)| serde_json::json!({ "name": name, "duration": duration }))
        .collect();
    let triangles: usize = model.meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok(serde_json::json!({
        "kind": "model",
        "file": file.display().to_string(),
        "boundsRadius": model.bounds_radius,
        "boundsCenter": model.bounds_center,
        "slots": slots,
        "clips": clips,
        "meshes": model.meshes.len(),
        "triangles": triangles,
    }))
}

/// The grid a count tiles into — as square as it gets, row-major.
pub fn grid_for(count: usize) -> (usize, usize) {
    let columns = (count as f64).sqrt().ceil().max(1.0) as usize;
    let rows = count.div_ceil(columns).max(1);
    (columns, rows)
}

/// One cell into the sheet, top-left corner at (x, y).
pub fn blit(sheet: &mut [u8], sheet_w: u32, cell: &[u8], cw: u32, ch: u32, x: u32, y: u32) {
    for row in 0..ch {
        let src = (row * cw * 4) as usize;
        let dst = (((y + row) * sheet_w + x) * 4) as usize;
        let span = (cw * 4) as usize;
        if src + span <= cell.len() && dst + span <= sheet.len() {
            sheet[dst..dst + span].copy_from_slice(&cell[src..src + span]);
        }
    }
}
