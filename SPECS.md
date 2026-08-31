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

## Format design (settled in discussion, 2026-08-30; check before "tidying")

- D1 **Transitions dress edges of visibility; keyframes animate
  properties.** An edge is a layer's own start/end (`transitionIn`/`Out`)
  or a content swap mid-life (a swap keyframe's `transition`) — one
  concept, attached wherever the edge lives. The two COMPOSE: the
  transition envelope multiplies over whatever the keyframes resolve to,
  which is what lets tools replace or copy a layer's keyframes (Motion
  apply, copy-lane) without ever destroying an arrival. Do not attach
  transitions to first/last keyframes: keyframes are optional and mobile,
  and gluing an arrival to one makes dragging a choreography diamond move
  the arrival — action at a distance.
- D2 **Shorthand + rich object is deliberate, not debt.** `fadeIn`/`fadeOut`
  are the one-number spelling of `transitionIn`/`Out` with `kind: fade`.
  The rich object wins when both are set — the richer statement is never
  silently overruled — the validator names the shadowed shorthand, and the
  shorthand is linear BY DEFINITION (a curved fade is the rich object with
  `easing`). The shorthand must be read forever (old files exist), so
  writers dropping it would add a migration and buy nothing observable.
- D3 **A rule is stored, re-resolved on every read, and its resolved
  answer is written beside it** — `placement`, `durationRule`, layer
  anchors, `wait`. The stored answer is what earns a feature NO reader
  rung: an older reader ignores the rule and plays the number. Resolution
  must be a fixed point over its own output (the `wait` lesson: a resolve
  that consumes the answer it wrote walks the file on every open).
- D4 **Known asymmetry, deliberate for now**: between two LAYERS a
  transition cannot coordinate the neighbour — `push` has nothing to
  shove at a layer's own edge and behaves as `slide`; at a resource swap
  it has both contents and truly pushes. Sequences that need strong
  transitions belong on ONE swapping layer (the store listing's shape).
  Coordinated two-layer transitions are future compositor work, rung-worthy,
  parked in the app repo's ROADMAP until a use case demands them.
