//! Resolves the diffuse texture slots of a model to catalog textures.

use std::collections::HashMap;

use serde::Serialize;

use crate::{
    catalog::{Asset, Catalog},
    model_texture_bindings::{color_textures, ModelColorTexture},
    rage_model::RageModel,
    xbox_assets::key,
    Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkCandidate {
    pub asset_id: String,
    pub source: String,
    pub evidence: String,
    pub key: Option<String>,
    pub name_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureLink {
    pub shader: i32,
    pub slot: String,
    pub parameter_hash: String,
    pub key: Option<String>,
    pub name_hash: Option<String>,
    pub texture_index: i32,
    pub system_offset: i32,
    pub descriptor_offset: i32,
    pub tag: String,
    pub status: String,
    pub reason: String,
    pub texture_id: Option<String>,
    pub candidates: Vec<LinkCandidate>,
}

/// Catalog textures grouped by native key and by name hash, in catalog order.
pub struct TextureIndex {
    by_key: HashMap<String, Vec<usize>>,
    by_hash: HashMap<String, Vec<usize>>,
}

impl TextureIndex {
    pub fn new(catalog: &Catalog) -> Self {
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_hash: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, asset) in catalog.assets.iter().enumerate().filter(|(_, a)| a.kind == "texture") {
            if let Some(key) = &asset.native_key {
                by_key.entry(key.clone()).or_default().push(i);
            }
            if let Some(hash) = &asset.name_hash {
                by_hash.entry(hash.clone()).or_default().push(i);
            }
        }
        Self { by_key, by_hash }
    }
}

pub fn resolve_model(catalog: &Catalog, index: &TextureIndex, asset: &Asset, model: &RageModel) -> Result<Vec<TextureLink>> {
    let mut links = Vec::new();
    for material in model.materials.iter().flatten() {
        for slot in color_textures(material) {
            links.push(resolve(catalog, index, asset, material.shader_index, &slot)?);
        }
    }
    Ok(links)
}

pub fn resolve(catalog: &Catalog, index: &TextureIndex, model: &Asset, shader: i32, slot: &ModelColorTexture) -> Result<TextureLink> {
    let reference = &slot.reference;
    let native_key = key(Some(reference.opaque_id64));
    let hash = key(Some(reference.name_hash() as u64));
    let lookup = |map: &HashMap<String, Vec<usize>>, value: &Option<String>| -> Vec<&Asset> {
        value
            .as_ref()
            .and_then(|v| map.get(v))
            .map(|items| items.iter().map(|&i| &catalog.assets[i]).collect())
            .unwrap_or_default()
    };
    let exact = lookup(&index.by_key, &native_key);
    let local: Vec<&Asset> = exact.iter().copied().filter(|a| a.parent_id.as_deref() == Some(model.id.as_str())).collect();
    let mut matches = if local.is_empty() { exact } else { local.clone() };
    let has_exact_key = !matches.is_empty();
    if !has_exact_key && hash.is_some() {
        matches = lookup(&index.by_hash, &hash);
    }
    let confirmed = has_exact_key && matches.len() == 1 && !slot.name_inferred;
    let status = if confirmed {
        "confirmed"
    } else if matches.len() > 1 {
        "ambiguous"
    } else {
        "unresolved"
    };
    let reason = match (status, !local.is_empty(), has_exact_key) {
        ("confirmed", true, _) => "Full key matches an embedded texture in this model.",
        ("confirmed", false, _) => "Full key matches one texture in the catalog.",
        ("ambiguous", _, true) => "Several textures share the full key in the applicable scope.",
        ("ambiguous", _, false) => "Several textures share the name hash. A full key match is unavailable.",
        ("unresolved", _, true) => "The color slot is inferred from its parameter hash.",
        _ if matches.len() == 1 => "A name hash alone provides insufficient evidence.",
        _ => "No matching texture identifier in the catalog.",
    };
    let candidates = matches
        .iter()
        .map(|asset| {
            Ok(LinkCandidate {
                asset_id: asset.id.clone(),
                source: catalog.source_of(asset)?.path.clone(),
                evidence: asset.evidence.clone(),
                key: asset.native_key.clone(),
                name_hash: asset.name_hash.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TextureLink {
        shader,
        slot: slot.parameter_name.to_string(),
        parameter_hash: format!("{:08X}", reference.parameter_hash),
        key: native_key,
        name_hash: hash,
        texture_index: reference.texture_index,
        system_offset: reference.system_offset,
        descriptor_offset: reference.asset_descriptor_offset,
        tag: format!("{:08X}", reference.tag),
        status: status.to_string(),
        reason: reason.to_string(),
        texture_id: confirmed.then(|| matches[0].id.clone()),
        candidates,
    })
}
