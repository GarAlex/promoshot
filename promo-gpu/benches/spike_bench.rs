//! IOSurface↔wgpu spike timings (P0 baseline: import + clear-render +
//! CPU readback at 1080p).

use criterion::{criterion_group, criterion_main, Criterion};

// Off macOS the body compiles away and `c` would be an unused-variable
// error under `-D warnings`, so the stub owns the unused name.
#[cfg(not(target_os = "macos"))]
fn spike(_c: &mut Criterion) {}

#[cfg(target_os = "macos")]
fn spike(c: &mut Criterion) {
    {
        let ctx = promo_gpu::GpuContext::new().expect("gpu");
        let mut group = c.benchmark_group("iosurface_spike");
        group.sample_size(20);
        group.bench_function("import_render_readback_4k", |b| {
            b.iter(|| promo_gpu::spike::run(&ctx, 3840, 2160).expect("spike"));
        });
        group.bench_function("import_render_readback_1080p", |b| {
            b.iter(|| promo_gpu::spike::run(&ctx, 1920, 1080).expect("spike"));
        });
        group.bench_function("import_render_readback_256", |b| {
            b.iter(|| promo_gpu::spike::run(&ctx, 256, 256).expect("spike"));
        });
        group.finish();
    }
}

criterion_group!(benches, spike);
criterion_main!(benches);
