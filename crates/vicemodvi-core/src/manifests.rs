//! Imports names and types from an rpf9-explorer manifest.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::{
    catalog::{hex_key, Asset, Catalog},
    Error, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub matched: i32,
    pub unresolved: i32,
    pub different_names: i32,
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn required<'a>(row: &'a Value, key: &str) -> Result<&'a Value> {
    row.get(key).ok_or_else(|| Error::invalid(format!("The manifest row has no '{key}' property.")))
}

fn int32(value: &Value, key: &str) -> Result<i32> {
    required(value, key)?
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| Error::invalid(format!("The manifest '{key}' is not a 32-bit integer.")))
}

pub fn import(catalog: &mut Catalog, source_id: &str, path: &Path) -> Result<ImportResult> {
    let source = catalog.source(source_id)?.clone();
    let mut file = source.open()?;
    source.fingerprint.check(source.path(), &mut file, false)?;
    let document: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(digest) = document.get("sha256") {
        let matches = digest.as_str().is_some_and(|d| d.eq_ignore_ascii_case(&source.fingerprint.sha256));
        if !matches {
            return Err(Error::invalid("Manifest SHA-256 differs from the catalog source."));
        }
    }
    let rows = required(&document, if source.format == "ttpf" { "Textures" } else { "Items" })?
        .as_array()
        .ok_or_else(|| Error::invalid("The manifest list is not an array."))?;
    // Positions of this source's assets, with a snapshot for matching.
    let snapshot: Vec<(Asset, usize)> = catalog
        .assets
        .iter()
        .enumerate()
        .filter(|(_, a)| a.source_id == source_id)
        .map(|(i, a)| (a.clone(), i))
        .collect();
    let mut result = ImportResult { matched: 0, unresolved: 0, different_names: 0 };
    let mut apply = |catalog: &mut Catalog, row: &Value, candidates: Vec<&(Asset, usize)>, kind: &str| -> Result<()> {
        let mut candidates = candidates;
        if let Some(key) = text(row, "AssetKey").or_else(|| text(row, "Key")) {
            let hex = hex_key(key)?;
            candidates.retain(|(asset, _)| {
                asset.native_key.as_deref() == Some(hex.as_str()) || (hex == "0000000000000000" && asset.native_key.is_none())
            });
        }
        if candidates.len() != 1 {
            result.unresolved += 1;
            return Ok(());
        }
        let index = candidates[0].1;
        let current = &mut catalog.assets[index];
        let name = text(row, "Name");
        if let Some(name) = name {
            if current.name_origin == "resource" && current.name != name {
                result.different_names += 1;
            }
        }
        match name {
            Some(name) if current.name_origin == "alias" && !name.trim().is_empty() && name != current.name => {
                current.name = name.to_string();
                current.name_origin = "manifest".into();
            }
            _ => {}
        }
        current.manifest_type = Some(kind.to_string());
        result.matched += 1;
        Ok(())
    };
    for row in rows {
        if source.format == "ttpf" {
            let index = int32(row, "Index")?;
            let candidates = snapshot.iter().filter(|(a, _)| a.index == Some(index)).collect();
            apply(catalog, row, candidates, "texture")?;
            continue;
        }
        let offset = required(row, "Offset")?.as_i64().ok_or_else(|| Error::invalid("The manifest 'Offset' is not an integer."))?;
        let dictionary = row.get("Textures");
        let kind = if dictionary.is_some() { "dictionary" } else { "model" };
        let parents: Vec<&(Asset, usize)> = snapshot
            .iter()
            .filter(|(a, _)| a.kind == kind && a.range.as_ref().map(|r| r.offset) == Some(offset))
            .collect();
        apply(catalog, row, parents.clone(), kind)?;
        let children = dictionary.or_else(|| row.get("Embedded"));
        if let Some(children) = children {
            let (Some(children), [parent]) = (children.as_array(), parents.as_slice()) else { continue };
            for child in children {
                let index = int32(child, "Index")?;
                let candidates = snapshot
                    .iter()
                    .filter(|(a, _)| a.parent_id.as_deref() == Some(parent.0.id.as_str()) && a.index == Some(index))
                    .collect();
                apply(catalog, child, candidates, "texture")?;
            }
        }
    }
    Ok(result)
}
