//! Lossless conversion (SVG-PLAN.md slice 5): an SVG's plain shapes as a
//! native drawing — "Make editable shapes" for tier-C elements, and the
//! drawing editor's Import SVG… for a whole file.
//!
//! A drawing is shapes and nothing else: a pen (a polyline), a line, an
//! oval, a rect with a corner radius, each with one solid fill and one
//! solid stroke, drawn fill then stroke, strokes always with round caps
//! and joins. So a conversion is offered only when that can draw what the
//! file draws, and it is proved, not assumed: the core draws the selection
//! both ways — resvg on the file, the drawing rasterizer (promo-gpu's
//! `VectorRenderer`, the one the engine draws drawings with) on the shapes
//! — and refuses when the two differ by more than antialiasing
//! ([`crate::likeness`]). A refusal names its reason: what the drawing
//! cannot hold ("a miter join", "a gradient fill", "a dashed stroke", …),
//! or where the pictures part.
//!
//! Geometry is the file's as usvg resolved it (transforms, units, arcs),
//! in the picture's CSS pixels: an axis-aligned rect, circle or ellipse
//! becomes its own kind (a rounded rect keeps its radius), a line a line,
//! anything else a pen whose curves are cut finely (to 1/20 000 of the
//! picture's longer side, so a 24-px icon zoomed to a 4K frame stays
//! smooth). A fill with several sub-paths (a letter's counter) is one pen
//! joined by zero-width bridges, keeping its fill rule; its stroke is a pen
//! per sub-path.

use promo_model::{DrawingDocument, DrawingShape, DrawingShapeKind, Point};
use tiny_skia::{Path, PathSegment, Transform};

use crate::model::{address_of, Tier};
use crate::{Picture, VectorOverride};

/// What a conversion made.
#[derive(Debug, Clone)]
pub struct Converted {
    /// The shapes, in paint order. With `frame`, the first shape is an
    /// invisible rect over the whole picture, so the drawing lays out
    /// exactly where the picture does.
    pub drawing: DrawingDocument,
    /// The elements converted (the selection, resolved to addresses).
    pub nodes: Vec<String>,
}

/// Why a conversion was refused, in the words the editor shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub reason: String,
    /// The element it is about, when one is to blame.
    pub node: Option<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self.node {
            Some(n) => write!(f, "\"{n}\" cannot become a drawing shape: {}", self.reason),
            None => write!(f, "{}", self.reason),
        }
    }
}

fn refused(node: Option<&str>, reason: impl Into<String>) -> Refused {
    Refused {
        reason: reason.into(),
        node: node.map(str::to_string),
    }
}

const TEXT: &str = "text — its words would become shapes no one can retype";
const PICTURE: &str = "a picture inside the picture";

/// The longest side the check draws at.
const CHECK_SIDE: f64 = 1024.0;

impl Picture {
    /// The elements `nodes` names (addresses or the author's names; a group
    /// takes everything drawn in it) — or, with `None`, the whole file — as
    /// a drawing, drawn with `overrides` (the resource's look). `frame`
    /// adds the invisible rect that keeps the picture's layout. Checked by
    /// drawing both ways; refused with a reason.
    pub fn convert(
        &self,
        overrides: &[VectorOverride],
        nodes: Option<&[String]>,
        frame: bool,
    ) -> Result<Converted, Refused> {
        let (model, tree) = self.named_tree(overrides).map_err(|e| refused(None, e))?;
        let selection: Option<Vec<String>> = match nodes {
            None => None,
            Some(list) => {
                if list.is_empty() {
                    return Err(refused(None, "nothing is selected"));
                }
                let mut out = Vec::new();
                for name in list {
                    let address = model
                        .resolve(name)
                        .map_err(|e| refused(Some(name.as_str()), e))?;
                    let node = model.node(address).expect("a resolved address");
                    let why = match (node.kind, node.tier) {
                        ("text", _) => Some(TEXT.to_string()),
                        ("image", _) => Some(PICTURE.to_string()),
                        (_, Tier::Locked) => Some(node.reason.clone()),
                        ("shape", t) if t != Tier::C => Some(node.reason.clone()),
                        ("use", _) => Some("a <use> copy — its original is kept as drawn".into()),
                        _ => None,
                    };
                    if let Some(why) = why {
                        return Err(refused(Some(address), why));
                    }
                    out.push(address.to_string());
                }
                Some(out)
            }
        };
        let (w, h) = self.size();
        if !(w > 0.0 && h > 0.0) {
            return Err(refused(None, "the picture has no size"));
        }
        // A whole file: text draws only from outlines a picture parse has
        // not got, so it would vanish rather than be drawn — refused.
        if selection.is_none() {
            if let Some(t) = model
                .nodes()
                .iter()
                .find(|n| n.kind == "text" && !in_definition(&model, n))
            {
                return Err(refused(Some(&t.node), TEXT));
            }
        }
        let tolerance = (w.max(h) / 20_000.0) as f32;
        let mut walker = Walker {
            model: &model,
            selection: selection.as_deref(),
            tolerance,
            shapes: Vec::new(),
            owners: Vec::new(),
            leaves: Vec::new(),
            stamp: self.stamp(),
        };
        walker.group(tree.root(), Transform::identity(), 1.0, &[])?;
        // Above the picture, the selection would cover what the file draws
        // over it: an element drawn later that overlaps it refuses.
        if let Some(sel) = &selection {
            if let Some((under, over)) = covered(tree.root(), &model, sel) {
                return Err(refused(
                    Some(&under),
                    format!(
                        "\"{over}\" is drawn over it, and the shapes would sit above the whole \
                         picture — convert \"{over}\" with it"
                    ),
                ));
            }
        }
        // The file cuts off what lies outside the picture; a drawing would
        // not.
        let outside = walker.shapes.iter().zip(&walker.owners).find(|(sh, _)| {
            let pad = sh.stroke_width / 2.0 + 0.5;
            sh.points
                .iter()
                .any(|p| p.0 < -pad || p.1 < -pad || p.0 > w + pad || p.1 > h + pad)
        });
        if let Some((_, owner)) = outside {
            return Err(refused(
                Some(owner),
                "it reaches outside the picture, where the file cuts it off",
            ));
        }
        if walker.shapes.is_empty() {
            return Err(refused(
                None,
                match &selection {
                    Some(_) => "the selection draws nothing",
                    None => "the file draws nothing a drawing could hold",
                },
            ));
        }
        // The check: the same pixels, but for antialiasing. What resvg draws
        // is the picture with every drawn element outside the selection
        // hidden.
        let mut reference = overrides.to_vec();
        if selection.is_some() {
            for n in model.nodes() {
                let leaf = matches!(n.kind, "shape" | "image" | "text" | "use");
                if leaf && n.drawn && n.tier != Tier::Locked && !walker.leaves.contains(&n.node) {
                    reference.push(VectorOverride {
                        node: n.node.clone(),
                        visible: Some(false),
                        ..Default::default()
                    });
                }
            }
        }
        let alone = if reference.len() == overrides.len() {
            self.with_overrides(overrides)
        } else {
            self.with_overrides(&reference)
        }
        .map_err(|e| refused(None, e))?;
        let scale = (CHECK_SIDE / w.max(h)).clamp(1.0, 16.0);
        let pw = (w * scale).ceil().max(1.0) as u32;
        let scale = pw as f64 / w;
        let ph = (h * scale).ceil().max(1.0) as u32;
        let file = alone
            .render_uniform(scale as f32, pw, ph)
            .ok_or_else(|| refused(None, "the picture could not be drawn to check it"))?;
        let mut shapes = walker.shapes.clone();
        shapes.insert(0, frame_rect(w, h, self.stamp()));
        let drawn = draw_shapes(&shapes, pw, ph).map_err(|e| refused(None, e))?;
        let d = crate::likeness::compare(&drawn, &file, pw, ph);
        if let Some((x, y)) = d.worst() {
            // Name the element drawn where the pictures part most.
            let (px, py) = (x as f64 / scale, y as f64 / scale);
            let blamed = walker
                .shapes
                .iter()
                .zip(&walker.owners)
                .rev()
                .find(|(s, _)| contains(s, px, py, 4.0 / scale))
                .map(|(_, o)| o.clone());
            return Err(Refused {
                reason: format!(
                    "the shapes would not draw as the file does there (differing by {} levels, \
                     beyond antialiasing) — keep it as an SVG picture",
                    d.interior.max(d.blocks.round() as u8)
                ),
                node: blamed,
            });
        }
        let mut out = walker.shapes;
        if frame {
            out.insert(0, frame_rect(w, h, self.stamp()));
        }
        Ok(Converted {
            drawing: DrawingDocument {
                shapes: out,
                background_color_hex: None,
            },
            nodes: selection.unwrap_or_default(),
        })
    }

    /// resvg's picture at a uniform `scale`, into `width`×`height`.
    fn render_uniform(&self, scale: f32, width: u32, height: u32) -> Option<Vec<u8>> {
        let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
        resvg::render(
            self.tree(),
            Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        Some(pixmap.take())
    }
}

/// A selected element and an unselected one drawn after it over it, if
/// any — in paint order, by their boxes on the picture.
fn covered(
    root: &usvg::Group,
    model: &crate::Model,
    selection: &[String],
) -> Option<(String, String)> {
    struct Leaf<'n> {
        owner: String,
        selected: bool,
        bbox: tiny_skia::Rect,
        node: &'n usvg::Node,
    }
    fn walk<'n>(
        g: &'n usvg::Group,
        model: &crate::Model,
        selection: &[String],
        chain: &[String],
        out: &mut Vec<Leaf<'n>>,
    ) {
        for child in g.children() {
            let mut chain = chain.to_vec();
            if let Some(a) = address_of(child.id()).filter(|a| model.node(a).is_some()) {
                chain.push(a.to_string());
            }
            match child {
                usvg::Node::Group(inner) => {
                    if !inner.id().starts_with(crate::model::PROBE) {
                        walk(inner, model, selection, &chain, out);
                    }
                }
                leaf => {
                    let b = leaf.abs_stroke_bounding_box();
                    if b.width() > 0.0 || b.height() > 0.0 {
                        out.push(Leaf {
                            owner: chain.last().cloned().unwrap_or_default(),
                            selected: chain.iter().any(|a| selection.contains(a)),
                            bbox: b,
                            node: leaf,
                        });
                    }
                }
            }
        }
    }
    let mut leaves = Vec::new();
    walk(root, model, selection, &[], &mut leaves);
    for (i, s) in leaves.iter().enumerate().filter(|(_, l)| l.selected) {
        for later in leaves[i + 1..].iter().filter(|l| !l.selected) {
            let (a, b) = (s.bbox, later.bbox);
            let overlap = a.left() < b.right()
                && b.left() < a.right()
                && a.top() < b.bottom()
                && b.top() < a.bottom();
            if overlap && inks_overlap(s.node, later.node, a, b) {
                return Some((s.owner.clone(), later.owner.clone()));
            }
        }
    }
    None
}

/// Whether two drawn leaves' ink overlaps — a quarter of a pixel's
/// coverage in both, at four pixels a unit — not just their boxes:
/// abutting shapes do not overlap.
fn inks_overlap(a: &usvg::Node, b: &usvg::Node, ba: tiny_skia::Rect, bb: tiny_skia::Rect) -> bool {
    let (x0, y0) = (ba.left().max(bb.left()), ba.top().max(bb.top()));
    let (x1, y1) = (ba.right().min(bb.right()), ba.bottom().min(bb.bottom()));
    let k = (4.0f32).min(2048.0 / (x1 - x0).max(y1 - y0).max(1e-3));
    let (w, h) = (
        ((x1 - x0) * k).ceil().max(1.0) as u32,
        ((y1 - y0) * k).ceil().max(1.0) as u32,
    );
    let ink = |node: &usvg::Node| -> Option<Vec<u8>> {
        let lb = node.abs_layer_bounding_box()?;
        let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
        let m = Transform::from_translate(-x0 * k, -y0 * k)
            .pre_scale(k, k)
            .pre_concat(node.abs_transform())
            .pre_translate(lb.x(), lb.y());
        resvg::render_node(node, m, &mut pixmap.as_mut());
        Some(pixmap.pixels().iter().map(|p| p.alpha()).collect())
    };
    let (Some(ia), Some(ib)) = (ink(a), ink(b)) else {
        // A flat leaf (a line with no area box): its box is its ink.
        return true;
    };
    ia.iter()
        .zip(&ib)
        .any(|(&p, &q)| (p as u32 * q as u32) / 255 > 96)
}

/// An invisible rect over the picture: the drawing's bounds are the
/// picture's.
fn frame_rect(w: f64, h: f64, stamp: u64) -> DrawingShape {
    DrawingShape {
        id: uuid_for(stamp, "frame", 0),
        kind: DrawingShapeKind::Rect,
        points: vec![Point(0.0, 0.0), Point(w, h)],
        stroke_color_hex: "000000".into(),
        stroke_width: 0.0,
        stroke_opacity: Some(0.0),
        fill_color_hex: None,
        fill_opacity: Some(0.0),
        arrow_start: false,
        arrow_end: false,
        even_odd_fill: None,
        corner_radius: None,
        group_id: None,
    }
}

/// A UUID-shaped id from the picture, the element and the shape's place:
/// the same conversion mints the same ids.
fn uuid_for(stamp: u64, address: &str, index: usize) -> String {
    let digest = crate::sha256_hex(format!("{stamp:x}\u{1f}{address}\u{1f}{index}").as_bytes());
    let h = digest.as_bytes();
    let s = |r: std::ops::Range<usize>| String::from_utf8_lossy(&h[r]).into_owned();
    // Version 4 / variant 1 bits, so every reader takes it as a UUID.
    format!(
        "{}-{}-4{}-a{}-{}",
        s(0..8),
        s(8..12),
        s(13..16),
        s(17..20),
        s(20..32)
    )
    .to_uppercase()
}

/// Whether `(x, y)` lies within `slop` of a shape's points' box.
fn contains(s: &DrawingShape, x: f64, y: f64, slop: f64) -> bool {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in &s.points {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    let pad = slop + s.stroke_width / 2.0;
    x >= x0 - pad && x <= x1 + pad && y >= y0 - pad && y <= y1 + pad
}

/// Draws drawing shapes with the engine's own drawing rasterizer into
/// premultiplied RGBA.
fn draw_shapes(shapes: &[DrawingShape], w: u32, h: u32) -> Result<Vec<u8>, String> {
    use promo_gpu::vector::{VectorRenderer, VectorShape, VectorShapeKind};
    use promo_gpu::wgpu;
    let ctx = promo_gpu::GpuContext::shared()
        .ok_or("no GPU to draw the shapes on, so the conversion cannot be checked")?;
    let rgba = |hex: &str| -> Option<[f32; 4]> {
        let v = hex.trim().trim_start_matches('#');
        if v.len() != 6 {
            return None;
        }
        let n = u32::from_str_radix(v, 16).ok()?;
        Some([
            ((n >> 16) & 0xFF) as f32 / 255.0,
            ((n >> 8) & 0xFF) as f32 / 255.0,
            (n & 0xFF) as f32 / 255.0,
            1.0,
        ])
    };
    // The engine's mapping (promo-engine `vector_shapes`, a palette aside).
    let list: Vec<VectorShape> = shapes
        .iter()
        .map(|s| {
            let sa = s.stroke_opacity.unwrap_or(1.0);
            let fa = s.fill_opacity.unwrap_or(1.0);
            VectorShape {
                kind: match s.kind {
                    DrawingShapeKind::Pen => VectorShapeKind::Pen,
                    DrawingShapeKind::Line => VectorShapeKind::Line,
                    DrawingShapeKind::Oval => VectorShapeKind::Oval,
                    DrawingShapeKind::Rect => VectorShapeKind::Rect,
                },
                points: s.points.iter().map(|p| (p.0, p.1)).collect(),
                stroke_rgba: rgba(&s.stroke_color_hex)
                    .map(|c| [c[0], c[1], c[2], sa])
                    .unwrap_or([1.0, 1.0, 1.0, sa]),
                stroke_width: s.stroke_width,
                fill_rgba: s
                    .fill_color_hex
                    .as_deref()
                    .and_then(rgba)
                    .map(|c| [c[0], c[1], c[2], fa]),
                arrow_start: s.arrow_start,
                arrow_end: s.arrow_end,
                even_odd_fill: s.even_odd_fill.unwrap_or(false),
                corner_radius: s.corner_radius.unwrap_or(0.0),
            }
        })
        .collect();
    let mut renderer = VectorRenderer::new(ctx).map_err(|e| e.to_string())?;
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("promo-svg-convert-check"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    renderer
        .render_to_texture(ctx, &list, &texture, w, h)
        .map_err(|e| e.to_string())?;
    let mut px =
        promo_gpu::vector::scene::read_texture(ctx, &texture, w, h).map_err(|e| e.to_string())?;
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Ok(px)
}

struct Walker<'m> {
    model: &'m crate::Model,
    selection: Option<&'m [String]>,
    tolerance: f32,
    shapes: Vec<DrawingShape>,
    /// The drawn element each shape came from, parallel to `shapes`.
    owners: Vec<String>,
    /// Every leaf element converted.
    leaves: Vec<String>,
    stamp: u64,
}

fn hex(c: usvg::Color) -> String {
    format!("{:02X}{:02X}{:02X}", c.red, c.green, c.blue)
}

impl Walker<'_> {
    fn selected(&self, chain: &[String]) -> bool {
        match self.selection {
            None => true,
            Some(sel) => chain.iter().any(|a| sel.contains(a)),
        }
    }

    /// The element a node is, when it is one the model lists.
    fn address(&self, id: &str) -> Option<String> {
        address_of(id)
            .filter(|a| self.model.node(a).is_some())
            .map(str::to_string)
    }

    fn group(
        &mut self,
        g: &usvg::Group,
        ts: Transform,
        opacity: f32,
        chain: &[String],
    ) -> Result<(), Refused> {
        for child in g.children() {
            let mut chain = chain.to_vec();
            if let Some(a) = self.address(child.id()) {
                chain.push(a);
            }
            let selected = self.selected(&chain);
            let owner = chain.last().cloned();
            match child {
                usvg::Node::Group(inner) => {
                    if inner.id().starts_with(crate::model::PROBE) {
                        continue;
                    }
                    if !selected && !self.holds_selection(inner) {
                        continue;
                    }
                    if selected {
                        let blame = owner.as_deref();
                        if inner.blend_mode() != usvg::BlendMode::Normal {
                            return Err(refused(blame, "a blend mode"));
                        }
                        if !inner.filters().is_empty() || inner.mask().is_some() {
                            return Err(refused(blame, "kept as drawn — under a filter or a mask"));
                        }
                        if inner.clip_path().is_some() {
                            return Err(refused(blame, "kept as drawn — clipped"));
                        }
                        if inner.opacity().get() < 1.0 && leaves(inner) > 1 {
                            return Err(refused(
                                blame,
                                "a group's opacity over several shapes — a drawing has no \
                                 groups to hold it",
                            ));
                        }
                    }
                    self.group(
                        inner,
                        ts.pre_concat(inner.transform()),
                        opacity * inner.opacity().get(),
                        &chain,
                    )?;
                }
                usvg::Node::Path(p) => {
                    if !selected {
                        continue;
                    }
                    let owner = owner.unwrap_or_default();
                    self.check_tier(&owner)?;
                    let made = self.path(p, ts, opacity, &owner)?;
                    if !made.is_empty() && !self.leaves.contains(&owner) {
                        self.leaves.push(owner.clone());
                    }
                    for s in made {
                        self.shapes.push(s);
                        self.owners.push(owner.clone());
                    }
                }
                usvg::Node::Image(_) => {
                    if selected {
                        return Err(refused(owner.as_deref(), PICTURE));
                    }
                }
                usvg::Node::Text(_) => {
                    if selected {
                        return Err(refused(owner.as_deref(), TEXT));
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether a group holds a selected element somewhere inside.
    fn holds_selection(&self, g: &usvg::Group) -> bool {
        let Some(sel) = self.selection else {
            return true;
        };
        g.children().iter().any(|c| {
            self.address(c.id()).is_some_and(|a| sel.contains(&a))
                || matches!(c, usvg::Node::Group(inner) if self.holds_selection(inner))
        })
    }

    fn check_tier(&self, owner: &str) -> Result<(), Refused> {
        let Some(node) = self.model.node(owner) else {
            return Err(refused(Some(owner), "it is not an element of the file"));
        };
        if node.kind == "text" {
            return Err(refused(Some(owner), TEXT));
        }
        if node.tier != Tier::C {
            return Err(refused(Some(owner), node.reason.clone()));
        }
        Ok(())
    }

    /// One path's drawing shapes.
    fn path(
        &mut self,
        p: &usvg::Path,
        ts: Transform,
        opacity: f32,
        owner: &str,
    ) -> Result<Vec<DrawingShape>, Refused> {
        let blame = Some(owner);
        if !p.is_visible() {
            return Ok(Vec::new());
        }
        if !p.rendering_mode().use_shape_antialiasing() {
            return Err(refused(blame, "crisp edges (no antialiasing)"));
        }
        let tag = self
            .model
            .node(owner)
            .map(|n| n.tag.clone())
            .unwrap_or_default();
        let data = p
            .data()
            .clone()
            .transform(ts)
            .ok_or_else(|| refused(blame, "a transform that cannot be applied"))?;
        let mut fill = None;
        if let Some(f) = p.fill() {
            let b = p.data().bounds();
            if b.width() != 0.0 && b.height() != 0.0 {
                let color = match f.paint() {
                    usvg::Paint::Color(c) => *c,
                    usvg::Paint::Pattern(_) => return Err(refused(blame, "a pattern fill")),
                    _ => return Err(refused(blame, "a gradient fill")),
                };
                fill = Some((
                    color,
                    f.opacity().get(),
                    f.rule() == usvg::FillRule::EvenOdd,
                ));
            }
        }
        let mut stroke = None;
        if let Some(s) = p.stroke() {
            let color = match s.paint() {
                usvg::Paint::Color(c) => *c,
                usvg::Paint::Pattern(_) => return Err(refused(blame, "a pattern stroke")),
                _ => return Err(refused(blame, "a gradient stroke")),
            };
            if s.dasharray().is_some() {
                return Err(refused(blame, "a dashed stroke"));
            }
            let width = uniform_scale(ts).ok_or_else(|| {
                refused(
                    blame,
                    "a stroke under a stretch or a skew — its width would vary",
                )
            })? * s.width().get();
            if has_corner(&data) {
                match s.linejoin() {
                    usvg::LineJoin::Round => {}
                    usvg::LineJoin::Bevel => return Err(refused(blame, "a bevel join")),
                    _ => return Err(refused(blame, "a miter join")),
                }
            }
            if has_open_end(&data) {
                match s.linecap() {
                    usvg::LineCap::Round => {}
                    usvg::LineCap::Butt => return Err(refused(blame, "a butt cap")),
                    usvg::LineCap::Square => return Err(refused(blame, "a square cap")),
                }
            }
            stroke = Some((color, s.opacity().get(), width as f64));
        }
        let subpaths = points_of(&data, self.tolerance);
        // A fill of lines only (an open path's default black) covers
        // nothing: no fill.
        if subpaths.iter().all(|sp| area(sp).abs() < 1e-9) {
            fill = None;
        }
        if fill.is_some() && stroke.is_some() && opacity < 1.0 {
            return Err(refused(
                blame,
                "an opacity over a fill and its stroke — a drawing fades each on its own",
            ));
        }
        let stroke_first = p.paint_order() == usvg::PaintOrder::StrokeAndFill;
        let both = fill.is_some() && stroke.is_some();
        let (stamp, base) = (self.stamp, self.shapes.len());
        let mut count = 0;
        let mut make = |kind: DrawingShapeKind, points: Vec<Point>, radius: Option<f64>| {
            count += 1;
            DrawingShape {
                id: uuid_for(stamp, owner, base + count),
                kind,
                points,
                stroke_color_hex: "000000".into(),
                stroke_width: 0.0,
                stroke_opacity: None,
                fill_color_hex: None,
                fill_opacity: None,
                arrow_start: false,
                arrow_end: false,
                even_odd_fill: None,
                corner_radius: radius,
                group_id: None,
            }
        };
        let paint_fill = |s: &mut DrawingShape| {
            if let Some((c, a, even_odd)) = fill {
                s.fill_color_hex = Some(hex(c));
                s.fill_opacity = Some(a * opacity);
                if even_odd {
                    s.even_odd_fill = Some(true);
                }
            }
        };
        let paint_stroke = |s: &mut DrawingShape| {
            if let Some((c, a, width)) = stroke {
                s.stroke_color_hex = hex(c);
                s.stroke_opacity = Some(a * opacity);
                s.stroke_width = width;
            }
        };
        let mut out: Vec<DrawingShape> = Vec::new();
        // One shape taking both paints, or — stroke first — two.
        let both_on = |out: &mut Vec<DrawingShape>, make: &mut dyn FnMut() -> DrawingShape| {
            if stroke_first && both {
                let mut s = make();
                paint_stroke(&mut s);
                out.push(s);
                let mut f = make();
                paint_fill(&mut f);
                out.push(f);
            } else {
                let mut s = make();
                paint_fill(&mut s);
                paint_stroke(&mut s);
                out.push(s);
            }
        };
        let b = data.bounds();
        let corners = vec![
            Point(b.left() as f64, b.top() as f64),
            Point(b.right() as f64, b.bottom() as f64),
        ];
        let axis = axis_aligned(ts);
        // The shape's own kind, when it has one.
        let own: Option<(DrawingShapeKind, Vec<Point>, Option<f64>)> = match tag.as_str() {
            "rect" if axis => {
                rect_radius(&data).map(|r| (DrawingShapeKind::Rect, corners.clone(), r))
            }
            "circle" | "ellipse" if axis => Some((DrawingShapeKind::Oval, corners.clone(), None)),
            "line" if fill.is_none() => match subpaths.as_slice() {
                [one] if one.len() == 2 => Some((DrawingShapeKind::Line, one.clone(), None)),
                _ => None,
            },
            _ => None,
        };
        if let Some((kind, points, radius)) = own {
            both_on(&mut out, &mut || make(kind, points.clone(), radius));
            return Ok(out);
        }
        // A pen. One sub-path takes both paints on itself (an open one's
        // fill closes, as the file's does).
        if let [only] = subpaths.as_slice() {
            both_on(&mut out, &mut || {
                make(DrawingShapeKind::Pen, only.clone(), None)
            });
            return Ok(out);
        }
        // Several: the fill as one polyline joined by zero-width bridges
        // (its winding, and so its holes, unchanged), the stroke a pen per
        // sub-path.
        let mut fill_shape = None;
        if fill.is_some() {
            let mut bridged: Vec<Point> = Vec::new();
            for sp in subpaths.iter().filter(|sp| area(sp).abs() >= 1e-9) {
                let mut closed = sp.clone();
                if closed.first() != closed.last() {
                    closed.push(closed[0]);
                }
                match bridged.last().copied() {
                    Some(back) => {
                        bridged.extend(closed);
                        bridged.push(back);
                    }
                    None => bridged.extend(closed),
                }
            }
            if bridged.len() >= 3 {
                let mut s = make(DrawingShapeKind::Pen, bridged, None);
                paint_fill(&mut s);
                fill_shape = Some(s);
            }
        }
        let mut stroke_shapes = Vec::new();
        if stroke.is_some() {
            for sp in subpaths.iter().filter(|sp| sp.len() >= 2) {
                let mut s = make(DrawingShapeKind::Pen, sp.clone(), None);
                paint_stroke(&mut s);
                stroke_shapes.push(s);
            }
        }
        if stroke_first {
            out.extend(stroke_shapes);
            out.extend(fill_shape);
        } else {
            out.extend(fill_shape);
            out.extend(stroke_shapes);
        }
        Ok(out)
    }
}

/// A closed polyline's signed area (the shoelace), closing it if open.
fn area(points: &[Point]) -> f64 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

/// Whether an element sits inside something never drawn as itself (a
/// definition, a clip, a mask, a pattern, a marker, a symbol).
fn in_definition(model: &crate::Model, node: &crate::Node) -> bool {
    let mut at = node.parent.clone();
    while let Some(address) = at {
        let Some(parent) = model.node(&address) else {
            break;
        };
        if matches!(
            parent.kind,
            "defs" | "symbol" | "clip" | "mask" | "pattern" | "marker" | "filter"
        ) {
            return true;
        }
        at = parent.parent.clone();
    }
    false
}

/// Drawn leaves in a group.
fn leaves(g: &usvg::Group) -> usize {
    g.children()
        .iter()
        .map(|c| match c {
            usvg::Node::Group(inner) => leaves(inner),
            _ => 1,
        })
        .sum()
}

/// The scale of a transform that scales both axes alike (rotation and
/// translation allowed), else `None`.
fn uniform_scale(ts: Transform) -> Option<f32> {
    let sx = (ts.sx * ts.sx + ts.ky * ts.ky).sqrt();
    let sy = (ts.kx * ts.kx + ts.sy * ts.sy).sqrt();
    let dot = ts.sx * ts.kx + ts.ky * ts.sy;
    let tol = 1e-4 * sx.max(sy).max(1e-6);
    ((sx - sy).abs() <= tol && dot.abs() <= tol * sx.max(sy)).then_some((sx + sy) / 2.0)
}

/// Whether a transform keeps axes as axes (no rotation or skew).
fn axis_aligned(ts: Transform) -> bool {
    ts.ky.abs() < 1e-6 && ts.kx.abs() < 1e-6 && ts.sx > 0.0 && ts.sy > 0.0
}

/// A rect path's corner radius: `Some(None)` square, `Some(Some(r))`
/// rounded alike, `None` when its corners are not one circle's quarter.
fn rect_radius(path: &Path) -> Option<Option<f64>> {
    let segs: Vec<PathSegment> = path.segments().collect();
    let curves = segs
        .iter()
        .filter(|s| matches!(s, PathSegment::CubicTo(..) | PathSegment::QuadTo(..)))
        .count();
    if curves == 0 {
        return Some(None);
    }
    let b = path.bounds();
    let PathSegment::MoveTo(start) = segs.first()? else {
        return None;
    };
    let rx = start.x - b.left();
    // The first corner's curve ends `ry` below the top.
    let end = segs.iter().find_map(|s| match s {
        PathSegment::CubicTo(_, _, e) | PathSegment::QuadTo(_, e) => Some(*e),
        _ => None,
    })?;
    let ry = end.y - b.top();
    ((rx - ry).abs() <= 1e-3 * rx.abs().max(1.0) && curves == 4).then_some(Some(rx as f64))
}

/// Each sub-path's points, curves cut to `tolerance`; a closed sub-path
/// ends where it began.
fn points_of(path: &Path, tolerance: f32) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut last = tiny_skia::Point::zero();
    let mut first = tiny_skia::Point::zero();
    let pt = |p: tiny_skia::Point| Point(p.x as f64, p.y as f64);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                if current.len() > 1 {
                    out.push(std::mem::take(&mut current));
                }
                current.clear();
                current.push(pt(p));
                first = p;
                last = p;
            }
            PathSegment::LineTo(p) => {
                current.push(pt(p));
                last = p;
            }
            PathSegment::QuadTo(c, p) => {
                let n = steps(&[last, c, p], tolerance);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    current.push(Point(
                        (u * u * last.x + 2.0 * u * t * c.x + t * t * p.x) as f64,
                        (u * u * last.y + 2.0 * u * t * c.y + t * t * p.y) as f64,
                    ));
                }
                last = p;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let n = steps(&[last, c1, c2, p], tolerance);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    current.push(Point(
                        (a * last.x + b * c1.x + c * c2.x + d * p.x) as f64,
                        (a * last.y + b * c1.y + c * c2.y + d * p.y) as f64,
                    ));
                }
                last = p;
            }
            PathSegment::Close => {
                if current.last() != Some(&pt(first)) {
                    current.push(pt(first));
                }
                if current.len() > 1 {
                    out.push(std::mem::take(&mut current));
                }
                current.clear();
                last = first;
            }
        }
    }
    if current.len() > 1 {
        out.push(current);
    }
    out
}

/// Segments that keep a curve within `tolerance` of its lines: from the
/// control polygon's second differences (the standard bound).
fn steps(points: &[tiny_skia::Point], tolerance: f32) -> usize {
    let mut dd = 0.0f32;
    for w in points.windows(3) {
        let x = w[0].x - 2.0 * w[1].x + w[2].x;
        let y = w[0].y - 2.0 * w[1].y + w[2].y;
        dd = dd.max((x * x + y * y).sqrt());
    }
    let degree = (points.len() - 1) as f32;
    let n = ((degree * (degree - 1.0) / 8.0 * dd / tolerance.max(1e-6)).sqrt()).ceil();
    (n as usize).clamp(1, 4096)
}

/// Whether a path has a corner a join would draw: a turn sharper than a
/// degree between two segments, or where a closed sub-path meets itself.
fn has_corner(path: &Path) -> bool {
    let mut prev_dir: Option<(f32, f32)> = None;
    let mut first_dir: Option<(f32, f32)> = None;
    let mut last = tiny_skia::Point::zero();
    let mut first = tiny_skia::Point::zero();
    let unit = |dx: f32, dy: f32| {
        let l = (dx * dx + dy * dy).sqrt();
        (l > 1e-6).then(|| (dx / l, dy / l))
    };
    let turned = |a: (f32, f32), b: (f32, f32)| a.0 * b.0 + a.1 * b.1 < 0.99985;
    for seg in path.segments() {
        let (start_dir, end_dir, end) = match seg {
            PathSegment::MoveTo(p) => {
                prev_dir = None;
                first_dir = None;
                first = p;
                last = p;
                continue;
            }
            PathSegment::LineTo(p) => {
                let d = unit(p.x - last.x, p.y - last.y);
                (d, d, p)
            }
            PathSegment::QuadTo(c, p) => (
                unit(c.x - last.x, c.y - last.y).or(unit(p.x - last.x, p.y - last.y)),
                unit(p.x - c.x, p.y - c.y).or(unit(p.x - last.x, p.y - last.y)),
                p,
            ),
            PathSegment::CubicTo(c1, c2, p) => (
                unit(c1.x - last.x, c1.y - last.y)
                    .or(unit(c2.x - last.x, c2.y - last.y))
                    .or(unit(p.x - last.x, p.y - last.y)),
                unit(p.x - c2.x, p.y - c2.y)
                    .or(unit(p.x - c1.x, p.y - c1.y))
                    .or(unit(p.x - last.x, p.y - last.y)),
                p,
            ),
            PathSegment::Close => {
                let closing = unit(first.x - last.x, first.y - last.y);
                if let (Some(c), Some(prev)) = (closing, prev_dir) {
                    if turned(prev, c) {
                        return true;
                    }
                }
                let into_first = closing.or(prev_dir);
                if let (Some(a), Some(b)) = (into_first, first_dir) {
                    if turned(a, b) {
                        return true;
                    }
                }
                prev_dir = None;
                first_dir = None;
                last = first;
                continue;
            }
        };
        if let (Some(prev), Some(s)) = (prev_dir, start_dir) {
            if turned(prev, s) {
                return true;
            }
        }
        if first_dir.is_none() {
            first_dir = start_dir;
        }
        if end_dir.is_some() {
            prev_dir = end_dir;
        }
        last = end;
    }
    false
}

/// Whether a path has an open sub-path a cap would end.
fn has_open_end(path: &Path) -> bool {
    let mut open = false;
    let mut drawn = false;
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(_) => {
                if open && drawn {
                    return true;
                }
                open = true;
                drawn = false;
            }
            PathSegment::Close => {
                open = false;
                drawn = false;
            }
            _ => drawn = true,
        }
    }
    open && drawn
}
