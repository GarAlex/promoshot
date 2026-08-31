//! The size a decorative frame makes of the picture inside it — and, since
//! issue #6, the BAKE itself.
//!
//! A `.device` frame is a SLAB — a bezel, an extruded depth, and an optional
//! 2.5D tilt. The apps bake it into the image with Core Graphics and hand
//! the engine the framed texture (`FLAG_PRE_FRAMED`); `bake_slab` here is
//! the same bake for every Rust host, so a headless render carries the slab
//! instead of silently dropping it (a documented field rendering as nothing
//! was issue #6's finding — worse, layout sizes for the slab, so the bare
//! picture also drew mis-sized). The picture a layer shows is bigger than
//! the resource's stored pixels and a different shape, and layout has to
//! know that before any pixels exist — `framed_pixel_size` is that answer,
//! and `bake_slab` allocates EXACTLY it, so the box layout predicts and the
//! bitmap the bake produces cannot drift apart.
//!
//! Swift twin: `ResourceFrameRenderer`. The geometry (clamps, sample count,
//! the `.rounded()` at the end) is pinned by the parity suite; the raster
//! half mirrors the CG draw order — shadow, back face, walls, front face,
//! warped screen — with flat fills where CG fills flat.

use promo_model::{ResourceFrame, ResourceFrameKind, Size};

/// The pixel size `content` becomes once `frame` is baked around it —
/// `content` itself when the frame does not grow the bitmap.
///
/// Only a slab answers differently. A plain border grows the app's bake too,
/// but a border has a SECOND path: with nothing pre-baked the engine strokes
/// it onto the media quad without growing it (`media_border_style`).
/// Compensating for it here would correct the baked path and skew the
/// stroked one, so a border keeps its content's size and the two paths go on
/// agreeing. A slab has one path only, so its baked size is always right.
pub fn framed_pixel_size(content: Size, frame: Option<&ResourceFrame>) -> Size {
    match frame {
        Some(f) => slab_geometry(content, f).map_or(content, |slab| slab.pixel_size()),
        None => content,
    }
}

/// The body a slab builds around a `content`-sized picture, and the projected
/// box that body occupies.
pub struct SlabGeometry {
    pub body_w: f64,
    pub body_h: f64,
    pub depth: f64,
    pub bezel: f64,
    /// Projected bounds, padded, in the geometry's own centred space.
    pub box_w: f64,
    pub box_h: f64,
}

impl SlabGeometry {
    /// The bitmap the bake allocates for this box.
    pub fn pixel_size(&self) -> Size {
        Size::new(self.box_w.round(), self.box_h.round())
    }
}

/// Nil for anything that is not a slab, and for a degenerate box.
pub fn slab_geometry(content: Size, frame: &ResourceFrame) -> Option<SlabGeometry> {
    let screen_w = content.width();
    let screen_h = content.height();
    if frame.kind != ResourceFrameKind::Device || screen_w <= 0.0 || screen_h <= 0.0 {
        return None;
    }
    let bezel_fraction = frame.bezel_fraction.clamp(0.005, 0.12);
    let depth_fraction = frame.depth_fraction.clamp(0.005, 0.25);

    let bezel = screen_w * bezel_fraction;
    let body_w = screen_w + bezel * 2.0;
    let body_h = screen_h + bezel * 2.0;
    let depth = body_w * depth_fraction;

    let geo = PhoneFrameGeometry::new(body_w, body_h, depth, frame.tilt_x, frame.tilt_y);
    // Pad so shadow / edges never clip.
    let pad = body_w.max(body_h) * 0.06;
    let bounds = geo.projected_bounds();
    let box_w = bounds.2 - bounds.0 + pad * 2.0;
    let box_h = bounds.3 - bounds.1 + pad * 2.0;
    if box_w <= 1.0 || box_h <= 1.0 {
        return None;
    }
    Some(SlabGeometry {
        body_w,
        body_h,
        depth,
        bezel,
        box_w,
        box_h,
    })
}

/// Swift twin: `PhoneFrameGeometry`. Builds the body's outline in a centred
/// face space, rotates by the two tilt angles, and projects with a simple
/// pinhole camera.
struct PhoneFrameGeometry {
    body_w: f64,
    body_h: f64,
    depth: f64,
    rot_x: f64,
    rot_y: f64,
    viewer_distance: f64,
}

impl PhoneFrameGeometry {
    fn new(body_w: f64, body_h: f64, depth: f64, tilt_x_deg: f64, tilt_y_deg: f64) -> Self {
        PhoneFrameGeometry {
            body_w,
            body_h,
            depth,
            rot_x: tilt_x_deg * std::f64::consts::PI / 180.0,
            rot_y: tilt_y_deg * std::f64::consts::PI / 180.0,
            // ~3.2 body-heights gives a natural, not-too-extreme
            // foreshortening.
            viewer_distance: body_w.max(body_h) * 3.2,
        }
    }

    /// Sampled rounded-rect outline of the face at `z`, in face space.
    fn rounded_outline(&self, z: f64, corner_radius: f64) -> Vec<(f64, f64, f64)> {
        self.rounded_outline_inset(0.0, z, corner_radius)
    }

    /// Four corners of the (optionally inset) face at `z`: TL, TR, BR, BL.
    fn face_quad(&self, inset: f64, z: f64) -> [(f64, f64, f64); 4] {
        let hw = self.body_w / 2.0 - inset;
        let hh = self.body_h / 2.0 - inset;
        [(-hw, -hh, z), (hw, -hh, z), (hw, hh, z), (-hw, hh, z)]
    }

    fn rounded_outline_inset(
        &self,
        inset: f64,
        z: f64,
        corner_radius: f64,
    ) -> Vec<(f64, f64, f64)> {
        let hw = self.body_w / 2.0 - inset;
        let hh = self.body_h / 2.0 - inset;
        let (min_x, min_y, max_x, max_y) = (-hw, -hh, hw, hh);
        // Clamp against the INSET rect, exactly as the Swift twin clamps
        // against the rect it was handed (an inset of 0 reproduces the old
        // body-sized clamp bit for bit).
        let r = corner_radius.clamp(0.0, hw.min(hh).max(0.0));
        let samples = 6;
        let half_pi = std::f64::consts::FRAC_PI_2;
        // Corner centres, clockwise from top-left.
        let corners = [
            (min_x + r, min_y + r, std::f64::consts::PI),
            (max_x - r, min_y + r, -half_pi),
            (max_x - r, max_y - r, 0.0),
            (min_x + r, max_y - r, half_pi),
        ];
        let mut pts = Vec::with_capacity(corners.len() * (samples + 1));
        for (cx, cy, start) in corners {
            for s in 0..=samples {
                let a = start + half_pi * (s as f64) / (samples as f64);
                pts.push((cx + a.cos() * r, cy + a.sin() * r, z));
            }
        }
        pts
    }

    /// `(min_x, min_y, max_x, max_y)` of the front and back body outlines.
    fn projected_bounds(&self) -> (f64, f64, f64, f64) {
        // The renderer's own bound uses a fixed 0.12 corner rather than the
        // body's `bodyCornerFraction`; harmless (a sampled rounded rect's
        // bounding box barely moves with its radius) but reproduced exactly
        // so the two sides answer the same number.
        let cr = self.body_w * 0.12;
        let front_z = self.depth / 2.0;
        let back_z = -front_z;
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;
        for z in [front_z, back_z] {
            for p in self.rounded_outline(z, cr) {
                let (x, y) = self.project(p);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        (min_x, min_y, max_x, max_y)
    }

    fn project(&self, p: (f64, f64, f64)) -> (f64, f64) {
        // Rotate about Y then X.
        let (cy, sy) = (self.rot_y.cos(), self.rot_y.sin());
        let x1 = p.0 * cy + p.2 * sy;
        let z1 = -p.0 * sy + p.2 * cy;
        let (cx, sx) = (self.rot_x.cos(), self.rot_x.sin());
        let y2 = p.1 * cx - z1 * sx;
        let z2 = p.1 * sx + z1 * cx;
        // Pinhole projection (camera on +z looking toward -z).
        let f = self.viewer_distance / (self.viewer_distance - z2);
        (x1 * f, y2 * f)
    }
}

// ---------------------------------------------------------------------------
// The bake: premultiplied BGRA in, the slab-framed bitmap out.

/// The slab's base body colour per material — the Swift `Material.bodyHex`
/// table, verbatim. Edge shading derives lighter/darker tones at bake time.
fn material_body_rgb(material: promo_model::FrameMaterial) -> [u8; 3] {
    use promo_model::FrameMaterial::*;
    let hex: u32 = match material {
        SpaceBlack => 0x2B2B2E,
        NaturalTitanium => 0x9B978F,
        Silver => 0xD8DADC,
        Gold => 0xE6D2A8,
        DeepBlue => 0x3A4A63,
        PlasticWhite => 0xF4F4F2,
        PlasticBlack => 0x202124,
        PlasticBlue => 0x2E6BE6,
        PlasticRed => 0xE5453B,
        PlasticGreen => 0x33B15B,
        PlasticYellow => 0xF5C518,
        PlasticPink => 0xF06AA6,
    };
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// Premultiplied-BGRA colour from an RGB base and a shade factor.
fn shaded(rgb: [u8; 3], factor: f64) -> [f64; 4] {
    let ch = |v: u8| (f64::from(v) / 255.0 * factor).clamp(0.0, 1.0);
    [ch(rgb[2]), ch(rgb[1]), ch(rgb[0]), 1.0]
}

/// Source-over of a premultiplied colour onto one premultiplied BGRA pixel.
fn over(dst: &mut [u8], src: [f64; 4]) {
    let inv = 1.0 - src[3];
    for (i, s) in src.iter().enumerate() {
        let d = f64::from(dst[i]) / 255.0;
        dst[i] = ((s + d * inv) * 255.0 + 0.5) as u8;
    }
}

/// Even-odd scanline fill of a polygon with a constant premultiplied colour.
/// The outlines a slab fills are convex or near-convex sampled paths; the
/// even-odd rule handles them all without caring.
fn fill_polygon(buf: &mut [u8], width: u32, height: u32, pts: &[(f64, f64)], color: [f64; 4]) {
    scanline(buf, width, height, pts, |dst, _, _| over(dst, color));
}

fn scanline<F: FnMut(&mut [u8], f64, f64)>(
    buf: &mut [u8],
    width: u32,
    height: u32,
    pts: &[(f64, f64)],
    mut paint: F,
) {
    if pts.len() < 3 {
        return;
    }
    let min_y = pts
        .iter()
        .map(|p| p.1)
        .fold(f64::MAX, f64::min)
        .floor()
        .max(0.0) as u32;
    let max_y = pts
        .iter()
        .map(|p| p.1)
        .fold(f64::MIN, f64::max)
        .ceil()
        .min(f64::from(height)) as u32;
    let mut crossings: Vec<f64> = Vec::with_capacity(8);
    for row in min_y..max_y {
        let y = f64::from(row) + 0.5;
        crossings.clear();
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            if (y0 <= y && y1 > y) || (y1 <= y && y0 > y) {
                crossings.push(x0 + (y - y0) / (y1 - y0) * (x1 - x0));
            }
        }
        crossings.sort_by(|a, b| a.total_cmp(b));
        for pair in crossings.chunks_exact(2) {
            let from = pair[0].max(0.0).round() as u32;
            let to = pair[1].min(f64::from(width)).round() as u32;
            for column in from..to {
                let at = ((row * width + column) * 4) as usize;
                paint(&mut buf[at..at + 4], f64::from(column) + 0.5, y);
            }
        }
    }
}

/// The projective map that carries the UNIT square onto `quad`
/// (TL, TR, BR, BL), inverted — so a destination pixel answers where in the
/// screenshot it looks. The Swift side asks Core Image the same question
/// (`CIPerspectiveTransform`); this is that filter's arithmetic.
struct Homography {
    m: [f64; 9],
}

impl Homography {
    fn unit_to_quad_inverse(quad: &[(f64, f64); 4]) -> Option<Homography> {
        let (p0, p1, p2, p3) = (quad[0], quad[1], quad[2], quad[3]);
        let dx1 = (p1.0 - p2.0, p1.1 - p2.1);
        let dx2 = (p3.0 - p2.0, p3.1 - p2.1);
        let sum = (p0.0 - p1.0 + p2.0 - p3.0, p0.1 - p1.1 + p2.1 - p3.1);
        let den = dx1.0 * dx2.1 - dx2.0 * dx1.1;
        if den.abs() < f64::EPSILON {
            return None;
        }
        let g = (sum.0 * dx2.1 - dx2.0 * sum.1) / den;
        let h = (dx1.0 * sum.1 - sum.0 * dx1.1) / den;
        let forward = [
            p1.0 - p0.0 + g * p1.0,
            p3.0 - p0.0 + h * p3.0,
            p0.0,
            p1.1 - p0.1 + g * p1.1,
            p3.1 - p0.1 + h * p3.1,
            p0.1,
            g,
            h,
            1.0,
        ];
        // Invert the 3x3 (adjugate over determinant).
        let m = &forward;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if det.abs() < f64::EPSILON {
            return None;
        }
        let inv = [
            (m[4] * m[8] - m[5] * m[7]) / det,
            (m[2] * m[7] - m[1] * m[8]) / det,
            (m[1] * m[5] - m[2] * m[4]) / det,
            (m[5] * m[6] - m[3] * m[8]) / det,
            (m[0] * m[8] - m[2] * m[6]) / det,
            (m[2] * m[3] - m[0] * m[5]) / det,
            (m[3] * m[7] - m[4] * m[6]) / det,
            (m[1] * m[6] - m[0] * m[7]) / det,
            (m[0] * m[4] - m[1] * m[3]) / det,
        ];
        Some(Homography { m: inv })
    }

    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let w = self.m[6] * x + self.m[7] * y + self.m[8];
        (
            (self.m[0] * x + self.m[1] * y + self.m[2]) / w,
            (self.m[3] * x + self.m[4] * y + self.m[5]) / w,
        )
    }
}

/// Bilinear sample of premultiplied BGRA at unit coordinates, edge-clamped.
fn sample(content: &[u8], width: u32, height: u32, u: f64, v: f64) -> [f64; 4] {
    let x = (u.clamp(0.0, 1.0) * f64::from(width) - 0.5).max(0.0);
    let y = (v.clamp(0.0, 1.0) * f64::from(height) - 0.5).max(0.0);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - f64::from(x0), y - f64::from(y0));
    let px = |cx: u32, cy: u32| {
        let at = ((cy * width + cx) * 4) as usize;
        [
            f64::from(content[at]) / 255.0,
            f64::from(content[at + 1]) / 255.0,
            f64::from(content[at + 2]) / 255.0,
            f64::from(content[at + 3]) / 255.0,
        ]
    };
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = a[i] * (1.0 - fx) + b[i] * fx;
        let bottom = c[i] * (1.0 - fx) + d[i] * fx;
        out[i] = top * (1.0 - fy) + bottom * fy;
    }
    out
}

/// One box-blur pass over an alpha mask, horizontal then vertical.
fn box_blur(mask: &mut [f32], width: usize, height: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    let mut scratch = vec![0.0f32; mask.len()];
    let window = (radius * 2 + 1) as f32;
    for row in 0..height {
        let line = &mask[row * width..(row + 1) * width];
        let mut acc: f32 =
            line[0] * radius as f32 + line[..(radius + 1).min(width)].iter().sum::<f32>();
        for column in 0..width {
            scratch[row * width + column] = acc / window;
            let add = line[(column + radius + 1).min(width - 1)];
            let sub = line[column.saturating_sub(radius)];
            acc += add - sub;
        }
    }
    for column in 0..width {
        let mut acc: f32 = 0.0;
        for row in 0..(radius + 1).min(height) {
            acc += scratch[row * width + column];
        }
        acc += scratch[column] * radius as f32;
        for row in 0..height {
            mask[row * width + column] = acc / window;
            let add = scratch[(row + radius + 1).min(height - 1) * width + column];
            let sub = scratch[row.saturating_sub(radius) * width + column];
            acc += add - sub;
        }
    }
}

/// Bakes a device slab around `content` (premultiplied BGRA), answering the
/// framed bitmap at EXACTLY `framed_pixel_size` — layout and texture agree
/// by construction. None when the frame is not a slab (callers use the raw
/// picture, and the engine strokes borders itself).
///
/// Draw order and shading mirror the Swift `renderSlab`: contact shadow
/// (silhouette, blurred, 35%), back face at 0.45, extruded walls at
/// 0.62/0.5 (left walls lighter), front face, then the screenshot warped
/// into the projected screen and clipped by its rounded outline. The slab
/// layer is rasterized at 2x and box-filtered down, which stands in for
/// Core Graphics' antialiasing.
pub fn bake_slab(
    content: &[u8],
    width: u32,
    height: u32,
    frame: &ResourceFrame,
) -> Option<(Vec<u8>, u32, u32)> {
    let slab = slab_geometry(Size::new(f64::from(width), f64::from(height)), frame)?;
    let out_w = slab.box_w.round() as u32;
    let out_h = slab.box_h.round() as u32;
    if out_w == 0 || out_h == 0 || content.len() < (width * height * 4) as usize {
        return None;
    }
    let geo = PhoneFrameGeometry::new(
        slab.body_w,
        slab.body_h,
        slab.depth,
        frame.tilt_x,
        frame.tilt_y,
    );
    let bounds = geo.projected_bounds();
    let pad = slab.body_w.max(slab.body_h) * 0.06;
    let (origin_x, origin_y) = (bounds.0 - pad, bounds.1 - pad);
    let body_corner_fraction = (frame.bezel_fraction.clamp(0.005, 0.12) * 2.2 + 0.01).min(0.12);
    let body_corner = slab.body_w * body_corner_fraction;
    let base = material_body_rgb(frame.material);

    const SS: u32 = 2; // supersample factor for the slab layer
    let place =
        |scale: f64| move |p: (f64, f64)| ((p.0 - origin_x) * scale, (p.1 - origin_y) * scale);
    let front: Vec<(f64, f64)> = geo
        .rounded_outline(geo_front_z(&geo), body_corner)
        .iter()
        .map(|p| geo.project(*p))
        .collect();
    let back: Vec<(f64, f64)> = geo
        .rounded_outline(geo_back_z(&geo), body_corner)
        .iter()
        .map(|p| geo.project(*p))
        .collect();

    // The contact shadow, at output resolution: the front silhouette at
    // 90%, blurred against the same ~1080-wide reference the app uses,
    // composited at 35%.
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];
    {
        let mut mask = vec![0.0f32; (out_w * out_h) as usize];
        let placed: Vec<(f64, f64)> = front.iter().map(|p| place(1.0)(*p)).collect();
        let mut silhouette = vec![0u8; (out_w * out_h * 4) as usize];
        fill_polygon(&mut silhouette, out_w, out_h, &placed, [0.0, 0.0, 0.0, 0.9]);
        for (i, px) in silhouette.chunks_exact(4).enumerate() {
            mask[i] = f32::from(px[3]) / 255.0;
        }
        let blur = (24.0 * slab.body_w / 1080.0).max(1.0);
        let radius = ((blur / 2.0).round() as usize).max(1);
        for _ in 0..3 {
            box_blur(&mut mask, out_w as usize, out_h as usize, radius);
        }
        for (i, value) in mask.iter().enumerate() {
            over(
                &mut out[i * 4..i * 4 + 4],
                [0.0, 0.0, 0.0, f64::from(*value) * 0.35],
            );
        }
    }

    // The slab itself, at 2x: back face, walls, front face, screen.
    let (ss_w, ss_h) = (out_w * SS, out_h * SS);
    let mut layer = vec![0u8; (ss_w * ss_h * 4) as usize];
    let up = place(f64::from(SS));
    let front2: Vec<(f64, f64)> = front.iter().map(|p| up(*p)).collect();
    let back2: Vec<(f64, f64)> = back.iter().map(|p| up(*p)).collect();
    fill_polygon(&mut layer, ss_w, ss_h, &back2, shaded(base, 0.45));
    let centroid_x = front2.iter().map(|p| p.0).sum::<f64>() / front2.len() as f64;
    for i in 0..front2.len() {
        let j = (i + 1) % front2.len();
        let wall = [front2[i], front2[j], back2[j], back2[i]];
        let mid = (front2[i].0 + front2[j].0) / 2.0;
        let side = if mid < centroid_x { 0.62 } else { 0.5 };
        fill_polygon(&mut layer, ss_w, ss_h, &wall, shaded(base, side));
    }
    fill_polygon(&mut layer, ss_w, ss_h, &front2, shaded(base, 1.0));

    let bezel = slab.bezel;
    let screen_radius = (body_corner - bezel).max(0.0);
    let screen_quad: [(f64, f64); 4] = {
        let q = geo.face_quad(bezel, geo_front_z(&geo));
        [
            up(geo.project(q[0])),
            up(geo.project(q[1])),
            up(geo.project(q[2])),
            up(geo.project(q[3])),
        ]
    };
    let screen_outline: Vec<(f64, f64)> = geo
        .rounded_outline_inset(bezel, geo_front_z(&geo), screen_radius)
        .iter()
        .map(|p| up(geo.project(*p)))
        .collect();
    if let Some(h) = Homography::unit_to_quad_inverse(&screen_quad) {
        scanline(&mut layer, ss_w, ss_h, &screen_outline, |dst, x, y| {
            let (u, v) = h.map(x, y);
            over(dst, sample(content, width, height, u, v));
        });
    } else {
        fill_polygon(
            &mut layer,
            ss_w,
            ss_h,
            &screen_outline,
            [0.05, 0.05, 0.05, 1.0],
        );
    }

    // Box-filter the 2x layer down and composite it over the shadow.
    for row in 0..out_h {
        for column in 0..out_w {
            let mut acc = [0.0f64; 4];
            for dy in 0..SS {
                for dx in 0..SS {
                    let at = (((row * SS + dy) * ss_w + column * SS + dx) * 4) as usize;
                    for (i, a) in acc.iter_mut().enumerate() {
                        *a += f64::from(layer[at + i]) / 255.0;
                    }
                }
            }
            let inv = 1.0 / f64::from(SS * SS);
            let at = ((row * out_w + column) * 4) as usize;
            over(
                &mut out[at..at + 4],
                [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv],
            );
        }
    }
    Some((out, out_w, out_h))
}

fn geo_front_z(geo: &PhoneFrameGeometry) -> f64 {
    geo.depth / 2.0
}

fn geo_back_z(geo: &PhoneFrameGeometry) -> f64 {
    -geo.depth / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(bezel: f64, tilt_x: f64, tilt_y: f64) -> ResourceFrame {
        ResourceFrame {
            kind: ResourceFrameKind::Device,
            bezel_fraction: bezel,
            tilt_x,
            tilt_y,
            ..Default::default()
        }
    }

    /// The numbers the Swift twin produces for the App Store wizard's iPhone
    /// preset. Both sides are pinned to these, which is what stops the box
    /// layout positions from drifting off the bitmap the app bakes.
    #[test]
    fn iphone_preset_matches_the_swift_bake() {
        let size = framed_pixel_size(Size::new(1290.0, 2796.0), Some(&device(0.035, 0.0, 0.0)));
        assert_eq!(size.width(), 1733.0);
        assert_eq!(size.height(), 3246.0);
    }

    /// Issue #6, closed the strong way: the bake exists in Rust, and it
    /// allocates EXACTLY the bitmap layout predicts — the agreement whose
    /// silent absence made headless drop the slab AND mis-place the bare
    /// picture inside a slab-sized box.
    #[test]
    fn the_bake_matches_layout_and_paints_the_slab() {
        let (w, h) = (200u32, 400u32);
        // Solid red screenshot, premultiplied BGRA.
        let mut content = vec![0u8; (w * h * 4) as usize];
        for px in content.chunks_exact_mut(4) {
            px[2] = 255;
            px[3] = 255;
        }
        let frame = device(0.035, 0.0, 10.0);
        let (baked, bw, bh) = bake_slab(&content, w, h, &frame).unwrap();
        let predicted = framed_pixel_size(Size::new(200.0, 400.0), Some(&frame));
        assert_eq!(
            (bw, bh),
            (predicted.width() as u32, predicted.height() as u32),
            "the bake and layout answer the same bitmap"
        );
        assert_eq!(baked[3], 0, "the padded corner stays transparent");
        let mid = ((bh / 2 * bw + bw / 2) * 4) as usize;
        assert!(
            baked[mid + 2] > 200 && baked[mid + 3] == 255,
            "the centre is the screenshot"
        );
        // Walking in from the left on the midline, the first fully opaque
        // pixel is the BODY — dark spaceBlack, not the red screen — proving
        // a bezel/wall is actually painted around the picture.
        let mut x = 0;
        while x < bw {
            if baked[((bh / 2 * bw + x) * 4 + 3) as usize] == 255 {
                break;
            }
            x += 1;
        }
        let edge = ((bh / 2 * bw + x) * 4) as usize;
        assert!(
            x > 0 && x < bw / 2,
            "an opaque edge exists left of the screen"
        );
        assert!(
            baked[edge + 2] < 100,
            "and it is body material, not screenshot: red={}",
            baked[edge + 2]
        );
    }

    /// The screen warp: the projective map that carries the unit square to
    /// the projected screen corners must invert exactly at those corners.
    #[test]
    fn the_screen_warp_inverts_its_corners() {
        let quad = [(10.0, 5.0), (110.0, 15.0), (100.0, 215.0), (5.0, 200.0)];
        let h = Homography::unit_to_quad_inverse(&quad).unwrap();
        for (corner, expect) in quad
            .iter()
            .zip([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)])
        {
            let (u, v) = h.map(corner.0, corner.1);
            assert!(
                (u - expect.0).abs() < 1e-9 && (v - expect.1).abs() < 1e-9,
                "corner {corner:?} mapped to ({u}, {v}), wanted {expect:?}"
            );
        }
    }

    /// A slab is WIDER in proportion than the screenshot inside it — the
    /// whole reason placement has to ask.
    #[test]
    fn a_slab_changes_the_aspect() {
        let raw = Size::new(1290.0, 2796.0);
        let framed = framed_pixel_size(raw, Some(&device(0.035, 0.0, 0.0)));
        let raw_aspect = raw.width() / raw.height();
        let framed_aspect = framed.width() / framed.height();
        assert!(
            framed_aspect > raw_aspect * 1.1,
            "{framed_aspect} vs {raw_aspect}"
        );
    }

    /// Turning the slab narrows it — the reason the aspect is asked for
    /// per-time rather than per-resource.
    #[test]
    fn tilt_narrows_the_projection() {
        let raw = Size::new(1290.0, 2796.0);
        let flat = framed_pixel_size(raw, Some(&device(0.035, 0.0, 0.0)));
        let turned = framed_pixel_size(raw, Some(&device(0.035, 0.0, -30.0)));
        assert!(turned.width() < flat.width());
    }

    /// Anything that is not a slab passes its content straight through, so
    /// every project without one resolves exactly as it always did.
    #[test]
    fn only_a_slab_changes_the_size() {
        let raw = Size::new(800.0, 600.0);
        assert_eq!(framed_pixel_size(raw, None).width(), 800.0);
        let border = ResourceFrame {
            kind: ResourceFrameKind::Border,
            border_width: 40.0,
            ..Default::default()
        };
        assert_eq!(framed_pixel_size(raw, Some(&border)).width(), 800.0);
        let none = ResourceFrame::default();
        assert_eq!(framed_pixel_size(raw, Some(&none)).height(), 600.0);
    }
}
