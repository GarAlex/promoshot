//! Canvas layout math — the WYSIWYG single source of truth shared by preview
//! and export. Mirrors Swift `LayerLayout` (CompositionSettings.swift) and
//! `VideoComposer.letterboxTransform`.

use promo_model::{Rect, Size};

/// Swift `LayerLayout.clampedZoom` — floored so a layer never collapses past
/// 30% of its base size.
pub fn clamped_zoom(zoom: f64) -> f64 {
    zoom.max(0.3)
}

/// Swift `LayerLayout.mediaRect` — rect for a video/image layer scaled to the
/// canvas height, zoom and shift applied.
pub fn media_rect(
    source_size: Size,
    canvas_size: Size,
    zoom: f64,
    horizontal_shift: f64,
    vertical_shift: f64,
) -> Rect {
    if source_size.width() <= 0.0 || source_size.height() <= 0.0 {
        return Rect::new(horizontal_shift, vertical_shift, 0.0, 0.0);
    }
    let scale = (canvas_size.height().max(1.0) / source_size.height()) * clamped_zoom(zoom);
    Rect::new(
        horizontal_shift,
        vertical_shift,
        source_size.width() * scale,
        source_size.height() * scale,
    )
}

/// Swift `LayerLayout.mediaCornerRadius`.
pub fn media_corner_radius(base: f64, zoom: f64) -> f64 {
    base * clamped_zoom(zoom)
}

/// Swift `LayerLayout.drawingRect` — aspect-fit into the canvas, centered,
/// then zoom and shift.
pub fn drawing_rect(
    natural_size: Size,
    canvas_size: Size,
    zoom: f64,
    horizontal_shift: f64,
    vertical_shift: f64,
) -> Rect {
    let natural = Size::new(
        natural_size.width().max(1.0),
        natural_size.height().max(1.0),
    );
    let fit_scale = (canvas_size.width() / natural.width())
        .min(canvas_size.height() / natural.height())
        * clamped_zoom(zoom);
    let width = natural.width() * fit_scale;
    let height = natural.height() * fit_scale;
    Rect::new(
        (canvas_size.width() - width) / 2.0 + horizontal_shift,
        (canvas_size.height() - height) / 2.0 + vertical_shift,
        width,
        height,
    )
}

/// Swift `VideoComposer.letterboxTransform(canvas:output:)` — aspect-fit
/// scale + centering offset for rendering the canvas into an arbitrary
/// output size.
pub fn letterbox_transform(canvas: Size, output: Size) -> (f64, (f64, f64)) {
    if canvas.width() <= 0.0 || canvas.height() <= 0.0 {
        return (1.0, (0.0, 0.0));
    }
    let scale = (output.width() / canvas.width()).min(output.height() / canvas.height());
    (
        scale,
        (
            (output.width() - canvas.width() * scale) / 2.0,
            (output.height() - canvas.height() * scale) / 2.0,
        ),
    )
}
