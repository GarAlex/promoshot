//! A path in the STAGE (rung 40): the 3D twin of a motion path.
//!
//! A `path` resource may carry a `route` — points in stage radii, its
//! own origin anywhere — and a `motionPath` on a stage member's keyframe
//! or on a camera bends the move into that keyframe along it. The fit is
//! the 2D rule lifted into space: the route's first point lands on where
//! the move starts, its last on where it ends, a similarity (translate,
//! turn, uniform scale) derived from the two chords, with the turn the
//! smallest one that carries the route's chord onto the move's. A closed
//! route (an orbit, a loop) plays at its own drawn size from the start.
//! Progress is measured in distance, so motion along a curve is even.
use crate::interpolation::{sorted_by_time, track_window};
use promo_model::{MotionPath, ProjectLayer, ProjectResource, ProjectResourceKind, Route};

type V3 = [f64; 3];

const MIN_SEGMENT: f64 = 1e-6;
const MIN_LENGTH: f64 = 1e-4;
/// Samples per span of a smooth route.
const CURVE_SAMPLES: usize = 16;

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn lerp(a: V3, b: V3, t: f64) -> V3 {
    add(a, scale(sub(b, a), t))
}

/// A route sampled to a polyline with an arc-length table.
#[derive(Debug, Clone, PartialEq)]
pub struct Route3 {
    points: Vec<V3>,
    lengths: Vec<f64>,
    closed: bool,
}

impl Route3 {
    /// A polyline through `points`, or `None` when they cannot be a route:
    /// fewer than two distinct points, a non-finite coordinate, no length.
    pub fn new(points: Vec<V3>, closed: bool) -> Option<Self> {
        if points.iter().any(|p| p.iter().any(|c| !c.is_finite())) {
            return None;
        }
        let mut cleaned: Vec<V3> = Vec::with_capacity(points.len() + 1);
        for p in points {
            if cleaned.last().is_some_and(|q| length(sub(p, *q)) < MIN_SEGMENT) {
                continue;
            }
            cleaned.push(p);
        }
        if closed {
            if let Some(first) = cleaned.first().copied() {
                if cleaned.last().is_some_and(|q| length(sub(first, *q)) >= MIN_SEGMENT) {
                    cleaned.push(first);
                }
            }
        }
        if cleaned.len() < 2 {
            return None;
        }
        let mut lengths = Vec::with_capacity(cleaned.len());
        let mut total = 0.0;
        lengths.push(0.0);
        for pair in cleaned.windows(2) {
            total += length(sub(pair[1], pair[0]));
            lengths.push(total);
        }
        if total < MIN_LENGTH {
            return None;
        }
        Some(Route3 {
            points: cleaned,
            lengths,
            closed,
        })
    }

    /// The route a resource means: its points, through a Catmull-Rom
    /// curve when `curve` is smooth (the default), straight otherwise.
    pub fn from_route(route: &Route) -> Option<Self> {
        let closed = route.closed();
        let pts: Vec<V3> = route.points.clone();
        if route.curve() == "linear" || pts.len() < 3 {
            return Self::new(pts, closed);
        }
        // Catmull-Rom through the points; the ends are padded by
        // reflection (open) or by wrapping (closed).
        let n = pts.len();
        let at = |i: isize| -> V3 {
            if closed {
                pts[i.rem_euclid(n as isize) as usize]
            } else if i < 0 {
                sub(scale(pts[0], 2.0), pts[1])
            } else if i >= n as isize {
                sub(scale(pts[n - 1], 2.0), pts[n - 2])
            } else {
                pts[i as usize]
            }
        };
        let spans = if closed { n } else { n - 1 };
        let mut out = Vec::with_capacity(spans * CURVE_SAMPLES + 1);
        for s in 0..spans {
            let (p0, p1, p2, p3) = (
                at(s as isize - 1),
                at(s as isize),
                at(s as isize + 1),
                at(s as isize + 2),
            );
            for k in 0..CURVE_SAMPLES {
                let t = k as f64 / CURVE_SAMPLES as f64;
                let (t2, t3) = (t * t, t * t * t);
                let mut p = [0.0; 3];
                for c in 0..3 {
                    p[c] = 0.5
                        * ((2.0 * p1[c])
                            + (-p0[c] + p2[c]) * t
                            + (2.0 * p0[c] - 5.0 * p1[c] + 4.0 * p2[c] - p3[c]) * t2
                            + (-p0[c] + 3.0 * p1[c] - 3.0 * p2[c] + p3[c]) * t3);
                }
                out.push(p);
            }
        }
        out.push(if closed { pts[0] } else { pts[n - 1] });
        Self::new(out, closed)
    }

    pub fn total_length(&self) -> f64 {
        *self.lengths.last().unwrap_or(&0.0)
    }
    pub fn start(&self) -> V3 {
        self.points[0]
    }
    pub fn end(&self) -> V3 {
        self.points[self.points.len() - 1]
    }
    /// Declared closed, or returning to where it began.
    pub fn is_closed(&self) -> bool {
        self.closed || length(sub(self.start(), self.end())) < self.total_length() * 0.02
    }

    /// The point `progress` of the way along, by DISTANCE; clamped.
    pub fn point_at(&self, progress: f64) -> V3 {
        let total = self.total_length();
        if progress <= 0.0 || total <= 0.0 {
            return self.start();
        }
        if progress >= 1.0 {
            return self.end();
        }
        let target = progress * total;
        let index = match self
            .lengths
            .binary_search_by(|v| v.partial_cmp(&target).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(exact) => return self.points[exact],
            Err(insert) => insert.max(1).min(self.points.len() - 1),
        };
        let (before, after) = (self.points[index - 1], self.points[index]);
        let (l0, l1) = (self.lengths[index - 1], self.lengths[index]);
        let t = if l1 > l0 { (target - l0) / (l1 - l0) } else { 0.0 };
        lerp(before, after, t)
    }

    /// The direction of travel at `progress`, unit length.
    pub fn tangent_at(&self, progress: f64) -> V3 {
        let h = 0.002;
        let a = self.point_at((progress - h).max(0.0));
        let b = self.point_at((progress + h).min(1.0));
        let d = sub(b, a);
        let l = length(d);
        if l < MIN_SEGMENT {
            [0.0, 0.0, -1.0]
        } else {
            scale(d, 1.0 / l)
        }
    }
}

/// The route a motion path names, when the resource is a `path` with one.
pub fn route_of(resources: &[ProjectResource], path: &MotionPath) -> Option<Route3> {
    let resource = resources
        .iter()
        .find(|r| r.id == path.path_resource_id && r.kind == ProjectResourceKind::Path)?;
    Route3::from_route(resource.route.as_ref()?)
}

/// The similarity that carries the route's chord onto `from → to`,
/// applied to `local` (a point relative to the route's chord origin):
/// the smallest turn from one chord direction to the other, a uniform
/// scale by their lengths, then the move to `from`.
fn fit(local: V3, chord: V3, from: V3, to: V3) -> V3 {
    let chord_length = length(chord);
    let target = sub(to, from);
    let target_length = length(target);
    if chord_length < MIN_SEGMENT || target_length < MIN_SEGMENT {
        // Nowhere to aim: the route plays at its own size from `from`.
        return add(from, local);
    }
    let s = target_length / chord_length;
    let u = scale(chord, 1.0 / chord_length);
    let v = scale(target, 1.0 / target_length);
    let axis = cross(u, v);
    let sin = length(axis);
    let cos = dot(u, v).clamp(-1.0, 1.0);
    let rotate = |p: V3| -> V3 {
        if sin < 1e-9 {
            if cos > 0.0 {
                p
            } else {
                // Straight back: turn half a circle about any axis square to u.
                let mut k = cross(u, [0.0, 1.0, 0.0]);
                if length(k) < 1e-9 {
                    k = cross(u, [1.0, 0.0, 0.0]);
                }
                let k = scale(k, 1.0 / length(k));
                // Rodrigues with θ = π: p' = 2 k (k·p) − p
                sub(scale(k, 2.0 * dot(k, p)), p)
            }
        } else {
            let k = scale(axis, 1.0 / sin);
            // Rodrigues: p cosθ + (k×p) sinθ + k (k·p)(1 − cosθ)
            add(
                add(scale(p, cos), scale(cross(k, p), sin)),
                scale(k, dot(k, p) * (1.0 - cos)),
            )
        }
    };
    let turned = rotate(local);
    // The one freedom a 3D fit has that a 2D one never had: the roll about
    // the chord. Settle it so the route's own UP lands as near world up as
    // the chord allows — an arc drawn bulging upward bulges upward in the
    // stage, whatever the move's direction.
    let up_image = rotate([0.0, 1.0, 0.0]);
    let world_up = [0.0, 1.0, 0.0];
    let flat = |w: V3| sub(w, scale(v, dot(w, v)));
    let (a, b) = (flat(up_image), flat(world_up));
    let (la, lb) = (length(a), length(b));
    let rolled = if la > 1e-6 && lb > 1e-6 {
        let a = scale(a, 1.0 / la);
        let b = scale(b, 1.0 / lb);
        let c = dot(a, b).clamp(-1.0, 1.0);
        let sn = dot(cross(a, b), v);
        // Rodrigues about v by the angle from a to b.
        add(
            add(scale(turned, c), scale(cross(v, turned), sn)),
            scale(v, dot(v, turned) * (1.0 - c)),
        )
    } else {
        turned
    };
    add(from, scale(rolled, s))
}

/// Where something is at `progress` of the way from `from` to `to`,
/// following the route between `start_at` and `end_at` of its length
/// (`start_at` above `end_at` runs it backwards), mirrored across the
/// chord when `flipped`.
#[allow(clippy::too_many_arguments)]
pub fn point_along3(
    route: &Route3,
    from: V3,
    to: V3,
    flipped: bool,
    start_at: f64,
    end_at: f64,
    progress: f64,
) -> V3 {
    let (start_at, end_at) = (start_at.clamp(0.0, 1.0), end_at.clamp(0.0, 1.0));
    let along = start_at + (end_at - start_at) * progress.clamp(0.0, 1.0);
    let origin = route.point_at(start_at);
    let finish = route.point_at(end_at);
    let chord = sub(finish, origin);
    let mut local = sub(route.point_at(along), origin);
    if flipped {
        let l = length(chord);
        if l > MIN_SEGMENT {
            let u = scale(chord, 1.0 / l);
            local = sub(scale(u, 2.0 * dot(local, u)), local);
        } else {
            local = [local[0], -local[1], local[2]];
        }
    }
    fit(local, chord, from, to)
}

/// The direction of travel at `progress`, in the fitted space.
#[allow(clippy::too_many_arguments)]
pub fn tangent_along3(
    route: &Route3,
    from: V3,
    to: V3,
    flipped: bool,
    start_at: f64,
    end_at: f64,
    progress: f64,
) -> V3 {
    let h = 0.004;
    let a = point_along3(route, from, to, flipped, start_at, end_at, (progress - h).max(0.0));
    let b = point_along3(route, from, to, flipped, start_at, end_at, (progress + h).min(1.0));
    let d = sub(b, a);
    let l = length(d);
    if l < MIN_SEGMENT {
        sub(to, from)
    } else {
        scale(d, 1.0 / l)
    }
}

/// The two camera keyframes a moment lies between and how far along the
/// eased ramp it is — the same window every other keyframe value uses.
pub fn camera_window<'a>(
    track: &[&'a promo_model::ProjectLayerKeyframe],
    local_time: f64,
) -> Option<(&'a promo_model::ProjectLayerKeyframe, &'a promo_model::ProjectLayerKeyframe, f64)> {
    track_window(track, local_time)
}

/// A stage member's place at `local_time` — `[across, up, depth]` in the
/// stage's radii — from its `stageOffset` and `depth` keyframes, the move
/// into a keyframe bent along its `motionPath` when that names a route.
/// `None` when no keyframe places it.
pub fn member_position(layer: &ProjectLayer, local_time: f64, resources: &[ProjectResource]) -> Option<V3> {
    let track = sorted_by_time(&layer.keyframes, |k| k.stage_offset.is_some() || k.depth.is_some());
    let (a, b, progress) = track_window(&track, local_time)?;
    let place = |k: &promo_model::ProjectLayerKeyframe| -> V3 {
        let o = k.stage_offset.unwrap_or([0.0, 0.0]);
        [o[0], o[1], k.depth.unwrap_or(0.0)]
    };
    let (pa, pb) = (place(a), place(b));
    if !std::ptr::eq(a, b) && b.easing.unwrap_or(promo_model::Easing::Linear).is_smooth() && b.motion_path.is_none() {
        // A smooth ramp: each coordinate a cubic through its neighbours.
        return Some([
            crate::interpolation::blend(&track, a, b, progress, &|k| place(k)[0]),
            crate::interpolation::blend(&track, a, b, progress, &|k| place(k)[1]),
            crate::interpolation::blend(&track, a, b, progress, &|k| place(k)[2]),
        ]);
    }
    if !std::ptr::eq(a, b) {
        if let Some(path) = b.motion_path.as_ref() {
            if let Some(route) = route_of(resources, path) {
                return Some(point_along3(
                    &route,
                    pa,
                    pb,
                    path.flipped.unwrap_or(false),
                    path.start_at.unwrap_or(0.0),
                    path.end_at.unwrap_or(1.0),
                    progress,
                ));
            }
        }
    }
    Some(lerp(pa, pb, progress))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bend() -> Route3 {
        Route3::new(vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]], false).unwrap()
    }

    fn close(a: V3, b: V3) -> bool {
        length(sub(a, b)) < 1e-6
    }

    /// Progress is distance: halfway along a right-angle bend is its corner.
    #[test]
    fn progress_is_distance() {
        let r = bend();
        assert!((r.total_length() - 20.0).abs() < 1e-9);
        assert!(close(r.point_at(0.5), [10.0, 0.0, 0.0]));
        assert!(close(r.point_at(0.25), [5.0, 0.0, 0.0]));
    }

    /// The fit lands the route's ends on the move's ends whatever the
    /// route's own size, place and direction, and bends the middle off the
    /// straight line — in three dimensions.
    #[test]
    fn a_route_is_fitted_between_the_keyframes() {
        let r = bend();
        let (from, to) = ([1.0, 2.0, 3.0], [1.0, 2.0, 9.0]);
        assert!(close(point_along3(&r, from, to, false, 0.0, 1.0, 0.0), from));
        assert!(close(point_along3(&r, from, to, false, 0.0, 1.0, 1.0), to));
        let mid = point_along3(&r, from, to, false, 0.0, 1.0, 0.5);
        // The corner sits 7.07 off the route's own chord (half the chord's
        // 14.14 length, square to it); the move is 6 long, so the fit
        // scales that to 6 / 14.14 × 7.07 = 3.
        let off_chord = {
            let chord = sub(to, from);
            let u = scale(chord, 1.0 / length(chord));
            let d = sub(mid, from);
            length(sub(d, scale(u, dot(d, u))))
        };
        assert!((off_chord - 3.0).abs() < 1e-3, "off the chord by {off_chord}");
        // Flipped mirrors it across the chord: same distance, other side.
        let flipped = point_along3(&r, from, to, true, 0.0, 1.0, 0.5);
        assert!((length(sub(flipped, from)) - length(sub(mid, from))).abs() < 1e-6);
        assert!(!close(flipped, mid));
    }

    /// An arc drawn bulging upward bulges upward in the stage whichever way
    /// the move runs: the fit's roll about the chord follows world up.
    #[test]
    fn a_route_keeps_its_up_toward_world_up() {
        let arc = Route3::new(vec![[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]], false).unwrap();
        for (from, to) in [
            ([0.0, 0.0, 0.0], [4.0, 0.0, 0.0]),
            ([0.0, 0.0, 0.0], [0.0, 0.0, -4.0]),
            ([3.0, 1.0, 2.0], [-1.0, 1.0, -2.0]),
            ([0.0, 0.0, 0.0], [-4.0, 0.0, 0.0]),
        ] {
            let mid = point_along3(&arc, from, to, false, 0.0, 1.0, 0.5);
            let centre = lerp(from, to, 0.5);
            assert!(mid[1] > centre[1] + 1.0, "bulges upward for {from:?} → {to:?}: {mid:?}");
        }
    }

    /// A closed route plays at its own size from the start when the move
    /// goes nowhere — an orbit.
    #[test]
    fn a_closed_route_orbits_at_its_own_size() {
        let circle: Vec<V3> = (0..24)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / 24.0;
                [a.cos() * 2.0, 0.0, a.sin() * 2.0]
            })
            .collect();
        let r = Route3::new(circle, true).unwrap();
        assert!(r.is_closed());
        let here = [5.0, 1.0, 5.0];
        let quarter = point_along3(&r, here, here, false, 0.0, 1.0, 0.25);
        // A quarter turn from (2, 0, 0) is (0, 0, 2): two units over and two back.
        assert!(close(quarter, [3.0, 1.0, 7.0]), "{quarter:?}");
    }

    /// A smooth route passes through its points and has more samples than
    /// a straight one; a two-point route is a line either way.
    #[test]
    fn a_smooth_route_passes_through_its_points() {
        let route = Route {
            points: vec![[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0], [3.0, 1.0, 1.0]],
            curve: None,
            closed: None,
        };
        let smooth = Route3::from_route(&route).unwrap();
        assert!(smooth.points.len() > 4);
        assert!(close(smooth.start(), [0.0, 0.0, 0.0]));
        assert!(close(smooth.end(), [3.0, 1.0, 1.0]));
        let linear = Route3::from_route(&Route {
            curve: Some("linear".into()),
            ..route.clone()
        })
        .unwrap();
        assert_eq!(linear.points.len(), 4);
        let t = smooth.tangent_at(0.5);
        assert!((length(t) - 1.0).abs() < 1e-6);
    }
}
