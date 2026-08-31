//! The two authoring tools: `promo_init` and `promo_upsert_layer`.
//!
//! The schema stays the source of truth — these tools build every record
//! THROUGH the wire: a `json!` template decoded by the format's own parser,
//! so a record a tool writes is by construction one the schema accepts.
//! What the tools add is exactly the boilerplate a model shouldn't spend
//! tokens on: UUIDs, the required top-level fields, the background layer,
//! and the arithmetic that keeps the composition covering its layers.
//! Everything they write can be read back, hand-edited, validated and
//! rendered like any hand-authored project — an author who outgrows the
//! tools just edits the JSON.

use std::path::{Path, PathBuf};

use promo_model::{Placement, ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource};
use serde_json::{json, Value};

fn mint() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{key}` is required"))
}

/// "1920x1080" or {"width":1920,"height":1080}.
fn canvas_size(args: &Value) -> Result<(f64, f64), String> {
    match args.get("canvas") {
        Some(Value::String(s)) => {
            let (w, h) = s
                .split_once(['x', 'X'])
                .ok_or("`canvas` looks like \"1920x1080\"")?;
            Ok((
                w.trim().parse().map_err(|_| "`canvas` width")?,
                h.trim().parse().map_err(|_| "`canvas` height")?,
            ))
        }
        Some(Value::Object(o)) => Ok((
            o.get("width")
                .and_then(Value::as_f64)
                .ok_or("`canvas.width`")?,
            o.get("height")
                .and_then(Value::as_f64)
                .ok_or("`canvas.height`")?,
        )),
        _ => Err("`canvas` is required — \"1920x1080\" or {width, height}".into()),
    }
}

/// Palette input: [{name, colorHex}] or {"name": "hex"} — either spelling,
/// normalized to the wire's array-of-entries.
fn palette_entries(args: &Value) -> Result<Vec<Value>, String> {
    match args.get("palette") {
        Some(Value::Array(entries)) => {
            let mut out = Vec::new();
            for entry in entries {
                out.push(json!({
                    "name": required_str(entry, "name")?,
                    "colorHex": required_str(entry, "colorHex")?,
                }));
            }
            Ok(out)
        }
        Some(Value::Object(map)) => {
            let mut out = Vec::new();
            for (key, value) in map {
                let hex = value
                    .as_str()
                    .ok_or_else(|| format!("palette.{key} must be a hex string"))?;
                out.push(json!({ "name": key, "colorHex": hex }));
            }
            Ok(out)
        }
        None => Ok(Vec::new()),
        _ => Err("`palette` is [{name, colorHex}] or {name: hex}".into()),
    }
}

/// A new project folder: metadata.json + Resources/, canvas and palette in,
/// a background layer ready, ids minted. Refuses to overwrite — an existing
/// metadata.json is someone's work, not boilerplate.
pub fn init(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let dir = PathBuf::from(required_str(args, "project")?);
    fence_new_path(&dir, root)?;
    let meta_path = dir.join("metadata.json");
    if meta_path.exists() {
        return Err(format!(
            "{} already exists — promo_init creates, never overwrites",
            meta_path.display()
        ));
    }
    let (width, height) = canvas_size(args)?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            dir.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into())
        });

    let palette = palette_entries(args)?;
    let has_canvas_colour = palette.iter().any(|p| {
        p["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case("canvas"))
    });

    let mut settings = json!({ "canvasWidth": width, "canvasHeight": height });
    if has_canvas_colour {
        settings["backgroundColorHex"] = json!("@canvas");
    }
    if !palette.is_empty() {
        settings["palette"] = json!(palette);
    }
    // The background layer states the ground; with a palette the colour is
    // the settings' own "@canvas", so the layer needs no keyframe at all.
    let document = json!({
        "id": mint(), "name": name, "createdAt": 0, "state": "recorded",
        "minReaderVersion": 18,
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
        "compositionSettings": settings,
        "resources": [],
        "layers": [{
            "id": mint(), "name": "Background", "sortIndex": 0,
            "kind": "background", "isEnabled": true, "startTime": 0,
            "duration": 0.1, "keyframes": []
        }]
    });
    // Through the parser on the way OUT too: what this tool writes is,
    // provably, what the schema accepts.
    let meta = ProjectMetadata::from_json(&document.to_string())
        .map_err(|e| format!("template rejected by the format itself: {e}"))?;

    std::fs::create_dir_all(dir.join("Resources"))
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    write_metadata(&meta, &meta_path)?;
    Ok(format!(
        "initialized {} ({width}x{height}) — add layers with promo_upsert_layer",
        dir.display()
    ))
}

/// Add or update one layer: image, video or caption, with a placement.
/// Media is COPIED into `Resources/`; a caption is words plus style. Every
/// call re-stretches the background and the composition to cover the
/// layers, so a project built one call at a time is coherent after each.
pub fn upsert_layer(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let dir = PathBuf::from(required_str(args, "project")?);
    let dir = std::fs::canonicalize(&dir).map_err(|e| format!("project: {e}"))?;
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
        .map_err(|_| format!("no metadata.json in {} — promo_init first", dir.display()))?;
    let mut meta = ProjectMetadata::from_json(&text).map_err(|e| format!("decode: {e}"))?;

    let kind_name = required_str(args, "kind")?;
    if !matches!(kind_name, "image" | "video" | "caption") {
        return Err(format!("kind `{kind_name}` — image, video or caption"));
    }
    let start_time = args.get("startTime").and_then(Value::as_f64).unwrap_or(0.0);
    let placement: Option<Placement> = match args.get("placement") {
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|e| format!("placement: {e} (see promo_schema)"))?,
        ),
        None => None,
    };

    // The resource half: media is copied in; a caption needs none.
    let mut resource_note = String::new();
    let mut probed_duration = None;
    let resource_id = if kind_name == "caption" {
        None
    } else {
        let source = PathBuf::from(required_str(args, "file")?);
        let filename = copy_into_resources(&dir, &source)?;
        let id = mint();
        let mut record = json!({
            "id": id, "kind": kind_name, "filename": filename,
            "displayName": source.file_stem().unwrap_or_default().to_string_lossy(),
            "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": []
        });
        // The source's own pixels, stamped the way the app stamps them on
        // import: placement anchors and widths resolve against the aspect,
        // and an unmeasured source is positioned as a SQUARE — which the
        // validator now names. ffprobe reads image headers too.
        let staged = dir.join("Resources").join(&filename);
        if let Ok((w, h)) = probe_pixels(&staged) {
            if kind_name == "video" {
                record["videoNaturalWidth"] = json!(w);
                record["videoNaturalHeight"] = json!(h);
            } else {
                record["pixelWidth"] = json!(w);
                record["pixelHeight"] = json!(h);
            }
        }
        if kind_name == "video" {
            let seconds = match args.get("duration").and_then(Value::as_f64) {
                Some(d) => d,
                None => probe_duration(&staged)?,
            };
            probed_duration = Some(seconds);
            record["duration"] = json!(seconds);
            record["trimStart"] = json!(0);
            record["trimEnd"] = json!(seconds);
        }
        let resource: ProjectResource = serde_json::from_value(record)
            .map_err(|e| format!("resource template rejected by the format: {e}"))?;
        resource_note = format!(", resource {id} ({filename})");
        meta.resources.get_or_insert_with(Vec::new).push(resource);
        Some(id)
    };

    let duration = args
        .get("duration")
        .and_then(Value::as_f64)
        .or(probed_duration)
        .unwrap_or(3.0);

    // The layer half: found by id when one is given — that is the UPSERT —
    // minted otherwise.
    let layers = meta.layers.get_or_insert_with(Vec::new);
    let next_sort = layers.iter().map(|l| l.sort_index + 1).max().unwrap_or(0);
    let wanted_id = args.get("id").and_then(Value::as_str).map(str::to_string);
    let index = wanted_id
        .as_ref()
        .and_then(|id| layers.iter().position(|l| &l.id == id));
    let layer_id = wanted_id.unwrap_or_else(mint);
    let default_name = match kind_name {
        "caption" => "Caption",
        "video" => "Clip",
        _ => "Picture",
    };
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(default_name);

    let mut record = json!({
        "id": layer_id, "name": name,
        "sortIndex": index.map(|i| layers[i].sort_index).unwrap_or(next_sort),
        "kind": kind_name, "isEnabled": true,
        "startTime": start_time, "duration": duration, "keyframes": []
    });
    if let Some(id) = &resource_id {
        record["resourceID"] = json!(id);
    }
    if kind_name == "caption" {
        if let Some(words) = args.get("captionText").and_then(Value::as_str) {
            record["captionText"] = json!(words);
        }
        let mut style = index
            .and_then(|i| layers[i].caption_style.clone())
            .map(|s| serde_json::to_value(s).unwrap_or_else(|_| json!({})))
            .unwrap_or_else(|| json!({}));
        if let Some(rule) = &placement {
            if rule.sizes() {
                return Err(
                    "a caption placement takes anchor and offset only — its size is fontSize"
                        .into(),
                );
            }
            style["placement"] = serde_json::to_value(rule).map_err(|e| e.to_string())?;
        }
        if let Some(size) = args.get("fontSize").and_then(Value::as_f64) {
            style["fontSize"] = json!(size);
        }
        record["captionStyle"] = style;
    } else if let Some(rule) = &placement {
        // Placement rides keyframe 0 — the upsert's contract is "this is
        // the layer's framing now".
        record["keyframes"] = json!([{
            "id": mint(), "time": 0, "transitionDuration": 0,
            "placement": serde_json::to_value(rule).map_err(|e| e.to_string())?
        }]);
    }
    let mut layer: ProjectLayer = serde_json::from_value(record)
        .map_err(|e| format!("layer template rejected by the format: {e}"))?;
    if kind_name == "caption" && layer.caption_text.is_none() {
        layer.caption_text = index.and_then(|i| layers[i].caption_text.clone());
    }

    match index {
        Some(i) => layers[i] = layer,
        None => layers.push(layer),
    }

    // The boilerplate arithmetic: composition and background cover the show.
    let end = layers
        .iter()
        .filter(|l| l.kind != ProjectLayerKind::Background)
        .filter_map(|l| l.duration.map(|d| l.start_time + d))
        .fold(0.0_f64, f64::max);
    for layer in layers.iter_mut() {
        if layer.kind == ProjectLayerKind::Background {
            layer.duration = Some(end.max(0.1));
        }
    }
    meta.trim_end = end;
    meta.video_duration = end;

    write_metadata(&meta, &meta_path)?;
    Ok(format!(
        "upserted layer {layer_id}{resource_note}; composition runs {end}s"
    ))
}

fn copy_into_resources(project: &Path, source: &Path) -> Result<String, String> {
    if !source.exists() {
        return Err(format!("file {} does not exist", source.display()));
    }
    let resources = project.join("Resources");
    std::fs::create_dir_all(&resources).map_err(|e| e.to_string())?;
    let base = source
        .file_name()
        .ok_or("`file` must name a file")?
        .to_string_lossy()
        .into_owned();
    let mut target = resources.join(&base);
    if target.exists() {
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let fresh = match source.extension() {
            Some(ext) => format!("{stem}-{}.{}", &mint()[..8], ext.to_string_lossy()),
            None => format!("{stem}-{}", &mint()[..8]),
        };
        target = resources.join(&fresh);
    }
    std::fs::copy(source, &target).map_err(|e| format!("copy: {e}"))?;
    Ok(target.file_name().unwrap().to_string_lossy().into_owned())
}

/// The frame size ffprobe reads from the first stream — images included.
fn probe_pixels(path: &Path) -> Result<(f64, f64), String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split(',');
    let width: f64 = parts
        .next()
        .and_then(|w| w.parse().ok())
        .ok_or("no width")?;
    let height: f64 = parts
        .next()
        .and_then(|h| h.parse().ok())
        .ok_or("no height")?;
    Ok((width, height))
}

/// A clip's length is the file's own — read with ffprobe, the same tool the
/// render pipeline already requires on PATH.
fn probe_duration(path: &Path) -> Result<f64, String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe (needed for a video's duration): {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| format!("ffprobe could not read a duration from {}", path.display()))
}

fn write_metadata(meta: &ProjectMetadata, path: &Path) -> Result<(), String> {
    let json = meta.to_json().map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("write: {e}"))
}

/// For a path that need not exist yet: the nearest existing ancestor must
/// be inside the root.
fn fence_new_path(path: &Path, root: Option<&Path>) -> Result<(), String> {
    let Some(root) = root else { return Ok(()) };
    let root = std::fs::canonicalize(root).map_err(|e| format!("--root: {e}"))?;
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = match probe.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Err("project path has no existing ancestor".into()),
        };
    }
    let anchored = std::fs::canonicalize(&probe).map_err(|e| e.to_string())?;
    if anchored.starts_with(&root) {
        Ok(())
    } else {
        Err(format!(
            "project is outside the served root {}",
            root.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("promo-authoring-{}", mint()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(dir: &Path) -> ProjectMetadata {
        let text = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
        ProjectMetadata::from_json(&text).unwrap()
    }

    /// init → two upserts → a project that decodes, validates with ZERO
    /// warnings, and carries the placement language it was asked for. The
    /// validator is the same one `promo validate` runs — a tool that built
    /// a warned-about project would fail right here.
    #[test]
    fn a_tool_built_project_validates_clean() {
        let root = scratch();
        let dir = root.join("Built.promo");
        let shot = root.join("shot.png");
        // A real 1x1 PNG: the tool stamps pixel size with ffprobe, and the
        // validator warns on an unmeasured placed source — this test holds
        // the whole chain to "zero warnings".
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&shot, PNG_1X1).unwrap();

        init(
            &json!({
                "project": dir.to_string_lossy(),
                "canvas": "1920x1080",
                "palette": {"canvas": "10182B", "text": "F3F5FF"}
            }),
            Some(&root),
        )
        .unwrap();

        upsert_layer(
            &json!({
                "project": dir.to_string_lossy(),
                "kind": "image", "file": shot.to_string_lossy(),
                "duration": 5.0,
                "placement": {"height": 640, "anchor": "center", "offset": [0, 40]}
            }),
            Some(&root),
        )
        .unwrap();

        upsert_layer(
            &json!({
                "project": dir.to_string_lossy(),
                "kind": "caption", "captionText": "Built by tools",
                "fontSize": 64.0, "duration": 5.0,
                "placement": {"anchor": "top", "offset": [0, 72]}
            }),
            Some(&root),
        )
        .unwrap();

        let meta = read(&dir);
        assert_eq!(meta.min_reader_version, Some(18));
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(warnings.is_empty(), "{warnings:?}");

        let layers = meta.layers.as_deref().unwrap();
        assert_eq!(layers.len(), 3, "background + picture + caption");
        let picture = layers.iter().find(|l| l.name == "Picture").unwrap();
        assert!(picture.keyframes[0].placement.is_some());
        assert!(
            uuid::Uuid::parse_str(&picture.id).is_ok(),
            "the server minted a UUID, as promised"
        );
        let caption = layers.iter().find(|l| l.name == "Caption").unwrap();
        let style = caption.caption_style.as_ref().unwrap();
        assert_eq!(style.font_size, Some(64.0));
        assert!(style.placement.is_some());
        assert!((meta.video_duration - 5.0).abs() < 1e-9, "covered");
        assert!(
            dir.join("Resources/shot.png").exists(),
            "media copied into the project"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The UPSERT half: the same id called again UPDATES, never duplicates —
    /// and the composition re-stretches around the change.
    #[test]
    fn calling_again_with_the_same_id_updates_in_place() {
        let root = scratch();
        let dir = root.join("Twice.promo");
        let shot = root.join("s.png");
        std::fs::write(&shot, b"bytes").unwrap();
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1280x720"}),
            None,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "caption",
                    "id": "HEADLINE", "captionText": "first", "duration": 3.0}),
            None,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "caption",
                    "id": "HEADLINE", "captionText": "second", "duration": 7.0}),
            None,
        )
        .unwrap();
        let meta = read(&dir);
        let captions: Vec<_> = meta
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .filter(|l| l.kind == ProjectLayerKind::Caption)
            .collect();
        assert_eq!(captions.len(), 1, "updated, not duplicated");
        assert_eq!(captions[0].caption_text.as_deref(), Some("second"));
        assert!((meta.video_duration - 7.0).abs() < 1e-9, "re-stretched");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// init never overwrites, and the fence holds for paths that do not
    /// exist yet.
    #[test]
    fn init_refuses_overwrite_and_the_fence_holds() {
        let root = scratch();
        let dir = root.join("Once.promo");
        let args = json!({"project": dir.to_string_lossy(), "canvas": "1280x720"});
        init(&args, Some(&root)).unwrap();
        let refused = init(&args, Some(&root)).unwrap_err();
        assert!(refused.contains("never overwrites"), "{refused}");

        let outside = std::env::temp_dir().join(format!("escapee-{}.promo", mint()));
        let err = init(
            &json!({"project": outside.to_string_lossy(), "canvas": "1280x720"}),
            Some(&root),
        )
        .unwrap_err();
        assert!(err.contains("outside the served root"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
