//! Models: a glTF 2.0 binary decoded into what the model pass draws.
//!
//! The loader is the whole of the file's I/O: meshes flattened to world
//! space (v1 draws the default scene at rest; clips come with P1),
//! materials by the SLOT NAME the file exports — what `materials` bindings
//! in the project refer to — and the bounds the plan's camera `distance`
//! and placement are measured against. Nothing here touches the GPU; the
//! pass in `promo-gpu` takes these buffers as they are.

use std::fmt;

/// A model, decoded. World-space geometry; one entry per primitive.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    /// Centre of the geometry's bounding box and the radius of the sphere
    /// round it, in the file's units.
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
}

/// One primitive, in world space, with one material.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub material: usize,
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

type Mat4 = [[f32; 4]; 4];

const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

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

/// Normals go through the upper 3×3 (uniform scale and rotation are what
/// models carry; a non-uniform scale would want the inverse transpose,
/// which v1 does not bother with) and are re-normalised.
fn transform_normal(m: &Mat4, n: [f32; 3]) -> [f32; 3] {
    normalize([
        m[0][0] * n[0] + m[1][0] * n[1] + m[2][0] * n[2],
        m[0][1] * n[0] + m[1][1] * n[1] + m[2][1] * n[2],
        m[0][2] * n[0] + m[1][2] * n[1] + m[2][2] * n[2],
    ])
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

        let mut meshes = Vec::new();
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .ok_or(ModelError::Empty)?;
        let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, IDENTITY)).collect();
        while let Some((node, parent)) = stack.pop() {
            let world = mul(&parent, &node.transform().matrix());
            if let Some(mesh) = node.mesh() {
                for primitive in mesh.primitives() {
                    if primitive.mode() != gltf::mesh::Mode::Triangles {
                        continue;
                    }
                    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                    let Some(positions) = reader.read_positions() else {
                        continue;
                    };
                    let positions: Vec<[f32; 3]> =
                        positions.map(|p| transform_point(&world, p)).collect();
                    let indices: Vec<u32> = match reader.read_indices() {
                        Some(read) => read.into_u32().collect(),
                        None => (0..positions.len() as u32).collect(),
                    };
                    let normals: Vec<[f32; 3]> = match reader.read_normals() {
                        Some(read) => read.map(|n| transform_normal(&world, n)).collect(),
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
                    });
                }
            }
            for child in node.children() {
                stack.push((child, world));
            }
        }
        if meshes.iter().all(|m| m.positions.is_empty()) {
            return Err(ModelError::Empty);
        }

        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in meshes.iter().flat_map(|m| m.positions.iter()) {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        let center = [
            (min[0] + max[0]) / 2.0,
            (min[1] + max[1]) / 2.0,
            (min[2] + max[2]) / 2.0,
        ];
        let radius = meshes
            .iter()
            .flat_map(|m| m.positions.iter())
            .map(|p| {
                let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
            })
            .fold(0.0f32, f32::max);

        Ok(Model {
            meshes,
            materials,
            bounds_center: center,
            bounds_radius: radius.max(1e-6),
        })
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

/// A phone-like slab as a `.glb`: a body 0.72 × 1.5 × 0.08 with a `Body`
/// slot and an inset front face with a `Screen` slot — the shape a
/// `materials` binding was made for (a screenshot on the screen, the
/// accent on the body). Generated, like the cube, so nothing binary is
/// checked in; the turntable template and the app's first model use it.
pub fn sample_slab_glb() -> Vec<u8> {
    let (w, h, d) = (0.36f32, 0.75f32, 0.04f32);
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
        let mut bin: Vec<u8> = Vec::new();
        let mut views = Vec::new();
        let mut push = |bytes: &[u8], target: u32| -> usize {
            let offset = bin.len();
            bin.extend_from_slice(bytes);
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            views.push(format!(
                r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{},"target":{target}}}"#,
                bytes.len()
            ));
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
        let json = format!(
            r#"{{"asset":{{"version":"2.0","generator":"promo-engine"}},
"buffers":[{{"byteLength":{}}}],
"bufferViews":[{}],
"accessors":[{}],
"materials":[{}],
"meshes":[{{"name":"generated","primitives":[{}]}}],
"nodes":[{{"mesh":0,"name":"generated"}}],
"scenes":[{{"nodes":[0]}}],"scene":0}}"#,
            bin.len(),
            views.join(","),
            accessors.join(","),
            material_json.join(","),
            primitives.join(","),
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
                .all(|&i| screen.positions[i as usize][2] > 0.04),
            "the screen sits in front of the face"
        );
        assert!(
            (model.bounds_radius - 0.833).abs() < 0.01,
            "radius {}",
            model.bounds_radius
        );
    }

    #[test]
    fn garbage_is_a_decode_error_not_a_panic() {
        assert!(matches!(
            Model::from_glb(b"not a model"),
            Err(ModelError::Decode(_))
        ));
    }
}
