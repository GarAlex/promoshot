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
/// The keyframes still decide WHERE the layer starts and ends and WHEN it
/// moves; the path only replaces the straight line between two positions with
/// a drawn one. Everything else — zoom, the hold-then-ease window,
/// `transitionPercent` — is untouched, which is why a path needs no keyframe
/// of its own.
pub fn layer_transform_along_paths(
    layer: &ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
    resources: &[promo_model::ProjectResource],
) -> Transform {
    let local_time = layer_local_time(layer, time);
    let sorted = sorted_by_time(&layer.keyframes, |k| {
        k.zoom.is_some() || k.vertical_shift.is_some() || k.horizontal_shift.is_some()
    });
    if sorted.is_empty() {
        return settings_interpolated_values(defaults, local_time);
    }
    let values = |k: &ProjectLayerKeyframe| Transform {
        zoom: k.zoom.unwrap_or(1.0),
        vertical_shift: k.vertical_shift.unwrap_or(0.0),
        horizontal_shift: k.horizontal_shift.unwrap_or(0.0),
    };
    let first = sorted[0];
    let last = sorted[sorted.len() - 1];
    if local_time <= first.time {
        return values(first);
    }
    if local_time >= last.time {
        return values(last);
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time >= a.time && local_time <= b.time {
            let gap = b.time - a.time;
            let effective_transition = ramp_seconds(b, gap);
            let transition_start = b.time - effective_transition;
            if local_time < transition_start {
                return values(a);
            }
            let progress = if effective_transition > 0.0 {
                (local_time - transition_start) / effective_transition
            } else {
                1.0
            };
            let av = values(a);
            let bv = values(b);
            let zoom = av.zoom + (bv.zoom - av.zoom) * progress;
            // A path moves the pair of shifts TOGETHER — they stop being two
            // independent scalars and become one point travelling a curve.
            if let Some(path) = b.motion_path.as_ref() {
                if let Some(polyline) = crate::motion::path_polyline(resources, path) {
                    let point = crate::motion::point_along_range(
                        &polyline,
                        promo_model::Point(av.horizontal_shift, av.vertical_shift),
                        promo_model::Point(bv.horizontal_shift, bv.vertical_shift),
                        path.flipped.unwrap_or(false),
                        path.start_at.unwrap_or(0.0),
                        path.end_at.unwrap_or(1.0),
                        progress,
                    );
                    return Transform {
                        zoom,
                        horizontal_shift: point.x(),
                        vertical_shift: point.y(),
                    };
                }
                // A path that cannot be resolved (resource gone, stroke
                // deleted, a shape that is not a route) falls back to the
                // straight line rather than pinning the layer anywhere
                // surprising.
            }
            return Transform {
                zoom,
                vertical_shift: av.vertical_shift
                    + (bv.vertical_shift - av.vertical_shift) * progress,
                horizontal_shift: av.horizontal_shift
                    + (bv.horizontal_shift - av.horizontal_shift) * progress,
            };
        }
    }
    values(first)
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
