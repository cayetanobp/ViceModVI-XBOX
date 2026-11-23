//! Staged texture replacements and `.vmp` project files.

use std::{collections::HashSet, path::Path};

use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    catalog::{Asset, Catalog},
    dds::{self, DdsTextureInfo},
    files,
    util::{eq_ignore_case, full_path, path_text},
    xbox_assets::TextureExport,
    Error, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementTarget {
    pub asset_id: String,
    pub source_sha256: String,
    #[serde(default)]
    pub index: Option<i32>,
    #[serde(default)]
    pub native_key: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Byte arrays as base64 strings, like System.Text.Json.
mod base64_bytes {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedTexture {
    pub target: ReplacementTarget,
    pub original_file_name: String,
    #[serde(with = "base64_bytes")]
    pub dds: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureDifference {
    pub field: String,
    pub original: String,
    pub replacement: String,
}

fn version_one() -> i32 {
    1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementProject {
    #[serde(default = "version_one")]
    pub version: i32,
    #[serde(default)]
    pub catalog_path: Option<String>,
    #[serde(default)]
    pub embedded_catalog: Option<Catalog>,
    #[serde(default)]
    pub replacements: Vec<StagedTexture>,
    #[serde(default)]
    pub selected_asset_id: Option<String>,
    #[serde(default)]
    pub selected_mesh: Option<i32>,
    #[serde(default)]
    pub show_replacement: bool,
}

impl ReplacementProject {
    pub fn new() -> Self {
        Self { version: 1, ..Self::default() }
    }

    pub fn save(&mut self, path: &Path, catalog: &Catalog) -> Result<()> {
        let destination = path_text(&full_path(path)?);
        let clashes = self.catalog_path.as_deref().is_some_and(|c| eq_ignore_case(c, &destination))
            || catalog.sources.iter().any(|s| eq_ignore_case(&s.path, &destination));
        if clashes {
            return Err(Error::io("Choose a separate .vmp file for the replacement project."));
        }
        self.embedded_catalog = Some(catalog.clone());
        files::save(path, self, true)
    }

    /// Loads a project and confirms every staged texture still targets the
    /// same catalog entry and an unchanged source file.
    pub fn load(path: &Path) -> Result<(Self, Catalog)> {
        let mut project: ReplacementProject = files::load(path)?;
        if project.version != 1 {
            return Err(Error::invalid("Unsupported replacement project version."));
        }
        let mut catalog = match project.embedded_catalog.take() {
            Some(catalog) => catalog,
            None => Catalog::load(Path::new(project.catalog_path.as_deref().ok_or_else(|| Error::invalid("Project catalog is missing."))?))?,
        };
        catalog.validate()?;
        let mut verified = HashSet::new();
        let mut seen = HashSet::new();
        for replacement in &project.replacements {
            let asset = catalog.get(&replacement.target.asset_id)?;
            if asset.kind != "texture" || !seen.insert(asset.id.clone()) || replacement.target != identify(&catalog, asset)? {
                return Err(Error::invalid("A staged replacement targets a different catalog entry."));
            }
            let source = catalog.source_of(asset)?;
            if verified.insert(source.id.clone()) {
                let mut file = source.open()?;
                source.fingerprint.check(source.path(), &mut file, true)?;
            }
            dds::inspect(&replacement.dds)?;
        }
        project.embedded_catalog = Some(catalog.clone());
        Ok((project, catalog))
    }

    pub fn stage(&mut self, catalog: &Catalog, asset: &Asset, name: &str, dds_bytes: Vec<u8>) -> Result<()> {
        if asset.kind != "texture" {
            return Err(Error::invalid("Select a texture to replace."));
        }
        dds::inspect(&dds_bytes)?;
        let file_name = Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let staged = StagedTexture { target: identify(catalog, asset)?, original_file_name: file_name, dds: dds_bytes };
        match self.replacements.iter_mut().find(|r| r.target.asset_id == asset.id) {
            Some(existing) => *existing = staged,
            None => self.replacements.push(staged),
        }
        Ok(())
    }

    pub fn find(&self, asset: &Asset) -> Option<&StagedTexture> {
        self.replacements.iter().find(|r| r.target.asset_id == asset.id)
    }

    pub fn revert(&mut self, asset: &Asset) -> bool {
        let before = self.replacements.len();
        self.replacements.retain(|r| r.target.asset_id != asset.id);
        before != self.replacements.len()
    }

    pub fn rebase_source(&mut self, catalog: &Catalog, source_id: &str) -> Result<()> {
        let sha = catalog.source(source_id)?.fingerprint.sha256.clone();
        for replacement in &mut self.replacements {
            if catalog.get(&replacement.target.asset_id)?.source_id == source_id {
                replacement.target.source_sha256 = sha.clone();
            }
        }
        Ok(())
    }

    pub fn compare(original: &TextureExport, replacement: &DdsTextureInfo) -> Vec<TextureDifference> {
        [
            (
                "Dimensions",
                format!("{} × {}", original.source_width, original.source_height),
                format!("{} × {}", replacement.width, replacement.height),
            ),
            ("DXGI format", original.source_format.to_string(), replacement.dxgi_format.to_string()),
            ("Mip levels", original.source_mips.to_string(), replacement.mips.to_string()),
        ]
        .into_iter()
        .filter(|(_, a, b)| a != b)
        .map(|(field, original, replacement)| TextureDifference { field: field.into(), original, replacement })
        .collect()
    }
}

fn identify(catalog: &Catalog, asset: &Asset) -> Result<ReplacementTarget> {
    Ok(ReplacementTarget {
        asset_id: asset.id.clone(),
        source_sha256: catalog.source_of(asset)?.fingerprint.sha256.clone(),
        index: asset.index,
        native_key: asset.native_key.clone(),
        parent_id: asset.parent_id.clone(),
    })
}
