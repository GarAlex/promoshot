# promo-core

Rust engine for PromoShot: model, timeline math, GPU compositing (wgpu →
Metal on Apple), media scheduling/caching, and export orchestration. The
Swift app keeps UI, capture, StoreKit, and platform codec I/O
(VideoToolbox), handed across a C ABI as zero-copy GPU surfaces.

Plan and phase gates: `RUST-CORE-PLAN.md` in the app repo
(ssh://nas10/volume1/public/git/promoshot.git).

- Build/verify: `./check-all.sh`
- Benches: `cargo bench` (baselines in `benches-baseline/`)
- Invariants: `SPECS.md`

P0 status: workspace + FFI + `GpuSurface` + IOSurface↔wgpu interop spike
(green: wgpu renders into an adopted IOSurface texture; bytes verified
through the CPU mapping — the zero-copy path VideoToolbox frames will use).
