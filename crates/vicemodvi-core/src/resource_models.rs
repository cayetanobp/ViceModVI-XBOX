//! Drawables listed and decoded by the vendored Resource Tools crates.
//!
//! The C# build reached these crates through a C ABI DLL. They are now called
//! directly, so no `unsafe` boundary remains. Unexpected panics inside the
//! vendored readers become errors instead of terminating the process.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::BufReader,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
};

use rpf9_identify::{
    identify_with_progress,
    resource::{classify, Kind},
};
use rpf9_index::{extract, Entry, Evidence, Payload};
use rpf9_models::{decode_geometry, drawables};

use crate::{
    catalog::{Asset, ByteRange, Source},
    drawable_materials,
    rage_model::{RageMesh, RageModel},
    util::eq_ignore_case,
    xbox_assets::IndexProgress,
    Cancel, Error, Result,
};

const MAX_IMAGE: usize = 256 * 1024 * 1024;

type Input = BufReader<File>;

/// One drawable listed by the reader.
#[derive(Clone, Debug)]
pub struct DrawableRow {
    pub offset: i64,
    pub end: i64,
    pub index: i32,
    pub name: String,
    pub locator: String,
}

pub struct Inventory {
    /// Offsets of zlib members, used as model and texture candidates.
    pub candidate_offsets: Vec<i64>,
    pub rows: Vec<DrawableRow>,
}

struct Cancelled;

/// Runs a vendored reader, mapping cancellation and panics to errors.
fn guarded<T>(work: impl FnOnce() -> std::io::Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Error::invalid(error.to_string())),
        Err(payload) if payload.is::<Cancelled>() => Err(Error::Cancelled),
        Err(_) => Err(Error::invalid("The native model reader failed unexpectedly.")),
    }
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn image(input: &mut Input, entries: &[Entry]) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in entries {
        if entry.decoded_len > MAX_IMAGE - bytes.len() {
            return Err(invalid("Resource image exceeds its limit"));
        }
        bytes.extend(extract(input, entry, Payload::Decoded)?);
    }
    Ok(bytes)
}

fn locator(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| format!("{},{},{},{},{}", e.offset, e.stored_end, e.decoded_len, e.chunks, u8::from(e.evidence == Evidence::ZlibSequence)))
        .collect::<Vec<_>>()
        .join(";")
}

fn parse_part(value: &str) -> std::io::Result<Entry> {
    let fields = value.split(',').map(str::parse::<u64>).collect::<std::result::Result<Vec<_>, _>>().map_err(|_| invalid("Invalid model range"))?;
    let &[offset, end, size, chunks, codec] = fields.as_slice() else {
        return Err(invalid("Malformed model range"));
    };
    if offset >= end || codec > 1 || size > MAX_IMAGE as u64 {
        return Err(invalid("Invalid model range"));
    }
    Ok(Entry {
        ordinal: 0,
        header: 0,
        offset,
        stored_end: end,
        decoded_len: size as usize,
        chunks: chunks as usize,
        signature: "unknown",
        evidence: if codec == 1 { Evidence::ZlibSequence } else { Evidence::ZlibStream },
    })
}

/// Inventories an RPF9 archive and lists its drawables with geometry.
pub fn list(source: &Source, progress: Option<&dyn Fn(IndexProgress)>, cancel: &Cancel) -> Result<Inventory> {
    cancel.check()?;
    let report = |phase: &str, done: u64, total: u64, unit: &str| {
        if cancel.is_cancelled() {
            resume_unwind(Box::new(Cancelled));
        }
        if let Some(progress) = progress {
            progress(IndexProgress::new(phase, done as i64, total as i64, unit));
        }
    };
    let result = guarded(|| {
        let mut input = BufReader::new(File::open(&source.path)?);
        let report_data = identify_with_progress(&mut input, report)?;
        let candidate_offsets = report_data
            .inventory
            .entries
            .iter()
            .filter(|e| matches!(e.evidence, Evidence::ZlibSequence | Evidence::ZlibStream | Evidence::ZlibUnterminated))
            .map(|e| e.offset as i64)
            .collect();
        let total = report_data.resources.len();
        let mut rows = Vec::new();
        for (done, resource) in report_data.resources.iter().enumerate() {
            if done % 32 == 0 {
                report("Reading resources", done as u64, total as u64, "entries");
            }
            if !matches!(&resource.kind, Kind::NamedDictionary { extension, .. } | Kind::Named { extension, .. } if extension == "#dr") {
                continue;
            }
            let entry = |i: &usize| report_data.inventory.entries[*i].clone();
            let virt: Vec<_> = std::iter::once(&resource.root_ordinal).chain(&resource.virtual_parts).map(entry).collect();
            let phys: Vec<_> = resource.physical.iter().map(entry).collect();
            let Some(last) = phys.last() else { continue };
            if virt.iter().chain(&phys).any(|p| !matches!(p.evidence, Evidence::ZlibSequence | Evidence::ZlibStream)) {
                continue;
            }
            let data = image(&mut input, &virt)?;
            let Ok(items) = drawables(&data, &resource.kind, resource.physical_len as usize) else { continue };
            let (v, p) = (locator(&virt), locator(&phys));
            for (i, item) in items.iter().enumerate() {
                if !item.lods.iter().flatten().any(|model| !model.geometries.is_empty()) {
                    continue;
                }
                let name = item.name.clone().unwrap_or_else(|| format!("drawable_{}_{i}", resource.id));
                rows.push(DrawableRow {
                    offset: virt[0].offset as i64,
                    end: last.stored_end as i64,
                    index: i as i32,
                    name,
                    locator: format!("{i}|{v}|{p}"),
                });
            }
        }
        report("Reading resources", total as u64, total as u64, "entries");
        Ok(Inventory { candidate_offsets, rows })
    });
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    result
}

fn drawable_name(name: &str) -> &str {
    if name.len() >= 4 && name.is_char_boundary(name.len() - 4) && name[name.len() - 4..].eq_ignore_ascii_case(".#dr") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// Adds listed drawables, skipping those already recovered at the same
/// offset under the same name.
pub fn add_drawables(source: &Source, assets: &mut Vec<Asset>, rows: &[DrawableRow]) -> Result<()> {
    let mut legacy: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, asset) in assets.iter().enumerate() {
        if asset.kind == "model" {
            if let Some(range) = &asset.range {
                legacy.entry(range.offset).or_default().push(i);
            }
        }
    }
    let mut matched = HashSet::new();
    let mut added = Vec::new();
    for row in rows {
        let range = ByteRange::new(row.offset, row.end - row.offset);
        range.validate(source.fingerprint.length)?;
        let name = drawable_name(&row.name);
        let existing = legacy.get(&row.offset).and_then(|candidates| {
            candidates.iter().copied().find(|&i| !matched.contains(&i) && eq_ignore_case(drawable_name(&assets[i].name), name))
        });
        if let Some(i) = existing {
            matched.insert(i);
            continue;
        }
        let mut asset = Asset::new(
            format!("{}:drawable:{:X}:{}", source.id, row.offset, row.index),
            &source.id,
            "model",
            name.to_string(),
            "resource",
            "Resource Tools drawable",
        );
        asset.range = Some(range);
        asset.drawable_locator = Some(row.locator.clone());
        added.push(asset);
    }
    assets.extend(added);
    Ok(())
}

/// Decodes the static geometry and materials of a listed drawable.
pub fn read(source: &Source, asset: &Asset) -> Result<RageModel> {
    let value = asset.drawable_locator.as_deref().ok_or_else(|| Error::invalid("Invalid drawable data."))?;
    let (index, virt, phys, meshes) = guarded(|| {
        let mut input = BufReader::new(File::open(&source.path)?);
        let fields: Vec<_> = value.split('|').collect();
        let [index, virt, phys] = fields.as_slice() else { return Err(invalid("Malformed drawable locator")) };
        let index: usize = index.parse().map_err(|_| invalid("Invalid drawable index"))?;
        let mut decode = |parts: &str| -> std::io::Result<Vec<u8>> {
            let entries = parts.split(';').map(parse_part).collect::<std::io::Result<Vec<_>>>()?;
            image(&mut input, &entries)
        };
        let virt = decode(virt)?;
        let phys = decode(phys)?;
        let items = drawables(&virt, &classify(&virt), phys.len()).map_err(invalid)?;
        let item = items.get(index).ok_or_else(|| invalid("Drawable index is out of range"))?;
        let mut meshes = Vec::new();
        for lod in &item.lods {
            for model in lod {
                for i in 0..model.geometries.len() {
                    meshes.push(decode_geometry(&phys, model, i).map_err(invalid)?);
                }
            }
            if !meshes.is_empty() {
                break;
            }
        }
        Ok((item.at, virt, phys.len(), meshes))
    })?;
    let _ = phys;
    if index > i32::MAX as usize || virt.len() < 16 || index > virt.len() - 16 || virt.len() > MAX_IMAGE {
        return Err(Error::invalid("Invalid drawable material data."));
    }
    let materials = drawable_materials::read(&virt, index as i64);
    if meshes.is_empty() || meshes.len() > 65536 {
        return Err(Error::invalid("Invalid mesh count."));
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut parts = Vec::with_capacity(meshes.len());
    for (part, mesh) in meshes.into_iter().enumerate() {
        let vertices = mesh.positions.len();
        if vertices < 1 || mesh.triangles.is_empty() || vertices > i32::MAX as usize {
            return Err(Error::invalid("Invalid drawable mesh lengths."));
        }
        for p in &mesh.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        let mut indices = Vec::with_capacity(mesh.triangles.len() * 3);
        for &corner in mesh.triangles.iter().flatten() {
            if corner as usize >= vertices {
                return Err(Error::invalid("Invalid drawable mesh index."));
            }
            indices.push(corner as i32);
        }
        if mesh.uv0.as_ref().is_some_and(|uv| uv.len() != vertices) {
            return Err(Error::invalid("Invalid drawable mesh lengths."));
        }
        parts.push(RageMesh {
            name: format!("Mesh {}", part + 1),
            positions: mesh.positions,
            indices,
            tex_coords: mesh.uv0,
            vertex_colors: None,
            shader_index: Some(mesh.shader as i32),
        });
    }
    let mut model = RageModel::new(asset.name.clone(), parts, min, max);
    model.materials = Some(materials);
    model.presentation_note = Some("Static geometry from Resource Tools.".into());
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locators_roundtrip() {
        let entry = parse_part("100,200,300,2,1").unwrap();
        assert_eq!(locator(&[entry]), "100,200,300,2,1");
        assert!(parse_part("200,100,300,2,1").is_err());
        assert_eq!(drawable_name("prop.#DR"), "prop");
    }
}
