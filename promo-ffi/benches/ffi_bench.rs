//! FFI overhead microbench (P0 gate: < 1 µs/call — expect ~1 ns).

use criterion::{criterion_group, criterion_main, Criterion};

fn ffi_overhead(c: &mut Criterion) {
    c.bench_function("ffi_noop_call", |b| {
        let mut x = 0u64;
        b.iter(|| {
            x = promo_ffi::promo_ffi_noop(std::hint::black_box(x));
        });
    });
    c.bench_function("ffi_version_call", |b| {
        b.iter(|| std::hint::black_box(promo_ffi::promo_core_version()));
    });
}

criterion_group!(benches, ffi_overhead);
criterion_main!(benches);
