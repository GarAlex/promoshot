//! The GPU's share of a picture (SVG-PLAN.md slice 4).
//!
//! [`Picture::scene`] reads the drawn tree in paint order and hands the GPU
//! every path it can draw the way resvg draws it — a solid, linear or
//! radial paint, any fill rule, any stroke (stroked and dashed here by
//! tiny-skia's own stroker, so joins, caps, miters and dashes are resvg's
//! geometry), under any transform, an element's opacity, a group's opacity
//! or a plain clip — and lets resvg draw the rest into pieces composited
//! between them in paint order: filters, masks, pattern paint, pictures
//! inside the picture, hairline strokes (thinner than a pixel: tiny-skia
//! draws those with a modulated hairline, not a filled outline),
//! `shape-rendering="crispEdges"`, a clip of a clip, the radial gradients
//! whose focus is on or outside their circle. A blend mode anywhere sends
//! the whole picture to resvg: it reads what is drawn beneath it.
//!
//! Every number the GPU evaluates is the number tiny-skia's pipeline would:
//! the paint's pixel → unit matrix, the gradient's stop table, the spread.
//! The ported pieces mirror tiny-skia 0.12's shaders (BSD-3-Clause, as
//! tiny-skia itself), pinned beside it in Cargo.toml.

use std::collections::BTreeMap;

use promo_gpu::vector::scene::{
    ClipShape, Gradient, GradientShape, Item, Layer, Paint, Raster, Scene, Seg, Shape, Spread, Stop,
};
use tiny_skia::{Path, PathSegment, Point, Transform};

use crate::Picture;

/// What the GPU took and what it left to resvg — the routing reads it.
#[derive(Debug, Clone, Default)]
pub struct SceneStats {
    /// Shapes (fills and stroke outlines) the GPU draws.
    pub shapes: usize,
    /// Path segments across them — the tessellation's work.
    pub segments: usize,
    /// Groups drawn as layers.
    pub layers: usize,
    /// Pieces resvg drew, and their pixels.
    pub rasters: usize,
    pub raster_pixels: u64,
    /// Why nodes went to resvg: reason → count.
    pub fallbacks: BTreeMap<&'static str, usize>,
}

/// A picture as the GPU draws it.
#[derive(Debug, Clone)]
pub struct SceneBuild {
    pub scene: Scene,
    pub stats: SceneStats,
}

const DEGENERATE_THRESHOLD: f32 = 1.0 / (1 << 15) as f32;
const SCALAR_NEARLY_ZERO: f32 = 1.0 / (1 << 12) as f32;

fn nearly_zero(v: f32) -> bool {
    v.abs() <= SCALAR_NEARLY_ZERO
}

fn bound(v: f32, min: f32, max: f32) -> f32 {
    max.min(v).max(min)
}

/// The pieces resvg drew for recent scenes, kept so a frame whose edit
/// leaves a filter or a picture inside the picture alone does not draw it
/// again: a keyed badge beside a large blur pays for the blur once.
#[derive(Default)]
pub struct PieceCache {
    entries: Vec<(u64, std::sync::Arc<Vec<u8>>, u64)>,
    clock: u64,
    /// Pieces served from the cache, and drawn.
    pub hits: u64,
    pub misses: u64,
}

impl PieceCache {
    /// The most pieces kept: a picture rarely has more than a few filters
    /// or pictures, and each piece is at most a frame.
    pub const KEEP: usize = 8;

    fn get(&mut self, key: u64) -> Option<std::sync::Arc<Vec<u8>>> {
        self.clock += 1;
        let clock = self.clock;
        let hit = self.entries.iter_mut().find(|(k, _, _)| *k == key)?;
        hit.2 = clock;
        self.hits += 1;
        Some(hit.1.clone())
    }

    fn put(&mut self, key: u64, pixels: std::sync::Arc<Vec<u8>>) {
        if self.entries.len() >= Self::KEEP {
            if let Some(i) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.2)
                .map(|(i, _)| i)
            {
                self.entries.remove(i);
            }
        }
        self.clock += 1;
        self.entries.push((key, pixels, self.clock));
    }
}

impl Picture {
    /// The picture — or the part `crop` names — as a scene of `width`×
    /// `height` pixels for the GPU, drawing exactly what [`Picture::render`]
    /// draws but for antialiasing. `Err` names why the whole picture is
    /// resvg's (a blend mode, an empty or over-cap size).
    pub fn scene(
        &self,
        width: u32,
        height: u32,
        crop: Option<[f64; 4]>,
    ) -> Result<SceneBuild, &'static str> {
        self.scene_with(width, height, crop, None)
    }

    /// [`Picture::scene`], serving resvg's pieces from `pieces` where this
    /// picture's edits leave them alone.
    pub fn scene_with(
        &self,
        width: u32,
        height: u32,
        crop: Option<[f64; 4]>,
        pieces: Option<&mut PieceCache>,
    ) -> Result<SceneBuild, &'static str> {
        if width == 0
            || height == 0
            || width > crate::MAX_RASTER_SIDE
            || height > crate::MAX_RASTER_SIDE
            || width as u64 * height as u64 > crate::MAX_RASTER_PIXELS
        {
            return Err("an empty or over-cap size");
        }
        let [cx, cy, cw, ch] = crop.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        if cw <= 0.0 || ch <= 0.0 {
            return Err("an empty crop");
        }
        if blends(self.tree().root()) {
            return Err("a blend mode, which reads what is drawn beneath it");
        }
        let (w, h) = self.size();
        let sx = width as f64 / (cw * w);
        let sy = height as f64 / (ch * h);
        let root = Transform::from_row(
            sx as f32,
            0.0,
            0.0,
            sy as f32,
            (-cx * w * sx) as f32,
            (-cy * h * sy) as f32,
        );
        let mut planner = Planner {
            width,
            height,
            stats: SceneStats::default(),
            pieces,
            edited: &self.edited,
            base: self.base_stamp,
        };
        let mut list = List::default();
        for child in self.tree().root().children() {
            planner.node(child, root, &mut list);
        }
        let items = planner.finish(list);
        Ok(SceneBuild {
            scene: Scene {
                width,
                height,
                items,
            },
            stats: planner.stats,
        })
    }
}

/// Whether any group draws with a blend mode other than normal.
fn blends(group: &usvg::Group) -> bool {
    if group.blend_mode() != usvg::BlendMode::Normal {
        return true;
    }
    group.children().iter().any(|child| {
        let mut found = matches!(child, usvg::Node::Group(g) if blends(g));
        if let usvg::Node::Text(t) = child {
            found |= blends(t.flattened());
        }
        child.subroots(|sub| found |= blends(sub));
        found
    })
}

/// One list of items being built, with the resvg nodes waiting to be drawn
/// together as one piece.
#[derive(Default)]
struct List<'a> {
    items: Vec<Item>,
    pending: Vec<(&'a usvg::Node, Transform)>,
}

struct Planner<'c> {
    width: u32,
    height: u32,
    stats: SceneStats,
    pieces: Option<&'c mut PieceCache>,
    /// The edited elements' ids.
    edited: &'c [String],
    /// The plain picture's stamp.
    base: u64,
}

/// FNV-1a, folding more bytes into `hash`.
fn fold(mut hash: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Every id in a node's subtree, in order, folded into `hash`; and whether
/// one of them is in `edited`.
fn signature(node: &usvg::Node, edited: &[String], hash: &mut u64) -> bool {
    *hash = fold(*hash, node.id().as_bytes());
    *hash = fold(*hash, &[0x1f]);
    let mut dirty = edited.iter().any(|e| e == node.id());
    let mut walk = |g: &usvg::Group, hash: &mut u64| {
        for child in g.children() {
            dirty |= signature(child, edited, hash);
        }
    };
    match node {
        usvg::Node::Group(g) => walk(g, hash),
        usvg::Node::Text(t) => walk(t.flattened(), hash),
        _ => {}
    }
    dirty
}

impl Planner<'_> {
    fn finish(&mut self, mut list: List) -> Vec<Item> {
        self.flush(&mut list);
        list.items
    }

    fn push(&mut self, list: &mut List, item: Item) {
        self.flush(list);
        list.items.push(item);
    }

    fn fallback(&mut self, why: &'static str) {
        *self.stats.fallbacks.entry(why).or_default() += 1;
    }

    fn node<'a>(&mut self, node: &'a usvg::Node, ts: Transform, list: &mut List<'a>) {
        match node {
            usvg::Node::Path(p) => match self.path(p, ts, 1.0) {
                Ok(Some(item)) => self.push(list, item),
                Ok(None) => {}
                Err(why) => {
                    self.fallback(why);
                    list.pending.push((node, ts));
                }
            },
            usvg::Node::Group(g) => self.group(node, g, ts, list),
            usvg::Node::Text(t) => self.group(node, t.flattened(), ts, list),
            usvg::Node::Image(_) => {
                self.fallback("a picture inside the picture");
                list.pending.push((node, ts));
            }
        }
    }

    fn group<'a>(
        &mut self,
        node: &'a usvg::Node,
        g: &'a usvg::Group,
        ts: Transform,
        list: &mut List<'a>,
    ) {
        let t = ts.pre_concat(g.transform());
        if !g.should_isolate() {
            for child in g.children() {
                self.node(child, t, list);
            }
            return;
        }
        let refused = if !g.filters().is_empty() {
            Some("a filter")
        } else if g.mask().is_some() {
            Some("a mask")
        } else {
            None
        };
        if let Some(why) = refused {
            self.fallback(why);
            list.pending.push((node, ts));
            return;
        }
        // An opacity over one shape is the shape's own: no layer.
        if g.clip_path().is_none() && !g.isolate() {
            if let Some((path, path_ts, opacity)) = single_path(g, t, g.opacity().get()) {
                match self.path(path, path_ts, opacity) {
                    Ok(Some(item)) => self.push(list, item),
                    Ok(None) => {}
                    Err(why) => {
                        self.fallback(why);
                        list.pending.push((node, ts));
                    }
                }
                return;
            }
        }
        let clip = match g.clip_path() {
            None => None,
            Some(c) => match clip_shapes(c, t) {
                Ok(shapes) => Some(shapes),
                Err(why) => {
                    self.fallback(why);
                    list.pending.push((node, ts));
                    return;
                }
            },
        };
        // resvg's layer: the group's layer box, out by 2 px.
        let Some(bbox) = g.layer_bounding_box().transform(t) else {
            return;
        };
        let (x, y) = (bbox.x().floor() as i32 - 2, bbox.y().floor() as i32 - 2);
        let (lw, lh) = (
            bbox.width().ceil() as i64 + 4,
            bbox.height().ceil() as i64 + 4,
        );
        let Some((x, y, lw, lh)) = self.clamp_rect(x as i64, y as i64, lw, lh) else {
            return;
        };
        let mut inner = List::default();
        for child in g.children() {
            self.node(child, t, &mut inner);
        }
        let items = self.finish(inner);
        self.stats.layers += 1;
        self.push(
            list,
            Item::Layer(Layer {
                x,
                y,
                width: lw,
                height: lh,
                opacity: g.opacity().get(),
                clip,
                items,
            }),
        );
    }

    /// `(x, y, w, h)` inside the frame.
    fn clamp_rect(&self, x: i64, y: i64, w: i64, h: i64) -> Option<(i32, i32, u32, u32)> {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.width as i64);
        let y1 = (y + h).min(self.height as i64);
        (x1 > x0 && y1 > y0).then_some((x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Draws the waiting nodes with resvg into one piece.
    fn flush(&mut self, list: &mut List) {
        if list.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut list.pending);
        let mut rect: Option<tiny_skia::Rect> = None;
        for (node, ts) in &pending {
            if let Some(r) = extent(node, *ts) {
                rect = Some(match rect {
                    None => r,
                    Some(o) => o.join(&r).unwrap_or(o),
                });
            }
        }
        let Some(r) = rect else { return };
        let (mut x, mut y) = (r.left().floor() as i64 - 2, r.top().floor() as i64 - 2);
        // A picture inside the picture is sampled at texel boundaries a
        // float rounding moves: drawn from the frame's own origin, its
        // transform is the full render's to the bit.
        if pending.iter().any(|(n, _)| has_image(n)) {
            (x, y) = (0, 0);
        }
        let (x1, y1) = (r.right().ceil() as i64 + 2, r.bottom().ceil() as i64 + 2);
        let Some((x, y, w, h)) = self.clamp_rect(x, y, x1 - x, y1 - y) else {
            return;
        };
        // A piece none of whose elements an edit touches is the plain
        // picture's, where it stands: served again when it was drawn.
        let mut key = fold(0xcbf29ce484222325, &self.base.to_le_bytes());
        for v in [x, y, w as i32, h as i32] {
            key = fold(key, &v.to_le_bytes());
        }
        let mut dirty = false;
        for (node, ts) in &pending {
            dirty |= signature(node, self.edited, &mut key);
            for v in [ts.sx, ts.ky, ts.kx, ts.sy, ts.tx, ts.ty] {
                key = fold(key, &v.to_bits().to_le_bytes());
            }
        }
        if !dirty {
            if let Some(pixels) = self.pieces.as_mut().and_then(|c| c.get(key)) {
                self.stats.rasters += 1;
                self.stats.raster_pixels += w as u64 * h as u64;
                list.items.push(Item::Raster(Raster {
                    x,
                    y,
                    width: w,
                    height: h,
                    rgba: pixels,
                }));
                return;
            }
        }
        let Some(mut pixmap) = tiny_skia::Pixmap::new(w, h) else {
            return;
        };
        let shift = Transform::from_translate(-(x as f32), -(y as f32));
        for (node, ts) in &pending {
            let at = shift.pre_concat(*ts);
            match node.abs_layer_bounding_box() {
                Some(b) => {
                    resvg::render_node(node, exact_pre(at, b.x(), b.y()), &mut pixmap.as_mut());
                }
                // A flat path resvg's render_node will not take (a
                // horizontal line): only its stroke draws, as resvg draws it.
                None => {
                    if let usvg::Node::Path(p) = node {
                        cpu_stroke(p, at, &mut pixmap);
                    }
                }
            }
        }
        self.stats.rasters += 1;
        self.stats.raster_pixels += w as u64 * h as u64;
        let pixels = std::sync::Arc::new(pixmap.take());
        if let Some(cache) = self.pieces.as_mut() {
            cache.misses += 1;
            if !dirty {
                cache.put(key, pixels.clone());
            }
        }
        list.items.push(Item::Raster(Raster {
            x,
            y,
            width: w,
            height: h,
            rgba: pixels,
        }));
    }

    /// A path as the GPU's shapes: its fill and its stroke outline, in
    /// paint order. `Ok(None)` when it draws nothing; `Err` when the GPU
    /// cannot draw it as resvg does.
    fn path(
        &mut self,
        p: &usvg::Path,
        ts: Transform,
        opacity: f32,
    ) -> Result<Option<Item>, &'static str> {
        if !p.is_visible() {
            return Ok(None);
        }
        if !p.rendering_mode().use_shape_antialiasing() {
            return Err("crisp edges (no antialiasing)");
        }
        if !ts.is_finite() {
            return Err("a transform that is not finite");
        }
        let mut shapes: Vec<Shape> = Vec::new();
        if let Some(fill) = p.fill() {
            let b = p.data().bounds();
            if b.width() != 0.0 && b.height() != 0.0 {
                if let Some(paint) = paint(fill.paint(), fill.opacity(), ts, 1.0)? {
                    if let Some(segs) = device(p.data(), ts) {
                        shapes.push(Shape {
                            segs,
                            even_odd: fill.rule() == usvg::FillRule::EvenOdd,
                            paint,
                        });
                    }
                }
            }
        }
        if let Some(stroke) = p.stroke() {
            let mut s = stroke.to_tiny_skia();
            if s.width >= 0.0 {
                let res_scale = tiny_skia::PathStroker::compute_resolution_scale(&ts);
                // A stroke no wider than a pixel: tiny-skia draws a
                // one-pixel line with its alpha scaled by the width. The
                // GPU strokes the same one-pixel line, scaled the same.
                let mut alpha = 1.0;
                if let Some(coverage) = hairline(s.width, ts) {
                    if coverage != 1.0 {
                        let scale = (coverage * 256.0) as i32;
                        alpha = ((255 * scale) >> 8) as f32 / 255.0;
                    }
                    s.width = 1.0 / device_scale(ts).max(1e-6);
                }
                let dashed;
                let data = match s.dash.take() {
                    Some(dash) => {
                        dashed = p.data().dash(&dash, res_scale);
                        dashed.as_ref()
                    }
                    None => Some(p.data()),
                };
                if let Some(outline) = data.and_then(|d| d.stroke(&s, res_scale)) {
                    if let Some(paint) = paint(stroke.paint(), stroke.opacity(), ts, alpha)? {
                        if let Some(segs) = device(&outline, ts) {
                            shapes.push(Shape {
                                segs,
                                even_odd: false,
                                paint,
                            });
                        }
                    }
                }
            }
        }
        if shapes.is_empty() {
            return Ok(None);
        }
        if p.paint_order() == usvg::PaintOrder::StrokeAndFill {
            shapes.reverse();
        }
        self.stats.shapes += shapes.len();
        self.stats.segments += shapes.iter().map(|s| s.segs.len()).sum::<usize>();
        Ok(Some(Item::Draw { shapes, opacity }))
    }
}

/// A group whose only drawn content is one path through single-child
/// groups that add nothing but a transform or an opacity: the path, its
/// transform and the opacity over it.
fn single_path(
    g: &usvg::Group,
    ts: Transform,
    opacity: f32,
) -> Option<(&usvg::Path, Transform, f32)> {
    let [child] = g.children() else {
        return None;
    };
    match child {
        usvg::Node::Path(p) => Some((p, ts, opacity)),
        usvg::Node::Group(inner) => {
            let plain = inner.clip_path().is_none()
                && inner.mask().is_none()
                && inner.filters().is_empty()
                && !inner.isolate()
                && inner.blend_mode() == usvg::BlendMode::Normal;
            plain
                .then(|| {
                    single_path(
                        inner,
                        ts.pre_concat(inner.transform()),
                        opacity * inner.opacity().get(),
                    )
                })
                .flatten()
        }
        _ => None,
    }
}

fn fast_len(p: Point) -> f32 {
    let mut x = p.x.abs();
    let mut y = p.y.abs();
    if x < y {
        std::mem::swap(&mut x, &mut y);
    }
    x + y / 2.0
}

/// tiny-skia's hairline test: a stroke no wider than a pixel either way
/// draws as a one-pixel line at this coverage.
fn hairline(width: f32, mut ts: Transform) -> Option<f32> {
    if width == 0.0 {
        return Some(1.0);
    }
    ts.tx = 0.0;
    ts.ty = 0.0;
    let mut points = [Point::from_xy(width, 0.0), Point::from_xy(0.0, width)];
    ts.map_points(&mut points);
    let (len0, len1) = (fast_len(points[0]), fast_len(points[1]));
    (len0 <= 1.0 && len1 <= 1.0).then_some((len0 + len1) / 2.0)
}

/// Device pixels per unit, averaged over the two axes.
fn device_scale(mut ts: Transform) -> f32 {
    ts.tx = 0.0;
    ts.ty = 0.0;
    let mut points = [Point::from_xy(1.0, 0.0), Point::from_xy(0.0, 1.0)];
    ts.map_points(&mut points);
    (fast_len(points[0]) + fast_len(points[1])) / 2.0
}

/// `at` pre-translated by `(bx, by)` so that resvg's render_node, which
/// pre-translates by `(-bx, -by)` again, draws with exactly `at`: a float
/// round trip a hair off would move a picture's nearest-neighbour sample
/// boundary across a pixel centre.
fn exact_pre(at: Transform, bx: f32, by: f32) -> Transform {
    let mut t = at.pre_translate(bx, by);
    for _ in 0..64 {
        let r = t.pre_translate(-bx, -by);
        if r.tx == at.tx && r.ty == at.ty {
            return t;
        }
        if r.tx != at.tx {
            t.tx = if r.tx < at.tx {
                t.tx.next_up()
            } else {
                t.tx.next_down()
            };
        }
        if r.ty != at.ty {
            t.ty = if r.ty < at.ty {
                t.ty.next_up()
            } else {
                t.ty.next_down()
            };
        }
    }
    at.pre_translate(bx, by)
}

/// A path moved into device pixels as tiny-skia's fill does, or `None`
/// where tiny-skia's fill draws nothing (flat, or too big for its math).
fn device(path: &Path, ts: Transform) -> Option<Vec<Seg>> {
    let moved = path.clone().transform(ts)?;
    let b = moved.bounds();
    if nearly_zero(b.width()) || nearly_zero(b.height()) {
        return None;
    }
    const MAX: f32 = f32::MAX * 0.25;
    if !(b.left() >= -MAX && b.top() >= -MAX && b.right() <= MAX && b.bottom() <= MAX) {
        return None;
    }
    let p = |pt: Point| [pt.x, pt.y];
    Some(
        moved
            .segments()
            .map(|s| match s {
                PathSegment::MoveTo(a) => Seg::Move(p(a)),
                PathSegment::LineTo(a) => Seg::Line(p(a)),
                PathSegment::QuadTo(a, b) => Seg::Quad(p(a), p(b)),
                PathSegment::CubicTo(a, b, c) => Seg::Cubic(p(a), p(b), p(c)),
                PathSegment::Close => Seg::Close,
            })
            .collect(),
    )
}

/// Whether a node draws a raster picture somewhere inside.
fn has_image(node: &usvg::Node) -> bool {
    match node {
        usvg::Node::Image(_) => true,
        usvg::Node::Group(g) => g.children().iter().any(has_image),
        usvg::Node::Text(t) => t.flattened().children().iter().any(has_image),
        usvg::Node::Path(_) => false,
    }
}

/// The pixels a node may draw on, in the transform's space.
fn extent(node: &usvg::Node, ts: Transform) -> Option<tiny_skia::Rect> {
    match node {
        usvg::Node::Group(g) => g
            .layer_bounding_box()
            .transform(ts.pre_concat(g.transform()))
            .map(|r| r.to_rect()),
        usvg::Node::Text(t) => {
            let f = t.flattened();
            f.layer_bounding_box()
                .transform(ts.pre_concat(f.transform()))
                .map(|r| r.to_rect())
        }
        other => other.stroke_bounding_box().transform(ts),
    }
}

/// resvg's stroke of a path, drawn with tiny-skia as resvg's own
/// path::render does — for the flat paths its render_node declines.
fn cpu_stroke(p: &usvg::Path, ts: Transform, pixmap: &mut tiny_skia::Pixmap) {
    let Some(stroke) = p.stroke() else { return };
    if !p.is_visible() {
        return;
    }
    let mut paint = tiny_skia::Paint::default();
    match stroke.paint() {
        usvg::Paint::Color(c) => {
            paint.set_color_rgba8(c.red, c.green, c.blue, stroke.opacity().to_u8());
        }
        usvg::Paint::LinearGradient(lg) => {
            let Some(shader) = ts_linear(lg, stroke.opacity()) else {
                return;
            };
            paint.shader = shader;
        }
        usvg::Paint::RadialGradient(rg) => {
            let Some(shader) = ts_radial(rg, stroke.opacity()) else {
                return;
            };
            paint.shader = shader;
        }
        usvg::Paint::Pattern(_) => return,
    }
    paint.anti_alias = p.rendering_mode().use_shape_antialiasing();
    pixmap.stroke_path(p.data(), &paint, &stroke.to_tiny_skia(), ts, None);
}

fn ts_stops(g: &usvg::BaseGradient, opacity: usvg::Opacity) -> Vec<tiny_skia::GradientStop> {
    g.stops()
        .iter()
        .map(|s| {
            let alpha = s.opacity() * opacity;
            tiny_skia::GradientStop::new(
                s.offset().get(),
                tiny_skia::Color::from_rgba8(
                    s.color().red,
                    s.color().green,
                    s.color().blue,
                    alpha.to_u8(),
                ),
            )
        })
        .collect()
}

fn ts_spread(g: &usvg::BaseGradient) -> tiny_skia::SpreadMode {
    match g.spread_method() {
        usvg::SpreadMethod::Pad => tiny_skia::SpreadMode::Pad,
        usvg::SpreadMethod::Reflect => tiny_skia::SpreadMode::Reflect,
        usvg::SpreadMethod::Repeat => tiny_skia::SpreadMode::Repeat,
    }
}

fn ts_linear(
    g: &usvg::LinearGradient,
    opacity: usvg::Opacity,
) -> Option<tiny_skia::Shader<'static>> {
    tiny_skia::LinearGradient::new(
        (g.x1(), g.y1()).into(),
        (g.x2(), g.y2()).into(),
        ts_stops(g, opacity),
        ts_spread(g),
        g.transform(),
    )
}

fn ts_radial(
    g: &usvg::RadialGradient,
    opacity: usvg::Opacity,
) -> Option<tiny_skia::Shader<'static>> {
    tiny_skia::RadialGradient::new(
        (g.fx(), g.fy()).into(),
        g.fr().get(),
        (g.cx(), g.cy()).into(),
        g.r().get(),
        ts_stops(g, opacity),
        ts_spread(g),
        g.transform(),
    )
}

/// A clip path's shapes in device pixels, as resvg's clip draws them.
fn clip_shapes(clip: &usvg::ClipPath, ts: Transform) -> Result<Vec<ClipShape>, &'static str> {
    if clip.clip_path().is_some() {
        return Err("a clip path that is clipped itself");
    }
    let mut out = Vec::new();
    fn walk(
        group: &usvg::Group,
        ts: Transform,
        out: &mut Vec<ClipShape>,
    ) -> Result<(), &'static str> {
        for child in group.children() {
            match child {
                usvg::Node::Path(p) => {
                    if !p.is_visible() {
                        continue;
                    }
                    let Some(fill) = p.fill() else { continue };
                    let b = p.data().bounds();
                    if b.width() == 0.0 || b.height() == 0.0 {
                        continue;
                    }
                    if !p.rendering_mode().use_shape_antialiasing() {
                        return Err("a clip with crisp edges");
                    }
                    if let Some(segs) = device(p.data(), ts) {
                        out.push(ClipShape {
                            segs,
                            even_odd: fill.rule() == usvg::FillRule::EvenOdd,
                        });
                    }
                }
                // resvg draws a clip's text from its glyphs as they stand.
                usvg::Node::Text(t) => walk(t.flattened(), ts, out)?,
                usvg::Node::Group(g) => {
                    if g.clip_path().is_some() {
                        return Err("a clipped group inside a clip path");
                    }
                    walk(g, ts.pre_concat(g.transform()), out)?;
                }
                usvg::Node::Image(_) => {}
            }
        }
        Ok(())
    }
    walk(clip.root(), ts.pre_concat(clip.transform()), &mut out)?;
    Ok(out)
}

/// One tiny-skia gradient stop: its position, clamped, and colour.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GStop {
    t: f32,
    c: [f32; 4],
}

fn color_of(c: usvg::Color, alpha: u8) -> [f32; 4] {
    [
        c.red as f32 / 255.0,
        c.green as f32 / 255.0,
        c.blue as f32 / 255.0,
        alpha as f32 / 255.0,
    ]
}

fn premultiplied(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// A usvg paint as the GPU's: `Ok(None)` where tiny-skia draws nothing
/// with it, `Err` for what the GPU path leaves to resvg.
///
/// `scale` multiplies every colour's alpha afterwards (tiny-skia's
/// `apply_opacity`, as its hairline does).
fn paint(
    paint: &usvg::Paint,
    opacity: usvg::Opacity,
    ts: Transform,
    scale: f32,
) -> Result<Option<Paint>, &'static str> {
    match paint {
        usvg::Paint::Color(c) => {
            let mut color = color_of(*c, opacity.to_u8());
            color[3] = (color[3] * scale.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            Ok(Some(Paint::Solid(premultiplied(color))))
        }
        usvg::Paint::LinearGradient(g) => Ok(linear(g, opacity, ts, scale)),
        usvg::Paint::RadialGradient(g) => radial(g, opacity, ts, scale),
        usvg::Paint::Pattern(_) => Err("a pattern paint"),
    }
}

fn stops_of(g: &usvg::BaseGradient, opacity: usvg::Opacity, scale: f32) -> Vec<GStop> {
    g.stops()
        .iter()
        .map(|s| {
            let alpha = s.opacity() * opacity;
            let o = s.offset().get();
            let mut c = color_of(s.color(), alpha.to_u8());
            c[3] = (c[3] * scale.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            GStop {
                t: if o.is_finite() {
                    o.clamp(0.0, 1.0)
                } else {
                    0.0
                },
                c,
            }
        })
        .collect()
}

fn spread_of(g: &usvg::BaseGradient) -> Spread {
    match g.spread_method() {
        usvg::SpreadMethod::Pad => Spread::Pad,
        usvg::SpreadMethod::Reflect => Spread::Reflect,
        usvg::SpreadMethod::Repeat => Spread::Repeat,
    }
}

/// tiny-skia's `LinearGradient::new`, then the shader moved by `ts`.
fn linear(
    g: &usvg::LinearGradient,
    opacity: usvg::Opacity,
    ts: Transform,
    scale: f32,
) -> Option<Paint> {
    let stops = stops_of(g, opacity, scale);
    let spread = spread_of(g);
    match stops.as_slice() {
        [] => return None,
        [one] => return Some(Paint::Solid(premultiplied(one.c))),
        _ => {}
    }
    let start = Point::from_xy(g.x1(), g.y1());
    let end = Point::from_xy(g.x2(), g.y2());
    let length = (end - start).length();
    if !length.is_finite() {
        return None;
    }
    if length.abs() <= DEGENERATE_THRESHOLD {
        return Some(Paint::Solid(premultiplied(match spread {
            Spread::Pad => stops.last().unwrap().c,
            _ => average(&stops),
        })));
    }
    g.transform().invert()?;
    let unit = points_to_unit(start, end);
    gradient(
        stops,
        spread,
        g.transform(),
        unit,
        GradientShape::Linear,
        ts,
    )
}

fn points_to_unit(start: Point, end: Point) -> Transform {
    let mut vec = end - start;
    let mag = vec.length();
    let inv = if mag != 0.0 { 1.0 / mag } else { 0.0 };
    vec.scale(inv);
    let mut ts = sin_cos_at(-vec.y, vec.x, start.x, start.y);
    ts = ts.post_translate(-start.x, -start.y);
    ts.post_scale(inv, inv)
}

fn sin_cos_at(sin: f32, cos: f32, px: f32, py: f32) -> Transform {
    let cos_inv = 1.0 - cos;
    Transform::from_row(
        cos,
        sin,
        -sin,
        cos,
        sin * py + cos_inv * px,
        -sin * px + cos_inv * py,
    )
}

/// The average colour of a gradient (tiny-skia's degenerate repeat).
fn average(points: &[GStop]) -> [f32; 4] {
    let mut blend = [0.0f32; 4];
    for i in 0..points.len() - 1 {
        let (c0, c1) = (points[i].c, points[i + 1].c);
        let w = points[i + 1].t - points[i].t;
        for (k, b) in blend.iter_mut().enumerate() {
            *b += 0.5 * w * (c1[k] + c0[k]);
        }
    }
    if points[0].t > 0.0 {
        for (b, c) in blend.iter_mut().zip(points[0].c) {
            *b += points[0].t * c;
        }
    }
    let last = points[points.len() - 1];
    if last.t < 1.0 {
        for (b, c) in blend.iter_mut().zip(last.c) {
            *b += (1.0 - last.t) * c;
        }
    }
    blend.map(|v| v.clamp(0.0, 1.0))
}

/// tiny-skia's `RadialGradient::new` and `create`, for the shapes the GPU
/// evaluates: plain and concentric circles, and a focus well inside.
fn radial(
    g: &usvg::RadialGradient,
    opacity: usvg::Opacity,
    ts: Transform,
    scale: f32,
) -> Result<Option<Paint>, &'static str> {
    let (r0, r1) = (g.fr().get(), g.r().get());
    if r0 < 0.0 || r1 < 0.0 {
        return Ok(None);
    }
    let mut stops = stops_of(g, opacity, scale);
    let spread = spread_of(g);
    match stops.as_slice() {
        [] => return Ok(None),
        [one] => return Ok(Some(Paint::Solid(premultiplied(one.c)))),
        _ => {}
    }
    if g.transform().invert().is_none() {
        return Ok(None);
    }
    let c0 = Point::from_xy(g.fx(), g.fy());
    let c1 = Point::from_xy(g.cx(), g.cy());
    let length = (c0 - c1).length();
    if !length.is_finite() {
        return Ok(None);
    }
    let simple = |center: Point, radius: f32, stops: Vec<GStop>| {
        let inv = 1.0 / radius;
        let unit = Transform::from_translate(-center.x, -center.y).post_scale(inv, inv);
        gradient(
            stops,
            spread,
            g.transform(),
            unit,
            GradientShape::Radial { scale_bias: None },
            ts,
        )
    };
    if length.abs() <= DEGENERATE_THRESHOLD {
        if (r0 - r1).abs() <= DEGENERATE_THRESHOLD {
            if spread == Spread::Pad && r1 > DEGENERATE_THRESHOLD {
                let (first, last) = (stops[0].c, stops[stops.len() - 1].c);
                stops = vec![
                    GStop { t: 0.0, c: first },
                    GStop { t: 1.0, c: first },
                    GStop { t: 1.0, c: last },
                ];
                return Ok(simple(c0, r1, stops));
            }
            return Ok(None);
        }
        if r0.abs() <= DEGENERATE_THRESHOLD {
            return Ok(simple(c0, r1, stops));
        }
    }
    // create()
    if nearly_zero((c0 - c1).length()) {
        if nearly_zero(r0.max(r1)) || (r0 - r1).abs() <= SCALAR_NEARLY_ZERO {
            return Ok(None);
        }
        let scale = 1.0 / r0.max(r1);
        let unit = Transform::from_translate(-c1.x, -c1.y).post_scale(scale, scale);
        let scale_bias = if r0 == 0.0 {
            None
        } else {
            let d = r1 - r0;
            Some([r0.max(r1) / d, -r0 / d])
        };
        return Ok(gradient(
            stops,
            spread,
            g.transform(),
            unit,
            GradientShape::Radial { scale_bias },
            ts,
        ));
    }
    let Some(mut matrix) = poly_to_poly(c0, c1, Point::from_xy(0.0, 0.0), Point::from_xy(1.0, 0.0))
    else {
        return Ok(None);
    };
    let d_center = (c0 - c1).length();
    if nearly_zero(r0 - r1) {
        return Err("a radial gradient between two equal circles");
    }
    // FocalData::set
    let (mut fr0, mut fr1) = (r0 / d_center, r1 / d_center);
    let mut is_swapped = false;
    let mut focal_x = fr0 / (fr0 - fr1);
    if nearly_zero(focal_x - 1.0) {
        matrix = matrix.post_translate(-1.0, 0.0).post_scale(-1.0, 1.0);
        std::mem::swap(&mut fr0, &mut fr1);
        focal_x = 0.0;
        is_swapped = true;
    }
    let Some(focal_matrix) = poly_to_poly(
        Point::from_xy(focal_x, 0.0),
        Point::from_xy(1.0, 0.0),
        Point::from_xy(0.0, 0.0),
        Point::from_xy(1.0, 0.0),
    ) else {
        return Ok(None);
    };
    matrix = matrix.post_concat(focal_matrix);
    let r1f = fr1 / (1.0 - focal_x).abs();
    let on_circle = nearly_zero(1.0 - r1f);
    let well_behaved = !on_circle && r1f > 1.0;
    if !well_behaved {
        return Err("a radial gradient whose focus is on or outside its circle");
    }
    matrix = matrix.post_scale(
        r1f / (r1f * r1f - 1.0),
        1.0 / (r1f * r1f - 1.0).abs().sqrt(),
    );
    matrix = matrix.post_scale((1.0 - focal_x).abs(), (1.0 - focal_x).abs());
    let shape = GradientShape::Focal {
        p0: 1.0 / r1f,
        p1: focal_x,
        negate: (1.0 - focal_x) < 0.0,
        compensate: !nearly_zero(focal_x),
        unswap: is_swapped,
    };
    Ok(gradient(stops, spread, g.transform(), matrix, shape, ts))
}

fn poly2(p0: Point, p1: Point) -> Transform {
    Transform::from_row(
        p1.y - p0.y,
        p0.x - p1.x,
        p1.x - p0.x,
        p1.y - p0.y,
        p0.x,
        p0.y,
    )
}

fn poly_to_poly(src1: Point, src2: Point, dst1: Point, dst2: Point) -> Option<Transform> {
    let res = poly2(src1, src2).invert()?;
    Some(poly2(dst1, dst2).pre_concat(res))
}

/// tiny-skia's `Gradient::new` and `push_stages`: the stops bracketed by
/// 0 and 1 and made monotonic, then the table of factors and biases, with
/// the pixel → unit matrix of the shader moved by `ts`.
fn gradient(
    mut stops: Vec<GStop>,
    spread: Spread,
    transform: Transform,
    points_to_unit: Transform,
    shape: GradientShape,
    ts: Transform,
) -> Option<Paint> {
    let dummy_first = stops[0].t != 0.0;
    let dummy_last = stops[stops.len() - 1].t != 1.0;
    if dummy_first {
        stops.insert(
            0,
            GStop {
                t: 0.0,
                c: stops[0].c,
            },
        );
    }
    if dummy_last {
        stops.push(GStop {
            t: 1.0,
            c: stops[stops.len() - 1].c,
        });
    }
    let opaque = stops.iter().all(|s| s.c[3] == 1.0);
    let start_index = if dummy_first { 0 } else { 1 };
    let mut prev = 0.0f32;
    let mut uniform = true;
    let uniform_step = stops[start_index].t - prev;
    for i in start_index..stops.len() {
        let curr = if i + 1 == stops.len() {
            1.0
        } else {
            bound(stops[i].t, prev, 1.0)
        };
        uniform &= (uniform_step - (curr - prev)).abs() <= SCALAR_NEARLY_ZERO;
        stops[i].t = curr.clamp(0.0, 1.0);
        prev = curr;
    }
    // The shader's transform, moved by the path's, inverted.
    let moved = transform.post_concat(ts);
    let to_unit = moved.invert()?.post_concat(points_to_unit);
    let mut table: Vec<Stop> = Vec::new();
    if stops.len() == 2 {
        let (c0, c1) = (stops[0].c, stops[1].c);
        table.push(Stop {
            t: 0.0,
            factor: [c1[0] - c0[0], c1[1] - c0[1], c1[2] - c0[2], c1[3] - c0[3]],
            bias: c0,
        });
    } else {
        let len = stops.len();
        let (first, last) = if len > 2 {
            (
                if stops[0].c != stops[1].c { 0 } else { 1 },
                if stops[len - 2].c != stops[len - 1].c {
                    len - 1
                } else {
                    len - 2
                },
            )
        } else {
            (0, 1)
        };
        let mut t_l = stops[first].t;
        let mut c_l = stops[first].c;
        table.push(Stop {
            t: 0.0,
            factor: [0.0; 4],
            bias: c_l,
        });
        for i in first..last {
            let t_r = stops[i + 1].t;
            let c_r = stops[i + 1].c;
            if t_l < t_r {
                let f = [
                    (c_r[0] - c_l[0]) / (t_r - t_l),
                    (c_r[1] - c_l[1]) / (t_r - t_l),
                    (c_r[2] - c_l[2]) / (t_r - t_l),
                    (c_r[3] - c_l[3]) / (t_r - t_l),
                ];
                table.push(Stop {
                    t: t_l.clamp(0.0, 1.0),
                    factor: f,
                    bias: [
                        c_l[0] - f[0] * t_l,
                        c_l[1] - f[1] * t_l,
                        c_l[2] - f[2] * t_l,
                        c_l[3] - f[3] * t_l,
                    ],
                });
            }
            t_l = t_r;
            c_l = c_r;
        }
        table.push(Stop {
            t: t_l.clamp(0.0, 1.0),
            factor: [0.0; 4],
            bias: c_l,
        });
    }
    Some(Paint::Gradient(Gradient {
        shape,
        to_unit: [
            to_unit.sx, to_unit.ky, to_unit.kx, to_unit.sy, to_unit.tx, to_unit.ty,
        ],
        spread,
        clamp: spread == Spread::Pad && uniform,
        stops: table,
        premultiply: !opaque,
    }))
}
