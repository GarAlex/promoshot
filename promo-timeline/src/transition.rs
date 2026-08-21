//! How a layer enters and leaves.
//!
//! One rule for all four kinds: a transition is a PROGRESS from 0 (not yet
//! arrived / fully gone) to 1 (fully present), and each kind turns that
//! number into an adjustment of the quad the layer draws — its opacity, where
//! it sits, how much of it is revealed, how big it is. Nothing here knows
//! about layer kinds or textures, so a caption, a screenshot and a drawing
//! all wipe the same way.

use promo_model::{LayerTransition, ProjectLayer, TransitionEdge, TransitionKind};

/// What a transition does to the quad, at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effect {
    /// Multiplied with the layer's own opacity.
    pub opacity: f64,
    /// Fraction of the quad to show, `[x, y, w, h]` in 0…1 of its own box —
    /// a wipe, which crops the drawn rect AND the texture together so the
    /// picture stays put while its edge travels.
    pub reveal: [f64; 4],
    /// Where the quad sits relative to where it belongs, as a fraction of
    /// the distance it travels: `[-1, 0]` is one full slide off to the left.
    pub travel: [f64; 2],
    /// Scale about the quad's own centre.
    pub scale: f64,
}

impl Effect {
    pub const IDENTITY: Effect = Effect {
        opacity: 1.0,
        reveal: [0.0, 0.0, 1.0, 1.0],
        travel: [0.0, 0.0],
        scale: 1.0,
    };

    pub fn is_identity(&self) -> bool {
        *self == Effect::IDENTITY
    }
}

/// The transition a layer uses on the way in, whichever way it was written.
///
/// `fadeIn` is the shorthand — one number instead of an object — and
/// `transitionIn` wins when a layer somehow carries both, so the richer
/// statement is never silently overruled by the simpler one.
pub fn incoming(layer: &ProjectLayer) -> Option<LayerTransition> {
    layer
        .transition_in
        .clone()
        .or_else(|| layer.fade_in.map(fade))
}

pub fn outgoing(layer: &ProjectLayer) -> Option<LayerTransition> {
    layer
        .transition_out
        .clone()
        .or_else(|| layer.fade_out.map(fade))
}

fn fade(duration: f64) -> LayerTransition {
    LayerTransition {
        kind: TransitionKind::Fade,
        from: None,
        duration,
    }
}

/// The effect in force at `time`.
///
/// When a layer is short enough that its two transitions overlap, whichever
/// is LESS complete wins outright rather than the two being multiplied
/// together: a layer caught between arriving and leaving should read as the
/// one that has further to go, not as a doubly-faded ghost.
pub fn effect(layer: &ProjectLayer, time: f64) -> Effect {
    let arriving = incoming(layer)
        .filter(|t| t.duration > 0.0)
        .map(|t| (progress((time - layer.start_time) / t.duration), t));
    // Leaving needs an end to count back from. A layer with no duration runs
    // to the end of the project, which it cannot see from here.
    let leaving = layer.duration.and_then(|span| {
        outgoing(layer)
            .filter(|t| t.duration > 0.0)
            .map(|t| (progress((layer.start_time + span - time) / t.duration), t))
    });

    match (arriving, leaving) {
        (Some((a, at)), Some((b, bt))) => {
            if a <= b {
                shape(&at, a)
            } else {
                shape(&bt, b)
            }
        }
        (Some((a, at)), None) => shape(&at, a),
        (None, Some((b, bt))) => shape(&bt, b),
        (None, None) => Effect::IDENTITY,
    }
}

fn progress(raw: f64) -> f64 {
    if raw.is_nan() {
        1.0
    } else {
        raw.clamp(0.0, 1.0)
    }
}

fn shape(transition: &LayerTransition, progress: f64) -> Effect {
    let edge = transition.edge();
    match transition.kind {
        TransitionKind::Fade => Effect {
            opacity: progress,
            ..Effect::IDENTITY
        },
        TransitionKind::Wipe => Effect {
            reveal: match edge {
                TransitionEdge::Left => [0.0, 0.0, progress, 1.0],
                TransitionEdge::Right => [1.0 - progress, 0.0, progress, 1.0],
                TransitionEdge::Top => [0.0, 0.0, 1.0, progress],
                TransitionEdge::Bottom => [0.0, 1.0 - progress, 1.0, progress],
            },
            ..Effect::IDENTITY
        },
        TransitionKind::Slide => Effect {
            travel: match edge {
                TransitionEdge::Left => [-(1.0 - progress), 0.0],
                TransitionEdge::Right => [1.0 - progress, 0.0],
                TransitionEdge::Top => [0.0, -(1.0 - progress)],
                TransitionEdge::Bottom => [0.0, 1.0 - progress],
            },
            ..Effect::IDENTITY
        },
        // Grows into place rather than shrinking out of nothing: 0.85 is
        // close enough to read as a push rather than a zoom, and small
        // enough to be seen.
        TransitionKind::Scale => Effect {
            scale: 0.85 + 0.15 * progress,
            opacity: progress,
            ..Effect::IDENTITY
        },
    }
}

/// The quad's rect and texture window after `effect`.
///
/// `canvas` is what a slide travels across: a layer slides in from beyond the
/// frame edge, not merely by its own width, or a layer already near an edge
/// would appear to start half on screen.
pub fn apply(
    effect: &Effect,
    rect: [f64; 4],
    uv: [f32; 4],
    canvas: (f64, f64),
) -> ([f64; 4], [f32; 4]) {
    let [mut x, mut y, mut w, mut h] = rect;
    let mut uv = uv;

    // Wipe: the rect shrinks against the edge it is revealed from, and the
    // texture window shrinks with it, so the image does not stretch or slide.
    let [rx, ry, rw, rh] = effect.reveal;
    if rw < 1.0 || rh < 1.0 || rx > 0.0 || ry > 0.0 {
        uv = [
            uv[0] + uv[2] * rx as f32,
            uv[1] + uv[3] * ry as f32,
            uv[2] * rw as f32,
            uv[3] * rh as f32,
        ];
        x += w * rx;
        y += h * ry;
        w *= rw;
        h *= rh;
    }

    if effect.scale != 1.0 {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        w *= effect.scale;
        h *= effect.scale;
        x = cx - w / 2.0;
        y = cy - h / 2.0;
    }

    // Travel is measured so that a full slide puts the quad entirely beyond
    // the frame edge it came from.
    if effect.travel != [0.0, 0.0] {
        if effect.travel[0] < 0.0 {
            x += effect.travel[0] * (x + w);
        } else if effect.travel[0] > 0.0 {
            x += effect.travel[0] * (canvas.0 - x);
        }
        if effect.travel[1] < 0.0 {
            y += effect.travel[1] * (y + h);
        } else if effect.travel[1] > 0.0 {
            y += effect.travel[1] * (canvas.1 - y);
        }
    }
    ([x, y, w, h], uv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(body: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                 "startTime":2,"duration":10,"keyframes":[]{body}}}"#
        ))
        .expect("layer")
    }

    #[test]
    fn a_wipe_reveals_from_its_edge_without_moving_the_picture() {
        let l = layer(r#","transitionIn":{"kind":"wipe","from":"left","duration":1}"#);
        // Half a second in: half revealed, anchored left.
        let half = effect(&l, 2.5);
        assert_eq!(half.reveal, [0.0, 0.0, 0.5, 1.0]);

        let (rect, uv) = apply(&half, [100.0, 50.0, 400.0, 200.0], [0.0, 0.0, 1.0, 1.0], (1920.0, 1080.0));
        assert_eq!(rect, [100.0, 50.0, 200.0, 200.0], "left half of the box");
        assert_eq!(uv, [0.0, 0.0, 0.5, 1.0], "and the left half of the texture");

        // Done: nothing left to do.
        assert!(effect(&l, 3.0).is_identity());
    }

    /// The texture window must shrink WITH the rect. Cropping only the rect
    /// squeezes the whole picture into the revealed sliver, which reads as a
    /// squash rather than a wipe.
    #[test]
    fn a_wipe_from_the_right_crops_the_far_side_of_the_texture() {
        let l = layer(r#","transitionIn":{"kind":"wipe","from":"right","duration":1}"#);
        let quarter = effect(&l, 2.25);
        let (rect, uv) = apply(&quarter, [0.0, 0.0, 400.0, 100.0], [0.0, 0.0, 1.0, 1.0], (1920.0, 1080.0));
        assert_eq!(rect, [300.0, 0.0, 100.0, 100.0]);
        assert_eq!(uv, [0.75, 0.0, 0.25, 1.0]);
    }

    /// A wipe composes with a viewport: the layer may already be showing a
    /// window of its source, and the wipe crops within that window rather
    /// than reaching back into the parts the viewport excluded.
    #[test]
    fn a_wipe_crops_inside_an_existing_viewport() {
        let l = layer(r#","transitionIn":{"kind":"wipe","from":"left","duration":1}"#);
        let half = effect(&l, 2.5);
        let (_, uv) = apply(&half, [0.0, 0.0, 400.0, 200.0], [0.25, 0.1, 0.5, 0.4], (1920.0, 1080.0));
        assert_eq!(uv, [0.25, 0.1, 0.25, 0.4], "half of the viewport, not half of the source");
    }

    #[test]
    fn a_slide_starts_beyond_the_frame_edge_and_arrives_on_time() {
        let l = layer(r#","transitionIn":{"kind":"slide","from":"left","duration":2}"#);
        let start = effect(&l, 2.0);
        let (rect, _) = apply(&start, [300.0, 100.0, 400.0, 200.0], [0.0, 0.0, 1.0, 1.0], (1920.0, 1080.0));
        assert_eq!(rect[0] + rect[2], 0.0, "entirely off the left edge at t=0");
        assert_eq!(rect[1], 100.0, "and no vertical drift");

        let arrived = effect(&l, 4.0);
        let (rect, _) = apply(&arrived, [300.0, 100.0, 400.0, 200.0], [0.0, 0.0, 1.0, 1.0], (1920.0, 1080.0));
        assert_eq!(rect, [300.0, 100.0, 400.0, 200.0], "back where it belongs");
    }

    #[test]
    fn a_slide_from_the_bottom_leaves_the_frame_below() {
        let l = layer(r#","transitionIn":{"kind":"slide","duration":1}"#);
        // No `from`: a slide defaults to coming up from the bottom.
        let start = effect(&l, 2.0);
        let (rect, _) = apply(&start, [0.0, 800.0, 400.0, 200.0], [0.0, 0.0, 1.0, 1.0], (1920.0, 1080.0));
        assert_eq!(rect[1], 1080.0, "sitting on the bottom edge of the canvas");
    }

    #[test]
    fn a_layer_leaves_by_its_own_transition() {
        let l = layer(r#","transitionOut":{"kind":"wipe","from":"right","duration":2}"#);
        assert!(effect(&l, 9.0).is_identity(), "still fully present");
        // Layer ends at 12; one second left of a two-second wipe. `from` is
        // the edge the picture is anchored to, so on the way out what is left
        // collapses TOWARDS that edge — the visible half is the right half.
        assert_eq!(effect(&l, 11.0).reveal, [0.5, 0.0, 0.5, 1.0]);
        assert_eq!(effect(&l, 12.0).reveal, [1.0, 0.0, 0.0, 1.0], "gone");
    }

    /// A layer short enough that its two transitions overlap reads as the one
    /// with further to go, not as both applied at once.
    #[test]
    fn overlapping_transitions_pick_the_less_complete_one() {
        let l = serde_json::from_str::<ProjectLayer>(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"duration":1,"keyframes":[],
                "fadeIn":1,"fadeOut":1}"#,
        )
        .expect("layer");
        // At the midpoint both are exactly half done; either answer is 0.5.
        assert!((effect(&l, 0.5).opacity - 0.5).abs() < 1e-9);
        // A quarter in, arriving is less complete than leaving.
        assert!((effect(&l, 0.25).opacity - 0.25).abs() < 1e-9);
        // Three quarters in, leaving is.
        assert!((effect(&l, 0.75).opacity - 0.25).abs() < 1e-9);
    }

    /// A layer with no end cannot count backwards from one.
    #[test]
    fn a_layer_with_no_duration_does_not_transition_out() {
        let l = serde_json::from_str::<ProjectLayer>(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"keyframes":[],
                "transitionOut":{"kind":"wipe","duration":1}}"#,
        )
        .expect("layer");
        assert!(effect(&l, 1000.0).is_identity());
    }

    /// `transitionIn` states more than `fadeIn` can, so it wins rather than
    /// being overruled by the simpler field.
    #[test]
    fn the_richer_statement_wins_when_a_layer_carries_both() {
        let l = layer(r#","fadeIn":1,"transitionIn":{"kind":"wipe","duration":1}"#);
        let half = effect(&l, 2.5);
        assert_eq!(half.opacity, 1.0, "a wipe does not fade");
        assert_eq!(half.reveal, [0.0, 0.0, 0.5, 1.0]);
    }
}
