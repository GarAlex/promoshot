//! The background gradient showing at a given time.
//!
//! It follows `layer_background_color_hex` exactly — a background layer's
//! keyframes override the composition's default, on the same hold-then-ramp
//! timing — because a background that animated its colour by one rule and its
//! gradient by another would be impossible to reason about.
//!
//! What is new is that the thing being interpolated has SHAPE. Two gradients
//! can only be blended when they agree on kind, repeat mode and stop count;
//! anything else is a cut at the later keyframe. Blending a three-stop linear
//! into a five-stop radial has no meaning, and inventing one would produce
//! something nobody asked for at every frame in between.

use promo_model::{
    BackgroundGradient, CompositionSettings, GradientStop, ProjectLayer, ProjectLayerKeyframe,
};

use crate::interpolation::{interpolate_color_hex, layer_local_time, ramp_seconds};

/// Linearly between two gradients of the same shape. `progress` outside 0…1
/// is clamped by the caller.
fn blend(
    a: &BackgroundGradient,
    b: &BackgroundGradient,
    progress: f64,
    settings: &CompositionSettings,
) -> BackgroundGradient {
    let lerp = |x: f64, y: f64| x + (y - x) * progress;
    let (from, to) = (a.resolved_stops(), b.resolved_stops());
    // Callers hand in RESOLVED gradients (geometry made concrete against
    // the plate), so the unwraps below cannot fire on real input.
    let fallback = BackgroundGradient::default_geometry(b.kind);
    let (a_start, a_end) = (a.start.unwrap_or(fallback.0), a.end.unwrap_or(fallback.1));
    let (b_start, b_end) = (b.start.unwrap_or(fallback.0), b.end.unwrap_or(fallback.1));
    BackgroundGradient {
        kind: b.kind,
        repeat: b.repeat,
        start: Some(promo_model::Point(
            lerp(a_start.x(), b_start.x()),
            lerp(a_start.y(), b_start.y()),
        )),
        end: Some(promo_model::Point(
            lerp(a_end.x(), b_end.x()),
            lerp(a_end.y(), b_end.y()),
        )),
        stops: from
            .iter()
            .zip(to.iter())
            .map(|(x, y)| GradientStop {
                // Resolve before mixing, like every colour lerp: a `@name`
                // cannot be averaged. The blended stop is a literal, so the
                // re-resolve at the scene builder is a no-op.
                color_hex: interpolate_color_hex(
                    settings.resolve_color(&x.color_hex),
                    settings.resolve_color(&y.color_hex),
                    progress,
                ),
                at: lerp(x.at, y.at),
            })
            .collect(),
    }
}

/// The gradient a background layer shows at `time`, or `None` when the
/// background is a flat colour.
pub fn layer_background_gradient(
    layer: &ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
) -> Option<BackgroundGradient> {
    let local_time = layer_local_time(layer, time);
    let keyed: Vec<&ProjectLayerKeyframe> = {
        let mut keyed: Vec<&ProjectLayerKeyframe> = layer
            .keyframes
            .iter()
            .filter(|k| k.gradient.is_some())
            .collect();
        keyed.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        keyed
    };
    // `defaults.background_gradient` carries the PLATE's gradient when a
    // background resource is bound (the engine merges it in), so resolving
    // every keyframed gradient against it is what makes absent
    // angle/width/repeat mean "the plate's, at every read".
    let base = defaults.background_gradient.as_ref();
    if keyed.is_empty() {
        return base.map(|gradient| gradient.resolved_geometry(None));
    }
    let at = |k: &ProjectLayerKeyframe| {
        k.gradient
            .clone()
            .expect("filtered")
            .resolved_geometry(base)
    };

    let first = keyed[0];
    let last = keyed[keyed.len() - 1];
    if local_time <= first.time {
        return Some(at(first));
    }
    if local_time >= last.time {
        return Some(at(last));
    }
    for pair in keyed.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_time < a.time || local_time > b.time {
            continue;
        }
        let (from, to) = (at(a), at(b));
        let gap = b.time - a.time;
        let transition = ramp_seconds(b, gap);
        let ramp_start = b.time - transition;
        if local_time < ramp_start {
            return Some(from);
        }
        // Shapes that cannot blend simply change over at the keyframe, the
        // way a colour would if it were unreadable.
        if !from.is_compatible_with(&to) {
            return Some(if local_time >= b.time { to } else { from });
        }
        let progress = if transition > 0.0 {
            ((local_time - ramp_start) / transition).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Same easing rule as every other track: a background that eases
        // while the layers over it do not would read as two different scenes.
        let eased = b
            .easing
            .unwrap_or(promo_model::Easing::Linear)
            .apply(progress);
        return Some(blend(&from, &to, eased, defaults));
    }
    Some(at(first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_model::{GradientKind, GradientRepeat, Point};

    fn gradient(json: &str) -> BackgroundGradient {
        serde_json::from_str(json).expect("gradient")
    }

    fn two_stop(from: &str, to: &str, start: [f64; 2], end: [f64; 2]) -> String {
        format!(
            r#"{{"kind": "linear", "repeat": "repeat",
                 "start": [{}, {}], "end": [{}, {}],
                 "stops": [{{"colorHex": "{from}", "at": 0}},
                           {{"colorHex": "{to}", "at": 1}}]}}"#,
            start[0], start[1], end[0], end[1]
        )
    }

    fn layer_with(keyframes: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": [{keyframes}]}}"#
        ))
        .expect("layer")
    }

    fn settings() -> CompositionSettings {
        CompositionSettings::default()
    }

    #[test]
    fn no_keyframes_falls_back_to_the_projects_own_gradient() {
        let layer = layer_with("");
        assert!(layer_background_gradient(&layer, 1.0, &settings()).is_none());

        let mut defaults = settings();
        defaults.background_gradient = Some(gradient(&two_stop(
            "000000",
            "FFFFFF",
            [0.0, 0.0],
            [1.0, 0.0],
        )));
        let answer = layer_background_gradient(&layer, 1.0, &defaults).expect("gradient");
        assert_eq!(answer.stops.len(), 2);
    }

    /// The scroll, which is the reason the geometry animates at all: with a
    /// repeating ramp, shifting start and end by exactly one period lands on
    /// a pattern identical to where it began — so a loop has no seam.
    #[test]
    fn shifting_a_repeating_ramp_by_one_period_is_seamless() {
        let layer = layer_with(&format!(
            r#"{{"id": "A", "time": 0, "transitionDuration": 0,
                 "gradient": {}}},
               {{"id": "B", "time": 4, "transitionPercent": 100,
                 "transitionDuration": 4, "gradient": {}}}"#,
            two_stop("112244", "AACCFF", [0.0, 0.0], [0.25, 0.0]),
            two_stop("112244", "AACCFF", [0.25, 0.0], [0.5, 0.0]),
        ));
        let start = layer_background_gradient(&layer, 0.0, &settings()).expect("start");
        let end = layer_background_gradient(&layer, 4.0, &settings()).expect("end");
        // One period along: the axis has moved by its own length.
        assert!((end.start.unwrap().x() - start.end.unwrap().x()).abs() < 1e-9);
        assert_eq!(start.effective_repeat(), GradientRepeat::Repeat);

        // Halfway, the axis is halfway — the geometry is what animates, not
        // the colours, which is what makes it read as motion rather than as
        // a cross-fade.
        let middle = layer_background_gradient(&layer, 2.0, &settings()).expect("middle");
        assert!(
            (middle.start.unwrap().x() - 0.125).abs() < 1e-9,
            "{:?}",
            middle.start
        );
        assert_eq!(middle.stops[0].color_hex, "112244", "colours unchanged");
    }

    /// A keyframe that only recolours — no start/end — INHERITS the
    /// plate's geometry at every read: re-angle the plate later and the
    /// keyframed look follows, because nothing was frozen at write.
    #[test]
    fn a_geometryless_keyframe_pulls_the_plates_angle_at_read() {
        let layer = layer_with(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "gradient": {"kind": "linear",
                             "stops": [{"colorHex": "FF0000", "at": 0},
                                       {"colorHex": "00FF00", "at": 1}]}}"#,
        );
        let mut defaults = settings();
        defaults.background_gradient = Some(BackgroundGradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop { color_hex: "000000".into(), at: 0.0 },
                GradientStop { color_hex: "FFFFFF".into(), at: 1.0 },
            ],
            start: Some(Point(0.0, 0.5)),
            end: Some(Point(1.0, 0.5)),
            repeat: Some(GradientRepeat::Repeat),
        });
        let resolved = layer_background_gradient(&layer, 0.0, &defaults).expect("gradient");
        // The keyframe's colours…
        assert_eq!(resolved.stops[0].color_hex, "FF0000");
        // …on the PLATE's axis and repeat.
        assert_eq!(resolved.start, Some(Point(0.0, 0.5)));
        assert_eq!(resolved.end, Some(Point(1.0, 0.5)));
        assert_eq!(resolved.effective_repeat(), GradientRepeat::Repeat);

        // The plate re-angles; the same keyframe follows at the next read.
        defaults.background_gradient.as_mut().unwrap().start = Some(Point(0.5, 0.0));
        defaults.background_gradient.as_mut().unwrap().end = Some(Point(0.5, 1.0));
        let followed = layer_background_gradient(&layer, 0.0, &defaults).expect("gradient");
        assert_eq!(followed.start, Some(Point(0.5, 0.0)));
        assert_eq!(followed.stops[0].color_hex, "FF0000");
    }

    #[test]
    fn colours_and_positions_blend_when_the_shape_matches() {
        let layer = layer_with(
            r#"{"id": "A", "time": 0, "transitionDuration": 0, "gradient":
                 {"kind": "linear", "start": [0, 0], "end": [1, 0],
                  "stops": [{"colorHex": "000000", "at": 0},
                            {"colorHex": "FFFFFF", "at": 1}]}},
               {"id": "B", "time": 2, "transitionDuration": 2, "transitionPercent": 100,
                "gradient":
                 {"kind": "linear", "start": [0, 0], "end": [1, 0],
                  "stops": [{"colorHex": "FFFFFF", "at": 0},
                            {"colorHex": "000000", "at": 1}]}}"#,
        );
        let middle = layer_background_gradient(&layer, 1.0, &settings()).expect("middle");
        assert_eq!(
            middle.stops[0].color_hex, "808080",
            "halfway between the ends"
        );
        assert_eq!(middle.stops[1].color_hex, "808080");
    }

    /// Shapes that cannot be blended must CUT, not produce something halfway
    /// that nobody wrote. A three-stop linear has no meaningful midpoint with
    /// a two-stop radial.
    #[test]
    fn an_incompatible_change_cuts_at_the_keyframe() {
        let layer = layer_with(
            r#"{"id": "A", "time": 0, "transitionDuration": 0, "gradient":
                 {"kind": "linear", "start": [0, 0], "end": [1, 0],
                  "stops": [{"colorHex": "000000", "at": 0},
                            {"colorHex": "888888", "at": 0.5},
                            {"colorHex": "FFFFFF", "at": 1}]}},
               {"id": "B", "time": 2, "transitionDuration": 2, "transitionPercent": 100,
                "gradient":
                 {"kind": "radial", "start": [0.5, 0.5], "end": [1, 0.5],
                  "stops": [{"colorHex": "FF0000", "at": 0},
                            {"colorHex": "0000FF", "at": 1}]}}"#,
        );
        let before = layer_background_gradient(&layer, 1.9, &settings()).expect("before");
        assert_eq!(before.kind, GradientKind::Linear);
        assert_eq!(before.stops.len(), 3, "still the first, whole");
        let after = layer_background_gradient(&layer, 2.0, &settings()).expect("after");
        assert_eq!(after.kind, GradientKind::Radial);
    }

    #[test]
    fn a_malformed_gradient_still_draws_something() {
        let none = gradient(r#"{"kind": "linear", "start": [0, 0], "end": [1, 0], "stops": []}"#);
        assert_eq!(none.resolved_stops().len(), 2, "never an empty ramp");
        let one = gradient(
            r#"{"kind": "linear", "start": [0, 0], "end": [1, 0],
                "stops": [{"colorHex": "123456", "at": 0.4}]}"#,
        );
        let resolved = one.resolved_stops();
        assert_eq!(resolved.len(), 2, "one colour is a flat fill");
        assert_eq!(resolved[0].color_hex, "123456");
        assert_eq!(resolved[0].at, 0.0);
        assert_eq!(resolved[1].at, 1.0);

        // Out of order and out of range: sorted and clamped, not refused.
        let messy = gradient(
            r#"{"kind": "linear", "start": [0, 0], "end": [1, 0],
                "stops": [{"colorHex": "FFFFFF", "at": 2.5},
                          {"colorHex": "000000", "at": -1}]}"#,
        );
        let resolved = messy.resolved_stops();
        assert_eq!(resolved[0].color_hex, "000000");
        assert_eq!(resolved[0].at, 0.0);
        assert_eq!(resolved[1].at, 1.0);
    }

    #[test]
    fn point_accessors_match_the_wire_order() {
        let g = gradient(
            r#"{"kind": "radial", "start": [0.25, 0.75], "end": [1, 0.5],
                "stops": [{"colorHex": "000000", "at": 0},
                          {"colorHex": "FFFFFF", "at": 1}]}"#,
        );
        assert_eq!(g.start, Some(Point(0.25, 0.75)));
        assert_eq!(g.end, Some(Point(1.0, 0.5)));
    }

    /// Stops that name palette colours blend exactly like written-out ones.
    /// The blend averages colours, and an average of two `@names` is only
    /// possible after resolving them.
    #[test]
    fn named_stops_blend_like_literal_ones() {
        let mut defaults = settings();
        defaults.palette = Some(
            serde_json::from_str(
                r#"[{"name": "night", "colorHex": "000000"},
                    {"name": "day", "colorHex": "FFFFFF"}]"#,
            )
            .expect("palette"),
        );
        let keyframes = |from: &str, to: &str| {
            format!(
                r#"{{"id": "A", "time": 0, "transitionDuration": 0, "gradient": {}}},
                   {{"id": "B", "time": 4, "transitionDuration": 4, "gradient": {}}}"#,
                two_stop(from, from, [0.0, 0.0], [1.0, 0.0]),
                two_stop(to, to, [0.0, 0.0], [1.0, 0.0]),
            )
        };
        let named = layer_with(&keyframes("@night", "@day"));
        let literal = layer_with(&keyframes("000000", "FFFFFF"));
        let named_mid = layer_background_gradient(&named, 2.0, &defaults).expect("named");
        let literal_mid = layer_background_gradient(&literal, 2.0, &defaults).expect("literal");
        assert_eq!(named_mid.stops[0].color_hex, literal_mid.stops[0].color_hex);
        assert_eq!(named_mid.stops[0].color_hex, "808080", "a real mix");
        // On a plateau the stored gradient passes through untouched, stops
        // still carrying their references for the scene builder to resolve.
        let plateau = layer_background_gradient(&named, 0.0, &defaults).expect("plateau");
        assert_eq!(plateau.stops[0].color_hex, "@night");
    }
}
