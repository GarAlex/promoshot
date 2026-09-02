//! Audio volume math — keyframed gain resolution and ramp-segment building
//! (`AVMutableAudioMixInputParameters` feeding), plus focus-ducking spans.
//! Values are `f32` exactly where Swift uses `Float`, so results are
//! bit-identical with the Swift implementation.

use crate::mapping::ExtendedPause;
use promo_model::{
    ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResourceKind, VideoTrimRange,
};

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

// ---------------------------------------------------------------------------
// The mix graph's INPUTS (Swift `AudioTimelineBuilder.audioInputs`,
// `placedSegments`, `scaledSegments`): which layers make sound, which slice
// of their file plays where on the output timeline, and at what level. The
// executor differs per host — AVFoundation in the apps, `mix_chunk` in the
// CLI — but the graph they execute is decided here, once.

/// While any focused layer plays, every other audible track is ducked to
/// this fraction of its amplitude (≈ −14 dB). Swift `duckFactor`.
pub const DUCK_FACTOR: f32 = 0.2;
/// Seconds of linear fade into and out of a duck, so it never clicks.
/// Swift `duckRamp`.
pub const DUCK_RAMP: f64 = 0.1;

/// One source slice placed on the output timeline. Swift `PlacedSegment`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedSegment {
    pub source_start: f64,
    pub duration: f64,
    pub output_start: f64,
}

/// Swift `placedSegments`: lay a layer's trim `ranges` end to end from
/// `start_output`, stopping once `layer_limit` output seconds are consumed
/// (the last range truncated to fit). Extended pauses open silent gaps at
/// their media time and push everything after them later. Empty or inverted
/// ranges are skipped. Pure — no media is touched.
pub fn placed_segments(
    ranges: &[VideoTrimRange],
    start_output: f64,
    layer_limit: f64,
    extended_pauses: &[ExtendedPause],
) -> Vec<PlacedSegment> {
    if layer_limit <= 0.0 {
        return Vec::new();
    }
    struct MediaPause {
        media_time: f64,
        duration: f64,
    }
    let mut sorted: Vec<&ExtendedPause> = extended_pauses
        .iter()
        .filter(|p| p.duration > 0.000_1 && p.start_time < layer_limit)
        .collect();
    sorted.sort_by(|a, b| {
        a.start_time
            .partial_cmp(&b.start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut inserted = 0.0;
    let media_pauses: Vec<MediaPause> = sorted
        .iter()
        .map(|p| {
            let pause = MediaPause {
                media_time: (p.start_time - inserted).max(0.0),
                duration: p.duration,
            };
            inserted += p.duration;
            pause
        })
        .collect();

    let mut result = Vec::new();
    let mut media_cursor = 0.0;
    'ranges: for r in ranges.iter().filter(|r| r.end > r.start) {
        let media_end = media_cursor + (r.end - r.start);
        let cuts: Vec<f64> = media_pauses
            .iter()
            .map(|p| p.media_time)
            .filter(|&t| t > media_cursor + 0.000_1 && t < media_end - 0.000_1)
            .collect();
        let mut boundaries = vec![media_cursor];
        boundaries.extend(cuts);
        boundaries.push(media_end);
        for pair in boundaries.windows(2) {
            let (piece_start, piece_end) = (pair[0], pair[1]);
            let pause_offset: f64 = media_pauses
                .iter()
                .filter(|p| p.media_time <= piece_start + 0.000_1)
                .map(|p| p.duration)
                .sum();
            let local_output_start = piece_start + pause_offset;
            if local_output_start >= layer_limit {
                break 'ranges;
            }
            let segment_duration = (piece_end - piece_start).min(layer_limit - local_output_start);
            if segment_duration <= 0.0 {
                continue;
            }
            result.push(PlacedSegment {
                source_start: r.start + (piece_start - media_cursor),
                duration: segment_duration,
                output_start: start_output + local_output_start,
            });
        }
        media_cursor = media_end;
    }
    result
}

/// A `PlacedSegment` on the TIMELINE clock: the source slice is unchanged,
/// where it lands and how long it occupies the timeline are divided by the
/// playback speed. Swift `ScaledSegment`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaledSegment {
    pub source_start: f64,
    pub source_duration: f64,
    pub output_start: f64,
    pub output_duration: f64,
}

/// Swift `scaledSegments`: the concatenation walk runs on the source-side 1x
/// clock (ranges, pauses and the length cap all in source seconds), and only
/// the OUTPUT placement is divided by `speed` at the end. `layer_limit` is
/// timeline seconds, converted to source seconds on the way in.
pub fn scaled_segments(
    ranges: &[VideoTrimRange],
    start_output: f64,
    layer_limit: f64,
    extended_pauses: &[ExtendedPause],
    speed: f64,
) -> Vec<ScaledSegment> {
    let clamped = if speed.is_finite() {
        speed.clamp(0.1, 10.0)
    } else {
        1.0
    };
    placed_segments(ranges, 0.0, layer_limit * clamped, extended_pauses)
        .into_iter()
        .map(|seg| ScaledSegment {
            source_start: seg.source_start,
            source_duration: seg.duration,
            output_start: start_output + seg.output_start / clamped,
            output_duration: seg.duration / clamped,
        })
        .collect()
}

/// Where an input's sound comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioSource {
    /// A declared resource's file (by resource id).
    Resource(String),
    /// A caption's narration clip, by filename under `Resources/`.
    VoiceClip(String),
}

/// One audible layer's fully resolved contribution. Swift `AudioInput`,
/// minus the URL — resolving files is the host's job; the graph is not.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInput {
    pub layer_id: String,
    pub source: AudioSource,
    /// Source-time slices to include. None means the whole asset; an empty
    /// list means the person excluded everything.
    pub included_ranges: Option<Vec<VideoTrimRange>>,
    /// Where the layer begins on the output timeline (may be negative; the
    /// executor clamps).
    pub start_time: f64,
    /// Output-time length cap (`layer.duration`).
    pub duration_cap: Option<f64>,
    /// Held-frame spans on this video layer's output timeline.
    pub extended_pauses: Vec<ExtendedPause>,
    /// Constant playback volume 0…1, used when `volume_points` is None.
    pub volume: f32,
    /// Volume automation in OUTPUT time, fraction domain; None = constant.
    pub volume_points: Option<Vec<VolumePoint>>,
    /// Zero-based source audio tracks to omit (multi-track video).
    pub disabled_audio_track_indices: Vec<i64>,
    /// Use only the first source audio track (sound layers, narration).
    pub single_track: bool,
    /// Ducks every other input while it plays; never ducked itself.
    pub is_focused: bool,
    /// Playback rate; the audio is time-stretched with pitch preserved.
    pub speed: f64,
}

/// Swift `audioInputs`: every enabled, renderable layer that makes sound,
/// with the focus spans the ducking runs on. `is_renderable` is the host's
/// "its media is still there" answer (Swift `renderableLayers`) — a layer
/// whose file is gone contributes nothing rather than an asset that fails
/// to load. Layers are walked in `sortIndex` order, as the apps do.
/// A layer's gain automation as absolute volume points — the ramps of
/// `gain_ramp_segments` placed on the timeline at the layer's start, with
/// a step's pre-point pulled 1 ms early so a step stays a step.
fn automation_points(layer: &ProjectLayer, base: f32) -> Option<Vec<VolumePoint>> {
    let segments = gain_ramp_segments(layer, base);
    let first = segments.first()?;
    let offset = layer.start_time.max(0.0);
    let frac = |v: f32| v.clamp(0.0, 1.0);
    let mut points = vec![VolumePoint {
        time: offset + first.start_time,
        volume: frac(first.start_value),
    }];
    for s in &segments {
        points.push(VolumePoint {
            time: offset + s.end_time,
            volume: frac(s.end_value),
        });
    }
    // A step (a keyframe with no ramp into it) arrives as two points at
    // one time. Consecutive points describe a ramp, so left as they are
    // the step smeared back to the previous breakpoint — a gain cut to
    // 25% at 6s faded from 4s. Pull the pre-step point one millisecond
    // early: the mix now holds until the keyframe, as `layer_gain` says.
    for i in 1..points.len() {
        if (points[i].time - points[i - 1].time).abs() < 0.000_1 {
            let floor = if i >= 2 { points[i - 2].time } else { f64::MIN };
            let early = points[i].time - 0.001;
            if early > floor {
                points[i - 1].time = early;
            }
        }
    }
    Some(points)
}

/// The sound of a composition placed as a clip by `parent`: every
/// sound-making layer inside, with its window moved onto the parent's
/// clock — offset by the parent's start, scaled by the parent's speed,
/// clipped to the part of the composition the parent plays (trims, cut)
/// — and a start that lands mid-clip expressed as a source offset in the
/// nested resource's included ranges. Nested compositions recurse to the
/// depth the model allows. v1 leaves a nested caption's narration and the
/// parent's loop and extended pauses out; a nested clip's own pauses are
/// not offset by a parent trim.
fn nested_inputs(
    parent: &ProjectLayer,
    shown: &promo_model::ProjectResource,
    resources: &[promo_model::ProjectResource],
    is_renderable: &dyn Fn(&ProjectLayer) -> bool,
    depth: usize,
) -> (Vec<AudioInput>, Vec<(f64, f64)>) {
    let mut inputs = Vec::new();
    let mut focus = Vec::new();
    let Some(composition) = shown.composition.as_ref() else {
        return (inputs, focus);
    };
    if depth > promo_model::nesting::MAX_DEPTH {
        return (inputs, focus);
    }
    let view = crate::mapping::resource_for_cut(shown, parent.media_cut_id.as_deref());
    let rate = crate::mapping::effective_speed(&view).abs();
    let rate = if rate > 0.0001 { rate } else { 1.0 };
    let trim0 = view.trim_start.unwrap_or(0.0).max(0.0);
    let length = view.duration.unwrap_or(0.0).max(0.0);
    let trim1 = view.trim_end.unwrap_or(length).min(length).max(trim0);
    let p_start = parent.start_time.max(0.0);
    let played = (trim1 - trim0) / rate;
    let p_end = p_start + parent.duration.unwrap_or(played).max(0.0).min(played);
    // Composition clock -> parent clock.
    let map = |t: f64| p_start + (t - trim0) / rate;

    let mut layers: Vec<&ProjectLayer> = composition.layers.iter().collect();
    layers.sort_by_key(|l| l.sort_index);
    for layer in layers {
        if !layer.is_enabled || !is_renderable(layer) {
            continue;
        }
        if !matches!(
            layer.kind,
            ProjectLayerKind::Video | ProjectLayerKind::Audio
        ) {
            continue;
        }
        if let Some(inner) = promo_model::nesting::composition_of(layer, resources) {
            // A composition inside the composition: its inputs on the inner
            // clock, then moved onto ours.
            let (nested, spans) = nested_inputs(layer, inner, resources, is_renderable, depth + 1);
            for mut input in nested {
                let end = map(input.start_time + input.duration_cap.unwrap_or(0.0));
                input.start_time = map(input.start_time);
                input.duration_cap = Some((end.min(p_end) - input.start_time).max(0.0));
                input.speed *= rate;
                if let Some(points) = input.volume_points.as_mut() {
                    for point in points.iter_mut() {
                        point.time = map(point.time);
                    }
                }
                if input.duration_cap.unwrap_or(0.0) > 0.0 && input.start_time < p_end {
                    inputs.push(input);
                }
            }
            for (a, b) in spans {
                let (a, b) = (map(a).max(p_start), map(b).min(p_end));
                if b > a {
                    focus.push((a, b));
                }
            }
            continue;
        }
        let want = if layer.kind == ProjectLayerKind::Video {
            ProjectResourceKind::Video
        } else {
            ProjectResourceKind::Audio
        };
        let Some(stored) = layer
            .resource_id
            .as_deref()
            .and_then(|rid| resources.iter().find(|r| r.id == rid && r.kind == want))
        else {
            continue;
        };
        let res = crate::mapping::resource_for_cut(stored, layer.media_cut_id.as_deref());
        let ranges = crate::mapping::playback_video_trim_ranges(&res);
        let own_rate = crate::mapping::effective_speed(&res);
        let own_rate = if own_rate.abs() > 0.0001 {
            own_rate.abs()
        } else {
            1.0
        };
        let played_length = ranges
            .as_ref()
            .map(|rs| rs.iter().map(|r| (r.end - r.start).max(0.0)).sum())
            .unwrap_or_else(|| res.duration.unwrap_or(0.0))
            / own_rate;
        // The nested layer's window on the composition clock, clipped to
        // what the parent plays of it.
        let n_start = layer.start_time.max(0.0);
        let n_end = n_start + layer.duration.unwrap_or(played_length).max(0.0);
        let (c_start, c_end) = (n_start.max(trim0), n_end.min(trim1));
        if c_end <= c_start {
            continue;
        }
        // Seconds of the nested layer the parent never plays, in the nested
        // resource's own source time.
        let skipped_source = (c_start - n_start) * own_rate;
        let included_ranges = match ranges {
            Some(rs) => Some(shift_ranges(rs, skipped_source)),
            None if skipped_source > 0.0 => res.duration.map(|d| {
                vec![VideoTrimRange {
                    start: skipped_source.min(d),
                    end: d,
                }]
            }),
            None => None,
        };
        let start_time = map(c_start);
        let duration_cap = ((map(c_end)).min(p_end) - start_time).max(0.0);
        if duration_cap <= 0.0 || start_time >= p_end {
            continue;
        }
        let is_video = layer.kind == ProjectLayerKind::Video;
        let volume_points = automation_points(layer, res.effective_volume()).map(|points| {
            points
                .into_iter()
                .map(|point| VolumePoint {
                    time: map(point.time),
                    volume: point.volume,
                })
                .collect()
        });
        inputs.push(AudioInput {
            layer_id: layer.id.clone(),
            source: AudioSource::Resource(stored.id.clone()),
            included_ranges,
            start_time,
            duration_cap: Some(duration_cap),
            extended_pauses: if is_video {
                crate::mapping::extended_video_pauses(&res)
            } else {
                Vec::new()
            },
            volume: res.effective_volume(),
            volume_points,
            disabled_audio_track_indices: if is_video {
                res.disabled_audio_track_indices.clone()
            } else {
                Vec::new()
            },
            single_track: !is_video,
            is_focused: layer.is_audio_focused(),
            speed: crate::mapping::effective_speed(&res) * rate,
        });
        if layer.is_audio_focused() {
            focus.push((start_time, start_time + duration_cap));
        }
    }
    (inputs, focus)
}

/// Drops `seconds` of source from the front of `ranges`.
fn shift_ranges(ranges: Vec<VideoTrimRange>, seconds: f64) -> Vec<VideoTrimRange> {
    let mut left = seconds.max(0.0);
    let mut out = Vec::new();
    for range in ranges {
        let len = (range.end - range.start).max(0.0);
        if left >= len {
            left -= len;
            continue;
        }
        out.push(VideoTrimRange {
            start: range.start + left,
            end: range.end,
        });
        left = 0.0;
    }
    out
}

pub fn audio_inputs(
    project: &ProjectMetadata,
    is_renderable: &dyn Fn(&ProjectLayer) -> bool,
) -> (Vec<AudioInput>, Vec<(f64, f64)>) {
    let mut inputs = Vec::new();
    let mut focus = Vec::new();

    // Per-layer volume automation (output time, fraction domain): keyframe
    // values are absolute; the resource's own volume fills the gaps.
    let automation = |layer: &ProjectLayer, base: f32| automation_points(layer, base);

    let mut layers: Vec<&ProjectLayer> = project.layers.as_deref().unwrap_or(&[]).iter().collect();
    layers.sort_by_key(|l| l.sort_index);
    let resources = project.resources.as_deref().unwrap_or(&[]);

    for layer in layers {
        if !layer.is_enabled || !is_renderable(layer) {
            continue;
        }
        // What the layer plays when it names no duration: its trimmed
        // ranges (or the whole clip) at the rate it runs.
        let mut played_length = 0.0;
        match layer.kind {
            ProjectLayerKind::Video | ProjectLayerKind::Audio => {
                // A composition placed as a clip: its layers make sound on
                // the parent's clock — offset by the parent's start, scaled
                // by its speed, clipped to its window.
                if let Some(shown) = promo_model::nesting::composition_of(layer, resources) {
                    let (nested, spans) = nested_inputs(layer, shown, resources, is_renderable, 1);
                    inputs.extend(nested);
                    focus.extend(spans);
                    continue;
                }
                let want = if layer.kind == ProjectLayerKind::Video {
                    ProjectResourceKind::Video
                } else {
                    ProjectResourceKind::Audio
                };
                let Some(stored) = layer
                    .resource_id
                    .as_deref()
                    .and_then(|rid| resources.iter().find(|r| r.id == rid && r.kind == want))
                else {
                    continue;
                };
                // The layer's cut decides which part plays and at what speed.
                let res = crate::mapping::resource_for_cut(stored, layer.media_cut_id.as_deref());
                let ranges = crate::mapping::playback_video_trim_ranges(&res);
                let rate = crate::mapping::effective_speed(&res);
                let rate = if rate.abs() > 0.0001 { rate.abs() } else { 1.0 };
                played_length = ranges
                    .as_ref()
                    .map(|rs| rs.iter().map(|r| (r.end - r.start).max(0.0)).sum())
                    .unwrap_or_else(|| res.duration.unwrap_or(0.0))
                    / rate;
                let is_video = layer.kind == ProjectLayerKind::Video;
                inputs.push(AudioInput {
                    layer_id: layer.id.clone(),
                    source: AudioSource::Resource(stored.id.clone()),
                    included_ranges: ranges,
                    start_time: layer.start_time,
                    duration_cap: layer.duration,
                    extended_pauses: if is_video {
                        crate::mapping::extended_video_pauses(&res)
                    } else {
                        Vec::new()
                    },
                    volume: res.effective_volume(),
                    volume_points: automation(layer, res.effective_volume()),
                    disabled_audio_track_indices: if is_video {
                        res.disabled_audio_track_indices.clone()
                    } else {
                        Vec::new()
                    },
                    single_track: !is_video,
                    is_focused: layer.is_audio_focused(),
                    speed: crate::mapping::effective_speed(&res),
                });
            }
            ProjectLayerKind::Caption => {
                let caption_resource = layer.resource_id.as_deref().and_then(|rid| {
                    resources
                        .iter()
                        .find(|r| r.id == rid && r.kind == ProjectResourceKind::Caption)
                });
                let Some(clip) = caption_resource
                    .and_then(|r| r.caption_voice_clip.as_ref())
                    .or(layer.caption_voice_clip.as_ref())
                else {
                    continue;
                };
                let base = caption_resource
                    .map(|r| r.effective_volume())
                    .unwrap_or(1.0);
                inputs.push(AudioInput {
                    layer_id: layer.id.clone(),
                    source: AudioSource::VoiceClip(clip.filename.clone()),
                    included_ranges: None,
                    start_time: layer.start_time,
                    duration_cap: None,
                    extended_pauses: Vec::new(),
                    volume: base,
                    volume_points: automation(layer, base),
                    disabled_audio_track_indices: Vec::new(),
                    single_track: true,
                    is_focused: layer.is_audio_focused(),
                    speed: 1.0,
                });
            }
            _ => continue,
        }

        if layer.is_audio_focused() {
            let start = layer.start_time.max(0.0);
            // A layer with no duration plays its whole clip, so it ducks for
            // as long as it speaks — reading the absent duration as zero
            // made such a layer audible while ducking nothing.
            let end = start + layer.duration.unwrap_or(played_length).max(0.0);
            if end > start {
                focus.push((start, end));
            }
        }
    }
    (inputs, focus)
}
