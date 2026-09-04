//! Layout warnings (issue #9): what `promo_validate` could not see while
//! it only read the document — a caption laid out flush with an edge or
//! past it, a caption drawn under a picture, a stage or a body that
//! sits above it, a viewport that crops a plate's edges. Every check
//! lays the caption out for real (promo-text, CPU only) at a few instants
//! of its life and reports numbers an agent can act on in the same turn:
//! "left edge at x=12 (safe ≥ 96)". Soft — never a refusal.
use crate::caption::caption_style;
use crate::{interpolation as ip, layer_transform_along_paths, media_rect, viewport};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResourceKind, Size};

/// The band inside the canvas a caption should keep clear of, as a share
/// of the shorter side: 5% is 54 px on a 1080-high canvas, 96 px on a
/// 1920-wide one.
const SAFE_SHARE: f64 = 0.05;
/// A viewport that trims less than this much of a side is a nudge, not a
/// crop worth naming.
const CROP_SHARE: f64 = 0.02;

/// Every layout warning for the project.
pub fn layout_warnings(meta: &ProjectMetadata) -> Vec<String> {
    layout_warnings_for(meta, None)
}

/// Layout warnings for one layer (by id), or for every layer.
pub fn layout_warnings_for(meta: &ProjectMetadata, only: Option<&str>) -> Vec<String> {
    let settings = &meta.composition_settings;
    let (w, h) = (settings.canvas_width, settings.canvas_height);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let canvas = Size::new(w, h);
    let resources = meta.resources.clone().unwrap_or_default();
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let mut out = Vec::new();
    for layer in layers {
        if only.is_some_and(|id| id != layer.id) {
            continue;
        }
        match layer.kind {
            ProjectLayerKind::Caption => caption_checks(meta, layer, layers, &resources, canvas, &mut out),
            ProjectLayerKind::Image | ProjectLayerKind::Video => viewport_checks(layer, &mut out),
            _ => {}
        }
    }
    out
}

/// A few instants of a layer's life: just after it starts (a reveal
/// done, a fade in done), its middle, just before it ends.
fn instants(layer: &ProjectLayer) -> Vec<f64> {
    let start = layer.start_time;
    let end = layer.duration.map(|d| start + d).unwrap_or(start + 4.0);
    let span = (end - start).max(0.0);
    let lead = (span / 2.0).min(1.0);
    let mut ts = vec![start + lead, start + span / 2.0, end - lead];
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    ts
}

fn caption_checks(
    meta: &ProjectMetadata,
    layer: &ProjectLayer,
    layers: &[ProjectLayer],
    resources: &[promo_model::ProjectResource],
    canvas: Size,
    out: &mut Vec<String>,
) {
    let settings = &meta.composition_settings;
    let (w, h) = (canvas.width(), canvas.height());
    let safe = SAFE_SHARE * w.min(h);
    // The worst finding of each kind across the instants, so a caption
    // says one thing about its edges and one about what covers it.
    let mut overflow: Option<(f64, String, f64)> = None; // (amount, side, t)
    let mut tight: Option<(f64, String, f64)> = None; // (distance, side, t)
    let mut covered: Option<(f64, String, f64)> = None; // (share, name, t)
    for t in instants(layer) {
        let showing = crate::layer_resource_id(layer, t, resources).map(str::to_string);
        let Some(text) = meta.caption_text_showing(layer, showing.as_deref()) else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let style_source = meta.caption_style_showing(layer, showing.as_deref());
        let mut style = caption_style(style_source.as_ref(), settings);
        if let Some(values) = ip::layer_caption_values(
            layer,
            t,
            ip::CaptionValues {
                font_size: style.font_size,
                vertical_margin: style.vertical_margin,
                left_margin: style.left_margin,
            },
        ) {
            style.font_size = values.font_size;
            style.vertical_margin = values.vertical_margin;
            style.left_margin = values.left_margin;
        }
        let Some(b) = promo_text::measure(text, w, h, &style) else {
            continue;
        };
        let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.width, b.y + b.height);
        // Past the canvas.
        let sides = [
            (-x0, "left"),
            (-y0, "top"),
            (x1 - w, "right"),
            (y1 - h, "bottom"),
        ];
        for (amount, side) in sides {
            if amount > 0.5 && overflow.as_ref().is_none_or(|o| amount > o.0) {
                overflow = Some((amount, side.into(), t));
            }
        }
        // Inside the canvas but within the safe band.
        let gaps = [(x0, "left"), (y0, "top"), (w - x1, "right"), (h - y1, "bottom")];
        for (gap, side) in gaps {
            if gap >= -0.5 && gap < safe && tight.as_ref().is_none_or(|g| gap < g.0) {
                tight = Some((gap, side.into(), t));
            }
        }
        // Under something drawn above it.
        for other in layers {
            if std::ptr::eq(other, layer) || other.sort_index <= layer.sort_index {
                continue;
            }
            if !matches!(
                other.kind,
                ProjectLayerKind::Image
                    | ProjectLayerKind::Video
                    | ProjectLayerKind::Model
                    | ProjectLayerKind::Stage
                    | ProjectLayerKind::Drawing
            ) || !ip::layer_is_visible(other, t)
            {
                continue;
            }
            let Some(r) = layer_rect(other, t, settings, resources, canvas) else {
                continue;
            };
            let ix = (x1.min(r[0] + r[2]) - x0.max(r[0])).max(0.0);
            let iy = (y1.min(r[1] + r[3]) - y0.max(r[1])).max(0.0);
            let share = (ix * iy) / (b.width * b.height).max(1.0);
            if share > 0.1 && covered.as_ref().is_none_or(|c| share > c.0) {
                covered = Some((share, other.name.clone(), t));
            }
        }
    }
    if let Some((amount, side, t)) = overflow {
        out.push(format!(
            "caption \"{}\" runs {:.0} px past the canvas's {side} edge at {t:.1}s; widen its margins, \
             shorten the line or move its placement",
            layer.name, amount
        ));
    } else if let Some((gap, side, t)) = tight {
        out.push(format!(
            "caption \"{}\" sits {:.0} px from the canvas's {side} edge at {t:.1}s (safe ≥ {:.0} px); a \
             leading or trailing alignment still needs margins, a placement offset or a smaller size \
             to clear the edge",
            layer.name, gap, safe
        ));
    }
    if let Some((share, name, t)) = covered {
        out.push(format!(
            "caption \"{}\" is {:.0}% covered by \"{name}\" at {t:.1}s, which draws above it; move the \
             caption, raise its sortIndex above \"{name}\", or trim the other layer's placement",
            layer.name,
            share * 100.0
        ));
    }
}

/// Where a picture, clip, body, stage or drawing sits on the canvas at
/// `t`: the same layout the renderer uses, with a square source standing
/// in for a body or a stage (their frames are made at render time).
fn layer_rect(
    layer: &ProjectLayer,
    t: f64,
    settings: &promo_model::CompositionSettings,
    resources: &[promo_model::ProjectResource],
    canvas: Size,
) -> Option<[f64; 4]> {
    let showing = crate::layer_resource_id(layer, t, resources);
    let resource = resources.iter().find(|r| Some(r.id.as_str()) == showing);
    let source = match layer.kind {
        ProjectLayerKind::Image | ProjectLayerKind::Video | ProjectLayerKind::Drawing => {
            let r = resource?;
            let (rw, rh) = match r.kind {
                ProjectResourceKind::Video => (r.video_natural_width, r.video_natural_height),
                _ => (r.pixel_width, r.pixel_height),
            };
            match (rw, rh) {
                (Some(rw), Some(rh)) if rw > 0.0 && rh > 0.0 => Size::new(rw, rh),
                _ => Size::new(canvas.height(), canvas.height()),
            }
        }
        _ => Size::new(canvas.height(), canvas.height()),
    };
    let tr = layer_transform_along_paths(layer, t, settings, resources);
    let rect = media_rect(source, canvas, tr.zoom, tr.horizontal_shift, tr.vertical_shift);
    Some([rect.x(), rect.y(), rect.width(), rect.height()])
}

fn viewport_checks(layer: &ProjectLayer, out: &mut Vec<String>) {
    let mut worst: Option<(f64, String, f64)> = None;
    for t in instants(layer) {
        let Some(v) = viewport::layer_viewport(layer, t) else {
            continue;
        };
        let trims = [
            (v[0], "left"),
            (v[1], "top"),
            (1.0 - (v[0] + v[2]), "right"),
            (1.0 - (v[1] + v[3]), "bottom"),
        ];
        for (share, side) in trims {
            if share > CROP_SHARE && worst.as_ref().is_none_or(|x| share > x.0) {
                worst = Some((share, side.into(), t));
            }
        }
    }
    if let Some((share, side, t)) = worst {
        out.push(format!(
            "layer \"{}\" crops {:.0}% off its picture's {side} edge at {t:.1}s (viewport); type or a \
             mark near that edge of the plate will be cut — keep the viewport inset off the edge that \
             carries it",
            layer.name,
            share * 100.0
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(layers: &str, resources: &str) -> ProjectMetadata {
        ProjectMetadata::from_json(&format!(
            r#"{{"id":"P","name":"Layout","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":4,
                "videoDuration":4,"subtitles":[],
                "compositionSettings":{{"canvasWidth":1920,"canvasHeight":1080,"subtitleFontSize":64,
                  "subtitleLeftMargin":120,"subtitleRightMargin":120,"subtitleVerticalMargin":900,
                  "subtitleBackgroundOpacity":0,"subtitleFontFamily":"system"}},
                "resources":[{resources}],"layers":[{layers}]}}"#
        ))
        .expect("decodes")
    }

    /// Fonts are the host's; without any, layout yields nothing and these
    /// checks have nothing to say — the same silence the renderer gives.
    fn fonts_available() -> bool {
        let style = promo_text::TextStyle {
            font_size: 48.0,
            ..caption_style(None, &promo_model::CompositionSettings::default())
        };
        promo_text::measure("Hello", 1920.0, 1080.0, &style).is_some()
    }

    /// A leading caption with a sliver of a margin is named with its
    /// distance from the edge and the safe distance; the same caption with
    /// a proper margin says nothing.
    #[test]
    fn a_flush_leading_caption_is_named_with_numbers() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let flush = project(
            r#"{"id":"C","name":"Hook","sortIndex":1,"kind":"caption","isEnabled":true,"startTime":0,"duration":4,
                "captionText":"Ship the film tonight",
                "captionStyle":{"alignment":"leading","leftMargin":12},"keyframes":[]}"#,
            "",
        );
        let found = layout_warnings(&flush);
        assert!(
            found.iter().any(|w| w.contains("caption \"Hook\" sits") && w.contains("left edge") && w.contains("safe ≥ 54")),
            "{found:?}"
        );
        let roomy = project(
            r#"{"id":"C","name":"Hook","sortIndex":1,"kind":"caption","isEnabled":true,"startTime":0,"duration":4,
                "captionText":"Ship the film tonight",
                "captionStyle":{"alignment":"leading","leftMargin":200},"keyframes":[]}"#,
            "",
        );
        assert!(layout_warnings(&roomy).is_empty(), "{:?}", layout_warnings(&roomy));
    }

    /// A caption under a picture that draws above it is named with the
    /// covering layer; a viewport that trims a plate's edge is named with
    /// the side and the share.
    #[test]
    fn a_covered_caption_and_a_cropping_viewport_are_named() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let meta = project(
            r#"{"id":"C","name":"CTA","sortIndex":0,"kind":"caption","isEnabled":true,"startTime":0,"duration":4,
                "captionText":"Get it now","captionStyle":{"alignment":"center","verticalMargin":500},"keyframes":[]},
               {"id":"P","name":"Phone","sortIndex":1,"kind":"image","isEnabled":true,"startTime":0,"duration":4,
                "resourceID":"S","keyframes":[{"id":"K","time":0,"placement":{"height":900,"anchor":"center"},
                "viewport":[0.06,0,0.94,1],"transitionDuration":0}]}"#,
            r#"{"id":"S","kind":"image","filename":"s.png","displayName":"Shot","addedAt":0,"pixelWidth":1200,"pixelHeight":900}"#,
        );
        let found = layout_warnings(&meta);
        assert!(found.iter().any(|w| w.contains("caption \"CTA\" is") && w.contains("covered by \"Phone\"")), "{found:?}");
        assert!(found.iter().any(|w| w.contains("layer \"Phone\" crops 6% off its picture's left edge")), "{found:?}");
        let only_phone = layout_warnings_for(&meta, Some("P"));
        assert!(only_phone.iter().all(|w| w.starts_with("layer \"Phone\"")), "{only_phone:?}");
    }
}
