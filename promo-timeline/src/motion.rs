//! A drawn stroke used as the SHAPE of a move.
//!
//! The keyframes still decide where a layer starts and ends. A motion path
//! only bends the route between them: the stroke's first point is fitted onto
//! the previous keyframe's position and its last onto the next one's, which
//! absorbs the drawing's own scale, rotation and place on the canvas. So one
//! arc drawn in a corner is a swoop for any pair of keyframes at any distance
//! or angle, and a layer with no path moves in a straight line exactly as
//! every project does today.
//!
//! Two consequences worth knowing, both deliberate:
//!
//! - The bulge scales with the distance, because the fit is a similarity. A
//!   gesture is proportionally as dramatic over a short move as a long one.
//! - When the two keyframes sit at the SAME position the fit has no scale to
//!   derive — there is no direction to aim at. A closed stroke (an oval) then
//!   plays at its own drawn size, which is what an orbit needs.

use promo_model::{MotionPath, PathDocument, Point, ProjectResource, ProjectResourceKind};

/// A stroke resampled as a polyline, with the cumulative length of each point
/// along it — what makes progress mean DISTANCE rather than raw curve
/// parameter. Without it a constant-speed move visibly races down the straight
/// stretches and crawls through the curves.
#[derive(Debug, Clone, PartialEq)]
pub struct Polyline {
    pub points: Vec<Point>,
    /// `lengths[i]` is the distance from `points[0]` to `points[i]`.
    lengths: Vec<f64>,
}

/// How many segments a closed ellipse is sampled into. Fine enough that the
/// arc-length table is smooth at any canvas size, cheap enough to build per
/// render if a cache ever misses.
/// Samples per curve. Uniform in the Bezier parameter; the arc-length table
/// is what turns that into uniform motion.
const CURVE_SAMPLES: usize = 128;
/// Points closer than this are the same point: a pen stroke records many, and
/// zero-length segments would put ties in the arc-length table.
const MIN_SEGMENT: f64 = 1e-6;
/// Below this total length a stroke is a dot, not a route.
const MIN_LENGTH: f64 = 1e-3;
/// An SVG import can hand over thousands of samples for one smooth curve;
/// beyond this the polyline is decimated evenly. Fitting is linear in the
/// point count, so this is about memory and cache churn, not correctness.
const MAX_POINTS: usize = 2048;

impl Polyline {
    /// Builds a polyline, or `None` when the stroke cannot be a route:
    /// fewer than two distinct points, a non-finite coordinate (the SVG arc
    /// approximation is where those come from), or no length at all.
    pub fn new(points: Vec<Point>) -> Option<Self> {
        if points.iter().any(|p| !p.x().is_finite() || !p.y().is_finite()) {
            return None;
        }
        // Collapse repeats so the table is strictly increasing.
        let mut cleaned: Vec<Point> = Vec::with_capacity(points.len());
        for point in points {
            match cleaned.last() {
                Some(previous) if distance(*previous, point) < MIN_SEGMENT => {}
                _ => cleaned.push(point),
            }
        }
        if cleaned.len() < 2 {
            return None;
        }
        if cleaned.len() > MAX_POINTS {
            let stride = (cleaned.len() as f64 / MAX_POINTS as f64).ceil() as usize;
            let last = *cleaned.last().expect("non-empty");
            let mut decimated: Vec<Point> = cleaned.iter().step_by(stride).copied().collect();
            if *decimated.last().expect("non-empty") != last {
                decimated.push(last);
            }
            cleaned = decimated;
        }

        let mut lengths = Vec::with_capacity(cleaned.len());
        let mut total = 0.0;
        lengths.push(0.0);
        for pair in cleaned.windows(2) {
            total += distance(pair[0], pair[1]);
            lengths.push(total);
        }
        if total < MIN_LENGTH {
            return None;
        }
        Some(Polyline {
            points: cleaned,
            lengths,
        })
    }

    pub fn total_length(&self) -> f64 {
        *self.lengths.last().unwrap_or(&0.0)
    }

    pub fn start(&self) -> Point {
        self.points[0]
    }

    pub fn end(&self) -> Point {
        self.points[self.points.len() - 1]
    }

    /// True when the stroke returns to where it began — an orbit rather than
    /// a journey. Measured against its own size, so a big loop and a small
    /// one answer the same.
    pub fn is_closed(&self) -> bool {
        distance(self.start(), self.end()) < self.total_length() * 0.02
    }

    /// The point `progress` of the way along, measured in DISTANCE. Outside
    /// 0…1 it clamps to the ends.
    pub fn point_at(&self, progress: f64) -> Point {
        let total = self.total_length();
        if progress <= 0.0 || total <= 0.0 {
            return self.start();
        }
        if progress >= 1.0 {
            return self.end();
        }
        let target = progress * total;
        // The table is sorted, so binary search rather than a walk: an SVG
        // path can carry a couple of thousand points and this runs per layer
        // per frame.
        let index = match self
            .lengths
            .binary_search_by(|value| value.partial_cmp(&target).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(exact) => return self.points[exact],
            Err(insert) => insert.max(1).min(self.points.len() - 1),
        };
        let (before, after) = (self.points[index - 1], self.points[index]);
        let (start_length, end_length) = (self.lengths[index - 1], self.lengths[index]);
        let span = end_length - start_length;
        let t = if span > 0.0 {
            (target - start_length) / span
        } else {
            0.0
        };
        Point(before.x() + (after.x() - before.x()) * t, before.y() + (after.y() - before.y()) * t)
    }
}

fn distance(a: Point, b: Point) -> f64 {
    ((b.x() - a.x()).powi(2) + (b.y() - a.y()).powi(2)).sqrt()
}

/// The polyline a path document means, sampled from its curve.
///
/// No controls is a straight line; one is quadratic, two cubic. Sampling is
/// uniform in the curve parameter and the arc-length table above then makes
/// motion uniform in DISTANCE — which is the part that has to be right, since
/// a Bezier's parameter races through its flat stretches.
pub fn path_document_polyline(document: &PathDocument) -> Option<Polyline> {
    let (start, end) = (document.start, document.end);
    let points: Vec<Point> = match document.controls.len() {
        0 => vec![start, end],
        1 => {
            let c = document.controls[0];
            (0..=CURVE_SAMPLES)
                .map(|step| {
                    let t = step as f64 / CURVE_SAMPLES as f64;
                    let u = 1.0 - t;
                    Point(
                        u * u * start.x() + 2.0 * u * t * c.x() + t * t * end.x(),
                        u * u * start.y() + 2.0 * u * t * c.y() + t * t * end.y(),
                    )
                })
                .collect()
        }
        _ => {
            let (c1, c2) = (document.controls[0], document.controls[1]);
            (0..=CURVE_SAMPLES)
                .map(|step| {
                    let t = step as f64 / CURVE_SAMPLES as f64;
                    let u = 1.0 - t;
                    Point(
                        u * u * u * start.x()
                            + 3.0 * u * u * t * c1.x()
                            + 3.0 * u * t * t * c2.x()
                            + t * t * t * end.x(),
                        u * u * u * start.y()
                            + 3.0 * u * u * t * c1.y()
                            + 3.0 * u * t * t * c2.y()
                            + t * t * t * end.y(),
                    )
                })
                .collect()
        }
    };
    Polyline::new(points)
}

/// The path a `MotionPath` names, resolved through the project's resources.
/// Only a `path` resource answers: a drawing is content, and content is not a
/// route.
pub fn path_polyline(resources: &[ProjectResource], path: &MotionPath) -> Option<Polyline> {
    let resource = resources
        .iter()
        .find(|r| r.id == path.path_resource_id && r.kind == ProjectResourceKind::Path)?;
    path_document_polyline(resource.path.as_ref()?)
}

/// Where a layer sits at `progress` of the way from `from` to `to`, following
/// the stroke.
///
/// The fit is a similarity — translate, rotate, uniform scale — derived from
/// two points: the stroke's first onto `from`, its last onto `to`. That is
/// what makes the drawing's own size and placement irrelevant.
///
/// When `from` and `to` coincide there is no direction to aim at and no scale
/// to derive, so the stroke plays at the size it was drawn, positioned so its
/// start sits on `from`. That is the orbit case, and it is the one place the
/// drawing's scale does matter.
pub fn point_along(path: &Polyline, from: Point, to: Point, flipped: bool, progress: f64) -> Point {
    point_along_range(path, from, to, flipped, 0.0, 1.0, progress)
}

/// `point_along` over a stretch of the path: `start_at` above `end_at` runs
/// it backwards, which is how reversal comes for free.
#[allow(clippy::too_many_arguments)]
pub fn point_along_range(
    path: &Polyline,
    from: Point,
    to: Point,
    flipped: bool,
    start_at: f64,
    end_at: f64,
    progress: f64,
) -> Point {
    let (start_at, end_at) = (start_at.clamp(0.0, 1.0), end_at.clamp(0.0, 1.0));
    let along = start_at + (end_at - start_at) * progress.clamp(0.0, 1.0);
    let raw = path.point_at(along);
    // The chord of the stretch in use — trimming changes which ends are
    // being aimed at, so the fit has to follow.
    let origin = path.point_at(start_at);
    let finish = path.point_at(end_at);
    let chord = Point(finish.x() - origin.x(), finish.y() - origin.y());
    let chord_length = (chord.x() * chord.x() + chord.y() * chord.y()).sqrt();
    let mut local = Point(raw.x() - origin.x(), raw.y() - origin.y());
    if flipped {
        // Mirror across the stroke's own chord before it is aimed anywhere.
        if chord_length > MIN_SEGMENT {
            let (ux, uy) = (chord.x() / chord_length, chord.y() / chord_length);
            // Reflection of `local` in the line through the origin along u.
            let dot = local.x() * ux + local.y() * uy;
            local = Point(2.0 * dot * ux - local.x(), 2.0 * dot * uy - local.y());
        } else {
            local = Point(local.x(), -local.y());
        }
    }

    let target = Point(to.x() - from.x(), to.y() - from.y());
    let target_length = (target.x() * target.x() + target.y() * target.y()).sqrt();
    if chord_length < MIN_SEGMENT || target_length < MIN_SEGMENT {
        // Nowhere to aim: play the stroke at its drawn size from `from`.
        return Point(from.x() + local.x(), from.y() + local.y());
    }

    // The rotation+scale that carries the stroke's chord onto from→to, as one
    // complex multiplication: (cos, sin) * scale.
    let scale = target_length / chord_length;
    let (cx, cy) = (chord.x() / chord_length, chord.y() / chord_length);
    let (tx, ty) = (target.x() / target_length, target.y() / target_length);
    let (cos, sin) = (cx * tx + cy * ty, cx * ty - cy * tx);
    Point(from.x() + scale * (local.x() * cos - local.y() * sin), from.y() + scale * (local.x() * sin + local.y() * cos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layer_transform, layer_transform_along_paths};
    use promo_model::{CompositionSettings, ProjectLayer};

    fn pt(x: f64, y: f64) -> Point {
        Point(x, y)
    }

    /// A right-angle bend: (0,0) → (10,0) → (10,10). Half the distance along
    /// is the corner, which is what makes it a good arc-length probe.
    fn bend() -> Polyline {
        Polyline::new(vec![pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0)]).unwrap()
    }

    #[test]
    fn progress_is_distance_not_curve_parameter() {
        let line = bend();
        assert!((line.total_length() - 20.0).abs() < 1e-9);
        // Halfway by DISTANCE is the corner. By raw parameter it would be the
        // midpoint of the first segment — the bug this table exists to avoid.
        let mid = line.point_at(0.5);
        assert!((mid.x() - 10.0).abs() < 1e-9 && (mid.y() - 0.0).abs() < 1e-9, "{mid:?}");
        let quarter = line.point_at(0.25);
        assert!((quarter.x() - 5.0).abs() < 1e-9 && quarter.y().abs() < 1e-9, "{quarter:?}");
    }

    #[test]
    fn a_stroke_is_aimed_from_a_to_b_whatever_its_own_size() {
        // Drawn tiny and horizontal; used across a long vertical move.
        let drawn = Polyline::new(vec![pt(0.0, 0.0), pt(1.0, 1.0), pt(2.0, 0.0)]).unwrap();
        let (a, b) = (pt(100.0, 100.0), pt(100.0, 900.0));
        assert_eq!(point_along(&drawn, a, b, false, 0.0), a, "starts at A");
        let end = point_along(&drawn, a, b, false, 1.0);
        assert!((end.x() - b.x()).abs() < 1e-9 && (end.y() - b.y()).abs() < 1e-9, "ends at B: {end:?}");
        // The bulge is perpendicular to A→B and scaled with it: the drawn
        // hump rose 1 against a chord of 2, so half the 800pt move = 400.
        let mid = point_along(&drawn, a, b, false, 0.5);
        assert!((mid.y() - 500.0).abs() < 1e-6, "half way along: {mid:?}");
        assert!((mid.x() - a.x()).abs() > 100.0, "and off the straight line: {mid:?}");
    }

    #[test]
    fn the_same_stroke_scales_with_the_distance() {
        let drawn = Polyline::new(vec![pt(0.0, 0.0), pt(1.0, 1.0), pt(2.0, 0.0)]).unwrap();
        let short = point_along(&drawn, pt(0.0, 0.0), pt(100.0, 0.0), false, 0.5);
        let long = point_along(&drawn, pt(0.0, 0.0), pt(400.0, 0.0), false, 0.5);
        // Four times the move, four times the bulge — a similarity fit.
        assert!((long.y() / short.y() - 4.0).abs() < 1e-6, "{short:?} {long:?}");
    }

    #[test]
    fn flipping_mirrors_the_bulge_and_keeps_the_ends() {
        let drawn = Polyline::new(vec![pt(0.0, 0.0), pt(1.0, 1.0), pt(2.0, 0.0)]).unwrap();
        let (a, b) = (pt(0.0, 0.0), pt(200.0, 0.0));
        let normal = point_along(&drawn, a, b, false, 0.5);
        let flipped = point_along(&drawn, a, b, true, 0.5);
        assert!((normal.y() + flipped.y()).abs() < 1e-6, "{normal:?} {flipped:?}");
        assert!((normal.x() - flipped.x()).abs() < 1e-6, "same distance along");
        assert_eq!(point_along(&drawn, a, b, true, 1.0).x().round(), b.x());
    }

    fn path_resource(controls: serde_json::Value) -> Vec<ProjectResource> {
        serde_json::from_value(serde_json::json!([{
            "id": "P1", "kind": "path", "filename": "",
            "displayName": "Swoop", "addedAt": 0,
            "path": {"start": [0.0, 0.0], "end": [2.0, 0.0], "controls": controls}
        }]))
        .unwrap()
    }

    fn motion(resource: &str) -> MotionPath {
        serde_json::from_value(serde_json::json!({"pathResourceID": resource})).unwrap()
    }

    #[test]
    fn a_path_resource_answers_and_a_drawing_does_not() {
        // Exclusive by design: content is content, a route is a route.
        let paths = path_resource(serde_json::json!([[1.0, -2.0]]));
        assert!(path_polyline(&paths, &motion("P1")).is_some());

        let drawing: Vec<ProjectResource> = serde_json::from_value(serde_json::json!([{
            "id": "D1", "kind": "drawing", "filename": "d.json",
            "displayName": "Art", "addedAt": 0,
            "drawing": {"shapes": [{
                "id": "S1", "kind": "pen", "points": [[0.0, 0.0], [5.0, 5.0]],
                "strokeColorHex": "FFFFFF", "strokeWidth": 2.0,
                "arrowStart": false, "arrowEnd": false
            }]}
        }]))
        .unwrap();
        assert!(path_polyline(&drawing, &motion("D1")).is_none());
    }

    #[test]
    fn no_controls_is_a_straight_line_and_curves_bend() {
        let straight = path_document_polyline(&PathDocument {
            start: pt(0.0, 0.0),
            end: pt(10.0, 0.0),
            controls: vec![],
        })
        .unwrap();
        assert!(straight.point_at(0.5).y().abs() < 1e-9, "no controls, no bend");

        // Quadratic: the curve reaches half of the control's offset.
        let quad = path_document_polyline(&PathDocument {
            start: pt(0.0, 0.0),
            end: pt(10.0, 0.0),
            controls: vec![pt(5.0, -10.0)],
        })
        .unwrap();
        assert!((quad.point_at(0.5).y() + 5.0).abs() < 0.05, "{:?}", quad.point_at(0.5));

        // Cubic takes two, and anything past them is ignored rather than
        // refused — a future multi-segment path must not break today's files.
        let cubic = path_document_polyline(&PathDocument {
            start: pt(0.0, 0.0),
            end: pt(10.0, 0.0),
            controls: vec![pt(0.0, -10.0), pt(10.0, -10.0), pt(99.0, 99.0)],
        })
        .unwrap();
        assert!(cubic.point_at(0.5).y() < -6.0, "{:?}", cubic.point_at(0.5));
    }

    #[test]
    fn trim_handles_reverse_and_shorten_without_extra_flags() {
        let line = Polyline::new(vec![pt(0.0, 0.0), pt(100.0, 0.0)]).unwrap();
        let (a, b) = (pt(0.0, 0.0), pt(100.0, 0.0));
        // Backwards: start above end runs the path the other way. The fit
        // still lands the ends on A and B, so this shows up as the ORDER the
        // path is traversed, not as a different destination.
        let forward = point_along_range(&line, a, b, false, 0.0, 1.0, 0.25);
        let backward = point_along_range(&line, a, b, false, 1.0, 0.0, 0.25);
        assert!((forward.x() - 25.0).abs() < 1e-6, "{forward:?}");
        assert!((backward.x() - 25.0).abs() < 1e-6, "ends still fitted: {backward:?}");

        // A half of a closed path is an arc rather than a degenerate loop:
        // used 0→0.5, its two ends are far apart and the fit has a direction.
        let circle: Vec<Point> = (0..=64)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / 64.0;
                pt(50.0 * t.cos(), 50.0 * t.sin())
            })
            .collect();
        let loop_line = Polyline::new(circle).unwrap();
        assert!(loop_line.is_closed());
        let half = point_along_range(&loop_line, a, b, false, 0.0, 0.5, 0.5);
        assert!(half.y().abs() > 10.0, "the arc bulges off the chord: {half:?}");
    }

    /// Everything above tests the geometry in isolation. This is the contract
    /// the feature actually rests on: a keyframe carrying a `motionPath`
    /// bends the route between two positions, and NOTHING else changes —
    /// the layer still departs from A, still arrives at B, still on the same
    /// timing. Without this the maths can be perfect while the wiring points
    /// at the wrong keyframe pair and only the app would show it.
    #[test]
    fn a_keyframe_with_a_path_bends_the_route_and_moves_neither_end() {
        let layer: ProjectLayer = serde_json::from_str(
            r#"{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                "isEnabled": true, "startTime": 0, "keyframes": [
                  {"id": "A", "time": 0, "zoom": 1,
                   "horizontalShift": 0, "verticalShift": 0,
                   "transitionDuration": 0},
                  {"id": "B", "time": 2, "zoom": 1,
                   "horizontalShift": 200, "verticalShift": 0,
                   "transitionDuration": 2,
                   "motionPath": {"pathResourceID": "P1"}}]}"#,
        )
        .expect("layer");
        // Drawn small and pointing elsewhere on purpose: the fit is what
        // places it, so its own coordinates must not reach the answer.
        let paths = path_resource(serde_json::json!([[1.0, -4.0]]));
        let defaults = CompositionSettings::default();

        let at = |t: f64| layer_transform_along_paths(&layer, t, &defaults, &paths);
        let straight = |t: f64| layer_transform(&layer, t, &defaults);

        // Both ends are the keyframes' own positions, path or no path.
        for edge in [0.0, 2.0] {
            let (curved, plain) = (at(edge), straight(edge));
            assert!((curved.horizontal_shift - plain.horizontal_shift).abs() < 1e-6, "{edge}");
            assert!((curved.vertical_shift - plain.vertical_shift).abs() < 1e-6, "{edge}");
        }

        // In between it leaves the straight line — the whole point.
        let mid = at(1.0);
        assert!(
            mid.vertical_shift.abs() > 10.0,
            "the route should bow off the chord: {mid:?}"
        );
        assert!(
            straight(1.0).vertical_shift.abs() < 1e-9,
            "and the same layer without the resources is still a straight line"
        );

        // A path names a route, not a schedule: zoom and timing are untouched.
        assert!((mid.zoom - straight(1.0).zoom).abs() < 1e-9);
    }

    /// A `motionPath` naming a resource the project does not have must fall
    /// back to the straight line. Anything else means a deleted path resource
    /// takes the layer's movement down with it.
    #[test]
    fn a_path_that_is_not_there_is_simply_a_straight_line() {
        let layer: ProjectLayer = serde_json::from_str(
            r#"{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                "isEnabled": true, "startTime": 0, "keyframes": [
                  {"id": "A", "time": 0, "zoom": 1,
                   "horizontalShift": 0, "verticalShift": 0,
                   "transitionDuration": 0},
                  {"id": "B", "time": 2, "zoom": 1,
                   "horizontalShift": 200, "verticalShift": 0,
                   "transitionDuration": 2,
                   "motionPath": {"pathResourceID": "GONE"}}]}"#,
        )
        .expect("layer");
        let defaults = CompositionSettings::default();
        let missing = layer_transform_along_paths(
            &layer, 1.0, &defaults, &path_resource(serde_json::json!([[1.0, -4.0]])));
        let plain = layer_transform(&layer, 1.0, &defaults);
        assert!((missing.horizontal_shift - plain.horizontal_shift).abs() < 1e-9);
        assert!(missing.vertical_shift.abs() < 1e-9, "{missing:?}");
    }

    /// A closed stroke has no chord, so there is nothing to aim at the
    /// keyframes: it plays at its DRAWN size around the first position and
    /// never reaches the second. That is what an orbit needs and what the
    /// docs promise — worth pinning, because it is the one case where the
    /// path does not land the layer on the keyframe it is attached to.
    #[test]
    fn a_closed_path_orbits_the_first_position_rather_than_reaching_the_second() {
        let circle: Vec<Point> = (0..=64)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / 64.0;
                pt(10.0 * t.cos(), 10.0 * t.sin())
            })
            .collect();
        let closed = Polyline::new(circle).unwrap();
        let (a, b) = (pt(0.0, 0.0), pt(500.0, 0.0));

        let end = point_along(&closed, a, b, false, 1.0);
        assert!((end.x() - a.x()).abs() < 1e-6 && (end.y() - a.y()).abs() < 1e-6,
                "back where it started, not at B: {end:?}");
        let quarter = point_along(&closed, a, b, false, 0.25);
        assert!(quarter.y().abs() > 5.0, "and it has been away in between: {quarter:?}");
        // Drawn size, not scaled to the 500pt gap it was given.
        assert!(quarter.x().abs() <= 20.0 && quarter.y().abs() <= 20.0, "{quarter:?}");
    }

    #[test]
    fn an_absurd_point_count_is_decimated_not_refused() {
        let points: Vec<Point> = (0..10_000)
            .map(|i| pt(i as f64 * 0.5, (i as f64 * 0.01).sin() * 20.0))
            .collect();
        let line = Polyline::new(points).unwrap();
        assert!(line.points.len() <= MAX_POINTS, "{}", line.points.len());
        // Still spans the same route.
        assert!((line.start().x() - 0.0).abs() < 1e-9);
        assert!((line.end().x() - 4999.5).abs() < 1e-9);
    }

    /// Flip and reverse are different mirrors and must compose. The fit
    /// always carries the walked stretch onto from->to, so NO combination
    /// moves the ends; what changes is the route. Flip mirrors it across the
    /// chord. Reverse walks the stroke end-for-end and the re-aim lands the
    /// bulge on the other side too - so on a stroke SYMMETRIC about its
    /// chord midpoint the two are the same mirror, and applying both cancels
    /// back to the plain route. On an asymmetric stroke they differ, because
    /// reverse also mirrors the route in time.
    #[test]
    fn flip_and_reverse_compose_without_interfering() {
        let (a, b) = (pt(0.0, 0.0), pt(200.0, 0.0));
        let near = |x: Point, y: Point| (x.x() - y.x()).abs() < 1e-9 && (x.y() - y.y()).abs() < 1e-9;

        let symmetric = Polyline::new(vec![pt(0.0, 0.0), pt(1.0, 1.0), pt(2.0, 0.0)]).unwrap();
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let flipped = point_along_range(&symmetric, a, b, true, 0.0, 1.0, p);
            let reversed = point_along_range(&symmetric, a, b, false, 1.0, 0.0, p);
            let both = point_along_range(&symmetric, a, b, true, 1.0, 0.0, p);
            let plain = point_along_range(&symmetric, a, b, false, 0.0, 1.0, p);
            assert!(near(flipped, reversed), "one mirror at {p}: {flipped:?} {reversed:?}");
            assert!(near(both, plain), "two mirrors cancel at {p}: {both:?} {plain:?}");
        }
        // Ends are keyframe positions, whatever the switches say.
        for (flip, range) in [(false, (1.0, 0.0)), (true, (0.0, 1.0)), (true, (1.0, 0.0))] {
            assert!(near(point_along_range(&symmetric, a, b, flip, range.0, range.1, 0.0), a));
            assert!(near(point_along_range(&symmetric, a, b, flip, range.0, range.1, 1.0), b));
        }

        // An early bulge tells flip and reverse apart: flip keeps it early,
        // reverse plays it late.
        let lopsided = Polyline::new(vec![pt(0.0, 0.0), pt(0.5, 1.0), pt(2.0, 0.0)]).unwrap();
        let flipped = point_along_range(&lopsided, a, b, true, 0.0, 1.0, 0.25);
        let reversed = point_along_range(&lopsided, a, b, false, 1.0, 0.0, 0.25);
        assert!(
            (flipped.x() - reversed.x()).abs() > 1.0,
            "different mirrors: {flipped:?} {reversed:?}"
        );
    }
}
