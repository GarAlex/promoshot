//! Keyframe interpolation — layer transforms, rotation/tilt scalars, colors.
//! Hold-then-ease semantics: between keyframes A and B the value holds at A
//! until `B.time - min(B.transitionDuration, gap)`, then eases linearly into
//! B. Mirrors `ProjectLayer.transform(at:)` / `interpolatedScalar` /
//! `backgroundColorHex(at:)` and the `CompositionSettings` twins.

use promo_model::{CompositionSettings, Easing, ProjectLayer, ProjectLayerKeyframe};

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
/// Whether a keyframe speaks for the layer's POSITION.
///
/// A zoom-only keyframe does NOT: it says nothing about where the layer is,
/// so it is not a waypoint on the route and must not be read as one. Reading
/// it as `(0, 0)` — an absent field is INHERITED, never zero — drew a leg
/// home to the origin that the layer never travels.
pub fn is_position_keyframe(k: &ProjectLayerKeyframe) -> bool {
    k.vertical_shift.is_some() || k.horizontal_shift.is_some() || k.placement.is_some()
}

/// A placement rule resolves against what the layer SHOWS at this instant:
/// the active resource's pixels (swap-aware), windowed by the viewport — the
/// same box the renderer lays out. A source the model has no size for reads
/// as square; `promo_validate` names that. Resolution happens HERE, before
/// the lerp, so ramps, easing and motion paths blend plain numbers — the rule
/// itself is never baked anywhere.
fn placement_aspect(
    layer: &ProjectLayer,
    time: f64,
    resources: &[promo_model::ProjectResource],
) -> f64 {
    if !layer.keyframes.iter().any(|k| k.placement.is_some()) {
        return 1.0;
    }
    let resource = crate::sprite::layer_resource_id(layer, time, resources)
        .and_then(|id| resources.iter().find(|r| r.id == id))
        .or_else(|| {
            layer
                .resource_id
                .as_ref()
                .and_then(|id| resources.iter().find(|r| &r.id == id))
        });
    let source = resource
        .and_then(crate::layout::resource_source_size)
        // A device frame is baked into the picture BEFORE anything lays the
        // layer out, so what the layer shows is the slab, not the screenshot
        // inside it. Resolving the rule against the bare pixels sized the
        // box for an image the renderer never draws — `media_quad` took its
        // width from the framed texture and its origin from here, and the
        // device landed off-centre by the difference.
        .map(|size| {
            crate::frame::framed_pixel_size(size, effective_frame(layer, time, resource).as_ref())
        });
    let mut aspect = source.map_or(1.0, |s| s.width() / s.height().max(f64::EPSILON));
    if let Some(vp) = crate::viewport::layer_viewport(layer, time) {
        aspect *= vp[2] / vp[3].max(f64::EPSILON);
    }
    aspect
}

/// The frame the layer WEARS at `time`: the named cut's, else the resource's
/// own, with tilt keyframes standing in for the stored tilt — the same frame
/// the app bakes. Swift twin: `ProjectLayer.effectiveFrame(at:resources:)`.
///
/// Only an IMAGE wears one here. A slab is a BAKE, and every bake site takes
/// an image — a device frame on a video is never pre-framed, so `media_quad`
/// lays that layer out at its own size and `media_border_style` degrades the
/// frame to a border. Inflating there would size the box for a slab nothing
/// draws: the same defect, pointing the other way.
///
/// A SPRITE sheet is left unframed deliberately. `media_quad` divides the
/// FRAMED texture into cells, so a framed sheet is already wrong further
/// down; compensating here would stack a second correction on the first and
/// bury the real defect.
fn effective_frame(
    layer: &ProjectLayer,
    time: f64,
    resource: Option<&promo_model::ProjectResource>,
) -> Option<promo_model::ResourceFrame> {
    let resource = resource?;
    if resource.kind != promo_model::ProjectResourceKind::Image || resource.sprite.is_some() {
        return None;
    }
    let mut frame = layer
        .image_cut_id
        .as_ref()
        .and_then(|cid| resource.image_cuts.iter().find(|c| &c.id == cid))
        .and_then(|cut| cut.frame.as_ref())
        .or(resource.frame.as_ref())?
        .clone();
    if frame.kind == promo_model::ResourceFrameKind::Device {
        if let Some((tilt_x, tilt_y)) = layer_tilt_offset(layer, time) {
            frame.tilt_x = tilt_x;
            frame.tilt_y = tilt_y;
        }
    }
    Some(frame)
}

/// What a keyframe SAYS its zoom is, resolved: a placement wins over the raw
/// number on the same keyframe. A position-only rule (no height/width/mode)
/// keeps the keyframe's own zoom for its box.
fn keyframe_zoom(k: &ProjectLayerKeyframe, aspect: f64, canvas: promo_model::Size) -> f64 {
    k.placement
        .as_ref()
        .and_then(|p| crate::layout::placement_zoom(p, aspect, canvas))
        .or(k.zoom)
        .unwrap_or(1.0)
}

/// Where a keyframe SAYS the layer is, resolved the same way.
fn keyframe_position(
    k: &ProjectLayerKeyframe,
    aspect: f64,
    canvas: promo_model::Size,
) -> (f64, f64) {
    match &k.placement {
        Some(p) => {
            crate::layout::placement_position(p, keyframe_zoom(k, aspect, canvas), aspect, canvas)
        }
        None => (
            k.horizontal_shift.unwrap_or(0.0),
            k.vertical_shift.unwrap_or(0.0),
        ),
    }
}

/// One waypoint of the route a layer's position travels: a position-track
/// keyframe with its point resolved exactly as `layer_transform` resolves it.
///
/// This exists so the editor's route overlay cannot invent its own answer.
/// It drew phantom legs for years' worth of two reasons at once — counting
/// zoom-only keyframes as waypoints, and reading a placement keyframe as the
/// origin — and both were re-derivations of a rule that lives here.
pub struct PositionWaypoint<'a> {
    pub keyframe: &'a ProjectLayerKeyframe,
    pub point: promo_model::Point,
}

/// The route a layer's position travels, in canvas points, at `time`'s
/// resolution of what the layer shows.
pub fn layer_position_waypoints<'a>(
    layer: &'a ProjectLayer,
    time: f64,
    defaults: &CompositionSettings,
    resources: &[promo_model::ProjectResource],
) -> Vec<PositionWaypoint<'a>> {
    let canvas = promo_model::Size::new(defaults.canvas_width, defaults.canvas_height);
    let aspect = placement_aspect(layer, time, resources);
    sorted_by_time(&layer.keyframes, is_position_keyframe)
        .into_iter()
        .map(|k| {
            let (x, y) = keyframe_position(k, aspect, canvas);
            PositionWaypoint {
                keyframe: k,
                point: promo_model::Point(x, y),
            }
        })
        .collect()
}

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
    // A placement rule resolves against what the layer SHOWS at this
    // instant: the active resource's pixels (swap-aware), windowed by the
    // viewport — the same box the renderer lays out. A source the model has
    // no size for reads as square; `promo_validate` names that. Resolution
    // happens HERE, before the lerp, so ramps, easing and motion paths blend
    // plain numbers — the rule itself is never baked anywhere.
    let canvas = promo_model::Size::new(defaults.canvas_width, defaults.canvas_height);
    let aspect = placement_aspect(layer, time, resources);
    let zoom_of = |k: &ProjectLayerKeyframe| keyframe_zoom(k, aspect, canvas);
    let position_of = |k: &ProjectLayerKeyframe| keyframe_position(k, aspect, canvas);
    let zoom_track = sorted_by_time(&layer.keyframes, |k| {
        k.zoom.is_some()
            || k.placement
                .as_ref()
                .is_some_and(promo_model::Placement::sizes)
    });
    let position_track = sorted_by_time(&layer.keyframes, is_position_keyframe);
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
            let av = zoom_of(a);
            av + (zoom_of(b) - av) * progress
        }
    };
    let (horizontal_shift, vertical_shift) = match track_window(&position_track, local_time) {
        None => (0.0, 0.0),
        Some((a, b, progress)) => {
            let (ah, av) = position_of(a);
            let (bh, bv) = position_of(b);
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
            // Eased HERE, once, so every consumer of this window shares one
            // clock: the zoom lerp, the position lerp and the arc-length walk
            // along a motion path all move together. Easing them separately
            // is how a layer ends up sliding along its curve at a different
            // rate than it grows.
            return Some((a, b, b.easing.unwrap_or(Easing::Linear).apply(progress)));
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
    // The same scan the transform tracks use — including the tie rule and
    // the easing — rather than a second copy of hold-then-ramp that would
    // have to be kept in step by hand.
    let sorted = sorted_by_time(&layer.keyframes, |k| select(k).is_some());
    let (a, b, progress) = track_window(&sorted, local_time)?;
    let av = select(a).unwrap_or(0.0);
    let bv = select(b).unwrap_or(0.0);
    Some(av + (bv - av) * progress)
}

/// Swift `ProjectLayer.rotation(at:)` — degrees, 0 when unkeyed.
pub fn layer_rotation(layer: &ProjectLayer, time: f64) -> f64 {
    layer_interpolated_scalar(layer, layer_local_time(layer, time), |k| k.rotation).unwrap_or(0.0)
}

/// The layer's shutter at `time` — the fraction of one frame interval its
/// blur keeps the shutter open, or None when the layer never asked for
/// blur at all.
///
/// Keyframes first: they are the full form, held-then-ramped on the same
/// clock as every other scalar, which is what lets a blur ramp INTO a whip
/// pan and back out. The layer's `motionBlur` constant is the shorthand
/// for the common case; when any keyframe carries a shutter, the keyframes
/// win — the same both-present rule fades and transitions settled on.
pub fn layer_shutter(layer: &ProjectLayer, time: f64) -> Option<f64> {
    if layer.keyframes.iter().any(|k| k.shutter.is_some()) {
        return layer_interpolated_scalar(layer, layer_local_time(layer, time), |k| k.shutter)
            .map(|s| s.clamp(0.0, 1.0));
    }
    layer
        .motion_blur
        .as_ref()
        .map(|blur| blur.shutter.clamp(0.0, 1.0))
}

/// A layer's grade at `time`, every field resolved: keyframes first (the
/// full form, held-then-ramped), else the layer constant, else identity.
/// None when the whole grade IS identity, so the renderer can skip the
/// shader work without inspecting fields.
pub struct ResolvedAdjustments {
    pub saturation: f64,
    pub contrast: f64,
    pub brightness: f64,
    pub tint_amount: f64,
    pub tint_hex: Option<String>,
}

/// Where a layer's mask sits at `time`, when any keyframe moves it.
///
/// Offsets in canvas px, zoom about the rect centre, rotation clockwise
/// degrees — a similarity transform, each field resolved on the same eased
/// clock as every scalar track. `None` when no keyframe carries a mask
/// placement field at all, so a static mask keeps the identity path free.
#[derive(Debug, Clone, Copy)]
pub struct MaskPlacement {
    pub dx: f64,
    pub dy: f64,
    /// Horizontal scale of the window.
    pub zoom: f64,
    /// Vertical scale. Follows `zoom` unless a keyframe says otherwise —
    /// the mask keeps its own proportions by default, and only a deliberate
    /// stretch makes a circle an oval.
    pub zoom_y: f64,
    pub rotation_deg: f64,
}

pub fn layer_mask_placement(layer: &ProjectLayer, time: f64) -> Option<MaskPlacement> {
    let any = layer.keyframes.iter().any(|k| {
        k.mask_offset_x.is_some()
            || k.mask_offset_y.is_some()
            || k.mask_zoom.is_some()
            || k.mask_zoom_y.is_some()
            || k.mask_rotation.is_some()
    });
    if !any {
        return None;
    }
    let local = layer_local_time(layer, time);
    let field = |pick: fn(&ProjectLayerKeyframe) -> Option<f64>, identity: f64| {
        layer_interpolated_scalar(layer, local, pick).unwrap_or(identity)
    };
    let zoom = field(|k| k.mask_zoom, 1.0).max(1e-6);
    Some(MaskPlacement {
        dx: field(|k| k.mask_offset_x, 0.0),
        dy: field(|k| k.mask_offset_y, 0.0),
        zoom,
        // Defaulting to the horizontal is what keeps the shape honest: a
        // mask only stretches when someone asks it to.
        zoom_y: layer_interpolated_scalar(layer, local, |k| k.mask_zoom_y)
            .unwrap_or(zoom)
            .max(1e-6),
        rotation_deg: field(|k| k.mask_rotation, 0.0),
    })
}

pub fn layer_adjustments(layer: &ProjectLayer, time: f64) -> Option<ResolvedAdjustments> {
    let local = layer_local_time(layer, time);
    let field =
        |pick: fn(&ProjectLayerKeyframe) -> Option<f64>, constant: Option<f64>, identity: f64| {
            layer_interpolated_scalar(layer, local, pick)
                .or(constant)
                .unwrap_or(identity)
        };
    let base = layer.adjustments.as_ref();
    let out = ResolvedAdjustments {
        saturation: field(|k| k.saturation, base.and_then(|a| a.saturation), 1.0).max(0.0),
        contrast: field(|k| k.contrast, base.and_then(|a| a.contrast), 1.0).max(0.0),
        brightness: field(|k| k.brightness, base.and_then(|a| a.brightness), 0.0).clamp(-1.0, 1.0),
        tint_amount: field(|k| k.tint_amount, base.and_then(|a| a.tint_amount), 0.0)
            .clamp(0.0, 1.0),
        tint_hex: base.and_then(|a| a.tint_hex.clone()),
    };
    let identity = out.saturation == 1.0
        && out.contrast == 1.0
        && out.brightness == 0.0
        && out.tint_amount == 0.0;
    (!identity).then_some(out)
}

/// Layer opacity at `time` — 0…1, and 1 when the layer has no opacity
/// keyframes, so an un-keyed layer is fully visible as before.
pub fn layer_opacity(layer: &ProjectLayer, time: f64) -> f64 {
    let keyed = layer_interpolated_scalar(layer, layer_local_time(layer, time), |k| k.opacity)
        .unwrap_or(1.0);
    // The transition multiplies rather than replaces, so a layer that fades
    // in and also dips in the middle does both. `fadeIn`/`fadeOut` are the
    // shorthand for a fade transition and resolve through the same path.
    (keyed * crate::transition::effect(layer, time).opacity).clamp(0.0, 1.0)
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
            // Resolved BEFORE mixing: a `@name` cannot be averaged, and the
            // unparseable fallback would hold-then-snap where literals fade.
            // The plateau returns above keep the reference untouched — draw
            // sites resolve those, and editors read them as written.
            let (from, to) = (color(a), color(b));
            return interpolate_color_hex(
                defaults.resolve_color(&from),
                defaults.resolve_color(&to),
                progress,
            );
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
            // Same rule as the layer variant: resolve before mixing, so a
            // fade between named colours blends instead of snapping.
            return interpolate_color_hex(
                settings.resolve_color(&a.color_hex),
                settings.resolve_color(&b.color_hex),
                t,
            );
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
mod placement_frame_tests {
    use super::*;
    use promo_model::Size;

    fn framed_shot(bezel: f64, tilt_y: f64) -> promo_model::ProjectResource {
        let json = format!(
            r#"{{"id": "R", "kind": "image", "filename": "shot.png",
                 "displayName": "Shot", "addedAt": 0,
                 "pixelWidth": 1290, "pixelHeight": 2796,
                 "frame": {{"kind": "device", "bezelFraction": {bezel},
                            "tiltY": {tilt_y}}}}}"#
        );
        serde_json::from_str(&json).expect("resource")
    }

    fn placed_layer(keys: &str) -> ProjectLayer {
        let json = format!(
            r#"{{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 5,
                 "resourceID": "R", "keyframes": [{keys}]}}"#
        );
        serde_json::from_str(&json).expect("layer")
    }

    fn settings(w: f64, h: f64) -> promo_model::CompositionSettings {
        let mut s = promo_model::CompositionSettings::default();
        s.canvas_width = w;
        s.canvas_height = h;
        s
    }

    /// A device-framed shot must land CENTRED.
    ///
    /// The app bakes the slab into the picture before the engine sees it, so
    /// `media_quad` takes the drawn WIDTH from the framed texture. The origin
    /// comes from here — and resolving against the resource's bare pixels
    /// made the two disagree by the bezel, leaving 215px of margin on the
    /// left against 80 on the right on the App Store wizard's iPhone preset.
    /// Swift twin: `testAFramedShotLandsCentredNotOffToOneSide`.
    #[test]
    fn a_framed_shot_lands_centred() {
        let canvas = settings(1290.0, 2796.0);
        let resources = vec![framed_shot(0.035, 0.0)];
        let layer = placed_layer(
            r#"{"id": "K", "time": 0, "transitionDuration": 0,
                "placement": {"height": 1800, "anchor": "center"}}"#,
        );
        let tr = layer_transform_along_paths(&layer, 1.0, &canvas, &resources);

        let shown = crate::frame::framed_pixel_size(
            Size::new(1290.0, 2796.0),
            resources[0].frame.as_ref(),
        );
        let drawn_width = 2796.0 * tr.zoom * (shown.width() / shown.height());
        let left = tr.horizontal_shift;
        let right = 1290.0 - (tr.horizontal_shift + drawn_width);
        assert!(
            (left - right).abs() < 1.0,
            "off centre: {left} left vs {right} right"
        );
    }

    /// The box is asked for PER TIME: a slab that turns is narrower than one
    /// face-on, so a tilt ramp that used one fixed aspect slid the device
    /// sideways for the whole length of the turn.
    #[test]
    fn a_turning_slab_stays_centred_the_whole_way_through() {
        let canvas = settings(1290.0, 2796.0);
        let resources = vec![framed_shot(0.035, 0.0)];
        let layer = placed_layer(
            r#"{"id": "A", "time": 0, "tiltY": 0, "transitionDuration": 0,
                "placement": {"height": 1800, "anchor": "center"}},
               {"id": "B", "time": 4, "tiltY": -30, "transitionDuration": 4}"#,
        );
        for step in 0..=8 {
            let t = step as f64 * 0.5;
            let tr = layer_transform_along_paths(&layer, t, &canvas, &resources);
            let mut frame = resources[0].frame.clone().expect("frame");
            frame.tilt_y = layer_tilt_offset(&layer, t).expect("tilt").1;
            let shown =
                crate::frame::framed_pixel_size(Size::new(1290.0, 2796.0), Some(&frame));
            let drawn_width = 2796.0 * tr.zoom * (shown.width() / shown.height());
            let centre = tr.horizontal_shift + drawn_width / 2.0;
            assert!(
                (centre - 645.0).abs() < 1.0,
                "at t={t} the device drifted to {centre}"
            );
        }
    }

    /// A device frame on a VIDEO does not inflate the box.
    ///
    /// Nothing bakes a slab for a video — every bake site takes an image —
    /// so `media_quad` gets the raw texture and `media_border_style`
    /// degrades the frame to a border. Sizing the rule for a slab that is
    /// never drawn is the same defect as sizing it for a screenshot when a
    /// slab IS drawn, just pointing the other way.
    #[test]
    fn a_device_frame_on_a_video_does_not_inflate() {
        let canvas = settings(1290.0, 2796.0);
        let json = r#"{"id": "R", "kind": "video", "filename": "clip.mp4",
                       "displayName": "Clip", "addedAt": 0,
                       "videoNaturalWidth": 1290, "videoNaturalHeight": 2796,
                       "frame": {"kind": "device", "bezelFraction": 0.035}}"#;
        let resources: Vec<promo_model::ProjectResource> =
            vec![serde_json::from_str(json).expect("resource")];
        let mut layer = placed_layer(
            r#"{"id": "K", "time": 0, "transitionDuration": 0,
                "placement": {"height": 1800, "anchor": "center"}}"#,
        );
        layer.kind = promo_model::ProjectLayerKind::Video;
        let tr = layer_transform_along_paths(&layer, 1.0, &canvas, &resources);
        let drawn_width = 1800.0 * (1290.0 / 2796.0);
        assert!(
            (tr.horizontal_shift - (1290.0 - drawn_width) / 2.0).abs() < 1e-9,
            "a video was laid out as if a slab had been baked around it"
        );
    }

    /// A rule on an unframed resource resolves exactly as it always did —
    /// the guarantee that no existing project moves.
    #[test]
    fn an_unframed_shot_is_untouched() {
        let canvas = settings(1290.0, 2796.0);
        let json = r#"{"id": "R", "kind": "image", "filename": "s.png",
                       "displayName": "S", "addedAt": 0,
                       "pixelWidth": 1290, "pixelHeight": 2796}"#;
        let resources: Vec<promo_model::ProjectResource> =
            vec![serde_json::from_str(json).expect("resource")];
        let layer = placed_layer(
            r#"{"id": "K", "time": 0, "transitionDuration": 0,
                "placement": {"height": 1800, "anchor": "center"}}"#,
        );
        let tr = layer_transform_along_paths(&layer, 1.0, &canvas, &resources);
        let drawn_width = 1800.0 * (1290.0 / 2796.0);
        assert!((tr.horizontal_shift - (1290.0 - drawn_width) / 2.0).abs() < 1e-9);
    }
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

    /// Four opacity keyframes per fading layer was ~40% of every hand-written
    /// project. The shorthand says the same thing in two numbers.
    #[test]
    fn a_layer_fades_in_and_out_without_keyframes() {
        let mut layer = layer_with(r#"{"id": "K", "time": 0, "transitionDuration": 0}"#);
        layer.start_time = 2.0;
        layer.duration = Some(10.0); // ends at 12
        layer.fade_in = Some(1.0);
        layer.fade_out = Some(2.0);

        assert_eq!(layer_opacity(&layer, 2.0), 0.0, "transparent at the start");
        assert_eq!(layer_opacity(&layer, 2.5), 0.5, "halfway up");
        assert_eq!(layer_opacity(&layer, 3.0), 1.0, "fully in after a second");
        assert_eq!(layer_opacity(&layer, 8.0), 1.0, "and stays there");
        assert_eq!(layer_opacity(&layer, 11.0), 0.5, "halfway down");
        assert_eq!(layer_opacity(&layer, 12.0), 0.0, "transparent at the end");
    }

    /// An ENVELOPE, not a replacement: a layer that fades in and also dips in
    /// the middle does both. If the shorthand won outright, adding a fade to
    /// a layer would quietly erase its opacity keyframes.
    #[test]
    fn a_fade_multiplies_the_keyframes_rather_than_replacing_them() {
        let mut layer = layer_with(
            r#"{"id": "A", "time": 0, "opacity": 1.0, "transitionDuration": 0},
               {"id": "B", "time": 4, "opacity": 0.5, "transitionDuration": 0}"#,
        );
        layer.duration = Some(8.0);
        layer.fade_in = Some(2.0);

        assert_eq!(
            layer_opacity(&layer, 1.0),
            0.5,
            "half a fade over full opacity"
        );
        assert_eq!(
            layer_opacity(&layer, 4.0),
            0.5,
            "past the fade the keys speak alone"
        );
    }

    /// A layer with no duration runs to the end of the project, which the
    /// layer cannot see — so it fades in and simply does not fade out,
    /// rather than guessing an end and going transparent early.
    #[test]
    fn a_layer_with_no_end_does_not_fade_out() {
        let mut layer = layer_with(r#"{"id": "K", "time": 0, "transitionDuration": 0}"#);
        layer.duration = None;
        layer.fade_in = Some(1.0);
        layer.fade_out = Some(1.0);
        assert_eq!(layer_opacity(&layer, 0.5), 0.5);
        assert_eq!(layer_opacity(&layer, 100.0), 1.0);
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

    /// Every waypoint the route overlay draws must be a place the layer
    /// actually IS at that moment. Not "close to" — a keyframe's own time
    /// sits at the END of its ramp, so the resolved position there is
    /// exactly the keyframe's point.
    ///
    /// The overlay used to answer this itself and got both halves wrong: it
    /// counted zoom-only keyframes as waypoints, and read every keyframe's
    /// position as `horizontal_shift.unwrap_or(0)`. An absent field is
    /// INHERITED, so a layer parked at x=400 that merely zooms later grew a
    /// leg home to the origin it never flies.
    #[test]
    fn waypoints_are_where_the_layer_really_is() {
        let cfg = settings(r#"{"canvasWidth": 64, "canvasHeight": 64}"#);
        let l = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "B", "time": 2, "transitionDuration": 0,
                "horizontalShift": 400, "verticalShift": 120},
               {"id": "C", "time": 6, "transitionDuration": 0, "zoom": 2.5}"#,
        );

        let route = layer_position_waypoints(&l, 0.0, &cfg, &[]);
        assert_eq!(
            route.len(),
            2,
            "the zoom-only keyframe is not a place the layer goes"
        );
        for w in &route {
            let at = layer_transform(&l, w.keyframe.time, &cfg);
            assert_eq!(
                (at.horizontal_shift, at.vertical_shift),
                (w.point.x(), w.point.y()),
                "waypoint at t={} is not where the layer is",
                w.keyframe.time
            );
        }

        // And the layer has NOT come home by the time it zooms.
        let zooming = layer_transform(&l, 6.0, &cfg);
        assert_eq!(
            (zooming.horizontal_shift, zooming.vertical_shift),
            (400.0, 120.0),
            "an absent shift inherits, it does not reset"
        );
    }

    /// A placement keyframe says where the layer is with a RULE, not two
    /// numbers. The overlay read its raw (absent) shifts and pinned the
    /// route to the origin.
    #[test]
    fn placement_waypoints_resolve_the_rule() {
        let cfg = settings(r#"{"canvasWidth": 1920, "canvasHeight": 1080}"#);
        let l = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "B", "time": 3, "transitionDuration": 0,
                "placement": {"anchor": "topTrailing", "width": 0.25}}"#,
        );

        let route = layer_position_waypoints(&l, 0.0, &cfg, &[]);
        assert_eq!(route.len(), 2, "a placement keyframe IS a waypoint");
        let placed = route[1].point;
        assert!(
            placed.x() != 0.0 || placed.y() != 0.0,
            "a corner placement is not the origin, got {placed:?}"
        );
        let at = layer_transform(&l, 3.0, &cfg);
        assert_eq!(
            (at.horizontal_shift, at.vertical_shift),
            (placed.x(), placed.y())
        );
    }

    fn transform(l: &ProjectLayer, t: f64) -> Transform {
        layer_transform(
            l,
            t,
            &settings(r#"{"canvasWidth": 64, "canvasHeight": 64}"#),
        )
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
        let zoom_only = layer(r#"{"id": "z", "time": 0, "transitionDuration": 0, "zoom": 2.0}"#);
        let t = transform(&zoom_only, 3.0);
        assert_eq!(
            (t.zoom, t.horizontal_shift, t.vertical_shift),
            (2.0, 0.0, 0.0)
        );

        let move_only = layer(
            r#"{"id": "p", "time": 0, "transitionDuration": 0,
                "horizontalShift": 40, "verticalShift": 8}"#,
        );
        let t = transform(&move_only, 3.0);
        assert_eq!(
            (t.zoom, t.horizontal_shift, t.vertical_shift),
            (1.0, 40.0, 8.0)
        );
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

    /// The whole point of per-type tracks: RAMPS OVERLAP. While a
    /// thirty-second glide is mid-flight, rotation runs its own ramp inside
    /// it, opacity fades on a third clock, zoom dives late, and the viewport
    /// pans on a fourth — five transitions in the air at once, none
    /// disturbing another. Every value below is sampled at the same instant
    /// and checked mid-ITS-own-ramp with exact arithmetic.
    #[test]
    fn transitions_of_different_types_overlap_freely() {
        let l = layer(
            r#"{"id": "p0", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "p1", "time": 30, "transitionPercent": 100,
                "transitionDuration": 0,
                "horizontalShift": 300, "verticalShift": 0},
               {"id": "r0", "time": 2, "transitionDuration": 0, "rotation": 0},
               {"id": "r1", "time": 8, "transitionDuration": 6, "rotation": 90},
               {"id": "o0", "time": 5, "transitionDuration": 0, "opacity": 1.0},
               {"id": "o1", "time": 12, "transitionDuration": 7, "opacity": 0.3},
               {"id": "z0", "time": 0, "transitionDuration": 0, "zoom": 1.0},
               {"id": "z1", "time": 30, "transitionDuration": 1, "zoom": 2.0},
               {"id": "v0", "time": 0, "transitionDuration": 0,
                "viewport": [0, 0, 1, 1]},
               {"id": "v1", "time": 20, "transitionDuration": 10,
                "viewport": [0.5, 0.5, 0.5, 0.5]}"#,
        );

        // t = 6.5: the move is 6.5/30 done, rotation is 4.5/6 through its
        // 2..8 ramp, opacity 1.5/7 through its 5..12 fade, zoom has not
        // begun, and the viewport has not begun (its ramp starts at 10).
        let t = 6.5;
        let tr = transform(&l, t);
        assert_eq!(tr.horizontal_shift, 65.0);
        assert_eq!(tr.zoom, 1.0, "zoom holds until 29");
        assert_eq!(layer_rotation(&l, t), 67.5);
        let o = layer_opacity(&l, t);
        assert!((o - (1.0 - 0.7 * 1.5 / 7.0)).abs() < 1e-9, "got {o}");
        assert_eq!(
            crate::viewport::layer_viewport(&l, t),
            Some([0.0, 0.0, 1.0, 1.0])
        );

        // t = 15: rotation and opacity have LANDED mid-glide, the viewport
        // is half way through its pan, the move still going, zoom still
        // waiting.
        let t = 15.0;
        let tr = transform(&l, t);
        assert_eq!(tr.horizontal_shift, 150.0);
        assert_eq!(tr.zoom, 1.0);
        assert_eq!(layer_rotation(&l, t), 90.0);
        assert_eq!(layer_opacity(&l, t), 0.3);
        assert_eq!(
            crate::viewport::layer_viewport(&l, t),
            Some([0.25, 0.25, 0.75, 0.75])
        );

        // t = 29.5: everything else is done or landing; zoom is half way
        // through the one-second dive it saved for the end.
        let t = 29.5;
        let tr = transform(&l, t);
        assert_eq!(tr.zoom, 1.5);
        assert_eq!(tr.horizontal_shift, 295.0);
        assert_eq!(layer_rotation(&l, t), 90.0);
    }

    /// The curves themselves: in at 0, out at 1, and the shape between.
    #[test]
    fn easing_curves_are_monotonic_and_pinned_at_the_ends() {
        use promo_model::Easing::*;
        for e in [Linear, EaseIn, EaseOut, EaseInOut] {
            assert_eq!(e.apply(0.0), 0.0, "{e:?} starts at 0");
            assert_eq!(e.apply(1.0), 1.0, "{e:?} ends at 1");
            // Out of range cannot send a layer past its own keyframe.
            assert_eq!(e.apply(-1.0), 0.0);
            assert_eq!(e.apply(2.0), 1.0);
            let mut prev = -1.0;
            for i in 0..=20 {
                let v = e.apply(i as f64 / 20.0);
                assert!(v >= prev, "{e:?} went backwards at {i}");
                prev = v;
            }
        }
        assert_eq!(EaseIn.apply(0.5), 0.25, "slow start");
        assert_eq!(EaseOut.apply(0.5), 0.75, "fast start");
        assert_eq!(EaseInOut.apply(0.5), 0.5, "symmetric");
        assert_eq!(EaseInOut.apply(0.25), 0.15625, "smoothstep");
    }

    /// Absent easing is linear — every project written before this existed
    /// renders exactly as it did.
    #[test]
    fn an_unkeyed_ramp_is_still_linear() {
        let l = layer(
            r#"{"id": "a", "time": 0, "transitionDuration": 0, "zoom": 1},
               {"id": "b", "time": 10, "transitionDuration": 0, "transitionPercent": 100, "zoom": 3}"#,
        );
        assert_eq!(transform(&l, 2.5).zoom, 1.5);
        assert_eq!(transform(&l, 5.0).zoom, 2.0);
        assert_eq!(transform(&l, 7.5).zoom, 2.5);
    }

    /// A value an older writer never heard of must not fail the file.
    #[test]
    fn an_unknown_easing_falls_back_to_linear() {
        let l = layer(
            r#"{"id": "a", "time": 0, "transitionDuration": 0, "zoom": 1},
               {"id": "b", "time": 10, "transitionDuration": 0, "transitionPercent": 100,
                "zoom": 3, "easing": "bounceOutElasticWhatever"}"#,
        );
        assert_eq!(transform(&l, 5.0).zoom, 2.0);
    }

    /// Easing rides the TRACK, so one property can ease while another on the
    /// same layer does not — the whole point of per-type ramps.
    #[test]
    fn easing_is_per_track() {
        let l = layer(
            r#"{"id": "z0", "time": 0, "transitionDuration": 0, "zoom": 1},
               {"id": "z1", "time": 10, "transitionDuration": 0, "transitionPercent": 100,
                "zoom": 3, "easing": "easeInOut"},
               {"id": "p0", "time": 0, "transitionDuration": 0,
                "horizontalShift": 0, "verticalShift": 0},
               {"id": "p1", "time": 10, "transitionDuration": 0, "transitionPercent": 100,
                "horizontalShift": 100, "verticalShift": 0}"#,
        );
        // At a quarter through: zoom on smoothstep, position still straight.
        let t = transform(&l, 2.5);
        assert_eq!(t.zoom, 1.0 + 2.0 * 0.15625);
        assert_eq!(t.horizontal_shift, 25.0, "position is not eased");
    }

    /// THE failure this feature could have shipped: an eased layer must
    /// travel its motion path on the SAME clock it zooms on. Easing the
    /// scalars but not the arc-length walk would slide the layer along the
    /// curve at one rate while it grew at another.
    #[test]
    fn an_eased_move_and_its_path_share_one_clock() {
        let l: ProjectLayer = serde_json::from_str(
            r#"{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, "keyframes": [
                   {"id": "a", "time": 0, "transitionDuration": 0,
                    "zoom": 1, "horizontalShift": 0, "verticalShift": 0},
                   {"id": "b", "time": 10, "transitionDuration": 0, "transitionPercent": 100,
                    "zoom": 3, "horizontalShift": 100, "verticalShift": 0,
                    "easing": "easeInOut",
                    "motionPath": {"pathResourceID": "PATH"}}]}"#,
        )
        .expect("layer");
        let resources: Vec<promo_model::ProjectResource> = vec![serde_json::from_str(
            r#"{"id": "PATH", "kind": "path", "filename": "", "displayName": "arc",
                "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
                "path": {"start": [0, 0], "end": [100, 0],
                         "controls": [[50, -60]]}}"#,
        )
        .expect("path")];
        let defaults = settings(r#"{"canvasWidth": 64, "canvasHeight": 64}"#);

        // A quarter of the way through the ramp, smoothstep says 0.15625.
        let at = layer_transform_along_paths(&l, 2.5, &defaults, &resources);
        assert_eq!(at.zoom, 1.0 + 2.0 * 0.15625, "zoom on the eased clock");

        // The layer sits where a LINEAR ramp would put it at exactly that
        // eased progress — same clock, both properties.
        let polyline = crate::motion::path_polyline(
            &resources,
            &promo_model::MotionPath {
                path_resource_id: "PATH".into(),
                flipped: None,
                start_at: None,
                end_at: None,
            },
        )
        .expect("polyline");
        let expected = crate::motion::point_along_range(
            &polyline,
            promo_model::Point(0.0, 0.0),
            promo_model::Point(100.0, 0.0),
            false,
            0.0,
            1.0,
            0.15625,
        );
        assert!(
            (at.horizontal_shift - expected.x()).abs() < 1e-9,
            "x {} vs {}",
            at.horizontal_shift,
            expected.x()
        );
        assert!(
            (at.vertical_shift - expected.y()).abs() < 1e-9,
            "y {} vs {}",
            at.vertical_shift,
            expected.y()
        );
        // And it is genuinely off the straight line, so the check has teeth.
        assert!(
            at.vertical_shift < -1.0,
            "curved, got {}",
            at.vertical_shift
        );
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
        assert!(
            bent.vertical_shift < -1.0,
            "curved above the chord, got {}",
            bent.vertical_shift
        );
    }

    /// The palette fix that matters at 2.5s, not at the keyframes: endpoints
    /// were always resolvable downstream, but a blend of two `@names` has to
    /// resolve BEFORE it averages. Before the fix this held the first colour
    /// and snapped — a fade for literals, a cut for named colours.
    #[test]
    fn a_named_background_fade_blends_like_a_literal_one() {
        let defaults = settings(
            r#"{"canvasWidth": 64, "canvasHeight": 64,
                "palette": [{"name": "night", "colorHex": "000000"},
                            {"name": "day", "colorHex": "FFFFFF"}]}"#,
        );
        let named = layer(
            r#"{"id": "A", "time": 0, "colorHex": "@night", "transitionDuration": 0},
               {"id": "B", "time": 4, "colorHex": "@day", "transitionDuration": 4}"#,
        );
        let literal = layer(
            r#"{"id": "A", "time": 0, "colorHex": "000000", "transitionDuration": 0},
               {"id": "B", "time": 4, "colorHex": "FFFFFF", "transitionDuration": 4}"#,
        );
        for time in [0.0, 1.0, 2.0, 3.0, 4.0] {
            let named_hex = layer_background_color_hex(&named, time, &defaults);
            let literal_hex = layer_background_color_hex(&literal, time, &defaults);
            assert_eq!(
                defaults.resolve_color(&named_hex),
                literal_hex,
                "at {time}s"
            );
        }
        // And the middle really is a mix, or the equality above proves nothing.
        assert_eq!(
            layer_background_color_hex(&literal, 2.0, &defaults),
            "808080"
        );
        // Plateaus still hand back the reference itself: editors read those.
        assert_eq!(layer_background_color_hex(&named, 0.0, &defaults), "@night");
    }

    /// The settings-level twin (`backgroundKeyframes`) blends by the same rule.
    #[test]
    fn named_settings_background_keyframes_blend_too() {
        let defaults = settings(
            r#"{"canvasWidth": 64, "canvasHeight": 64,
                "backgroundKeyframes": [
                    {"id": "00000000-0000-0000-0000-000000000001",
                     "time": 0, "colorHex": "@night", "transitionDuration": 0},
                    {"id": "00000000-0000-0000-0000-000000000002",
                     "time": 4, "colorHex": "@day", "transitionDuration": 4}],
                "palette": [{"name": "night", "colorHex": "000000"},
                            {"name": "day", "colorHex": "FFFFFF"}]}"#,
        );
        assert_eq!(settings_background_color_hex(&defaults, 2.0), "808080");
        // Off the ramp the reference passes through for the draw site to
        // resolve, same contract as everywhere else.
        assert_eq!(settings_background_color_hex(&defaults, 0.0), "@night");
    }

    /// The canonical placement example: "620 tall, centered" on a portrait
    /// screenshot must land on exactly the zoom/shift numbers an author
    /// computes by hand today — placement is those numbers as a rule.
    #[test]
    fn a_placement_is_the_same_box_the_numbers_describe() {
        let defaults = settings(r#"{"canvasWidth": 1080, "canvasHeight": 1920}"#);
        let resources: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id": "IMG", "kind": "image", "filename": "i.png",
                 "displayName": "I", "addedAt": 0,
                 "pixelWidth": 1170, "pixelHeight": 2532}]"#,
        )
        .unwrap();
        let mut ruled = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "placement": {"height": 620, "anchor": "center"}}"#,
        );
        ruled.resource_id = Some("IMG".into());

        let zoom = 620.0 / 1920.0;
        let drawn_width = 1170.0 * (1920.0 / 2532.0) * zoom;
        let tr = layer_transform_along_paths(&ruled, 1.0, &defaults, &resources);
        assert!((tr.zoom - zoom).abs() < 1e-12);
        assert!((tr.horizontal_shift - (1080.0 - drawn_width) / 2.0).abs() < 1e-9);
        assert!((tr.vertical_shift - (1920.0 - 620.0) / 2.0).abs() < 1e-9);
    }

    /// Rules resolve BEFORE the lerp: halfway between two placements is
    /// halfway between their resolved numbers — same clock, same ramps as
    /// everything else.
    #[test]
    fn a_ramp_between_two_placements_blends_their_numbers() {
        let defaults = settings(r#"{"canvasWidth": 1000, "canvasHeight": 2000}"#);
        let resources: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id": "IMG", "kind": "image", "filename": "i.png",
                 "displayName": "I", "addedAt": 0,
                 "pixelWidth": 100, "pixelHeight": 100}]"#,
        )
        .unwrap();
        let mut ruled = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "placement": {"height": 400, "anchor": "topLeft"}},
               {"id": "B", "time": 4, "transitionDuration": 4,
                "placement": {"height": 800, "anchor": "bottomRight"}}"#,
        );
        ruled.resource_id = Some("IMG".into());
        let mid = layer_transform_along_paths(&ruled, 2.0, &defaults, &resources);
        // Zoom: (0.2 + 0.4) / 2. Box at each end: 400x400 at (0,0), 800x800
        // at (200, 1200); midway is their average.
        assert!((mid.zoom - 0.3).abs() < 1e-12);
        assert!((mid.horizontal_shift - 100.0).abs() < 1e-9);
        assert!((mid.vertical_shift - 600.0).abs() < 1e-9);
    }

    /// A placement outranks raw numbers written on the same keyframe, and a
    /// position-only rule keeps the keyframe's own zoom for its box.
    #[test]
    fn a_placement_wins_over_the_numbers_beside_it() {
        let defaults = settings(r#"{"canvasWidth": 1000, "canvasHeight": 2000}"#);
        let resources: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id": "IMG", "kind": "image", "filename": "i.png",
                 "displayName": "I", "addedAt": 0,
                 "pixelWidth": 100, "pixelHeight": 100}]"#,
        )
        .unwrap();
        let mut ruled = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "zoom": 0.5, "horizontalShift": 111, "verticalShift": 222,
                "placement": {"anchor": "bottomRight"}}"#,
        );
        ruled.resource_id = Some("IMG".into());
        let tr = layer_transform_along_paths(&ruled, 0.0, &defaults, &resources);
        // Position-only rule: zoom stays the keyframe's own 0.5 -> box
        // 1000x1000, hung bottom-right of a 1000x2000 canvas.
        assert_eq!(tr.zoom, 0.5);
        assert!((tr.horizontal_shift - 0.0).abs() < 1e-9);
        assert!((tr.vertical_shift - 1000.0).abs() < 1e-9);
    }

    /// The rule anchors what the layer SHOWS: a viewport that windows half
    /// the width halves the drawn box, and centering follows that.
    #[test]
    fn placement_follows_the_viewport_window() {
        let defaults = settings(r#"{"canvasWidth": 1000, "canvasHeight": 1000}"#);
        let resources: Vec<promo_model::ProjectResource> = serde_json::from_str(
            r#"[{"id": "IMG", "kind": "image", "filename": "i.png",
                 "displayName": "I", "addedAt": 0,
                 "pixelWidth": 100, "pixelHeight": 100}]"#,
        )
        .unwrap();
        let mut ruled = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "viewport": [0, 0, 0.5, 1],
                "placement": {"height": 500, "anchor": "center"}}"#,
        );
        ruled.resource_id = Some("IMG".into());
        let tr = layer_transform_along_paths(&ruled, 0.0, &defaults, &resources);
        // Windowed aspect 0.5: the box is 250 wide, 500 tall, centered.
        assert!((tr.horizontal_shift - 375.0).abs() < 1e-9);
        assert!((tr.vertical_shift - 250.0).abs() < 1e-9);
    }

    /// No stored size: the rule still resolves, assuming a square source —
    /// a degraded answer validation names, never a refusal.
    #[test]
    fn an_unmeasured_source_resolves_as_square() {
        let defaults = settings(r#"{"canvasWidth": 1000, "canvasHeight": 2000}"#);
        let ruled = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0,
                "placement": {"height": 500, "anchor": "center"}}"#,
        );
        let tr = layer_transform_along_paths(&ruled, 0.0, &defaults, &[]);
        assert!((tr.zoom - 0.25).abs() < 1e-12);
        // Square assumption: 500x500, centered on 1000x2000.
        assert!((tr.horizontal_shift - 250.0).abs() < 1e-9);
        assert!((tr.vertical_shift - 750.0).abs() < 1e-9);
    }

    /// THE regression test for placement: the feature moved this arithmetic
    /// out of the author's hands and into the engine, so the thing to prove
    /// is that the engine computes what the author used to compute.
    ///
    /// The author's formula — documented in the skill, and used by every
    /// template written before `placement` existed:
    ///
    ///     zoom   = desiredHeight / canvasHeight
    ///     drawnW = sourceWidth * (canvasHeight / sourceHeight) * zoom
    ///     hShift = (canvasWidth - drawnW) / 2          // to centre
    ///
    /// `hShift` is the line that needed the SOURCE's pixel width, which the
    /// author usually did not have — the whole reason the rule exists.
    fn author_maths(canvas: (f64, f64), source: (f64, f64), desired_h: f64) -> (f64, f64, f64) {
        let zoom = desired_h / canvas.1;
        let drawn_w = source.0 * (canvas.1 / source.1) * zoom;
        (
            (canvas.0 - drawn_w) / 2.0,
            (canvas.1 - desired_h) / 2.0,
            zoom,
        )
    }

    fn ruled_layer(
        rule: &str,
        resource_px: (f64, f64),
    ) -> (ProjectLayer, Vec<promo_model::ProjectResource>) {
        let mut layer = layer(&format!(
            r#"{{"id": "A", "time": 0, "transitionDuration": 0, "placement": {rule}}}"#
        ));
        layer.resource_id = Some("IMG".into());
        let resources = serde_json::from_str(&format!(
            r#"[{{"id": "IMG", "kind": "image", "filename": "i.png",
                  "displayName": "I", "addedAt": 0,
                  "pixelWidth": {}, "pixelHeight": {}}}]"#,
            resource_px.0, resource_px.1
        ))
        .unwrap();
        (layer, resources)
    }

    #[test]
    fn a_centred_rule_computes_what_the_author_used_to_compute() {
        // The two real fixtures are the first rows: canvas and source sizes
        // taken from templates 01 and 02, whose hand-written JSON carried
        // horizontalShift 224 and 352 respectively.
        let cases = [
            ((1440.0, 900.0), (1216.0, 760.0), 620.0, Some(224.0)),
            ((1920.0, 1080.0), (2560.0, 1600.0), 760.0, Some(352.0)),
            // ...and a matrix around them, so this is not two lucky numbers.
            ((1920.0, 1080.0), (1920.0, 1080.0), 1080.0, None),
            ((1920.0, 1080.0), (1170.0, 2532.0), 900.0, None),
            ((1080.0, 1920.0), (1216.0, 760.0), 300.0, None),
            ((2560.0, 1600.0), (800.0, 800.0), 512.0, None),
        ];
        for (canvas, source, desired_h, recorded) in cases {
            let (want_h, want_v, want_zoom) = author_maths(canvas, source, desired_h);
            let defaults = settings(&format!(
                r#"{{"canvasWidth": {}, "canvasHeight": {}}}"#,
                canvas.0, canvas.1
            ));
            let (layer, resources) = ruled_layer(
                &format!(r#"{{"height": {desired_h}, "anchor": "center"}}"#),
                source,
            );
            let tr = layer_transform_along_paths(&layer, 0.0, &defaults, &resources);
            assert!(
                (tr.zoom - want_zoom).abs() < 1e-12,
                "zoom for {canvas:?}/{source:?}: {} vs {want_zoom}",
                tr.zoom
            );
            assert!(
                (tr.horizontal_shift - want_h).abs() < 1e-9,
                "hShift for {canvas:?}/{source:?}: {} vs {want_h}",
                tr.horizontal_shift
            );
            assert!(
                (tr.vertical_shift - want_v).abs() < 1e-9,
                "vShift for {canvas:?}/{source:?}: {} vs {want_v}",
                tr.vertical_shift
            );
            // Where a real project recorded the number by hand, the engine
            // must land on that exact value — not merely on its own formula.
            if let Some(recorded) = recorded {
                assert!(
                    (tr.horizontal_shift - recorded).abs() < 1e-9,
                    "the hand-written project said {recorded}, engine says {}",
                    tr.horizontal_shift
                );
            }
        }
    }

    /// The nine anchors are the same arithmetic with the divisor changed, so
    /// they are pinned against it rather than against copied constants.
    #[test]
    fn every_anchor_agrees_with_the_arithmetic_it_replaces() {
        let canvas = (1920.0, 1080.0);
        let source = (1216.0, 760.0);
        let desired_h = 620.0;
        let (centre_h, centre_v, _) = author_maths(canvas, source, desired_h);
        let drawn_w = canvas.0 - 2.0 * centre_h;
        let defaults = settings(r#"{"canvasWidth": 1920, "canvasHeight": 1080}"#);
        for (anchor, want_h, want_v) in [
            ("topLeft", 0.0, 0.0),
            ("top", centre_h, 0.0),
            ("topRight", canvas.0 - drawn_w, 0.0),
            ("left", 0.0, centre_v),
            ("center", centre_h, centre_v),
            ("right", canvas.0 - drawn_w, centre_v),
            ("bottomLeft", 0.0, canvas.1 - desired_h),
            ("bottom", centre_h, canvas.1 - desired_h),
            ("bottomRight", canvas.0 - drawn_w, canvas.1 - desired_h),
        ] {
            let (layer, resources) = ruled_layer(
                &format!(r#"{{"height": {desired_h}, "anchor": "{anchor}"}}"#),
                source,
            );
            let tr = layer_transform_along_paths(&layer, 0.0, &defaults, &resources);
            assert!((tr.horizontal_shift - want_h).abs() < 1e-9, "{anchor} h");
            assert!((tr.vertical_shift - want_v).abs() < 1e-9, "{anchor} v");
        }
    }

    /// And the half the author's arithmetic could NOT do: the numbers were
    /// baked against one source, so swapping the media for another aspect
    /// left them wrong. The rule is resolved against whatever is actually
    /// there, so it stays centred.
    #[test]
    fn the_rule_re_resolves_when_the_source_changes_where_baked_numbers_would_not() {
        let canvas = (1920.0, 1080.0);
        let defaults = settings(r#"{"canvasWidth": 1920, "canvasHeight": 1080}"#);
        let wide = (1216.0, 760.0);
        let tall = (1170.0, 2532.0);
        let baked_for_wide = author_maths(canvas, wide, 620.0).0;

        for source in [wide, tall] {
            let (layer, resources) = ruled_layer(r#"{"height": 620, "anchor": "center"}"#, source);
            let tr = layer_transform_along_paths(&layer, 0.0, &defaults, &resources);
            let drawn_w = source.0 * (canvas.1 / source.1) * tr.zoom;
            // Centred against THIS source, whatever it is.
            assert!((tr.horizontal_shift - (canvas.0 - drawn_w) / 2.0).abs() < 1e-9);
            assert!(
                (tr.zoom - 620.0 / canvas.1).abs() < 1e-12,
                "height is honoured either way"
            );
        }
        // The baked number, meanwhile, is wrong by hundreds of pixels on the
        // second source — which is the failure this feature exists to remove.
        let (layer, resources) = ruled_layer(r#"{"height": 620, "anchor": "center"}"#, tall);
        let tr = layer_transform_along_paths(&layer, 0.0, &defaults, &resources);
        assert!(
            (tr.horizontal_shift - baked_for_wide).abs() > 300.0,
            "the two sources must genuinely disagree for this test to mean anything"
        );
    }

    /// New projects no longer seed the pre-layer timeline, so both readers
    /// meet an empty list routinely rather than only in odd old files.
    #[test]
    fn an_empty_legacy_timeline_reads_as_no_timeline_at_all() {
        let settings = promo_model::CompositionSettings::default();
        assert!(
            settings.video_keyframes.is_empty(),
            "default must not seed one"
        );
        let transform = settings_interpolated_values(&settings, 3.0);
        assert_eq!(transform.zoom, 1.0);
        assert_eq!(transform.vertical_shift, 0.0);
        assert_eq!(transform.horizontal_shift, 0.0);
        assert_eq!(
            settings_background_color_hex(&settings, 3.0),
            settings.background_color_hex,
            "with no keyframes the flat background colour is the answer"
        );
    }
}
