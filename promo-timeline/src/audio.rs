//! Audio volume math — keyframed gain resolution and ramp-segment building
//! (`AVMutableAudioMixInputParameters` feeding), plus focus-ducking spans.
//! Values are `f32` exactly where Swift uses `Float`, so results are
//! bit-identical with the Swift implementation.

use promo_model::{ProjectLayer, ProjectMetadata};

/// Anchor tuple from Swift `volumeAnchors(defaultGain:)`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeAnchor {
    time: f64,
    value: f32,
    transition: f64,
}

fn volume_anchors(layer: &ProjectLayer, default_gain: f32) -> Vec<VolumeAnchor> {
    let mut anchors: Vec<VolumeAnchor> = {
        let mut keyed: Vec<_> = layer
            .keyframes
            .iter()
            .filter(|k| k.gain.is_some())
            .collect();
        keyed.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        keyed
            .iter()
            .map(|k| VolumeAnchor {
                time: k.time,
                value: k.gain.unwrap_or(default_gain),
                transition: k.transition_duration,
            })
            .collect()
    };
    if anchors.is_empty() {
        return anchors;
    }
    if anchors[0].time > 0.0001 {
        anchors.insert(
            0,
            VolumeAnchor {
                time: 0.0,
                value: default_gain,
                transition: 0.0,
            },
        );
    }
    anchors
}

/// Swift `ProjectLayer.gain(atLocalTime:defaultGain:)` — the resolved volume
/// at a local time, hold-then-ease between anchors.
pub fn layer_gain(layer: &ProjectLayer, local_time: f64, default_gain: f32) -> f32 {
    let anchors = volume_anchors(layer, default_gain);
    let (Some(first), Some(last)) = (anchors.first(), anchors.last()) else {
        return default_gain;
    };
    if local_time <= first.time {
        return first.value;
    }
    if local_time >= last.time {
        return last.value;
    }
    for pair in anchors.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = b.transition.min(gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return a.value;
            }
            let progress: f32 = if effective_transition > 0.0 {
                ((local_time - transition_start) / effective_transition) as f32
            } else {
                1.0
            };
            return a.value + (b.value - a.value) * progress;
        }
    }
    first.value
}

/// Swift `ProjectLayer.GainRampSegment`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainRampSegment {
    pub start_time: f64,
    pub end_time: f64,
    pub start_value: f32,
    pub end_value: f32,
}

/// Swift `ProjectLayer.gainRampSegments(defaultGain:)` — layer-local ramps;
/// empty when the layer has no gain keyframes.
pub fn gain_ramp_segments(layer: &ProjectLayer, default_gain: f32) -> Vec<GainRampSegment> {
    let anchors = volume_anchors(layer, default_gain);
    if anchors.is_empty() {
        return Vec::new();
    }
    if anchors.len() == 1 {
        return vec![GainRampSegment {
            start_time: 0.0,
            end_time: 0.0,
            start_value: anchors[0].value,
            end_value: anchors[0].value,
        }];
    }

    let mut segments = Vec::new();
    for pair in anchors.windows(2) {
        let (prev, curr) = (pair[0], pair[1]);
        let gap = curr.time - prev.time;
        if gap < 0.001 {
            continue;
        }
        let ease_window = curr.transition.min(gap);

        // Hold prev's value during the pre-ease window.
        let hold_end = curr.time - ease_window;
        if hold_end > prev.time + 0.001 {
            segments.push(GainRampSegment {
                start_time: prev.time,
                end_time: hold_end,
                start_value: prev.value,
                end_value: prev.value,
            });
        }

        if ease_window > 0.001 {
            segments.push(GainRampSegment {
                start_time: hold_end,
                end_time: curr.time,
                start_value: prev.value,
                end_value: curr.value,
            });
        } else {
            // Instantaneous step: anchor curr's value at curr.time.
            segments.push(GainRampSegment {
                start_time: curr.time,
                end_time: curr.time,
                start_value: curr.value,
                end_value: curr.value,
            });
        }
    }
    segments
}

/// Swift `RecordingProject.audioFocusIntervals()` — output-time spans during
/// which any enabled, audio-focused layer plays (raw, unmerged), in
/// `orderedLayers` order.
pub fn audio_focus_intervals(project: &ProjectMetadata) -> Vec<(f64, f64)> {
    let mut layers: Vec<&ProjectLayer> = project.layers.as_deref().unwrap_or(&[]).iter().collect();
    layers.sort_by_key(|l| l.sort_index);
    layers
        .iter()
        .filter_map(|layer| {
            if !layer.is_enabled || !layer.is_audio_focused() {
                return None;
            }
            let start = layer.start_time.max(0.0);
            let end = start + layer.duration.unwrap_or(0.0).max(0.0);
            (end > start).then_some((start, end))
        })
        .collect()
}
