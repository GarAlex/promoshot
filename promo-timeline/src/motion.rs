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

use promo_model::{DrawingShape, DrawingShapeKind, MotionPath, Point, ProjectResource};

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
const OVAL_SEGMENTS: usize = 96;
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

/// The polyline a drawn shape means as a route, or `None` when it cannot be
/// one. Paint — colour, width, fill, arrowheads — is ignored rather than
/// rejected: none of it has any bearing on a trajectory.
pub fn shape_polyline(shape: &DrawingShape) -> Option<Polyline> {
    match shape.kind {
        // A pen stroke is already a polyline; a line is its two ends.
        DrawingShapeKind::Pen | DrawingShapeKind::Line => Polyline::new(shape.points.clone()),
        // An oval is stored as the two corners of its bounding box. Sampled
        // into a closed loop, it is an orbit.
        DrawingShapeKind::Oval => {
            let (a, b) = (*shape.points.first()?, *shape.points.get(1)?);
            let (cx, cy) = ((a.x() + b.x()) / 2.0, (a.y() + b.y()) / 2.0);
            let (rx, ry) = ((b.x() - a.x()).abs() / 2.0, (b.y() - a.y()).abs() / 2.0);
            let points = (0..=OVAL_SEGMENTS)
                .map(|step| {
                    let angle =
                        std::f64::consts::TAU * (step as f64) / (OVAL_SEGMENTS as f64);
                    Point(cx + rx * angle.cos(), cy + ry * angle.sin())
                })
                .collect();
            Polyline::new(points)
        }
    }
}

/// The stroke a `MotionPath` names, resolved through the project's resources.
pub fn path_polyline(resources: &[ProjectResource], path: &MotionPath) -> Option<Polyline> {
    let resource = resources
        .iter()
        .find(|r| r.id == path.path_resource_id)?;
    let shape = resource
        .drawing
        .as_ref()?
        .shapes
        .iter()
        .find(|s| s.id == path.path_shape_id)?;
    shape_polyline(shape)
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
    let raw = path.point_at(progress.clamp(0.0, 1.0));
    let origin = path.start();
    // Local coordinates, with the stroke's own chord as the x axis.
    let chord = Point(path.end().x() - origin.x(), path.end().y() - origin.y());
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
    use crate::layer_transform_along_paths;

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

    #[test]
    fn a_stroke_between_two_identical_points_keeps_its_drawn_size() {
        // The orbit case: no direction to aim at, so the drawing's own scale
        // is all there is to go on.
        let oval = shape_polyline(&oval_shape(0.0, 0.0, 100.0, 60.0)).unwrap();
        assert!(oval.is_closed());
        let a = pt(500.0, 500.0);
        let quarter = point_along(&oval, a, a, false, 0.25);
        // A quarter around an ellipse 100 wide and 60 tall, from its right
        // edge: down to the bottom, which is 30 below the centre.
        assert!((quarter.y() - (a.y() + 30.0)).abs() < 1.0, "{quarter:?}");
    }

    #[test]
    fn a_keyframe_with_a_path_bends_the_layer_route() {
        // The whole feature end to end: two keyframes 0→800 across, and a
        // drawn hump that carries the layer above the straight line without
        // touching where it starts or ends.
        use promo_model::{CompositionSettings, ProjectLayer};
        let resources: Vec<ProjectResource> = serde_json::from_value(serde_json::json!([{
            "id": "D1", "kind": "drawing", "filename": "path.json",
            "displayName": "Swoop", "addedAt": 0,
            "drawing": {"shapes": [{
                "id": "S1", "kind": "pen",
                "points": [[0.0, 0.0], [1.0, -1.0], [2.0, 0.0]],
                "strokeColorHex": "FFFFFF", "strokeWidth": 2.0,
                "arrowStart": false, "arrowEnd": false
            }]}
        }]))
        .unwrap();
        let mut layer: ProjectLayer = serde_json::from_value(serde_json::json!({
            "id": "L", "name": "Mover", "sortIndex": 1, "kind": "image",
            "isEnabled": true, "startTime": 0.0, "duration": 4.0,
            "keyframes": [
                {"id": "K1", "time": 0.0, "horizontalShift": 0.0,
                 "verticalShift": 0.0, "transitionDuration": 0.0},
                // transitionDuration stays required by the wire format (it
                // is non-optional in Swift too); the percentage overrides it.
                {"id": "K2", "time": 4.0, "horizontalShift": 800.0,
                 "verticalShift": 0.0, "transitionDuration": 0.0,
                 "transitionPercent": 100.0}
            ]
        }))
        .unwrap();
        let defaults = CompositionSettings::default();

        // Without a path: a straight line, so the midpoint sits on it.
        let straight = layer_transform_along_paths(&layer, 2.0, &defaults, &resources);
        assert!(straight.vertical_shift.abs() < 1e-9, "{straight:?}");
        assert!((straight.horizontal_shift - 400.0).abs() < 1e-9);

        // With it: same ends, lifted in between.
        layer.keyframes[1].motion_path = Some(promo_model::MotionPath {
            path_resource_id: "D1".into(),
            path_shape_id: "S1".into(),
            flipped: None,
        });
        let start = layer_transform_along_paths(&layer, 0.0, &defaults, &resources);
        let mid = layer_transform_along_paths(&layer, 2.0, &defaults, &resources);
        let end = layer_transform_along_paths(&layer, 4.0, &defaults, &resources);
        assert!(start.horizontal_shift.abs() < 1e-9 && start.vertical_shift.abs() < 1e-9);
        assert!((end.horizontal_shift - 800.0).abs() < 1e-9 && end.vertical_shift.abs() < 1e-9,
                "the path never moves the endpoints: {end:?}");
        assert!((mid.vertical_shift + 400.0).abs() < 1e-6,
                "half way it rides the hump: {mid:?}");

        // A path naming a stroke that is not there falls back to straight.
        layer.keyframes[1].motion_path = Some(promo_model::MotionPath {
            path_resource_id: "D1".into(),
            path_shape_id: "GONE".into(),
            flipped: None,
        });
        let fallback = layer_transform_along_paths(&layer, 2.0, &defaults, &resources);
        assert!(fallback.vertical_shift.abs() < 1e-9, "{fallback:?}");
    }

    #[test]
    fn transition_percent_is_the_ramp_share_of_the_gap() {
        use promo_model::ProjectLayerKeyframe;
        let mut k: ProjectLayerKeyframe = serde_json::from_value(serde_json::json!({
            "id": "K", "time": 4.0, "transitionDuration": 0.0
        }))
        .unwrap();
        // 100% starts moving immediately: the ramp fills the whole gap.
        k.transition_percent = Some(100.0);
        assert!((crate::ramp_seconds(&k, 4.0) - 4.0).abs() < 1e-9);
        // 0% holds still and is simply there when the keyframe lands.
        k.transition_percent = Some(0.0);
        assert!(crate::ramp_seconds(&k, 4.0).abs() < 1e-9);
        // 25% moves through the last quarter.
        k.transition_percent = Some(25.0);
        assert!((crate::ramp_seconds(&k, 4.0) - 1.0).abs() < 1e-9);
        // Out of range clamps rather than running past the keyframe.
        k.transition_percent = Some(400.0);
        assert!((crate::ramp_seconds(&k, 4.0) - 4.0).abs() < 1e-9);
        // Without a percentage the seconds still rule, clamped to the gap.
        k.transition_percent = None;
        k.transition_duration = 9.0;
        assert!((crate::ramp_seconds(&k, 4.0) - 4.0).abs() < 1e-9);
    }

    fn oval_shape(x0: f64, y0: f64, x1: f64, y1: f64) -> DrawingShape {
        serde_json::from_value(serde_json::json!({
            "id": "S1", "kind": "oval",
            "points": [[x0, y0], [x1, y1]],
            "strokeColorHex": "FFFFFF", "strokeWidth": 2.0,
            "arrowStart": false, "arrowEnd": false
        }))
        .unwrap()
    }

    #[test]
    fn strokes_that_cannot_be_a_route_are_refused() {
        assert!(Polyline::new(vec![pt(5.0, 5.0)]).is_none(), "one point");
        assert!(
            Polyline::new(vec![pt(5.0, 5.0), pt(5.0, 5.0), pt(5.0, 5.0)]).is_none(),
            "a dot recorded many times"
        );
        assert!(
            Polyline::new(vec![pt(0.0, 0.0), pt(f64::NAN, 1.0)]).is_none(),
            "a non-finite coordinate — where an SVG arc approximation ends up"
        );
        // Paint is ignored, not rejected: a filled, arrow-headed stroke is
        // still a perfectly good route.
        let mut painted = oval_shape(0.0, 0.0, 10.0, 10.0);
        painted.fill_color_hex = Some("FF0000".into());
        painted.arrow_end = true;
        assert!(shape_polyline(&painted).is_some());
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
}
