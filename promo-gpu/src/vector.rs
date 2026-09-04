//! GPU vector renderer: tessellates drawing shapes (freehand polylines,
//! lines with arrowheads, ellipses, imported SVG paths) into triangle meshes
//! and rasterizes them on the GPU at whatever resolution the frame needs.
//!
//! This replaces host-side Core Graphics rasterization into a fixed-size
//! bitmap. Two wins: drawings stay resolution-independent (a zoom-in renders
//! fresh geometry instead of magnifying pixels), and the CPU stops
//! rasterizing.
//!
//! Parity contract: mirrors `VideoComposer.drawDrawingShape` — per shape,
//! fill first (even-odd or non-zero) then stroke with round caps/joins and
//! `max(0.5, strokeWidth * scale)` width; shapes render in array order.
//! Edges are antialiased with 4× MSAA, resolved into the output texture.

use crate::{GpuContext, GpuError};
use lyon_tessellation::geom::{point, Point};
use lyon_tessellation::path::Path;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, LineCap, LineJoin,
    StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
};

/// Shape kinds, mirroring `DrawingShape.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorShapeKind {
    Pen,
    Line,
    Oval,
    Rect,
}

/// One drawing shape in drawing-space coordinates.
#[derive(Debug, Clone)]
pub struct VectorShape {
    pub kind: VectorShapeKind,
    pub points: Vec<(f64, f64)>,
    /// Non-premultiplied sRGB.
    pub stroke_rgba: [f32; 4],
    pub stroke_width: f64,
    /// `None` = no fill.
    pub fill_rgba: Option<[f32; 4]>,
    pub arrow_start: bool,
    pub arrow_end: bool,
    pub even_odd_fill: bool,
    /// A `Rect`'s corner radius in drawing units; zero is square corners,
    /// and anything larger than half the shorter side is clamped to it, so
    /// a big radius gives a pill rather than a knot. Ignored by every
    /// other kind.
    pub corner_radius: f64,
}

/// The bounding box of every shape's points — the drawing's natural size.
/// Mirrors Swift `DrawingDocument.contentBounds`, including its
/// 1080×1920 fallback for an empty/degenerate document.
pub fn content_bounds(shapes: &[VectorShape]) -> (f64, f64, f64, f64) {
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut any = false;
    for shape in shapes {
        for &(x, y) in &shape.points {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            any = true;
        }
    }
    if !any || max_x <= min_x || max_y <= min_y {
        return (0.0, 0.0, 1080.0, 1920.0);
    }
    (min_x, min_y, max_x - min_x, max_y - min_y)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MeshVertex {
    pos: [f32; 2],
    /// Premultiplied, matching the compositor's blend setup.
    color: [f32; 4],
}

const MESH_SHADER: &str = r#"
struct Globals { size: vec4<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    // Target pixel space (top-left origin, y down) -> NDC.
    out.pos = vec4<f32>(
        pos.x / globals.size.x * 2.0 - 1.0,
        1.0 - pos.y / globals.size.y * 2.0,
        0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// Multisample count for edge antialiasing. CG computes analytic coverage,
/// so MSAA's quantized coverage is the whole parity difference — 4 samples
/// gives 5 coverage levels and visibly missed the stills golden gate on thin
/// strokes; 8 halves that error. Chosen from what the adapter actually
/// supports, highest first.
fn sample_count(ctx: &GpuContext) -> u32 {
    // The adapter's answer only counts when the DEVICE holds the
    // adapter-specific-format-features flag; without it the WebGPU spec's
    // [1, 4] is the whole menu, and asking for 8 anyway is a pipeline
    // refused at creation (it was, on D3D12 — Metal allowed it and hid
    // this for every Apple-side run).
    let adapter_specific = ctx
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    let flags = ctx
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::Bgra8Unorm)
        .flags;
    for count in [8u32, 4, 2] {
        if count > 4 && !adapter_specific {
            continue;
        }
        if flags.sample_count_supported(count) {
            return count;
        }
    }
    1
}

/// Renders vector shapes into textures. Owns the pipeline and reuses its
/// buffers across frames.
pub struct VectorRenderer {
    samples: u32,
    pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    vertex_buf: Option<wgpu::Buffer>,
    vertex_capacity: usize,
    index_buf: Option<wgpu::Buffer>,
    index_capacity: usize,
    /// MSAA target, recreated when the output size changes.
    msaa: Option<(wgpu::TextureView, u32, u32)>,
}

impl VectorRenderer {
    pub fn new(ctx: &GpuContext) -> Result<Self, GpuError> {
        let samples = sample_count(ctx);
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("promo-vector"),
            source: wgpu::ShaderSource::Wgsl(MESH_SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("promo-vector-globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("promo-vector-globals-buf"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("promo-vector-globals-bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("promo-vector-layout"),
            bind_group_layouts: &[&globals_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("promo-vector-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    // Premultiplied source-over, same as the compositor.
                    blend: Some(wgpu::BlendState {
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
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });
        Ok(VectorRenderer {
            samples,
            pipeline,
            globals_buf,
            globals_bind,
            vertex_buf: None,
            vertex_capacity: 0,
            index_buf: None,
            index_capacity: 0,
            msaa: None,
        })
    }

    /// Renders `shapes` into `output` (a `width`×`height` render target),
    /// scaling the drawing's content bounds to fill it — the same mapping
    /// `rasterizeDrawingContent` used.
    pub fn render_to_texture(
        &mut self,
        ctx: &GpuContext,
        shapes: &[VectorShape],
        output: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        let (bx, by, bw, _bh) = content_bounds(shapes);
        let scale = width as f64 / bw.max(1.0);
        let mesh = tessellate(shapes, scale, -bx * scale, -by * scale);
        self.draw_mesh(ctx, &mesh, output, width, height)
    }

    /// Rasterizes `shapes` into a fresh `width`×`height` frame the compositor
    /// can sample directly — no readback, no host surface. This is what lets
    /// the ENGINE draw vector layers itself: every front end used to have to
    /// remember to rasterize drawings and hand them over, and the one that
    /// forgot (the CLI) rendered compositions with the strokes missing.
    pub fn render_to_frame(
        &mut self,
        ctx: &GpuContext,
        shapes: &[VectorShape],
        width: u32,
        height: u32,
    ) -> Result<crate::ImportedFrame, GpuError> {
        let (w, h) = (width.max(1), height.max(1));
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("promo-vector-frame"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.render_to_texture(ctx, shapes, &texture, w, h)?;
        Ok(crate::ImportedFrame::from_owned_texture(texture, w, h))
    }

    /// Renders into a BGRA IOSurface (zero-copy adoption, macOS).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn render_to_iosurface(
        &mut self,
        ctx: &GpuContext,
        shapes: &[VectorShape],
        surface: crate::iosurface::IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        let texture = crate::compositor::adopt_iosurface(
            ctx,
            surface,
            width,
            height,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )?;
        self.render_to_texture(ctx, shapes, &texture, width, height)
    }

    fn ensure_msaa(&mut self, ctx: &GpuContext, width: u32, height: u32) {
        if let Some((_, w, h)) = &self.msaa {
            if *w == width && *h == height {
                return;
            }
        }
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("promo-vector-msaa"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: self.samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.msaa = Some((texture.create_view(&Default::default()), width, height));
    }

    fn draw_mesh(
        &mut self,
        ctx: &GpuContext,
        mesh: &VertexBuffers<MeshVertex, u32>,
        output: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        ctx.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck_cast(&[width as f32, height as f32, 0.0, 0.0]),
        );
        self.ensure_msaa(ctx, width, height);

        if !mesh.indices.is_empty() {
            if self.vertex_capacity < mesh.vertices.len() {
                let capacity = (mesh.vertices.len() * 2).max(1024);
                self.vertex_buf = Some(ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("promo-vector-vertices"),
                    size: (capacity * std::mem::size_of::<MeshVertex>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.vertex_capacity = capacity;
            }
            if self.index_capacity < mesh.indices.len() {
                let capacity = (mesh.indices.len() * 2).max(2048);
                self.index_buf = Some(ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("promo-vector-indices"),
                    size: (capacity * 4) as u64,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.index_capacity = capacity;
            }
            let vbuf = self.vertex_buf.as_ref().expect("vertex buffer");
            let ibuf = self.index_buf.as_ref().expect("index buffer");
            ctx.queue
                .write_buffer(vbuf, 0, vertices_as_bytes(&mesh.vertices));
            ctx.queue
                .write_buffer(ibuf, 0, indices_as_bytes(&mesh.indices));
        }

        let target = output.create_view(&Default::default());
        let (msaa_view, _, _) = self.msaa.as_ref().expect("msaa target");
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("promo-vector-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("promo-vector-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(&target),
                    ops: wgpu::Operations {
                        // Transparent: the drawing composites over the frame.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !mesh.indices.is_empty() {
                let vbuf = self.vertex_buf.as_ref().expect("vertex buffer");
                let ibuf = self.index_buf.as_ref().expect("index buffer");
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.globals_bind, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
            }
        }
        ctx.queue.submit([encoder.finish()]);
        ctx.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }
}

fn vertices_as_bytes(v: &[MeshVertex]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn indices_as_bytes(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn bytemuck_cast(v: &[f32; 4]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, 16) }
}

fn premultiply(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// Builds one shape's path in target pixel space, mirroring the CG path
/// construction (including arrowheads as extra open subpaths).
fn shape_path(shape: &VectorShape, scale: f64, dx: f64, dy: f64) -> Option<Path> {
    let tx = |p: (f64, f64)| point((p.0 * scale + dx) as f32, (p.1 * scale + dy) as f32);
    let mut builder = Path::builder();
    match shape.kind {
        VectorShapeKind::Pen => {
            let first = *shape.points.first()?;
            builder.begin(tx(first));
            for &p in shape.points.iter().skip(1) {
                builder.line_to(tx(p));
            }
            builder.end(false);
        }
        VectorShapeKind::Line => {
            if shape.points.len() < 2 {
                return None;
            }
            let a = tx(shape.points[0]);
            let b = tx(shape.points[1]);
            builder.begin(a);
            builder.line_to(b);
            builder.end(false);
            // Arrowheads: two open strokes back from the tip (CG appends the
            // same two segments to this path).
            let head = (shape.stroke_width * 3.0).max(10.0) * scale;
            if shape.arrow_end {
                arrow_head(&mut builder, b, a, head as f32);
            }
            if shape.arrow_start {
                arrow_head(&mut builder, a, b, head as f32);
            }
        }
        VectorShapeKind::Rect => {
            if shape.points.len() < 2 {
                return None;
            }
            let (a, b) = (shape.points[0], shape.points[1]);
            let min_x = a.0.min(b.0) * scale + dx;
            let min_y = a.1.min(b.1) * scale + dy;
            let w = (b.0 - a.0).abs() * scale;
            let h = (b.1 - a.1).abs() * scale;
            let (max_x, max_y) = (min_x + w, min_y + h);
            let p = |x: f64, y: f64| point(x as f32, y as f32);
            // Half the shorter side is the largest radius that still means
            // something: past it the two corners of a side would overlap.
            let r = (shape.corner_radius * scale).clamp(0.0, w.min(h) / 2.0);
            if r <= 0.0 {
                builder.begin(p(min_x, min_y));
                builder.line_to(p(max_x, min_y));
                builder.line_to(p(max_x, max_y));
                builder.line_to(p(min_x, max_y));
                builder.end(true);
            } else {
                // The same kappa quarter-arcs CGPath's rounded rect uses,
                // so a plate drawn here and one drawn by the app's own
                // Core Graphics path are the same curve.
                const KAPPA: f64 = 0.552_284_749_830_793_4;
                let o = r * KAPPA;
                builder.begin(p(min_x + r, min_y));
                builder.line_to(p(max_x - r, min_y));
                builder.cubic_bezier_to(
                    p(max_x - r + o, min_y),
                    p(max_x, min_y + r - o),
                    p(max_x, min_y + r),
                );
                builder.line_to(p(max_x, max_y - r));
                builder.cubic_bezier_to(
                    p(max_x, max_y - r + o),
                    p(max_x - r + o, max_y),
                    p(max_x - r, max_y),
                );
                builder.line_to(p(min_x + r, max_y));
                builder.cubic_bezier_to(
                    p(min_x + r - o, max_y),
                    p(min_x, max_y - r + o),
                    p(min_x, max_y - r),
                );
                builder.line_to(p(min_x, min_y + r));
                builder.cubic_bezier_to(
                    p(min_x, min_y + r - o),
                    p(min_x + r - o, min_y),
                    p(min_x + r, min_y),
                );
                builder.end(true);
            }
        }
        VectorShapeKind::Oval => {
            if shape.points.len() < 2 {
                return None;
            }
            let (a, b) = (shape.points[0], shape.points[1]);
            let min_x = a.0.min(b.0) * scale + dx;
            let min_y = a.1.min(b.1) * scale + dy;
            let w = (b.0 - a.0).abs() * scale;
            let h = (b.1 - a.1).abs() * scale;
            // Ellipse inscribed in the rect, built from the same four
            // cubic arcs CGPath.addEllipse uses (kappa), so the curve is
            // identical to the CG reference rather than merely similar.
            const KAPPA: f64 = 0.552_284_749_830_793_4;
            let (rx, ry) = (w / 2.0, h / 2.0);
            let (cx, cy) = (min_x + rx, min_y + ry);
            let (ox, oy) = (rx * KAPPA, ry * KAPPA);
            let p = |x: f64, y: f64| point(x as f32, y as f32);
            builder.begin(p(cx + rx, cy));
            builder.cubic_bezier_to(p(cx + rx, cy + oy), p(cx + ox, cy + ry), p(cx, cy + ry));
            builder.cubic_bezier_to(p(cx - ox, cy + ry), p(cx - rx, cy + oy), p(cx - rx, cy));
            builder.cubic_bezier_to(p(cx - rx, cy - oy), p(cx - ox, cy - ry), p(cx, cy - ry));
            builder.cubic_bezier_to(p(cx + ox, cy - ry), p(cx + rx, cy - oy), p(cx + rx, cy));
            builder.end(true);
        }
    }
    Some(builder.build())
}

fn arrow_head(
    builder: &mut lyon_tessellation::path::path::Builder,
    tip: Point<f32>,
    base: Point<f32>,
    size: f32,
) {
    let dx = tip.x - base.x;
    let dy = tip.y - base.y;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let angle = 0.45f32;
    let mut wing = |a: f32| {
        let rx = ux * a.cos() - uy * a.sin();
        let ry = uy * a.cos() + ux * a.sin();
        builder.begin(tip);
        builder.line_to(point(tip.x - rx * size, tip.y - ry * size));
        builder.end(false);
    };
    wing(angle);
    wing(-angle);
}

/// Tessellates every shape into one mesh, fill-then-stroke per shape in
/// array order (the CG draw order).
fn tessellate(
    shapes: &[VectorShape],
    scale: f64,
    dx: f64,
    dy: f64,
) -> VertexBuffers<MeshVertex, u32> {
    let mut mesh: VertexBuffers<MeshVertex, u32> = VertexBuffers::new();
    let mut fill_tess = FillTessellator::new();
    let mut stroke_tess = StrokeTessellator::new();

    for shape in shapes {
        let Some(path) = shape_path(shape, scale, dx, dy) else {
            continue;
        };
        if let Some(fill) = shape.fill_rgba {
            let color = premultiply(fill);
            let options = FillOptions::tolerance(0.1).with_fill_rule(if shape.even_odd_fill {
                FillRule::EvenOdd
            } else {
                FillRule::NonZero
            });
            let _ = fill_tess.tessellate_path(
                &path,
                &options,
                &mut BuffersBuilder::new(&mut mesh, |v: FillVertex| MeshVertex {
                    pos: [v.position().x, v.position().y],
                    color,
                }),
            );
        }
        if shape.stroke_width > 0.0 {
            let color = premultiply(shape.stroke_rgba);
            let width = (shape.stroke_width * scale).max(0.5);
            let options = StrokeOptions::tolerance(0.1)
                .with_line_width(width as f32)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);
            let _ = stroke_tess.tessellate_path(
                &path,
                &options,
                &mut BuffersBuilder::new(&mut mesh, |v: StrokeVertex| MeshVertex {
                    pos: [v.position().x, v.position().y],
                    color,
                }),
            );
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(points: Vec<(f64, f64)>, width: f64) -> VectorShape {
        VectorShape {
            kind: VectorShapeKind::Pen,
            points,
            stroke_rgba: [1.0, 0.0, 0.0, 1.0],
            stroke_width: width,
            fill_rgba: None,
            arrow_start: false,
            arrow_end: false,
            even_odd_fill: false,
            corner_radius: 0.0,
        }
    }

    #[test]
    fn content_bounds_matches_swift_fallback() {
        assert_eq!(content_bounds(&[]), (0.0, 0.0, 1080.0, 1920.0));
        // Degenerate (zero-area) bounds also fall back.
        assert_eq!(
            content_bounds(&[pen(vec![(5.0, 5.0), (5.0, 5.0)], 1.0)]),
            (0.0, 0.0, 1080.0, 1920.0)
        );
        assert_eq!(
            content_bounds(&[pen(vec![(10.0, 20.0), (30.0, 60.0)], 1.0)]),
            (10.0, 20.0, 20.0, 40.0)
        );
    }

    #[test]
    fn stroke_and_fill_produce_geometry() {
        let mut shape = pen(
            vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 0.0)],
            4.0,
        );
        let stroke_only = tessellate(std::slice::from_ref(&shape), 1.0, 0.0, 0.0);
        assert!(!stroke_only.indices.is_empty(), "stroke tessellates");

        shape.fill_rgba = Some([0.0, 0.0, 1.0, 1.0]);
        let filled = tessellate(std::slice::from_ref(&shape), 1.0, 0.0, 0.0);
        assert!(
            filled.indices.len() > stroke_only.indices.len(),
            "fill adds geometry"
        );
        // Premultiplication: opaque blue fill stays [0,0,1,1].
        assert!(filled
            .vertices
            .iter()
            .any(|v| v.color == [0.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn zero_width_stroke_emits_nothing() {
        let mesh = tessellate(&[pen(vec![(0.0, 0.0), (10.0, 10.0)], 0.0)], 1.0, 0.0, 0.0);
        assert!(mesh.indices.is_empty());
    }

    /// A rectangle is a real shape, not a four-point pen stroke: the fill
    /// covers exactly the rect, and a corner radius takes off exactly the
    /// area four quarter-circles leave behind — (4 − π)r², measured from
    /// the tessellated triangles rather than assumed.
    #[test]
    fn a_rect_fills_its_bounds_and_rounds_its_corners() {
        let rect = |radius: f64| VectorShape {
            kind: VectorShapeKind::Rect,
            points: vec![(10.0, 20.0), (210.0, 140.0)],
            stroke_rgba: [1.0, 1.0, 1.0, 1.0],
            stroke_width: 0.0,
            fill_rgba: Some([0.0, 0.0, 1.0, 1.0]),
            arrow_start: false,
            arrow_end: false,
            even_odd_fill: false,
            corner_radius: radius,
        };
        let area = |mesh: &VertexBuffers<MeshVertex, u32>| -> f64 {
            mesh.indices
                .chunks_exact(3)
                .map(|t| {
                    let p = |i: u32| mesh.vertices[i as usize].pos;
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    (((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) as f64).abs()
                        / 2.0
                })
                .sum()
        };
        let square = tessellate(&[rect(0.0)], 1.0, 0.0, 0.0);
        assert!((area(&square) - 200.0 * 120.0).abs() < 1.0, "{}", area(&square));
        let bounds = |mesh: &VertexBuffers<MeshVertex, u32>| {
            let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.pos[0]).collect();
            let ys: Vec<f32> = mesh.vertices.iter().map(|v| v.pos[1]).collect();
            (
                xs.iter().cloned().fold(f32::MAX, f32::min),
                ys.iter().cloned().fold(f32::MAX, f32::min),
                xs.iter().cloned().fold(f32::MIN, f32::max),
                ys.iter().cloned().fold(f32::MIN, f32::max),
            )
        };
        assert_eq!(bounds(&square), (10.0, 20.0, 210.0, 140.0));

        let rounded = tessellate(&[rect(30.0)], 1.0, 0.0, 0.0);
        let corners = (4.0 - std::f64::consts::PI) * 30.0 * 30.0;
        let lost = area(&square) - area(&rounded);
        assert!((lost - corners).abs() < corners * 0.02, "four quarter-circles: {lost} vs {corners}");
        // Still exactly as wide and tall — a radius rounds the corners, it
        // does not inset the shape.
        assert_eq!(bounds(&rounded), (10.0, 20.0, 210.0, 140.0));

        // Half the shorter side is the most a radius can mean: 200 on a
        // 120-tall rect is clamped to 60, which is a stadium, not a knot.
        let pill = tessellate(&[rect(200.0)], 1.0, 0.0, 0.0);
        let stadium = 200.0 * 120.0 - (4.0 - std::f64::consts::PI) * 60.0 * 60.0;
        assert!((area(&pill) - stadium).abs() < stadium * 0.02, "clamped: {}", area(&pill));
        assert_eq!(bounds(&pill), (10.0, 20.0, 210.0, 140.0));
    }

    #[test]
    fn arrowheads_add_geometry() {
        let mut line = VectorShape {
            kind: VectorShapeKind::Line,
            points: vec![(0.0, 0.0), (100.0, 0.0)],
            stroke_rgba: [1.0, 1.0, 1.0, 1.0],
            stroke_width: 3.0,
            fill_rgba: None,
            arrow_start: false,
            arrow_end: false,
            even_odd_fill: false,
            corner_radius: 0.0,
        };
        let plain = tessellate(std::slice::from_ref(&line), 1.0, 0.0, 0.0);
        line.arrow_end = true;
        let arrowed = tessellate(std::slice::from_ref(&line), 1.0, 0.0, 0.0);
        assert!(arrowed.indices.len() > plain.indices.len());
    }
}
