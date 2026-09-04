//! The scaffold over the .promo format: `promo_init` and
//! `promo_upsert_layer`, as ONE implementation for every server.
//!
//! The headless MCP server calls this directly; the apps reach it over the
//! C ABI (`promo_author_*` in promo-ffi) — so "the same tool" on two
//! servers is the same code, not two interpretations. The one thing a host
//! must bring is MEDIA PROBING: the server probes with ffprobe, an app
//! with its own stack (AVFoundation), injected as a closure — everything
//! else, the file layout included, is shared ground.
//!
//! The eventual home for mutation is `promo-editor`'s command system (the
//! core owns the document); this crate is the scaffold-shaped doorway
//! until those commands cover it.
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
    // Short ids are the author's vocabulary and the tool is an author:
    // a stated `id` is used verbatim, the background layer is always
    // "bg" (documented, and the file reads like the recipes), and only
    // what nobody named gets a canonical UUID.
    let project_id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(mint);
    let document = json!({
        "id": project_id, "name": name, "createdAt": 0, "state": "recorded",
        "minReaderVersion": 18,
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
        "compositionSettings": settings,
        "resources": [],
        "layers": [{
            "id": "bg", "name": "Background", "sortIndex": 0,
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

/// What a host knows about a media file it is staging. The server fills
/// this with ffprobe; an app with AVFoundation; a test with a literal.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    /// (width, height) in source pixels.
    pub pixels: Option<(f64, f64)>,
}

/// How a host measures media: handed the SOURCE path and whether the file
/// is being staged as video. Best effort — absent facts degrade exactly as
/// the format degrades (a video still needs a duration from somewhere).
pub type Probe<'a> = &'a dyn Fn(&Path, bool) -> MediaInfo;

/// Add or update one layer: image, video or caption, with a placement.
/// Media is COPIED into `Resources/`; a caption is words plus style. Every
/// call re-stretches the background and the composition to cover the
/// layers, so a project built one call at a time is coherent after each.
pub fn upsert_layer(args: &Value, root: Option<&Path>, probe: Probe) -> Result<String, String> {
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
    let start_time = args.get("startTime").and_then(Value::as_f64);
    let explicit_duration = args.get("duration").and_then(Value::as_f64);
    let fade_in = args.get("fadeIn").and_then(Value::as_f64);
    let frame = args.get("frame").cloned();
    if frame.is_some() && kind_name == "caption" {
        return Err("`frame` dresses media resources; a caption has none".into());
    }
    let placement: Option<Placement> = match args.get("placement") {
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|e| format!("placement: {e} (see promo_schema)"))?,
        ),
        None => None,
    };
    if let Some(rule) = &placement {
        if kind_name == "caption" && rule.sizes() {
            return Err(
                "a caption placement takes anchor and offset only — its size is fontSize".into(),
            );
        }
    }

    // Whether this call CREATES or UPDATES decides everything below: a
    // create is a template, an update touches only the fields the call
    // states, because the untouched half of an existing layer is someone's
    // work — a hand-added motion keyframe, a transition, a release pool.
    let wanted_id = args.get("id").and_then(Value::as_str).map(str::to_string);
    let existing_index = {
        let layers = meta.layers.get_or_insert_with(Vec::new);
        wanted_id
            .as_ref()
            .and_then(|id| layers.iter().position(|l| &l.id == id))
    };

    // The resource half: media is copied in when a `file` is given —
    // REQUIRED on create for image/video, optional on update (a repoint).
    let mut resource_note = String::new();
    let mut probed_duration = None;
    let file_arg = args.get("file").and_then(Value::as_str);
    if kind_name != "caption" && existing_index.is_none() && file_arg.is_none() {
        return Err("`file` is required when creating an image or video layer".into());
    }
    let resource_id = match (kind_name, file_arg) {
        ("caption", _) | (_, None) => None,
        (_, Some(file)) => {
            let source = PathBuf::from(file);
            if !source.exists() {
                return Err(format!("file {} does not exist", source.display()));
            }
            let filename = copy_into_resources(&dir, &source)?;
            let id = args
                .get("resourceId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(mint);
            let mut record = json!({
                "id": id, "kind": kind_name, "filename": filename,
                "displayName": source.file_stem().unwrap_or_default().to_string_lossy(),
                "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": []
            });
            // The source's own pixels, stamped the way the app stamps them
            // on import: placement anchors and widths resolve against the
            // aspect, and an unmeasured source is positioned as a SQUARE —
            // which the validator names. The HOST measures (ffprobe here,
            // AVFoundation in an app); this code only writes what it learns.
            let info = probe(&source, kind_name == "video");
            if let Some((w, h)) = info.pixels {
                if kind_name == "video" {
                    record["videoNaturalWidth"] = json!(w);
                    record["videoNaturalHeight"] = json!(h);
                } else {
                    record["pixelWidth"] = json!(w);
                    record["pixelHeight"] = json!(h);
                }
            }
            if kind_name == "video" {
                let seconds = explicit_duration.or(info.duration).ok_or_else(|| {
                    format!(
                        "no duration for {} — pass `duration`, or probe the file",
                        source.display()
                    )
                })?;
                probed_duration = Some(seconds);
                record["duration"] = json!(seconds);
                record["trimStart"] = json!(0);
                record["trimEnd"] = json!(seconds);
            }
            if let Some(dress) = &frame {
                record["frame"] = dress.clone();
            }
            let resource: ProjectResource = serde_json::from_value(record)
                .map_err(|e| format!("resource template rejected by the format: {e}"))?;
            resource_note = format!(", resource {id} ({filename})");
            meta.resources.get_or_insert_with(Vec::new).push(resource);
            Some(id)
        }
    };

    let layers = meta.layers.get_or_insert_with(Vec::new);
    let next_sort = layers.iter().map(|l| l.sort_index + 1).max().unwrap_or(0);
    let layer_id = wanted_id.unwrap_or_else(mint);

    match existing_index {
        // UPDATE: mutate exactly what was stated. Nothing here rebuilds the
        // layer — the first version of this tool did, and "nudge the card
        // 20px" deleted the push-in keyframe someone had added by hand.
        Some(i) => {
            let wire_kind: ProjectLayerKind =
                serde_json::from_value(json!(kind_name)).map_err(|e| e.to_string())?;
            if layers[i].kind != wire_kind {
                return Err(format!(
                    "layer {layer_id} is a {:?} — upsert cannot change a layer's kind",
                    layers[i].kind
                ));
            }
            if let Some(name) = args.get("name").and_then(Value::as_str) {
                layers[i].name = name.to_string();
            }
            if let Some(t) = start_time {
                layers[i].start_time = t;
            }
            if let Some(d) = explicit_duration.or(probed_duration) {
                layers[i].duration = Some(d);
            }
            if let Some(f) = fade_in {
                layers[i].fade_in = Some(f);
            }
            if let Some(id) = &resource_id {
                layers[i].resource_id = Some(id.clone());
            }
            // A frame with no new file dresses the resource the layer
            // already shows.
            if let (Some(dress), None) = (&frame, &resource_id) {
                let shown = layers[i]
                    .resource_id
                    .clone()
                    .ok_or("`frame` needs a resource — this layer shows none")?;
                let parsed = serde_json::from_value(dress.clone())
                    .map_err(|e| format!("frame: {e} (see promo_schema)"))?;
                let resources = meta.resources.get_or_insert_with(Vec::new);
                let resource = resources
                    .iter_mut()
                    .find(|r| r.id == shown)
                    .ok_or("the layer's resource is not in this project")?;
                resource.frame = Some(parsed);
            }
            if kind_name == "caption" {
                if let Some(words) = args.get("captionText").and_then(Value::as_str) {
                    layers[i].caption_text = Some(words.to_string());
                }
                if placement.is_some() || args.get("fontSize").is_some() {
                    let mut style = layers[i].caption_style.take().unwrap_or_default();
                    if let Some(rule) = &placement {
                        style.placement = Some(rule.clone());
                    }
                    if let Some(size) = args.get("fontSize").and_then(Value::as_f64) {
                        style.font_size = Some(size);
                    }
                    layers[i].caption_style = Some(style);
                }
            } else if let Some(rule) = &placement {
                // Placement MERGES into the earliest keyframe; every other
                // keyframe — the motion someone added in JSON — survives.
                match layers[i]
                    .keyframes
                    .iter_mut()
                    .min_by(|a, b| a.time.total_cmp(&b.time))
                {
                    Some(first) => first.placement = Some(rule.clone()),
                    None => {
                        let keyframe: promo_model::ProjectLayerKeyframe =
                            serde_json::from_value(json!({
                                "id": mint(), "time": 0, "transitionDuration": 0,
                                "placement": serde_json::to_value(rule)
                                    .map_err(|e| e.to_string())?
                            }))
                            .map_err(|e| format!("keyframe: {e}"))?;
                        layers[i].keyframes.push(keyframe);
                    }
                }
            }
        }
        // CREATE: the template, decoded by the format's own parser.
        None => {
            let mut record = json!({
                "id": layer_id, "name": args
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(match kind_name {
                        "caption" => "Caption",
                        "video" => "Clip",
                        _ => "Picture",
                    }),
                "sortIndex": next_sort, "kind": kind_name, "isEnabled": true,
                "startTime": start_time.unwrap_or(0.0),
                "duration": explicit_duration.or(probed_duration).unwrap_or(3.0),
                "keyframes": []
            });
            if let Some(id) = &resource_id {
                record["resourceID"] = json!(id);
            }
            if let Some(f) = fade_in {
                record["fadeIn"] = json!(f);
            }
            if kind_name == "caption" {
                if let Some(words) = args.get("captionText").and_then(Value::as_str) {
                    record["captionText"] = json!(words);
                }
                let mut style = json!({});
                if let Some(rule) = &placement {
                    style["placement"] = serde_json::to_value(rule).map_err(|e| e.to_string())?;
                }
                if let Some(size) = args.get("fontSize").and_then(Value::as_f64) {
                    style["fontSize"] = json!(size);
                }
                record["captionStyle"] = style;
            } else if let Some(rule) = &placement {
                record["keyframes"] = json!([{
                    "id": mint(), "time": 0, "transitionDuration": 0,
                    "placement": serde_json::to_value(rule).map_err(|e| e.to_string())?
                }]);
            }
            let layer: ProjectLayer = serde_json::from_value(record)
                .map_err(|e| format!("layer template rejected by the format: {e}"))?;
            layers.push(layer);
        }
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
    // Issue #9: the layout findings for THIS layer ride the reply, so an
    // agent fixes a flush caption in the same turn.
    let notes = layout_notes(&meta, Some(&layer_id));
    Ok(format!(
        "upserted layer {layer_id}{resource_note}; composition runs {end}s{notes}"
    ))
}

/// Layout warnings (a caption at an edge, under a picture; a viewport
/// cropping a plate) as lines under a reply, or nothing.
fn layout_notes(meta: &ProjectMetadata, only: Option<&str>) -> String {
    let found = promo_timeline::layout_check::layout_warnings_for(meta, only);
    if found.is_empty() {
        String::new()
    } else {
        let mut out = String::from("\nlayout:");
        for w in found {
            out.push_str("\n  - ");
            out.push_str(&w);
        }
        out
    }
}

/// The motion half of the scaffold (issue #1): one keyframe, created or
/// merged, in the format's own language — a second placement keyframe is a
/// push-in, viewport keyframes are a Ken Burns, colorHex keyframes ramp a
/// background. UPDATE touches only the stated fields, exactly the
/// discipline `upsert_layer` learned the hard way; CREATE without a stated
/// ramp defaults `transitionDuration` to span from the previous keyframe —
/// "ramp the whole way there" is what a push-in means, and a stated 0 is
/// preserved. Deeper structures — swaps, waits, motion paths — stay
/// ordinary JSON edits.
pub fn upsert_keyframe(args: &Value, root: Option<&Path>) -> Result<String, String> {
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

    let layer_id = required_str(args, "layer")?.to_string();
    let layers = meta.layers.get_or_insert_with(Vec::new);
    let layer = layers
        .iter_mut()
        .find(|l| l.id == layer_id)
        .ok_or_else(|| format!("no layer `{layer_id}` — promo_inspect lists the ids"))?;

    if let Some(spelling) = args.get("easing").and_then(Value::as_str) {
        if !matches!(spelling, "linear" | "easeIn" | "easeOut" | "easeInOut") {
            return Err(format!(
                "easing `{spelling}` — linear, easeIn, easeOut or easeInOut \
                 (the renderer would degrade a typo to linear silently; \
                 this tool refuses it instead)"
            ));
        }
    }
    let placement: Option<Placement> = match args.get("placement") {
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|e| format!("placement: {e} (see promo_schema)"))?,
        ),
        None => None,
    };
    let viewport: Option<[f64; 4]> = match args.get("viewport") {
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|_| "viewport is [x, y, w, h], unit source coordinates".to_string())?,
        ),
        None => None,
    };
    let time = args.get("time").and_then(Value::as_f64);

    let wanted_id = args.get("id").and_then(Value::as_str).map(str::to_string);
    let existing = wanted_id
        .as_ref()
        .and_then(|id| layer.keyframes.iter().position(|k| &k.id == id));

    let number = |key: &str| -> Option<f64> { args.get(key).and_then(Value::as_f64) };
    let (keyframe_id, moment) = match existing {
        Some(i) => {
            let k = &mut layer.keyframes[i];
            if let Some(t) = time {
                k.time = t;
            }
            if let Some(rule) = placement {
                k.placement = Some(rule);
            }
            if let Some(window) = viewport {
                k.viewport = Some(window);
            }
            if let Some(v) = number("opacity") {
                k.opacity = Some(v);
            }
            if let Some(v) = number("zoom") {
                k.zoom = Some(v);
            }
            if let Some(v) = number("fontSize") {
                k.font_size = Some(v);
            }
            if let Some(v) = number("transitionDuration") {
                k.transition_duration = v;
            }
            if let Some(v) = number("tiltX") {
                k.tilt_x = Some(v);
            }
            if let Some(v) = number("tiltY") {
                k.tilt_y = Some(v);
            }
            if let Some(hex) = args.get("colorHex").and_then(Value::as_str) {
                k.color_hex = Some(hex.to_string());
            }
            if let Some(spelling) = args.get("easing") {
                k.easing =
                    Some(serde_json::from_value(spelling.clone()).map_err(|e| e.to_string())?);
            }
            (k.id.clone(), k.time)
        }
        None => {
            let t = time.ok_or("`time` is required when creating a keyframe")?;
            // The default ramp: from the previous keyframe all the way here.
            let previous = layer
                .keyframes
                .iter()
                .map(|k| k.time)
                .filter(|earlier| *earlier < t)
                .fold(f64::NEG_INFINITY, f64::max);
            let ramp = number("transitionDuration").unwrap_or(if previous.is_finite() {
                t - previous
            } else {
                0.0
            });
            let mut record = json!({
                "id": wanted_id.clone().unwrap_or_else(mint),
                "time": t, "transitionDuration": ramp,
            });
            for key in ["opacity", "zoom", "fontSize", "tiltX", "tiltY"] {
                if let Some(v) = number(key) {
                    record[key] = json!(v);
                }
            }
            if let Some(rule) = &placement {
                record["placement"] = serde_json::to_value(rule).map_err(|e| e.to_string())?;
            }
            if let Some(window) = viewport {
                record["viewport"] = json!(window);
            }
            if let Some(hex) = args.get("colorHex").and_then(Value::as_str) {
                record["colorHex"] = json!(hex);
            }
            if let Some(spelling) = args.get("easing") {
                record["easing"] = spelling.clone();
            }
            let keyframe: promo_model::ProjectLayerKeyframe =
                serde_json::from_value(record).map_err(|e| format!("keyframe: {e}"))?;
            let id = keyframe.id.clone();
            layer.keyframes.push(keyframe);
            (id, t)
        }
    };
    // Keyframes read in time order; hand-authored order is preserved for
    // equal times (stable sort).
    layer.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
    let count = layer.keyframes.len();

    write_metadata(&meta, &meta_path)?;
    Ok(format!(
        "upserted keyframe {keyframe_id} on layer {layer_id} at {moment}s — \
         the layer has {count} keyframe(s)"
    ))
}

/// The whole vocabulary through one door (REVIEW-2026-09 P2): a batch of
/// promo-editor `Command`s applied to the file as ONE atomic group — every
/// command succeeds or the file is untouched — through the same `Document`
/// the editors use, so an agent's "delete this layer, move that one under
/// it, give it a wipe" is the exact mutation a person's gesture would be.
/// The scaffold tools stay for what they teach; this is for everything
/// else the format can say.
pub fn apply(args: &Value, root: Option<&Path>) -> Result<String, String> {
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

    let raw = args
        .get("commands")
        .and_then(Value::as_array)
        .ok_or("`commands` is required — an array of {\"kind\": ..., ...}")?;
    if raw.is_empty() {
        return Err("`commands` is empty — nothing to apply".into());
    }
    let kinds: Vec<String> = raw
        .iter()
        .map(|c| {
            c.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        })
        .collect();
    let commands: Vec<promo_editor::Command> = raw
        .iter()
        .enumerate()
        .map(|(i, c)| {
            serde_json::from_value(c.clone())
                .map_err(|e| format!("commands[{i}] ({}): {e} — see the tool's schema", kinds[i]))
        })
        .collect::<Result<_, _>>()?;

    let mut document = promo_editor::Document::open(&text)?;
    document
        .apply_group(&commands)
        .map_err(|e| format!("nothing applied — {e}"))?;
    let meta = ProjectMetadata::from_json(&document.to_json()?)
        .map_err(|e| format!("the edited document no longer parses: {e}"))?;
    write_metadata(&meta, &meta_path)?;
    let notes = layout_notes(&meta, None);
    Ok(format!(
        "applied {} command(s) as one step: {}{notes}",
        commands.len(),
        kinds.join(", ")
    ))
}

/// The wizard, for agents (REVIEW-2026-09 A2): pictures and clips in, a
/// complete show out — the SAME `promo_editor::author` the Windows wizard
/// runs, ported rule for rule from the Mac wizard. This function is the
/// host half the core deliberately leaves out: the folder, the copies into
/// Resources/, the probing (pixels; a clip's length is the file's), and the
/// spec assembled from only what the call states so the core's defaults
/// decide the rest. Never overwrites.
pub fn slideshow(args: &Value, root: Option<&Path>, probe: Probe) -> Result<String, String> {
    let dir = PathBuf::from(required_str(args, "project")?);
    fence_new_path(&dir, root)?;
    let meta_path = dir.join("metadata.json");
    if meta_path.exists() {
        return Err(format!(
            "{} already exists — promo_slideshow creates, never overwrites",
            meta_path.display()
        ));
    }
    let wanted = args
        .get("slides")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or("`slides` is required — [{file, caption?, duration?, looped?}, ...]")?;
    std::fs::create_dir_all(dir.join("Resources"))
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    let mut slides = Vec::with_capacity(wanted.len());
    for (i, slide) in wanted.iter().enumerate() {
        let file = slide
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("slides[{i}].file is required"))?;
        let source = PathBuf::from(file);
        if !source.exists() {
            return Err(format!(
                "slides[{i}]: file {} does not exist",
                source.display()
            ));
        }
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let is_video = matches!(extension.as_str(), "mp4" | "mov" | "m4v" | "webm" | "mkv");
        let filename = copy_into_resources(&dir, &source)?;
        let info = probe(&source, is_video);
        let mut record = json!({
            "filename": filename,
            "kind": if is_video { "video" } else { "image" },
        });
        if let Some((w, h)) = info.pixels {
            record["pixelWidth"] = json!(w);
            record["pixelHeight"] = json!(h);
        }
        // Seconds on screen: stated, else a clip's own length (the file is
        // the answer), else the wizard's default for a picture.
        if let Some(seconds) = slide.get("duration").and_then(Value::as_f64) {
            record["duration"] = json!(seconds);
        } else if is_video {
            if let Some(seconds) = info.duration {
                record["duration"] = json!(seconds);
            }
        }
        for key in ["caption", "displayName", "transitionDuration", "looped"] {
            if let Some(v) = slide.get(key) {
                record[key] = v.clone();
            }
        }
        slides.push(record);
    }

    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            dir.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Show".into())
        });
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut spec = json!({ "name": name, "createdAt": created_at, "slides": slides });
    for key in [
        "kind",
        "transition",
        "transitionEdge",
        "direction",
        "sizing",
        "device",
        "framing",
        "backgroundColorHex",
    ] {
        if let Some(v) = args.get(key) {
            spec[key] = v.clone();
        }
    }
    if args.get("canvas").is_some() {
        let (w, h) = canvas_size(args)?;
        spec["canvasWidth"] = json!(w);
        spec["canvasHeight"] = json!(h);
    }
    let kind = spec["kind"].as_str().unwrap_or("classic").to_string();
    let spec: promo_editor::author::AuthorSpec =
        serde_json::from_value(spec).map_err(|e| format!("slideshow: {e}"))?;
    let authored = promo_editor::author::author(&spec)?;
    let mut meta = ProjectMetadata::from_json(&authored)
        .map_err(|e| format!("the show the wizard wrote no longer parses: {e}"))?;
    meta.min_reader_version = Some(18);
    write_metadata(&meta, &meta_path)?;
    Ok(format!(
        "authored a {kind} show at {}: {} slide(s), {:.1}s — refine with the tools, or open it",
        dir.display(),
        wanted.len(),
        meta.video_duration
    ))
}

/// The fenced, existing project: its folder and its metadata text.
fn open_existing(
    args: &Value,
    key: &str,
    root: Option<&Path>,
) -> Result<(PathBuf, String), String> {
    let raw = required_str(args, key)?;
    let mut dir = std::fs::canonicalize(raw).map_err(|e| format!("{key}: {e}"))?;
    if dir.is_file() {
        dir = dir.parent().map(Path::to_path_buf).unwrap_or(dir);
    }
    if let Some(root) = root {
        let root = std::fs::canonicalize(root).map_err(|e| format!("--root: {e}"))?;
        if !dir.starts_with(&root) {
            return Err(format!(
                "{key} is outside the served root {}",
                root.display()
            ));
        }
    }
    let text = std::fs::read_to_string(dir.join("metadata.json"))
        .map_err(|_| format!("no metadata.json in {}", dir.display()))?;
    Ok((dir, text))
}

/// The agent's debugger (REVIEW-2026-09 P3): "why is this layer where it
/// is" answered with the numbers the renderer actually uses at one moment
/// — the same `promo-timeline` functions the engine calls, so the answer
/// cannot disagree with the pixels. Per layer: whether it is visible and
/// why not, the resource it shows (swap-aware), the resolved transform and
/// the RECT on the canvas in pixels, opacity, rotation, tilt, viewport,
/// gain, and the keyframes bracketing the moment; per project: the timing
/// resolver's problems and validate's warnings. JSON, machine-first.
pub fn explain(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let (_, text) = open_existing(args, "project", root)?;
    let meta = ProjectMetadata::from_json(&text).map_err(|e| format!("decode: {e}"))?;
    let settings = &meta.composition_settings;
    let canvas = promo_model::Size::new(settings.canvas_width, settings.canvas_height);
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let only = args.get("layer").and_then(Value::as_str);
    let time = args.get("time").and_then(Value::as_f64).unwrap_or_else(|| {
        // The composition's midpoint: never t=0, where fade-ins hide everything.
        meta.video_duration / 2.0
    });
    if let Some(id) = only {
        if !layers.iter().any(|l| l.id == id) {
            return Err(format!("no layer `{id}` — promo_inspect lists the ids"));
        }
    }

    let mut docs = Vec::new();
    for layer in layers.iter().filter(|l| only.is_none_or(|id| l.id == id)) {
        let local = promo_timeline::layer_local_time(layer, time);
        let end = layer.duration.map(|d| layer.start_time + d);
        let visible = promo_timeline::layer_is_visible(layer, time);
        let why_hidden = if visible {
            None
        } else if !layer.is_enabled {
            Some("disabled")
        } else if time < layer.start_time {
            Some("not started yet")
        } else {
            Some("already ended")
        };
        // The resource shown at this moment: the latest swap keyframe at or
        // before the local time, else the layer's own.
        let showing = layer
            .keyframes
            .iter()
            .filter(|k| k.time <= local && k.resource_id.is_some())
            .max_by(|a, b| a.time.total_cmp(&b.time))
            .and_then(|k| k.resource_id.clone())
            .or_else(|| layer.resource_id.clone());
        let resource = showing
            .as_ref()
            .and_then(|id| resources.iter().find(|r| &r.id == id));
        let tr = promo_timeline::layer_transform_along_paths(layer, time, settings, resources);
        let source = resource.and_then(promo_timeline::layout::resource_source_size);
        let framed = source.map(|size| {
            promo_timeline::framed_pixel_size(size, resource.and_then(|r| r.frame.as_ref()))
        });
        let rect = framed.map(|size| {
            let r = if layer.kind == ProjectLayerKind::Drawing {
                promo_timeline::drawing_rect(
                    size,
                    canvas,
                    tr.zoom,
                    tr.horizontal_shift,
                    tr.vertical_shift,
                )
            } else {
                promo_timeline::media_rect(
                    size,
                    canvas,
                    tr.zoom,
                    tr.horizontal_shift,
                    tr.vertical_shift,
                )
            };
            json!({ "x": r.x(), "y": r.y(), "width": r.width(), "height": r.height() })
        });
        let sorted: Vec<_> = {
            let mut k: Vec<_> = layer.keyframes.iter().collect();
            k.sort_by(|a, b| a.time.total_cmp(&b.time));
            k
        };
        let previous = sorted.iter().rev().find(|k| k.time <= local);
        let next = sorted.iter().find(|k| k.time > local);
        let brief = |k: &&promo_model::ProjectLayerKeyframe| json!({ "id": k.id, "time": k.time, "transitionDuration": k.transition_duration });
        let default_gain = resource.map(|r| r.effective_volume()).unwrap_or(1.0);
        let caption = if layer.kind == ProjectLayerKind::Caption {
            Some(json!({
                "text": layer.caption_text,
                "placement": layer.caption_style.as_ref().and_then(|s| s.placement.clone()),
                "fontSize": layer.caption_style.as_ref().and_then(|s| s.font_size),
            }))
        } else {
            None
        };
        docs.push(json!({
            "id": layer.id,
            "name": layer.name,
            "kind": serde_json::to_value(layer.kind).unwrap_or(Value::Null),
            "visible": visible,
            "whyHidden": why_hidden,
            "life": { "start": layer.start_time, "end": end },
            "localTime": local,
            "showing": showing,
            "source": framed.map(|s| json!({ "width": s.width(), "height": s.height(),
                "framed": resource.and_then(|r| r.frame.as_ref()).is_some() })),
            "transform": { "zoom": tr.zoom, "horizontalShift": tr.horizontal_shift,
                           "verticalShift": tr.vertical_shift },
            "rect": rect,
            "opacity": promo_timeline::layer_opacity(layer, time),
            "rotation": promo_timeline::layer_rotation(layer, time),
            "tilt": promo_timeline::layer_tilt_offset(layer, time).map(|(x, y)| json!([x, y])),
            "viewport": promo_timeline::layer_viewport(layer, time),
            "gain": promo_timeline::layer_gain(layer, local, default_gain),
            "caption": caption,
            "keyframes": { "count": layer.keyframes.len(),
                           "previous": previous.map(brief), "next": next.map(brief) },
            "transitionIn": layer.transition_in.as_ref().map(|t| serde_json::to_value(t).unwrap_or(Value::Null)),
            "transitionOut": layer.transition_out.as_ref().map(|t| serde_json::to_value(t).unwrap_or(Value::Null)),
            "fadeIn": layer.fade_in, "fadeOut": layer.fade_out,
        }));
    }
    let mut timing_copy = meta.clone();
    let timing_problems: Vec<String> = promo_timeline::resolve_attachments(&mut timing_copy)
        .iter()
        .map(|p| format!("{p:?}"))
        .collect();
    let out = json!({
        "time": time,
        "canvas": { "width": settings.canvas_width, "height": settings.canvas_height },
        "duration": meta.video_duration,
        "layers": docs,
        "timingProblems": timing_problems,
        "warnings": promo_timeline::validate::warnings(&meta),
    });
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// "What changed since I last looked" in the format's own terms: two
/// projects (or a project and another's metadata.json) compared by
/// entity — settings by key, resources and layers by id, keyframes by id —
/// as plain lines an agent can act on. The other author's turn, made
/// legible.
pub fn diff(args: &Value, root: Option<&Path>) -> Result<String, String> {
    let (_, before_text) = open_existing(args, "against", root)?;
    let (_, after_text) = open_existing(args, "project", root)?;
    let before: Value = serde_json::from_str(&before_text).map_err(|e| e.to_string())?;
    let after: Value = serde_json::from_str(&after_text).map_err(|e| e.to_string())?;
    let mut lines = Vec::new();

    let show = |v: &Value| {
        let text = v.to_string();
        if text.len() > 60 {
            format!("{}…", &text[..57])
        } else {
            text
        }
    };
    // Settings, key by key.
    let (bs, as_) = (
        before
            .get("compositionSettings")
            .cloned()
            .unwrap_or(json!({})),
        after
            .get("compositionSettings")
            .cloned()
            .unwrap_or(json!({})),
    );
    if let (Some(b), Some(a)) = (bs.as_object(), as_.as_object()) {
        let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            match (b.get(key), a.get(key)) {
                (Some(x), Some(y)) if x != y => {
                    lines.push(format!("settings.{key}: {} → {}", show(x), show(y)))
                }
                (None, Some(y)) => lines.push(format!("settings.{key}: added {}", show(y))),
                (Some(x), None) => lines.push(format!("settings.{key}: removed (was {})", show(x))),
                _ => {}
            }
        }
    }
    for (family, singular) in [("resources", "resource"), ("layers", "layer")] {
        let by_id = |doc: &Value| -> Vec<(String, Value)> {
            doc.get(family)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|i| {
                            (
                                i.get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string(),
                                i.clone(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let (b, a) = (by_id(&before), by_id(&after));
        for (id, item) in &a {
            match b.iter().find(|(bid, _)| bid == id) {
                None => lines.push(format!(
                    "{singular} {id}: added ({})",
                    item.get("name")
                        .or(item.get("displayName"))
                        .and_then(Value::as_str)
                        .unwrap_or("unnamed")
                )),
                Some((_, old)) if old != item => {
                    if let (Some(o), Some(n)) = (old.as_object(), item.as_object()) {
                        let mut keys: Vec<&String> = o.keys().chain(n.keys()).collect();
                        keys.sort();
                        keys.dedup();
                        for key in keys {
                            if key == "keyframes" {
                                let ids = |v: Option<&Value>| -> Vec<String> {
                                    v.and_then(Value::as_array)
                                        .map(|ks| {
                                            ks.iter()
                                                .filter_map(|k| k.get("id").and_then(Value::as_str))
                                                .map(str::to_string)
                                                .collect()
                                        })
                                        .unwrap_or_default()
                                };
                                let (oi, ni) = (ids(o.get(key)), ids(n.get(key)));
                                for k in ni.iter().filter(|k| !oi.contains(k)) {
                                    lines.push(format!("{singular} {id}: keyframe {k} added"));
                                }
                                for k in oi.iter().filter(|k| !ni.contains(k)) {
                                    lines.push(format!("{singular} {id}: keyframe {k} removed"));
                                }
                                if oi == ni && o.get(key) != n.get(key) {
                                    lines.push(format!("{singular} {id}: keyframes changed"));
                                }
                                continue;
                            }
                            match (o.get(key), n.get(key)) {
                                (Some(x), Some(y)) if x != y => lines.push(format!(
                                    "{singular} {id}.{key}: {} → {}",
                                    show(x),
                                    show(y)
                                )),
                                (None, Some(y)) => {
                                    lines.push(format!("{singular} {id}.{key}: added {}", show(y)))
                                }
                                (Some(x), None) => lines.push(format!(
                                    "{singular} {id}.{key}: removed (was {})",
                                    show(x)
                                )),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (id, item) in &b {
            if !a.iter().any(|(aid, _)| aid == id) {
                lines.push(format!(
                    "{singular} {id}: removed ({})",
                    item.get("name")
                        .or(item.get("displayName"))
                        .and_then(Value::as_str)
                        .unwrap_or("unnamed")
                ));
            }
        }
    }
    for key in ["name", "trimEnd", "videoDuration", "updatedAt"] {
        if before.get(key) != after.get(key) {
            lines.push(format!(
                "{key}: {} → {}",
                show(before.get(key).unwrap_or(&Value::Null)),
                show(after.get(key).unwrap_or(&Value::Null))
            ));
        }
    }
    if lines.is_empty() {
        return Ok("no differences".into());
    }
    Ok(format!(
        "{} difference(s):\n  {}",
        lines.len(),
        lines.join("\n  ")
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

    /// The probe a test injects: a literal, which is the point of the
    /// seam — these tests hold the DOCUMENT logic, not a host's media
    /// stack, so they run identically with or without ffmpeg installed.
    fn measured(_: &Path, video: bool) -> MediaInfo {
        MediaInfo {
            duration: video.then_some(4.0),
            pixels: Some((800.0, 600.0)),
        }
    }

    /// A 1x1 PNG fixture: the bytes only need to EXIST — measuring is the
    /// injected probe's job now.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

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
            &measured,
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
            &measured,
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

    /// The clobber case, verbatim from review: add a push-in keyframe by
    /// hand, then "nudge the card 20px" through the tool — the motion must
    /// SURVIVE, the placement must merge into the first keyframe, and the
    /// timing the call never mentioned must stay put.
    #[test]
    fn a_nudge_survives_the_hand_added_motion() {
        let root = scratch();
        let dir = root.join("Nudge.promo");
        let shot = root.join("s.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1920x1080"}),
            None,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image",
                    "id": "CARD", "file": shot.to_string_lossy(),
                    "startTime": 1.0, "duration": 6.0,
                    "placement": {"height": 640, "anchor": "center"}}),
            None,
            &measured,
        )
        .unwrap();

        // The hand edit: a second keyframe — the push-in — in ordinary JSON.
        let meta_path = dir.join("metadata.json");
        let mut meta =
            ProjectMetadata::from_json(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        {
            let layers = meta.layers.as_mut().unwrap();
            let card = layers.iter_mut().find(|l| l.id == "CARD").unwrap();
            let motion: promo_model::ProjectLayerKeyframe = serde_json::from_value(json!({
                "id": "K1", "time": 5.5, "transitionDuration": 5.0,
                "easing": "easeInOut",
                "placement": {"height": 720, "anchor": "center"}
            }))
            .unwrap();
            card.keyframes.push(motion);
        }
        std::fs::write(&meta_path, meta.to_json().unwrap()).unwrap();

        // The nudge: same id, ONLY a new placement offset.
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image",
                    "id": "CARD",
                    "placement": {"height": 640, "anchor": "center",
                                   "offset": [20, 0]}}),
            None,
            &measured,
        )
        .unwrap();

        let meta = read(&dir);
        let card = meta
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .find(|l| l.id == "CARD")
            .unwrap();
        assert_eq!(card.keyframes.len(), 2, "the push-in survived");
        let first = card
            .keyframes
            .iter()
            .min_by(|a, b| a.time.total_cmp(&b.time))
            .unwrap();
        assert_eq!(
            first.placement.as_ref().unwrap().offset,
            Some([20.0, 0.0]),
            "the nudge landed on the first keyframe"
        );
        let motion = card.keyframes.iter().find(|k| k.id == "K1").unwrap();
        assert_eq!(
            motion.placement.as_ref().unwrap().height,
            Some(720.0),
            "the motion keyframe is untouched"
        );
        assert!((card.start_time - 1.0).abs() < 1e-9, "unstated timing kept");
        assert_eq!(card.duration, Some(6.0), "unstated duration kept");
        assert_eq!(
            meta.resources.as_deref().unwrap().len(),
            1,
            "no phantom re-copy of the media"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The scaffold fields the product-promo recipe needs: a device frame
    /// on the resource and a fadeIn on the layer — and a kind change is a
    /// refusal, not a rebuild.
    #[test]
    fn frame_and_fade_scaffold_and_kind_is_identity() {
        let root = scratch();
        let dir = root.join("Framed.promo");
        let shot = root.join("s.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1920x1080",
                    "palette": {"canvas": "10182B", "edge": "26364F"}}),
            None,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image",
                    "id": "CARD", "file": shot.to_string_lossy(),
                    "fadeIn": 0.4,
                    "frame": {"kind": "device", "material": "spaceBlack",
                               "tiltY": 10},
                    "placement": {"height": 640, "anchor": "center"}}),
            None,
            &measured,
        )
        .unwrap();
        let meta = read(&dir);
        // The device FRAME is legacy 2.5D: the validator names it (and only
        // it); the scaffold still lands, so old recipes keep working.
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(
            warnings.len() == 1 && warnings[0].contains("legacy 2.5D"),
            "{warnings:?}"
        );
        let card = meta
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .find(|l| l.id == "CARD")
            .unwrap();
        assert_eq!(card.fade_in, Some(0.4));
        let resource = &meta.resources.as_deref().unwrap()[0];
        let frame = resource.frame.as_ref().expect("frame landed");
        assert!((frame.tilt_y - 10.0).abs() < 1e-9);

        let refused = upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "caption",
                    "id": "CARD", "captionText": "nope"}),
            None,
            &measured,
        )
        .unwrap_err();
        assert!(
            refused.contains("cannot change a layer's kind"),
            "{refused}"
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
            &measured,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "caption",
                    "id": "HEADLINE", "captionText": "second", "duration": 7.0}),
            None,
            &measured,
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

    /// Short ids are the author's vocabulary and the tools are authors:
    /// stated ids land verbatim — project, layer, resource — the
    /// background is always "bg", and only the unnamed gets a UUID.
    #[test]
    fn stated_short_ids_land_verbatim() {
        let root = scratch();
        let dir = root.join("Short.promo");
        let shot = root.join("s.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1280x720",
                    "id": "card"}),
            None,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image",
                    "id": "shot", "resourceId": "shot-media",
                    "file": shot.to_string_lossy(),
                    "placement": {"height": 400, "anchor": "center"}}),
            None,
            &measured,
        )
        .unwrap();
        let meta = read(&dir);
        assert_eq!(meta.id, "card");
        let layers = meta.layers.as_deref().unwrap();
        assert!(
            layers.iter().any(|l| l.id == "bg"),
            "the ground reads as bg"
        );
        let layer = layers.iter().find(|l| l.id == "shot").expect("verbatim");
        assert_eq!(layer.resource_id.as_deref(), Some("shot-media"));
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(warnings.is_empty(), "{warnings:?}");
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

    /// Issue #1's done-when, as a test: schema → init → upsert_layer →
    /// upsert_keyframe builds the ProductCard push-in — a second placement
    /// keyframe with the default ramp spanning from the first — without a
    /// hand JSON edit, and the file validates clean.
    #[test]
    fn a_keyframe_upsert_builds_the_push_in_without_hand_json() {
        let root = scratch();
        let dir = root.join("Push.promo");
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1920x1080",
                    "palette": {"canvas": "10182B"}}),
            Some(&root),
        )
        .unwrap();
        let shot = root.join("shot.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image", "id": "card",
                    "file": shot.to_string_lossy(), "duration": 6,
                    "placement": {"height": 620, "anchor": "center"}}),
            Some(&root),
            &measured,
        )
        .unwrap();

        let answer = upsert_keyframe(
            &json!({"project": dir.to_string_lossy(), "layer": "card",
                    "id": "k1", "time": 6, "zoom": 1.25, "easing": "easeInOut"}),
            Some(&root),
        )
        .unwrap();
        assert!(answer.contains("k1"), "{answer}");

        let meta = read(&dir);
        let card = meta
            .layers
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.id == "card")
            .unwrap();
        assert_eq!(card.keyframes.len(), 2, "placement kf0 plus the push-in");
        let k1 = card.keyframes.iter().find(|k| k.id == "k1").unwrap();
        assert_eq!(k1.zoom, Some(1.25));
        assert!(
            (k1.transition_duration - 6.0).abs() < 1e-9,
            "the default ramp spans from the previous keyframe: {}",
            k1.transition_duration
        );
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The FocusPush shape: viewport keyframes make a Ken Burns through the
    /// tool. And an UPDATE touches only what it states — the viewport and
    /// ramp survive an opacity nudge.
    #[test]
    fn viewport_keyframes_ride_and_updates_never_clobber() {
        let root = scratch();
        let dir = root.join("Ken.promo");
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1920x1080"}),
            Some(&root),
        )
        .unwrap();
        let shot = root.join("rec.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        upsert_layer(
            &json!({"project": dir.to_string_lossy(), "kind": "image", "id": "rec",
                    "file": shot.to_string_lossy(), "duration": 8}),
            Some(&root),
            &measured,
        )
        .unwrap();
        for (id, time, window) in [
            ("v0", 0.0, [0.0, 0.0, 1.0, 1.0]),
            ("v1", 8.0, [0.25, 0.25, 0.5, 0.5]),
        ] {
            upsert_keyframe(
                &json!({"project": dir.to_string_lossy(), "layer": "rec",
                        "id": id, "time": time, "viewport": window}),
                Some(&root),
            )
            .unwrap();
        }
        upsert_keyframe(
            &json!({"project": dir.to_string_lossy(), "layer": "rec",
                    "id": "v1", "opacity": 0.9}),
            Some(&root),
        )
        .unwrap();

        let meta = read(&dir);
        let rec = meta
            .layers
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.id == "rec")
            .unwrap();
        let v1 = rec.keyframes.iter().find(|k| k.id == "v1").unwrap();
        assert_eq!(
            v1.viewport,
            Some([0.25, 0.25, 0.5, 0.5]),
            "the ride survives"
        );
        assert_eq!(v1.opacity, Some(0.9), "the nudge landed");
        assert!(
            (v1.transition_duration - 8.0).abs() < 1e-9,
            "the ramp survives"
        );
        assert_eq!(
            rec.keyframes.iter().map(|k| k.time).collect::<Vec<_>>(),
            vec![0.0, 8.0],
            "keyframes read in time order"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// REVIEW A1's done-when: through tools alone an agent deletes,
    /// reorders, trims, swaps and adds a wipe — none of which the scaffold
    /// can say — and the file still validates clean.
    #[test]
    fn the_apply_door_reaches_what_the_scaffold_cannot() {
        let root = scratch();
        let dir = root.join("Door.promo");
        let project = dir.to_string_lossy().to_string();
        init(
            &json!({"project": project, "canvas": "1920x1080",
                    "palette": {"canvas": "10182B"}}),
            Some(&root),
        )
        .unwrap();
        let shot = root.join("shot.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        let other = root.join("other.png");
        std::fs::write(&other, PNG_1X1).unwrap();
        upsert_layer(
            &json!({"project": project, "kind": "image", "id": "card", "resourceId": "shotres",
                    "file": shot.to_string_lossy(), "duration": 6,
                    "placement": {"height": 620, "anchor": "center"}}),
            Some(&root),
            &measured,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": project, "kind": "image", "id": "second", "resourceId": "otherres",
                    "file": other.to_string_lossy(), "duration": 6}),
            Some(&root),
            &measured,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": project, "kind": "caption", "id": "words",
                    "captionText": "gone soon", "duration": 2}),
            Some(&root),
            &measured,
        )
        .unwrap();

        let answer = apply(
            &json!({"project": project, "commands": [
                {"kind": "deleteLayer", "layerID": "words"},
                {"kind": "moveLayer", "layerID": "second", "index": 1},
                {"kind": "updateLayer", "layerID": "card",
                 "patch": {"transitionIn": {"kind": "wipe", "duration": 0.5}}},
                {"kind": "upsertKeyframe", "layerID": "card",
                 "keyframe": {"id": "swap", "time": 3, "transitionDuration": 0,
                              "resourceID": "otherres",
                              "transition": {"kind": "wipe", "duration": 0.4}}},
                {"kind": "patchResource", "resourceID": "shotres",
                 "patch": {"displayName": "The shot"}}
            ]}),
            Some(&root),
        )
        .unwrap();
        assert!(answer.contains("5 command(s)"), "{answer}");

        let meta = read(&dir);
        let layers = meta.layers.as_ref().unwrap();
        assert!(layers.iter().all(|l| l.id != "words"), "deleted");
        // The z-order is sortIndex; a move renumbers in place and leaves
        // the array where it was (the app's reorder does the same).
        let at_rank_1 = layers.iter().find(|l| l.sort_index == 1).unwrap();
        assert_eq!(at_rank_1.id, "second", "reordered under the card");
        let card = layers.iter().find(|l| l.id == "card").unwrap();
        let wire = serde_json::to_value(card).unwrap();
        assert_eq!(wire["transitionIn"]["kind"], "wipe", "the wipe landed");
        assert_eq!(card.keyframes.len(), 2, "placement kf0 plus the swap");
        let shot_res = meta
            .resources
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.id == "shotres")
            .unwrap();
        assert_eq!(shot_res.display_name, "The shot");
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(warnings.is_empty(), "{warnings:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// REVIEW A2's done-when: "make a show from these pictures" is one call
    /// headless — the wizard the apps ship, reached through a tool, its
    /// media copied in, the file validating clean.
    #[test]
    fn the_wizard_authors_a_show_for_agents() {
        let root = scratch();
        let (a, b) = (root.join("a.png"), root.join("b.png"));
        std::fs::write(&a, PNG_1X1).unwrap();
        std::fs::write(&b, PNG_1X1).unwrap();
        let dir = root.join("Show.promo");
        let answer = slideshow(
            &json!({"project": dir.to_string_lossy(), "slides": [
                {"file": a.to_string_lossy(), "caption": "One"},
                {"file": b.to_string_lossy()}
            ]}),
            Some(&root),
            &measured,
        )
        .unwrap();
        assert!(answer.contains("2 slide(s)"), "{answer}");
        let meta = read(&dir);
        let layers = meta.layers.as_ref().unwrap();
        assert_eq!(
            layers.len(),
            4,
            "background, two pictures, and a caption for the slide that had words"
        );
        assert_eq!(
            layers
                .iter()
                .filter(|l| l.kind == promo_model::ProjectLayerKind::Caption)
                .count(),
            1,
            "issue #7: a classic slide's caption is a layer"
        );
        assert_eq!(meta.min_reader_version, Some(18));
        assert!(dir.join("Resources/a.png").exists() && dir.join("Resources/b.png").exists());
        let warnings = promo_timeline::validate::warnings(&meta);
        assert!(warnings.is_empty(), "{warnings:?}");

        // A store listing takes the STORE's canvas, not the default.
        let listing = root.join("Listing.promo");
        slideshow(
            &json!({"project": listing.to_string_lossy(), "kind": "appStore",
                    "device": "iPhone", "framing": "angled",
                    "slides": [{"file": a.to_string_lossy(), "caption": "Headline"}]}),
            Some(&root),
            &measured,
        )
        .unwrap();
        let store = read(&listing);
        assert!(
            store.composition_settings.canvas_height > store.composition_settings.canvas_width,
            "an iPhone listing is portrait"
        );

        let err = slideshow(
            &json!({"project": dir.to_string_lossy(), "slides": [{"file": a.to_string_lossy()}]}),
            Some(&root),
            &measured,
        )
        .unwrap_err();
        assert!(err.contains("never overwrites"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// REVIEW A3: explain answers with the renderer's own numbers — a
    /// placed card's rect at the composition's midpoint, the keyframes
    /// bracketing the moment, why a not-yet-started layer is hidden — and
    /// diff reads the other author's turn in the format's terms.
    #[test]
    fn explain_and_diff_speak_the_renderers_numbers() {
        let root = scratch();
        let dir = root.join("Why.promo");
        let project = dir.to_string_lossy().to_string();
        init(
            &json!({"project": project, "canvas": "1920x1080"}),
            Some(&root),
        )
        .unwrap();
        let shot = root.join("shot.png");
        std::fs::write(&shot, PNG_1X1).unwrap();
        upsert_layer(
            &json!({"project": project, "kind": "image", "id": "card",
                    "file": shot.to_string_lossy(), "duration": 6,
                    "placement": {"height": 540, "anchor": "center"}}),
            Some(&root),
            &measured,
        )
        .unwrap();
        upsert_layer(
            &json!({"project": project, "kind": "caption", "id": "late",
                    "captionText": "later", "startTime": 5, "duration": 1}),
            Some(&root),
            &measured,
        )
        .unwrap();
        // A snapshot to diff against: the same folder, copied.
        let before = root.join("Before.promo");
        std::fs::create_dir_all(&before).unwrap();
        std::fs::copy(dir.join("metadata.json"), before.join("metadata.json")).unwrap();

        let text = explain(&json!({"project": project, "time": 3.0}), Some(&root)).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        let card = doc["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == "card")
            .unwrap();
        assert_eq!(card["visible"], true);
        let rect = &card["rect"];
        assert!(
            (rect["height"].as_f64().unwrap() - 540.0).abs() < 1.0,
            "the placement rule's height IS the rect's: {rect}"
        );
        assert!(
            ((rect["x"].as_f64().unwrap() + rect["width"].as_f64().unwrap() / 2.0) - 960.0).abs()
                < 1.0,
            "centred: {rect}"
        );
        assert_eq!(card["keyframes"]["count"], 1);
        let late = doc["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == "late")
            .unwrap();
        assert_eq!(late["visible"], false);
        assert_eq!(late["whyHidden"], "not started yet");
        let err = explain(&json!({"project": project, "layer": "ghost"}), Some(&root)).unwrap_err();
        assert!(err.contains("promo_inspect"), "{err}");

        // Change something, then read the turn.
        upsert_keyframe(
            &json!({"project": project, "layer": "card", "id": "k1", "time": 6, "zoom": 1.2}),
            Some(&root),
        )
        .unwrap();
        apply(
            &json!({"project": project, "commands": [
                {"kind": "deleteLayer", "layerID": "late"},
                {"kind": "renameProject", "name": "Renamed"}
            ]}),
            Some(&root),
        )
        .unwrap();
        let report = diff(
            &json!({"project": project, "against": before.to_string_lossy()}),
            Some(&root),
        )
        .unwrap();
        assert!(report.contains("layer card: keyframe k1 added"), "{report}");
        assert!(report.contains("layer late: removed"), "{report}");
        assert!(report.contains("name:"), "{report}");
        let same = diff(
            &json!({"project": before.to_string_lossy(), "against": before.to_string_lossy()}),
            Some(&root),
        )
        .unwrap();
        assert_eq!(same, "no differences");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// One step means one step: a bad command anywhere in the batch leaves
    /// the file byte-identical.
    #[test]
    fn a_failing_batch_applies_nothing() {
        let root = scratch();
        let dir = root.join("Atomic.promo");
        let project = dir.to_string_lossy().to_string();
        init(
            &json!({"project": project, "canvas": "1280x720"}),
            Some(&root),
        )
        .unwrap();
        let before = std::fs::read(dir.join("metadata.json")).unwrap();
        let err = apply(
            &json!({"project": project, "commands": [
                {"kind": "renameProject", "name": "Renamed"},
                {"kind": "deleteLayer", "layerID": "ghost"}
            ]}),
            Some(&root),
        )
        .unwrap_err();
        assert!(err.contains("nothing applied"), "{err}");
        assert_eq!(std::fs::read(dir.join("metadata.json")).unwrap(), before);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The refusals that keep a script honest: no time on create, a typo'd
    /// easing (the renderer would silently degrade it to linear), a layer
    /// that is not there.
    #[test]
    fn keyframe_refusals_name_the_problem() {
        let root = scratch();
        let dir = root.join("No.promo");
        init(
            &json!({"project": dir.to_string_lossy(), "canvas": "1280x720"}),
            Some(&root),
        )
        .unwrap();
        let project = dir.to_string_lossy();
        let err = upsert_keyframe(
            &json!({"project": project, "layer": "bg", "zoom": 1.0}),
            Some(&root),
        )
        .unwrap_err();
        assert!(err.contains("`time` is required"), "{err}");
        let err = upsert_keyframe(
            &json!({"project": project, "layer": "bg", "time": 0, "easing": "easInOut"}),
            Some(&root),
        )
        .unwrap_err();
        assert!(err.contains("easeInOut"), "{err}");
        let err = upsert_keyframe(
            &json!({"project": project, "layer": "ghost", "time": 0}),
            Some(&root),
        )
        .unwrap_err();
        assert!(err.contains("promo_inspect"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
