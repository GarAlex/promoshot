//! The size a decorative frame makes of the picture inside it.
//!
//! A `.device` frame is a SLAB — a bezel, an extruded depth, and an optional
//! 2.5D tilt — and it is not drawn here or anywhere in Rust: the app bakes it
//! into the image with Core Graphics and hands the engine the framed texture
//! (`FLAG_PRE_FRAMED`). That means the picture a layer shows is bigger than
//! the resource's stored pixels and a different shape, and layout has to know
//! that before any pixels exist. This module is the pure-arithmetic half of
//! the bake: the same body, the same projection, the same rounding, with the
//! drawing left out.
//!
//! Swift twin: `ResourceFrameRenderer.slabGeometry` / `.framedPixelSize`. The
//! two are held together by the parity suite; keep the clamps, the sample
//! count, and the `.rounded()` at the end identical or the box the engine
//! positions stops matching the bitmap it fills.

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
        let hw = self.body_w / 2.0;
        let hh = self.body_h / 2.0;
        let (min_x, min_y, max_x, max_y) = (-hw, -hh, hw, hh);
        let r = corner_radius.clamp(0.0, (self.body_w.min(self.body_h)) / 2.0);
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

    /// A slab is WIDER in proportion than the screenshot inside it — the
    /// whole reason placement has to ask.
    #[test]
    fn a_slab_changes_the_aspect() {
        let raw = Size::new(1290.0, 2796.0);
        let framed = framed_pixel_size(raw, Some(&device(0.035, 0.0, 0.0)));
        let raw_aspect = raw.width() / raw.height();
        let framed_aspect = framed.width() / framed.height();
        assert!(framed_aspect > raw_aspect * 1.1, "{framed_aspect} vs {raw_aspect}");
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
