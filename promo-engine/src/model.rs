//! Models: a glTF 2.0 binary decoded into what the model pass draws.
//!
//! The loader is the whole of the file's I/O: meshes flattened to world
//! space (v1 draws the default scene at rest; clips come with P1),
//! materials by the SLOT NAME the file exports — what `materials` bindings
//! in the project refer to — and the bounds the plan's camera `distance`
//! and placement are measured against. Nothing here touches the GPU; the
//! pass in `promo-gpu` takes these buffers as they are.

use std::collections::HashMap;
use std::fmt;

/// A model, decoded. Geometry in each node's LOCAL space with the node
/// tree beside it, so a clip can pose it; one mesh entry per primitive.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    /// The node tree of the default scene, parents before children.
    pub nodes: Vec<Node>,
    /// The file's animations, by name, sampled by [`Model::pose`].
    pub clips: Vec<Clip>,
    /// Centre of the geometry's bounding box at rest and the radius of the
    /// sphere round it, in the file's units, world space.
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
}

/// One primitive, in its node's local space, with one material.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub material: usize,
    /// The node this primitive hangs from — whose matrix places it.
    pub node: usize,
}

/// A node of the scene: its rest transform and its parent.
#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<usize>,
    pub translation: [f32; 3],
    /// Unit quaternion, `[x, y, z, w]`.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

/// One animation: what it moves and for how long.
#[derive(Debug, Clone, Default)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<Channel>,
}

/// One animated property of one node, keyed in seconds.
#[derive(Debug, Clone)]
pub struct Channel {
    pub node: usize,
    pub property: Property,
    pub times: Vec<f32>,
    /// Translations and scales use three components, rotations four.
    pub values: Vec<[f32; 4]>,
    pub step: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    Translation,
    Rotation,
    Scale,
}

/// A glTF metallic-roughness material, PBR-lite: what the v1 pass shades.
#[derive(Debug, Clone)]
pub struct Material {
    /// The slot name as exported — `materials` bindings name this.
    pub name: String,
    /// Straight RGBA, linear.
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub double_sided: bool,
    /// The base colour texture, RGBA8 sRGB, if the file carries one.
    pub base_texture: Option<Texture>,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            name: String::new(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            double_sided: false,
            base_texture: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, sRGB-encoded.
    pub rgba: Vec<u8>,
}

impl fmt::Debug for Texture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Texture({}x{})", self.width, self.height)
    }
}

#[derive(Debug)]
pub enum ModelError {
    Decode(String),
    Empty,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Decode(why) => write!(f, "model: {why}"),
            ModelError::Empty => write!(f, "model: no geometry in the default scene"),
        }
    }
}

impl std::error::Error for ModelError {}

pub type Mat4 = [[f32; 4]; 4];

/// Column-major, as glTF stores them: `m[column][row]`.
fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let x = m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0];
    let y = m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1];
    let z = m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2];
    let w = m[0][3] * p[0] + m[1][3] * p[1] + m[2][3] * p[2] + m[3][3];
    if w.abs() > 1e-8 && (w - 1.0).abs() > 1e-6 {
        [x / w, y / w, z / w]
    } else {
        [x, y, z]
    }
}

/// Column-major matrix from translation, unit quaternion `[x, y, z, w]`
/// and scale — glTF's node transform.
fn trs_matrix(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> Mat4 {
    let [x, y, z, w] = r;
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    [
        [
            (1.0 - 2.0 * (yy + zz)) * s[0],
            (2.0 * (xy + wz)) * s[0],
            (2.0 * (xz - wy)) * s[0],
            0.0,
        ],
        [
            (2.0 * (xy - wz)) * s[1],
            (1.0 - 2.0 * (xx + zz)) * s[1],
            (2.0 * (yz + wx)) * s[1],
            0.0,
        ],
        [
            (2.0 * (xz + wy)) * s[2],
            (2.0 * (yz - wx)) * s[2],
            (1.0 - 2.0 * (xx + yy)) * s[2],
            0.0,
        ],
        [t[0], t[1], t[2], 1.0],
    ]
}

impl Channel {
    /// The value at `t` seconds: held before the first key and after the
    /// last, stepped or linearly blended between (rotations slerp).
    pub fn sample(&self, t: f32) -> [f32; 4] {
        let n = self.times.len();
        if n == 0 {
            return [0.0; 4];
        }
        if t <= self.times[0] || n == 1 {
            return self.values[0];
        }
        if t >= self.times[n - 1] {
            return self.values[n - 1];
        }
        let next = self.times.partition_point(|&k| k <= t).min(n - 1);
        let prev = next.saturating_sub(1);
        if self.step {
            return self.values[prev];
        }
        let span = (self.times[next] - self.times[prev]).max(1e-6);
        let f = ((t - self.times[prev]) / span).clamp(0.0, 1.0);
        let (a, b) = (self.values[prev], self.values[next]);
        match self.property {
            Property::Rotation => slerp(a, b, f),
            _ => [
                a[0] + (b[0] - a[0]) * f,
                a[1] + (b[1] - a[1]) * f,
                a[2] + (b[2] - a[2]) * f,
                0.0,
            ],
        }
    }
}

fn slerp(a: [f32; 4], mut b: [f32; 4], f: f32) -> [f32; 4] {
    let mut dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        b = [-b[0], -b[1], -b[2], -b[3]];
        dot = -dot;
    }
    let (wa, wb) = if dot > 0.9995 {
        (1.0 - f, f)
    } else {
        let theta = dot.clamp(-1.0, 1.0).acos();
        let s = theta.sin().max(1e-6);
        (((1.0 - f) * theta).sin() / s, (f * theta).sin() / s)
    };
    let q = [
        a[0] * wa + b[0] * wb,
        a[1] * wa + b[1] * wb,
        a[2] * wa + b[2] * wb,
        a[3] * wa + b[3] * wb,
    ];
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3])
        .sqrt()
        .max(1e-9);
    [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-12 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Flat normals for a mesh the file left bare: each triangle's face normal
/// on each of its (already unshared) vertices — shared vertices get the
/// sum, which is a serviceable smooth normal.
fn flat_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if a >= positions.len() || b >= positions.len() || c >= positions.len() {
            continue;
        }
        let (pa, pb, pc) = (positions[a], positions[b], positions[c]);
        let n = cross(
            [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]],
            [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]],
        );
        for i in [a, b, c] {
            out[i][0] += n[0];
            out[i][1] += n[1];
            out[i][2] += n[2];
        }
    }
    out.into_iter().map(normalize).collect()
}

impl Model {
    /// Decode a `.glb` (or a self-contained `.gltf` with data URIs).
    pub fn from_glb(bytes: &[u8]) -> Result<Model, ModelError> {
        let (document, buffers, images) =
            gltf::import_slice(bytes).map_err(|e| ModelError::Decode(e.to_string()))?;

        let materials: Vec<Material> = document
            .materials()
            .map(|m| {
                let pbr = m.pbr_metallic_roughness();
                let base_texture = pbr
                    .base_color_texture()
                    .and_then(|info| images.get(info.texture().source().index()))
                    .and_then(texture_rgba8);
                Material {
                    name: m.name().unwrap_or("").to_string(),
                    base_color: pbr.base_color_factor(),
                    metallic: pbr.metallic_factor(),
                    roughness: pbr.roughness_factor(),
                    double_sided: m.double_sided(),
                    base_texture,
                }
            })
            .collect();
        // A primitive with no material draws with glTF's default one.
        let default_material = materials.len();
        let mut materials = materials;
        materials.push(Material {
            name: String::new(),
            ..Material::default()
        });

        // The node tree, parents before children, with each primitive in
        // its node's own space; the glTF node index maps to ours so clips
        // can find their targets.
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .ok_or(ModelError::Empty)?;
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_gltf: HashMap<usize, usize> = HashMap::new();
        let mut meshes = Vec::new();
        let mut stack: Vec<(gltf::Node, Option<usize>)> =
            scene.nodes().map(|n| (n, None)).collect();
        while let Some((node, parent)) = stack.pop() {
            let (translation, rotation, scale) = node.transform().decomposed();
            let index = nodes.len();
            nodes.push(Node {
                parent,
                translation,
                rotation,
                scale,
            });
            by_gltf.insert(node.index(), index);
            if let Some(mesh) = node.mesh() {
                for primitive in mesh.primitives() {
                    if primitive.mode() != gltf::mesh::Mode::Triangles {
                        continue;
                    }
                    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                    let Some(positions) = reader.read_positions() else {
                        continue;
                    };
                    let positions: Vec<[f32; 3]> = positions.collect();
                    let indices: Vec<u32> = match reader.read_indices() {
                        Some(read) => read.into_u32().collect(),
                        None => (0..positions.len() as u32).collect(),
                    };
                    let normals: Vec<[f32; 3]> = match reader.read_normals() {
                        Some(read) => read.collect(),
                        None => flat_normals(&positions, &indices),
                    };
                    let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                        Some(read) => read.into_f32().collect(),
                        None => vec![[0.0, 0.0]; positions.len()],
                    };
                    meshes.push(Mesh {
                        positions,
                        normals,
                        uvs,
                        indices,
                        material: primitive.material().index().unwrap_or(default_material),
                        node: index,
                    });
                }
            }
            for child in node.children() {
                stack.push((child, Some(index)));
            }
        }
        if meshes.iter().all(|m| m.positions.is_empty()) {
            return Err(ModelError::Empty);
        }

        // Clips: every channel whose target is in the scene; times in
        // seconds from the sampler's input accessor.
        let mut clips = Vec::new();
        for animation in document.animations() {
            let mut clip = Clip {
                name: animation
                    .name()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("clip{}", animation.index())),
                ..Clip::default()
            };
            for channel in animation.channels() {
                let Some(&node) = by_gltf.get(&channel.target().node().index()) else {
                    continue;
                };
                let property = match channel.target().property() {
                    gltf::animation::Property::Translation => Property::Translation,
                    gltf::animation::Property::Rotation => Property::Rotation,
                    gltf::animation::Property::Scale => Property::Scale,
                    gltf::animation::Property::MorphTargetWeights => continue,
                };
                let reader = channel.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                let Some(inputs) = reader.read_inputs() else {
                    continue;
                };
                let times: Vec<f32> = inputs.collect();
                let Some(outputs) = reader.read_outputs() else {
                    continue;
                };
                let mut values: Vec<[f32; 4]> = match outputs {
                    gltf::animation::util::ReadOutputs::Translations(it) => {
                        it.map(|v| [v[0], v[1], v[2], 0.0]).collect()
                    }
                    gltf::animation::util::ReadOutputs::Scales(it) => {
                        it.map(|v| [v[0], v[1], v[2], 0.0]).collect()
                    }
                    gltf::animation::util::ReadOutputs::Rotations(it) => it.into_f32().collect(),
                    gltf::animation::util::ReadOutputs::MorphTargetWeights(_) => continue,
                };
                let interpolation = channel.sampler().interpolation();
                // A cubic spline stores in-tangent, value, out-tangent per
                // key; the value is what a linear read of it wants.
                if interpolation == gltf::animation::Interpolation::CubicSpline
                    && values.len() == times.len() * 3
                {
                    values = values.chunks_exact(3).map(|c| c[1]).collect();
                }
                if values.len() != times.len() || times.is_empty() {
                    continue;
                }
                if let Some(&last) = times.last() {
                    clip.duration = clip.duration.max(last);
                }
                clip.channels.push(Channel {
                    node,
                    property,
                    times,
                    values,
                    step: interpolation == gltf::animation::Interpolation::Step,
                });
            }
            if !clip.channels.is_empty() {
                clips.push(clip);
            }
        }

        let mut model = Model {
            meshes,
            materials,
            nodes,
            clips,
            bounds_center: [0.0; 3],
            bounds_radius: 1e-6,
        };
        // Bounds at rest, world space.
        let matrices = model.rest_matrices();
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for mesh in &model.meshes {
            let m = &matrices[mesh.node];
            for p in &mesh.positions {
                let w = transform_point(m, *p);
                for i in 0..3 {
                    min[i] = min[i].min(w[i]);
                    max[i] = max[i].max(w[i]);
                }
            }
        }
        let center = [
            (min[0] + max[0]) / 2.0,
            (min[1] + max[1]) / 2.0,
            (min[2] + max[2]) / 2.0,
        ];
        let radius = model
            .meshes
            .iter()
            .flat_map(|mesh| {
                let m = &matrices[mesh.node];
                mesh.positions.iter().map(move |p| transform_point(m, *p))
            })
            .map(|p| {
                let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
            })
            .fold(0.0f32, f32::max);
        model.bounds_center = center;
        model.bounds_radius = radius.max(1e-6);
        Ok(model)
    }

    /// Every node's world matrix at rest, indexed like `nodes`.
    pub fn rest_matrices(&self) -> Vec<Mat4> {
        self.world_matrices(|node| {
            let n = &self.nodes[node];
            (n.translation, n.rotation, n.scale)
        })
    }

    /// Every node's world matrix with `clip` sampled at `time` seconds;
    /// the rest pose when the clip is unknown. `looping` wraps the time
    /// over the clip's duration (a clip running on layer time); otherwise
    /// it clamps, so a keyed `time` of the full duration is the end pose.
    /// Channels the clip does not touch keep their rest value.
    pub fn pose(&self, clip: &str, time: f64, looping: bool) -> Vec<Mat4> {
        let Some(clip) = self.clips.iter().find(|c| c.name == clip) else {
            return self.rest_matrices();
        };
        let t = if clip.duration <= 0.0 {
            0.0
        } else if looping {
            (time as f32).rem_euclid(clip.duration)
        } else {
            (time as f32).clamp(0.0, clip.duration)
        };
        self.world_matrices(|node| {
            let n = &self.nodes[node];
            let mut trs = (n.translation, n.rotation, n.scale);
            for channel in clip.channels.iter().filter(|c| c.node == node) {
                let v = channel.sample(t);
                match channel.property {
                    Property::Translation => trs.0 = [v[0], v[1], v[2]],
                    Property::Rotation => trs.1 = v,
                    Property::Scale => trs.2 = [v[0], v[1], v[2]],
                }
            }
            trs
        })
    }

    /// The clip names and lengths, as an import writes them.
    pub fn clip_summary(&self) -> Vec<(String, f32)> {
        self.clips
            .iter()
            .map(|c| (c.name.clone(), c.duration))
            .collect()
    }

    fn world_matrices<F>(&self, local: F) -> Vec<Mat4>
    where
        F: Fn(usize) -> ([f32; 3], [f32; 4], [f32; 3]),
    {
        // Parents precede children in `nodes` (the walk pushes a parent
        // before its children are popped), so one pass suffices.
        let mut out: Vec<Mat4> = Vec::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            let (t, r, s) = local(index);
            let m = trs_matrix(t, r, s);
            let world = match node.parent {
                Some(parent) if parent < out.len() => mul(&out[parent], &m),
                _ => m,
            };
            out.push(world);
        }
        out
    }

    /// The slot names, as bindings name them.
    pub fn slot_names(&self) -> Vec<&str> {
        self.materials
            .iter()
            .filter(|m| !m.name.is_empty())
            .map(|m| m.name.as_str())
            .collect()
    }
}

fn texture_rgba8(data: &gltf::image::Data) -> Option<Texture> {
    use gltf::image::Format;
    let (w, h) = (data.width, data.height);
    let px = (w * h) as usize;
    let rgba = match data.format {
        Format::R8G8B8A8 => data.pixels.clone(),
        Format::R8G8B8 => {
            let mut out = Vec::with_capacity(px * 4);
            for c in data.pixels.chunks_exact(3) {
                out.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            out
        }
        Format::R8G8 => {
            let mut out = Vec::with_capacity(px * 4);
            for c in data.pixels.chunks_exact(2) {
                out.extend_from_slice(&[c[0], c[0], c[0], c[1]]);
            }
            out
        }
        Format::R8 => data.pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    if rgba.len() != px * 4 {
        return None;
    }
    Some(Texture {
        width: w,
        height: h,
        rgba,
    })
}

/// A unit cube as a `.glb`, one material slot named `Body`: the fixture
/// every model test and oracle uses, generated rather than checked in.
/// 24 vertices (four per face, so each face has its own flat normal),
/// 36 indices, uvs covering each face.
pub fn sample_cube_glb() -> Vec<u8> {
    sample_cube_glb_with(0.5, "Body", [0.8, 0.2, 0.1, 1.0])
}

pub fn sample_cube_glb_with(half: f32, slot: &str, base_color: [f32; 4]) -> Vec<u8> {
    let h = half;
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
    let mut geo = GlbGeometry::default();
    let mut indices = Vec::new();
    for (n, corners) in &faces {
        geo.quad(*corners, *n, &mut indices);
    }
    geo.build(
        &[GlbMaterial {
            slot,
            base_color,
            metallic: 0.0,
            roughness: 0.6,
            indices: &indices,
        }],
        [-h, -h, -h],
        [h, h, h],
    )
}

/// The sample cube with one clip, `Turn`: a quarter turn about Y over
/// one second — the fixture for everything that samples a clip.
pub fn sample_turning_cube_glb() -> Vec<u8> {
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
    let mut geo = GlbGeometry::default();
    let mut indices = Vec::new();
    for (n, corners) in &faces {
        geo.quad(*corners, *n, &mut indices);
    }
    geo.build_with_turn(
        &[GlbMaterial {
            slot: "Body",
            base_color: [0.75, 0.75, 0.75, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            indices: &indices,
        }],
        [-h, -h, -h],
        [h, h, h],
        Some(("Turn", 1.0, 90.0)),
    )
}

/// A tablet-like slab as a `.glb`: a 16:10 body 1.28 × 0.8 × 0.06 with a
/// `Body` slot and an inset front face with a `Screen` slot — the shape
/// a `materials` binding was made for (a screenshot on the screen, the
/// accent on the body). Generated, like the cube, so nothing binary is
/// checked in; the turntable template and the app's first model use it.
pub fn sample_slab_glb() -> Vec<u8> {
    let (w, h, d) = (0.64f32, 0.40f32, 0.03f32);
    let inset = 0.03f32;
    let mut geo = GlbGeometry::default();
    let mut body = Vec::new();
    let mut screen = Vec::new();
    geo.quad(
        [[-w, -h, d], [w, -h, d], [w, h, d], [-w, h, d]],
        [0.0, 0.0, 1.0],
        &mut body,
    );
    geo.quad(
        [[w, -h, -d], [-w, -h, -d], [-w, h, -d], [w, h, -d]],
        [0.0, 0.0, -1.0],
        &mut body,
    );
    geo.quad(
        [[w, -h, d], [w, -h, -d], [w, h, -d], [w, h, d]],
        [1.0, 0.0, 0.0],
        &mut body,
    );
    geo.quad(
        [[-w, -h, -d], [-w, -h, d], [-w, h, d], [-w, h, -d]],
        [-1.0, 0.0, 0.0],
        &mut body,
    );
    geo.quad(
        [[-w, h, d], [w, h, d], [w, h, -d], [-w, h, -d]],
        [0.0, 1.0, 0.0],
        &mut body,
    );
    geo.quad(
        [[-w, -h, -d], [w, -h, -d], [w, -h, d], [-w, -h, d]],
        [0.0, -1.0, 0.0],
        &mut body,
    );
    // The screen: an inset rectangle a hair in front of the face, so it
    // wins the depth test without fighting.
    let (sw, sh, sz) = (w - inset, h - inset * 1.4, d + 0.002);
    geo.quad(
        [[-sw, -sh, sz], [sw, -sh, sz], [sw, sh, sz], [-sw, sh, sz]],
        [0.0, 0.0, 1.0],
        &mut screen,
    );
    geo.build(
        &[
            GlbMaterial {
                slot: "Body",
                base_color: [0.12, 0.13, 0.16, 1.0],
                metallic: 0.6,
                roughness: 0.35,
                indices: &body,
            },
            GlbMaterial {
                slot: "Screen",
                base_color: [0.05, 0.05, 0.06, 1.0],
                metallic: 0.0,
                roughness: 0.2,
                indices: &screen,
            },
        ],
        [-w, -h, -d],
        [w, h, sz],
    )
}

/// The built-in device bodies (3D plan P4): a phone, a tablet and a
/// laptop, each with a `Body` slot and a `Screen` slot — the laptop with a
/// `Deck` too — generated with rounded corners and a bezel so a
/// screenshot or a recording on the screen reads as the product. What
/// the device frames were, as models: the tilt that was baked is a
/// camera keyframe now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Phone,
    Tablet,
    Laptop,
}

impl DeviceKind {
    pub fn parse(name: &str) -> Option<DeviceKind> {
        match name.trim().to_ascii_lowercase().as_str() {
            "phone" => Some(DeviceKind::Phone),
            "tablet" => Some(DeviceKind::Tablet),
            "laptop" => Some(DeviceKind::Laptop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DeviceKind::Phone => "phone",
            DeviceKind::Tablet => "tablet",
            DeviceKind::Laptop => "laptop",
        }
    }

    pub const ALL: [DeviceKind; 3] = [DeviceKind::Phone, DeviceKind::Tablet, DeviceKind::Laptop];
}

/// A device body as a `.glb`.
pub fn device_glb(kind: DeviceKind) -> Vec<u8> {
    let body = GlbMaterial {
        slot: "Body",
        base_color: [0.16, 0.17, 0.2, 1.0],
        metallic: 0.7,
        roughness: 0.35,
        indices: &[],
    };
    let screen = GlbMaterial {
        slot: "Screen",
        base_color: [0.03, 0.03, 0.04, 1.0],
        metallic: 0.0,
        roughness: 0.15,
        indices: &[],
    };
    let mut geo = GlbGeometry::default();
    let mut body_idx = Vec::new();
    let mut screen_idx = Vec::new();
    let mut deck_idx = Vec::new();
    let (min, max) = match kind {
        DeviceKind::Phone => {
            // To scale with the tablet and laptop (one unit is about 200 mm):
            // a phone stands three quarters of a unit tall. The first fresh
            // agent given both bodies scaled the phone down itself.
            let (w, h, d) = (0.18f32, 0.38f32, 0.019f32);
            geo.rounded_slab(w, h, d, 0.035, [0.0; 3], &mut body_idx);
            geo.rounded_plate(
                w - 0.014,
                h - 0.016,
                0.025,
                d + 0.001,
                [0.0; 3],
                &mut screen_idx,
            );
            ([-w, -h, -d], [w, h, d + 0.001])
        }
        DeviceKind::Tablet => {
            let (w, h, d) = (0.64f32, 0.45f32, 0.028f32);
            geo.rounded_slab(w, h, d, 0.05, [0.0; 3], &mut body_idx);
            geo.rounded_plate(
                w - 0.04,
                h - 0.04,
                0.03,
                d + 0.001,
                [0.0; 3],
                &mut screen_idx,
            );
            ([-w, -h, -d], [w, h, d + 0.001])
        }
        DeviceKind::Laptop => {
            // The base lies flat (its thin axis is Y); the lid stands up
            // from the back edge, leaning 12° past vertical, its screen
            // facing +Z. Both are baked into one node.
            let (w, depth, t) = (0.70f32, 0.46f32, 0.014f32);
            geo.rounded_slab_flat(w, depth, t, 0.03, [0.0, -0.45, 0.0], &mut body_idx);
            // The deck: a plate on the base's top face, a shade lighter.
            geo.rounded_plate_flat(
                w - 0.05,
                depth - 0.06,
                0.02,
                -0.45 + t + 0.001,
                [0.0; 3],
                &mut deck_idx,
            );
            let (lw, lh, lt) = (0.70f32, 0.45f32, 0.009f32);
            let lean = 12.0f32.to_radians();
            let hinge = [0.0, -0.45 + t, -depth];
            geo.rounded_slab_leaning(lw, lh, lt, 0.03, hinge, lean, &mut body_idx);
            geo.rounded_plate_leaning(
                lw - 0.04,
                lh - 0.05,
                0.02,
                hinge,
                lean,
                lt + 0.001,
                &mut screen_idx,
            );
            (
                [-w, -0.45 - t, -depth - lh * lean.sin() - lt],
                [w, -0.45 + t + lh * lean.cos() * 2.0, depth],
            )
        }
    };
    let mut materials = vec![
        GlbMaterial {
            indices: &body_idx,
            ..body
        },
        GlbMaterial {
            indices: &screen_idx,
            ..screen
        },
    ];
    if !deck_idx.is_empty() {
        materials.push(GlbMaterial {
            slot: "Deck",
            base_color: [0.22, 0.23, 0.26, 1.0],
            metallic: 0.5,
            roughness: 0.5,
            indices: &deck_idx,
        });
    }
    geo.build(&materials, min, max)
}

/// One material slot of a generated model: its factors and the indices
/// (into the shared vertex list) that wear it.
struct GlbMaterial<'a> {
    slot: &'a str,
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    indices: &'a [u16],
}

/// Vertices for a generated model — positions, flat normals, face uvs —
/// and the `.glb` they become with one primitive per material.
#[derive(Default)]
struct GlbGeometry {
    positions: Vec<f32>,
    normals: Vec<f32>,
    uvs: Vec<f32>,
}

impl GlbGeometry {
    /// Points of a rounded rectangle (half-extents `hw`, `hh`, corner
    /// radius `r`) counter-clockwise in the XY plane, 8 segments a corner.
    fn rounded_outline(hw: f32, hh: f32, r: f32) -> Vec<[f32; 2]> {
        let r = r.min(hw).min(hh).max(0.0);
        let segments = 8;
        let corners = [
            ([hw - r, hh - r], 0.0f32),
            ([-hw + r, hh - r], 90.0),
            ([-hw + r, -hh + r], 180.0),
            ([hw - r, -hh + r], 270.0),
        ];
        let mut out = Vec::with_capacity(4 * (segments + 1));
        for (centre, start) in corners {
            for i in 0..=segments {
                let a = (start + 90.0 * i as f32 / segments as f32).to_radians();
                out.push([centre[0] + r * a.cos(), centre[1] + r * a.sin()]);
            }
        }
        out
    }

    fn push_vertex(&mut self, p: [f32; 3], n: [f32; 3], uv: [f32; 2]) -> u16 {
        let index = (self.positions.len() / 3) as u16;
        self.positions.extend_from_slice(&p);
        self.normals.extend_from_slice(&n);
        self.uvs.extend_from_slice(&uv);
        index
    }

    /// A rounded-rectangle face at `z` facing ±Z, uv over its box; a fan
    /// round the centre (the outline is convex).
    #[allow(clippy::too_many_arguments)]
    fn face(
        &mut self,
        outline: &[[f32; 2]],
        z: f32,
        front: bool,
        place: &dyn Fn([f32; 3]) -> [f32; 3],
        normal: [f32; 3],
        hw: f32,
        hh: f32,
        into: &mut Vec<u16>,
    ) {
        let centre = self.push_vertex(place([0.0, 0.0, z]), normal, [0.5, 0.5]);
        let ring: Vec<u16> = outline
            .iter()
            .map(|p| {
                self.push_vertex(
                    place([p[0], p[1], z]),
                    normal,
                    [(p[0] / hw + 1.0) / 2.0, 1.0 - (p[1] / hh + 1.0) / 2.0],
                )
            })
            .collect();
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if front {
                into.extend_from_slice(&[centre, a, b]);
            } else {
                into.extend_from_slice(&[centre, b, a]);
            }
        }
    }

    /// The side band between the front and back outlines.
    #[allow(clippy::too_many_arguments)]
    fn band(
        &mut self,
        outline: &[[f32; 2]],
        front_z: f32,
        back_z: f32,
        place: &dyn Fn([f32; 3]) -> [f32; 3],
        turn_normal: &dyn Fn([f32; 3]) -> [f32; 3],
        into: &mut Vec<u16>,
    ) {
        let n = outline.len();
        for i in 0..n {
            let (p, q) = (outline[i], outline[(i + 1) % n]);
            let edge = [q[0] - p[0], q[1] - p[1]];
            let len = (edge[0] * edge[0] + edge[1] * edge[1]).sqrt().max(1e-6);
            let normal = turn_normal([edge[1] / len, -edge[0] / len, 0.0]);
            let a = self.push_vertex(place([p[0], p[1], front_z]), normal, [0.0, 0.0]);
            let b = self.push_vertex(place([q[0], q[1], front_z]), normal, [1.0, 0.0]);
            let c = self.push_vertex(place([q[0], q[1], back_z]), normal, [1.0, 1.0]);
            let d = self.push_vertex(place([p[0], p[1], back_z]), normal, [0.0, 1.0]);
            into.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    /// A rounded slab standing in XY (its thin axis Z), centred at `at`.
    #[allow(clippy::too_many_arguments)]
    fn rounded_slab(
        &mut self,
        hw: f32,
        hh: f32,
        hd: f32,
        r: f32,
        at: [f32; 3],
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hh, r);
        let place = |p: [f32; 3]| [p[0] + at[0], p[1] + at[1], p[2] + at[2]];
        let same = |n: [f32; 3]| n;
        self.face(&outline, hd, true, &place, [0.0, 0.0, 1.0], hw, hh, into);
        self.face(&outline, -hd, false, &place, [0.0, 0.0, -1.0], hw, hh, into);
        self.band(&outline, hd, -hd, &place, &same, into);
    }

    /// A thin rounded plate facing +Z at `z`, centred at `at` in XY.
    #[allow(clippy::too_many_arguments)]
    fn rounded_plate(
        &mut self,
        hw: f32,
        hh: f32,
        r: f32,
        z: f32,
        at: [f32; 3],
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hh, r);
        let place = |p: [f32; 3]| [p[0] + at[0], p[1] + at[1], p[2] + at[2]];
        self.face(&outline, z, true, &place, [0.0, 0.0, 1.0], hw, hh, into);
    }

    /// A rounded slab lying flat (its thin axis Y): XY of the outline maps
    /// to XZ of the world, `y` up.
    #[allow(clippy::too_many_arguments)]
    fn rounded_slab_flat(
        &mut self,
        hw: f32,
        hdepth: f32,
        ht: f32,
        r: f32,
        at: [f32; 3],
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hdepth, r);
        // outline (x, y) -> world (x, -, -y): +y of the outline is the back.
        let place = |p: [f32; 3]| [p[0] + at[0], p[2] + at[1], -p[1] + at[2]];
        let turn = |n: [f32; 3]| [n[0], n[2], -n[1]];
        self.face(
            &outline,
            ht,
            true,
            &place,
            [0.0, 1.0, 0.0],
            hw,
            hdepth,
            into,
        );
        self.face(
            &outline,
            -ht,
            false,
            &place,
            [0.0, -1.0, 0.0],
            hw,
            hdepth,
            into,
        );
        self.band(&outline, ht, -ht, &place, &turn, into);
    }
    #[allow(clippy::too_many_arguments)]
    fn rounded_plate_flat(
        &mut self,
        hw: f32,
        hdepth: f32,
        r: f32,
        y: f32,
        at: [f32; 3],
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hdepth, r);
        let place = |p: [f32; 3]| [p[0] + at[0], y + at[1], -p[1] + at[2]];
        self.face(
            &outline,
            0.0,
            true,
            &place,
            [0.0, 1.0, 0.0],
            hw,
            hdepth,
            into,
        );
    }

    /// A rounded slab hinged at `hinge` (its bottom edge), standing up and
    /// leaning back by `lean` radians about the X axis, its front facing +Z.
    #[allow(clippy::too_many_arguments)]
    fn rounded_slab_leaning(
        &mut self,
        hw: f32,
        hh: f32,
        ht: f32,
        r: f32,
        hinge: [f32; 3],
        lean: f32,
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hh, r);
        let (c, s) = (lean.cos(), lean.sin());
        // Local (x, y, z) with y up from the hinge: rotate about X by -lean
        // (top goes back, toward -Z), then translate to the hinge.
        let place = move |p: [f32; 3]| {
            let y = p[1] + hh;
            [
                p[0] + hinge[0],
                y * c - p[2] * s + hinge[1],
                -(y * s) + p[2] * c + hinge[2],
            ]
        };
        let turn = move |n: [f32; 3]| [n[0], n[1] * c - n[2] * s, -(n[1] * s) + n[2] * c];
        self.face(
            &outline,
            ht,
            true,
            &place,
            turn([0.0, 0.0, 1.0]),
            hw,
            hh,
            into,
        );
        self.face(
            &outline,
            -ht,
            false,
            &place,
            turn([0.0, 0.0, -1.0]),
            hw,
            hh,
            into,
        );
        self.band(&outline, ht, -ht, &place, &turn, into);
    }
    #[allow(clippy::too_many_arguments)]
    fn rounded_plate_leaning(
        &mut self,
        hw: f32,
        hh: f32,
        r: f32,
        hinge: [f32; 3],
        lean: f32,
        z: f32,
        into: &mut Vec<u16>,
    ) {
        let outline = Self::rounded_outline(hw, hh, r);
        let (c, s) = (lean.cos(), lean.sin());
        let place = move |p: [f32; 3]| {
            let y = p[1] + hh;
            [
                p[0] + hinge[0],
                y * c - p[2] * s + hinge[1],
                -(y * s) + p[2] * c + hinge[2],
            ]
        };
        let normal = [0.0, s, c];
        self.face(&outline, z, true, &place, normal, hw, hh, into);
    }

    /// Four corners counter-clockwise seen from outside, as two triangles.
    fn quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], into: &mut Vec<u16>) {
        let base = (self.positions.len() / 3) as u16;
        for (k, c) in corners.iter().enumerate() {
            self.positions.extend_from_slice(c);
            self.normals.extend_from_slice(&normal);
            self.uvs
                .extend_from_slice(&[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]][k]);
        }
        into.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn build(&self, materials: &[GlbMaterial<'_>], min: [f32; 3], max: [f32; 3]) -> Vec<u8> {
        self.build_with_turn(materials, min, max, None)
    }

    /// As `build`, with an animation named `name` that turns the node
    /// about Y from rest to `degrees` over `seconds` (linear), when given.
    fn build_with_turn(
        &self,
        materials: &[GlbMaterial<'_>],
        min: [f32; 3],
        max: [f32; 3],
        turn: Option<(&str, f32, f32)>,
    ) -> Vec<u8> {
        let mut bin: Vec<u8> = Vec::new();
        let mut views = Vec::new();
        let mut push = |bytes: &[u8], target: u32| -> usize {
            let offset = bin.len();
            bin.extend_from_slice(bytes);
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            views.push(if target == 0 {
                format!(
                    r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{}}}"#,
                    bytes.len()
                )
            } else {
                format!(
                    r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{},"target":{target}}}"#,
                    bytes.len()
                )
            });
            views.len() - 1
        };
        let f32s = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let vp = push(&f32s(&self.positions), 34962);
        let vn = push(&f32s(&self.normals), 34962);
        let vt = push(&f32s(&self.uvs), 34962);
        let vertex_count = self.positions.len() / 3;
        let mut accessors = vec![
            format!(
                r#"{{"bufferView":{vp},"componentType":5126,"count":{vertex_count},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}}"#,
                min[0], min[1], min[2], max[0], max[1], max[2]
            ),
            format!(
                r#"{{"bufferView":{vn},"componentType":5126,"count":{vertex_count},"type":"VEC3"}}"#
            ),
            format!(
                r#"{{"bufferView":{vt},"componentType":5126,"count":{vertex_count},"type":"VEC2"}}"#
            ),
        ];
        let mut material_json = Vec::new();
        let mut primitives = Vec::new();
        for (i, m) in materials.iter().enumerate() {
            let vi = push(
                &m.indices
                    .iter()
                    .flat_map(|x| x.to_le_bytes())
                    .collect::<Vec<u8>>(),
                34963,
            );
            accessors.push(format!(
                r#"{{"bufferView":{vi},"componentType":5123,"count":{},"type":"SCALAR"}}"#,
                m.indices.len()
            ));
            let accessor = accessors.len() - 1;
            material_json.push(format!(
                r#"{{"name":"{}","pbrMetallicRoughness":{{"baseColorFactor":[{},{},{},{}],"metallicFactor":{},"roughnessFactor":{}}}}}"#,
                m.slot, m.base_color[0], m.base_color[1], m.base_color[2], m.base_color[3], m.metallic, m.roughness
            ));
            primitives.push(format!(
                r#"{{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":{accessor},"material":{i}}}"#
            ));
        }
        let mut animations = String::new();
        if let Some((name, seconds, degrees)) = turn {
            let half = degrees.to_radians() / 2.0;
            let times = [0.0f32, seconds];
            let rotations = [[0.0f32, 0.0, 0.0, 1.0], [0.0, half.sin(), 0.0, half.cos()]];
            let vt = push(&f32s(&times), 0);
            let vr = push(
                &f32s(&rotations.iter().flatten().copied().collect::<Vec<f32>>()),
                0,
            );
            accessors.push(format!(
                r#"{{"bufferView":{vt},"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[{seconds}]}}"#
            ));
            let at = accessors.len() - 1;
            accessors.push(format!(
                r#"{{"bufferView":{vr},"componentType":5126,"count":2,"type":"VEC4"}}"#
            ));
            let ar = accessors.len() - 1;
            animations = format!(
                r#","animations":[{{"name":"{name}","samplers":[{{"input":{at},"output":{ar},"interpolation":"LINEAR"}}],"channels":[{{"sampler":0,"target":{{"node":0,"path":"rotation"}}}}]}}]"#
            );
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0","generator":"promo-engine"}},
"buffers":[{{"byteLength":{}}}],
"bufferViews":[{}],
"accessors":[{}],
"materials":[{}],
"meshes":[{{"name":"generated","primitives":[{}]}}],
"nodes":[{{"mesh":0,"name":"generated"}}],
"scenes":[{{"nodes":[0]}}],"scene":0{}}}"#,
            bin.len(),
            views.join(","),
            accessors.join(","),
            material_json.join(","),
            primitives.join(","),
            animations,
        );
        let mut json_bytes = json.into_bytes();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
        out.extend_from_slice(&bin);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_cube_decodes_to_what_it_says() {
        let model = Model::from_glb(&sample_cube_glb()).expect("decodes");
        assert_eq!(model.meshes.len(), 1);
        let mesh = &model.meshes[0];
        assert_eq!(mesh.positions.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(mesh.normals.len(), 24);
        assert_eq!(mesh.uvs.len(), 24);
        assert_eq!(model.slot_names(), vec!["Body"]);
        let body = &model.materials[mesh.material];
        assert!((body.base_color[0] - 0.8).abs() < 1e-6 && (body.roughness - 0.6).abs() < 1e-6);
        assert!((model.bounds_radius - (3.0f32).sqrt() * 0.5).abs() < 1e-4);
        assert!(model.bounds_center.iter().all(|c| c.abs() < 1e-6));
        // The +Z face's normal survived the (identity) world transform.
        assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_bare_mesh_gets_flat_normals_and_a_missing_material_the_default() {
        // Strip NORMAL and the material from the cube by editing its JSON.
        let glb = sample_cube_glb();
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let json = std::str::from_utf8(&glb[20..20 + json_len]).unwrap();
        let edited = json
            .replace(r#""NORMAL":1,"#, "")
            .replace(r#","material":0"#, "");
        let mut bytes = glb[..12].to_vec();
        let mut padded = edited.into_bytes();
        while !padded.len().is_multiple_of(4) {
            padded.push(b' ');
        }
        bytes.extend_from_slice(&(padded.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&glb[16..20]);
        bytes.extend_from_slice(&padded);
        bytes.extend_from_slice(&glb[20 + json_len..]);
        let total = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&total.to_le_bytes());
        let model = Model::from_glb(&bytes).expect("decodes without normals");
        let mesh = &model.meshes[0];
        assert_eq!(
            mesh.normals[0],
            [0.0, 0.0, 1.0],
            "flat normal of the +Z face"
        );
        assert_eq!(
            mesh.material,
            model.materials.len() - 1,
            "the default material"
        );
        assert!(model.materials[mesh.material].name.is_empty());
    }

    #[test]
    fn the_slab_has_a_body_and_a_screen() {
        let model = Model::from_glb(&sample_slab_glb()).expect("decodes");
        assert_eq!(model.slot_names(), vec!["Body", "Screen"]);
        assert_eq!(model.meshes.len(), 2, "one primitive per material");
        let screen = &model.meshes[1];
        assert_eq!(screen.indices.len(), 6);
        // Primitives share one vertex list; the screen's own corners are the
        // ones its indices name.
        assert!(
            screen
                .indices
                .iter()
                .all(|&i| screen.positions[i as usize][2] > 0.03),
            "the screen sits in front of the face"
        );
        assert!(
            (model.bounds_radius - 0.7554).abs() < 0.01,
            "radius {}",
            model.bounds_radius
        );
    }

    /// The turning cube's clip samples: identity at 0, a quarter turn at
    /// 1 s, half-way between at 0.5 s, and the pose loops past the end.
    #[test]
    fn a_clip_poses_the_node_over_time() {
        let model = Model::from_glb(&sample_turning_cube_glb()).expect("decodes");
        assert_eq!(model.clip_summary(), vec![("Turn".to_string(), 1.0)]);
        let at = |t: f64| model.pose("Turn", t, false)[0];
        let x_axis = |m: Mat4| [m[0][0], m[0][1], m[0][2]];
        assert!((x_axis(at(0.0))[0] - 1.0).abs() < 1e-5, "rest: x stays x");
        let quarter = x_axis(at(1.0));
        assert!(
            quarter[0].abs() < 1e-4 && (quarter[2] + 1.0).abs() < 1e-4,
            "a quarter turn about Y: {quarter:?}"
        );
        let half = x_axis(at(0.5));
        assert!(
            (half[0] - half[2].abs()).abs() < 1e-3,
            "half-way is 45°: {half:?}"
        );
        assert_eq!(at(1.25), at(1.0), "a keyed time past the end clamps");
        assert_eq!(
            model.pose("Turn", 1.25, true),
            model.pose("Turn", 0.25, true),
            "a running clip loops past the end"
        );
        assert_eq!(
            model.pose("nope", 0.7, false),
            model.rest_matrices(),
            "an unknown clip is the rest pose"
        );
    }

    /// Every device body decodes with a Body and a Screen (the laptop a
    /// Deck too), its screen in front of its body where a picture shows,
    /// and its bounds the size a camera distance expects.
    #[test]
    fn the_device_bodies_have_screens() {
        for kind in DeviceKind::ALL {
            let model = Model::from_glb(&device_glb(kind))
                .unwrap_or_else(|e| panic!("{}: {e}", kind.name()));
            let slots = model.slot_names();
            assert!(
                slots.contains(&"Body") && slots.contains(&"Screen"),
                "{}: {slots:?}",
                kind.name()
            );
            if kind == DeviceKind::Laptop {
                assert!(slots.contains(&"Deck"));
            }
            let screen = model
                .meshes
                .iter()
                .find(|m| model.materials[m.material].name == "Screen")
                .expect("screen mesh");
            assert!(
                screen.indices.len() >= 3 * 36,
                "{}: a rounded screen fan",
                kind.name()
            );
            assert!(
                model.bounds_radius > 0.3 && model.bounds_radius < 1.2,
                "{}: {}",
                kind.name(),
                model.bounds_radius
            );
            // The screen's centre is in front of the body's centre (for the
            // laptop: the lid's screen faces +Z above the base).
            let z: f32 = screen
                .indices
                .iter()
                .map(|&i| screen.positions[i as usize][2])
                .sum::<f32>()
                / screen.indices.len() as f32;
            if kind != DeviceKind::Laptop {
                assert!(z > 0.0, "{}: screen at z {z}", kind.name());
            }
        }
    }

    #[test]
    fn garbage_is_a_decode_error_not_a_panic() {
        assert!(matches!(
            Model::from_glb(b"not a model"),
            Err(ModelError::Decode(_))
        ));
    }
}
