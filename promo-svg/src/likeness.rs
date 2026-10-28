//! Whether two pictures of one drawing differ only by antialiasing — the
//! one tolerance the core holds a second renderer to (SVG-PLAN.md slices 4
//! and 5): the GPU's scene against resvg, and a drawing converted from an
//! SVG against the SVG.
//!
//! Both pictures are premultiplied RGBA, compared channel by channel; `b`
//! is the reference (resvg's).
//!
//! - **Interior ≤ 3 levels.** Where the reference is flat (a pixel and its
//!   eight neighbours within 2 levels: inside a shape, along a gradient,
//!   the background; a pixel on the picture's border never counts, its
//!   outer neighbours being unknown) the other must give the same colour
//!   on any two neighbouring flat pixels. tiny-skia blends in 8 bits with
//!   its own rounding, the GPU in floats rounded once per pass, and a
//!   layer adds a pass: a level or two, three at most. A LONE pixel may
//!   differ more: one multisample landing on a sliver thinner than tiny-
//!   skia's coverage resolves (1/8 of a colour at 8 samples, seen on
//!   Mesa 26's lavapipe) — a real recolour covers neighbours too.
//! - **Rims ≤ 128 levels.** Elsewhere a pixel's value is its coverage.
//!   tiny-skia measures coverage on a fine grid (4 rows, quarter-pixel
//!   columns); the GPU counts 4 multisamples at the standard positions. An
//!   edge passing between two samples flips both at once, so the two may
//!   disagree by up to half a pixel's coverage — never more: a shape moved
//!   by a pixel puts 255 on its rim.
//! - **4×4 blocks ≤ 24 levels.** Averaged over 4×4 pixels (what a viewer's
//!   eye does) the rims' disagreements largely cancel along an edge — 18 at
//!   most across the corpus. An edge moved by half a pixel along four rows
//!   is 32; a shape recoloured, thickened or missing far more.
//! - **Mean ≤ 1 level.** Rims are a small share of any picture; the
//!   densest corpus file, 2,400 shapes at 1080p, measures 0.5.

/// How far one picture is from the reference.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Diff {
    /// The largest difference two neighbouring pixels share where the
    /// reference is flat.
    pub interior: u8,
    /// The largest difference on the rest: the antialiased rims.
    pub edge: u8,
    /// The mean difference over every channel of every pixel.
    pub mean: f64,
    /// The largest difference between the two pictures' 4×4 block means.
    pub blocks: f64,
    /// Pixels differing by more than 2 levels, as a share of all.
    pub changed: f64,
    /// The top-left pixel of the block that differs most, and of the
    /// worst interior pixel — where to look, and what to name.
    pub worst_block: (u32, u32),
    pub worst_interior: (u32, u32),
}

impl Diff {
    /// Whether the difference is antialiasing and nothing else.
    pub fn passes(&self) -> bool {
        self.interior <= 3 && self.edge <= 128 && self.blocks <= 24.0 && self.mean <= 1.0
    }

    /// Where it fails most, when it fails: a pixel to name.
    pub fn worst(&self) -> Option<(u32, u32)> {
        if self.passes() {
            None
        } else if self.interior > 3 {
            Some(self.worst_interior)
        } else {
            Some(self.worst_block)
        }
    }
}

/// `a` against the reference `b`, both `w`×`h` premultiplied RGBA.
pub fn compare(a: &[u8], b: &[u8], w: u32, h: u32) -> Diff {
    let (w, h) = (w as usize, h as usize);
    assert!(
        a.len() >= w * h * 4 && b.len() >= w * h * 4,
        "two pictures of {w}x{h}"
    );
    let at = |img: &[u8], x: usize, y: usize, c: usize| img[(y * w + x) * 4 + c] as i32;
    let mut d = Diff::default();
    let mut sum = 0u64;
    let mut changed = 0usize;
    // Each pixel's worst channel difference where the reference is flat,
    // -1 elsewhere: the interior is judged on neighbouring pairs below.
    let mut flat_diff = vec![-1i32; w * h];
    for y in 0..h {
        for x in 0..w {
            let border = x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            let mut flat = !border;
            'channels: for c in 0..4 {
                let v = at(b, x, y, c);
                for (dx, dy) in [
                    (-1i32, -1i32),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if (at(b, nx as usize, ny as usize, c) - v).abs() > 2 {
                        flat = false;
                        break 'channels;
                    }
                }
            }
            let mut worst = 0;
            for c in 0..4 {
                let e = (at(a, x, y, c) - at(b, x, y, c)).unsigned_abs();
                sum += e as u64;
                worst = worst.max(e);
            }
            if worst > 2 {
                changed += 1;
            }
            let worst = worst.min(255) as u8;
            if flat {
                flat_diff[y * w + x] = worst as i32;
            } else {
                d.edge = d.edge.max(worst);
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let here = flat_diff[y * w + x];
            if here < 0 {
                continue;
            }
            let mut pair = -1;
            if x + 1 < w {
                pair = pair.max(flat_diff[y * w + x + 1].min(here));
            }
            if y + 1 < h {
                pair = pair.max(flat_diff[(y + 1) * w + x].min(here));
            }
            if x > 0 {
                pair = pair.max(flat_diff[y * w + x - 1].min(here));
            }
            if y > 0 {
                pair = pair.max(flat_diff[(y - 1) * w + x].min(here));
            }
            if pair > d.interior as i32 {
                d.interior = pair as u8;
                d.worst_interior = (x as u32, y as u32);
            }
        }
    }
    d.mean = sum as f64 / (w * h * 4).max(1) as f64;
    d.changed = changed as f64 / (w * h).max(1) as f64;
    for by in (0..h).step_by(4) {
        for bx in (0..w).step_by(4) {
            for c in 0..4 {
                let (mut sa, mut sb, mut n) = (0i64, 0i64, 0i64);
                for y in by..(by + 4).min(h) {
                    for x in bx..(bx + 4).min(w) {
                        sa += at(a, x, y, c) as i64;
                        sb += at(b, x, y, c) as i64;
                        n += 1;
                    }
                }
                let e = (sa - sb).abs() as f64 / n as f64;
                if e > d.blocks {
                    d.blocks = e;
                    d.worst_block = (bx as u32, by as u32);
                }
            }
        }
    }
    d
}
