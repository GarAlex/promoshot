//! Model drawings → renderer shapes.
//!
//! One conversion, used by the engine's own drawing rasterization and by the
//! FFI's `promo_vector_render`. It resolves hex colors and opacities the way
//! the Swift rasterizer did (missing opacity = 1, unparseable stroke color =
//! white, no `fillColorHex` = no fill).

use promo_gpu::vector::{VectorShape, VectorShapeKind};
use promo_model::{DrawingDocument, DrawingShapeKind};

/// Tessellates a drawing's shapes.
///
/// `settings` is here only to resolve `@name` colours: a drawing's stroke and
/// fill are document colours like any other, so they follow the palette too.
pub fn vector_shapes(
    doc: &DrawingDocument,
    settings: &promo_model::CompositionSettings,
) -> Vec<VectorShape> {
    doc.shapes
        .iter()
        .map(|s| {
            let stroke_alpha = s.stroke_opacity.unwrap_or(1.0);
            let fill_alpha = s.fill_opacity.unwrap_or(1.0);
            VectorShape {
                kind: match s.kind {
                    DrawingShapeKind::Pen => VectorShapeKind::Pen,
                    DrawingShapeKind::Line => VectorShapeKind::Line,
                    DrawingShapeKind::Oval => VectorShapeKind::Oval,
                },
                points: s.points.iter().map(|p| (p.x(), p.y())).collect(),
                stroke_rgba: rgba_from_hex(settings.resolve_color(&s.stroke_color_hex))
                    .map(|c| [c[0], c[1], c[2], stroke_alpha])
                    .unwrap_or([1.0, 1.0, 1.0, stroke_alpha]),
                stroke_width: s.stroke_width,
                fill_rgba: s
                    .fill_color_hex
                    .as_deref()
                    .map(|hex| settings.resolve_color(hex))
                    .and_then(rgba_from_hex)
                    .map(|c| [c[0], c[1], c[2], fill_alpha]),
                arrow_start: s.arrow_start,
                arrow_end: s.arrow_end,
                even_odd_fill: s.even_odd_fill.unwrap_or(false),
            }
        })
        .collect()
}

/// `#RRGGBB` / `RRGGBB` → sRGB components. `None` when the string isn't a
/// 6-digit hex color (mirrors Swift's optional CGColor).
pub fn rgba_from_hex(hex: &str) -> Option<[f32; 4]> {
    let value = hex.trim().trim_start_matches('#').to_uppercase();
    if value.len() != 6 {
        return None;
    }
    let parsed = u32::from_str_radix(&value, 16).ok()?;
    Some([
        ((parsed >> 16) & 0xFF) as f32 / 255.0,
        ((parsed >> 8) & 0xFF) as f32 / 255.0,
        (parsed & 0xFF) as f32 / 255.0,
        1.0,
    ])
}
