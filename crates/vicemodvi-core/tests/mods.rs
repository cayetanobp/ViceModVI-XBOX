//! The game's mods folder on a synthetic installation: replacements stack in
//! one archive copy, removing one keeps the others, a failed install leaves
//! nothing behind, and the original game files are never written.

mod support;

use std::path::{Path, PathBuf};

use support::{archive, pool};
use vicemodvi_core::{
    mods::{self, Mods},
    texture_conversion,
    xbox_assets::{self, BuildOptions},
    Asset, Cancel, Catalog,
};

fn build(paths: &[PathBuf]) -> Catalog {
    xbox_assets::build(paths, BuildOptions { use_cache: false, ..Default::default() }, &Cancel::new()).unwrap()
}

fn texture<'a>(catalog: &'a Catalog, file: &str, kind_index: usize) -> &'a Asset {
    let source = catalog.sources.iter().find(|s| s.path.ends_with(file)).unwrap();
    catalog.assets.iter().filter(|a| a.source_id == source.id && a.kind == "texture").nth(kind_index).unwrap()
}

fn read(catalog: &Catalog, asset: &Asset) -> Vec<u8> {
    xbox_assets::read_texture(catalog, asset, &Cancel::none()).unwrap().0
}

fn edited(original: &[u8], seed: usize) -> Vec<u8> {
    let mut bytes = original.to_vec();
    for (i, byte) in bytes[148..].iter_mut().enumerate() {
        *byte = ((i + seed) % 7) as u8;
    }
    bytes
}

/// An installation with the game's manifest, a pool and an archive.
fn installation(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let game = root.join("game");
    std::fs::create_dir_all(game.join("data/ttdb")).unwrap();
    std::fs::write(game.join("MicrosoftGame.Config"), b"<Game/>").unwrap();
    let pool_path = game.join("data/ttdb/pool.ttpf");
    std::fs::write(&pool_path, pool(&[(256, 256, 71, 9, 0x1111), (64, 64, 28, 7, 0x2222)])).unwrap();
    let rpf = game.join("archive.rpf");
    std::fs::write(&rpf, archive()).unwrap();
    (game, pool_path, rpf)
}

#[test]
fn replacements_stack_in_the_mods_folder_and_leave_the_game_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (game, pool_path, rpf) = installation(dir.path());
    let (pool_before, rpf_before) = (std::fs::read(&pool_path).unwrap(), std::fs::read(&rpf).unwrap());
    let catalog = build(&[pool_path.clone(), rpf.clone()]);
    let (first, second) = (texture(&catalog, "pool.ttpf", 0), texture(&catalog, "pool.ttpf", 1));
    let (first_original, second_original) = (read(&catalog, first), read(&catalog, second));
    let mods = Mods::for_source(catalog.source_of(first).unwrap(), None).unwrap();
    assert_eq!(mods.game(), game.as_path());
    let none = Cancel::none();

    // A replacement the writer refuses leaves no mods folder behind.
    let png = dir.path().join("small.png");
    image::RgbaImage::from_fn(128, 128, |x, y| image::Rgba([x as u8, y as u8, 9, 255])).save(&png).unwrap();
    let small = texture_conversion::import(&png).unwrap();
    assert!(mods.install(&catalog, first, &small, false, &none).is_err());
    assert!(!game.join(mods::FOLDER).exists());

    // Two replacements in one pool end up in one copy with the game's layout.
    let (first_edit, second_edit) = (edited(&first_original, 0), edited(&second_original, 3));
    let installed = mods.install(&catalog, first, &first_edit, false, &none).unwrap();
    let copy = game.join("mods/files/data/ttdb/pool.ttpf");
    assert_eq!(PathBuf::from(&installed.copy), copy);
    mods.install(&catalog, second, &second_edit, false, &none).unwrap();
    let modded = build(std::slice::from_ref(&copy));
    assert_eq!(read(&modded, &modded.assets[0])[148..], first_edit[148..]);
    assert_eq!(read(&modded, &modded.assets[1])[148..], second_edit[148..]);
    assert_eq!(mods.saved(&catalog, first).unwrap(), Some(first_edit.clone()));
    let manifest = mods.manifest().unwrap();
    assert_eq!((manifest.textures.len(), manifest.files.len()), (2, 1));
    assert_eq!(manifest.files[0].path, "data/ttdb/pool.ttpf");

    // Folder scans skip the copies, and copies cannot be modded themselves.
    assert_eq!(xbox_assets::source_files(&game).unwrap(), vec![rpf.clone(), pool_path.clone()]);
    let copy_mods = Mods::for_source(&modded.sources[0], None).unwrap();
    assert!(copy_mods.install(&modded, &modded.assets[0], &first_edit, false, &none).is_err());

    // An archive texture: the unchanged image reproduces the original bytes.
    let rpf_texture = texture(&catalog, "archive.rpf", 0);
    mods.install(&catalog, rpf_texture, &read(&catalog, rpf_texture), false, &none).unwrap();
    assert_eq!(std::fs::read(game.join("mods/files/archive.rpf")).unwrap(), rpf_before);
    assert!(mods.uninstall(&catalog, rpf_texture, &none).unwrap());
    assert!(!game.join("mods/files/archive.rpf").exists());

    // Removing one replacement rebuilds the copy with the other one only.
    assert!(mods.uninstall(&catalog, first, &none).unwrap());
    assert!(!mods.uninstall(&catalog, first, &none).unwrap());
    let modded = build(std::slice::from_ref(&copy));
    assert_eq!(read(&modded, &modded.assets[0]), first_original);
    assert_eq!(read(&modded, &modded.assets[1])[148..], second_edit[148..]);
    assert_eq!(mods.saved(&catalog, first).unwrap(), None);

    // Removing the last one removes the whole mods folder.
    assert!(mods.uninstall(&catalog, second, &none).unwrap());
    assert!(!game.join(mods::FOLDER).exists());
    assert_eq!(std::fs::read(&pool_path).unwrap(), pool_before);
    assert_eq!(std::fs::read(&rpf).unwrap(), rpf_before);
    let leftovers: Vec<_> = walk(&game).into_iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))).collect();
    assert!(leftovers.is_empty(), "temporary files remain: {leftovers:?}");
}

fn walk(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        }
        found.push(path);
    }
    found
}
