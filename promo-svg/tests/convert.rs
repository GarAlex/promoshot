//! Lossless conversion (SVG-PLAN.md slice 5): plain geometry becomes a
//! drawing that draws as the file does; everything else is refused with a
//! reason. `cargo test -p promo-svg --features gpu --test convert`.

use std::path::PathBuf;

use promo_model::DrawingShapeKind;
use promo_svg::{Context, Picture};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn picture(path: &str) -> Picture {
    let bytes = std::fs::read(dir().join(path)).unwrap();
    Picture::parse(
        &bytes,
        &Context {
            file_dir: dir(),
            project_root: dir(),
        },
    )
    .unwrap()
}

fn gpu() -> bool {
    promo_gpu::gpu_for_test().is_some()
}

/// A whole file of plain shapes converts, each into its own kind where it
/// has one, checked against resvg by the conversion itself.
#[test]
fn a_file_of_plain_shapes_becomes_a_drawing() {
    if !gpu() {
        return;
    }
    let p = picture("convert/plain.svg");
    let done = p.convert(&[], None, true).unwrap_or_else(|e| panic!("{e}"));
    let kinds: Vec<DrawingShapeKind> = done.drawing.shapes.iter().map(|s| s.kind).collect();
    // The frame, the background, the card (rounded), the dot, the
    // ellipse, the polygon (a pen), the wave, the letter (one pen with its
    // counter), the stroke-first rect as two shapes.
    assert_eq!(kinds[0], DrawingShapeKind::Rect, "the frame first");
    assert_eq!(done.drawing.shapes[0].stroke_width, 0.0);
    assert_eq!(done.drawing.shapes[0].fill_color_hex, None);
    let card = &done.drawing.shapes[2];
    assert_eq!(card.kind, DrawingShapeKind::Rect);
    assert_eq!(card.corner_radius, Some(14.0));
    assert_eq!(card.fill_color_hex.as_deref(), Some("5BC0BE"));
    assert_eq!(card.stroke_width, 4.0);
    assert_eq!(kinds[3], DrawingShapeKind::Oval);
    assert_eq!(kinds[4], DrawingShapeKind::Oval);
    assert!(kinds[5..]
        .iter()
        .take(3)
        .all(|k| *k == DrawingShapeKind::Pen));
    let letter = &done.drawing.shapes[7];
    assert_eq!(letter.even_odd_fill, Some(true));
    assert!(
        (letter.fill_opacity.unwrap() - 0.8).abs() < 1e-6,
        "the element's opacity"
    );
    // Stroke first: the stroke shape, then the fill shape.
    let n = done.drawing.shapes.len();
    assert_eq!(done.drawing.shapes[n - 2].fill_color_hex, None);
    assert_eq!(done.drawing.shapes[n - 1].stroke_width, 0.0);
    // The same conversion mints the same ids, shaped as UUIDs.
    let again = p.convert(&[], None, true).unwrap();
    assert_eq!(done.drawing.shapes[3].id, again.drawing.shapes[3].id);
    assert_eq!(done.drawing.shapes[3].id.len(), 36);
}

/// Each element the drawing cannot hold is refused by name, with the
/// reason.
#[test]
fn what_a_drawing_cannot_hold_is_refused_by_name() {
    if !gpu() {
        return;
    }
    let p = picture("convert/refused.svg");
    for (node, reason) in [
        ("mitered", "a miter join"),
        ("butt", "a butt cap"),
        ("dashed", "a dashed stroke"),
        ("graded", "a gradient fill"),
        ("faded", "a group's opacity over several shapes"),
        ("both", "an opacity over a fill and its stroke"),
        ("skewed", "a stroke under a stretch or a skew"),
        ("clipped", "clipped"),
        ("words", "text"),
    ] {
        let err = p
            .convert(&[], Some(&[node.to_string()]), true)
            .expect_err(node);
        assert!(err.reason.contains(reason), "{node}: {err}");
        assert!(err.to_string().contains(node), "{node} named: {err}");
    }
    // A whole file with text is refused for the text (which a picture
    // without outlines would not even draw); without it, at the first
    // element that cannot convert.
    let err = p.convert(&[], None, true).unwrap_err();
    assert_eq!(err.node.as_deref(), Some("words"), "{err}");
    let bytes = std::fs::read_to_string(dir().join("convert/refused.svg")).unwrap();
    let start = bytes.find("<text").unwrap();
    let end = bytes[start..].find("</text>").unwrap() + start + "</text>".len();
    let wordless = format!("{}{}", &bytes[..start], &bytes[end..]);
    let p = Picture::parse(
        wordless.as_bytes(),
        &Context {
            file_dir: dir(),
            project_root: dir(),
        },
    )
    .unwrap();
    let err = p.convert(&[], None, true).unwrap_err();
    assert_eq!(err.node.as_deref(), Some("mitered"), "{err}");
}

/// What the reasons do not foresee, the check sees: two translucent
/// strokes crossing in one path — the file paints the crossing once, a
/// drawing's stroke twice — is refused where they cross.
#[test]
fn the_render_check_refuses_what_the_reasons_miss() {
    if !gpu() {
        return;
    }
    let p = picture("convert/refused.svg");
    let err = p
        .convert(&[], Some(&["crossing".to_string()]), true)
        .unwrap_err();
    assert!(
        err.reason.contains("would not draw as the file does"),
        "{err}"
    );
    assert_eq!(err.node.as_deref(), Some("crossing"));
}

/// A selection: the logo's glyph (a letter with a counter) and the
/// wordmark group convert, laid out on the picture's frame; the badge
/// holds a gradient and is refused, naming its rect.
#[test]
fn a_selection_converts_on_the_pictures_frame() {
    if !gpu() {
        return;
    }
    let p = picture("corpus/logo.svg");
    let glyph = p.convert(&[], Some(&["glyph".into()]), true).unwrap();
    assert_eq!(glyph.nodes, vec!["glyph".to_string()]);
    let frame = &glyph.drawing.shapes[0];
    assert_eq!(frame.points[1].0, 240.0);
    assert_eq!(frame.points[1].1, 80.0);
    assert_eq!(glyph.drawing.shapes.len(), 2, "the frame and the glyph");
    let word = p.convert(&[], Some(&["wordmark".into()]), true).unwrap();
    assert_eq!(word.drawing.shapes.len(), 6, "the frame and five letters");
    assert!(word.drawing.shapes[1..]
        .iter()
        .all(|s| s.fill_color_hex.as_deref() == Some("1B1B3A")));
    let err = p.convert(&[], Some(&["badge".into()]), true).unwrap_err();
    assert_eq!(err.node.as_deref(), Some("badge/rect[0]"), "{err}");
    assert!(err.reason.contains("a gradient fill"));
    // The resource's overrides are its look: a recoloured glyph converts
    // recoloured.
    let red = vec![promo_model::vector::VectorOverride {
        node: "glyph".into(),
        fill: Some("FF0000".into()),
        ..Default::default()
    }];
    let recoloured = p.convert(&red, Some(&["glyph".into()]), true).unwrap();
    assert_eq!(
        recoloured.drawing.shapes[1].fill_color_hex.as_deref(),
        Some("FF0000")
    );
}

/// Converted shapes sit above the whole picture, so an element the file
/// draws over the selection refuses it — by name — unless it comes along.
#[test]
fn what_the_file_draws_over_the_selection_comes_along_or_refuses() {
    if !gpu() {
        return;
    }
    let p = picture("convert/plain.svg");
    // The background rect is drawn under everything: converting it alone
    // would put it above the card.
    let err = p.convert(&[], Some(&["rect[0]".into()]), true).unwrap_err();
    assert_eq!(err.node.as_deref(), Some("rect[0]"), "{err}");
    assert!(err.reason.contains("\"card\" is drawn over it"), "{err}");
    // The wave lies under the letter: refused; with what is over it, or
    // the topmost rect alone, nothing is.
    let err = p.convert(&[], Some(&["wave".into()]), true).unwrap_err();
    assert!(err.reason.contains("\"letter\" is drawn over it"), "{err}");
    p.convert(
        &[],
        Some(&["wave".into(), "letter".into(), "rect[2]".into()]),
        true,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    p.convert(&[], Some(&["rect[2]".into()]), true)
        .unwrap_or_else(|e| panic!("{e}"));
}
