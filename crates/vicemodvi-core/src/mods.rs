//! The game's mods folder. Confirmed texture replacements and the archive
//! copies built from them live in one folder inside the game installation,
//! so the original game files are never written and deleting that folder
//! removes every mod:
//!
//! ```text
//! <game>/mods/
//!   vicemodvi-mods.json              installed replacements and their originals
//!   textures/<game path>/<entry>.dds replacement images, one per texture
//!   files/<game path>                patched archive copies, in the game's layout
//! ```
//!
//! A runtime that loads `files/<game path>` in place of `<game path>`
//! shows the mods in game. Each copy is rebuilt from the untouched original
//! with every replacement of that archive, so mods to one archive stack and
//! removing one leaves the others.

use std::{
    fs::File,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    catalog::{Asset, Catalog, Fingerprint, Source},
    dds, files,
    pool_writer::{self, ScratchPath},
    rpf_copy,
    util::{eq_ignore_case, full_path, path_text, sha256_hex},
    Cancel, Error, Result,
};

/// Name of the mods folder in the game installation.
pub const FOLDER: &str = "mods";
/// Manifest file in the mods folder.
pub const MANIFEST: &str = "vicemodvi-mods.json";
/// Subfolder of the mods folder that mirrors the game's own layout.
pub const FILES: &str = "files";
const TEXTURES: &str = "textures";
/// Files in the root of an Xbox Series installation.
const MARKERS: [&str; 3] = ["game_scarlett_master.exe", "MicrosoftGame.Config", "appxmanifest.xml"];

fn version_one() -> i32 {
    1
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    #[serde(default = "version_one")]
    pub version: i32,
    #[serde(default)]
    pub textures: Vec<ModTexture>,
    #[serde(default)]
    pub files: Vec<ModFile>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self { version: 1, textures: Vec::new(), files: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModTexture {
    /// Game-relative path of the original archive, `/`-separated.
    pub source: String,
    /// Full SHA-256 of the original archive the replacement was made for.
    pub source_sha256: String,
    /// Catalog id without the source prefix, such as `ttpf:1510`.
    pub entry: String,
    #[serde(default)]
    pub native_key: Option<String>,
    pub name: String,
    /// Replacement DDS, relative to the mods folder.
    pub replacement: String,
    pub replacement_sha256: String,
    #[serde(default)]
    pub allow_resize: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModFile {
    /// Game-relative path. The copy is `files/<path>` in the mods folder.
    pub path: String,
    pub original_sha256: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub texture: ModTexture,
    /// The rebuilt archive copy in the mods folder.
    pub copy: String,
    pub copy_sha256: String,
}

/// The installation folder that holds `source`: the nearest folder with a
/// game executable or manifest, else `workspace` when it contains the source,
/// else the folder of the source itself.
pub fn game_root(source: &Path, workspace: Option<&Path>) -> Result<PathBuf> {
    let source = full_path(source)?;
    if let Some(root) = source.ancestors().skip(1).find(|folder| MARKERS.iter().any(|marker| folder.join(marker).is_file())) {
        return Ok(root.to_path_buf());
    }
    if let Some(workspace) = workspace.map(full_path).transpose()? {
        if source.starts_with(&workspace) && source != workspace {
            return Ok(workspace);
        }
    }
    source.parent().map(Path::to_path_buf).ok_or_else(|| Error::io("The source has no parent folder."))
}

/// Whether `folder` is a mods folder written by ViceModVI. Folder scans skip
/// it so the copies are not indexed as game sources.
pub fn is_mods_folder(folder: &Path) -> bool {
    folder.file_name().is_some_and(|name| eq_ignore_case(&name.to_string_lossy(), FOLDER)) && folder.join(MANIFEST).is_file()
}

/// Joins a `/`-separated relative path that may only name plain folders and files.
fn join(base: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = base.to_path_buf();
    let mut parts = 0;
    for part in relative.split('/') {
        let plain = Path::new(part).components().count() == 1 && matches!(Path::new(part).components().next(), Some(Component::Normal(_)));
        if !plain || part.contains([':', '\\']) {
            return Err(Error::invalid(format!("Invalid path in the mods manifest: {relative}")));
        }
        path.push(part);
        parts += 1;
    }
    if parts == 0 {
        return Err(Error::invalid("Empty path in the mods manifest."));
    }
    Ok(path)
}

/// Catalog id of `asset` without its source prefix.
fn entry_of(asset: &Asset, source: &Source) -> String {
    asset.id.strip_prefix(&source.id).and_then(|tail| tail.strip_prefix(':')).unwrap_or(&asset.id).to_string()
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The mods folder of one game installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mods {
    game: PathBuf,
}

impl Mods {
    pub fn new(game: &Path) -> Result<Self> {
        Ok(Self { game: full_path(game)? })
    }

    /// The mods folder of the installation that holds `source`.
    pub fn for_source(source: &Source, workspace: Option<&Path>) -> Result<Self> {
        Self::new(&game_root(source.path(), workspace)?)
    }

    pub fn game(&self) -> &Path {
        &self.game
    }

    pub fn folder(&self) -> PathBuf {
        self.game.join(FOLDER)
    }

    fn manifest_path(&self) -> PathBuf {
        self.folder().join(MANIFEST)
    }

    /// The installed mods; empty when the folder has none.
    pub fn manifest(&self) -> Result<Manifest> {
        let path = self.manifest_path();
        if !path.is_file() {
            return Ok(Manifest::default());
        }
        let manifest: Manifest = files::load(&path)?;
        if manifest.version != 1 {
            return Err(Error::invalid(format!("Unsupported mods manifest version {}.", manifest.version)));
        }
        for texture in &manifest.textures {
            join(&self.game, &texture.source)?;
            join(&self.game, &texture.replacement)?;
        }
        for file in &manifest.files {
            join(&self.game, &file.path)?;
        }
        Ok(manifest)
    }

    fn save(&self, manifest: &Manifest) -> Result<()> {
        let path = self.manifest_path();
        if manifest.textures.is_empty() && manifest.files.is_empty() {
            if path.is_file() {
                std::fs::remove_file(&path)?;
            }
            self.prune(&path);
            return Ok(());
        }
        std::fs::create_dir_all(self.folder())?;
        files::save(&path, manifest, true)
    }

    /// Removes the empty folders above `path`, up to and including the mods folder.
    fn prune(&self, path: &Path) {
        let folder = self.folder();
        let mut current = path.parent();
        while let Some(directory) = current.filter(|d| d.starts_with(&folder)) {
            if std::fs::remove_dir(directory).is_err() {
                break;
            }
            current = directory.parent();
        }
    }

    /// Game-relative path of `source`, `/`-separated.
    pub fn relative(&self, source: &Source) -> Result<String> {
        let path = full_path(source.path())?;
        let relative = path.strip_prefix(&self.game).map_err(|_| Error::invalid(format!("{} is outside the game folder.", path.display())))?;
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                _ => return Err(Error::invalid("Invalid source path for the mods folder.")),
            }
        }
        match parts.first() {
            None => Err(Error::invalid("Invalid source path for the mods folder.")),
            Some(first) if eq_ignore_case(first, FOLDER) => {
                Err(Error::invalid("This archive is a copy in the mods folder. Open the original game file to change a mod."))
            }
            Some(_) => Ok(parts.join("/")),
        }
    }

    /// Where the patched copy of a game-relative archive goes.
    pub fn copy_path(&self, relative: &str) -> Result<PathBuf> {
        join(&self.folder().join(FILES), relative)
    }

    /// The installed replacement of `asset`, if any. Fails when the game
    /// file changed since the replacement was made.
    pub fn saved(&self, catalog: &Catalog, asset: &Asset) -> Result<Option<Vec<u8>>> {
        let source = catalog.source_of(asset)?;
        let (relative, entry) = (self.relative(source)?, entry_of(asset, source));
        let manifest = self.manifest()?;
        let Some(texture) = manifest.textures.iter().find(|t| eq_ignore_case(&t.source, &relative) && t.entry == entry) else {
            return Ok(None);
        };
        if texture.source_sha256 != source.fingerprint.sha256 {
            return Err(Error::invalid("The saved replacement was made for a different version of this game file."));
        }
        let bytes = std::fs::read(join(&self.folder(), &texture.replacement)?)?;
        if sha256_hex(&bytes) != texture.replacement_sha256 {
            return Err(Error::invalid("The saved replacement file changed outside ViceModVI."));
        }
        Ok(Some(bytes))
    }

    /// Saves `dds_bytes` as the replacement of `asset` and rebuilds the
    /// patched copy of its archive in the mods folder. On failure the mods
    /// folder is left as it was.
    pub fn install(&self, catalog: &Catalog, asset: &Asset, dds_bytes: &[u8], allow_resize: bool, cancel: &Cancel) -> Result<Installed> {
        if asset.kind != "texture" {
            return Err(Error::invalid("Select a texture to replace."));
        }
        dds::decode_texture_pixels(dds_bytes, 0)?;
        let source = catalog.source_of(asset)?;
        let (relative, entry) = (self.relative(source)?, entry_of(asset, source));
        let mut manifest = self.manifest()?;
        let replacement = format!("{TEXTURES}/{relative}/{}.dds", entry.replace(':', "_"));
        let texture = ModTexture {
            source: relative.clone(),
            source_sha256: source.fingerprint.sha256.clone(),
            entry: entry.clone(),
            native_key: asset.native_key.clone(),
            name: asset.name.clone(),
            replacement: replacement.clone(),
            replacement_sha256: sha256_hex(dds_bytes),
            allow_resize,
        };
        let target = join(&self.folder(), &replacement)?;
        let previous = read_optional(&target)?;
        std::fs::create_dir_all(target.parent().ok_or_else(|| Error::io("The replacement has no folder."))?)?;
        files::write_bytes(&target, dds_bytes, true)?;
        manifest.textures.retain(|t| !(eq_ignore_case(&t.source, &relative) && t.entry == entry));
        manifest.textures.push(texture.clone());
        manifest.textures.sort_by(|a, b| (a.source.to_uppercase(), &a.entry).cmp(&(b.source.to_uppercase(), &b.entry)));
        let built = self.rebuild(catalog, &mut manifest, &relative, cancel).and_then(|copy| {
            self.save(&manifest)?;
            Ok(copy)
        });
        match built {
            Ok(Some((copy, copy_sha256))) => Ok(Installed { texture, copy: path_text(&copy), copy_sha256 }),
            Ok(None) => Err(Error::invalid("The archive copy was not built.")),
            Err(error) => {
                let _ = match previous {
                    Some(bytes) => files::write_bytes(&target, &bytes, true),
                    None => std::fs::remove_file(&target).map_err(Error::from),
                };
                self.prune(&target);
                if let Ok(copy) = self.copy_path(&relative) {
                    self.prune(&copy);
                }
                Err(error)
            }
        }
    }

    /// Removes the replacement of `asset` and rebuilds or deletes the copy of
    /// its archive. Returns false when `asset` had no replacement.
    pub fn uninstall(&self, catalog: &Catalog, asset: &Asset, cancel: &Cancel) -> Result<bool> {
        let source = catalog.source_of(asset)?;
        let (relative, entry) = (self.relative(source)?, entry_of(asset, source));
        let mut manifest = self.manifest()?;
        let Some(position) = manifest.textures.iter().position(|t| eq_ignore_case(&t.source, &relative) && t.entry == entry) else {
            return Ok(false);
        };
        let removed = manifest.textures.remove(position);
        self.rebuild(catalog, &mut manifest, &relative, cancel)?;
        let target = join(&self.folder(), &removed.replacement)?;
        if target.is_file() {
            std::fs::remove_file(&target)?;
        }
        self.prune(&target);
        self.save(&manifest)?;
        Ok(true)
    }

    /// Writes the copy of one archive with every replacement the manifest
    /// lists for it, starting from the untouched original, and records it.
    /// Deletes the copy when no replacement is left.
    fn rebuild(&self, catalog: &Catalog, manifest: &mut Manifest, relative: &str, cancel: &Cancel) -> Result<Option<(PathBuf, String)>> {
        let destination = self.copy_path(relative)?;
        let wanted = path_text(&join(&self.game, relative)?);
        let source = catalog
            .sources
            .iter()
            .find(|s| full_path(s.path()).is_ok_and(|p| eq_ignore_case(&path_text(&p), &wanted)))
            .ok_or_else(|| Error::invalid(format!("The catalog does not include {relative}.")))?;
        let textures: Vec<ModTexture> = manifest.textures.iter().filter(|t| eq_ignore_case(&t.source, relative)).cloned().collect();
        manifest.files.retain(|f| !eq_ignore_case(&f.path, relative));
        if textures.is_empty() {
            if destination.is_file() {
                std::fs::remove_file(&destination)?;
            }
            self.prune(&destination);
            return Ok(None);
        }
        let extension = match source.format.as_str() {
            "ttpf" => ".ttpf",
            "rpf9" => ".rpf",
            _ => return Err(Error::not_supported("Mods support TTPF pools and RPF9 archives.")),
        };
        {
            let mut original = source.open()?;
            source.fingerprint.check(source.path(), &mut original, false)?;
        }
        std::fs::create_dir_all(destination.parent().ok_or_else(|| Error::io("The copy has no folder."))?)?;
        let mut current = source.clone();
        let mut written: Option<ScratchPath> = None;
        for texture in &textures {
            cancel.check()?;
            if texture.source_sha256 != source.fingerprint.sha256 {
                return Err(Error::invalid(format!("{} changed since the replacement of {} was made.", relative, texture.entry)));
            }
            let asset = catalog.get(&format!("{}:{}", source.id, texture.entry))?;
            if asset.native_key != texture.native_key {
                return Err(Error::invalid(format!("Entry {} of {} has a different native key.", texture.entry, relative)));
            }
            let bytes = std::fs::read(join(&self.folder(), &texture.replacement)?)?;
            if sha256_hex(&bytes) != texture.replacement_sha256 {
                return Err(Error::invalid(format!("The replacement of {} changed outside ViceModVI.", texture.entry)));
            }
            // A catalog with only this archive, so the writer reads the
            // previous copy instead of the original.
            let mut single = Catalog::new();
            single.sources.push(current.clone());
            single.assets.push(asset.clone());
            if let Some(parent) = &asset.parent_id {
                single.assets.push(catalog.get(parent)?.clone());
            }
            single.validate()?;
            let next = ScratchPath::beside(&destination, extension)?;
            if source.format == "ttpf" {
                pool_writer::copy(&mut single, asset, &bytes, next.path(), false, texture.allow_resize, cancel)?;
            } else {
                rpf_copy::copy(&single, asset, &bytes, next.path(), texture.allow_resize, cancel)?;
            }
            let mut file = File::open(next.path())?;
            current.path = path_text(next.path());
            current.fingerprint = Fingerprint::create(next.path(), &mut file, None, cancel)?;
            drop(file);
            written = Some(next);
        }
        let last = written.ok_or_else(|| Error::invalid("The archive copy was not built."))?;
        files::publish(last.path(), &destination, true)?;
        manifest.files.push(ModFile {
            path: relative.to_string(),
            original_sha256: source.fingerprint.sha256.clone(),
            sha256: current.fingerprint.sha256.clone(),
        });
        manifest.files.sort_by_key(|f| f.path.to_uppercase());
        Ok(Some((destination, current.fingerprint.sha256)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_paths_must_stay_inside_the_mods_folder() {
        let base = Path::new("C:\\game");
        assert!(join(base, "data/ttdb/pool.ttpf").is_ok());
        for bad in ["", "../x", "data/../../x", "C:/x", "data\\x", "a//b", "./x"] {
            assert!(join(base, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_the_game_root_from_its_markers() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        std::fs::create_dir_all(game.join("data/ttdb")).unwrap();
        let pool = game.join("data/ttdb/pool.ttpf");
        std::fs::write(&pool, b"x").unwrap();
        assert_eq!(game_root(&pool, None).unwrap(), game.join("data/ttdb"));
        assert_eq!(game_root(&pool, Some(&game)).unwrap(), game);
        std::fs::write(game.join("MicrosoftGame.Config"), b"").unwrap();
        assert_eq!(game_root(&pool, Some(&game.join("data"))).unwrap(), game);
    }
}
