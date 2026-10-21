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

use promo_model::musical::{
    self, After, BeatClock, BeatGrid, MusicalTime, SequenceEntry, SnapUnit, TimeSpec, TimeSpecs,
    TimeValue,
};
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
    /// An analysis's sections, by resource id — what a generator's
    /// `{"section": …}` range names.
    pub sections: BTreeMap<String, Vec<musical::BeatSection>>,
    /// An analysis's confidence, by resource id: under 0.5 is a warning.
    pub confidence: BTreeMap<String, f64>,
    /// An analysis's beats (the song's seconds) and which is beat 1, by
    /// resource id — the list a clock counts on, so the cuts land on the
    /// beats `promo beats` prints.
    pub beats: BTreeMap<String, (Vec<f64>, usize)>,
}

impl BeatGrids {
    /// The grids, sections and confidences of songs' analyses, by resource
    /// id (`promo_model::beat_cache::cached_analyses`).
    pub fn from_analyses(analyses: &BTreeMap<String, musical::BeatAnalysis>) -> BeatGrids {
        let mut grids = BeatGrids::default();
        for (id, analysis) in analyses.iter().filter(|(_, a)| a.bpm > 0.0) {
            grids.by_resource.insert(id.clone(), analysis.grid());
            grids.sections.insert(id.clone(), analysis.sections.clone());
            grids.confidence.insert(id.clone(), analysis.confidence);
            if analysis.beats.len() >= 2 {
                // Beat 1 is the first downbeat — the beat nearest it.
                let first = analysis
                    .beats
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        (a.1 - analysis.offset)
                            .abs()
                            .total_cmp(&(b.1 - analysis.offset).abs())
                    })
                    .map_or(0, |(k, _)| k);
                grids
                    .beats
                    .insert(id.clone(), (analysis.beats.clone(), first));
            }
        }
        grids
    }
}

/// One beat clock as resolution read it: what `validate` and `inspect`
/// list.
#[derive(Debug, Clone, PartialEq)]
pub struct BeatClockInfo {
    /// `composition` for the project's own, else `resources[<id>]`.
    pub scope: String,
    pub resource: String,
    /// `beatGrid` (stated on the resource), `analysis`, or `nominal` (the
    /// 120 BPM stand-in).
    pub source: String,
    pub bpm: f64,
    pub meter: u32,
    /// The first downbeat, in the song's seconds.
    pub offset: f64,
    /// Where the song's second 0 sits on the output timeline.
    pub clock: f64,
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
    /// `after` or a `sequence` entry names a layer the scope does not hold.
    UnknownLayer {
        layer: String,
        field: String,
        id: String,
    },
    /// A layer listed twice in one sequence: the first listing counts.
    InSequenceTwice {
        layer: String,
    },
    /// A sequenced layer that also says `after` or `timing.start`: the
    /// sequence places it.
    SequenceWins {
        layer: String,
        over: String,
    },
    /// A layer with both `after` and `timing.start`: `after` places it.
    AfterWins {
        layer: String,
    },
    /// Layers each starting after the other: none of them can be placed,
    /// and each keeps its stored start.
    AfterCircular {
        layer: String,
    },
    /// `after` (or the sequence) follows a layer that never ends.
    FollowsAnOpenEnd {
        layer: String,
        target: String,
    },
    /// A beat generator that cannot be expanded as written.
    Generator {
        layer: String,
        why: String,
    },
    /// A layer edge off the beat grid by more than half a frame.
    OffGrid {
        layer: String,
        edge: &'static str,
        at: f64,
        off: f64,
        /// The nearest grid point in words — "beat 3 at 1.25 s", a pickup
        /// before beat 1, or a point before the song starts; never a
        /// negative beat number.
        nearest: String,
    },
    /// A hand-set `beatGrid` whose tempo differs from the song's measured
    /// one: by the end of the timeline its beats are `drift` seconds from
    /// the song's.
    HandGridDrifts {
        resource: String,
        stated: f64,
        measured: f64,
        drift: f64,
        at: f64,
    },
    /// A layer shorter than one beat of its clock.
    ShorterThanABeat {
        layer: String,
        length: f64,
        beat: f64,
        bpm: f64,
    },
    /// The clock's song ends before the timeline does.
    SongEndsEarly {
        resource: String,
        ends: f64,
        timeline: f64,
    },
    /// The tracker was unsure of the song's beats.
    UnsureBeats {
        resource: String,
        confidence: f64,
    },
    /// A pulse over a track already keyed: its base is sampled at beat
    /// resolution.
    PulseOverKeys {
        layer: String,
        property: String,
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
                | TimingProblem::UnknownLayer { .. }
                | TimingProblem::InSequenceTwice { .. }
                | TimingProblem::SequenceWins { .. }
                | TimingProblem::AfterWins { .. }
                | TimingProblem::AfterCircular { .. }
                | TimingProblem::FollowsAnOpenEnd { .. }
                | TimingProblem::Generator { .. }
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
            Self::UnknownLayer { layer, field, id } => write!(
                f,
                "layer \"{layer}\": {field} names \"{id}\", which is no layer here — \
                 ids are the layers' own, in the same composition"
            ),
            Self::InSequenceTwice { layer } => write!(
                f,
                "layer \"{layer}\" is in the sequence twice — the first place counts"
            ),
            Self::SequenceWins { layer, over } => write!(
                f,
                "layer \"{layer}\" is in the sequence and also has {over} — the sequence \
                 places it; remove one"
            ),
            Self::AfterWins { layer } => write!(
                f,
                "layer \"{layer}\" has both after and timing.start — after places it; \
                 remove one"
            ),
            Self::AfterCircular { layer } => write!(
                f,
                "layer \"{layer}\" starts after a layer that (through others) starts after \
                 it — none of them can be placed, and each keeps its stored start"
            ),
            Self::Generator { layer, why } => write!(f, "layer \"{layer}\": {why}"),
            Self::OffGrid {
                layer,
                edge,
                at,
                off,
                nearest,
            } => write!(
                f,
                "layer \"{layer}\" at {at:.2}s: {edge} {:.2} s off the beat grid — nearest is \
                 {nearest}",
                off.abs()
            ),
            Self::HandGridDrifts {
                resource,
                stated,
                measured,
                drift,
                at,
            } => write!(
                f,
                "the beatGrid on \"{resource}\" says {stated} BPM, the song measures {measured} \
                 BPM — by {at:.2} s its beats are {:.1} ms from the song's; drop the grid to \
                 count the measured beats, or state the measured tempo",
                drift * 1000.0
            ),
            Self::ShorterThanABeat {
                layer,
                length,
                beat,
                bpm,
            } => write!(
                f,
                "layer \"{layer}\": lasts {length:.2} s, shorter than one beat ({beat:.2} s at \
                 {bpm} BPM)"
            ),
            Self::SongEndsEarly {
                resource,
                ends,
                timeline,
            } => write!(
                f,
                "the beat clock's audio \"{resource}\" ends at {ends:.1} s but the timeline runs \
                 to {timeline:.1} s — the grid after {ends:.1} s is extrapolated"
            ),
            Self::UnsureBeats {
                resource,
                confidence,
            } => write!(
                f,
                "beat detection for \"{resource}\" is unsure (confidence {confidence:.2}) — set \
                 beatGrid by hand"
            ),
            Self::PulseOverKeys { layer, property } => write!(
                f,
                "pulse on layer \"{layer}\" reshapes keyed {property} keyframes at beat \
                 resolution"
            ),
            Self::FollowsAnOpenEnd { layer, target } => write!(
                f,
                "layer \"{layer}\" follows \"{target}\", which never ends — give \
                 \"{target}\" a duration"
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
    /// Clock resources that state their grid but have no measure yet —
    /// their sections (a generator's `range`) wait for one.
    pub unmeasured: Vec<String>,
    /// Every scope's beat clock.
    pub clocks: Vec<BeatClockInfo>,
}

/// Every musical value and attachment in `meta`, resolved to seconds in
/// place. Never refuses: what cannot be honoured keeps its stored number
/// and is named in the report.
pub fn resolve_timing(meta: &mut ProjectMetadata, grids: &BeatGrids) -> TimingReport {
    let mut report = TimingReport::default();
    // Generated content as written goes first — it is made again at the end.
    crate::beat_effects::settle(meta);
    let resources = meta.resources.clone().unwrap_or_default();

    // A value someone edited stands as seconds, as a save would fold it;
    // a sequenced or `after` layer someone moved leaves its rule.
    settle_edits(
        meta.layers.as_deref_mut().unwrap_or(&mut []),
        &mut meta.composition_settings.sequence,
    );
    for resource in meta.resources.iter_mut().flatten() {
        if let Some(composition) = resource.composition.as_mut() {
            settle_edits(&mut composition.layers, &mut composition.sequence);
        }
    }

    // The project's scope: the musical pass, `sequence` and `after`, and
    // the attachments, until none of them moves anything. Each round can
    // only settle what the last one made known, so the layer count plus one
    // bounds it.
    let clock = meta.composition_settings.beat_clock.clone();
    let rounds = meta.layers.as_ref().map_or(0, Vec::len) + 1;
    let plan = Plan::new(
        meta.layers.as_deref().unwrap_or(&[]),
        meta.composition_settings.sequence.as_deref(),
    );
    let mut attachment_problems = Vec::new();
    let mut late = Vec::new();
    for _ in 0..rounds {
        let before = numbers(meta.layers.as_deref().unwrap_or(&[]));
        let scope = Scope::new(
            clock.as_ref(),
            meta.layers.as_deref().unwrap_or(&[]),
            &resources,
            grids,
        );
        let layers = meta.layers.as_deref_mut().unwrap_or(&mut []);
        positions_and_lengths(layers, &scope);
        late = plan.place(layers, &scope);
        // A layer the sequence or `after` places is no attachment's: its
        // start anchor sits out the attachment pass, and comes back.
        let held = plan.hold_start_anchors(meta.layers.as_deref_mut().unwrap_or(&mut []));
        attachment_problems = crate::attachment::resolve_attachments(meta);
        Plan::restore_start_anchors(meta.layers.as_deref_mut().unwrap_or(&mut []), held);
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
    report.problems.extend(plan.problems.iter().cloned());
    report.problems.extend(late);
    report.problems.extend(
        attachment_problems
            .into_iter()
            .map(TimingProblem::Attachment),
    );
    // The generators, on the final numbers.
    let fps = meta
        .composition_settings
        .fps
        .filter(|f| *f > 0.0)
        .unwrap_or(30.0);
    let end = crate::composition_duration(meta);
    let settings = meta.composition_settings.clone();
    {
        let sections = scope
            .clock_resource
            .as_ref()
            .and_then(|id| grids.sections.get(id))
            .map(Vec::as_slice);
        expand_scope(
            meta.layers.as_deref_mut().unwrap_or(&mut []),
            &scope,
            fps,
            sections,
            end,
            &settings,
            &resources,
            &mut report,
        );
        let flash = crate::beat_effects::Context {
            grid: scope.grid.clone(),
            fps,
            sections,
            end,
        };
        report
            .problems
            .extend(crate::beat_effects::expand_flash(meta, &flash));
        let timeline = crate::composition_duration(meta);
        findings(
            meta.layers.as_deref().unwrap_or(&[]),
            &scope,
            &resources,
            grids,
            fps,
            timeline,
            "composition",
            &mut report,
        );
    }

    // Each composition: its own clock, never its parent's — it may be
    // placed at several times. No attachments run inside (as before).
    for resource in meta.resources.iter_mut().flatten() {
        let id = resource.id.clone();
        let Some(composition) = resource.composition.as_mut() else {
            continue;
        };
        let clock = composition.beat_clock.clone();
        let plan = Plan::new(&composition.layers, composition.sequence.as_deref());
        let mut late = Vec::new();
        for _ in 0..composition.layers.len() + 1 {
            let before = numbers(&composition.layers);
            let scope = Scope::new(clock.as_ref(), &composition.layers, &resources, grids);
            positions_and_lengths(&mut composition.layers, &scope);
            late = plan.place(&mut composition.layers, &scope);
            if numbers(&composition.layers) == before {
                break;
            }
        }
        let scope = Scope::new(clock.as_ref(), &composition.layers, &resources, grids);
        finish(
            &mut composition.layers,
            &scope,
            &format!("resources[{id}].composition.layers"),
            &mut report,
        );
        report.problems.extend(plan.problems.iter().cloned());
        report.problems.extend(late);
        let sections = scope
            .clock_resource
            .as_ref()
            .and_then(|id| grids.sections.get(id))
            .map(Vec::as_slice);
        let end = composition
            .layers
            .iter()
            .filter_map(|l| l.duration.map(|d| l.start_time + d))
            .fold(0.0, f64::max);
        expand_scope(
            &mut composition.layers,
            &scope,
            fps,
            sections,
            end,
            &settings,
            &resources,
            &mut report,
        );
        findings(
            &composition.layers,
            &scope,
            &resources,
            grids,
            fps,
            end,
            &format!("resources[{id}]"),
            &mut report,
        );
    }
    report.missing_grids.sort();
    report.missing_grids.dedup();
    report.unmeasured.sort();
    report.unmeasured.dedup();
    report
}

/// A grid point in the words a person reads: "beat 3 at 1.25 s" from beat 1
/// on, "the pickup 2 beats before beat 1, at 0.73 s" inside the song before
/// its first downbeat, and for a point before the song even plays, where
/// the song starts and its first beat — never a negative beat number.
pub(crate) fn grid_point_words(grid: &Grid, index: f64, half_frame: f64) -> String {
    let at = grid.time_at(index);
    let song = grid.clock;
    if at + half_frame < song {
        let mut first = grid.index_at(song).ceil();
        if grid.time_at(first) + half_frame < song {
            first += 1.0;
        }
        return format!(
            "a point before the song starts (at {song:.2} s; its first beat is at {:.2} s)",
            grid.time_at(first)
        );
    }
    if index < 0.0 {
        let n = -index;
        return format!(
            "the pickup {} beat{} before beat 1, at {at:.2} s",
            n,
            if n == 1.0 { "" } else { "s" }
        );
    }
    format!("beat {} at {at:.2} s", index as i64 + 1)
}

/// What a reader of a beat-cut edit wants to hear: a visual layer's edge off
/// the grid (by more than half a frame), one shorter than a beat, a song
/// that ends before the timeline, a detection the tracker was unsure of —
/// and the scope's clock, for the listing.
#[allow(clippy::too_many_arguments)]
fn findings(
    layers: &[ProjectLayer],
    scope: &Scope,
    resources: &[ProjectResource],
    grids: &BeatGrids,
    fps: f64,
    timeline: f64,
    name: &str,
    report: &mut TimingReport,
) {
    let Some(id) = scope.clock_resource.clone() else {
        return;
    };
    let grid = scope.grid.clone();
    report.clocks.push(BeatClockInfo {
        scope: name.into(),
        resource: id.clone(),
        source: scope.source.into(),
        bpm: grid.bpm,
        meter: grid.meter,
        offset: grid.offset,
        clock: grid.clock,
    });
    let half_frame = 0.5 / fps.max(1.0);
    fn walk(layers: &[ProjectLayer], scope: &Scope, half_frame: f64, report: &mut TimingReport) {
        for layer in layers {
            if let Some(members) = layer.members.as_deref() {
                walk(members, scope, half_frame, report);
            }
            if matches!(
                layer.kind,
                ProjectLayerKind::Audio | ProjectLayerKind::Background
            ) || layer.generated_by.is_some()
                || scope.places_clock(layer)
                || scope.clockless_for(layer)
                || !layer.is_enabled
            {
                continue;
            }
            let grid = scope.grid_of(layer);
            let mut edges = vec![("starts", layer.start_time)];
            if let Some(d) = layer.duration {
                edges.push(("ends", layer.start_time + d));
                // The beat where the layer plays — a measured song's beats
                // are not all one length — and half a frame of give.
                let here = grid.index_at(layer.start_time).floor();
                let beat = grid.time_at(here + 1.0) - grid.time_at(here);
                if d + half_frame < beat {
                    report.problems.push(TimingProblem::ShorterThanABeat {
                        layer: layer.name.clone(),
                        length: d,
                        beat,
                        bpm: grid.bpm,
                    });
                }
            }
            for (edge, at) in edges {
                let index = grid.index_at(at).round();
                let nearest = grid.time_at(index);
                let off = at - nearest;
                if off.abs() > half_frame {
                    report.problems.push(TimingProblem::OffGrid {
                        layer: layer.name.clone(),
                        edge,
                        at,
                        off,
                        nearest: grid_point_words(&grid, index, half_frame),
                    });
                }
            }
        }
    }
    walk(layers, scope, half_frame, report);
    if let Some(song) = resources.iter().find(|r| r.id == id) {
        if let Some(length) = song.duration.filter(|d| *d > 0.0) {
            let ends = grid.clock + length / grid.speed;
            if ends + half_frame < timeline {
                report.problems.push(TimingProblem::SongEndsEarly {
                    resource: id.clone(),
                    ends,
                    timeline,
                });
            }
        }
        // A hand grid against the song's measure: how far apart their beats
        // come by the end of the timeline.
        if let (Some(stated), Some((beats, first)), Some(measured)) = (
            song.beat_grid.as_ref(),
            grids.beats.get(&id),
            grids.by_resource.get(&id),
        ) {
            let song_grid = Grid::listed(
                beats,
                *first,
                measured.bpm,
                grid.meter,
                grid.clock,
                grid.speed,
            );
            let mut worst: Option<(f64, f64)> = None;
            let mut k = grid.index_at(grid.clock).ceil().max(0.0);
            loop {
                let t = grid.time_at(k);
                if t > timeline + half_frame || k > 100_000.0 {
                    break;
                }
                let theirs = song_grid.time_at(song_grid.index_at(t).round());
                let apart = (t - theirs).abs();
                if worst.is_none_or(|(w, _)| apart > w) {
                    worst = Some((apart, t));
                }
                k += 1.0;
            }
            if let Some((drift, at)) = worst.filter(|(d, _)| *d > 0.005) {
                report.problems.push(TimingProblem::HandGridDrifts {
                    resource: id.clone(),
                    stated: stated.bpm,
                    measured: measured.bpm,
                    drift,
                    at,
                });
            }
        }
        if song.beat_grid.is_none() {
            if let Some(&confidence) = grids.confidence.get(&id) {
                if confidence < 0.5 {
                    report.problems.push(TimingProblem::UnsureBeats {
                        resource: id.clone(),
                        confidence,
                    });
                }
            }
        }
    }
}

/// Each layer's `pulse` and `shake`, stage members included, on its grid.
/// (The settings and resources are what a layer's base position resolves
/// against; the rest is its scope.)
#[allow(clippy::too_many_arguments)]
fn expand_scope(
    layers: &mut [ProjectLayer],
    scope: &Scope,
    fps: f64,
    sections: Option<&[musical::BeatSection]>,
    end: f64,
    settings: &promo_model::CompositionSettings,
    resources: &[ProjectResource],
    report: &mut TimingReport,
) {
    for layer in layers {
        let ctx = crate::beat_effects::Context {
            grid: scope.grid_of(layer),
            fps,
            sections,
            end,
        };
        report.problems.extend(crate::beat_effects::expand_layer(
            layer, &ctx, settings, resources,
        ));
        if let Some(members) = layer.members.as_mut() {
            expand_scope(
                members, scope, fps, sections, end, settings, resources, report,
            );
        }
    }
}

/// Drops every spec whose field no longer holds its resolved seconds, and
/// every derived entry: a rule's number is worked out again each pass, and
/// one someone moved detaches its layer from the rule — `after` goes, a
/// sequenced start leaves the sequence, a sequenced length becomes that
/// many seconds (docs/BEAT-SYNC.md §1.3, the save's fold rule).
fn settle_edits(layers: &mut [ProjectLayer], sequence: &mut Option<Vec<SequenceEntry>>) {
    fn walk(layers: &mut [ProjectLayer], detaches: &mut Vec<(String, Option<f64>)>) {
        for layer in layers {
            let numbers: Vec<(String, Option<f64>)> = layer
                .time_specs
                .iter()
                .flatten()
                .map(|(key, _)| (key.clone(), musical::layer_number(layer, key)))
                .collect();
            let start_moved = layer
                .time_specs
                .as_ref()
                .and_then(|m| m.get("startTime"))
                .filter(|e| e.rule.is_some())
                .map(|e| {
                    (
                        e.rule.clone().unwrap_or_default(),
                        !e.stands(Some(layer.start_time)),
                    )
                });
            match &start_moved {
                Some((rule, true)) if rule == "after" => layer.after = None,
                Some((rule, true)) if rule == "sequence" => {
                    detaches.push((layer.id.clone(), None));
                }
                _ => {
                    let resized = layer
                        .time_specs
                        .as_ref()
                        .and_then(|m| m.get("duration"))
                        .filter(|e| e.rule.as_deref() == Some("sequence"))
                        .is_some_and(|e| !e.stands(layer.duration));
                    if resized {
                        if let Some(seconds) = layer.duration {
                            detaches.push((layer.id.clone(), Some(seconds)));
                        }
                    }
                }
            }
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
                walk(members, detaches);
            }
        }
    }
    let mut detaches = Vec::new();
    walk(layers, &mut detaches);
    let Some(entries) = sequence.as_mut() else {
        return;
    };
    for (id, seconds) in detaches {
        match seconds {
            None => entries.retain(|e| e.layer != id),
            Some(seconds) => {
                for entry in entries.iter_mut().filter(|e| e.layer == id) {
                    *entry = SequenceEntry {
                        layer: id.clone(),
                        beats: None,
                        bars: None,
                        seconds: Some(seconds),
                        duration: None,
                        extra: Default::default(),
                    };
                }
            }
        }
    }
}

/// Keeps the specs that still hold; drops every derived entry (the pass
/// derives it again) and every spec someone edited.
fn retain_held(specs: &mut Option<TimeSpecs>, numbers: &[(String, Option<f64>)]) {
    let Some(map) = specs.as_mut() else { return };
    for (key, number) in numbers {
        if map
            .get(key)
            .is_some_and(|entry| entry.rule.is_some() || !entry.holds(*number))
        {
            map.remove(key);
        }
    }
    if map.is_empty() {
        *specs = None;
    }
}

/// A layer edge snapped to the grid its clock counts (docs/BEAT-SYNC.md
/// §6.3): the musical value an editor writes with `setTime`, the seconds it
/// resolves to (a start's position, an end's length) and the core's words
/// for it — so a host rounds nothing itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapped {
    pub value: MusicalTime,
    pub seconds: f64,
    pub label: String,
}

/// Which edge of a layer a snap moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// The grid a project-scope layer (or a stage's member) counts on, or why
/// it has none.
fn layer_grid<'a>(
    meta: &'a ProjectMetadata,
    grids: &BeatGrids,
    layer_id: &str,
) -> Result<(&'a ProjectLayer, Grid), String> {
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let layer =
        find(layers, layer_id).ok_or_else(|| format!("no layer \"{layer_id}\" in the project"))?;
    let scope = Scope::new(
        meta.composition_settings.beat_clock.as_ref(),
        layers,
        meta.resources.as_deref().unwrap_or(&[]),
        grids,
    );
    if scope.clockless_for(layer) {
        return Err(
            "the project has no beat clock — choose the music that counts the beats".into(),
        );
    }
    Ok((layer, scope.grid_of(layer)))
}

/// The start of `layer_id` on the nearest beat (`Beat`) or bar start
/// (`Downbeat`, `Bar`), or its end there — written as the position the
/// edge sits on (`{"beat": n}` or `{"bar": n}`; an end's length then runs
/// to it). An end never snaps onto or before the start: it takes the next
/// grid point after.
pub fn snap_edge(
    meta: &ProjectMetadata,
    grids: &BeatGrids,
    layer_id: &str,
    edge: Edge,
    unit: SnapUnit,
) -> Result<Snapped, String> {
    let (layer, grid) = layer_grid(meta, grids, layer_id)?;
    let step = match unit {
        SnapUnit::Beat => 1.0,
        SnapUnit::Downbeat | SnapUnit::Bar => f64::from(grid.meter),
    };
    let start = layer.start_time;
    let near = match edge {
        Edge::Start => start,
        Edge::End => {
            start
                + layer
                    .duration
                    .ok_or("the layer has no set length to snap")?
        }
    };
    let mut index = grid.snap(unit, near);
    if edge == Edge::End {
        // A tenth of a millisecond: a length of nothing is not a length.
        while grid.time_at(index) <= start + 1e-4 {
            index += step;
        }
    }
    let value = match unit {
        SnapUnit::Beat => MusicalTime::Beat(index + 1.0),
        SnapUnit::Downbeat | SnapUnit::Bar => MusicalTime::Bar(index / step + 1.0),
    };
    let at = grid.time_at(index);
    Ok(Snapped {
        label: value.label(),
        seconds: match edge {
            Edge::Start => at,
            Edge::End => at - start,
        },
        value,
    })
}

/// Where output time `seconds` falls on a layer's grid, in the core's
/// words: "bar 3 · beat 2" (a beat between two is written to two decimals).
/// None when the layer counts on no clock.
pub fn bars_beats(
    meta: &ProjectMetadata,
    grids: &BeatGrids,
    layer_id: &str,
    seconds: f64,
) -> Option<String> {
    let (_, grid) = layer_grid(meta, grids, layer_id).ok()?;
    let meter = f64::from(grid.meter);
    // Rounded to a microbeat, so a value resolved onto a beat reads as it.
    let index = (grid.index_at(seconds) * 1e6).round() / 1e6;
    // Before beat 1: how far before it, never a bar 0 or a negative beat.
    if index < 0.0 {
        let n = -index;
        let n = if (n - n.round()).abs() < 1e-3 {
            format!("{}", n.round() as i64)
        } else {
            let text = format!("{n:.2}");
            text.trim_end_matches('0').to_string()
        };
        return Some(format!(
            "{n} beat{} before bar 1",
            if n == "1" { "" } else { "s" }
        ));
    }
    let bar = (index / meter).floor();
    let beat = index - bar * meter + 1.0;
    let beat = if (beat - beat.round()).abs() < 1e-3 {
        format!("{}", beat.round() as i64)
    } else {
        let text = format!("{beat:.2}");
        text.trim_end_matches('0').to_string()
    };
    Some(format!("bar {} · beat {beat}", bar as i64 + 1))
}

fn find<'a>(layers: &'a [ProjectLayer], id: &str) -> Option<&'a ProjectLayer> {
    for layer in layers {
        if layer.id == id {
            return Some(layer);
        }
        if let Some(found) = layer.members.as_deref().and_then(|m| find(m, id)) {
            return Some(found);
        }
    }
    None
}

fn find_mut<'a>(layers: &'a mut [ProjectLayer], id: &str) -> Option<&'a mut ProjectLayer> {
    for layer in layers {
        if layer.id == id {
            return Some(layer);
        }
        if let Some(found) = layer.members.as_deref_mut().and_then(|m| find_mut(m, id)) {
            return Some(found);
        }
    }
    None
}

fn name_of(layers: &[ProjectLayer], id: &str) -> String {
    find(layers, id).map_or_else(|| id.to_string(), |l| l.name.clone())
}

/// Writes a start a rule derived, with its entry.
fn place_start(layer: &mut ProjectLayer, rule: &str, seconds: f64) {
    layer.start_time = seconds;
    layer
        .time_specs
        .get_or_insert_with(Default::default)
        .insert("startTime".into(), TimeSpec::derived(rule, seconds));
}

type Link = (String, String, Option<TimeValue>);

/// What `sequence` and `after` say for one scope, checked once: the
/// members in order, the `after` links, the layers they place, and what
/// cannot be honoured (docs/BEAT-SYNC.md §5).
struct Plan {
    sequence: Vec<SequenceEntry>,
    afters: Vec<Link>,
    placed: std::collections::BTreeSet<String>,
    problems: Vec<TimingProblem>,
}

impl Plan {
    fn new(layers: &[ProjectLayer], sequence: Option<&[SequenceEntry]>) -> Plan {
        let mut plan = Plan {
            sequence: Vec::new(),
            afters: Vec::new(),
            placed: Default::default(),
            problems: Vec::new(),
        };
        for entry in sequence.unwrap_or(&[]) {
            if find(layers, &entry.layer).is_none() {
                plan.problems.push(TimingProblem::UnknownLayer {
                    layer: entry.layer.clone(),
                    field: "the sequence".into(),
                    id: entry.layer.clone(),
                });
            } else if plan.placed.contains(&entry.layer) {
                plan.problems.push(TimingProblem::InSequenceTwice {
                    layer: name_of(layers, &entry.layer),
                });
            } else {
                plan.placed.insert(entry.layer.clone());
                plan.sequence.push(entry.clone());
            }
        }
        fn walk(layers: &[ProjectLayer], out: &mut Vec<Link>) {
            for layer in layers {
                match &layer.after {
                    Some(After::Layer(id)) => out.push((layer.id.clone(), id.clone(), None)),
                    Some(After::Spec(spec)) => {
                        out.push((layer.id.clone(), spec.layer.clone(), spec.offset))
                    }
                    None => {}
                }
                if let Some(members) = layer.members.as_deref() {
                    walk(members, out);
                }
            }
        }
        let mut links = Vec::new();
        walk(layers, &mut links);
        for (id, target, offset) in links {
            let name = name_of(layers, &id);
            if plan.placed.contains(&id) {
                plan.problems.push(TimingProblem::SequenceWins {
                    layer: name,
                    over: "after".into(),
                });
            } else if find(layers, &target).is_none() {
                plan.problems.push(TimingProblem::UnknownLayer {
                    layer: name,
                    field: "after".into(),
                    id: target,
                });
            } else {
                plan.afters.push((id, target, offset));
            }
        }
        // A chain of `after` that comes back to where it started places
        // nobody: each of its layers keeps its stored start.
        let next: std::collections::BTreeMap<String, String> = plan
            .afters
            .iter()
            .map(|(id, target, _)| (id.clone(), target.clone()))
            .collect();
        let circular: Vec<String> = plan
            .afters
            .iter()
            .filter(|(id, _, _)| {
                let mut at = id.clone();
                for _ in 0..=next.len() {
                    match next.get(&at) {
                        Some(target) if target == id => return true,
                        Some(target) => at = target.clone(),
                        None => return false,
                    }
                }
                false
            })
            .map(|(id, _, _)| id.clone())
            .collect();
        for id in &circular {
            plan.problems.push(TimingProblem::AfterCircular {
                layer: name_of(layers, id),
            });
        }
        plan.afters.retain(|(id, _, _)| !circular.contains(id));
        for (id, _, _) in &plan.afters {
            plan.placed.insert(id.clone());
        }
        // A start anchor beside them is overruled, and said so.
        fn anchors(layers: &[ProjectLayer], plan: &mut Plan) {
            for layer in layers {
                if layer.timing.as_ref().is_some_and(|t| t.start.is_some()) {
                    if plan.sequence.iter().any(|e| e.layer == layer.id) {
                        plan.problems.push(TimingProblem::SequenceWins {
                            layer: layer.name.clone(),
                            over: "timing.start".into(),
                        });
                    } else if plan.afters.iter().any(|(id, _, _)| *id == layer.id) {
                        plan.problems.push(TimingProblem::AfterWins {
                            layer: layer.name.clone(),
                        });
                    }
                }
                if let Some(members) = layer.members.as_deref() {
                    anchors(members, plan);
                }
            }
        }
        anchors(layers, &mut plan);
        plan
    }

    /// Places every sequenced and `after` layer on the numbers as they
    /// stand; what cannot be placed this round is returned.
    fn place(&self, layers: &mut [ProjectLayer], scope: &Scope) -> Vec<TimingProblem> {
        let mut late = Vec::new();
        // The sequence: back to back, each for its length.
        let mut previous: Option<(f64, f64, String)> = None;
        for (index, entry) in self.sequence.iter().enumerate() {
            let name = name_of(layers, &entry.layer);
            let Some(layer) = find_mut(layers, &entry.layer) else {
                continue;
            };
            let grid = scope.grid_of(layer);
            let clocked = !scope.clockless_for(layer);
            let (start, first) = if index == 0 {
                // The first member's own start opens the sequence: a beat
                // as written, or a number snapped to the nearest beat when
                // a clock counts.
                if spec_of(&layer.time_specs, "startTime").is_some() || !clocked {
                    (layer.start_time, start_index(layer, &grid, scope))
                } else {
                    let mut first = grid.snap(musical::SnapUnit::Beat, layer.start_time);
                    // Never before the timeline: a start of 0 with the
                    // first downbeat a moment later ties toward the pickup.
                    while grid.time_at(first) < 0.0 {
                        first += 1.0;
                    }
                    let start = grid.time_at(first);
                    layer.start_time = start;
                    (start, first)
                }
            } else {
                match previous.take() {
                    Some((end, end_index, _)) => {
                        place_start(layer, "sequence", end);
                        (end, end_index)
                    }
                    None => break,
                }
            };
            let stated = |beats: f64| Some((grid.time_at(first + beats), first + beats));
            let seconds = |s: f64| {
                let end = start + s;
                if clocked {
                    let snapped = grid.snap(musical::SnapUnit::Beat, end);
                    if grid.time_at(snapped) > start {
                        return Some((grid.time_at(snapped), snapped));
                    }
                }
                Some((end, grid.index_at(end)))
            };
            let length = if let Some(n) = entry.beats {
                stated(n)
            } else if let Some(n) = entry.bars {
                stated(grid.bars(n))
            } else if let Some(s) = entry.seconds {
                seconds(s)
            } else {
                match entry.duration {
                    Some(TimeValue::Seconds(s)) => seconds(s),
                    Some(TimeValue::Musical(m)) => match grid.length(&m) {
                        Some(beats) => stated(beats),
                        None => {
                            let at = grid.position(&m);
                            Some((grid.time_at(at), at))
                        }
                    },
                    None => None,
                }
            };
            let end = match length {
                Some((end, end_index)) => {
                    let duration = end - start;
                    layer.duration = Some(duration);
                    layer
                        .time_specs
                        .get_or_insert_with(Default::default)
                        .insert("duration".into(), TimeSpec::derived("sequence", duration));
                    Some((end, end_index))
                }
                // No length in the entry: the layer's own duration.
                None => layer
                    .duration
                    .map(|d| (start + d, grid.index_at(start + d))),
            };
            match end {
                Some((end, end_index)) => previous = Some((end, end_index, name)),
                None => {
                    if let Some(next) = self.sequence.get(index + 1) {
                        late.push(TimingProblem::FollowsAnOpenEnd {
                            layer: name_of(layers, &next.layer),
                            target: name,
                        });
                    }
                    break;
                }
            }
        }
        // `after`: start where the named layer ends, plus the offset.
        for (id, target, offset) in &self.afters {
            let Some(followed) = find(layers, target) else {
                continue;
            };
            let Some(end) = followed.duration.map(|d| followed.start_time + d) else {
                late.push(TimingProblem::FollowsAnOpenEnd {
                    layer: name_of(layers, id),
                    target: followed.name.clone(),
                });
                continue;
            };
            let Some(layer) = find_mut(layers, id) else {
                continue;
            };
            let grid = scope.grid_of(layer);
            let start = match offset {
                None => end,
                Some(TimeValue::Seconds(n)) => end + n,
                Some(TimeValue::Musical(m)) => match grid.length(m) {
                    Some(beats) => grid.time_at(grid.index_at(end) + beats),
                    None => {
                        late.push(TimingProblem::PositionForLength {
                            layer: layer.name.clone(),
                            field: "after.offset".into(),
                            spec: m.canonical_text(),
                        });
                        end
                    }
                },
            };
            place_start(layer, "after", start);
        }
        late
    }

    /// Takes the start anchor off every layer this plan places, for the
    /// attachment pass; [`Plan::restore_start_anchors`] puts it back.
    fn hold_start_anchors(
        &self,
        layers: &mut [ProjectLayer],
    ) -> Vec<(String, promo_model::LayerTiming)> {
        let mut held = Vec::new();
        for id in &self.placed {
            if let Some(layer) = find_mut(layers, id) {
                if let Some(timing) = layer.timing.clone().filter(|t| t.start.is_some()) {
                    layer.timing = timing.end.map(|end| promo_model::LayerTiming {
                        start: None,
                        end: Some(end),
                    });
                    held.push((id.clone(), timing));
                }
            }
        }
        held
    }

    fn restore_start_anchors(
        layers: &mut [ProjectLayer],
        held: Vec<(String, promo_model::LayerTiming)>,
    ) {
        for (id, timing) in held {
            if let Some(layer) = find_mut(layers, &id) {
                layer.timing = Some(timing);
            }
        }
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
    /// Clock songs stating a grid with no analysis to give sections.
    unmeasured: Vec<String>,
    /// The layer that places the clock's audio, by id.
    clock_layer: Option<String>,
    /// Layers with a `beatClock` of their own, by id.
    overrides: BTreeMap<String, Grid>,
    /// The resource the scope's clock counts.
    clock_resource: Option<String>,
    /// Where the scope's grid came from: `beatGrid`, `analysis`, `nominal`.
    source: &'static str,
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
            unmeasured: Vec::new(),
            clock_layer: None,
            overrides: BTreeMap::new(),
            clock_resource: clock.and_then(|c| c.resource_id.clone()),
            source: "nominal",
        };
        if let Some(clock) = clock {
            let (grid, layer) = scope.grid_for(clock, layers, resources, grids);
            scope.grid = grid;
            scope.clock_layer = layer;
            let id = clock.resource_id.as_deref().unwrap_or_default();
            scope.source = match resources.iter().find(|r| r.id == id) {
                Some(r) if r.beat_grid.is_some() => "beatGrid",
                Some(_) if grids.by_resource.contains_key(id) => "analysis",
                _ => "nominal",
            };
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
        if resource.beat_grid.is_none() {
            if let (Some((beats, first)), Some(measured)) =
                (grids.beats.get(id), grids.by_resource.get(id))
            {
                let meter = measured
                    .time_signature
                    .map(|[beats, _]| beats)
                    .filter(|b| *b > 0)
                    .unwrap_or(4);
                return (
                    Grid::listed(beats, *first, measured.bpm, meter, origin, speed),
                    placed_by,
                );
            }
        }
        if resource.beat_grid.is_some() && !grids.by_resource.contains_key(id) {
            self.unmeasured.push(id.into());
        }
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
        self.overrides
            .get(&layer.id)
            .cloned()
            .unwrap_or_else(|| self.grid.clone())
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
            grid: self.grid.clone(),
            clockless: self.clockless,
            problems: Vec::new(),
            missing: Vec::new(),
            unmeasured: Vec::new(),
            clock_layer: self.clock_layer.clone(),
            overrides: self.overrides.clone(),
            clock_resource: self.clock_resource.clone(),
            source: self.source,
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
    report.unmeasured.extend(scope.unmeasured.iter().cloned());
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
                // A derived start or length: listed by its rule.
                if let Some(rule) = entry.rule.clone() {
                    report.resolved.push(ResolvedTime {
                        at: format!("{here}.{key}"),
                        spec: rule,
                        seconds: musical::layer_number(layer, key).unwrap_or(entry.seconds),
                    });
                }
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
            // A project that declares the musical reader is musical on purpose —
            // the beat-cut recipe, its example, the beat demos.
            if meta.min_reader_version.unwrap_or(0) >= 49 {
                continue;
            }
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
