//! The GPU path's gate (SVG-PLAN.md slice 4): every corpus file, and a
//! file per feature the GPU path claims, drawn through the GPU scene and by
//! resvg, compared with [`gate`]. `cargo test -p promo-svg --features gpu
//! --test gpu`.

use std::path::PathBuf;

use promo_svg::{Context, Picture};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn context() -> Context {
    Context {
        file_dir: corpus(),
        project_root: corpus(),
    }
}

/// How far a GPU picture is from resvg's, channel by channel on
/// premultiplied RGBA, 0–255.
#[derive(Debug, Clone, Copy, Default)]
pub struct Diff {
    /// The largest difference where resvg's picture is flat — the pixel
    /// and its eight neighbours within 2 levels of each other: inside a
    /// shape, a gradient, the background.
    pub interior: u8,
    /// The largest difference on the rest: the antialiased rims.
    pub edge: u8,
    /// The mean difference over every channel of every pixel.
    pub mean: f64,
    /// The largest difference between the two pictures' 4×4 block means:
    /// what is left of a rim's difference once the eye's blur averages
    /// the samples — a shifted, thickened or missing shape stays large.
    pub blocks: f64,
    /// Pixels differing by more than 2 levels, as a share of all.
    pub changed: f64,
}

pub fn diff(gpu: &[u8], cpu: &[u8], w: u32, h: u32) -> Diff {
    let (w, h) = (w as usize, h as usize);
    let at = |img: &[u8], x: usize, y: usize, c: usize| img[(y * w + x) * 4 + c] as i32;
    let mut d = Diff::default();
    let mut sum = 0u64;
    let mut changed = 0usize;
    for y in 0..h {
        for x in 0..w {
            let mut flat = true;
            for c in 0..4 {
                let v = at(cpu, x, y, c);
                for (dx, dy) in [
                    (-1i32, -1i32),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if (at(cpu, nx as usize, ny as usize, c) - v).abs() > 2 {
                        flat = false;
                    }
                }
            }
            let mut worst = 0;
            for c in 0..4 {
                let e = (at(gpu, x, y, c) - at(cpu, x, y, c)).unsigned_abs();
                sum += e as u64;
                worst = worst.max(e);
            }
            if worst > 2 {
                changed += 1;
            }
            let worst = worst.min(255) as u8;
            if flat {
                d.interior = d.interior.max(worst);
            } else {
                d.edge = d.edge.max(worst);
            }
        }
    }
    d.mean = sum as f64 / (w * h * 4) as f64;
    d.changed = changed as f64 / (w * h) as f64;
    for by in (0..h).step_by(4) {
        for bx in (0..w).step_by(4) {
            for c in 0..4 {
                let (mut a, mut b, mut n) = (0i64, 0i64, 0i64);
                for y in by..(by + 4).min(h) {
                    for x in bx..(bx + 4).min(w) {
                        a += at(gpu, x, y, c) as i64;
                        b += at(cpu, x, y, c) as i64;
                        n += 1;
                    }
                }
                let e = (a - b).abs() as f64 / n as f64;
                if e > d.blocks {
                    d.blocks = e;
                }
            }
        }
    }
    d
}

/// The tolerance the GPU path is held to: antialiasing, and nothing else.
///
/// Both pictures are premultiplied RGBA, compared channel by channel.
///
/// - **Interior ≤ 3 levels.** Where resvg's picture is flat (a pixel and
///   its eight neighbours within 2 levels: inside a shape, along a
///   gradient, the background), the GPU must give the same colour.
///   tiny-skia blends in 8 bits with its own rounding, the GPU in floats
///   rounded once per pass, and a layer adds a pass: a level or two, three
///   at most (lavapipe's rounding).
/// - **Rims ≤ 128 levels.** Elsewhere a pixel's value is its coverage.
///   tiny-skia measures coverage on a fine grid (4 rows, quarter-pixel
///   columns); the GPU counts 4 multisamples at the standard positions. An
///   edge passing between two samples flips both at once, so the two may
///   disagree by up to half a pixel's coverage — never more: a shape moved
///   by a pixel puts 255 on its rim.
/// - **4×4 blocks ≤ 24 levels.** Averaged over 4×4 pixels (what a viewer's
///   eye does) the rim's disagreements largely cancel along an edge — 18 at
///   most across the corpus. An edge moved by half a pixel along four rows
///   is 32, a shape recoloured or missing far more.
/// - **Mean ≤ 1 level.** Rims are a small share of any picture; the
///   densest file, 2,400 shapes at 1080p, measures 0.5.
pub fn gate(name: &str, d: Diff) {
    if std::env::var_os("PROMO_GATE_REPORT").is_some() {
        return;
    }
    assert!(
        d.interior <= 3,
        "{name}: interior differs by {} — {d:?}",
        d.interior
    );
    assert!(d.edge <= 128, "{name}: a rim differs by {} — {d:?}", d.edge);
    assert!(
        d.blocks <= 24.0,
        "{name}: a 4×4 block differs by {:.1} — {d:?}",
        d.blocks
    );
    assert!(
        d.mean <= 1.0,
        "{name}: mean difference {:.3} — {d:?}",
        d.mean
    );
}

/// Whether [`gate`] passes, without asserting.
fn passes(d: Diff) -> bool {
    d.interior <= 3 && d.edge <= 128 && d.blocks <= 24.0 && d.mean <= 1.0
}

fn render_both(
    picture: &Picture,
    w: u32,
    h: u32,
) -> Option<(Vec<u8>, Vec<u8>, promo_svg::SceneStats)> {
    let ctx = promo_gpu::gpu_for_test()?;
    let mut r = promo_gpu::vector::scene::SceneRenderer::new(ctx).unwrap();
    let build = picture.scene(w, h, None).expect("a scene");
    let gpu = r.render_pixels(ctx, &build.scene).unwrap();
    eprintln!("{} samples on {}", r.samples(), ctx.adapter_summary());
    let cpu = picture.render(w, h, None).unwrap();
    Some((gpu, cpu, build.stats))
}

fn check(name: &str, bytes: &[u8], scales: &[f64]) {
    let picture = Picture::parse(bytes, &context()).unwrap();
    let (pw, ph) = picture.size();
    for &s in scales {
        let (w, h) = ((pw * s).round() as u32, (ph * s).round() as u32);
        let Some((gpu, cpu, stats)) = render_both(&picture, w, h) else {
            return;
        };
        let d = diff(&gpu, &cpu, w, h);
        if let Some(dir) = std::env::var_os("PROMO_GATE_DUMP") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (tag, img) in [("gpu", &gpu), ("cpu", &cpu)] {
                let file =
                    std::fs::File::create(dir.join(format!("{name}-{s}-{tag}.png"))).unwrap();
                let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let pixmap = tiny_skia::Pixmap::from_vec(
                    img.to_vec(),
                    tiny_skia::IntSize::from_wh(w, h).unwrap(),
                )
                .unwrap();
                let straight: Vec<u8> = pixmap
                    .pixels()
                    .iter()
                    .flat_map(|p| {
                        let c = p.demultiply();
                        [c.red(), c.green(), c.blue(), c.alpha()]
                    })
                    .collect();
                enc.write_header()
                    .unwrap()
                    .write_image_data(&straight)
                    .unwrap();
            }
        }
        eprintln!("{name} ×{s} {w}x{h}: {d:?} {stats:?}");
        gate(&format!("{name} ×{s}"), d);
    }
}

const CORPUS: &[&str] = &[
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

#[test]
fn the_corpus_draws_on_the_gpu_as_resvg_draws_it() {
    for name in CORPUS {
        let bytes = std::fs::read(corpus().join(format!("{name}.svg"))).unwrap();
        check(name, &bytes, &[1.0, 2.0, 3.7]);
    }
}

#[test]
fn the_dense_file_draws_on_the_gpu_as_resvg_draws_it() {
    let bytes = std::fs::read(corpus().join("complex.svg")).unwrap();
    check("complex", &bytes, &[1.2, 2.4]);
}

/// Per-frame cost of a keyed element on each path (SVG-PLAN.md slice 4,
/// "As built"): every frame changes the keyed value, so each frame edits
/// the file, parses it and draws it — resvg into pixels uploaded as a
/// texture, or the scene built and drawn on the GPU (timed to the GPU's
/// completion). `cargo test --release -p promo-svg --features gpu --test
/// gpu -- --ignored --nocapture gpu_cost`.
#[test]
#[ignore]
fn gpu_cost() {
    let Some(ctx) = promo_gpu::gpu_for_test() else {
        return;
    };
    let mut r = promo_gpu::vector::scene::SceneRenderer::new(ctx).unwrap();
    println!("{} — {} samples", ctx.adapter_summary(), r.samples());
    let upload = |px: &[u8], w: u32, h: u32| {
        let t = ctx
            .device
            .create_texture(&promo_gpu::wgpu::TextureDescriptor {
                label: None,
                size: promo_gpu::wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: promo_gpu::wgpu::TextureDimension::D2,
                format: promo_gpu::wgpu::TextureFormat::Bgra8Unorm,
                usage: promo_gpu::wgpu::TextureUsages::TEXTURE_BINDING
                    | promo_gpu::wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
        ctx.queue.write_texture(
            promo_gpu::wgpu::ImageCopyTexture {
                texture: &t,
                mip_level: 0,
                origin: promo_gpu::wgpu::Origin3d::ZERO,
                aspect: promo_gpu::wgpu::TextureAspect::All,
            },
            px,
            promo_gpu::wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            promo_gpu::wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit([]);
        ctx.device.poll(promo_gpu::wgpu::Maintain::Wait);
    };
    let measure = |r: &mut promo_gpu::vector::scene::SceneRenderer,
                   name: &str,
                   node: &str,
                   w: u32,
                   h: u32| {
        let mut pieces = promo_svg::PieceCache::default();
        let bytes = std::fs::read(corpus().join(format!("{name}.svg"))).unwrap();
        let picture = Picture::parse(&bytes, &context()).unwrap();
        let o = |v: f64| {
            vec![promo_model::vector::VectorOverride {
                node: node.into(),
                opacity: Some(0.5 + v * 0.4),
                transform: Some(promo_model::vector::NodeTransform {
                    rotate: Some(v * 30.0),
                    ..Default::default()
                }),
                ..Default::default()
            }]
        };
        // Warm both paths.
        let p0 = picture.with_overrides(&o(0.0)).unwrap();
        let _ = p0.render(w, h, None);
        let _ = r.render_pixels(ctx, &p0.scene(w, h, None).unwrap().scene);
        let runs = if w * h > 4_000_000 { 6 } else { 12 };
        let (mut edit, mut cpu, mut up, mut build, mut gpu_cpu, mut gpu_all) =
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        for i in 0..runs {
            let v = (i + 1) as f64 / runs as f64;
            let t = std::time::Instant::now();
            let p = picture.with_overrides(&o(v)).unwrap();
            edit += ms(t);
            let t = std::time::Instant::now();
            let mut px = p.render(w, h, None).unwrap();
            for q in px.chunks_exact_mut(4) {
                q.swap(0, 2);
            }
            cpu += ms(t);
            let t = std::time::Instant::now();
            upload(&px, w, h);
            up += ms(t);
            let t = std::time::Instant::now();
            let s = p.scene_with(w, h, None, Some(&mut pieces)).unwrap();
            build += ms(t);
            let t = std::time::Instant::now();
            let tex = r.render_texture(ctx, &s.scene).unwrap();
            gpu_cpu += ms(t);
            ctx.device.poll(promo_gpu::wgpu::Maintain::Wait);
            gpu_all += ms(t);
            drop(tex);
        }
        let n = runs as f64;
        println!(
            "{name:8} {w:>4}x{h:<4} edit+parse {:6.2}  | resvg {:7.2} + upload {:5.2} = {:7.2} ms | scene {:6.2} + encode {:6.2} (+GPU {:6.2}) = {:7.2} ms  [{} shapes, {} triangles, {} passes, {} calls, tessellate {:.2} encode {:.2}]",
            edit / n,
            cpu / n,
            up / n,
            (cpu + up) / n,
            build / n,
            gpu_cpu / n,
            (gpu_all - gpu_cpu) / n,
            (build + gpu_all) / n,
            p0.scene(w, h, None).unwrap().stats.shapes,
            r.stats.triangles,
            r.stats.passes,
            r.stats.draw_calls,
            r.stats.tessellate_ms,
            r.stats.encode_ms,
        );
    };
    for (w, h) in [(640, 213), (1280, 427), (1920, 640), (3840, 1280)] {
        measure(&mut r, "logo", "badge", w, h);
    }
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        measure(&mut r, "complex", "g[0]", w, h);
    }
}

fn feature(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/gpu/{name}.svg")))
        .unwrap()
}

/// Every stroke the GPU claims: caps, joins, miters, dashes, gradient
/// strokes, paint-order, the pair under an opacity — all on the GPU, none
/// left to resvg.
#[test]
fn strokes_draw_on_the_gpu_as_resvg_draws_them() {
    let picture = Picture::parse(&feature("strokes"), &context()).unwrap();
    let build = picture.scene(640, 400, None).unwrap();
    assert_eq!(build.stats.rasters, 0, "{:?}", build.stats);
    assert!(build.stats.shapes >= 15, "{:?}", build.stats);
    check("strokes", &feature("strokes"), &[1.0, 2.0, 3.3]);
}

/// Every paint the GPU claims, each spread, the well-behaved focus and
/// concentric circles — on the GPU.
#[test]
fn paints_draw_on_the_gpu_as_resvg_draws_them() {
    let picture = Picture::parse(&feature("paints"), &context()).unwrap();
    let build = picture.scene(720, 480, None).unwrap();
    assert_eq!(build.stats.rasters, 0, "{:?}", build.stats);
    check("paints", &feature("paints"), &[1.0, 2.0, 3.3]);
}

/// Opacity over several shapes and clips become layers; nothing goes to
/// resvg.
#[test]
fn layers_draw_on_the_gpu_as_resvg_draws_them() {
    let picture = Picture::parse(&feature("groups"), &context()).unwrap();
    let build = picture.scene(640, 400, None).unwrap();
    assert_eq!(build.stats.rasters, 0, "{:?}", build.stats);
    assert!(build.stats.layers >= 3, "{:?}", build.stats);
    check("groups", &feature("groups"), &[1.0, 2.0, 3.3]);
}

/// What the GPU leaves to resvg is drawn by resvg and composited in paint
/// order between the GPU's shapes — a blur between two shapes it overlaps.
#[test]
fn resvgs_pieces_fall_in_paint_order_between_the_gpus_shapes() {
    let picture = Picture::parse(&feature("mixed"), &context()).unwrap();
    let build = picture.scene(600, 360, None).unwrap();
    let why = &build.stats.fallbacks;
    for reason in [
        "a filter",
        "a pattern paint",
        "crisp edges (no antialiasing)",
        "a radial gradient whose focus is on or outside its circle",
        "a mask",
    ] {
        assert!(why.contains_key(reason), "{reason}: {why:?}");
    }
    // The blur sits between two shapes: GPU, resvg, GPU.
    let kinds: Vec<&str> = build
        .scene
        .items
        .iter()
        .map(|i| match i {
            promo_gpu::vector::scene::Item::Draw { .. } => "gpu",
            promo_gpu::vector::scene::Item::Raster(_) => "resvg",
            promo_gpu::vector::scene::Item::Layer(_) => "layer",
        })
        .collect();
    assert_eq!(&kinds[..4], &["gpu", "gpu", "resvg", "gpu"], "{kinds:?}");
    check("mixed", &feature("mixed"), &[1.0, 2.0, 3.3]);
}

/// The gate sees what antialiasing cannot explain: the same scene moved
/// by half a pixel, a paint a few levels off, a shape left out — each
/// fails it, while the scene as built passes.
#[test]
fn the_gate_refuses_a_moved_recoloured_or_missing_shape() {
    use promo_gpu::vector::scene::{Item, Paint, Seg};
    let Some(ctx) = promo_gpu::gpu_for_test() else {
        return;
    };
    let mut r = promo_gpu::vector::scene::SceneRenderer::new(ctx).unwrap();
    let bytes = std::fs::read(corpus().join("logo.svg")).unwrap();
    let picture = Picture::parse(&bytes, &context()).unwrap();
    let (w, h) = (480, 160);
    let cpu = picture.render(w, h, None).unwrap();
    let build = picture.scene(w, h, None).unwrap();
    let ok = r.render_pixels(ctx, &build.scene).unwrap();
    assert!(passes(diff(&ok, &cpu, w, h)));
    let mut moved = build.scene.clone();
    for item in &mut moved.items {
        if let Item::Draw { shapes, .. } = item {
            for s in shapes {
                for seg in &mut s.segs {
                    let shift = |p: &mut [f32; 2]| p[0] += 0.5;
                    match seg {
                        Seg::Move(a) | Seg::Line(a) => shift(a),
                        Seg::Quad(a, b) => {
                            shift(a);
                            shift(b)
                        }
                        Seg::Cubic(a, b, c) => {
                            shift(a);
                            shift(b);
                            shift(c)
                        }
                        Seg::Close => {}
                    }
                }
            }
        }
    }
    let d = diff(&r.render_pixels(ctx, &moved).unwrap(), &cpu, w, h);
    assert!(!passes(d), "half a pixel to the right passed: {d:?}");
    let mut recoloured = build.scene.clone();
    if let Some(Item::Draw { shapes, .. }) = recoloured
        .items
        .iter_mut()
        .rev()
        .find(|i| matches!(i, Item::Draw { .. }))
    {
        for s in shapes {
            if let Paint::Solid(c) = &mut s.paint {
                c[0] = (c[0] + 6.0 / 255.0).min(c[3]);
            }
        }
    }
    let d = diff(&r.render_pixels(ctx, &recoloured).unwrap(), &cpu, w, h);
    assert!(!passes(d), "a shape 6 levels redder passed: {d:?}");
    let mut missing = build.scene.clone();
    let last = missing
        .items
        .iter()
        .rposition(|i| matches!(i, Item::Draw { .. }))
        .unwrap();
    missing.items.remove(last);
    let d = diff(&r.render_pixels(ctx, &missing).unwrap(), &cpu, w, h);
    assert!(!passes(d), "a missing shape passed: {d:?}");
}

/// A piece resvg drew (here the blur) is drawn once while keyed frames
/// edit other elements, and again when an edit reaches inside it — and a
/// scene served from the cache is the scene drawn afresh.
#[test]
fn a_piece_no_edit_reaches_is_drawn_once() {
    let picture = Picture::parse(&feature("mixed"), &context()).unwrap();
    let mut pieces = promo_svg::PieceCache::default();
    let edit = |node: &str, v: f64| {
        vec![promo_model::vector::VectorOverride {
            node: node.into(),
            opacity: Some(v),
            ..Default::default()
        }]
    };
    let (w, h) = (600, 360);
    let pixels = |b: &promo_svg::SceneBuild| -> Vec<Vec<u8>> {
        b.scene
            .items
            .iter()
            .filter_map(|i| match i {
                promo_gpu::vector::scene::Item::Raster(r) => Some(r.rgba.to_vec()),
                _ => None,
            })
            .collect()
    };
    // The red rect after the blur, keyed: the blur is drawn on the first
    // frame only.
    let first = picture.with_overrides(&edit("rect[2]", 0.3)).unwrap();
    let a = first.scene_with(w, h, None, Some(&mut pieces)).unwrap();
    let misses = pieces.misses;
    assert!(misses > 0);
    let second = picture.with_overrides(&edit("rect[2]", 0.6)).unwrap();
    let b = second.scene_with(w, h, None, Some(&mut pieces)).unwrap();
    assert_eq!(pieces.misses, misses, "nothing drawn again");
    assert!(pieces.hits >= misses);
    assert_eq!(pixels(&b), pixels(&second.scene(w, h, None).unwrap()));
    assert_eq!(pixels(&a), pixels(&b));
    // An edit to the blurred circle itself draws its piece again.
    let blurred = picture.with_overrides(&edit("circle[0]", 0.5)).unwrap();
    let c = blurred.scene_with(w, h, None, Some(&mut pieces)).unwrap();
    assert!(pieces.misses > misses, "the edited piece is drawn");
    assert_ne!(pixels(&c), pixels(&b));
    assert_eq!(pixels(&c), pixels(&blurred.scene(w, h, None).unwrap()));
}
