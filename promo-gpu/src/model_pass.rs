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
}

/// A material's factors and, if any, its base colour texture (RGBA8 sRGB).
pub struct MaterialInput<'a> {
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub double_sided: bool,
    pub texture: Option<(u32, u32, &'a [u8])>,
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
};
struct Material {
    base_color: vec4<f32>,
    // x = metallic, y = roughness, z = 1 file texture (lit) / 2 a picture
    // bound by the project (unlit), w = 1 double sided
    factors: vec4<f32>,
    // x = the slot's own aspect (width / height of the surface its uvs
    // span), so a bound picture is fitted rather than stretched.
    fit: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var base_tex: texture_2d<f32>;
@group(1) @binding(2) var base_samp: sampler;

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
    out.clip = frame.view_proj * vec4<f32>(v.pos, 1.0);
    out.world = v.pos;
    out.normal = v.normal;
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

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    if (!front) {
        if (material.factors.w < 0.5) { discard; }
        n = -n;
    }
    var albedo = material.base_color;
    if (material.factors.z > 1.5) {
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
    }
    if (material.factors.z > 0.5) {
        let t = textureSample(base_tex, base_samp, in.uv);
        albedo = vec4<f32>(albedo.rgb * srgb_to_linear(t.rgb), albedo.a * t.a);
    }
    let metallic = material.factors.x;
    let roughness = clamp(material.factors.y, 0.05, 1.0);
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
    let ambient = albedo.rgb * frame.ambient_rgb.rgb;
    let rim = frame.rim_rgb.rgb * pow(1.0 - ndv, 3.0) * 0.6;
    let lit = key + ambient + rim;
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
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MaterialRaw {
    base_color: [f32; 4],
    factors: [f32; 4],
    fit: [f32; 4],
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
}

struct GpuMaterial {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    double_sided: bool,
    /// 0 none, 1 the file's own texture (lit), 2 a picture the project
    /// bound to the slot (shown as-is).
    textured: f32,
    /// Width over height of the surface this slot's uvs span, from the
    /// geometry that wears it — what a bound picture is fitted to.
    aspect: f32,
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
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
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
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model-pass"),
            bind_group_layouts: &[&frame_layout, &material_layout],
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
        Ok(ModelPass {
            pipeline,
            frame_layout,
            material_layout,
            sampler,
            white,
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
            let texture = m
                .texture
                .filter(|(w, h, px)| *w > 0 && *h > 0 && px.len() == (*w * *h * 4) as usize)
                .map(|(w, h, px)| upload_rgba(ctx, w, h, px).create_view(&Default::default()));
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model-material"),
                contents: as_bytes(&MaterialRaw {
                    base_color: m.base_color,
                    factors: [
                        m.metallic,
                        m.roughness,
                        if texture.is_some() { 1.0 } else { 0.0 },
                        if m.double_sided { 1.0 } else { 0.0 },
                    ],
                    fit: [aspects[index], 0.0, 0.0, 0.0],
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
                ],
            });
            gpu_materials.push(GpuMaterial {
                uniform,
                bind,
                base_color: m.base_color,
                metallic: m.metallic,
                roughness: m.roughness,
                double_sided: m.double_sided,
                textured: if texture.is_some() { 1.0 } else { 0.0 },
                aspect: aspects[index],
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
            gpu_meshes.push(GpuMesh {
                vertices,
                indices: index_buffer,
                index_count,
                material: mesh.material.min(gpu_materials.len().saturating_sub(1)),
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
        if let Some(m) = model.materials.get_mut(material) {
            m.base_color = rgba;
            ctx.queue.write_buffer(
                &m.uniform,
                0,
                as_bytes(&MaterialRaw {
                    base_color: m.base_color,
                    factors: [
                        m.metallic,
                        m.roughness,
                        m.textured,
                        if m.double_sided { 1.0 } else { 0.0 },
                    ],
                    fit: [m.aspect, 0.0, 0.0, 0.0],
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
        m.textured = 2.0;
        ctx.queue.write_buffer(
            &m.uniform,
            0,
            as_bytes(&MaterialRaw {
                base_color: m.base_color,
                factors: [
                    m.metallic,
                    m.roughness,
                    2.0,
                    if m.double_sided { 1.0 } else { 0.0 },
                ],
                fit: [m.aspect, 0.0, 0.0, 0.0],
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
            ],
        });
    }

    /// Render the model at `width × height` and hand the texture over.
    pub fn render(
        &self,
        ctx: &GpuContext,
        model: &GpuModel,
        view: &ModelView,
        width: u32,
        height: u32,
    ) -> Result<InputTexture, GpuError> {
        let texture = self.render_to_texture(ctx, model, view, width, height)?;
        Ok(crate::compositor::Compositor::adopt_owned_texture(texture))
    }

    /// Render the model at `width × height` into a texture the caller
    /// owns — what an engine wraps as a cached frame.
    pub fn render_to_texture(
        &self,
        ctx: &GpuContext,
        model: &GpuModel,
        view: &ModelView,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, GpuError> {
        use wgpu::util::DeviceExt;
        let (width, height) = (width.max(1), height.max(1));
        let device = &ctx.device;
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
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buffer.as_entire_binding(),
            }],
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
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, GpuError> {
        let texture = self.render_to_texture(ctx, model, view, width, height)?;
        read_texture(ctx, &texture, width, height)
    }
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

/// A direction from yaw (about +Y, 0 = toward +Z, the viewer's side) and
/// pitch (up from the ground plane), degrees.
fn direction(yaw: f64, pitch: f64) -> [f32; 3] {
    let (y, p) = (deg(yaw), deg(pitch));
    [p.cos() * y.sin(), p.sin(), p.cos() * y.cos()]
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
    }
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
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                }],
            )
            .expect("model");
        pass.render_to_bytes(&ctx, &model, &view, 96, 96)
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
                }],
                &[MaterialInput {
                    base_color: [0.8, 0.8, 0.8, 1.0],
                    metallic: 0.0,
                    roughness: 0.6,
                    double_sided: false,
                    texture: None,
                }],
            )
            .expect("model");
        let view = ModelView {
            bounds_radius: (3.0f32).sqrt() * 0.5,
            ..Default::default()
        };
        let grey = pass
            .render_to_bytes(&ctx, &model, &view, 64, 64)
            .expect("grey");
        pass.recolor(&ctx, &mut model, 0, [0.9, 0.1, 0.1, 1.0]);
        let red = pass
            .render_to_bytes(&ctx, &model, &view, 64, 64)
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
