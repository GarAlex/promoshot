//! The SVG corpus (SVG-PLAN.md slice 1): realistic files authored for the
//! features the format claims — transforms and units, CSS, `<use>`, arcs,
//! gradients, clips, masks, filters, embedded pictures, text — drawn
//! through promo-svg and compared:
//!
//! - against resvg called directly on the same bytes: pixel-identical, so
//!   the guards and the crop transform change nothing they should not;
//! - against the reference images in `tests/reference/` (resvg 0.47's
//!   output, checked in): a renderer change is a pixel change made on
//!   purpose. `PROMO_UPDATE_SVG_REFERENCES=1` rewrites them. The same
//!   references pass on macOS, Linux and the iOS simulator — that is the
//!   platform-parity claim, in bytes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use promo_svg::{Context, Picture, Refusal};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/reference")
}

/// Every corpus file but the perf one, by name.
const FILES: &[&str] = &[
    "transforms",
    "css",
    "use",
    "arcs",
    "gradients",
    "clip",
    "mask",
    "blur",
    "images",
    "text",
    "logo",
];

fn context() -> Context {
    Context {
        file_dir: corpus(),
        project_root: corpus(),
    }
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(corpus().join(format!("{name}.svg"))).unwrap()
}

/// Two pixels per CSS pixel: every file's natural size is whole, so the
/// scale is exact on both axes and our transform equals resvg's own.
fn size_of(picture: &Picture) -> (u32, u32) {
    let (w, h) = picture.size();
    ((w * 2.0).round() as u32, (h * 2.0).round() as u32)
}

/// resvg as its own CLI drives it: usvg's default options with an empty
/// font database (the render never has fonts), the file's folder for
/// relative hrefs, a scale transform.
fn resvg_directly(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let options = usvg::Options {
        resources_dir: Some(corpus()),
        fontdb: Arc::new(usvg::fontdb::Database::new()),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).unwrap();
    let scale = width as f32 / tree.size().width();
    let mut pixmap = tiny_skia::Pixmap::new(width, height).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, height as f32 / tree.size().height()),
        &mut pixmap.as_mut(),
    );
    pixmap.take()
}

#[test]
fn every_corpus_file_draws_as_resvg_draws_it() {
    for name in FILES.iter().chain(&["complex"]) {
        let bytes = read(name);
        let picture = Picture::parse(&bytes, &context()).unwrap();
        let (w, h) = size_of(&picture);
        let ours = picture.render(w, h, None).unwrap();
        let theirs = resvg_directly(&bytes, w, h);
        assert_eq!(ours.len(), theirs.len(), "{name}");
        let differing = ours.iter().zip(&theirs).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "{name}: {differing} bytes differ from resvg");
        // And it drew something.
        assert!(ours.chunks_exact(4).any(|p| p[3] > 0), "{name}: blank");
    }
}

/// Straight RGBA, the way a PNG holds it.
fn straight(premultiplied: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixmap = tiny_skia::Pixmap::from_vec(
        premultiplied.to_vec(),
        tiny_skia::IntSize::from_wh(width, height).unwrap(),
    )
    .unwrap();
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn read_png(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    buf.truncate(info.buffer_size());
    Some((buf, info.width, info.height))
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(rgba)
        .unwrap();
}

/// What each reference draws from: the file, or for text the outlines an
/// import on the reference machine made (`tests/fixtures`), since the
/// text itself is drawn with no fonts at all.
fn reference_source(name: &str) -> Vec<u8> {
    if name == "text" {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/text.outlines.svg");
        return std::fs::read(fixture).unwrap();
    }
    read(name)
}

#[test]
fn every_feature_matches_its_reference_image() {
    let update = std::env::var_os("PROMO_UPDATE_SVG_REFERENCES").is_some();
    if update {
        std::fs::create_dir_all(reference_dir()).unwrap();
        // The text fixture: this machine's outlines of text.svg.
        let imported =
            promo_svg::import_bytes(&read("text"), &context(), promo_svg::system_fonts()).unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        std::fs::create_dir_all(&fixtures).unwrap();
        std::fs::write(
            fixtures.join("text.outlines.svg"),
            imported.outlines.unwrap(),
        )
        .unwrap();
    }
    let mut failures = Vec::new();
    for name in FILES {
        let picture = Picture::parse(&reference_source(name), &context()).unwrap();
        let (w, h) = size_of(&picture);
        let rgba = straight(&picture.render(w, h, None).unwrap(), w, h);
        let path = reference_dir().join(format!("{name}.png"));
        if update {
            write_png(&path, &rgba, w, h);
            continue;
        }
        let Some((expected, ew, eh)) = read_png(&path) else {
            failures.push(format!("{name}: no reference at {}", path.display()));
            continue;
        };
        if (ew, eh) != (w, h) {
            failures.push(format!("{name}: {w}x{h}, the reference is {ew}x{eh}"));
            continue;
        }
        let differing = rgba
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        if differing > 0 {
            failures.push(format!(
                "{name}: {differing} pixels differ from the reference"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A crop is the matching part of the whole: the same geometry, within
/// one level of a channel (a translated gradient rounds a step apart).
#[test]
fn a_crop_is_that_part_of_the_whole() {
    let picture = Picture::parse(&read("gradients"), &context()).unwrap();
    // 400x200 at 2x: the right half is columns 400..800.
    let whole = picture.render(800, 400, None).unwrap();
    let half = picture
        .render(400, 400, Some([0.5, 0.0, 0.5, 1.0]))
        .unwrap();
    let mut worst = 0u8;
    for y in 0..400usize {
        let row = &whole[(y * 800 + 400) * 4..(y * 800 + 800) * 4];
        for (a, b) in row.iter().zip(&half[y * 400 * 4..(y + 1) * 400 * 4]) {
            worst = worst.max(a.abs_diff(*b));
        }
    }
    assert!(worst <= 1, "a crop strays {worst} levels from the whole");
}

#[test]
fn links_and_paths_outside_the_project_are_refused() {
    let picture = Picture::parse(&read("images"), &context()).unwrap();
    let refused = picture.refused().join("\n");
    assert!(
        refused.contains("https://example.com/tracker.png (a link"),
        "{refused}"
    );
    assert!(
        refused.contains("../../Cargo.toml (outside the project"),
        "{refused}"
    );
    // The embedded data: PNG draws: a pixel of its checkerboard, scaled
    // up nearest-neighbour, is opaque in the top-left quarter.
    let rgba = picture.render(240, 120, None).unwrap();
    let at = |x: usize, y: usize| &rgba[(y * 240 + x) * 4..(y * 240 + x) * 4 + 4];
    assert_eq!(at(12, 12)[3], 255);
    // The refused pictures draw nothing: white page under the dashed box.
    assert_eq!(at(180, 60), &[255, 255, 255, 255]);
}

#[test]
fn a_file_over_the_byte_cap_is_refused() {
    let mut big = b"<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><!--".to_vec();
    big.resize(17 << 20, b' ');
    big.extend_from_slice(b"--></svg>");
    match Picture::parse(&big, &context()) {
        Err(Refusal::TooManyBytes { .. }) => {}
        other => panic!("expected the byte cap, got {other:?}"),
    }
}

/// Ten levels of ten `<use>` each is 10^10 elements: refused before usvg
/// builds a single one.
#[test]
fn a_use_bomb_is_refused_before_it_is_built() {
    let mut svg = String::from(
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' \
         width='10' height='10'><defs><rect id='l0' width='1' height='1'/>",
    );
    for level in 1..=10 {
        svg.push_str(&format!("<g id='l{level}'>"));
        for _ in 0..10 {
            svg.push_str(&format!("<use xlink:href='#l{}'/>", level - 1));
        }
        svg.push_str("</g>");
    }
    svg.push_str("</defs><use xlink:href='#l10'/></svg>");
    let started = std::time::Instant::now();
    match Picture::parse(svg.as_bytes(), &context()) {
        Err(Refusal::TooManyNodes { nodes, .. }) => assert!(nodes > 1_000_000),
        other => panic!("expected the node cap, got {other:?}"),
    }
    assert!(
        started.elapsed().as_secs_f64() < 1.0,
        "the refusal itself must be quick"
    );
}

#[test]
fn a_filter_region_over_the_cap_is_refused() {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'>\
               <filter id='f' filterUnits='userSpaceOnUse' x='-5000' y='-5000' width='10000' \
               height='10000'><feGaussianBlur stdDeviation='4'/></filter>\
               <rect width='10' height='10' filter='url(#f)'/></svg>";
    match Picture::parse(svg.as_bytes(), &context()) {
        Err(Refusal::FilterRegion { times, .. }) => assert!(times > 16.0),
        other => panic!("expected the filter cap, got {other:?}"),
    }
}

/// The import turns text into outline paths with this machine's fonts,
/// names the family it could not find, and the render from the outlines
/// needs no font at all.
#[test]
fn the_import_turns_text_into_outlines() {
    let project = std::env::temp_dir().join(format!("promo-svg-import-{}", std::process::id()));
    let resources = project.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::copy(corpus().join("text.svg"), resources.join("text.svg")).unwrap();
    let installed = promo_svg::install(&resources, "text.svg").unwrap();
    let block = &installed["svg"];
    assert_eq!(block["size"], serde_json::json!([360.0, 140.0]));
    assert_eq!(block["outlines"], "text.svg");
    assert_eq!(
        block["sourceHash"].as_str().unwrap(),
        promo_svg::sha256_hex(&read("text"))
    );
    let missing = installed["missingFonts"].to_string();
    assert!(missing.contains("No Such Font Family"), "{missing}");
    let outlines = std::fs::read_to_string(resources.join(".svg/text.svg")).unwrap();
    assert!(!outlines.contains("<text"), "the outlines hold no text");
    // The original is untouched.
    assert_eq!(
        std::fs::read(resources.join("text.svg")).unwrap(),
        read("text")
    );
    if promo_svg::system_fonts().is_empty() {
        // A bare container has no fonts: nothing to outline with.
        return;
    }
    assert!(outlines.contains("<path"));
    let resource: promo_model::ProjectResource = serde_json::from_value(serde_json::json!({
        "id": "T", "kind": "vector", "filename": "text.svg", "displayName": "Text",
        "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [], "svg": block
    }))
    .unwrap();
    let (_, from_outlines) = promo_svg::render_source(&resources, &resource).unwrap();
    assert!(from_outlines, "the render reads the outlines");
    let picture = promo_svg::load(&resources, &resource).unwrap();
    let rgba = picture.render(360, 140, None).unwrap();
    // Gold ink in the wordmark's band, where the original drawn without
    // fonts has only the badge's navy.
    let gold = rgba
        .chunks_exact(4)
        .enumerate()
        .filter(|(i, p)| {
            let y = i / 360;
            (35..75).contains(&y) && p[0] > 200 && p[1] > 120 && p[2] < 80
        })
        .count();
    assert!(
        gold > 200,
        "the wordmark draws from its outlines ({gold} px)"
    );
    // An edited original makes the outlines stale: the render goes back to
    // the file itself (text draws nothing) rather than drawing old words.
    std::fs::write(
        resources.join("text.svg"),
        String::from_utf8(read("text"))
            .unwrap()
            .replace("Now on iPad", "Now on Mac"),
    )
    .unwrap();
    let (_, from_outlines) = promo_svg::render_source(&resources, &resource).unwrap();
    assert!(!from_outlines);
    let _ = std::fs::remove_dir_all(&project);
}

/// The render-cost measure behind SVG-PLAN.md's "As built": a logo-sized
/// and a complex file at 1080p and 4K. `cargo test --release -p promo-svg
/// --test corpus -- --ignored --nocapture render_cost`.
#[test]
#[ignore]
fn render_cost() {
    let measure = |name: &str, w: u32, h: u32| {
        let bytes = read(name);
        let started = std::time::Instant::now();
        let picture = Picture::parse(&bytes, &context()).unwrap();
        let parse = started.elapsed().as_secs_f64() * 1000.0;
        let runs = 10;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(picture.render(w, h, None).unwrap());
        }
        let render = started.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("{name:8} {w:>4}x{h:<4} parse {parse:7.2} ms  render {render:7.2} ms");
    };
    // Any file: PROMO_SVG_COST=<path.svg> measures it full-frame too.
    if let Some(path) = std::env::var_os("PROMO_SVG_COST") {
        let bytes = std::fs::read(&path).unwrap();
        let picture = Picture::parse(&bytes, &context()).unwrap();
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            let started = std::time::Instant::now();
            std::hint::black_box(picture.render(w, h, None).unwrap());
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            println!("{:?} {w}x{h} render {ms:7.2} ms", path);
        }
    }
    // A logo a third of the frame wide, and the frame filled.
    measure("logo", 640, 213);
    measure("logo", 1280, 427);
    measure("logo", 1920, 640);
    measure("complex", 1920, 1080);
    measure("complex", 3840, 2160);
}

/// The per-frame cost of a keyed element (SVG-PLAN.md slice 3, "As
/// built"): each frame whose keyed values changed edits the file's XML,
/// parses the edited tree and draws it; a frame in a hold only stamps the
/// values (the raster is cached). `cargo test --release -p promo-svg
/// --test corpus -- --ignored --nocapture keyed_cost`.
#[test]
#[ignore]
fn keyed_cost() {
    let measure = |name: &str, node: &str, w: u32, h: u32| {
        let bytes = read(name);
        let picture = Picture::parse(&bytes, &context()).unwrap();
        let o = |v: f64| {
            vec![promo_model::vector::VectorOverride {
                node: node.into(),
                opacity: Some(v),
                transform: Some(promo_model::vector::NodeTransform {
                    rotate: Some(v * 30.0),
                    ..Default::default()
                }),
                ..Default::default()
            }]
        };
        let started = std::time::Instant::now();
        std::hint::black_box(picture.with_overrides(&o(0.01)).unwrap());
        let first = started.elapsed().as_secs_f64() * 1000.0;
        let runs = 10;
        let started = std::time::Instant::now();
        for i in 0..runs {
            std::hint::black_box(picture.with_overrides(&o(0.1 + i as f64 * 0.05)).unwrap());
        }
        let edit = started.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        let edited = picture.with_overrides(&o(0.5)).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(edited.render(w, h, None).unwrap());
        }
        let render = started.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        let started = std::time::Instant::now();
        for i in 0..1000 {
            std::hint::black_box(promo_svg::edited_stamp(
                picture.stamp(),
                &o(i as f64 / 1000.0),
            ));
        }
        let stamp = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{name:8} {w:>4}x{h:<4} first edit {first:7.2} ms  edit+parse {edit:7.2} ms  \
             render {render:7.2} ms  stamp {stamp:6.4} ms"
        );
    };
    measure("logo", "badge", 640, 213);
    measure("logo", "badge", 1280, 427);
    measure("logo", "badge", 1920, 640);
    measure("complex", "g0", 1920, 1080);
    measure("complex", "g0", 3840, 2160);
}

/// Animation inside a file is noticed (and never played): validate says
/// it draws as its still frame.
#[test]
fn animation_inside_a_file_is_named() {
    let project = std::env::temp_dir().join(format!("promo-svg-anim-{}", std::process::id()));
    let resources = project.join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(
        resources.join("spin.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='5' \
         height='5'><animateTransform attributeName='transform' type='rotate' from='0' \
         to='360' dur='1s'/></rect></svg>",
    )
    .unwrap();
    std::fs::write(
        resources.join("still.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='5' \
         height='5'/></svg>",
    )
    .unwrap();
    let meta = promo_model::ProjectMetadata::from_json(
        r#"{"id":"P","name":"P","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":1,
            "videoDuration":1,"subtitles":[],"compositionSettings":{},"resources":[
            {"id":"A","kind":"vector","filename":"spin.svg","displayName":"Spin","addedAt":0,
             "pixelWidth":10,"pixelHeight":10},
            {"id":"B","kind":"vector","filename":"still.svg","displayName":"Still","addedAt":0,
             "pixelWidth":10,"pixelHeight":10}]}"#,
    )
    .unwrap();
    let findings = promo_svg::findings(&meta, &resources);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].contains("\"Spin\" animates itself"),
        "{findings:?}"
    );
    let _ = std::fs::remove_dir_all(&project);
}
