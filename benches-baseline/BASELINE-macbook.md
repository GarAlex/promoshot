# promo-core bench baselines — MacBook (Apple Silicon), 2026-07-20

Recorded with `cargo bench` (criterion, release). Regression gate: >25%
slower than these numbers fails the push gate (bench-guard pattern).

| Bench | Baseline | P0 gate | Notes |
|---|---|---|---|
| ffi_noop_call | ~976 ps | < 1 µs | C-ABI call overhead — gate beaten ~1000× |
| ffi_version_call | ~394 ps | — | static CStr return |
| iosurface_spike/import_render_readback_1080p | ~3.29 ms | runs green | FULL round trip incl. fresh IOSurface create, wgpu texture adoption, clear render, and 8 MB CPU readback per iteration — production pools surfaces and never reads back on the hot path, so per-frame cost will be far below this |
| iosurface_spike/import_render_readback_256 | ~1.78 ms | runs green | fixed-cost dominated (device sync + allocation) |

Spike verification: every pixel of the wgpu clear is observed byte-exact
through the IOSurface CPU mapping (BGRA), at 256² and 1920×1080 — the
zero-copy adoption path is real, not theoretical.
