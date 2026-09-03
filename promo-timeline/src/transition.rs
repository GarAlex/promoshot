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
    /// A plain offset in canvas pixels, unlike `travel`, which is measured
    /// against the distance to the frame edge. A word rising 24px into place
    /// wants this; a layer sliding in from off-canvas wants that.
    pub offset: [f64; 2],
    /// A blur over the quad, in canvas px at a 900-tall canvas (the engine
    /// scales it): the blur dissolve's and the zoom's softness.
    pub blur: f64,
    /// White mixed in, 0…1: the flash.
    pub flash: f64,
    /// Torn bands and split channels, 0…1: the glitch.
    pub glitch: f64,
    /// A lean in perspective, degrees about X then Y, about the quad's
    /// own centre — a unit flipping in.
    pub tilt: [f64; 2],
    /// A turn in the plane, degrees clockwise, about the quad's own
    /// centre — a unit tumbling in.
    pub rotate: f64,
}

/// The softness the blurring kinds reach at their far end, canvas px at
/// a 900-tall canvas.
const BLUR_PX: f64 = 28.0;

impl Effect {
    pub const IDENTITY: Effect = Effect {
        opacity: 1.0,
        reveal: [0.0, 0.0, 1.0, 1.0],
        travel: [0.0, 0.0],
        scale: 1.0,
        offset: [0.0, 0.0],
        blur: 0.0,
        flash: 0.0,
        glitch: 0.0,
        tilt: [0.0, 0.0],
        rotate: 0.0,
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
    // The shorthand is linear on purpose: `fadeIn: 0.3` says the common
    // thing; a fade with a CURVE is what `transitionIn` with `easing` is
    // for.
    LayerTransition {
        easing: None,
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

/// A hump that peaks halfway and is EXACTLY zero at both ends — the
/// sine's rounding error at π would otherwise keep a finished glitch from
/// being the identity.
fn burst(progress: f64) -> f64 {
    if progress <= 0.0 || progress >= 1.0 {
        0.0
    } else {
        (progress * std::f64::consts::PI).sin()
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
    // Eased HERE, once per half, so all five kinds share the ramp's clock —
    // the same central-easing rule the keyframe tracks follow.
    let progress = transition
        .easing
        .unwrap_or(promo_model::Easing::Linear)
        .apply(progress);
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
        TransitionKind::Slide | TransitionKind::Push => Effect {
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
        // A fade whose picture sharpens as it arrives.
        TransitionKind::BlurDissolve => Effect {
            opacity: progress,
            blur: (1.0 - progress) * BLUR_PX,
            ..Effect::IDENTITY
        },
        // In from 35% larger, soft, settling to size as it clears.
        TransitionKind::Zoom => Effect {
            scale: 1.0 + 0.35 * (1.0 - progress),
            opacity: progress,
            blur: (1.0 - progress) * BLUR_PX * 0.6,
            ..Effect::IDENTITY
        },
        // Present almost at once, white-hot, cooling to itself.
        TransitionKind::Flash => Effect {
            opacity: (progress * 3.0).min(1.0),
            flash: 1.0 - progress,
            ..Effect::IDENTITY
        },
        // Pops in under a burst that peaks halfway and is gone at the end.
        TransitionKind::Glitch => Effect {
            opacity: (progress * 4.0).min(1.0),
            glitch: burst(progress),
            ..Effect::IDENTITY
        },
        // Through black: nothing for the first half, a fade over the second.
        TransitionKind::Dip => Effect {
            opacity: (progress * 2.0 - 1.0).max(0.0),
            ..Effect::IDENTITY
        },
    }
}

/// What the material being REPLACED does while the new material arrives.
///
/// Only a push moves it: a wipe is revealed OVER it, a fade dissolves over
/// it, a slide travels over it. Push is the one kind where the outgoing
/// material is part of the motion — which is why it only means anything at a
/// swap, where there IS an outgoing.
pub fn departing(transition: &LayerTransition, progress: f64) -> Effect {
    let progress = transition
        .easing
        .unwrap_or(promo_model::Easing::Linear)
        .apply(progress);
    match transition.kind {
        TransitionKind::Push => Effect {
            travel: match transition.edge() {
                // Shoved out the far side: in from the right, out to the left.
                TransitionEdge::Left => [progress, 0.0],
                TransitionEdge::Right => [-progress, 0.0],
                TransitionEdge::Top => [0.0, progress],
                TransitionEdge::Bottom => [0.0, -progress],
            },
            ..Effect::IDENTITY
        },
        // The old picture softens as the new one sharpens over it.
        TransitionKind::BlurDissolve => Effect {
            blur: progress * BLUR_PX,
            ..Effect::IDENTITY
        },
        // Pushed out through the zoom: larger, softer, gone.
        TransitionKind::Zoom => Effect {
            scale: 1.0 + 0.35 * progress,
            opacity: 1.0 - progress,
            blur: progress * BLUR_PX * 0.6,
            ..Effect::IDENTITY
        },
        // Goes white as the new one comes from white.
        TransitionKind::Flash => Effect {
            flash: progress,
            ..Effect::IDENTITY
        },
        // Torn the same way, at the same moment.
        TransitionKind::Glitch => Effect {
            glitch: burst(progress),
            ..Effect::IDENTITY
        },
        // Out over the first half; the new one is hidden until then.
        TransitionKind::Dip => Effect {
            opacity: 1.0 - (progress * 2.0).min(1.0),
            ..Effect::IDENTITY
        },
        TransitionKind::Fade
        | TransitionKind::Wipe
        | TransitionKind::Slide
        | TransitionKind::Scale => Effect::IDENTITY,
    }
}

/// A resource swap caught mid-transition: what the layer is coming FROM, and
/// how far through it is.
///
/// A swap has always been a step — "there is no halfway between two images,
/// and dissolving needs both drawn at once, which one layer cannot do". That
/// last part is what this lifts: for the transition's duration the layer
/// draws BOTH, the outgoing one whole and the incoming one arriving over it,
/// which is the transition between two clips rather than at a layer's edge.
#[derive(Debug, Clone)]
pub struct Swap {
    /// The resource being replaced. `None` when the layer's own resource is
    /// what is going away.
    pub previous: Option<String>,
    /// What the arriving material does.
    pub effect: Effect,
    /// What the departing material does — identity for every kind but push.
    pub departing: Effect,
}

/// The swap in force at `time`, if it is still arriving.
///
/// Takes `resources` for the same reason [`crate::sprite::layer_resource_id`]
/// does: a swap whose target has been deleted, or points at the wrong kind,
/// is SKIPPED there. Resolving the pair by different rules is how the two
/// come to disagree — and when they do the engine draws the surviving image
/// twice, one copy ramping up over the other, which reads as a brightness
/// pulse where a crossfade should be.
/// The window (layer-LOCAL) during which the resource showing at `time`
/// is the one showing: from the swap that brought it in (or the layer's
/// start) to the swap that will replace it (or the layer's end). What a
/// caption's reveal counts against — a statement swapped in at 0:07 starts
/// typing at 0:07, and a reveal with no stated pace spreads across the
/// statement's own tenure rather than the whole layer's life.
pub fn tenure(layer: &ProjectLayer, time: f64) -> (f64, Option<f64>) {
    let local = crate::layer_local_time(layer, time);
    let mut times: Vec<f64> = layer
        .keyframes
        .iter()
        .filter(|k| k.resource_id.is_some())
        .map(|k| k.time)
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let start = times
        .iter()
        .rev()
        .find(|t| **t <= local)
        .copied()
        .unwrap_or(0.0);
    let end = times
        .iter()
        .find(|t| **t > local)
        .copied()
        .or(layer.duration);
    (start, end)
}

pub fn active_swap(
    layer: &ProjectLayer,
    time: f64,
    resources: &[promo_model::ProjectResource],
) -> Option<Swap> {
    active_swap_sampled(layer, time, time, resources)
}

/// The swap with its two clocks separated, for motion blur: WHICH swap is
/// active — and whether one is active at all — is decided at
/// `identity_time`, the frame's own clock, so a cut inside a shutter stays
/// a cut and every sub-sample agrees on how many quads exist. How far the
/// travel has got is read at `effect_time`, the sub-sample's clock, clamped
/// to the transition's own window — which is what lets a push's moving
/// edge smear. The plain [`active_swap`] is this with both clocks equal.
pub fn active_swap_sampled(
    layer: &ProjectLayer,
    identity_time: f64,
    effect_time: f64,
    resources: &[promo_model::ProjectResource],
) -> Option<Swap> {
    let local = crate::layer_local_time(layer, identity_time);
    let usable = |keyframe: &promo_model::ProjectLayerKeyframe| {
        keyframe
            .resource_id
            .as_deref()
            .and_then(|id| crate::sprite::swappable(layer, id, resources))
            .is_some()
    };
    let mut swaps: Vec<&promo_model::ProjectLayerKeyframe> = layer
        .keyframes
        .iter()
        .filter(|k| k.resource_id.is_some() && usable(k))
        .collect();
    swaps.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let index = swaps.iter().rposition(|k| k.time <= local)?;
    let current = swaps[index];
    let transition = current.transition.as_ref().filter(|t| t.duration > 0.0)?;

    let progress = ((local - current.time) / transition.duration).clamp(0.0, 1.0);
    if progress >= 1.0 {
        return None;
    }
    // From here the sub-sample's clock takes over: the swap EXISTS because
    // the frame's own instant is mid-transition, but the travel is read at
    // the sample, saturating at the window's ends rather than extrapolating
    // past them.
    let effect_local = crate::layer_local_time(layer, effect_time);
    let progress = ((effect_local - current.time) / transition.duration).clamp(0.0, 1.0);
    // What it is replacing: the swap before it, else the layer's own. Also
    // filtered, so a deleted predecessor falls back the way the resolver
    // does rather than naming a resource nothing can draw.
    let previous = if index == 0 {
        layer.resource_id.clone()
    } else {
        swaps[index - 1].resource_id.clone()
    };
    let previous = previous.filter(|id| crate::sprite::swappable(layer, id, resources).is_some());
    // Nothing to fade FROM, and nothing to fade INTO that is not already
    // showing: without a distinct predecessor this is a cut.
    if previous.as_deref() == current.resource_id.as_deref() {
        return None;
    }
    Some(Swap {
        previous,
        effect: shape(transition, progress),
        departing: departing(transition, progress),
    })
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

    x += effect.offset[0];
    y += effect.offset[1];

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

    fn resources() -> Vec<promo_model::ProjectResource> {
        serde_json::from_str(
            r#"[{"id":"A","kind":"image","filename":"a.png","displayName":"a","addedAt":0},
                {"id":"B","kind":"image","filename":"b.png","displayName":"b","addedAt":0}]"#,
        )
        .expect("resources")
    }

    /// A push is the one kind where BOTH sides move: the new material comes
    /// in from its edge and shoves the old one out the far side. Every other
    /// kind arrives over material that stays put.
    #[test]
    fn a_push_moves_the_material_it_replaces_and_nothing_else_does() {
        let push = LayerTransition {
            easing: None,
            kind: TransitionKind::Push,
            from: Some(TransitionEdge::Right),
            duration: 1.0,
        };
        // A quarter in: the new one is still mostly off to the right, the old
        // one has started leaving to the left.
        let arriving = shape(&push, 0.25);
        let leaving = departing(&push, 0.25);
        assert_eq!(arriving.travel, [0.75, 0.0], "in from the right");
        assert_eq!(leaving.travel, [-0.25, 0.0], "out to the left");

        // At the end the old one is a full frame away and the new one home.
        assert_eq!(shape(&push, 1.0).travel, [0.0, 0.0]);
        assert_eq!(departing(&push, 1.0).travel, [-1.0, 0.0]);

        for kind in [
            TransitionKind::Wipe,
            TransitionKind::Fade,
            TransitionKind::Slide,
            TransitionKind::Scale,
        ] {
            let other = LayerTransition {
                kind,
                from: None,
                duration: 1.0,
                easing: None,
            };
            assert!(
                departing(&other, 0.5).is_identity(),
                "{kind:?} arrives OVER what it replaces, which does not move"
            );
        }
    }

    /// The swap resolver SKIPS a keyframe whose target has been deleted or is
    /// the wrong kind. If the transition does not skip it too, the two answer
    /// differently: the engine ends up drawing the surviving image twice, one
    /// copy ramping over the other, which reads as a brightness pulse where a
    /// crossfade should be.
    #[test]
    fn a_swap_to_a_resource_that_is_gone_is_not_a_transition() {
        let with_target = serde_json::from_str::<ProjectLayer>(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"duration":10,"resourceID":"A",
                "keyframes":[{"id":"K","time":2,"transitionDuration":0,"resourceID":"B",
                  "transition":{"kind":"wipe","duration":2}}]}"#,
        )
        .expect("layer");
        assert!(
            active_swap(&with_target, 3.0, &resources()).is_some(),
            "both exist"
        );

        // The same project after someone deleted the second image.
        let only_a: Vec<promo_model::ProjectResource> =
            resources().into_iter().filter(|r| r.id == "A").collect();
        assert!(
            active_swap(&with_target, 3.0, &only_a).is_none(),
            "the resolver falls back to A here, so there is nothing to cross-fade"
        );

        // And a swap naming a resource of the wrong kind is ignored the same
        // way the resolver ignores it.
        let wrong_kind: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id":"A","kind":"image","filename":"a.png","displayName":"a","addedAt":0},
                {"id":"B","kind":"audio","filename":"b.m4a","displayName":"b","addedAt":0}]"#,
        )
        .expect("resources");
        assert!(active_swap(&with_target, 3.0, &wrong_kind).is_none());
    }

    #[test]
    fn a_wipe_reveals_from_its_edge_without_moving_the_picture() {
        let l = layer(r#","transitionIn":{"kind":"wipe","from":"left","duration":1}"#);
        // Half a second in: half revealed, anchored left.
        let half = effect(&l, 2.5);
        assert_eq!(half.reveal, [0.0, 0.0, 0.5, 1.0]);

        let (rect, uv) = apply(
            &half,
            [100.0, 50.0, 400.0, 200.0],
            [0.0, 0.0, 1.0, 1.0],
            (1920.0, 1080.0),
        );
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
        let (rect, uv) = apply(
            &quarter,
            [0.0, 0.0, 400.0, 100.0],
            [0.0, 0.0, 1.0, 1.0],
            (1920.0, 1080.0),
        );
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
        let (_, uv) = apply(
            &half,
            [0.0, 0.0, 400.0, 200.0],
            [0.25, 0.1, 0.5, 0.4],
            (1920.0, 1080.0),
        );
        assert_eq!(
            uv,
            [0.25, 0.1, 0.25, 0.4],
            "half of the viewport, not half of the source"
        );
    }

    #[test]
    fn a_slide_starts_beyond_the_frame_edge_and_arrives_on_time() {
        let l = layer(r#","transitionIn":{"kind":"slide","from":"left","duration":2}"#);
        let start = effect(&l, 2.0);
        let (rect, _) = apply(
            &start,
            [300.0, 100.0, 400.0, 200.0],
            [0.0, 0.0, 1.0, 1.0],
            (1920.0, 1080.0),
        );
        assert_eq!(rect[0] + rect[2], 0.0, "entirely off the left edge at t=0");
        assert_eq!(rect[1], 100.0, "and no vertical drift");

        let arrived = effect(&l, 4.0);
        let (rect, _) = apply(
            &arrived,
            [300.0, 100.0, 400.0, 200.0],
            [0.0, 0.0, 1.0, 1.0],
            (1920.0, 1080.0),
        );
        assert_eq!(rect, [300.0, 100.0, 400.0, 200.0], "back where it belongs");
    }

    #[test]
    fn a_slide_from_the_bottom_leaves_the_frame_below() {
        let l = layer(r#","transitionIn":{"kind":"slide","duration":1}"#);
        // No `from`: a slide defaults to coming up from the bottom.
        let start = effect(&l, 2.0);
        let (rect, _) = apply(
            &start,
            [0.0, 800.0, 400.0, 200.0],
            [0.0, 0.0, 1.0, 1.0],
            (1920.0, 1080.0),
        );
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

    /// One easing field shapes all five kinds — pinned on the two halves
    /// that move differently: a fade's opacity and a push's travel.
    #[test]
    fn a_transition_can_carry_a_curve() {
        let layer: ProjectLayer = serde_json::from_str(
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1001","name":"L",
                "sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"duration":4,
                "transitionIn":{"kind":"fade","duration":1.0,"easing":"easeOut"},
                "keyframes":[]}"#,
        )
        .expect("layer");
        // ease-out at the midpoint runs AHEAD of linear: 1-(1-t)^2 = 0.75.
        let mid = effect(&layer, 0.5);
        assert!(
            (mid.opacity - 0.75).abs() < 1e-9,
            "an eased fade is ahead of linear at the midpoint, got {}",
            mid.opacity
        );

        // And the eased clock reaches a push's travel through the swap path.
        let eased: ProjectLayer = serde_json::from_str(
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1002","name":"L",
                "sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"duration":8,
                "resourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1003",
                "keyframes":[{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1004",
                  "time":4,"transitionDuration":0,
                  "resourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1005",
                  "transition":{"kind":"push","from":"left","duration":1.0,
                                "easing":"easeOut"}}]}"#,
        )
        .expect("layer");
        let resources: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1003","kind":"image",
                 "filename":"a.png","displayName":"a","addedAt":0},
                {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B1005","kind":"image",
                 "filename":"b.png","displayName":"b","addedAt":0}]"#,
        )
        .expect("resources");
        let swap = active_swap(&eased, 4.5, &resources).expect("mid-swap");
        // Incoming travel is (1 - progress) toward home; eased 0.75 → 0.25.
        assert!(
            (swap.effect.travel[0].abs() - 0.25).abs() < 1e-9,
            "the push's travel rides the eased clock, got {}",
            swap.effect.travel[0]
        );
    }

    /// The five newer kinds turn progress into their own channels — blur,
    /// flash, glitch, scale, opacity — and every one of them is the
    /// identity once it has arrived.
    #[test]
    fn the_newer_kinds_ramp_their_own_channels_and_end_at_identity() {
        let of = |kind: TransitionKind| LayerTransition {
            easing: None,
            kind,
            from: None,
            duration: 1.0,
        };
        let soft = shape(&of(TransitionKind::BlurDissolve), 0.25);
        assert!(soft.blur > 0.0 && (soft.opacity - 0.25).abs() < 1e-9);
        assert!(
            departing(&of(TransitionKind::BlurDissolve), 0.75).blur > 0.0,
            "the old one softens too"
        );
        let zoom = shape(&of(TransitionKind::Zoom), 0.0);
        assert!((zoom.scale - 1.35).abs() < 1e-9 && zoom.opacity == 0.0 && zoom.blur > 0.0);
        let gone = departing(&of(TransitionKind::Zoom), 1.0);
        assert!(
            gone.opacity == 0.0 && gone.scale > 1.3,
            "pushed out through the zoom"
        );
        assert!((shape(&of(TransitionKind::Flash), 0.0).flash - 1.0).abs() < 1e-9);
        assert!((departing(&of(TransitionKind::Flash), 1.0).flash - 1.0).abs() < 1e-9);
        assert!(
            shape(&of(TransitionKind::Glitch), 0.5).glitch > 0.99,
            "peaks halfway"
        );
        assert!(
            shape(&of(TransitionKind::Dip), 0.25).opacity == 0.0,
            "hidden through the first half"
        );
        assert!((shape(&of(TransitionKind::Dip), 0.75).opacity - 0.5).abs() < 1e-9);
        assert!((departing(&of(TransitionKind::Dip), 0.25).opacity - 0.5).abs() < 1e-9);
        for kind in [
            TransitionKind::BlurDissolve,
            TransitionKind::Zoom,
            TransitionKind::Flash,
            TransitionKind::Glitch,
            TransitionKind::Dip,
        ] {
            assert!(
                shape(&of(kind), 1.0).is_identity(),
                "{kind:?} arrives at identity"
            );
        }
    }
}
