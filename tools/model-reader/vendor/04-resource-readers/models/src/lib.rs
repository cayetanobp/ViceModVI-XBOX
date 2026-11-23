//! Model reader: walks the drawables identified by phase 03 (`.#dr`
//! dictionaries and objects), decodes their compressed vertex blocks and
//! meshlet index blocks, and builds triangle meshes ready for glTF export.
//!
//! Every layout below was deduced from the samples; none comes from a
//! specification. The drawable, LOD and model objects follow the same field
//! order as the drawables of earlier RAGE titles, which is why they carry
//! those names.

pub mod glb;

use rpf9_identify::resource::{self, physical_offset, u16_at, u32_at, u64_at, virtual_offset, Kind};

/// A GPU buffer in the physical section: `[offset, offset + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    pub offset: usize,
    pub len: usize,
}

/// Buffer object: zero first field, size in 4-byte words at `+0x08`, the
/// value 4 as u16 at `+0x0c` and the physical data pointer at `+0x18`.
fn buffer_object(virt: &[u8], at: usize, physical_len: usize) -> Option<Buffer> {
    if u64_at(virt, at)? != 0 || u16_at(virt, at + 0x0c)? != 4 {
        return None;
    }
    let len = u32_at(virt, at + 0x08)? as usize * 4;
    let offset = physical_offset(u64_at(virt, at + 0x18)?)? as usize;
    (offset + len <= physical_len).then_some(Buffer { offset, len })
}

fn vptr(virt: &[u8], at: usize) -> Option<usize> {
    virtual_offset(u64_at(virt, at)?, virt.len())
}

#[derive(Debug, Clone)]
pub struct GeometryRef {
    /// Offset of the geometry object in the virtual section.
    pub at: usize,
    pub vertices: Buffer,
    pub indices: Buffer,
}

/// Geometry object: vertex buffer object pointer at `+0x08` and, at
/// `+0x40`, a pointer to a structure whose first field points to the index
/// buffer object.
fn geometry_ref(virt: &[u8], at: usize, physical_len: usize) -> Option<GeometryRef> {
    if u64_at(virt, at)? != 0 {
        return None;
    }
    let vertices = buffer_object(virt, vptr(virt, at + 0x08)?, physical_len)?;
    let indices = buffer_object(virt, vptr(virt, vptr(virt, at + 0x40)?)?, physical_len)?;
    Some(GeometryRef { at, vertices, indices })
}

#[derive(Debug, Clone)]
pub struct Model {
    pub at: usize,
    pub geometries: Vec<GeometryRef>,
    /// Shader index of each geometry.
    pub shaders: Vec<u16>,
    /// Bounding boxes (min, max) from the array at `+0x18`. With one
    /// geometry it holds that geometry's box; with several, entry 0 is the
    /// whole model and entry `k + 1` geometry `k`.
    pub bounds: Vec<[f32; 6]>,
}

impl Model {
    pub fn geometry_bounds(&self, k: usize) -> Option<[f32; 6]> {
        let i = if self.geometries.len() == 1 { 0 } else { k + 1 };
        self.bounds.get(i).copied()
    }
}

/// Model object: geometry pointer array at `+0x08` with its u16 count at
/// `+0x10`, bounds array at `+0x18` and shader index array (u16 per
/// geometry) at `+0x20`.
fn model(virt: &[u8], at: usize, physical_len: usize) -> Result<Model, String> {
    let err = |what: &str| format!("model at {at:#x}: {what}");
    let array = vptr(virt, at + 0x08).ok_or_else(|| err("no geometry array"))?;
    let count = u16_at(virt, at + 0x10).ok_or_else(|| err("truncated"))? as usize;
    let mut geometries = Vec::with_capacity(count);
    for k in 0..count {
        let g = vptr(virt, array + 8 * k).ok_or_else(|| err("geometry pointer outside the section"))?;
        geometries.push(geometry_ref(virt, g, physical_len).ok_or_else(|| err("geometry object not recognised"))?);
    }
    let shaders = match vptr(virt, at + 0x20) {
        Some(p) => (0..count).map(|k| u16_at(virt, p + 2 * k)).collect::<Option<_>>().ok_or_else(|| err("shader array truncated"))?,
        None => vec![0; count],
    };
    let entries = if count > 1 { count + 1 } else { count };
    let bounds = vptr(virt, at + 0x18).map_or(Vec::new(), |p| {
        (0..entries).map_while(|e| {
            let mut b = [0f32; 6];
            for (i, slot) in [0, 1, 2, 4, 5, 6].iter().enumerate() {
                b[i] = f32::from_le_bytes(virt.get(p + 32 * e + 4 * slot..p + 32 * e + 4 * slot + 4)?.try_into().ok()?);
            }
            Some(b)
        }).collect()
    });
    Ok(Model { at, geometries, shaders, bounds })
}

#[derive(Debug, Clone)]
pub struct Drawable {
    pub at: usize,
    pub name: Option<String>,
    /// A skeleton pointer is present at `+0x18`. Skinning is not exported.
    pub skeleton: bool,
    /// Models of each level of detail, from the highest.
    pub lods: [Vec<Model>; 4],
    /// LOD switch distances at `+0x70`.
    pub lod_distances: [f32; 4],
}

/// Drawable object: LOD model collections at `+0x50`, `+0x58`, `+0x60`
/// and `+0x68` (pointer array and u16 count), distances at `+0x70` and name
/// pointer at `+0xa8`.
fn drawable(virt: &[u8], at: usize, physical_len: usize) -> Result<Drawable, String> {
    let mut lods: [Vec<Model>; 4] = Default::default();
    for (level, models) in lods.iter_mut().enumerate() {
        let Some(collection) = vptr(virt, at + 0x50 + 8 * level) else { continue };
        let array = vptr(virt, collection).ok_or_else(|| format!("LOD {level} at {collection:#x}: no model array"))?;
        let count = u16_at(virt, collection + 8).ok_or("truncated LOD collection")? as usize;
        for m in 0..count {
            let mo = vptr(virt, array + 8 * m).ok_or_else(|| format!("LOD {level}: model pointer outside the section"))?;
            models.push(model(virt, mo, physical_len)?);
        }
    }
    let mut lod_distances = [0f32; 4];
    for (i, d) in lod_distances.iter_mut().enumerate() {
        *d = f32::from_bits(u32_at(virt, at + 0x70 + 4 * i).unwrap_or(0));
    }
    let name = resource::named_object(virt, at).map(|(n, _)| n);
    Ok(Drawable { at, name, skeleton: vptr(virt, at + 0x18).is_some(), lods, lod_distances })
}

/// Drawables of a resource classified by phase 03: every element of a
/// named dictionary, or the root object itself.
pub fn drawables(virt: &[u8], kind: &Kind, physical_len: usize) -> Result<Vec<Drawable>, String> {
    let roots = match kind {
        Kind::NamedDictionary { .. } => resource::dictionary(virt).ok_or("dictionary no longer parses")?.elements,
        Kind::Named { .. } => vec![0],
        _ => return Err("not a drawable resource".into()),
    };
    roots.into_iter().map(|at| drawable(virt, at, physical_len)).collect()
}

/// One attribute stream of a vertex block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub slot: usize,
    pub kind: u8,
    /// Offset from the start of the block.
    pub offset: usize,
    pub element_bytes: usize,
}

pub const VERTEX_MAGIC: &[u8; 4] = b"B@\0\0";
const SLOTS: usize = 22;
const POSITIONS_AT: usize = 0x80;
/// Component order of the packed positions for each value of the axis
/// field at `0x6c`: the third component, stored with 16 bits, is the axis
/// that field names.
const AXES: [[usize; 3]; 3] = [[2, 1, 0], [2, 0, 1], [0, 1, 2]];
const POSITION_BITS: [u32; 3] = [24, 24, 16];

/// Header of a vertex block, as observed: magic `B@\0\0`, 22 attribute
/// slots (type in the low byte, offset in 2-byte units above it), the
/// quantisation minimum at `0x60`, the 16-bit axis at `0x6c`, the
/// quantisation steps at `0x70` and the texture coordinate range at `0x7c`.
/// Positions start at `0x80`, 8 bytes per vertex; the streams follow, each
/// with `count` elements of 4, 8 or 16 bytes (bits 5 to 7 of the type).
#[derive(Debug, Clone)]
pub struct VertexBlock {
    pub count: usize,
    pub axis: u32,
    pub min: [f32; 3],
    pub step: [f32; 3],
    pub uv_range: f32,
    pub streams: Vec<Stream>,
}

fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    u32_at(b, at).map(f32::from_bits)
}

pub fn vertex_block(b: &[u8]) -> Result<VertexBlock, String> {
    if b.get(..4) != Some(VERTEX_MAGIC) || b.len() < POSITIONS_AT {
        return Err("vertex block does not start with B@".into());
    }
    let mut streams = Vec::new();
    for slot in 0..SLOTS {
        let v = u32_at(b, 4 + 4 * slot).unwrap();
        if v == 0 {
            continue;
        }
        let kind = (v & 0xff) as u8;
        let element_bytes = match kind >> 5 {
            1 => 4,
            2 => 8,
            4 => 16,
            _ => return Err(format!("slot {slot}: type {kind:#x} has no known element size")),
        };
        streams.push(Stream { slot, kind, offset: (v >> 8) as usize * 2, element_bytes });
    }
    streams.sort_by_key(|s| s.offset);
    let first = streams.first().ok_or("vertex block without attribute streams")?.offset;
    if first < POSITIONS_AT || !(first - POSITIONS_AT).is_multiple_of(8) {
        return Err(format!("first stream at {first:#x} does not follow whole 8-byte positions"));
    }
    let count = (first - POSITIONS_AT) / 8;
    let mut cursor = first;
    for s in &streams {
        if s.offset != cursor {
            return Err(format!("slot {} starts at {:#x}, expected {cursor:#x}", s.slot, s.offset));
        }
        cursor += s.element_bytes * count;
    }
    if cursor > b.len() {
        return Err(format!("streams end at {cursor:#x}, block has {:#x} bytes", b.len()));
    }
    let axis = u32_at(b, 0x6c).unwrap();
    if axis > 2 {
        return Err(format!("axis field {axis} not observed"));
    }
    let f = |at| f32_at(b, at).unwrap();
    Ok(VertexBlock {
        count, axis,
        min: [f(0x60), f(0x64), f(0x68)],
        step: [f(0x70), f(0x74), f(0x78)],
        uv_range: f(0x7c),
        streams,
    })
}

/// One normal word: 10-bit octahedral fields when bit 31 is set, 8-bit
/// fields otherwise.
pub fn decode_normal(w: u32) -> [f32; 3] {
    let (bits, max) = if w >> 31 == 1 { (10, 1023.0) } else { (8, 255.0) };
    let mask = (1u32 << bits) - 1;
    octahedral((w & mask) as f32 / max, ((w >> bits) & mask) as f32 / max)
}

/// Octahedral decoding of two values in `[0, 1]`.
pub fn octahedral(u: f32, v: f32) -> [f32; 3] {
    let (mut x, mut y) = (u * 2.0 - 1.0, v * 2.0 - 1.0);
    let z = 1.0 - x.abs() - y.abs();
    let t = (-z).max(0.0);
    x += if x >= 0.0 { -t } else { t };
    y += if y >= 0.0 { -t } else { t };
    let len = (x * x + y * y + z * z).sqrt();
    if len == 0.0 { [0.0, 0.0, 1.0] } else { [x / len, y / len, z / len] }
}

impl VertexBlock {
    pub fn stream(&self, slot: usize) -> Option<&Stream> {
        self.streams.iter().find(|s| s.slot == slot)
    }

    /// Each position is a u64: component 0 takes bits 16..32 as its high 16
    /// bits and bits 0..8 as its low 8; component 1 takes bits 32..48 and
    /// 8..16; component 2 is bits 48..64. Value = min + q * step.
    pub fn positions(&self, b: &[u8]) -> Vec<[f32; 3]> {
        let axes = AXES[self.axis as usize];
        (0..self.count).map(|i| {
            let raw = u64::from_le_bytes(b[POSITIONS_AT + 8 * i..][..8].try_into().unwrap());
            let q = [
                ((raw >> 16) & 0xffff) << 8 | (raw & 0xff),
                ((raw >> 32) & 0xffff) << 8 | ((raw >> 8) & 0xff),
                raw >> 48,
            ];
            let mut p = [0f32; 3];
            for c in 0..3 {
                p[axes[c]] = (f64::from(self.min[c]) + q[c] as f64 * f64::from(self.step[c])) as f32;
            }
            p
        }).collect()
    }

    /// Bounds implied by the quantisation: min and min + step * 2^bits.
    pub fn bounds(&self) -> [f32; 6] {
        let axes = AXES[self.axis as usize];
        let mut b = [0f32; 6];
        for c in 0..3 {
            b[axes[c]] = self.min[c];
            b[3 + axes[c]] = (f64::from(self.min[c]) + f64::from(self.step[c]) * f64::from(1u32 << POSITION_BITS[c])) as f32;
        }
        b
    }

    /// Slot 1 (type 0x21): octahedral normal whose precision depends on
    /// bit 31 of each word. With bit 31 set, two 10-bit fields at bits 0 and
    /// 10; clear, two 8-bit fields at bits 0 and 8. The remaining bits
    /// (probably the tangent frame) are not decoded.
    pub fn normals(&self, b: &[u8]) -> Option<Vec<[f32; 3]>> {
        let s = self.stream(1).filter(|s| s.kind == 0x21)?;
        Some((0..self.count).map(|i| decode_normal(u32_at(b, s.offset + 4 * i).unwrap())).collect())
    }

    /// Texture coordinates in slot 2 (set 0) or 3 (set 1): two i16 scaled
    /// to the range at `0x7c` (type 0x28) or two f32 (type 0x42).
    pub fn uvs(&self, b: &[u8], slot: usize) -> Option<Vec<[f32; 2]>> {
        let s = self.stream(slot)?;
        match s.kind {
            0x28 => Some((0..self.count).map(|i| {
                let at = s.offset + 4 * i;
                let c = |k: usize| f32::from(i16::from_le_bytes([b[at + k], b[at + k + 1]])) / 32768.0 * self.uv_range;
                [c(0), c(2)]
            }).collect()),
            0x42 => Some((0..self.count).map(|i| {
                let at = s.offset + 8 * i;
                [f32_at(b, at).unwrap(), f32_at(b, at + 4).unwrap()]
            }).collect()),
            _ => None,
        }
    }
}

/// Index block: header `0x8000_0000 | meshlets`, triangle count and vertex
/// reference count; one 16-byte record per meshlet whose second word holds
/// its first vertex reference (low 16 bits) and first triangle (high 16
/// bits); one u32 per triangle with three 7-bit local indices; and the u16
/// vertex references that turn local indices into vertex indices. A flagged
/// block adds a u16 bone palette entry per meshlet after the references.
#[derive(Debug, Clone)]
pub struct IndexBlock {
    pub meshlets: usize,
    pub triangles: Vec<[u32; 3]>,
    pub references: usize,
    /// Bytes after the vertex references, before the end of the buffer.
    pub slack: usize,
}

pub fn index_block(b: &[u8], vertex_count: usize) -> Result<IndexBlock, String> {
    let word = |at: usize| u32_at(b, at).ok_or_else(|| format!("index block truncated at {at:#x}"));
    let head = word(0)?;
    if head >> 31 != 1 {
        return Err("index block header lacks the 0x80000000 flag".into());
    }
    let has_palette = head & 0x4000_0000 != 0;
    let count_mask = if has_palette { 0xffff } else { 0x7fff_ffff };
    let meshlets = (head & count_mask) as usize;
    let (tris, refs) = (word(4)? as usize, word(8)? as usize);
    let tri_at = 12 + 16 * meshlets;
    let refs_at = tri_at + 4 * tris;
    let palette_at = refs_at + 2 * refs;
    let end = palette_at + if has_palette { 2 * meshlets } else { 0 };
    if end > b.len() {
        return Err(format!("index block needs {end:#x} bytes, buffer has {:#x}", b.len()));
    }
    if has_palette {
        let binding_hint = (head >> 16) & 0x1fff;
        let constant_binding = head & 0x2000_0000 != 0;
        if end.next_multiple_of(16) != b.len() || b[end..].iter().any(|&byte| byte != 0) ||
            (!constant_binding && binding_hint != 0) ||
            (constant_binding && (binding_hint % 2 != 0 || b[palette_at..end].chunks_exact(2)
                .any(|entry| u16::from_le_bytes([entry[0], entry[1]]) as u32 != binding_hint / 2)))
        {
            return Err("index block has an invalid bone palette".into());
        }
    }
    let mut first_ref = Vec::with_capacity(meshlets + 1);
    let mut first_tri = Vec::with_capacity(meshlets + 1);
    for m in 0..meshlets {
        let w = word(12 + 16 * m + 4)?;
        first_ref.push((w & 0xffff) as usize);
        first_tri.push((w >> 16) as usize);
    }
    first_ref.push(refs);
    first_tri.push(tris);
    let increasing = |v: &[usize]| v.first() == Some(&0) && v.windows(2).all(|w| w[0] <= w[1]);
    if meshlets == 0 || !increasing(&first_ref) || !increasing(&first_tri) {
        return Err("meshlet offsets are not increasing from 0".into());
    }
    let references: Vec<u32> = (0..refs).map(|k| u32::from(u16_at(b, refs_at + 2 * k).unwrap())).collect();
    if let Some(&bad) = references.iter().find(|&&r| r as usize >= vertex_count) {
        return Err(format!("vertex reference {bad} past {vertex_count} vertices"));
    }
    let mut triangles = Vec::with_capacity(tris);
    for m in 0..meshlets {
        let local = first_ref[m + 1] - first_ref[m];
        for t in first_tri[m]..first_tri[m + 1] {
            let w = word(tri_at + 4 * t)?;
            let corner = |s: u32| ((w >> s) & 0x7f) as usize;
            let mut tri = [0u32; 3];
            for (k, s) in [0, 7, 14].into_iter().enumerate() {
                let l = corner(s);
                if l >= local {
                    return Err(format!("meshlet {m} triangle {t}: local index {l} past {local} vertices"));
                }
                tri[k] = references[first_ref[m] + l];
            }
            triangles.push(tri);
        }
    }
    Ok(IndexBlock { meshlets, triangles, references: refs, slack: b.len() - end })
}

/// A decoded geometry with the checks of the functional test.
#[derive(Debug, Clone)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uv0: Option<Vec<[f32; 2]>>,
    pub uv1: Option<Vec<[f32; 2]>>,
    pub triangles: Vec<[u32; 3]>,
    pub shader: u16,
    pub meshlets: usize,
    /// Every vertex is referenced by at least one triangle.
    pub all_vertices_used: bool,
    /// The quantisation bounds equal the model's box for this geometry
    /// (within 1e-3), when the model has one.
    pub bounds_match: Option<bool>,
    /// Comparison of the decoded normals with normals rebuilt from the
    /// triangles. `None` when no vertex has a rebuilt normal.
    pub normal_agreement: Option<NormalAgreement>,
}

pub fn decode_geometry(physical: &[u8], model: &Model, k: usize) -> Result<Mesh, String> {
    let g = &model.geometries[k];
    let vb = &physical[g.vertices.offset..g.vertices.offset + g.vertices.len];
    let ib = &physical[g.indices.offset..g.indices.offset + g.indices.len];
    let block = vertex_block(vb)?;
    let index = index_block(ib, block.count)?;
    let positions = block.positions(vb);
    let normals = block.normals(vb);
    let mut used = vec![false; block.count];
    for t in &index.triangles {
        for &v in t {
            used[v as usize] = true;
        }
    }
    let bounds_match = model.geometry_bounds(k).map(|b| {
        let q = block.bounds();
        b.iter().zip(q).all(|(&a, c)| (a - c).abs() < 1e-3)
    });
    let normal_agreement = normals.as_ref().and_then(|n| normal_agreement(&positions, &index.triangles, n));
    Ok(Mesh {
        uv0: block.uvs(vb, 2),
        uv1: block.uvs(vb, 3),
        positions, normals,
        triangles: index.triangles,
        shader: model.shaders[k],
        meshlets: index.meshlets,
        all_vertices_used: used.iter().all(|&u| u),
        bounds_match, normal_agreement,
    })
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalAgreement {
    /// Median cosine between the decoded and the rebuilt normals. Authored
    /// normals of low-polygon meshes legitimately differ from rebuilt ones.
    pub median: f32,
    /// Share of vertices whose decoded normal points to the same side of
    /// the surface as the rebuilt one (positive cosine). A wrong decoding
    /// gives about one half.
    pub same_side: f32,
}

/// Compares `normals` with area-weighted face normals summed at each
/// vertex.
pub fn normal_agreement(positions: &[[f32; 3]], triangles: &[[u32; 3]], normals: &[[f32; 3]]) -> Option<NormalAgreement> {
    let mut acc = vec![[0f32; 3]; positions.len()];
    for t in triangles {
        let [a, b, c] = t.map(|i| positions[i as usize]);
        let n = cross(sub(b, a), sub(c, a));
        for &i in t {
            for k in 0..3 {
                acc[i as usize][k] += n[k];
            }
        }
    }
    let mut cos: Vec<f32> = acc.iter().zip(normals).filter_map(|(g, n)| {
        let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        (len > 0.0).then(|| (g[0] * n[0] + g[1] * n[1] + g[2] * n[2]) / len)
    }).collect();
    if cos.is_empty() {
        return None;
    }
    cos.sort_by(f32::total_cmp);
    let same_side = cos.iter().filter(|&&c| c > 0.0).count() as f32 / cos.len() as f32;
    Some(NormalAgreement { median: cos[cos.len() / 2], same_side })
}

#[cfg(test)]
mod tests;
