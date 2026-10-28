//! An SVG's elements in motion (SVG-PLAN.md slice 3): the overrides a
//! layer draws its vector with at one instant — the resource's static
//! overrides, with every element the layer's keyframes key (`nodes`)
//! evaluated on the layer's clock.
//!
//! Each (element, key) is its own track, the way each keyframe field is:
//! the keyframes that key it, held then eased by the ramp INTO each
//! (`transitionDuration` / `transitionPercent`, `easing`, `overshoot`),
//! the same window and blend every layer scalar uses. Before the first
//! keyframe a track holds its first value, after the last its last. A key
//! never written on any keyframe is the resource's override, else the
//! file's own value. Colours mix per channel the way a background colour
//! does; `visible` is a step (no halfway between shown and hidden), held
//! from the keyframe that says it — before the first, the base.
//!
//! A keyed `transform` replaces the base's component by component (move,
//! scale and rotate are separate tracks), so a key that only turns an
//! element keeps the move its resource gives it.

use std::collections::BTreeSet;

use promo_model::vector::{NodeKey, NodeScale, NodeTransform, VectorOverride};
use promo_model::{ProjectLayer, ProjectLayerKeyframe};

use crate::interpolation::{blend, sorted_by_time, track_window};

/// Whether any keyframe of `layer` keys an SVG element.
pub fn keys_vector_nodes(layer: &ProjectLayer) -> bool {
    layer
        .keyframes
        .iter()
        .any(|k| k.nodes.as_ref().is_some_and(|n| !n.is_empty()))
}

/// The overrides `layer` draws its vector with at `local_time` (the
/// layer's own seconds): `base` (the resource's overrides) with every
/// keyed element's keys evaluated. `base` itself, unchanged, when nothing
/// is keyed — so a layer without motion draws exactly the static picture.
pub fn layer_vector_overrides(
    layer: &ProjectLayer,
    local_time: f64,
    base: &[VectorOverride],
) -> Vec<VectorOverride> {
    if !keys_vector_nodes(layer) {
        return base.to_vec();
    }
    let addresses: BTreeSet<&str> = layer
        .keyframes
        .iter()
        .flat_map(|k| k.nodes.iter().flatten().map(|(a, _)| a.as_str()))
        .collect();
    let mut out = base.to_vec();
    for address in addresses {
        let at = match out.iter().position(|o| o.node == address) {
            Some(i) => i,
            None => {
                out.push(VectorOverride {
                    node: address.to_string(),
                    ..Default::default()
                });
                out.len() - 1
            }
        };
        let o = &mut out[at];
        let a = address;
        let t = local_time;

        if let Some(v) = step(layer, t, |k| node_key(k, a)?.visible) {
            o.visible = Some(v);
        }
        if let Some(v) = scalar(layer, t, |k| node_key(k, a)?.opacity) {
            o.opacity = Some(v.clamp(0.0, 1.0));
        }
        if let Some(v) = scalar(layer, t, |k| node_key(k, a)?.stroke_width) {
            o.stroke_width = Some(v.max(0.0));
        }
        if let Some(v) = colour(layer, t, |k| node_key(k, a)?.fill.clone()) {
            o.fill = Some(v);
        }
        if let Some(v) = colour(layer, t, |k| node_key(k, a)?.stroke.clone()) {
            o.stroke = Some(v);
        }
        if let Some(v) = stops(layer, t, |k| node_key(k, a)?.fill_stops.clone()) {
            o.fill_stops = Some(v);
        }
        if let Some(v) = stops(layer, t, |k| node_key(k, a)?.stroke_stops.clone()) {
            o.stroke_stops = Some(v);
        }

        let dx = scalar(layer, t, |k| Some(part(k, a)?.offset?[0]));
        let dy = scalar(layer, t, |k| Some(part(k, a)?.offset?[1]));
        let sx = scalar(layer, t, |k| axes(k, a).map(|s| s.0));
        let sy = scalar(layer, t, |k| axes(k, a).map(|s| s.1));
        let turn = scalar(layer, t, |k| part(k, a)?.rotate);
        if dx.is_some() || sx.is_some() || turn.is_some() {
            let mut tr = o.transform.clone().unwrap_or_default();
            if let (Some(x), Some(y)) = (dx, dy) {
                tr.offset = Some([x, y]);
            }
            if let (Some(x), Some(y)) = (sx, sy) {
                tr.scale = Some(if x == y {
                    NodeScale::Uniform(x)
                } else {
                    NodeScale::Axes([x, y])
                });
            }
            if let Some(r) = turn {
                tr.rotate = Some(r);
            }
            o.transform = (tr != NodeTransform::default()).then_some(tr);
        }
    }
    out
}

fn node_key<'k>(k: &'k ProjectLayerKeyframe, address: &str) -> Option<&'k NodeKey> {
    k.nodes.as_ref()?.get(address)
}

fn part<'k>(k: &'k ProjectLayerKeyframe, address: &str) -> Option<&'k NodeTransform> {
    node_key(k, address)?.transform.as_ref()
}

fn axes(k: &ProjectLayerKeyframe, address: &str) -> Option<(f64, f64)> {
    let t = part(k, address)?;
    t.scale.map(|_| t.scales())
}

/// One scalar track: hold, ease, the blend every layer scalar uses.
fn scalar<F>(layer: &ProjectLayer, t: f64, select: F) -> Option<f64>
where
    F: Fn(&ProjectLayerKeyframe) -> Option<f64>,
{
    let sorted = sorted_by_time(&layer.keyframes, |k| select(k).is_some());
    let (a, b, progress) = track_window(&sorted, t)?;
    let value = |k: &ProjectLayerKeyframe| select(k).unwrap_or(0.0);
    Some(blend(&sorted, a, b, progress, &value))
}

/// A step: the value of the last keyframe at or before `t` that says one;
/// none before the first.
fn step<F>(layer: &ProjectLayer, t: f64, select: F) -> Option<bool>
where
    F: Fn(&ProjectLayerKeyframe) -> Option<bool>,
{
    sorted_by_time(&layer.keyframes, |k| select(k).is_some())
        .into_iter()
        .take_while(|k| k.time <= t)
        .last()
        .and_then(select)
}

/// A colour track: the window, then the two colours mixed at the eased
/// progress — clamped, since a colour has no overshoot.
fn colour<F>(layer: &ProjectLayer, t: f64, select: F) -> Option<String>
where
    F: Fn(&ProjectLayerKeyframe) -> Option<String>,
{
    let sorted = sorted_by_time(&layer.keyframes, |k| select(k).is_some());
    let (a, b, progress) = track_window(&sorted, t)?;
    Some(mix(&select(a)?, &select(b)?, progress))
}

fn stops<F>(layer: &ProjectLayer, t: f64, select: F) -> Option<Vec<String>>
where
    F: Fn(&ProjectLayerKeyframe) -> Option<Vec<String>>,
{
    let sorted = sorted_by_time(&layer.keyframes, |k| select(k).is_some());
    let (a, b, progress) = track_window(&sorted, t)?;
    let (from, to) = (select(a)?, select(b)?);
    if from.len() != to.len() {
        // No stop-for-stop mix: hold, then arrive.
        return Some(if progress < 1.0 { from } else { to });
    }
    Some(
        from.iter()
            .zip(&to)
            .map(|(x, y)| mix(x, y, progress))
            .collect(),
    )
}

/// `RRGGBB` or `RRGGBBAA` (a leading `#` allowed) → channels.
fn channels(text: &str) -> Option<(Vec<f64>, bool)> {
    let t = text.trim().trim_start_matches('#');
    if !(t.len() == 6 || t.len() == 8) || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let values = (0..t.len() / 2)
        .map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).map(f64::from))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some((values, t.len() == 8))
}

/// Two colours `progress` of the way, per channel and rounded — the rule
/// [`crate::interpolate_color_hex`] mixes a background colour by, with
/// alpha mixed too when either side states one (a side that states none
/// is opaque). At the ends each colour stands as written; an unparseable
/// one holds, then arrives.
fn mix(from: &str, to: &str, progress: f64) -> String {
    let p = progress.clamp(0.0, 1.0);
    if p <= 0.0 {
        return from.to_string();
    }
    if p >= 1.0 {
        return to.to_string();
    }
    let (Some((mut a, a_alpha)), Some((mut b, b_alpha))) = (channels(from), channels(to)) else {
        return from.to_string();
    };
    let alpha = a_alpha || b_alpha;
    if alpha {
        a.resize(4, 255.0);
        b.resize(4, 255.0);
    }
    a.iter()
        .zip(&b)
        .map(|(x, y)| format!("{:02X}", (x + (y - x) * p).round() as i64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(keyframes: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id":"L","name":"Logo","sortIndex":1,"kind":"image","isEnabled":true,
                "startTime":2,"duration":10,"resourceID":"V","keyframes":[{keyframes}]}}"#
        ))
        .unwrap()
    }

    fn at(layer: &ProjectLayer, t: f64, base: &[VectorOverride], node: &str) -> VectorOverride {
        layer_vector_overrides(layer, t, base)
            .into_iter()
            .find(|o| o.node == node)
            .unwrap()
    }

    /// No keys: the static overrides, untouched.
    #[test]
    fn nothing_keyed_is_the_base() {
        let l = layer(r#"{"id":"A","time":0,"zoom":1}"#);
        let base = vec![VectorOverride {
            node: "badge".into(),
            opacity: Some(0.4),
            ..Default::default()
        }];
        assert!(!keys_vector_nodes(&l));
        assert_eq!(layer_vector_overrides(&l, 3.0, &base), base);
    }

    /// Every interpolated kind pinned at its midpoint, on a linear ramp
    /// of the whole gap: opacity, stroke width, move, scale (one factor
    /// and two), rotate, fill (with and without alpha), stops — and the
    /// ends, held before the first and after the last.
    #[test]
    fn every_kind_at_its_midpoint() {
        let l = layer(
            r#"{"id":"A","time":1,"transitionDuration":0,"nodes":{"badge":{
                   "opacity":0,"strokeWidth":2,"fill":"000000","stroke":"00000000",
                   "fillStops":["000000","FF0000"],
                   "transform":{"move":[0,0],"scale":0.5,"rotate":-90}}}},
               {"id":"B","time":3,"transitionPercent":100,"nodes":{"badge":{
                   "opacity":1,"strokeWidth":6,"fill":"FF8040","stroke":"FFFFFFFF",
                   "fillStops":["FFFFFF","0000FF"],
                   "transform":{"move":[40,-20],"scale":[1.5,2.5],"rotate":90}}}}"#,
        );
        let mid = at(&l, 2.0, &[], "badge");
        assert_eq!(mid.opacity, Some(0.5));
        assert_eq!(mid.stroke_width, Some(4.0));
        assert_eq!(mid.fill.as_deref(), Some("804020"));
        assert_eq!(mid.stroke.as_deref(), Some("80808080"));
        assert_eq!(
            mid.fill_stops,
            Some(vec!["808080".to_string(), "800080".to_string()])
        );
        let tr = mid.transform.unwrap();
        assert_eq!(tr.offset, Some([20.0, -10.0]));
        assert_eq!(tr.scales(), (1.0, 1.5));
        assert_eq!(tr.rotate, Some(0.0));
        // Held at each end, the colours as written.
        let before = at(&l, 0.0, &[], "badge");
        assert_eq!(before.opacity, Some(0.0));
        assert_eq!(before.fill.as_deref(), Some("000000"));
        let after = at(&l, 9.0, &[], "badge");
        assert_eq!(after.opacity, Some(1.0));
        assert_eq!(after.fill.as_deref(), Some("FF8040"));
        assert_eq!(after.transform.unwrap().scales(), (1.5, 2.5));
    }

    /// Hold-then-ease: the default half-second ramp INTO the second key
    /// holds the first value until it starts; an ease-out pins its own
    /// curve's midpoint.
    #[test]
    fn hold_then_ease() {
        let l = layer(
            r#"{"id":"A","time":0,"nodes":{"b":{"opacity":0}}},
               {"id":"B","time":2,"nodes":{"b":{"opacity":1}}},
               {"id":"C","time":3,"transitionDuration":1,"easing":"easeOut","nodes":{"b":{"opacity":0}}}"#,
        );
        assert_eq!(at(&l, 1.4, &[], "b").opacity, Some(0.0), "held");
        assert_eq!(at(&l, 1.75, &[], "b").opacity, Some(0.5), "linear midpoint");
        let eased = promo_model::Easing::EaseOut.curve(0.5, None);
        let mid = at(&l, 2.5, &[], "b").opacity.unwrap();
        assert!((mid - (1.0 - eased)).abs() < 1e-12, "{mid}");
    }

    /// `visible` steps on the keyframe that says it; before the first,
    /// the base's.
    #[test]
    fn visible_is_a_step() {
        let l = layer(
            r#"{"id":"A","time":1,"nodes":{"b":{"visible":false}}},
               {"id":"B","time":2,"nodes":{"b":{"visible":true}}}"#,
        );
        let base = vec![VectorOverride {
            node: "b".into(),
            visible: Some(true),
            fill: Some("FF0000".into()),
            ..Default::default()
        }];
        assert_eq!(at(&l, 0.5, &base, "b").visible, Some(true));
        assert_eq!(at(&l, 1.0, &base, "b").visible, Some(false));
        assert_eq!(at(&l, 1.99, &base, "b").visible, Some(false));
        assert_eq!(at(&l, 2.0, &base, "b").visible, Some(true));
        // A key never written stays the base's.
        assert_eq!(at(&l, 1.5, &base, "b").fill.as_deref(), Some("FF0000"));
    }

    /// A keyed turn keeps the base's move: components are tracks.
    #[test]
    fn transform_components_are_tracks() {
        let l = layer(
            r#"{"id":"A","time":0,"nodes":{"b":{"transform":{"rotate":0}}}},
               {"id":"B","time":1,"transitionDuration":1,"nodes":{"b":{"transform":{"rotate":30}}}}"#,
        );
        let base = vec![VectorOverride {
            node: "b".into(),
            transform: Some(NodeTransform {
                offset: Some([5.0, 6.0]),
                ..Default::default()
            }),
            ..Default::default()
        }];
        let tr = at(&l, 0.5, &base, "b").transform.unwrap();
        assert_eq!(tr.offset, Some([5.0, 6.0]));
        assert_eq!(tr.rotate, Some(15.0));
        assert_eq!(tr.scale, None);
    }

    #[test]
    fn colours_mix_like_a_background() {
        assert_eq!(
            mix("102030", "203040", 0.5),
            crate::interpolate_color_hex("102030", "203040", 0.5)
        );
        assert_eq!(mix("#000000", "FFFFFF80", 0.5), "808080C0");
        assert_eq!(mix("nope", "FFFFFF", 0.5), "nope");
    }
}
