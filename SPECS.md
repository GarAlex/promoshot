# promo-core SPECS — invariant catalog

Check before changing internal logic; every fix ships with a test (same
discipline as rustrator-core).

## Timeline
- T1 `loop_fold(local, period)`: identity when `period <= 0.01` or
  `local < period`; otherwise `local - floor(local/period)*period` with
  `offset = floor(local/period)*period`. Mirrors Swift
  `ProjectResource.loopFolded` — the two must stay value-identical
  (Phase-1 parity harness enforces via shared fixtures).

## GPU
- G1 GpuSurface raw handles are inert until an import module touches them on
  a device-appropriate thread; the enum itself is Send.
- G2 IOSurface import: BGRA8Unorm (non-sRGB) — bytes written by wgpu appear
  byte-exact through the IOSurface CPU mapping after queue wait (spike test
  is the guard).
- G3 No full-resolution pixel data may cross the FFI as CPU bytes on preview
  or export paths (CpuPixels is the software-decode fallback ONLY).

## FFI
- F1 `promo_core_version` returns static storage; callers never free.
- F2 The C ABI is additive-only until Phase 5 (see app repo RUST-CORE-PLAN.md).
