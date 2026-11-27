//! Resolves shader texture references to catalog textures.

use std::collections::HashMap;

use serde::Serialize;

use crate::{
    catalog::{Asset, Catalog},
    native::{
        model::{Model, TextureRef},
        slots::{color_textures, ColorTexture},
    },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapRole {
    Diffuse,
    /// The parameter name is inferred and the color binding is unverified.
    InferredDiffuse,
    // These parameter conventions were measured on map and character materials.
    // They do not establish how an individual shader samples the texture.
    InferredNormal,
    InferredPackedSpecular,
    InferredPackedMaterial,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialMap {
    pub role: MapRole,
    /// Reference evidence. The slot name is empty if the parameter is unknown.
    pub link: TextureLink,
}

impl MaterialMap {
    /// A confirmed reference suitable for the default color preview.
    pub fn verified_color_id(&self) -> Option<&str> {
        self.link.texture_id.as_deref().filter(|_| self.role == MapRole::Diffuse)
    }
}

/// The earliest recognized color binding of a material, in slot priority order.
/// An unverified primary binding takes precedence over later layers or slots.
pub fn preferred_color_map(maps: &[MaterialMap], shader: i32) -> Option<(usize, &MaterialMap)> {
    maps.iter().enumerate().find(|(_, map)| {
        map.link.shader == shader && matches!(map.role, MapRole::Diffuse | MapRole::InferredDiffuse)
    })
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

pub fn resolve_model(catalog: &Catalog, index: &TextureIndex, asset: &Asset, model: &Model) -> Result<Vec<TextureLink>> {
    let mut links = Vec::new();
    for material in model.materials.iter().flatten() {
        for slot in color_textures(material) {
            links.push(resolve(catalog, index, asset, material.shader_index, &slot)?);
        }
    }
    Ok(links)
}

/// All parsed shader maps, with reference resolution and shader role evaluated
/// independently. Color slots retain preview priority within each material.
pub fn resolve_material_maps(catalog: &Catalog, index: &TextureIndex, asset: &Asset, model: &Model) -> Result<Vec<MaterialMap>> {
    let mut maps = Vec::new();
    for material in model.materials.iter().flatten() {
        let color_slots = color_textures(material);
        for slot in &color_slots {
            let role = if slot.name_inferred { MapRole::InferredDiffuse } else { MapRole::Diffuse };
            let link = resolve_reference(catalog, index, asset, material.shader_index, &slot.reference, slot.parameter_name)?;
            maps.push(MaterialMap { role, link });
        }
        for reference in &material.textures {
            if color_slots.iter().any(|slot| slot.reference.parameter_hash == reference.parameter_hash) {
                continue;
            }
            // Leonida's measured character and map bindings agree on these
            // parameter names and broad roles (dde82f27e, docs/textures.md).
            let (role, name) = match reference.parameter_hash {
                0x1E77_BF4C => (MapRole::InferredNormal, "bumptex"),
                0xC6F4_6E79 => (MapRole::InferredPackedSpecular, "speculartex"),
                0x739E_3D0D => (MapRole::InferredPackedMaterial, "speculartex2"),
                _ => (MapRole::Unknown, ""),
            };
            let link = resolve_reference(catalog, index, asset, material.shader_index, reference, name)?;
            maps.push(MaterialMap { role, link });
        }
    }
    Ok(maps)
}

pub fn resolve(catalog: &Catalog, index: &TextureIndex, model: &Asset, shader: i32, slot: &ColorTexture) -> Result<TextureLink> {
    let mut link = resolve_reference(catalog, index, model, shader, &slot.reference, slot.parameter_name)?;
    // The diffuse-only API confirms both the reference and its color role.
    if slot.name_inferred && link.texture_id.is_some() {
        link.status = "unresolved".into();
        link.reason = "The color slot is inferred from its parameter hash.".into();
        link.texture_id = None;
    }
    Ok(link)
}

fn resolve_reference(catalog: &Catalog, index: &TextureIndex, model: &Asset, shader: i32, reference: &TextureRef, slot: &str) -> Result<TextureLink> {
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
    let has_local_match = !local.is_empty();
    let mut matches = if has_local_match { local } else { exact };
    let has_exact_key = !matches.is_empty();
    if !has_exact_key && hash.is_some() {
        matches = lookup(&index.by_hash, &hash);
    }
    let confirmed = has_exact_key && matches.len() == 1;
    let status = if confirmed {
        "confirmed"
    } else if matches.len() > 1 {
        "ambiguous"
    } else {
        "unresolved"
    };
    let reason = match (status, has_local_match, has_exact_key) {
        ("confirmed", true, _) => "Full key matches an embedded texture in this model.",
        ("confirmed", false, _) => "Full key matches one texture in the catalog.",
        ("ambiguous", _, true) => "Several textures share the full key in the applicable scope.",
        ("ambiguous", _, false) => "Several textures share the name hash. A full key match is unavailable.",
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
        slot: slot.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{Fingerprint, Source},
        native::{model::Material, slots::joaat},
    };

    #[test]
    fn material_map_roles_are_independent_of_reference_resolution() {
        let mut catalog = Catalog::new();
        catalog.sources.push(Source {
            id: "49".into(),
            path: "ttdb_weapons.ttpf".into(),
            format: "TTPF".into(),
            fingerprint: Fingerprint {
                length: 0,
                sha256: String::new(),
                samples_sha256: String::new(),
                last_write_utc: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        });
        let mut texture = Asset::new("49:ttpf:1475".into(), "49", "texture", "sample".into(), "synthetic", "test");
        texture.native_key = key(Some(1));
        texture.name_hash = key(Some(2));
        catalog.assets.push(texture);
        let asset = Asset::new("model".into(), "4", "model", "sample".into(), "synthetic", "test");
        let reference = |parameter_hash, opaque_id64| TextureRef {
            parameter_hash,
            texture_index: 0,
            system_offset: 0,
            asset_descriptor_offset: 0,
            opaque_id64,
            opaque_id32: 2,
            tag: 6,
        };
        let material = |shader_index, shader_hash, textures| Material {
            shader_index,
            shader_hash,
            preset_hash: 0,
            textures,
            render_bucket: None,
            render_bucket_mask: None,
        };
        let mut model = Model::new("sample".into(), Vec::new(), [0.0; 3], [0.0; 3]);
        model.materials = Some(vec![
            material(0, 0xE68A_4A98, vec![
                reference(joaat("bumptex"), 1),
                reference(joaat("speculartex"), 1),
                reference(joaat("speculartex2"), 1),
                reference(0x1234_5678, 1),
                reference(0x8765_4321, 3),
            ]),
            material(1, 0, vec![reference(joaat("lyr0diffusetex"), 1), reference(joaat("diffusetex"), 1)]),
            material(2, 0, vec![reference(joaat("diffusetex2"), 1), reference(joaat("lyr0diffusetex"), 1)]),
        ]);
        let index = TextureIndex::new(&catalog);
        let maps = resolve_material_maps(&catalog, &index, &asset, &model).unwrap();
        let entry = Some("49:ttpf:1475");
        assert_eq!(
            maps.iter().map(|m| (m.role, m.link.status.as_str(), m.link.texture_id.as_deref())).collect::<Vec<_>>(),
            [
                (MapRole::InferredNormal, "confirmed", entry),
                (MapRole::InferredPackedSpecular, "confirmed", entry),
                (MapRole::InferredPackedMaterial, "confirmed", entry),
                (MapRole::Unknown, "confirmed", entry),
                (MapRole::Unknown, "unresolved", None),
                (MapRole::Diffuse, "confirmed", entry),
                (MapRole::InferredDiffuse, "confirmed", entry),
                (MapRole::InferredDiffuse, "confirmed", entry),
                (MapRole::Diffuse, "confirmed", entry),
            ]
        );
        assert_eq!(maps[0].link.parameter_hash, "1E77BF4C");
        assert_eq!(maps[0].link.slot, "bumptex");
        assert_eq!(maps[0].link.candidates[0].source, "ttdb_weapons.ttpf");
        assert_eq!(maps[4].link.candidates[0].asset_id, "49:ttpf:1475");
        assert!(preferred_color_map(&maps, 0).is_none());
        assert_eq!(preferred_color_map(&maps, 1).unwrap().1.verified_color_id(), entry);
        assert_eq!(preferred_color_map(&maps, 2).unwrap().1.verified_color_id(), None);
        assert_eq!(maps[8].verified_color_id(), entry);

        let legacy = resolve_model(&catalog, &index, &asset, &model).unwrap();
        assert_eq!(legacy[0], maps[5].link);
        assert_eq!(legacy[1].status, "unresolved");
        assert_eq!(legacy[1].texture_id, None);
    }
}
