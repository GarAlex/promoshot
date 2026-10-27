//! Slice 2 (SVG-PLAN.md): the element tree, its tiers, and overrides.
//!
//! - Tiers and reasons on the corpus: what every element of a real file
//!   may change, and why not more.
//! - Overrides drawn against resvg called directly on an equivalently
//!   EDITED file — the source changed by hand the way the override says —
//!   so the edit path is held to the renderer, not to itself.
//! - Removing every override gives back exactly the file's picture.
//! - Out-of-tier edits refused with the reason the list shows.

use std::path::PathBuf;
use std::sync::Arc;

use promo_svg::vector::VectorOverride;
use promo_svg::{Context, Picture, Tier};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn context() -> Context {
    Context {
        file_dir: corpus(),
        project_root: corpus(),
    }
}

fn read(name: &str) -> String {
    std::fs::read_to_string(corpus().join(format!("{name}.svg"))).unwrap()
}

fn picture(text: &str) -> Picture {
    Picture::parse(text.as_bytes(), &context()).unwrap()
}

fn size_of(picture: &Picture) -> (u32, u32) {
    let (w, h) = picture.size();
    ((w * 2.0).round() as u32, (h * 2.0).round() as u32)
}

/// resvg on `bytes`, as its CLI drives it (no fonts, a scale).
fn resvg_directly(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let options = usvg::Options {
        resources_dir: Some(corpus()),
        fontdb: Arc::new(usvg::fontdb::Database::new()),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).unwrap();
    let mut pixmap = tiny_skia::Pixmap::new(width, height).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(
            width as f32 / tree.size().width(),
            height as f32 / tree.size().height(),
        ),
        &mut pixmap.as_mut(),
    );
    pixmap.take()
}

fn o(json: serde_json::Value) -> VectorOverride {
    serde_json::from_value(json).unwrap()
}

/// (largest channel difference, pixels that differ by more than 2).
fn difference(a: &[u8], b: &[u8]) -> (u8, usize) {
    assert_eq!(a.len(), b.len());
    let mut worst = 0u8;
    let mut count = 0;
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let d = p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        worst = worst.max(d);
        if d > 2 {
            count += 1;
        }
    }
    (worst, count)
}

/// The override drawn by promo-svg against `edited` drawn by resvg: the
/// same bytes — the edit goes into the file's own XML, so nothing it does
/// not touch is re-printed. And never the unedited picture.
fn assert_draws_as(file: &str, overrides: &[VectorOverride], edited: &str) {
    let plain = picture(&read(file));
    let (w, h) = size_of(&plain);
    let drawn = plain
        .with_overrides(overrides)
        .unwrap()
        .render(w, h, None)
        .unwrap();
    let expected = resvg_directly(edited.as_bytes(), w, h);
    let (worst, count) = difference(&drawn, &expected);
    assert!(
        drawn == expected,
        "{file}: {count} pixels differ (worst {worst}) from the hand-edited file"
    );
    let original = plain.render(w, h, None).unwrap();
    assert!(
        difference(&drawn, &original).1 > 20,
        "{file}: the override changed nothing"
    );
}

fn edit(file: &str, from: &str, to: &str) -> String {
    let text = read(file);
    assert_eq!(text.matches(from).count(), 1, "{file}: anchor {from}");
    text.replacen(from, to, 1)
}

#[test]
fn tiers_and_reasons_on_the_corpus() {
    let tier = |file: &str, node: &str| {
        let p = picture(&read(file));
        let m = p.model().unwrap();
        let n = m.node(node).unwrap_or_else(|| panic!("{file}: no {node}"));
        (n.tier, n.reason.clone())
    };
    // Plain shapes and groups.
    assert_eq!(tier("logo", "badge").0, Tier::A);
    assert_eq!(tier("logo", "glyph"), (Tier::C, "a plain shape".into()));
    assert_eq!(tier("logo", "badge/rect[0]").0, Tier::C);
    // Under effects: paint still edits, geometry is not plain.
    assert_eq!(
        tier("blur", "circle[0]"),
        (Tier::B, "a painted shape, under a blur filter".into())
    );
    assert_eq!(
        tier("blur", "rect[1]"),
        (
            Tier::B,
            "a painted shape, under a drop shadow filter".into()
        )
    );
    assert_eq!(
        tier("clip", "g[0]/rect[0]"),
        (Tier::B, "a painted shape, clipped".into())
    );
    assert_eq!(tier("clip", "rect[1]").0, Tier::B);
    assert_eq!(
        tier("mask", "rect[1]"),
        (Tier::B, "a painted shape, masked".into())
    );
    // Locked, each with its reason — and listed, never hidden.
    assert_eq!(
        tier("mask", "lum/rect[0]"),
        (Tier::Locked, "kept as drawn — used as a mask".into())
    );
    assert_eq!(
        tier("clip", "round/circle[0]"),
        (Tier::Locked, "kept as drawn — used as a clip".into())
    );
    assert_eq!(
        tier("blur", "soft"),
        (Tier::Locked, "kept as drawn — a blur filter".into())
    );
    assert_eq!(
        tier("use", "heart/path[0]"),
        (
            Tier::Locked,
            "kept as drawn — copied elsewhere with <use>".into()
        )
    );
    assert_eq!(tier("use", "dot").0, Tier::Locked);
    // A <use> copy: placement only.
    assert_eq!(tier("use", "use[0]").0, Tier::A);
    assert_eq!(tier("use", "g[0]/use[2]").0, Tier::A);
    // CSS-painted shapes read the paint CSS gives them.
    let css = picture(&read("css"));
    let m = css.model().unwrap();
    assert_eq!(
        m.node("rect[0]")
            .unwrap()
            .fill
            .as_ref()
            .unwrap()
            .color
            .as_deref(),
        Some("1D3557")
    );
    assert_eq!(m.node("star").unwrap().tier, Tier::C);
    // Text with no outlines draws nothing, and says what to do.
    let text = picture(&read("text"));
    let m = text.model().unwrap();
    let wordmark = m.node("wordmark").unwrap();
    assert_eq!(wordmark.tier, Tier::Locked);
    assert!(wordmark.reason.contains("import"), "{}", wordmark.reason);
    assert_eq!(wordmark.text.as_deref(), Some("PromoShot"));
    // Every row of every file has a reason; a gradient-only <defs> is no row.
    for file in [
        "transforms",
        "css",
        "use",
        "arcs",
        "gradients",
        "clip",
        "mask",
        "blur",
        "images",
        "logo",
    ] {
        let p = picture(&read(file));
        let m = p.model().unwrap();
        assert!(!m.nodes().is_empty(), "{file}");
        for n in m.nodes() {
            assert!(!n.reason.is_empty(), "{file}: {}", n.node);
            if n.tier != Tier::Locked {
                assert!(n.drawn && n.bounds.is_some(), "{file}: {}", n.node);
            }
        }
    }
    assert!(picture(&read("logo"))
        .model()
        .unwrap()
        .node("defs[0]")
        .is_none());
}

/// The report's sentences, refused edits included, are the editor's.
#[test]
fn out_of_tier_edits_are_refused_with_the_reason() {
    let mask = picture(&read("mask"));
    let m = mask.model().unwrap();
    let err = m
        .check(
            &o(serde_json::json!({"node": "lum/rect[0]", "opacity": 0.5})),
            true,
        )
        .unwrap_err();
    assert!(
        err.contains("used as a mask") && err.contains("cannot change"),
        "{err}"
    );
    let logo = picture(&read("logo"));
    let m = logo.model().unwrap();
    let err = m
        .check(
            &o(serde_json::json!({"node": "badge", "fill": "FF0000"})),
            true,
        )
        .unwrap_err();
    assert!(
        err.contains("takes no paint") && err.contains("a group"),
        "{err}"
    );
    let err = m
        .check(
            &o(serde_json::json!({"node": "glyph", "fillStops": ["FF0000"]})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("not a gradient"), "{err}");
    let err = m
        .check(
            &o(serde_json::json!({"node": "badge/rect[0]", "fillStops": ["FF0000"]})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("has 2 stops"), "{err}");
    let err = m
        .check(
            &o(serde_json::json!({"node": "glyph", "fillColor": "FF0000"})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("not \"fillColor\""), "{err}");
    let err = m
        .check(
            &o(serde_json::json!({"node": "nowhere", "opacity": 1.0})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("no element \"nowhere\""), "{err}");
    let err = m
        .check(&o(serde_json::json!({"node": "glyph", "text": "Hi"})), true)
        .unwrap_err();
    assert!(err.contains("not text"), "{err}");
    let err = m
        .check(
            &o(serde_json::json!({"node": "glyph", "fill": "red"})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("not a colour"), "{err}");
    assert_eq!(
        m.check(
            &o(serde_json::json!({"node": "glyph", "fill": "#FF000080"})),
            true
        )
        .unwrap(),
        "glyph"
    );
    // A pattern-painted shape keeps its paint.
    let patterned = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
        <defs><pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse">
          <rect width="4" height="4" fill="#f00"/></pattern></defs>
        <rect width="40" height="40" fill="url(#p)"/></svg>"##;
    let p = picture(patterned);
    let m = p.model().unwrap();
    assert_eq!(m.node("rect[0]").unwrap().tier, Tier::A);
    assert_eq!(
        m.node("p/rect[0]").unwrap().reason,
        "kept as drawn — a pattern tile"
    );
    let err = m
        .check(
            &o(serde_json::json!({"node": "rect[0]", "fill": "00FF00"})),
            true,
        )
        .unwrap_err();
    assert!(err.contains("pattern"), "{err}");
}

#[test]
fn paint_overrides_draw_as_the_edited_file() {
    assert_draws_as(
        "logo",
        &[o(serde_json::json!({"node": "glyph", "fill": "FF0000"}))],
        &edit("logo", r#"fill="white"/>"#, r##"fill="#FF0000"/>"##),
    );
    // A colour on a path whose paint its group gives.
    assert_draws_as(
        "logo",
        &[o(
            serde_json::json!({"node": "wordmark/path[2]", "fill": "00AA00"}),
        )],
        &edit(
            "logo",
            "<path d=\"M147 33.5",
            "<path fill=\"#00AA00\" d=\"M147 33.5",
        ),
    );
    assert_draws_as(
        "logo",
        &[o(
            serde_json::json!({"node": "badge/rect[0]", "fillStops": ["00FF00", "0000FF80"]}),
        )],
        &edit(
            "logo",
            r##"<stop stop-color="#7B5CFF"/>
      <stop offset="1" stop-color="#FF5C8A"/>"##,
            r##"<stop stop-color="#00FF00"/>
      <stop offset="1" stop-color="#0000FF" stop-opacity="0.50196"/>"##,
        ),
    );
    assert_draws_as(
        "logo",
        &[o(
            serde_json::json!({"node": "badge/rect[1]", "stroke": "000000", "strokeWidth": 6.0}),
        )],
        &edit(
            "logo",
            r#"stroke="white" stroke-opacity="0.25" stroke-width="2""#,
            r#"stroke="black" stroke-opacity="0.25" stroke-width="6""#,
        ),
    );
    // CSS gives the paint; the override wins over the class.
    assert_draws_as(
        "css",
        &[o(serde_json::json!({"node": "rect[0]", "fill": "00FF00"}))],
        &edit(
            "css",
            ".cls-1 { fill: #1d3557; }",
            ".cls-1 { fill: #00ff00; }",
        ),
    );
    // Under a blur, the new colour is blurred.
    assert_draws_as(
        "blur",
        &[o(
            serde_json::json!({"node": "circle[0]", "fill": "FF8800"}),
        )],
        &edit(
            "blur",
            r##"fill="#4361ee" filter"##,
            r##"fill="#ff8800" filter"##,
        ),
    );
}

#[test]
fn placement_overrides_draw_as_the_edited_file() {
    assert_draws_as(
        "logo",
        &[o(serde_json::json!({"node": "badge", "opacity": 0.5}))],
        &edit(
            "logo",
            r#"<g id="badge">"#,
            r#"<g id="badge" opacity="0.5">"#,
        ),
    );
    assert_draws_as(
        "logo",
        &[o(serde_json::json!({"node": "wordmark", "visible": false}))],
        &edit(
            "logo",
            r##"<g id="wordmark" fill="#1B1B3A">"##,
            r##"<g id="wordmark" fill="#1B1B3A" display="none">"##,
        ),
    );
    // A move, scale and turn about the badge's centre (40, 40).
    assert_draws_as(
        "logo",
        &[o(serde_json::json!({"node": "badge", "transform": {"move": [12.0, -4.0], "rotate": 30.0, "scale": 0.8}}))],
        &edit(
            "logo",
            r#"<g id="badge">"#,
            r#"<g transform="translate(52 36) rotate(30) scale(0.8) translate(-40 -40)"><g id="badge">"#,
        )
        .replacen("</g>\n  <g id=\"wordmark\"", "</g></g>\n  <g id=\"wordmark\"", 1),
    );
    // Order: the glyph drawn first, under the badge's fill.
    assert_draws_as(
        "logo",
        &[o(serde_json::json!({"node": "glyph", "order": 0}))],
        &{
            let text = read("logo");
            let start = text.find("<path id=\"glyph\"").unwrap();
            let end = start + text[start..].find("/>").unwrap() + 2;
            let glyph = text[start..end].to_string();
            let without = format!("{}{}", &text[..start], &text[end..]);
            let first = without.find("<rect width=\"80\"").unwrap();
            format!("{}{glyph}{}", &without[..first], &without[first..])
        },
    );
    // A clipped group moves with its clip.
    assert_draws_as(
        "clip",
        &[o(
            serde_json::json!({"node": "g[0]", "transform": {"move": [30.0, 0.0]}}),
        )],
        &edit(
            "clip",
            r#"<g clip-path="url(#round)">"#,
            r#"<g transform="translate(30 0)"><g clip-path="url(#round)">"#,
        )
        .replacen("</g>\n  <rect x=\"180\"", "</g></g>\n  <rect x=\"180\"", 1),
    );
    // A <use> copy turns and fades as one.
    assert_draws_as(
        "use",
        &[o(serde_json::json!({"node": "use[2]", "opacity": 0.4}))],
        &edit(
            "use",
            r##"<use href="#heart" x="190" y="40" width="40" height="40" fill="#003049"/>"##,
            r##"<use href="#heart" x="190" y="40" width="40" height="40" fill="#003049" opacity="0.4"/>"##,
        ),
    );
}

/// A move is in the picture's own pixels, whatever the element's parents
/// do: the navy circle (under translate, rotate(30), scale(0.5) and the
/// viewBox's millimetres) moves 10 px right and nowhere else.
#[test]
fn a_move_is_in_the_pictures_pixels() {
    let p = picture(&read("transforms"));
    let (w, h) = size_of(&p);
    let centroid = |px: &[u8]| {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for (i, c) in px.chunks_exact(4).enumerate() {
            let (x, y) = ((i as u32 % w) as f64, (i as u32 / w) as f64);
            // #29335C, left of the polygon.
            if x < w as f64 * 0.4 && c[0] < 60 && c[1] < 70 && c[2] > 70 && c[2] < 110 {
                sx += x;
                sy += y;
                n += 1.0;
            }
        }
        assert!(n > 100.0, "{n}");
        (sx / n, sy / n)
    };
    let before = centroid(&p.render(w, h, None).unwrap());
    let moved = p
        .with_overrides(&[o(serde_json::json!({"node": "g[0]/g[0]/g[0]/circle[0]", "transform": {"move": [10.0, 0.0]}}))])
        .unwrap();
    let after = centroid(&moved.render(w, h, None).unwrap());
    // Two raster pixels per CSS pixel.
    assert!(
        (after.0 - before.0 - 20.0).abs() < 1.0,
        "{before:?} → {after:?}"
    );
    assert!((after.1 - before.1).abs() < 1.0, "{before:?} → {after:?}");
    // Turning a shape about its centre keeps its centre.
    let spun = p
        .with_overrides(&[o(
            serde_json::json!({"node": "g[0]/g[0]/g[0]/circle[0]", "transform": {"rotate": 90.0}}),
        )])
        .unwrap();
    let still = centroid(&spun.render(w, h, None).unwrap());
    assert!((still.0 - before.0).abs() < 1.0 && (still.1 - before.1).abs() < 1.0);
}

/// Stops on one shape of a shared gradient leave the other's.
#[test]
fn a_shared_gradient_keeps_its_stops_elsewhere() {
    let shared = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40">
        <defs><linearGradient id="g" x1="0" x2="1"><stop offset="0" stop-color="#f00"/>
        <stop offset="1" stop-color="#00f"/></linearGradient></defs>
        <rect id="a" width="50" height="40" fill="url(#g)"/>
        <rect id="b" x="50" width="50" height="40" fill="url(#g)"/></svg>"##;
    let p = picture(shared);
    let edited = p
        .with_overrides(&[o(
            serde_json::json!({"node": "a", "fillStops": ["00FF00", "00FF00"]}),
        )])
        .unwrap();
    let px = edited.render(100, 40, None).unwrap();
    let at = |x: usize, y: usize| &px[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
    assert_eq!(at(25, 20), [0, 255, 0, 255]);
    let plain = p.render(100, 40, None).unwrap();
    assert_eq!(
        at(75, 20),
        &plain[(20 * 100 + 75) * 4..(20 * 100 + 75) * 4 + 4]
    );
}

/// Removing every override gives back exactly the file's picture; and
/// overrides that do not change the tree (none, or only words) draw it
/// unchanged. An override that changes nothing visible goes through the
/// edit — the file read, changed, written and parsed again — and is still
/// the file's picture to the byte: the round trip loses nothing.
#[test]
fn no_override_is_the_files_picture() {
    for file in [
        "transforms",
        "css",
        "use",
        "arcs",
        "gradients",
        "clip",
        "mask",
        "blur",
        "images",
        "logo",
    ] {
        let p = picture(&read(file));
        let (w, h) = size_of(&p);
        let plain = p.render(w, h, None).unwrap();
        assert_eq!(
            p.with_overrides(&[]).unwrap().render(w, h, None).unwrap(),
            plain,
            "{file}"
        );
        assert_eq!(p.with_overrides(&[]).unwrap().stamp(), p.stamp());
        // An order that keeps the first drawn element first: the whole
        // tree written and read again.
        let m = p.model().unwrap();
        let first = m
            .nodes()
            .iter()
            .find(|n| n.tier != Tier::Locked && n.order.is_some())
            .unwrap();
        let same = o(serde_json::json!({"node": first.node, "order": first.order.unwrap()}));
        let through = p.with_overrides(&[same]).unwrap();
        assert_ne!(through.stamp(), p.stamp());
        let (worst, count) = difference(&through.render(w, h, None).unwrap(), &plain);
        assert!(
            through.render(w, h, None).unwrap() == plain,
            "{file}: {count} pixels, worst {worst}"
        );
    }
}

/// Text: the words change on the machine that edits them, into outlines,
/// so drawing still needs no font; the outlines name every element, so
/// the text and its neighbours stay editable.
#[test]
fn a_text_override_is_outlined_on_the_editing_machine() {
    let dir = std::env::temp_dir().join(format!("promo-svg-text-{}", std::process::id()));
    let resources = dir.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::copy(corpus().join("text.svg"), resources.join("text.svg")).unwrap();
    let installed = promo_svg::install(&resources, "text.svg").unwrap();
    let block: promo_svg::vector::VectorBlock =
        serde_json::from_value(installed["svg"].clone()).unwrap();
    let path = resources.join("text.svg");
    let plain = promo_svg::load_path(&path, Some(&block)).unwrap();
    let m = plain.model().unwrap();
    let tagline = m.node("tagline").unwrap();
    assert_eq!(tagline.tier, Tier::A, "{}", tagline.reason);
    assert_eq!(tagline.text.as_deref(), Some("Now on iPad"));
    assert!(m.editable());
    if promo_svg::system_fonts().is_empty() {
        return;
    }
    // The outlines are the file with its words as glyphs: they draw what
    // resvg draws from the file with this machine's fonts.
    let outlined = std::fs::read_to_string(resources.join(".svg/text.svg")).unwrap();
    assert!(outlined.contains(promo_svg::MARKER) && !outlined.contains("<text"));
    let with_fonts = {
        let options = usvg::Options {
            fontdb: promo_svg::system_fonts(),
            ..Default::default()
        };
        let tree = usvg::Tree::from_data(read("text").as_bytes(), &options).unwrap();
        let mut pixmap = tiny_skia::Pixmap::new(360, 140).unwrap();
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        pixmap.take()
    };
    let (worst, count) = difference(&plain.render(360, 140, None).unwrap(), &with_fonts);
    // Only the glyphs are re-printed (by usvg's writer, which rounds):
    // a few edge pixels by a few levels.
    assert!(
        count <= 8 && worst < 32,
        "{count} pixels differ (worst {worst}) from resvg with fonts"
    );
    let texts = vec![("wordmark".to_string(), "Hello".to_string())];
    let edited = promo_svg::outline(&resources, "text.svg", &texts).unwrap();
    let name = edited["svg"]["outlines"].as_str().unwrap().to_string();
    assert_ne!(name, "text.svg", "words get outlines of their own");
    assert!(resources.join(".svg").join(&name).is_file());
    assert!(
        resources.join(".svg/text.svg").is_file(),
        "the old words' outlines stay, for undo"
    );
    let edited_block: promo_svg::vector::VectorBlock =
        serde_json::from_value(edited["svg"].clone()).unwrap();
    let hello = promo_svg::load_path(&path, Some(&edited_block)).unwrap();
    let a = plain.render(360, 140, None).unwrap();
    let b = hello.render(360, 140, None).unwrap();
    assert!(difference(&a, &b).1 > 200, "the words changed");
    // And a placement edit still finds the text in the new outlines.
    let moved = hello
        .with_overrides(&[o(
            serde_json::json!({"node": "wordmark", "transform": {"move": [0.0, 20.0]}}),
        )])
        .unwrap()
        .render(360, 140, None)
        .unwrap();
    assert!(difference(&moved, &b).1 > 200);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The author's names for unnamed elements are kept on import and name
/// them for a tool.
#[test]
fn author_names_are_kept_and_resolve() {
    let inkscape = r##"<svg xmlns="http://www.w3.org/2000/svg"
        xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" width="40" height="40">
        <g inkscape:label="Layer 1"><rect width="20" height="20" fill="#f00" data-name="Red box"/>
        <circle cx="30" cy="30" r="5"><title>Dot</title></circle></g></svg>"##;
    let dir = std::env::temp_dir().join(format!("promo-svg-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("names.svg"), inkscape).unwrap();
    let installed = promo_svg::install(&dir, "names.svg").unwrap();
    assert_eq!(installed["svg"]["handles"]["g[0]/rect[0]"], "Red box");
    assert_eq!(installed["svg"]["handles"]["g[0]"], "Layer 1");
    assert_eq!(installed["svg"]["handles"]["g[0]/circle[0]"], "Dot");
    let p = picture(inkscape);
    let m = p.model().unwrap();
    assert_eq!(m.resolve("Red box").unwrap(), "g[0]/rect[0]");
    assert_eq!(m.node("g[0]").unwrap().name.as_deref(), Some("Layer 1"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Validate names an override for an element the file lacks, one the
/// element's tier refuses, and new words with no outlines of their own.
#[test]
fn validate_names_overrides_that_do_not_apply() {
    let dir = std::env::temp_dir().join(format!("promo-svg-findings-{}", std::process::id()));
    let resources = dir.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::copy(corpus().join("logo.svg"), resources.join("logo.svg")).unwrap();
    std::fs::copy(corpus().join("text.svg"), resources.join("text.svg")).unwrap();
    let words = promo_svg::install(&resources, "text.svg").unwrap();
    let meta = promo_model::ProjectMetadata::from_json(
        &serde_json::json!({"id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 2, "videoDuration": 2, "subtitles": [],
        "compositionSettings": {},
        "resources": [
            {"id": "L", "kind": "vector", "filename": "logo.svg", "displayName": "Logo",
             "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
             "pixelWidth": 240, "pixelHeight": 80,
             "svg": {"size": [240, 80], "overrides": [
                 {"node": "gone", "opacity": 0.5},
                 {"node": "badge", "fill": "FF0000"},
                 {"node": "glyph", "fill": "FF0000"}]}},
            {"id": "T", "kind": "vector", "filename": "text.svg", "displayName": "Words",
             "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
             "pixelWidth": 360, "pixelHeight": 140,
             "svg": {"size": [360, 140], "outlines": words["svg"]["outlines"],
                     "sourceHash": words["svg"]["sourceHash"],
                     "overrides": [{"node": "tagline", "text": "Hello"}]}}
        ]})
        .to_string(),
    )
    .unwrap();
    let found = promo_svg::findings(&meta, &resources);
    let has = |s: &str| found.iter().any(|f| f.contains(s));
    assert!(has("\"gone\", which names no element"), "{found:#?}");
    assert!(has("takes no paint"), "{found:#?}");
    assert!(has("not outlined yet"), "{found:#?}");
    assert_eq!(
        found.iter().filter(|f| f.contains("\"Logo\"")).count(),
        2,
        "{found:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
