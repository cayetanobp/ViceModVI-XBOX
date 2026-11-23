//! Observed layout of RAGE resources stored as decoded zlib units.
//!
//! A resource is a virtual (system) memory image, whose pointers fall in
//! `0x5000_0000 + offset`, followed by zero or more physical (graphics) units
//! whose pointers fall in `0x6000_0000 + offset`. The root object starts with
//! a zeroed 8-byte field and a pointer to a page map whose bytes 8 and 9 hold
//! the virtual and physical page counts. Field names below describe what was
//! observed in the samples; none is confirmed by a specification.

pub const VIRTUAL_BASE: u64 = 0x5000_0000;
pub const PHYSICAL_BASE: u64 = 0x6000_0000;
const SEGMENT: u64 = 0x1000_0000;

pub fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Offset of a virtual pointer that lands inside a section of `len` bytes.
pub fn virtual_offset(ptr: u64, len: usize) -> Option<usize> {
    (VIRTUAL_BASE..VIRTUAL_BASE + len as u64).contains(&ptr).then(|| (ptr - VIRTUAL_BASE) as usize)
}

/// Offset of a pointer into the physical segment, without a length check.
pub fn physical_offset(ptr: u64) -> Option<u64> {
    (PHYSICAL_BASE..PHYSICAL_BASE + SEGMENT).contains(&ptr).then(|| ptr - PHYSICAL_BASE)
}

/// Jenkins one-at-a-time hash of the lowercased bytes, as used by RAGE.
pub fn joaat(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &c in bytes {
        h = h.wrapping_add(u32::from(c.to_ascii_lowercase()));
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// Offset of the page map inside the virtual section.
    pub page_map: usize,
    pub virtual_pages: u8,
    pub physical_pages: u8,
}

/// Recognises a resource root: zero first field, page map pointer inside the
/// section, the page map's first 8 bytes zero and at least one virtual page.
pub fn root(b: &[u8]) -> Option<Root> {
    if b.len() < 0x40 || u64_at(b, 0)? != 0 {
        return None;
    }
    let page_map = virtual_offset(u64_at(b, 8)?, b.len())?;
    if page_map % 8 != 0 || u64_at(b, page_map)? != 0 {
        return None;
    }
    let (virtual_pages, physical_pages) = (*b.get(page_map + 8)?, *b.get(page_map + 9)?);
    (virtual_pages >= 1).then_some(Root { page_map, virtual_pages, physical_pages })
}

/// The start of a root whose page map pointer lands in the virtual segment
/// but past these bytes: the section continues in the next unit.
pub fn split_root_candidate(b: &[u8]) -> bool {
    b.len() >= 0x40 && u64_at(b, 0) == Some(0) && u64_at(b, 8).is_some_and(|p| {
        (VIRTUAL_BASE + b.len() as u64..VIRTUAL_BASE + SEGMENT).contains(&p) && p % 8 == 0
    })
}

/// NUL-terminated printable ASCII string at `at`, if any.
pub fn c_string(b: &[u8], at: usize) -> Option<&[u8]> {
    let rest = b.get(at..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    let s = &rest[..end];
    (!s.is_empty() && s.iter().all(|c| (0x20..0x7f).contains(c))).then_some(s)
}

#[derive(Debug, Clone)]
pub struct Dictionary {
    pub hashes: Vec<u32>,
    /// Offsets of the elements inside the virtual section.
    pub elements: Vec<usize>,
    pub capacity: u16,
}

/// Reads the dictionary observed at `0x20` of many roots: a hash array and an
/// element pointer array, each with a u16 count and capacity. It is accepted
/// only when both counts match, both arrays fit, the hashes are strictly
/// increasing and every element pointer lands inside the section.
pub fn dictionary(b: &[u8]) -> Option<Dictionary> {
    let len = b.len();
    let hashes_at = virtual_offset(u64_at(b, 0x20)?, len)?;
    let (count, capacity) = (u16_at(b, 0x28)?, u16_at(b, 0x2a)?);
    let items_at = virtual_offset(u64_at(b, 0x30)?, len)?;
    let (items, items_capacity) = (u16_at(b, 0x38)?, u16_at(b, 0x3a)?);
    if count == 0 || count != items || capacity < count || items_capacity < items {
        return None;
    }
    let n = count as usize;
    let hashes: Vec<u32> = (0..n).map(|i| u32_at(b, hashes_at + 4 * i)).collect::<Option<_>>()?;
    if hashes.windows(2).any(|w| w[0] >= w[1]) {
        return None;
    }
    let elements: Vec<usize> = (0..n)
        .map(|i| u64_at(b, items_at + 8 * i).and_then(|p| virtual_offset(p, len)))
        .collect::<Option<_>>()?;
    Some(Dictionary { hashes, elements, capacity })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub hash: u32,
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub mips: u8,
    pub dxgi: u8,
    /// Offset of the pixel data in the physical section.
    pub data_offset: u64,
    /// Bytes needed by all mip levels without padding, when the format is
    /// known.
    pub min_len: Option<u64>,
}

/// Name, bytes per 4x4 block (compressed) or per pixel (uncompressed).
pub fn dxgi_format(code: u8) -> Option<(&'static str, u64, bool)> {
    Some(match code {
        2 => ("R32G32B32A32_FLOAT", 16, false),
        10 => ("R16G16B16A16_FLOAT", 8, false),
        24 => ("R10G10B10A2_UNORM", 4, false),
        28 => ("R8G8B8A8_UNORM", 4, false),
        29 => ("R8G8B8A8_UNORM_SRGB", 4, false),
        49 => ("R8G8_UNORM", 2, false),
        56 => ("R16_UNORM", 2, false),
        61 => ("R8_UNORM", 1, false),
        65 => ("A8_UNORM", 1, false),
        71 => ("BC1_UNORM", 8, true),
        72 => ("BC1_UNORM_SRGB", 8, true),
        74 => ("BC2_UNORM", 16, true),
        75 => ("BC2_UNORM_SRGB", 16, true),
        77 => ("BC3_UNORM", 16, true),
        78 => ("BC3_UNORM_SRGB", 16, true),
        80 => ("BC4_UNORM", 8, true),
        81 => ("BC4_SNORM", 8, true),
        83 => ("BC5_UNORM", 16, true),
        84 => ("BC5_SNORM", 16, true),
        87 => ("B8G8R8A8_UNORM", 4, false),
        91 => ("B8G8R8A8_UNORM_SRGB", 4, false),
        95 => ("BC6H_UF16", 16, true),
        96 => ("BC6H_SF16", 16, true),
        98 => ("BC7_UNORM", 16, true),
        99 => ("BC7_UNORM_SRGB", 16, true),
        _ => return None,
    })
}

/// Sum of the unpadded sizes of `mips` levels.
pub fn mip_chain_len(width: u16, height: u16, depth: u16, mips: u8, dxgi: u8) -> Option<u64> {
    let (_, unit, compressed) = dxgi_format(dxgi)?;
    let (mut w, mut h) = (u64::from(width), u64::from(height));
    let mut total = 0;
    for _ in 0..mips {
        total += if compressed { w.div_ceil(4) * h.div_ceil(4) * unit } else { w * h * unit };
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Some(total * u64::from(depth.max(1)))
}

/// Reads a texture element: physical data pointer at `+0x18`, name pointer
/// at `+0x30` (to 8 zero bytes followed by the name), width, height and depth
/// as u16 at `+0x38`, DXGI format at `+0x3f` and mip count at `+0x42`.
pub fn texture(b: &[u8], at: usize, hash: u32) -> Option<Texture> {
    if u64_at(b, at)? != 0 {
        return None;
    }
    let data_offset = physical_offset(u64_at(b, at + 0x18)?)?;
    let name_at = virtual_offset(u64_at(b, at + 0x30)?, b.len())?;
    if u64_at(b, name_at)? != 0 {
        return None;
    }
    let name = c_string(b, name_at + 8)?;
    let (width, height, depth) = (u16_at(b, at + 0x38)?, u16_at(b, at + 0x3a)?, u16_at(b, at + 0x3c)?);
    let (dxgi, mips) = (*b.get(at + 0x3f)?, *b.get(at + 0x42)?);
    let max_mips = 16 - width.max(height).max(1).leading_zeros() as u8;
    if width == 0 || height == 0 || mips == 0 || mips > max_mips {
        return None;
    }
    Some(Texture {
        hash,
        name: String::from_utf8_lossy(name).into_owned(),
        width, height, depth, mips, dxgi, data_offset,
        min_len: mip_chain_len(width, height, depth, mips, dxgi),
    })
}

/// Name referenced by the pointer at `+0xa8` of a drawable-like object,
/// ending in an extension such as `.#dr`.
pub fn named_object(b: &[u8], at: usize) -> Option<(String, String)> {
    let name_at = virtual_offset(u64_at(b, at + 0xa8)?, b.len())?;
    let name = std::str::from_utf8(c_string(b, name_at)?).ok()?;
    let (base, ext) = name.rsplit_once(".#")?;
    (!base.is_empty() && ext.len() == 2 && ext.bytes().all(|c| c.is_ascii_lowercase()))
        .then(|| (name.to_string(), format!("#{ext}")))
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// Dictionary of textures whose names hash to the dictionary keys.
    TextureDictionary(Vec<Texture>),
    /// Dictionary of named objects (such as `.#dr`) whose base names hash to
    /// the dictionary keys.
    NamedDictionary { extension: String, names: Vec<String>, hashes: Vec<u32> },
    /// Dictionary structure valid, element type not identified.
    Dictionary { count: usize },
    /// Root object with a name such as `.#dr` at `+0xa8`.
    Named { extension: String, name: String },
    /// Root and page map recognised, layout not identified.
    Unknown,
}

impl Kind {
    pub fn label(&self) -> String {
        match self {
            Kind::TextureDictionary(_) => "texture_dictionary".into(),
            Kind::NamedDictionary { extension, .. } => format!("dictionary_{extension}"),
            Kind::Dictionary { .. } => "dictionary".into(),
            Kind::Named { extension, .. } => format!("object_{extension}"),
            Kind::Unknown => "unknown_root".into(),
        }
    }

    pub fn count(&self) -> usize {
        match self {
            Kind::TextureDictionary(t) => t.len(),
            Kind::NamedDictionary { names, .. } => names.len(),
            Kind::Dictionary { count } => *count,
            Kind::Named { .. } | Kind::Unknown => 1,
        }
    }
}

/// Classifies a root by walking its observed structures. A dictionary is
/// typed only when every element passes the element check and every key is
/// the joaat hash of the element's name, in the same order.
pub fn classify(b: &[u8]) -> Kind {
    if let Some(dict) = dictionary(b) {
        let textures: Option<Vec<Texture>> = dict.elements.iter().zip(&dict.hashes)
            .map(|(&at, &h)| texture(b, at, h).filter(|t| joaat(t.name.as_bytes()) == h))
            .collect();
        if let Some(textures) = textures {
            return Kind::TextureDictionary(textures);
        }
        let named: Option<Vec<(String, String)>> = dict.elements.iter().zip(&dict.hashes)
            .map(|(&at, &h)| named_object(b, at).filter(|(name, ext)| {
                let base = &name[..name.len() - ext.len() - 1];
                joaat(base.as_bytes()) == h
            }))
            .collect();
        if let Some(named) = named {
            let extension = named[0].1.clone();
            if named.iter().all(|(_, e)| *e == extension) {
                return Kind::NamedDictionary {
                    extension,
                    names: named.into_iter().map(|(n, _)| n).collect(),
                    hashes: dict.hashes,
                };
            }
        }
        return Kind::Dictionary { count: dict.elements.len() };
    }
    match named_object(b, 0) {
        Some((name, extension)) => Kind::Named { extension, name },
        None => Kind::Unknown,
    }
}

/// Checks that every texture's unpadded mip chain lies inside the physical
/// bytes and that no two chains overlap. Returns a description of the first
/// failure.
pub fn check_texture_extents(textures: &[Texture], physical_len: u64) -> Result<(), String> {
    let mut spans = Vec::with_capacity(textures.len());
    for t in textures {
        let len = t.min_len.ok_or_else(|| format!("{}: DXGI format {} not in the size table", t.name, t.dxgi))?;
        let end = t.data_offset + len;
        if end > physical_len {
            return Err(format!("{}: data [{:#x}, {end:#x}) past physical end {physical_len:#x}", t.name, t.data_offset));
        }
        spans.push((t.data_offset, end, &t.name));
    }
    spans.sort();
    for w in spans.windows(2) {
        if w[1].0 < w[0].1 {
            return Err(format!("{} and {} overlap", w[0].2, w[1].2));
        }
    }
    Ok(())
}
