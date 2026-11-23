//! Experimental TTPF copy writer: replaces one texture in a pool copy and
//! verifies the copy before publishing it.

use std::path::{Path, PathBuf};

use rpf9_explorer::{
    rds5_writer,
    ttpf::{TtpfIndex, TtpfInfo, TtpfTextureInfo},
};
use serde::Serialize;

use crate::{
    catalog::{Asset, Catalog, Fingerprint, Source},
    dds, files,
    texture_conversion::{self, TextureConversionPlan, TextureShape},
    util::{eq_ignore_case, full_path, path_text},
    xbox_assets::key,
    Cancel, Error, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolWriteResult {
    pub index: i32,
    pub native_key: Option<String>,
    pub neighbor: i32,
    pub neighbor_verified: bool,
    pub replaced_source: bool,
    pub content_changed: bool,
    pub changed_chunks: i32,
    pub changed_decoded_bytes: i64,
    pub source_bytes: i64,
    pub destination_bytes: i64,
    pub compatibility: String,
    pub conversion: TextureConversionPlan,
}

/// Plans the conversion of `dds` for the pool descriptor of `asset`.
pub fn plan(catalog: &Catalog, asset: &Asset, dds_bytes: &[u8], cancel: &Cancel) -> Result<TextureConversionPlan> {
    let (source, index) = locate(catalog, asset)?;
    let mut input = source.open()?;
    source.fingerprint.check(source.path(), &mut input, false)?;
    let pool = TtpfIndex::read(source.path(), cancel)?;
    let texture = find_texture(asset, &pool.info, index)?;
    plan_texture(texture, dds_bytes)
}

fn plan_texture(texture: &TtpfTextureInfo, dds_bytes: &[u8]) -> Result<TextureConversionPlan> {
    let mut limitation = texture.limitation.clone();
    if !texture.supports_native_replacement && limitation.is_none() {
        limitation = Some(if texture.partial_mips() {
            format!(
                "RDS5 entry {} exposes {} of {} mips starting at mip {}. The writer needs a readable full native mip chain.",
                texture.index, texture.mips, texture.source_mips, texture.base_mip_level
            )
        } else {
            format!("RDS5 entry {} uses a storage layout that the current writer supports only for reading.", texture.index)
        });
    }
    if texture.dimension != 1 || texture.depth != 1 {
        limitation.get_or_insert_with(|| "The current RDS5 writer requires a single 2D texture descriptor.".into());
    }
    texture_conversion::plan(dds_bytes, TextureShape::new(texture.width, texture.height, texture.dxgi_format, texture.source_mips), limitation)
}

fn locate<'a>(catalog: &'a Catalog, asset: &Asset) -> Result<(&'a Source, i32)> {
    let source = catalog.source_of(asset)?;
    match (source.format.as_str(), asset.kind.as_str(), asset.index) {
        ("ttpf", "texture", Some(index)) => Ok((source, index)),
        _ => Err(Error::not_supported("Select a TTPF texture for this export.")),
    }
}

fn find_texture<'a>(asset: &Asset, pool: &'a TtpfInfo, index: i32) -> Result<&'a TtpfTextureInfo> {
    let mut found = pool.textures.iter().filter(|t| t.index == index);
    let texture = match (found.next(), found.next()) {
        (Some(texture), None) => texture,
        _ => return Err(Error::invalid("The pool has no single texture at this index.")),
    };
    if key(Some(texture.asset_key)) != asset.native_key {
        return Err(Error::invalid("Native texture key differs."));
    }
    Ok(texture)
}

fn payload(dds_bytes: &[u8]) -> Result<&[u8]> {
    let info = dds::inspect(dds_bytes)?;
    Ok(&dds_bytes[info.data_offset as usize..(info.data_offset + info.data_size) as usize])
}

/// Hidden unique path next to `output` that does not exist yet. It is
/// removed when dropped.
pub(crate) struct ScratchPath(PathBuf);

impl ScratchPath {
    pub(crate) fn beside(output: &Path, extension: &str) -> Result<Self> {
        let directory = output.parent().ok_or_else(|| Error::io("The destination has no folder."))?;
        let file = tempfile::Builder::new().prefix(".vicemodvi-").suffix(extension).tempfile_in(directory)?;
        let path = file.path().to_path_buf();
        drop(file);
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn copy(
    catalog: &mut Catalog,
    asset: &Asset,
    dds_bytes: &[u8],
    output: &Path,
    overwrite_existing: bool,
    allow_resize: bool,
    cancel: &Cancel,
) -> Result<PoolWriteResult> {
    let (source, index) = locate(catalog, asset)?;
    let source = source.clone();
    let output = full_path(output)?;
    if output.is_dir() {
        return Err(Error::io("The destination is a directory."));
    }
    if output.exists() && !overwrite_existing {
        return Err(Error::io("The destination already exists. Use --overwrite or choose a new path."));
    }
    {
        let mut input = source.open()?;
        source.fingerprint.check(source.path(), &mut input, true)?;
    }
    let pool = TtpfIndex::read(source.path(), cancel)?;
    let selected = find_texture(asset, &pool.info, index)?.clone();
    let conversion = plan_texture(&selected, dds_bytes)?;
    let prepared = texture_conversion::apply(dds_bytes, &conversion, allow_resize)?;
    let neighbor = pool
        .info
        .textures
        .iter()
        .filter(|t| t.index != index && t.can_extract())
        .min_by_key(|t| (t.index - index).abs())
        .ok_or_else(|| Error::not_supported("A readable neighboring entry is required to verify the copy."))?
        .clone();
    let original = pool.create_dds(index, cancel)?;
    let adjacent = pool.create_dds(neighbor.index, cancel)?;
    let scratch = ScratchPath::beside(&output, ".ttpf")?;
    let result = rds5_writer::export_replacement_copy(source.path(), index, &prepared, scratch.path(), cancel)?;
    let reopened = TtpfIndex::read(scratch.path(), cancel)?;
    let same_key = reopened.info.textures.iter().find(|t| t.index == index).is_some_and(|t| t.asset_key == selected.asset_key);
    if reopened.info.textures.len() != pool.info.textures.len() || !same_key {
        return Err(Error::invalid("The copy's texture index changed."));
    }
    let actual = reopened.create_dds(index, cancel)?;
    if payload(&actual)? != payload(&prepared)? || reopened.create_dds(neighbor.index, cancel)? != adjacent {
        return Err(Error::invalid("Reopened pool failed replacement or neighbor verification."));
    }
    {
        let mut current = source.open()?;
        source.fingerprint.check(source.path(), &mut current, true)?;
    }
    files::publish(scratch.path(), &output, overwrite_existing)?;
    let replaced_source = eq_ignore_case(&path_text(&output), &path_text(&full_path(source.path())?));
    if replaced_source {
        let mut current = source.open()?;
        let refreshed = Fingerprint::create(source.path(), &mut current, None, cancel)?;
        if let Some(entry) = catalog.sources.iter_mut().find(|s| s.id == source.id) {
            entry.fingerprint = refreshed;
        }
    }
    Ok(PoolWriteResult {
        index,
        native_key: asset.native_key.clone(),
        neighbor: neighbor.index,
        neighbor_verified: true,
        replaced_source,
        content_changed: payload(&actual)? != payload(&original)?,
        changed_chunks: result.changed_chunks,
        changed_decoded_bytes: result.changed_decoded_bytes,
        source_bytes: result.source_bytes,
        destination_bytes: result.destination_bytes,
        compatibility: "Pending platform and build verification".into(),
        conversion,
    })
}
