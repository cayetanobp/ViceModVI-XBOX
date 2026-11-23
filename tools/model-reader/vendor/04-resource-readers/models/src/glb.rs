//! Minimal glTF 2.0 binary (GLB) writer for decoded meshes.
//!
//! One scene with a root node per drawable. The root node carries a -90°
//! rotation about X: the source data is Z-up and glTF is Y-up; vertex data
//! is written unchanged. Each level of detail is a child node with one mesh,
//! and each geometry is a primitive whose material stands for its shader
//! index. Textures are not linked yet.

use crate::Mesh;
use std::collections::BTreeMap;
use std::fmt::Write;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
/// Rotation of -90° about X, as an (x, y, z, w) quaternion.
const Z_UP_TO_Y_UP: [f32; 4] = [-std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2];

/// A node of the scene: a level of detail with its geometries.
pub struct Lod<'a> {
    pub name: String,
    pub meshes: Vec<&'a Mesh>,
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => { let _ = write!(out, "\\u{:04x}", c as u32); }
            c => out.push(c),
        }
    }
    out
}

fn num(v: f32) -> String {
    if v.is_finite() { format!("{v:?}") } else { "0.0".into() }
}

#[derive(Default)]
struct Builder {
    bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Builder {
    fn view(&mut self, bytes: &[u8], target: u32) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views.push(format!(
            "{{\"buffer\":0,\"byteOffset\":{offset},\"byteLength\":{},\"target\":{target}}}", bytes.len()
        ));
        self.views.len() - 1
    }

    fn accessor(&mut self, view: usize, component: u32, count: usize, kind: &str, bounds: Option<(String, String)>) -> usize {
        let mut a = format!("{{\"bufferView\":{view},\"componentType\":{component},\"count\":{count},\"type\":\"{kind}\"");
        if let Some((min, max)) = bounds {
            let _ = write!(a, ",\"min\":[{min}],\"max\":[{max}]");
        }
        a.push('}');
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn floats<const N: usize>(&mut self, data: &[[f32; N]], kind: &str, with_bounds: bool) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes())).collect();
        let view = self.view(&bytes, ARRAY_BUFFER);
        let bounds = with_bounds.then(|| {
            let mut lo = [f32::INFINITY; N];
            let mut hi = [f32::NEG_INFINITY; N];
            for v in data {
                for k in 0..N {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            let join = |a: [f32; N]| a.iter().map(|&f| num(f)).collect::<Vec<_>>().join(",");
            (join(lo), join(hi))
        });
        self.accessor(view, FLOAT, data.len(), kind, bounds)
    }
}

/// Builds a GLB file for one drawable.
pub fn write(name: &str, lods: &[Lod]) -> Vec<u8> {
    let mut b = Builder::default();
    let mut materials: BTreeMap<u16, usize> = BTreeMap::new();
    for lod in lods {
        for m in &lod.meshes {
            let next = materials.len();
            materials.entry(m.shader).or_insert(next);
        }
    }
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for lod in lods {
        let mut prims = Vec::new();
        for m in &lod.meshes {
            let pos = b.floats(&m.positions, "VEC3", true);
            let mut attrs = format!("\"POSITION\":{pos}");
            if let Some(n) = &m.normals {
                let a = b.floats(n, "VEC3", false);
                let _ = write!(attrs, ",\"NORMAL\":{a}");
            }
            for (set, uv) in [&m.uv0, &m.uv1].into_iter().enumerate() {
                if let Some(uv) = uv {
                    let a = b.floats(uv, "VEC2", false);
                    let _ = write!(attrs, ",\"TEXCOORD_{set}\":{a}");
                }
            }
            let flat: Vec<u32> = m.triangles.iter().flatten().copied().collect();
            let indices = if m.positions.len() <= usize::from(u16::MAX) {
                let bytes: Vec<u8> = flat.iter().flat_map(|&i| (i as u16).to_le_bytes()).collect();
                let v = b.view(&bytes, ELEMENT_ARRAY_BUFFER);
                b.accessor(v, UNSIGNED_SHORT, flat.len(), "SCALAR", None)
            } else {
                let bytes: Vec<u8> = flat.iter().flat_map(|&i| i.to_le_bytes()).collect();
                let v = b.view(&bytes, ELEMENT_ARRAY_BUFFER);
                b.accessor(v, UNSIGNED_INT, flat.len(), "SCALAR", None)
            };
            prims.push(format!(
                "{{\"attributes\":{{{attrs}}},\"indices\":{indices},\"material\":{},\"mode\":4}}",
                materials[&m.shader]
            ));
        }
        meshes.push(format!("{{\"name\":\"{}\",\"primitives\":[{}]}}", escape(&lod.name), prims.join(",")));
        nodes.push(format!("{{\"name\":\"{}\",\"mesh\":{}}}", escape(&lod.name), meshes.len() - 1));
    }
    let children: Vec<String> = (1..=nodes.len()).map(|i| i.to_string()).collect();
    let root = format!(
        "{{\"name\":\"{}\",\"rotation\":[{}],\"children\":[{}]}}",
        escape(name), Z_UP_TO_Y_UP.map(num).join(","), children.join(",")
    );
    let materials_json: Vec<String> = materials.keys().map(|s| format!(
        "{{\"name\":\"shader_{s}\",\"pbrMetallicRoughness\":{{\"baseColorFactor\":[0.8,0.8,0.8,1.0],\"metallicFactor\":0.0,\"roughnessFactor\":0.8}}}}"
    )).collect();
    while !b.bin.len().is_multiple_of(4) {
        b.bin.push(0);
    }
    let mut json = format!(
        "{{\"asset\":{{\"version\":\"2.0\",\"generator\":\"rpf9-models\"}},\"scene\":0,\"scenes\":[{{\"nodes\":[0]}}],\
\"nodes\":[{root}{}{}],\"meshes\":[{}],\"materials\":[{}],\"accessors\":[{}],\"bufferViews\":[{}],\"buffers\":[{{\"byteLength\":{}}}]}}",
        if nodes.is_empty() { "" } else { "," }, nodes.join(","), meshes.join(","), materials_json.join(","),
        b.accessors.join(","), b.views.join(","), b.bin.len()
    );
    while !json.len().is_multiple_of(4) {
        json.push(' ');
    }
    let total = 12 + 8 + json.len() + 8 + b.bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(json.as_bytes());
    out.extend_from_slice(&(b.bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&b.bin);
    out
}
