//! Converts linear texel data to 8-bit BGRA for previews.

use half::f16;

use super::{
    bin::Bin,
    dxgi::{self, PREVIEW_PIXELS},
};
use crate::{Error, Result};

/// Decoded 8-bit BGRA pixels, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: i32,
    pub height: i32,
    pub bgra32: Vec<u8>,
}

/// Maps 0..1 to 0..255 with ties to even. NaN becomes 0.
fn unit(value: f64) -> u8 {
    if value.is_nan() {
        0
    } else {
        (value.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
    }
}

fn half_at(data: Bin, at: usize) -> Result<f64> {
    Ok(f16::from_bits(data.u16(at)?).to_f64())
}

/// Decodes one linear mip plane of `width` × `height` pixels.
pub fn decode(linear: &[u8], width: i32, height: i32, format: i32) -> Result<Pixels> {
    if width < 1 || height < 1 || width as u64 * height as u64 > PREVIEW_PIXELS {
        return Err(Error::not_supported("The preview exceeds the 16-megapixel output limit."));
    }
    let side = dxgi::block_side(format);
    let element = dxgi::element_size_of(format)?;
    let expected = (width as u64).div_ceil(side) * (height as u64).div_ceil(side) * element;
    if linear.len() as u64 != expected {
        return Err(Error::invalid("Linear texture data length disagrees with its descriptor."));
    }
    let (w, h) = (width as usize, height as usize);
    let bgra32 = if dxgi::is_compressed(format) {
        // The block decoders index fixed-size buffers; a bug in them must not
        // take down the viewer, so a panic becomes a decode error.
        std::panic::catch_unwind(|| decode_blocks(linear, w, h, format, element as usize))
            .map_err(|_| Error::invalid("The block-compressed data could not be decoded."))??
    } else {
        decode_plain(Bin(linear), w * h, format, element as usize)?
    };
    Ok(Pixels { width, height, bgra32 })
}

/// Expands one block into 16 RGBA8 texels.
fn expand_block(block: &[u8], format: i32, rgba: &mut [u8; 64]) -> Result<()> {
    match format {
        71 | 72 => bcdec_rs::bc1(block, rgba, 16),
        74 | 75 => bcdec_rs::bc2(block, rgba, 16),
        77 | 78 => bcdec_rs::bc3(block, rgba, 16),
        98 | 99 => bcdec_rs::bc7(block, rgba, 16),
        80 => {
            let mut red = [0u8; 16];
            bcdec_rs::bc4(block, &mut red, 4, false);
            for (texel, value) in rgba.chunks_exact_mut(4).zip(red) {
                texel.copy_from_slice(&[value, 0, 0, 255]);
            }
        }
        83 => {
            let mut red_green = [0u8; 32];
            bcdec_rs::bc5(block, &mut red_green, 8, false);
            for (texel, pair) in rgba.chunks_exact_mut(4).zip(red_green.chunks_exact(2)) {
                texel.copy_from_slice(&[pair[0], pair[1], 0, 255]);
            }
        }
        95 | 96 => {
            let mut rgb = [0f32; 48];
            bcdec_rs::bc6h_float(block, &mut rgb, 12, format == 96);
            for (texel, color) in rgba.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
                texel.copy_from_slice(&[unit(color[0] as f64), unit(color[1] as f64), unit(color[2] as f64), 255]);
            }
        }
        _ => {
            return Err(Error::not_supported(
                "This signed or HDR BC format supports DDS export but not this 8-bit preview.",
            ))
        }
    }
    Ok(())
}

fn decode_blocks(linear: &[u8], width: usize, height: usize, format: i32, block_bytes: usize) -> Result<Vec<u8>> {
    let columns = width.div_ceil(4);
    let mut out = vec![0u8; width * height * 4];
    let mut rgba = [0u8; 64];
    for (number, block) in linear.chunks_exact(block_bytes).enumerate() {
        expand_block(block, format, &mut rgba)?;
        let (left, top) = (number % columns * 4, number / columns * 4);
        // Edge blocks are clipped to the image.
        for row in 0..4.min(height - top) {
            for column in 0..4.min(width - left) {
                let texel = &rgba[(row * 4 + column) * 4..][..4];
                let at = ((top + row) * width + left + column) * 4;
                out[at..at + 4].copy_from_slice(&[texel[2], texel[1], texel[0], texel[3]]);
            }
        }
    }
    Ok(out)
}

/// One uncompressed texel as RGBA.
fn plain_texel(data: Bin, at: usize, format: i32) -> Result<[u8; 4]> {
    let gray = |v: u8| [v, v, v, 255];
    Ok(match format {
        28 | 29 => {
            let t = data.bytes(at, 4)?;
            [t[0], t[1], t[2], t[3]]
        }
        87 | 91 => {
            let t = data.bytes(at, 4)?;
            [t[2], t[1], t[0], t[3]]
        }
        88 | 93 => {
            let t = data.bytes(at, 4)?;
            [t[2], t[1], t[0], 255]
        }
        61 | 65 => gray(data.u8(at)?),
        49 => {
            let t = data.bytes(at, 2)?;
            [t[0], t[1], 0, 255]
        }
        56 => gray(unit(data.u16(at)? as f64 / 65535.0)),
        54 => gray(unit(half_at(data, at)?)),
        41 => gray(unit(data.f32(at)? as f64)),
        34 => [unit(half_at(data, at)?), unit(half_at(data, at + 2)?), 0, 255],
        10 => [
            unit(half_at(data, at)?),
            unit(half_at(data, at + 2)?),
            unit(half_at(data, at + 4)?),
            unit(half_at(data, at + 6)?),
        ],
        24 => {
            let packed = data.u32(at)?;
            let channel = |shift: u32| unit(((packed >> shift) & 1023) as f64 / 1023.0);
            [channel(0), channel(10), channel(20), ((packed >> 30) * 85) as u8]
        }
        _ => return Err(Error::not_supported("This raw format has no pixel decoder.")),
    })
}

fn decode_plain(data: Bin, count: usize, format: i32, element: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(count * 4);
    for texel in 0..count {
        let [r, g, b, a] = plain_texel(data, texel * element, format)?;
        out.extend_from_slice(&[b, g, r, a]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_becomes_bgra() {
        let pixels = decode(&[1, 2, 3, 4, 5, 6, 7, 8], 2, 1, 28).unwrap();
        assert_eq!(pixels.bgra32, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn partial_bc1_blocks_are_clipped() {
        let white = [0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0];
        let pixels = decode(&white, 3, 2, 71).unwrap();
        assert_eq!(pixels.bgra32.len(), 3 * 2 * 4);
        assert!(pixels.bgra32.iter().all(|&v| v == 255));
    }

    #[test]
    fn wrong_lengths_are_rejected() {
        assert!(decode(&[0; 7], 2, 1, 28).is_err());
    }
}
