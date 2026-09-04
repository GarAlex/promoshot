//! Particles in a STAGE (rung 39): a morph — `count` points sampled on
//! one body's surface fly apart and gather on another's, driven by the
//! member's `progress` (0 the first body, 1 the second). Every point is
//! a closed-form function of the progress and the recipe's seed, so any
//! frame renders alone, scrubs, caches, and matches on every host —
//! the same rule the flat particles keep.
use crate::model::{Mat4, Model};

/// A point on a body's surface at its rest pose, with the surface's
/// outward direction there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

/// One particle of a cloud for one instant: where it is, how big, and
/// which of the recipe's colours it wears.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub position: [f32; 3],
    pub size: f32,
    pub color: usize,
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit(state: &mut u64) -> f32 {
    ((splitmix(state) >> 11) as f64 / (1u64 << 53) as f64) as f32
}

/// Column-major, as the model's matrices are: a point when `w` is 1, a
/// direction when it is 0.
fn apply(m: &Mat4, p: [f32; 3], w: f32) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0] * w,
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1] * w,
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2] * w,
    ]
}

/// A direction through a placement: the inverse transpose of its upper
/// 3x3, so an unevenly scaled node still reports the surface's true
/// outward direction. Falls back to the matrix when it is singular.
fn normal_direction(m: &Mat4, n: [f32; 3]) -> [f32; 3] {
    let a = [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        return norm(apply(m, n, 0.0));
    }
    let inv = |r: usize, c: usize| -> f32 {
        let (r0, r1) = ((c + 1) % 3, (c + 2) % 3);
        let (c0, c1) = ((r + 1) % 3, (r + 2) % 3);
        (a[r0][c0] * a[r1][c1] - a[r0][c1] * a[r1][c0]) / det
    };
    // inverse transpose: (A^-1)^T
    norm([
        inv(0, 0) * n[0] + inv(0, 1) * n[1] + inv(0, 2) * n[2],
        inv(1, 0) * n[0] + inv(1, 1) * n[1] + inv(1, 2) * n[2],
        inv(2, 0) * n[0] + inv(2, 1) * n[1] + inv(2, 2) * n[2],
    ])
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-9 {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

/// `count` points over the model's surface at its rest pose, weighted by
/// area so a big face gets its share, in a fixed order — by height, then
/// round the axis — so the samples of two bodies pair up neighbour to
/// neighbour and a cloud gathers coherently instead of crossing itself.
pub fn sample_surface(model: &Model, rest: &[Mat4], count: usize, seed: u64) -> Vec<Sample> {
    let identity: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    // Every triangle in the rest pose, with its normal and area.
    struct Tri {
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
        normal: [f32; 3],
        area: f32,
    }
    let mut tris: Vec<Tri> = Vec::new();
    for mesh in &model.meshes {
        let m = rest.get(mesh.node).unwrap_or(&identity);
        for t in mesh.indices.chunks_exact(3) {
            let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let (Some(pa), Some(pb), Some(pc)) = (
                mesh.positions.get(ia),
                mesh.positions.get(ib),
                mesh.positions.get(ic),
            ) else {
                continue;
            };
            let a = apply(m, *pa, 1.0);
            let b = apply(m, *pb, 1.0);
            let c = apply(m, *pc, 1.0);
            let n = cross(sub(b, a), sub(c, a));
            let area = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() * 0.5;
            if area.is_nan() || area <= 1e-12 {
                continue;
            }
            // The file's own normal where it has one, the face's otherwise.
            // A normal goes through the inverse TRANSPOSE of the placement,
            // or a node scaled unevenly points its samples the wrong way.
            let vn = match (mesh.normals.get(ia), mesh.normals.get(ib), mesh.normals.get(ic)) {
                (Some(na), Some(nb), Some(nc)) => normal_direction(
                    m,
                    [
                        na[0] + nb[0] + nc[0],
                        na[1] + nb[1] + nc[1],
                        na[2] + nb[2] + nc[2],
                    ],
                ),
                _ => norm(n),
            };
            tris.push(Tri {
                a,
                b,
                c,
                normal: vn,
                area,
            });
        }
    }
    if tris.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut cumulative = Vec::with_capacity(tris.len());
    let mut total = 0.0f32;
    for t in &tris {
        total += t.area;
        cumulative.push(total);
    }
    let mut state = seed ^ 0xA5A5_5A5A_C3C3_3C3C;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let r = unit(&mut state) * total;
        let i = cumulative.partition_point(|c| *c < r).min(tris.len() - 1);
        let (a, b, c, n) = (tris[i].a, tris[i].b, tris[i].c, tris[i].normal);
        // Uniform over the triangle.
        let (u, v) = (unit(&mut state), unit(&mut state));
        let su = u.sqrt();
        let (wa, wb, wc) = (1.0 - su, su * (1.0 - v), su * v);
        out.push(Sample {
            position: [
                a[0] * wa + b[0] * wb + c[0] * wc,
                a[1] * wa + b[1] * wb + c[1] * wc,
                a[2] * wa + b[2] * wb + c[2] * wc,
            ],
            normal: n,
        });
    }
    // Height bands, then round the axis within a band: a fixed, spatial
    // order both bodies share.
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for s in &out {
        lo = lo.min(s.position[1]);
        hi = hi.max(s.position[1]);
    }
    let span = (hi - lo).max(1e-6);
    let bands = 24.0;
    out.sort_by(|p, q| {
        let bp = ((p.position[1] - lo) / span * bands).floor() as i32;
        let bq = ((q.position[1] - lo) / span * bands).floor() as i32;
        let ap = p.position[2].atan2(p.position[0]);
        let aq = q.position[2].atan2(q.position[0]);
        bp.cmp(&bq).then(ap.partial_cmp(&aq).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

/// Every particle at `progress`: on the first body at 0, on the second
/// at 1, and between them flung out along the first body's surface and
/// back in along a curve, each at its own pace (`stagger` spreads the
/// paces; nothing waits and nothing sits), wobbling on the way
/// (`turbulence`), swelling mid-flight.
/// `spread` and `size` are in the same units as the samples.
#[allow(clippy::too_many_arguments)]
pub fn morph_points(
    from: &[Sample],
    to: &[Sample],
    progress: f64,
    spread: f32,
    size: f32,
    turbulence: f32,
    stagger: f32,
    seed: u64,
    colors: usize,
) -> Vec<Point> {
    use std::f32::consts::{PI, TAU};
    let n = from.len().min(to.len());
    let mut out = Vec::with_capacity(n);
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5DEE_CE66_D1CE_4E5B;
    let stagger = stagger.clamp(0.0, 0.95);
    let progress = progress.clamp(0.0, 1.0) as f32;
    // The points grow out of the first surface and shrink into the second,
    // so at 0 and at 1 there is nothing to see but the bodies, and the
    // member can live before and after the flight without a speckle.
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    let presence = smooth(progress / 0.06) * (1.0 - smooth((progress - 0.94) / 0.06));
    for i in 0..n {
        let delay = unit(&mut state);
        let fling = 0.6 + 0.8 * unit(&mut state);
        let jitter = [
            unit(&mut state) * 2.0 - 1.0,
            unit(&mut state) * 2.0 - 1.0,
            unit(&mut state) * 2.0 - 1.0,
        ];
        let phase = unit(&mut state) * TAU;
        let color = (splitmix(&mut state) % colors.max(1) as u64) as usize;
        let (a, b) = (from[i], to[i]);
        // This particle's own PACE: every point leaves at 0 and lands at 1,
        // but each flies on its own power of the progress — leaders ahead,
        // laggards behind, none ever waiting on a surface or sitting on
        // one early. `stagger` is how far the paces spread.
        let pace = ((delay - 0.5) * 2.0 * stagger).exp();
        // The keyframes' own easing shapes the flight; no ease per point,
        // which would park the laggards at the start.
        let e = progress.powf(pace);
        // The way out: halfway between where it was and where it goes,
        // flung along the first body's surface direction.
        let mid = [
            (a.position[0] + b.position[0]) * 0.5 + (a.normal[0] * fling + jitter[0] * 0.5) * spread,
            (a.position[1] + b.position[1]) * 0.5 + (a.normal[1] * fling + jitter[1] * 0.5) * spread,
            (a.position[2] + b.position[2]) * 0.5 + (a.normal[2] * fling + jitter[2] * 0.5) * spread,
        ];
        let (w0, w1, w2) = ((1.0 - e) * (1.0 - e), 2.0 * (1.0 - e) * e, e * e);
        let wobble = (PI * e).sin() * turbulence * spread;
        let position = [
            a.position[0] * w0 + mid[0] * w1 + b.position[0] * w2 + (phase + e * 6.0).sin() * wobble,
            a.position[1] * w0 + mid[1] * w1 + b.position[1] * w2 + (phase * 1.3 + e * 5.0).cos() * wobble,
            a.position[2] * w0 + mid[2] * w1 + b.position[2] * w2 + (phase * 0.7 + e * 7.0).sin() * wobble,
        ];
        out.push(Point {
            position,
            size: size * (1.0 + 0.6 * (PI * e).sin()) * presence,
            color,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    fn cube() -> Model {
        Model::from_glb(&crate::model::sample_cube_glb_with(0.5, "Body", [0.7, 0.7, 0.7, 1.0]))
            .expect("cube")
    }

    /// Samples lie on the body: every point of a unit cube sits on one of
    /// its faces, and the same seed gives the same points twice.
    #[test]
    fn samples_sit_on_the_surface_and_repeat() {
        let model = cube();
        let rest = model.rest_matrices();
        let a = sample_surface(&model, &rest, 500, 7);
        let b = sample_surface(&model, &rest, 500, 7);
        assert_eq!(a.len(), 500);
        assert_eq!(a, b, "the same seed samples the same points");
        for s in &a {
            let on_face = (0..3).any(|k| (s.position[k].abs() - 0.5).abs() < 1e-4);
            assert!(on_face, "{:?} is not on a face of the half-unit cube", s.position);
            let inside = (0..3).all(|k| s.position[k].abs() <= 0.5 + 1e-4);
            assert!(inside, "{:?} lies outside the cube", s.position);
        }
        let c = sample_surface(&model, &rest, 500, 8);
        assert_ne!(a, c, "another seed samples other points");
    }

    /// A morph starts on the first body, ends on the second, and is
    /// somewhere else — further out — in between.
    #[test]
    fn a_morph_leaves_one_body_and_arrives_on_the_other() {
        let model = cube();
        let rest = model.rest_matrices();
        let from = sample_surface(&model, &rest, 300, 1);
        // The second body: the same cube moved two units to the right.
        let to: Vec<Sample> = from
            .iter()
            .map(|s| Sample {
                position: [s.position[0] + 2.0, s.position[1], s.position[2]],
                normal: s.normal,
            })
            .collect();
        let at = |p: f64| morph_points(&from, &to, p, 1.0, 0.02, 0.2, 0.3, 3, 2);
        let start = at(0.0);
        let end = at(1.0);
        let mid = at(0.5);
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-4);
        for (i, p) in start.iter().enumerate() {
            assert!(close(p.position, from[i].position), "at 0 every point is on the first body: {:?} vs {:?}", p.position, from[i].position);
        }
        for (i, p) in end.iter().enumerate() {
            assert!(close(p.position, to[i].position), "at 1 every point is on the second body: {:?} vs {:?}", p.position, to[i].position);
        }
        let radius = |pts: &[Point]| -> f32 {
            let c = pts.iter().fold([0.0f32; 3], |c, p| {
                [c[0] + p.position[0], c[1] + p.position[1], c[2] + p.position[2]]
            });
            let n = pts.len() as f32;
            let c = [c[0] / n, c[1] / n, c[2] / n];
            pts.iter()
                .map(|p| {
                    let d = sub(p.position, c);
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                })
                .sum::<f32>()
                / n
        };
        assert!(
            radius(&mid) > radius(&start) * 1.3,
            "mid-flight the cloud is wider: {} vs {}",
            radius(&mid),
            radius(&start)
        );
        assert!(mid.iter().all(|p| p.size > 0.02), "points swell on the way");
        assert!(start.iter().all(|p| p.size == 0.0) && end.iter().all(|p| p.size == 0.0),
                "at rest on a body there is nothing to see");
        assert!(at(0.03).iter().all(|p| p.size > 0.0), "the points grow out of the first surface");
        assert_eq!(at(0.5), mid, "the same instant is the same cloud");
    }

    /// No particle stands still mid-flight: between the presence ramps
    /// every point moves between one instant and the next, whatever its
    /// pace — a stagger spreads paces, it never parks a point.
    #[test]
    fn no_particle_stands_still_mid_flight() {
        let model = cube();
        let rest = model.rest_matrices();
        let from = sample_surface(&model, &rest, 400, 5);
        let to: Vec<Sample> = from
            .iter()
            .map(|s| Sample {
                position: [s.position[0] + 3.0, s.position[1] * 0.5, s.position[2]],
                normal: s.normal,
            })
            .collect();
        let at = |p: f64| morph_points(&from, &to, p, 1.2, 0.02, 0.2, 0.9, 11, 3);
        let mut slowest = f32::MAX;
        let mut p = 0.08;
        while p < 0.92 {
            let (now, next) = (at(p), at(p + 0.01));
            for (a, b) in now.iter().zip(&next) {
                let d = sub(b.position, a.position);
                slowest = slowest.min((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt());
            }
            p += 0.07;
        }
        assert!(slowest > 1e-4, "every point keeps moving; the slowest step was {slowest}");
    }
}
