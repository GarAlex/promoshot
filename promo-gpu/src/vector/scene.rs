//! An SVG picture's painted paths, drawn on the GPU (core-private
//! docs/SVG-PLAN.md, slice 4).
//!
//! The scene is what promo-svg hands over after reading a picture's tree:
//! every path the GPU draws the way resvg would, already in the raster's
//! pixel space, in paint order, between the pieces resvg drew itself
//! (filters, masks, pictures — [`Raster`]) and inside the groups that need
//! a layer of their own ([`Layer`]: an opacity over several children, a
//! clip). Nothing here knows SVG.
//!
//! **What makes it match resvg.** A shape is a filled path in device
//! pixels: promo-svg strokes and dashes with tiny-skia's own stroker before
//! it hands a path over, so a stroke's joins, caps and dashes are resvg's
//! geometry, not a second implementation. A paint is a solid colour or a
//! gradient expressed as tiny-skia's own pipeline would evaluate it (the
//! pixel → unit matrix, the stop table of factors and biases, the spread
//! rule), evaluated per pixel at the pixel's centre. Blending is
//! premultiplied source-over in 8 bits. What differs is coverage: resvg
//! computes it analytically-ish, this rasterises with MSAA (8 samples where
//! the adapter has them, else 4), so the two pictures differ only on the
//! antialiased rim of shapes — the gate in promo-svg's tests measures that.
//!
//! **Order and overlap.** A filled path's triangles never overlap (lyon's
//! fill tessellation), so each sample is drawn once per shape, as tiny-skia
//! fills it once. One shape's fill and stroke under an element opacity is
//! one "pair": resvg draws the two into a layer and composites it with the
//! opacity; here the stencil makes each sample take the pair's composite
//! colour exactly once, so no layer is needed.

use crate::{GpuContext, GpuError};
use lyon_tessellation::geom::point;
use lyon_tessellation::path::Path as LyonPath;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers,
};

/// One segment of a path in device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Move([f32; 2]),
    Line([f32; 2]),
    Quad([f32; 2], [f32; 2]),
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    Close,
}

/// A filled path and its paint.
#[derive(Debug, Clone)]
pub struct Shape {
    pub segs: Vec<Seg>,
    pub even_odd: bool,
    pub paint: Paint,
}

/// How a shape is painted.
#[derive(Debug, Clone)]
pub enum Paint {
    /// Premultiplied RGBA, 0–1.
    Solid([f32; 4]),
    Gradient(Gradient),
}

/// A gradient as tiny-skia's raster pipeline evaluates it: a device pixel
/// centre is mapped by `to_unit` into the gradient's unit space, turned
/// into `t` by its shape, tiled by its spread, and looked up in `stops`.
#[derive(Debug, Clone)]
pub struct Gradient {
    pub shape: GradientShape,
    /// Device pixel → unit space, `[sx, ky, kx, sy, tx, ty]` (tiny-skia's
    /// row order: `x' = sx·x + kx·y + tx`, `y' = ky·x + sy·y + ty`).
    pub to_unit: [f32; 6],
    pub spread: Spread,
    /// Pad with evenly spaced stops clamps `t` to 0–1 first.
    pub clamp: bool,
    /// Entry 0 is the colour before the first stop; entry `i` applies from
    /// `t >= stops[i].t`: colour = `t · factor + bias`.
    pub stops: Vec<Stop>,
    /// The colours are straight (some stop is not opaque): premultiply
    /// after the lookup.
    pub premultiply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub t: f32,
    pub factor: [f32; 4],
    pub bias: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientShape {
    /// `t = x`.
    Linear,
    /// `t = |xy|`, then `t·s + b` for concentric circles of two radii.
    Radial { scale_bias: Option<[f32; 2]> },
    /// A two-point conical gradient whose focus is well inside its circle
    /// (tiny-skia's "well behaved" focal case): `t = |xy| − x·p0`, then
    /// negated, shifted by `p1` and un-swapped as the flags say.
    Focal {
        p0: f32,
        p1: f32,
        negate: bool,
        compensate: bool,
        unswap: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spread {
    Pad,
    Reflect,
    Repeat,
}

/// Pixels resvg drew (premultiplied RGBA), placed at `(x, y)` in the
/// coordinates of the scene's pixels.
#[derive(Debug, Clone)]
pub struct Raster {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Shared, so a piece resvg drew for one frame serves the next.
    pub rgba: std::sync::Arc<Vec<u8>>,
}

/// A group drawn into a layer of its own and composited with `opacity`,
/// masked by the union of `clip`'s shapes when it has one (`Some(empty)`
/// clips everything away). The rect is in scene pixels.
#[derive(Debug, Clone)]
pub struct Layer {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub clip: Option<Vec<ClipShape>>,
    pub items: Vec<Item>,
}

/// One shape of a clip path, in device pixels.
#[derive(Debug, Clone)]
pub struct ClipShape {
    pub segs: Vec<Seg>,
    pub even_odd: bool,
}

#[derive(Debug, Clone)]
pub enum Item {
    /// One shape, or a pair (the first drawn first: fill then stroke, or
    /// the reverse under `paint-order`), with an opacity over all of it —
    /// the opacity of a group that holds just this element.
    Draw {
        shapes: Vec<Shape>,
        opacity: f32,
    },
    Raster(Raster),
    Layer(Layer),
}

/// A picture to draw: `width`×`height` pixels, transparent where nothing
/// is drawn.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub width: u32,
    pub height: u32,
    pub items: Vec<Item>,
}

impl Paint {
    /// Whether every pixel it paints is opaque.
    pub fn opaque(&self) -> bool {
        match self {
            Paint::Solid(c) => c[3] >= 1.0,
            Paint::Gradient(g) => !g.premultiply,
        }
    }
}

/// The largest side one multisampled pass covers; a bigger region is drawn
/// in tiles, so the multisampled buffers stay bounded (2048² at 8 samples
/// is 128 MB of colour and 32 MB of stencil, the size the drawing
/// rasterizer already uses for a 1080p frame).
pub const MAX_TILE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Vertex {
    pos: [f32; 2],
    paint: u32,
    /// The paint drawn over `paint` where a pair's two shapes overlap, or
    /// `NONE`.
    over: u32,
    opacity: f32,
}

const NONE: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct GpuPaint {
    kind: u32,
    spread: u32,
    flags: u32,
    stops_start: u32,
    stops_len: u32,
    _pad: u32,
    p0: f32,
    p1: f32,
    color: [f32; 4],
    m0: [f32; 4],
    m1: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct GpuStop {
    t: [f32; 4],
    factor: [f32; 4],
    bias: [f32; 4],
}

const FLAG_CLAMP: u32 = 1;
const FLAG_PREMULTIPLY: u32 = 2;
const FLAG_SCALE_BIAS: u32 = 4;
const FLAG_NEGATE: u32 = 8;
const FLAG_COMPENSATE: u32 = 16;
const FLAG_UNSWAP: u32 = 32;

const SHADER: &str = r#"
struct Globals { origin: vec2<f32>, size: vec2<f32> };
@group(0) @binding(0) var<uniform> g: Globals;

struct Paint {
    kind: u32, spread: u32, flags: u32, stops_start: u32,
    stops_len: u32, pad: u32, p0: f32, p1: f32,
    color: vec4<f32>, m0: vec4<f32>, m1: vec4<f32>,
};
struct Stop { t: vec4<f32>, factor: vec4<f32>, bias: vec4<f32> };
@group(0) @binding(1) var<storage, read> paints: array<Paint>;
@group(0) @binding(2) var<storage, read> stops: array<Stop>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) paint: u32,
    @location(1) @interpolate(flat) over: u32,
    @location(2) @interpolate(flat) opacity: f32,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) paint: u32, @location(2) over: u32,
           @location(3) opacity: f32) -> VsOut {
    var out: VsOut;
    let p = (pos - g.origin) / g.size;
    out.pos = vec4<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    out.paint = paint;
    out.over = over;
    out.opacity = opacity;
    return out;
}

fn paint_at(i: u32, xy: vec2<f32>) -> vec4<f32> {
    let p = paints[i];
    if (p.kind == 0u) {
        return p.color;
    }
    let x = p.m0.x * xy.x + p.m0.z * xy.y + p.m1.x;
    let y = p.m0.y * xy.x + p.m0.w * xy.y + p.m1.y;
    var t: f32;
    if (p.kind == 1u) {
        t = x;
    } else if (p.kind == 2u) {
        t = sqrt(x * x + y * y);
        if ((p.flags & 4u) != 0u) {
            t = t * p.p0 + p.p1;
        }
    } else {
        t = sqrt(x * x + y * y) - x * p.p0;
        if ((p.flags & 8u) != 0u) { t = -t; }
        if ((p.flags & 16u) != 0u) { t = t + p.p1; }
        if ((p.flags & 32u) != 0u) { t = 1.0 - t; }
    }
    if (p.spread == 1u) {
        t = clamp(abs((t - 1.0) - 2.0 * floor((t - 1.0) * 0.5) - 1.0), 0.0, 1.0);
    } else if (p.spread == 2u) {
        t = clamp(t - floor(t), 0.0, 1.0);
    } else if ((p.flags & 1u) != 0u) {
        t = clamp(t, 0.0, 1.0);
    }
    var idx = 0u;
    for (var k = 1u; k < p.stops_len; k = k + 1u) {
        if (t >= stops[p.stops_start + k].t.x) {
            idx = idx + 1u;
        }
    }
    let s = stops[p.stops_start + idx];
    var c = t * s.factor + s.bias;
    if ((p.flags & 2u) != 0u) {
        c = vec4<f32>(c.rgb * c.a, c.a);
    }
    return c;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let xy = in.pos.xy + g.origin;
    var c = paint_at(in.paint, xy);
    if (in.over != 0xffffffffu) {
        let top = paint_at(in.over, xy);
        c = top + c * (1.0 - top.a);
    }
    return c * in.opacity;
}
"#;

const COMPOSITE: &str = r#"
struct C {
    origin: vec2<f32>, size: vec2<f32>, target_size: vec2<f32>, src_offset: vec2<f32>,
    opacity: f32, use_mask: u32, pad0: f32, pad1: f32,
};
@group(0) @binding(0) var<uniform> c: C;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var mask: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0));
    let p = (c.origin + corners[i] * c.size) / c.target_size;
    return vec4<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(pos.xy - c.origin + c.src_offset));
    var v = textureLoad(src, p, 0);
    if (c.use_mask != 0u) {
        v = v * textureLoad(mask, p, 0).a;
    }
    return v * c.opacity;
}
"#;

/// The pipelines' stencil roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pipe {
    /// Drawn plainly (no stencil test).
    Plain,
    /// Marks its samples with the reference, draws no colour.
    Mark,
    /// Draws where the stencil equals the reference and increments it.
    EqIncr,
    /// Draws where the stencil, its lowest bit ignored, is not the
    /// reference: neither marked nor done by this batch.
    NeKeep,
    /// Draws where the stencil equals the reference, leaves it.
    EqKeep,
    /// Zeroes the stencil, draws no colour.
    ZeroAll,
}

const PIPES: [Pipe; 6] = [
    Pipe::Plain,
    Pipe::Mark,
    Pipe::EqIncr,
    Pipe::NeKeep,
    Pipe::EqKeep,
    Pipe::ZeroAll,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl IRect {
    fn intersect(self, o: IRect) -> Option<IRect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        (x1 > x0 && y1 > y0).then_some(IRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    fn union(self, o: IRect) -> IRect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        IRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Call {
    pipe: Pipe,
    start: u32,
    count: u32,
    stencil: u32,
    bbox: IRect,
}

/// Where a pass draws: the scene's frame, a layer, or a layer's clip mask.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// In scene pixels.
    rect: IRect,
}

#[derive(Debug)]
enum Op {
    Clear {
        target: usize,
    },
    Run {
        target: usize,
        rect: IRect,
        calls: Vec<Call>,
    },
    Raster {
        target: usize,
        raster: usize,
    },
    Composite {
        target: usize,
        src: usize,
        opacity: f32,
        mask: Option<usize>,
    },
}

/// The scene flattened: one vertex and index list, paints, the targets and
/// the passes in order.
struct Compiled<'a> {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    paints: Vec<GpuPaint>,
    stops: Vec<GpuStop>,
    targets: Vec<Target>,
    ops: Vec<Op>,
    rasters: Vec<&'a Raster>,
    tess: FillTessellator,
    /// Paths tessellated ahead, by (segments, count, rule).
    meshes: std::collections::HashMap<(usize, usize, bool), Option<Mesh>>,
}

/// What a scene cost to draw, for the measurements.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneStats {
    pub triangles: usize,
    pub passes: usize,
    pub draw_calls: usize,
    /// Milliseconds tessellating, and encoding the passes.
    pub tessellate_ms: f64,
    pub encode_ms: f64,
}

impl<'a> Compiled<'a> {
    fn new() -> Self {
        Compiled {
            vertices: Vec::new(),
            indices: Vec::new(),
            paints: Vec::new(),
            stops: Vec::new(),
            targets: Vec::new(),
            ops: Vec::new(),
            rasters: Vec::new(),
            tess: FillTessellator::new(),
            meshes: std::collections::HashMap::new(),
        }
    }

    fn paint(&mut self, paint: &Paint) -> u32 {
        let index = self.paints.len() as u32;
        let mut gp = GpuPaint::default();
        match paint {
            Paint::Solid(c) => {
                gp.kind = 0;
                gp.color = *c;
            }
            Paint::Gradient(g) => {
                gp.kind = match g.shape {
                    GradientShape::Linear => 1,
                    GradientShape::Radial { .. } => 2,
                    GradientShape::Focal { .. } => 3,
                };
                gp.spread = match g.spread {
                    Spread::Pad => 0,
                    Spread::Reflect => 1,
                    Spread::Repeat => 2,
                };
                let mut flags = 0;
                if g.clamp {
                    flags |= FLAG_CLAMP;
                }
                if g.premultiply {
                    flags |= FLAG_PREMULTIPLY;
                }
                match g.shape {
                    GradientShape::Linear => {}
                    GradientShape::Radial { scale_bias } => {
                        if let Some([s, b]) = scale_bias {
                            flags |= FLAG_SCALE_BIAS;
                            gp.p0 = s;
                            gp.p1 = b;
                        }
                    }
                    GradientShape::Focal {
                        p0,
                        p1,
                        negate,
                        compensate,
                        unswap,
                    } => {
                        gp.p0 = p0;
                        gp.p1 = p1;
                        if negate {
                            flags |= FLAG_NEGATE;
                        }
                        if compensate {
                            flags |= FLAG_COMPENSATE;
                        }
                        if unswap {
                            flags |= FLAG_UNSWAP;
                        }
                    }
                }
                gp.flags = flags;
                let m = g.to_unit;
                gp.m0 = [m[0], m[1], m[2], m[3]];
                gp.m1 = [m[4], m[5], 0.0, 0.0];
                gp.stops_start = self.stops.len() as u32;
                gp.stops_len = g.stops.len() as u32;
                for s in &g.stops {
                    self.stops.push(GpuStop {
                        t: [s.t, 0.0, 0.0, 0.0],
                        factor: s.factor,
                        bias: s.bias,
                    });
                }
            }
        }
        self.paints.push(gp);
        index
    }

    /// One path's triangles, tessellated ahead (in parallel) or now,
    /// appended to the vertex list with its paint; answers its indices and
    /// device bounds.
    fn fill(
        &mut self,
        segs: &[Seg],
        even_odd: bool,
        paint: u32,
        over: u32,
        opacity: f32,
    ) -> Option<(Vec<u32>, IRect)> {
        let key = (segs.as_ptr() as usize, segs.len(), even_odd);
        let owned;
        let mesh = match self.meshes.get(&key) {
            Some(m) => m.as_ref()?,
            None => {
                owned = tessellate(segs, even_odd, &mut self.tess)?;
                &owned
            }
        };
        let base = self.vertices.len() as u32;
        self.vertices
            .extend(mesh.positions.iter().map(|&pos| Vertex {
                pos,
                paint,
                over,
                opacity,
            }));
        Some((mesh.indices.iter().map(|i| i + base).collect(), mesh.bbox))
    }

    /// Tessellates every path of `items` ahead, on several threads: the
    /// work that grows with a picture's detail.
    fn tessellate_ahead(&mut self, items: &[Item]) {
        fn collect<'s>(items: &'s [Item], out: &mut Vec<(&'s [Seg], bool)>) {
            for item in items {
                match item {
                    Item::Draw { shapes, .. } => {
                        out.extend(shapes.iter().map(|s| (s.segs.as_slice(), s.even_odd)))
                    }
                    Item::Raster(_) => {}
                    Item::Layer(l) => {
                        collect(&l.items, out);
                        if let Some(clip) = &l.clip {
                            out.extend(clip.iter().map(|s| (s.segs.as_slice(), s.even_odd)));
                        }
                    }
                }
            }
        }
        let mut paths = Vec::new();
        collect(items, &mut paths);
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, 8);
        if paths.len() < 64 || threads == 1 {
            return;
        }
        let chunk = paths.len().div_ceil(threads);
        // A part whose thread failed is left out: its paths tessellate
        // inline when drawn.
        let results: Vec<Option<Vec<Option<Mesh>>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(chunk)
                .map(|part| {
                    scope.spawn(move || {
                        let mut tess = FillTessellator::new();
                        part.iter()
                            .map(|(segs, even_odd)| tessellate(segs, *even_odd, &mut tess))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().ok()).collect()
        });
        for (part, meshes) in paths.chunks(chunk).zip(results) {
            let Some(meshes) = meshes else { continue };
            for (path, mesh) in part.iter().zip(meshes) {
                self.meshes
                    .insert((path.0.as_ptr() as usize, path.0.len(), path.1), mesh);
            }
        }
    }

    fn target(&mut self, rect: IRect) -> usize {
        self.targets.push(Target { rect });
        self.ops.push(Op::Clear {
            target: self.targets.len() - 1,
        });
        self.targets.len() - 1
    }

    /// Appends indices as one call.
    fn call(&mut self, pipe: Pipe, indices: &[u32], stencil: u32, bbox: IRect) -> Call {
        let start = self.indices.len() as u32;
        self.indices.extend_from_slice(indices);
        Call {
            pipe,
            start,
            count: indices.len() as u32,
            stencil,
            bbox,
        }
    }

    /// The batch's draws as calls: its plain shapes in one, then each
    /// stencil step of its pairs in one (no pair overlaps another in a
    /// batch, and no plain shape overlaps a pair it follows).
    fn flush_batch(&mut self, run: &mut Run) {
        let batch = std::mem::take(&mut run.batch);
        if let Some(bbox) = batch.plain_bbox {
            let c = self.call(Pipe::Plain, &batch.plain, 0, bbox);
            run.calls.push(c);
        }
        if let Some(bbox) = batch.pair_bbox {
            // Each batch of pairs marks with its own value (2k, done =
            // 2k + 1), so no step has to clear behind it; after 127 the
            // values come round again and the stencil is zeroed first.
            if run.k >= 127 {
                let quad = self.clear_quad(run.target);
                run.calls.push(quad);
                run.k = 0;
            }
            run.k += 1;
            let r = 2 * run.k;
            let mark = self.call(Pipe::Mark, &batch.a, r, bbox);
            let over = self.call(Pipe::EqIncr, &batch.b_over, r, bbox);
            let alone = self.call(Pipe::NeKeep, &batch.b, r, bbox);
            let rest = self.call(Pipe::EqKeep, &batch.a, r, bbox);
            run.calls.extend([mark, over, alone, rest]);
        }
    }

    /// A quad over the whole target that zeroes the stencil.
    fn clear_quad(&mut self, target: usize) -> Call {
        let r = self.targets[target].rect;
        let base = self.vertices.len() as u32;
        let (x0, y0, x1, y1) = (
            r.x as f32,
            r.y as f32,
            (r.x + r.w) as f32,
            (r.y + r.h) as f32,
        );
        for pos in [[x0, y0], [x1, y0], [x1, y1], [x0, y1]] {
            self.vertices.push(Vertex {
                pos,
                paint: 0,
                over: NONE,
                opacity: 0.0,
            });
        }
        self.call(
            Pipe::ZeroAll,
            &[base, base + 1, base + 2, base, base + 2, base + 3],
            0,
            r,
        )
    }

    /// The run's calls as one pass into its target (tiled when large).
    fn flush(&mut self, run: &mut Run) {
        self.flush_batch(run);
        run.k = 0;
        if run.calls.is_empty() {
            return;
        }
        let mut rect = run.calls[0].bbox;
        for c in run.calls.iter().skip(1) {
            rect = rect.union(c.bbox);
        }
        let grown = IRect {
            x: rect.x - 1,
            y: rect.y - 1,
            w: rect.w + 2,
            h: rect.h + 2,
        };
        let calls = std::mem::take(&mut run.calls);
        if let Some(rect) = grown.intersect(self.targets[run.target].rect) {
            self.ops.push(Op::Run {
                target: run.target,
                rect,
                calls,
            });
        }
    }

    fn plain(&mut self, run: &mut Run, indices: Vec<u32>, bbox: IRect) {
        if run.batch.overlaps_pair(bbox) {
            self.flush_batch(run);
        }
        let b = &mut run.batch;
        b.plain.extend(indices);
        b.plain_bbox = Some(b.plain_bbox.map_or(bbox, |o| o.union(bbox)));
    }

    fn items(&mut self, items: &'a [Item], target: usize) {
        let mut run = Run {
            target,
            calls: Vec::new(),
            batch: Batch::default(),
            k: 0,
        };
        for item in items {
            match item {
                Item::Draw { shapes, opacity } => {
                    let opacity = opacity.clamp(0.0, 1.0);
                    if opacity <= 0.0 {
                        continue;
                    }
                    if shapes.len() == 2 && opacity < 1.0 {
                        self.pair(&shapes[0], &shapes[1], opacity, &mut run);
                    } else {
                        for shape in shapes {
                            let paint = self.paint(&shape.paint);
                            if let Some((indices, bbox)) =
                                self.fill(&shape.segs, shape.even_odd, paint, NONE, opacity)
                            {
                                self.plain(&mut run, indices, bbox);
                            }
                        }
                    }
                }
                Item::Raster(raster) => {
                    self.flush(&mut run);
                    if raster.width > 0
                        && raster.height > 0
                        && raster.rgba.len() >= (raster.width * raster.height * 4) as usize
                    {
                        self.rasters.push(raster);
                        self.ops.push(Op::Raster {
                            target,
                            raster: self.rasters.len() - 1,
                        });
                    }
                }
                Item::Layer(layer) => {
                    self.flush(&mut run);
                    let rect = IRect {
                        x: layer.x,
                        y: layer.y,
                        w: layer.width as i32,
                        h: layer.height as i32,
                    };
                    let Some(rect) = rect.intersect(self.targets[target].rect) else {
                        continue;
                    };
                    if layer.opacity <= 0.0 {
                        continue;
                    }
                    let inner = self.target(rect);
                    self.items(&layer.items, inner);
                    let mask = match &layer.clip {
                        None => None,
                        Some(shapes) => {
                            let mask = self.target(rect);
                            let white = self.paint(&Paint::Solid([1.0; 4]));
                            let mut clip_run = Run {
                                target: mask,
                                calls: Vec::new(),
                                batch: Batch::default(),
                                k: 0,
                            };
                            for s in shapes {
                                if let Some((indices, bbox)) =
                                    self.fill(&s.segs, s.even_odd, white, NONE, 1.0)
                                {
                                    self.plain(&mut clip_run, indices, bbox);
                                }
                            }
                            self.flush(&mut clip_run);
                            Some(mask)
                        }
                    };
                    self.ops.push(Op::Composite {
                        target,
                        src: inner,
                        opacity: layer.opacity.min(1.0),
                        mask,
                    });
                }
            }
        }
        self.flush(&mut run);
    }

    /// Two shapes under one opacity, each sample taking their composite
    /// once: mark the first, draw the second over it where marked (the
    /// over-colour, marking it done), the second alone where the first is
    /// not, then the first where the second left it.
    fn pair(&mut self, first: &Shape, second: &Shape, opacity: f32, run: &mut Run) {
        let a = self.paint(&first.paint);
        let b = self.paint(&second.paint);
        let fa = self.fill(&first.segs, first.even_odd, a, NONE, opacity);
        let fb = self.fill(&second.segs, second.even_odd, b, NONE, opacity);
        match (fa, fb) {
            (Some((ia, ba)), Some((ib, bb))) => {
                let Some((iover, _)) = self.fill(&second.segs, second.even_odd, a, b, opacity)
                else {
                    return;
                };
                let bbox = ba.union(bb);
                if run.batch.overlaps_pair(bbox) {
                    self.flush_batch(run);
                }
                let batch = &mut run.batch;
                batch.a.extend(ia);
                batch.b.extend(ib);
                batch.b_over.extend(iover);
                batch.pairs.push(bbox);
                batch.pair_bbox = Some(batch.pair_bbox.map_or(bbox, |o| o.union(bbox)));
            }
            (Some((only, bbox)), None) | (None, Some((only, bbox))) => {
                self.plain(run, only, bbox);
            }
            (None, None) => {}
        }
    }
}

/// Draws gathered so far, before they become calls.
#[derive(Default)]
struct Batch {
    plain: Vec<u32>,
    plain_bbox: Option<IRect>,
    a: Vec<u32>,
    b: Vec<u32>,
    b_over: Vec<u32>,
    pairs: Vec<IRect>,
    pair_bbox: Option<IRect>,
}

impl Batch {
    /// Whether `bbox` (grown by a pixel for antialiasing) touches a pair
    /// already gathered — the only thing that forces an order inside a
    /// batch.
    fn overlaps_pair(&self, bbox: IRect) -> bool {
        let grown = IRect {
            x: bbox.x - 1,
            y: bbox.y - 1,
            w: bbox.w + 2,
            h: bbox.h + 2,
        };
        self.pair_bbox
            .is_some_and(|all| all.intersect(grown).is_some())
            && self.pairs.iter().any(|p| p.intersect(grown).is_some())
    }
}

/// One run of draws into a target.
struct Run {
    target: usize,
    calls: Vec<Call>,
    batch: Batch,
    /// Pair batches marked since the stencil was last zeroed.
    k: u32,
}

/// A path's triangles in device pixels.
struct Mesh {
    positions: Vec<[f32; 2]>,
    indices: Vec<u32>,
    bbox: IRect,
}

/// The fill tolerance: a curve is cut into lines no farther than a
/// twentieth of a pixel from it.
const TOLERANCE: f32 = 0.05;

fn tessellate(segs: &[Seg], even_odd: bool, tess: &mut FillTessellator) -> Option<Mesh> {
    let path = lyon_path(segs)?;
    let bbox = seg_bounds(segs)?;
    let mut mesh: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(TOLERANCE).with_fill_rule(if even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    });
    tess.tessellate_path(
        &path,
        &options,
        &mut BuffersBuilder::new(&mut mesh, |v: FillVertex| [v.position().x, v.position().y]),
    )
    .ok()?;
    if mesh.indices.is_empty() {
        return None;
    }
    Some(Mesh {
        positions: mesh.vertices,
        indices: mesh.indices,
        bbox,
    })
}

fn lyon_path(segs: &[Seg]) -> Option<LyonPath> {
    let mut b = LyonPath::builder();
    let mut open = false;
    let p = |v: [f32; 2]| point(v[0], v[1]);
    for seg in segs {
        match *seg {
            Seg::Move(to) => {
                if open {
                    b.end(false);
                }
                b.begin(p(to));
                open = true;
            }
            Seg::Line(to) => {
                if open {
                    b.line_to(p(to));
                }
            }
            Seg::Quad(c, to) => {
                if open {
                    b.quadratic_bezier_to(p(c), p(to));
                }
            }
            Seg::Cubic(c1, c2, to) => {
                if open {
                    b.cubic_bezier_to(p(c1), p(c2), p(to));
                }
            }
            Seg::Close => {
                if open {
                    b.end(true);
                    open = false;
                }
            }
        }
    }
    if open {
        b.end(false);
    }
    Some(b.build())
}

/// The bounds of a path's points (curves lie inside their control
/// points), rounded out to whole pixels.
fn seg_bounds(segs: &[Seg]) -> Option<IRect> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut add = |v: [f32; 2]| {
        x0 = x0.min(v[0]);
        y0 = y0.min(v[1]);
        x1 = x1.max(v[0]);
        y1 = y1.max(v[1]);
    };
    for seg in segs {
        match *seg {
            Seg::Move(a) | Seg::Line(a) => add(a),
            Seg::Quad(a, b) => {
                add(a);
                add(b)
            }
            Seg::Cubic(a, b, c) => {
                add(a);
                add(b);
                add(c)
            }
            Seg::Close => {}
        }
    }
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) || x1 < x0 {
        return None;
    }
    let (ix, iy) = (x0.floor() as i32, y0.floor() as i32);
    Some(IRect {
        x: ix,
        y: iy,
        w: (x1.ceil() as i32 - ix).max(1),
        h: (y1.ceil() as i32 - iy).max(1),
    })
}

/// The multisample count for both the colour and the stencil: 8 where the
/// device may use it, else 4 (the spec's guarantee).
fn sample_count(ctx: &GpuContext) -> u32 {
    let adapter_specific = ctx
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    let color = ctx
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::Bgra8Unorm)
        .flags;
    let stencil = ctx
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::Stencil8)
        .flags;
    for count in [8u32, 4] {
        if count > 4 && !adapter_specific {
            continue;
        }
        if color.sample_count_supported(count) && stencil.sample_count_supported(count) {
            return count;
        }
    }
    4
}

/// Multisampled attachments of one size, kept between scenes.
struct Attachments {
    width: u32,
    height: u32,
    color: wgpu::TextureView,
    stencil: wgpu::TextureView,
    #[allow(dead_code)]
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    used: u64,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// Draws [`Scene`]s. Owns the pipelines and keeps its multisampled buffers
/// between scenes.
pub struct SceneRenderer {
    samples: u32,
    run_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    pipes: Vec<(Pipe, wgpu::RenderPipeline)>,
    composite: wgpu::RenderPipeline,
    blank: wgpu::TextureView,
    attachments: Vec<Attachments>,
    clock: u64,
    align: u64,
    /// What the last scene cost.
    pub stats: SceneStats,
}

impl SceneRenderer {
    pub fn new(ctx: &GpuContext) -> Result<Self, GpuError> {
        let device = &ctx.device;
        let samples = sample_count(ctx);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("promo-svg-scene"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("promo-svg-composite"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE.into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: None,
            },
            count: None,
        };
        let run_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("promo-svg-run"),
            entries: &[uniform, storage(1), storage(2)],
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("promo-svg-composite"),
            entries: &[uniform, texture(1), texture(2)],
        });
        let run_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("promo-svg-run"),
            bind_group_layouts: &[&run_layout],
            push_constant_ranges: &[],
        });
        let over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let mut pipes = Vec::new();
        for pipe in PIPES {
            let (compare, pass_op, colour) = match pipe {
                Pipe::Plain => (
                    wgpu::CompareFunction::Always,
                    wgpu::StencilOperation::Keep,
                    true,
                ),
                Pipe::Mark => (
                    wgpu::CompareFunction::Always,
                    wgpu::StencilOperation::Replace,
                    false,
                ),
                Pipe::EqIncr => (
                    wgpu::CompareFunction::Equal,
                    wgpu::StencilOperation::IncrementClamp,
                    true,
                ),
                Pipe::NeKeep => (
                    wgpu::CompareFunction::NotEqual,
                    wgpu::StencilOperation::Keep,
                    true,
                ),
                Pipe::EqKeep => (
                    wgpu::CompareFunction::Equal,
                    wgpu::StencilOperation::Keep,
                    true,
                ),
                Pipe::ZeroAll => (
                    wgpu::CompareFunction::Always,
                    wgpu::StencilOperation::Zero,
                    false,
                ),
            };
            let face = wgpu::StencilFaceState {
                compare,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op,
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("promo-svg-scene"),
                layout: Some(&run_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Uint32,
                                offset: 8,
                                shader_location: 1,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Uint32,
                                offset: 12,
                                shader_location: 2,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 16,
                                shader_location: 3,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(over),
                        write_mask: if colour {
                            wgpu::ColorWrites::ALL
                        } else {
                            wgpu::ColorWrites::empty()
                        },
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Stencil8,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: wgpu::StencilState {
                        front: face,
                        back: face,
                        read_mask: if pipe == Pipe::NeKeep { 0xfe } else { 0xff },
                        write_mask: 0xff,
                    },
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            });
            pipes.push((pipe, pipeline));
        }
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("promo-svg-composite"),
                bind_group_layouts: &[&composite_layout],
                push_constant_ranges: &[],
            });
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("promo-svg-composite"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: Some(over),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("promo-svg-blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let align = device.limits().min_uniform_buffer_offset_alignment.max(64) as u64;
        Ok(SceneRenderer {
            samples,
            run_layout,
            composite_layout,
            pipes,
            composite,
            blank,
            attachments: Vec::new(),
            clock: 0,
            align,
            stats: SceneStats::default(),
        })
    }

    /// The multisample count in use.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    fn pipeline(&self, pipe: Pipe) -> &wgpu::RenderPipeline {
        &self
            .pipes
            .iter()
            .find(|(p, _)| *p == pipe)
            .expect("every pipe is built")
            .1
    }

    /// Multisampled attachments at least `w`×`h` (`exact` for a pass that
    /// resolves straight into its target), from the ones kept.
    fn attachments(&mut self, ctx: &GpuContext, w: u32, h: u32, exact: bool) -> usize {
        self.clock += 1;
        let fits = |a: &Attachments| {
            if exact {
                a.width == w && a.height == h
            } else {
                a.width >= w
                    && a.height >= h
                    && (a.width as u64 * a.height as u64) <= (w as u64 + 256) * (h as u64 + 256)
            }
        };
        if let Some(i) = self.attachments.iter().position(fits) {
            self.attachments[i].used = self.clock;
            return i;
        }
        let (aw, ah) = if exact {
            (w, h)
        } else {
            (w.div_ceil(128) * 128, h.div_ceil(128) * 128)
        };
        let (aw, ah) = (aw.min(MAX_TILE.max(w)), ah.min(MAX_TILE.max(h)));
        let make = |format, samples, usage| {
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("promo-svg-attachment"),
                size: wgpu::Extent3d {
                    width: aw,
                    height: ah,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = make(FORMAT, self.samples, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let stencil = make(
            wgpu::TextureFormat::Stencil8,
            self.samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let resolve = make(
            FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let resolve_view = resolve.create_view(&Default::default());
        self.attachments.push(Attachments {
            width: aw,
            height: ah,
            color,
            stencil,
            resolve,
            resolve_view,
            used: self.clock,
        });
        self.attachments.len() - 1
    }

    /// Draws `scene` into a new texture the compositor can sample.
    pub fn render(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
    ) -> Result<crate::ImportedFrame, GpuError> {
        let (w, h) = (scene.width.max(1), scene.height.max(1));
        let texture = self.render_texture(ctx, scene)?;
        Ok(crate::ImportedFrame::from_owned_texture(texture, w, h))
    }

    /// Draws `scene` and reads it back: premultiplied RGBA rows, as resvg
    /// draws them — for tests and measurements.
    pub fn render_pixels(&mut self, ctx: &GpuContext, scene: &Scene) -> Result<Vec<u8>, GpuError> {
        let (w, h) = (scene.width.max(1), scene.height.max(1));
        let texture = self.render_texture(ctx, scene)?;
        let mut px = read_texture(ctx, &texture, w, h)?;
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Ok(px)
    }

    /// Draws `scene` into a new `Bgra8Unorm` texture (render attachment,
    /// sampled, copy source). Submitted, not waited on: the compositor's
    /// next pass on the same queue reads it after it is drawn.
    pub fn render_texture(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
    ) -> Result<wgpu::Texture, GpuError> {
        let (w, h) = (scene.width.max(1), scene.height.max(1));
        // Keep a few attachments between scenes: the least recently used go
        // (only here, so an index planned below stays valid).
        while self.attachments.len() > 4 {
            if let Some((i, _)) = self
                .attachments
                .iter()
                .enumerate()
                .min_by_key(|(_, a)| a.used)
            {
                self.attachments.remove(i);
            }
        }
        let started = std::time::Instant::now();
        let mut compiled = Compiled::new();
        compiled.targets.push(Target {
            rect: IRect {
                x: 0,
                y: 0,
                w: w as i32,
                h: h as i32,
            },
        });
        compiled.ops.push(Op::Clear { target: 0 });
        compiled.tessellate_ahead(&scene.items);
        compiled.items(&scene.items, 0);
        let tessellate_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = std::time::Instant::now();
        let device = &ctx.device;
        let make_target = |tw: u32, th: u32, label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: tw.max(1),
                    height: th.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let frame = make_target(w, h, "promo-svg-frame");
        let views: Vec<wgpu::TextureView> = compiled
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == 0 {
                    frame.create_view(&Default::default())
                } else {
                    make_target(t.rect.w as u32, t.rect.h as u32, "promo-svg-layer")
                        .create_view(&Default::default())
                }
            })
            .collect();

        // The passes, planned: each run's tiles and attachments, and the
        // uniform slots every pass reads.
        enum Planned {
            Clear(usize),
            Tile {
                target: usize,
                tile: IRect,
                att: usize,
                direct: bool,
                calls: Vec<Call>,
                slot: u64,
            },
            Composite {
                target: usize,
                src: Src,
                mask: Option<usize>,
                slot: u64,
            },
        }
        #[derive(Clone, Copy)]
        enum Src {
            Target(usize),
            Attachment(usize),
            Raster(usize),
        }
        let mut slots: Vec<[f32; 12]> = Vec::new();
        let mut slot = |data: [f32; 12]| {
            slots.push(data);
            (slots.len() - 1) as u64
        };
        let mut planned: Vec<Planned> = Vec::new();
        let mut fresh: Vec<bool> = vec![true; compiled.targets.len()];
        let ops = std::mem::take(&mut compiled.ops);
        for op in ops {
            match op {
                Op::Clear { target } => planned.push(Planned::Clear(target)),
                Op::Run {
                    target,
                    rect,
                    calls,
                } => {
                    let trect = compiled.targets[target].rect;
                    let whole = rect == trect;
                    let direct = fresh[target]
                        && whole
                        && trect.w as u32 <= MAX_TILE
                        && trect.h as u32 <= MAX_TILE;
                    fresh[target] = false;
                    let mut ty = rect.y;
                    while ty < rect.y + rect.h {
                        let th = (rect.y + rect.h - ty).min(MAX_TILE as i32);
                        let mut tx = rect.x;
                        while tx < rect.x + rect.w {
                            let tw = (rect.x + rect.w - tx).min(MAX_TILE as i32);
                            let tile = IRect {
                                x: tx,
                                y: ty,
                                w: tw,
                                h: th,
                            };
                            let tile_calls: Vec<Call> = calls
                                .iter()
                                .filter(|c| c.bbox.intersect(tile).is_some())
                                .copied()
                                .collect();
                            if !tile_calls.is_empty() {
                                let att = self.attachments(ctx, tw as u32, th as u32, direct);
                                // A pass that resolves over the whole target
                                // makes its clear redundant.
                                if direct {
                                    if let Some(Planned::Clear(t)) = planned.last() {
                                        if *t == target {
                                            planned.pop();
                                        }
                                    }
                                }
                                let s = slot([
                                    tile.x as f32,
                                    tile.y as f32,
                                    tile.w as f32,
                                    tile.h as f32,
                                    0.0,
                                    0.0,
                                    0.0,
                                    0.0,
                                    0.0,
                                    0.0,
                                    0.0,
                                    0.0,
                                ]);
                                planned.push(Planned::Tile {
                                    target,
                                    tile,
                                    att,
                                    direct,
                                    calls: tile_calls,
                                    slot: s,
                                });
                                if !direct {
                                    let cs =
                                        composite_slot(&mut slot, trect, tile, [0, 0], 1.0, false);
                                    planned.push(Planned::Composite {
                                        target,
                                        src: Src::Attachment(att),
                                        mask: None,
                                        slot: cs,
                                    });
                                }
                            }
                            tx += tw;
                        }
                        ty += th;
                    }
                }
                Op::Raster { target, raster } => {
                    fresh[target] = false;
                    let r = compiled.rasters[raster];
                    let rrect = IRect {
                        x: r.x,
                        y: r.y,
                        w: r.width as i32,
                        h: r.height as i32,
                    };
                    let trect = compiled.targets[target].rect;
                    if let Some(rect) = rrect.intersect(trect) {
                        let cs = composite_slot(
                            &mut slot,
                            trect,
                            rect,
                            [rect.x - r.x, rect.y - r.y],
                            1.0,
                            false,
                        );
                        planned.push(Planned::Composite {
                            target,
                            src: Src::Raster(raster),
                            mask: None,
                            slot: cs,
                        });
                    }
                }
                Op::Composite {
                    target,
                    src,
                    opacity,
                    mask,
                } => {
                    fresh[target] = false;
                    let trect = compiled.targets[target].rect;
                    let rect = compiled.targets[src].rect;
                    let cs =
                        composite_slot(&mut slot, trect, rect, [0, 0], opacity, mask.is_some());
                    planned.push(Planned::Composite {
                        target,
                        src: Src::Target(src),
                        mask,
                        slot: cs,
                    });
                }
            }
        }

        // Buffers.
        if compiled.paints.is_empty() {
            compiled.paints.push(GpuPaint::default());
        }
        if compiled.stops.is_empty() {
            compiled.stops.push(GpuStop::default());
        }
        let buffer = |label, bytes: &[u8], usage| {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (bytes.len() as u64).max(16).div_ceil(4) * 4,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !bytes.is_empty() {
                ctx.queue.write_buffer(&b, 0, bytes);
            }
            b
        };
        let vbuf = buffer(
            "promo-svg-vertices",
            as_bytes(&compiled.vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let ibuf = buffer(
            "promo-svg-indices",
            as_bytes(&compiled.indices),
            wgpu::BufferUsages::INDEX,
        );
        let pbuf = buffer(
            "promo-svg-paints",
            as_bytes(&compiled.paints),
            wgpu::BufferUsages::STORAGE,
        );
        let sbuf = buffer(
            "promo-svg-stops",
            as_bytes(&compiled.stops),
            wgpu::BufferUsages::STORAGE,
        );
        let align = self.align;
        let mut uniform_bytes = vec![0u8; (slots.len().max(1) as u64 * align) as usize];
        for (i, s) in slots.iter().enumerate() {
            let at = (i as u64 * align) as usize;
            uniform_bytes[at..at + 48].copy_from_slice(as_bytes(s));
        }
        let ubuf = buffer(
            "promo-svg-uniforms",
            &uniform_bytes,
            wgpu::BufferUsages::UNIFORM,
        );
        let ubinding = wgpu::BufferBinding {
            buffer: &ubuf,
            offset: 0,
            size: std::num::NonZeroU64::new(48),
        };
        let run_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("promo-svg-run"),
            layout: &self.run_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(ubinding.clone()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: pbuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: sbuf.as_entire_binding(),
                },
            ],
        });
        // Raster pieces, uploaded.
        let raster_views: Vec<wgpu::TextureView> = compiled
            .rasters
            .iter()
            .map(|r| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("promo-svg-raster"),
                    size: wgpu::Extent3d {
                        width: r.width,
                        height: r.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                ctx.queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &t,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &r.rgba[..(r.width * r.height * 4) as usize],
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(r.width * 4),
                        rows_per_image: Some(r.height),
                    },
                    wgpu::Extent3d {
                        width: r.width,
                        height: r.height,
                        depth_or_array_layers: 1,
                    },
                );
                t.create_view(&Default::default())
            })
            .collect();

        let mut stats = SceneStats {
            triangles: compiled.indices.len() / 3,
            tessellate_ms,
            ..Default::default()
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("promo-svg-scene"),
        });
        for p in &planned {
            stats.passes += 1;
            match p {
                Planned::Clear(target) => {
                    let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("promo-svg-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &views[*target],
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                }
                Planned::Tile {
                    target,
                    tile,
                    att,
                    direct,
                    calls,
                    slot,
                } => {
                    let a = &self.attachments[*att];
                    let resolve = if *direct {
                        &views[*target]
                    } else {
                        &a.resolve_view
                    };
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("promo-svg-run"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &a.color,
                            resolve_target: Some(resolve),
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Discard,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &a.stencil,
                            depth_ops: None,
                            stencil_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0),
                                store: wgpu::StoreOp::Discard,
                            }),
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_viewport(0.0, 0.0, tile.w as f32, tile.h as f32, 0.0, 1.0);
                    pass.set_scissor_rect(0, 0, tile.w as u32, tile.h as u32);
                    pass.set_bind_group(0, &run_bind, &[(*slot * align) as u32]);
                    pass.set_vertex_buffer(0, vbuf.slice(..));
                    pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    let mut current: Option<Pipe> = None;
                    for c in calls {
                        if current != Some(c.pipe) {
                            pass.set_pipeline(self.pipeline(c.pipe));
                            current = Some(c.pipe);
                        }
                        pass.set_stencil_reference(c.stencil);
                        pass.draw_indexed(c.start..c.start + c.count, 0, 0..1);
                        stats.draw_calls += 1;
                    }
                }
                Planned::Composite {
                    target,
                    src,
                    mask,
                    slot,
                } => {
                    let src_view = match src {
                        Src::Target(i) => &views[*i],
                        Src::Attachment(i) => &self.attachments[*i].resolve_view,
                        Src::Raster(i) => &raster_views[*i],
                    };
                    let mask_view = mask.map(|m| &views[m]).unwrap_or(&self.blank);
                    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("promo-svg-composite"),
                        layout: &self.composite_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(ubinding.clone()),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(src_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(mask_view),
                            },
                        ],
                    });
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("promo-svg-composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &views[*target],
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_pipeline(&self.composite);
                    pass.set_bind_group(0, &bind, &[(*slot * align) as u32]);
                    pass.draw(0..6, 0..1);
                    stats.draw_calls += 1;
                }
            }
        }
        ctx.queue.submit([encoder.finish()]);
        stats.encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.stats = stats;
        Ok(frame)
    }
}

/// A composite's uniform slot: `rect` in scene pixels drawn into a target
/// at `trect`, sampling the source from `src_offset`.
fn composite_slot(
    slot: &mut impl FnMut([f32; 12]) -> u64,
    trect: IRect,
    rect: IRect,
    src_offset: [i32; 2],
    opacity: f32,
    use_mask: bool,
) -> u64 {
    slot([
        (rect.x - trect.x) as f32,
        (rect.y - trect.y) as f32,
        rect.w as f32,
        rect.h as f32,
        trect.w as f32,
        trect.h as f32,
        src_offset[0] as f32,
        src_offset[1] as f32,
        opacity,
        f32::from_bits(use_mask as u32),
        0.0,
        0.0,
    ])
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    // SAFETY: plain-old-data `#[repr(C)]` structs and numbers only.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// Reads a `Bgra8Unorm` texture back, tightly packed.
pub fn read_texture(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, GpuError> {
    let padded = (width * 4).div_ceil(256) * 256;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("promo-svg-readback"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| GpuError::Readback("no reply".into()))?
        .map_err(|e| GpuError::Readback(format!("{e:?}")))?;
    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + (width * 4) as usize]);
    }
    drop(data);
    buffer.unmap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_segs(x: f32, y: f32, w: f32, h: f32) -> Vec<Seg> {
        vec![
            Seg::Move([x, y]),
            Seg::Line([x + w, y]),
            Seg::Line([x + w, y + h]),
            Seg::Line([x, y + h]),
            Seg::Close,
        ]
    }

    fn solid(c: [f32; 4]) -> Paint {
        Paint::Solid([c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]])
    }

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    /// Pixel-aligned rects draw exact colours; a later shape covers an
    /// earlier one; a pair under an opacity takes the pair's composite
    /// once (not the stroke blended over the faded fill).
    #[test]
    fn shapes_pairs_layers_and_rasters_compose_in_order() {
        let Some(ctx) = crate::gpu_for_test() else {
            return;
        };
        let mut r = SceneRenderer::new(ctx).unwrap();
        let scene = Scene {
            width: 64,
            height: 32,
            items: vec![
                Item::Draw {
                    shapes: vec![Shape {
                        segs: rect_segs(0.0, 0.0, 32.0, 32.0),
                        even_odd: false,
                        paint: solid([1.0, 0.0, 0.0, 1.0]),
                    }],
                    opacity: 1.0,
                },
                Item::Draw {
                    shapes: vec![
                        Shape {
                            segs: rect_segs(4.0, 4.0, 16.0, 16.0),
                            even_odd: false,
                            paint: solid([0.0, 0.0, 1.0, 1.0]),
                        },
                        Shape {
                            segs: rect_segs(12.0, 4.0, 16.0, 16.0),
                            even_odd: false,
                            paint: solid([0.0, 1.0, 0.0, 1.0]),
                        },
                    ],
                    opacity: 0.5,
                },
                Item::Raster(Raster {
                    x: 40,
                    y: 8,
                    width: 2,
                    height: 2,
                    rgba: std::sync::Arc::new(vec![
                        0, 0, 128, 128, 0, 0, 128, 128, 0, 0, 128, 128, 0, 0, 128, 128,
                    ]),
                }),
                Item::Layer(Layer {
                    x: 48,
                    y: 0,
                    width: 16,
                    height: 32,
                    opacity: 1.0,
                    clip: Some(vec![ClipShape {
                        segs: rect_segs(48.0, 0.0, 8.0, 32.0),
                        even_odd: false,
                    }]),
                    items: vec![Item::Draw {
                        shapes: vec![Shape {
                            segs: rect_segs(40.0, 0.0, 24.0, 32.0),
                            even_odd: false,
                            paint: solid([1.0, 1.0, 1.0, 1.0]),
                        }],
                        opacity: 1.0,
                    }],
                }),
            ],
        };
        let img = r.render_pixels(ctx, &scene).unwrap();
        assert_eq!(px(&img, 64, 1, 1), [255, 0, 0, 255]);
        // The blue alone, at half over red: 0.5 blue + 0.5 red.
        assert_eq!(px(&img, 64, 6, 10), [128, 0, 128, 255]);
        // Where green covers blue, green alone at half.
        let both = px(&img, 64, 16, 10);
        assert_eq!(both, [128, 128, 0, 255], "the pair's composite, once");
        assert_eq!(px(&img, 64, 24, 10), [128, 128, 0, 255]);
        assert_eq!(px(&img, 64, 40, 8), [0, 0, 128, 128]);
        assert_eq!(px(&img, 64, 50, 5), [255, 255, 255, 255]);
        assert_eq!(px(&img, 64, 60, 5), [0, 0, 0, 0], "clipped away");
        assert_eq!(px(&img, 64, 44, 20), [0, 0, 0, 0], "outside the layer");
    }

    /// A linear gradient across a rect evaluates per pixel centre.
    #[test]
    fn a_linear_gradient_is_evaluated_at_pixel_centres() {
        let Some(ctx) = crate::gpu_for_test() else {
            return;
        };
        let mut r = SceneRenderer::new(ctx).unwrap();
        // t = x / 100.
        let g = Gradient {
            shape: GradientShape::Linear,
            to_unit: [0.01, 0.0, 0.0, 0.01, 0.0, 0.0],
            spread: Spread::Pad,
            clamp: true,
            stops: vec![Stop {
                t: 0.0,
                factor: [1.0, 0.0, -1.0, 0.0],
                bias: [0.0, 0.0, 1.0, 1.0],
            }],
            premultiply: false,
        };
        let scene = Scene {
            width: 100,
            height: 4,
            items: vec![Item::Draw {
                shapes: vec![Shape {
                    segs: rect_segs(0.0, 0.0, 100.0, 4.0),
                    even_odd: false,
                    paint: Paint::Gradient(g),
                }],
                opacity: 1.0,
            }],
        };
        let img = r.render_pixels(ctx, &scene).unwrap();
        // Pixel 50's centre is 50.5 → t = 0.505.
        let p = px(&img, 100, 50, 2);
        assert_eq!(p[0], (0.505f32 * 255.0).round() as u8);
        assert_eq!(p[2], (0.495f32 * 255.0).round() as u8);
        assert_eq!(p[3], 255);
    }

    /// A region larger than one tile draws whole.
    #[test]
    fn a_scene_larger_than_a_tile_draws_whole() {
        let Some(ctx) = crate::gpu_for_test() else {
            return;
        };
        let mut r = SceneRenderer::new(ctx).unwrap();
        let (w, h) = (MAX_TILE + 300, 64);
        let scene = Scene {
            width: w,
            height: h,
            items: vec![
                Item::Raster(Raster {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    rgba: std::sync::Arc::new(vec![0, 0, 0, 255]),
                }),
                Item::Draw {
                    shapes: vec![Shape {
                        segs: rect_segs(0.0, 0.0, w as f32, h as f32),
                        even_odd: false,
                        paint: solid([0.0, 1.0, 0.0, 1.0]),
                    }],
                    opacity: 1.0,
                },
            ],
        };
        let img = r.render_pixels(ctx, &scene).unwrap();
        for x in [0, 10, MAX_TILE - 1, MAX_TILE, w - 1] {
            assert_eq!(px(&img, w, x, 30), [0, 255, 0, 255], "x {x}");
        }
    }
}
