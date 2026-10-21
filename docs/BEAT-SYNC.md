# Beat sync: implementation design

Status: slices 1 to 8 built (2026-10-20); the rest is design. Where
building changed the design, the "Slice N as built" notes under §9 say
what and why. Covers the request in
`8a7e43a8-beat-sync.md`: all of Part A plus resource provenance.
Owner decisions: the format grows in one direction only, the apps open,
render and save without flattening, and seconds come from the core.

The rule everything below follows is the one `promo-timeline/src/attachment.rs`
already states: **resolve once, write numbers, and leave nothing for a
renderer to interpret.** Musical values are specs that a person edits.
`start_time`, `duration` and the other fields stay plain `f64` answers, and
the engine never learns that a beat exists.

---

## 1. Wire format and the Rust time value

### 1.1 The value

A new module, `promo-model/src/musical.rs`:

```rust
pub enum TimeValue { Seconds(f64), Musical(MusicalTime) }

pub enum MusicalTime {
    Beats(f64),                          // {"beats": 4}    a length, or an offset from beat 1
    Bars(f64),                           // {"bars": 2}
    Beat(f64),                           // {"beat": 17}    an absolute grid position, 1-based
    Bar(f64),                            // {"bar": 5}
    Snap { to: SnapUnit, near: f64 },    // {"snap":"beat","near":12.3}
}
pub enum SnapUnit { Beat, Downbeat, Bar }   // `downbeat` and `bar` mean the same point
```

Decoding is strict:
- The object must have exactly one of `beats`, `bars`, `beat` or `bar`, or else exactly `snap` with `near`.
- Each number must be finite. `beats` and `bars` must be at least 0. Fractions are allowed.
- Any other key is an error that names its path, for example `layers[s1].duration: unknown key "beets"`.

Encoding is canonical: the same key, and an integral value is written as an
integer (`4`, never `4.0`). That keeps a hand-written file byte-stable through
a save.

### 1.2 Which fields accept it

| Field | Lengths (`beats`, `bars`) | Positions (`beat`, `bar`, `snap`) |
|---|---|---|
| layer `startTime` | offset from the clock's beat 1 (`{"beats":n}` is the same as `{"beat":n+1}`) | the point on the grid |
| layer `duration` | measured along the grid from the resolved start | the END point ("until bar 9") |
| layer `fadeIn`, `transitionIn.duration` | measured forward from the start | not allowed (a break) |
| layer `fadeOut`, `transitionOut.duration` | measured backward from the end | not allowed |
| keyframe `time` | from the layer's start, then made layer-local | absolute grid point, made layer-local (minus the start) |
| keyframe `transitionDuration` | measured backward from the keyframe | not allowed |
| sequence entry, `after.offset`, generator `decay`, `duration`, `range` | yes | yes where the field is a point |

"Along the grid" means this: t becomes a fractional beat index through the
piecewise-linear inverse of the beat list, n is added, and the result is
mapped back. On a detected grid with drifting tempo, 4 beats is 4 real beats
and not 4 × 60/bpm. Bars use the downbeat list the same way.

Nothing else changes: resource trims, subtitles and legacy settings
keyframes all stay seconds-only.

### 1.3 Keeping it lossless without touching ~300 `f64` call sites

The `f64` fields stay as they are. The value moves aside at the JSON level:

- **`musical::lift(&mut Value)`** runs inside `ProjectMetadata::from_json`, before serde.
  - It walks every layer list: the project's layers, stage `members`, and each composition resource's `composition.layers`, recursively.
  - Wherever a listed field holds an object, it moves that object into the entity's side map `timeSpecs` and writes a placeholder `0` in its place.
  - The side map's shape:
    ```
    "timeSpecs": { "duration": { "spec": "{\"beats\":2}", "seconds": 0, "label": "2 beats" } }
    ```
    The keys are `startTime`, `duration`, `fadeIn`, `fadeOut`, `transitionIn.duration` and `transitionOut.duration` on a layer, and `time` and `transitionDuration` on a keyframe.
  - `spec` is canonical JSON text. A string keeps the Swift side opaque.
  - `label` is the core's own wording, so Swift never has to format a beat count.
  - Entries also record derived starts: `"startTime": {"rule": "sequence" | "after", "seconds": …}`.
- **`ProjectLayer.time_specs` and `ProjectLayerKeyframe.time_specs`**: `Option<BTreeMap<String, TimeSpec>>` with serde key `timeSpecs`. This is real model state: it survives clone, undo snapshot, duplicate and paste.
- **`musical::fold(&mut Value)`** turns the "app form" back into the "file form". `ProjectMetadata::to_json` runs it, and so does `Document::saved_json_from` (after `keep_unread`). For each entry:
  - If the number at the field is within 1e-4 of `seconds` (the `PromoTiming.detachEditedLayers` epsilon), fold writes the spec object back at the field.
  - Otherwise the person edited that value: the number stands, and only that value is flattened.
  - A `rule: "after"` entry whose start moved: drop the layer's `after`.
  - A `rule: "sequence"` entry whose start moved: remove the layer from the sequence. If only its duration moved, rewrite the layer's entry as `{"layer": id, "seconds": n}`.
  - Generated content is stripped (§4).
  - Then `timeSpecs` is removed. It never reaches a file.
- There are two serializations:
  - `to_json()` produces the file form.
  - `to_app_json()` produces the app form, for the FFI and the editor document's internal snapshots.
  - Every `serde_json::to_value(&meta)` that writes a file goes through `to_json`. `promo_project_resolve_timing` (core-private `promo-ffi/src/project.rs:1748`) does this today, so it changes.
- Rust editing commands (`SetLayerTiming`, `UpsertKeyframe`, `UpdateLayer` in `promo-editor/src/document.rs`, and the matching `promo-author` tools) clear the `timeSpecs` entry for each field they set. That handles a headless edit on a file that was never resolved, where the placeholder `0` would otherwise compare equal.
- A new editor command, `SetTime { layerID, keyframeID?, field, value: TimeValue }`, is the one door for setting a musical value. It serves the app UI and `promo_apply`.

### 1.4 New fields

**Composition settings**
- `compositionSettings.beatClock`, `{"resourceID": "…", "startTime": 0}`. The same key goes on `Composition` (a nested composition's own clock), and on a layer as an override.
  - `startTime` is seconds only: it is the output time of the audio's source second 0.
  - When `startTime` is absent, the clock follows the first enabled audio layer that plays the resource. Its output time = layer start + (source − the layer's source start) / speed, the inverse of `mapping::source_time_for_layer`.
- `compositionSettings.sequence` (and `Composition.sequence`): an array of entries.
  - The length forms are `{"layer": "s1", "beats": 4}`, `{"layer": "s2", "bars": 1}`, `{"layer": "s3", "seconds": 2}` or `{"layer": "s4", "duration": <time value>}`.
  - An entry with no length keeps the layer's own `duration`.
- `compositionSettings.flash`: an object or an array (§4).

**Layer**
- `after`: `"<layerId>"`, or `{"layer": "<id>", "offset": <time value>}`.
- `pulse` and `shake`: an object or an array each (§4).

**Resource**
- `beatGrid`, `{"bpm": 131.2, "offset": 0.42, "timeSignature": [4, 4]}`, on audio resources.
  - `offset` is the first downbeat in source seconds and defaults to 0.
  - `bpm` counts the denominator's note.
- `provenance` on any resource (§8).

All of these are typed structs in `project.rs`. Each carries the `extra` bag
(the lossless-reader convention) and goes into `promo.schema.json`. Through
`#[schemars(with = …)]`, the time fields become `oneOf: [number, MusicalTime]`
in the schema.

### 1.5 Rung

The app's `JSONDecoder` and every reader at rung 48 or below decode `startTime`
as a number. An object there cannot be decoded, which is exactly the
strict-kind case in `adding-a-wire-field-is-not-always-additive`. Rung 49 is
therefore new. `minimum_reader_version` (`project.rs:4651`) gets one check at
the top of the ladder, walked over `nesting::all_layers` and the compositions.

| A file carrying… | Rung |
|---|---|
| any `timeSpecs` entry with a `spec` (a musical value in a time field) | 49 |
| `sequence` or `after` (an older writer would edit around a derived start and contradict it) | 49 |
| `pulse`, `shake` or `flash` (an older reader renders a different film) | 49 |
| `beatClock`, `beatGrid` or `provenance` alone (ignorable; nothing renders differently) | 48 (`LOSSLESS_READER`) |
| seconds only | unchanged (whatever it is today) |

- `READER_VERSION` = 49 in `promo-model/src/lib.rs`. `LOSSLESS_READER` stays 48.
- `ProjectStore.currentFormatVersion` and `supportedFormatVersion` = 49. The pin `the_apps_reader_version_is_the_cores` in core-private `tests/wire.rs` moves them together.
- `schema.md` keeps its single header statement, bumped to 49, with no per-feature callouts (`one-version-no-compat-docs`).
- The ladder reads the generator FIELDS, not the generated content, which `fold` has already stripped.

---

## 2. The resolution pass (core only)

### 2.1 One entry point

`promo-timeline/src/musical.rs` (new):

```rust
pub fn resolve_timing(meta: &mut ProjectMetadata, grids: &BeatGrids) -> TimingReport
```

`resolve_attachments` becomes `pub(crate)`, so the compiler finds every caller
and each one moves to `resolve_timing`:
- `promo-cli/src/project.rs:74` (`Project::from_text`), the CLI and every MCP tool that shells out to it
- `promo-timeline/src/report.rs:220` (`file_report`), behind `validate` on both doors
- `promo-author/src/lib.rs:1167` (explain, which today resolves only a copy; it now explains the resolved project)
- `promo-editor/src/author.rs:1241`, plus `document.rs:1029`, `:1071` and `:1341`
- core-private `promo-ffi/src/project.rs:392` and `:1748`

**Why this covers every path.** The preview engine, the Mac video export
(`VideoComposer`) and the still export (`renderCompositeStills`) never
resolve. They receive the app's model, which already holds the core's numbers
(§6). The CLI renders after `Project::from_text`. So one function feeds every
path, and the engine is unchanged.

**What the result carries.** `BeatGrids` is a map from clock key to `Grid`.
`TimingReport { problems: Vec<TimingProblem>, resolved: Vec<ResolvedTime>, generated: GeneratedIds, missing_grids: Vec<String> }`.
`TimingProblem` wraps `AttachmentProblem` and adds the beat cases (§7).

### 2.2 Order

The pass runs per scope: the top level, then each composition resource. Stage
members belong to their parent's scope.

1. **Clocks.** For each layer, the governing clock is the layer's `beatClock`, else the scope's. A `Grid` is built from `grids` and mapped onto the output timeline. A nested composition does not inherit its parent's clock: it may be placed at several times.
2. **Absolute starts.** `startTime` values in `beat`, `bar`, `snap`, `beats` or `bars` become seconds. They depend on nothing else.
3. **Fixpoint loop**, bounded by the layer count plus one:
   - a. `sequence` and `after`: a layer whose predecessor's end is known gets its start (plus offset).
   - b. Musical `duration` for any layer whose start is now known. Keyframe `time` values too, since waits read them.
   - c. `resolve_attachments` runs unchanged on the numbers. It reads non-attached layers' `start_time` as inputs, so an `after` layer can anchor attachments, and an attached layer can be what an `after` names.
   - The loop stops when a round changes nothing. Whatever is still unknown is `Circular`, and it keeps its stored number.
4. **Layer-relative lengths:** fades, transition durations and keyframe `transitionDuration`.
5. **Generators expand** (§4). They need final starts and ends.
6. **Side entries.** Every `timeSpecs.seconds` gets the value written. `resolved` lists `{at, spec, seconds}` for validate, explain and inspect.

The pass is idempotent. Resolving a resolved project changes nothing, and the
fold-then-lift round trip changes nothing; tests pin both. Without that, the
app's re-resolution after edits, and the loop in step 3, would drift.

### 2.3 Without a clock, or without a grid

- **No `beatClock`, but musical values present.** They resolve against a nominal grid, 120 BPM 4/4 with beat 1 at 0 s, so the project still lays out. A warning names every such value.
- **A clock with no grid.** The resource has no `beatGrid` override, no cache, and nothing has analysed it yet: the app before its analysis finishes, or a headless build without ffmpeg.
  - The same nominal grid applies, and the clock's resource id goes into `missing_grids`.
  - The app analyses and re-resolves. The CLI analyses before resolving (§3.6), so it reaches this state only when ffmpeg is missing, and then validate reports a break.
  - An export never starts while `missing_grids` is non-empty.
- **The grid ends before the timeline does.** It extrapolates using the last local period (and the first period backwards for pickups), and validate warns.

---

## 3. The beat tracker

### 3.1 Where it lives

- `promo-media/src/beats.rs` holds the analysis. It is pure and fed in chunks, shaped like `loudness::LoudnessMeter`:
  ```rust
  pub const TRACKER_VERSION: u32 = 1;
  pub struct BeatMeter { .. }
  impl BeatMeter {
      pub fn new(channels: usize, sample_rate: f64, options: BeatOptions) -> Option<Self>;
      pub fn push(&mut self, interleaved: &[f32]);
      pub fn finish(self) -> BeatAnalysis;
  }
  ```
  `BeatOptions` carries `meter` (default 4) and an optional `bpm_hint`.
- The wire types `BeatAnalysis` and `BeatGrid` live in promo-model, so the timeline can read them without depending on media.
- `promo-timeline/src/beats.rs` holds `Grid`: it is built from `BeatGrid` or `BeatAnalysis` plus the clock mapping. Its methods are `index_at`, `time_at`, `bar_at`, `bar_time`, `snap`, `advance` and `sections`.

### 3.2 Algorithm (Ellis 2007, librosa `beat_track`)

1. **Resampling.** The host gives native-rate PCM. The core downmixes to mono and resamples to 22 050 Hz with its own fixed windowed-sinc kernel (64 taps). The host never resamples.
2. **Onset strength.**
   - STFT: n_fft 2048, hop 512 (43.07 frames/s), Hann window.
   - 40 triangular log-spaced bands from 30 Hz to 11 kHz. Each band is compressed as `ln(1 + 1000·E)`.
   - The envelope is spectral flux, the mean over bands of `max(0, ΔS)`. It is detrended by subtracting a 1 s moving mean and normalised by its standard deviation.
   - A separate bass-band flux (30–150 Hz) is kept for downbeats.
3. **Tempo.**
   - Autocorrelation of the envelope over lags of 0.2–2.0 s (30–300 BPM).
   - Weighted by librosa's log-normal prior: centre 120 BPM, σ = 1 octave, or centred on `bpm_hint`.
   - The argmax is refined by parabolic interpolation, giving a fractional period P.
4. **Beats.**
   - A DP over the local score (the envelope convolved with a Gaussian of width P/32): `score[t] = local[t] + max over τ ∈ [t−2P, t−P/2] of (score[τ] − 100·ln((t−τ)/P)²)`.
   - Backtrack from the last strong local maximum. Weak beats at the leading and trailing edges are trimmed (local score below 0.5 × its RMS).
   - Each beat is refined to sub-frame time with a parabolic peak within ±2 frames.
5. **Uniform fit.** This is the fixed-BPM path. If at least 8 beats exist and every interval is within 4% of the median, a least-squares fit gives `t_k = a + k·P`. If the fit's RMS residual is under 10 ms, the output is exactly that arithmetic grid, with `"grid": "uniform"`. Generated music (Suno, ACE-Step) and the test fixtures land here, which is sub-frame accurate and stable under small PCM differences. Otherwise the output is `"grid": "tracked"`.
6. **Downbeats.** For meter m, the phase p ∈ [0, m) maximises bass flux at the beats k ≡ p, plus 0.5 × the spectral change across them (the cosine distance between the mean spectra of the beat segments). Ties go to the lowest p. `offset` is the first downbeat.
7. **Sections.**
   - Per-bar features: RMS in dB, bass ratio and spectral centroid, each standardised.
   - Novelty is the distance between the means of the 4 bars before and the 4 bars after each downbeat.
   - Boundaries are novelty peaks above mean + 1σ, at least 4 bars apart.
   - Each section gets `energy` (0–1) and `index`. `label` is best effort: `intro` (first and quiet), `outro` (last and quiet), `drop` (the largest energy rise), otherwise `section`.
8. **Confidence** is the mean envelope on the beats versus off them, squashed to 0–1. A value under 0.5 draws a warning (§7).

### 3.3 Output (cache file, CLI `--json`, MCP)

```json
{ "trackerVersion": 1, "source": {"filename": "song.m4a", "sha256": "…32 hex…", "bytes": 4810233},
  "duration": 30.0, "bpm": 128.0, "offset": 0.469, "timeSignature": [4,4],
  "grid": "uniform", "confidence": 0.94,
  "beats": [0.469, 0.938, …], "downbeats": [0.469, 2.344, …],
  "sections": [ {"index":0,"label":"intro","start":0.469,"end":15.469,"startBar":1,"bars":8,"energy":0.31}, … ] }
```

All times are in source seconds.

### 3.4 Determinism

Determinism is needed for "bit-identical across machines" and for the cache to mean one thing.

- All transcendentals (sin, cos, exp, ln, pow) come from the pure-Rust `libm` crate, which is already in `Cargo.lock`, and never from `std`, because macOS and glibc libm differ in the last ulp.
- The FFT is a small, in-house radix-2 implementation with twiddles from `libm`. `rustfft` picks a SIMD path at runtime, and rounding differs between those paths.
- Every reduction is sequential in a fixed order, accumulated in f64. There is no rayon, no fast-math and no `mul_add`. Rust does not contract to FMA on its own.
- Ties always go to the lowest index.
- Output times are rounded to 1 µs, and `bpm` to 0.001. Serialisation is serde_json (ryu, shortest round trip).
- The core resamples, so feeding the CLI and the app the same samples gives identical JSON. A test pins this (§9, S8).
- **The cache is authoritative.** Once a package holds an analysis, every host and every later tracker version reads it. It is never silently re-analysed, so an upgrade cannot re-time an edit. Re-analysis happens only through `promo beats --refresh` or the app's "Analyse again".
- **The override is closed-form.** `t_k = clock + (offset + k·60/bpm)/speed` is plain IEEE arithmetic and bit-identical everywhere. This is acceptance criterion 3.

### 3.5 Cache

- **Location:** `<project>/Resources/.beats/<key>.json`.
- **Key:** the first 32 hex digits of the SHA-256 of the audio file's bytes (`sha2` is already a workspace dependency). It is independent of path and name, so the key survives moving the package and renaming the file.
- **Who knows the naming:** only the core, through `promo_media::beats::{cache_path, load_cached, store}`. In-process, a (path, size, mtime) memo saves re-hashing a large file.
- **Nothing reads the folder as resources.** `Project::listing` already skips directories, and the app's scanners must skip dot-folders too (a risk, §10).

### 3.6 Feeding it

- **CLI** (`promo-media`):
  - `beats::analyse_file(path, options)` decodes f32le at the native rate through ffmpeg, using the `FfmpegAudioReader` argv without `-ar`. That keeps resampling in the core. It then calls `BeatMeter`.
  - `Project::from_text` builds `BeatGrids`, checking in order: the resource's `beatGrid` override, then the cache, then analysing and writing the cache. `--no-analyse` (and a read-only package) only reports `missing_grids`.
- **App** (core-private `promo-ffi/src/beats.rs`, new, with the same shape as `audio.rs`):
  ```c
  uint32_t promo_beat_meter_version(void);
  void*    promo_beat_meter_new(uint32_t channels, double sample_rate, uint32_t meter);
  int      promo_beat_meter_push(void* m, const float* samples, size_t len);
  char*    promo_beat_meter_finish(void* m);            // BeatAnalysis JSON (source filled by store)
  void     promo_beat_meter_free(void* m);
  int      promo_beats_store(const char* project_dir, const char* filename, const char* analysis_json);
  char*    promo_beats_lookup(const char* project_dir, const char* filename); // JSON or NULL
  ```
  - The Swift wrapper is `PromoCore.BeatMeter`, beside `LoudnessMeter`.
  - `ReVoice/Services/BeatAnalyzer.swift` (new, shared by both targets) reads an `AVAudioFile` into `AVAudioPCMBuffer` chunks and pushes them interleaved, as `AudioTimelineBuilder.loudness(of:)` does. It runs off the main thread and finishes with `promo_beats_store`.
  - Bindings are regenerated with `PROMO_UPDATE_BINDINGS=1 cargo test -p promo-ffi --test bindings`.

### 3.7 Override

When a resource has `beatGrid`, there is no detection for tempo or downbeats.
Its `bpm`, `offset` and `timeSignature` form the grid, extending backwards
through pickups. Sections still come from analysis, and only if a generator's
`range` names a section.

---

## 4. Generators

**Decision: expand into ordinary keyframes and layers during resolution, not
engine evaluation.**

Why:
- Keyframes already reach the preview and both export paths with no new engine input (`engine-inputs-three-paths`).
- The app's lanes show them, and the editors can retime or delete them.
- validate and explain see them.
- The ownership model is Auto zoom's (`autozoom.rs`, the keyframe `auto` signature), which has already been reviewed.

An engine modifier would be exact over a keyed base. But it would need a new
input that Swift must carry to all three hosts, and nothing could show it.

### 4.1 Format

```json
"pulse": {"every": "downbeat", "property": "viewport", "amount": 0.04, "decay": {"beats": 0.5},
          "range": {"bar": 9, "bars": 4}}
"shake": {"on": "beat", "amplitude": 8, "decay": {"beats": 0.25}, "range": {"section": "drop"}}
"flash": {"on": "downbeat", "colorHex": "FFFFFF", "opacity": 0.6, "duration": {"beats": 0.25},
          "blendMode": "screen", "range": {"from": {"bar": 1}, "to": {"bar": 17}}}
```

- `every` and `on` are synonyms. They take `beat`, `downbeat`, `bar`, or `{"beats": n}` (every nth beat).
- `range` takes `{"bar", "bars"}`, `{"beat", "beats"}`, `{"section": index|label}` or `{"from", "to"}`. Any of these is clipped to the layer's window. Without `range`, the generator covers the whole layer.
- `pulse` and `shake` sit on a layer. `flash` sits on `compositionSettings`. Each takes an object or an array.

### 4.2 Expansion (`promo-timeline/src/beat_effects.rs`, step 5 of §2.2)

**Pulse**
- For each grid point t in range, it writes two keys:
  - at t, the base value with the punch applied, with `transitionDuration` = attack (one frame at the project fps), so the peak lands ON the beat;
  - at t + decay, the base value, with ramp = decay and easing `easeOut`.
- The punch per property:
  - `viewport`: the window scales by 1/(1+amount) about its centre.
  - `placement`: the box's `height`/`width` scale by 1+amount, or `zoom` when the layer is placed by raw zoom.
  - `opacity`: base + amount, clamped to 0–1.
- "Base" is the layer's existing track sampled through `interpolation.rs` at that time. If the track already has keyframes, an extra base key goes in at each t − attack. The base is then piecewise-linear at beat resolution, which validate notes.

**Shake**
- Four keys per beat on `horizontalShift`/`verticalShift`: +a, −a/2 and +a/4, then the base.
- The direction comes from a splitmix64 hash of (layer id, beat index): deterministic, with no RNG crate.

**Flash**
- One generated drawing resource: a full-canvas `rect` filled with `colorHex`.
- One generated drawing layer above everything, with an opacity envelope per point: 0, then `opacity` at t, then 0 at t + duration, eased out.

**Ownership**
- Generated keyframe ids are deterministic: `beat-<layer>-<gen>-<k>-<role>`.
- Generated keyframes carry `auto: "beat:<hash of values as written>"`. Generated layers and resources carry `generatedBy: "flash:<n>:<hash>"`.
- On save, `fold` handles them like this:
  - **Unchanged** (the hash matches): stripped, since they regenerate on the next load. This is why changing the track re-times them.
  - **Edited:** kept as the person's, with the tag removed. Expansion then skips a grid point where a person's key on the same track sits within half a frame.
  - **Generated flash layer deleted in the app:** `fold` removes that `flash` entry, so the delete sticks.
  - **Generated flash layer edited:** it becomes the person's layer and its generator is removed.
- Resolution writes nothing to disk and pushes no undo step (`opening-a-page-edits-nothing`).

---

## 5. `sequence` and `after` versus attachment anchors

| | `timing` anchors (existing) | `after` (new) | `sequence` (new) |
|---|---|---|---|
| names its target | by stack position (previous/next by sortIndex) | by layer id, anywhere in the same scope | by list order |
| sets the length | through an end anchor | no | yes (beats, bars, seconds or a time value) |
| on the grid | no | offset may be musical | cuts snap: beat lengths are on-grid by construction, and `seconds` ends snap to the nearest beat when a clock is set |
| cycles | impossible / `Circular` | detected by the fixpoint and reported `Circular` | impossible (a list) |

- The first sequence member's own `startTime` (a number or musical) is the sequence's start. With a clock, a number snaps to the nearest beat.
- **Conflicts.** One layer in `sequence` and also carrying `after` or `timing.start` is a break: the sequence wins, `after` comes second, and validate says so. A layer listed twice, or an unknown id, is also a break.
- **Stored numbers.** A derived `startTime` stays in the file as the resolved number, as attachments do. A rung-48 reader is kept out anyway (§1.5).
- **Hand edits** in the app or a tool detach, the same rule as `PromoTiming.detachEditedLayers`, applied by `fold`.
- **Why add these** when `timing.previousPeerEnd` exists: slideshows interleave stills with captions and narration in arbitrary stack orders. "Back to back, N beats each" is a list, not a chain of neighbours.

---

## 6. The Swift apps

### 6.1 Model (`ReVoice/Models`)

- `ProjectLayer` and `ProjectLayerKeyframe` get `timeSpecs: [String: PromoTimeSpec]?`, with `PromoTimeSpec { spec: String?, rule: String?, seconds: Double, label: String }`. Both types use synthesized Codable, so the field needs no hand-written decoder.
- `ProjectLayer` and `ProjectResource` get `generatedBy: String?`. `ProjectResource` has custom CodingKeys and init at lines 259 and 307, so the key is added there.
- `ProjectResource` also gets `beatGrid: BeatGrid?`.
- `CompositionSettings` gets `beatClock: BeatClock?` (CodingKeys at line 809, init at 834).
- `startTime`, `duration`, `fadeIn` and the other time fields stay `TimeInterval`. Every existing arithmetic and drag site is untouched. There is no union type in Swift, because the core hands Swift numbers.
- `sequence`, `after`, `pulse`, `shake`, `flash` and `provenance` stay unknown to Swift. `keep_unread` restores them from the file on save, and `fold` runs after it, so it can detach `after` and sequence entries.

### 6.2 Open, resolve, save (`ProjectStore.swift`, `PromoCore.swift`)

- **Read** (`ProjectStore.read`, l.467). After the version gate and before `defaultingKeyframeRamps` and decode, the raw file goes through `PromoProjectJSON.opened(json, projectDir)`. That is the new `promo_project_opened(json, dir)`: lift, load grids (override or cache, never analysing), `resolve_timing`, and return the app form plus `timingProblems` and `missingGrids`.
  - There is one funnel for every read of a `metadata.json`: read, `adoptExternalEdits`, library and templates.
  - If `missingGrids` is non-empty, `BeatAnalyzer` runs, stores the result, and re-resolves.
- **Re-resolve.** `PromoTiming.reconcile` (l.3107) changes in two ways:
  - It resolves through the document, using the new `promo_doc_resolved_json(doc, json, dir)`: keep the unread source (the sequence and generators Swift dropped), resolve, return the app form.
  - It copies back every field that has a `timeSpecs` entry, not only `timing`/`durationRule` layers, and replaces generated keyframes and layers by id.
  - Triggers: the existing call sites (`RecordingProject.swift:650`, `LayersManagementView.swift:3084`, `ProjectStore.swift:712` and `:1437`), plus a beatClock or beatGrid edit, a replaced audio file, and an analysis finishing.
  - Resolution marks nothing dirty.
- **Save.** `ProjectStore.save` (l.907) is unchanged in shape. Swift encodes the app form, and `document.savedJSON(from:)` runs `keep_unread`, then `fold`, then `floor_stamp`. `detachEditedLayers` keeps doing `timing`; `fold` handles musical values and `after`/sequence.
- **Engine.** `PromoCorePreview.applySnapshot` and both export paths encode the same app form: numbers plus generated keys. `from_json` lifts nothing, since the app form holds no objects. The three paths match by construction.

### 6.3 Minimal UI (shared views, adapted by size class, not platform)

- **Audio resource inspector** (`ResourcesView.swift`, `AudioResourceTrimView` l.3208), a "Beats" section:
  - status: Analysing…, or "128.0 BPM · 4/4 · confidence 0.94", or "Set by hand"
  - actions: Analyse again, Set BPM… (bpm, first downbeat, meter, which writes `beatGrid`), Use as beat clock
- **Composition settings:** a beat clock picker (None, or the project's audio resources), and a start that either follows its layer or is a number in seconds.
- **Timing rows** (`LayerEditSheet` start, duration and transition rows, `LayersManagementView.swift` l.3716–3769 and l.4700):
  - With a spec, a row shows the core's `label` plus the resolved seconds, for example "2 beats · 0.938 s".
  - Actions: "Use seconds" (flatten), "Length in beats…" (a stepper), "Snap to beat / bar".
  - Each action calls the editor command `SetTime`. The value comes from the new `promo_project_time_spec_for(json, dir, layerID, field, seconds, unit)`, so rounding is the core's.
- **Timeline** (`LayerTimelineRow`, `LaneTimelineView`):
  - Beat ticks, with bar numbers on the ruler, come from the `grids` the resolve output carries (output-timeline beat and downbeat lists).
  - With a clock, drags snap to those ticks. Option (⌥) suspends snapping. A drag still writes seconds, which flattens the value, as in §1.3.
  - Generated keyframes are drawn in the lane's "auto" style. The flash layer's row reads "Flash · on downbeat".
- **App MCP server** (`PromoShotMac/MCPTools.swift`): `promo_beats` runs `BeatAnalyzer` and returns the core JSON, a twin of the headless tool.

### 6.4 Swift tests (ReVoiceTests, both suites)

- `BeatSyncModelTests`:
  - `testMusicalFileOpensAndSavesUnflattened`: the saved file equals the source for every spec
  - `testEditedValueFlattensOnlyItself`
  - `testResolvedSecondsComeFromTheCore`: compares against `promo_project_opened`, with no Swift arithmetic
  - `testOpeningABeatProjectMarksNothingDirty`
  - `testSequenceAndGeneratorsSurviveAnInAppEdit`
  - `testGeneratedKeyframesNeverReachTheFile`
- `BeatAnalyzerTests`:
  - `testFixtureMatchesCoreGolden`: the WAV through AVAudioFile gives the committed golden JSON byte for byte
  - `testAnalysisWritesTheCoreCache`
- `DemoPreviewExportParityTests` and `CrossPathRenderTests` each gain a beat-slideshow case: app export versus CLI export, with cut frames identical.

---

## 7. validate, explain, inspect

**Findings.** These are prose, using the existing `layer "NAME" at Ts: …` prefix, as `Report::warn` or `Report::breaks`:

| Finding | Severity |
|---|---|
| `layer "s3" at 4.21s: starts 0.08 s off the beat grid — nearest is beat 17 at 4.13 s`. Checked for every visual layer start and end when a clock governs it, with a tolerance of half a frame. Audio, background and generated layers are skipped. | warn |
| `layer "s9": lasts 0.31 s, shorter than one beat (0.47 s at 128 BPM)` | warn |
| `layer "s1": duration {"beats":2} has no beat clock — resolved against 120 BPM from 0 s` | warn |
| `the beat clock's audio "song" ends at 28.4 s but the timeline runs to 30.0 s — the grid after 28.4 s is extrapolated` | warn |
| `beat detection for "song" is unsure (confidence 0.32) — set beatGrid by hand` | warn |
| `pulse on layer "s2" reshapes keyed viewport keyframes at beat resolution` | warn |
| `"song" has no beat grid yet` (only in an `--no-analyse` or app context) | warn |
| a beat clock naming a missing or non-audio resource | break |
| `fadeIn {"bar":3}` is a position where a length is required | break |
| an unknown `after` or sequence id, a duplicate sequence member, `after` with `Circular`, sequence and `after`/`timing` on one layer (who wins is stated) | break |
| a generator `range` naming an unknown section, or a `property` its kind cannot take | break |
| `timeSpecs` or `generatedBy` written in a file (internal keys) | warn |

**`promo validate`.** Text mode adds a block:

```
Musical times
  layers[s1].startTime   {"beat":1}   → 0.469 s
  layers[s1].duration    {"beats":2}  → 0.938 s
  layers[s2].startTime   sequence     → 1.406 s
```

`--json` and `promo_validate` add a `resolvedTimes` key, the same rows as
`{"at", "value", "seconds"}`, and a `beatClocks` key.

**`promo explain`** (`promo-author/src/lib.rs`, `layer_doc`).
- Each layer object gains `"times": {"startTime": {"value": {"beat":1}, "seconds": 0.469}, …}`, listing only the fields that are not plain seconds.
- `generated` counts the layer's beat keys.
- The explained layer is the resolved one.

**`promo inspect`** (`promo-cli/src/inspect.rs`).
- A `beats` section: for each clock, the resource, the source (`override`, `cache` or `detected now`), bpm, meter, offset, confidence, the beat/bar count, and sections.
- Each layer gains its musical fields in both forms, `{"beats":4} → 1.832 s`.
- A `credits` section (§8).

---

## 8. Provenance

```json
"provenance": {"tool": "ace-step", "model": "v1.5", "license": "Apache-2.0", "prompt": "…",
               "cost": 0, "currency": "USD", "createdAt": "2026-10-20T10:00:00Z", "url": "…"}
```

- It is allowed on any resource. All fields are optional, and unknown keys go to `extra`.
- It is ignorable, so it stamps rung 48.
- **`promo inspect`** shows it on each resource row. It adds `credits`: the resources the project uses (the inverse of `unused_resources`: nesting, swaps, slots and masks included), grouped by tool and license, with costs summed by currency.
- **Render summary** (the CLI's `video`, `still` and `frames` output, and therefore `promo_render_*`): a `credits` block listing only the resources that render reached in the rendered span. `--json` puts it under `"credits"`.
- **The apps:** they preserve it through `keep_unread`, with no UI in this scope. The Swift test `testProvenanceSurvivesASave` covers it.

---

## 9. Build order

Each slice compiles, passes `check-all.sh` (and both Xcode suites where Swift
is touched), and is committed on its own. AC*n* marks the request's
acceptance criteria:
1. 30 s slideshow
2. track swap
3. override bit-identical
4. seconds-only unchanged
5. `promo_beats`
6. schema docs
7. recipe

**S1 — Format and lossless plumbing** (core, core-private, macapp in lockstep)
- Core:
  - `musical.rs` with the types, `lift`, `fold`, `to_app_json` and `timeSpecs`
  - the new fields typed, with no behaviour yet
  - rung 49 and `READER_VERSION` = 49, with the app constants
  - `promo.schema.json` regenerated; the schema.md and schema-quick.md field entries; the editor commands clear specs, plus `SetTime`
- core-private: `promo_project_opened`, without grids yet; the nominal grid only.
- macapp:
  - the Swift fields of §6.1 and the read funnel
  - save through `fold`
- Tests:
  - `musical_values_round_trip_byte_for_byte`
  - `malformed_musical_value_names_its_path`
  - `edited_value_flattens_only_itself`
  - `headless_edit_clears_the_spec`
  - `musical_file_stamps_49`
  - `ignorable_beat_fields_stamp_48`
  - **AC4** `seconds_only_projects_are_unchanged`: over every repo fixture, demo and example, decode then encode is byte-identical, the rung is unchanged, and `resolve_timing` equals the old `resolve_attachments`
  - `unread_sweep` stays green
  - Swift: `testMusicalFileOpensAndSavesUnflattened`, `testEditedValueFlattensOnlyItself`

**Slice 1 as built** (2026-10-20) — where the implementation departs from
the text above, and why:
- **`timeSpecs` is in the machine schema.** `keep_unread` restores only keys
  the schema does not list; left out, a stale side-map entry from the
  absorbed file could come back over the apps' own copy. Listed, it is a
  key the apps read, which is the truth. Files never carry it (§7's
  warning still applies to one that does).
- **Only the file writers fold.** `to_json` folds; every FFI door that hands
  JSON to the apps' decoder answers in the app form: `promo_project_lifted`,
  `promo_project_lowered`, `promo_project_to_json`, `promo_doc_to_json`
  (`Document::to_json`, which is also every undo snapshot), and
  `promo_project_resolve_timing`, whose answer Swift decodes — so §1.3's
  "it changes" is reversed for that one. The folding writers are
  `Document::saved_json_from` (keep_unread, then fold, then floor_stamp)
  and `promo-author`'s `write_metadata` (through `to_json`).
- **Byte order.** A project with no side map encodes straight from the
  structs, byte-identical to every earlier build (AC4). One with a side map
  is folded as a `serde_json::Value`, so its keys come out sorted — the
  order a save already wrote whenever it put unread keys back.
- **Decode stays the old path for seconds-only files.** `from_json` tries
  serde on the text first and lifts only when that fails; `unread_keys`
  lifts the same way.
- **Placeholder seconds.** Until slice 2 resolves them, a lifted value has
  `seconds: 0` and a `0` at its field; `SetTime` with a musical value keeps
  the field's current number as `seconds` (0 for an absent field). Fold
  compares against `seconds`, so both stay lossless.
- **The Swift model carries only `timeSpecs` for now.** `beatClock` and
  `beatGrid` join the Swift model with their UI (S9); until then the core
  carries them, with `after`, `sequence`, the generators and
  `provenance`, as typed fields the apps do not read
  (`unread::carry_field`: Fill on the apps' save, Mirror on absorbing a
  file). `generatedBy` waits for the generators (S6).
- **`promo_project_opened`'s answer** is an envelope:
  `{"lifted", "project"?, "timingProblems", "missingGrids"}`, or
  `{"error"}` naming the malformed value's path. With `lifted: false` there
  is no `project` and the app keeps the file's own bytes. The Swift funnel
  is `ProjectStore.readable(_:in:)` (keyframe ramps, then the core): the
  open, the backup restore, `adoptExternalEdits`, the JSON editor, and the
  app's MCP validate, render and proxy tools. The app's validate compares
  the keys it "drops" against the lifted form, not the raw file.
- **Commands.** `UpsertKeyframe` replaces the whole keyframe, its
  `timeSpecs` included, so it needs no clearing; `UpdateLayer` drops the
  entries of the time fields a patch names, then lifts a musical value in
  the patch like a file's. `SetLayerTiming` and `promo-author`'s layer and
  keyframe upserts drop the entries they set.
- **Docs.** No schema.md or schema-quick.md entries yet: a musical value
  lays out at 0 until slice 2 and a generator does nothing until slice 6,
  and the docs teach what renders. The entries land with S2/S10; the
  kitchen-sink doc test leaves the new fields unset until then. schema.md's
  one version statement names no number, so it needed no bump.

**Slice 2 as built** (2026-10-20):
- **The grid is closed-form only** (`promo-timeline/src/beats.rs`): beat
  index k at `clock + (offset + k·60/bpm)/speed`, a bar = the time
  signature's numerator in beats, `snap` to the nearest beat or bar start
  with ties going earlier. The piecewise form for detected grids comes with
  the tracker (S4) behind the same methods. Exact indices are carried from a
  musical start to the lengths measured from it, so a stated grid's
  numbers are the closed form's bits (AC3, pinned as `to_bits` literals;
  run on macOS and in `rust:1.96-bookworm` under Docker — linux/arm64 on
  this machine, not x86_64).
- **`BeatGrids` is a map of `BeatGrid` by resource id** (what an analysis
  will fill in S4/S5); a resource's own `beatGrid` wins over it.
- **Edits settle first.** Before resolving, any spec whose field no longer
  holds its `seconds` is dropped — the save's fold rule, applied up front —
  so a Swift drag or a headless edit is never resolved back.
- **The loop** (§2.2 step 3) is starts, musical durations and keyframe
  times, then the attachment resolver, repeated until no number moves
  (layer count + 1 rounds). `sequence` and `after` join it in S3.
  Attachments still run at the top level only, as before; compositions get
  the musical pass with their own clock. Stage members resolve against
  their stage's scope.
- **A missing grid is a warning plus a refusal to render**, not a break:
  until S5 analyses audio there is no other way to get a grid, so validate
  says "has no beat grid yet" and the CLI's `still`, `frames`, `video` and
  `gif` refuse with the same advice. In the app, the editor's video, image
  and GIF exports and the app's MCP render tools refuse with a sentence
  (`PromoTiming.exportWaitsForGrids`) — the minimal form of S8's guard.
- **Problems** are `TimingProblem` (`is_break()`): no clock (warn, per
  value), a bad clock resource, a position where a length is required, the
  clock's own audio layer starting on its beat, a position as a duration
  that ends before the start (breaks).
- **The app's resolve carries what Swift cannot read.** Swift's encoding
  has no `beatClock` or `beatGrid`, so `promo_project_resolve_timing` took
  a second argument, `source` (the core document's JSON, else the file as
  opened), whose unread fields are carried in before resolving — in place
  of the design's `promo_doc_resolved_json(doc, json, dir)`.
  `PromoTiming.reconcile` copies back every field of a layer that carries
  `timeSpecs`, and runs when a layer does (top-level layers; members and
  composition layers get their seconds from `promo_project_opened`).
- **`promo_project_opened` resolves**, then copies only the time fields
  and side maps of the resolved project back into the lifted file
  (`musical::copy_times`), so keys below the bags survive the trip.
- **`explain` explains the resolved project**, as §2.1 said.
- **JSON text is not bit-exact across the FFI**: serde_json's default float
  parser can land one ulp off the shortest representation another encoder
  printed, so a number that crossed Swift's encoder is equal only within
  fold's 1e-4. Bit identity is a property of the core's own arithmetic.

**Slice 8 as built** (2026-10-20):
- **The FFI** is §3.6's seven functions (`promo-ffi/src/beats.rs`), plus
  `promo_doc_set_project_dir(doc, dir)`. `promo_project_opened` and
  `promo_project_resolve_timing` (which gained a `project_dir` argument)
  resolve on `BeatGrids::from_analyses(beat_cache::cached_analyses(dir))` —
  the CLI's cache reader — so an analysed song lays out on its own beats and
  its sections, and only a song never analysed reaches `missingGrids`.
- **No `promo_doc_resolved_json`.** The re-resolve stays
  `promo_project_resolve_timing(json, source, dir)` (S2's door, carrying
  the document's unread content); what the design wanted from a document
  door is the document's own grids, which it now has:
  `Document::set_beat_grids` stores them for every later `apply` and
  `apply_group` (they resolved on the stand-in before) and re-resolves at
  once — no step, no version, since resolving is not an edit.
- **Swift.** `RecordingProject.directoryURL` (in-memory, set by
  `ProjectStore.build` before the apply and by `projectDirectory(for:)`)
  is the folder every `PromoTiming` resolve and the core document read the
  cache under. `PromoCore.BeatMeter` wraps the meter, store and lookup;
  `BeatAnalyzer` decodes with `AVAudioFile` and pushes every channel
  interleaved — the core downmixes, as for the CLI's ffmpeg decode — off
  the main thread. `BeatAnalyzer.analyse(in:)` measures the clock songs in
  `missingGrids` (the editor runs it once when a project opens, and every
  export runs it before its guard: `gridsBeforeExport`, so only a song that
  cannot be measured holds an export), then reconciles the model and then
  points the document at the folder.
- **That order matters.** A flash the model holds still carries the
  document's tag; re-resolved the other way round, the document's tag is
  for the new beats and the model's flash reads as a person's edit, which
  retires the generator. For the same reason `PromoTiming.reconcile` now
  takes the layers and the flash from one resolve (S6 asked twice, the
  second time after the first had already moved the flash's keys home).
- **Fixture.** `core/fixtures/beats/click-120.wav` (16-bit 22.05 kHz mono,
  120 BPM, first downbeat 0.25 s, 353 KB) and `click-120.json`, the
  tracker's answer for that file: `promo-media`'s
  `committed_fixture_is_the_synthesizer_and_its_golden` holds both to the
  synthesizer, and the app's `testFixtureMatchesCoreGolden` reads the same
  file through AVFoundation and compares value for value.
- **Parity cases** live beside S6's in `BeatAnalyzerTests.swift`, not in
  `DemoPreviewExportParityTests`/`CrossPathRenderTests`: the preview and the
  image export agree on the analysed downbeats (both suites), and the
  app's video export matches the core's export of the same folder (Mac).
- **Left for later:** a library-referenced song is looked for under the
  project's `Resources/`, so it is never found in the cache;
  `promo_project_warnings` still resolves on the stand-in.

**Slice 7 as built** (2026-10-20):
- **Findings** come from the timing pass (`TimingProblem`): off-grid edges
  (start and end of every enabled visual layer a clock governs, skipping
  audio, backgrounds, generated layers and the clock's own audio layer;
  tolerance half a frame; the nearest beat numbered from 1), a layer
  shorter than one beat, the clock's song ending before the timeline
  (`clock + duration / speed`), an unsure analysis (`BeatGrids.confidence`
  under 0.5, only when no `beatGrid` overrides it) and a pulse over a keyed
  track — all warnings. `timeSpecs` or `generatedBy` found in a file is a
  warning from `file_report_with`, which sees the raw text.
- **`Report`** gained `resolved_times` and `beat_clocks` (`BeatClockInfo`:
  scope, resource, source `beatGrid`/`analysis`/`nominal`, bpm, meter,
  offset, clock); `text()` adds the "Musical times" block (a derived start
  or length shows its rule's name, `sequence` or `after`), `json()` the
  `resolvedTimes` and `beatClocks` keys.
- **explain** adds `times` (only fields stated musically, keyframes as
  `keyframes[<id>].<field>`) and `generated` (the layer's beat keys), and
  resolves on the song's cached analysis — it reads, never analyses.
- **The cache's naming moved to `promo_model::beat_cache`** (`source_of`
  with its memo, `cache_path`, `load_cached`, `store`, `clock_songs`,
  `cached_analyses`), re-exported from `promo_media::beats`: explain lives
  in `promo-author`, which the headless server links without
  `promo-media`'s GPU stack, and must find the same files. The core still
  owns the naming in one place; promo-model now does that file I/O.
- **inspect** adds `beats` (each clock: song, source — `override`,
  `cache`, `detected now`, or the 120 BPM stand-in — bpm, meter, offset,
  confidence, beat and bar counts, sections), each layer's `musical` times
  in both forms, `provenance` per resource and `credits`.
- **Credits** (`promo-cli/src/credits.rs`): resources a span reaches — the
  enabled layers overlapping it, the settings, then what those resources
  point at in turn — with provenance, grouped by tool and licence, costs
  summed per currency. `still`, `frames`, `video` and `gif` add a `credits`
  block (text) or key (JSON) when there is any; the whole project's are
  inspect's. The headless server's render tools pass the CLI's answer on.

**Slice 6 as built** (2026-10-20):
- **Expansion** is `promo-timeline/src/beat_effects.rs`, the last step of
  `resolve_timing` (after the fades, on the final numbers), and its
  settling the first step: unedited generated content goes before anything
  resolves, so a new grid or a moved layer re-times it, and a second pass
  changes nothing.
- **Signatures** (`musical::signature`) are 16 hex digits of FNV-1a over
  the entity's JSON with numbers quantised to 1e-5, leaving out `id`,
  `auto`, `timeSpecs`, `generatedBy` and `sortIndex` — so a value that went
  through Swift's encoder and back hashes the same. Keyframes are tagged
  `auto: "beat:<sig>"` (beside Auto zoom's own tags), the flash layer and
  drawing `generatedBy: "flash:<n>:<sig>"`. `musical::strip_generated` is
  the one rule, called by `fold` at save and by the settling step.
- **Ids**: `beat-<layer>-pulse<n>-<k>-{base,peak,rest}`,
  `beat-<layer>-shake<n>-<k>-{hit,back,settle,rest}`, and `beat-flash-<n>`
  for the flash layer and its drawing (a rect over the canvas). A pulse
  over keyed motion gets its `base` key one frame before the peak, ramping
  from the person's previous key on that track.
- **Shake** directions are one of eight, from a splitmix64 of the layer's
  id and the beat index — no trigonometry, no RNG crate.
- **Section ranges** read `BeatGrids.sections`, filled by the CLI's open
  from the analysis; in the apps (no cache read yet, S8) a section range is
  a break.
- **Flash** is the composition's only; `Composition.flash` stays parsed and
  not expanded. The design's choice of a deleted-flash test: a generated
  drawing whose layer is gone means the layer was deleted.
- **The apps** get generated content from `promo_project_opened` (which now
  resolves any file that needs the musical reader, not only one with
  musical values, and copies generated keyframes, layers and resources into
  the file's own form — `musical::copy_generated`). `PromoTiming.reconcile`
  replaces a layer's beat keyframes with the core's and, when the project
  shows a flash layer, its `beat-flash-` layers and drawings. Swift keeps no
  `generatedBy`; the core carries it by id (`unread::carry_field`), and an
  edited flash layer's mismatched signature makes it the person's at save.
- **Engine-input parity**: generated content is ordinary keyframes and an
  ordinary drawing layer, so no new engine input; `BeatEffectsTests` holds
  the preview and the image export to the same pixels on a downbeat and off
  it, and `BeatEffectsCrossPathTests` (Mac) the app's video export to the
  core's own export, which resolves the generators from the file itself.

**Slice 5 as built** (2026-10-20):
- **The CLI's open measures** (`Project::open_with` / `from_text_with`):
  every beat clock's song — the composition settings', a composition's, a
  layer's — that states no `beatGrid` gets its grid from the project's cache,
  else a fresh analysis that is stored there (or only used, when the folder
  cannot be written). `--no-analyse` reads the cache only. A render waits
  only for a song that could not be measured.
- **A song that cannot be measured is a break** in `validate` (no file in
  `Resources/`, a compressed format with no ffmpeg, no beat found), beside
  slice 2's "has no beat grid yet" warning. `report::file_report_with`
  takes the grids, so validate counts the beats a render counts.
- **The apps still read stated grids only**: `promo_project_opened` and
  the app's re-resolve do not read the cache yet (S8 brings the app's own
  analysis and the cache together), so a project the CLI analysed opens in
  the app on the nominal grid with "missing grid" until then.
- **The hash memo** is in `beats::source_of`: one hash per (path, size,
  modification time) per process, counted by `beats::files_hashed` for its
  test.
- **AC1 and AC2** render through the CLI's video path and read the frames
  back with ffmpeg (centre pixel of each frame, scaled to 16×9, nearest of
  sixteen colours). Every cut is within one frame of both the detected and
  the true beat. The 64 BPM song is 30 s of 32 beats. The first slide
  starts on beat 1 at 0.25 s, so the show runs to 30.25 s; the design's
  "32 beats = 30.0 s" holds for the length, not the end time.

**Slice 4 as built** (2026-10-20):
- **The tracker** is `promo-media/src/beats.rs` as §3.2 lays it out —
  `BeatMeter` fed in chunks, its own 64-tap Blackman-windowed sinc
  resampler (256 fractional phases, each normalised), its own radix-2 FFT,
  every transcendental from `libm`, sequential `f64` reductions — with
  these differences:
  - **Beats are placed on their attack in the waveform.** After the DP and
    its parabola, each beat moves within ±60 ms to the sample where the
    next 5 ms most outweigh the 20 ms before. A 23 ms frame and a log
    band's early rise put frame-level beats up to 2 ms off on the
    fixtures; on the attack they are within 0.7 ms, and the uniform fit
    runs on those.
  - **Edge trimming uses half the MEDIAN beat strength** (the smoothed
    score's peak within two frames), not half the RMS: accented downbeats
    made every plain beat after the last downbeat look weak.
  - **Section features are standardised over a floor** — 1 dB, 5 % bass
    share, 100 Hz centroid — so a feature that does not move cannot turn
    its noise into change, and a boundary keeps four bars from either end
    as well as from other boundaries (a song's last bar is not a section).
  - **Confidence** is `2 × (on / (on + off) − 0.5)`, clamped: the envelope
    on the beats against halfway between them.
- **The wire types** (`BeatAnalysis`, `BeatSource`, `BeatSection`) are in
  `promo_model::musical`; `BeatAnalysis::grid()` is the stated grid it
  implies. Nothing resolves on an analysis yet (S5 puts it in the load
  path); today its `bpm` and `offset` go into `beatGrid` by hand.
- **Fixtures are synthesized in the tests**, not committed: a noise click
  and a 180 Hz thump on every beat, a 55 Hz bass note on every downbeat,
  first downbeat 0.25 s, all `libm`. The loud section is on the 128 BPM
  track, bars 9–16 — the design put it on 64 BPM, but 30 s at 64 BPM is
  only eight bars. One golden is committed
  (`promo-media/tests/golden/click-128.json`) and matches on macOS and in
  `rust:1.96-bookworm` (linux/arm64), in debug and release.
- **The cache** is `Resources/.beats/<first 32 hex of SHA-256>.json`
  (`beats::{cache_path, load_cached, store, analysis_for}`), authoritative
  once written; `refresh` replaces it. A WAV is read directly; anything
  else goes through ffmpeg at its own rate and channels. No (path, size,
  mtime) memo yet.
- **`promo beats <audio | project> [--resource id] [--json] [--meter n]
  [--bpm-hint n] [--refresh]`**: on a project, the named audio resource,
  else the beat clock's, else the only one; `--json` adds `fromCache` in
  project mode.
- **`promo_beats` is headless only** (a `HOST_DIFFERENCES` entry: the app
  analyses through its own decoder in S8). The tool surface ceiling
  (`the_tool_surface_stays_small`) rose from 45,000 to 46,000 bytes for
  it. The skill teaches it (and the app's bundled copy follows).

**Slice 3 as built** (2026-10-20):
- **Derived entries.** A start the sequence or `after` placed, and a length
  the sequence set, carry `timeSpecs` entries with `rule` and no `spec`.
  They are re-derived every pass: resolution drops them first, and one
  whose number moved detaches its layer — the same rule `fold` applies at
  save (a moved start leaves the sequence or loses `after`; a resized
  sequenced layer's entry becomes `{"layer": id, "seconds": n}`), so a
  Swift drag is never resolved back. `musical::forget` keeps derived
  entries, so a command's new number is compared against them.
- **Commands detach outright.** `SetLayerTiming`, an `UpdateLayer` patch
  naming `startTime` (unless it also names the rule) and the author tools'
  layer upsert with a `startTime` drop `after` and the sequence membership
  directly: a document opened headless is never resolved first (opening
  edits nothing), so there may be no derived entry to compare against.
  The author upsert drops `after` only; sequence membership through that
  tool waits for the save's fold.
- **Who places a layer:** the sequence, then `after`, then a start anchor.
  The overruled `timing.start` sits out the attachment pass (held and
  restored around it), and each overrule is a break that says who won.
- **The first member's start** is its own: a musical start as written, or
  a number snapped to the nearest beat when a clock counts — re-snapped on
  every pass, not a derived entry, so moving the first member moves the
  whole sequence.
- **Cycles of `after`** are found up front (following each chain), not by
  the loop running out: every layer on the circle is named and keeps its
  stored start; a layer following one of them still resolves.
- **A member with no length** keeps its own duration; one that never ends
  stops the sequence there, and the next member is named.
- **An entry's length wins over the layer's own** `duration`, musical or
  not, and the file then holds the sequence's number there (as a derived
  start does). To keep a layer's own musical length, list it with no
  length: `{"layer": "s4"}`.
- **The apps re-resolve every scope** (slice-2 gap closed):
  `PromoTiming.reconcile` copies the core's answer into stage members and
  composition layers too, and runs when any of them carries `timeSpecs`.

**S2 — Grid and resolution pass (override grids only)**
- `promo-timeline/src/beats.rs` (`Grid`), `musical.rs` `resolve_timing` (steps 1–4 and 6), `resolve_attachments` made `pub(crate)`, and every caller moved.
- Swift `PromoTiming.reconcile` copies `timeSpecs` fields.
- Tests:
  - **AC3** `override_grid_is_bit_identical`: a golden file of `f64::to_bits`, run on mac and in the Linux Docker image
  - `beats_and_bars_measure_along_the_grid`
  - `absolute_positions_are_one_based_with_pickups`
  - `snap_picks_the_nearest_and_the_earlier_on_a_tie`
  - `lengths_reject_positions`
  - `clock_follows_its_audio_layer_trim_and_speed`
  - `nested_composition_uses_its_own_clock`
  - `no_beat_clock_uses_the_nominal_grid_and_warns`
  - `resolve_timing_is_idempotent`
  - `musical_lengths_and_attachments_compose`
  - `every_loader_gives_the_same_numbers` (CLI, `file_report`, explain and FFI agree)
  - Swift: `testResolvedSecondsComeFromTheCore`, `testOpeningABeatProjectMarksNothingDirty`

**S3 — `sequence` and `after`**
- Tests:
  - `sequence_lays_layers_back_to_back`
  - `sequence_seconds_cuts_snap_to_the_grid`
  - `after_names_a_layer_by_id_across_the_stack`
  - `after_can_follow_an_attached_layer`
  - `after_cycle_is_reported_and_keeps_numbers`
  - `sequence_wins_over_after_and_timing_and_says_so`
  - `moved_sequenced_layer_leaves_the_sequence_on_save`
  - `resized_sequenced_layer_rewrites_its_entry`
  - Swift: `testSequenceAndGeneratorsSurviveAnInAppEdit` (sequence half)

**S4 — Tracker, cache, CLI and MCP**
- `promo-media/src/beats.rs`; `fixtures/beats/` with `click-64-4x4.wav`, `click-96-4x4.wav` and `click-128-4x4.wav`, 30 s each (the 64 BPM file's bars 9–16 are louder, for sections), plus the golden analyses.
- The fixtures are synthesized by `tests/support/click_track.rs`: a kick on every beat, a bass note on each downbeat, and no subdivisions, which avoids octave ambiguity at 64 BPM.
- `promo beats <audio|project> [--resource id] [--json] [--meter n] [--bpm-hint n] [--refresh]` in `promo-cli/src/main.rs`; `promo_beats` in `promoshot-mcp/src/main.rs`, beside `promo_media_silences`, shelling out to the CLI.
- Tests:
  - `committed_fixtures_match_the_synthesizer`
  - `tracker_finds_fixture_tempo_and_beats`: bpm within 0.05, every beat within 5 ms of the truth
  - `tracker_finds_downbeats`
  - `tracker_finds_the_loud_section_at_bar_9`
  - `chunked_push_equals_one_push`
  - `tracker_matches_its_golden`, on mac and Linux
  - `cache_is_keyed_by_content_not_name`
  - `cache_is_never_silently_reanalysed`
  - `promo_beats_json_shape`
  - **AC5** `tools_list_includes_promo_beats` and `promo_beats_returns_the_analysis`

**S5 — Analysis in the load path, plus the two headline criteria**
- `Project::from_text` builds grids (override, then cache, then analyse and store).
- Tests:
  - **AC1** `beat_cut_slideshow_cuts_land_on_beats`: 16 distinctly coloured stills, `sequence` of `{"beats":2}` on the 64 BPM fixture (32 beats = 30.0 s). The video is rendered through the CLI, and the cut frames are found from pixel colour, an independent oracle. Each is within ±1 frame at 30 fps of both the detected and the true beats.
  - **AC2** `swapping_the_track_retimes_the_edit`: only the resource's `filename` changes, to the 96 BPM fixture. Every cut lands on the new beats, and the JSON is otherwise byte-identical.
  - `missing_ffmpeg_reports_a_break`

**S6 — Generators**
- `promo-timeline/src/beat_effects.rs` (step 5) and `fold` stripping.
- Tests:
  - `pulse_expands_to_tagged_viewport_keys`
  - `pulse_over_a_keyed_track_follows_the_base`
  - `opacity_and_placement_pulses`
  - `shake_is_seeded_and_deterministic`
  - `flash_brightens_each_downbeat_frame` (a rendered pixel oracle)
  - `range_limits_by_bar_section_and_span`
  - `edited_generated_key_becomes_the_persons`
  - `deleted_flash_layer_removes_its_generator`
  - `generated_content_is_not_saved`
  - `generators_retime_with_the_grid`
  - Swift: `testGeneratedKeyframesNeverReachTheFile`

**S7 — validate, explain, inspect and provenance**
- Tests:
  - `off_grid_cut_names_the_nearest_beat`
  - `short_slide_warning`
  - `no_clock_warning`
  - `audio_shorter_than_timeline_warning`
  - `low_confidence_warning`
  - `validate_lists_resolved_times`
  - `explain_shows_both_forms`
  - `inspect_lists_clocks_grids_and_credits`
  - `provenance_round_trips_and_stamps_48`
  - `render_credits_list_only_used_resources`
  - Swift: `testProvenanceSurvivesASave`

**S8 — App analysis over FFI**
- `promo-ffi/src/beats.rs`, regenerated bindings, `BeatAnalyzer.swift`, `promo_doc_resolved_json`, re-resolution on finish, and the export guard on `missingGrids`.
- Tests:
  - Rust `ffi_meter_matches_cli_analysis`: the same PCM through both doors gives identical JSON
  - `bindings` up to date
  - Swift: `testFixtureMatchesCoreGolden`, `testAnalysisWritesTheCoreCache`
  - `DemoPreviewExportParityTests` and `CrossPathRenderTests` beat cases, Mac and iOS

**S9 — App UI**
- The audio inspector section, the beat clock picker, timing rows with the core label, Set length in beats, Snap, ruler ticks and drag snapping.
- Tests:
  - `testSetLengthInBeatsWritesAMusicalValue`
  - `testSnapUsesTheCoresSpec`
  - `testTimingRowShowsTheCoresLabel`
  - both suites green
  - checked in the simulator at phone width and on iPad

**S10 — Recipe and agent docs**
- schema.md gets a "Beats — beats, bars, beatClock, beatGrid, sequence, after, pulse, flash, shake, snap" section, with a cross-reference from Timing, and its header goes to 49.
- schema-quick.md gets a fifth complete recipe, "beat-cut stills slideshow" (a clock, `sequence` of `{"beats":2}`, a downbeat zoom `pulse`, an optional `flash` on the drop).
- SKILL.md gets `promo_beats` and the recipe.
- `promo_slideshow` gains `music` and `beatsPerSlide`, writing the same shape.
- Tests:
  - **AC6** `schema_doc_tests` cover every new key
  - **AC7** `beat_cut_recipe_validates_and_renders`: the recipe copied verbatim validates with no break and renders on the fixture

---

## 10. Risks

1. **Real music is not a click track.** Octave errors, swing, rubato and long beatless intros are the risks. Mitigations: the tempo prior, `bpm_hint`, confidence warnings and the `beatGrid` override. The tests prove determinism and fixtures, not musical taste. Before S5 ships, check real songs by hand (Suno, ACE-Step, a live recording).
2. **The two hosts decode audio differently.** For MP3/AAC, AVFoundation and ffmpeg can handle priming and encoder delay differently by a few ms to tens of ms, so the two hosts can detect slightly different grids. The package cache makes the first analysis everyone's answer. Add an m4a cross-check test with a one-hop tolerance.
3. **A Swift read that bypasses the funnel.** Any path that decodes `metadata.json` directly will call a musical file "damaged". Put a single `ProjectStore.decodeMetadata(fileJSON:dir:)` and a source test that greps for other `JSONDecoder().decode(ProjectMetadata` calls.
4. **Swift loses `timeSpecs`** on a copy, paste or rebuild path that constructs a fresh `ProjectLayer`. The musical value is then silently flattened at save. Cover it with model tests per path (duplicate, paste, undo, stage fold/lift) and `parity-comment-without-test` discipline: no Swift comment claims the rule without a test.
5. **Repeated attachment resolution.** The fixpoint relies on `resolve_attachments` (waits included) being idempotent. Pin it in S2 before the loop leans on it.
6. **Pulse over keyed tracks** is approximated at beat resolution. It is acceptable for punches, and validate says so. If it bites, a later engine modifier can replace the expansion without a format change.
7. **Generated-keyframe volume.** A 3-minute song with a pulse on every beat is about 800 keys. That is fine for the engine, but the lanes need the auto style and should collapse it.
8. **Dot-folder in Resources.** The app's resource scanners, iCloud and Files sync, and the inventory all must skip `Resources/.beats`. Audit them in S4.
9. **The clock's own audio layer** with a musical `startTime` would be circular. It is refused, as a break. A looping music layer (`beyondEnd: loop`) gets an extrapolated grid in v1, not a repeated one, with a warning.
10. **Rung 49 locks out shipped apps** (3.2 and older) for files that use the new forms. This is accepted by the owner. Seconds-only files are unaffected (AC4).
11. **Nominal fallback grid.** A project validated before its audio is analysed lays out at 120 BPM and then re-times. That is visible, but it is always reported, never silent.
