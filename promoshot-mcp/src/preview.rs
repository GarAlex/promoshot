//! The glance after every touch: `promo_init`, `promo_upsert_layer` and
//! `promo_validate` answer with a small inline thumbnail, so "look before
//! you ship" stops being a discipline the model must remember and becomes
//! a property of the pipeline.
//!
//! The image travels as an MCP image content block — a multimodal client
//! hands it to the model as an IMAGE (a 480px thumbnail is ~170 image
//! tokens), and a text-only client just sees the text result it always
//! saw. The same pixels land at `Exports/preview.png`, overwritten each
//! call, so a person can keep the file open and watch the composition
//! form — the closest headless gets to an editor viewport.
//!
//! Two rules keep it honest. The sample time is NEVER t=0 by default: a
//! layer with a fadeIn sits at opacity 0 at its own start, and a thumbnail
//! of an empty canvas misleads worse than no thumbnail — so it samples the
//! touched layer's temporal midpoint (the composition's, when no one layer
//! was touched). And a preview failure never fails the call it rides on: a
//! scaffold that succeeded reports success, with a one-line note where the
//! image would have been.

use std::path::Path;

use serde_json::{json, Value};

use crate::Config;

/// The long edge of a thumbnail, in pixels. Small enough to be ~free in
/// image tokens, large enough to catch an off-centre card or an invisible
/// caption.
const LONG_EDGE: f64 = 480.0;

/// Does this tool answer with a thumbnail? The authoring tools and the
/// check step do; renders return paths, and everything else returns facts.
pub fn wanted(tool: &str, args: &Value) -> bool {
    matches!(
        tool,
        "promo_init"
            | "promo_upsert_layer"
            | "promo_upsert_keyframe"
            | "promo_apply"
            | "promo_validate"
    ) && args.get("preview").and_then(Value::as_bool) != Some(false)
}

/// Render the thumbnail and hand back the MCP image block. Any failure is
/// an Err carrying the one-line note — the caller appends it to the text
/// and moves on.
pub fn thumbnail<R>(tool: &str, args: &Value, config: &Config, run: &R) -> Result<Value, String>
where
    R: Fn(&Config, &[String]) -> Result<String, String>,
{
    let project = crate::fenced_project(args, config)?;
    let dir = Path::new(&project);
    let text = std::fs::read_to_string(dir.join("metadata.json"))
        .map_err(|_| "no metadata.json".to_string())?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    let keyframe_time = (tool == "promo_upsert_keyframe")
        .then(|| args.get("time").and_then(Value::as_f64))
        .flatten();
    let time = sample_time(&doc, touched_layer(tool, args, &doc), keyframe_time);
    let (width, height) = thumb_size(&doc);
    let exports = dir.join("Exports");
    std::fs::create_dir_all(&exports).map_err(|e| e.to_string())?;
    let out = exports.join("preview.png");

    run(
        config,
        &[
            "still".into(),
            project,
            "--out".into(),
            out.display().to_string(),
            "--time".into(),
            time.to_string(),
            "--size".into(),
            format!("{width}x{height}"),
        ],
    )?;
    let bytes = std::fs::read(&out).map_err(|e| format!("preview.png: {e}"))?;
    Ok(json!({
        "type": "image",
        "data": base64(&bytes),
        "mimeType": "image/png",
    }))
}

/// The layer this call touched: a layer upsert's `id` when it names one,
/// else the newest layer (an upsert CREATE appends); a keyframe upsert
/// names its layer outright. Init and validate touch the whole
/// composition — no single layer.
fn touched_layer<'d>(tool: &str, args: &Value, doc: &'d Value) -> Option<&'d Value> {
    let layers = doc.get("layers")?.as_array()?;
    let by_id = |id: &str| {
        layers
            .iter()
            .find(|l| l.get("id").and_then(Value::as_str) == Some(id))
    };
    match tool {
        "promo_upsert_layer" => args
            .get("id")
            .and_then(Value::as_str)
            .and_then(by_id)
            .or_else(|| layers.last()),
        "promo_upsert_keyframe" => args.get("layer").and_then(Value::as_str).and_then(by_id),
        _ => None,
    }
}

/// Where to look: a keyframe upsert's own moment (start + its layer-local
/// time — where the motion ARRIVES), else the touched layer's temporal
/// midpoint — past its fadeIn, squarely inside its life — else the
/// composition's midpoint. Never a default of t=0, where fade-ins show an
/// empty canvas.
fn sample_time(doc: &Value, touched: Option<&Value>, keyframe_time: Option<f64>) -> f64 {
    if let Some(layer) = touched {
        let start = layer
            .get("startTime")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if let Some(local) = keyframe_time {
            return start + local;
        }
        if let Some(duration) = layer.get("duration").and_then(Value::as_f64) {
            return start + duration / 2.0;
        }
    }
    let duration = doc
        .get("videoDuration")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    duration / 2.0
}

/// Canvas aspect at thumbnail scale: the long edge pinned, nothing ever
/// upscaled past the canvas itself.
fn thumb_size(doc: &Value) -> (u32, u32) {
    let side = |key: &str, fallback: f64| {
        doc.pointer(&format!("/compositionSettings/{key}"))
            .and_then(Value::as_f64)
            .filter(|v| *v > 0.0)
            .unwrap_or(fallback)
    };
    let width = side("canvasWidth", 1920.0);
    let height = side("canvasHeight", 1080.0);
    let scale = (LONG_EDGE / width.max(height)).min(1.0);
    (
        (width * scale).round().max(16.0) as u32,
        (height * scale).round().max(16.0) as u32,
    )
}

/// Standard-alphabet base64 with padding — the counterpart of the decoder
/// in `speak`, and as with that one, small enough to own.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// The trap this module exists to avoid: a naive t=0 thumbnail of a
    /// fading-in layer shows an empty canvas. The sample is the touched
    /// layer's midpoint, or the composition's.
    #[test]
    fn the_sample_time_is_a_midpoint_never_zero_by_default() {
        let doc = json!({
            "videoDuration": 6.0,
            "layers": [
                { "id": "bg", "kind": "background", "startTime": 0 },
                { "id": "card", "kind": "image", "startTime": 1.0, "duration": 4.0 }
            ]
        });
        let card = &doc["layers"][1];
        assert_eq!(
            sample_time(&doc, Some(card), None),
            3.0,
            "start + duration/2"
        );
        assert_eq!(sample_time(&doc, None, None), 3.0, "composition midpoint");
        assert_eq!(
            sample_time(&doc, Some(card), Some(4.0)),
            5.0,
            "a keyframe glance looks where the motion arrives: start + local time"
        );
        let bg = &doc["layers"][0];
        assert_eq!(
            sample_time(&doc, Some(bg), None),
            3.0,
            "a layer with no duration falls back to the composition"
        );
    }

    #[test]
    fn the_thumbnail_keeps_the_canvas_aspect_at_480() {
        let canvas = |w: f64, h: f64| json!({ "compositionSettings": { "canvasWidth": w, "canvasHeight": h } });
        assert_eq!(thumb_size(&canvas(1920.0, 1080.0)), (480, 270));
        assert_eq!(thumb_size(&canvas(1290.0, 2796.0)), (221, 480), "portrait");
        assert_eq!(
            thumb_size(&canvas(320.0, 200.0)),
            (320, 200),
            "never upscaled"
        );
        assert_eq!(
            thumb_size(&json!({})),
            (480, 270),
            "absent canvas assumes 16:9"
        );
    }
}
