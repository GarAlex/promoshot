# promo-core bench baselines — MacBook (Apple Silicon), 2026-07-20

Recorded with `cargo bench` (criterion, release). Regression gate: >25%
slower than these numbers fails the push gate (bench-guard pattern).

| Bench | Baseline | P0 gate | Notes |
|---|---|---|---|
| ffi_noop_call | ~976 ps | < 1 µs | C-ABI call overhead — gate beaten ~1000× |
| ffi_version_call | ~394 ps | — | static CStr return |
| iosurface_spike/import_render_readback_4k | ~7.98 ms | runs green | 3840×2160: FULL round trip incl. fresh IOSurface create, wgpu texture adoption, clear render, and 32 MB CPU readback + full-frame pixel verification per iteration — production pools surfaces and never reads back on the hot path. Even so: ~125 fps at 4K, >4× the 30 fps realtime budget |
| iosurface_spike/import_render_readback_1080p | ~2.87 ms | runs green | same full round trip, 8 MB readback |
| iosurface_spike/import_render_readback_256 | ~1.70 ms | runs green | fixed-cost dominated (device sync + allocation) |

Spike verification: every pixel of the wgpu clear is observed byte-exact
through the IOSurface CPU mapping (BGRA), at 256², 1920×1080, and
3840×2160 — the zero-copy adoption path is real, not theoretical.

## P1 — timeline math (2026-08-11)

| Bench | Baseline | Notes |
|---|---|---|
| keyframe_interpolation/transform_10_keys | ~97 ns | |
| keyframe_interpolation/transform_100_keys | ~302 ns | |
| keyframe_interpolation/transform_1000_keys | ~1.59 µs | scales ~linearly (per-call sort of keyed frames, same as Swift) |
| keyframe_interpolation/gain_10_keys | ~121 ns | f32 path |
| keyframe_interpolation/gain_1000_keys | ~2.39 µs | |
| mapping_3h_200cuts/source_time | ~49 µs | 3 h looped resource, 200 trim cuts + 25 held-frame pauses; cost dominated by per-call range/pause rebuild (mirrors the Swift design 1:1 — a session-level cache is a later-phase optimization, not P1's parity mandate) |
| mapping_3h_200cuts/video_segment | ~74 µs | same workload |
| mapping_3h_200cuts/trim_ranges_rebuild | ~778 ns | the 200-cut range walk itself |
| layout/media_rect | ~1.23 ns | |
| layout/letterbox_transform | ~1.18 ns | |

## P2 — GPU compositor (2026-08-11)

| Bench | Baseline | Notes |
|---|---|---|
| compose_frame/1080p | ~1.49 ms | background + three 1080p IOSurface-adopted quads (rotation, radius, inside border) + solid overlay, rendered into an IOSurface — includes per-frame texture adoption and device sync |
| compose_frame/4k | ~3.12 ms | same scene at 3840×2160 → ~320 fps of full-frame 4K compositing |

Golden-frame gate (ReVoice `PromoCoreGoldenTests.testGoldenParity_CGvsGPU`):
CG vs GPU render of a frame with background keyframes, rotated/zoomed image
layer with border + corner radius, vector drawing, caption, and watermark —
mean channel diff **~0.16**, pixels differing >8 **~0.83%** (all on AA edges /
image interpolation), asserted < 2.0 mean and < 1.5% over-8, and asserted
non-identical (proves the GPU path engaged, not the CG fallback).

Swift↔Rust head-to-head (ReVoice `PromoCoreParityTests.testRustHotPathNotSlowerThanSwift`,
release-built core, debug-built Swift test host, 20 000 calls of
`layer.transform` on the synthetic fixture's 3-key video layer):
Swift ~1.17 µs/call vs Rust-through-FFI ~0.22 µs/call — **Rust 5.2× faster**;
the "Rust must not be slower" P1 gate holds with an asserted 1.5× ceiling.
