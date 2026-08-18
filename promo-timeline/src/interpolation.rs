//! Keyframe interpolation — layer transforms, rotation/tilt scalars, colors.
//! Hold-then-ease semantics: between keyframes A and B the value holds at A
//! until `B.time - min(B.transitionDuration, gap)`, then eases linearly into
//! B. Mirrors `ProjectLayer.transform(at:)` / `interpolatedScalar` /
//! `backgroundColorHex(at:)` and the `CompositionSettings` twins.

use promo_model::{CompositionSettings, ProjectLayer, ProjectLayerKeyframe};

/// (zoom, verticalShift, horizontalShift) — Swift's transform tuple.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub zoom: f64,
    pub vertical_shift: f64,
    pub horizontal_shift: f64,
}

fn sorted_by_time<F>(keyframes: &[ProjectLayerKeyframe], has_field: F) -> Vec<&ProjectLayerKeyframe>
where
    F: Fn(&ProjectLayerKeyframe) -> bool,
{
    let mut sorted: Vec<_> = keyframes.iter().filter(|k| has_field(k)).collect();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted
}

/// Swift `ProjectLayer.localTime(for:)`.
/// Seconds of ramp before `b`, from whichever unit the keyframe uses.
///
/// `transitionPercent` wins when present: it is the same quantity as
/// `transitionDuration` in units that survive retiming — 100 starts moving
/// immediately and arrives exactly at the keyframe, 0 holds still and is
/// simply there when it lands. Seconds are the older spelling and still
/// clamp to the gap.
pub fn ramp_seconds(b: &ProjectLayerKeyframe, gap: f64) -> f64 {
    match b.transition_percent {
        Some(percent) if percent.is_finite() => gap * (percent.clamp(0.0, 100.0) / 100.0),
        _ => b.transition_duration.min(gap),
    }
}

pub fn layer_local_time(layer: &ProjectLayer, global_time: f64) -> f64 {
    (global_time - layer.start_time).max(0.0)
}

/// Swift `ProjectLayer.isVisible(at:)`.
pub fn layer_is_visible(layer: &ProjectLayer, time: f64) -> bool {
    if !layer.is_enabled || time < layer.start_time {
        return false;
    }
    match layer.duration {
        Some(duration) => time < layer.start_time + duration,
        None => true,
    }
}

/// Swift `ProjectLayer.transform(at:defaults:)`.
pub fn layer_transform(
    layer: &ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
) -> Transform {
    layer_transform_along_paths(layer, time, defaults, &[])
}

/// `layer_transform`, with the project's resources on hand so a keyframe
/// carrying a `motionPath` can bend the route to it.
///
/// Zoom and position are SEPARATE tracks: a keyframe speaks only for the
/// fields it carries, so zoom keyed at 0/60/120 coexists with movement keyed
/// at 0/0.5/1.5 on one layer, each ramp using its own keyframe's
/// `transitionDuration`/`transitionPercent`. Two keyframes may share a time —
/// one per type — which is how a zoom ramps through only the last second of
/// a thirty-second move. Within ONE track a tie resolves by ARRAY ORDER, the
/// later keyframe winning from that instant on: the list is authoritative
/// the same way layer order is for z. (The two shifts stay one track — a
/// position is a point, and a motion path moves it as one.)
///
/// A keyframe carrying zoom but no shifts used to be a position waypoint at
/// the DEFAULTS (0,0) — it yanked the layer and split the chord a motion
/// path fits onto. Now it simply is not on the position track.
pub fn layer_transform_along_paths(
    layer: &ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
    resources: &[promo_model::ProjectResource],
) -> Transform {
    let local_time = layer_local_time(layer, time);
    let zoom_track = sorted_by_time(&layer.keyframes, |k| k.zoom.is_some());
    let position_track = sorted_by_time(&layer.keyframes, |k| {
        k.vertical_shift.is_some() || k.horizontal_shift.is_some()
    });
    // No keyframe carries any of the three: the legacy settings timeline
    // stands, exactly as before the split. One EMPTY track falls back to its
    // constant (zoom 1, position 0,0) — the same numbers the fused track
    // produced for it — so a zoom-only or move-only layer renders
    // bit-identically to before.
    if zoom_track.is_empty() && position_track.is_empty() {
        return settings_interpolated_values(defaults, local_time);
    }
    let zoom = match track_window(&zoom_track, local_time) {
        None => 1.0,
        Some((a, b, progress)) => {
            let av = a.zoom.unwrap_or(1.0);
            av + (b.zoom.unwrap_or(1.0) - av) * progress
        }
    };
    let (horizontal_shift, vertical_shift) = match track_window(&position_track, local_time) {
        None => (0.0, 0.0),
        Some((a, b, progress)) => {
            let (ah, av) = (
                a.horizontal_shift.unwrap_or(0.0),
                a.vertical_shift.unwrap_or(0.0),
            );
            let (bh, bv) = (
                b.horizontal_shift.unwrap_or(0.0),
                b.vertical_shift.unwrap_or(0.0),
            );
            // A path moves the pair of shifts TOGETHER — they stop being two
            // independent scalars and become one point travelling a curve.
            // Only a genuine ramp between two keyframes takes it: a hold or
            // an end clamp (a == b) has no chord to fit the stroke onto.
            let path_point = if !std::ptr::eq(a, b) {
                b.motion_path.as_ref().and_then(|path| {
                    crate::motion::path_polyline(resources, path).map(|polyline| {
                        crate::motion::point_along_range(
                            &polyline,
                            promo_model::Point(ah, av),
                            promo_model::Point(bh, bv),
                            path.flipped.unwrap_or(false),
                            path.start_at.unwrap_or(0.0),
                            path.end_at.unwrap_or(1.0),
                            progress,
                        )
                    })
                    // A path that cannot be resolved (resource gone, stroke
                    // deleted, a shape that is not a route) falls back to the
                    // straight line rather than pinning the layer anywhere
                    // surprising.
                })
            } else {
                None
            };
            match path_point {
                Some(point) => (point.x(), point.y()),
                None => (ah + (bh - ah) * progress, av + (bv - av) * progress),
            }
        }
    };
    Transform {
        zoom,
        vertical_shift,
        horizontal_shift,
    }
}

/// One track's hold-then-ramp scan: the pair of keyframes bracketing
/// `local_time` and the ramp progress between them. Before the first
/// keyframe, after the last, and during a hold the pair degenerates to
/// `(k, k, 1.0)` — the value simply IS that keyframe's. `None` on an empty
/// track, so the caller owns the track's resting constant.
fn track_window<'a>(
    sorted: &[&'a ProjectLayerKeyframe],
    local_time: f64,
) -> Option<(&'a ProjectLayerKeyframe, &'a ProjectLayerKeyframe, f64)> {
    let first = *sorted.first()?;
    let last = *sorted.last()?;
    if local_time <= first.time {
        return Some((first, first, 1.0));
    }
    if local_time >= last.time {
        return Some((last, last, 1.0));
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = ramp_seconds(b, gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return Some((a, a, 1.0));
            }
            let progress = if effective_transition > 0.0 {
                (local_time - transition_start) / effective_transition
            } else {
                1.0
            };
            return Some((a, b, progress));
        }
    }
    Some((first, first, 1.0))
}

/// Swift `ProjectLayer.interpolatedScalar(atLocalTime:_:)` — interpolates one
/// optional keyframe field; `None` when no keyframe defines it.
pub fn layer_interpolated_scalar<F>(layer: &ProjectLayer, local_time: f64, select: F) -> Option<f64>
where
    F: Fn(&ProjectLayerKeyframe) -> Option<f64>,
{
    let sorted = sorted_by_time(&layer.keyframes, |k| select(k).is_some());
    let first = *sorted.first()?;
    let last = *sorted.last()?;
    if local_time <= first.time {
        return select(first);
    }
    if local_time >= last.time {
        return select(last);
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = ramp_seconds(b, gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return select(a);
            }
            let progress = if effective_transition > 0.0 {
                (local_time - transition_start) / effective_transition
            } else {
                1.0
            };
            let av = select(a).unwrap_or(0.0);
            let bv = select(b).unwrap_or(0.0);
            return Some(av + (bv - av) * progress);
        }
    }
    select(first)
}

/// Swift `ProjectLayer.rotation(at:)` — degrees, 0 when unkeyed.
pub fn layer_rotation(layer: &ProjectLayer, time: f64) -> f64 {
    layer_interpolated_scalar(layer, layer_local_time(layer, time), |k| k.rotation).unwrap_or(0.0)
}

/// Layer opacity at `time` — 0…1, and 1 when the layer has no opacity
/// keyframes, so an un-keyed layer is fully visible as before.
pub fn layer_opacity(layer: &ProjectLayer, time: f64) -> f64 {
    layer_interpolated_scalar(layer, layer_local_time(layer, time), |k| k.opacity)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0)
}

/// Swift `ProjectLayer.hasTiltKeyframes`.
pub fn layer_has_tilt_keyframes(layer: &ProjectLayer) -> bool {
    layer
        .keyframes
        .iter()
        .any(|k| k.tilt_x.is_some() || k.tilt_y.is_some())
}

/// Swift `ProjectLayer.tiltKeyframeOffset(at:)` — `(x, y)` degrees, `None`
/// when the layer has no tilt keyframes.
pub fn layer_tilt_offset(layer: &ProjectLayer, time: f64) -> Option<(f64, f64)> {
    if !layer_has_tilt_keyframes(layer) {
        return None;
    }
    let lt = layer_local_time(layer, time);
    Some((
        layer_interpolated_scalar(layer, lt, |k| k.tilt_x).unwrap_or(0.0),
        layer_interpolated_scalar(layer, lt, |k| k.tilt_y).unwrap_or(0.0),
    ))
}

/// The caption style values a layer's keyframes ask for at `time`.
///
/// Swift `ProjectLayer.captionStyle(at:defaults:)`. A caption keyframe reuses
/// the media-layer fields for something else entirely, and this mapping is the
/// app's, not an invention here:
///
///   zoom            -> font size
///   verticalShift   -> vertical margin (from the TOP of the canvas)
///   horizontalShift -> left margin
///
/// A field the keyframe omits falls back to the BASE style, not to the
/// previous keyframe — again matching the app. `None` means the layer keys
/// none of the three, so the caption is static and the base style stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionValues {
    pub font_size: f64,
    pub vertical_margin: f64,
    pub left_margin: f64,
}

pub fn layer_caption_values(
    layer: &ProjectLayer,
    time: f64,
    base: CaptionValues,
) -> Option<CaptionValues> {
    let sorted = sorted_by_time(&layer.keyframes, |k| {
        k.zoom.is_some() || k.vertical_shift.is_some() || k.horizontal_shift.is_some()
    });
    if sorted.is_empty() {
        return None;
    }
    let values = |k: &ProjectLayerKeyframe| CaptionValues {
        font_size: k.zoom.unwrap_or(base.font_size),
        vertical_margin: k.vertical_shift.unwrap_or(base.vertical_margin),
        left_margin: k.horizontal_shift.unwrap_or(base.left_margin),
    };
    let local_time = layer_local_time(layer, time);
    let first = sorted[0];
    let last = sorted[sorted.len() - 1];
    if local_time <= first.time {
        return Some(values(first));
    }
    if local_time >= last.time {
        return Some(values(last));
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = ramp_seconds(b, gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return Some(values(a));
            }
            let progress = if effective_transition > 0.0 {
                (local_time - transition_start) / effective_transition
            } else {
                1.0
            };
            let (av, bv) = (values(a), values(b));
            let lerp = |x: f64, y: f64| x + (y - x) * progress;
            return Some(CaptionValues {
                font_size: lerp(av.font_size, bv.font_size),
                vertical_margin: lerp(av.vertical_margin, bv.vertical_margin),
                left_margin: lerp(av.left_margin, bv.left_margin),
            });
        }
    }
    Some(values(first))
}

/// Swift `ProjectLayer.backgroundColorHex(at:defaults:)`.
pub fn layer_background_color_hex(
    layer: &ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
) -> String {
    let local_time = layer_local_time(layer, time);
    let sorted = sorted_by_time(&layer.keyframes, |k| k.color_hex.is_some());
    if sorted.is_empty() {
        return defaults.background_color_hex.clone();
    }
    let color = |k: &ProjectLayerKeyframe| -> String {
        k.color_hex
            .clone()
            .unwrap_or_else(|| defaults.background_color_hex.clone())
    };
    let first = sorted[0];
    let last = sorted[sorted.len() - 1];
    if local_time <= first.time {
        return color(first);
    }
    if local_time >= last.time {
        return color(last);
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = ramp_seconds(b, gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return color(a);
            }
            let progress = if effective_transition > 0.0 {
                (local_time - transition_start) / effective_transition
            } else {
                1.0
            };
            return interpolate_color_hex(&color(a), &color(b), progress);
        }
    }
    color(first)
}

/// Swift `CompositionSettings.interpolatedValues(at:)`.
pub fn settings_interpolated_values(settings: &CompositionSettings, time: f64) -> Transform {
    let mut sorted: Vec<_> = settings.video_keyframes.iter().collect();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let Some(first) = sorted.first().copied() else {
        return Transform {
            zoom: 1.0,
            vertical_shift: 0.0,
            horizontal_shift: 0.0,
        };
    };
    let last = sorted[sorted.len() - 1];
    if time <= first.time {
        return Transform {
            zoom: first.zoom,
            vertical_shift: first.vertical_shift,
            horizontal_shift: first.horizontal_shift,
        };
    }
    if time >= last.time {
        return Transform {
            zoom: last.zoom,
            vertical_shift: last.vertical_shift,
            horizontal_shift: last.horizontal_shift,
        };
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if time >= a.time && time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = b.transition_duration.min(gap);
            let transition_start = b.time - effective_transition;
            if time < transition_start {
                return Transform {
                    zoom: a.zoom,
                    vertical_shift: a.vertical_shift,
                    horizontal_shift: a.horizontal_shift,
                };
            }
            let t = if effective_transition > 0.0 {
                (time - transition_start) / effective_transition
            } else {
                1.0
            };
            return Transform {
                zoom: a.zoom + (b.zoom - a.zoom) * t,
                vertical_shift: a.vertical_shift + (b.vertical_shift - a.vertical_shift) * t,
                horizontal_shift: a.horizontal_shift
                    + (b.horizontal_shift - a.horizontal_shift) * t,
            };
        }
    }
    Transform {
        zoom: first.zoom,
        vertical_shift: first.vertical_shift,
        horizontal_shift: first.horizontal_shift,
    }
}

/// Swift `CompositionSettings.backgroundColorHex(at:)`. Note the Swift twin
/// clamps the ease span to ≥ 0.0001 (unlike the layer variant).
pub fn settings_background_color_hex(settings: &CompositionSettings, time: f64) -> String {
    let mut sorted: Vec<_> = settings.background_keyframes.iter().collect();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let Some(first) = sorted.first().copied() else {
        return settings.background_color_hex.clone();
    };
    let last = sorted[sorted.len() - 1];
    if time <= first.time {
        return first.color_hex.clone();
    }
    if time >= last.time {
        return last.color_hex.clone();
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if time >= a.time && time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = b.transition_duration.min(gap);
            let transition_start = b.time - effective_transition;
            if time < transition_start {
                return a.color_hex.clone();
            }
            let span = effective_transition.max(0.0001);
            let t = (time - transition_start) / span;
            return interpolate_color_hex(&a.color_hex, &b.color_hex, t);
        }
    }
    first.color_hex.clone()
}

/// Swift `CompositionSettings.interpolateColorHex(from:to:progress:)`.
pub fn interpolate_color_hex(start: &str, end: &str, progress: f64) -> String {
    let (Some(a), Some(b)) = (rgb(start), rgb(end)) else {
        return if progress < 1.0 {
            start.to_string()
        } else {
            end.to_string()
        };
    };
    let p = progress.clamp(0.0, 1.0);
    let r = a.0 + (b.0 - a.0) * p;
    let g = a.1 + (b.1 - a.1) * p;
    let bl = a.2 + (b.2 - a.2) * p;
    format!(
        "{:02X}{:02X}{:02X}",
        r.round() as i64,
        g.round() as i64,
        bl.round() as i64
    )
}

fn rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let mut value = hex.trim().to_uppercase();
    if let Some(stripped) = value.strip_prefix('#') {
        value = stripped.to_string();
    }
    if value.len() != 6 {
        return None;
    }
    let parsed = u64::from_str_radix(&value, 16).ok()?;
    Some((
        ((parsed & 0xFF0000) >> 16) as f64,
        ((parsed & 0x00FF00) >> 8) as f64,
        (parsed & 0x0000FF) as f64,
    ))
}

#[cfg(test)]
mod opacity_tests {
    use super::*;

    fn layer_with(keys: &str) -> ProjectLayer {
        let json = format!(
            r#"{{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, "keyframes": [{keys}]}}"#
        );
        serde_json::from_str(&json).expect("layer")
    }

    /// The default matters most: every existing project has no opacity
    /// keyframes and must keep rendering at full strength.
    #[test]
    fn an_unkeyed_layer_is_fully_opaque() {
        let layer = layer_with(r#"{"id": "K", "time": 0, "zoom": 1, "transitionDuration": 0}"#);
        assert_eq!(layer_opacity(&layer, 0.0), 1.0);
        assert_eq!(layer_opacity(&layer, 5.0), 1.0);
    }

    #[test]
    fn opacity_ramps_between_keyframes() {
        let layer = layer_with(
            r#"{"id": "A", "time": 0, "opacity": 0.0, "transitionDuration": 0},
               {"id": "B", "time": 1, "opacity": 1.0, "transitionDuration": 1}"#,
        );
        assert_eq!(layer_opacity(&layer, 0.0), 0.0);
        assert!((layer_opacity(&layer, 0.5) - 0.5).abs() < 1e-9, "midpoint");
        assert_eq!(layer_opacity(&layer, 1.0), 1.0);
        assert_eq!(layer_opacity(&layer, 9.0), 1.0, "holds after the last key");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let layer = layer_with(
            r#"{"id": "A", "time": 0, "opacity": -2.0, "transitionDuration": 0},
               {"id": "B", "time": 1, "opacity": 5.0, "transitionDuration": 1}"#,
        );
        assert_eq!(layer_opacity(&layer, 0.0), 0.0);
        assert_eq!(layer_opacity(&layer, 1.0), 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(keyframes: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, "keyframes": [{keyframes}]}}"#
        ))
        .expect("layer")
    }

    fn settings(json: &str) -> CompositionSettings {
        serde_json::from_str(json).expect("settings")
    }

    fn transform(l: &ProjectLayer, t: f64) -> Transform {
        layer_transform(l, t, &settings(r#"{"canvasWidth": 64, "canvasHeight": 64}"#))
    }

    /// The user-facing promise: zoom keyed sparsely over a long move, each
    /// ramp on its own clock. Two keyframes SHARE t=30 — one carries the
    /// move's end (ramping the whole thirty seconds), one the zoom's end
    /// (ramping only the last one) — and neither speaks for the other.
    #[test]
    fn zoom_and_position_are_independent_tracks() {
        let l = layer(
            r#"{"id": "p0", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "z0", "time": 0, "transitionDuration": 0, "zoom": 1.0},
               {"id": "p1", "time": 30, "transitionPercent": 100,
                "transitionDuration": 0,
                "horizontalShift": 300, "verticalShift": 0},
               {"id": "z1", "time": 30, "transitionDuration": 1, "zoom": 2.0}"#,
        );
        // Mid-move: the layer has travelled half way and zoom has not begun.
        let mid = transform(&l, 15.0);
        assert_eq!(mid.horizontal_shift, 150.0);
        assert_eq!(mid.zoom, 1.0);
        // Last second: zoom is half way through ITS ramp, the move nearly done.
        let late = transform(&l, 29.5);
        assert_eq!(late.zoom, 1.5);
        assert_eq!(late.horizontal_shift, 295.0);
        let end = transform(&l, 30.0);
        assert_eq!((end.zoom, end.horizontal_shift), (2.0, 300.0));
    }

    /// The bug the split kills: a zoom-only keyframe between two move
    /// keyframes used to be a position waypoint at the DEFAULTS (0,0),
    /// yanking the layer through the origin mid-glide.
    #[test]
    fn a_zoom_only_keyframe_is_not_a_position_waypoint() {
        let l = layer(
            r#"{"id": "p0", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "z", "time": 5, "transitionDuration": 0, "zoom": 3.0},
               {"id": "p1", "time": 10, "transitionPercent": 100,
                "transitionDuration": 0,
                "horizontalShift": 100, "verticalShift": 0}"#,
        );
        let at5 = transform(&l, 5.0);
        assert_eq!(at5.horizontal_shift, 50.0, "half way, not yanked to 0");
        assert_eq!(at5.zoom, 3.0);
    }

    /// Two keyframes of ONE track at one instant: array order is
    /// authoritative, the later winning from that moment on — the same rule
    /// layer order plays for z.
    #[test]
    fn same_time_keys_in_one_track_resolve_by_list_order() {
        let l = layer(
            r#"{"id": "a", "time": 5, "transitionDuration": 0, "zoom": 2.0},
               {"id": "b", "time": 5, "transitionDuration": 0, "zoom": 4.0}"#,
        );
        assert_eq!(transform(&l, 5.1).zoom, 4.0, "later in the list wins");
        assert_eq!(transform(&l, 9.0).zoom, 4.0);
    }

    /// One empty track rests at its constant — the same numbers the fused
    /// track produced — so a zoom-only or move-only layer is unchanged.
    #[test]
    fn an_empty_track_rests_at_its_constant() {
        let zoom_only = layer(
            r#"{"id": "z", "time": 0, "transitionDuration": 0, "zoom": 2.0}"#,
        );
        let t = transform(&zoom_only, 3.0);
        assert_eq!((t.zoom, t.horizontal_shift, t.vertical_shift), (2.0, 0.0, 0.0));

        let move_only = layer(
            r#"{"id": "p", "time": 0, "transitionDuration": 0,
                "horizontalShift": 40, "verticalShift": 8}"#,
        );
        let t = transform(&move_only, 3.0);
        assert_eq!((t.zoom, t.horizontal_shift, t.vertical_shift), (1.0, 40.0, 8.0));
    }

    /// A layer whose keyframes carry NEITHER zoom nor shifts still falls
    /// back to the legacy settings timeline — the pre-layer model keeps
    /// rendering. Non-vacuous because the settings here answer 0.5, not the
    /// constants' 1.0.
    #[test]
    fn layers_without_trio_keys_still_use_the_settings_timeline() {
        let l = layer(r#"{"id": "o", "time": 0, "transitionDuration": 0, "opacity": 0.5}"#);
        let s = settings(
            r#"{"canvasWidth": 64, "canvasHeight": 64,
                "videoKeyframes": [{"time": 0, "zoom": 0.5, "verticalShift": 7}]}"#,
        );
        let t = layer_transform(&l, 3.0, &s);
        assert_eq!((t.zoom, t.vertical_shift), (0.5, 7.0));
    }

    /// A hold has no chord for a motion path to fit onto, so the path only
    /// bends the RAMP. Before the ramp begins the layer sits exactly at the
    /// previous keyframe — not somewhere on an unanchored curve.
    #[test]
    fn a_motion_path_bends_only_the_ramp() {
        let l: ProjectLayer = serde_json::from_str(
            r#"{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, "keyframes": [
                   {"id": "p0", "time": 0, "transitionDuration": 0,
                    "horizontalShift": 0, "verticalShift": 0},
                   {"id": "p1", "time": 10, "transitionPercent": 50,
                    "transitionDuration": 0,
                    "horizontalShift": 100, "verticalShift": 0,
                    "motionPath": {"pathResourceID": "PATH"}}]}"#,
        )
        .expect("layer");
        let resources: Vec<promo_model::ProjectResource> = vec![serde_json::from_str(
            r#"{"id": "PATH", "kind": "path", "filename": "", "displayName": "arc",
                "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
                "path": {"start": [0, 0], "end": [100, 0],
                         "controls": [[50, -60]]}}"#,
        )
        .expect("path resource")];
        let defaults = settings(r#"{"canvasWidth": 64, "canvasHeight": 64}"#);
        // Holding (ramp runs 5..10): exactly at the start, no curve applied.
        let held = layer_transform_along_paths(&l, 3.0, &defaults, &resources);
        assert_eq!((held.horizontal_shift, held.vertical_shift), (0.0, 0.0));
        // Mid-ramp the path pulls the layer OFF the straight line.
        let bent = layer_transform_along_paths(&l, 7.5, &defaults, &resources);
        assert!(bent.vertical_shift < -1.0, "curved above the chord, got {}", bent.vertical_shift);
    }
}
