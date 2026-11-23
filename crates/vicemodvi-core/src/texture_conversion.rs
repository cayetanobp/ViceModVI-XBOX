//! Imports replacement images and converts DDS files to a target shape.

use std::{fmt, io::Cursor, path::Path};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use rayon::prelude::*;
use serde::Serialize;

use crate::{
    dds::{self, DdsTextureInfo},
    resample::{self, Filter, Image},
    util::extension,
    Error, Result,
};

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PIXELS: i64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureShape {
    pub width: i32,
    pub height: i32,
    pub dxgi_format: i32,
    pub mips: i32,
}

impl TextureShape {
    pub fn new(width: i32, height: i32, dxgi_format: i32, mips: i32) -> Self {
        Self { width, height, dxgi_format, mips }
    }

    pub fn from_dds(texture: &DdsTextureInfo) -> Self {
        Self::new(texture.width, texture.height, texture.dxgi_format, texture.mips)
    }
}

impl fmt::Display for TextureShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} × {} · DXGI {} · {} mips", self.width, self.height, self.dxgi_format, self.mips)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureConversionPlan {
    pub input: TextureShape,
    pub output: TextureShape,
    pub requires_resize: bool,
    pub requires_conversion: bool,
    pub warnings: Vec<String>,
    pub limitation: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Raster {
    Image(ImageFormat),
    Psd,
}

/// Image extensions that [`import`] decodes, besides `.dds`.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "apng", "bmp", "dib", "exr", "gif", "hdr", "ico", "jfif", "jpeg", "jpg", "pam", "pbm", "pgm", "png", "pnm", "ppm", "psd", "qoi",
    "tga", "tif", "tiff", "webp",
];

fn raster_format(extension: &str) -> Option<Raster> {
    Some(Raster::Image(match extension {
        ".png" | ".apng" => ImageFormat::Png,
        ".jpg" | ".jpeg" | ".jfif" => ImageFormat::Jpeg,
        ".bmp" | ".dib" => ImageFormat::Bmp,
        ".gif" => ImageFormat::Gif,
        ".tiff" | ".tif" => ImageFormat::Tiff,
        ".tga" => ImageFormat::Tga,
        ".webp" => ImageFormat::WebP,
        ".qoi" => ImageFormat::Qoi,
        ".ico" => ImageFormat::Ico,
        ".exr" => ImageFormat::OpenExr,
        ".hdr" => ImageFormat::Hdr,
        ".pbm" | ".pgm" | ".ppm" | ".pnm" | ".pam" => ImageFormat::Pnm,
        ".psd" => return Some(Raster::Psd),
        _ => return None,
    }))
}

/// Reads a replacement as DDS. Native DDS files are kept as they are;
/// other images become a one-mip B8G8R8A8_UNORM_SRGB DDS.
pub fn import(path: &Path) -> Result<Vec<u8>> {
    let length = std::fs::metadata(path)?.len();
    if length == 0 || length > MAX_FILE_BYTES {
        return Err(Error::invalid("Replacement files must be between 1 byte and 256 MiB."));
    }
    let bytes = std::fs::read(path)?;
    if bytes.starts_with(b"DDS ") {
        return match dds::inspect(&bytes) {
            Ok(_) => Ok(bytes),
            Err(rpf9_explorer::Error::NotSupported(_)) => read_raster(&bytes, Raster::Image(ImageFormat::Dds)),
            Err(rpf9_explorer::Error::InvalidData(message)) if message == "Unsupported BC format." => {
                read_raster(&bytes, Raster::Image(ImageFormat::Dds))
            }
            Err(error) => Err(error.into()),
        };
    }
    let ext = extension(path);
    if ext == ".dds" {
        return Err(Error::invalid("The file does not start with the DDS signature."));
    }
    let format = raster_format(&ext)
        .ok_or_else(|| Error::not_supported(format!("No raster decoder is configured for '{ext}'. Choose a supported image format.")))?;
    read_raster(&bytes, format)
}

fn check_pixel_count(width: u64, height: u64, label: &str) -> Result<()> {
    if width == 0 || width > 16384 || height == 0 || height > 16384 || (width * height) as i64 > MAX_PIXELS {
        return Err(Error::not_supported(format!("{label} exceeds the 16 megapixel or 16,384 pixels per side limit.")));
    }
    Ok(())
}

fn format_label(format: Raster) -> String {
    match format {
        Raster::Psd => "Psd".into(),
        Raster::Image(f) => format!("{f:?}"),
    }
}

fn read_raster(bytes: &[u8], format: Raster) -> Result<Vec<u8>> {
    let failed = |message: String| Error::invalid(format!("Could not decode {}: {message}", format_label(format)));
    let (width, height, mut rgba) = match format {
        Raster::Psd => {
            let psd = psd::Psd::from_bytes(bytes).map_err(|e| failed(e.to_string()))?;
            check_pixel_count(psd.width() as u64, psd.height() as u64, "Imported image")?;
            (psd.width(), psd.height(), psd.rgba())
        }
        Raster::Image(image_format) => {
            let mut decoder = ImageReader::with_format(Cursor::new(bytes), image_format).into_decoder().map_err(|e| failed(e.to_string()))?;
            let (w, h) = decoder.dimensions();
            check_pixel_count(w as u64, h as u64, "Imported image")?;
            let orientation = decoder.orientation().map_err(|e| failed(e.to_string()))?;
            let mut image = DynamicImage::from_decoder(decoder).map_err(|e| failed(e.to_string()))?;
            image.apply_orientation(orientation);
            let rgba = image.to_rgba8();
            (rgba.width(), rgba.height(), rgba.into_raw())
        }
    };
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(failed("The decoded pixel count disagrees with the image size.".into()));
    }
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    build_dds(TextureShape::new(width as i32, height as i32, 91, 1), &[rgba], 0, 0)
}

pub fn plan(dds_bytes: &[u8], output: TextureShape, limitation: Option<String>) -> Result<TextureConversionPlan> {
    let info = dds::inspect(dds_bytes)?;
    let input = TextureShape::from_dds(&info);
    let straight = matches!(info.alpha_mode, 0 | 1);
    let resize = input.width != output.width || input.height != output.height;
    let conversion = input != output || !straight;
    let keep_mips = !resize && input.dxgi_format == output.dxgi_format && straight;
    let needs_encoding = conversion && (!keep_mips || input.mips < output.mips);
    let mut limitation = limitation;
    let mut limit = |message: String| {
        limitation.get_or_insert(message);
    };
    if info.dimension != 1 || info.depth != 1 || info.slice_count != 1 || info.resource_dimension != 3 {
        limit(format!("The writer requires one 2D image. The imported DDS contains {}.", info.shape()));
    }
    if !(0..=3).contains(&info.alpha_mode) {
        limit(format!("DDS alpha mode {} has no defined conversion to straight alpha.", info.alpha_mode));
    }
    if needs_encoding {
        if matches!(input.dxgi_format, 81 | 84) {
            limit(format!("Decoding DXGI {} signed BC pixels for conversion is unavailable.", input.dxgi_format));
        }
        if bytes_per_element(output.dxgi_format) == 0 {
            limit(format!("Encoding DXGI {} is unavailable. A matching native DDS can be used directly.", output.dxgi_format));
        }
        if input.width as i64 * input.height as i64 > MAX_PIXELS || output.width as i64 * output.height as i64 > MAX_PIXELS {
            limit("Texture conversion supports up to 16 megapixels at mip 0.".into());
        }
    }
    let mut warnings = Vec::new();
    if resize {
        warnings.push("Resizing changes the sampled detail. A change of aspect ratio can stretch the texture.".into());
    }
    if input.dxgi_format != output.dxgi_format {
        warnings.push("Changing DXGI format can alter precision, compressed detail, or alpha coverage.".into());
    }
    if needs_encoding && matches!(input.dxgi_format, 10 | 34 | 41 | 54 | 56 | 95 | 96) {
        warnings.push("Conversion samples the image at 8-bit precision and clips values outside 0–1.".into());
    }
    if needs_encoding && output.mips > 1 {
        warnings.push(
            if keep_mips { "Missing mip levels are generated from the last imported mip." } else { "The output mip chain is regenerated from mip 0." }
                .into(),
        );
    }
    if keep_mips && input.mips > output.mips {
        warnings.push("The output keeps the first mip levels needed by the pool.".into());
    }
    Ok(TextureConversionPlan { input, output, requires_resize: resize, requires_conversion: conversion, warnings, limitation })
}

pub fn apply(dds_bytes: &[u8], plan: &TextureConversionPlan, allow_resize: bool) -> Result<Vec<u8>> {
    let info = dds::inspect(dds_bytes)?;
    if TextureShape::from_dds(&info) != plan.input {
        return Err(Error::invalid("The input DDS changed after the conversion was planned."));
    }
    if let Some(limitation) = &plan.limitation {
        return Err(Error::not_supported(limitation.clone()));
    }
    if plan.requires_resize && !allow_resize {
        return Err(Error::invalid(format!(
            "The writer requires {} × {}; the input is {} × {}. Confirm resizing with --resize.",
            plan.output.width, plan.output.height, plan.input.width, plan.input.height
        )));
    }
    if !plan.requires_conversion {
        return Ok(dds_bytes.to_vec());
    }
    let target = plan.output;
    let mut levels: Vec<Vec<u8>> = Vec::with_capacity(target.mips as usize);
    let mut reused = 0usize;
    if !plan.requires_resize && info.dxgi_format == target.dxgi_format && matches!(info.alpha_mode, 0 | 1) {
        reused = info.mips.min(target.mips) as usize;
        for mip in &info.subresources[..reused] {
            levels.push(dds_bytes[mip.offset as usize..(mip.offset + mip.size) as usize].to_vec());
        }
        if reused == target.mips as usize {
            return build_dds(target, &levels, dds_element_size(&info), info.alpha_mode);
        }
    }
    let seed = &info.subresources[reused.saturating_sub(1)];
    let seed_shape = TextureShape::new(seed.width, seed.height, info.dxgi_format, 1);
    let seed_data = dds_bytes[seed.offset as usize..(seed.offset + seed.size) as usize].to_vec();
    let top = build_dds(seed_shape, &[seed_data], dds_element_size(&info), info.alpha_mode)?;
    let mut pixels = dds::decode_texture_pixels(&top, 0)?.bgra32;
    if info.dxgi_format == 65 {
        for p in pixels.chunks_exact_mut(4) {
            p[3] = p[0];
            p[0] = 255;
            p[1] = 255;
            p[2] = 255;
        }
    }
    if info.alpha_mode == 3 {
        for p in pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    let mut image = Image { width: seed.width as usize, height: seed.height as usize, bgra: pixels };
    if plan.requires_resize {
        image = resample::resize(&image, target.width as usize, target.height as usize, Filter::Triangle);
    }
    for mip in reused as i32..target.mips {
        let width = (target.width >> mip).max(1);
        let height = (target.height >> mip).max(1);
        if mip > 0 {
            image = resample::resize(&image, width as usize, height as usize, Filter::Box);
        }
        let level = encode_mip(&image.bgra, width as usize, height as usize, target.dxgi_format)?;
        let block = if is_bc(target.dxgi_format) { 4 } else { 1 };
        let expected = ((width + block - 1) / block) as usize * ((height + block - 1) / block) as usize * bytes_per_element(target.dxgi_format) as usize;
        if level.len() != expected {
            return Err(Error::invalid(format!("The DDS encoder produced the wrong byte count for mip {mip}.")));
        }
        levels.push(level);
    }
    build_dds(target, &levels, 0, 1)
}

fn is_bc(dxgi: i32) -> bool {
    matches!(dxgi, 70..=84 | 94..=99)
}

fn dds_element_size(info: &DdsTextureInfo) -> i32 {
    let block = if is_bc(info.dxgi_format) { 4 } else { 1 };
    info.subresources[0].row_bytes / ((info.width + block - 1) / block)
}

/// Bytes per pixel or block for the formats this module can encode.
fn bytes_per_element(dxgi: i32) -> i32 {
    match dxgi {
        61 | 65 => 1,
        49 | 54 | 56 => 2,
        24 | 28 | 29 | 34 | 41 | 87 | 88 | 91 | 93 => 4,
        10 | 71 | 72 | 80 => 8,
        74 | 75 | 77 | 78 | 83 | 98 | 99 => 16,
        _ => 0,
    }
}

fn to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = bgra.to_vec();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    rgba
}

fn encode_mip(bgra: &[u8], width: usize, height: usize, format: i32) -> Result<Vec<u8>> {
    if is_bc(format) {
        let rgba = to_rgba(bgra);
        let squish = match format {
            71 | 72 => Some(texpresso::Format::Bc1),
            74 | 75 => Some(texpresso::Format::Bc2),
            77 | 78 => Some(texpresso::Format::Bc3),
            80 => Some(texpresso::Format::Bc4),
            83 => Some(texpresso::Format::Bc5),
            98 | 99 => None,
            _ => return Err(Error::not_supported(format!("Encoding DXGI {format} is unavailable."))),
        };
        return Ok(match squish {
            Some(codec) => {
                let mut output = vec![0u8; codec.compressed_size(width, height)];
                let params = texpresso::Params { algorithm: texpresso::Algorithm::ClusterFit, ..Default::default() };
                codec.compress(&rgba, width, height, params, &mut output);
                output
            }
            None => encode_bc7(&rgba, width, height),
        });
    }
    if matches!(format, 87 | 91) {
        return Ok(bgra.to_vec());
    }
    let stride = bytes_per_element(format) as usize;
    let mut pixels = vec![0u8; width * height * stride];
    for (source, j) in bgra.chunks_exact(4).zip((0..).step_by(stride)) {
        let (b, g, r, a) = (source[0], source[1], source[2], source[3]);
        let out = &mut pixels[j..j + stride];
        let half = |v: u8| half::f16::from_f32(v as f32 / 255.0).to_bits().to_le_bytes();
        match format {
            28 | 29 => out.copy_from_slice(&[r, g, b, a]),
            88 | 93 => out.copy_from_slice(&[b, g, r, 255]),
            61 => out[0] = r,
            65 => out[0] = a,
            49 => out.copy_from_slice(&[r, g]),
            56 => out.copy_from_slice(&(r as u16 * 257).to_le_bytes()),
            41 => out.copy_from_slice(&(r as f32 / 255.0).to_le_bytes()),
            54 => out.copy_from_slice(&half(r)),
            34 => {
                out[..2].copy_from_slice(&half(r));
                out[2..].copy_from_slice(&half(g));
            }
            10 => {
                for (k, v) in [r, g, b, a].into_iter().enumerate() {
                    out[k * 2..k * 2 + 2].copy_from_slice(&half(v));
                }
            }
            24 => {
                let scale = |v: u8, max: u32| (v as u32 * max + 127) / 255;
                let packed = scale(r, 1023) | scale(g, 1023) << 10 | scale(b, 1023) << 20 | scale(a, 3) << 30;
                out.copy_from_slice(&packed.to_le_bytes());
            }
            _ => return Err(Error::not_supported(format!("Encoding DXGI {format} is unavailable."))),
        }
    }
    Ok(pixels)
}

/// BC7 with alpha. The image is padded to whole blocks by repeating its edge
/// pixels, and block rows are encoded in parallel.
fn encode_bc7(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    use block_compression::{encode::compress_rgba8, BC7Settings, CompressionVariant};
    let (pw, ph) = (width.div_ceil(4) * 4, height.div_ceil(4) * 4);
    let mut padded = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        let sy = y.min(height - 1);
        for x in 0..pw {
            let sx = x.min(width - 1);
            let from = (sy * width + sx) * 4;
            padded[(y * pw + x) * 4..(y * pw + x) * 4 + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    let variant = CompressionVariant::BC7(BC7Settings::alpha_basic());
    let row_bytes = variant.blocks_byte_size(pw as u32, 4);
    let mut output = vec![0u8; variant.blocks_byte_size(pw as u32, ph as u32)];
    output.par_chunks_mut(row_bytes).enumerate().for_each(|(row, blocks)| {
        let start = row * 4 * pw * 4;
        let source = &padded[start..start + 4 * pw * 4];
        compress_rgba8(variant, source, blocks, pw as u32, 4, (pw * 4) as u32);
    });
    output
}

/// Writes a DX10 2D DDS and validates it.
pub fn build_dds(shape: TextureShape, levels: &[Vec<u8>], element_size: i32, alpha_mode: i32) -> Result<Vec<u8>> {
    let bc = is_bc(shape.dxgi_format);
    let mut out = Vec::with_capacity(148 + levels.iter().map(Vec::len).sum::<usize>());
    let mut put = |value: i32| out.extend_from_slice(&value.to_le_bytes());
    put(i32::from_le_bytes(*b"DDS "));
    put(124);
    put(0x1007 | if bc { 0x8_0000 } else { 8 } | if shape.mips > 1 { 0x2_0000 } else { 0 });
    put(shape.height);
    put(shape.width);
    let first = levels.first().map_or(0, Vec::len) as i32;
    put(if bc { first } else { shape.width * if element_size > 0 { element_size } else { bytes_per_element(shape.dxgi_format) } });
    put(0);
    put(shape.mips);
    for _ in 0..11 {
        put(0);
    }
    put(32);
    put(4);
    put(i32::from_le_bytes(*b"DX10"));
    for _ in 0..5 {
        put(0);
    }
    put(if shape.mips > 1 { 0x40_1008 } else { 0x1000 });
    for _ in 0..4 {
        put(0);
    }
    put(shape.dxgi_format);
    put(3);
    put(0);
    put(1);
    put(alpha_mode);
    for level in levels {
        out.extend_from_slice(level);
    }
    dds::inspect(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_dds(width: i32, height: i32) -> Vec<u8> {
        let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i * 3) as u8, (i * 5) as u8, (i * 7) as u8, 255]).collect();
        build_dds(TextureShape::new(width, height, 87, 1), &[pixels], 0, 0).unwrap()
    }

    #[test]
    fn plans_and_converts_to_bc_with_mips() {
        let source = rgba_dds(64, 32);
        for format in [71, 74, 77, 80, 83, 98, 28, 10, 24, 61, 65] {
            let target = TextureShape::new(64, 32, format, 7);
            let plan = plan(&source, target, None).unwrap();
            assert!(plan.requires_conversion && plan.limitation.is_none(), "{format}");
            let converted = apply(&source, &plan, false).unwrap();
            let info = dds::inspect(&converted).unwrap();
            assert_eq!(TextureShape::from_dds(&info), target);
            dds::decode_texture_pixels(&converted, 0).unwrap();
        }
    }

    #[test]
    fn resize_requires_consent() {
        let source = rgba_dds(30, 20);
        let plan = plan(&source, TextureShape::new(64, 64, 71, 1), None).unwrap();
        assert!(plan.requires_resize);
        assert!(apply(&source, &plan, false).is_err());
        assert_eq!(dds::inspect(&apply(&source, &plan, true).unwrap()).unwrap().width, 64);
    }

    #[test]
    fn matching_dds_is_kept() {
        let source = rgba_dds(8, 8);
        let plan = plan(&source, TextureShape::new(8, 8, 87, 1), None).unwrap();
        assert!(!plan.requires_conversion);
        assert_eq!(apply(&source, &plan, false).unwrap(), source);
    }

    #[test]
    fn png_import_becomes_srgb_dds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        image::RgbaImage::from_pixel(5, 3, image::Rgba([10, 20, 30, 40])).save(&path).unwrap();
        let dds_bytes = import(&path).unwrap();
        let info = dds::inspect(&dds_bytes).unwrap();
        assert_eq!((info.width, info.height, info.dxgi_format), (5, 3, 91));
        assert_eq!(&dds_bytes[148..152], &[30, 20, 10, 40]);
    }
}
