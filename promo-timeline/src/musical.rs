//! The one timing pass (docs/BEAT-SYNC.md §2): musical time values and
//! attachments resolved into plain numbers, in one entry point every loader
//! calls — the CLI's open, `validate`, `explain`, the editor document and
//! the FFI. Resolve once, write numbers, and leave nothing for a renderer to
//! interpret: the engine never learns that a beat exists.
//!
//! The order, per scope (the project's layers with their stage members,
//! then each composition's):
//! 1. clocks — each layer's governing grid: its own `beatClock`, else the
//!    scope's, else the nominal 120 BPM grid;
//! 2. absolute starts (`beat`, `bar`, `snap`, and `beats`/`bars` counted
//!    from the clock's beat 1);
//! 3. a bounded loop of musical durations and keyframe times (they need the
//!    start) and the attachment resolver on the numbers, until a round
//!    changes nothing — so an attached layer can carry a musical length and
//!    a musical layer can anchor an attachment (`sequence` and `after` join
//!    this loop in slice 3);
//! 4. lengths measured from an edge: fades, transition durations and
//!    keyframe ramps;
//! 6. every `timeSpecs` entry's `seconds` is the number written.
//!
//! Before any of it, a value someone edited is settled the way a save would
//! settle it: a spec whose field no longer holds its resolved seconds is
//! dropped and the number stands. Resolving a resolved project changes
//! nothing.

use std::collections::BTreeMap;

use promo_model::musical::{self, BeatClock, BeatGrid, MusicalTime, TimeSpecs};
use promo_model::{
    ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, ProjectResourceKind,
};

use crate::attachment::AttachmentProblem;
use crate::beats::Grid;

/// Grids from outside the file — analyses of the clocks' audio (slice 4),
/// by resource id. A resource's own `beatGrid` wins over these.
#[derive(Debug, Clone, Default)]
pub struct BeatGrids {
    pub by_resource: BTreeMap<String, BeatGrid>,
}

/// A musical value and the seconds it resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTime {
    /// Where it is: `layers[s1].duration`, `layers[s1].keyframes[k1].time`.
    pub at: String,
    /// The value as written: `{"beats":2}`.
    pub spec: String,
    pub seconds: f64,
}

/// Why timing could not be worked out as written — each one names the layer.
#[derive(Debug, Clone, PartialEq)]
pub enum TimingProblem {
    Attachment(AttachmentProblem),
    /// A musical value with no beat clock over it: resolved against the
    /// nominal grid (120 BPM, beat 1 at 0 s).
    NoBeatClock {
        layer: String,
        field: String,
        spec: String,
    },
    /// The clock's audio has no grid yet — no `beatGrid` and no analysis:
    /// laid out on the nominal grid, and an export waits for one.
    MissingGrid {
        resource: String,
    },
    /// The clock names a resource the project lacks, or one that is not
    /// audio.
    BadClock {
        resource: String,
        why: String,
    },
    /// A position (`beat`, `bar`, `snap`) where a length is required.
    PositionForLength {
        layer: String,
        field: String,
        spec: String,
    },
    /// The layer that places the clock's audio has a musical start: the
    /// clock would wait on itself. Its stored start places the clock.
    ClockOnItsOwnBeat {
        layer: String,
    },
    /// A musical end at or before the layer's start.
    EndsBeforeItStarts {
        layer: String,
        spec: String,
    },
}

impl TimingProblem {
    /// Whether the file renders something other than it says.
    pub fn is_break(&self) -> bool {
        matches!(
            self,
            TimingProblem::BadClock { .. }
                | TimingProblem::PositionForLength { .. }
                | TimingProblem::ClockOnItsOwnBeat { .. }
                | TimingProblem::EndsBeforeItStarts { .. }
        )
    }
}

impl std::fmt::Display for TimingProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Attachment(problem) => problem.fmt(f),
            Self::NoBeatClock { layer, field, spec } => write!(
                f,
                "layer \"{layer}\": {field} {spec} has no beat clock — resolved against \
                 120 BPM from 0 s; set compositionSettings.beatClock to the music"
            ),
            Self::MissingGrid { resource } => write!(
                f,
                "the beat clock's audio \"{resource}\" has no beat grid yet — laid out at \
                 120 BPM from 0 s, and an export waits for a grid; set \"beatGrid\": \
                 {{\"bpm\": …}} on the resource"
            ),
            Self::BadClock { resource, why } => {
                write!(f, "the beat clock names \"{resource}\", {why}")
            }
            Self::PositionForLength { layer, field, spec } => write!(
                f,
                "layer \"{layer}\": {field} {spec} is a position where a length is \
                 required — use beats or bars; the stored seconds stand"
            ),
            Self::ClockOnItsOwnBeat { layer } => write!(
                f,
                "layer \"{layer}\" plays the beat clock's audio and starts on a beat of \
                 that same clock — give it a start in seconds"
            ),
            Self::EndsBeforeItStarts { layer, spec } => write!(
                f,
                "layer \"{layer}\": duration {spec} ends at or before its start — the \
                 stored seconds stand"
            ),
        }
    }
}

/// What the pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingReport {
    pub problems: Vec<TimingProblem>,
    pub resolved: Vec<ResolvedTime>,
    /// Clock resources with no grid: the nominal grid stood in.
    pub missing_grids: Vec<String>,
}

/// Every musical value and attachment in `meta`, resolved to seconds in
/// place. Never refuses: what cannot be honoured keeps its stored number
/// and is named in the report.
pub fn resolve_timing(meta: &mut ProjectMetadata, grids: &BeatGrids) -> TimingReport {
    let mut report = TimingReport::default();
    let resources = meta.resources.clone().unwrap_or_default();

    // A value someone edited stands as seconds, as a save would fold it.
    settle_edits(meta.layers.as_deref_mut().unwrap_or(&mut []));
    for resource in meta.resources.iter_mut().flatten() {
        if let Some(composition) = resource.composition.as_mut() {
            settle_edits(&mut composition.layers);
        }
    }

    // The project's scope: the musical pass and the attachments, until
    // neither moves anything. Each round can only settle what the last one
    // made known, so the layer count plus one bounds it.
    let clock = meta.composition_settings.beat_clock.clone();
    let rounds = meta.layers.as_ref().map_or(0, Vec::len) + 1;
    let mut attachment_problems = Vec::new();
    for _ in 0..rounds {
        let before = numbers(meta.layers.as_deref().unwrap_or(&[]));
        let scope = Scope::new(
            clock.as_ref(),
            meta.layers.as_deref().unwrap_or(&[]),
            &resources,
            grids,
        );
        positions_and_lengths(meta.layers.as_deref_mut().unwrap_or(&mut []), &scope);
        attachment_problems = crate::attachment::resolve_attachments(meta);
        if numbers(meta.layers.as_deref().unwrap_or(&[])) == before {
            break;
        }
    }
    let scope = Scope::new(
        clock.as_ref(),
        meta.layers.as_deref().unwrap_or(&[]),
        &resources,
        grids,
    );
    finish(
        meta.layers.as_deref_mut().unwrap_or(&mut []),
        &scope,
        "layers",
        &mut report,
    );
    report.problems.extend(
        attachment_problems
            .into_iter()
            .map(TimingProblem::Attachment),
    );

    // Each composition: its own clock, never its parent's — it may be
    // placed at several times. No attachments run inside (as before).
    for resource in meta.resources.iter_mut().flatten() {
        let id = resource.id.clone();
        let Some(composition) = resource.composition.as_mut() else {
            continue;
        };
        let clock = composition.beat_clock.clone();
        let scope = Scope::new(clock.as_ref(), &composition.layers, &resources, grids);
        positions_and_lengths(&mut composition.layers, &scope);
        let scope = Scope::new(clock.as_ref(), &composition.layers, &resources, grids);
        finish(
            &mut composition.layers,
            &scope,
            &format!("resources[{id}].composition.layers"),
            &mut report,
        );
    }
    report.missing_grids.sort();
    report.missing_grids.dedup();
    report
}

/// Drops every spec whose field no longer holds its resolved seconds.
fn settle_edits(layers: &mut [ProjectLayer]) {
    for layer in layers {
        let numbers: Vec<(String, Option<f64>)> = layer
            .time_specs
            .iter()
            .flatten()
            .map(|(key, _)| (key.clone(), musical::layer_number(layer, key)))
            .collect();
        retain_held(&mut layer.time_specs, &numbers);
        for keyframe in &mut layer.keyframes {
            let numbers: Vec<(String, Option<f64>)> = keyframe
                .time_specs
                .iter()
                .flatten()
                .map(|(key, _)| (key.clone(), musical::keyframe_number(keyframe, key)))
                .collect();
            retain_held(&mut keyframe.time_specs, &numbers);
        }
        if let Some(members) = layer.members.as_mut() {
            settle_edits(members);
        }
    }
}

fn retain_held(specs: &mut Option<TimeSpecs>, numbers: &[(String, Option<f64>)]) {
    let Some(map) = specs.as_mut() else { return };
    for (key, number) in numbers {
        if map.get(key).is_some_and(|entry| !entry.holds(*number)) {
            map.remove(key);
        }
    }
    if map.is_empty() {
        *specs = None;
    }
}

/// Every time number of the layers, for "did this round move anything".
fn numbers(layers: &[ProjectLayer]) -> Vec<u64> {
    let mut out = Vec::new();
    for layer in layers {
        out.push(layer.start_time.to_bits());
        out.push(layer.duration.map_or(u64::MAX, f64::to_bits));
        out.extend(layer.keyframes.iter().map(|k| k.time.to_bits()));
        if let Some(members) = layer.members.as_deref() {
            out.extend(numbers(members));
        }
    }
    out
}

/// The clock of one scope: the grid its layers resolve against, and why —
/// worked out from the layers as they stand, owning what it found.
struct Scope {
    grid: Grid,
    /// No clock at all: every musical value gets a warning.
    clockless: bool,
    problems: Vec<TimingProblem>,
    missing: Vec<String>,
    /// The layer that places the clock's audio, by id.
    clock_layer: Option<String>,
    /// Layers with a `beatClock` of their own, by id.
    overrides: BTreeMap<String, Grid>,
}

impl Scope {
    fn new(
        clock: Option<&BeatClock>,
        layers: &[ProjectLayer],
        resources: &[ProjectResource],
        grids: &BeatGrids,
    ) -> Scope {
        let mut scope = Scope {
            grid: Grid::nominal(),
            clockless: clock.is_none(),
            problems: Vec::new(),
            missing: Vec::new(),
            clock_layer: None,
            overrides: BTreeMap::new(),
        };
        if let Some(clock) = clock {
            let (grid, layer) = scope.grid_for(clock, layers, resources, grids);
            scope.grid = grid;
            scope.clock_layer = layer;
        }
        fn walk(
            scope: &mut Scope,
            list: &[ProjectLayer],
            layers: &[ProjectLayer],
            resources: &[ProjectResource],
            grids: &BeatGrids,
        ) {
            for layer in list {
                if let Some(clock) = layer.beat_clock.as_ref() {
                    let (grid, _) = scope.grid_for(clock, layers, resources, grids);
                    scope.overrides.insert(layer.id.clone(), grid);
                }
                if let Some(members) = layer.members.as_deref() {
                    walk(scope, members, layers, resources, grids);
                }
            }
        }
        walk(&mut scope, layers, layers, resources, grids);
        scope
    }

    /// The grid a clock stands for, and the layer that places it.
    fn grid_for(
        &mut self,
        clock: &BeatClock,
        layers: &[ProjectLayer],
        resources: &[ProjectResource],
        grids: &BeatGrids,
    ) -> (Grid, Option<String>) {
        let Some(id) = clock.resource_id.as_deref() else {
            self.problems.push(TimingProblem::BadClock {
                resource: String::new(),
                why: "no resourceID — it needs the audio whose beats it counts".into(),
            });
            return (Grid::nominal(), None);
        };
        let Some(resource) = resources.iter().find(|r| r.id == id) else {
            self.problems.push(TimingProblem::BadClock {
                resource: id.into(),
                why: "which the project has no resource of".into(),
            });
            return (Grid::nominal(), None);
        };
        if resource.kind != ProjectResourceKind::Audio {
            self.problems.push(TimingProblem::BadClock {
                resource: id.into(),
                why: "which is not an audio resource".into(),
            });
            return (Grid::nominal(), None);
        }
        // Where the audio's source second 0 lands on the output timeline:
        // stated, or the first enabled audio layer that plays it (the
        // inverse of `mapping::source_time_for_layer`).
        let speed = crate::mapping::effective_speed(resource);
        let mut placed_by = None;
        let origin = match clock.start_time {
            Some(start) if start.is_finite() => start,
            _ => {
                let mut players: Vec<&ProjectLayer> = layers
                    .iter()
                    .filter(|l| {
                        l.kind == ProjectLayerKind::Audio
                            && l.is_enabled
                            && l.resource_id.as_deref() == Some(id)
                    })
                    .collect();
                players.sort_by_key(|l| l.sort_index);
                match players.first() {
                    Some(layer) => {
                        placed_by = Some(layer.id.clone());
                        let material = crate::mapping::resource_for_cut(
                            resource,
                            layer.media_cut_id.as_deref(),
                        );
                        let first = crate::mapping::source_time_for_layer(&material, 0.0, None)
                            .unwrap_or(0.0);
                        layer.start_time - first / speed
                    }
                    None => 0.0,
                }
            }
        };
        let stated = resource
            .beat_grid
            .as_ref()
            .or_else(|| grids.by_resource.get(id));
        match stated {
            Some(grid) => (Grid::stated(grid, origin, speed), placed_by),
            None => {
                self.missing.push(id.into());
                (Grid::nominal(), placed_by)
            }
        }
    }

    /// The grid one layer resolves against: its own clock, or the scope's.
    fn grid_of(&self, layer: &ProjectLayer) -> Grid {
        self.overrides.get(&layer.id).copied().unwrap_or(self.grid)
    }

    /// Whether this layer resolves against no clock at all.
    fn clockless_for(&self, layer: &ProjectLayer) -> bool {
        self.clockless && !self.overrides.contains_key(&layer.id)
    }

    /// Whether this layer places the clock's audio — its start is then the
    /// clock's, never a beat of it.
    fn places_clock(&self, layer: &ProjectLayer) -> bool {
        self.clock_layer.as_deref() == Some(layer.id.as_str())
    }

    /// The same clock for a stage's members, with nothing to report twice.
    fn for_members(&self) -> Scope {
        Scope {
            grid: self.grid,
            clockless: self.clockless,
            problems: Vec::new(),
            missing: Vec::new(),
            clock_layer: self.clock_layer.clone(),
            overrides: self.overrides.clone(),
        }
    }
}

fn spec_of(specs: &Option<TimeSpecs>, key: &str) -> Option<MusicalTime> {
    specs.as_ref()?.get(key)?.value()
}

/// The beat index a layer starts at: exact when its start is a musical
/// position, else read off the grid.
fn start_index(layer: &ProjectLayer, grid: &Grid, scope: &Scope) -> f64 {
    match spec_of(&layer.time_specs, "startTime").filter(|_| !scope.places_clock(layer)) {
        Some(value) => grid.position(&value),
        None => grid.index_at(layer.start_time),
    }
}

/// The beat index a layer ends at, when it ends.
fn end_index(layer: &ProjectLayer, grid: &Grid, scope: &Scope) -> Option<f64> {
    match spec_of(&layer.time_specs, "duration") {
        Some(value) => Some(match grid.length(&value) {
            Some(beats) => start_index(layer, grid, scope) + beats,
            None => grid.position(&value),
        }),
        None => layer.duration.map(|d| grid.index_at(layer.start_time + d)),
    }
}

/// Writes `seconds` at a layer's field and its entry.
fn write_layer(layer: &mut ProjectLayer, key: &str, seconds: f64) {
    if musical::set_layer_number(layer, key, seconds) {
        if let Some(entry) = layer.time_specs.as_mut().and_then(|m| m.get_mut(key)) {
            entry.seconds = seconds;
        }
    }
}

/// Steps 2 and 3b: starts, durations and keyframe times, for every layer of
/// a scope (stage members included — they keep the project clock).
fn positions_and_lengths(layers: &mut [ProjectLayer], scope: &Scope) {
    for layer in layers.iter_mut() {
        let grid = scope.grid_of(layer);
        if let Some(value) = spec_of(&layer.time_specs, "startTime") {
            if !scope.places_clock(layer) {
                write_layer(layer, "startTime", grid.time_at(grid.position(&value)));
            }
        }
        let first = start_index(layer, &grid, scope);
        if let Some(value) = spec_of(&layer.time_specs, "duration") {
            let end = match grid.length(&value) {
                Some(beats) => grid.time_at(first + beats),
                None => grid.time_at(grid.position(&value)),
            };
            let length = end - layer.start_time;
            if length > 0.0 {
                write_layer(layer, "duration", length);
            }
        }
        let start = layer.start_time;
        for keyframe in &mut layer.keyframes {
            let Some(value) = spec_of(&keyframe.time_specs, "time") else {
                continue;
            };
            let at = match grid.length(&value) {
                Some(beats) => grid.time_at(first + beats),
                None => grid.time_at(grid.position(&value)),
            };
            let local = at - start;
            musical::set_keyframe_number(keyframe, "time", local);
            if let Some(entry) = keyframe.time_specs.as_mut().and_then(|m| m.get_mut("time")) {
                entry.seconds = local;
            }
        }
        if let Some(members) = layer.members.as_mut() {
            positions_and_lengths(members, scope);
        }
    }
}

/// Steps 4 and 6, and the report: lengths measured from an edge, then every
/// value listed with the seconds it resolved to.
fn finish(layers: &mut [ProjectLayer], scope: &Scope, at: &str, report: &mut TimingReport) {
    report.problems.extend(scope.problems.iter().cloned());
    report.missing_grids.extend(scope.missing.iter().cloned());
    for layer in layers.iter_mut() {
        let here = format!("{at}[{}]", layer.id);
        let grid = scope.grid_of(layer);
        let clockless = scope.clockless_for(layer);
        if scope.places_clock(layer) && spec_of(&layer.time_specs, "startTime").is_some() {
            report.problems.push(TimingProblem::ClockOnItsOwnBeat {
                layer: layer.name.clone(),
            });
        }
        // A position given as a duration must end after the start.
        if let Some(value) = spec_of(&layer.time_specs, "duration") {
            if grid.length(&value).is_none() {
                let end = grid.time_at(grid.position(&value));
                if end <= layer.start_time {
                    report.problems.push(TimingProblem::EndsBeforeItStarts {
                        layer: layer.name.clone(),
                        spec: value.canonical_text(),
                    });
                }
            }
        }
        let first = start_index(layer, &grid, scope);
        let last = end_index(layer, &grid, scope);
        let start = layer.start_time;
        let end = layer.duration.map(|d| start + d);
        for key in [
            "fadeIn",
            "transitionIn.duration",
            "fadeOut",
            "transitionOut.duration",
        ] {
            let Some(value) = spec_of(&layer.time_specs, key) else {
                continue;
            };
            let Some(beats) = grid.length(&value) else {
                report.problems.push(TimingProblem::PositionForLength {
                    layer: layer.name.clone(),
                    field: key.into(),
                    spec: value.canonical_text(),
                });
                continue;
            };
            let seconds = if key.contains("In") {
                grid.time_at(first + beats) - start
            } else {
                // Backward from the end; an open-ended layer has none.
                let (Some(last), Some(end)) = (last, end) else {
                    continue;
                };
                end - grid.time_at(last - beats)
            };
            write_layer(layer, key, seconds);
        }
        for keyframe in &mut layer.keyframes {
            let Some(value) = spec_of(&keyframe.time_specs, "transitionDuration") else {
                continue;
            };
            let Some(beats) = grid.length(&value) else {
                report.problems.push(TimingProblem::PositionForLength {
                    layer: layer.name.clone(),
                    field: format!("keyframes[{}].transitionDuration", keyframe.id),
                    spec: value.canonical_text(),
                });
                continue;
            };
            let moment = start + keyframe.time;
            let index = match spec_of(&keyframe.time_specs, "time") {
                Some(time) => match grid.length(&time) {
                    Some(n) => first + n,
                    None => grid.position(&time),
                },
                None => grid.index_at(moment),
            };
            let ramp = moment - grid.time_at(index - beats);
            musical::set_keyframe_number(keyframe, "transitionDuration", ramp);
            if let Some(entry) = keyframe
                .time_specs
                .as_mut()
                .and_then(|m| m.get_mut("transitionDuration"))
            {
                entry.seconds = ramp;
            }
        }
        // The report: every value as written and as resolved.
        for (key, entry) in layer.time_specs.iter().flatten() {
            let Some(spec) = entry.spec.clone() else {
                continue;
            };
            if clockless {
                report.problems.push(TimingProblem::NoBeatClock {
                    layer: layer.name.clone(),
                    field: key.clone(),
                    spec: spec.clone(),
                });
            }
            report.resolved.push(ResolvedTime {
                at: format!("{here}.{key}"),
                spec,
                seconds: musical::layer_number(layer, key).unwrap_or(entry.seconds),
            });
        }
        for keyframe in &layer.keyframes {
            for (key, entry) in keyframe.time_specs.iter().flatten() {
                let Some(spec) = entry.spec.clone() else {
                    continue;
                };
                if clockless {
                    report.problems.push(TimingProblem::NoBeatClock {
                        layer: layer.name.clone(),
                        field: format!("keyframes[{}].{key}", keyframe.id),
                        spec: spec.clone(),
                    });
                }
                report.resolved.push(ResolvedTime {
                    at: format!("{here}.keyframes[{}].{key}", keyframe.id),
                    spec,
                    seconds: musical::keyframe_number(keyframe, key).unwrap_or(entry.seconds),
                });
            }
        }
        if let Some(members) = layer.members.as_mut() {
            finish(
                members,
                &scope.for_members(),
                &format!("{here}.members"),
                report,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn project_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name != "target" && name != "runs" && !name.starts_with('.') {
                    project_files(&path, out);
                }
            } else if name.ends_with(".json") {
                out.push(path);
            }
        }
    }

    /// Every seconds-only project in the repository — fixtures, demos,
    /// examples, recipes — resolves through the one pass exactly as the
    /// attachment resolver alone resolved it: the same numbers, bit for
    /// bit, and the same problems.
    #[test]
    fn resolve_timing_equals_resolve_attachments_on_seconds_only_projects() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        project_files(&root, &mut files);
        let mut texts: Vec<(String, String)> = files
            .iter()
            .filter_map(|f| Some((f.display().to_string(), std::fs::read_to_string(f).ok()?)))
            .collect();
        texts.extend(
            promo_model::quick_schema_recipes()
                .into_iter()
                .enumerate()
                .map(|(i, r)| (format!("recipe {i}"), r.to_string())),
        );
        let mut checked = 0;
        let mut attached = 0;
        for (name, text) in &texts {
            let Ok(meta) = ProjectMetadata::from_json(text) else {
                continue;
            };
            assert!(!musical::carries_specs(&meta), "{name} is not seconds-only");
            checked += 1;
            let mut old = meta.clone();
            let old_problems = crate::attachment::resolve_attachments(&mut old);
            let mut new = meta.clone();
            let report = resolve_timing(&mut new, &BeatGrids::default());
            assert_eq!(new, old, "{name}: the numbers differ");
            let new_problems: Vec<AttachmentProblem> = report
                .problems
                .into_iter()
                .map(|p| match p {
                    TimingProblem::Attachment(a) => a,
                    other => panic!("{name}: a beat finding on a seconds-only file: {other}"),
                })
                .collect();
            assert_eq!(new_problems, old_problems, "{name}: the problems differ");
            if meta != old {
                attached += 1;
            }
        }
        assert!(checked > 20, "decoded only {checked}");
        // Guard the guard: the corpus holds files the resolver moves.
        assert!(
            attached > 0,
            "no project in the corpus has an attachment to resolve"
        );
    }
}
