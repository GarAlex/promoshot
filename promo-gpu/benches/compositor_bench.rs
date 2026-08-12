//! P2 perf baseline: full-frame composition (background + 3 textured quads
//! with rotation/radius/border + overlay quad) at 1080p and 4K.

use criterion::{criterion_group, criterion_main, Criterion};

fn compositor(c: &mut Criterion) {
    #[cfg(target_os = "macos")]
    {
        use promo_gpu::compositor::{Compositor, Scene, SceneQuad};
        use promo_gpu::iosurface::OwnedIoSurface;

        let ctx = promo_gpu::GpuContext::new().expect("gpu");
        let comp = Compositor::new(&ctx).expect("compositor");

        // Three 1080p-ish inputs adopted from IOSurfaces, like production.
        let inputs: Vec<OwnedIoSurface> = (0..3)
            .map(|_| {
                let s = OwnedIoSurface::new_bgra(1920, 1080).expect("input");
                s.write_pixels(&[128u8, 64, 200, 255].repeat(1920 * 1080))
                    .expect("fill");
                s
            })
            .collect();
        let textures: Vec<_> = inputs
            .iter()
            .map(|s| Compositor::import_iosurface(&ctx, s.raw(), 1920, 1080).expect("import"))
            .collect();

        let scene_for = |w: u32, h: u32| Scene {
            canvas_width: w as f64,
            canvas_height: h as f64,
            background_rgba: [0.1, 0.2, 0.3, 1.0],
            output_width: w,
            output_height: h,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![
                SceneQuad {
                    texture: Some(0),
                    rect: [0.0, 0.0, w as f64, h as f64],
                    corner_radius: 24.0,
                    border_width: 6.0,
                    border_rgba: [1.0, 0.5, 0.0, 1.0],
                    ..Default::default()
                },
                SceneQuad {
                    texture: Some(1),
                    rect: [w as f64 * 0.55, h as f64 * 0.1, w as f64 * 0.35, h as f64 * 0.35],
                    rotation_deg: 15.0,
                    corner_radius: 12.0,
                    ..Default::default()
                },
                SceneQuad {
                    texture: Some(2),
                    rect: [w as f64 * 0.1, h as f64 * 0.55, w as f64 * 0.3, h as f64 * 0.3],
                    rotation_deg: -20.0,
                    border_width: 4.0,
                    border_rgba: [0.0, 0.8, 1.0, 1.0],
                    ..Default::default()
                },
            ],
        };

        let mut group = c.benchmark_group("compose_frame");
        group.sample_size(30);
        for (label, w, h) in [("1080p", 1920u32, 1080u32), ("4k", 3840, 2160)] {
            let out = OwnedIoSurface::new_bgra(w as usize, h as usize).expect("out");
            let scene = scene_for(w, h);
            group.bench_function(label, |b| {
                b.iter(|| {
                    comp.compose_to_iosurface(&ctx, &scene, &textures, out.raw())
                        .expect("compose")
                });
            });
        }
        group.finish();
    }
}

criterion_group!(benches, compositor);
criterion_main!(benches);
