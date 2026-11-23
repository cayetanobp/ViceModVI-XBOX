//! DXGI format facts used by the texture readers and writers.

use crate::{Error, Result};

/// Previews decode the largest mip that fits in 16 megapixels.
pub const PREVIEW_PIXELS: u64 = 16 * 1024 * 1024;

/// Block-compressed formats store 4×4 pixel blocks.
pub fn is_compressed(format: i32) -> bool {
    matches!(format, 70..=84 | 94..=99)
}

/// Pixels per element along each axis.
pub fn block_side(format: i32) -> u64 {
    if is_compressed(format) {
        4
    } else {
        1
    }
}

/// Bytes per element (a pixel, or a block for compressed formats).
pub fn element_size(format: i32) -> Option<u64> {
    Some(match format {
        61 | 65 => 1,
        49 | 54 | 56 => 2,
        24 | 28 | 29 | 34 | 41 | 87 | 88 | 91 | 93 => 4,
        10 | 71 | 72 | 80 | 81 => 8,
        74 | 75 | 77 | 78 | 83 | 84 | 95 | 96 | 98 | 99 => 16,
        _ => return None,
    })
}

/// [`element_size`] that reports unknown formats as unsupported.
pub fn element_size_of(format: i32) -> Result<u64> {
    element_size(format).ok_or_else(|| Error::not_supported(format!("DXGI format {format} is not supported.")))
}

/// log2 of an element size: 0 for 1 byte up to 4 for 16 bytes.
pub(crate) fn size_class(element: u64) -> Result<usize> {
    match element {
        1 | 2 | 4 | 8 | 16 => Ok(element.trailing_zeros() as usize),
        _ => Err(Error::invalid("Invalid texel element size.")),
    }
}

pub fn name(format: i32) -> &'static str {
    match format {
        10 => "R16G16B16A16_FLOAT",
        24 => "R10G10B10A2_UNORM",
        28 => "R8G8B8A8_UNORM",
        29 => "R8G8B8A8_UNORM_SRGB",
        34 => "R16G16_FLOAT",
        41 => "R32_FLOAT",
        49 => "R8G8_UNORM",
        54 => "R16_FLOAT",
        56 => "R16_UNORM",
        61 => "R8_UNORM",
        65 => "A8_UNORM",
        71 => "BC1_UNORM",
        72 => "BC1_UNORM_SRGB",
        74 => "BC2_UNORM",
        75 => "BC2_UNORM_SRGB",
        77 => "BC3_UNORM",
        78 => "BC3_UNORM_SRGB",
        80 => "BC4_UNORM",
        81 => "BC4_SNORM",
        83 => "BC5_UNORM",
        84 => "BC5_SNORM",
        87 => "B8G8R8A8_UNORM",
        88 => "B8G8R8X8_UNORM",
        91 => "B8G8R8A8_UNORM_SRGB",
        93 => "B8G8R8X8_UNORM_SRGB",
        95 => "BC6H_UF16",
        96 => "BC6H_SF16",
        98 => "BC7_UNORM",
        99 => "BC7_UNORM_SRGB",
        _ => "Unknown",
    }
}

/// Size of `pixels` at `level`, never below one.
pub fn mip_extent(pixels: i32, level: i32) -> i32 {
    (pixels >> level).max(1)
}

/// Number of mip levels in a full chain for the largest extent.
pub fn full_chain(largest: i64) -> i64 {
    64 - (largest.max(1) as u64).leading_zeros() as i64
}

/// First mip level that fits [`PREVIEW_PIXELS`], or the last level.
pub fn preview_level(width: i32, height: i32, mips: i32) -> i32 {
    (0..mips.max(1))
        .find(|&level| mip_extent(width, level) as u64 * mip_extent(height, level) as u64 <= PREVIEW_PIXELS)
        .unwrap_or(mips.max(1) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_and_preview_levels() {
        assert_eq!(full_chain(1), 1);
        assert_eq!(full_chain(256), 9);
        assert_eq!(full_chain(300), 9);
        assert_eq!(preview_level(4096, 4096, 13), 0);
        assert_eq!(preview_level(8192, 4096, 14), 1);
        assert_eq!(preview_level(16384, 16384, 1), 0);
        assert_eq!(size_class(16).unwrap(), 4);
    }
}
