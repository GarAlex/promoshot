//! Vector resources through the built binary (SVG-PLAN.md slice 1): an SVG
//! added with `promo resource add`, shown by image layers, and rendered by
//! the engine at the size it shows — read back as pixels. The engine is
//! the renderer under test; promo-svg's own raster is the oracle for what
//! the picture looks like.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../promo-svg/tests/corpus/{name}.svg"))
}

/// One `promo` at a time: each render starts a GPU device (lavapipe on
/// CI), and eight at once beside the rest of the suite is load a shared
/// runner need not carry.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn promo(args: &[&str]) -> (bool, String) {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(args)
        .output()
        .expect("run promo");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A fresh project with a dark ground, 640x360, holding `files` as vector
/// resources (id = file stem).
fn project(tag: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("promo-vector-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (ok, text) = promo(&[
        "init",
        dir.to_str().unwrap(),
        "--args",
        r#"{"canvas":"640x360"}"#,
    ]);
    assert!(ok, "{text}");
    for file in files {
        let (ok, text) = promo(&[
            "resource",
            "add",
            dir.to_str().unwrap(),
            corpus(file).to_str().unwrap(),
            "--id",
            file,
        ]);
        assert!(ok, "{text}");
    }
    dir
}

fn edit(dir: &Path, f: impl FnOnce(&mut Value)) {
    let path = dir.join("metadata.json");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    f(&mut doc);
    doc["compositionSettings"]["backgroundColorHex"] = json!("000000");
    std::fs::write(&path, doc.to_string()).unwrap();
}

fn image_layer(id: &str, resource: &str, sort: i64, placement: Value) -> Value {
    json!({"id": id, "name": id, "sortIndex": sort, "kind": "image", "isEnabled": true,
           "startTime": 0, "duration": 2, "resourceID": resource,
           "keyframes": [{"id": format!("{id}k"), "time": 0, "transitionDuration": 0,
                          "placement": placement}]})
}

/// An image layer at a plain zoom (a share of the canvas height), its
/// top-left at `origin`.
fn zoomed_layer(id: &str, resource: &str, sort: i64, zoom: f64, origin: [f64; 2]) -> Value {
    json!({"id": id, "name": id, "sortIndex": sort, "kind": "image", "isEnabled": true,
           "startTime": 0, "duration": 2, "resourceID": resource,
           "keyframes": [{"id": format!("{id}k"), "time": 0, "transitionDuration": 0,
                          "zoom": zoom, "horizontalShift": origin[0],
                          "verticalShift": origin[1]}]})
}

/// Renders a still, or `None` when this machine has no GPU.
fn still(dir: &Path, time: f64) -> Option<image::RgbaImage> {
    let out = dir.join("still.png");
    let (ok, text) = promo(&[
        "still",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--time",
        &time.to_string(),
    ]);
    if !ok && text.contains("no GPU") {
        eprintln!("skipped: no GPU adapter");
        return None;
    }
    assert!(ok, "{text}");
    Some(image::open(&out).unwrap().to_rgba8())
}

/// The box of everything not the black ground.
fn ink_box(img: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for (x, y, p) in img.enumerate_pixels() {
        if p[0] as u32 + p[1] as u32 + p[2] as u32 > 24 {
            b = Some(match b {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    b
}

/// The logo laid out by a placement rule at its natural aspect (240x80):
/// 120 tall is 360 wide, centred — and its pixels are the picture's own,
/// drawn at that size (promo-svg's raster, the oracle).
#[test]
fn a_vector_layer_draws_its_picture_at_the_placed_size() {
    let dir = project("placed", &["logo"]);
    edit(&dir, |doc| {
        doc["layers"].as_array_mut().unwrap().push(image_layer(
            "L",
            "logo",
            1,
            json!({"height": 120, "anchor": "center"}),
        ));
    });
    let (ok, text) = promo(&["validate", dir.to_str().unwrap()]);
    assert!(ok && text.starts_with("ok"), "{text}");
    let Some(img) = still(&dir, 1.0) else { return };
    let (x0, y0, x1, y1) = ink_box(&img).expect("the logo draws");
    // 360x120 centred on 640x360: x 140..500, y 120..240 (the badge's
    // rounded corners and the wordmark's right edge sit a little inside).
    assert!(
        (139..=142).contains(&x0) && (119..=122).contains(&y0),
        "{x0},{y0}"
    );
    assert!(
        (460..=500).contains(&x1) && (232..=240).contains(&y1),
        "{x1},{y1}"
    );
    // The oracle: the picture drawn by promo-svg at exactly 360x120.
    let bytes = std::fs::read(corpus("logo")).unwrap();
    let picture = promo_svg::Picture::parse(
        &bytes,
        &promo_svg::Context::for_resources(&dir.join("Resources")),
    )
    .unwrap();
    let oracle = picture.render(360, 120, None).unwrap();
    let mut worst = 0i32;
    let mut sum = 0i64;
    for y in 0..120u32 {
        for x in 0..360u32 {
            let o = &oracle[((y * 360 + x) * 4) as usize..][..4];
            let p = img.get_pixel(140 + x, 120 + y);
            for c in 0..3 {
                // Over black, premultiplied colour is what shows.
                let d = (p[c] as i32 - o[c] as i32).abs();
                worst = worst.max(d);
                sum += d as i64;
            }
        }
    }
    let mean = sum as f64 / (360.0 * 120.0 * 3.0);
    // A bucketed raster resampled by the compositor: edges move by a
    // fraction of a pixel, flat colour does not.
    assert!(
        mean < 2.0,
        "mean channel difference {mean:.2} (worst {worst})"
    );
}

/// Zoomed up a long way the picture stays sharp: it is drawn again at the
/// size it shows, never magnified. An edge of the glyph crosses from
/// white to the badge's violet within two pixels at 6x.
#[test]
fn a_zoomed_vector_stays_sharp() {
    let dir = project("sharp", &["logo"]);
    edit(&dir, |doc| {
        doc["layers"].as_array_mut().unwrap().push(image_layer(
            "L",
            "logo",
            1,
            json!({"height": 480, "anchor": "topLeft", "offset": [0, -60]}),
        ));
    });
    let Some(img) = still(&dir, 1.0) else { return };
    // 6 px per unit: the glyph's stem starts at unit x 26 = 156 px. Along
    // y = 240 the badge's green channel (92) climbs to the glyph's white
    // (255) across the edge: drawn at size it takes two pixels; a raster
    // at natural size magnified 6x takes six (break-tested).
    let y = 240;
    let ramp: Vec<u8> = (140..170)
        .map(|x| img.get_pixel(x, y)[1])
        .filter(|g| (100..250).contains(g))
        .collect();
    assert!(ramp.len() <= 3, "a soft edge: {ramp:?}");
}

/// A vector masks with its ink: a gradient picture windowed by the logo
/// shows only where the logo draws.
#[test]
fn a_vector_is_a_mask() {
    let dir = project("mask", &["logo", "gradients"]);
    edit(&dir, |doc| {
        let mut layer = image_layer(
            "G",
            "gradients",
            1,
            json!({"height": 240, "anchor": "center"}),
        );
        layer["maskResourceID"] = json!("logo");
        doc["layers"].as_array_mut().unwrap().push(layer);
    });
    let (ok, text) = promo(&["validate", dir.to_str().unwrap()]);
    assert!(ok && text.starts_with("ok"), "{text}");
    let Some(img) = still(&dir, 1.0) else { return };
    // The gradients picture is 480x240 centred (x 80..560); the logo mask
    // fits its 3:1 into it, 480x160, centred: y 100..260. Above the mask's
    // box the picture is cut away; inside the badge it shows.
    let above = img.get_pixel(320, 90);
    assert!(above[0] < 10 && above[1] < 10 && above[2] < 10, "{above:?}");
    let inside = img.get_pixel(120, 180);
    assert!(
        inside[2] > 60,
        "the picture shows through the badge: {inside:?}"
    );
    // Between the badge and the wordmark the mask has no ink.
    let gap = img.get_pixel(250, 120);
    assert!(gap[0] < 10 && gap[1] < 10 && gap[2] < 10, "{gap:?}");
}

/// An image cut of a vector is a crop of the picture itself: the layer
/// lays out at the cut's aspect and shows only that region.
#[test]
fn an_image_cut_crops_the_picture() {
    let dir = project("cut", &["gradients"]);
    edit(&dir, |doc| {
        doc["resources"][0]["imageCuts"] = json!([{"id": "right", "rect": [[0.5, 0.0], [0.5, 1.0]],
                                                   "filename": "", "createdAt": 0}]);
        // A plain zoom: a placement rule resolves its anchor against the
        // whole resource's aspect, a cut or not — the same for pictures.
        let mut layer = zoomed_layer("C", "gradients", 1, 200.0 / 360.0, [220.0, 80.0]);
        layer["imageCutID"] = json!("right");
        doc["layers"].as_array_mut().unwrap().push(layer);
    });
    let Some(img) = still(&dir, 1.0) else { return };
    // The right half is 200x200 units: square, 200 px, x 220..420.
    let (x0, _, x1, _) = ink_box(&img).unwrap();
    assert!(
        (219..=221).contains(&x0) && (418..=420).contains(&x1),
        "{x0}..{x1}"
    );
    // It holds the sun (orange-yellow), not the pink band of the left half.
    let sun = img.get_pixel(310, 180);
    assert!(sun[0] > 200 && sun[1] > 150 && sun[2] < 120, "{sun:?}");
}

/// Inside a nested composition, and through a fade: the picture plays
/// like any other.
#[test]
fn a_vector_plays_in_a_composition_and_fades() {
    let dir = project("nested", &["logo"]);
    edit(&dir, |doc| {
        let inner = image_layer(
            "inner",
            "logo",
            0,
            json!({"height": 100, "anchor": "center"}),
        );
        doc["resources"].as_array_mut().unwrap().push(json!({
            "id": "C", "kind": "composition", "filename": "", "displayName": "Comp",
            "addedAt": 0, "duration": 2, "pixelWidth": 640, "pixelHeight": 360,
            "imageCuts": [], "disabledAudioTrackIndices": [],
            "composition": {"canvasWidth": 640, "canvasHeight": 360, "layers": [inner]}
        }));
        let mut placed = json!({"id": "V", "name": "V", "sortIndex": 1, "kind": "video",
            "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "C",
            "fadeIn": 1.0, "keyframes": []});
        placed["keyframes"] = json!([]);
        doc["layers"].as_array_mut().unwrap().push(placed);
    });
    let (ok, text) = promo(&["validate", dir.to_str().unwrap()]);
    assert!(ok, "{text}");
    let Some(full) = still(&dir, 1.5) else { return };
    let (x0, y0, x1, y1) = ink_box(&full).expect("the nested picture draws");
    // 300x100 centred in the composition, which fills the canvas.
    assert!(
        (169..=172).contains(&x0) && (129..=132).contains(&y0),
        "{x0},{y0}"
    );
    assert!(x1 >= 440 && y1 >= 222, "{x1},{y1}");
    // Half way through the fade the glyph's white is about half as bright.
    // The glyph's stem: 1.25 px a unit from (170, 130), units (30, 40).
    let bright = full.get_pixel(207, 180)[0] as f64;
    let half = still(&dir, 0.5).unwrap().get_pixel(207, 180)[0] as f64;
    assert!(bright > 240.0, "{bright}");
    assert!((half / bright - 0.5).abs() < 0.12, "{half} of {bright}");
}

/// An `.svg` dropped into Resources/ with no entry plays by its derived id
/// (the inventory adopts it as a vector), laid out at its own aspect.
#[test]
fn an_undeclared_svg_plays_by_its_derived_id() {
    let dir = project("derived", &[]);
    std::fs::copy(corpus("logo"), dir.join("Resources/logo.svg")).unwrap();
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    let pid = doc["id"].as_str().unwrap().to_string();
    let id = promo_model::derived_id(&pid, "logo.svg");
    edit(&dir, |doc| {
        doc["layers"].as_array_mut().unwrap().push(image_layer(
            "L",
            &id,
            1,
            json!({"height": 120, "anchor": "center"}),
        ));
    });
    let Some(img) = still(&dir, 1.0) else { return };
    let (x0, y0, x1, _) = ink_box(&img).expect("the adopted picture draws");
    assert!(
        (139..=142).contains(&x0) && (119..=122).contains(&y0),
        "{x0},{y0}"
    );
    assert!(x1 >= 460, "laid out 3:1, not square: {x1}");
}

/// `promo inspect` names the vector and who shows it.
#[test]
fn inspect_lists_vectors_and_their_use() {
    let dir = project("inspect", &["logo", "gradients"]);
    edit(&dir, |doc| {
        let mut layer = image_layer("G", "gradients", 1, json!({"height": 200}));
        layer["maskResourceID"] = json!("logo");
        doc["layers"].as_array_mut().unwrap().push(layer);
    });
    let (ok, text) = promo(&["inspect", dir.to_str().unwrap()]);
    assert!(ok, "{text}");
    assert!(text.contains("vectors:   2"), "{text}");
    assert!(
        text.contains("logo  \"logo\"  logo.svg  240x80  placed by 0 layers, masks 1 layer"),
        "{text}"
    );
    assert!(
        text.contains("gradients.svg  400x200  placed by 1 layer"),
        "{text}"
    );
    assert!(
        !text.contains("unused resources"),
        "a mask is a use: {text}"
    );
    let (ok, json) = promo(&["inspect", dir.to_str().unwrap(), "--json"]);
    assert!(ok);
    let doc: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(doc["vectors"][0]["masks"], json!(["G"]));
}

/// `promo_upsert_layer` with an `.svg` as its file makes the same vector
/// resource `resource add` does, stamped 51; a refused file lands nothing.
#[test]
fn upsert_layer_imports_an_svg_as_a_vector() {
    let dir = project("upsert", &[]);
    let args = json!({"kind": "image", "file": corpus("text"), "resourceId": "t",
                      "placement": {"height": 100}})
    .to_string();
    let (ok, text) = promo(&["upsert-layer", dir.to_str().unwrap(), "--args", &args]);
    assert!(ok, "{text}");
    assert!(text.contains("vector resource t (text.svg"), "{text}");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(doc["minReaderVersion"], 51);
    let resource = &doc["resources"][0];
    assert_eq!(resource["kind"], "vector");
    assert_eq!(resource["svg"]["size"], json!([360.0, 140.0]));
    assert_eq!(resource["pixelHeight"], 140.0);
    assert!(dir.join("Resources/.svg/text.svg").is_file());

    let bomb = dir.join("bomb.svg");
    let mut svg = String::from(
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' \
         width='10' height='10'><defs><rect id='l0' width='1' height='1'/>",
    );
    for level in 1..=8 {
        svg.push_str(&format!("<g id='l{level}'>"));
        for _ in 0..10 {
            svg.push_str(&format!("<use xlink:href='#l{}'/>", level - 1));
        }
        svg.push_str("</g>");
    }
    svg.push_str("</defs><use xlink:href='#l8'/></svg>");
    std::fs::write(&bomb, svg).unwrap();
    let (ok, text) = promo(&[
        "resource",
        "add",
        dir.to_str().unwrap(),
        bomb.to_str().unwrap(),
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("too large"), "{text}");
    assert!(
        !dir.join("Resources/bomb.svg").exists(),
        "a refused file lands nothing"
    );
}

/// Slice 2 through the binary: `promo apply` edits an SVG's elements, the
/// still draws the edit (the engine reads the block's overrides), inspect
/// lists the element tree with ● on the edited row, validate names an
/// override that points nowhere, and "reset all" draws the file again.
#[test]
fn svg_element_overrides_render_inspect_and_validate() {
    let dir = project("overrides", &["logo"]);
    edit(&dir, |doc| {
        doc["layers"].as_array_mut().unwrap().push(image_layer(
            "L",
            "logo",
            1,
            json!({"height": 240}),
        ));
    });
    let project_arg = dir.to_str().unwrap().to_string();
    let apply = |commands: Value| {
        promo(&[
            "apply",
            &project_arg,
            "--args",
            &json!({ "commands": commands }).to_string(),
        ])
    };
    let Some(plain) = still(&dir, 1.0) else {
        return;
    };
    let (ok, text) = apply(json!([{"kind": "setVectorOverride", "resourceID": "logo",
        "node": "glyph", "patch": {"fill": "FF0000"}}]));
    assert!(ok, "{text}");
    let red = still(&dir, 1.0).unwrap();
    let reds = |img: &image::RgbaImage| {
        img.pixels()
            .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
            .count()
    };
    assert_eq!(reds(&plain), 0);
    assert!(reds(&red) > 500, "the glyph draws red: {}", reds(&red));
    // Out of tier: the element's reason, nothing applied.
    let (ok, text) = apply(json!([{"kind": "setVectorOverride", "resourceID": "logo",
        "node": "wordmark", "patch": {"stroke": "FFFFFF"}}]));
    assert!(!ok && text.contains("takes no paint"), "{text}");
    let (ok, text) = promo(&["inspect", &project_arg]);
    assert!(ok, "{text}");
    assert!(
        text.contains("elements: 11 (A 2, B 0, C 9, locked 0) · 1 overridden"),
        "{text}"
    );
    assert!(
        text.contains("glyph  path  C — a plain shape ● fill"),
        "{text}"
    );
    // An override for an element the file lacks: a warning, drawn as is.
    edit(&dir, |doc| {
        doc["resources"][0]["svg"]["overrides"]
            .as_array_mut()
            .unwrap()
            .push(json!({"node": "gone", "opacity": 0.2}));
    });
    let (_, text) = promo(&["validate", &project_arg]);
    assert!(text.contains("names no element of the file"), "{text}");
    let (ok, text) = apply(json!([{"kind": "resetVectorOverride", "resourceID": "logo"}]));
    assert!(ok, "{text}");
    let back = still(&dir, 1.0).unwrap();
    assert_eq!(back.as_raw(), plain.as_raw(), "reset all draws the file");
}
