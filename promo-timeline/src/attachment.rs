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

use promo_model::{LayerTiming, ProjectLayer, ProjectMetadata, TimingReference};

/// Why a layer's timing could not be worked out. Each names both layers,
/// because "which one" is the first thing anyone asks.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentProblem {
    /// The topmost layer has nothing above it to attach to.
    NoPreviousLayer { layer: String },
    /// A START anchored to the end of a layer that never ends. The end of an
    /// open-ended layer is the end of the composition, and a layer beginning
    /// there has no time to exist.
    StartsAtTheEnd { layer: String },
    /// The offsets put the end at or before the start.
    NotPositive { layer: String, duration: f64 },
}

impl std::fmt::Display for AttachmentProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPreviousLayer { layer } => write!(
                f,
                "layer \"{layer}\" is attached to the previous layer, but nothing is above it"
            ),
            Self::StartsAtTheEnd { layer } => write!(
                f,
                "layer \"{layer}\" starts at the end of a layer that runs to the end of \
                 the composition, so it would never play — anchor its start to that \
                 layer's start instead, or its end to that end"
            ),
            Self::NotPositive { layer, duration } => write!(
                f,
                "layer \"{layer}\" resolves to a duration of {duration:.3}s; \
                 its end offset lands at or before its start"
            ),
        }
    }
}

/// The resolved window of a layer, in composition time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Window {
    start: f64,
    /// `None` for a layer that runs to the end of the composition.
    end: Option<f64>,
}

/// `None` when the anchor asks for the end of a layer that has none.
///
/// The caller decides what that means, because it depends which end is
/// asking: a layer ENDING with an open-ended background should run to the
/// end of the composition, while a layer STARTING there would begin after
/// everything is over.
fn anchor_time(window: Window, reference: TimingReference) -> Option<f64> {
    match reference {
        TimingReference::PreviousStart => Some(window.start),
        TimingReference::PreviousEnd => window.end,
    }
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

/// Resolves every attached layer in place, in `sortIndex` order.
///
/// Layers are visited in the same order a person sees them, so an attachment
/// chain resolves front to back in one pass: B settles before C, which asks
/// about B. Returns every problem found rather than the first, so a person
/// fixing a project sees all of it at once.
pub fn resolve_attachments(project: &mut ProjectMetadata) -> Vec<AttachmentProblem> {
    let Some(layers) = project.layers.as_mut() else {
        return Vec::new();
    };

    // sortIndex decides who "previous" is, not array position — the array is
    // free to be in any order and often is.
    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by(|&a, &b| {
        layers[a]
            .sort_index
            .cmp(&layers[b].sort_index)
            .then_with(|| a.cmp(&b))
    });

    let end_of_composition = composition_end(layers);
    let mut problems = Vec::new();
    let mut previous: Option<Window> = None;

    for &index in &order {
        let window = {
            let layer = &layers[index];
            match resolve_one(layer, previous, end_of_composition, &mut problems) {
                Some(window) => window,
                // Unresolvable: leave the stored values alone so the project
                // still renders something, and report why.
                None => Window {
                    start: layer.start_time,
                    end: layer.duration.map(|d| layer.start_time + d),
                },
            }
        };
        let layer = &mut layers[index];
        if layer.timing.is_some() {
            layer.start_time = window.start;
            if let Some(end) = window.end {
                layer.duration = Some(end - window.start);
            }
        }
        previous = Some(window);
    }

    problems
}

fn resolve_one(
    layer: &ProjectLayer,
    previous: Option<Window>,
    end_of_composition: f64,
    problems: &mut Vec<AttachmentProblem>,
) -> Option<Window> {
    let timing = layer.timing.as_ref()?;
    if timing.start.is_none() && timing.end.is_none() {
        return None;
    }
    let Some(previous) = previous else {
        problems.push(AttachmentProblem::NoPreviousLayer {
            layer: layer.name.clone(),
        });
        return None;
    };

    let start = match timing.start.as_ref() {
        Some(anchor) => match anchor_time(previous, anchor.from) {
            Some(time) => time + anchor.offset,
            None => {
                problems.push(AttachmentProblem::StartsAtTheEnd {
                    layer: layer.name.clone(),
                });
                return None;
            }
        },
        None => layer.start_time,
    };
    // An end anchored to an open-ended layer means "run to the finish", which
    // is what attaching to a background should do.
    let end = match timing.end.as_ref() {
        Some(anchor) => {
            Some(anchor_time(previous, anchor.from).unwrap_or(end_of_composition) + anchor.offset)
        }
        None => layer.duration.map(|d| start + d),
    };

    if let Some(end) = end {
        let duration = end - start;
        if duration <= 0.0 {
            problems.push(AttachmentProblem::NotPositive {
                layer: layer.name.clone(),
                duration,
            });
            return None;
        }
    }
    Some(Window { start, end })
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

    let attached = |layer: &ProjectLayer| {
        layer
            .timing
            .as_ref()
            .is_some_and(|t: &LayerTiming| t.start.is_some() || t.end.is_some())
    };

    let mut first = position;
    while first > 0 && attached(ordered[first]) {
        first -= 1;
    }
    let mut last = position;
    while last + 1 < ordered.len() && attached(ordered[last + 1]) {
        last += 1;
    }
    ordered[first..=last]
        .iter()
        .map(|layer| layer.id.clone())
        .collect()
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
            start_time: start,
            duration,
            resource_id: None,
            image_filename: None,
            image_cut_id: None,
            image_orientation: None,
            image_border_color_hex: None,
            image_border_width: None,
            caption_text: None,
            caption_style: None,
            caption_voice_clip: None,
            audio_focus: None,
            timing: None,
            keyframes: Vec::new(),
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
    fn an_end_before_its_start_is_reported_not_clamped() {
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
            Some(AttachmentProblem::NotPositive { .. })
        ));
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
}
