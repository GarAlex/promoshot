//! Follow the pointer: a viewport derived from a recording's pointer
//! track, and the clicks to ring.
//!
//! Stateless on purpose — a render asks for frames in any order — so the
//! smoothing is an exponentially weighted average over the samples BEHIND
//! the instant, which every frame can compute alone and every frame
//! agrees on.

use promo_model::{PointerTrack, ProjectLayer, ProjectResource};

/// How long a click's ring lives, in source seconds.
pub const RING_SECONDS: f64 = 0.5;

/// The window to show at source time `t`, unit coordinates `[x, y, w, h]`,
/// or `None` when the layer does not follow, the resource has no track,
/// or the track is empty.
pub fn follow_viewport(
    layer: &ProjectLayer,
    resource: &ProjectResource,
    t: f64,
) -> Option<[f64; 4]> {
    let follow = layer.follow.as_ref()?;
    let track = resource.pointer.as_ref()?;
    let (cx, cy) = smoothed(track, t, follow.smoothing.unwrap_or(0.35).max(0.01))?;
    let zoom = follow.zoom.unwrap_or(2.0).max(1.0);
    let (w, h) = (1.0 / zoom, 1.0 / zoom);
    Some(crate::viewport::clamped_window([
        cx - w / 2.0,
        cy - h / 2.0,
        w,
        h,
    ]))
}

/// The pointer's smoothed position at `t`: samples at or before `t`
/// weighted by `exp(-(t - s) / tau)`, over the last `4 tau` seconds. The
/// first sample stands for everything before it.
pub fn smoothed(track: &PointerTrack, t: f64, tau: f64) -> Option<(f64, f64)> {
    let first = track.samples.first()?;
    if t <= first[0] {
        return Some((first[1], first[2]));
    }
    let horizon = t - 4.0 * tau;
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    // The position the pointer HELD between samples counts for the whole
    // hold: integrate each held span against the kernel, so a pointer that
    // stopped for a second is where it stopped, not where it was going.
    let mut prev: Option<[f64; 3]> = None;
    for s in track.samples.iter().chain(std::iter::once(&[t, 0.0, 0.0])) {
        let s = *s;
        if let Some(p) = prev {
            let (a, b) = (p[0].max(horizon), s[0].min(t));
            if b > a {
                // ∫ exp(-(t - u)/tau) du over [a, b].
                let weight = tau * ((-(t - b) / tau).exp() - (-(t - a) / tau).exp());
                sx += p[1] * weight;
                sy += p[2] * weight;
                sw += weight;
            }
        }
        if s[0] > t {
            break;
        }
        prev = Some(s);
    }
    if sw <= 0.0 {
        // Nothing within the horizon: hold the last sample before t.
        let last = track.samples.iter().rev().find(|s| s[0] <= t)?;
        return Some((last[1], last[2]));
    }
    Some((sx / sw, sy / sw))
}

/// The clicks alive at source time `t`: unit position and age in seconds
/// (0 just clicked, `RING_SECONDS` about to vanish).
pub fn live_clicks(
    layer: &ProjectLayer,
    resource: &ProjectResource,
    t: f64,
) -> Vec<(f64, f64, f64)> {
    let Some(follow) = layer.follow.as_ref() else {
        return Vec::new();
    };
    if !follow.clicks.unwrap_or(true) {
        return Vec::new();
    }
    let Some(track) = resource.pointer.as_ref() else {
        return Vec::new();
    };
    track
        .clicks
        .iter()
        .filter(|c| c[0] <= t && t - c[0] < RING_SECONDS)
        .map(|c| (c[1], c[2], t - c[0]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(samples: &[[f64; 3]], clicks: &[[f64; 3]]) -> ProjectResource {
        serde_json::from_value(serde_json::json!({
            "id": "R", "kind": "video", "filename": "r.mp4", "displayName": "r", "addedAt": 0,
            "pointer": {"samples": samples, "clicks": clicks}
        }))
        .expect("resource")
    }

    fn following(zoom: f64) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id":"L","name":"L","sortIndex":0,"kind":"video","isEnabled":true,"startTime":0,
                 "duration":10,"resourceID":"R","follow":{{"zoom":{zoom}}},"keyframes":[]}}"#
        ))
        .expect("layer")
    }

    /// A pointer that jumps from the left to the right is FOLLOWED, not
    /// snapped to: shortly after the jump the window is still mostly on
    /// the left, and after a few time constants it has arrived.
    #[test]
    fn the_window_follows_a_jump_with_a_lag_and_settles() {
        let rec = track(&[[0.0, 0.25, 0.5], [2.0, 0.75, 0.5]], &[]);
        let layer = following(2.0);
        let before = follow_viewport(&layer, &rec, 1.0).unwrap();
        assert!(
            (before[0] - 0.0).abs() < 1e-9 && (before[2] - 0.5).abs() < 1e-9,
            "on the left: {before:?}"
        );
        let soon = follow_viewport(&layer, &rec, 2.1).unwrap();
        assert!(
            soon[0] > 0.0 && soon[0] < 0.25,
            "moving, not there yet: {soon:?}"
        );
        let later = follow_viewport(&layer, &rec, 4.0).unwrap();
        assert!(
            (later[0] - 0.5).abs() < 0.01,
            "arrived at the right: {later:?}"
        );
        assert!(
            follow_viewport(&following(1.0), &rec, 3.0).unwrap() == [0.0, 0.0, 1.0, 1.0],
            "zoom 1 shows it all"
        );
    }

    #[test]
    fn a_pointer_that_stops_is_where_it_stopped() {
        let rec = track(&[[0.0, 0.2, 0.2], [1.0, 0.8, 0.8]], &[]);
        let (x, y) = smoothed(rec.pointer.as_ref().unwrap(), 30.0, 0.35).unwrap();
        assert!((x - 0.8).abs() < 1e-6 && (y - 0.8).abs() < 1e-6);
    }

    #[test]
    fn clicks_live_for_half_a_second_and_can_be_switched_off() {
        let rec = track(&[[0.0, 0.5, 0.5]], &[[1.0, 0.6, 0.4], [3.0, 0.1, 0.1]]);
        let layer = following(2.0);
        assert_eq!(live_clicks(&layer, &rec, 0.9).len(), 0);
        let live = live_clicks(&layer, &rec, 1.2);
        assert_eq!(live.len(), 1);
        assert!((live[0].2 - 0.2).abs() < 1e-9 && live[0].0 == 0.6);
        assert_eq!(live_clicks(&layer, &rec, 1.6).len(), 0);
        let mut quiet = layer.clone();
        quiet.follow.as_mut().unwrap().clicks = Some(false);
        assert!(live_clicks(&quiet, &rec, 1.2).is_empty());
        let mut plain = layer.clone();
        plain.follow = None;
        assert!(follow_viewport(&plain, &rec, 1.0).is_none());
    }
}
