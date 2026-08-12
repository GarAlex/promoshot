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
