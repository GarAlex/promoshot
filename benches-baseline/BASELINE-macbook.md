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

## P3 slice 1 — preview engine (2026-08-11)

| Bench | Baseline | Notes |
|---|---|---|
| preview/render_warm_1080p_2layers | ~1.53 ms | render-at-time with all layer frames cached — the scrub-latency proxy (P3 target < 50 ms: 30× headroom before real decode) |
| preview/render_cold_1080p_2layers | ~2.07 ms | cache miss: provider call (1080p surface alloc) + CFRetain + texture adoption + compose; real video decode is host-side and additive |

Governor/caching invariants are unit-tested (LRU eviction to budget,
oversized-entry admission, hit/miss/eviction stats) — see promo-engine tests.

Soak / seek-latency suite (ReVoice `PromoCoreSoakTests.testScrubSoak4K`,
CI variant: 60 s synthesized 4K fixture, 15 s random scrub; full plan gate =
`PROMO_SOAK_FIXTURE_SECONDS=10800 PROMO_SOAK_SECONDS=1800`):
- tier-1 (proxy) seek: p50 ~6.9 ms, **p95 ~12.5 ms** (gate < 50 ms), max ~132 ms
  (first-touch decode)
- tier-0 full-res 4K refine: p50 ~77 ms (once per pause, async in the UI)
- memory: high-water growth **~638 MB** (gate < 2.5 GB), cache bytes held at
  the 512 MB budget with ~1 200 evictions over 660 seeks
- proxy generation on the 4K fixture: ~6.5× realtime
The suite's first run caught a real leak (autoreleased 4K CGImages piling up
in the render loop, +5.5 GB); the provider now drains a pool per decode.

Intermediate soak (2026-08-12, 10 min 4K fixture / 5 min scrub — the first
real scale-up):
- **Caught a second real bug** (diagnosed correctly only after the full 3 h
  run): proxy generation silently failed on long sources with VideoToolbox
  -11821/-12137 "Cannot Decode". 60 s segmentation made the 10-minute case
  pass and looked like a fix; the 3 h run showed the failure sits at
  ~780–840 s of content regardless, and the truncated proxy still reported
  success. **Real cause**: the writer-feed block runs for minutes and every
  CMSampleBuffer/CVPixelBuffer in it was autoreleased with nothing draining
  the pool — the decoder's buffer pool never came back. **Fix**: drain per
  frame (same class as the 4K CGImage pile-up above). Verified on the 3 h
  fixture: 1162 s (9.3× realtime), proxy duration 10800.0 s of 10800.0 s.
  The regression test now targets the 3 h fixture and asserts proxy
  duration == source duration.
- Results after the fix: 14 310 seeks / 5 min — tier-1 seek p50 6.7 ms /
  **p95 12.0 ms / max 17.2 ms** (gate < 50 ms); memory growth 575 MB
  (< 2.5 GB); cache pinned at its 512 MB budget through ~30 k evictions;
  segmented proxy generation ~6.9× realtime on 4K.
**FULL 3 h / 30 min soak — PASSED (2026-08-12)** (the plan's P3 gate):
3 h 4K fixture (1.6 GB, built at 2.1× realtime), proxy 10800 s → 1152 s
(**9.37× realtime**, full duration verified), then 30 minutes of random
scrubbing:

| Metric | Result | Gate |
|---|---|---|
| tier-1 (proxy) seek | p50 **6.6 ms**, p95 **12.2 ms**, max 19.1 ms | < 50 ms ✓ |
| tier-0 full-res 4K refine | p50 76.6 ms, p95 108.3 ms | (async, once per pause) |
| memory high-water growth | **742 MB** (base 50 → 792 MB) | < 2.5 GB ✓ |
| frame cache | 522 MB held, 169 156 evictions over 169 303 fetches | at 512 MB budget ✓ |

80 620 random seeks. Cache hits = 0 by construction: random seeks across a
3 h timeline never repeat, and a 512 MB budget holds ~4 s of 4K frames —
the cache and the next-tick prefetch pay off for playback/scrub locality,
not for uniform random access. The latency numbers are therefore
worst-case (every seek a cold decode).

CG-vs-GPU throughput head-to-head (ReVoice
`PromoCoreGoldenTests.testStillsBatchThroughput_GPUvsCG`, permanent gate):
30-frame stills batch @1080p canvas, rotated/bordered/rounded 2560×1440
image layer + watermark — CG ~12.7 ms/frame vs GPU-through-core
~4.5 ms/frame → **2.8× faster**; the test asserts the GPU path is never
slower than CG.

## P4 — export engine (2026-08-12)

Video export CG-vs-GPU (ReVoice `PromoCoreExportTests`, permanent gates):
- **4K→1080p, 10 s project: CG 1.27× realtime vs GPU 7.17× realtime —
  5.6× faster** (the pure-Swift pipeline fails the plan's ≥ 2× gate on 4K
  source; the core path clears it 3.5× over). Memory growth ~15 MB.
- Decoded-frame parity on a letterboxed multi-layer export: mean diff ~1.2,
  over-12 outliers ~2.5% (both sides H.264-lossy).
- Color-space regression caught by the gate: raw decoded BGRA (BT.709
  transfer) fed to the GPU shifted saturated colors; fixed with an on-GPU
  CI conversion to sRGB matching the CG path.

## P5 slice 1 — audio mix graph (2026-08-12)

| Bench | Baseline | Notes |
|---|---|---|
| pcm_mix/mix_60s_4in_stereo_48k_chunked | ~57 ms | 4 stereo 48 kHz inputs, 24-point automation each, 4096-frame chunks → **~1050× realtime** (3 h mix graph ≈ 10 s CPU) |

Level-point parity (ReVoice `PromoCoreParityTests.testAudioLevelPointsParity`):
7-case matrix (multi-point automation, overlapping/clamped focus intervals,
focused/unfocused) — exact f32 equality with `AudioTimelineBuilder.levelPoints`.

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

## Realistic composition soak — 3 h timeline, 30 min scrub (2026-08-12)

The app's actual workload, not a single giant clip: a 4K canvas with 280
layers — 75 instances of a 400×400 screen-recording clip (looped resource),
112 static-image instances, 56 SVG-annotation instances (imported through
the app's `SVGImporter`), and a soundtrack every 5 minutes — each visual
instance living 1–5 minutes and keyframe-animated between random canvas
positions with zoom and rotation. **~5.2 visual layers composited per
frame.** Deterministic (seeded), assets cached.
(ReVoice `PromoCoreCompositionSoakTests`, gate run:
`TEST_RUNNER_PROMO_COMP_SOAK_SECONDS=1800`.)

| Metric | Result | Gate |
|---|---|---|
| tier-1 seek (125 720 seeks) | p50 **6.1 ms**, p95 **18.6 ms**, max 77.6 ms | < 50 ms p95 ✓ |
| tier-0 full-res refine | p50 7.9 ms, p95 22.8 ms | — |
| memory high-water growth | **263 MB** | < 2.5 GB ✓ |
| frame cache | 536 MB held, 341 677 evictions, **230 564 hits / 342 039 misses (40% hit rate)** | at budget ✓ |
| 4K still export | 142 ms/frame | — |
| GIF export | 40 frames / 359 KB | — |
| video export slice (4K→1080p + audio mux) | 2.12× realtime, 9 MB growth | ≥ 2× ✓ |

Versus the single-4K-clip soak: harder per frame (5 layers with keyframe
interpolation vs 1) — p95 18.6 ms vs 12.2 ms — but far better cached
(40% hit rate vs 0%), because overlapping layers reuse the same static
images. Tier-0 refine is 10× cheaper here (7.9 ms vs 77 ms): the assets are
small, so full resolution costs little; the earlier number was dominated by
decoding one 4K source frame.
