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

## promo-cli

Render a project folder without an app:

```
cargo build -p promo-cli --release        # -> target/release/promo

promo inspect <project-dir>               # what is in it, and what will be skipped
promo still   <project-dir> --out f.png --time 2.5
promo frames  <project-dir> --out frames/ --fps 30
promo video   <project-dir> --out out.mp4 --fps 30 --size 1920x1080
```

A project is `metadata.json` plus `Resources/` (and `Images/`). The CLI acts as
a host for `promo-engine`: it decodes assets to BGRA and lets the same
compositor the apps use produce the frames, so output matches the app for the
same metadata.

`video` needs `ffmpeg` on PATH; frames are rendered on the GPU and piped to it
raw. Video layers and captions do not render yet — see LINUX-READY-PLAN R2 and
the egui plan's E5.
