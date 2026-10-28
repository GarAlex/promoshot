//! Slice 3 (SVG-PLAN.md): what a layer keyframe may key on an element,
//! and the geometry the editor needs — each element's box through every
//! transform above it, and the element under a tap.

use std::path::PathBuf;

use promo_svg::vector::{NodeKey, VectorOverride};
use promo_svg::{Context, Picture};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn logo() -> Picture {
    let bytes = std::fs::read(corpus().join("logo.svg")).unwrap();
    Picture::parse(
        &bytes,
        &Context {
            file_dir: corpus(),
            project_root: corpus(),
        },
    )
    .unwrap()
}

fn key(json: serde_json::Value) -> NodeKey {
    serde_json::from_value(json).unwrap()
}

fn o(json: serde_json::Value) -> VectorOverride {
    serde_json::from_value(json).unwrap()
}

/// A keyframe keys tier A anywhere and paint on a painted shape, refused
/// with the override's own reasons outside them; `order` and `text` are
/// never keyed, and an unknown key is named.
#[test]
fn a_key_stays_inside_the_elements_tier() {
    let model = logo().model().unwrap();
    assert_eq!(
        model
            .check_key(
                "badge",
                &key(serde_json::json!({"opacity": 0.5, "transform": {"rotate": 9}}))
            )
            .unwrap(),
        "badge"
    );
    assert_eq!(
        model
            .check_key(
                "glyph",
                &key(serde_json::json!({"fill": "FF0000", "strokeWidth": 2}))
            )
            .unwrap(),
        "glyph"
    );
    let paint = model
        .check_key("badge", &key(serde_json::json!({"fill": "FF0000"})))
        .unwrap_err();
    let static_reason = model
        .check(
            &o(serde_json::json!({"node": "badge", "fill": "FF0000"})),
            true,
        )
        .unwrap_err();
    assert_eq!(paint, static_reason, "the same words as an override's");
    let order = model
        .check_key("glyph", &key(serde_json::json!({"order": 0})))
        .unwrap_err();
    assert!(order.contains("order is not keyed"), "{order}");
    let text = model
        .check_key("glyph", &key(serde_json::json!({"text": "Hi"})))
        .unwrap_err();
    assert!(text.contains("words are not keyed"), "{text}");
    let typo = model
        .check_key("glyph", &key(serde_json::json!({"colour": "FF0000"})))
        .unwrap_err();
    assert!(typo.contains("not \"colour\""), "{typo}");
    let missing = model
        .check_key("nope", &key(serde_json::json!({"opacity": 1})))
        .unwrap_err();
    assert!(missing.contains("no element \"nope\""), "{missing}");
    let range = model
        .check_key("glyph", &key(serde_json::json!({"opacity": 2})))
        .unwrap_err();
    assert!(range.contains("outside 0–1"), "{range}");
}

/// Boxes follow every transform above an element: the glyph's corners
/// turn with its badge, which its own override never touched.
#[test]
fn boxes_follow_ancestor_transforms() {
    let picture = logo();
    let plain = picture.element_boxes(&[]).unwrap();
    // The glyph's file box: x 26–60, y 22–60.
    assert_eq!(plain["glyph"][0], [26.0, 22.0]);
    assert_eq!(plain["glyph"][2], [60.0, 60.0]);
    assert!(plain.contains_key("badge/rect[0]") && plain.contains_key("wordmark"));
    // The badge turned a quarter about its own centre (40, 40): a point
    // (x, y) lands on (80 − y, x).
    let turned = picture
        .element_boxes(&[o(
            serde_json::json!({"node": "badge", "transform": {"rotate": 90}}),
        )])
        .unwrap();
    let corner = turned["glyph"][0];
    assert!(
        (corner[0] - 58.0).abs() < 0.01 && (corner[1] - 26.0).abs() < 0.01,
        "{corner:?}"
    );
    // A hidden element has no box.
    let hidden = picture
        .element_boxes(&[o(serde_json::json!({"node": "glyph", "visible": false}))])
        .unwrap();
    assert!(!hidden.contains_key("glyph"));
}

/// A tap names the innermost selectable element holding ink there, on
/// top in paint order; bare canvas names nothing; a moved element is hit
/// where it now is.
#[test]
fn a_tap_selects_the_element_under_it() {
    let picture = logo();
    let at =
        |overrides: &[VectorOverride], x: f64, y: f64| picture.hit(overrides, x, y, 0.0).unwrap();
    assert_eq!(
        at(&[], 30.0, 40.0).as_deref(),
        Some("glyph"),
        "the glyph's stem"
    );
    assert_eq!(
        at(&[], 10.0, 10.0).as_deref(),
        Some("badge/rect[0]"),
        "the badge's fill"
    );
    assert_eq!(at(&[], 100.0, 40.0).as_deref(), Some("wordmark/path[0]"));
    assert_eq!(at(&[], 120.0, 70.0), None, "bare canvas");
    // Inside the glyph's box but in its counter (the hole of the P): the
    // badge under it, not the glyph.
    assert_eq!(at(&[], 40.0, 36.0).as_deref(), Some("badge/rect[0]"));
    // The glyph moved 30 px down: its old place shows the badge, its
    // new place is the glyph (over the badge, drawn after it).
    let moved = [o(
        serde_json::json!({"node": "glyph", "transform": {"move": [0, 30]}}),
    )];
    assert_eq!(at(&moved, 30.0, 40.0).as_deref(), Some("badge/rect[0]"));
    assert_eq!(at(&moved, 30.0, 75.0).as_deref(), Some("glyph"));
    // A finger's slop reaches a stroke it does not quite touch.
    assert_eq!(at(&[], 96.5, 40.0), None);
    assert_eq!(
        picture.hit(&[], 96.5, 40.0, 2.0).unwrap().as_deref(),
        Some("wordmark/path[0]")
    );
}

/// Validate names a layer's key for an element the file lacks and one
/// outside its element's tier — once per element — and nothing for good
/// keys, also inside a composition.
#[test]
fn validate_names_keys_that_do_not_apply() {
    let dir = std::env::temp_dir().join(format!("promo-svg-keyed-{}", std::process::id()));
    let resources = dir.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::copy(corpus().join("logo.svg"), resources.join("logo.svg")).unwrap();
    let layer = |id: &str, keyframes: serde_json::Value| {
        serde_json::json!({"id": id, "name": id, "sortIndex": 1, "kind": "image",
            "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "L",
            "keyframes": keyframes})
    };
    let meta = promo_model::ProjectMetadata::from_json(
        &serde_json::json!({"id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 2, "videoDuration": 2, "subtitles": [],
        "compositionSettings": {},
        "layers": [layer("Top", serde_json::json!([
            {"id": "a", "time": 0, "nodes": {"gone": {"opacity": 0}, "badge": {"fill": "FF0000"}}},
            {"id": "b", "time": 1, "nodes": {"gone": {"opacity": 1}, "glyph": {"fill": "00FF00"}}}
        ]))],
        "resources": [
            {"id": "L", "kind": "vector", "filename": "logo.svg", "displayName": "Logo",
             "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
             "pixelWidth": 240, "pixelHeight": 80, "svg": {"size": [240, 80]}},
            {"id": "C", "kind": "composition", "filename": "", "displayName": "Scene",
             "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
             "composition": {"canvasWidth": 100, "canvasHeight": 100, "duration": 2,
                "layers": [layer("Inner", serde_json::json!([
                    {"id": "c", "time": 0, "nodes": {"glyph": {"order": 1}}}
                ]))]}}
        ]})
        .to_string(),
    )
    .unwrap();
    let found = promo_svg::findings(&meta, &resources);
    let has = |s: &str| found.iter().filter(|f| f.contains(s)).count();
    assert_eq!(
        has("keys \"gone\" of the SVG \"Logo\", which names no element"),
        1,
        "{found:#?}"
    );
    assert_eq!(
        has("\"Top\" keys the SVG \"Logo\" where it does not apply"),
        1,
        "{found:#?}"
    );
    assert_eq!(has("takes no paint"), 1, "{found:#?}");
    assert_eq!(
        has("\"Inner\" keys the SVG \"Logo\" where it does not apply — order is not keyed"),
        1,
        "{found:#?}"
    );
    assert_eq!(found.len(), 3, "{found:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The leftover outlines go: a `<hash12>.<file>` variant for words no SVG
/// has any more. The current outlines, the import's own (the file's own
/// words) and anything not named like a variant of a vector stay.
#[test]
fn unused_outline_variants_are_pruned() {
    let dir = std::env::temp_dir().join(format!("promo-svg-prune-{}", std::process::id()));
    let outlines = dir.join("Resources/.svg");
    std::fs::create_dir_all(&outlines).unwrap();
    for name in [
        "text.svg",
        "aaaaaaaaaaaa.text.svg",
        "bbbbbbbbbbbb.text.svg",
        "cccccccccccc.gone.svg",
        "notes.txt",
    ] {
        std::fs::write(outlines.join(name), "<svg/>").unwrap();
    }
    let meta = promo_model::ProjectMetadata::from_json(
        &serde_json::json!({"id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 2, "videoDuration": 2, "subtitles": [],
        "compositionSettings": {},
        "resources": [{"id": "T", "kind": "vector", "filename": "text.svg",
            "displayName": "Words", "addedAt": 0, "imageCuts": [],
            "disabledAudioTrackIndices": [],
            "svg": {"size": [360, 140], "outlines": "bbbbbbbbbbbb.text.svg",
                    "overrides": [{"node": "tagline", "text": "Hello"}]}}]})
        .to_string(),
    )
    .unwrap();
    let removed = promo_svg::prune_outlines(&meta, &dir.join("Resources"));
    assert_eq!(removed, vec!["aaaaaaaaaaaa.text.svg".to_string()]);
    let mut left: Vec<String> = std::fs::read_dir(&outlines)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "bbbbbbbbbbbb.text.svg",
            "cccccccccccc.gone.svg",
            "notes.txt",
            "text.svg"
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
