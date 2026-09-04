//! Layout warnings (issue #9): what `promo_validate` could not see while
//! it only read the document — a caption laid out flush with an edge or
//! past it, and a caption drawn under a picture, a stage or a body that
//! sits above it. Every check lays the caption out for real (promo-text,
//! CPU only) at a few instants of its life and reports numbers an agent
//! can act on in the same turn: "left edge at x=12 (safe ≥ 96)". Soft —
//! never a refusal.
//!
//! There was a viewport check too, and it is gone: it fired on shipped
//! projects that looked right, and a check an agent learns to ignore is
//! worse than no check. The same rule governs what is here — a drawing is
//! measured by its own shapes, and a particle system covers nothing.
use crate::caption::caption_style;
use crate::{interpolation as ip, layer_transform_along_paths, media_rect};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResourceKind, Size};

/// The band inside the canvas a caption should keep clear of, as a share
/// of the shorter side: 5% is 54 px on a 1080-high canvas, 96 px on a
/// 1920-wide one.
const SAFE_SHARE: f64 = 0.05;

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
        // A layer switched off draws nothing, so it has no layout to warn
        // about and covers nothing either.
        if !layer.is_enabled {
            continue;
        }
        if layer.kind == ProjectLayerKind::Caption {
            caption_checks(meta, layer, layers, &resources, canvas, &mut out);
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
        // Inside the canvas but within the safe band. LEFT and RIGHT only:
        // that is where reading breaks and where the ticket's cases sat,
        // while a caption close to the bottom is an ordinary lower third.
        // Running PAST any edge is still reported, above.
        let gaps = [(x0, "left"), (w - x1, "right")];
        for (gap, side) in gaps {
            if gap >= -0.5 && gap < safe && tight.as_ref().is_none_or(|g| gap < g.0) {
                tight = Some((gap, side.into(), t));
            }
        }
        // Under something drawn above it. A picture or a clip fills its
        // rect, so a tenth of the caption behind one is already a problem;
        // a body, a stage or a drawing is mostly TRANSPARENT inside its
        // rect, so only a large overlap is worth naming — anything less
        // reads as a false alarm and teaches an agent to ignore the check.
        for other in layers {
            if std::ptr::eq(other, layer) || other.sort_index <= layer.sort_index {
                continue;
            }
            // A particle system covers nothing. It is a drawing layer by
            // kind, but it is sparse by construction — confetti, dust, a
            // burst — and modelling it as a rect made the checker tell an
            // agent to move a caption out from under a shower of dots. It
            // fired on this repo's own 31-confetti reference.
            let showing = crate::layer_resource_id(other, t, resources);
            let other_resource = resources.iter().find(|r| Some(r.id.as_str()) == showing);
            if other_resource.is_some_and(|r| r.particles.is_some()) {
                continue;
            }
            let opaque_rect = matches!(
                other.kind,
                ProjectLayerKind::Image | ProjectLayerKind::Video
            );
            if !opaque_rect
                && !matches!(
                    other.kind,
                    ProjectLayerKind::Model | ProjectLayerKind::Stage | ProjectLayerKind::Drawing
                )
            {
                continue;
            }
            // Something switched off, not yet on, or faded out covers nothing.
            if !other.is_enabled
                || !ip::layer_is_visible(other, t)
                || ip::layer_opacity(other, t) < 0.5
            {
                continue;
            }
            let Some(r) = layer_rect(other, t, settings, resources, canvas) else {
                continue;
            };
            let ix = (x1.min(r[0] + r[2]) - x0.max(r[0])).max(0.0);
            let iy = (y1.min(r[1] + r[3]) - y0.max(r[1])).max(0.0);
            let share = (ix * iy) / (b.width * b.height).max(1.0);
            let floor = if opaque_rect { 0.1 } else { 0.6 };
            if share > floor && covered.as_ref().is_none_or(|c| share > c.0) {
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
            "caption \"{}\" is {:.0}% overlapped by \"{name}\" at {t:.1}s, which draws above it; move the \
             caption, raise its sortIndex above \"{name}\", or trim the other layer's placement",
            layer.name,
            share * 100.0
        ));
    }
}

/// Where a picture, clip, body, stage or drawing sits on the canvas at
/// `t`: the same layout the renderer uses, with a square source standing
/// in for a body or a stage (their frames are made at render time).
/// A drawing document's natural size: the bounds of its shapes' points,
/// and the same 1080x1920 fallback `promo_gpu::vector::content_bounds`
/// uses, so the checker measures the rect the renderer draws.
fn drawing_bounds(doc: &promo_model::DrawingDocument) -> Size {
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for shape in &doc.shapes {
        for point in &shape.points {
            min_x = min_x.min(point.x());
            min_y = min_y.min(point.y());
            max_x = max_x.max(point.x());
            max_y = max_y.max(point.y());
        }
    }
    if max_x <= min_x || max_y <= min_y {
        return Size::new(1080.0, 1920.0);
    }
    Size::new(max_x - min_x, max_y - min_y)
}

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
        // A drawing is sized by its own SHAPES, exactly as the engine sizes
        // it (`content_bounds`). Reading `pixelWidth` here — which a
        // hand-written drawing resource does not carry — fell through to a
        // canvas-height square, so a two-inch rule read as covering half
        // the canvas.
        ProjectLayerKind::Drawing => match resource.and_then(|r| r.drawing.as_ref()) {
            Some(doc) => drawing_bounds(doc),
            None => Size::new(canvas.height(), canvas.height()),
        },
        ProjectLayerKind::Image | ProjectLayerKind::Video => {
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
    // A sprite shows one CELL, not the sheet it is stored in.
    let source = match crate::sheet_for(resource?) {
        Some(sheet) => {
            let local = ip::layer_local_time(layer, t);
            match crate::sprite_frame_at(sheet, layer, local, source) {
                Some(frame) => Size::new(frame.cell.width(), frame.cell.height()),
                None => return None,
            }
        }
        None => source,
    };
    let tr = layer_transform_along_paths(layer, t, settings, resources);
    // A drawing lays out by its own rule, which is what the renderer uses.
    let rect = if layer.kind == ProjectLayerKind::Drawing {
        crate::drawing_rect(
            source,
            canvas,
            tr.zoom,
            tr.horizontal_shift,
            tr.vertical_shift,
        )
    } else {
        media_rect(
            source,
            canvas,
            tr.zoom,
            tr.horizontal_shift,
            tr.vertical_shift,
        )
    };
    Some([rect.x(), rect.y(), rect.width(), rect.height()])
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
            found.iter().any(|w| w.contains("caption \"Hook\" sits")
                && w.contains("left edge")
                && w.contains("safe ≥ 54")),
            "{found:?}"
        );
        let roomy = project(
            r#"{"id":"C","name":"Hook","sortIndex":1,"kind":"caption","isEnabled":true,"startTime":0,"duration":4,
                "captionText":"Ship the film tonight",
                "captionStyle":{"alignment":"leading","leftMargin":200},"keyframes":[]}"#,
            "",
        );
        assert!(
            layout_warnings(&roomy).is_empty(),
            "{:?}",
            layout_warnings(&roomy)
        );
    }

    /// A caption under a picture that draws above it is named with the
    /// overlapping layer and the share.
    #[test]
    fn a_caption_under_a_picture_is_named() {
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
        assert!(
            found
                .iter()
                .any(|w| w.contains("caption \"CTA\" is") && w.contains("overlapped by \"Phone\"")),
            "{found:?}"
        );
        assert!(
            layout_warnings_for(&meta, Some("P")).is_empty(),
            "a picture that covers nothing of its own says nothing"
        );
    }

    /// Two shapes that cover nothing: a particle system, which is sparse by
    /// construction, and a drawing sized by its own SHAPES rather than by a
    /// `pixelWidth` a hand-written resource does not carry.
    ///
    /// Both used to fall through to a canvas-height square and read as
    /// covering the caption — the confetti one fired on this repo's own
    /// 31-confetti reference, which scores 10/10. A checker that tells an
    /// agent to break a good project teaches it to ignore the checker.
    #[test]
    fn confetti_and_a_small_drawing_cover_nothing() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let caption = r#"{"id":"C","name":"Title","sortIndex":0,"kind":"caption","isEnabled":true,
                "startTime":0,"duration":4,"captionText":"Get it now",
                "captionStyle":{"alignment":"center","verticalMargin":500},"keyframes":[]}"#;
        let confetti = project(
            &format!(
                r#"{caption},
                {{"id":"F","name":"Confetti","sortIndex":1,"kind":"drawing","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"PT","keyframes":[]}}"#
            ),
            r#"{"id":"PT","kind":"particles","filename":"","displayName":"Confetti","addedAt":0,
                "particles":{"count":300,"colors":["FFFFFF"]}}"#,
        );
        assert!(
            layout_warnings(&confetti).is_empty(),
            "{:?}",
            layout_warnings(&confetti)
        );

        // A 260x8 rule drawn near the top covers nothing at 500px down.
        let rule = project(
            &format!(
                r#"{caption},
                {{"id":"R","name":"Rule","sortIndex":1,"kind":"drawing","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"D","keyframes":[]}}"#
            ),
            r#"{"id":"D","kind":"drawing","filename":"","displayName":"Rule","addedAt":0,
                "drawing":{"shapes":[{"id":"S","kind":"line","points":[[0,0],[260,8]],
                  "strokeColorHex":"FFFFFF","strokeWidth":8,"arrowStart":false,"arrowEnd":false}]}}"#,
        );
        assert!(
            layout_warnings(&rule).is_empty(),
            "{:?}",
            layout_warnings(&rule)
        );

        // And a drawing that really does cover the caption still says so.
        let over = project(
            &format!(
                r#"{caption},
                {{"id":"R","name":"Slab","sortIndex":1,"kind":"drawing","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"D","keyframes":[]}}"#
            ),
            r#"{"id":"D","kind":"drawing","filename":"","displayName":"Slab","addedAt":0,
                "drawing":{"shapes":[{"id":"S","kind":"rect","points":[[0,0],[1920,1080]],
                  "strokeColorHex":"FFFFFF","strokeWidth":4,"fillColorHex":"FFFFFF",
                  "arrowStart":false,"arrowEnd":false}]}}"#,
        );
        assert!(
            layout_warnings(&over)
                .iter()
                .any(|w| w.contains("overlapped by \"Slab\"")),
            "{:?}",
            layout_warnings(&over)
        );
    }
}
