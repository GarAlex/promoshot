//! The model pass: a glTF model lit and drawn into an offscreen texture at
//! a quad's pixel size, which the compositor then draws like any other
//! picture — the slab pattern per frame on the GPU (3D plan, section 2).
//!
//! Shading is metallic-roughness PBR: one key light through a GGX lobe
//! (Smith visibility, Schlick Fresnel, energy-conserving), an ambient
//! term and a rim from the theme, and an environment read through the
//! split-sum approximation — prefiltered per mip with the same lobe. A
//! finish WORD (rung 44) adds what two sliders cannot say: a clear coat,
//! a grain, the dielectric's reflectance, and transmission through a
//! thin body. Highlights past 0.76 roll off toward white instead of
//! clipping. sRGB textures decode to linear and the result encodes
//! back, premultiplied, so the compositor's existing input path
//! applies. Depth is real within the pass; between layers `sortIndex`
//! orders, and within it the meshes light passes through are drawn
//! after the opaque ones.

use crate::compositor::InputTexture;
use crate::{GpuContext, GpuError};

/// One primitive's geometry, world space, as the loader hands it over.
pub struct MeshInput<'a> {
    pub positions: &'a [[f32; 3]],
    pub normals: &'a [[f32; 3]],
    pub uvs: &'a [[f32; 2]],
    pub indices: &'a [u32],
    pub material: usize,
    /// Index into the matrices `render` is given — the node the mesh
    /// hangs from. 0 for a model that is one piece.
    pub node: usize,
}

/// What a finish WORD adds beyond metallic and roughness (rung 44): a
/// clear coat and its roughness, a grain, the dielectric's reflectance
/// straight on, and how much light a thin body passes. Every value is
/// 0…1; the default is a plain surface, what every slot had before the
/// words existed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceFinish {
    pub clearcoat: f32,
    pub clearcoat_roughness: f32,
    pub anisotropy: f32,
    pub specular: f32,
    pub transmission: f32,
}

impl Default for SurfaceFinish {
    fn default() -> Self {
        SurfaceFinish {
            clearcoat: 0.0,
            clearcoat_roughness: 0.05,
            anisotropy: 0.0,
            specular: 0.04,
            transmission: 0.0,
        }
    }
}

impl SurfaceFinish {
    /// The two uniform rows the shader reads.
    fn raw(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.clearcoat.clamp(0.0, 1.0),
                self.clearcoat_roughness.clamp(0.0, 1.0),
                self.anisotropy.clamp(0.0, 1.0),
                self.specular.clamp(0.0, 1.0),
            ],
            [self.transmission.clamp(0.0, 1.0), 0.0, 0.0, 0.0],
        )
    }
}

/// One body's footprint on a stage's floor (rung 45): where it stands,
/// in world x/z, and how high it reaches above the floor — what the
/// contact darkening and the mirror's reach are measured from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Footprint {
    pub center_xz: [f32; 2],
    pub half_xz: [f32; 2],
    /// The body's lowest and highest point, above the floor.
    pub bottom: f32,
    pub top: f32,
}

/// A stage's floor (rung 45): the plane its bodies stand on, drawn as a
/// CATCHER — invisible itself, it darkens by the key light's shadow and
/// where a body touches, and shows the stage mirrored in it; what lies
/// beneath the stage layer shows through everywhere else. The light
/// moves; the floor does not.
#[derive(Debug, Clone, PartialEq)]
pub struct FloorView {
    /// The plane's height, world units.
    pub y: f32,
    /// How dark the shadow falls: 0 none … 1 black.
    pub shadow: f32,
    /// The shadow's edge, in shadow-map texels: a small hot source (a
    /// sunset) casts a hard one, big soft boxes (the studio) a soft one.
    pub softness: f32,
    /// How much of the mirrored stage shows: 0 none … 1 whole.
    pub reflection: f32,
    /// How blurred the mirror is, in mip levels.
    pub blur: f32,
    pub footprints: Vec<Footprint>,
}

/// A material's factors and, if any, its textures: base colour (RGBA8
/// sRGB), a tangent-space normal map and a metallic-roughness map (both
/// RGBA8 linear, glTF's layout: roughness in G, metallic in B).
pub struct MaterialInput<'a> {
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub double_sided: bool,
    pub texture: Option<(u32, u32, &'a [u8])>,
    pub normal: Option<(u32, u32, &'a [u8])>,
    pub metal_rough: Option<(u32, u32, &'a [u8])>,
}

/// One thing on a stage: a model with its node matrices, or a picture
/// standing in the scene facing the camera.
pub enum StageItem<'a> {
    Model {
        model: &'a GpuModel,
        matrices: &'a [Mat4],
    },
    Billboard {
        texture: &'a wgpu::TextureView,
        /// World-space centre of the picture.
        center: [f32; 3],
        /// Width and height in world units.
        size: [f32; 2],
    },
}

/// Where the model is looked at from and how it is lit, for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ModelView {
    /// Orbit about the bounds centre, degrees.
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    /// Camera distance in units of the bounds radius.
    pub distance: f64,
    /// Vertical field of view, degrees.
    pub fov: f64,
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
    /// The key light's direction, degrees, and strength.
    pub light_yaw: f64,
    pub light_pitch: f64,
    pub light_intensity: f64,
    /// Linear RGB: the key light, the ambient fill and the rim.
    pub key_rgb: [f32; 3],
    pub ambient_rgb: [f32; 3],
    pub rim_rgb: [f32; 3],
    /// The world's light beyond the key: a built-in environment metals
    /// mirror. `EnvPreset::None` keeps the synthetic sky and ground.
    pub environment: EnvironmentView,
    /// A camera flown along a route (rung 40): where it IS, in world
    /// units, instead of the orbit yaw/pitch/distance describe; and what
    /// it looks at instead of the bounds centre. Either alone works.
    pub eye: Option<[f32; 3]>,
    pub target: Option<[f32; 3]>,
}

/// A built-in environment, as an equirectangular HDR the pass generates
/// once: a soft studio, a low warm sunset, a cold night.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvPreset {
    #[default]
    None,
    Studio,
    Sunset,
    Night,
}

impl EnvPreset {
    pub fn parse(name: &str) -> Option<EnvPreset> {
        match name {
            "studio" => Some(EnvPreset::Studio),
            "sunset" => Some(EnvPreset::Sunset),
            "night" => Some(EnvPreset::Night),
            _ => None,
        }
    }
}

/// Which environment a frame mirrors, how strongly, and turned how far
/// about the vertical (degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvironmentView {
    pub preset: EnvPreset,
    pub intensity: f32,
    pub rotation_deg: f32,
}

impl Default for EnvironmentView {
    fn default() -> Self {
        EnvironmentView {
            preset: EnvPreset::None,
            intensity: 1.0,
            rotation_deg: 0.0,
        }
    }
}

impl Default for ModelView {
    fn default() -> Self {
        ModelView {
            yaw: -25.0,
            pitch: 10.0,
            roll: 0.0,
            distance: 4.2,
            fov: 30.0,
            bounds_center: [0.0; 3],
            bounds_radius: 1.0,
            light_yaw: 40.0,
            light_pitch: 50.0,
            light_intensity: 1.0,
            key_rgb: [1.0, 1.0, 1.0],
            ambient_rgb: [0.18, 0.19, 0.22],
            rim_rgb: [0.25, 0.3, 0.4],
            environment: EnvironmentView::default(),
            eye: None,
            target: None,
        }
    }
}

/// Which position axis a surface's u runs along, and which its v: the
/// axis whose coordinate moves most with u (and with v), by covariance
/// over the surface's vertices. None when there are no uvs to ask, or the
/// uvs do not follow any axis.
fn uv_axes(samples: &[([f32; 3], [f32; 2])]) -> Option<(usize, usize)> {
    if samples.len() < 3 {
        return None;
    }
    let n = samples.len() as f32;
    let mut mean_p = [0.0f32; 3];
    let mut mean_uv = [0.0f32; 2];
    for (p, uv) in samples {
        for k in 0..3 {
            mean_p[k] += p[k] / n;
        }
        mean_uv[0] += uv[0] / n;
        mean_uv[1] += uv[1] / n;
    }
    let mut cov = [[0.0f32; 3]; 2];
    for (p, uv) in samples {
        for (t, c) in cov.iter_mut().enumerate() {
            for k in 0..3 {
                c[k] += (p[k] - mean_p[k]) * (uv[t] - mean_uv[t]);
            }
        }
    }
    let pick = |c: &[f32; 3]| -> Option<usize> {
        let (best, value) = c
            .iter()
            .enumerate()
            .map(|(k, v)| (k, v.abs()))
            .fold((0, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
        (value > 1e-9).then_some(best)
    };
    Some((pick(&cov[0])?, pick(&cov[1])?))
}

/// Where the camera stands and what it looks at: the orbit the view
/// describes unless a flown eye or a gaze replaces them.
fn eye_and_center(view: &ModelView) -> ([f32; 3], [f32; 3]) {
    let radius = view.bounds_radius.max(1e-6);
    let distance = (view.distance.max(1.05) as f32) * radius;
    let center = view.target.unwrap_or(view.bounds_center);
    let eye = view.eye.unwrap_or_else(|| {
        let toward = direction(view.yaw, view.pitch);
        let about = view.bounds_center;
        [
            about[0] + toward[0] * distance,
            about[1] + toward[1] * distance,
            about[2] + toward[2] * distance,
        ]
    });
    (eye, center)
}

const SHADER: &str = r#"
struct Frame {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,      // xyz toward the light, w = intensity
    key_rgb: vec4<f32>,
    ambient_rgb: vec4<f32>,
    rim_rgb: vec4<f32>,
    // x = 1 when an environment is bound, y = its intensity, z = its
    // rotation (radians), w = the blurriest mip level.
    env_params: vec4<f32>,
    // The key light's own camera (orthographic) — what the shadow map
    // was drawn through — and the camera mirrored in the floor, whose
    // picture the floor shows.
    light_view_proj: mat4x4<f32>,
    mirror_view_proj: mat4x4<f32>,
    // The floor: x = its height, y = how dark the shadow, z = the
    // shadow's edge in shadow-map texels, w = how much of the mirror.
    floor: vec4<f32>,
    // x = the mirror's blur (mip levels), y = the scene's radius, z = 1
    // in the mirror pass (nothing under the floor is drawn), w = 1 when a
    // shadow map is bound.
    floor2: vec4<f32>,
    // The bodies' footprints (x, z centre; x, z half size) and their
    // lowest and highest point above the floor; `counts.x` says how many.
    footprints: array<vec4<f32>, 8>,
    lifts: array<vec4<f32>, 8>,
    counts: vec4<f32>,
};
struct Material {
    base_color: vec4<f32>,
    // x = metallic, y = roughness, z = 1 file texture (lit) / 2 a picture
    // bound by the project (unlit), w = 1 double sided
    factors: vec4<f32>,
    // x = the slot's own aspect (width / height of the surface its uvs
    // span), so a bound picture is fitted rather than stretched; y = 1
    // when a normal map is bound, z = 1 when a metallic-roughness map is.
    fit: vec4<f32>,
    // A worn picture's tiling: xy repeats across u and v, zw shifts.
    uv: vec4<f32>,
    // x = how much of the surface has dissolved (0 whole, 1 gone), y =
    // the size of the cells it goes in, world units.
    extra: vec4<f32>,
    // A finish word's extras: x = clear coat (0 none … 1 a full coat),
    // y = the coat's roughness, z = anisotropy (a grain), w = the
    // dielectric's reflectance straight on (F0; 0.04 plastic and glass).
    finish: vec4<f32>,
    // x = transmission through a thin body (0 opaque … 1 clear).
    finish2: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var env_tex: texture_2d<f32>;
@group(0) @binding(2) var env_samp: sampler;
@group(0) @binding(3) var shadow_tex: texture_depth_2d;
@group(0) @binding(4) var shadow_samp: sampler_comparison;
@group(0) @binding(5) var refl_tex: texture_2d<f32>;
@group(0) @binding(6) var refl_samp: sampler;
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var base_tex: texture_2d<f32>;
@group(1) @binding(2) var base_samp: sampler;
@group(1) @binding(3) var normal_tex: texture_2d<f32>;
@group(1) @binding(4) var mr_tex: texture_2d<f32>;
struct Placement {
    model: mat4x4<f32>,
    // The upper 3×3's inverse transpose, for normals under a scale.
    normal: mat4x4<f32>,
};
@group(2) @binding(0) var<uniform> placement: Placement;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

// The shadow pass: the same geometry through the light's camera, depth
// only.
@vertex
fn vs_shadow(v: VsIn) -> @builtin(position) vec4<f32> {
    let world = placement.model * vec4<f32>(v.pos, 1.0);
    return frame.light_view_proj * world;
}

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    let world = placement.model * vec4<f32>(v.pos, 1.0);
    out.clip = frame.view_proj * world;
    out.world = world.xyz;
    out.normal = (placement.normal * vec4<f32>(v.normal, 0.0)).xyz;
    out.uv = v.uv;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

// A tangent frame from screen-space derivatives (Schüler's cotangent
// frame), so a normal map needs no tangent attribute: `tn` is the map's
// tangent-space normal, +Y up as glTF stores it.
fn perturb_normal(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>, tn: vec3<f32>) -> vec3<f32> {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;
    let invmax = inverseSqrt(max(dot(t, t), dot(b, b)));
    let tbn = mat3x3<f32>(-t * invmax, b * invmax, n);
    return normalize(tbn * tn);
}

fn env_sample(dir: vec3<f32>, lod: f32) -> vec3<f32> {
    let d = normalize(dir);
    let yaw = atan2(d.x, d.z) + frame.env_params.z;
    let u = yaw / 6.2831853 + 0.5;
    let v = 0.5 - asin(clamp(d.y, -1.0, 1.0)) / 3.1415927;
    return textureSampleLevel(env_tex, env_samp, vec2<f32>(u, v), lod).rgb * frame.env_params.y;
}

const PI: f32 = 3.1415927;

// GGX (Trowbridge-Reitz) normal distribution; `a` is roughness squared.
fn ggx_d(ndh: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}
// The anisotropic form (Burley, as Filament writes it): the lobe
// stretched along the tangent, `at` and `ab` the two roughnesses.
fn ggx_d_aniso(ndh: f32, tdh: f32, bdh: f32, at: f32, ab: f32) -> f32 {
    let a2 = at * ab;
    let v = vec3<f32>(ab * tdh, at * bdh, a2 * ndh);
    let v2 = dot(v, v);
    let w2 = a2 / max(v2, 1e-8);
    return a2 * w2 * w2 / PI;
}
// Height-correlated Smith visibility for GGX (Heitz), with the
// 1 / (4 n·l n·v) folded in.
fn smith_vis(ndv: f32, ndl: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = ndl * sqrt(ndv * ndv * (1.0 - a2) + a2);
    let gl = ndv * sqrt(ndl * ndl * (1.0 - a2) + a2);
    return 0.5 / max(gv + gl, 1e-4);
}
fn fresnel_schlick(f0: vec3<f32>, vdh: f32) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);
}
// The split-sum's second half without a lookup table (Karis's fit for
// mobile): what a surface of this F0 and roughness reflects of a
// prefiltered environment at this viewing angle.
fn env_brdf_approx(f0: vec3<f32>, roughness: f32, ndv: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * ndv)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + vec3<f32>(ab.y);
}
// The shoulder of Khronos's PBR-neutral tone map, alone: highlights past
// 0.76 roll off toward white instead of clipping, and nothing under it
// moves, so a theme-lit body renders as it always did.
fn compress_highlights(c: vec3<f32>) -> vec3<f32> {
    let start = 0.76;
    let peak = max(c.r, max(c.g, c.b));
    if (peak <= start) {
        return c;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    let scaled = c * (new_peak / peak);
    let g = 1.0 - 1.0 / (0.15 * (peak - new_peak) + 1.0);
    return mix(scaled, vec3<f32>(new_peak), g);
}
// The surface's tangent from the same screen-space derivatives the
// normal map uses — the direction a brushed grain runs, along u.
fn tangent_of(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    if (dot(t, t) < 1e-14) {
        let up = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.y) < 0.9);
        return normalize(cross(up, n));
    }
    return normalize(t - n * dot(n, t));
}
// What the world contributes to a direction: the bound environment
// through its prefiltered mips, else the theme's stand-in sky-to-ground
// gradient with a darker horizon band.
fn world_light(dir: vec3<f32>, roughness: f32) -> vec3<f32> {
    if (frame.env_params.x > 0.5) {
        return env_sample(dir, roughness * frame.env_params.w);
    }
    let up = clamp(dir.y * 0.5 + 0.5, 0.0, 1.0);
    let sky = frame.rim_rgb.rgb * 1.4 + vec3<f32>(0.35);
    let ground = frame.ambient_rgb.rgb * 0.6;
    let env = mix(ground, sky, up);
    return env * (0.7 + 0.3 * smoothstep(0.0, 0.25, abs(dir.y)));
}
// How much of the key light reaches a point: the shadow map read with
// a 4×4 grid of compared taps over the floor's softness, so the edge is
// soft. Points outside the map, or with no map bound, are lit.
fn shadow_at(p: vec3<f32>, n: vec3<f32>) -> f32 {
    if (frame.floor2.w < 0.5) {
        return 1.0;
    }
    let l = normalize(frame.light_dir.xyz);
    let ndl = max(dot(n, l), 0.0);
    let offset = p + n * frame.floor2.y * 0.004 * (1.0 - ndl * 0.5);
    let c = frame.light_view_proj * vec4<f32>(offset, 1.0);
    if (abs(c.w) < 1e-6) {
        return 1.0;
    }
    let ndc = c.xyz / c.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z > 1.0 || ndc.z < 0.0) {
        return 1.0;
    }
    let depth = ndc.z - 0.0008 - 0.002 * (1.0 - ndl);
    let texel = 1.0 / f32(textureDimensions(shadow_tex).x);
    let r = max(frame.floor.z, 0.5) * texel;
    var sum = 0.0;
    for (var y: i32 = 0; y < 4; y = y + 1) {
        for (var x: i32 = 0; x < 4; x = x + 1) {
            let o = vec2<f32>((f32(x) - 1.5) / 1.5, (f32(y) - 1.5) / 1.5) * r;
            sum = sum + textureSampleCompareLevel(shadow_tex, shadow_samp, uv + o, depth);
        }
    }
    return sum / 16.0;
}
// The darkening where a body touches the floor: 1 open … 0 fully
// occluded, from each footprint, strongest under a body that stands on
// the floor and gone once it is lifted a footprint's width above it.
fn contact_at(p: vec3<f32>) -> f32 {
    var open = 1.0;
    let n = i32(frame.counts.x);
    for (var i: i32 = 0; i < 8; i = i + 1) {
        if (i >= n) {
            break;
        }
        let f = frame.footprints[i];
        let spread = max(max(f.z, f.w) * 0.6, 0.02);
        let d = vec2<f32>(abs(p.x - f.x) - f.z, abs(p.z - f.y) - f.w);
        let outside = length(max(d, vec2<f32>(0.0)));
        let near = 1.0 - smoothstep(0.0, spread, outside);
        let touching = clamp(1.0 - frame.lifts[i].x / spread, 0.0, 1.0);
        open = open * (1.0 - near * touching * 0.7);
    }
    return open;
}
// How much the floor catches at a point: 1 among the bodies, fading to
// 0 a few footprints away, further for a tall body whose mirror image
// reaches further.
fn floor_fade(p: vec3<f32>) -> f32 {
    var fade = 0.0;
    let n = i32(frame.counts.x);
    for (var i: i32 = 0; i < 8; i = i + 1) {
        if (i >= n) {
            break;
        }
        let f = frame.footprints[i];
        // Whole within half the reach — a body's mirror image lies about
        // its own height along the floor — then off to nothing.
        let reach = max(max(f.z, f.w), 0.02) * 3.0 + frame.lifts[i].y * 1.5;
        let d = vec2<f32>(abs(p.x - f.x) - f.z, abs(p.z - f.y) - f.w);
        let outside = length(max(d, vec2<f32>(0.0)));
        fade = max(fade, 1.0 - smoothstep(reach * 0.5, reach, outside));
    }
    return fade;
}

// A clear coat's own light: the key through a narrow GGX lobe and the
// world at the coat's roughness, both at glass's F0 — added over a body
// or over a screen alike.
fn coat_light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, key: vec3<f32>, cc: f32, cc_rough: f32) -> vec3<f32> {
    let h = normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    let ndh = max(dot(n, h), 0.0);
    let ndv = max(dot(n, v), 1e-3);
    let vdh = max(dot(v, h), 0.0);
    let a = max(cc_rough * cc_rough, 0.001);
    let fc = 0.04 + 0.96 * pow(1.0 - vdh, 5.0);
    let direct = ggx_d(ndh, a) * smith_vis(ndv, ndl, a) * fc * PI * ndl * key;
    let world = world_light(reflect(-v, n), cc_rough) * env_brdf_approx(vec3<f32>(0.04), cc_rough, ndv);
    return (direct + world) * cc;
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    // The mirror pass: nothing under the floor exists.
    if (frame.floor2.z > 0.5 && in.world.y < frame.floor.x - 0.0005) {
        discard;
    }
    // The floor itself, a catcher: invisible but for the shadow, the
    // contact and the mirror it holds, fading away from the bodies.
    if (material.factors.z > 4.5) {
        let p = in.world;
        let up = vec3<f32>(0.0, 1.0, 0.0);
        let fade = floor_fade(p);
        if (fade <= 0.001) {
            discard;
        }
        let visible = shadow_at(p, up);
        let shadow_dark = frame.floor.y * (1.0 - visible) * fade;
        let contact_dark = (1.0 - contact_at(p)) * fade;
        let occlusion = 1.0 - (1.0 - shadow_dark) * (1.0 - contact_dark);
        var mirror = vec4<f32>(0.0);
        if (frame.floor.w > 0.0) {
            let c = frame.mirror_view_proj * vec4<f32>(p, 1.0);
            if (c.w > 1e-5) {
                let ndc = c.xyz / c.w;
                let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
                if (uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0) {
                    let v = normalize(frame.camera_pos.xyz - p);
                    let grazing = pow(1.0 - max(dot(up, v), 0.0), 3.0);
                    let w = frame.floor.w * fade * (0.75 + 0.25 * grazing);
                    mirror = textureSampleLevel(refl_tex, refl_samp, uv, frame.floor2.x) * w;
                }
            }
        }
        // The mirror's picture is the pass's own output — encoded and
        // premultiplied already; the darkening adds no colour, only
        // alpha, and the mirror covers by its own.
        let alpha = 1.0 - (1.0 - occlusion) * (1.0 - mirror.a);
        return vec4<f32>(mirror.rgb, alpha);
    }
    // A dissolving body: cells of its surface go in a fixed random order
    // as the amount rises — the order a morph's points leave in.
    if (material.extra.x > 0.0) {
        let cell = floor(in.world / max(material.extra.y, 0.0001));
        let h = fract(sin(dot(cell, vec3<f32>(12.9898, 78.233, 37.719))) * 43758.547);
        if (h < material.extra.x) { discard; }
    }
    var n = normalize(in.normal);
    if (!front) {
        if (material.factors.w < 0.5) { discard; }
        n = -n;
    }
    var albedo = material.base_color;
    if (material.factors.z > 3.5) {
        // A picture WORN by the slot — a label, a print, a video on a
        // body: the slot's own colour where the picture is clear, the
        // picture's colour where it is not, then lit and finished like
        // any surface. Tiled by repeat, shifted by offset; frames arrive
        // premultiplied.
        let uv = in.uv * material.uv.xy + material.uv.zw;
        let t = textureSample(base_tex, base_samp, uv);
        let straight = t.rgb / max(t.a, 0.001);
        albedo = vec4<f32>(mix(albedo.rgb, srgb_to_linear(straight), t.a), albedo.a);
    } else if (material.factors.z > 2.5) {
        // A picture standing in the scene with its own transparency — a
        // caption's raster on a billboard: straight through, premultiplied,
        // and nothing written where it is clear.
        let t = textureSample(base_tex, base_samp, in.uv);
        if (t.a < 0.02) { discard; }
        return t;
    } else if (material.factors.z > 1.5) {
        // A picture BOUND to the slot by the project — a screenshot on a
        // screen — reads as the screen would: the picture itself, unlit,
        // fitted inside the surface (its own proportions kept, the slot's
        // colour where it does not reach), over the slot's colour where
        // it is transparent.
        let size = vec2<f32>(textureDimensions(base_tex));
        let picture_aspect = max(size.x, 1.0) / max(size.y, 1.0);
        let slot_aspect = max(material.fit.x, 0.0001);
        var scale = vec2<f32>(1.0, 1.0);
        if (picture_aspect > slot_aspect) {
            scale.y = picture_aspect / slot_aspect;
        } else {
            scale.x = slot_aspect / picture_aspect;
        }
        let uv = (in.uv - vec2<f32>(0.5)) * scale + vec2<f32>(0.5);
        let inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);
        let t = textureSample(base_tex, base_samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
        let under = linear_to_srgb(clamp(material.base_color.rgb * frame.ambient_rgb.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
        let shown = mix(under, t.rgb, t.a * inside);
        let coat = material.finish.x;
        if (coat > 0.0) {
            // Glass over the screen: the picture stays the display it is,
            // dimmed a little where the coat turns to mirror, with the
            // coat's own highlight and the world laid over it.
            let v = normalize(frame.camera_pos.xyz - in.world);
            let l = normalize(frame.light_dir.xyz);
            let ndv = max(dot(n, v), 1e-3);
            let fc_view = 0.04 + 0.96 * pow(1.0 - ndv, 5.0);
            let key = frame.key_rgb.rgb * frame.light_dir.w;
            let lit = srgb_to_linear(shown) * (1.0 - coat * fc_view)
                + coat_light(n, v, l, key, coat, material.finish.y);
            let out = linear_to_srgb(clamp(compress_highlights(lit), vec3<f32>(0.0), vec3<f32>(1.0)));
            return vec4<f32>(out * material.base_color.a, material.base_color.a);
        }
        return vec4<f32>(shown * material.base_color.a, material.base_color.a);
    } else if (material.factors.z > 0.5) {
        let t = textureSample(base_tex, base_samp, in.uv);
        albedo = vec4<f32>(albedo.rgb * srgb_to_linear(t.rgb), albedo.a * t.a);
    }
    if (material.fit.y > 0.5) {
        let tn = textureSample(normal_tex, base_samp, in.uv).xyz * 2.0 - vec3<f32>(1.0);
        n = perturb_normal(n, in.world, in.uv, tn);
    }
    var metallic = material.factors.x;
    var roughness = clamp(material.factors.y, 0.05, 1.0);
    if (material.fit.z > 0.5) {
        let mr = textureSample(mr_tex, base_samp, in.uv);
        roughness = clamp(material.factors.y * mr.g, 0.05, 1.0);
        metallic = material.factors.x * mr.b;
    }
    let cc = material.finish.x;
    let cc_rough = clamp(material.finish.y, 0.03, 1.0);
    let aniso = material.finish.z;
    let f0_dielectric = vec3<f32>(material.finish.w);
    let transmission = clamp(material.finish2.x, 0.0, 1.0);

    let l = normalize(frame.light_dir.xyz);
    let v = normalize(frame.camera_pos.xyz - in.world);
    let h = normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    let ndh = max(dot(n, h), 0.0);
    let ndv = max(dot(n, v), 1e-3);
    let vdh = max(dot(v, h), 0.0);
    let a = max(roughness * roughness, 0.002);
    // A metal's reflectance is its colour; a dielectric's is its F0, and
    // its diffuse is what Fresnel leaves.
    let f0 = mix(f0_dielectric, albedo.rgb, metallic);
    let f = fresnel_schlick(f0, vdh);
    var d: f32;
    // A grain: the lobe stretched along the tangent, and the world's
    // reflection bent the same way (Filament's anisotropic reflection),
    // so brushed metal smears the light boxes across it.
    var refl = reflect(-v, n);
    if (aniso > 0.001) {
        let t = tangent_of(n, in.world, in.uv);
        let b = cross(n, t);
        let at = max(a * (1.0 + aniso), 0.002);
        let ab = max(a * (1.0 - aniso), 0.002);
        d = ggx_d_aniso(ndh, dot(t, h), dot(b, h), at, ab);
        let grain_tangent = cross(b, v);
        let grain_normal = cross(grain_tangent, b);
        let bent = normalize(mix(n, grain_normal, aniso));
        refl = reflect(-v, bent);
    } else {
        d = ggx_d(ndh, a);
    }
    // The key's strength is the irradiance a facing Lambert surface
    // receives, as it always was here; the BRDF times π keeps that.
    let key = frame.key_rgb.rgb * frame.light_dir.w;
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    // A stage with a floor casts real shadows: the key is what the
    // shadow map lets through.
    let lit_by_key = shadow_at(in.world, n);
    var body = kd * albedo.rgb * ndl * key * lit_by_key;
    var spec = d * smith_vis(ndv, ndl, a) * f * PI * ndl * key * lit_by_key;
    let ambient = albedo.rgb * (1.0 - metallic) * frame.ambient_rgb.rgb;
    let rim = frame.rim_rgb.rgb * pow(1.0 - ndv, 3.0) * 0.6 * (1.0 - metallic * 0.5);
    // The world: the reflection through the prefiltered environment
    // weighted by the split-sum's BRDF term, and a diffuse fill from
    // its blurriest levels when one is bound.
    var fill = vec3<f32>(0.0);
    if (frame.env_params.x > 0.5) {
        fill = albedo.rgb * (1.0 - metallic) * env_sample(n, frame.env_params.w - 1.0) * 0.5;
    }
    var spec_env = world_light(refl, roughness) * env_brdf_approx(f0, roughness, ndv);
    body = body + ambient + fill + rim;
    if (cc > 0.0) {
        // The coat takes its share of the light first; what it reflects
        // the body under it never gets.
        let fc_view = 0.04 + 0.96 * pow(1.0 - ndv, 5.0);
        let under = 1.0 - cc * fc_view;
        body = body * under;
        spec = spec * under;
        spec_env = spec_env * under + coat_light(n, v, l, key, cc, cc_rough);
    }
    // Thin glass: the body's weight drops with what passes through, the
    // reflections stay, and the alpha carries what stands behind — more
    // straight on, less at a grazing angle where glass turns to mirror.
    let fv = f0_dielectric.x + (1.0 - f0_dielectric.x) * pow(1.0 - ndv, 5.0);
    let through = transmission * (1.0 - fv);
    let lit = compress_highlights(body * (1.0 - transmission) + spec + spec_env);
    let encoded = linear_to_srgb(clamp(lit, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(encoded * albedo.a, albedo.a * (1.0 - through));
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct FrameRaw {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    key_rgb: [f32; 4],
    ambient_rgb: [f32; 4],
    rim_rgb: [f32; 4],
    env_params: [f32; 4],
    light_view_proj: [[f32; 4]; 4],
    mirror_view_proj: [[f32; 4]; 4],
    floor: [f32; 4],
    floor2: [f32; 4],
    footprints: [[f32; 4]; 8],
    lifts: [[f32; 4]; 8],
    counts: [f32; 4],
}

impl FrameRaw {
    /// The floor's rows: its plane, the light's camera for the shadow
    /// map, the mirrored camera for the reflection, and the bodies'
    /// footprints. `shadow_bound` says the map exists; `mirror_pass`
    /// marks the frame drawn from under the floor.
    fn with_floor(
        mut self,
        view: &ModelView,
        floor: &FloorView,
        aspect: f32,
        shadow_bound: bool,
        mirror_pass: bool,
    ) -> FrameRaw {
        self.light_view_proj = light_view_proj(view);
        self.mirror_view_proj = frame_uniforms(&mirrored_view(view, floor.y), aspect).view_proj;
        self.floor = [
            floor.y,
            floor.shadow.clamp(0.0, 1.0),
            floor.softness.max(0.0),
            floor.reflection.clamp(0.0, 1.0),
        ];
        self.floor2 = [
            floor.blur.max(0.0),
            view.bounds_radius.max(1e-6),
            if mirror_pass { 1.0 } else { 0.0 },
            if shadow_bound { 1.0 } else { 0.0 },
        ];
        for (i, f) in floor.footprints.iter().take(8).enumerate() {
            self.footprints[i] = [f.center_xz[0], f.center_xz[1], f.half_xz[0], f.half_xz[1]];
            self.lifts[i] = [f.bottom.max(0.0), f.top.max(0.0), 0.0, 0.0];
        }
        self.counts = [floor.footprints.len().min(8) as f32, 0.0, 0.0, 0.0];
        self
    }
}

/// The key light's own camera: orthographic, looking along the light at
/// the bounds centre, wide enough for the bounds sphere — what the
/// shadow map is drawn through.
fn light_view_proj(view: &ModelView) -> Mat4 {
    let r = view.bounds_radius.max(1e-6) * 1.25;
    let c = view.bounds_center;
    let l = norm(direction(view.light_yaw, view.light_pitch));
    let eye = [
        c[0] + l[0] * r * 2.0,
        c[1] + l[1] * r * 2.0,
        c[2] + l[2] * r * 2.0,
    ];
    let forward = norm(sub(c, eye));
    let mut up = [0.0, 1.0, 0.0];
    if dot(forward, up).abs() > 0.99 {
        up = [1.0, 0.0, 0.0];
    }
    let right = norm(cross(forward, up));
    let up = cross(right, forward);
    let view_m = [
        [right[0], up[0], -forward[0], 0.0],
        [right[1], up[1], -forward[1], 0.0],
        [right[2], up[2], -forward[2], 0.0],
        [-dot(right, eye), -dot(up, eye), dot(forward, eye), 1.0],
    ];
    let (near, far) = (0.01f32, r * 4.0);
    let proj = [
        [1.0 / r, 0.0, 0.0, 0.0],
        [0.0, 1.0 / r, 0.0, 0.0],
        [0.0, 0.0, 1.0 / (near - far), 0.0],
        [0.0, 0.0, near / (near - far), 1.0],
    ];
    mul(&proj, &view_m)
}

/// The camera mirrored in the floor: what a floor point shows is what
/// this camera sees through the same point.
fn mirrored_view(view: &ModelView, floor_y: f32) -> ModelView {
    let (eye, center) = eye_and_center(view);
    let mut mirrored = *view;
    mirrored.eye = Some([eye[0], 2.0 * floor_y - eye[1], eye[2]]);
    mirrored.target = Some([center[0], 2.0 * floor_y - center[1], center[2]]);
    mirrored.roll = -view.roll;
    mirrored
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MaterialRaw {
    base_color: [f32; 4],
    factors: [f32; 4],
    fit: [f32; 4],
    uv: [f32; 4],
    extra: [f32; 4],
    finish: [f32; 4],
    finish2: [f32; 4],
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

fn slice_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

const BLIT_SHADER: &str = r#"
struct BlitOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_blit(@builtin(vertex_index) i: u32) -> BlitOut {
    var corners = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: BlitOut;
    let p = corners[i];
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>(p.x * 0.5 + 0.5, 1.0 - (p.y * 0.5 + 0.5));
    return out;
}
@group(0) @binding(0) var blit_src: texture_2d<f32>;
@group(0) @binding(1) var blit_samp: sampler;
@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    return textureSample(blit_src, blit_samp, in.uv);
}
"#;

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    material: usize,
    /// Which of the caller's matrices places this mesh.
    node: usize,
    placement: wgpu::Buffer,
    placement_bind: wgpu::BindGroup,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PlacementRaw {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 4],
}

/// A column-major 4×4 world matrix, `m[column][row]`.
pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

struct GpuMaterial {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    /// The file's own colour and factors, what a slot falls back to for
    /// whatever a binding leaves unsaid (a finish without a colour keeps
    /// the file's colour; a colour without a finish keeps the file's).
    file_base_color: [f32; 4],
    file_metallic: f32,
    file_roughness: f32,
    double_sided: bool,
    /// 0 none, 1 the file's own texture (lit), 2 a picture the project
    /// bound to the slot (shown as-is, a screen), 3 a billboard's own
    /// picture, 4 a picture WORN by the slot (lit, under the finish).
    textured: f32,
    /// Whether a bound picture is worn (lit) rather than shown (a
    /// screen), and its tiling: repeat u, v and offset u, v.
    worn: bool,
    uv: [f32; 4],
    /// How much of the surface has dissolved (0 whole, 1 gone) and the
    /// size of the cells it goes in — a morph's body on the way out or in.
    dissolve: f32,
    cell: f32,
    /// Width over height of the surface this slot's uvs span, from the
    /// geometry that wears it — what a bound picture is fitted to.
    aspect: f32,
    /// The file's normal map and metallic-roughness map, if any — kept so
    /// a rebind (a picture on the slot) carries them along.
    normal_view: Option<wgpu::TextureView>,
    mr_view: Option<wgpu::TextureView>,
    /// The bound picture's own mip chain, kept alive while it is bound.
    bound_copy: Option<wgpu::Texture>,
    /// What a finish word adds beyond the two factors; plain by default.
    finish: SurfaceFinish,
}

impl GpuMaterial {
    /// `fit`'s flags: a normal map bound, a metallic-roughness map bound.
    fn map_flags(&self) -> (f32, f32) {
        (
            if self.normal_view.is_some() { 1.0 } else { 0.0 },
            if self.mr_view.is_some() { 1.0 } else { 0.0 },
        )
    }

    /// The uniform, from every field — one place, so no setter can leave
    /// a row stale.
    fn raw(&self) -> MaterialRaw {
        let (has_normal, has_mr) = self.map_flags();
        let (finish, finish2) = self.finish.raw();
        MaterialRaw {
            uv: self.uv,
            extra: [self.dissolve, self.cell, 0.0, 0.0],
            base_color: self.base_color,
            factors: [
                self.metallic,
                self.roughness,
                self.textured,
                if self.double_sided { 1.0 } else { 0.0 },
            ],
            fit: [self.aspect, has_normal, has_mr, 0.0],
            finish,
            finish2,
        }
    }

    /// Light passes through this slot (thin glass): drawn after the
    /// opaque meshes, so what stands behind it is there to blend over.
    fn transmissive(&self) -> bool {
        self.finish.transmission > 0.0
    }
}

/// A model's buffers on the GPU, built once per resource and drawn many
/// times. Bind groups are the pass's, so build one through
/// [`ModelPass::upload`].
pub struct GpuModel {
    meshes: Vec<GpuMesh>,
    materials: Vec<GpuMaterial>,
}

impl GpuModel {
    /// The aspect a slot's picture is fitted against — width over height
    /// of the surface its uvs span.
    pub fn slot_aspect(&self, index: usize) -> Option<f32> {
        self.materials.get(index).map(|m| m.aspect)
    }
}

/// The pass itself: one pipeline, shared across every model layer.
pub struct ModelPass {
    pipeline: wgpu::RenderPipeline,
    /// Depth only, through the key light's camera (rung 45).
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_sampler: wgpu::Sampler,
    dummy_shadow: wgpu::TextureView,
    dummy_mirror: wgpu::TextureView,
    mirror_sampler: wgpu::Sampler,
    frame_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    /// Copies a picture level by level into a mip chain, so a bound
    /// picture minifies smoothly on a small or distant slot.
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    /// The built-in environments (studio, sunset, night), generated once.
    envs: [wgpu::TextureView; 3],
    env_sampler: wgpu::Sampler,
    placement_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    /// A one-texel "straight up" normal map, what an unmapped material binds.
    flat_normal: wgpu::TextureView,
}

impl ModelPass {
    pub fn new(ctx: &GpuContext) -> Result<ModelPass, GpuError> {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model-pass"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-frame"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
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
        let placement_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-placement"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model-pass"),
            bind_group_layouts: &[&frame_layout, &material_layout, &placement_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model-pass"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OUTPUT_FORMAT,
                    // Premultiplied over: opaque geometry writes 1, the
                    // cleared background stays 0.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // Back faces are discarded in the shader when the material
                // is single-sided, so double-sided materials shade both.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLES,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });
        // The shadow pass: depth only, through the light's camera, with a
        // slope-scaled bias so a lit face does not shadow itself.
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model-shadow"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
                }],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        // With no floor, a one-texel map that shadows nothing and a
        // one-texel mirror that shows nothing are bound in their place.
        let dummy_shadow = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model-shadow-none"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_shadow_view = dummy_shadow.create_view(&Default::default());
        {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("model-shadow-none"),
            });
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model-shadow-none"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dummy_shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            ctx.queue.submit(Some(encoder.finish()));
        }
        let dummy_mirror = upload_rgba(ctx, 1, 1, &[0, 0, 0, 0]).create_view(&Default::default());
        let mirror_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-mirror"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model-blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model-blit"),
            bind_group_layouts: &[&blit_layout],
            push_constant_ranges: &[],
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model-blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_blit"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OUTPUT_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });
        let white = upload_rgba(ctx, 1, 1, &[255, 255, 255, 255]).create_view(&Default::default());
        let flat_normal =
            upload_rgba(ctx, 1, 1, &[128, 128, 255, 255]).create_view(&Default::default());
        let envs = [
            upload_env(ctx, EnvPreset::Studio).create_view(&Default::default()),
            upload_env(ctx, EnvPreset::Sunset).create_view(&Default::default()),
            upload_env(ctx, EnvPreset::Night).create_view(&Default::default()),
        ];
        let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-environment"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Ok(ModelPass {
            pipeline,
            shadow_pipeline,
            shadow_sampler,
            dummy_shadow: dummy_shadow_view,
            dummy_mirror,
            mirror_sampler,
            frame_layout,
            material_layout,
            blit_pipeline,
            blit_layout,
            blit_sampler,
            envs,
            env_sampler,
            placement_layout,
            sampler,
            white,
            flat_normal,
        })
    }

    /// Build a model's buffers and material bind groups.
    pub fn upload(
        &self,
        ctx: &GpuContext,
        meshes: &[MeshInput<'_>],
        materials: &[MaterialInput<'_>],
    ) -> Result<GpuModel, GpuError> {
        use wgpu::util::DeviceExt;
        let device = &ctx.device;
        // Each slot's surface aspect: width over height of the surface its
        // uvs span. The uvs say which way is which — u runs across the
        // width, v down the height — so the extent along the axis u
        // follows is the width and along the axis v follows the height,
        // whatever plane the mesh lies in and however its node stands it
        // up. "The two longest extents" was the rule before, and a plate
        // lying in XZ with a rotation standing it up gave a phone's LONG
        // side as its width: every portrait screen fitted its picture
        // squeezed. A mesh without uvs keeps that rule, which is as good
        // as a wrapped picture on a box can be placed.
        let mut aspects = vec![1.0f32; materials.len()];
        for (index, aspect) in aspects.iter_mut().enumerate() {
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            let mut any = false;
            let mut samples: Vec<([f32; 3], [f32; 2])> = Vec::new();
            for mesh in meshes.iter().filter(|m| m.material == index) {
                for &i in mesh.indices {
                    if let Some(p) = mesh.positions.get(i as usize) {
                        any = true;
                        for k in 0..3 {
                            min[k] = min[k].min(p[k]);
                            max[k] = max[k].max(p[k]);
                        }
                        if let Some(uv) = mesh.uvs.get(i as usize) {
                            samples.push((*p, *uv));
                        }
                    }
                }
            }
            if !any {
                continue;
            }
            let extents = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
            let (w, h) = match uv_axes(&samples) {
                Some((across, down)) if across != down => (extents[across], extents[down]),
                _ => {
                    let mut sorted = extents;
                    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                    if extents[2] <= extents[0].min(extents[1]) {
                        (extents[0], extents[1])
                    } else {
                        (sorted[0], sorted[1])
                    }
                }
            };
            if w > 1e-6 && h > 1e-6 {
                *aspect = w / h;
            }
        }
        let mut gpu_materials = Vec::with_capacity(materials.len());
        for (index, m) in materials.iter().enumerate() {
            let well_formed =
                |t: &(u32, u32, &[u8])| t.0 > 0 && t.1 > 0 && t.2.len() == (t.0 * t.1 * 4) as usize;
            let texture = m
                .texture
                .filter(well_formed)
                .map(|(w, h, px)| upload_rgba(ctx, w, h, px).create_view(&Default::default()));
            let normal_view = m
                .normal
                .filter(well_formed)
                .map(|(w, h, px)| upload_rgba(ctx, w, h, px).create_view(&Default::default()));
            let mr_view = m
                .metal_rough
                .filter(well_formed)
                .map(|(w, h, px)| upload_rgba(ctx, w, h, px).create_view(&Default::default()));
            let (finish, finish2) = SurfaceFinish::default().raw();
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-material"),
                contents: as_bytes(&MaterialRaw {
                    uv: [1.0, 1.0, 0.0, 0.0],
                    extra: [0.0; 4],
                    base_color: m.base_color,
                    factors: [
                        m.metallic,
                        m.roughness,
                        if texture.is_some() { 1.0 } else { 0.0 },
                        if m.double_sided { 1.0 } else { 0.0 },
                    ],
                    fit: [
                        aspects[index],
                        if normal_view.is_some() { 1.0 } else { 0.0 },
                        if mr_view.is_some() { 1.0 } else { 0.0 },
                        0.0,
                    ],
                    finish,
                    finish2,
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("model-material"),
                layout: &self.material_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            texture.as_ref().unwrap_or(&self.white),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(
                            normal_view.as_ref().unwrap_or(&self.flat_normal),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(
                            mr_view.as_ref().unwrap_or(&self.white),
                        ),
                    },
                ],
            });
            gpu_materials.push(GpuMaterial {
                uniform,
                bind,
                base_color: m.base_color,
                metallic: m.metallic,
                roughness: m.roughness,
                file_base_color: m.base_color,
                file_metallic: m.metallic,
                file_roughness: m.roughness,
                double_sided: m.double_sided,
                textured: if texture.is_some() { 1.0 } else { 0.0 },
                worn: false,
                uv: [1.0, 1.0, 0.0, 0.0],
                dissolve: 0.0,
                cell: 0.0,
                aspect: aspects[index],
                bound_copy: None,
                normal_view,
                mr_view,
                finish: SurfaceFinish::default(),
            });
        }
        let mut gpu_meshes = Vec::with_capacity(meshes.len());
        for mesh in meshes {
            let n = mesh.positions.len();
            if n == 0 || mesh.indices.is_empty() {
                continue;
            }
            let mut interleaved: Vec<f32> = Vec::with_capacity(n * 8);
            for i in 0..n {
                let p = mesh.positions[i];
                let nn = mesh.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]);
                let uv = mesh.uvs.get(i).copied().unwrap_or([0.0, 0.0]);
                interleaved
                    .extend_from_slice(&[p[0], p[1], p[2], nn[0], nn[1], nn[2], uv[0], uv[1]]);
            }
            let indices: Vec<u32> = mesh
                .indices
                .iter()
                .copied()
                .filter(|&i| (i as usize) < n)
                .collect();
            let index_count = (indices.len() / 3 * 3) as u32;
            if index_count == 0 {
                continue;
            }
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-vertices"),
                contents: slice_bytes(&interleaved),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-indices"),
                contents: slice_bytes(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            let placement = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-placement"),
                contents: as_bytes(&PlacementRaw {
                    model: IDENTITY,
                    normal: IDENTITY,
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let placement_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("model-placement"),
                layout: &self.placement_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: placement.as_entire_binding(),
                }],
            });
            gpu_meshes.push(GpuMesh {
                vertices,
                indices: index_buffer,
                index_count,
                material: mesh.material.min(gpu_materials.len().saturating_sub(1)),
                node: mesh.node,
                placement,
                placement_bind,
            });
        }
        Ok(GpuModel {
            meshes: gpu_meshes,
            materials: gpu_materials,
        })
    }

    /// Paint one material slot a colour (a `materials` binding), keeping
    /// its texture and factors. Straight RGBA, linear.
    pub fn recolor(&self, ctx: &GpuContext, model: &mut GpuModel, material: usize, rgba: [f32; 4]) {
        self.paint(ctx, model, material, Some(rgba), None, None);
    }

    /// Paint one material slot from a `materials` binding: a colour, a
    /// finish (`metallic`, `roughness`, each 0…1), or both — whatever the
    /// binding leaves out falls back to the file's own value, so a
    /// binding removed later paints the file back. The slot's texture is
    /// kept. Colour is straight RGBA, linear.
    pub fn paint(
        &self,
        ctx: &GpuContext,
        model: &mut GpuModel,
        material: usize,
        rgba: Option<[f32; 4]>,
        metallic: Option<f32>,
        roughness: Option<f32>,
    ) {
        if let Some(m) = model.materials.get_mut(material) {
            m.base_color = rgba.unwrap_or(m.file_base_color);
            m.metallic = metallic.unwrap_or(m.file_metallic).clamp(0.0, 1.0);
            m.roughness = roughness.unwrap_or(m.file_roughness).clamp(0.0, 1.0);
            ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
        }
    }

    /// What a finish WORD adds to one slot beyond the two factors (rung
    /// 44): the coat, the grain, the reflectance, the transmission. The
    /// default is the plain surface every slot starts with.
    pub fn set_finish(
        &self,
        ctx: &GpuContext,
        model: &mut GpuModel,
        material: usize,
        finish: SurfaceFinish,
    ) {
        if let Some(m) = model.materials.get_mut(material) {
            if m.finish == finish {
                return;
            }
            m.finish = finish;
            ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
        }
    }

    /// Paint one material slot with a picture — a `materials` binding to a
    /// resource: the slot's base colour texture becomes `view`, sampled
    /// with the mesh's own uvs (sRGB, as every imported frame is). With
    /// its size in `pixels` the picture is copied into a mip chain first,
    /// so it minifies smoothly on a small or distant slot instead of
    /// shimmering; without, it is bound as it is.
    pub fn set_texture(
        &self,
        ctx: &GpuContext,
        model: &mut GpuModel,
        material: usize,
        view: &wgpu::TextureView,
        pixels: Option<(u32, u32)>,
    ) {
        let Some(m) = model.materials.get_mut(material) else {
            return;
        };
        m.bound_copy = match pixels {
            Some((w, h)) if w.max(h) > 64 => Some(self.mipmapped(ctx, view, w, h)),
            _ => None,
        };
        let chain_view = m
            .bound_copy
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        let view = chain_view.as_ref().unwrap_or(view);
        m.textured = if m.worn { 4.0 } else { 2.0 };
        ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
        m.bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model-material-bound"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: m.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        m.normal_view.as_ref().unwrap_or(&self.flat_normal),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(
                        m.mr_view.as_ref().unwrap_or(&self.white),
                    ),
                },
            ],
        });
    }

    /// A copy of `view` (`width × height`) with every mip level filled,
    /// each a box filter of the one above — what a bound picture samples
    /// through, so the trilinear sampler has something to minify to.
    fn mipmapped(
        &self,
        ctx: &GpuContext,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let (width, height) = (width.max(1), height.max(1));
        let levels = 32 - width.max(height).leading_zeros();
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model-picture-mips"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("model-picture-mips"),
            });
        let level_view = |level: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let mut previous: Option<wgpu::TextureView> = None;
        for level in 0..levels.max(1) {
            let source = previous.as_ref().unwrap_or(view);
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("model-blit"),
                layout: &self.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                    },
                ],
            });
            let target = level_view(level);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("model-blit"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
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
                pass.set_pipeline(&self.blit_pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            previous = Some(target);
        }
        ctx.queue.submit(Some(encoder.finish()));
        texture
    }

    /// How much of every slot of a body has dissolved — 0 whole, 1 gone —
    /// in cells `cell` wide, world units. A morph sets it on the body
    /// its points leave and on the one they land on.
    pub fn set_dissolve(&self, ctx: &GpuContext, model: &mut GpuModel, amount: f32, cell: f32) {
        let amount = amount.clamp(0.0, 1.0);
        for m in &mut model.materials {
            if (m.dissolve - amount).abs() < 1e-6 && (m.cell - cell).abs() < 1e-6 {
                continue;
            }
            m.dissolve = amount;
            m.cell = cell;
            ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
        }
    }

    /// How one slot wears its bound picture: `worn` lights it as the
    /// slot's colour under the finish (a label, a video on a body),
    /// else it is shown unlit and fitted, as a screen; `repeat` tiles
    /// it across u and v, `offset` shifts it. Takes effect on the
    /// picture already bound and on the next one.
    pub fn set_wear(
        &self,
        ctx: &GpuContext,
        model: &mut GpuModel,
        material: usize,
        worn: bool,
        repeat: [f32; 2],
        offset: [f32; 2],
    ) {
        let Some(m) = model.materials.get_mut(material) else {
            return;
        };
        m.worn = worn;
        m.uv = [repeat[0], repeat[1], offset[0], offset[1]];
        if m.textured > 1.5 && m.textured < 2.5 || m.textured > 3.5 {
            m.textured = if worn { 4.0 } else { 2.0 };
        }
        ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
    }

    /// A stage: several models and camera-facing pictures drawn through
    /// one camera into one depth buffer, at `width × height`. Models draw
    /// as `render_to_texture` does; a billboard is an unlit quad of its
    /// picture at `center`, `size` wide and tall in world units, facing
    /// the camera.
    pub fn render_scene(
        &self,
        ctx: &GpuContext,
        items: &[StageItem<'_>],
        view: &ModelView,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, GpuError> {
        self.render_scene_with_floor(ctx, items, view, width, height, None)
    }

    /// `render_scene` on a floor (rung 45): first the shadow map through
    /// the key light, then — when the floor mirrors — the stage from
    /// under the floor into a mirror with a mip chain, then the stage
    /// itself with the floor drawn as a catcher between the bodies.
    pub fn render_scene_with_floor(
        &self,
        ctx: &GpuContext,
        items: &[StageItem<'_>],
        view: &ModelView,
        width: u32,
        height: u32,
        floor: Option<&FloorView>,
    ) -> Result<wgpu::Texture, GpuError> {
        let (width, height) = (width.max(1), height.max(1));
        let device = &ctx.device;
        let aspect = width as f32 / height as f32;
        let plain = frame_uniforms(view, aspect);
        let (right, up, forward) = camera_basis(view);

        // Billboards become one-quad models for this frame.
        let mut billboards: Vec<GpuModel> = Vec::new();
        for item in items {
            if let StageItem::Billboard {
                texture,
                center,
                size,
            } = item
            {
                let (hw, hh) = (size[0] / 2.0, size[1] / 2.0);
                let corner = |sx: f32, sy: f32| -> [f32; 3] {
                    [
                        center[0] + right[0] * sx * hw + up[0] * sy * hh,
                        center[1] + right[1] * sx * hw + up[1] * sy * hh,
                        center[2] + right[2] * sx * hw + up[2] * sy * hh,
                    ]
                };
                let positions = [
                    corner(-1.0, -1.0),
                    corner(1.0, -1.0),
                    corner(1.0, 1.0),
                    corner(-1.0, 1.0),
                ];
                let normal = [-forward[0], -forward[1], -forward[2]];
                let normals = [normal; 4];
                let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
                let indices = [0u32, 1, 2, 0, 2, 3];
                let mut model = self.upload(
                    ctx,
                    &[MeshInput {
                        positions: &positions,
                        normals: &normals,
                        uvs: &uvs,
                        indices: &indices,
                        material: 0,
                        node: 0,
                    }],
                    &[MaterialInput {
                        base_color: [0.0, 0.0, 0.0, 1.0],
                        metallic: 0.0,
                        roughness: 1.0,
                        double_sided: true,
                        texture: None,
                        normal: None,
                        metal_rough: None,
                    }],
                )?;
                self.set_texture(ctx, &mut model, 0, texture, None);
                if let Some(m) = model.materials.get_mut(0) {
                    // The quad IS the picture's box: no letterbox, and the
                    // picture's own alpha decides what is drawn.
                    m.aspect = size[0] / size[1].max(1e-6);
                    m.textured = 3.0;
                    m.double_sided = true;
                    ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
                }
                billboards.push(model);
            }
        }

        // The floor's catcher: a quad on the plane, wide beyond the
        // bodies, with the material mode the shader reads as "the floor".
        let catcher: Option<GpuModel> = match floor {
            Some(f) if !f.footprints.is_empty() => {
                let reach = view.bounds_radius.max(1e-3) * 6.0;
                let c = view.bounds_center;
                let positions = [
                    [c[0] - reach, f.y, c[2] - reach],
                    [c[0] + reach, f.y, c[2] - reach],
                    [c[0] + reach, f.y, c[2] + reach],
                    [c[0] - reach, f.y, c[2] + reach],
                ];
                let normals = [[0.0, 1.0, 0.0]; 4];
                let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
                let indices = [0u32, 2, 1, 0, 3, 2];
                let mut model = self.upload(
                    ctx,
                    &[MeshInput {
                        positions: &positions,
                        normals: &normals,
                        uvs: &uvs,
                        indices: &indices,
                        material: 0,
                        node: 0,
                    }],
                    &[MaterialInput {
                        base_color: [0.0, 0.0, 0.0, 1.0],
                        metallic: 0.0,
                        roughness: 1.0,
                        double_sided: true,
                        texture: None,
                        normal: None,
                        metal_rough: None,
                    }],
                )?;
                if let Some(m) = model.materials.get_mut(0) {
                    m.textured = 5.0;
                    m.double_sided = true;
                    ctx.queue.write_buffer(&m.uniform, 0, as_bytes(&m.raw()));
                }
                Some(model)
            }
            _ => None,
        };

        // The shadow map: every body (not the pictures, not the floor)
        // through the light's camera, depth only.
        let shadow_view: Option<wgpu::TextureView> = match floor {
            Some(f) if catcher.is_some() && f.shadow > 0.0 => {
                let side = 2048u32;
                let map = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("stage-shadow"),
                    size: wgpu::Extent3d {
                        width: side,
                        height: side,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: DEPTH_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let map_view = map.create_view(&Default::default());
                let frame = plain.with_floor(view, f, aspect, false, false);
                let bind = self.frame_bind_group(
                    ctx,
                    "stage-shadow",
                    &frame,
                    view.environment.preset,
                    None,
                    None,
                );
                for item in items {
                    if let StageItem::Model { model, matrices } = item {
                        for mesh in &model.meshes {
                            let m = matrices.get(mesh.node).copied().unwrap_or(IDENTITY);
                            ctx.queue.write_buffer(
                                &mesh.placement,
                                0,
                                as_bytes(&PlacementRaw {
                                    model: m,
                                    normal: normal_matrix(&m),
                                }),
                            );
                        }
                    }
                }
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("stage-shadow"),
                });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("stage-shadow"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &map_view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_pipeline(&self.shadow_pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    for item in items {
                        let StageItem::Model { model, .. } = item else {
                            continue;
                        };
                        for mesh in &model.meshes {
                            let Some(material) = model.materials.get(mesh.material) else {
                                continue;
                            };
                            if material.transmissive() {
                                continue;
                            }
                            pass.set_bind_group(1, &material.bind, &[]);
                            pass.set_bind_group(2, &mesh.placement_bind, &[]);
                            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                            pass.set_index_buffer(
                                mesh.indices.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                        }
                    }
                }
                ctx.queue.submit(Some(encoder.finish()));
                Some(map_view)
            }
            _ => None,
        };

        // The mirror: the stage from under the floor, into a picture with
        // a mip chain so a satin floor reads it blurred.
        let mirror_chain: Option<wgpu::Texture> = match floor {
            Some(f) if catcher.is_some() && f.reflection > 0.0 => {
                let mirrored = mirrored_view(view, f.y);
                let frame = frame_uniforms(&mirrored, aspect).with_floor(
                    view,
                    f,
                    aspect,
                    shadow_view.is_some(),
                    true,
                );
                let bind = self.frame_bind_group(
                    ctx,
                    "stage-mirror",
                    &frame,
                    view.environment.preset,
                    shadow_view.as_ref(),
                    None,
                );
                let picture =
                    self.draw_scene(ctx, items, &billboards, None, &bind, (width, height))?;
                let view_of = picture.create_view(&Default::default());
                Some(self.mipmapped(ctx, &view_of, width, height))
            }
            _ => None,
        };
        let mirror_view = mirror_chain
            .as_ref()
            .map(|t| t.create_view(&Default::default()));

        let frame = match floor {
            Some(f) if catcher.is_some() => {
                plain.with_floor(view, f, aspect, shadow_view.is_some(), false)
            }
            _ => plain,
        };
        let frame_bind = self.frame_bind_group(
            ctx,
            "stage-frame",
            &frame,
            view.environment.preset,
            shadow_view.as_ref(),
            mirror_view.as_ref(),
        );
        self.draw_scene(
            ctx,
            items,
            &billboards,
            catcher.as_ref(),
            &frame_bind,
            (width, height),
        )
    }

    /// One picture of the stage: the models and the billboards (and the
    /// floor's catcher, when there is one) through `frame_bind`, opaque
    /// first, then what light passes through.
    fn draw_scene(
        &self,
        ctx: &GpuContext,
        items: &[StageItem<'_>],
        billboards: &[GpuModel],
        catcher: Option<&GpuModel>,
        frame_bind: &wgpu::BindGroup,
        (width, height): (u32, u32),
    ) -> Result<wgpu::Texture, GpuError> {
        let device = &ctx.device;
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stage-output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stage-msaa"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stage-depth"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        for item in items {
            if let StageItem::Model { model, matrices } = item {
                for mesh in &model.meshes {
                    let m = matrices.get(mesh.node).copied().unwrap_or(IDENTITY);
                    ctx.queue.write_buffer(
                        &mesh.placement,
                        0,
                        as_bytes(&PlacementRaw {
                            model: m,
                            normal: normal_matrix(&m),
                        }),
                    );
                }
            }
        }
        let output_view = output.create_view(&Default::default());
        let msaa_view = msaa.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("stage-pass"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stage-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa_view,
                    resolve_target: Some(&output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, frame_bind, &[]);
            // The floor first, so the bodies overwrite it where they stand
            // and a pane of glass blends over it.
            if let Some(model) = catcher {
                for mesh in &model.meshes {
                    let Some(material) = model.materials.get(mesh.material) else {
                        continue;
                    };
                    pass.set_bind_group(1, &material.bind, &[]);
                    pass.set_bind_group(2, &mesh.placement_bind, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
            }
            // The opaque meshes of every member first, then the ones light
            // passes through, so what stands behind a pane is there when
            // the pane blends over it.
            for through in [false, true] {
                let mut next_billboard = billboards.iter();
                for item in items {
                    let model: &GpuModel = match item {
                        StageItem::Model { model, .. } => model,
                        StageItem::Billboard { .. } => match next_billboard.next() {
                            Some(b) => b,
                            None => continue,
                        },
                    };
                    for mesh in &model.meshes {
                        let Some(material) = model.materials.get(mesh.material) else {
                            continue;
                        };
                        if material.transmissive() != through {
                            continue;
                        }
                        pass.set_bind_group(1, &material.bind, &[]);
                        pass.set_bind_group(2, &mesh.placement_bind, &[]);
                        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    }
                }
            }
        }
        ctx.queue.submit(Some(encoder.finish()));
        Ok(output)
    }

    /// Render the model at `width × height` and hand the texture over.
    /// `matrices` places each mesh by its node (one per node; a mesh whose
    /// node is out of range draws at the identity).
    pub fn render(
        &self,
        ctx: &GpuContext,
        model: &GpuModel,
        view: &ModelView,
        matrices: &[Mat4],
        width: u32,
        height: u32,
    ) -> Result<InputTexture, GpuError> {
        let texture = self.render_to_texture(ctx, model, view, matrices, width, height)?;
        Ok(crate::compositor::Compositor::adopt_owned_texture(texture))
    }

    /// Render the model at `width × height` into a texture the caller
    /// owns — what an engine wraps as a cached frame.
    pub fn render_to_texture(
        &self,
        ctx: &GpuContext,
        model: &GpuModel,
        view: &ModelView,
        matrices: &[Mat4],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, GpuError> {
        let (width, height) = (width.max(1), height.max(1));
        let device = &ctx.device;
        for mesh in &model.meshes {
            let m = matrices.get(mesh.node).copied().unwrap_or(IDENTITY);
            ctx.queue.write_buffer(
                &mesh.placement,
                0,
                as_bytes(&PlacementRaw {
                    model: m,
                    normal: normal_matrix(&m),
                }),
            );
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model-output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // (COPY_SRC above: the engine cuts the model's box out of it.)
        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model-msaa"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model-depth"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let frame = frame_uniforms(view, width as f32 / height as f32);
        let frame_bind = self.frame_bind_group(
            ctx,
            "model-frame",
            &frame,
            view.environment.preset,
            None,
            None,
        );
        let output_view = output.create_view(&Default::default());
        let msaa_view = msaa.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("model-pass"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa_view,
                    resolve_target: Some(&output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &frame_bind, &[]);
            // Opaque first, then what light passes through.
            for through in [false, true] {
                for mesh in &model.meshes {
                    let Some(material) = model.materials.get(mesh.material) else {
                        continue;
                    };
                    if material.transmissive() != through {
                        continue;
                    }
                    pass.set_bind_group(1, &material.bind, &[]);
                    pass.set_bind_group(2, &mesh.placement_bind, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
            }
        }
        ctx.queue.submit(Some(encoder.finish()));
        Ok(output)
    }

    /// The rendered pixels, BGRA premultiplied, for tests and probes.
    pub fn render_to_bytes(
        &self,
        ctx: &GpuContext,
        model: &GpuModel,
        view: &ModelView,
        matrices: &[Mat4],
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, GpuError> {
        let texture = self.render_to_texture(ctx, model, view, matrices, width, height)?;
        read_texture(ctx, &texture, width, height)
    }
}

impl ModelPass {
    /// Group 0 for one frame: the uniform, the environment, and the
    /// shadow map and mirror (their one-texel stand-ins when absent).
    fn frame_bind_group(
        &self,
        ctx: &GpuContext,
        label: &str,
        frame: &FrameRaw,
        preset: EnvPreset,
        shadow: Option<&wgpu::TextureView>,
        mirror: Option<&wgpu::TextureView>,
    ) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let frame_buffer = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: as_bytes(frame),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(self.env_view(preset)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.env_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        shadow.unwrap_or(&self.dummy_shadow),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(
                        mirror.unwrap_or(&self.dummy_mirror),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.mirror_sampler),
                },
            ],
        })
    }

    /// The bound environment for a preset; `None` binds the studio, which
    /// the shader ignores when `env_params.x` is 0.
    fn env_view(&self, preset: EnvPreset) -> &wgpu::TextureView {
        match preset {
            EnvPreset::None | EnvPreset::Studio => &self.envs[0],
            EnvPreset::Sunset => &self.envs[1],
            EnvPreset::Night => &self.envs[2],
        }
    }
}

/// Equirectangular size of a generated environment and its mip count
/// (256 × 128 down to 1 × 1). Level i is the sphere seen through a GGX
/// lobe of roughness i / (ENV_MIPS − 1), so a reflection read at
/// `roughness * (ENV_MIPS − 1)` is the prefiltered radiance the
/// split-sum wants; the bottom texel is the mean.
const ENV_WIDTH: u32 = 256;
const ENV_HEIGHT: u32 = 128;
const ENV_MIPS: u32 = 8;

/// One preset as linear RGB radiance over the sphere: `yaw` from -π to π
/// about the vertical, `el` the elevation, -π/2 at the floor.
fn env_radiance(preset: EnvPreset, yaw: f32, el: f32) -> [f32; 3] {
    let gauss = |dy: f32, de: f32, sy: f32, se: f32| -> f32 {
        let dy = (dy + std::f32::consts::PI).rem_euclid(2.0 * std::f32::consts::PI)
            - std::f32::consts::PI;
        (-(dy * dy) / (2.0 * sy * sy) - (de * de) / (2.0 * se * se)).exp()
    };
    let t = (el / std::f32::consts::FRAC_PI_2).clamp(-1.0, 1.0);
    let mix3 = |a: [f32; 3], b: [f32; 3], k: f32| {
        [
            a[0] + (b[0] - a[0]) * k,
            a[1] + (b[1] - a[1]) * k,
            a[2] + (b[2] - a[2]) * k,
        ]
    };
    let add =
        |a: [f32; 3], b: [f32; 3], k: f32| [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k];
    match preset {
        EnvPreset::None | EnvPreset::Studio => {
            let base = if t >= 0.0 {
                mix3([0.42, 0.42, 0.44], [0.9, 0.9, 0.95], t.powf(0.8))
            } else {
                mix3([0.42, 0.42, 0.44], [0.12, 0.12, 0.13], (-t).powf(0.7))
            };
            let band = (-(el / 0.12) * (el / 0.12)).exp() * 0.08;
            let key_box = gauss(yaw - 0.3, el - 0.8, 0.9, 0.35) * 2.6;
            let fill_box = gauss(yaw - 2.6, el - 0.35, 0.7, 0.3) * 1.0;
            add(
                add(add(base, [1.0, 1.0, 1.0], band), [1.0, 1.0, 1.0], key_box),
                [1.0, 1.0, 1.0],
                fill_box,
            )
        }
        EnvPreset::Sunset => {
            let base = if t >= 0.0 {
                mix3([1.0, 0.55, 0.3], [0.2, 0.3, 0.55], t.powf(0.6))
            } else {
                mix3([0.6, 0.35, 0.22], [0.16, 0.12, 0.1], (-t).powf(0.6))
            };
            let sun = gauss(yaw - 0.5, el - 0.14, 0.05, 0.05);
            let glow = gauss(yaw - 0.5, el - 0.14, 0.5, 0.35);
            add(add(base, [8.0, 5.0, 2.5], sun), [1.2, 0.7, 0.35], glow)
        }
        EnvPreset::Night => {
            let base = if t >= 0.0 {
                mix3([0.06, 0.08, 0.14], [0.02, 0.03, 0.07], t.powf(0.7))
            } else {
                mix3([0.06, 0.08, 0.14], [0.02, 0.02, 0.03], (-t).powf(0.7))
            };
            let moon = gauss(yaw + 1.0, el - 1.0, 0.08, 0.08);
            let halo = gauss(yaw + 1.0, el - 1.0, 0.5, 0.4);
            let city = (gauss(yaw - 0.9, el, 0.15, 0.04)
                + gauss(yaw - 1.7, el, 0.12, 0.04)
                + gauss(yaw + 2.2, el, 0.2, 0.04))
                * 0.8;
            add(
                add(add(base, [2.0, 2.2, 2.8], moon), [0.25, 0.27, 0.35], halo),
                [1.0, 0.62, 0.3],
                city,
            )
        }
    }
}

/// IEEE half from a float — what an Rgba16Float texel takes.
fn f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let mant = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00;
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let half = sign | ((e as u16) << 10) | ((mant >> 13) as u16);
    if mant & 0x1000 != 0 {
        half + 1
    } else {
        half
    }
}

/// One level of a prefiltered environment: every texel's direction seen
/// through a GGX lobe of `roughness` (importance-sampled, the view taken
/// along the normal as the split-sum assumes), read from the full-size
/// level `base` bilinearly. Roughness 0 is the base itself.
fn prefilter_level(
    base: &[[f32; 4]],
    bw: usize,
    bh: usize,
    nw: usize,
    nh: usize,
    roughness: f32,
) -> Vec<[f32; 4]> {
    use std::f32::consts::PI;
    let alpha = (roughness * roughness).max(1e-3);
    const K: u32 = 64;
    let lookup = |d: [f32; 3]| -> [f32; 3] {
        let yaw = d[0].atan2(d[2]);
        let el = d[1].clamp(-1.0, 1.0).asin();
        let fx = (yaw / (2.0 * PI) + 0.5) * bw as f32 - 0.5;
        let fy = (0.5 - el / PI) * bh as f32 - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |x: i64, y: i64| -> [f32; 4] {
            let xi = x.rem_euclid(bw as i64) as usize;
            let yi = y.clamp(0, bh as i64 - 1) as usize;
            base[yi * bw + xi]
        };
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (
            at(x0, y0),
            at(x0 + 1, y0),
            at(x0, y0 + 1),
            at(x0 + 1, y0 + 1),
        );
        let mut out = [0.0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            *o = top + (bottom - top) * ty;
        }
        out
    };
    let mut out = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        let el = PI / 2.0 - (y as f32 + 0.5) / nh as f32 * PI;
        for x in 0..nw {
            let yaw = (x as f32 + 0.5) / nw as f32 * 2.0 * PI - PI;
            let n = [el.cos() * yaw.sin(), el.sin(), el.cos() * yaw.cos()];
            if roughness <= 0.0 {
                let c = lookup(n);
                out.push([c[0], c[1], c[2], 1.0]);
                continue;
            }
            let up = if n[1].abs() < 0.999 {
                [0.0, 1.0, 0.0]
            } else {
                [1.0, 0.0, 0.0]
            };
            let cross = |a: [f32; 3], b: [f32; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            let norm = |v: [f32; 3]| {
                let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
                [v[0] / l, v[1] / l, v[2] / l]
            };
            let t = norm(cross(up, n));
            let b = cross(n, t);
            let mut acc = [0.0f32; 3];
            let mut weight = 0.0f32;
            for i in 0..K {
                // Hammersley: stratified over the lobe.
                let u1 = (i as f32 + 0.5) / K as f32;
                let u2 = (i.reverse_bits() as f32) / 4_294_967_296.0;
                let phi = 2.0 * PI * u1;
                let cos_theta = ((1.0 - u2) / (1.0 + (alpha * alpha - 1.0) * u2)).sqrt();
                let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
                let hl = [sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta];
                let hw = [
                    t[0] * hl[0] + b[0] * hl[1] + n[0] * hl[2],
                    t[1] * hl[0] + b[1] * hl[1] + n[1] * hl[2],
                    t[2] * hl[0] + b[2] * hl[1] + n[2] * hl[2],
                ];
                let ndh = n[0] * hw[0] + n[1] * hw[1] + n[2] * hw[2];
                let l = [
                    2.0 * ndh * hw[0] - n[0],
                    2.0 * ndh * hw[1] - n[1],
                    2.0 * ndh * hw[2] - n[2],
                ];
                let ndl = n[0] * l[0] + n[1] * l[1] + n[2] * l[2];
                if ndl <= 0.0 {
                    continue;
                }
                let c = lookup(l);
                for k in 0..3 {
                    acc[k] += c[k] * ndl;
                }
                weight += ndl;
            }
            let w = weight.max(1e-6);
            out.push([acc[0] / w, acc[1] / w, acc[2] / w, 1.0]);
        }
    }
    out
}

/// A preset as an Rgba16Float equirectangular texture with a full mip
/// chain, each level prefiltered for the roughness it stands for, the
/// last a single texel of the sphere's mean radiance.
fn upload_env(ctx: &GpuContext, preset: EnvPreset) -> wgpu::Texture {
    let (w, h) = (ENV_WIDTH as usize, ENV_HEIGHT as usize);
    let mut level: Vec<[f32; 4]> = Vec::with_capacity(w * h);
    for y in 0..h {
        let el = std::f32::consts::FRAC_PI_2 - (y as f32 + 0.5) / h as f32 * std::f32::consts::PI;
        for x in 0..w {
            let yaw =
                (x as f32 + 0.5) / w as f32 * 2.0 * std::f32::consts::PI - std::f32::consts::PI;
            let c = env_radiance(preset, yaw, el);
            level.push([c[0], c[1], c[2], 1.0]);
        }
    }
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model-environment"),
        size: wgpu::Extent3d {
            width: ENV_WIDTH,
            height: ENV_HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: ENV_MIPS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let base = level.clone();
    let (mut lw, mut lh) = (w, h);
    for mip in 0..ENV_MIPS {
        let bytes: Vec<u8> = level
            .iter()
            .flat_map(|p| p.iter().flat_map(|c| f16(*c).to_le_bytes()))
            .collect();
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some((lw * 8) as u32),
                rows_per_image: Some(lh as u32),
            },
            wgpu::Extent3d {
                width: lw as u32,
                height: lh as u32,
                depth_or_array_layers: 1,
            },
        );
        if mip + 1 == ENV_MIPS {
            break;
        }
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        let roughness = (mip + 1) as f32 / (ENV_MIPS - 1) as f32;
        level = prefilter_level(&base, w, h, nw, nh, roughness);
        lw = nw;
        lh = nh;
    }
    texture
}

fn upload_rgba(ctx: &GpuContext, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model-texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
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
        rgba,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    texture
}

fn read_texture(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, GpuError> {
    let padded = (width * 4).div_ceil(256) * 256;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("model-readback"),
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
        .map_err(|_| GpuError::Import("model readback: no reply".into()))?
        .map_err(|e| GpuError::Import(format!("model readback: {e:?}")))?;
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

// ---------------------------------------------------------------------------
// Camera and light: the plan's orbit — yaw about Y, pitch about the
// camera's right axis, roll about the view axis — at `distance` radii from
// the bounds centre, a vertical field of view, wgpu's 0…1 depth.

fn deg(d: f64) -> f32 {
    d.to_radians() as f32
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt().max(1e-9);
    [v[0] / l, v[1] / l, v[2] / l]
}

/// The camera's right, up and forward axes in world space — what a
/// billboard aligns to.
pub fn camera_basis(view: &ModelView) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let (eye, center) = eye_and_center(view);
    let mut forward = norm(sub(center, eye));
    if dot(forward, forward) < 1e-6 {
        forward = [0.0, 0.0, -1.0];
    }
    let mut right = norm(cross(forward, [0.0, 1.0, 0.0]));
    if dot(right, right) < 1e-6 {
        right = [1.0, 0.0, 0.0];
    }
    let mut up = cross(right, forward);
    let r = deg(view.roll);
    if r.abs() > 1e-6 {
        let (c, s) = (r.cos(), r.sin());
        let new_right = [
            right[0] * c + up[0] * s,
            right[1] * c + up[1] * s,
            right[2] * c + up[2] * s,
        ];
        up = [
            up[0] * c - right[0] * s,
            up[1] * c - right[1] * s,
            up[2] * c - right[2] * s,
        ];
        right = new_right;
    }
    (right, up, forward)
}

/// A direction from yaw (about +Y, 0 = toward +Z, the viewer's side) and
/// pitch (up from the ground plane), degrees.
fn direction(yaw: f64, pitch: f64) -> [f32; 3] {
    let (y, p) = (deg(yaw), deg(pitch));
    [p.cos() * y.sin(), p.sin(), p.cos() * y.cos()]
}

/// Where a world-space point lands in the output, as a fraction of its
/// width and height (0…1, y down) — the same camera the pass draws with,
/// so a caller can find the model's box on the picture. `None` behind the
/// camera.
pub fn project_point(view: &ModelView, aspect: f32, p: [f32; 3]) -> Option<[f32; 2]> {
    let frame = frame_uniforms(view, aspect);
    let m = frame.view_proj;
    let x = m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0];
    let y = m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1];
    let w = m[0][3] * p[0] + m[1][3] * p[1] + m[2][3] * p[2] + m[3][3];
    if w <= 1e-6 {
        return None;
    }
    Some([(x / w + 1.0) / 2.0, (1.0 - y / w) / 2.0])
}

/// A `width × height` cut of `source` from `(x, y)`, as its own texture
/// in the pass's output format — the model's box out of the square it
/// was drawn on.
pub fn crop_texture(
    ctx: &GpuContext,
    source: &wgpu::Texture,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: width.max(1),
        height: height.max(1),
        depth_or_array_layers: 1,
    };
    let out = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model-crop"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OUTPUT_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("model-crop"),
        });
    encoder.copy_texture_to_texture(
        wgpu::ImageCopyTexture {
            texture: source,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyTexture {
            texture: &out,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        size,
    );
    ctx.queue.submit(Some(encoder.finish()));
    out
}

fn frame_uniforms(view: &ModelView, aspect: f32) -> FrameRaw {
    let radius = view.bounds_radius.max(1e-6);
    let (eye, center) = eye_and_center(view);
    // Depth range along the VIEW AXIS: how far the bounds centre lies in
    // front of the eye, which is what near and far must bracket. Measuring
    // the straight-line distance instead slices an off-axis body open once
    // the gaze points away from the centre.
    let to_center = sub(view.bounds_center, eye);
    let mut forward_probe = norm(sub(view.target.unwrap_or(view.bounds_center), eye));
    if dot(forward_probe, forward_probe) < 1e-6 {
        forward_probe = [0.0, 0.0, -1.0];
    }
    // How far in front of the eye the bounds centre lies. No floor tied to
    // the ORBIT's distance: a flown eye may sit much closer or further, and
    // borrowing the orbit's number there pushed the near plane through the
    // bodies.
    let distance = dot(to_center, forward_probe);
    let mut forward = norm(sub(center, eye));
    if dot(forward, forward) < 1e-6 {
        forward = [0.0, 0.0, -1.0];
    }
    let world_up = [0.0, 1.0, 0.0];
    let mut right = norm(cross(forward, world_up));
    if dot(right, right) < 1e-6 {
        right = [1.0, 0.0, 0.0];
    }
    let mut up = cross(right, forward);
    // Roll about the view axis.
    let r = deg(view.roll);
    if r.abs() > 1e-6 {
        let (c, s) = (r.cos(), r.sin());
        let new_right = [
            right[0] * c + up[0] * s,
            right[1] * c + up[1] * s,
            right[2] * c + up[2] * s,
        ];
        up = [
            up[0] * c - right[0] * s,
            up[1] * c - right[1] * s,
            up[2] * c - right[2] * s,
        ];
        right = new_right;
    }
    // View matrix (column-major): rows are right, up, -forward.
    let view_m = [
        [right[0], up[0], -forward[0], 0.0],
        [right[1], up[1], -forward[1], 0.0],
        [right[2], up[2], -forward[2], 0.0],
        [-dot(right, eye), -dot(up, eye), dot(forward, eye), 1.0],
    ];
    let fov = deg(view.fov.clamp(5.0, 120.0));
    // Bracket the bounds sphere along the view axis, and keep the near
    // plane positive however close the eye comes.
    let near = (distance - radius * 1.5).max(radius * 0.01).max(1e-4);
    let far = (distance + radius * 3.0).max(near * 1.001);
    let f = 1.0 / (fov / 2.0).tan();
    // Perspective, right-handed, depth 0…1 (wgpu).
    let proj = [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ];
    let view_proj = mul(&proj, &view_m);
    let light = direction(view.light_yaw, view.light_pitch);
    FrameRaw {
        light_view_proj: IDENTITY,
        mirror_view_proj: IDENTITY,
        floor: [0.0; 4],
        floor2: [0.0; 4],
        footprints: [[0.0; 4]; 8],
        lifts: [[0.0; 4]; 8],
        counts: [0.0; 4],
        view_proj,
        camera_pos: [eye[0], eye[1], eye[2], 1.0],
        light_dir: [
            light[0],
            light[1],
            light[2],
            view.light_intensity.max(0.0) as f32,
        ],
        key_rgb: [view.key_rgb[0], view.key_rgb[1], view.key_rgb[2], 1.0],
        ambient_rgb: [
            view.ambient_rgb[0],
            view.ambient_rgb[1],
            view.ambient_rgb[2],
            1.0,
        ],
        rim_rgb: [view.rim_rgb[0], view.rim_rgb[1], view.rim_rgb[2], 1.0],
        env_params: [
            if view.environment.preset == EnvPreset::None {
                0.0
            } else {
                1.0
            },
            view.environment.intensity.max(0.0),
            deg(view.environment.rotation_deg as f64),
            (ENV_MIPS - 1) as f32,
        ],
    }
}

/// The inverse transpose of the upper 3×3, as a 4×4 — what normals go
/// through under a non-uniform scale. Falls back to the matrix itself
/// when it is singular.
fn normal_matrix(m: &Mat4) -> Mat4 {
    let a = [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]; // row-major 3×3
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        return *m;
    }
    let inv = |r: usize, c: usize| -> f32 {
        // cofactor of (c, r) over det gives inverse(r, c); the transpose of
        // the inverse is then inverse(c, r) — so take cofactor(r, c) / det.
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        (a[r1][c1] * a[r2][c2] - a[r1][c2] * a[r2][c1]) / det
    };
    // Column-major output: out[col][row] = inverse-transpose(row, col) = cofactor(row, col)/det.
    let mut out = IDENTITY;
    for (col, column) in out.iter_mut().enumerate().take(3) {
        for (row, cell) in column.iter_mut().enumerate().take(3) {
            *cell = inv(row, col);
        }
    }
    out
}

fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type CubeData = (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>);

    /// A unit cube, four vertices a face with flat normals, one material.
    fn cube() -> CubeData {
        let h = 0.5f32;
        let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
            (
                [0.0, 0.0, 1.0],
                [[-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]],
            ),
            (
                [0.0, 0.0, -1.0],
                [[h, -h, -h], [-h, -h, -h], [-h, h, -h], [h, h, -h]],
            ),
            (
                [1.0, 0.0, 0.0],
                [[h, -h, h], [h, -h, -h], [h, h, -h], [h, h, h]],
            ),
            (
                [-1.0, 0.0, 0.0],
                [[-h, -h, -h], [-h, -h, h], [-h, h, h], [-h, h, -h]],
            ),
            (
                [0.0, 1.0, 0.0],
                [[-h, h, h], [h, h, h], [h, h, -h], [-h, h, -h]],
            ),
            (
                [0.0, -1.0, 0.0],
                [[-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]],
            ),
        ];
        let (mut p, mut n, mut uv, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (i, (normal, corners)) in faces.iter().enumerate() {
            let base = (i * 4) as u32;
            for (k, c) in corners.iter().enumerate() {
                p.push(*c);
                n.push(*normal);
                uv.push([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]][k]);
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        (p, n, uv, idx)
    }

    fn render(view: ModelView) -> Vec<u8> {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("model");
        pass.render_to_bytes(&ctx, &model, &view, &[IDENTITY], 96, 96)
            .expect("render")
    }

    /// Luminance of every opaque pixel, and how many pixels are opaque.
    fn plateaus(px: &[u8]) -> (Vec<u8>, usize) {
        let mut lum = Vec::new();
        for p in px.chunks_exact(4) {
            if p[3] > 250 {
                lum.push(
                    ((p[2] as u32 * 299 + p[1] as u32 * 587 + p[0] as u32 * 114) / 1000) as u8,
                );
            }
        }
        let n = lum.len();
        (lum, n)
    }

    /// Renders the test cube face on under one material, 96 px square,
    /// and returns the mean brightness of two patches on the front face:
    /// the left half (uv.x < 0.5) and the right half.
    fn halves(material: MaterialInput<'_>, light_yaw: f64, light_pitch: f64) -> (u32, u32) {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[material],
            )
            .expect("upload");
        let view = ModelView {
            yaw: 0.0,
            pitch: 0.0,
            distance: 3.0,
            light_yaw,
            light_pitch,
            ..ModelView::default()
        };
        let px = pass
            .render_to_bytes(&ctx, &model, &view, &[IDENTITY], 96, 96)
            .expect("render");
        let mean = |x0: usize, x1: usize| -> u32 {
            let mut sum = 0u32;
            let mut count = 0u32;
            for y in 40..56 {
                for x in x0..x1 {
                    let i = (y * 96 + x) * 4;
                    sum += px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32;
                    count += 3;
                }
            }
            sum / count
        };
        // The face spans roughly x 24..72 at distance 3; uv.x runs left
        // to right across it.
        (mean(28, 44), mean(52, 68))
    }

    /// One grey cube under the studio, shot four ways by finish word:
    /// chrome (a mirror of the light boxes), brushed (the same metal
    /// with a grain), matte plastic, and thin glass. Each pair reads
    /// differently, and only the glass lets the background through.
    fn finished_cube(finish: SurfaceFinish, metallic: f32, roughness: f32) -> Vec<u8> {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let mut model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.7, 0.7, 0.7, 1.0],
                    metallic,
                    roughness,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("upload");
        pass.set_finish(&ctx, &mut model, 0, finish);
        let view = ModelView {
            yaw: 25.0,
            pitch: 20.0,
            distance: 3.0,
            light_yaw: 40.0,
            light_pitch: 30.0,
            environment: EnvironmentView {
                preset: EnvPreset::Studio,
                intensity: 1.0,
                rotation_deg: 0.0,
            },
            ..ModelView::default()
        };
        pass.render_to_bytes(&ctx, &model, &view, &[IDENTITY], 96, 96)
            .expect("render")
    }

    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(x, y)| (0..3).any(|k| (x[k] as i32 - y[k] as i32).abs() > 10))
            .count()
    }

    #[test]
    fn a_finish_word_shades_its_own_way() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let plain = SurfaceFinish::default();
        let chrome = finished_cube(plain, 1.0, 0.06);
        let brushed = finished_cube(
            SurfaceFinish {
                anisotropy: 0.8,
                ..plain
            },
            1.0,
            0.35,
        );
        let anodized = finished_cube(plain, 1.0, 0.35);
        let matte = finished_cube(plain, 0.0, 0.85);
        let lacquer = finished_cube(
            SurfaceFinish {
                clearcoat: 1.0,
                clearcoat_roughness: 0.04,
                ..plain
            },
            0.3,
            0.5,
        );
        let uncoated = finished_cube(plain, 0.3, 0.5);
        let glass = finished_cube(
            SurfaceFinish {
                transmission: 0.92,
                ..plain
            },
            0.0,
            0.05,
        );
        let body = 96 * 96 / 6;
        assert!(
            differing(&chrome, &matte) > body,
            "chrome vs matte: {}",
            differing(&chrome, &matte)
        );
        assert!(
            differing(&brushed, &anodized) > body / 4,
            "the grain changes the highlight: {}",
            differing(&brushed, &anodized)
        );
        assert!(
            differing(&lacquer, &uncoated) > body / 4,
            "the coat adds its own light: {}",
            differing(&lacquer, &uncoated)
        );
        // Alpha at the cube's centre: opaque for matte, mostly open for
        // glass, and the glass never fully vanishes (it still reflects).
        let centre = |px: &[u8]| px[(48 * 96 + 48) * 4 + 3];
        assert_eq!(centre(&matte), 255);
        let g = centre(&glass);
        assert!(g < 100, "glass lets the background through: alpha {g}");
        assert!(g > 0, "glass still stands there");
    }

    /// Glass over a screen: the same bound picture reads brighter where
    /// the coat catches the studio, and the picture itself is still a
    /// display — the same pixels away from the highlight.
    #[test]
    fn a_coat_on_a_screen_adds_the_worlds_highlight() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let picture = upload_rgba(&ctx, 4, 4, &[[40u8, 40, 40, 255]; 16].concat());
        let render = |coat: f32| -> Vec<u8> {
            let mut model = pass
                .upload(
                    &ctx,
                    &[MeshInput {
                        positions: &p,
                        normals: &n,
                        uvs: &uv,
                        indices: &idx,
                        material: 0,
                        node: 0,
                    }],
                    &[MaterialInput {
                        base_color: [0.1, 0.1, 0.1, 1.0],
                        metallic: 0.0,
                        roughness: 0.5,
                        double_sided: false,
                        texture: None,
                        normal: None,
                        metal_rough: None,
                    }],
                )
                .expect("upload");
            pass.set_texture(
                &ctx,
                &mut model,
                0,
                &picture.create_view(&Default::default()),
                None,
            );
            pass.set_finish(
                &ctx,
                &mut model,
                0,
                SurfaceFinish {
                    clearcoat: coat,
                    clearcoat_roughness: 0.05,
                    ..SurfaceFinish::default()
                },
            );
            let view = ModelView {
                yaw: 25.0,
                pitch: 20.0,
                distance: 3.0,
                environment: EnvironmentView {
                    preset: EnvPreset::Studio,
                    intensity: 1.0,
                    rotation_deg: 0.0,
                },
                ..ModelView::default()
            };
            pass.render_to_bytes(&ctx, &model, &view, &[IDENTITY], 96, 96)
                .expect("render")
        };
        let bare = render(0.0);
        let glass = render(1.0);
        let sum = |px: &[u8]| -> u64 {
            px.chunks_exact(4)
                .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
                .sum()
        };
        assert!(
            sum(&glass) > sum(&bare) + 96 * 96 / 6 * 3 * 4,
            "the coat brightens the screen with the world: {} vs {}",
            sum(&glass),
            sum(&bare)
        );
        // A lift, never a replacement: glass over a dark screen reflects
        // the room everywhere, but no pixel goes darker and the picture
        // shows through — the alpha, the display's own, is untouched.
        assert!(
            bare.iter()
                .zip(&glass)
                .all(|(b, g)| *g as i32 >= *b as i32 - 2),
            "the coat only adds light"
        );
        assert!(
            bare.chunks_exact(4)
                .zip(glass.chunks_exact(4))
                .all(|(b, g)| b[3] == g[3]),
            "a screen's alpha is the display's own"
        );
    }

    /// The environment's levels get smoother as they get rougher: each
    /// holds less contrast than the one above, and the sunset's sun is
    /// a bright point at the top and a glow further down.
    #[test]
    fn the_environment_levels_are_prefiltered_by_roughness() {
        let (w, h) = (ENV_WIDTH as usize, ENV_HEIGHT as usize);
        let mut base = Vec::with_capacity(w * h);
        for y in 0..h {
            let el =
                std::f32::consts::FRAC_PI_2 - (y as f32 + 0.5) / h as f32 * std::f32::consts::PI;
            for x in 0..w {
                let yaw =
                    (x as f32 + 0.5) / w as f32 * 2.0 * std::f32::consts::PI - std::f32::consts::PI;
                let c = env_radiance(EnvPreset::Sunset, yaw, el);
                base.push([c[0], c[1], c[2], 1.0]);
            }
        }
        let peak = |level: &[[f32; 4]]| level.iter().map(|p| p[0]).fold(0.0f32, f32::max);
        let mean =
            |level: &[[f32; 4]]| level.iter().map(|p| p[0]).sum::<f32>() / level.len() as f32;
        let sharp = prefilter_level(&base, w, h, w, h, 0.0);
        assert!(
            (peak(&sharp) - peak(&base)).abs() < 1e-3,
            "roughness 0 is the base itself"
        );
        let mut last_peak = peak(&base);
        for (i, roughness) in [0.2f32, 0.45, 0.7, 1.0].iter().enumerate() {
            let level = prefilter_level(&base, w, h, w >> (i + 1), h >> (i + 1), *roughness);
            let p = peak(&level);
            assert!(
                p < last_peak,
                "roughness {roughness}: peak {p} under {last_peak}"
            );
            assert!(
                (mean(&level) - mean(&base)).abs() < mean(&base) * 0.5,
                "the light is spread, not lost: {} vs {}",
                mean(&level),
                mean(&base)
            );
            last_peak = p;
        }
    }

    /// A cube on a floor (rung 45): with `none` the floor region is
    /// empty; `matte` darkens where the key light's shadow falls and
    /// leaves the mirror empty; `mirror` shows the cube's image below it.
    /// The cube itself still reads the same in all three.
    #[test]
    fn a_floor_catches_the_shadow_and_the_mirror() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.85, 0.3, 0.2, 1.0],
                    metallic: 0.0,
                    roughness: 0.5,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("upload");
        let view = ModelView {
            yaw: 25.0,
            pitch: 22.0,
            distance: 4.0,
            bounds_radius: 1.4,
            // The key from behind the cube, so its shadow falls toward the
            // camera where the floor is in view.
            light_yaw: 205.0,
            light_pitch: 45.0,
            environment: EnvironmentView {
                preset: EnvPreset::Studio,
                intensity: 1.0,
                rotation_deg: 0.0,
            },
            ..ModelView::default()
        };
        let side = 160u32;
        let floor = |shadow: f32, reflection: f32, blur: f32| FloorView {
            y: -0.5,
            shadow,
            softness: 4.0,
            reflection,
            blur,
            footprints: vec![Footprint {
                center_xz: [0.0, 0.0],
                half_xz: [0.5, 0.5],
                bottom: 0.0,
                top: 1.0,
            }],
        };
        let render = |floor: Option<&FloorView>| -> Vec<u8> {
            let items = [StageItem::Model {
                model: &model,
                matrices: &[IDENTITY],
            }];
            let texture = pass
                .render_scene_with_floor(&ctx, &items, &view, side, side, floor)
                .expect("render");
            read_texture(&ctx, &texture, side, side).expect("read")
        };
        let none = render(None);
        let matte = render(Some(&floor(0.55, 0.0, 0.0)));
        let mirror = render(Some(&floor(0.35, 0.9, 0.0)));
        let at = |px: &[u8], world: [f32; 3]| -> [u8; 4] {
            let q = project_point(&view, 1.0, world).expect("in front");
            let x = ((q[0] * side as f32) as usize).min(side as usize - 1);
            let y = ((q[1] * side as f32) as usize).min(side as usize - 1);
            let i = (y * side as usize + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // Where the top's shadow lands on the floor along the light.
        let l = direction(view.light_yaw, view.light_pitch);
        let top = [0.0f32, 0.5, 0.0];
        let t = (top[1] - -0.5) / l[1];
        let shadow_spot = [top[0] - l[0] * t, -0.5, top[2] - l[2] * t];
        // The floor point where the mirror of the cube's top shows: the
        // same point the mirrored camera sees the real top through.
        let mirrored = mirrored_view(&view, -0.5);
        let (eye, _) = eye_and_center(&mirrored);
        let s = (eye[1] - -0.5) / (eye[1] - top[1]);
        let mirror_spot = [
            eye[0] + (top[0] - eye[0]) * s,
            -0.5,
            eye[2] + (top[2] - eye[2]) * s,
        ];
        assert_eq!(
            at(&none, shadow_spot)[3],
            0,
            "no floor, nothing under the cube"
        );
        assert_eq!(at(&none, mirror_spot)[3], 0);
        let shadow_px = at(&matte, shadow_spot);
        assert!(
            shadow_px[3] > 40 && shadow_px[0] < 40 && shadow_px[2] < 40,
            "matte darkens where the shadow falls: {shadow_px:?}"
        );
        // The shadow may fall across the mirror spot too, so what tells a
        // matte floor from a mirror is COLOUR: darkening has none, the
        // cube's image is red (BGRA: red is index 2).
        let matte_px = at(&matte, mirror_spot);
        assert!(
            matte_px[0] < 30 && matte_px[2] < 30,
            "matte shows no mirror, only darkening: {matte_px:?}"
        );
        let mirror_px = at(&mirror, mirror_spot);
        assert!(
            mirror_px[3] > 40 && mirror_px[2] > 40 && mirror_px[2] > mirror_px[0] + 20,
            "the mirror shows the red cube's image: {mirror_px:?}"
        );
        // The cube's own top face reads the same with or without a floor.
        let face = at(&none, [0.0, 0.5, 0.0]);
        let face_floored = at(&mirror, [0.0, 0.5, 0.0]);
        for k in 0..3 {
            assert!(
                (face[k] as i32 - face_floored[k] as i32).abs() < 24,
                "the body is the body: {face:?} vs {face_floored:?}"
            );
        }
    }

    /// A dissolving body loses its surface in cells as the amount rises:
    /// none at 0, about half at 0.5, all at 1.
    #[test]
    fn a_dissolve_takes_the_surface_away_in_cells() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let mut model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.9, 0.9, 0.9, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("upload");
        let opaque = |model: &GpuModel| -> usize {
            let view = ModelView {
                yaw: 0.0,
                pitch: 0.0,
                distance: 3.0,
                ..ModelView::default()
            };
            let px = pass
                .render_to_bytes(&ctx, model, &view, &[IDENTITY], 96, 96)
                .expect("render");
            px.chunks_exact(4).filter(|c| c[3] > 8).count()
        };
        let whole = opaque(&model);
        assert!(whole > 1000, "the cube is drawn: {whole}");
        pass.set_dissolve(&ctx, &mut model, 0.5, 0.05);
        let half = opaque(&model);
        assert!(
            half > whole / 4 && half < whole * 3 / 4,
            "half dissolved: {half} of {whole}"
        );
        pass.set_dissolve(&ctx, &mut model, 1.0, 0.05);
        assert_eq!(opaque(&model), 0, "all gone");
        pass.set_dissolve(&ctx, &mut model, 0.0, 0.05);
        assert_eq!(opaque(&model), whole, "and back");
    }

    /// A bound picture minifies through a mip chain: a fine checkerboard
    /// on a slot seen small reads as an even grey, not as the aliased
    /// black-and-white it is when bound raw.
    #[test]
    fn a_bound_picture_minifies_through_its_mips() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let mut model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [1.0, 1.0, 1.0, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("upload");
        let (w, h) = (512u32, 512u32);
        let mut checks = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = (x + y) % 2 == 0;
                let v = if on { 255 } else { 0 };
                checks.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let picture = upload_rgba(&ctx, w, h, &checks);
        let picture_view = picture.create_view(&Default::default());
        // Face on, far away: the face is a few dozen pixels wide, the
        // checks a small fraction of a pixel.
        let face = |model: &GpuModel| -> (u32, u32) {
            let view = ModelView {
                yaw: 0.0,
                pitch: 0.0,
                distance: 12.0,
                ..ModelView::default()
            };
            let px = pass
                .render_to_bytes(&ctx, model, &view, &[IDENTITY], 96, 96)
                .expect("render");
            let mut values = Vec::new();
            for y in 44..52 {
                for x in 44..52 {
                    let i = (y * 96 + x) * 4;
                    values.push(px[i] as u32);
                }
            }
            let mean = values.iter().sum::<u32>() / values.len() as u32;
            let spread = values
                .iter()
                .map(|v| (*v as i32 - mean as i32).unsigned_abs())
                .max()
                .unwrap_or(0);
            (mean, spread)
        };
        pass.set_texture(&ctx, &mut model, 0, &picture_view, None);
        let (raw_mean, raw_spread) = face(&model);
        pass.set_texture(&ctx, &mut model, 0, &picture_view, Some((w, h)));
        let (mip_mean, mip_spread) = face(&model);
        assert!(
            mip_spread < 24 && (90..=170).contains(&mip_mean),
            "through the mips the checks average to grey: mean {mip_mean}, spread {mip_spread} \
             (raw: mean {raw_mean}, spread {raw_spread})"
        );
        assert!(
            mip_spread < raw_spread || raw_spread == 0,
            "the raw binding is the aliased one: raw spread {raw_spread}, mips {mip_spread}"
        );
    }

    /// A picture WORN by a slot takes the light and a picture SHOWN as a
    /// screen does not: the same picture bound to a cube's face reads the
    /// same under a frontal and a grazing light as a screen, and darker
    /// under the grazing one once worn — and tiling it changes nothing
    /// about that.
    #[test]
    fn a_worn_picture_takes_the_light_and_a_screen_does_not() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let mut model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("upload");
        let picture = upload_rgba(&ctx, 2, 2, &[170, 120, 60, 255].repeat(4));
        let picture_view = picture.create_view(&Default::default());
        let face = |model: &GpuModel, light_yaw: f64| -> u32 {
            let view = ModelView {
                yaw: 0.0,
                pitch: 0.0,
                distance: 3.0,
                light_yaw,
                light_pitch: 15.0,
                ..ModelView::default()
            };
            let px = pass
                .render_to_bytes(&ctx, model, &view, &[IDENTITY], 96, 96)
                .expect("render");
            let mut sum = 0u32;
            let mut count = 0u32;
            for y in 40..56 {
                for x in 30..66 {
                    let i = (y * 96 + x) * 4;
                    sum += px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32;
                    count += 3;
                }
            }
            sum / count
        };
        pass.set_texture(&ctx, &mut model, 0, &picture_view, None);
        let (front, grazing) = (face(&model, 0.0), face(&model, 85.0));
        assert!(
            (front as i32 - grazing as i32).abs() < 3,
            "a screen ignores the light: frontal {front}, grazing {grazing}"
        );
        pass.set_wear(&ctx, &mut model, 0, true, [1.0, 1.0], [0.0, 0.0]);
        let (front, grazing) = (face(&model, 0.0), face(&model, 85.0));
        assert!(
            front as i32 - grazing as i32 > 20,
            "a worn picture is lit: frontal {front}, grazing {grazing}"
        );
        pass.set_wear(&ctx, &mut model, 0, true, [4.0, 4.0], [0.5, 0.0]);
        let tiled = face(&model, 0.0);
        assert!(
            (tiled as i32 - front as i32).abs() < 3,
            "a flat picture tiled is the same picture: {tiled} vs {front}"
        );
        pass.set_wear(&ctx, &mut model, 0, false, [1.0, 1.0], [0.0, 0.0]);
        let (front, grazing) = (face(&model, 0.0), face(&model, 85.0));
        assert!(
            (front as i32 - grazing as i32).abs() < 3,
            "back to a screen: frontal {front}, grazing {grazing}"
        );
    }

    /// A normal map tilts the shading: a map whose left half leans its
    /// normals toward +X reads brighter than the flat right half under a
    /// light from the right — and darker under one from the left. Both
    /// directions, so a wrong-signed tangent frame cannot pass.
    #[test]
    fn a_normal_map_tilts_the_shading() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let (w, h) = (64u32, 64u32);
        let mut map = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                // Left half: (0.8, 0, 0.6) — leaning toward +X; right half: straight up.
                let px: [u8; 4] = if x < w / 2 {
                    [230, 128, 204, 255]
                } else {
                    [128, 128, 255, 255]
                };
                map.extend_from_slice(&px);
            }
        }
        let material = || MaterialInput {
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            double_sided: false,
            texture: None,
            normal: Some((w, h, &map)),
            metal_rough: None,
        };
        let (left, right) = halves(material(), 60.0, 20.0);
        assert!(
            left > right + 8,
            "leaning toward the light reads brighter: left {left} right {right}"
        );
        let (left, right) = halves(material(), -60.0, 20.0);
        assert!(
            left + 8 < right,
            "leaning away from the light reads darker: left {left} right {right}"
        );
        // And the map's +Y: a map leaning every normal upward reads
        // brighter than a flat one under a light from above, darker under
        // one from below the horizon.
        let up_map: Vec<u8> = (0..w * h).flat_map(|_| [128, 230, 204, 255]).collect();
        let flat_map: Vec<u8> = (0..w * h).flat_map(|_| [128, 128, 255, 255]).collect();
        let face = |map: &[u8], pitch: f64| -> u32 {
            let (l, r) = halves(
                MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: Some((w, h, map)),
                    metal_rough: None,
                },
                0.0,
                pitch,
            );
            (l + r) / 2
        };
        assert!(
            face(&up_map, 60.0) > face(&flat_map, 60.0) + 8,
            "leaning up toward a high light reads brighter: {} vs {}",
            face(&up_map, 60.0),
            face(&flat_map, 60.0)
        );
        assert!(
            face(&up_map, -40.0) + 8 < face(&flat_map, -40.0),
            "leaning up under a low light reads darker: {} vs {}",
            face(&up_map, -40.0),
            face(&flat_map, -40.0)
        );
    }

    /// A metallic-roughness map varies the finish across one material:
    /// with the factor at full metal, a map whose blue channel is 0 on the
    /// left and 255 on the right makes the left a dielectric and the right
    /// a metal, and the two halves read differently.
    #[test]
    fn a_metallic_roughness_map_varies_the_finish() {
        if GpuContext::new().is_err() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let (w, h) = (64u32, 64u32);
        let mut map = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                let px: [u8; 4] = if x < w / 2 {
                    [0, 90, 0, 255]
                } else {
                    [0, 90, 255, 255]
                };
                map.extend_from_slice(&px);
            }
        }
        let material = MaterialInput {
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 1.0,
            roughness: 1.0,
            double_sided: false,
            texture: None,
            normal: None,
            metal_rough: Some((w, h, &map)),
        };
        let (left, right) = halves(material, 40.0, 20.0);
        assert!(
            (left as i32 - right as i32).abs() > 12,
            "a dielectric half and a metal half read differently: left {left} right {right}"
        );
    }

    #[test]
    fn a_cube_seen_face_on_is_one_shade_and_from_a_corner_is_several() {
        let face_on = render(ModelView {
            yaw: 0.0,
            pitch: 0.0,
            bounds_radius: (3.0f32).sqrt() * 0.5,
            ..Default::default()
        });
        let (lum, opaque) = plateaus(&face_on);
        assert!(
            opaque > 96 * 96 / 10 && opaque < 96 * 96 * 9 / 10,
            "framed: {opaque} opaque of 9216"
        );
        let (lo, hi) = (*lum.iter().min().unwrap(), *lum.iter().max().unwrap());
        assert!(hi - lo < 24, "one face, one shade: {lo}..{hi}");
        assert_eq!(face_on[3], 0, "the corner outside the model is clear");

        let corner = render(ModelView {
            yaw: 35.0,
            pitch: 25.0,
            bounds_radius: (3.0f32).sqrt() * 0.5,
            ..Default::default()
        });
        let (lum, opaque) = plateaus(&corner);
        assert!(opaque > 96 * 96 / 10, "framed: {opaque}");
        let (lo, hi) = (*lum.iter().min().unwrap(), *lum.iter().max().unwrap());
        assert!(
            hi as i32 - lo as i32 > 40,
            "three faces, three shades: {lo}..{hi}"
        );
        // Lit from the upper left: the top face is the brightest region.
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for (i, p) in corner.chunks_exact(4).enumerate() {
            if p[3] > 250 {
                let y = i / 96;
                let l = (p[2] as u32 * 299 + p[1] as u32 * 587 + p[0] as u32 * 114) / 1000;
                if y < 40 {
                    top.push(l);
                } else if y > 56 {
                    bottom.push(l);
                }
            }
        }
        let mean = |v: &[u32]| v.iter().sum::<u32>() as f64 / v.len().max(1) as f64;
        assert!(
            mean(&top) > mean(&bottom) + 10.0,
            "top {:.0} vs bottom {:.0}",
            mean(&top),
            mean(&bottom)
        );
    }

    /// A matrix places the mesh: the face-on cube turned 45° about Y by
    /// its node's matrix shows two faces, two shades.
    #[test]
    fn a_node_matrix_turns_the_mesh() {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("model");
        let view = ModelView {
            yaw: 0.0,
            pitch: 0.0,
            bounds_radius: (3.0f32).sqrt() * 0.5,
            ..Default::default()
        };
        let (c, s) = (45f32.to_radians().cos(), 45f32.to_radians().sin());
        let turned: Mat4 = [
            [c, 0.0, -s, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [s, 0.0, c, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let px = pass
            .render_to_bytes(&ctx, &model, &view, &[turned], 96, 96)
            .expect("render");
        let (lum, opaque) = plateaus(&px);
        assert!(opaque > 96 * 96 / 10, "framed: {opaque}");
        let (lo, hi) = (*lum.iter().min().unwrap(), *lum.iter().max().unwrap());
        assert!(
            hi as i32 - lo as i32 > 30,
            "two faces, two shades: {lo}..{hi}"
        );
    }

    /// A stage: a green picture standing in front of the cube covers its
    /// middle; the same picture behind the cube is covered by it.
    #[test]
    fn a_billboard_and_a_model_share_one_depth_buffer() {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("model");
        let green =
            upload_rgba(&ctx, 2, 2, &[20, 220, 60, 255].repeat(4)).create_view(&Default::default());
        let view = ModelView {
            yaw: 0.0,
            pitch: 0.0,
            bounds_radius: 1.5,
            distance: 4.0,
            ..Default::default()
        };
        let centre = |px: &[u8]| -> [u8; 3] {
            let i = (32 * 64 + 32) * 4;
            [px[i + 2], px[i + 1], px[i]]
        };
        let render = |z: f32| {
            let items = [
                StageItem::Model {
                    model: &model,
                    matrices: &[IDENTITY],
                },
                StageItem::Billboard {
                    texture: &green,
                    center: [0.0, 0.0, z],
                    size: [0.6, 0.6],
                },
            ];
            let texture = pass
                .render_scene(&ctx, &items, &view, 64, 64)
                .expect("scene");
            read_texture(&ctx, &texture, 64, 64).expect("read")
        };
        let front = centre(&render(1.0));
        assert!(
            front[1] > 150 && front[1] > front[0] + 60,
            "in front, the picture shows: {front:?}"
        );
        let behind = centre(&render(-1.0));
        assert!(
            (behind[1] as i32 - behind[0] as i32).abs() < 30,
            "behind, the cube covers it: {behind:?}"
        );
    }

    #[test]
    fn a_recoloured_slot_changes_the_pixels() {
        let ctx = GpuContext::new().expect("gpu");
        let pass = ModelPass::new(&ctx).expect("pass");
        let (p, n, uv, idx) = cube();
        let mut model = pass
            .upload(
                &ctx,
                &[MeshInput {
                    positions: &p,
                    normals: &n,
                    uvs: &uv,
                    indices: &idx,
                    material: 0,
                    node: 0,
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                    normal: None,
                    metal_rough: None,
                }],
            )
            .expect("model");
        let view = ModelView {
            bounds_radius: (3.0f32).sqrt() * 0.5,
            ..Default::default()
        };
        let grey = pass
            .render_to_bytes(&ctx, &model, &view, &[IDENTITY], 64, 64)
            .expect("grey");
        pass.recolor(&ctx, &mut model, 0, [0.9, 0.1, 0.1, 1.0]);
        let red = pass
            .render_to_bytes(&ctx, &model, &view, &[IDENTITY], 64, 64)
            .expect("red");
        let centre = (32 * 64 + 32) * 4;
        assert!(
            (grey[centre + 2] as i32 - grey[centre] as i32).abs() < 20,
            "grey is grey"
        );
        assert!(
            red[centre + 2] as i32 > red[centre] as i32 + 60,
            "red is red: {:?}",
            &red[centre..centre + 4]
        );
    }
}
