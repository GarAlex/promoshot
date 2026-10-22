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
    let mut scope = |layers: &[ProjectLayer], canvas: Size, within: Option<&str>| {
        for layer in layers {
            if only.is_some_and(|id| id != layer.id) {
                continue;
            }
            // A layer switched off draws nothing, so it has no layout to
            // warn about and covers nothing either.
            if !layer.is_enabled {
                continue;
            }
            if layer.kind == ProjectLayerKind::Caption {
                caption_checks(meta, layer, layers, &resources, canvas, within, &mut out);
            }
        }
    };
    scope(layers, canvas, None);
    // A composition's captions lay out on the composition's own canvas,
    // with the project's caption defaults — as the renderer draws them.
    for resource in meta.resources.iter().flatten() {
        if let Some(c) = resource.composition.as_ref() {
            if c.canvas_width > 0.0 && c.canvas_height > 0.0 {
                scope(
                    &c.layers,
                    Size::new(c.canvas_width, c.canvas_height),
                    Some(&resource.display_name),
                );
            }
        }
    }
    out
}

/// A few instants of a layer's life: just after it starts (a reveal
/// done, a fade in done), its middle, just before it ends — and every
/// keyframe inside it, where a keyed move or size comes to rest, so a
/// caption that is pushed off an edge on its way is caught there.
fn instants(layer: &ProjectLayer) -> Vec<f64> {
    let start = layer.start_time;
    let end = layer.duration.map(|d| start + d).unwrap_or(start + 4.0);
    let span = (end - start).max(0.0);
    let lead = (span / 2.0).min(1.0);
    let mut ts = vec![start + lead, start + span / 2.0, end - lead];
    for key in &layer.keyframes {
        let t = start + key.time;
        if t > start && t < end {
            ts.push(t);
        }
    }
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
    within: Option<&str>,
    out: &mut Vec<String>,
) {
    let settings = &meta.composition_settings;
    let (w, h) = (canvas.width(), canvas.height());
    // The caption as the messages name it.
    let named = match within {
        Some(comp) => format!("caption \"{}\" (in composition \"{comp}\")", layer.name),
        None => format!("caption \"{}\"", layer.name),
    };
    let safe = SAFE_SHARE * w.min(h);
    // The worst finding of each kind across the instants, so a caption
    // says one thing about its edges and one about what covers it.
    let mut overflow: Option<(f64, String, f64, &'static str)> = None; // (amount, side, t, why)
    let mut tight: Option<(f64, String, f64)> = None; // (distance, side, t)
                                                      // What the fix would move: its placement offset, or its margins.
    let mut placed = false;
    let mut margins = (0.0, 0.0);
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
        // Where the keyframes put the box, when they place it — the same
        // track the renderer draws.
        let (bx, by) =
            ip::caption_box_origin(layer, t, (b.width, b.height), (w, h)).unwrap_or((b.x, b.y));
        placed = style.placement.is_some() || layer.keyframes.iter().any(|k| k.placement.is_some());
        margins = (style.left_margin, style.right_margin);
        let (x0, y0, x1, y1) = (bx, by, bx + b.width, by + b.height);
        // What shows is the ink, not the box: with no plate behind the
        // words (and no stroke round them), the box's padding is empty and
        // a caption whose words clear the edge does not clip.
        let plateless = style.background_rgba[3] == 0 && style.stroke_width <= 0.0;
        let inset = if plateless { style.padding } else { 0.0 };
        let (x0, y0, x1, y1) = (x0 + inset, y0 + inset, x1 - inset, y1 - inset);
        // Past the canvas.
        let sides = [
            (-x0, "left"),
            (-y0, "top"),
            (x1 - w, "right"),
            (y1 - h, "bottom"),
        ];
        for (amount, side) in sides {
            if amount > 0.5 && overflow.as_ref().is_none_or(|o| amount > o.0) {
                overflow = Some((amount, side.into(), t, ""));
            }
        }
        // A reveal that moves its words in: a slide brings each from the
        // right, a rise or a tumble from below. Half way in a word is half
        // there and plainly visible, so that is where it is measured.
        let reveal = style_source
            .as_ref()
            .and_then(|s| s.reveal.clone())
            .or_else(|| settings.subtitle_reveal.clone());
        if let Some(reveal) = reveal {
            let line = style.font_size * style.line_height;
            let (side, extra) = match reveal.mode {
                promo_model::RevealMode::Slide => ("right", x1 + 0.5 * line - w),
                promo_model::RevealMode::Rise | promo_model::RevealMode::Tumble => {
                    ("bottom", y1 + 0.5 * reveal.rise.unwrap_or(0.5) * line - h)
                }
                _ => ("", 0.0),
            };
            if extra > 0.5 && overflow.as_ref().is_none_or(|o| extra > o.0) {
                overflow = Some((extra, side.into(), t, " while its words move in"));
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
            // A beat flash is a full-canvas wash that is dark between its
            // beats: it lights the caption, it does not hide it.
            if other
                .generated_by
                .as_deref()
                .is_some_and(|g| g.starts_with("flash:"))
            {
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
    // The fix, in numbers: how far in the edge must come to sit `safe` px
    // from it, as the field that moves it.
    let fix = |side: &str, by: f64| -> String {
        let by = by.ceil();
        match side {
            "left" if placed => format!("move its placement offset {by:.0} px right"),
            "left" => format!("set leftMargin ≥ {:.0}", margins.0 + by),
            "right" if placed => format!("move its placement offset {by:.0} px left"),
            "right" => format!(
                "set rightMargin ≥ {:.0} or shorten the line",
                margins.1 + by
            ),
            "top" => format!("move it {by:.0} px down"),
            _ => format!("move it {by:.0} px up"),
        }
    };
    if let Some((amount, side, t, why)) = overflow {
        out.push(format!(
            "{named} runs {:.0} px past the canvas's {side} edge at {t:.1}s{why}; {} (or a smaller \
             fontSize) to keep it {:.0} px clear",
            amount,
            fix(&side, amount + safe),
            safe
        ));
    } else if let Some((gap, side, t)) = tight {
        out.push(format!(
            "{named} sits {:.0} px from the canvas's {side} edge at {t:.1}s (safe ≥ {:.0} px); {} — \
             a leading or trailing alignment keeps its side, it just needs room",
            gap,
            safe,
            fix(&side, safe - gap)
        ));
    }
    if let Some((share, name, t)) = covered {
        out.push(format!(
            "{named} is {:.0}% overlapped by \"{name}\" at {t:.1}s, which draws above it; move the \
             caption, raise its sortIndex above \"{name}\", or trim the other layer's placement",
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
            let frame = crate::sprite_frame_at(sheet, layer, local, source)?;
            Size::new(frame.cell.width(), frame.cell.height())
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

    /// Issue #9: a caption whose keyframes push it off the left edge is
    /// caught at the keyframe where it comes to rest — between the three
    /// sampled instants — and the fix names its placement offset with the
    /// pixels it must move.
    #[test]
    fn a_keyed_move_off_the_edge_is_caught_at_its_keyframe() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let meta = project(
            r#"{"id":"C","name":"Slide","sortIndex":1,"kind":"caption","isEnabled":true,"startTime":0,"duration":4,
                "captionText":"A long line that travels left",
                "keyframes":[{"id":"A","time":0,"transitionDuration":0,"placement":{"anchor":"center"}},
                             {"id":"B","time":2.2,"transitionDuration":0.4,"placement":{"anchor":"left","offset":[-260,0]}},
                             {"id":"D","time":2.6,"transitionDuration":0.4,"placement":{"anchor":"center"}}]}"#,
            "",
        );
        let found = layout_warnings(&meta);
        assert!(
            found.iter().any(|w| w.contains("caption \"Slide\" runs")
                && w.contains("past the canvas's left edge at 2.2s")
                && w.contains("move its placement offset")
                && w.contains("px right")),
            "{found:?}"
        );
    }

    /// The fix in numbers: a margin-placed caption is told the margin that
    /// clears the edge.
    #[test]
    fn the_fix_names_the_margin_that_clears_the_edge() {
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
        let line = found
            .iter()
            .find(|w| w.contains("left edge"))
            .expect("named");
        // Its ink is 12 + 16 (the empty padding) = 28 px in; 54 is safe.
        assert!(line.contains("sits 28 px"), "{line}");
        assert!(line.contains("set leftMargin ≥ 38"), "{line}");
    }

    /// A slide reveal brings each word in from the right: a trailing line
    /// close to the edge runs past it while its words move in.
    #[test]
    fn a_sliding_reveal_is_measured_as_it_moves() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let caption = |mode: &str| {
            project(
                &format!(
                    r#"{{"id":"C","name":"Tag","sortIndex":1,"kind":"caption","isEnabled":true,"startTime":0,
                    "duration":4,"captionText":"Now live",
                    "captionStyle":{{"alignment":"trailing","fontSize":120,"rightMargin":30,
                      "reveal":{{"by":"word","mode":"{mode}"}}}},"keyframes":[]}}"#
                ),
                "",
            )
        };
        let slide = layout_warnings(&caption("slide"));
        assert!(
            slide
                .iter()
                .any(|w| w.contains("past the canvas's right edge")
                    && w.contains("while its words move in")),
            "{slide:?}"
        );
        let wipe = layout_warnings(&caption("wipe"));
        assert!(
            wipe.iter().all(|w| !w.contains("move in")),
            "a wipe moves nothing: {wipe:?}"
        );
    }

    /// A composition's captions are measured on the composition's canvas,
    /// and named as its.
    #[test]
    fn a_composition_caption_is_measured_on_its_canvas() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let meta = project(
            "",
            r#"{"id":"K","kind":"composition","filename":"","displayName":"Card","addedAt":0,
                "composition":{"canvasWidth":600,"canvasHeight":400,"layers":[
                  {"id":"N","name":"Inner","sortIndex":0,"kind":"caption","isEnabled":true,"startTime":0,
                   "duration":3,"captionText":"A headline far too long for a small card",
                   "captionStyle":{"alignment":"leading","leftMargin":10},"keyframes":[]}]}}"#,
        );
        let found = layout_warnings(&meta);
        assert!(
            found
                .iter()
                .any(|w| w.contains("caption \"Inner\" (in composition \"Card\")")),
            "{found:?}"
        );
    }

    /// A beat flash — a full-canvas wash, dark between its beats — lights a
    /// caption; it does not cover it. It fired on this repo's beat-cut demo.
    #[test]
    fn a_beat_flash_covers_nothing() {
        if !fonts_available() {
            eprintln!("no fonts; skipping");
            return;
        }
        let mut meta = ProjectMetadata::from_json(
            r#"{"id":"P","name":"Flash","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":4,
                "videoDuration":4,"subtitles":[],
                "compositionSettings":{"canvasWidth":1920,"canvasHeight":1080,"subtitleFontSize":64,
                  "beatClock":{"resourceID":"song","startTime":0},
                  "flash":{"on":"beat","opacity":0.9,"duration":{"beats":0.5}}},
                "resources":[{"id":"song","kind":"audio","filename":"s.wav","displayName":"Song",
                  "addedAt":0,"duration":10,"beatGrid":{"bpm":120}}],
                "layers":[{"id":"C","name":"End card","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":4,"captionText":"See you","keyframes":[]}]}"#,
        )
        .unwrap();
        crate::resolve_timing(&mut meta, &Default::default());
        assert!(
            meta.layers
                .as_ref()
                .unwrap()
                .iter()
                .any(|l| l.generated_by.is_some()),
            "the flash was made"
        );
        assert!(
            layout_warnings(&meta).is_empty(),
            "{:?}",
            layout_warnings(&meta)
        );
    }
}
