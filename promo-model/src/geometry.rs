//! CoreGraphics-compatible geometry types. Swift's Codable conformances
//! serialize `CGPoint` as `[x, y]`, `CGSize` as `[w, h]`, and `CGRect` as
//! `[[x, y], [w, h]]` (unkeyed origin + size). Tuple structs mirror that
//! wire shape exactly.

use serde::{Deserialize, Serialize};

/// `CGPoint` — serializes as `[x, y]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Point(pub f64, pub f64);

impl Point {
    pub fn x(&self) -> f64 {
        self.0
    }
    pub fn y(&self) -> f64 {
        self.1
    }
}

/// `CGSize` — serializes as `[width, height]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Size(pub f64, pub f64);

impl Size {
    pub fn width(&self) -> f64 {
        self.0
    }
    pub fn height(&self) -> f64 {
        self.1
    }
    pub fn new(width: f64, height: f64) -> Self {
        Size(width, height)
    }
}

/// `CGRect` — serializes as `[[x, y], [width, height]]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Rect(pub Point, pub Size);

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Rect(Point(x, y), Size(width, height))
    }
    pub fn x(&self) -> f64 {
        self.0 .0
    }
    pub fn y(&self) -> f64 {
        self.0 .1
    }
    pub fn width(&self) -> f64 {
        self.1 .0
    }
    pub fn height(&self) -> f64 {
        self.1 .1
    }

    /// Mirrors Swift `CGRect.normalizedUnitRect` (ImageCrop.swift): sorts the
    /// corners, then clamps into the 0…1 unit square.
    pub fn normalized_unit_rect(&self) -> Rect {
        let x = self.x().min(self.x() + self.width());
        let y = self.y().min(self.y() + self.height());
        let w = self.width().abs();
        let h = self.height().abs();
        let clamped_x = x.clamp(0.0, 1.0);
        let clamped_y = y.clamp(0.0, 1.0);
        let clamped_max_x = clamped_x.max((x + w).min(1.0));
        let clamped_max_y = clamped_y.max((y + h).min(1.0));
        Rect::new(
            clamped_x,
            clamped_y,
            clamped_max_x - clamped_x,
            clamped_max_y - clamped_y,
        )
    }
}
