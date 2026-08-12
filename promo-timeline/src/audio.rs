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

// ---------------------------------------------------------------------------
// Mix-graph math (Swift `AudioTimelineBuilder` twins): the per-track
// amplitude automation that AVFoundation (today) or the PCM mixer (core)
// executes. All f32 exactly where Swift uses Float.

/// A volume breakpoint on the output timeline. Consecutive points with
/// different volumes describe a linear ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePoint {
    pub time: f64,
    pub volume: f32,
}

/// Swift `AudioTimelineBuilder.amplitude(forFraction:)` — perceptual taper:
/// squared fraction (50% ≈ −12 dB, 10% ≈ −40 dB).
pub fn amplitude_for_fraction(fraction: f32) -> f32 {
    let f = fraction.clamp(0.0, 1.0);
    f * f
}

/// Swift `mergeIntervals` — sorted, disjoint union of (start, end) spans.
pub fn merge_intervals(intervals: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut sorted: Vec<(f64, f64)> = intervals.iter().copied().filter(|(a, b)| b > a).collect();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut iter = sorted.into_iter();
    let Some(mut current) = iter.next() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for interval in iter {
        if interval.0 <= current.1 {
            current.1 = current.1.max(interval.1);
        } else {
            result.push(current);
            current = interval;
        }
    }
    result.push(current);
    result
}

/// Swift `duckGate(at:mergedFocus:duckFactor:ramp:)` — linear-amplitude duck
/// multiplier: 1 outside any focus interval, `duck_factor` fully inside,
/// `ramp`-second linear fades on the edges.
pub fn duck_gate(t: f64, merged_focus: &[(f64, f64)], duck_factor: f32, ramp: f64) -> f32 {
    for &(f0, f1) in merged_focus {
        if t <= f0 - ramp || t >= f1 + ramp {
            continue;
        }
        if t >= f0 && t <= f1 {
            return duck_factor;
        }
        if t < f0 {
            let p = if ramp > 0.0 {
                ((t - (f0 - ramp)) / ramp) as f32
            } else {
                1.0
            };
            return 1.0 + (duck_factor - 1.0) * p;
        }
        let p = if ramp > 0.0 {
            (((f1 + ramp) - t) / ramp) as f32
        } else {
            1.0
        };
        return 1.0 + (duck_factor - 1.0) * p;
    }
    1.0
}

/// Swift `sampleAutomation(at:points:)` — linear interpolation over a sorted
/// fraction-domain curve, clamped outside its range.
pub fn sample_automation(t: f64, points: &[VolumePoint]) -> f32 {
    let Some(first) = points.first() else {
        return 1.0;
    };
    if t <= first.time {
        return first.volume;
    }
    let Some(last) = points.last() else {
        return first.volume;
    };
    if t >= last.time {
        return last.volume;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t >= a.time && t <= b.time {
            let span = b.time - a.time;
            let p = if span > 0.0 {
                ((t - a.time) / span) as f32
            } else {
                1.0
            };
            return a.volume + (b.volume - a.volume) * p;
        }
    }
    last.volume
}

/// Swift `levelPoints(...)` — the final per-track amplitude breakpoints:
/// user automation through the perceptual taper, multiplied by the focus
/// duck gate. Empty when the result is constant full amplitude.
#[allow(clippy::too_many_arguments)]
pub fn level_points(
    automation: &[VolumePoint],
    track_start: f64,
    track_end: f64,
    focus_intervals: &[(f64, f64)],
    is_focused: bool,
    duck_factor: f32,
    ramp: f64,
) -> Vec<VolumePoint> {
    if track_end <= track_start {
        return Vec::new();
    }
    let Some(first_auto) = automation.first() else {
        return Vec::new();
    };
    let merged_focus: Vec<(f64, f64)> = if is_focused {
        Vec::new()
    } else {
        merge_intervals(
            &focus_intervals
                .iter()
                .filter_map(|&(lo, hi)| {
                    let lo = lo.max(track_start);
                    let hi = hi.min(track_end);
                    (hi > lo).then_some((lo, hi))
                })
                .collect::<Vec<_>>(),
        )
    };

    let mut times = vec![track_start, track_end];
    for p in automation {
        if p.time > track_start && p.time < track_end {
            times.push(p.time);
        }
    }
    for &(lo, hi) in &merged_focus {
        for edge in [lo - ramp, lo, hi, hi + ramp] {
            if edge > track_start && edge < track_end {
                times.push(edge);
            }
        }
    }
    // Millisecond quantization + dedup + sort (Swift: Set of rounded values).
    let mut sorted_times: Vec<f64> = times
        .iter()
        .map(|t| (t * 1000.0).round() / 1000.0)
        .collect();
    sorted_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted_times.dedup();

    let mut points: Vec<VolumePoint> = Vec::new();
    for t in sorted_times {
        let frac = if automation.len() == 1 {
            first_auto.volume
        } else {
            sample_automation(t, automation)
        };
        let amp = amplitude_for_fraction(frac) * duck_gate(t, &merged_focus, duck_factor, ramp);
        if let Some(last) = points.last_mut() {
            if (last.time - t).abs() < 0.000_1 {
                *last = VolumePoint {
                    time: t,
                    volume: amp,
                };
                continue;
            }
        }
        points.push(VolumePoint {
            time: t,
            volume: amp,
        });
    }
    if points.iter().all(|p| (p.volume - 1.0).abs() < 0.000_1) {
        return Vec::new();
    }
    points
}
