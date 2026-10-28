//! Slice 6 (SVG-PLAN.md): the draw-on stroke reveal (`trimStart` /
//! `trimEnd`) and theme colours (`@name`) on an SVG's elements.

use std::sync::Arc;

use promo_svg::vector::VectorOverride;
use promo_svg::{Context, Picture};

const LINES: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <rect width="200" height="100" fill="#ffffff"/>
  <path id="line" d="M10 50 H 190" stroke="#c00000" stroke-width="10" fill="none"/>
  <path id="two" d="M10 20 H 110 M10 80 H 190" stroke="#0000c0" stroke-width="6" fill="none"/>
  <rect id="solid" x="150" y="5" width="20" height="10" fill="#00a000"/>
  <path id="dashed" d="M10 95 H 190" stroke="#000" stroke-dasharray="4 4"/>
</svg>"##;

fn context() -> Context {
    let dir = std::env::temp_dir();
    Context {
        file_dir: dir.clone(),
        project_root: dir,
    }
}

fn draw(svg: &str, overrides: &[VectorOverride]) -> Vec<u8> {
    Picture::parse(svg.as_bytes(), &context())
        .unwrap()
        .with_overrides(overrides)
        .unwrap()
        .render(200, 100, None)
        .unwrap()
}

fn resvg(svg: &str) -> Vec<u8> {
    let options = usvg::Options {
        fontdb: Arc::new(usvg::fontdb::Database::new()),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options).unwrap();
    let mut pixmap = tiny_skia::Pixmap::new(200, 100).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.take()
}

fn o(json: serde_json::Value) -> VectorOverride {
    serde_json::from_value(json).unwrap()
}

fn px(img: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 200 + x) * 4;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

/// A reveal draws exactly what the same dash written into the file draws:
/// the shown share of the stroke, from where it starts; the fill and the
/// other elements untouched; the whole stroke at 0–1; nothing at an empty
/// share.
#[test]
fn a_reveal_is_the_stroke_share_resvg_draws() {
    let half = draw(
        LINES,
        &[o(
            serde_json::json!({"node": "line", "trimStart": 0.25, "trimEnd": 0.75}),
        )],
    );
    let edited = LINES.replace(
        r##"id="line" d="M10 50 H 190" stroke="#c00000""##,
        r##"id="line" d="M10 50 H 190" stroke="#c00000" stroke-dasharray="90 361" stroke-dashoffset="-45""##,
    );
    assert!(
        half == resvg(&edited),
        "the hand-dashed file, byte for byte"
    );
    assert_eq!(px(&half, 30, 50), [255, 255, 255, 255], "before the share");
    assert_eq!(px(&half, 100, 50)[0], 192, "inside it");
    assert_eq!(px(&half, 170, 50), [255, 255, 255, 255], "after it");
    let whole = draw(
        LINES,
        &[o(
            serde_json::json!({"node": "line", "trimStart": 0, "trimEnd": 1}),
        )],
    );
    assert!(whole == draw(LINES, &[]), "0–1 is the file");
    let none = draw(
        LINES,
        &[o(
            serde_json::json!({"node": "line", "trimStart": 0.4, "trimEnd": 0.4}),
        )],
    );
    assert_eq!(
        px(&none, 100, 50),
        [255, 255, 255, 255],
        "an empty share draws no stroke"
    );
    // Several sub-paths draw at one pace: at half the longest, the short
    // one is nine tenths drawn and the long one half.
    let pace = draw(
        LINES,
        &[o(serde_json::json!({"node": "two", "trimEnd": 0.5}))],
    );
    assert_eq!(px(&pace, 95, 20)[2], 192);
    assert_eq!(px(&pace, 105, 20), [255, 255, 255, 255]);
    assert_eq!(px(&pace, 95, 80)[2], 192);
    assert_eq!(px(&pace, 110, 80), [255, 255, 255, 255]);
}

/// What a reveal cannot apply to is refused with the reason.
#[test]
fn a_reveal_needs_an_undashed_stroke() {
    let p = Picture::parse(LINES.as_bytes(), &context()).unwrap();
    let model = p.model().unwrap();
    let err = model
        .check(
            &o(serde_json::json!({"node": "solid", "trimEnd": 0.5})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("no stroke to reveal"), "{err}");
    let err = model
        .check(
            &o(serde_json::json!({"node": "dashed", "trimEnd": 0.5})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("dashed stroke"), "{err}");
    let err = model
        .check(
            &o(serde_json::json!({"node": "line", "trimEnd": 1.5})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("outside 0–1"), "{err}");
}

/// A theme colour is a colour an override may name; drawn unresolved it
/// is the file's own (the host resolves it through the palette first).
#[test]
fn a_theme_colour_is_accepted_and_resolved_by_the_host() {
    let p = Picture::parse(LINES.as_bytes(), &context()).unwrap();
    let model = p.model().unwrap();
    model
        .check(
            &o(serde_json::json!({"node": "solid", "fill": "@accent"})),
            true,
        )
        .unwrap();
    let err = model
        .check(&o(serde_json::json!({"node": "solid", "fill": "@"})), true)
        .unwrap_err();
    assert!(err.contains("theme colour"), "{err}");
    let unresolved = draw(
        LINES,
        &[o(serde_json::json!({"node": "solid", "fill": "@accent"}))],
    );
    assert!(
        unresolved == draw(LINES, &[]),
        "an unresolved name draws the file's colour"
    );
    let mut bound = o(serde_json::json!({"node": "solid", "fill": "@accent",
        "fillStops": ["@a", "112233"]}));
    let names: Vec<String> = bound.colours_mut().map(|c| c.clone()).collect();
    assert_eq!(names, ["@accent", "@a", "112233"]);
}
