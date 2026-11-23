//! Experimental RPF9 copy writer: replaces one texture of a recovered
//! dictionary by recompressing only the changed segments to their original
//! packed size, so the archive layout stays byte-for-byte compatible.

use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use rpf9_explorer::{native_replacement, rage_texture};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    catalog::{ByteRange, Catalog},
    dds, files,
    pool_writer::ScratchPath,
    texture_conversion::{self, TextureConversionPlan, TextureShape},
    util::{full_path, hex_upper},
    xbox_assets::{decode_segments, key},
    zlib, Asset, Cancel, Error, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpfCopyResult {
    pub texture_id: String,
    pub archive_bytes: i64,
    pub original_sha256: String,
    pub copy_sha256: String,
    pub rewritten_segments: i32,
    pub verified_neighbor: Option<i32>,
    pub conversion: TextureConversionPlan,
    pub compatibility: String,
}

struct Patch {
    offset: i64,
    packed: Vec<u8>,
}

const BUFFER: usize = 1024 * 1024;

pub fn copy(catalog: &Catalog, asset: &Asset, dds_bytes: &[u8], destination: &Path, allow_resize: bool, cancel: &Cancel) -> Result<RpfCopyResult> {
    let source = catalog.source_of(asset)?;
    let (Some(index), Some(parent_id), true, true) = (asset.index, asset.parent_id.as_deref(), source.format == "rpf9", asset.kind == "texture")
    else {
        return Err(Error::not_supported("Select a texture in a recovered RPF9 dictionary."));
    };
    let parent = catalog.get(parent_id)?;
    let segments = parent.segments.as_ref().filter(|s| !s.is_empty());
    let (Some(segments), Some(system_size), true) = (segments, parent.system_bytes.filter(|&s| s > 0), parent.kind == "dictionary") else {
        return Err(Error::not_supported("The experimental copy requires a standalone segmented RPF9 dictionary."));
    };
    let destination = full_path(destination)?;
    if destination.exists() {
        return Err(Error::io("Choose a new file for the RPF9 copy."));
    }
    let mut input = source.open()?;
    source.fingerprint.check(source.path(), &mut input, true)?;
    let original = decode_segments(&mut input, parent, cancel)?;
    let system_size = system_size as usize;
    if system_size >= original.len() {
        return Err(Error::invalid("Invalid resource section lengths."));
    }
    let (system, graphics) = original.split_at(system_size);
    let dictionary = rage_texture::inspect(system, graphics).ok_or_else(|| Error::invalid("The source texture dictionary failed validation."))?;
    let texture = dictionary
        .textures
        .iter()
        .find(|t| t.index == index)
        .ok_or_else(|| Error::invalid("The source texture dictionary failed validation."))?;
    if key(texture.asset_key) != asset.native_key || key(Some(texture.name_hash as u64)) != asset.name_hash {
        return Err(Error::invalid("Native texture identifiers differ."));
    }
    let limitation = (texture.dimension != 1 || texture.depth != 1).then(|| "The experimental RPF9 writer supports single 2D textures.".to_string());
    let plan = texture_conversion::plan(dds_bytes, TextureShape::new(texture.width, texture.height, texture.dxgi_format, texture.mip_count), limitation)?;
    let expected_dds = texture_conversion::apply(dds_bytes, &plan, allow_resize)?;
    let staged = native_replacement::stage(system, graphics, index, &expected_dds, cancel)?;
    let patches = prepare_patches(segments, &original, &staged.resource_data)?;
    let neighbor = dictionary.textures.iter().filter(|t| t.index != index).min_by_key(|t| (t.index - index).abs());
    let neighbor_dds = neighbor.map(|n| rage_texture::create_dds(system, graphics, n.index)).transpose()?;
    let input_length = input.metadata()?.len() as i64;

    let scratch = ScratchPath::beside(&destination, ".rpf")?;
    {
        let file = OpenOptions::new().write(true).create_new(true).open(scratch.path())?;
        let mut output = BufWriter::with_capacity(BUFFER, file);
        let mut buffer = vec![0u8; BUFFER];
        input.seek(SeekFrom::Start(0))?;
        let mut position = 0i64;
        for patch in &patches {
            copy_bytes(&mut input, &mut output, patch.offset - position, &mut buffer, cancel)?;
            output.write_all(&patch.packed)?;
            position = patch.offset + patch.packed.len() as i64;
            input.seek(SeekFrom::Start(position as u64))?;
        }
        copy_bytes(&mut input, &mut output, input_length - position, &mut buffer, cancel)?;
        let file = output.into_inner().map_err(|e| Error::Io(e.into_error()))?;
        file.sync_all()?;
    }
    let digest = {
        let mut reopened = File::open(scratch.path())?;
        let digest = check_copy(&mut input, &mut reopened, &patches, cancel)?;
        let raw = decode_segments(&mut reopened, parent, cancel)?;
        let (reopened_system, reopened_graphics) = raw.split_at(system_size);
        let actual = rage_texture::create_dds(reopened_system, reopened_graphics, index)?;
        let neighbor_matches = match (neighbor, &neighbor_dds) {
            (Some(n), Some(expected)) => &rage_texture::create_dds(reopened_system, reopened_graphics, n.index)? == expected,
            _ => true,
        };
        if reopened_system != system || raw != staged.resource_data || payload(&actual)? != payload(&expected_dds)? || !neighbor_matches {
            return Err(Error::invalid("The reopened RPF9 failed dictionary or texture verification."));
        }
        digest
    };
    files::publish(scratch.path(), &destination, false)?;
    Ok(RpfCopyResult {
        texture_id: asset.id.clone(),
        archive_bytes: input_length,
        original_sha256: source.fingerprint.sha256.clone(),
        copy_sha256: digest,
        rewritten_segments: patches.len() as i32,
        verified_neighbor: neighbor.map(|n| n.index),
        conversion: plan,
        compatibility: "Pending a test in the exact game build and platform".into(),
    })
}

fn payload(dds_bytes: &[u8]) -> Result<&[u8]> {
    let info = dds::inspect(dds_bytes)?;
    Ok(&dds_bytes[info.data_offset as usize..(info.data_offset + info.data_size) as usize])
}

fn prepare_patches(segments: &[ByteRange], original: &[u8], replacement: &[u8]) -> Result<Vec<Patch>> {
    let mut patches = Vec::new();
    let mut offset = 0usize;
    for segment in segments {
        let size = segment.decoded_length.filter(|&d| d > 0 && d <= i32::MAX as i64).ok_or_else(|| Error::invalid("Invalid segment length."))? as usize;
        let edited = replacement.get(offset..offset + size).ok_or_else(|| Error::invalid("The staged resource is shorter than its segments."))?;
        let before = original.get(offset..offset + size).ok_or_else(|| Error::invalid("The resource is shorter than its segments."))?;
        if edited != before {
            let packed = zlib::pack_exactly(edited, segment.length)?.ok_or_else(|| {
                Error::not_supported(format!(
                    "The changed segment at 0x{:X} could not be encoded in exactly {} bytes. This writer requires the original packed size for each segment.",
                    segment.offset, segment.length
                ))
            })?;
            patches.push(Patch { offset: segment.offset, packed });
        }
        offset += size;
    }
    Ok(patches)
}

fn copy_bytes(source: &mut impl Read, output: &mut impl Write, mut length: i64, buffer: &mut [u8], cancel: &Cancel) -> Result<()> {
    while length > 0 {
        cancel.check()?;
        let count = (length as usize).min(buffer.len());
        source.read_exact(&mut buffer[..count])?;
        output.write_all(&buffer[..count])?;
        length -= count as i64;
    }
    Ok(())
}

/// Confirms that only the patched ranges differ and hashes the copy.
fn check_copy(source: &mut File, copy: &mut File, patches: &[Patch], cancel: &Cancel) -> Result<String> {
    let length = source.metadata()?.len() as i64;
    if length != copy.metadata()?.len() as i64 {
        return Err(Error::invalid("The copy changed the archive size."));
    }
    let mut hash = Sha256::new();
    let mut expected = vec![0u8; BUFFER];
    let mut actual = vec![0u8; BUFFER];
    source.seek(SeekFrom::Start(0))?;
    copy.seek(SeekFrom::Start(0))?;
    let mut position = 0i64;
    let mut compare = |end: i64, position: &mut i64, hash: &mut Sha256, source: &mut File, copy: &mut File| -> Result<()> {
        while *position < end {
            cancel.check()?;
            let count = ((end - *position) as usize).min(expected.len());
            source.read_exact(&mut expected[..count])?;
            copy.read_exact(&mut actual[..count])?;
            if expected[..count] != actual[..count] {
                return Err(Error::invalid("Bytes outside the replacement segment changed."));
            }
            hash.update(&actual[..count]);
            *position += count as i64;
        }
        Ok(())
    };
    for patch in patches {
        compare(patch.offset, &mut position, &mut hash, source, copy)?;
        let mut found = vec![0u8; patch.packed.len()];
        copy.read_exact(&mut found)?;
        if found != patch.packed {
            return Err(Error::invalid(format!("Replacement segment differs at 0x{:X}.", patch.offset)));
        }
        hash.update(&found);
        position += found.len() as i64;
        source.seek(SeekFrom::Start(position as u64))?;
    }
    compare(length, &mut position, &mut hash, source, copy)?;
    Ok(hex_upper(&hash.finalize()))
}
