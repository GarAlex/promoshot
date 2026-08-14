//! The GPU compositor (Phase 2): renders one composition frame — background,
//! z-ordered textured quads with rotation / corner radius / inside border /
//! opacity, letterboxed into the output — with one render pass.
//!
//! Parity contract: mirrors the Core Graphics compositor in the Swift app
//! (`VideoComposer` + `LayerLayout`). Blending happens on sRGB-encoded
//! values (`Bgra8Unorm`, non-sRGB view) with premultiplied source-over,
//! exactly like CG in device space; edges are ~1px analytically antialiased
//! via a rounded-rect SDF (CG antialiases too — golden tests are
//! tolerance-based).
//!
//! Text, vector drawings, and device frames arrive as pre-rasterized quads
//! (the CG pipeline bakes device frames into layer bitmaps as well); the 3D
//! slab shader is a later phase.

use crate::{GpuContext, GpuError};

/// One layer quad. `rect` is canvas-space (top-left origin, y down), matching
/// `LayerLayout.mediaRect`. Colors are non-premultiplied sRGB.
#[derive(Debug, Clone, Copy)]
pub struct SceneQuad {
    /// Index into the textures passed to `compose`, or `None` for a solid
    /// fill of `solid_rgba` (background rects, color mattes).
    pub texture: Option<usize>,
    pub rect: [f64; 4],
    /// Clockwise degrees about the rect center (CG `withRotation`).
    pub rotation_deg: f64,
    /// Corner radius in canvas px (`LayerLayout.mediaCornerRadius` result).
    pub corner_radius: f64,
    /// Inside border stroke width in canvas px (0 = none).
    pub border_width: f64,
    pub border_rgba: [f32; 4],
    pub solid_rgba: [f32; 4],
    pub opacity: f32,
    /// Texture carries BT.709-encoded values (decoded video): the shader
    /// re-encodes to sRGB after sampling, replacing the host's per-frame
    /// CIContext conversion pass.
    pub color_709: bool,
}

impl Default for SceneQuad {
    fn default() -> Self {
        SceneQuad {
            texture: None,
            rect: [0.0; 4],
            rotation_deg: 0.0,
            corner_radius: 0.0,
            border_width: 0.0,
            border_rgba: [0.0; 4],
            solid_rgba: [0.0; 4],
            opacity: 1.0,
            color_709: false,
        }
    }
}

/// A full frame description. The canvas is aspect-fit into the output
/// (letterbox), bars filled with `bars_rgba`; `background_rgba` fills the
/// canvas region itself. Quads render in array order (z-order).
#[derive(Debug, Clone)]
pub struct Scene {
    pub canvas_width: f64,
    pub canvas_height: f64,
    pub background_rgba: [f32; 4],
    pub output_width: u32,
    pub output_height: u32,
    pub bars_rgba: [f32; 4],
    pub quads: Vec<SceneQuad>,
}

const SHADER: &str = r#"
struct Globals {
    // xy = letterbox scale (uniform), zw = letterbox offset in output px.
    fit: vec4<f32>,
    // xy = output size in px.
    output_size: vec4<f32>,
};

struct Quad {
    // xy = rect origin (canvas px), zw = rect size.
    rect: vec4<f32>,
    // x = cos(rot), y = sin(rot), z = corner radius, w = border width.
    rot_radius_border: vec4<f32>,
    border_color: vec4<f32>,
    solid_color: vec4<f32>,
    // x = opacity, y = 1 textured / 0 solid, z = 1 BT.709-encoded texture.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> quad: Quad;
@group(1) @binding(1) var quad_tex: texture_2d<f32>;
@group(1) @binding(2) var quad_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Rect-local position in canvas px (unrotated space).
    @location(0) local: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Triangle-strip unit quad.
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0),
    );
    let unit = corners[vi];
    let size = quad.rect.zw;
    let local = unit * size;

    // Rotate about the rect center in canvas space (y-down, CW positive —
    // identical to CGContext.rotate in the composer's top-left space).
    let center = quad.rect.xy + size * 0.5;
    let p = local + quad.rect.xy - center;
    let c = quad.rot_radius_border.x;
    let s = quad.rot_radius_border.y;
    let rotated = vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c) + center;

    // Canvas -> output px (letterbox), then -> NDC (y flip).
    let out_px = rotated * globals.fit.xy + globals.fit.zw;
    let ndc = vec2<f32>(
        out_px.x / globals.output_size.x * 2.0 - 1.0,
        1.0 - out_px.y / globals.output_size.y * 2.0,
    );

    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.local = local;
    return out;
}

// Rec.709 video -> sRGB, matching Apple's Rec. 709 ICC as ColorSync/CI
// apply it: linearize with a PURE gamma 1.961 (measured against an actual
// CIContext render of a 709-tagged gray ramp to sRGB — the piecewise
// BT.709 OETF is NOT what CI uses and diverges by up to 13/255 in the
// shadows), then the standard sRGB encode.
fn bt709_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lin = pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.961));
    let lo = lin * 12.92;
    let hi = 1.055 * pow(lin, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, lin <= vec3<f32>(0.0031308));
}

// Signed distance to a rounded rect centered at `half_size` with `radius`.
fn sd_round_rect(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p - half_size) - half_size + vec2<f32>(radius, radius);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let size = quad.rect.zw;
    let half = size * 0.5;
    // Radius clamped like the CG path (cannot exceed half the smaller side).
    let radius = min(quad.rot_radius_border.z, min(half.x, half.y));
    let aa = 1.0;

    let d_outer = sd_round_rect(in.local, half, radius);
    let coverage = clamp(0.5 - d_outer / aa, 0.0, 1.0);
    if coverage <= 0.0 {
        discard;
    }

    var color: vec4<f32>;
    if quad.params.y > 0.5 {
        color = textureSample(quad_tex, quad_samp, in.local / size);
        if quad.params.z > 0.5 {
            // Video frames are opaque (alpha 1), so premultiplied rgb == rgb.
            color = vec4<f32>(bt709_to_srgb(color.rgb), color.a);
        }
    } else {
        color = quad.solid_color;
        color = vec4<f32>(color.rgb * color.a, color.a);
    }

    // Inside border: ring between the outer edge and the inset rounded rect.
    let bw = quad.rot_radius_border.w;
    if bw > 0.0 {
        let inner_radius = max(radius - bw, 0.0);
        let d_inner = sd_round_rect(
            in.local - vec2<f32>(bw, bw), half - vec2<f32>(bw, bw), inner_radius);
        let border_cov = clamp(0.5 + d_inner / aa, 0.0, 1.0);
        let b = quad.border_color;
        let border_pm = vec4<f32>(b.rgb * b.a, b.a) * border_cov;
        // border over content (premultiplied source-over).
        color = border_pm + color * (1.0 - border_pm.a);
    }

    return color * coverage * quad.params.x;
}
"#;

/// Uniform layouts (std140-compatible: all vec4s).
#[repr(C)]
#[derive(Clone, Copy)]
struct GlobalsRaw {
    fit: [f32; 4],
    output_size: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct QuadRaw {
    rect: [f32; 4],
    rot_radius_border: [f32; 4],
    border_color: [f32; 4],
    solid_color: [f32; 4],
    params: [f32; 4],
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

/// An input texture the compositor can sample (adopted zero-copy from an
/// IOSurface on macOS, or uploaded from CPU bytes in tests / other hosts).
/// Clonable (Arc-backed) because the compositor's adoption cache hands the
/// same texture out across frames; wgpu 23 resources aren't Clone themselves.
#[derive(Clone)]
pub struct InputTexture {
    view: std::sync::Arc<wgpu::TextureView>,
    /// Identity for bind-group caching (monotonic, never reused).
    id: u64,
    // Keeps the wgpu texture (and through it the Metal adoption) alive.
    _texture: std::sync::Arc<wgpu::Texture>,
}

fn next_texture_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// An in-flight GPU submission from a deferred compose. Wrapping wgpu's
/// index keeps the wgpu types out of dependent crates (promo-ffi holds these
/// as opaque tokens).
pub struct Fence(wgpu::SubmissionIndex);

impl Fence {
    /// Blocks until this submission finishes.
    pub fn wait(self, ctx: &GpuContext) {
        ctx.device
            .poll(wgpu::Maintain::WaitForSubmissionIndex(self.0));
    }
}

/// Uniform stride for the per-quad block, padded to the alignment every
/// backend accepts for dynamic offsets (256 B).
const QUAD_STRIDE: u64 = 256;

/// The persistent compositor: pipeline, sampler, and the GPU resources
/// reused across frames. Creating a uniform buffer and a bind group per
/// quad per frame (the first implementation) costs millions of driver
/// allocations across a long export; these are allocated once and reused.
pub struct Compositor {
    pipeline: wgpu::RenderPipeline,
    quad_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy: InputTexture,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    /// Quad uniforms for a whole frame, one QUAD_STRIDE block per quad.
    quad_buf: wgpu::Buffer,
    quad_capacity: usize,
    /// One bind group per input texture (keyed by texture identity), valid
    /// until `quad_buf` is reallocated. BOUNDED: a bind group retains its
    /// texture view — and therefore the IOSurface behind it — so an unbounded
    /// map pins every frame's surfaces in memory (measured: +38 GB across a
    /// 600-frame 4K export before this cap existed).
    binds: std::collections::HashMap<u64, wgpu::BindGroup>,
    /// Insertion order for eviction.
    bind_order: std::collections::VecDeque<u64>,
    /// IOSurface→texture adoption cache (macOS/iOS), keyed by IOSurfaceID +
    /// render-attachment flag. Adopting is a per-call Metal object creation
    /// (~100 µs each) that used to run for EVERY input surface of EVERY
    /// frame; static images, the decoder's reusable conversion buffer, and
    /// the writer pool's recycled output buffers all re-adopt the same
    /// surfaces, so a cache turns per-frame imports into lookups — and keeps
    /// texture ids stable so the bind-group cache survives across frames.
    /// Cached surfaces are CFRetained until eviction (LRU, bounded).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    imports: std::collections::HashMap<(u32, bool), CachedImport>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    import_order: std::collections::VecDeque<(u32, bool)>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    import_hits: u64,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    import_misses: u64,
    /// When set, compose submits without blocking and stashes the fence in
    /// `last_submission` for the caller to wait on.
    defer_completion: bool,
    last_submission: Option<wgpu::SubmissionIndex>,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
struct CachedImport {
    input: InputTexture,
    /// The adopted texture again, for render-attachment entries.
    texture: std::sync::Arc<wgpu::Texture>,
    width: u32,
    height: u32,
    /// CFRetained; released on eviction/drop.
    surface: crate::iosurface::IOSurfaceRef,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl Drop for Compositor {
    fn drop(&mut self) {
        for (_, entry) in self.imports.drain() {
            unsafe { core_foundation::base::CFRelease(entry.surface as _) };
        }
    }
}

impl Compositor {
    pub fn new(ctx: &GpuContext) -> Result<Self, GpuError> {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let quad_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<QuadRaw>() as u64
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compositor"),
            bind_group_layouts: &[&globals_layout, &quad_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compositor"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8Unorm,
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
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("compositor"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let dummy = Self::upload_texture(ctx, &[255, 255, 255, 255], 1, 1)?;

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<GlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let quad_capacity = 32;
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quads"),
            size: QUAD_STRIDE * quad_capacity as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            pipeline,
            quad_layout,
            sampler,
            dummy,
            globals_buf,
            globals_bind,
            quad_buf,
            quad_capacity,
            binds: std::collections::HashMap::new(),
            bind_order: std::collections::VecDeque::new(),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            imports: std::collections::HashMap::new(),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            import_order: std::collections::VecDeque::new(),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            import_hits: 0,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            import_misses: 0,
            defer_completion: false,
            last_submission: None,
        })
    }

    /// Composes without blocking on GPU completion: each compose stashes its
    /// fence, and `take_fence` hands it to whoever must see finished pixels.
    /// Only for pipelines that separate rendering from reading (the export
    /// producer/consumer split) — stills and preview read immediately and
    /// must keep the default blocking behaviour.
    pub fn set_defer_completion(&mut self, defer: bool) {
        self.defer_completion = defer;
    }

    /// The pending fence for the last deferred compose, if any.
    pub fn take_fence(&mut self) -> Option<Fence> {
        self.last_submission.take().map(Fence)
    }

    /// Bind group for one input texture, created once and reused.
    fn bind_group_for(&mut self, ctx: &GpuContext, texture: Option<&InputTexture>) -> u64 {
        let texture = texture.unwrap_or(&self.dummy);
        let id = texture.id;
        if !self.binds.contains_key(&id) {
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("quad"),
                layout: &self.quad_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.quad_buf,
                            offset: 0,
                            size: wgpu::BufferSize::new(std::mem::size_of::<QuadRaw>() as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&texture.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.binds.insert(id, bind);
            self.bind_order.push_back(id);
        }
        id
    }

    /// Drops cached bind groups BETWEEN frames once the map outgrows the
    /// working set. Evicting mid-frame could remove an entry the frame in
    /// flight still needs, so this runs only at frame boundaries; the few
    /// textures a frame actually uses are re-cached on demand.
    fn trim_bind_cache(&mut self) {
        const MAX_BINDS: usize = 64;
        if self.binds.len() > MAX_BINDS {
            self.binds.clear();
            self.bind_order.clear();
        }
    }

    /// Grows the per-frame quad uniform buffer (invalidates cached bind
    /// groups, which reference it).
    fn ensure_quad_capacity(&mut self, ctx: &GpuContext, needed: usize) {
        if needed <= self.quad_capacity {
            return;
        }
        let capacity = needed.next_power_of_two();
        self.quad_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quads"),
            size: QUAD_STRIDE * capacity as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.quad_capacity = capacity;
        self.binds.clear();
        self.bind_order.clear();
    }

    /// Uploads tightly-packed premultiplied BGRA bytes as a sampleable
    /// texture (test path / small overlays; large frames adopt IOSurfaces).
    pub fn upload_texture(
        ctx: &GpuContext,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<InputTexture, GpuError> {
        if bgra.len() != (width * height * 4) as usize {
            return Err(GpuError::Import("upload_texture: size mismatch".into()));
        }
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("compositor-upload"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bgra,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        Ok(InputTexture {
            view: std::sync::Arc::new(view),
            id: next_texture_id(),
            _texture: std::sync::Arc::new(texture),
        })
    }

    /// Import any [`GpuSurface`](crate::GpuSurface) — the single entry point
    /// the engine uses, so nothing above this layer names a platform surface.
    ///
    /// `IoSurface` adopts zero-copy and retains for the frame's lifetime;
    /// `CpuPixels` uploads (repacking first if the rows are padded). The
    /// remaining variants are named by the enum so capability negotiation can
    /// see them, and rejected here until their platform lands.
    pub fn import(
        ctx: &GpuContext,
        surface: &crate::GpuSurface,
    ) -> Result<crate::ImportedFrame, GpuError> {
        use crate::surface::KeepAlive;
        match surface {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            crate::GpuSurface::IoSurface { raw } => {
                let raw = *raw;
                if raw.is_null() {
                    return Err(GpuError::Import("import: null IOSurface".into()));
                }
                // Retain BEFORE adopting: the texture is a view onto these
                // bytes, so the surface must not be freed under it.
                unsafe { crate::iosurface::retain(raw) };
                let (width, height) = unsafe { crate::iosurface::dimensions(raw) };
                match Self::import_iosurface(ctx, raw, width, height) {
                    Ok(texture) => Ok(crate::ImportedFrame::owning(
                        texture,
                        width,
                        height,
                        KeepAlive::IoSurface(raw),
                    )),
                    Err(e) => {
                        unsafe { crate::iosurface::release(raw) };
                        Err(e)
                    }
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            crate::GpuSurface::IoSurface { .. } => Err(GpuError::Import(
                "import: IOSurface is Apple-only".into(),
            )),
            crate::GpuSurface::CpuPixels {
                data,
                width,
                height,
                bytes_per_row,
            } => {
                let (w, h, stride) = (*width, *height, *bytes_per_row);
                let tight = (w as usize) * 4;
                if stride as usize == tight {
                    let texture = Self::upload_texture(ctx, data, w, h)?;
                    Ok(crate::ImportedFrame::owning(texture, w, h, KeepAlive::Nothing))
                } else {
                    // Padded rows (a decoder's natural stride) — repack once
                    // rather than teaching every caller about alignment.
                    if data.len() < stride as usize * h as usize {
                        return Err(GpuError::Import("import: pixel buffer short".into()));
                    }
                    let mut packed = Vec::with_capacity(tight * h as usize);
                    for row in 0..h as usize {
                        let start = row * stride as usize;
                        packed.extend_from_slice(&data[start..start + tight]);
                    }
                    let texture = Self::upload_texture(ctx, &packed, w, h)?;
                    Ok(crate::ImportedFrame::owning(texture, w, h, KeepAlive::Nothing))
                }
            }
            crate::GpuSurface::DmaBuf { .. } => Err(GpuError::Import(
                "import: DMA-BUF import not implemented (Linux VAAPI path)".into(),
            )),
            crate::GpuSurface::D3DSharedHandle { .. } => Err(GpuError::Import(
                "import: D3D shared-handle import not implemented (Windows path)".into(),
            )),
        }
    }

    /// Adopts an IOSurface as a sampleable input texture (zero-copy, macOS).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn import_iosurface(
        ctx: &GpuContext,
        surface: crate::iosurface::IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> Result<InputTexture, GpuError> {
        let texture = adopt_iosurface(
            ctx,
            surface,
            width,
            height,
            wgpu::TextureUsages::TEXTURE_BINDING,
        )?;
        let view = texture.create_view(&Default::default());
        Ok(InputTexture {
            view: std::sync::Arc::new(view),
            id: next_texture_id(),
            _texture: std::sync::Arc::new(texture),
        })
    }

    /// Renders `scene` into `output` (a render-attachment texture of the
    /// scene's output size) and waits for completion.
    pub fn compose_to_texture(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
        textures: &[InputTexture],
        output: &wgpu::Texture,
    ) -> Result<(), GpuError> {
        let refs: Vec<&InputTexture> = textures.iter().collect();
        self.compose_to_texture_borrowed(ctx, scene, &refs, output)
    }

    /// Like `compose_to_texture`, but over borrowed textures — callers that
    /// keep textures in a cache (the preview engine) compose without moving
    /// or cloning them.
    pub fn compose_to_texture_borrowed(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
        textures: &[&InputTexture],
        output: &wgpu::Texture,
    ) -> Result<(), GpuError> {
        self.trim_bind_cache();
        let (ow, oh) = (scene.output_width as f64, scene.output_height as f64);
        let (cw, ch) = (scene.canvas_width, scene.canvas_height);
        // Letterbox transform (same math as VideoComposer.letterboxTransform).
        let (scale, off_x, off_y) = if cw > 0.0 && ch > 0.0 {
            let s = (ow / cw).min(oh / ch);
            (s, (ow - cw * s) / 2.0, (oh - ch * s) / 2.0)
        } else {
            (1.0, 0.0, 0.0)
        };
        let globals = GlobalsRaw {
            fit: [scale as f32, scale as f32, off_x as f32, off_y as f32],
            output_size: [ow as f32, oh as f32, 0.0, 0.0],
        };
        ctx.queue
            .write_buffer(&self.globals_buf, 0, as_bytes(&globals));

        // Background canvas rect renders as the first solid quad.
        let mut quads = Vec::with_capacity(scene.quads.len() + 1);
        quads.push(SceneQuad {
            texture: None,
            rect: [0.0, 0.0, cw, ch],
            solid_rgba: scene.background_rgba,
            ..Default::default()
        });
        quads.extend_from_slice(&scene.quads);
        self.ensure_quad_capacity(ctx, quads.len());

        // One staging write for the whole frame's quad uniforms.
        let mut staging = vec![0u8; QUAD_STRIDE as usize * quads.len()];
        let mut binds: Vec<u64> = Vec::with_capacity(quads.len());
        for (i, q) in quads.iter().enumerate() {
            let rot = q.rotation_deg.to_radians();
            let raw = QuadRaw {
                rect: [
                    q.rect[0] as f32,
                    q.rect[1] as f32,
                    q.rect[2] as f32,
                    q.rect[3] as f32,
                ],
                rot_radius_border: [
                    rot.cos() as f32,
                    rot.sin() as f32,
                    q.corner_radius as f32,
                    q.border_width as f32,
                ],
                border_color: q.border_rgba,
                solid_color: q.solid_rgba,
                params: [
                    q.opacity,
                    if q.texture.is_some() { 1.0 } else { 0.0 },
                    if q.color_709 { 1.0 } else { 0.0 },
                    0.0,
                ],
            };
            let offset = QUAD_STRIDE as usize * i;
            staging[offset..offset + std::mem::size_of::<QuadRaw>()]
                .copy_from_slice(as_bytes(&raw));
            let texture = match q.texture {
                Some(index) => Some(*textures.get(index).ok_or_else(|| {
                    GpuError::Import(format!("texture index {index} out of range"))
                })?),
                None => None,
            };
            binds.push(self.bind_group_for(ctx, texture));
        }
        ctx.queue.write_buffer(&self.quad_buf, 0, &staging);

        let out_view = output.create_view(&Default::default());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose"),
            });
        {
            let bars = scene.bars_rgba;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("compose"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: (bars[0] * bars[3]) as f64,
                            g: (bars[1] * bars[3]) as f64,
                            b: (bars[2] * bars[3]) as f64,
                            a: bars[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.globals_bind, &[]);
            for (i, id) in binds.iter().enumerate() {
                let bind = self.binds.get(id).expect("bind group cached above");
                pass.set_bind_group(1, bind, &[(QUAD_STRIDE as usize * i) as u32]);
                pass.draw(0..4, 0..1);
            }
        }
        let index = ctx.queue.submit([encoder.finish()]);
        if self.defer_completion {
            // The caller takes the fence and waits later (export pipeline:
            // the encoder thread waits just before it reads the frame, so
            // the next frame's decode/compose overlaps this GPU work).
            self.last_submission = Some(index);
        } else {
            ctx.device.poll(wgpu::Maintain::Wait);
            self.last_submission = None;
        }
        Ok(())
    }

    /// Renders `scene` into an IOSurface-backed output (zero-copy, macOS).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn compose_to_iosurface(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
        textures: &[InputTexture],
        output: crate::iosurface::IOSurfaceRef,
    ) -> Result<(), GpuError> {
        let refs: Vec<&InputTexture> = textures.iter().collect();
        self.compose_to_iosurface_borrowed(ctx, scene, &refs, output)
    }

    /// Borrowed-texture variant of `compose_to_iosurface` (macOS).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn compose_to_iosurface_borrowed(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
        textures: &[&InputTexture],
        output: crate::iosurface::IOSurfaceRef,
    ) -> Result<(), GpuError> {
        let texture = self.adopt_cached(
            ctx,
            output,
            scene.output_width,
            scene.output_height,
            true,
        )?.1;
        self.compose_to_texture_borrowed(ctx, scene, textures, &texture)
    }

    /// Cached IOSurface adoption for sampling. Repeat calls with the same
    /// surface return the SAME texture identity, so downstream bind groups
    /// stay valid across frames; the adoption is zero-copy, so cached
    /// textures always show the surface's current contents.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn import_iosurface_cached(
        &mut self,
        ctx: &GpuContext,
        surface: crate::iosurface::IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> Result<InputTexture, GpuError> {
        Ok(self.adopt_cached(ctx, surface, width, height, false)?.0)
    }

    /// (hits, misses) of the adoption cache — perf gates.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn import_cache_stats(&self) -> (u64, u64) {
        (self.import_hits, self.import_misses)
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn adopt_cached(
        &mut self,
        ctx: &GpuContext,
        surface: crate::iosurface::IOSurfaceRef,
        width: u32,
        height: u32,
        render_attachment: bool,
    ) -> Result<(InputTexture, std::sync::Arc<wgpu::Texture>), GpuError> {
        // A cached ID can't be stale: entries CFRetain their surface, and the
        // OS never recycles an ID while a retain is outstanding.
        let key = (unsafe { crate::iosurface::IOSurfaceGetID(surface) }, render_attachment);
        if let Some(entry) = self.imports.get(&key) {
            if entry.width == width && entry.height == height {
                self.import_hits += 1;
                if let Some(pos) = self.import_order.iter().position(|k| *k == key) {
                    self.import_order.remove(pos);
                }
                self.import_order.push_back(key);
                return Ok((entry.input.clone(), entry.texture.clone()));
            }
        }
        self.import_misses += 1;
        let usage = if render_attachment {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        } else {
            wgpu::TextureUsages::TEXTURE_BINDING
        };
        let texture = std::sync::Arc::new(adopt_iosurface(ctx, surface, width, height, usage)?);
        let input = InputTexture {
            view: std::sync::Arc::new(texture.create_view(&Default::default())),
            id: next_texture_id(),
            _texture: texture.clone(),
        };
        unsafe { core_foundation::base::CFRetain(surface as _) };
        if let Some(old) = self.imports.insert(
            key,
            CachedImport {
                input: input.clone(),
                texture: texture.clone(),
                width,
                height,
                surface,
            },
        ) {
            // Same key, different size (surface was replaced): release the
            // stale retain; the key is already in import_order.
            unsafe { core_foundation::base::CFRelease(old.surface as _) };
        } else {
            self.import_order.push_back(key);
        }
        const MAX_IMPORTS: usize = 256;
        while self.imports.len() > MAX_IMPORTS {
            let Some(victim) = self.import_order.pop_front() else { break };
            if let Some(entry) = self.imports.remove(&victim) {
                unsafe { core_foundation::base::CFRelease(entry.surface as _) };
            }
        }
        Ok((input, texture))
    }
}

/// Adopts an IOSurface as a wgpu texture with the given usage (macOS).
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) fn adopt_iosurface(
    ctx: &GpuContext,
    surface: crate::iosurface::IOSurfaceRef,
    width: u32,
    height: u32,
    usage: wgpu::TextureUsages,
) -> Result<wgpu::Texture, GpuError> {
    let metal_device = unsafe {
        ctx.device
            .as_hal::<wgpu::hal::api::Metal, _, _>(|dev| dev.map(|d| d.raw_device().lock().clone()))
            .flatten()
            .ok_or_else(|| GpuError::Import("not a Metal device".into()))?
    };
    let mtl_texture = crate::iosurface::metal_texture_from_iosurface(
        &metal_device,
        surface,
        width as usize,
        height as usize,
    )?;
    let hal_texture = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            mtl_texture,
            wgpu::TextureFormat::Bgra8Unorm,
            metal::MTLTextureType::D2,
            1,
            1,
            wgpu::hal::CopyExtent {
                width,
                height,
                depth: 1,
            },
        )
    };
    Ok(unsafe {
        ctx.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("compositor-iosurface"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage,
                view_formats: &[],
            },
        )
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::iosurface::OwnedIoSurface;

    fn px(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * width + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    fn compose(scene: &Scene, textures: &[InputTexture], ctx: &GpuContext) -> Vec<u8> {
        let mut comp = Compositor::new(ctx).expect("compositor");
        let out =
            OwnedIoSurface::new_bgra(scene.output_width as usize, scene.output_height as usize)
                .expect("output surface");
        comp.compose_to_iosurface(ctx, scene, textures, out.raw())
            .expect("compose");
        out.read_pixels().expect("readback")
    }

    #[test]
    fn background_letterbox_and_solid_quad() {
        let ctx = GpuContext::new().expect("gpu");
        // 100×100 canvas into 200×100 output → pillarbox bars 50px each side.
        let scene = Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [0.0, 1.0, 0.0, 1.0], // green canvas
            output_width: 200,
            output_height: 100,
            bars_rgba: [1.0, 0.0, 0.0, 1.0], // red bars
            quads: vec![SceneQuad {
                rect: [25.0, 25.0, 50.0, 50.0],
                solid_rgba: [0.0, 0.0, 1.0, 1.0], // blue square center
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[], &ctx);
        // BGRA order.
        assert_eq!(px(&pixels, 200, 10, 50), [0, 0, 255, 255], "left bar red");
        assert_eq!(px(&pixels, 200, 190, 50), [0, 0, 255, 255], "right bar red");
        assert_eq!(px(&pixels, 200, 60, 10), [0, 255, 0, 255], "canvas green");
        assert_eq!(px(&pixels, 200, 100, 50), [255, 0, 0, 255], "quad blue");
    }

    #[test]
    fn textured_quad_with_rotation_and_border() {
        let ctx = GpuContext::new().expect("gpu");
        // A solid white 8×8 texture.
        let tex = Compositor::upload_texture(&ctx, &[255u8; 8 * 8 * 4], 8, 8).expect("tex");
        let scene = Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 100,
            output_height: 100,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [30.0, 30.0, 40.0, 40.0],
                rotation_deg: 45.0,
                corner_radius: 6.0,
                border_width: 4.0,
                border_rgba: [1.0, 0.0, 0.0, 1.0],
                opacity: 1.0,
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[tex], &ctx);
        // Center: white texture body.
        assert_eq!(
            px(&pixels, 100, 50, 50),
            [255, 255, 255, 255],
            "center white"
        );
        // Rotated 45°: the quad's top corner sits near (50, 50-28.3) — the
        // unrotated corner (30,30) region is outside the diamond → black.
        assert_eq!(
            px(&pixels, 100, 32, 32),
            [0, 0, 0, 255],
            "corner outside diamond"
        );
        // Border: the rotated right edge's midpoint lands at ~(64.1, 64.1);
        // just inside it the 4px inside-stroke shows the red border.
        let b = px(&pixels, 100, 63, 63);
        assert!(b[2] > 200 && b[1] < 60, "border red on edge, got {b:?}");
    }

    #[test]
    fn iosurface_input_is_sampled() {
        let ctx = GpuContext::new().expect("gpu");
        let input = OwnedIoSurface::new_bgra(4, 4).expect("input");
        // Solid magenta premultiplied BGRA.
        input
            .write_pixels(&[255, 0, 255, 255].repeat(16))
            .expect("write");
        let tex = Compositor::import_iosurface(&ctx, input.raw(), 4, 4).expect("import");
        let scene = Scene {
            canvas_width: 10.0,
            canvas_height: 10.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 10,
            output_height: 10,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 10.0, 10.0],
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[tex], &ctx);
        assert_eq!(px(&pixels, 10, 5, 5), [255, 0, 255, 255], "magenta sampled");
    }

    /// Re-composing with the same IOSurfaces must reuse cached adoptions
    /// (input AND output) instead of re-creating Metal textures per frame,
    /// while live surface contents still flow through the zero-copy wrap.
    #[test]
    fn iosurface_adoption_is_cached_across_frames() {
        let ctx = GpuContext::new().expect("gpu");
        let mut comp = Compositor::new(&ctx).expect("compositor");
        let input = OwnedIoSurface::new_bgra(4, 4).expect("input");
        input.write_pixels(&[255, 0, 255, 255].repeat(16)).expect("write");
        let out = OwnedIoSurface::new_bgra(8, 8).expect("out");
        let scene = Scene {
            canvas_width: 8.0,
            canvas_height: 8.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 8,
            output_height: 8,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 8.0, 8.0],
                ..Default::default()
            }],
        };

        for frame in 0..3 {
            let tex = comp
                .import_iosurface_cached(&ctx, input.raw(), 4, 4)
                .expect("import");
            comp.compose_to_iosurface(&ctx, &scene, std::slice::from_ref(&tex), out.raw())
                .expect("compose");
            let px = out.read_pixels().expect("read");
            assert_eq!(&px[0..4], &[255, 0, 255, 255], "frame {frame} magenta");
        }
        let (hits, misses) = comp.import_cache_stats();
        assert_eq!(misses, 2, "one input + one output adoption, ever");
        assert_eq!(hits, 4, "frames 2 and 3 reuse both adoptions");

        // Contents written after caching must show through (live wrap).
        input.write_pixels(&[0, 255, 0, 255].repeat(16)).expect("write2");
        let tex = comp
            .import_iosurface_cached(&ctx, input.raw(), 4, 4)
            .expect("import");
        comp.compose_to_iosurface(&ctx, &scene, std::slice::from_ref(&tex), out.raw())
            .expect("compose");
        let px = out.read_pixels().expect("read");
        assert_eq!(&px[0..4], &[0, 255, 0, 255], "updated contents sampled");
    }

    /// The bind-group cache must not grow without bound: each entry retains
    /// a texture view (and its IOSurface). Regression for a measured +38 GB.
    #[test]
    fn bind_group_cache_is_bounded() {
        let ctx = GpuContext::new().expect("gpu");
        let mut comp = Compositor::new(&ctx).expect("compositor");
        let out = OwnedIoSurface::new_bgra(16, 16).expect("out");
        // 300 distinct one-off textures, as a long export would produce.
        for i in 0..300 {
            let tex = Compositor::upload_texture(
                &ctx, &[(i % 255) as u8, 0, 0, 255], 1, 1).expect("tex");
            let scene = Scene {
                canvas_width: 16.0,
                canvas_height: 16.0,
                background_rgba: [0.0, 0.0, 0.0, 1.0],
                output_width: 16,
                output_height: 16,
                bars_rgba: [0.0, 0.0, 0.0, 1.0],
                quads: vec![SceneQuad {
                    texture: Some(0),
                    rect: [0.0, 0.0, 16.0, 16.0],
                    ..Default::default()
                }],
            };
            comp.compose_to_iosurface(&ctx, &scene, &[tex], out.raw())
                .expect("compose");
        }
        assert!(
            comp.binds.len() <= 64,
            "bind cache unbounded: {} entries",
            comp.binds.len()
        );
    }

    #[test]
    fn bt709_quad_converts_to_srgb() {
        let ctx = GpuContext::new().expect("gpu");
        // Mid-gray 128/255 in Rec.709 video encoding. CI's measured mapping
        // (gamma-1.961 linearize, sRGB encode) sends 128 -> 139.
        let tex = Compositor::upload_texture(&ctx, &[128, 128, 128, 255], 1, 1).expect("tex");
        let scene = Scene {
            canvas_width: 8.0,
            canvas_height: 8.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 8,
            output_height: 8,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 8.0, 8.0],
                color_709: true,
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[tex], &ctx);
        let got = px(&pixels, 8, 4, 4);
        for ch in 0..3 {
            assert!(
                (got[ch] as i32 - 139).abs() <= 2,
                "709 mid-gray must map to sRGB ~139 (CI-measured), got {got:?}"
            );
        }
        assert_eq!(got[3], 255);

        // Without the flag the value passes through untouched.
        let tex = Compositor::upload_texture(&ctx, &[128, 128, 128, 255], 1, 1).expect("tex");
        let mut plain = scene.clone();
        plain.quads[0].color_709 = false;
        let pixels = compose(&plain, &[tex], &ctx);
        assert_eq!(px(&pixels, 8, 4, 4), [128, 128, 128, 255]);
    }

    #[test]
    fn opacity_blends_over_background() {
        let ctx = GpuContext::new().expect("gpu");
        let scene = Scene {
            canvas_width: 10.0,
            canvas_height: 10.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 10,
            output_height: 10,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                rect: [0.0, 0.0, 10.0, 10.0],
                solid_rgba: [1.0, 1.0, 1.0, 1.0],
                opacity: 0.5,
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[], &ctx);
        let p = px(&pixels, 10, 5, 5);
        assert!(
            (p[0] as i32 - 128).abs() <= 1,
            "50% white over black, got {p:?}"
        );
    }
}
