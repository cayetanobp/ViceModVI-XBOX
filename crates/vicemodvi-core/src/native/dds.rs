//! DDS files: validation, subresource layout, preview decoding and the
//! DX10 header that every exporter writes.

use super::{
    bin::{self, Bin, Out},
    dxgi,
    pixels::{self, Pixels},
};
use crate::{Error, Result};

pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SLICES: u64 = 2048;
/// Size of the classic header plus the DX10 extension.
pub const DX10_HEADER_BYTES: usize = 148;

/// One mip of one slice inside the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdsPlane {
    pub slice: i32,
    pub mip: i32,
    pub width: i32,
    pub height: i32,
    pub depth: i32,
    pub offset: i32,
    pub size: i32,
    pub row_bytes: i32,
    pub slice_bytes: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdsInfo {
    pub width: i32,
    pub height: i32,
    pub depth: i32,
    /// 1 = 2D or array, 2 = cube or cube array, 3 = volume.
    pub dimension: i32,
    /// D3D10 resource dimension: 2 = 1D, 3 = 2D, 4 = 3D.
    pub resource_dimension: i32,
    pub array_size: i32,
    pub slice_count: i32,
    pub mips: i32,
    pub dxgi_format: i32,
    pub format: String,
    pub data_offset: i32,
    pub data_size: i32,
    pub has_dx10_header: bool,
    pub alpha_mode: i32,
    pub subresources: Vec<DdsPlane>,
}

impl DdsInfo {
    pub fn preview_mip_level(&self) -> i32 {
        dxgi::preview_level(self.width, self.height, self.mips)
    }

    pub fn preview_width(&self) -> i32 {
        dxgi::mip_extent(self.width, self.preview_mip_level())
    }

    pub fn preview_height(&self) -> i32 {
        dxgi::mip_extent(self.height, self.preview_mip_level())
    }

    pub fn shape(&self) -> String {
        match self.dimension {
            3 => format!("3D volume ({} slices)", self.depth),
            2 => format!("Cube array ({} cubes)", self.array_size),
            _ if self.slice_count > 1 => format!("2D array ({} slices)", self.slice_count),
            _ if self.resource_dimension == 2 => "1D".into(),
            _ => "2D".into(),
        }
    }

    /// The texel bytes after the header.
    pub fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        data.get(self.data_offset as usize..).unwrap_or_default()
    }
}

fn too_large() -> Error {
    Error::invalid("DDS dimensions or subresource sizes overflow the supported limits.")
}

fn extent(value: u32) -> Result<i32> {
    if (1..=16384).contains(&value) {
        Ok(value as i32)
    } else {
        Err(Error::invalid("DDS dimension is invalid or exceeds 16384."))
    }
}

fn small(value: u32) -> Result<i32> {
    i32::try_from(value).map_err(|_| too_large())
}

/// Shape decided by the header before the subresources are laid out.
struct Shape {
    format: i32,
    alpha_mode: i32,
    dimension: i32,
    resource_dimension: i32,
    array: u64,
    slices: u64,
    depth: i32,
    start: usize,
}

const DDPF_FOURCC: u32 = 0x4;
const DX10: u32 = u32::from_le_bytes(*b"DX10");
const CUBE_FLAG: u32 = 0x4;

/// Validates a DDS file and lists its subresources.
pub fn inspect(data: &[u8]) -> Result<DdsInfo> {
    let file = Bin(data);
    if data.len() < 128 || data.len() as u64 > MAX_FILE_BYTES || &data[..4] != b"DDS " || file.u32(4)? != 124 || file.u32(76)? != 32 {
        return Err(Error::invalid("Invalid, truncated or oversized DDS header."));
    }
    let width = extent(file.u32(16)?)?;
    let height = extent(file.u32(12)?)?;
    let mips = small(file.u32(28)?)?.max(1);
    let flags = file.u32(80)?;
    let four_cc = file.u32(84)?;
    let dx10 = flags & DDPF_FOURCC != 0 && four_cc == DX10;
    let shape = if dx10 { dx10_shape(file, width, height)? } else { legacy_shape(file, width, height, flags, four_cc)? };
    if shape.slices > MAX_SLICES {
        return Err(Error::not_supported("DDS has too many preview slices."));
    }
    let largest = width.max(height).max(if shape.dimension == 3 { shape.depth } else { 1 });
    if mips as i64 > dxgi::full_chain(largest as i64) {
        return Err(Error::invalid("DDS mip count exceeds its dimensions."));
    }
    let subresources = layout(data.len(), &shape, width, height, mips)?;
    Ok(DdsInfo {
        width,
        height,
        depth: shape.depth,
        dimension: shape.dimension,
        resource_dimension: shape.resource_dimension,
        array_size: shape.array as i32,
        slice_count: shape.slices as i32,
        mips,
        dxgi_format: shape.format,
        format: dxgi::name(shape.format).to_string(),
        data_offset: shape.start as i32,
        data_size: (data.len() - shape.start) as i32,
        has_dx10_header: dx10,
        alpha_mode: shape.alpha_mode,
        subresources,
    })
}

fn dx10_shape(file: Bin, width: i32, height: i32) -> Result<Shape> {
    if file.len() < DX10_HEADER_BYTES {
        return Err(Error::invalid("Truncated DDS DX10 extension."));
    }
    let format = small(file.u32(128)?)?;
    let resource_dimension = small(file.u32(132)?)?;
    let cube = file.u32(136)? & CUBE_FLAG != 0;
    let array = small(file.u32(140)?)? as u64;
    let alpha_mode = (file.u32(144)? & 7) as i32;
    if !(1..=MAX_SLICES).contains(&array) || alpha_mode > 4 {
        return Err(Error::invalid("Invalid DDS array or alpha mode."));
    }
    let mut shape = Shape { format, alpha_mode, dimension: 1, resource_dimension, array, slices: array, depth: array as i32, start: DX10_HEADER_BYTES };
    match resource_dimension {
        2 if height == 1 && !cube => {}
        2 => return Err(Error::invalid("DDS 1D texture shape is inconsistent.")),
        3 => {
            if cube {
                if width != height {
                    return Err(Error::invalid("Cube faces must be square."));
                }
                shape.dimension = 2;
                shape.slices = array * 6;
                shape.depth = shape.slices as i32;
            }
        }
        4 if array == 1 && !cube => {
            shape.dimension = 3;
            shape.depth = extent(file.u32(24)?)?;
            shape.slices = shape.depth as u64;
        }
        4 => return Err(Error::invalid("DDS volume cannot be an array or cube.")),
        _ => return Err(Error::not_supported("Unsupported DDS resource dimension.")),
    }
    Ok(shape)
}

fn legacy_shape(file: Bin, width: i32, height: i32, flags: u32, four_cc: u32) -> Result<Shape> {
    let (format, alpha_mode) = legacy_format(file, flags, four_cc)?;
    let caps2 = file.u32(112)?;
    let volume = caps2 & 0x20_0000 != 0;
    let cube = caps2 & 0x200 != 0;
    let mut shape = Shape { format, alpha_mode, dimension: 1, resource_dimension: 3, array: 1, slices: 1, depth: 1, start: 128 };
    match (volume, cube) {
        (true, true) => return Err(Error::invalid("DDS cannot be both a cube and a volume.")),
        (true, false) => {
            shape.dimension = 3;
            shape.resource_dimension = 4;
            shape.depth = extent(file.u32(24)?)?;
            shape.slices = shape.depth as u64;
        }
        (false, true) => {
            if caps2 & 0xFC00 != 0xFC00 || width != height {
                return Err(Error::not_supported("Incomplete or nonsquare legacy cube maps are not supported."));
            }
            shape.dimension = 2;
            shape.depth = 6;
            shape.slices = 6;
        }
        (false, false) => {}
    }
    Ok(shape)
}

fn layout(file_length: usize, shape: &Shape, width: i32, height: i32, mips: i32) -> Result<Vec<DdsPlane>> {
    let element = dxgi::element_size_of(shape.format)?;
    let side = dxgi::block_side(shape.format);
    let volume = shape.dimension == 3;
    let mut planes = Vec::new();
    let mut offset = shape.start as u64;
    for slice in 0..if volume { 1 } else { shape.slices } {
        for mip in 0..mips {
            let w = dxgi::mip_extent(width, mip) as u64;
            let h = dxgi::mip_extent(height, mip) as u64;
            let d = if volume { dxgi::mip_extent(shape.depth, mip) as u64 } else { 1 };
            let row = w.div_ceil(side) * element;
            let plane = row * h.div_ceil(side);
            let size = plane * d;
            if plane > i32::MAX as u64 {
                return Err(too_large());
            }
            if size > MAX_FILE_BYTES || offset + size > file_length as u64 {
                return Err(Error::invalid("DDS mip/slice data is truncated or exceeds the output limit."));
            }
            planes.push(DdsPlane {
                slice: slice as i32,
                mip,
                width: w as i32,
                height: h as i32,
                depth: d as i32,
                offset: offset as i32,
                size: size as i32,
                row_bytes: row as i32,
                slice_bytes: plane as i32,
            });
            offset += size;
        }
    }
    if offset != file_length as u64 {
        return Err(Error::invalid("DDS has trailing bytes or inconsistent declared mip/slice data."));
    }
    Ok(planes)
}

/// DXGI format and alpha mode of a header without the DX10 extension.
fn legacy_format(file: Bin, flags: u32, four_cc: u32) -> Result<(i32, i32)> {
    if flags & DDPF_FOURCC != 0 {
        return Ok(match &four_cc.to_le_bytes() {
            b"DXT1" => (71, 0),
            b"DXT2" => (74, 2),
            b"DXT3" => (74, 0),
            b"DXT4" => (77, 2),
            b"DXT5" => (77, 0),
            b"ATI1" | b"BC4U" => (80, 0),
            b"BC4S" => (81, 0),
            b"ATI2" | b"BC5U" => (83, 0),
            b"BC5S" => (84, 0),
            // D3DFMT codes stored in the FourCC field.
            [111, 0, 0, 0] => (54, 0),
            [112, 0, 0, 0] => (34, 0),
            [113, 0, 0, 0] => (10, 0),
            [114, 0, 0, 0] => (41, 0),
            _ => return Err(Error::not_supported("Unsupported legacy DDS FOURCC. Use a supported DX10 DDS format.")),
        });
    }
    let bits = file.u32(88)?;
    let masks = [file.u32(92)?, file.u32(96)?, file.u32(100)?, file.u32(104)?];
    const RGB: u32 = 0x40;
    const LUMINANCE: u32 = 0x2_0000;
    const ALPHA: u32 = 0x2;
    let found = match (bits, masks) {
        (32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000]) if flags & RGB != 0 => (28, 1),
        (32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000]) if flags & RGB != 0 => (87, 1),
        (32, [0xFF_0000, 0xFF00, 0xFF, 0]) if flags & RGB != 0 => (88, 3),
        (32, [0x3FF, 0xF_FC00, 0x3FF0_0000, 0xC000_0000]) if flags & RGB != 0 => (24, 1),
        (8, [0xFF, 0, 0, 0]) if flags & LUMINANCE != 0 => (61, 0),
        (16, [0xFFFF, 0, 0, 0]) if flags & LUMINANCE != 0 => (56, 0),
        (8, [_, _, _, 0xFF]) if flags & ALPHA != 0 => (65, 0),
        _ => return Err(Error::not_supported("Unsupported legacy DDS pixel masks.")),
    };
    Ok(found)
}

/// Decodes the preview mip of one slice to BGRA. Premultiplied alpha
/// (mode 2) is converted back to straight alpha.
pub fn decode_texture_pixels(data: &[u8], slice: i32) -> Result<Pixels> {
    let info = inspect(data)?;
    if !(0..info.slice_count).contains(&slice) {
        return Err(Error::argument("The slice index is outside the DDS."));
    }
    let mip = info.preview_mip_level();
    let z = if info.dimension == 3 { (dxgi::mip_extent(info.depth, mip) - 1).min(slice >> mip) } else { slice };
    let mut pixels = pixels::decode(mip_plane(data, &info, mip, z)?, info.preview_width(), info.preview_height(), info.dxgi_format)?;
    if info.alpha_mode == 2 {
        for texel in pixels.bgra32.chunks_exact_mut(4) {
            let alpha = texel[3] as u32;
            for channel in &mut texel[..3] {
                *channel = (*channel as u32 * 255 + alpha / 2).checked_div(alpha).map_or(0, |v| v.min(255) as u8);
            }
        }
    }
    Ok(pixels)
}

/// Bytes of one mip of one slice (or one depth slice of a volume mip).
pub fn mip_plane<'a>(data: &'a [u8], info: &DdsInfo, mip: i32, slice: i32) -> Result<&'a [u8]> {
    if !(0..info.mips).contains(&mip) {
        return Err(Error::argument("The mip level is outside the DDS."));
    }
    let plane = if info.dimension == 3 {
        let level = &info.subresources[mip as usize];
        if !(0..level.depth).contains(&slice) {
            return Err(Error::argument("The slice index is outside the DDS mip."));
        }
        (level.offset as usize + slice as usize * level.slice_bytes as usize, level.slice_bytes as usize)
    } else {
        if !(0..info.slice_count).contains(&slice) {
            return Err(Error::argument("The slice index is outside the DDS."));
        }
        let level = info
            .subresources
            .get((slice * info.mips + mip) as usize)
            .ok_or_else(|| Error::argument("The subresource is outside the DDS."))?;
        (level.offset as usize, level.size as usize)
    };
    Ok(&data[bin::range(data.len(), plane.0, plane.1)?])
}

/// How the slices of an exported texture are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layers {
    /// `n` 2D slices (1 for a plain texture).
    Flat(i32),
    /// `faces` cube faces, a multiple of six.
    Cube(i32),
    /// A volume of `depth` slices.
    Volume(i32),
}

/// Writes a DX10 DDS header for a texture whose payload follows directly.
pub fn dx10_header(width: i32, height: i32, layers: Layers, mips: i32, format: i32) -> Result<Vec<u8>> {
    let element = dxgi::element_size_of(format)? as u32;
    let compressed = dxgi::is_compressed(format);
    let (volume, cube, slices) = match layers {
        Layers::Flat(n) => (false, false, n),
        Layers::Cube(n) => (false, true, n),
        Layers::Volume(n) => (true, false, n),
    };
    // DDSD_CAPS | HEIGHT | WIDTH | PIXELFORMAT | MIPMAPCOUNT, plus
    // LINEARSIZE for compressed data or PITCH otherwise, plus DEPTH.
    let mut flags = if compressed { 0x000A_1007 } else { 0x0002_100F };
    if volume {
        flags |= 0x80_0000;
    }
    let pitch = if compressed {
        (width as u32).div_ceil(4) * (height as u32).div_ceil(4) * element
    } else {
        width as u32 * element
    };
    let mut caps = if mips > 1 { 0x40_1008 } else { 0x1000 };
    if volume || cube || slices > 1 {
        caps |= 0x8;
    }
    let caps2 = if volume {
        0x20_0000
    } else if cube {
        0xFE00
    } else {
        0
    };
    let array = match layers {
        Layers::Flat(n) => n,
        Layers::Cube(n) => n / 6,
        Layers::Volume(_) => 1,
    };
    let mut out = Out::with_capacity(DX10_HEADER_BYTES);
    out.raw(b"DDS ").u32(124).u32(flags).u32(height as u32).u32(width as u32).u32(pitch);
    out.u32(if volume { slices as u32 } else { 0 }).u32(mips as u32).zeros(44);
    out.u32(32).u32(DDPF_FOURCC).u32(DX10).zeros(20);
    out.u32(caps).u32(caps2).zeros(12);
    out.u32(format as u32).u32(if volume { 4 } else { 3 }).u32(if cube { CUBE_FLAG } else { 0 }).u32(array as u32).u32(0);
    Ok(out.0)
}

/// One-mip B8G8R8A8 DDS for decoded preview pixels.
pub fn from_pixels(pixels: &Pixels) -> Vec<u8> {
    let mut out = dx10_header(pixels.width, pixels.height, Layers::Flat(1), 1, 87).unwrap_or_default();
    out.extend_from_slice(&pixels.bgra32);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 2D DX10 DDS with the given mip payloads.
    pub(crate) fn build(width: i32, height: i32, format: i32, levels: &[Vec<u8>], alpha: u32) -> Vec<u8> {
        let mut out = dx10_header(width, height, Layers::Flat(1), levels.len() as i32, format).unwrap_or_else(|_| {
            // Unknown formats still get a structurally valid header.
            dx10_header(width, height, Layers::Flat(1), levels.len() as i32, 28).unwrap()
        });
        out[128..132].copy_from_slice(&(format as u32).to_le_bytes());
        out[144..148].copy_from_slice(&alpha.to_le_bytes());
        for level in levels {
            out.extend_from_slice(level);
        }
        out
    }

    #[test]
    fn lists_a_mip_chain() {
        let data = build(8, 4, 28, &[vec![0; 128], vec![0; 32], vec![0; 8], vec![0; 4]], 0);
        let info = inspect(&data).unwrap();
        assert_eq!((info.width, info.height, info.mips, info.dxgi_format), (8, 4, 4, 28));
        assert_eq!(info.subresources[1].offset, 148 + 128);
        assert_eq!(info.data_size, 172);
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut data = build(4, 4, 28, &[vec![0; 64]], 0);
        data.push(0);
        assert!(inspect(&data).is_err());
    }

    #[test]
    fn unknown_formats_are_unsupported() {
        let data = build(4, 4, 200, &[vec![0; 64]], 0);
        assert!(matches!(inspect(&data), Err(Error::NotSupported(_))));
    }

    #[test]
    fn premultiplied_alpha_is_restored() {
        let data = build(1, 1, 87, &[vec![64, 64, 64, 128]], 2);
        assert_eq!(decode_texture_pixels(&data, 0).unwrap().bgra32, vec![128, 128, 128, 128]);
    }

    #[test]
    fn preview_dds_reopens() {
        let pixels = Pixels { width: 2, height: 1, bgra32: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        let dds = from_pixels(&pixels);
        assert_eq!(decode_texture_pixels(&dds, 0).unwrap(), pixels);
    }
}
