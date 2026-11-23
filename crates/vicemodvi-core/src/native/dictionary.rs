//! Texture dictionaries inside decoded RPF9 resources.
//!
//! A dictionary lives in the system section: a header with a name-hash
//! array and a texture-pointer array, then one descriptor per texture. The
//! texel data sits in the graphics section as 4 KiB AMD tiles.

use super::{
    bin::{self, Bin, GRAPHICS_BASE, SYSTEM_BASE},
    dds::{self, Layers},
    dxgi,
    pixels::{self, Pixels},
    swizzle::{Surface, Tiling},
};
use crate::{
    util::{hex_upper, sha256},
    Cancel, Error, Result,
};

pub const MAX_SYSTEM_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_GRAPHICS_BYTES: u64 = 256 * 1024 * 1024;
/// Dictionaries embedded in drawables may sit in a larger system section.
const MAX_EMBEDDING_SYSTEM_BYTES: u64 = 32 * 1024 * 1024;
const HEADER: usize = 64;
const DESCRIPTOR: usize = 0x70;
const NAME_LIMIT: usize = 1024;

/// Header fields, relative to the dictionary start.
mod at {
    pub const PAGE_INFO: usize = 0x08;
    pub const HASHES: usize = 0x20;
    pub const HASH_COUNT: usize = 0x28;
    pub const TEXTURES: usize = 0x30;
    pub const COUNT: usize = 0x38;
    pub const CAPACITY: usize = 0x3A;
}

/// Descriptor fields, relative to the descriptor start.
mod field {
    pub const ELEMENTS: usize = 0x08;
    pub const ELEMENT_BYTES: usize = 0x0C;
    pub const GRAPHICS: usize = 0x18;
    pub const SELF_LINK: usize = 0x28;
    pub const NAME: usize = 0x30;
    pub const WIDTH: usize = 0x38;
    pub const HEIGHT: usize = 0x3A;
    pub const DEPTH: usize = 0x3C;
    pub const DIMENSION: usize = 0x3E;
    pub const FORMAT: usize = 0x3F;
    pub const TILE_MODE: usize = 0x40;
    pub const MIPS: usize = 0x42;
}

/// One texture of a dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryTexture {
    pub index: i32,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub dxgi_format: i32,
    pub format: String,
    pub mip_count: i32,
    pub graphics_offset: i32,
    pub allocation_bytes: i32,
    pub tile_mode: i32,
    pub name_hash: u32,
    /// Present when the descriptor names the texture by native key.
    pub asset_key: Option<u64>,
    /// Slices: array layers, cube faces or volume depth.
    pub depth: i32,
    /// 1 = 2D, 2 = cube, 3 = volume.
    pub dimension: i32,
}

impl DictionaryTexture {
    pub fn preview_mip_level(&self) -> i32 {
        dxgi::preview_level(self.width, self.height, self.mip_count)
    }

    fn surface(&self) -> Result<Surface> {
        let tiling = if self.dimension == 3 { Tiling::Volume4K } else { Tiling::Flat4K };
        Surface::new(self.width as u64, self.height as u64, self.depth as u64, self.dxgi_format, self.mip_count as u64, tiling)
    }

    fn layers(&self) -> Layers {
        match self.dimension {
            2 => Layers::Cube(self.depth),
            3 => Layers::Volume(self.depth),
            _ => Layers::Flat(self.depth),
        }
    }

    /// Depth slices stored for `level`.
    fn planes(&self, level: i32) -> i32 {
        if self.dimension == 3 {
            dxgi::mip_extent(self.depth, level)
        } else {
            self.depth
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dictionary {
    pub system_bytes: i32,
    pub graphics_bytes: i32,
    pub textures: Vec<DictionaryTexture>,
}

fn is_system_pointer(value: u64) -> bool {
    (SYSTEM_BASE..SYSTEM_BASE + MAX_SYSTEM_BYTES).contains(&value)
}

/// System offset of the pointer at `at`; the target must exist.
fn target(system: Bin, at: usize) -> Result<usize> {
    let pointer = system.u64(at)?;
    if !is_system_pointer(pointer) {
        return Err(Error::invalid("Invalid system pointer."));
    }
    let offset = (pointer - SYSTEM_BASE) as usize;
    bin::range(system.len(), offset, 1)?;
    Ok(offset)
}

/// Cheap header test on the first bytes of a candidate dictionary.
pub fn looks_like_dictionary(system: &[u8]) -> bool {
    let data = Bin(system);
    let test = || -> Result<bool> {
        let count = data.u16(at::COUNT)?;
        Ok((1..=4096).contains(&count)
            && data.u16(at::CAPACITY)? >= count
            && data.u16(at::HASH_COUNT)? == count
            && is_system_pointer(data.u64(at::HASHES)?)
            && is_system_pointer(data.u64(at::TEXTURES)?))
    };
    system.len() >= HEADER && test().unwrap_or(false)
}

/// Validates a standalone dictionary at the start of `system`.
pub fn inspect(system: &[u8], graphics: &[u8]) -> Option<Dictionary> {
    inspect_at(system, graphics.len() as u64, 0)
}

/// Validates a dictionary that starts `offset` bytes into `system`.
pub fn inspect_embedded_dictionary(system: &[u8], graphics_length: i64, offset: i64) -> Option<Dictionary> {
    inspect_at(system, u64::try_from(graphics_length).ok()?, usize::try_from(offset).ok()?)
}

fn inspect_at(system: &[u8], graphics_length: u64, offset: usize) -> Option<Dictionary> {
    let textures = parse(system, graphics_length, offset).ok()?;
    Some(Dictionary { system_bytes: system.len() as i32, graphics_bytes: graphics_length as i32, textures })
}

fn parse(system: &[u8], graphics_length: u64, start: usize) -> Result<Vec<DictionaryTexture>> {
    let limit = if start == 0 { MAX_SYSTEM_BYTES } else { MAX_EMBEDDING_SYSTEM_BYTES };
    if system.len() as u64 > limit
        || graphics_length > MAX_GRAPHICS_BYTES
        || start + HEADER > system.len()
        || !looks_like_dictionary(&system[start..])
    {
        return Err(Error::invalid("Not a supported bounded texture dictionary."));
    }
    let data = Bin(system);
    let count = data.u16(start + at::COUNT)? as usize;
    let hashes = target(data, start + at::HASHES)?;
    let pointers = target(data, start + at::TEXTURES)?;
    data.bytes(hashes, count * 4)?;
    data.bytes(pointers, count * 8)?;
    let mut textures: Vec<DictionaryTexture> = Vec::with_capacity(count);
    for index in 0..count {
        let texture = read_texture(data, index, target(data, pointers + index * 8)?, data.u32(hashes + index * 4)?, graphics_length)?;
        let (from, to) = (texture.graphics_offset as u64, texture.graphics_offset as u64 + texture.allocation_bytes as u64);
        if textures.iter().any(|other| from < other.graphics_offset as u64 + other.allocation_bytes as u64 && (other.graphics_offset as u64) < to) {
            return Err(Error::invalid("Overlapping texture allocations."));
        }
        textures.push(texture);
    }
    Ok(textures)
}

/// Name of a descriptor: inline text, or a keyed descriptor that carries
/// the native 64-bit key.
fn read_name(data: Bin, descriptor: usize) -> Result<(String, Option<u64>)> {
    let name = target(data, descriptor + field::NAME)?;
    data.bytes(name, 8)?;
    if data.u64(name)? == 0 {
        let text = bin::c_name(data.0, name + 8, NAME_LIMIT).ok_or_else(|| Error::invalid("Invalid texture name."))?;
        return Ok((text, None));
    }
    data.bytes(name, 16)?;
    let key = data.u64(name + 8)?;
    let keyed = name >= 16
        && data.u64(name)? == SYSTEM_BASE + name as u64 - 11
        && key != 0
        && data.u32(name - 16)? == 6
        && data.u32(name - 12)? == 0
        && data.u32(name - 4)? == 0;
    if !keyed {
        return Err(Error::invalid("Unsupported keyed texture descriptor."));
    }
    Ok((format!("texture_{key:016X}"), Some(key)))
}

fn read_texture(data: Bin, index: usize, descriptor: usize, name_hash: u32, graphics_length: u64) -> Result<DictionaryTexture> {
    data.bytes(descriptor, DESCRIPTOR)?;
    let (name, asset_key) = read_name(data, descriptor)?;
    let width = data.u16(descriptor + field::WIDTH)? as i32;
    let height = data.u16(descriptor + field::HEIGHT)? as i32;
    let depth = data.u16(descriptor + field::DEPTH)? as i32;
    let dimension = data.u8(descriptor + field::DIMENSION)? as i32;
    let format = data.u8(descriptor + field::FORMAT)? as i32;
    let element = dxgi::element_size_of(format)?;
    let valid = (1..=16384).contains(&width)
        && (1..=16384).contains(&height)
        && (1..=2048).contains(&depth)
        && (1..=3).contains(&dimension)
        && (dimension != 2 || depth % 6 == 0)
        && data.u8(descriptor + field::TILE_MODE)? == 5
        && data.u16(descriptor + field::ELEMENT_BYTES)? as u64 == element
        && data.u64(descriptor + field::SELF_LINK)? == SYSTEM_BASE + descriptor as u64 + 0x50;
    if !valid {
        return Err(Error::invalid("Unsupported texture descriptor."));
    }
    let graphics = data
        .u64(descriptor + field::GRAPHICS)?
        .checked_sub(GRAPHICS_BASE)
        .filter(|&offset| offset <= i32::MAX as u64)
        .ok_or_else(|| Error::invalid("Invalid graphics pointer."))?;
    let allocation = data.u32(descriptor + field::ELEMENTS)? as u64 * element;
    if allocation > i32::MAX as u64 {
        return Err(Error::invalid("Texture allocation overflows."));
    }
    bin::range(graphics_length as usize, graphics as usize, allocation as usize)?;
    let texture = DictionaryTexture {
        index: index as i32,
        name,
        width,
        height,
        dxgi_format: format,
        format: dxgi::name(format).to_string(),
        mip_count: data.u8(descriptor + field::MIPS)? as i32,
        graphics_offset: graphics as i32,
        allocation_bytes: allocation as i32,
        tile_mode: 5,
        name_hash,
        asset_key,
        depth,
        dimension,
    };
    let surface = texture.surface()?;
    let slices = if dimension == 3 { 1 } else { depth as u64 };
    if surface.size * slices != allocation {
        return Err(Error::invalid("Descriptor allocation disagrees with AMD mip layout."));
    }
    Ok(texture)
}

fn pick(textures: Vec<DictionaryTexture>, index: i32) -> Result<DictionaryTexture> {
    usize::try_from(index)
        .ok()
        .and_then(|i| textures.into_iter().nth(i))
        .ok_or_else(|| Error::argument("Texture index is out of range."))
}

/// Bytes of one slice (or the whole volume) of a texture allocation.
fn slice_bytes<'a>(graphics: &'a [u8], texture: &DictionaryTexture, surface: &Surface, slice: i32) -> Result<&'a [u8]> {
    let allocation = Bin(graphics).bytes(texture.graphics_offset as usize, texture.allocation_bytes as usize)?;
    if texture.dimension == 3 {
        Ok(allocation)
    } else {
        Bin(allocation).bytes(slice as usize * surface.size as usize, surface.size as usize)
    }
}

/// Decodes one mip of a texture embedded in a drawable's dictionary.
pub fn decode_embedded_texture_pixels(system: &[u8], graphics: &[u8], dictionary: i64, index: i32, level: i32) -> Result<Pixels> {
    let start = usize::try_from(dictionary).map_err(|_| Error::invalid("Invalid dictionary offset."))?;
    let texture = pick(parse(system, graphics.len() as u64, start)?, index)?;
    decode_level(&texture, graphics, level)
}

fn decode_level(texture: &DictionaryTexture, graphics: &[u8], level: i32) -> Result<Pixels> {
    if !(0..texture.mip_count).contains(&level) {
        return Err(Error::argument("The mip level is outside the texture."));
    }
    let (width, height) = (dxgi::mip_extent(texture.width, level), dxgi::mip_extent(texture.height, level));
    if width as u64 * height as u64 > dxgi::PREVIEW_PIXELS {
        return Err(Error::not_supported(
            "This texture has no available mip within the 16-megapixel preview limit. DDS export remains available.",
        ));
    }
    let surface = texture.surface()?;
    let linear = surface.read_level(slice_bytes(graphics, texture, &surface, 0)?, 0, level as usize, 0)?;
    pixels::decode(&linear, width, height, texture.dxgi_format)
}

/// Exports every mip and slice of one texture as a DX10 DDS.
pub fn create_dds(system: &[u8], graphics: &[u8], index: i32) -> Result<Vec<u8>> {
    let texture = pick(parse(system, graphics.len() as u64, 0)?, index)?;
    let surface = texture.surface()?;
    let mut out = dds::dx10_header(texture.width, texture.height, texture.layers(), texture.mip_count, texture.dxgi_format)?;
    out.reserve(texture.allocation_bytes as usize);
    if texture.dimension == 3 {
        let volume = slice_bytes(graphics, &texture, &surface, 0)?;
        for level in 0..texture.mip_count {
            for z in 0..texture.planes(level) {
                out.extend(surface.read_level(volume, 0, level as usize, z as u64)?);
            }
        }
    } else {
        for slice in 0..texture.depth {
            let bytes = slice_bytes(graphics, &texture, &surface, slice)?;
            for level in 0..texture.mip_count {
                out.extend(surface.read_level(bytes, 0, level as usize, 0)?);
            }
        }
    }
    Ok(out)
}

/// Returns a copy of `graphics` with one texture replaced by a DDS of the
/// same shape, and checks that the copy exports that DDS again.
pub(crate) fn retile(system: &[u8], graphics: &[u8], index: i32, replacement: &[u8], cancel: &Cancel) -> Result<Vec<u8>> {
    let texture = pick(parse(system, graphics.len() as u64, 0)?, index)?;
    let imported = dds::inspect(replacement)?;
    let same_shape = imported.width == texture.width
        && imported.height == texture.height
        && imported.depth == texture.depth
        && imported.dimension == texture.dimension
        && imported.mips == texture.mip_count
        && imported.dxgi_format == texture.dxgi_format
        && imported.resource_dimension == if texture.dimension == 3 { 4 } else { 3 };
    if !same_shape {
        return Err(Error::invalid(
            "Replacement DDS must exactly match the native texture dimensions, format, mip count and array/cube/volume shape.",
        ));
    }
    if !matches!(imported.alpha_mode, 0 | 1) {
        return Err(Error::not_supported(
            "Replacement alpha mode must be unknown or straight; native alpha semantics cannot be converted implicitly.",
        ));
    }
    let surface = texture.surface()?;
    let mut staged = graphics.to_vec();
    let start = texture.graphics_offset as usize;
    let allocation = &mut staged[bin::range(graphics.len(), start, texture.allocation_bytes as usize)?];
    for level in 0..texture.mip_count {
        for plane in 0..texture.planes(level) {
            cancel.check()?;
            let linear = dds::mip_plane(replacement, &imported, level, plane)?;
            if texture.dimension == 3 {
                surface.write_level(allocation, 0, level as usize, plane as u64, linear)?;
            } else {
                let size = surface.size as usize;
                let span = bin::range(allocation.len(), plane as usize * size, size)?;
                let slice = &mut allocation[span];
                surface.write_level(slice, 0, level as usize, 0, linear)?;
            }
        }
    }
    let roundtrip = create_dds(system, &staged, index)?;
    if roundtrip.get(dds::DX10_HEADER_BYTES..) != Some(imported.payload(replacement)) {
        return Err(Error::invalid("Native retile roundtrip did not reproduce the imported DDS payload."));
    }
    Ok(staged)
}

/// System and graphics page counts from the header of a dictionary.
pub fn page_counts(first_page: &[u8]) -> Option<(i32, i32)> {
    if !looks_like_dictionary(first_page) {
        return None;
    }
    let data = Bin(first_page);
    let info = target(data, at::PAGE_INFO).ok()?;
    data.bytes(info, 16).ok()?;
    let (system, graphics) = (data.u8(info + 8).ok()? as i32, data.u8(info + 9).ok()? as i32);
    ((1..=32).contains(&system) && (1..=32).contains(&graphics)).then_some((system, graphics))
}

/// Graphics bytes the textures of a complete system section reach.
pub fn required_graphics_bytes(system: &[u8]) -> Option<i64> {
    required_graphics_bytes_at(system, 0)
}

/// [`required_graphics_bytes`] for a dictionary `start` bytes into `system`.
pub(crate) fn required_graphics_bytes_at(system: &[u8], start: usize) -> Option<i64> {
    let textures = parse(system, MAX_GRAPHICS_BYTES, start).ok()?;
    let end = textures.iter().map(|t| t.graphics_offset as i64 + t.allocation_bytes as i64).max()?;
    (1..=i32::MAX as i64).contains(&end).then_some(end)
}

/// Lower bound for the system section needed to parse the dictionary that
/// `partial` starts. `None` means the bytes cannot start a dictionary.
pub fn minimum_system_bytes(partial: &[u8]) -> Option<i64> {
    if !looks_like_dictionary(partial) {
        return None;
    }
    let data = Bin(partial);
    let offset = |at: usize| data.u64(at).ok().filter(|&p| is_system_pointer(p)).map(|p| p - SYSTEM_BASE);
    let count = data.u16(at::COUNT).ok()? as u64;
    let mut needed = (offset(at::PAGE_INFO)? + 16).max(offset(at::HASHES)? + count * 4).max(offset(at::TEXTURES)? + count * 8);
    let pointers = offset(at::TEXTURES)? as usize;
    if needed > MAX_SYSTEM_BYTES {
        return None;
    }
    if needed > partial.len() as u64 {
        return Some(needed as i64);
    }
    for index in 0..count as usize {
        let descriptor = offset(pointers + index * 8)?;
        needed = needed.max(descriptor + DESCRIPTOR as u64);
        if needed > MAX_SYSTEM_BYTES {
            return None;
        }
        if descriptor + DESCRIPTOR as u64 > partial.len() as u64 {
            continue;
        }
        let d = descriptor as usize;
        let format = data.u8(d + field::FORMAT).ok()? as i32;
        let plausible = (1..=3).contains(&data.u8(d + field::DIMENSION).ok()?)
            && data.u8(d + field::TILE_MODE).ok()? == 5
            && Some(data.u16(d + field::ELEMENT_BYTES).ok()? as u64) == dxgi::element_size(format)
            && data.u64(d + field::SELF_LINK).ok()? == SYSTEM_BASE + descriptor + 0x50;
        if !plausible {
            return None;
        }
        let name = offset(d + field::NAME)? + 8;
        needed = needed.max(name + 1);
        if name < partial.len() as u64 {
            let window = &partial[name as usize..(name as usize + NAME_LIMIT).min(partial.len())];
            if !window.contains(&0) {
                needed = needed.max(name + NAME_LIMIT as u64);
            }
        }
    }
    (needed <= MAX_SYSTEM_BYTES).then_some(needed as i64)
}

/// Result of staging a replacement into a decoded dictionary resource.
#[derive(Clone, Debug)]
pub struct StagedReplacement {
    /// Original system bytes followed by the retiled graphics section.
    pub resource_data: Vec<u8>,
    pub resource_system_bytes: i32,
    pub texture: DictionaryTexture,
    pub changed_graphics_bytes: i64,
    pub source_graphics_sha256: String,
    pub staged_graphics_sha256: String,
}

/// Stages `replacement` into a copy of the resource and reports the change.
pub fn stage(system: &[u8], graphics: &[u8], index: i32, replacement: &[u8], cancel: &Cancel) -> Result<StagedReplacement> {
    cancel.check()?;
    if (system.len() + graphics.len()) as u64 > MAX_GRAPHICS_BYTES {
        return Err(Error::not_supported("The decoded resource exceeds the 256 MiB staging limit."));
    }
    let dictionary = inspect(system, graphics).ok_or_else(|| Error::invalid("Source is not a supported validated native texture dictionary."))?;
    let texture = pick(dictionary.textures, index)?;
    let staged = retile(system, graphics, index, replacement, cancel)?;
    let changed = graphics.iter().zip(&staged).filter(|(a, b)| a != b).count() as i64;
    let mut resource_data = Vec::with_capacity(system.len() + staged.len());
    resource_data.extend_from_slice(system);
    resource_data.extend_from_slice(&staged);
    Ok(StagedReplacement {
        resource_data,
        resource_system_bytes: system.len() as i32,
        texture,
        changed_graphics_bytes: changed,
        source_graphics_sha256: hex_upper(&sha256(graphics)),
        staged_graphics_sha256: hex_upper(&sha256(&staged)),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn put(buffer: &mut [u8], at: usize, bytes: &[u8]) {
        buffer[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// A one-texture dictionary and its graphics section.
    pub(crate) fn sample(width: u16, height: u16, format: u8, mips: u8, keyed: bool) -> (Vec<u8>, Vec<u8>) {
        let element = dxgi::element_size(format as i32).unwrap();
        let surface = Surface::new(width as u64, height as u64, 1, format as i32, mips as u64, Tiling::Flat4K).unwrap();
        let mut system = vec![0u8; 0x400];
        let base = SYSTEM_BASE;
        put(&mut system, at::PAGE_INFO, &(base + 0x80).to_le_bytes());
        system[0x88] = 1;
        system[0x89] = 1;
        put(&mut system, at::HASHES, &(base + 0x100).to_le_bytes());
        put(&mut system, at::HASH_COUNT, &1u16.to_le_bytes());
        put(&mut system, at::TEXTURES, &(base + 0x110).to_le_bytes());
        put(&mut system, at::COUNT, &1u16.to_le_bytes());
        put(&mut system, at::CAPACITY, &1u16.to_le_bytes());
        put(&mut system, 0x100, &0xDEAD_BEEFu32.to_le_bytes());
        let d = 0x140usize;
        put(&mut system, 0x110, &(base + d as u64).to_le_bytes());
        put(&mut system, d + field::ELEMENTS, &((surface.size / element) as u32).to_le_bytes());
        put(&mut system, d + field::ELEMENT_BYTES, &(element as u16).to_le_bytes());
        put(&mut system, d + field::GRAPHICS, &GRAPHICS_BASE.to_le_bytes());
        put(&mut system, d + field::SELF_LINK, &(base + d as u64 + 0x50).to_le_bytes());
        put(&mut system, d + field::WIDTH, &width.to_le_bytes());
        put(&mut system, d + field::HEIGHT, &height.to_le_bytes());
        put(&mut system, d + field::DEPTH, &1u16.to_le_bytes());
        system[d + field::DIMENSION] = 1;
        system[d + field::FORMAT] = format;
        system[d + field::TILE_MODE] = 5;
        system[d + field::MIPS] = mips;
        let name = 0x300usize;
        put(&mut system, d + field::NAME, &(base + name as u64).to_le_bytes());
        if keyed {
            put(&mut system, name - 16, &6u32.to_le_bytes());
            put(&mut system, name, &(base + name as u64 - 11).to_le_bytes());
            put(&mut system, name + 8, &0x0123_4567_89AB_CDEFu64.to_le_bytes());
        } else {
            put(&mut system, name + 8, b"sample_texture\0");
        }
        let graphics = (0..surface.size).map(|i| (i * 31 % 251) as u8).collect();
        (system, graphics)
    }

    #[test]
    fn reads_named_and_keyed_textures() {
        let (system, graphics) = sample(64, 64, 28, 7, false);
        let found = inspect(&system, &graphics).unwrap();
        assert_eq!(found.textures[0].name, "sample_texture");
        assert_eq!(found.textures[0].name_hash, 0xDEAD_BEEF);
        let (system, graphics) = sample(128, 64, 71, 8, true);
        let found = inspect(&system, &graphics).unwrap();
        assert_eq!(found.textures[0].asset_key, Some(0x0123_4567_89AB_CDEF));
        assert_eq!(found.textures[0].name, "texture_0123456789ABCDEF");
    }

    #[test]
    fn replacements_roundtrip() {
        for (w, h, f, m) in [(64u16, 64u16, 28u8, 7u8), (256, 128, 71, 9), (32, 32, 98, 6), (16, 8, 61, 5)] {
            let (system, graphics) = sample(w, h, f, m, true);
            let exported = create_dds(&system, &graphics, 0).unwrap();
            dds::inspect(&exported).unwrap();
            let mut edited = exported.clone();
            for byte in &mut edited[dds::DX10_HEADER_BYTES..] {
                *byte = byte.wrapping_mul(3).wrapping_add(1);
            }
            let staged = retile(&system, &graphics, 0, &edited, &Cancel::none()).unwrap();
            assert_eq!(create_dds(&system, &staged, 0).unwrap(), edited);
            assert_eq!(retile(&system, &graphics, 0, &exported, &Cancel::none()).unwrap(), graphics);
        }
    }

    #[test]
    fn partial_sections_report_what_they_need() {
        let (system, graphics) = sample(64, 64, 28, 7, true);
        assert!(minimum_system_bytes(&system).unwrap() <= system.len() as i64);
        assert!(minimum_system_bytes(&system[..0x100]).unwrap() > 0x100);
        assert_eq!(required_graphics_bytes(&system), Some(graphics.len() as i64));
        assert_eq!(page_counts(&system), Some((1, 1)));
    }
}
