//! Drawable and fragment resources: static geometry, materials and the
//! embedded texture dictionary.
//!
//! A drawable header (176 bytes) points to a shader group, a level-of-detail
//! list of models and a name. Each model lists geometries; a geometry has a
//! vertex buffer with a vertex layout (FVF) and a clustered index buffer.
//! A fragment wraps a drawable and adds physics data that is not read here.

use std::collections::HashSet;

use super::{
    bin::{self, Bin, GRAPHICS_BASE, SYSTEM_BASE},
    dictionary,
    model::{Material, Mesh, Model, TextureRef, Vec2, Vec3},
};
use crate::{Error, Result};

pub const MAX_SYSTEM_BYTES: i64 = 32 * 1024 * 1024;
pub const MAX_GRAPHICS_BYTES: i64 = 224 * 1024 * 1024;
const HEADER: usize = 176;
const MAX_MESHES: usize = 1024;
const MAX_TOTAL_VERTICES: u64 = 2_000_000;
const MAX_TOTAL_TRIANGLES: u64 = 2_000_000;
const MAX_MESH_VERTICES: u64 = 1_000_000;
/// Shader preset tag of supported materials.
const PRESET_META: u32 = u32::from_le_bytes(*b"atem");

/// Offsets inside the drawable header.
mod header {
    pub const PAGES: usize = 8;
    pub const SHADERS: usize = 16;
    pub const FRAGMENT_DRAWABLE: usize = 24;
    pub const RADIUS: usize = 44;
    pub const BOUNDS_MIN: usize = 48;
    pub const BOUNDS_MAX: usize = 64;
    pub const LODS: usize = 80;
    pub const NAME: usize = 168;
    pub const RENDER_LISTS: usize = 176;
}

/// Offsets inside a vertex layout (FVF).
mod layout {
    pub const OFFSETS: usize = 0;
    pub const SIZES: usize = 0xD0;
    pub const FORMATS: usize = 0x104;
    pub const FLAGS: usize = 0x138;
    pub const COLOR: usize = 24;
    pub const UV: usize = 28;
}

fn system_pointer(value: u64) -> bool {
    (SYSTEM_BASE..SYSTEM_BASE + MAX_SYSTEM_BYTES as u64).contains(&value)
}

fn finite(point: Vec3) -> bool {
    point.iter().all(|v| v.is_finite() && v.abs() < 10_000_000.0)
}

fn ordered(min: Vec3, max: Vec3) -> bool {
    finite(min) && finite(max) && (0..3).all(|axis| min[axis] <= max[axis])
}

/// Bounds slack: 2 mm plus 0.001 % of the largest extent.
fn slack(min: Vec3, max: Vec3) -> f32 {
    0.002 + (max[0] - min[0]).max((max[1] - min[1]).max(max[2] - min[2])) * 0.00001
}

fn header_drawable(data: &[u8], embedded: bool) -> bool {
    let bytes = Bin(data);
    if data.len() < HEADER || bytes.u64_or_zero(0) != 0 {
        return false;
    }
    let pages = bytes.u64_or_zero(header::PAGES);
    let name = bytes.u64_or_zero(header::NAME);
    let pages_ok = if embedded { pages == 0 } else { system_pointer(pages) };
    let pointers_ok = system_pointer(bytes.u64_or_zero(header::SHADERS)) && system_pointer(bytes.u64_or_zero(header::LODS)) && (system_pointer(name) || (embedded && name == 0));
    let (Ok(min), Ok(max), Ok(radius)) = (bytes.vec3(header::BOUNDS_MIN), bytes.vec3(header::BOUNDS_MAX), bytes.f32(header::RADIUS)) else {
        return false;
    };
    pages_ok && pointers_ok && ordered(min, max) && radius.is_finite() && radius > 0.0 && radius < 1_000_000.0
}

pub fn looks_like_drawable(prefix: &[u8]) -> bool {
    header_drawable(prefix, false)
}

pub fn looks_like_fragment(prefix: &[u8]) -> bool {
    let bytes = Bin(prefix);
    if prefix.len() < HEADER || bytes.u64_or_zero(0) != 0 {
        return false;
    }
    let pointers = [8, 16, 24, 32, 40, 168].iter().all(|&at| system_pointer(bytes.u64_or_zero(at)));
    let (a, b) = (bytes.u16_or_zero(48), bytes.u16_or_zero(52));
    pointers && (1..=4096).contains(&a) && (1..=4096).contains(&b) && a == bytes.u16_or_zero(50) && b == bytes.u16_or_zero(54)
}

pub fn looks_like_resource(prefix: &[u8]) -> bool {
    looks_like_drawable(prefix) || looks_like_fragment(prefix)
}

// ---------------------------------------------------------------------------
// Extent probe
// ---------------------------------------------------------------------------

/// How much of each section a resource needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub required_system_bytes: i64,
    pub required_graphics_bytes: i64,
    /// False when more system bytes are needed before the metadata can be
    /// read completely; the byte counts are then lower bounds.
    pub metadata_complete: bool,
    pub drawable_offset: i64,
    pub is_fragment: bool,
}

/// Why a probe stopped early.
enum Stop {
    /// The system section must reach at least this many bytes.
    Need(u64),
    /// The bytes are not a supported resource.
    Invalid,
}

type Step<T> = std::result::Result<T, Stop>;

fn hold(condition: bool) -> Step<()> {
    if condition {
        Ok(())
    } else {
        Err(Stop::Invalid)
    }
}

/// Walks the metadata of a possibly incomplete system section and records
/// the furthest system and graphics bytes it references.
struct Probe<'a> {
    data: &'a [u8],
    system_end: u64,
    graphics_end: u64,
    drawable: usize,
    fragment: bool,
}

impl Probe<'_> {
    fn touch(&mut self, at: u64, len: u64) -> Step<()> {
        let end = at.checked_add(len).filter(|&end| end <= MAX_SYSTEM_BYTES as u64).ok_or(Stop::Invalid)?;
        self.system_end = self.system_end.max(end);
        if end > self.data.len() as u64 {
            return Err(Stop::Need(end));
        }
        Ok(())
    }

    fn read<const N: usize>(&mut self, at: usize) -> Step<[u8; N]> {
        self.touch(at as u64, N as u64)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.data[at..at + N]);
        Ok(out)
    }

    fn u8(&mut self, at: usize) -> Step<u8> {
        Ok(self.read::<1>(at)?[0])
    }

    fn u16(&mut self, at: usize) -> Step<u16> {
        self.read(at).map(u16::from_le_bytes)
    }

    fn u32(&mut self, at: usize) -> Step<u32> {
        self.read(at).map(u32::from_le_bytes)
    }

    fn u64(&mut self, at: usize) -> Step<u64> {
        self.read(at).map(u64::from_le_bytes)
    }

    /// Follows a system pointer and touches `len` bytes at its target.
    fn follow(&mut self, at: usize, len: usize) -> Step<usize> {
        let target = self.u64(at)?.checked_sub(SYSTEM_BASE).filter(|&t| t <= MAX_SYSTEM_BYTES as u64).ok_or(Stop::Invalid)?;
        self.touch(target, len as u64)?;
        Ok(target as usize)
    }

    /// Records a graphics range from the pointer at `at`.
    fn graphics(&mut self, at: usize, len: u64) -> Step<()> {
        let max = MAX_GRAPHICS_BYTES as u64;
        let target = self.u64(at)?.checked_sub(GRAPHICS_BASE).filter(|&t| t <= max).ok_or(Stop::Invalid)?;
        hold(len > 0 && len <= max - target)?;
        self.graphics_end = self.graphics_end.max(target + len);
        Ok(())
    }

    fn name(&mut self, at: usize, skip: usize) -> Step<()> {
        let start = self.follow(at, skip + 1)? + skip;
        let available = 256.min(self.data.len() - start);
        let window = &self.data[start..start + available];
        let Some(end) = window.iter().position(|&b| b == 0) else {
            // Either the name continues past the bytes we have, or it is too long.
            if available < 256 {
                self.touch(start as u64, available as u64 + 1)?;
            }
            return Err(Stop::Invalid);
        };
        hold(end > 0)?;
        self.touch(start as u64, end as u64 + 1)?;
        hold(bin::c_name(self.data, start, 256).is_some())
    }

    fn buffer(&mut self, at: usize) -> Step<()> {
        self.touch(at as u64, 64)?;
        let stride = self.u16(at + 12)? as u64;
        let bytes = self.u32(at + 8)? as u64 * stride;
        hold(bytes <= i32::MAX as u64 && (1..=32).contains(&stride) && bytes > 0)?;
        self.graphics(at + 24, bytes)?;
        if self.u64(at + 40)? != 0 {
            self.follow(at + 40, 32)?;
        }
        Ok(())
    }

    fn run(&mut self) -> Step<()> {
        self.touch(0, HEADER as u64)?;
        let pages = self.follow(header::PAGES, 16)?;
        let (system_pages, graphics_pages) = (self.u8(pages + 8)? as u64, self.u8(pages + 9)? as u64);
        hold((1..=32).contains(&system_pages) && (1..=32).contains(&graphics_pages))?;
        self.touch(pages as u64, 16 + 8 * (system_pages + graphics_pages))?;
        self.fragment = !looks_like_drawable(self.data);
        if self.fragment {
            hold(looks_like_fragment(self.data))?;
            self.name(header::NAME, 0)?;
            self.drawable = self.follow(header::FRAGMENT_DRAWABLE, HEADER)?;
            hold(self.drawable != 0 && header_drawable(&self.data[self.drawable..], true))?;
        }
        let drawable = self.drawable;
        if self.u64(drawable + header::NAME)? != 0 {
            self.name(drawable + header::NAME, 0)?;
        }
        self.models(drawable)?;
        let shaders = self.follow(drawable + header::SHADERS, 64)?;
        if self.u64(shaders + 48)? != 0 {
            let descriptor = self.follow(shaders + 48, 32)?;
            let buffer = self.follow(descriptor, 64)?;
            self.buffer(buffer)?;
        }
        if self.u64(shaders + 8)? != 0 {
            let dictionary = self.follow(shaders + 8, 64)?;
            self.dictionary(dictionary)?;
        }
        if self.u64(drawable + header::RENDER_LISTS)? != 0 {
            let lists = self.follow(drawable + header::RENDER_LISTS, 80)?;
            for list in 0..5 {
                let at = lists + list * 16;
                let count = self.u16(at + 8)? as usize;
                hold(count == self.u16(at + 10)? as usize)?;
                if count > 0 {
                    self.follow(at, if list < 4 { count * 4 } else { 1 })?;
                }
            }
        }
        hold(self.graphics_end > 0)
    }

    fn models(&mut self, drawable: usize) -> Step<()> {
        let lods = self.follow(drawable + header::LODS, 16)?;
        let models = self.u16(lods + 8)? as usize;
        hold((1..=256).contains(&models) && models == self.u16(lods + 10)? as usize)?;
        let list = self.follow(lods, models * 8)?;
        let (mut meshes, mut seen_models, mut seen_geometry) = (0, HashSet::new(), HashSet::new());
        for m in 0..models {
            let model = self.follow(list + m * 8, 56)?;
            let count = self.u16(model + 16)? as usize;
            hold(seen_models.insert(model) && (1..=256).contains(&count) && count == self.u16(model + 18)? as usize && count == self.u16(model + 54)? as usize)?;
            hold(self.u16(model + 20)? <= 4096)?;
            let geometries = self.follow(model + 8, count * 8)?;
            self.follow(model + 24, (count + usize::from(count > 1)) * 32)?;
            if self.u64(model + 40)? != 0 {
                self.follow(model + 40, 1)?;
            }
            for g in 0..count {
                meshes += 1;
                hold(meshes <= MAX_MESHES)?;
                let geometry = self.follow(geometries + g * 8, 160)?;
                hold(seen_geometry.insert(geometry))?;
                let vertices = self.follow(geometry + 8, 80)?;
                let fvf = self.follow(vertices + 64, 320)?;
                let flags = self.u32(fvf + layout::FLAGS)?;
                let vertex_count = (flags >> 10) as u64;
                let size = self.u8(fvf + layout::SIZES)?;
                let format = self.u8(fvf + layout::FORMATS)?;
                hold(flags & 1 != 0 && (1..=MAX_MESH_VERTICES).contains(&vertex_count) && matches!((size, format), (12, 6) | (8, 16)))?;
                let bytes = self.u32(vertices + 8)? as u64 * self.u16(vertices + 12)? as u64;
                let offset = self.u32(fvf + layout::OFFSETS)? as u64;
                hold(bytes <= i32::MAX as u64 && offset <= i32::MAX as u64)?;
                hold(offset >= 96 && offset <= bytes && vertex_count * size as u64 <= bytes - offset)?;
                self.buffer(vertices)?;
                let descriptor = self.follow(geometry + 64, 16)?;
                let indices = self.follow(descriptor, 64)?;
                hold(self.u16(indices + 12)? == 4)?;
                self.buffer(indices)?;
            }
        }
        Ok(())
    }

    fn dictionary(&mut self, start: usize) -> Step<()> {
        let count = self.u16(start + 56)? as usize;
        hold(count <= 4096 && count == self.u16(start + 58)? as usize)?;
        if count == 0 {
            return Ok(());
        }
        hold(start >= 64 && self.u16(start + 40)? as usize == count && self.u16(start + 42)? as usize == count)?;
        hold(self.follow(start + 32, count * 4)? >= 64)?;
        let array = self.follow(start + 48, count * 8)?;
        hold(array >= 64)?;
        for i in 0..count {
            let texture = self.follow(array + i * 8, 112)?;
            hold(texture >= 64)?;
            if self.u64(texture + 40)? != 0 {
                hold(self.follow(texture + 40, 32)? >= 64)?;
            }
            let name = self.follow(texture + 48, 9)?;
            hold(name >= 64)?;
            if self.u64(name)? == 0 {
                self.name(texture + 48, 8)?;
            } else {
                self.touch(name as u64 - 16, 32)?;
            }
        }
        let required = dictionary::required_graphics_bytes_at(self.data, start).ok_or(Stop::Invalid)?;
        hold(required > 0 && required <= MAX_GRAPHICS_BYTES)?;
        self.graphics_end = self.graphics_end.max(required as u64);
        Ok(())
    }
}

/// Reports how much of each section a resource needs, or that more system
/// bytes are needed before its metadata can be read.
pub fn probe_required_extents(system: &[u8]) -> Option<Extent> {
    if system.len() as i64 > MAX_SYSTEM_BYTES {
        return None;
    }
    if system.len() < HEADER {
        let start = system.len() >= 16 && Bin(system).u64_or_zero(0) == 0 && system_pointer(Bin(system).u64_or_zero(8));
        return start.then_some(Extent { required_system_bytes: HEADER as i64, required_graphics_bytes: 0, metadata_complete: false, drawable_offset: 0, is_fragment: false });
    }
    if !looks_like_resource(system) {
        return None;
    }
    let mut probe = Probe { data: system, system_end: 0, graphics_end: 0, drawable: 0, fragment: false };
    let (complete, system_end) = match probe.run() {
        Ok(()) => (true, probe.system_end),
        Err(Stop::Need(end)) => (false, end.max(probe.system_end)),
        Err(Stop::Invalid) => return None,
    };
    Some(Extent {
        required_system_bytes: system_end as i64,
        required_graphics_bytes: probe.graphics_end as i64,
        metadata_complete: complete,
        drawable_offset: probe.drawable as i64,
        is_fragment: probe.fragment,
    })
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

fn unsupported() -> Error {
    Error::invalid("Unsupported or inconsistent drawable layout.")
}

fn need(condition: bool) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(unsupported())
    }
}

/// Decodes the static geometry of a drawable or fragment resource.
pub fn decode(system: &[u8], graphics: &[u8]) -> Option<Model> {
    if system.len() as i64 > MAX_SYSTEM_BYTES || graphics.is_empty() || graphics.len() as i64 > MAX_GRAPHICS_BYTES || !looks_like_resource(system) {
        return None;
    }
    Decoder { system: Bin(system), graphics: Bin(graphics), vertices: 0, triangles: 0, bones: false }.model().ok()
}

struct Decoder<'a> {
    system: Bin<'a>,
    graphics: Bin<'a>,
    vertices: u64,
    triangles: u64,
    bones: bool,
}

/// Where a geometry's index data and its cluster tables sit.
struct Clusters<'a> {
    data: Bin<'a>,
    count: usize,
    triangles: usize,
    references: Vec<usize>,
    triangle_table: usize,
}

impl<'a> Decoder<'a> {
    /// Graphics bytes at the pointer stored at `at`.
    fn graphics_at(&self, at: usize, len: usize) -> Result<Bin<'a>> {
        let target = self.system.u64(at)?.checked_sub(GRAPHICS_BASE).filter(|&t| t <= self.graphics.len() as u64).ok_or_else(unsupported)?;
        self.graphics.bytes(target as usize, len).map(Bin).map_err(|_| unsupported())
    }

    fn follow(&self, at: usize, len: usize) -> Result<usize> {
        self.system.system(at, len).map_err(|_| unsupported())
    }

    fn name(&self, at: usize) -> Result<String> {
        let start = self.follow(at, 1)?;
        bin::c_name(self.system.0, start, 256).ok_or_else(unsupported)
    }

    fn model(mut self) -> Result<Model> {
        let s = self.system;
        let pages = self.follow(header::PAGES, 16)?;
        need((1..=32).contains(&s.u8(pages + 8)?) && (1..=32).contains(&s.u8(pages + 9)?))?;
        let fragment = !looks_like_drawable(s.0);
        let (mut drawable, mut parent) = (0, None);
        if fragment {
            need(looks_like_fragment(s.0))?;
            parent = Some(self.name(header::NAME)?);
            drawable = self.follow(header::FRAGMENT_DRAWABLE, HEADER)?;
            need(drawable != 0 && header_drawable(&s.0[drawable..], true))?;
        }
        let named = s.u64(drawable + header::NAME)? != 0;
        let name = match (&parent, named) {
            (Some(parent), false) => parent.clone(),
            _ => self.name(drawable + header::NAME)?,
        };
        if let (Some(parent), true) = (&parent, named) {
            need(name.strip_suffix(".#ft") == Some(parent.as_str()))?;
        }
        let min = s.vec3(drawable + header::BOUNDS_MIN)?;
        let max = s.vec3(drawable + header::BOUNDS_MAX)?;
        let materials = self.materials(drawable).ok();
        let (embedded, embedded_offset) = self.embedded_dictionary(drawable).unwrap_or((None, None));
        let lods = self.follow(drawable + header::LODS, 16)?;
        let model_count = s.u16(lods + 8)? as usize;
        need((1..=256).contains(&model_count) && model_count == s.u16(lods + 10)? as usize)?;
        let list = self.follow(lods, model_count * 8)?;
        let (mut meshes, mut seen_models, mut seen_geometry) = (Vec::new(), HashSet::new(), HashSet::new());
        for m in 0..model_count {
            let model = self.follow(list + m * 8, 56)?;
            need(seen_models.insert(model))?;
            let count = s.u16(model + 16)? as usize;
            need((1..=256).contains(&count) && count == s.u16(model + 18)? as usize && count == s.u16(model + 54)? as usize)?;
            let bone_count = s.u16(model + 20)? as u64;
            need(bone_count <= 4096)?;
            self.bones |= bone_count > 0;
            let geometries = self.follow(model + 8, count * 8)?;
            let shader_map = materials.as_ref().and_then(|list| self.shader_map(model, count, list.len()).ok());
            // With several geometries, the first bounds entry covers the model.
            let shared = usize::from(count > 1);
            let bounds = self.follow(model + 24, (count + shared) * 32)?;
            for g in 0..count {
                need(meshes.len() < MAX_MESHES)?;
                let geometry = self.follow(geometries + g * 8, 160)?;
                need(seen_geometry.insert(geometry))?;
                let bound = bounds + (g + shared) * 32;
                let (part_min, part_max) = (s.vec3(bound)?, s.vec3(bound + 16)?);
                need(ordered(part_min, part_max))?;
                let mut mesh = self.mesh(geometry, format!("model_{m}_geometry_{g}"), part_min, part_max, bone_count)?;
                if let Some(map) = shader_map {
                    mesh.shader_index = Some(s.u16(map + g * 2)? as i32);
                }
                let tolerance = slack(min, max);
                need(within(min, max, part_min, tolerance) && within(min, max, part_max, tolerance))?;
                meshes.push(mesh);
            }
        }
        need(!meshes.is_empty())?;
        let presentation_note = if fragment {
            Some("Raw bind-pose geometry; bone transforms, physics and fragment assembly are not applied.".to_string())
        } else if self.bones {
            Some("Raw bind-pose geometry; bone transforms are not applied.".to_string())
        } else {
            None
        };
        Ok(Model {
            name,
            meshes,
            bounds_min: min,
            bounds_max: max,
            is_fragment: fragment,
            has_bone_bindings: self.bones,
            drawable_offset: drawable as i32,
            presentation_note,
            materials,
            embedded_textures: embedded,
            embedded_texture_dictionary_offset: embedded_offset,
        })
    }

    /// Per-geometry material indices, when they all name a material.
    fn shader_map(&self, model: usize, count: usize, materials: usize) -> Result<usize> {
        let map = self.follow(model + 32, count * 2)?;
        for g in 0..count {
            need((self.system.u16(map + g * 2)? as usize) < materials)?;
        }
        Ok(map)
    }

    fn materials(&self, drawable: usize) -> Result<Vec<Material>> {
        let s = self.system;
        let group = self.follow(drawable + header::SHADERS, 64)?;
        let count = s.u16(group + 24)? as usize;
        need((1..=512).contains(&count) && count == s.u16(group + 26)? as usize)?;
        let shaders = self.follow(group + 16, count * 8)?;
        (0..count).map(|i| self.material(i, self.follow(shaders + i * 8, 64)?)).collect()
    }

    fn material(&self, index: usize, shader: usize) -> Result<Material> {
        let s = self.system;
        need(s.u32(shader + 4)? == PRESET_META)?;
        let info = self.follow(shader + 32, 8)?;
        let texture_count = s.u8(info + 1)? as usize;
        let parameters = s.u8(info + 4)? as usize;
        need(s.u8(info + 5)? == 0 && s.u8(info + 6)? == 0 && (1..=16).contains(&s.u8(info + 7)?))?;
        need(info + 8 + parameters * 8 <= s.len())?;
        let textures = if texture_count > 0 { self.follow(shader + 16, texture_count * 8)? } else { 0 };
        let mut seen = HashSet::new();
        let mut refs = Vec::new();
        for p in 0..parameters {
            let at = info + 8 + p * 8;
            let data = s.u32(at + 4)?;
            // Low bits set mark constant (non-texture) parameters.
            if data & 3 != 0 {
                continue;
            }
            let slot = ((data >> 2) & 255) as usize;
            need(slot < texture_count && seen.insert(slot))?;
            if let Some(reference) = self.texture_ref(textures + slot * 8, s.u32(at)?, slot)? {
                refs.push(reference);
            }
        }
        Ok(Material {
            shader_index: index as i32,
            shader_hash: s.u32(shader)?,
            preset_hash: PRESET_META,
            textures: refs,
            render_bucket: Some(s.u8(shader + 57)?),
            render_bucket_mask: Some(s.u32(shader + 60)?),
        })
    }

    /// A texture slot that references a keyed texture descriptor. Slots
    /// with other descriptor kinds are skipped.
    fn texture_ref(&self, pointer: usize, parameter_hash: u32, slot: usize) -> Result<Option<TextureRef>> {
        let s = self.system;
        if s.u64(pointer)? == 0 {
            return Ok(None);
        }
        let texture = self.follow(pointer, 80)?;
        let reference_shape = s.u32(texture + 16)? == 0x980
            && s.u32(texture + 20)? == 0
            && s.u64(texture + 24)? == 0
            && s.u64(texture + 40)? == 0
            && s.u32(texture + 56)? == 0
            && s.u16(texture + 60)? == 1
            && s.u16(texture + 62)? == 1
            && s.u8(texture + 64)? == 255;
        if !reference_shape || s.u64(texture + 48)? == 0 {
            return Ok(None);
        }
        let asset = self.follow(texture + 48, 16)?;
        let keyed = asset >= 16
            && s.u32(asset - 16)? == 6
            && s.u32(asset - 12)? == 0
            && s.u32(asset - 4)? == 0
            && s.u64(asset)? == SYSTEM_BASE + asset as u64 - 11
            && s.u64(asset + 8)? != 0;
        if !keyed {
            return Ok(None);
        }
        Ok(Some(TextureRef {
            parameter_hash,
            texture_index: slot as i32,
            system_offset: texture as i32,
            asset_descriptor_offset: asset as i32,
            opaque_id64: s.u64(asset + 8)?,
            opaque_id32: s.u32(asset - 8)?,
            tag: 6,
        }))
    }

    fn embedded_dictionary(&self, drawable: usize) -> Result<(Option<Vec<dictionary::DictionaryTexture>>, Option<i32>)> {
        let group = self.follow(drawable + header::SHADERS, 64)?;
        if self.system.u64(group + 8)? == 0 {
            return Ok((None, None));
        }
        let start = self.follow(group + 8, 64)?;
        Ok(match dictionary::inspect_embedded_dictionary(self.system.0, self.graphics.len() as i64, start as i64) {
            Some(found) => (Some(found.textures), Some(start as i32)),
            None => (None, None),
        })
    }

    fn mesh(&mut self, geometry: usize, name: String, bound_min: Vec3, bound_max: Vec3, bone_count: u64) -> Result<Mesh> {
        let s = self.system;
        let buffer = self.follow(geometry + 8, 80)?;
        let fvf = self.follow(buffer + 64, 320)?;
        let flags = s.u32(fvf + layout::FLAGS)?;
        let count = (flags >> 10) as u64;
        need(flags & 1 != 0 && (1..=MAX_MESH_VERTICES).contains(&count))?;
        self.vertices += count;
        need(self.vertices <= MAX_TOTAL_VERTICES)?;
        let length = s.u32(buffer + 8)? as u64 * s.u16(buffer + 12)? as u64;
        need(length > 0 && length <= i32::MAX as u64)?;
        let vertex = self.graphics_at(buffer + 24, length as usize)?;
        let positions = self.positions(fvf, count as usize, vertex)?;

        let descriptor = self.follow(geometry + 64, 16)?;
        let clusters = self.clusters(descriptor, count as usize, bone_count)?;
        let indices = expand(&clusters, &positions, bound_min, bound_max)?;
        Ok(Mesh {
            name,
            tex_coords: tex_coords(s, fvf, count as usize, vertex),
            vertex_colors: vertex_colors(s, fvf, count as usize, vertex),
            positions,
            indices,
            shader_index: None,
        })
    }

    fn positions(&self, fvf: usize, count: usize, vertex: Bin) -> Result<Vec<Vec3>> {
        let s = self.system;
        let offset = s.u32(fvf + layout::OFFSETS)? as usize;
        let size = s.u8(fvf + layout::SIZES)? as usize;
        let format = s.u8(fvf + layout::FORMATS)?;
        need(offset <= i32::MAX as usize && offset >= 96 && offset <= vertex.len() && size * count <= vertex.len() - offset)?;
        let points: Vec<Vec3> = match (size, format) {
            // Three floats per vertex.
            (12, 6) => {
                need(vertex.u32(0)? & 127 == 0x63)?;
                (0..count).map(|i| vertex.vec3(offset + i * 12)).collect::<Result<_>>()?
            }
            // 64-bit quantized positions: 24, 24 and 16 bits with a bias,
            // a scale and an axis order in the buffer header.
            (8, 16) => {
                need(offset >= 128 && vertex.u32(0)? & 127 == 0x42)?;
                let (bias, scale, axis) = (vertex.vec3(96)?, vertex.vec3(112)?, vertex.u32(108)?);
                need(axis <= 2 && finite(bias) && finite(scale) && scale.iter().all(|&v| v >= 0.0))?;
                let mut points = Vec::with_capacity(count);
                for i in 0..count {
                    let q = vertex.u64(offset + i * 8)?;
                    let fields = [(q >> 8) & 0xFF_FFFF, ((q >> 32) & 0xFFFF) << 8 | (q & 0xFF), q >> 48];
                    let p: [f32; 3] = std::array::from_fn(|a| (bias[a] as f64 + scale[a] as f64 * fields[a] as f64) as f32);
                    points.push(match axis {
                        0 => [p[2], p[1], p[0]],
                        1 => [p[1], p[2], p[0]],
                        _ => p,
                    });
                }
                points
            }
            _ => return Err(Error::invalid("Unsupported position encoding.")),
        };
        need(points.iter().all(|&p| finite(p)))?;
        Ok(points)
    }

    fn clusters(&mut self, descriptor: usize, vertex_count: usize, bone_count: u64) -> Result<Clusters<'a>> {
        let s = self.system;
        let buffer = self.follow(descriptor, 64)?;
        let stride = s.u16(buffer + 12)?;
        let length = s.u32(buffer + 8)? as usize * stride as usize;
        need(stride == 4 && length >= 32 && length <= i32::MAX as usize)?;
        let data = self.graphics_at(buffer + 24, length)?;
        let head = data.u32(0)?;
        let palette = head & 0x4000_0000 != 0;
        let count = (head & if palette { 0xFFFF } else { 0x7FFF_FFFF }) as usize;
        let triangles = data.u32(4)? as usize;
        let refs = data.u32(8)? as usize;
        let binding = s.u32(descriptor + 12)?;
        need((1..=2048).contains(&count)
            && (1..=65535).contains(&triangles)
            && (1..=65535).contains(&refs)
            && count == s.u16(descriptor + 8)? as usize
            && triangles == s.u16(descriptor + 10)? as usize
            && refs == ((binding >> 8) & 0x7F_FFFF) as usize
            && (binding & 0x8000_0000 != 0) == palette
            && (!palette || binding & 255 == 0x80))?;
        need(!palette || bone_count > 0)?;
        self.bones |= palette;
        self.triangles += triangles as u64;
        need(self.triangles <= MAX_TOTAL_TRIANGLES)?;
        let wide = head & 0x8000_0000 == 0;
        let ref_size = if wide { 4 } else { 2 };
        let triangle_table = 12 + count * 16;
        let ref_table = triangle_table + triangles * 4;
        need(ref_table <= length && refs * ref_size <= length - ref_table)?;
        let palette_table = ref_table + refs * ref_size;
        let used = palette_table + if palette { count * 2 } else { 0 };
        need(used <= length && bin::align(used as u64, 16) == length as u64 && bin::all_zero(&data.0[used..]))?;
        if palette {
            let hint = (head >> 16) & 0x1FFF;
            let constant = head & 0x2000_0000 != 0;
            need(if constant { hint & 1 == 0 && ((hint / 2) as u64) < bone_count } else { hint == 0 })?;
            for c in 0..count {
                let bone = data.u16(palette_table + c * 2)?;
                need(bone == u16::MAX || (bone as u64) < bone_count)?;
                need(!constant || bone as u32 == hint / 2)?;
            }
        }
        let references = (0..refs)
            .map(|i| {
                let value = if wide { data.u32(ref_table + i * 4)? as usize } else { data.u16(ref_table + i * 2)? as usize };
                need(value < vertex_count)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Clusters { data, count, triangles, references, triangle_table })
    }
}

/// Turns clustered triangles into vertex indices and checks that they
/// reach the geometry bounds.
fn expand(clusters: &Clusters, positions: &[Vec3], bound_min: Vec3, bound_max: Vec3) -> Result<Vec<i32>> {
    let data = clusters.data;
    let mut indices = vec![0i32; clusters.triangles * 3];
    let (mut refs_done, mut triangles_done) = (0usize, 0usize);
    let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for cluster in 0..clusters.count {
        let packed = data.u32(12 + cluster * 16)?;
        let starts = data.u32(12 + cluster * 16 + 4)?;
        let vertices = (packed & 127) as usize + 1;
        let triangles = ((packed >> 7) & 255) as usize + 1;
        let (first_ref, first_triangle) = ((starts & 0xFFFF) as usize, (starts >> 16) as usize);
        need(first_ref == refs_done
            && first_triangle == triangles_done
            && vertices <= clusters.references.len() - first_ref
            && triangles <= clusters.triangles - first_triangle)?;
        for t in first_triangle..first_triangle + triangles {
            // Three 7-bit cluster-local corners, then the cluster number.
            let word = data.u32(clusters.triangle_table + t * 4)?;
            need((word >> 21) as usize == cluster)?;
            for corner in 0..3 {
                let local = ((word >> (corner * 7)) & 127) as usize;
                need(local < vertices)?;
                let vertex = clusters.references[first_ref + local];
                indices[t * 3 + corner] = vertex as i32;
                for axis in 0..3 {
                    let value = positions[vertex][axis];
                    if value < low[axis] {
                        low[axis] = value;
                    }
                    if value > high[axis] {
                        high[axis] = value;
                    }
                }
            }
        }
        refs_done += vertices;
        triangles_done += triangles;
    }
    need(refs_done == clusters.references.len() && triangles_done == clusters.triangles)?;
    let tolerance = slack(bound_min, bound_max);
    need((0..3).all(|a| (low[a] - bound_min[a]).abs() <= tolerance && (high[a] - bound_max[a]).abs() <= tolerance))?;
    Ok(indices)
}

fn within(min: Vec3, max: Vec3, point: Vec3, tolerance: f32) -> bool {
    (0..3).all(|a| point[a] >= min[a] - tolerance && point[a] <= max[a] + tolerance)
}

/// Offset of a vertex attribute when the FVF and the buffer header agree
/// on its position, size and type.
#[allow(clippy::too_many_arguments)]
fn attribute(system: Bin, fvf: usize, slot: usize, vertex: Bin, header_at: usize, size: usize, kind: u32, count: usize) -> Option<usize> {
    let offset = system.u32(fvf + layout::OFFSETS + slot * 4).ok()? as usize;
    if offset > vertex.len() || count * size > vertex.len() - offset || vertex.len() < header_at + 4 {
        return None;
    }
    let native = vertex.u32(header_at).ok()?;
    let agrees = ((native >> 7) & 0x1FF_FFFC) as usize == offset && ((native >> 3) & 60) as usize == size && native & 15 == kind;
    agrees.then_some(offset)
}

/// RGBA8 vertex colors (layout slot 24).
fn vertex_colors(system: Bin, fvf: usize, count: usize, vertex: Bin) -> Option<Vec<u32>> {
    let slot = layout::COLOR;
    if system.u8(fvf + layout::SIZES + slot).ok()? != 4 || system.u8(fvf + layout::FORMATS + slot).ok()? != 28 {
        return None;
    }
    let offset = attribute(system, fvf, slot, vertex, 4, 4, 6, count)?;
    (0..count).map(|i| vertex.u32(offset + i * 4).ok()).collect()
}

/// UVs (layout slot 28): two floats, or two signed 16-bit values scaled by
/// a factor in the buffer header.
fn tex_coords(system: Bin, fvf: usize, count: usize, vertex: Bin) -> Option<Vec<Vec2>> {
    let slot = layout::UV;
    let size = system.u8(fvf + layout::SIZES + slot).ok()? as usize;
    let format = system.u8(fvf + layout::FORMATS + slot).ok()?;
    let floating = match (size, format) {
        (8, 16) => true,
        (4, 38) => false,
        _ => return None,
    };
    if vertex.len() < 16 {
        return None;
    }
    let offset = attribute(system, fvf, slot, vertex, 12, size, if floating { 2 } else { 8 }, count)?;
    let scale = if floating {
        1.0
    } else {
        let scale = vertex.f32(124).ok()?;
        if vertex.len() < 128 || !scale.is_finite() || scale <= 0.0 || scale > 1_000_000.0 {
            return None;
        }
        scale
    };
    let unit = |raw: i16| (raw as f32 / 32767.0).max(-1.0) * scale;
    let mut uv = Vec::with_capacity(count);
    for i in 0..count {
        let at = offset + i * size;
        let (u, v) = if floating { (vertex.f32(at).ok()?, vertex.f32(at + 4).ok()?) } else { (unit(vertex.i16(at).ok()?), unit(vertex.i16(at + 2).ok()?)) };
        if !(u.is_finite() && v.is_finite() && u.abs() <= 1_000_000.0 && v.abs() <= 1_000_000.0) {
            return None;
        }
        uv.push([u, v]);
    }
    Some(uv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_or_empty_prefixes_are_not_resources() {
        assert!(!looks_like_resource(&[0; 100]));
        assert!(probe_required_extents(&[0; 8]).is_none());
        assert!(decode(&[0; 200], &[0; 10]).is_none());
        let mut start = vec![0u8; 32];
        start[8..16].copy_from_slice(&(SYSTEM_BASE + 64).to_le_bytes());
        assert!(!probe_required_extents(&start).unwrap().metadata_complete);
    }
}
