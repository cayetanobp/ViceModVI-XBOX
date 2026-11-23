//! Source catalogs: fingerprinted sources and the assets found in them.
//!
//! The JSON layout matches the catalogs written by the C# build, so existing
//! `catalog.json` files and `.vmp` projects still load.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    files, time,
    util::{contains_ignore_case, eq_ignore_case, full_path, hex_upper, parse_hex_u64, path_text},
    Cancel, Error, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub offset: i64,
    pub length: i64,
    #[serde(default)]
    pub decoded_length: Option<i64>,
}

impl ByteRange {
    pub fn new(offset: i64, length: i64) -> Self {
        Self { offset, length, decoded_length: None }
    }

    pub fn validate(&self, file_length: i64) -> Result<()> {
        if self.offset < 0 || self.length <= 0 || self.offset > file_length || self.length > file_length - self.offset {
            return Err(Error::invalid(format!(
                "Range 0x{:X}+{} is outside the source ({} bytes).",
                self.offset,
                group_digits(self.length),
                group_digits(file_length)
            )));
        }
        Ok(())
    }
}

/// `N0` formatting: thousands separated by commas.
pub fn group_digits(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub length: i64,
    pub sha256: String,
    pub samples_sha256: String,
    #[serde(with = "crate::time")]
    pub last_write_utc: DateTime<Utc>,
}

const HASH_BUFFER: usize = 1024 * 1024;

impl Fingerprint {
    /// Full SHA-256 plus a sampled hash for quick later checks.
    pub fn create(path: &Path, file: &mut File, progress: Option<&dyn Fn(i64)>, cancel: &Cancel) -> Result<Self> {
        let stamp = time::last_write_utc(path)?;
        file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0u8; HASH_BUFFER];
        let mut position = 0i64;
        let mut next_report = 0i64;
        loop {
            cancel.check()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
            position += count as i64;
            if position >= next_report {
                if let Some(report) = progress {
                    report(position);
                }
                next_report = position + 32 * 1024 * 1024;
            }
        }
        if let Some(report) = progress {
            report(position);
        }
        let sha = hex_upper(&digest.finalize());
        let length = file.metadata()?.len() as i64;
        let (samples, _) = sample(file, length)?;
        if stamp != time::last_write_utc(path)? {
            return Err(Error::io("Source changed while being fingerprinted."));
        }
        Ok(Self { length, sha256: sha, samples_sha256: samples, last_write_utc: stamp })
    }

    /// Confirms that `file` still matches. A full check rehashes every byte;
    /// a quick check compares length, timestamp and sampled blocks. Returns
    /// the number of bytes read.
    pub fn check(&self, path: &Path, file: &mut File, full: bool) -> Result<i64> {
        let differs = || Error::invalid(format!("Source fingerprint differs: {}", path.display()));
        let length = file.metadata()?.len() as i64;
        if length != self.length || (!full && time::last_write_utc(path)? != self.last_write_utc) {
            return Err(differs());
        }
        if full {
            if hash_all(file)? != self.sha256 {
                return Err(Error::invalid(format!("Source SHA-256 differs: {}", path.display())));
            }
            return Ok(length);
        }
        let (hash, bytes) = sample(file, length)?;
        if hash != self.samples_sha256 || time::last_write_utc(path)? != self.last_write_utc {
            return Err(differs());
        }
        Ok(bytes)
    }
}

pub(crate) fn hash_all(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_upper(&digest.finalize()))
}

/// Hash of eight evenly spaced 4 KiB blocks, or of the whole file when small.
fn sample(file: &mut File, length: i64) -> Result<(String, i64)> {
    const COUNT: i64 = 8;
    const BLOCK: i64 = 4096;
    if length <= COUNT * BLOCK {
        return Ok((hash_all(file)?, length));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; BLOCK as usize];
    let span = length - BLOCK;
    for i in 0..COUNT {
        file.seek(SeekFrom::Start((span / (COUNT - 1) * i + span % (COUNT - 1) * i / (COUNT - 1)) as u64))?;
        file.read_exact(&mut buffer)?;
        digest.update(buffer);
    }
    Ok((hex_upper(&digest.finalize()), COUNT * BLOCK))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: String,
    pub path: String,
    pub format: String,
    pub fingerprint: Fingerprint,
}

impl Source {
    pub fn open(&self) -> Result<File> {
        Ok(File::open(&self.path)?)
    }

    pub fn path(&self) -> &Path {
        Path::new(&self.path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub source_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub name_origin: String,
    pub evidence: String,
    #[serde(default)]
    pub range: Option<ByteRange>,
    #[serde(default)]
    pub segments: Option<Vec<ByteRange>>,
    #[serde(default)]
    pub system_bytes: Option<i32>,
    #[serde(default)]
    pub index: Option<i32>,
    #[serde(default)]
    pub native_key: Option<String>,
    #[serde(default)]
    pub name_hash: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub manifest_type: Option<String>,
    #[serde(default)]
    pub drawable_locator: Option<String>,
}

impl Asset {
    pub fn new(id: String, source_id: &str, kind: &str, name: String, name_origin: &str, evidence: &str) -> Self {
        Self {
            id,
            source_id: source_id.to_string(),
            kind: kind.to_string(),
            name,
            name_origin: name_origin.to_string(),
            evidence: evidence.to_string(),
            range: None,
            segments: None,
            system_bytes: None,
            index: None,
            native_key: None,
            name_hash: None,
            parent_id: None,
            manifest_type: None,
            drawable_locator: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadMeasurement {
    pub asset_id: String,
    pub verification_bytes: i64,
    pub payload_bytes: i64,
    pub total_bytes: i64,
}

pub const CURRENT_VERSION: i32 = 1;

fn current_version() -> i32 {
    CURRENT_VERSION
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    #[serde(default = "current_version")]
    pub version: i32,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub assets: Vec<Asset>,
    /// Asset id to position. Rebuilt by [`Catalog::validate`].
    #[serde(skip)]
    index: HashMap<String, usize>,
}

impl Catalog {
    pub fn new() -> Self {
        Self { version: CURRENT_VERSION, ..Self::default() }
    }

    pub fn source(&self, id: &str) -> Result<&Source> {
        let mut found = self.sources.iter().filter(|s| s.id == id);
        match (found.next(), found.next()) {
            (Some(source), None) => Ok(source),
            _ => Err(Error::invalid(format!("Unknown source {id}."))),
        }
    }

    pub fn source_of(&self, asset: &Asset) -> Result<&Source> {
        self.source(&asset.source_id)
    }

    pub fn get(&self, id: &str) -> Result<&Asset> {
        self.position(id).map(|i| &self.assets[i]).ok_or_else(|| Error::invalid(format!("Catalog entry not found: {id}")))
    }

    pub fn position(&self, id: &str) -> Option<usize> {
        if let Some(&i) = self.index.get(id) {
            if self.assets.get(i).is_some_and(|a| a.id == id) {
                return Some(i);
            }
        }
        self.assets.iter().position(|a| a.id == id)
    }

    /// Rebuilds the id lookup after assets were added or replaced.
    pub fn reindex(&mut self) {
        self.index = self.assets.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
    }

    pub fn find(&self, source: Option<&str>, kind: Option<&str>, name: Option<&str>, hash: Option<&str>, offset: Option<i64>) -> Result<Vec<&Asset>> {
        let sources: HashSet<&str> = self
            .sources
            .iter()
            .filter(|s| source.is_none_or(|text| s.id == text || contains_ignore_case(&s.path, text)))
            .map(|s| s.id.as_str())
            .collect();
        let key = hash.map(hex_key).transpose()?;
        Ok(self
            .assets
            .iter()
            .filter(|a| {
                sources.contains(a.source_id.as_str())
                    && kind.is_none_or(|k| eq_ignore_case(&a.kind, k))
                    && name.is_none_or(|n| contains_ignore_case(&a.name, n))
                    && key.as_ref().is_none_or(|k| a.native_key.as_ref() == Some(k) || a.name_hash.as_ref() == Some(k))
                    && offset.is_none_or(|o| a.range.as_ref().map(|r| r.offset) == Some(o))
            })
            .collect())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut catalog: Catalog = files::load(path)?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Opens the catalog against the selected installation, even if it was moved.
    pub fn load_for_folder(path: &Path, folder: &Path, cancel: &Cancel) -> Result<Self> {
        let mut catalog = Self::load(path)?;
        let Some(first) = catalog.sources.first() else {
            return Ok(catalog);
        };
        let old_root = first.path().ancestors().skip(1)
            .find(|root| catalog.sources.iter().all(|source| source.path().starts_with(root)))
            .ok_or_else(|| Error::invalid("Catalog sources do not share an installation folder."))?
            .to_path_buf();

        for source in &mut catalog.sources {
            cancel.check()?;
            let original = source.path();
            if original.starts_with(folder) {
                continue;
            }
            let relative = original.strip_prefix(&old_root)
                .map_err(|_| Error::invalid("Source is outside the catalog installation folder."))?;
            if !relative.components().all(|part| matches!(part, std::path::Component::Normal(_)))
                || relative.as_os_str().is_empty()
            {
                return Err(Error::invalid("Invalid relative path in the catalog."));
            }
            let relocated = folder.join(relative);
            let mut file = File::open(&relocated).map_err(|error| {
                Error::invalid(format!("Cannot relocate catalog source {}: {error}", relocated.display()))
            })?;
            source.fingerprint.check(&relocated, &mut file, false)?;
            source.path = path_text(&relocated);
        }
        Ok(catalog)
    }

    pub fn save(&mut self, path: &Path, replace: bool) -> Result<()> {
        let target = path_text(&full_path(path)?);
        if self.sources.iter().any(|s| eq_ignore_case(&s.path, &target)) {
            return Err(Error::io("A catalog cannot replace a source file."));
        }
        self.validate()?;
        files::save(path, self, replace)
    }

    pub fn validate(&mut self) -> Result<()> {
        if self.version != CURRENT_VERSION {
            return Err(Error::invalid(format!("Unsupported catalog version {}.", self.version)));
        }
        let mut sources: HashMap<&str, &Source> = HashMap::new();
        for source in &self.sources {
            if sources.insert(source.id.as_str(), source).is_some() {
                return Err(Error::invalid(format!("Duplicate catalog source {}.", source.id)));
            }
        }
        let mut assets: HashMap<&str, &Asset> = HashMap::with_capacity(self.assets.len());
        for asset in &self.assets {
            if assets.insert(asset.id.as_str(), asset).is_some() {
                return Err(Error::invalid(format!("Duplicate catalog entry {}.", asset.id)));
            }
        }
        for asset in &self.assets {
            let source = sources
                .get(asset.source_id.as_str())
                .ok_or_else(|| Error::invalid(format!("Unknown source for {}.", asset.id)))?;
            let length = source.fingerprint.length;
            if let Some(range) = &asset.range {
                range.validate(length)?;
            }
            if asset.index.is_some_and(|i| i < 0) {
                return Err(Error::invalid(format!("Invalid local index for {}.", asset.id)));
            }
            if let Some(parent_id) = &asset.parent_id {
                let valid = assets
                    .get(parent_id.as_str())
                    .is_some_and(|p| p.source_id == asset.source_id && matches!(p.kind.as_str(), "model" | "dictionary"));
                if !valid {
                    return Err(Error::invalid(format!("Invalid parent for {}.", asset.id)));
                }
            }
            let Some(segments) = asset.segments.as_ref().filter(|s| !s.is_empty()) else { continue };
            let extent = asset
                .range
                .as_ref()
                .filter(|r| r.offset == segments[0].offset)
                .ok_or_else(|| Error::invalid(format!("Missing segment extent for {}.", asset.id)))?;
            let mut end = extent.offset;
            for segment in segments {
                segment.validate(length)?;
                if segment.offset < end
                    || segment.offset + segment.length > extent.offset + extent.length
                    || segment.decoded_length.is_none_or(|d| d <= 0)
                {
                    return Err(Error::invalid(format!("Invalid segment for {}.", asset.id)));
                }
                end = segment.offset + segment.length;
            }
        }
        self.reindex();
        Ok(())
    }

    /// Copies the stored bytes of a ranged asset after checking its source.
    /// With an alternate source, that file must match the full fingerprint.
    pub fn copy_stored(&self, asset: &Asset, output: &mut dyn Write, alternate: Option<&Path>) -> Result<ReadMeasurement> {
        let source = self.source_of(asset)?;
        let path = match alternate {
            Some(path) => full_path(path)?,
            None => source.path().to_path_buf(),
        };
        let mut file = File::open(&path)?;
        let checked = source.fingerprint.check(&path, &mut file, alternate.is_some())?;
        let range = asset.range.as_ref().ok_or_else(|| Error::invalid("This entry is addressed by its native index."))?;
        range.validate(file.metadata()?.len() as i64)?;
        file.seek(SeekFrom::Start(range.offset as u64))?;
        let mut remaining = range.length;
        let mut buffer = vec![0u8; 65536];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as i64) as usize;
            file.read_exact(&mut buffer[..count])?;
            output.write_all(&buffer[..count])?;
            remaining -= count as i64;
        }
        Ok(ReadMeasurement {
            asset_id: asset.id.clone(),
            verification_bytes: checked,
            payload_bytes: range.length,
            total_bytes: checked + range.length,
        })
    }
}

/// Normalizes a hexadecimal identifier to 16 uppercase digits.
pub fn hex_key(value: &str) -> Result<String> {
    parse_hex_u64(value)
        .map(|key| format!("{key:016X}"))
        .ok_or_else(|| Error::invalid(format!("Invalid hexadecimal identifier: {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_csharp_catalog_json() {
        let json = r#"{
  "version": 1,
  "sources": [ { "id": "0", "path": "C:\\x.ttpf", "format": "ttpf",
    "fingerprint": { "length": 100, "sha256": "AB", "samplesSha256": "CD", "lastWriteUtc": "2025-01-02T03:04:05.1234567Z" } } ],
  "assets": [ { "id": "0:ttpf:1", "sourceId": "0", "type": "texture", "name": "t", "nameOrigin": "alias",
    "evidence": "RDS5/ETPF descriptor", "range": null, "segments": null, "systemBytes": null, "index": 1,
    "nativeKey": "00000000000000AA", "nameHash": null, "parentId": null, "manifestType": null, "drawableLocator": null } ]
}"#;
        let mut catalog: Catalog = serde_json::from_str(json).unwrap();
        catalog.validate().unwrap();
        assert_eq!(catalog.get("0:ttpf:1").unwrap().index, Some(1));
        let text = files::to_json(&catalog).unwrap();
        assert!(text.contains("\"lastWriteUtc\": \"2025-01-02T03:04:05.1234567Z\""));
        assert!(text.contains("\"type\": \"texture\""));
        assert!(text.contains("\"drawableLocator\": null"));
        assert_eq!(catalog.find(None, Some("TEXTURE"), None, Some("0xaa"), None).unwrap().len(), 1);
    }

    #[test]
    fn rejects_duplicate_ids_and_bad_ranges() {
        let mut catalog: Catalog = serde_json::from_str(r#"{"version":1,"sources":[],"assets":[]}"#).unwrap();
        catalog.validate().unwrap();
        assert!(ByteRange::new(10, 5).validate(12).is_err());
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(hex_key("0x1f").unwrap(), "000000000000001F");
        assert!(hex_key("+1").is_err());
    }
}
