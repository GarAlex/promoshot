//! Particles (rung 36): many small things over time from one rule, every
//! particle a closed-form function of its birth time and the recipe's
//! seed — position from launch, gravity, wind and analytic drag; a
//! turbulence of two sines; size and opacity over its life — so any
//! frame is computed alone, which is what scrubbing, caching and parity
//! across hosts need. Output is vector shapes in canvas pixels for the
//! drawing rasterizer, behind an invisible frame across the canvas so
//! the drawing's bounds are the canvas.

use crate::vector::rgba_from_hex;
use promo_gpu::vector::{VectorShape, VectorShapeKind};
use promo_model::{CompositionSettings, ParticleRecipe};

/// Live particles the engine will draw at most for one frame.
const BUDGET: usize = 4000;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit(state: &mut u64) -> f64 {
    (splitmix(state) >> 11) as f64 / (1u64 << 53) as f64
}

fn lerp(range: [f64; 2], u: f64) -> f64 {
    range[0] + (range[1] - range[0]) * u
}

/// Every particle alive at `time` (seconds since the layer's start), as
/// shapes in canvas pixels, the first an invisible frame across the
/// canvas.
pub fn particle_shapes(
    recipe: &ParticleRecipe,
    time: f64,
    canvas_w: f64,
    canvas_h: f64,
    settings: &CompositionSettings,
) -> Vec<VectorShape> {
    let h = canvas_h.max(1.0);
    let frame = VectorShape {
        kind: VectorShapeKind::Pen,
        points: vec![(0.0, 0.0), (canvas_w, canvas_h)],
        stroke_rgba: [0.0, 0.0, 0.0, 0.0],
        stroke_width: 0.0,
        fill_rgba: None,
        arrow_start: false,
        arrow_end: false,
        even_odd_fill: false,
        corner_radius: 0.0,
    };
    let mut out = vec![frame];
    if time < 0.0 {
        return out;
    }
    let colors: Vec<[f32; 4]> = recipe
        .colors()
        .iter()
        .filter_map(|hex| rgba_from_hex(settings.resolve_color(hex)))
        .collect();
    let colors = if colors.is_empty() {
        vec![[1.0, 1.0, 1.0, 1.0]]
    } else {
        colors
    };
    let life = recipe.life();
    let max_life = life[0].max(life[1]).max(0.01);
    let rate = recipe.rate().max(0.0);
    let emit_until = recipe.emit_for().unwrap_or(f64::INFINITY);
    let seed = recipe.seed();
    let (anchor, extent) = (recipe.anchor(), recipe.extent());
    let (speed, size, spin) = (recipe.speed(), recipe.size(), recipe.spin());
    let (direction, spread) = (recipe.direction(), recipe.spread());
    let (gravity, wind, drag, turbulence) = (
        recipe.gravity(),
        recipe.wind(),
        recipe.drag(),
        recipe.turbulence(),
    );
    let (size_curve, opacity_curve, shape) = (
        recipe.size_over_life(),
        recipe.opacity_over_life(),
        recipe.shape(),
    );
    let mut budget = BUDGET;

    let emit = |index: u64, born: f64, out: &mut Vec<VectorShape>, budget: &mut usize| {
        let age = time - born;
        if age < 0.0 || *budget == 0 {
            return;
        }
        let mut state = seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        let _ = splitmix(&mut state);
        let life = lerp(life, unit(&mut state)).max(0.01);
        if age >= life {
            return;
        }
        let ox = (anchor[0] + extent[0] * (unit(&mut state) - 0.5)) * canvas_w;
        let oy = (anchor[1] + extent[1] * (unit(&mut state) - 0.5)) * h;
        let angle = (direction + spread * (2.0 * unit(&mut state) - 1.0)).to_radians();
        let launch = lerp(speed, unit(&mut state)) * h;
        let v0 = (angle.cos() * launch, -angle.sin() * launch);
        let g = (wind * h, gravity * h);
        let (px, py, vx, vy) = if drag > 1e-6 {
            let e = (-drag * age).exp();
            let f = (1.0 - e) / drag;
            (
                ox + v0.0 * f + g.0 * (age - f) / drag,
                oy + v0.1 * f + g.1 * (age - f) / drag,
                v0.0 * e + g.0 * f,
                v0.1 * e + g.1 * f,
            )
        } else {
            (
                ox + v0.0 * age + 0.5 * g.0 * age * age,
                oy + v0.1 * age + 0.5 * g.1 * age * age,
                v0.0 + g.0 * age,
                v0.1 + g.1 * age,
            )
        };
        let phases = [
            unit(&mut state) * std::f64::consts::TAU,
            unit(&mut state) * std::f64::consts::TAU,
            unit(&mut state) * std::f64::consts::TAU,
            unit(&mut state) * std::f64::consts::TAU,
        ];
        let (px, py) = if turbulence > 0.0 {
            let t = turbulence * h;
            (
                px + t
                    * ((age * 2.1 + phases[0]).sin() * 0.6 + (age * 5.3 + phases[1]).sin() * 0.4),
                py + t
                    * ((age * 1.7 + phases[2]).sin() * 0.6 + (age * 4.3 + phases[3]).sin() * 0.4),
            )
        } else {
            (px, py)
        };
        let frac = age / life;
        let grown = match size_curve {
            "shrink" => 1.0 - frac,
            "grow" => 0.3 + 0.7 * frac,
            _ => 1.0,
        };
        let s = (lerp(size, unit(&mut state)) * h * grown).max(0.0);
        let alpha = match opacity_curve {
            "fade" => (1.0 - frac) * (age / 0.08).min(1.0),
            _ => 1.0,
        } as f32;
        let pick = (unit(&mut state) * colors.len() as f64) as usize % colors.len();
        let mut rgba = colors[pick];
        rgba[3] *= alpha;
        let turn = unit(&mut state) * 360.0 + lerp(spin, unit(&mut state)) * age;
        if s <= 0.0 || alpha <= 0.002 {
            return;
        }
        let shape = match shape {
            "square" => {
                let (c, sn) = (turn.to_radians().cos(), turn.to_radians().sin());
                let half = s / 2.0;
                let corner = |x: f64, y: f64| (px + x * c - y * sn, py + x * sn + y * c);
                VectorShape {
                    kind: VectorShapeKind::Pen,
                    points: vec![
                        corner(-half, -half),
                        corner(half, -half),
                        corner(half, half),
                        corner(-half, half),
                        corner(-half, -half),
                    ],
                    stroke_rgba: [rgba[0], rgba[1], rgba[2], 0.0],
                    stroke_width: 0.0,
                    fill_rgba: Some(rgba),
                    arrow_start: false,
                    arrow_end: false,
                    even_odd_fill: false,
                    corner_radius: 0.0,
                }
            }
            "streak" => {
                let len = (vx * vx + vy * vy).sqrt().max(1e-6);
                let (dx, dy) = (vx / len, vy / len);
                let tail = s * 3.0;
                VectorShape {
                    kind: VectorShapeKind::Line,
                    points: vec![(px - dx * tail, py - dy * tail), (px, py)],
                    stroke_rgba: rgba,
                    stroke_width: (s * 0.5).max(0.5),
                    fill_rgba: None,
                    arrow_start: false,
                    arrow_end: false,
                    even_odd_fill: false,
                    corner_radius: 0.0,
                }
            }
            _ => VectorShape {
                kind: VectorShapeKind::Oval,
                points: vec![(px - s / 2.0, py - s / 2.0), (px + s / 2.0, py + s / 2.0)],
                stroke_rgba: [rgba[0], rgba[1], rgba[2], 0.0],
                stroke_width: 0.0,
                fill_rgba: Some(rgba),
                arrow_start: false,
                arrow_end: false,
                even_odd_fill: false,
                corner_radius: 0.0,
            },
        };
        out.push(shape);
        *budget -= 1;
    };

    for j in 0..recipe.burst() as u64 {
        emit(1_000_000 + j, 0.0, &mut out, &mut budget);
    }
    if rate > 0.0 {
        let first = ((time - max_life) * rate).floor().max(0.0) as u64;
        let last = (time * rate).floor() as u64;
        for i in first..=last {
            let born = i as f64 / rate;
            if born > emit_until {
                break;
            }
            emit(i, born, &mut out, &mut budget);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> ParticleRecipe {
        ParticleRecipe {
            rate: Some(60.0),
            burst: Some(30),
            colors: Some(vec!["FF8800".into()]),
            ..Default::default()
        }
    }

    /// The same instant twice is the same picture — no state between
    /// frames — and different instants differ.
    #[test]
    fn a_frame_is_a_function_of_time_alone() {
        let settings = CompositionSettings::default();
        let a = particle_shapes(&recipe(), 0.7, 1920.0, 1080.0, &settings);
        let b = particle_shapes(&recipe(), 0.7, 1920.0, 1080.0, &settings);
        assert_eq!(a.len(), b.len());
        assert!(a
            .iter()
            .zip(&b)
            .all(|(x, y)| x.points == y.points && x.fill_rgba == y.fill_rgba));
        let c = particle_shapes(&recipe(), 1.4, 1920.0, 1080.0, &settings);
        assert!(
            a.len() > 20,
            "a burst and a second of emission: {}",
            a.len()
        );
        assert!(a.iter().zip(&c).any(|(x, y)| x.points != y.points));
    }

    /// Nothing is alive before the layer starts; the frame across the
    /// canvas is always there so the drawing's bounds are the canvas.
    #[test]
    fn before_the_start_only_the_frame_remains() {
        let settings = CompositionSettings::default();
        let shapes = particle_shapes(&recipe(), -0.5, 1920.0, 1080.0, &settings);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].points, vec![(0.0, 0.0), (1920.0, 1080.0)]);
        assert_eq!(shapes[0].stroke_rgba[3], 0.0);
    }

    /// Gravity pulls: at a later age the same particle sits lower on the
    /// canvas (y down) than its launch would carry it.
    #[test]
    fn gravity_pulls_particles_down() {
        let settings = CompositionSettings::default();
        let calm = ParticleRecipe {
            rate: Some(0.0),
            burst: Some(1),
            life: Some([10.0, 10.0]),
            speed: Some([0.0, 0.0]),
            gravity: Some(1.0),
            drag: Some(0.0),
            size: Some([0.01, 0.01]),
            size_over_life: Some("hold".into()),
            opacity_over_life: Some("hold".into()),
            ..Default::default()
        };
        let at = |t: f64| particle_shapes(&calm, t, 1000.0, 1000.0, &settings)[1].points[0].1;
        assert!(at(1.0) > at(0.5), "{} vs {}", at(1.0), at(0.5));
        assert!(
            (at(1.0) - at(0.0) - 500.0).abs() < 1.0,
            "½·g·t² with g one canvas height: {}",
            at(1.0) - at(0.0)
        );
    }
}
