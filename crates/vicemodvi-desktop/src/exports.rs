//! Desktop exports: native files, PNG previews and UV layouts.

use std::{io::Cursor, path::Path};

use vicemodvi_core::{
    files, xbox_assets, Asset, Catalog, Error, Mesh, Pixels, Result,
};

use crate::browse::export_name;

pub fn extension(asset: &Asset) -> &'static str {
    match asset.kind.as_str() {
        "model" => ".obj",
        "texture" => ".dds",
        _ => ".bin",
    }
}

pub fn can_export(asset: &Asset) -> bool {
    matches!(asset.kind.as_str(), "texture" | "model") || asset.range.is_some()
}

/// Native export: DDS for textures, OBJ for models, stored bytes otherwise.
pub fn export_native(catalog: &Catalog, asset: &Asset, path: &Path) -> Result<()> {
    if matches!(asset.kind.as_str(), "texture" | "model") {
        xbox_assets::export(catalog, asset, path)?;
    } else {
        files::write(path, |stream| catalog.copy_stored(asset, stream, None).map(|_| ()), false)?;
    }
    Ok(())
}

/// Exports each asset into `directory`, collecting failures.
pub fn export_batch(catalog: &Catalog, assets: &[Asset], directory: &Path) -> (usize, Vec<String>) {
    let mut exported = 0;
    let mut failures = Vec::new();
    for asset in assets {
        let path = directory.join(format!("{}{}", export_name(&asset.name, &asset.id), extension(asset)));
        match export_native(catalog, asset, &path) {
            Ok(()) => exported += 1,
            Err(error) => failures.push(format!("{} — {error}", asset.name)),
        }
    }
    (exported, failures)
}

fn png_bytes(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| Error::invalid("The pixel buffer has the wrong size."))?;
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

pub fn write_png(pixels: &Pixels, path: &Path) -> Result<()> {
    let mut rgba = pixels.bgra32.clone();
    for p in rgba.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    files::write_bytes(path, &png_bytes(pixels.width as u32, pixels.height as u32, rgba)?, false)
}

/// 1024×1024 transparent PNG with each triangle outlined: a 3 px white line
/// under a 1 px black line.
pub fn write_uv_layout(mesh: &Mesh, path: &Path) -> Result<()> {
    const SIZE: usize = 1024;
    let uv = mesh.tex_coords.as_ref().filter(|uv| !uv.is_empty() && uv.len() == mesh.positions.len()).ok_or_else(|| Error::invalid("This mesh has no UV coordinates."))?;
    if uv.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
        return Err(Error::invalid("This mesh has invalid UV coordinates."));
    }
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let point = |i: i32| (uv[i as usize][0] as f64 * SIZE as f64, uv[i as usize][1] as f64 * SIZE as f64);
    for (color, radius) in [([255u8, 255, 255, 255], 1.5f64), ([0, 0, 0, 255], 0.5)] {
        for face in mesh.indices.as_chunks::<3>().0 {
            let corners = [point(face[0]), point(face[1]), point(face[2])];
            for k in 0..3 {
                draw_line(&mut rgba, SIZE, corners[k], corners[(k + 1) % 3], radius, color);
            }
        }
    }
    files::write_bytes(path, &png_bytes(SIZE as u32, SIZE as u32, rgba)?, false)
}

/// Stamps a square brush along a segment, clipped to the image.
fn draw_line(rgba: &mut [u8], size: usize, from: (f64, f64), to: (f64, f64), radius: f64, color: [u8; 4]) {
    let length = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
    // Segments far outside the layout are skipped rather than walked.
    if length > (size * 64) as f64 {
        return;
    }
    let steps = (length * 2.0).ceil().max(1.0) as usize;
    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        let (x0, x1) = ((x - radius).round() as i64, (x + radius).round() as i64);
        let (y0, y1) = ((y - radius).round() as i64, (y + radius).round() as i64);
        for py in y0.max(0)..=y1.min(size as i64 - 1) {
            for px in x0.max(0)..=x1.min(size as i64 - 1) {
                let at = (py as usize * size + px as usize) * 4;
                rgba[at..at + 4].copy_from_slice(&color);
            }
        }
    }
}
