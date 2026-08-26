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
    /// The part of the texture this quad shows, as `[u, v, width, height]`
    /// in 0…1. The whole texture by default; a sprite sheet's current cell
    /// otherwise, which is what lets one resident texture animate without a
    /// re-upload per frame.
    pub uv_rect: [f32; 4],
    /// Sample without smoothing. Pixel art needs it to stay crisp when
    /// scaled, and a sprite sheet needs it for CORRECTNESS: bilinear
    /// sampling near a cell's edge reaches into the neighbouring frame and
    /// bleeds it in.
    pub nearest: bool,
    /// Fill from the frame's background gradient rather than `solid_rgba`.
    /// Set on the background quad alone.
    pub gradient_fill: bool,
    /// Per-layer colour adjustments, applied to THIS quad's own pixels and
    /// nothing else: `[saturation, contrast, brightness, tint_amount]`.
    /// Identity is `[1, 1, 0, 0]` — the shader skips the work then.
    pub adjust: [f32; 4],
    /// The tint's colour (straight, unpremultiplied). Multiplied in at
    /// `tint_amount`: 1 gels the layer fully, 0 leaves it alone.
    pub tint_rgba: [f32; 4],
    /// How this quad combines with what is already drawn.
    pub blend: QuadBlend,
    /// Index of this quad's MASK texture in the textures passed to
    /// `compose`, or `None` for no mask. Sampled in QUAD-LOCAL coordinates
    /// (corner to corner over the rect, ignoring `uv_rect`), its alpha
    /// multiplied into the quad's final colour: the mask is a window fixed
    /// on the canvas while the uv pans the content behind it.
    pub mask: Option<usize>,
    /// Flips the mask: ink becomes the hole instead of the window.
    pub mask_invert: bool,
    /// The window's VERTICAL scale. Separate from `mask_transform[2]` so a
    /// mask can be stretched deliberately; equal to it means "unstretched".
    pub mask_zoom_y: f32,
    /// Half-size, in quad-local px, of the box the mask's own proportions
    /// occupy inside the rect — the drawing aspect-fitted and centred. This
    /// is what keeps a circle a circle on a layer of any shape. `[0, 0]`
    /// means the old behaviour: stretch corner to corner over the rect.
    pub mask_half: [f32; 2],
    /// The mask's own placement over the rect: `[dx, dy, zoom, rotation_deg]`
    /// in rect-local px (canvas px on an unrotated layer), zoom and spin
    /// about the flown centre. Identity `[0, 0, 1, 0]` samples exactly as
    /// before — the window sits where the rect put it.
    pub mask_transform: [f32; 4],
    /// Soft-edge length in canvas px: 0 keeps the crisp ~1px analytic AA,
    /// anything more turns the edge into a smoothstep penumbra fading to
    /// nothing at the rect's edge. This is how a drop-shadow quad is drawn:
    /// a solid rounded rect whose rect and radius arrive pre-inflated by
    /// half the penumbra (rounded rects are closed under offsetting, so the
    /// falloff bands match the true rect's outside distance exactly).
    pub edge_soften: f64,
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
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            nearest: false,
            gradient_fill: false,
            adjust: [1.0, 1.0, 0.0, 0.0],
            tint_rgba: [1.0, 1.0, 1.0, 1.0],
            blend: QuadBlend::Normal,
            mask: None,
            mask_invert: false,
            mask_zoom_y: 1.0,
            mask_half: [0.0, 0.0],
            mask_transform: [0.0, 0.0, 1.0, 0.0],
            edge_soften: 0.0,
        }
    }
}

/// The blend functions the compositor can draw with — the ones a
/// fixed-function blend state can express. Overlay and friends branch on
/// the destination and need a different architecture; these three cover
/// the overlay-asset cases (black-backed light on `Screen`/`Add`,
/// white-backed texture on `Multiply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QuadBlend {
    #[default]
    Normal,
    Multiply,
    Screen,
    Add,
}

/// A full frame description. The canvas is aspect-fit into the output
/// (letterbox), bars filled with `bars_rgba`; `background_rgba` fills the
/// canvas region itself. Quads render in array order (z-order).
#[derive(Debug, Clone)]
pub struct Scene {
    pub canvas_width: f64,
    pub canvas_height: f64,
    pub background_rgba: [f32; 4],
    /// The background's gradient, already resolved for this frame. `None` is
    /// a flat `background_rgba`, which is what every project has until it
    /// asks for otherwise.
    pub background_gradient: Option<SceneGradient>,
    pub output_width: u32,
    pub output_height: u32,
    pub bars_rgba: [f32; 4],
    pub quads: Vec<SceneQuad>,
}

/// A resolved gradient: canvas-space axis, straight-alpha stops in order.
/// The compositor makes no decisions about it — sorting, clamping and the
/// two-stop minimum are the model's job, so the renderer and the editor
/// cannot disagree about what a malformed gradient means.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneGradient {
    /// False for linear, true for radial.
    pub radial: bool,
    /// 0 clamp, 1 repeat, 2 mirror.
    pub repeat: u32,
    /// Canvas px. Radial: `start` is the centre and `end` a point on the rim.
    pub start: [f32; 2],
    pub end: [f32; 2],
    /// Up to `MAX_GRADIENT_STOPS`, ascending by position.
    pub stops: Vec<([f32; 4], f32)>,
}

/// Matches `BackgroundGradient::MAX_STOPS`.
pub const MAX_GRADIENT_STOPS: usize = 8;

const SHADER: &str = r#"
struct Globals {
    // xy = letterbox scale (uniform), zw = letterbox offset in output px.
    fit: vec4<f32>,
    // xy = output size in px.
    output_size: vec4<f32>,
    // A frame has exactly one background, so its gradient lives here rather
    // than on every quad.
    // x = kind (0 none, 1 linear, 2 radial), y = stop count,
    // z = repeat (0 clamp, 1 repeat, 2 mirror), w = unused.
    grad: vec4<f32>,
    // xy = start (canvas px), zw = end (canvas px; radial: a point on the
    // rim, so the distance is the radius).
    grad_axis: vec4<f32>,
    // Stop positions, four to a vector.
    grad_at: array<vec4<f32>, 2>,
    // Stop colours, straight (not premultiplied) sRGB.
    grad_color: array<vec4<f32>, 8>,
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
    // xy = uv origin, zw = uv size. Whole texture is (0,0,1,1).
    uv_rect: vec4<f32>,
    // x = saturation (1 = as-is, 0 = grey), y = contrast (1 = as-is),
    // z = brightness (additive, 0 = as-is), w = tint amount (0 = none).
    adjust: vec4<f32>,
    // The tint's colour, straight alpha.
    tint_color: vec4<f32>,
    // x = 1 masked / 0 not, y = 1 inverted, z = the mask's own zoom.
    // A masked quad samples `quad_mask`'s alpha in quad-local coordinates
    // and multiplies it in.
    mask: vec4<f32>,
    // The mask's own placement: xy = offset in local px, zw = cos/sin of
    // its rotation. Identity is (0, 0, 1, 0).
    mask_xform: vec4<f32>,
    // xy = half-size of the mask's own box inside the rect (its proportions,
    // aspect-fitted and centred). (0, 0) stretches it over the whole rect.
    mask_box: vec4<f32>,
    // x = edge soften length in canvas px (0 = crisp ~1px AA edge). A
    // drop-shadow quad sets it: the edge becomes a smoothstep penumbra
    // fading to zero at the (pre-inflated) rect's edge.
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> quad: Quad;
@group(1) @binding(1) var quad_tex: texture_2d<f32>;
@group(1) @binding(2) var quad_samp: sampler;
@group(1) @binding(3) var quad_mask: texture_2d<f32>;

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

/// Where a canvas point falls along the gradient, before the repeat mode.
fn gradient_t(p: vec2<f32>) -> f32 {
    let origin = globals.grad_axis.xy;
    let rim = globals.grad_axis.zw;
    let axis = rim - origin;
    let span = dot(axis, axis);
    if (span < 1e-9) {
        return 0.0;
    }
    if (globals.grad.x > 1.5) {
        // Radial: distance from the centre over the radius.
        return length(p - origin) / sqrt(span);
    }
    return dot(p - origin, axis) / span;
}

/// Clamp, repeat or mirror — the choice that decides whether animating the
/// axis scrolls the pattern or just drags two flat regions across it.
fn gradient_wrap(t: f32) -> f32 {
    if (globals.grad.z > 1.5) {
        let two = fract(t * 0.5) * 2.0;
        return select(two, 2.0 - two, two > 1.0);
    }
    if (globals.grad.z > 0.5) {
        return fract(t);
    }
    return clamp(t, 0.0, 1.0);
}

fn gradient_stop_at(index: i32) -> f32 {
    return globals.grad_at[index / 4][index % 4];
}

/// The colour of the ramp at a canvas point. Straight alpha; the caller
/// premultiplies, exactly as the solid path does.
fn gradient_color(p: vec2<f32>) -> vec4<f32> {
    let count = i32(globals.grad.y);
    let t = gradient_wrap(gradient_t(p));
    if (t <= gradient_stop_at(0)) {
        return globals.grad_color[0];
    }
    for (var i = 1; i < count; i = i + 1) {
        let hi = gradient_stop_at(i);
        if (t <= hi) {
            let lo = gradient_stop_at(i - 1);
            let span = max(hi - lo, 1e-6);
            return mix(globals.grad_color[i - 1], globals.grad_color[i], (t - lo) / span);
        }
    }
    return globals.grad_color[count - 1];
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
    var coverage: f32;
    let soften = quad.extra.x;
    if soften > 0.0 {
        // Soft edge (drop shadows): full inside, smoothstep to nothing at
        // the rect's edge. The caller pre-inflated rect and radius by half
        // the penumbra, so the band straddles the TRUE edge like a blur.
        coverage = 1.0 - smoothstep(-soften, 0.0, d_outer);
    } else {
        coverage = clamp(0.5 - d_outer / aa, 0.0, 1.0);
    }
    if coverage <= 0.0 {
        discard;
    }

    var color: vec4<f32>;
    if quad.params.y > 0.5 {
        let uv = quad.uv_rect.xy + (in.local / size) * quad.uv_rect.zw;
        color = textureSample(quad_tex, quad_samp, uv);
        if quad.params.z > 0.5 {
            // Video frames are opaque (alpha 1), so premultiplied rgb == rgb.
            color = vec4<f32>(bt709_to_srgb(color.rgb), color.a);
        }
    } else if (quad.params.w > 0.5 && globals.grad.x > 0.5) {
        // The background quad, filled from the frame's gradient. `in.local`
        // is already canvas-space, which is where the axis is expressed.
        color = gradient_color(in.local + quad.rect.xy);
        color = vec4<f32>(color.rgb * color.a, color.a);
    } else {
        color = quad.solid_color;
        color = vec4<f32>(color.rgb * color.a, color.a);
    }

    // Per-layer colour adjustments, on THIS quad's pixels alone. The
    // colour is premultiplied by here, and colour maths on premultiplied
    // values fringes at soft edges — so un-premultiply, adjust, and fold
    // the alpha back in. Saturation, then contrast, then brightness, then
    // tint: saturation-then-tint is what makes a duotone (grey first, gel
    // after) come out as one.
    let adj = quad.adjust;
    if adj.x != 1.0 || adj.y != 1.0 || adj.z != 0.0 || adj.w != 0.0 {
        var rgb = color.rgb;
        if color.a > 0.0 {
            rgb = rgb / color.a;
        }
        let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        rgb = mix(vec3<f32>(luma, luma, luma), rgb, adj.x);
        rgb = (rgb - vec3<f32>(0.5)) * adj.y + vec3<f32>(0.5);
        rgb = rgb + vec3<f32>(adj.z);
        rgb = rgb * mix(vec3<f32>(1.0), quad.tint_color.rgb, adj.w);
        rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        color = vec4<f32>(rgb * color.a, color.a);
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

    // The mask is a WINDOW over the whole layer — content, adjustments and
    // border alike — so it cuts last, beside the rect's own coverage. Local
    // coordinates, not uv: a viewport pans the content behind the window
    // without moving the window.
    var mask_cov = 1.0;
    if quad.mask.x > 0.5 {
        // The window's own flight: un-fly the sample point (inverse
        // similarity — translate, then spin and scale about the flown
        // centre), so the PATTERN translates, spins and scales. The star
        // spins about itself wherever it is.
        let mc = size * 0.5;
        let mcs = quad.mask_xform.z;
        let msn = quad.mask_xform.w;
        // The mask's OWN box: its proportions, fitted inside the rect. A
        // circle drawn round stays round on a 2:3 layer, because the rect's
        // shape no longer decides the mask's. (0,0) keeps the old
        // stretch-to-fill for a caller that wants it.
        var half = quad.mask_box.xy;
        if half.x <= 0.0 || half.y <= 0.0 {
            half = mc;
        }
        var p = in.local - mc - quad.mask_xform.xy;
        p = vec2<f32>(p.x * mcs + p.y * msn, -p.x * msn + p.y * mcs);
        p = vec2<f32>(p.x / max(quad.mask.z, 1e-6), p.y / max(quad.mask.w, 1e-6));
        var ink = textureSample(quad_mask, quad_samp, p / (half * 2.0) + vec2<f32>(0.5)).a;
        // Outside the window's own box there is nothing to sample: the
        // raster's tips touch its edges, and clamp-sampling would smear
        // them into streaks across the rect.
        if abs(p.x) > half.x || abs(p.y) > half.y {
            ink = 0.0;
        }
        if quad.mask.y > 0.5 {
            ink = 1.0 - ink;
        }
        mask_cov = ink;
    }

    return color * coverage * mask_cov * quad.params.x;
}
"#;

/// Uniform layouts (std140-compatible: all vec4s).
#[repr(C)]
#[derive(Clone, Copy)]
struct GlobalsRaw {
    fit: [f32; 4],
    output_size: [f32; 4],
    grad: [f32; 4],
    grad_axis: [f32; 4],
    grad_at: [[f32; 4]; 2],
    grad_color: [[f32; 4]; MAX_GRADIENT_STOPS],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct QuadRaw {
    rect: [f32; 4],
    rot_radius_border: [f32; 4],
    border_color: [f32; 4],
    solid_color: [f32; 4],
    params: [f32; 4],
    uv_rect: [f32; 4],
    adjust: [f32; 4],
    tint_color: [f32; 4],
    mask: [f32; 4],
    mask_xform: [f32; 4],
    mask_box: [f32; 4],
    extra: [f32; 4],
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
    nearest_sampler: wgpu::Sampler,
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
    binds: std::collections::HashMap<(u64, u64), wgpu::BindGroup>,
    /// Insertion order for eviction.
    bind_order: std::collections::VecDeque<(u64, u64)>,
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
    /// The same shader aimed at an Rgba16Float target with ADDITIVE blending
    /// — the accumulator half of motion blur. Averaging N samples by
    /// repeated 8-bit source-over would re-quantise the running total every
    /// pass; adding each sample once into 16-bit float and resolving at the
    /// end quantises exactly once.
    accum_pipeline: wgpu::RenderPipeline,
    pipeline_multiply: wgpu::RenderPipeline,
    pipeline_screen: wgpu::RenderPipeline,
    pipeline_add: wgpu::RenderPipeline,
    /// Lazily sized to the output: `scratch` receives each sub-sample's
    /// ordinary compose, `accum` sums them.
    accum_targets: Option<AccumTargets>,
}

struct AccumTargets {
    width: u32,
    height: u32,
    scratch: InputTexture,
    scratch_tex: std::sync::Arc<wgpu::Texture>,
    accum: InputTexture,
    accum_tex: std::sync::Arc<wgpu::Texture>,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compositor"),
            bind_group_layouts: &[&globals_layout, &quad_layout],
            push_constant_ranges: &[],
        });

        // One shader, one blend state per mode. The colour factors are the
        // whole difference; alpha always composites source-over so coverage
        // stays sane whatever the colours do.
        let make_pipeline = |label: &str, color: wgpu::BlendComponent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
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
                            color,
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
            })
        };
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = make_pipeline("compositor", over);
        // Multiply: S·D where the source covers, D where it does not —
        // premultiplied S makes the two ends meet at soft edges.
        let pipeline_multiply = make_pipeline(
            "compositor-multiply",
            wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        );
        // Screen: S + D·(1−S). Black sources vanish, which is the point.
        let pipeline_screen = make_pipeline(
            "compositor-screen",
            wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrc,
                operation: wgpu::BlendOperation::Add,
            },
        );
        // Add: pure light, clips sooner than screen.
        let pipeline_add = make_pipeline(
            "compositor-add",
            wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        );

        let accum_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compositor-accum"),
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
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
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
        // The unsmoothed twin. Pixel art scaled up stays pixels, and a sprite
        // sheet's cells stop bleeding into each other at their edges.
        let nearest_sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("compositor-nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
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
            nearest_sampler,
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
            accum_pipeline,
            pipeline_multiply,
            pipeline_screen,
            pipeline_add,
            accum_targets: None,
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
    ///
    /// Keyed by texture AND filter, since the sampler is part of the bind
    /// group: the same sheet drawn smoothed and unsmoothed in one scene needs
    /// two. The low bit carries the filter, so ids stay dense. The mask
    /// texture widens the key: an unmasked quad binds the dummy there and
    /// the shader's flag keeps it unread.
    fn bind_group_for(
        &mut self,
        ctx: &GpuContext,
        texture: Option<&InputTexture>,
        nearest: bool,
        mask: Option<&InputTexture>,
    ) -> (u64, u64) {
        let texture = texture.unwrap_or(&self.dummy);
        let mask = mask.unwrap_or(&self.dummy);
        let id = ((texture.id << 1) | u64::from(nearest), mask.id);
        if !self.binds.contains_key(&id) {
            let sampler = if nearest {
                &self.nearest_sampler
            } else {
                &self.sampler
            };
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
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&mask.view),
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

    /// Wraps an owned texture as an [`InputTexture`] the compositor can
    /// sample. The texture must already carry `TEXTURE_BINDING`; ownership
    /// moves in, so whatever drew into it can drop its handle.
    pub fn adopt_owned_texture(texture: wgpu::Texture) -> InputTexture {
        let view = texture.create_view(&Default::default());
        InputTexture {
            view: std::sync::Arc::new(view),
            id: next_texture_id(),
            _texture: std::sync::Arc::new(texture),
        }
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
            crate::GpuSurface::IoSurface { .. } => {
                Err(GpuError::Import("import: IOSurface is Apple-only".into()))
            }
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
                    Ok(crate::ImportedFrame::owning(
                        texture,
                        w,
                        h,
                        KeepAlive::Nothing,
                    ))
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
                    Ok(crate::ImportedFrame::owning(
                        texture,
                        w,
                        h,
                        KeepAlive::Nothing,
                    ))
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
        let mut globals = GlobalsRaw {
            fit: [scale as f32, scale as f32, off_x as f32, off_y as f32],
            output_size: [ow as f32, oh as f32, 0.0, 0.0],
            grad: [0.0; 4],
            grad_axis: [0.0; 4],
            grad_at: [[0.0; 4]; 2],
            grad_color: [[0.0; 4]; MAX_GRADIENT_STOPS],
        };
        if let Some(gradient) = &scene.background_gradient {
            let stops = gradient.stops.len().min(MAX_GRADIENT_STOPS);
            if stops >= 2 {
                globals.grad = [
                    if gradient.radial { 2.0 } else { 1.0 },
                    stops as f32,
                    gradient.repeat as f32,
                    0.0,
                ];
                globals.grad_axis = [
                    gradient.start[0],
                    gradient.start[1],
                    gradient.end[0],
                    gradient.end[1],
                ];
                for (index, (color, at)) in gradient.stops.iter().take(stops).enumerate() {
                    globals.grad_at[index / 4][index % 4] = *at;
                    globals.grad_color[index] = *color;
                }
            }
        }
        ctx.queue
            .write_buffer(&self.globals_buf, 0, as_bytes(&globals));

        // Background canvas rect renders as the first solid quad.
        let mut quads = Vec::with_capacity(scene.quads.len() + 1);
        quads.push(SceneQuad {
            texture: None,
            rect: [0.0, 0.0, cw, ch],
            solid_rgba: scene.background_rgba,
            // Only this quad reads the frame's gradient; every other solid
            // keeps its own colour.
            gradient_fill: scene.background_gradient.is_some(),
            ..Default::default()
        });
        quads.extend_from_slice(&scene.quads);
        self.ensure_quad_capacity(ctx, quads.len());

        // One staging write for the whole frame's quad uniforms.
        let mut staging = vec![0u8; QUAD_STRIDE as usize * quads.len()];
        let mut binds: Vec<(u64, u64)> = Vec::with_capacity(quads.len());
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
                    if q.gradient_fill { 1.0 } else { 0.0 },
                ],
                uv_rect: q.uv_rect,
                adjust: q.adjust,
                tint_color: q.tint_rgba,
                mask: [
                    if q.mask.is_some() { 1.0 } else { 0.0 },
                    if q.mask_invert { 1.0 } else { 0.0 },
                    q.mask_transform[2],
                    q.mask_zoom_y,
                ],
                mask_xform: {
                    let rot = (q.mask_transform[3] as f64).to_radians();
                    [
                        q.mask_transform[0],
                        q.mask_transform[1],
                        rot.cos() as f32,
                        rot.sin() as f32,
                    ]
                },
                mask_box: [q.mask_half[0], q.mask_half[1], 0.0, 0.0],
                extra: [q.edge_soften as f32, 0.0, 0.0, 0.0],
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
            let mask = match q.mask {
                Some(index) => Some(*textures.get(index).ok_or_else(|| {
                    GpuError::Import(format!("mask texture index {index} out of range"))
                })?),
                None => None,
            };
            binds.push(self.bind_group_for(ctx, texture, q.nearest, mask));
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
            let mut current = QuadBlend::Normal;
            for (i, id) in binds.iter().enumerate() {
                let wanted = quads[i].blend;
                if wanted != current {
                    pass.set_pipeline(match wanted {
                        QuadBlend::Normal => &self.pipeline,
                        QuadBlend::Multiply => &self.pipeline_multiply,
                        QuadBlend::Screen => &self.pipeline_screen,
                        QuadBlend::Add => &self.pipeline_add,
                    });
                    current = wanted;
                }
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
        let texture = self
            .adopt_cached(ctx, output, scene.output_width, scene.output_height, true)?
            .1;
        self.compose_to_texture_borrowed(ctx, scene, textures, &texture)
    }

    fn ensure_accum_targets(&mut self, ctx: &GpuContext, width: u32, height: u32) {
        if self
            .accum_targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        let make = |label: &str, format: wgpu::TextureFormat| {
            let tex = std::sync::Arc::new(ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            }));
            let input = InputTexture {
                view: std::sync::Arc::new(tex.create_view(&Default::default())),
                id: next_texture_id(),
                _texture: tex.clone(),
            };
            (input, tex)
        };
        let (scratch, scratch_tex) = make("blur-scratch", wgpu::TextureFormat::Bgra8Unorm);
        let (accum, accum_tex) = make("blur-accum", wgpu::TextureFormat::Rgba16Float);
        self.accum_targets = Some(AccumTargets {
            width,
            height,
            scratch,
            scratch_tex,
            accum,
            accum_tex,
        });
    }

    /// One fullscreen textured quad into `target` — the plumbing both halves
    /// of the accumulator share. `weight` rides the quad's opacity, which the
    /// shader multiplies into the premultiplied sample.
    #[allow(clippy::too_many_arguments)]
    fn fullscreen_source_pass(
        &mut self,
        ctx: &GpuContext,
        source: InputTexture,
        weight: f32,
        target: &wgpu::TextureView,
        additive: bool,
        load: wgpu::LoadOp<wgpu::Color>,
        width: u32,
        height: u32,
    ) {
        let globals = GlobalsRaw {
            fit: [1.0, 1.0, 0.0, 0.0],
            output_size: [width as f32, height as f32, 0.0, 0.0],
            grad: [0.0; 4],
            grad_axis: [0.0; 4],
            grad_at: [[0.0; 4]; 2],
            grad_color: [[0.0; 4]; MAX_GRADIENT_STOPS],
        };
        ctx.queue
            .write_buffer(&self.globals_buf, 0, as_bytes(&globals));
        let raw = QuadRaw {
            rect: [0.0, 0.0, width as f32, height as f32],
            rot_radius_border: [1.0, 0.0, 0.0, 0.0],
            border_color: [0.0; 4],
            solid_color: [0.0; 4],
            params: [weight, 1.0, 0.0, 0.0],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            adjust: [1.0, 1.0, 0.0, 0.0],
            tint_color: [1.0, 1.0, 1.0, 1.0],
            mask: [0.0; 4],
            mask_xform: [0.0, 0.0, 1.0, 0.0],
            mask_box: [0.0; 4],
            extra: [0.0; 4],
        };
        self.ensure_quad_capacity(ctx, 1);
        ctx.queue.write_buffer(&self.quad_buf, 0, as_bytes(&raw));
        let bind_id = self.bind_group_for(ctx, Some(&source), true, None);

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blur-blend"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-blend"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(if additive {
                &self.accum_pipeline
            } else {
                &self.pipeline
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);
            let bind = self.binds.get(&bind_id).expect("bind group cached above");
            pass.set_bind_group(1, bind, &[0u32]);
            pass.draw(0..4, 0..1);
        }
        let index = ctx.queue.submit([encoder.finish()]);
        if self.defer_completion {
            self.last_submission = Some(index);
        } else {
            ctx.device.poll(wgpu::Maintain::Wait);
            self.last_submission = None;
        }
    }

    /// Motion blur, sample by sample: composes `scene` normally into an
    /// internal scratch, then ADDS it into a 16-bit accumulator at
    /// `1/sample_count`. The first sample clears the accumulator; after the
    /// last, [`accumulate_resolve_to_texture`](Self::accumulate_resolve_to_texture)
    /// writes the average out.
    ///
    /// Weighted this way — every sample at `1/N` into float, one resolve —
    /// rather than a running source-over average, because the running form
    /// re-quantises the total through 8 bits on every pass and bands.
    pub fn accumulate_scene_to_texture_borrowed(
        &mut self,
        ctx: &GpuContext,
        scene: &Scene,
        textures: &[&InputTexture],
        sample_index: usize,
        sample_count: usize,
    ) -> Result<(), GpuError> {
        self.ensure_accum_targets(ctx, scene.output_width, scene.output_height);
        let targets = self.accum_targets.as_ref().expect("ensured above");
        let (scratch, scratch_tex) = (targets.scratch.clone(), targets.scratch_tex.clone());
        let accum_view = targets.accum_tex.create_view(&Default::default());
        self.compose_to_texture_borrowed(ctx, scene, textures, &scratch_tex)?;
        let load = if sample_index == 0 {
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
        } else {
            wgpu::LoadOp::Load
        };
        self.fullscreen_source_pass(
            ctx,
            scratch,
            1.0 / sample_count.max(1) as f32,
            &accum_view,
            true,
            load,
            scene.output_width,
            scene.output_height,
        );
        Ok(())
    }

    /// The accumulator's average, written to the real output. The summed
    /// alpha is 1 (N samples at 1/N, each opaque), so source-over here is a
    /// plain replace.
    pub fn accumulate_resolve_to_texture(
        &mut self,
        ctx: &GpuContext,
        output: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        let accum = self
            .accum_targets
            .as_ref()
            .ok_or_else(|| GpuError::Import("resolve with no accumulated samples".into()))?
            .accum
            .clone();
        let view = output.create_view(&Default::default());
        self.fullscreen_source_pass(
            ctx,
            accum,
            1.0,
            &view,
            false,
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            width,
            height,
        );
        Ok(())
    }

    /// IOSurface-backed variant of the resolve (macOS/iOS).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn accumulate_resolve_to_iosurface(
        &mut self,
        ctx: &GpuContext,
        output: crate::iosurface::IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        let texture = self.adopt_cached(ctx, output, width, height, true)?.1;
        self.accumulate_resolve_to_texture(ctx, &texture, width, height)
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
        let key = (
            unsafe { crate::iosurface::IOSurfaceGetID(surface) },
            render_attachment,
        );
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
            let Some(victim) = self.import_order.pop_front() else {
                break;
            };
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
            background_gradient: None,
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

    /// A soft-edged solid quad is the drop-shadow primitive: full inside
    /// the TRUE rect, fading to nothing at the (pre-inflated) rect's edge,
    /// half-covered exactly at the true edge — the blur-like penumbra.
    #[test]
    fn a_soft_edge_fades_a_solid_quad_out() {
        let ctx = GpuContext::new().expect("gpu");
        // True rect 40..60 with a 20px penumbra: the engine convention
        // inflates rect and radius by half of it → 30..70, radius 10.
        let scene = Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            background_gradient: None,
            output_width: 100,
            output_height: 100,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                rect: [30.0, 30.0, 40.0, 40.0],
                solid_rgba: [1.0, 1.0, 1.0, 1.0],
                corner_radius: 10.0,
                edge_soften: 20.0,
                ..Default::default()
            }],
        };
        let pixels = compose(&scene, &[], &ctx);
        assert_eq!(px(&pixels, 100, 50, 50), [255, 255, 255, 255], "core solid");
        let edge = px(&pixels, 100, 40, 50); // the true edge: half covered
        assert!(
            (edge[0] as i32 - 128).abs() <= 12,
            "true edge ~half: {edge:?}"
        );
        assert_eq!(px(&pixels, 100, 25, 50), [0, 0, 0, 255], "beyond: nothing");
        // Monotonic fade across the penumbra.
        let a = px(&pixels, 100, 35, 50)[0];
        let b = px(&pixels, 100, 45, 50)[0];
        assert!(a < 128 && b > 128, "fade runs outward: {a} < 128 < {b}");
    }

    /// A mask is a second texture whose ALPHA windows the quad: where the
    /// mask has no ink the quad must vanish and the ground behind it show,
    /// and `mask_invert` flips the window into a hole. Probes sit at 5% and
    /// 95% of the quad so the two-texel mask's bilinear ramp stays away.
    #[test]
    fn a_mask_windows_a_quad_and_inverts() {
        let ctx = GpuContext::new().expect("gpu");
        let red = Compositor::upload_texture(&ctx, &[0, 0, 255, 255], 1, 1).expect("content");
        // Left texel clear, right texel inked (premultiplied white).
        let mask = Compositor::upload_texture(&ctx, &[0, 0, 0, 0, 255, 255, 255, 255], 2, 1)
            .expect("mask");
        let scene = |invert: bool| Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [0.0, 1.0, 0.0, 1.0], // green ground
            background_gradient: None,
            output_width: 100,
            output_height: 100,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 100.0, 100.0],
                mask: Some(1),
                mask_invert: invert,
                ..Default::default()
            }],
        };
        let textures = [red, mask];
        let px = compose(&scene(false), &textures, &ctx);
        assert_eq!(
            px_(&px, 5, 50),
            [0, 255, 0, 255],
            "no ink: the ground shows"
        );
        assert_eq!(px_(&px, 95, 50), [0, 0, 255, 255], "ink: the content shows");

        let px = compose(&scene(true), &textures, &ctx);
        assert_eq!(
            px_(&px, 5, 50),
            [0, 0, 255, 255],
            "inverted: content where ink was not"
        );
        assert_eq!(px_(&px, 95, 50), [0, 255, 0, 255], "inverted: the hole");
    }

    /// The mask's own flight: offset translates the window, zoom scales it
    /// about its flown centre, rotation spins it the same visual direction
    /// as layer rotation — and beyond the flown box the window cuts to
    /// NOTHING (clamp-sampling would smear the raster's edge texels into
    /// streaks). Red content over a green ground; the mask's ink is its
    /// right half, so every placement has an unambiguous where.
    #[test]
    fn a_mask_flies_spins_and_cuts_at_its_box() {
        let ctx = GpuContext::new().expect("gpu");
        let red = Compositor::upload_texture(&ctx, &[0, 0, 255, 255], 1, 1).expect("content");
        // 8x1: four clear texels, four inked — a sharp-enough edge that
        // half-resolution sampling keeps probes out of the bilinear ramp.
        let mut bytes = vec![0u8; 16];
        bytes.extend([255u8; 16]);
        let mask = Compositor::upload_texture(&ctx, &bytes, 8, 1).expect("mask");
        let scene = |xform: [f32; 4], invert: bool| Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [0.0, 1.0, 0.0, 1.0],
            background_gradient: None,
            output_width: 100,
            output_height: 100,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 100.0, 100.0],
                mask: Some(1),
                mask_invert: invert,
                mask_transform: xform,
                ..Default::default()
            }],
        };
        let textures = [red, mask];
        const RED: [u8; 4] = [0, 0, 255, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];

        // Flown left by 50: the ink lands on the left half, and the right
        // half is BEYOND the flown box — ground, not smeared edge texels.
        let px = compose(&scene([-50.0, 0.0, 1.0, 0.0], false), &textures, &ctx);
        assert_eq!(px_(&px, 25, 50), RED, "ink flew left");
        assert_eq!(px_(&px, 75, 50), GREEN, "beyond the box cuts to nothing");

        // Inverted, same flight: the hole flies with it, and beyond the box
        // the CONTENT shows — outside the window's world there is no ink.
        let px = compose(&scene([-50.0, 0.0, 1.0, 0.0], true), &textures, &ctx);
        assert_eq!(px_(&px, 25, 50), GREEN, "the hole flew left");
        assert_eq!(px_(&px, 75, 50), RED, "beyond the box the layer shows");

        // Zoom 0.5 about the centre: the box shrinks to [25, 75], ink to
        // [50, 75], and past 75 is outside the box.
        let px = compose(&scene([0.0, 0.0, 0.5, 0.0], false), &textures, &ctx);
        assert_eq!(px_(&px, 60, 50), RED, "ink inside the shrunk window");
        assert_eq!(px_(&px, 30, 50), GREEN, "clear half of the shrunk window");
        assert_eq!(px_(&px, 85, 50), GREEN, "past the shrunk box");

        // 90 degrees: the right-half ink swings to the BOTTOM half — the
        // same clockwise-on-screen direction a layer's own rotation turns.
        let px = compose(&scene([0.0, 0.0, 1.0, 90.0], false), &textures, &ctx);
        assert_eq!(px_(&px, 50, 75), RED, "ink swung to the bottom");
        assert_eq!(px_(&px, 50, 25), GREEN, "clear swung to the top");
    }

    fn gradient_scene(gradient: SceneGradient) -> Scene {
        Scene {
            canvas_width: 100.0,
            canvas_height: 100.0,
            background_rgba: [1.0, 0.0, 1.0, 1.0], // magenta: must never show
            background_gradient: Some(gradient),
            output_width: 100,
            output_height: 100,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![],
        }
    }

    /// Black on the left, white on the right, and grey exactly halfway — the
    /// whole of a linear ramp. The flat `background_rgba` is magenta, so if
    /// the gradient is not applied the test does not merely drift, it screams.
    #[test]
    fn a_linear_gradient_ramps_across_the_canvas() {
        let ctx = GpuContext::new().expect("gpu");
        let scene = gradient_scene(SceneGradient {
            radial: false,
            repeat: 0,
            start: [0.0, 0.0],
            end: [100.0, 0.0],
            stops: vec![([0.0, 0.0, 0.0, 1.0], 0.0), ([1.0, 1.0, 1.0, 1.0], 1.0)],
        });
        let px = compose(&scene, &[], &ctx);
        // Sampled at pixel CENTRES, so the first pixel is 1.5px along a
        // 100px axis rather than at zero — hence a couple of levels, not none.
        assert!(
            px_(&px, 1, 50)[2] <= 6,
            "black at the left, got {}",
            px_(&px, 1, 50)[2]
        );
        assert!(
            px_(&px, 98, 50)[2] >= 248,
            "white at the right, got {}",
            px_(&px, 98, 50)[2]
        );
        let middle = px_(&px, 50, 50)[2];
        assert!(
            (middle as i32 - 128).abs() <= 3,
            "grey halfway, got {middle}"
        );
        // It runs along the AXIS, so the ramp does not vary down the canvas.
        assert_eq!(px_(&px, 50, 10)[2], middle, "constant across the axis");
    }

    /// Radial measures distance from the centre, so the corners are the far
    /// end of the ramp and the middle is the near end.
    #[test]
    fn a_radial_gradient_runs_outward_from_its_centre() {
        let ctx = GpuContext::new().expect("gpu");
        let scene = gradient_scene(SceneGradient {
            radial: true,
            repeat: 0,
            start: [50.0, 50.0],
            end: [100.0, 50.0], // radius 50
            stops: vec![([1.0, 1.0, 1.0, 1.0], 0.0), ([0.0, 0.0, 0.0, 1.0], 1.0)],
        });
        let px = compose(&scene, &[], &ctx);
        assert!(px_(&px, 50, 50)[2] > 250, "white at the centre");
        assert_eq!(px_(&px, 1, 1)[2], 0, "clamped to black in the corner");
        // Equidistant points match: it is a distance, not a projection.
        assert_eq!(px_(&px, 25, 50)[2], px_(&px, 50, 25)[2]);
    }

    /// The reason `repeat` exists: it TILES, so animating the axis scrolls
    /// the pattern instead of dragging two flat regions across the canvas.
    #[test]
    fn repeat_tiles_the_ramp_and_mirror_reverses_each_tile() {
        let ctx = GpuContext::new().expect("gpu");
        let make = |repeat: u32| {
            gradient_scene(SceneGradient {
                radial: false,
                repeat,
                start: [0.0, 0.0],
                end: [50.0, 0.0], // half the canvas: exactly two periods
                stops: vec![([0.0, 0.0, 0.0, 1.0], 0.0), ([1.0, 1.0, 1.0, 1.0], 1.0)],
            })
        };
        let repeated = compose(&make(1), &[], &ctx);
        // Two identical ramps: a point and the same point one period along
        // are the same colour, which is what makes a scroll seamless.
        assert_eq!(px_(&repeated, 10, 50)[2], px_(&repeated, 60, 50)[2]);
        assert!(px_(&repeated, 49, 50)[2] > 240, "end of the first tile");
        assert!(px_(&repeated, 51, 50)[2] < 40, "and straight back to black");

        // Mirror turns that hard return into a fold, so the ends never meet.
        let mirrored = compose(&make(2), &[], &ctx);
        assert!(
            mirrored[51 * 4 + 2] > 200 || px_(&mirrored, 51, 50)[2] > 200,
            "the second tile runs backwards from white"
        );
        assert!(
            px_(&mirrored, 99, 50)[2] < 40,
            "and reaches black at the far end"
        );
    }

    /// No gradient means the flat colour, unchanged — every project that has
    /// never heard of gradients renders exactly as before.
    #[test]
    fn without_a_gradient_the_flat_background_still_wins() {
        let ctx = GpuContext::new().expect("gpu");
        let mut scene = gradient_scene(SceneGradient {
            radial: false,
            repeat: 0,
            start: [0.0, 0.0],
            end: [100.0, 0.0],
            stops: vec![([0.0, 0.0, 0.0, 1.0], 0.0), ([1.0, 1.0, 1.0, 1.0], 1.0)],
        });
        scene.background_gradient = None;
        let px = compose(&scene, &[], &ctx);
        assert_eq!(px_(&px, 50, 50), [255, 0, 255, 255], "the magenta fill");
    }

    fn px_(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        px(pixels, 100, x, y)
    }

    /// The sprite-sheet contract in one frame: a uv rect shows ONE cell, and
    /// `nearest` is what keeps the cell next door out of it.
    ///
    /// With smoothing on, a pixel near the cell's edge samples across the
    /// boundary and blends in the neighbouring frame — the classic
    /// sprite-sheet artifact. That is why `nearest` is a correctness flag
    /// here and not a matter of taste, and this test fails on exactly that
    /// pixel if the two are ever decoupled.
    #[test]
    fn a_uv_rect_shows_one_cell_and_nearest_keeps_the_neighbour_out() {
        let ctx = GpuContext::new().expect("gpu");
        // A 2×2 "sheet" of four one-texel cells. Premultiplied BGRA.
        let sheet: Vec<u8> = vec![
            0, 0, 255, 255, /* red   */ 0, 255, 0, 255, /* green */
            255, 0, 0, 255, /* blue  */ 255, 255, 255, 255, /* white */
        ];
        let tex = Compositor::upload_texture(&ctx, &sheet, 2, 2).expect("tex");

        let render = |uv: [f32; 4], nearest: bool| {
            let scene = Scene {
                canvas_width: 100.0,
                canvas_height: 100.0,
                background_rgba: [0.0, 0.0, 0.0, 1.0],
                output_width: 100,
                output_height: 100,
                background_gradient: None,
                bars_rgba: [0.0, 0.0, 0.0, 1.0],
                quads: vec![SceneQuad {
                    texture: Some(0),
                    rect: [0.0, 0.0, 100.0, 100.0],
                    uv_rect: uv,
                    nearest,
                    ..Default::default()
                }],
            };
            compose(&scene, std::slice::from_ref(&tex), &ctx)
        };

        // The top-left cell, blown up to the whole canvas.
        let crisp = render([0.0, 0.0, 0.5, 0.5], true);
        assert_eq!(px(&crisp, 100, 50, 50), [0, 0, 255, 255], "cell is red");
        // The last column maps to the very edge of the cell. Unsmoothed, it
        // is still that cell and nothing else.
        assert_eq!(
            px(&crisp, 100, 99, 50),
            [0, 0, 255, 255],
            "no green from the cell next door"
        );

        // The same quad smoothed: the edge pixel is half the neighbouring
        // frame. This is the artifact, asserted so the flag cannot quietly
        // stop being applied.
        let smoothed = render([0.0, 0.0, 0.5, 0.5], false);
        let edge = px(&smoothed, 100, 99, 50);
        assert!(edge[1] > 60, "smoothing bleeds green in, got {edge:?}");

        // The origin is honoured too, not just the size.
        let second = render([0.5, 0.0, 0.5, 0.5], true);
        assert_eq!(
            px(&second, 100, 50, 50),
            [0, 255, 0, 255],
            "the next cell along is green"
        );
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
            background_gradient: None,
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
            background_gradient: None,
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
        input
            .write_pixels(&[255, 0, 255, 255].repeat(16))
            .expect("write");
        let out = OwnedIoSurface::new_bgra(8, 8).expect("out");
        let scene = Scene {
            canvas_width: 8.0,
            canvas_height: 8.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 8,
            output_height: 8,
            background_gradient: None,
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
        input
            .write_pixels(&[0, 255, 0, 255].repeat(16))
            .expect("write2");
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
            let tex =
                Compositor::upload_texture(&ctx, &[(i % 255) as u8, 0, 0, 255], 1, 1).expect("tex");
            let scene = Scene {
                canvas_width: 16.0,
                canvas_height: 16.0,
                background_rgba: [0.0, 0.0, 0.0, 1.0],
                output_width: 16,
                output_height: 16,
                background_gradient: None,
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
            background_gradient: None,
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

    /// A texture with a SOFT edge must blend, not saturate.
    ///
    /// The compositor blends premultiplied, so a straight-alpha texture makes
    /// every half-covered pixel come out fully lit — which is what turned
    /// antialiased captions into binary-edged text. Opaque textures hide the
    /// bug entirely, so this uses a half-alpha one on purpose.
    #[test]
    fn a_half_transparent_texture_blends_instead_of_saturating() {
        let ctx = GpuContext::new().expect("gpu");
        // Premultiplied white at 50%: rgb and alpha both 128.
        let pixels_in = vec![128u8; 4 * 4 * 4];
        let tex = Compositor::upload_texture(&ctx, &pixels_in, 4, 4).expect("upload");
        let scene = Scene {
            canvas_width: 4.0,
            canvas_height: 4.0,
            background_rgba: [0.0, 0.0, 0.0, 1.0],
            output_width: 4,
            output_height: 4,
            background_gradient: None,
            bars_rgba: [0.0, 0.0, 0.0, 1.0],
            quads: vec![SceneQuad {
                texture: Some(0),
                rect: [0.0, 0.0, 4.0, 4.0],
                ..Default::default()
            }],
        };
        let out = compose(&scene, &[tex], &ctx);
        let p = px(&out, 4, 2, 2);
        assert!(
            (p[0] as i32 - 128).abs() <= 2,
            "50% white over black should be mid grey, got {p:?} — a saturated \
             255 means the texture path is not blending premultiplied"
        );
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
            background_gradient: None,
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
