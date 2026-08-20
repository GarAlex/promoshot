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

/// The pixel size ONE DRAWN FRAME of this resource shows: an image's own
/// pixels, a video's natural size, a sprite sheet's single cell. This is
/// what a placement rule resolves width and anchoring against, and it comes
/// from the MODEL — every reader computes the same numbers from the same
/// stored size. `None` when the project has not been told (a hand-written
/// file): placement then assumes a square source, and validation names it.
pub fn resource_source_size(resource: &promo_model::ProjectResource) -> Option<Size> {
    let width = resource.pixel_width.or(resource.video_natural_width)?;
    let height = resource.pixel_height.or(resource.video_natural_height)?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    if let Some(sprite) = &resource.sprite {
        let columns = sprite.columns.max(1) as f64;
        let rows = sprite.rows.max(1) as f64;
        return Some(Size::new(width / columns, height / rows));
    }
    Some(Size::new(width, height))
}

/// The SIZE half of a placement rule: the zoom that makes the drawn box the
/// asked size, on the same rule `media_rect` lays out with
/// (`drawnHeight = canvasHeight x zoom`, width from the source's aspect).
/// `None` for a position-only rule — the keyframe's own zoom stands.
/// `height` wins over `width` wins over `mode` when more than one is given;
/// validation names the conflict rather than this code guessing.
pub fn placement_zoom(
    placement: &promo_model::Placement,
    aspect: f64,
    canvas: Size,
) -> Option<f64> {
    let canvas_height = canvas.height().max(1.0);
    let canvas_width = canvas.width().max(1.0);
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    if let Some(height) = placement.height {
        return Some((height / canvas_height).max(0.0));
    }
    if let Some(width) = placement.width {
        return Some((width / (canvas_height * aspect)).max(0.0));
    }
    placement.mode.map(|mode| {
        let widthwise = canvas_width / (canvas_height * aspect);
        match mode {
            promo_model::PlacementMode::Fit => widthwise.min(1.0),
            promo_model::PlacementMode::Fill => widthwise.max(1.0),
        }
    })
}

/// The POSITION half: the drawn box's top-left for the rule's anchor and
/// offset, given the zoom the box actually has. The result is exactly the
/// `horizontalShift`/`verticalShift` pair `media_rect` treats as an origin.
pub fn placement_position(
    placement: &promo_model::Placement,
    zoom: f64,
    aspect: f64,
    canvas: Size,
) -> (f64, f64) {
    use promo_model::Anchor;
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    let drawn_height = canvas.height() * zoom.max(0.0);
    let drawn_width = drawn_height * aspect;
    let anchor = placement.anchor.unwrap_or(Anchor::Center);
    let (column, row) = match anchor {
        Anchor::TopLeft => (0, 0),
        Anchor::Top => (1, 0),
        Anchor::TopRight => (2, 0),
        Anchor::Left => (0, 1),
        Anchor::Center => (1, 1),
        Anchor::Right => (2, 1),
        Anchor::BottomLeft => (0, 2),
        Anchor::Bottom => (1, 2),
        Anchor::BottomRight => (2, 2),
    };
    let place = |span: f64, drawn: f64, cell: i32| match cell {
        0 => 0.0,
        1 => (span - drawn) / 2.0,
        _ => span - drawn,
    };
    let offset = placement.offset.unwrap_or([0.0, 0.0]);
    (
        place(canvas.width(), drawn_width, column) + offset[0],
        place(canvas.height(), drawn_height, row) + offset[1],
    )
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    fn placement(json: &str) -> promo_model::Placement {
        serde_json::from_str(json).expect("placement")
    }

    #[test]
    fn height_is_zoom_in_canvas_pixels() {
        let canvas = Size::new(1080.0, 1920.0);
        let zoom = placement_zoom(&placement(r#"{"height": 620}"#), 0.5, canvas).unwrap();
        assert!((zoom - 620.0 / 1920.0).abs() < 1e-12);
        // Height needs no aspect at all — same answer whatever the source.
        let square = placement_zoom(&placement(r#"{"height": 620}"#), 1.0, canvas).unwrap();
        assert_eq!(zoom, square);
    }

    #[test]
    fn width_needs_the_aspect_and_uses_it() {
        let canvas = Size::new(1080.0, 1920.0);
        let aspect: f64 = 1170.0 / 2532.0;
        let zoom = placement_zoom(&placement(r#"{"width": 900}"#), aspect, canvas).unwrap();
        // drawnW = canvasH * zoom * aspect must land on the asked width.
        assert!((1920.0 * zoom * aspect - 900.0).abs() < 1e-9);
    }

    #[test]
    fn fit_contains_and_fill_covers() {
        let canvas = Size::new(1080.0, 1920.0);
        // A wide source: fitting is width-bound, filling is height-bound.
        let wide = 16.0 / 9.0;
        let fit = placement_zoom(&placement(r#"{"mode": "fit"}"#), wide, canvas).unwrap();
        assert!((1920.0 * fit * wide - 1080.0).abs() < 1e-9, "width-bound");
        let fill = placement_zoom(&placement(r#"{"mode": "fill"}"#), wide, canvas).unwrap();
        assert_eq!(fill, 1.0, "covers the height");
        // A tall source fits at its full height.
        let tall = 9.0 / 16.0;
        assert_eq!(
            placement_zoom(&placement(r#"{"mode": "fit"}"#), tall, canvas).unwrap(),
            1.0
        );
    }

    #[test]
    fn a_position_only_rule_declines_to_zoom() {
        assert!(placement_zoom(
            &placement(r#"{"anchor": "bottomRight"}"#),
            1.0,
            Size::new(1080.0, 1920.0)
        )
        .is_none());
    }

    #[test]
    fn the_nine_anchors_land_where_they_say() {
        let canvas = Size::new(1000.0, 2000.0);
        let zoom = 0.25; // drawn 250x500 at aspect 0.5
        let aspect = 0.5;
        let corners = [
            ("topLeft", (0.0, 0.0)),
            ("top", (375.0, 0.0)),
            ("topRight", (750.0, 0.0)),
            ("left", (0.0, 750.0)),
            ("center", (375.0, 750.0)),
            ("right", (750.0, 750.0)),
            ("bottomLeft", (0.0, 1500.0)),
            ("bottom", (375.0, 1500.0)),
            ("bottomRight", (750.0, 1500.0)),
        ];
        for (name, expected) in corners {
            let rule = placement(&format!(r#"{{"anchor": "{name}"}}"#));
            let got = placement_position(&rule, zoom, aspect, canvas);
            assert_eq!(got, expected, "{name}");
        }
        // The offset is a plain nudge from wherever the anchor put it.
        let nudged = placement(r#"{"anchor": "topLeft", "offset": [-40, 12]}"#);
        assert_eq!(placement_position(&nudged, zoom, aspect, canvas), (-40.0, 12.0));
    }

    #[test]
    fn a_sprite_resolves_to_one_cell() {
        let resource: promo_model::ProjectResource = serde_json::from_str(
            r#"{"id": "S", "kind": "image", "filename": "s.png",
                "displayName": "S", "addedAt": 0,
                "pixelWidth": 800, "pixelHeight": 400,
                "sprite": {"columns": 4, "rows": 2}}"#,
        )
        .expect("resource");
        let cell = resource_source_size(&resource).expect("cell");
        assert_eq!((cell.width(), cell.height()), (200.0, 200.0));
    }

    #[test]
    fn an_unmeasured_source_has_no_size() {
        let resource: promo_model::ProjectResource = serde_json::from_str(
            r#"{"id": "I", "kind": "image", "filename": "i.png",
                "displayName": "I", "addedAt": 0}"#,
        )
        .expect("resource");
        assert!(resource_source_size(&resource).is_none());
    }
}
