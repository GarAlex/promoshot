# Linux-readiness refactor: core + macapp (2026-08-14)

The work that has to happen **before** the egui app (`../egui/PLAN.md`) is worth
starting. Nothing here adds a feature; it moves the Apple boundary to where it
belongs and gives the editor a home outside SwiftUI.

## 0. Decisions locked

- **Linux** — ffmpeg, free of charge, **no capture**.
- **macOS** — unchanged: Metal, VideoToolbox, zero-copy IOSurface. This
  refactor must not cost the Mac app a millisecond.
- **Windows** — its own encoders (Media Foundation) and its own capture. Last.
- **`promo-editor`** — a new crate in this workspace.

*On ffmpeg licensing, one caveat and then it is settled:* free-of-charge does
not by itself satisfy the GPL — that asks for source, not for a price. The
route that costs nothing and closes the question is dynamic-linking the
system's LGPL ffmpeg on Linux, which is what a `.deb` gets from the distro
anyway. If you would rather publish the Linux app's source, static GPL is fine
too. Either way it stops being a blocker.

---

## 1. Where the Apple boundary actually is

Measured, per crate, for `x86_64-unknown-linux-gnu`:

| Crate | Portable today | Apple-gated | Missing |
|---|---|---|---|
| `promo-model` | all | — | — |
| `promo-timeline` | all | — | — |
| `promo-gpu` | `compositor`, `vector`, `surface` | `iosurface`, `spike` | Linux import path |
| `promo-engine` | `governor`, `mixer` | **`preview` — the entire preview engine** | — |
| `promo-ffi` | `compose`, `vector`, `preview`, `project` | 2 modules | — |
| `promo-media` | traits | — | **every backend** |

**Correction to an earlier claim.** "The core compiles clean for Linux" is
true, and misleading. It compiles because the non-portable parts are compiled
*out*: `promo-engine/src/lib.rs` gates `pub mod preview` behind
`cfg(any(target_os = "macos", target_os = "ios"))`. On Linux the crate today
offers a memory governor and a PCM mixer, and no preview engine at all.

Two specifics that decide the shape of R1:

- `promo-engine::preview` is typed **directly** on `IOSurfaceRef` — its
  `FrameProviderFn` writes an `IOSurfaceRef` out-param, and the module declares
  and calls `CFRetain` / `CFRelease` / `IOSurfaceGetWidth` itself. CoreFoundation
  knowledge has leaked out of `promo-gpu` and into the conductor.
- `GpuSurface` — the enum designed for exactly this, with `IoSurface`,
  `D3DSharedHandle`, `DmaBuf` and `CpuPixels` variants and a doc comment saying
  "everything downstream sees only a wgpu texture" — **is exported and never
  used**. The abstraction was designed in P0 and then bypassed.

So the refactor is less "design a portability layer" than "finish wiring the
one that is already there".

---

## 2. R1 — Wire `GpuSurface` (the gate for every Linux pixel) — **DONE 2026-08-14**

1. **Compositor gains a single import entry**: `Compositor::import(&GpuSurface)`
   dispatching to
   - `IoSurface` → the existing zero-copy adoption path **and its cache**,
   - `CpuPixels` → the existing `upload_texture`,
   - `DmaBuf` / `D3DSharedHandle` → `Err(Unsupported)` for now, with the
     variants already named so capability negotiation can see them.
2. **Provider becomes surface-agnostic**: `FrameProviderFn` yields a
   `GpuSurface` instead of an `IOSurfaceRef`. The Swift side keeps handing over
   IOSurfaces — it just names the variant.
3. **CoreFoundation moves back into `promo-gpu::iosurface`**, the only module
   that should know what a CFRetain is. `promo-engine` stops declaring Apple
   externs.
4. **Un-gate `promo-engine::preview`.**

**Shipped.** The provider now fills a `HostSurface` descriptor (a C struct
covering IOSurface / D3D handle / DMA-BUF / CPU pixels) instead of writing a
bare `IOSurfaceRef`; `Compositor::import` is the single entry point;
CoreFoundation is back inside `promo-gpu::iosurface`; `preview` is un-gated
and the whole workspace — including `promo-ffi` — now checks for
`x86_64-unknown-linux-gnu`.

Two things worth knowing for later:

- The module carried **two** gates: the `pub mod preview` line *and* an inner
  `#![cfg(...)]` at the top of `preview.rs`. Removing only the outer one gives
  a confusing "unresolved import" rather than an error at the gate.
- Swift structs are not C-representable, so the callback takes a raw pointer
  and binds it. `promo_host_surface_layout` exposes the Rust offsets and
  `testHostSurfaceLayoutMatchesRust` asserts every one of them — it caught a
  real mismatch on its first run (Swift `size` 44 vs Rust `size_of` 48; the
  Swift equivalent is `stride`).

*Gates met*: `portable_tests::cpu_pixels_render_the_same_composition` renders
the SAME fixture as the IOSurface suite through CPU pixels into a wgpu texture
and asserts the SAME pixels. Mac 241 tests, iOS 212, core 21 + clippy clean.
Perf, re-measured old-vs-new on one machine in one session: warm render
1.5947 → 1.6041 ms (+0.6%, noise); cold 1.9437 → 2.1614 ms nominal, with
overlapping confidence intervals and no added work on that path. Both far
inside the 25% gate.

After it, the Linux app can render a real composition of images, drawings and
captions with no codec at all.

## 3. R2 — `promo-media` stops being a skeleton — **STARTED 2026-08-14**

1. Traits become a **registry with capability negotiation** — backends
   register, the engine picks per asset.
2. **VideoToolbox becomes a registered backend** rather than an ad-hoc host
   arrangement. Behaviour on Mac is unchanged; what changes is that it now sits
   behind the same contract ffmpeg will implement.
3. **Conformance suite + fixtures**, written once and run against every
   backend: display rotation (the `preferredTransform` lesson the trait doc
   already flags), odd dimensions, variable frame rate, long files,
   keyframe-aligned seek accuracy.

**Landed (2026-08-14):** the registry, `GpuSurface` hand-off (the placeholder
`promo_gpu_surface::Frame` is gone), and an **ffmpeg backend driven as a
separate process** — decode and encode both. Reading frames off a pipe rather
than linking libav means no build dependency, no licence entanglement, and a
small enough surface to get the trait shape right; a linked backend implements
the same traits later and this one stays as the portable fallback.

The decoder is built for how rendering actually asks: time walks forward, so
one reader serves a whole clip, a backwards jump restarts it, and a forward
jump beyond a second re-seeks rather than decoding everything in between.

`promo-cli` now renders video layers and encodes through the same crate, so
the CLI and a future egui app share one codec layer instead of each spawning
ffmpeg for themselves.

**The conformance suite exists** (`promo-media::conformance`) and runs over
any `DecoderBackend`, so a new backend is judged against the same invariants
rather than whatever its author thought to test: display dimensions, frame
size matching what `info` promised, forward walking that actually advances,
a repeatable rewind, and `None` past the end.

It earned itself on the first run. The **rotation** case failed: ffprobe
reports a clip's STORED size, ffmpeg's decoder auto-applies the display
matrix, so a quarter-turned capture arrived transposed — and since a 90° swap
leaves the pixel count identical, nothing errored, the picture was simply
scrambled. Fixed by reporting display dimensions; pinned by a fixture built
with `-display_rotation 90`.

**Still open in R2**: VideoToolbox registered as a backend. Until it is, the
suite proves each backend meets the contract but nothing diffs ffmpeg against
VideoToolbox on the same asset — the cross-backend half of the gate.

## 4. R3 — `promo-editor`

**Expanded into its own document: [EDITOR-PLAN.md](EDITOR-PLAN.md).** Summary
below; the decision it turns on (the core, not Swift, owns the document) and
the staging are there.

A headless editor crate: app state and commands, no rendering, no I/O. Both
front ends drive it — the Mac app through `promo-ffi`, the egui app by direct
dependency.

Today this logic lives inside three SwiftUI views totalling **8.6k lines**
(`ResourcesView` 3545, `ProjectEditorView` 2712, `LayersManagementView` 2356).
Not all of that is editor logic — much is genuinely view code — but everything
that is, is trapped.

Move in dependency order, one slice at a time:

| Slice | What | Why first/last |
|---|---|---|
| 1 | Lane packing + timeline viewport (`TimelineLanes.swift`, 195 lines) | Already pure view-model logic with its own tests; `TimelineLanesTests` becomes the parity fixture. Proves the pattern cheaply. |
| 2 | Selection, pinning, "reveal a newly added layer" | Small, rule-shaped, already specified by this session's fixes |
| 3 | Transport: playhead, play/pause, **scrub semantics** | This session fixed the same scrub bug in four separate players in one app — the strongest argument in the document for a shared owner |
| 4 | Timeline window / zoom | Depends on 1 |
| 5 | Keyframe edit operations | The largest slice; do it once the pattern is proven |
| 6 | Undo/redo | **New capability** — the Mac app has no `UndoManager` anywhere today. Design it in the crate rather than retrofitting it twice. |

*Gates*: each slice is diffed against the Swift implementation over
`fixtures/projects` before the Swift copy is deleted.

## 5. R4 — macapp adopts it

Strangler, exactly as the core migration was done: each slice lands behind a
flag (the `PromoCoreTimeline` pattern), parity is asserted against the Swift
path, then the Swift path is deleted and the flag retired. **The Mac app ships
green after every slice** — that rule is what made the P1–P6 migration
survivable and it applies unchanged here.

---

## 6. Order

```
R1 (GpuSurface)  ────────────►  unblocks Linux rendering
      │
      ├── R2 (promo-media)  ──►  unblocks Linux video      [independent of R3]
      │
      └── R3 (promo-editor) ──►  unblocks a non-forked UI  [independent of R2]
                │
                └── R4 (macapp adoption, per slice)
```

R1 first and alone: it is small, it is the gate for everything visual, and it
removes an Apple leak that would otherwise get copied. R2 and R3 are
independent of each other and can proceed in either order or in parallel.

## 7. Explicitly not in this refactor

Capture (any platform), Media Foundation, caption text shaping, audio playback,
packaging, licensing enforcement. Those belong to the egui and Windows plans;
listing them here is how they stay out.

---

## Status

**R1 done** (2026-08-14). Next: R2 or R3 — independent of each other. R3 is
expanded in [EDITOR-PLAN.md](EDITOR-PLAN.md); its first slice is the lane
packer.

Known debt, pre-existing and unrelated: `cargo fmt --check` in `check-all.sh`
was already failing before this work — 23 diffs across 8 files. Left alone so
this change is not buried in a repo-wide reformat; worth its own commit.
