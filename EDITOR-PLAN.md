# `promo-editor`: one editor layer for macapp, egui and winui (2026-08-14)

R3 of [LINUX-READY-PLAN.md](LINUX-READY-PLAN.md), expanded. No feature work in
here — this moves editor behaviour out of SwiftUI so a second and third front
end do not each reimplement it.

---

## 1. Where the editor lives today

Measured, not assumed:

- **The FFI is read-only.** 39 `pub extern "C"` functions: `promo_project_parse`,
  `promo_project_to_json`, and pure queries (`promo_layer_transform`,
  `promo_media_rect`, `promo_layer_gain`, …). The core answers questions about
  a snapshot. It has never edited anything.
- **Swift owns the document.** `RecordingProject` is an `ObservableObject`
  *class* with `@Published` fields, mutated in place by the views; persistence
  is a callback, `onProjectChanged()`. Ten views observe it.
- **Editor state is SwiftUI `@State`.** From `LayersManagementView` alone:
  `selectedLayerID`, `editingLayerID`, `editingResourceRoute`, `dragStarts`,
  `reorderStarts`, `isScrubbing`, `resumeAfterScrub`, `clock`,
  `pendingScrollTarget`, `layerPendingDelete`, `exactEdit`, plus `@AppStorage`
  for `useLanes`, `windowSeconds`, `focusWindow`.
- **~65 mutating functions** across the three big views — 24 in
  `LayersManagementView` (`addImageLayer`, `addVideoLayer`, `addCaptionLayer`,
  `addAudioLayer`, `addDrawingLayer`, `deleteLayer`, `reorderLayer`,
  `applyLayerOrder`, `setStart`, `setDuration`, `setTimelineDuration`,
  `setTotalDuration`, `applyExactEdit`, transport…) and 41 across
  `ResourcesView` and `ProjectEditorView`.
- **No undo anywhere.** No `UndoManager`, no command history, nothing to
  invert.

## 2. The decision this plan turns on: who owns the document?

Today Swift owns it and the core computes. If that stays true, egui and winui
each need their own document ownership and their own mutation code — which is
precisely the duplication this exists to prevent. Rustrator's egui app is what
that looks like after a year.

**So the endgame is: the core owns the document.** Front ends hold a handle,
send commands, and re-read. Undo lives in one place because there is only one
place edits happen.

The cost is real and lands entirely on macapp: `RecordingProject` stops being
the truth and becomes a projection of it. That is why this is staged rather
than done in one commit.

## 3. Crate shape

```
promo-editor/            depends ONLY on promo-model + promo-timeline
├── document.rs          owns ProjectMetadata; version counter; dirty flag
├── command.rs           the Command enum + apply(); every mutation is one
├── history.rs           undo/redo
├── selection.rs         selection, pinning, "reveal newly added"
├── timeline.rs          viewport, zoom, lane packing  ← port of TimelineLanes.swift
└── transport.rs         playhead / play / pause / scrub state machine
```

No I/O, no GPU, no wgpu, no platform crates — so it builds anywhere the model
builds, including Windows and wasm, and its tests run on any machine.

## 4. Stages

Strangler, exactly as P1–P6 were done: a flag per slice, parity asserted
against the Swift path, then the Swift path deleted. **macapp ships green after
every slice.**

### Stage 1 — derived state, no ownership change *(start here)*

Nothing about who owns the document changes. Only the *computed* and
*ephemeral* editor state moves, so this is nearly risk-free and immediately
useful to egui.

| Slice | Moves | Parity fixture | |
|---|---|---|---|
| 1.1 | Lane packing + viewport (`TimelineLanes.swift`, 195 lines) | `TimelineLanesTests` — already exists | **DONE 2026-08-14** |
| 1.2 | Selection + pinning + reveal-newly-added | this session's rules, already specified by their fixes | |
| 1.3 | Transport state machine (§5) | new table-driven tests | |

After Stage 1, an egui app can render a correct, interactive timeline over a
read-only document. That alone is most of E0–E2 of the egui plan.

### Stage 2 — the core owns the document

`promo-editor::Document` holds the parsed `ProjectMetadata` and a version
counter. Every mutation becomes a `Command`. Migrate the ~65 functions in
groups — layers, then timing, then resources, then composition settings — each
behind the flag, each with a parity test that applies the command in Rust and
the old path in Swift and asserts the resulting JSON is byte-identical.

`RecordingProject` survives as an `ObservableObject`, but its fields are filled
from the core on version change, and `onProjectChanged()` becomes "the core
says version bumped".

### Stage 3 — undo/redo, and deletion

With every edit expressed as a command, `history.rs` is a stack, not a
retrofit. Then the Swift mutation paths and their flags are deleted.

Snapshot-based history first (projects are small JSON; a whole-document
snapshot per command is simple and obviously correct), with command inversion
only if a measurement says snapshots are too heavy. Do not start with the
clever one.

## 5. Transport deserves to be a state machine

This session fixed the *same* scrub bug four times — `LivePreviewView`,
`LayersManagementView`, and both resource trim views — because four players
each re-implemented the same three booleans. That is the single strongest
argument in this document, and it should be answered with a state machine that
is written once and tested exhaustively:

```
Idle ──play──► Playing ──grab──► Scrubbing{resume: true}
                  ▲                    │
                  └──── release ───────┘   (seek, then resume iff resume)

Idle ──grab──► Scrubbing{resume: false} ──release──► Idle (seek only)
```

Invariants each of the four bugs violated, and which the tests must pin:

1. While `Scrubbing`, the clock must not write the playhead.
2. A tick that lands between the grab and the task noticing must not move it.
3. Release seeks first, then resumes only if playback was running at grab.
4. A seek issued during playback must not be treated as failed when it is
   superseded (`finished == false` also means "not ready yet").

## 6. The FFI, and why it serves winui too

Handle-based, matching the existing `ProjectHandle` pattern:

```
promo_doc_open(json) -> *mut DocHandle      promo_doc_apply(doc, command_json)
promo_doc_free(doc)                         promo_doc_undo(doc) / _redo(doc)
promo_doc_to_json(doc)                      promo_doc_version(doc) -> u64
promo_editor_*(…)                           selection / viewport / transport
```

Commands cross as JSON. They are rare and tiny — unlike the per-frame export
scene, which stays flat binary (`promo_compose_frame_raw`) for exactly the
reason JSON is fine here.

egui and any Rust front end skip the FFI and depend on the crate directly.
**winui gets the same C ABI Swift uses** — whether it is C#/WinUI 3 or native,
a C ABI is what it can consume. One boundary, three front ends, no third
implementation.

## 7. Gates

- **Parity**: every slice diffed against the Swift implementation across
  `fixtures/projects` before the Swift copy is deleted.
- **Performance**: command apply benched with a committed baseline. The
  100+-layer responsiveness won this cycle (drag re-sync 4 ms at 150 layers,
  cold sync 18 ms) is the number to defend — see §8.
- **Undo**: apply-N-commands-then-undo-N returns byte-identical JSON, over
  every fixture, property-test style.
- **Transport**: the §5 table, one test per invariant.

## 8. Risks

**SwiftUI observation granularity — the one that can undo real work.** If a
version bump invalidates the whole project object, every layer row redraws on
every drag tick and the 100+-layer responsiveness regresses. Mitigation:
version *per layer* alongside the document version, and let the views observe
the narrow thing. Decide this in Stage 2 slice 1, not after the regression.

**Dual ownership during migration.** For the length of Stage 2 both sides can
write. Mitigation: the flag chooses exactly one writer, never both, and the
parity test runs both and compares rather than letting both mutate.

**Command granularity.** Too fine and undo becomes per-keystroke; too coarse
and it loses work. Resolve per command group, guided by what a user would
expect one ⌘Z to undo.

**`ResourcesView` is 3.5k lines** and is the least separable of the three. It
is deliberately last in Stage 2.

## 9. Not in scope

Rendering, media backends, capture, persistence policy (file layout and
security-scoped bookmarks stay host-side), text shaping, and any new editing
feature. Undo is the one exception — it arrives as a consequence of the
command model, not as a feature request.

---

## Status

**Slice 1.1 done** (2026-08-14). `promo-editor` exists — model + timeline deps
only, so it builds anywhere — with `timeline.rs` carrying lane packing, the
viewport and the width policy, and all 14 Swift cases ported.

The whole pattern is proven end to end: crate → `promo-ffi::editor` (JSON in,
JSON out, since editor calls are rare and small) → `PromoLanes` in Swift →
`TimelineLanesParityTests`, which packs every shipped fixture both ways —
fitted, with a gutter, at four window positions, and with a pinned selection —
plus the synthetic shapes, row identity and the width policy.

It earned its keep immediately: the gate failed on first run because serde's
`camelCase` emits `rowId`/`layerIds` while this project's Swift-facing keys
capitalise ID (`resourceID`, `imageCutID`). Swift's decoder silently yielded
nothing rather than erroring — exactly the kind of mismatch that would have
looked like "lanes just don't work" much later.

Swift still owns the packing the app *uses*; the core agreeing is the
precondition for deleting the Swift copy, which happens once slices 1.2–1.4
land and `LaneTimelineView` reads lanes from the core.

Next: **slice 1.2** — selection, pinning and reveal-newly-added.
