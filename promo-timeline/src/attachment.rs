//! Resolving attached layers into plain start times and durations.
//!
//! A layer may state its timing outright, or derive it from the layer above —
//! "start when that one ends, less a beat", "end just before it does". The
//! spec is what a person edits; `start_time` and `duration` stay the answer,
//! and every renderer keeps reading those.
//!
//! Keeping the two apart is the point. A spec interpreted independently by the
//! preview, the exporter and the CLI is three chances to disagree, which is
//! precisely how caption placement drifted. Resolve once, write numbers, and
//! there is nothing left to interpret.
//!
//! Anchors reach only at the previous layer, so a cycle cannot be written down
//! and one ordered pass is enough. It also means a chain of attachments is
//! always a **contiguous run** of layers — which is what lets a UI treat one
//! as a group without storing a group anywhere.

use promo_model::{
    DurationRuleKind, ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource,
    TimingReference,
};

/// Why a layer's timing could not be worked out. Each names both layers,
/// because "which one" is the first thing anyone asks.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentProblem {
    /// The topmost layer has nothing above it to attach to.
    NoPreviousLayer { layer: String },
    /// The bottom layer has nothing below it, and anchors forward anyway.
    NoNextLayer { layer: String },
    /// A START anchored to the end of a layer that never ends. The end of an
    /// open-ended layer is the end of the composition, and a layer beginning
    /// there has no time to exist.
    StartsAtTheEnd { layer: String },
    /// Two neighbours each waiting on the other. Only possible now that
    /// anchors reach both ways; found while resolving rather than prevented
    /// by the shape.
    Circular { layer: String },
    /// The offsets put the end at or before the start, so the layer resolves
    /// to nothing. Clamped to zero rather than refused — resolution always
    /// produces an answer — but said out loud, because layer visibility is
    /// half-open (`time < start + duration`) and a zero-length layer therefore
    /// renders NO frames at all. A one-frame flash needs a real duration.
    ZeroLength { layer: String },
}

impl std::fmt::Display for AttachmentProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPreviousLayer { layer } => write!(
                f,
                "layer \"{layer}\" is attached to the previous layer, but nothing is above it"
            ),
            Self::NoNextLayer { layer } => write!(
                f,
                "layer \"{layer}\" is attached to the next layer, but nothing is below it"
            ),
            Self::StartsAtTheEnd { layer } => write!(
                f,
                "layer \"{layer}\" starts at the end of a layer that runs to the end of \
                 the composition, so it would never play — anchor its start to that \
                 layer's start instead, or its end to that end"
            ),
            Self::Circular { layer } => write!(
                f,
                "layer \"{layer}\" and its neighbour each wait on the other — one of \
                 them has to anchor to something already settled"
            ),
            Self::ZeroLength { layer } => write!(
                f,
                "layer \"{layer}\" resolves to zero length — its end offset lands at or \
                 before its start, so it renders no frames at all"
            ),
        }
    }
}

/// A layer's own stored duration, with `fitContent` resolved through it:
/// the layer plays its RESOURCE out, however long the file turned out to be.
/// Unknown content (a speech draft not yet synthesized) falls back to the
/// stored number, so a draft show still lays out.
fn effective_duration(layer: &ProjectLayer, resources: &[ProjectResource]) -> Option<f64> {
    if layer
        .duration_rule
        .is_some_and(|r| r.kind == DurationRuleKind::FitContent)
    {
        if let Some(length) = layer
            .resource_id
            .as_ref()
            .and_then(|id| resources.iter().find(|r| &r.id == id))
            .and_then(|r| r.duration)
            .filter(|d| *d > 0.0)
        {
            return Some(length);
        }
    }
    layer.duration
}

/// Measured once, before anything is resolved. A layer that then resolves
/// past it does not stretch the answer for its neighbours: one pass, one set
/// of numbers, no chasing its own tail.
fn composition_end(layers: &[ProjectLayer]) -> f64 {
    layers
        .iter()
        .filter_map(|layer| layer.duration.map(|d| layer.start_time + d))
        .fold(0.0, f64::max)
}

/// What is known about one layer's window while resolving.
///
/// `end` is doubly optional on purpose: not-yet-worked-out and runs-forever
/// are different answers, and collapsing them would make an open-ended
/// background look like a layer still waiting on a neighbour.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    start: Option<f64>,
    end: Option<Option<f64>>,
}

/// The right "nothing there" report for an anchor's direction — a forward
/// anchor at the bottom must not claim something is missing ABOVE.
fn missing_neighbour(from: TimingReference, layer: &ProjectLayer) -> AttachmentProblem {
    match from {
        TimingReference::PreviousStart
        | TimingReference::PreviousEnd
        | TimingReference::PreviousPeerStart
        | TimingReference::PreviousPeerEnd => AttachmentProblem::NoPreviousLayer {
            layer: layer.name.clone(),
        },
        TimingReference::NextStart
        | TimingReference::NextEnd
        | TimingReference::NextPeerStart
        | TimingReference::NextPeerEnd => AttachmentProblem::NoNextLayer {
            layer: layer.name.clone(),
        },
    }
}

/// Resolves every attached layer in place.
///
/// Anchors may point either way, so this is not a single ordered walk: it
/// settles what it can and goes round again until a pass changes nothing.
/// Dependencies only ever join adjacent layers, so the number of rounds is
/// bounded by the number of layers, and anything still unresolved at the end
/// is a cycle — two neighbours each waiting on the other. Those are named and
/// left with their stored values, so the project still plays.
pub fn resolve_attachments(project: &mut ProjectMetadata) -> Vec<AttachmentProblem> {
    let resources = project.resources.clone().unwrap_or_default();
    let Some(layers) = project.layers.as_mut() else {
        return Vec::new();
    };

    // sortIndex decides who the neighbours are, not array position — the
    // array is free to be in any order and often is.
    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by(|&a, &b| {
        layers[a]
            .sort_index
            .cmp(&layers[b].sort_index)
            .then_with(|| a.cmp(&b))
    });

    let end_of_composition = composition_end(layers);
    let mut problems = Vec::new();

    // Everything not attached is known from the outset, and is what the
    // attached layers hang off.
    let kinds: Vec<ProjectLayerKind> = order.iter().map(|&i| layers[i].kind).collect();
    let mut slots: Vec<Slot> = order
        .iter()
        .map(|&index| {
            let layer = &layers[index];
            if is_attached(layer) {
                Slot::default()
            } else if layer.duration_rule.is_some() {
                // Start is its own; the END is the rule's to decide.
                Slot {
                    start: Some(layer.start_time),
                    end: None,
                }
            } else {
                Slot {
                    start: Some(layer.start_time),
                    end: Some(layer.duration.map(|d| layer.start_time + d)),
                }
            }
        })
        .collect();

    // Who hangs off whom: position -> the positions whose start is anchored
    // to this layer's START. That is CONTAINMENT — "things that begin inside
    // me" — and it is the relation `fitDependents` fits. Sequencing anchors
    // (…End) and peer chains are deliberately NOT dependents: the next slide
    // follows this one, it does not live inside it — and counting it would
    // make the rule wait on a layer that waits on the rule.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for position in 0..order.len() {
        if let Some(anchor) = layers[order[position]]
            .timing
            .as_ref()
            .and_then(|t| t.start.as_ref())
        {
            let containment = matches!(
                anchor.from,
                TimingReference::PreviousStart | TimingReference::NextStart
            );
            if containment {
                if let Some(target) = anchor_target(&kinds, position, anchor.from) {
                    dependents[target].push(position);
                }
            }
        }
    }

    let mut stalled = vec![false; order.len()];
    loop {
        let mut progressed = false;
        for position in 0..order.len() {
            if stalled[position] {
                continue;
            }
            let layer = &layers[order[position]];
            if !participates(layer) {
                continue;
            }
            // A rule layer may carry no anchors at all; it still runs the
            // loop so its END gets decided.
            let timing = layer.timing.clone().unwrap_or_default();
            if slots[position].start.is_some() && slots[position].end.is_some() {
                continue;
            }

            // Start.
            if slots[position].start.is_none() {
                match timing.start.as_ref() {
                    None => {
                        slots[position].start = Some(layer.start_time);
                        progressed = true;
                    }
                    Some(anchor) => match neighbour_time(&slots, &kinds, position, anchor.from) {
                        Known::Time(time) => {
                            slots[position].start = Some(time + anchor.offset);
                            progressed = true;
                        }
                        Known::OpenEnded => {
                            // "Begin when a layer that never ends, ends."
                            problems.push(AttachmentProblem::StartsAtTheEnd {
                                layer: layer.name.clone(),
                            });
                            slots[position].start = Some(layer.start_time);
                            stalled[position] = true;
                            progressed = true;
                        }
                        Known::Missing => {
                            problems.push(missing_neighbour(anchor.from, layer));
                            slots[position].start = Some(layer.start_time);
                            stalled[position] = true;
                            progressed = true;
                        }
                        Known::NotYet => {}
                    },
                }
            }

            // End, which may need the start first.
            if slots[position].end.is_none() {
                // A duration RULE owns the end; an end anchor on the same
                // layer would be two producers for one number, which
                // validate names. The rule waits for every dependent.
                if layer
                    .duration_rule
                    .is_some_and(|r| r.kind == DurationRuleKind::FitDependents)
                {
                    if let Some(start) = slots[position].start {
                        let tail = layer.duration_rule.and_then(|r| r.tail).unwrap_or(0.0);
                        let mut floor = effective_duration(layer, &resources).map(|d| start + d);
                        let mut ready = true;
                        for &child in &dependents[position] {
                            match slots[child].end {
                                Some(Some(end)) => {
                                    let fitted = end + tail;
                                    floor = Some(match floor {
                                        Some(f) => f.max(fitted),
                                        None => fitted,
                                    });
                                }
                                // An open-ended dependent runs to the
                                // composition's end; the floor stands.
                                Some(None) => {}
                                None => ready = false,
                            }
                        }
                        if ready {
                            slots[position].end = Some(floor);
                            progressed = true;
                        }
                    }
                    continue;
                }
                match timing.end.as_ref() {
                    None => {
                        if let Some(start) = slots[position].start {
                            slots[position].end =
                                Some(effective_duration(layer, &resources).map(|d| start + d));
                            progressed = true;
                        }
                    }
                    Some(anchor) => match neighbour_time(&slots, &kinds, position, anchor.from) {
                        // An end anchored to an open-ended layer means "run to
                        // the finish", which is what attaching to a background
                        // should do.
                        Known::Time(time) => {
                            slots[position].end = Some(Some(time + anchor.offset));
                            progressed = true;
                        }
                        Known::OpenEnded => {
                            slots[position].end = Some(Some(end_of_composition + anchor.offset));
                            progressed = true;
                        }
                        Known::Missing => {
                            // One report per layer: the start block may have
                            // already said it this pass.
                            if !stalled[position] {
                                problems.push(missing_neighbour(anchor.from, layer));
                            }
                            slots[position].end =
                                Some(layer.duration.map(|d| layer.start_time + d));
                            stalled[position] = true;
                            progressed = true;
                        }
                        Known::NotYet => {}
                    },
                }
            }
        }
        if !progressed {
            break;
        }
    }

    // Whatever never settled is waiting on something that is waiting on it.
    for position in 0..order.len() {
        let layer = &layers[order[position]];
        if participates(layer) && (slots[position].start.is_none() || slots[position].end.is_none())
        {
            problems.push(AttachmentProblem::Circular {
                layer: layer.name.clone(),
            });
            slots[position].start = Some(layer.start_time);
            slots[position].end = Some(layer.duration.map(|d| layer.start_time + d));
        }
    }

    // Clamp and write back.
    for position in 0..order.len() {
        let index = order[position];
        if !participates(&layers[index]) {
            continue;
        }
        let start = slots[position].start.unwrap_or(layers[index].start_time);
        let end = slots[position].end.flatten();
        let end = match end {
            Some(end) if end <= start => {
                problems.push(AttachmentProblem::ZeroLength {
                    layer: layers[index].name.clone(),
                });
                Some(start)
            }
            other => other,
        };
        layers[index].start_time = start;
        if let Some(end) = end {
            layers[index].duration = Some(end - start);
        }
    }

    problems
}

fn is_attached(layer: &ProjectLayer) -> bool {
    layer
        .timing
        .as_ref()
        .is_some_and(|t| t.start.is_some() || t.end.is_some())
}

/// Whether resolution has anything to decide for this layer. A duration rule
/// participates exactly like an anchor: the stored numbers become a cache.
fn participates(layer: &ProjectLayer) -> bool {
    is_attached(layer) || layer.duration_rule.is_some()
}

/// The answer to "what time is that anchor", which has four shapes.
enum Known {
    Time(f64),
    /// The neighbour runs to the end of the composition.
    OpenEnded,
    /// There is no such neighbour.
    Missing,
    /// The neighbour has not been worked out yet — go round again.
    NotYet,
}

/// The position an anchor points at, peers included: a peer reference walks
/// past layers of OTHER kinds to the nearest layer of the SAME kind — what
/// lets a slide chain to the previous slide across the narration between
/// them.
fn anchor_target(
    kinds: &[ProjectLayerKind],
    position: usize,
    reference: TimingReference,
) -> Option<usize> {
    match reference {
        TimingReference::PreviousStart | TimingReference::PreviousEnd => position.checked_sub(1),
        TimingReference::NextStart | TimingReference::NextEnd => {
            let next = position + 1;
            (next < kinds.len()).then_some(next)
        }
        TimingReference::PreviousPeerStart | TimingReference::PreviousPeerEnd => {
            (0..position).rev().find(|&i| kinds[i] == kinds[position])
        }
        TimingReference::NextPeerStart | TimingReference::NextPeerEnd => {
            ((position + 1)..kinds.len()).find(|&i| kinds[i] == kinds[position])
        }
    }
}

fn neighbour_time(
    slots: &[Slot],
    kinds: &[ProjectLayerKind],
    position: usize,
    reference: TimingReference,
) -> Known {
    let Some(neighbour) = anchor_target(kinds, position, reference) else {
        return Known::Missing;
    };
    match reference {
        TimingReference::PreviousStart
        | TimingReference::NextStart
        | TimingReference::PreviousPeerStart
        | TimingReference::NextPeerStart => match slots[neighbour].start {
            Some(time) => Known::Time(time),
            None => Known::NotYet,
        },
        TimingReference::PreviousEnd
        | TimingReference::NextEnd
        | TimingReference::PreviousPeerEnd
        | TimingReference::NextPeerEnd => match slots[neighbour].end {
            Some(Some(time)) => Known::Time(time),
            Some(None) => Known::OpenEnded,
            None => Known::NotYet,
        },
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Backward,
    Forward,
}

fn anchors(layer: &ProjectLayer, direction: Direction) -> bool {
    let Some(timing) = layer.timing.as_ref() else {
        return false;
    };
    [timing.start.as_ref(), timing.end.as_ref()]
        .into_iter()
        .flatten()
        .any(|anchor| {
            matches!(
                (direction, anchor.from),
                (
                    Direction::Backward,
                    TimingReference::PreviousStart | TimingReference::PreviousEnd
                ) | (
                    Direction::Forward,
                    TimingReference::NextStart | TimingReference::NextEnd
                )
            )
        })
}

/// The contiguous run of layers a layer belongs to, as indices into the
/// `sortIndex` order.
///
/// A run is every layer joined by attachments — moving one in z-order has to
/// take the whole run, or the chain stops being contiguous and every anchor
/// after the gap silently points somewhere new. Because attachment only ever
/// reaches one layer back, membership is decided by looking at neighbours, and
/// no group needs to be stored anywhere.
pub fn run_containing(project: &ProjectMetadata, layer_id: &str) -> Vec<String> {
    let Some(layers) = project.layers.as_ref() else {
        return Vec::new();
    };
    let mut ordered: Vec<&ProjectLayer> = layers.iter().collect();
    ordered.sort_by_key(|layer| layer.sort_index);

    let Some(position) = ordered.iter().position(|layer| layer.id == layer_id) else {
        return Vec::new();
    };

    // A layer joins its neighbour above if it anchors backwards, or if that
    // neighbour anchors forwards at it — the link belongs to the pair, not to
    // whichever side wrote it down.
    let joined_to_the_one_above = |position: usize| -> bool {
        if position == 0 {
            return false;
        }
        anchors(ordered[position], Direction::Backward)
            || anchors(ordered[position - 1], Direction::Forward)
    };

    let mut first = position;
    while joined_to_the_one_above(first) {
        first -= 1;
    }
    let mut last = position;
    while last + 1 < ordered.len() && joined_to_the_one_above(last + 1) {
        last += 1;
    }
    ordered[first..=last]
        .iter()
        .map(|layer| layer.id.clone())
        .collect()
}

/// Every run among `layers` at once, each in `sortIndex` order.
///
/// Same link rule as [`run_containing`], but computed in one pass so a UI can
/// paint the chain marks and gate reordering without asking per layer.
/// Singletons are omitted: a layer with no attachments is not in a run, and
/// reporting it as a run of one would make every caller filter. Takes bare
/// layers rather than a project because that is all it reads — and all the
/// editor FFI sends.
pub fn runs(layers: &[ProjectLayer]) -> Vec<Vec<String>> {
    let mut ordered: Vec<&ProjectLayer> = layers.iter().collect();
    ordered.sort_by_key(|layer| layer.sort_index);

    let mut out: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for position in 0..ordered.len() {
        let joined = position > 0
            && (anchors(ordered[position], Direction::Backward)
                || anchors(ordered[position - 1], Direction::Forward));
        if !joined {
            if current.len() > 1 {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
        current.push(ordered[position].id.clone());
    }
    if current.len() > 1 {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_model::{LayerTiming, ProjectLayerKind, TimingAnchor};

    fn layer(id: &str, sort: i64, start: f64, duration: Option<f64>) -> ProjectLayer {
        ProjectLayer {
            id: id.to_string(),
            name: id.to_string(),
            sort_index: sort,
            kind: ProjectLayerKind::Video,
            is_enabled: true,
            fade_in: None,
            fade_out: None,
            transition_in: None,
            transition_out: None,
            motion_blur: None,
            adjustments: None,
            blend_mode: None,
            mask_resource_id: None,
            mask_inverted: None,
            start_time: start,
            duration,
            resource_id: None,
            image_filename: None,
            image_cut_id: None,
            media_cut_id: None,
            beyond_end: None,
            image_orientation: None,
            image_border_color_hex: None,
            image_border_width: None,
            caption_text: None,
            caption_style: None,
            caption_voice_clip: None,
            audio_focus: None,
            timing: None,
            duration_rule: None,
            keyframes: Vec::new(),
            extra: Default::default(),
        }
    }

    fn anchor(from: TimingReference, offset: f64) -> TimingAnchor {
        TimingAnchor { from, offset }
    }

    /// Built from JSON rather than a struct literal: `ProjectMetadata` has a
    /// lot of fields that have nothing to do with attachment, and spelling
    /// them out here would bury the part under test.
    fn project(layers: Vec<ProjectLayer>) -> ProjectMetadata {
        let mut project: ProjectMetadata = serde_json::from_str(
            r#"{
                "id": "T", "name": "Test", "createdAt": 0, "state": "recorded",
                "subtitles": [], "trimStart": 0, "trimEnd": 0,
                "videoDuration": 0, "compositionSettings": {}
            }"#,
        )
        .expect("minimal project decodes");
        project.layers = Some(layers);
        project
    }

    #[test]
    fn attached_layer_takes_its_window_from_the_one_above() {
        let mut a = layer("A", 0, 2.0, Some(4.0)); // 2.0 → 6.0
        a.timing = None;
        let mut b = layer("B", 1, 0.0, None);
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, -0.5)),
            end: Some(anchor(TimingReference::PreviousEnd, 2.0)),
        });
        let mut p = project(vec![a, b]);
        assert!(resolve_attachments(&mut p).is_empty());

        let layers = p.layers.unwrap();
        assert_eq!(layers[1].start_time, 5.5);
        // End anchors at A's end (6.0) + 2.0, so duration is 8.0 - 5.5.
        assert!((layers[1].duration.unwrap() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn a_chain_resolves_front_to_back_in_one_pass() {
        let a = layer("A", 0, 0.0, Some(3.0)); // 0 → 3
        let mut b = layer("B", 1, 0.0, Some(2.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None, // keeps its own duration
        });
        let mut c = layer("C", 2, 0.0, Some(1.0));
        c.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.5)),
            end: None,
        });
        let mut p = project(vec![c.clone(), a, b]); // deliberately out of order
        assert!(resolve_attachments(&mut p).is_empty());

        let layers = p.layers.unwrap();
        let by_id = |id: &str| layers.iter().find(|l| l.id == id).unwrap().clone();
        assert_eq!(by_id("B").start_time, 3.0);
        assert_eq!(by_id("B").duration, Some(2.0));
        // C follows B's resolved end (5.0), not B's stored start.
        assert_eq!(by_id("C").start_time, 5.5);
    }

    #[test]
    fn moving_the_anchor_moves_everything_downstream() {
        let a = layer("A", 0, 0.0, Some(3.0));
        let mut b = layer("B", 1, 0.0, Some(2.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None,
        });
        let mut c = layer("C", 2, 0.0, Some(1.0));
        c.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None,
        });
        let mut p = project(vec![a, b, c]);
        resolve_attachments(&mut p);
        let before = p.layers.as_ref().unwrap()[2].start_time;

        // Shift A by a second; the whole run should follow with no other edit.
        p.layers.as_mut().unwrap()[0].start_time += 1.0;
        resolve_attachments(&mut p);
        let after = p.layers.as_ref().unwrap()[2].start_time;
        assert!((after - before - 1.0).abs() < 1e-12, "the run moves as one");
    }

    #[test]
    fn attaching_to_an_open_ended_layer_means_the_end_of_the_composition() {
        // A background carries no duration: it runs the whole way. A layer
        // that ENDS with it should run to the finish rather than fail.
        let clip = layer("Clip", 0, 0.0, Some(9.0)); // sets the composition end
        let background = layer("Background", 1, 0.0, None);
        let mut outro = layer("Outro", 2, 0.0, None);
        outro.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 1.0)),
            end: Some(anchor(TimingReference::PreviousEnd, 0.0)),
        });
        let mut p = project(vec![clip, background, outro]);
        let problems = resolve_attachments(&mut p);
        assert!(
            problems.is_empty(),
            "an open end is not a failure: {problems:?}"
        );

        let layers = p.layers.unwrap();
        let outro = layers.iter().find(|l| l.id == "Outro").unwrap();
        assert_eq!(outro.start_time, 1.0);
        // The composition ends at 9.0, so the outro fills 1.0 → 9.0.
        assert_eq!(outro.duration, Some(8.0));
    }

    #[test]
    fn an_end_before_its_start_clamps_to_zero_and_says_so() {
        let a = layer("A", 0, 0.0, Some(4.0));
        let mut b = layer("B", 1, 0.0, None);
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: Some(anchor(TimingReference::PreviousEnd, -1.0)),
        });
        let mut p = project(vec![a, b]);
        let problems = resolve_attachments(&mut p);
        assert!(matches!(
            problems.first(),
            Some(AttachmentProblem::ZeroLength { .. })
        ));
        // Clamped, not refused, and never negative.
        assert_eq!(p.layers.unwrap()[1].duration, Some(0.0));
    }

    #[test]
    fn a_forward_anchor_reaches_the_layer_below() {
        // A caption must draw ABOVE its clip, so z-order is not free. Reaching
        // forward is what lets the relationship be written from either side.
        let mut caption = layer("Caption", 0, 0.0, None);
        caption.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::NextStart, -2.5)),
            end: Some(anchor(TimingReference::NextEnd, 0.0)),
        });
        let clip = layer("Clip", 1, 10.0, Some(4.0));
        let mut p = project(vec![caption, clip]);
        let problems = resolve_attachments(&mut p);
        assert!(problems.is_empty(), "{problems:?}");

        let layers = p.layers.unwrap();
        let caption = layers.iter().find(|l| l.id == "Caption").unwrap();
        assert_eq!(caption.start_time, 7.5);
        assert_eq!(caption.duration, Some(6.5)); // 7.5 → 14.0
    }

    #[test]
    fn two_neighbours_waiting_on_each_other_are_reported() {
        let mut a = layer("A", 0, 1.0, Some(1.0));
        a.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::NextStart, -1.0)),
            end: None,
        });
        let mut b = layer("B", 1, 2.0, Some(1.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 1.0)),
            end: None,
        });
        let mut p = project(vec![a, b]);
        let problems = resolve_attachments(&mut p);
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, AttachmentProblem::Circular { .. })),
            "expected a cycle, got {problems:?}"
        );
        // Both keep what they had, so the project still plays.
        let layers = p.layers.unwrap();
        assert_eq!(layers[0].start_time, 1.0);
        assert_eq!(layers[1].start_time, 2.0);
    }

    #[test]
    fn a_run_is_the_contiguous_stretch_of_attached_layers() {
        let a = layer("A", 0, 0.0, Some(1.0));
        let mut b = layer("B", 1, 0.0, Some(1.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None,
        });
        let mut c = layer("C", 2, 0.0, Some(1.0));
        c.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None,
        });
        let d = layer("D", 3, 9.0, Some(1.0)); // detached: ends the run
        let p = project(vec![a, b, c, d]);

        assert_eq!(run_containing(&p, "B"), vec!["A", "B", "C"]);
        assert_eq!(run_containing(&p, "A"), vec!["A", "B", "C"]);
        assert_eq!(run_containing(&p, "D"), vec!["D"]);
    }

    #[test]
    fn runs_reports_every_group_and_skips_singletons() {
        // A←B chained, C loose, D→E chained by D anchoring FORWARD — the link
        // belongs to the pair no matter which side wrote it down.
        let a = layer("A", 0, 0.0, Some(1.0));
        let mut b = layer("B", 1, 0.0, Some(1.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousEnd, 0.0)),
            end: None,
        });
        let c = layer("C", 2, 5.0, Some(1.0));
        let mut d = layer("D", 3, 0.0, Some(1.0));
        d.timing = Some(LayerTiming {
            start: None,
            end: Some(anchor(TimingReference::NextStart, 0.0)),
        });
        let e = layer("E", 4, 8.0, Some(1.0));

        assert_eq!(runs(&[a, b, c, d, e]), vec![vec!["A", "B"], vec!["D", "E"]]);
    }

    #[test]
    fn runs_orders_by_sort_index_not_array_position() {
        let mut b = layer("B", 1, 0.0, Some(1.0));
        b.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 0.5)),
            end: None,
        });
        let a = layer("A", 0, 0.0, Some(1.0));
        // B stored before A: the run must still come out A-then-B.
        assert_eq!(runs(&[b, a]), vec![vec!["A", "B"]]);
    }

    /// The narrated-slideshow scheme, whole: slides chain PEER to peer so no
    /// slide's start depends on a sound's length; each narration hangs off
    /// its slide and plays its file out (`fitContent`); each slide holds its
    /// stored N or stretches to fit its narration plus a breath
    /// (`fitDependents`). One long take moves everything after it — by
    /// resolution, with no imperative walk anywhere.
    #[test]
    fn a_slide_waits_for_its_narration_by_rule() {
        use promo_model::{DurationRule, DurationRuleKind};

        let fit_deps = DurationRule {
            kind: DurationRuleKind::FitDependents,
            tail: Some(2.5),
        };
        let fit_content = DurationRule {
            kind: DurationRuleKind::FitContent,
            tail: None,
        };

        let mut slide1 = layer("Slide1", 1, 0.0, Some(20.0));
        slide1.kind = ProjectLayerKind::Image;
        slide1.duration_rule = Some(fit_deps);

        let mut audio1 = layer("Audio1", 2, 0.0, Some(1.0));
        audio1.kind = ProjectLayerKind::Audio;
        audio1.resource_id = Some("SOUND1".into());
        audio1.duration_rule = Some(fit_content);
        audio1.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 0.4)),
            end: None,
        });

        let mut slide2 = layer("Slide2", 3, 0.0, Some(20.0));
        slide2.kind = ProjectLayerKind::Image;
        slide2.duration_rule = Some(fit_deps);
        slide2.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousPeerEnd, -1.0)),
            end: None,
        });

        let mut audio2 = layer("Audio2", 4, 0.0, Some(1.0));
        audio2.kind = ProjectLayerKind::Audio;
        audio2.duration_rule = Some(fit_content); // no file yet: placeholder
        audio2.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 0.4)),
            end: None,
        });

        let mut p = project(vec![slide1, audio1, slide2, audio2]);
        p.resources = Some(vec![serde_json::from_str(
            r#"{"id": "SOUND1", "kind": "audio", "filename": "n.mp3",
                "displayName": "n", "addedAt": 0, "duration": 25.0}"#,
        )
        .expect("resource decodes")]);

        let problems = resolve_attachments(&mut p);
        assert!(problems.is_empty(), "{problems:?}");

        let layers = p.layers.unwrap();
        let by_id = |id: &str| layers.iter().find(|l| l.id == id).unwrap().clone();
        // The narration plays its 25s file out...
        assert!((by_id("Audio1").duration.unwrap() - 25.0).abs() < 1e-9);
        // ...so its slide waits: 0.4 lead + 25 + 2.5 breath.
        assert!((by_id("Slide1").duration.unwrap() - 27.9).abs() < 1e-9);
        // The next slide chained through the PEER, not the sound.
        assert!((by_id("Slide2").start_time - 26.9).abs() < 1e-9);
        // Whose own draft narration is far below its floor: N stands.
        assert!((by_id("Slide2").duration.unwrap() - 20.0).abs() < 1e-9);
        assert!((by_id("Audio2").start_time - 27.3).abs() < 1e-9);
    }

    /// The floor half: a short take changes nothing. Resolution reproduces
    /// the stored numbers exactly, because max(N, sound + gap) = N.
    #[test]
    fn a_short_narration_never_shrinks_its_slide() {
        use promo_model::{DurationRule, DurationRuleKind};

        let mut slide = layer("Slide", 1, 0.0, Some(20.0));
        slide.kind = ProjectLayerKind::Image;
        slide.duration_rule = Some(DurationRule {
            kind: DurationRuleKind::FitDependents,
            tail: Some(2.5),
        });
        let mut audio = layer("Audio", 2, 0.0, Some(1.0));
        audio.kind = ProjectLayerKind::Audio;
        audio.resource_id = Some("S".into());
        audio.duration_rule = Some(DurationRule {
            kind: DurationRuleKind::FitContent,
            tail: None,
        });
        audio.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 0.4)),
            end: None,
        });
        let mut p = project(vec![slide, audio]);
        p.resources = Some(vec![serde_json::from_str(
            r#"{"id": "S", "kind": "audio", "filename": "n.mp3",
                "displayName": "n", "addedAt": 0, "duration": 8.0}"#,
        )
        .unwrap()]);

        assert!(resolve_attachments(&mut p).is_empty());
        let layers = p.layers.unwrap();
        assert!(
            (layers[0].duration.unwrap() - 20.0).abs() < 1e-9,
            "N stands"
        );
        assert!((layers[1].duration.unwrap() - 8.0).abs() < 1e-9);
    }

    /// A peer anchor walks past other kinds to the nearest layer of its own,
    /// and reports a missing PREVIOUS when there is none.
    #[test]
    fn peer_anchors_skip_other_kinds() {
        let mut first = layer("First", 0, 1.0, Some(5.0)); // image, 1 → 6
        first.kind = ProjectLayerKind::Image;
        let voice = layer("Voice", 1, 0.0, Some(2.0)); // video kind, in between
        let mut second = layer("Second", 2, 0.0, Some(3.0));
        second.kind = ProjectLayerKind::Image;
        second.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousPeerEnd, 0.5)),
            end: None,
        });
        let mut p = project(vec![first, voice, second]);
        assert!(resolve_attachments(&mut p).is_empty());
        assert!(
            (p.layers.as_ref().unwrap()[2].start_time - 6.5).abs() < 1e-9,
            "chained to the previous IMAGE, not the layer in between"
        );

        let mut orphan = layer("Orphan", 0, 0.0, Some(1.0));
        orphan.kind = ProjectLayerKind::Caption;
        orphan.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousPeerEnd, 0.0)),
            end: None,
        });
        let mut p2 = project(vec![orphan]);
        let problems = resolve_attachments(&mut p2);
        assert!(matches!(
            problems.first(),
            Some(AttachmentProblem::NoPreviousLayer { .. })
        ));
    }

    /// A dependent that anchors its END back onto the ruled layer is a real
    /// cycle — value-circular, not just shape-circular — and is NAMED, with
    /// both layers keeping their stored numbers.
    #[test]
    fn a_dependent_riding_the_ruled_end_is_named_circular() {
        use promo_model::{DurationRule, DurationRuleKind};

        let mut slide = layer("Slide", 1, 0.0, Some(20.0));
        slide.kind = ProjectLayerKind::Image;
        slide.duration_rule = Some(DurationRule {
            kind: DurationRuleKind::FitDependents,
            tail: Some(2.5),
        });
        let mut audio = layer("Audio", 2, 0.0, Some(1.0));
        audio.kind = ProjectLayerKind::Audio;
        audio.timing = Some(LayerTiming {
            start: Some(anchor(TimingReference::PreviousStart, 0.4)),
            end: Some(anchor(TimingReference::PreviousEnd, -0.2)),
        });
        let mut p = project(vec![slide, audio]);
        let problems = resolve_attachments(&mut p);
        assert!(
            problems
                .iter()
                .any(|pr| matches!(pr, AttachmentProblem::Circular { .. })),
            "the ride-the-end pattern cycles with fitDependents: {problems:?}"
        );
    }
}
