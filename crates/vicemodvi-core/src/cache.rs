//! Per-source index cache in the local application data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    catalog::{Asset, Source},
    files, time,
    util::hex_upper,
};

/// Bumped when the indexer changes the assets it produces. Revision 3 is
/// the first Rust indexer; it does not reuse caches written by the C# build.
const REVISION: &str = "3";
const VARIANT: &str = "rust";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    source: Source,
    assets: Vec<Asset>,
}

fn data_folder() -> Option<PathBuf> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(local));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(data));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
}

fn cache_path(path: &str) -> Option<PathBuf> {
    let identity = if cfg!(windows) { path.to_uppercase() } else { path.to_string() };
    let key = hex_upper(&Sha256::digest(identity.as_bytes()));
    Some(data_folder()?.join("ViceModVI").join(format!("source-index-{REVISION}-{VARIANT}")).join(format!("{key}.json")))
}

/// Restores the indexed assets of `path` when the file still matches the
/// cached fingerprint. Ids are rewritten for the new source id.
pub fn try_restore(path: &str, id: &str) -> Option<(Source, Vec<Asset>)> {
    let cache = cache_path(path)?;
    if !cache.is_file() {
        return None;
    }
    let cached: Snapshot = files::load(&cache).ok()?;
    let metadata = std::fs::metadata(path).ok()?;
    if cached.source.path != path
        || cached.source.fingerprint.length != metadata.len() as i64
        || cached.source.fingerprint.last_write_utc != time::last_write_utc(Path::new(path)).ok()?
    {
        return None;
    }
    let mut file = cached.source.open().ok()?;
    cached.source.fingerprint.check(Path::new(path), &mut file, false).ok()?;
    let old = cached.source.id.clone();
    let remap = |value: &str| -> Option<String> { Some(format!("{id}{}", value.get(old.len()..)?)) };
    let mut assets = Vec::with_capacity(cached.assets.len());
    for mut asset in cached.assets {
        asset.id = remap(&asset.id)?;
        asset.source_id = id.to_string();
        if let Some(parent) = &asset.parent_id {
            asset.parent_id = Some(remap(parent)?);
        }
        assets.push(asset);
    }
    let mut source = cached.source;
    source.id = id.to_string();
    Some((source, assets))
}

/// Stores the assets of one source. Failures leave indexing unaffected.
pub fn store(source: &Source, assets: &[Asset]) {
    let Some(cache) = cache_path(&source.path) else { return };
    if let Some(folder) = cache.parent() {
        if std::fs::create_dir_all(folder).is_err() {
            return;
        }
    }
    let snapshot = Snapshot { source: source.clone(), assets: assets.to_vec() };
    let _ = files::save(&cache, &snapshot, true);
}
