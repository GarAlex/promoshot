//! Viewport — which part of the source a layer shows.
//!
//! A viewport is `[x, y, w, h]` in UNIT source coordinates: a window onto
//! the texture, mapped onto the layer's rect on the canvas. The rect itself
//! — position, zoom, corner radius, border — is untouched, which is the
//! point: a framed screen recording keeps its place on the canvas while its
//! content zooms toward whatever the app under recording is doing.
//!
//! The window RAMPS. Each of the four numbers interpolates on the ordinary
//! hold-then-ramp rule (`layer_interpolated_scalar`), so
//! `transitionDuration`/`transitionPercent` mean here exactly what they mean
//! for zoom, and the ramp is the visible pan. All four components come from
//! the same keyframes — a keyframe carries a whole viewport or none — so the
//! four interpolations always pick the same keyframe pairs and the window
//! never shears.
//!
//! Rules, both refusals:
//!
//! - Image and video layers only. A caption or drawing is laid out from its
//!   own content, a background has no source to window; handing them a
//!   viewport would mean inventing what it crops.
//! - Motion paths do not bend a viewport move. A path bends the LAYER's
//!   route across the canvas; a curved pan across screen content reads as
//!   drunk, so the window always travels straight.
//!
//! Before the first viewport keyframe the first one holds, the same rule as
//! zoom. A project that wants "full frame, then dive in" says so with an
//! explicit `[0, 0, 1, 1]` keyframe — absence is not an animation cue.

use promo_model::{ProjectLayer, ProjectLayerKind};

use crate::interpolation::{layer_interpolated_scalar, layer_local_time};

/// The narrowest a window may get: 1% of the source per axis. A zero-size
/// viewport would divide by zero in layout; anything under a percent is a
/// smear of a handful of source pixels and plainly an editing accident.
const MIN_SIZE: f64 = 0.01;

/// The layer's viewport at `time` (global), `None` when the layer has no
/// viewport keyframes — the whole source, exactly as before the feature
/// existed. Resolved values are clamped inside the source; the model stores
/// what was written, the resolver refuses to sample outside the texture.
pub fn layer_viewport(layer: &ProjectLayer, time: f64) -> Option<[f64; 4]> {
    match layer.kind {
        ProjectLayerKind::Image | ProjectLayerKind::Video => {}
        _ => return None,
    }
    let local = layer_local_time(layer, time);
    let at = |i: usize| layer_interpolated_scalar(layer, local, |k| k.viewport.map(|v| v[i]));
    Some(clamped([at(0)?, at(1)?, at(2)?, at(3)?]))
}

/// Clamp a window inside the unit square, size first so a window pushed
/// against an edge slides back in rather than shrinking.
/// The window a project ASKED for, when the renderer will not give it —
/// `Some(what_it_becomes)`, or `None` when the request is already legal.
///
/// Deliberately expressed as "run the real clamp and see if it moved",
/// rather than re-deriving the bounds: a second copy of the rule would drift
/// from `clamped` the first time the rule changed, and then the warning
/// would describe a correction that no longer happens.
pub fn out_of_bounds(v: [f64; 4]) -> Option<[f64; 4]> {
    let fixed = clamped(v);
    (fixed != v).then_some(fixed)
}

fn clamped(v: [f64; 4]) -> [f64; 4] {
    let w = v[2].clamp(MIN_SIZE, 1.0);
    let h = v[3].clamp(MIN_SIZE, 1.0);
    let x = v[0].clamp(0.0, 1.0 - w);
    let y = v[1].clamp(0.0, 1.0 - h);
    [x, y, w, h]
}

/// Map `inner` — unit coordinates of a window — inside `outer`, a region of
/// the texture already selected. This is how a viewport composes with a
/// sprite cell: the cell picks the frame out of the sheet, the viewport
/// picks the window out of THAT, and neither needs to know about the other.
pub fn compose_uv(outer: [f64; 4], inner: [f64; 4]) -> [f64; 4] {
    [
        outer[0] + inner[0] * outer[2],
        outer[1] + inner[1] * outer[3],
        inner[2] * outer[2],
        inner[3] * outer[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(kind: &str, keyframes: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id": "L", "name": "L", "sortIndex": 0, "kind": "{kind}",
                 "isEnabled": true, "startTime": 0, "keyframes": [{keyframes}]}}"#
        ))
        .expect("layer")
    }

    #[test]
    fn unkeyed_layer_has_no_viewport() {
        assert_eq!(layer_viewport(&layer("video", ""), 1.0), None);
    }

    #[test]
    fn only_image_and_video_layers_have_one() {
        let k = r#"{"id": "k", "time": 0, "transitionDuration": 0,
                    "viewport": [0.25, 0.25, 0.5, 0.5]}"#;
        assert_eq!(layer_viewport(&layer("caption", k), 0.0), None);
        assert_eq!(layer_viewport(&layer("drawing", k), 0.0), None);
        assert_eq!(layer_viewport(&layer("background", k), 0.0), None);
        assert_eq!(
            layer_viewport(&layer("image", k), 0.0),
            Some([0.25, 0.25, 0.5, 0.5])
        );
    }

    #[test]
    fn one_keyframe_holds_before_and_after() {
        let l = layer(
            "video",
            r#"{"id": "k", "time": 5, "transitionDuration": 0,
                "viewport": [0.1, 0.2, 0.5, 0.5]}"#,
        );
        assert_eq!(layer_viewport(&l, 0.0), Some([0.1, 0.2, 0.5, 0.5]));
        assert_eq!(layer_viewport(&l, 99.0), Some([0.1, 0.2, 0.5, 0.5]));
    }

    #[test]
    fn the_window_ramps_like_zoom() {
        // Full frame at t=0, quarter window at t=10, moving over the last
        // four seconds (transitionDuration 4 → ramp runs 6..10).
        let l = layer(
            "video",
            r#"{"id": "a", "time": 0, "transitionDuration": 0,
                "viewport": [0, 0, 1, 1]},
               {"id": "b", "time": 10, "transitionDuration": 4,
                "viewport": [0.5, 0.5, 0.5, 0.5]}"#,
        );
        // Holding before the ramp starts.
        assert_eq!(layer_viewport(&l, 6.0), Some([0.0, 0.0, 1.0, 1.0]));
        // Halfway through the ramp: every component halfway.
        assert_eq!(layer_viewport(&l, 8.0), Some([0.25, 0.25, 0.75, 0.75]));
        assert_eq!(layer_viewport(&l, 10.0), Some([0.5, 0.5, 0.5, 0.5]));
    }

    #[test]
    fn resolved_windows_are_clamped_into_the_source() {
        // Hangs off the right edge: slides back in, keeps its size.
        let l = layer(
            "image",
            r#"{"id": "k", "time": 0, "transitionDuration": 0,
                "viewport": [0.8, 0.0, 0.5, 0.5]}"#,
        );
        assert_eq!(layer_viewport(&l, 0.0), Some([0.5, 0.0, 0.5, 0.5]));
        // Degenerate size: floored, not divided by.
        let l = layer(
            "image",
            r#"{"id": "k", "time": 0, "transitionDuration": 0,
                "viewport": [0.5, 0.5, 0.0, -1.0]}"#,
        );
        assert_eq!(layer_viewport(&l, 0.0), Some([0.5, 0.5, MIN_SIZE, MIN_SIZE]));
    }

    #[test]
    fn keyframes_without_viewports_do_not_participate() {
        // A zoom keyframe between two viewport keyframes must not drag the
        // window toward zero — the scalar rule already filters, this pins it.
        let l = layer(
            "video",
            r#"{"id": "a", "time": 0, "transitionDuration": 0,
                "viewport": [0, 0, 1, 1]},
               {"id": "m", "time": 5, "transitionDuration": 5, "zoom": 2.0},
               {"id": "b", "time": 10, "transitionDuration": 10,
                "viewport": [0.5, 0.5, 0.5, 0.5]}"#,
        );
        assert_eq!(layer_viewport(&l, 5.0), Some([0.25, 0.25, 0.75, 0.75]));
    }

    #[test]
    fn compose_maps_the_window_inside_a_cell() {
        let cell = [0.5, 0.0, 0.25, 0.5]; // third cell of a 4x2 sheet
        assert_eq!(compose_uv(cell, [0.0, 0.0, 1.0, 1.0]), cell);
        assert_eq!(
            compose_uv(cell, [0.5, 0.5, 0.5, 0.5]),
            [0.625, 0.25, 0.125, 0.25]
        );
    }
}

#[cfg(test)]
mod overhang_tests {
    use super::*;

    /// A window hanging off an edge slides back in at full size, and until
    /// now did so silently — `promo_validate` had nothing to say about a
    /// project whose framing the renderer was quietly rewriting.
    #[test]
    fn a_window_past_the_edge_is_reported_with_what_it_becomes() {
        assert_eq!(out_of_bounds([0.0, 0.0, 1.0, 1.0]), None, "the whole frame is legal");
        assert_eq!(out_of_bounds([0.25, 0.25, 0.5, 0.5]), None);

        // Slides back in, size first — the same rule `clamped` applies.
        assert_eq!(out_of_bounds([0.55, 0.1, 0.6, 0.4]), Some([0.4, 0.1, 0.6, 0.4]));
        assert_eq!(out_of_bounds([-0.2, 0.0, 0.5, 0.5]), Some([0.0, 0.0, 0.5, 0.5]));
        // Too big shrinks to the frame; too small grows to the floor.
        assert_eq!(out_of_bounds([0.0, 0.0, 2.0, 1.0]), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(out_of_bounds([0.0, 0.0, 0.0, 0.5]), Some([0.0, 0.0, MIN_SIZE, 0.5]));
    }
}
