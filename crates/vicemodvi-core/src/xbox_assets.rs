//! Builds catalogs from RPF9 archives and TTPF pools, and reads, exports and
//! converts their assets.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;

use crate::{
    cache,
    catalog::{Asset, ByteRange, Catalog, Fingerprint, Source},
    files,
    native::{
        dds,
        dictionary::{self, DictionaryTexture},
        drawable, model,
        pool::PoolIndex,
        scan::{self, Recovered},
    },
    resource_models,
    util::{full_path, parse_number, path_text},
    zlib, Cancel, Error, Model, Result,
};

const MAX_RESOURCE_BYTES: i64 = 256 * 1024 * 1024;
const BATCH: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: String,
    pub completed: i64,
    pub total: i64,
    pub unit: String,
}

impl IndexProgress {
    pub fn new(phase: &str, completed: i64, total: i64, unit: &str) -> Self {
        Self { phase: phase.to_string(), completed, total, unit: unit.to_string() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshExport {
    pub vertices: i64,
    pub faces: i64,
    pub meshes: i64,
    pub materials: i64,
    pub decoder: String,
    pub coverage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureExport {
    pub width: i32,
    pub height: i32,
    pub dxgi_format: i32,
    pub source_width: i32,
    pub source_height: i32,
    pub source_format: i32,
    pub source_mips: i32,
    pub exported_mips: i32,
    pub base_mip: i32,
    pub decoder: String,
    pub coverage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ExportResult {
    Texture(TextureExport),
    Mesh(MeshExport),
}

/// Progress callback: source path and progress entry.
pub type Progress<'a> = &'a (dyn Fn(&Path, &IndexProgress) + Sync);

#[derive(Clone, Copy, Default)]
pub struct BuildOptions<'a> {
    /// Carve inventory TSV paired with exactly one RPF.
    pub inventory: Option<&'a Path>,
    /// Restore and store per-source indexes in the local cache.
    pub use_cache: bool,
    pub progress: Option<Progress<'a>>,
}

/// 16 uppercase hexadecimal digits, or `None` for a missing or zero key.
pub fn key(value: Option<u64>) -> Option<String> {
    value.filter(|&v| v != 0).map(|v| format!("{v:016X}"))
}

fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8)
}

/// Indexes the given RPF and TTPF files into a new catalog. Sources are read
/// in parallel; source ids follow the order of `files`.
pub fn build(files: &[PathBuf], options: BuildOptions, cancel: &Cancel) -> Result<Catalog> {
    let mut paths: Vec<String> = Vec::new();
    for file in files {
        let path = path_text(&full_path(file)?);
        if !paths.iter().any(|p| crate::util::eq_ignore_case(p, &path)) {
            paths.push(path);
        }
    }
    let use_cache = options.use_cache && options.inventory.is_none();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(workers()).build().map_err(|e| Error::io(e.to_string()))?;
    let stop = Cancel::new();
    let mut results: Vec<Result<(Source, Vec<Asset>)>> = Vec::with_capacity(paths.len());
    if options.inventory.is_some() {
        // An inventory belongs to one RPF, so these sources stay in order.
        let mut imported = false;
        for (i, path) in paths.iter().enumerate() {
            cancel.check()?;
            let inventory = if path_is_rpf(path) {
                if imported {
                    return Err(Error::invalid("An inventory must be paired with exactly one RPF."));
                }
                imported = true;
                options.inventory
            } else {
                None
            };
            let result = pool.install(|| build_source(path, &i.to_string(), inventory, false, options.progress, cancel));
            results.push(Ok(result?));
        }
        if !imported {
            return Err(Error::invalid("An inventory requires an RPF source."));
        }
    } else {
        let token = Cancel::linked(&[cancel, &stop]);
        let slots: Vec<Mutex<Option<Result<(Source, Vec<Asset>)>>>> = paths.iter().map(|_| Mutex::new(None)).collect();
        pool.install(|| {
            paths.par_iter().enumerate().for_each(|(i, path)| {
                let result = build_source(path, &i.to_string(), None, use_cache, options.progress, &token);
                if matches!(&result, Err(e) if !e.is_cancelled()) {
                    stop.cancel();
                }
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            });
        });
        results = slots
            .into_iter()
            .map(|slot| slot.into_inner().unwrap_or_else(|e| e.into_inner()).unwrap_or(Err(Error::Cancelled)))
            .collect();
    }
    cancel.check()?;
    let mut catalog = Catalog::new();
    if let Some(error) = results.iter().position(|r| matches!(r, Err(e) if !e.is_cancelled())) {
        return Err(results.swap_remove(error).err().unwrap_or(Error::Cancelled));
    }
    for result in results {
        let (source, assets) = result?;
        catalog.sources.push(source);
        catalog.assets.extend(assets);
    }
    if catalog.sources.is_empty() {
        return Err(Error::invalid("No RPF or TTPF sources found."));
    }
    catalog.reindex();
    Ok(catalog)
}

fn path_is_rpf(path: &str) -> bool {
    crate::util::extension(Path::new(path)) == ".rpf"
}

/// Reads one source: fingerprint, then its texture table or its models and
/// dictionaries.
fn build_source(
    path: &str,
    id: &str,
    inventory: Option<&Path>,
    use_cache: bool,
    progress: Option<Progress>,
    cancel: &Cancel,
) -> Result<(Source, Vec<Asset>)> {
    cancel.check()?;
    let file_path = Path::new(path);
    let report = |entry: IndexProgress| {
        if let Some(progress) = progress {
            progress(file_path, &entry);
        }
    };
    if use_cache {
        if let Some(restored) = cache::try_restore(path, id) {
            return Ok(restored);
        }
    }
    let format = match crate::util::extension(file_path).as_str() {
        ".rpf" => "rpf9",
        ".ttpf" => "ttpf",
        _ => return Err(Error::invalid(format!("Unsupported source: {path}"))),
    };
    let mut input = File::open(file_path)?;
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    if magic != if format == "rpf9" { *b"9FPR" } else { *b"RDS5" } {
        return Err(Error::invalid(format!("Unexpected {format} signature: {path}")));
    }
    let length = input.metadata()?.len() as i64;
    report(IndexProgress::new("Reading source fingerprint", 0, length, "bytes"));
    let fingerprint_progress = |bytes: i64| report(IndexProgress::new("Reading source fingerprint", bytes, length, "bytes"));
    let fingerprint = Fingerprint::create(file_path, &mut input, Some(&fingerprint_progress), cancel)?;
    let source = Source { id: id.to_string(), path: path.to_string(), format: format.to_string(), fingerprint };
    let mut assets = Vec::new();
    if format == "ttpf" {
        report(IndexProgress::new("Reading texture table", 0, 0, "entries"));
        let index = PoolIndex::read(file_path, cancel)?;
        let total = index.info.textures.len();
        for (read, texture) in index.info.textures.iter().enumerate() {
            if read % 1024 == 0 {
                cancel.check()?;
            }
            let mut asset = Asset::new(format!("{id}:ttpf:{}", texture.index), id, "texture", texture.name.clone(), "alias", "RDS5/ETPF descriptor");
            asset.index = Some(texture.index);
            asset.native_key = key(Some(texture.asset_key));
            assets.push(asset);
            if (read + 1) % 128 == 0 || read + 1 == total {
                report(IndexProgress::new("Indexing textures", (read + 1) as i64, total as i64, "entries"));
            }
        }
        if use_cache {
            cache::store(&source, &assets);
        }
        return Ok((source, assets));
    }
    let mut offsets = None;
    if let Some(inventory) = inventory {
        let rows = read_inventory(&source, inventory)?;
        offsets = Some(
            rows.iter()
                .filter(|row| row.evidence.starts_with("zlib"))
                .filter_map(|row| row.range.as_ref().map(|r| r.offset))
                .collect::<Vec<_>>(),
        );
        assets.extend(rows);
    }
    let report_entry = |entry: IndexProgress| report(entry);
    let drawables = resource_models::list(&source, Some(&report_entry), cancel)?;
    let candidates = offsets.unwrap_or_else(|| drawables.candidate_offsets.clone());
    scan_rpf(&source, &mut assets, &candidates, &report, cancel)?;
    resource_models::add_drawables(&source, &mut assets, &drawables.rows)?;
    if use_cache {
        cache::store(&source, &assets);
    }
    Ok((source, assets))
}

/// Writes a carve index JSON as an inventory TSV.
pub fn convert_carve_index(index: &Path, output: &Path) -> Result<()> {
    let data: Value = serde_json::from_slice(&std::fs::read(index)?)?;
    let rows = data
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("The carve index has no 'files' array."))?;
    let field = |row: &Value, name: &str| -> Result<i64> {
        row.get(name).and_then(Value::as_i64).ok_or_else(|| Error::invalid(format!("The carve index row has no '{name}' integer.")))
    };
    let mut text = String::from("# entry\tordinal\theader\toffset\tstored_end\tstored_len\tdecoded_len\tchunks\tevidence\tsignature\n");
    for (ordinal, row) in rows.iter().enumerate() {
        let offset = field(row, "off")?;
        let length = field(row, "stored")?;
        let size = field(row, "size")?;
        let end = offset.checked_add(length).ok_or_else(|| Error::invalid("The carve index range overflows."))?;
        let evidence = if row.get("type").is_some() { "bink_header" } else { "zlib_stream" };
        let head = row.get("head").and_then(Value::as_str).unwrap_or_default();
        text.push_str(&format!("entry\t{ordinal}\t\t0x{offset:X}\t0x{end:X}\t{length}\t{size}\t1\t{evidence}\t{head}\n"));
    }
    files::write_bytes(output, text.as_bytes(), false)
}

fn read_inventory(source: &Source, path: &Path) -> Result<Vec<Asset>> {
    let text = std::fs::read_to_string(path)?;
    let mut assets = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields[0] != "entry" {
            continue;
        }
        if fields.len() < 10 {
            return Err(Error::invalid("Incomplete RPF inventory entry."));
        }
        let range = ByteRange { offset: parse_number(fields[3])?, length: parse_number(fields[5])?, decoded_length: Some(parse_number(fields[6])?) };
        range.validate(source.fingerprint.length)?;
        if range.offset + range.length != parse_number(fields[4])? {
            return Err(Error::invalid("Stored length and end disagree in inventory."));
        }
        let mut asset = Asset::new(
            format!("{}:range:{:X}", source.id, range.offset),
            &source.id,
            "unknown",
            format!("0x{:X}", range.offset),
            "alias",
            fields[8],
        );
        asset.range = Some(range);
        assets.push(asset);
    }
    Ok(assets)
}

fn scan_rpf(source: &Source, assets: &mut Vec<Asset>, candidates: &[i64], report: &(dyn Fn(IndexProgress) + Sync), cancel: &Cancel) -> Result<()> {
    let path = source.path();
    let length = source.fingerprint.length;
    let batches: Vec<&[i64]> = candidates.chunks(BATCH).collect();

    let models = recover_batches(&batches, "Validating model candidates", candidates.len(), report, cancel, |batch| {
        scan::recover_models(path, length, batch, cancel)
    })?;
    for entry in models.into_iter().flatten() {
        let mut asset = locator(source, &entry, "model");
        let model = &entry.content;
        if !model.name.trim().is_empty() {
            asset.name = model.name.clone();
            asset.name_origin = "resource".into();
        }
        let parent = asset.id.clone();
        assets.push(asset);
        for texture in model.embedded_textures.iter().flatten() {
            assets.push(texture_child(source, &parent, texture, "embedded dictionary"));
        }
    }

    let dictionaries = recover_batches(&batches, "Validating texture candidates", candidates.len(), report, cancel, |batch| {
        scan::recover_dictionaries(path, length, batch, cancel)
    })?;
    for entry in dictionaries.into_iter().flatten() {
        let asset = locator(source, &entry, "dictionary");
        let parent = asset.id.clone();
        assets.push(asset);
        for texture in &entry.content.textures {
            assets.push(texture_child(source, &parent, texture, "RPF texture dictionary"));
        }
    }
    Ok(())
}

/// Validates candidate batches in parallel and returns them in input order.
fn recover_batches<T: Send>(
    batches: &[&[i64]],
    phase: &str,
    total: usize,
    report: &(dyn Fn(IndexProgress) + Sync),
    cancel: &Cancel,
    recover: impl Fn(&[i64]) -> Result<Vec<T>> + Sync,
) -> Result<Vec<Vec<T>>> {
    report(IndexProgress::new(phase, 0, total as i64, "entries"));
    let completed = AtomicUsize::new(0);
    let results: Vec<Result<Vec<T>>> = batches
        .par_iter()
        .map(|batch| {
            cancel.check()?;
            let found = recover(batch)?;
            let done = completed.fetch_add(batch.len(), Ordering::Relaxed) + batch.len();
            report(IndexProgress::new(phase, done as i64, total as i64, "entries"));
            Ok(found)
        })
        .collect();
    results.into_iter().collect()
}

fn locator<T>(source: &Source, entry: &Recovered<T>, kind: &str) -> Asset {
    let segments: Vec<ByteRange> = entry
        .segments
        .iter()
        .map(|s| ByteRange { offset: s.offset, length: s.stored_size, decoded_length: Some(s.size) })
        .collect();
    let last = segments.last().cloned().unwrap_or(ByteRange::new(entry.offset, 1));
    let mut asset = Asset::new(format!("{}:{kind}:{:X}", source.id, entry.offset), &source.id, kind, entry.name.clone(), "alias", entry.compression);
    asset.range = Some(ByteRange { offset: entry.offset, length: last.offset + last.length - entry.offset, decoded_length: Some(entry.size) });
    asset.segments = Some(segments);
    asset.system_bytes = Some(entry.system_bytes);
    asset
}

fn texture_child(source: &Source, parent: &str, texture: &DictionaryTexture, evidence: &str) -> Asset {
    let alias = texture.name.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("texture_"));
    let mut asset = Asset::new(
        format!("{parent}:texture:{}", texture.index),
        &source.id,
        "texture",
        texture.name.clone(),
        if alias { "alias" } else { "resource" },
        evidence,
    );
    asset.index = Some(texture.index);
    asset.native_key = key(texture.asset_key);
    asset.name_hash = key(Some(texture.name_hash as u64));
    asset.parent_id = Some(parent.to_string());
    asset
}

/// Inflates every segment of a recovered resource and joins them.
pub fn decode_segments(file: &mut File, asset: &Asset, cancel: &Cancel) -> Result<Vec<u8>> {
    let segments = asset.segments.as_ref().filter(|s| !s.is_empty());
    let (Some(segments), Some(system)) = (segments, asset.system_bytes.filter(|&s| s > 0)) else {
        return Err(Error::invalid("A validated segmented resource is required."));
    };
    let size: i64 = segments.iter().map(|s| s.decoded_length.unwrap_or(0)).sum();
    if size > MAX_RESOURCE_BYTES || system as i64 >= size {
        return Err(Error::invalid("Invalid resource section lengths."));
    }
    let file_length = file.metadata()?.len() as i64;
    let mut output = Vec::with_capacity(size as usize);
    for segment in segments {
        cancel.check()?;
        segment.validate(file_length)?;
        if segment.length > MAX_RESOURCE_BYTES {
            return Err(Error::invalid("Compressed segment exceeds the resource limit."));
        }
        let mut packed = vec![0u8; segment.length as usize];
        file.seek(SeekFrom::Start(segment.offset as u64))?;
        file.read_exact(&mut packed)?;
        let count = segment.decoded_length.unwrap_or(0);
        if count <= 0 || count > i32::MAX as i64 {
            return Err(Error::invalid("Invalid resource section lengths."));
        }
        output.extend(zlib::inflate_exactly(&packed, count as usize)?);
    }
    Ok(output)
}

fn split(data: &[u8], asset: &Asset) -> Result<(usize, usize)> {
    let system = asset.system_bytes.unwrap_or(0) as usize;
    if system == 0 || system >= data.len() {
        return Err(Error::invalid("Invalid resource section lengths."));
    }
    Ok((system, data.len()))
}

pub fn read_model(catalog: &Catalog, asset: &Asset) -> Result<Model> {
    if asset.kind != "model" {
        return Err(Error::invalid("Select a validated model."));
    }
    let source = catalog.source_of(asset)?;
    let mut file = source.open()?;
    source.fingerprint.check(source.path(), &mut file, false)?;
    if asset.drawable_locator.is_some() {
        return resource_models::read(source, asset);
    }
    let data = decode_segments(&mut file, asset, &Cancel::none())?;
    let (system, _) = split(&data, asset)?;
    drawable::decode(&data[..system], &data[system..]).ok_or_else(|| Error::invalid("Model structure failed validation."))
}

/// Reads a texture as DDS: the native mip chain for pools and dictionaries,
/// or an 8-bit preview mip for textures embedded in models.
pub fn read_texture(catalog: &Catalog, asset: &Asset, cancel: &Cancel) -> Result<(Vec<u8>, TextureExport)> {
    cancel.check()?;
    let (Some(index), true) = (asset.index, asset.kind == "texture") else {
        return Err(Error::invalid("Select a validated texture."));
    };
    let source = catalog.source_of(asset)?;
    let mut file = source.open()?;
    source.fingerprint.check(source.path(), &mut file, false)?;
    let (dds_bytes, width, height, source_format, source_mips, base_mip, decoder);
    if source.format == "ttpf" {
        let pool = PoolIndex::read(source.path(), cancel)?;
        let texture = pool
            .info
            .textures
            .iter()
            .find(|t| t.index == index)
            .ok_or_else(|| Error::invalid("Texture structure failed validation."))?;
        if key(Some(texture.asset_key)) != asset.native_key {
            return Err(Error::invalid("Native texture key differs."));
        }
        dds_bytes = pool.create_dds(index, cancel)?;
        (width, height, source_format, source_mips, base_mip) =
            (texture.width, texture.height, texture.dxgi_format, texture.source_mips, texture.base_mip_level);
        decoder = "ViceModVI pool reader";
    } else {
        let parent = catalog.get(asset.parent_id.as_deref().ok_or_else(|| Error::invalid("Missing RPF texture container."))?)?;
        let data = decode_segments(&mut file, parent, cancel)?;
        cancel.check()?;
        let (system_length, _) = split(&data, parent)?;
        let (system, graphics) = data.split_at(system_length);
        let dictionary_offset = if parent.kind == "model" {
            Some(
                drawable::decode(system, graphics)
                    .and_then(|m| m.embedded_texture_dictionary_offset)
                    .ok_or_else(|| Error::invalid("Embedded texture dictionary is unavailable."))?,
            )
        } else {
            None
        };
        let dictionary = match dictionary_offset {
            Some(start) => dictionary::inspect_embedded_dictionary(system, graphics.len() as i64, start as i64),
            None => dictionary::inspect(system, graphics),
        };
        let texture = dictionary
            .and_then(|d| d.textures.into_iter().find(|t| t.index == index))
            .ok_or_else(|| Error::invalid("Texture structure failed validation."))?;
        if key(texture.asset_key) != asset.native_key || key(Some(texture.name_hash as u64)) != asset.name_hash {
            return Err(Error::invalid("RPF texture identifiers differ."));
        }
        if let Some(at) = dictionary_offset {
            base_mip = texture.preview_mip_level();
            let pixels = dictionary::decode_embedded_texture_pixels(system, graphics, at as i64, index, base_mip)?;
            dds_bytes = dds::from_pixels(&pixels);
        } else {
            base_mip = 0;
            dds_bytes = dictionary::create_dds(system, graphics, index)?;
        }
        (width, height, source_format, source_mips) = (texture.width, texture.height, texture.dxgi_format, texture.mip_count);
        decoder = "ViceModVI dictionary reader";
    }
    cancel.check()?;
    let info = dds::inspect(&dds_bytes)?;
    let mut coverage = if base_mip == 0 && info.mips == source_mips { "complete mip chain" } else { "partial mip chain" }.to_string();
    if source_format != info.dxgi_format {
        coverage.push_str(", converted pixels");
    }
    let export = TextureExport {
        width: info.width,
        height: info.height,
        dxgi_format: info.dxgi_format,
        source_width: width,
        source_height: height,
        source_format,
        source_mips,
        exported_mips: info.mips,
        base_mip,
        decoder: decoder.to_string(),
        coverage,
    };
    Ok((dds_bytes, export))
}

/// Exports a texture as DDS or a model as OBJ.
pub fn export(catalog: &Catalog, asset: &Asset, output: &Path) -> Result<ExportResult> {
    if asset.kind == "texture" {
        let (bytes, info) = read_texture(catalog, asset, &Cancel::none())?;
        files::write_bytes(output, &bytes, false)?;
        return Ok(ExportResult::Texture(info));
    }
    let model = read_model(catalog, asset)?;
    let obj = model::to_obj(&model);
    files::write(output, |w| Ok(w.write_all(obj.as_bytes())?), false)?;
    Ok(ExportResult::Mesh(MeshExport {
        vertices: model.meshes.iter().map(|m| m.positions.len() as i64).sum(),
        faces: model.meshes.iter().map(|m| m.indices.len() as i64).sum::<i64>() / 3,
        meshes: model.meshes.len() as i64,
        materials: model.materials.as_ref().map_or(0, |m| m.len() as i64),
        decoder: if asset.drawable_locator.is_none() { "ViceModVI drawable reader" } else { "Resource Tools" }.to_string(),
        coverage: "static geometry and available UVs".to_string(),
    }))
}

/// Recursively lists RPF and TTPF files in a folder, sorted ignoring case.
/// The archive copies in a ViceModVI mods folder are not game sources and
/// are skipped.
pub fn source_files(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                if !crate::mods::is_mods_folder(&path) {
                    pending.push(path);
                }
            } else if matches!(crate::util::extension(&path).as_str(), ".rpf" | ".ttpf") {
                found.push(path);
            }
        }
    }
    found.sort_by_key(|p| path_text(p).to_uppercase());
    Ok(found)
}
