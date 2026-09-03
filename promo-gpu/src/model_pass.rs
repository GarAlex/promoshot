//! The model pass: a glTF model lit and drawn into an offscreen texture at
//! a quad's pixel size, which the compositor then draws like any other
//! picture — the slab pattern per frame on the GPU (3D plan, section 2).
//!
//! Shading v1 is PBR-lite: one key light (Lambert + a Blinn-Phong lobe
//! narrowed by roughness), an ambient term and a rim, all from the theme
//! by default. sRGB textures decode to linear and the result encodes back,
//! premultiplied, so the compositor's existing input path applies.
//! Depth is real within the pass; between layers `sortIndex` orders.

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
        }
    }
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
};
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var env_tex: texture_2d<f32>;
@group(0) @binding(2) var env_samp: sampler;
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

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
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
    let l = normalize(frame.light_dir.xyz);
    let v = normalize(frame.camera_pos.xyz - in.world);
    let h = normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    let ndh = max(dot(n, h), 0.0);
    let ndv = max(dot(n, v), 0.0);
    // Diffuse dims with metalness; the specular lobe takes the base colour
    // for a metal and stays white for a dielectric.
    let diffuse = albedo.rgb * (1.0 - metallic) * ndl;
    let shininess = mix(8.0, 160.0, (1.0 - roughness) * (1.0 - roughness));
    let spec_color = mix(vec3<f32>(0.04), albedo.rgb, metallic);
    let specular = spec_color * pow(ndh, shininess) * (1.0 - roughness * 0.6) * step(0.0001, ndl);
    let key = (diffuse + specular) * frame.key_rgb.rgb * frame.light_dir.w;
    let ambient = albedo.rgb * (1.0 - metallic * 0.8) * frame.ambient_rgb.rgb;
    let rim = frame.rim_rgb.rgb * pow(1.0 - ndv, 3.0) * 0.6 * (1.0 - metallic * 0.5);
    // What a metal mirrors and a glossy dielectric sheens: the scene's
    // environment when one is bound — an equirectangular HDR sampled at
    // the reflection, blurrier with roughness, its blurriest level
    // standing in for irradiance — else a stand-in sky-to-ground gradient
    // by the reflection's height with a darker horizon band.
    let refl = reflect(-v, n);
    var env: vec3<f32>;
    var ambient_env = vec3<f32>(0.0);
    if (frame.env_params.x > 0.5) {
        env = env_sample(refl, roughness * frame.env_params.w);
        ambient_env = env_sample(n, frame.env_params.w);
    } else {
        let up = clamp(refl.y * 0.5 + 0.5, 0.0, 1.0);
        let sky = frame.rim_rgb.rgb * 1.4 + vec3<f32>(0.35);
        let ground = frame.ambient_rgb.rgb * 0.6;
        env = mix(ground, sky, up);
        env = env * (0.7 + 0.3 * smoothstep(0.0, 0.25, abs(refl.y)));
    }
    let fresnel = pow(1.0 - ndv, 5.0);
    let gloss = 1.0 - roughness;
    let mirror = env * albedo.rgb * metallic * (0.75 + 0.25 * fresnel) * (0.5 + 0.5 * gloss);
    let sheen = env * (0.04 + 0.35 * fresnel) * gloss * (1.0 - metallic);
    let fill = albedo.rgb * (1.0 - metallic * 0.8) * ambient_env * 0.5;
    let lit = key + ambient + fill + rim + mirror + sheen;
    let encoded = linear_to_srgb(clamp(lit, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(encoded * albedo.a, albedo.a);
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
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MaterialRaw {
    base_color: [f32; 4],
    factors: [f32; 4],
    fit: [f32; 4],
    uv: [f32; 4],
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

fn slice_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

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
    /// Width over height of the surface this slot's uvs span, from the
    /// geometry that wears it — what a bound picture is fitted to.
    aspect: f32,
    /// The file's normal map and metallic-roughness map, if any — kept so
    /// a rebind (a picture on the slot) carries them along.
    normal_view: Option<wgpu::TextureView>,
    mr_view: Option<wgpu::TextureView>,
}

impl GpuMaterial {
    /// `fit`'s flags: a normal map bound, a metallic-roughness map bound.
    fn map_flags(&self) -> (f32, f32) {
        (
            if self.normal_view.is_some() { 1.0 } else { 0.0 },
            if self.mr_view.is_some() { 1.0 } else { 0.0 },
        )
    }
}

/// A model's buffers on the GPU, built once per resource and drawn many
/// times. Bind groups are the pass's, so build one through
/// [`ModelPass::upload`].
pub struct GpuModel {
    meshes: Vec<GpuMesh>,
    materials: Vec<GpuMaterial>,
}

/// The pass itself: one pipeline, shared across every model layer.
pub struct ModelPass {
    pipeline: wgpu::RenderPipeline,
    frame_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
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
        let flat_normal = upload_rgba(ctx, 1, 1, &[128, 128, 255, 255]).create_view(&Default::default());
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
            frame_layout,
            material_layout,
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
        // Each slot's surface aspect: the two largest extents of the
        // vertices its primitives index. A flat screen gives width over
        // height; a box gives its two longest sides, which is as good as a
        // wrapped picture can be placed.
        let mut aspects = vec![1.0f32; materials.len()];
        for (index, aspect) in aspects.iter_mut().enumerate() {
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            let mut any = false;
            for mesh in meshes.iter().filter(|m| m.material == index) {
                for &i in mesh.indices {
                    if let Some(p) = mesh.positions.get(i as usize) {
                        any = true;
                        for k in 0..3 {
                            min[k] = min[k].min(p[k]);
                            max[k] = max[k].max(p[k]);
                        }
                    }
                }
            }
            if any {
                let mut extents = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
                // Width is the horizontal extent, height the vertical one,
                // when the surface faces the viewer; otherwise the longest
                // two axes in that order.
                let (w, h) = if extents[2] <= extents[0].min(extents[1]) {
                    (extents[0], extents[1])
                } else {
                    extents.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                    (extents[0], extents[1])
                };
                if w > 1e-6 && h > 1e-6 {
                    *aspect = w / h;
                }
            }
        }
        let mut gpu_materials = Vec::with_capacity(materials.len());
        for (index, m) in materials.iter().enumerate() {
            let well_formed = |t: &(u32, u32, &[u8])| t.0 > 0 && t.1 > 0 && t.2.len() == (t.0 * t.1 * 4) as usize;
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
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-material"),
                contents: as_bytes(&MaterialRaw {
                    uv: [1.0, 1.0, 0.0, 0.0],
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
                aspect: aspects[index],
                normal_view,
                mr_view,
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
            let (has_normal, has_mr) = m.map_flags();
            ctx.queue.write_buffer(
                &m.uniform,
                0,
                as_bytes(&MaterialRaw {
                    uv: m.uv,
                    base_color: m.base_color,
                    factors: [
                        m.metallic,
                        m.roughness,
                        m.textured,
                        if m.double_sided { 1.0 } else { 0.0 },
                    ],
                    fit: [m.aspect, has_normal, has_mr, 0.0],
                }),
            );
        }
    }

    /// Paint one material slot with a picture — a `materials` binding to a
    /// resource: the slot's base colour texture becomes `view`, sampled
    /// with the mesh's own uvs (sRGB, as every imported frame is).
    pub fn set_texture(
        &self,
        ctx: &GpuContext,
        model: &mut GpuModel,
        material: usize,
        view: &wgpu::TextureView,
    ) {
        let Some(m) = model.materials.get_mut(material) else {
            return;
        };
        m.textured = if m.worn { 4.0 } else { 2.0 };
        let (has_normal, has_mr) = m.map_flags();
        ctx.queue.write_buffer(
            &m.uniform,
            0,
            as_bytes(&MaterialRaw {
                uv: m.uv,
                base_color: m.base_color,
                factors: [
                    m.metallic,
                    m.roughness,
                    m.textured,
                    if m.double_sided { 1.0 } else { 0.0 },
                ],
                fit: [m.aspect, has_normal, has_mr, 0.0],
            }),
        );
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
        let (has_normal, has_mr) = m.map_flags();
        ctx.queue.write_buffer(
            &m.uniform,
            0,
            as_bytes(&MaterialRaw {
                uv: m.uv,
                base_color: m.base_color,
                factors: [
                    m.metallic,
                    m.roughness,
                    m.textured,
                    if m.double_sided { 1.0 } else { 0.0 },
                ],
                fit: [m.aspect, has_normal, has_mr, 0.0],
            }),
        );
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
        use wgpu::util::DeviceExt;
        let (width, height) = (width.max(1), height.max(1));
        let device = &ctx.device;
        let aspect = width as f32 / height as f32;
        let frame = frame_uniforms(view, aspect);
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
                self.set_texture(ctx, &mut model, 0, texture);
                if let Some(m) = model.materials.get_mut(0) {
                    // The quad IS the picture's box: no letterbox, and the
                    // picture's own alpha decides what is drawn.
                    m.aspect = size[0] / size[1].max(1e-6);
                    m.textured = 3.0;
                    ctx.queue.write_buffer(
                        &m.uniform,
                        0,
                        as_bytes(&MaterialRaw {
                            uv: m.uv,
                            base_color: m.base_color,
                            factors: [m.metallic, m.roughness, 3.0, 1.0],
                            fit: [m.aspect, 0.0, 0.0, 0.0],
                        }),
                    );
                }
                billboards.push(model);
            }
        }

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
        let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stage-frame"),
            contents: as_bytes(&frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stage-frame"),
            layout: &self.frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(self.env_view(view.environment.preset)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.env_sampler),
                },
            ],
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
            pass.set_bind_group(0, &frame_bind, &[]);
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
        use wgpu::util::DeviceExt;
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
        let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("model-frame"),
            contents: as_bytes(&frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model-frame"),
            layout: &self.frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(self.env_view(view.environment.preset)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.env_sampler),
                },
            ],
        });
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
/// (256 × 128 down to 1 × 1, whose one texel is the irradiance stand-in).
const ENV_WIDTH: u32 = 256;
const ENV_HEIGHT: u32 = 128;
const ENV_MIPS: u32 = 8;

/// One preset as linear RGB radiance over the sphere: `yaw` from -π to π
/// about the vertical, `el` the elevation, -π/2 at the floor.
fn env_radiance(preset: EnvPreset, yaw: f32, el: f32) -> [f32; 3] {
    let gauss = |dy: f32, de: f32, sy: f32, se: f32| -> f32 {
        let dy = (dy + std::f32::consts::PI).rem_euclid(2.0 * std::f32::consts::PI) - std::f32::consts::PI;
        (-(dy * dy) / (2.0 * sy * sy) - (de * de) / (2.0 * se * se)).exp()
    };
    let t = (el / std::f32::consts::FRAC_PI_2).clamp(-1.0, 1.0);
    let mix3 = |a: [f32; 3], b: [f32; 3], k: f32| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k];
    let add = |a: [f32; 3], b: [f32; 3], k: f32| [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k];
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
            add(add(add(base, [1.0, 1.0, 1.0], band), [1.0, 1.0, 1.0], key_box), [1.0, 1.0, 1.0], fill_box)
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
            let city = (gauss(yaw - 0.9, el, 0.15, 0.04) + gauss(yaw - 1.7, el, 0.12, 0.04) + gauss(yaw + 2.2, el, 0.2, 0.04)) * 0.8;
            add(add(add(base, [2.0, 2.2, 2.8], moon), [0.25, 0.27, 0.35], halo), [1.0, 0.62, 0.3], city)
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

/// A preset as an Rgba16Float equirectangular texture with a full mip
/// chain — each level a box filter of the one above, the last a single
/// texel of the sphere's mean radiance.
fn upload_env(ctx: &GpuContext, preset: EnvPreset) -> wgpu::Texture {
    let (w, h) = (ENV_WIDTH as usize, ENV_HEIGHT as usize);
    let mut level: Vec<[f32; 4]> = Vec::with_capacity(w * h);
    for y in 0..h {
        let el = std::f32::consts::FRAC_PI_2 - (y as f32 + 0.5) / h as f32 * std::f32::consts::PI;
        for x in 0..w {
            let yaw = (x as f32 + 0.5) / w as f32 * 2.0 * std::f32::consts::PI - std::f32::consts::PI;
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
        let mut next = Vec::with_capacity(nw * nh);
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0.0f32; 4];
                let mut n = 0.0;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (sx, sy) = ((x * 2 + dx).min(lw - 1), (y * 2 + dy).min(lh - 1));
                        let p = level[sy * lw + sx];
                        for k in 0..4 {
                            acc[k] += p[k];
                        }
                        n += 1.0;
                    }
                }
                next.push([acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n]);
            }
        }
        level = next;
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
    let radius = view.bounds_radius.max(1e-6);
    let distance = (view.distance.max(1.05) as f32) * radius;
    let center = view.bounds_center;
    let toward = direction(view.yaw, view.pitch);
    let eye = [
        center[0] + toward[0] * distance,
        center[1] + toward[1] * distance,
        center[2] + toward[2] * distance,
    ];
    let forward = norm(sub(center, eye));
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
    let distance = (view.distance.max(1.05) as f32) * radius;
    let center = view.bounds_center;
    let toward = direction(view.yaw, view.pitch);
    let eye = [
        center[0] + toward[0] * distance,
        center[1] + toward[1] * distance,
        center[2] + toward[2] * distance,
    ];
    let forward = norm(sub(center, eye));
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
    let near = (distance - radius * 1.5).max(distance * 0.02);
    let far = distance + radius * 3.0;
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
            if view.environment.preset == EnvPreset::None { 0.0 } else { 1.0 },
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
        pass.set_texture(&ctx, &mut model, 0, &picture_view);
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
                let px: [u8; 4] = if x < w / 2 { [230, 128, 204, 255] } else { [128, 128, 255, 255] };
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
                let px: [u8; 4] = if x < w / 2 { [0, 90, 0, 255] } else { [0, 90, 255, 255] };
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
