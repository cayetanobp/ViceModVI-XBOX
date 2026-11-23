//! End-to-end checks on synthetic RPF9 and TTPF files: catalog, export,
//! conversion and the experimental copy writers.

mod support;

use std::path::Path;

use support::{archive, pool};
use vicemodvi_core::{
    dds, pool_writer, rpf_copy,
    texture_conversion::{self, TextureShape},
    xbox_assets::{self, BuildOptions, ExportResult},
    Cancel, Catalog, Error,
};

fn build(paths: &[&Path]) -> Catalog {
    let files: Vec<_> = paths.iter().map(|p| p.to_path_buf()).collect();
    xbox_assets::build(&files, BuildOptions { use_cache: false, ..Default::default() }, &Cancel::new()).unwrap()
}

#[test]
fn ttpf_catalog_export_and_pool_copy() {
    let dir = tempfile::tempdir().unwrap();
    let pool_path = dir.path().join("pool.ttpf");
    std::fs::write(&pool_path, pool(&[(256, 256, 71, 9, 0x1111), (64, 64, 28, 7, 0x2222)])).unwrap();
    let mut catalog = build(&[&pool_path]);
    assert_eq!(catalog.assets.len(), 2);
    assert_eq!(catalog.assets[0].native_key.as_deref(), Some("0000000000001111"));

    let catalog_path = dir.path().join("catalog.json");
    catalog.save(&catalog_path, false).unwrap();
    let mut catalog = Catalog::load(&catalog_path).unwrap();
    let texture = catalog.assets[0].clone();

    let (original, info) = xbox_assets::read_texture(&catalog, &texture, &Cancel::none()).unwrap();
    assert_eq!((info.width, info.height, info.exported_mips, info.coverage.as_str()), (256, 256, 9, "complete mip chain"));
    let exported = dir.path().join("t.dds");
    assert!(matches!(xbox_assets::export(&catalog, &texture, &exported).unwrap(), ExportResult::Texture(_)));
    assert!(xbox_assets::export(&catalog, &texture, &exported).is_err(), "exports never overwrite");

    // A PNG replacement needs conversion to BC1 with a regenerated mip chain.
    let png = dir.path().join("new.png");
    image::RgbaImage::from_fn(128, 128, |x, y| image::Rgba([x as u8, y as u8, 99, 255])).save(&png).unwrap();
    let replacement = texture_conversion::import(&png).unwrap();
    let plan = pool_writer::plan(&catalog, &texture, &replacement, &Cancel::none()).unwrap();
    assert!(plan.requires_resize && plan.limitation.is_none());
    assert_eq!(plan.output, TextureShape::new(256, 256, 71, 9));

    let output = dir.path().join("copy.ttpf");
    let error = pool_writer::copy(&mut catalog, &texture, &replacement, &output, false, false, &Cancel::none()).unwrap_err();
    assert!(error.to_string().contains("--resize"), "{error}");
    let result = pool_writer::copy(&mut catalog, &texture, &replacement, &output, false, true, &Cancel::none()).unwrap();
    assert!(result.content_changed && !result.replaced_source);
    assert_eq!(result.neighbor, 1);

    // The copy carries the converted texture and the untouched neighbor.
    let copy_catalog = build(&[&output]);
    let (copied, _) = xbox_assets::read_texture(&copy_catalog, &copy_catalog.assets[0], &Cancel::none()).unwrap();
    let expected = texture_conversion::apply(&replacement, &plan, true).unwrap();
    assert_eq!(copied[148..], expected[148..]);
    let (neighbor, _) = xbox_assets::read_texture(&copy_catalog, &copy_catalog.assets[1], &Cancel::none()).unwrap();
    let (neighbor_before, _) = xbox_assets::read_texture(&catalog, &catalog.assets[1], &Cancel::none()).unwrap();
    assert_eq!(neighbor, neighbor_before);
    assert_ne!(copied, original);

    // Writing over an existing destination needs explicit consent.
    assert!(pool_writer::copy(&mut catalog, &texture, &replacement, &output, false, true, &Cancel::none()).is_err());
    pool_writer::copy(&mut catalog, &texture, &replacement, &output, true, true, &Cancel::none()).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with('.')).collect();
    assert!(leftovers.is_empty(), "temporary files remain: {leftovers:?}");
}

#[test]
fn rpf_catalog_and_identical_copy() {
    let dir = tempfile::tempdir().unwrap();
    let rpf = dir.path().join("archive.rpf");
    std::fs::write(&rpf, archive()).unwrap();
    let catalog = build(&[&rpf]);
    let kinds: Vec<_> = catalog.assets.iter().map(|a| a.kind.as_str()).collect();
    assert_eq!(kinds, ["dictionary", "texture", "texture"], "{:#?}", catalog.assets);
    let texture = catalog.assets[1].clone();
    assert_eq!(texture.native_key.as_deref(), Some("AAAA000000000001"));
    assert_eq!(texture.name_hash.as_deref(), Some("0000000010000000"));

    let (exported, info) = xbox_assets::read_texture(&catalog, &texture, &Cancel::none()).unwrap();
    assert_eq!((info.width, info.height, info.exported_mips), (64, 64, 7));

    // Writing the untouched texture reproduces the archive exactly.
    let same = dir.path().join("same.rpf");
    let result = rpf_copy::copy(&catalog, &texture, &exported, &same, false, &Cancel::none()).unwrap();
    assert_eq!(result.rewritten_segments, 0);
    assert_eq!(result.copy_sha256, catalog.sources[0].fingerprint.sha256);
    assert_eq!(std::fs::read(&same).unwrap(), std::fs::read(&rpf).unwrap());

    // An edit either fits the original packed size or is refused.
    let mut edited = exported.clone();
    for (i, byte) in edited[148..].iter_mut().enumerate() {
        *byte ^= (i % 3) as u8;
    }
    let changed = dir.path().join("changed.rpf");
    match rpf_copy::copy(&catalog, &texture, &edited, &changed, false, &Cancel::none()) {
        Ok(result) => {
            assert_eq!(result.rewritten_segments, 1);
            let copy_catalog = build(&[&changed]);
            let (actual, _) = xbox_assets::read_texture(&copy_catalog, &copy_catalog.assets[1], &Cancel::none()).unwrap();
            assert_eq!(actual, edited);
        }
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("exactly"), "{message}");
            assert!(!changed.exists());
        }
        Err(error) => panic!("{error}"),
    }
    dds::inspect(&exported).unwrap();
}

#[test]
fn cancellation_stops_catalog_build() {
    let dir = tempfile::tempdir().unwrap();
    let pool_path = dir.path().join("pool.ttpf");
    std::fs::write(&pool_path, pool(&[(64, 64, 28, 7, 1)])).unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let result = xbox_assets::build(&[pool_path], BuildOptions::default(), &cancel);
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn rpf_model_decode_and_obj_export() {
    let dir = tempfile::tempdir().unwrap();
    let rpf = dir.path().join("props.rpf");
    std::fs::write(&rpf, support::model_archive()).unwrap();
    let pool_path = dir.path().join("pool.ttpf");
    std::fs::write(&pool_path, pool(&[(256, 256, 71, 9, 0x1111), (64, 64, 28, 7, 0x2222)])).unwrap();
    let catalog = build(&[&rpf, &pool_path]);
    let models: Vec<_> = catalog.assets.iter().filter(|a| a.kind == "model").collect();
    assert_eq!(models.len(), 1, "{:#?}", catalog.assets);
    let asset = models[0];
    assert_eq!((asset.name.as_str(), asset.name_origin.as_str()), ("test_prop", "resource"));
    let model = xbox_assets::read_model(&catalog, asset).unwrap();
    let mesh = &model.meshes[0];
    assert_eq!(mesh.positions, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    assert_eq!(mesh.tex_coords.as_deref(), Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]][..]));
    assert_eq!(mesh.vertex_colors.as_deref(), Some(&[0xFF00_00FF, 0xFF00_FF00, 0xFFFF_0000][..]));
    assert_eq!(mesh.shader_index, Some(0));
    let materials = model.materials.as_ref().unwrap();
    assert_eq!((materials.len(), materials[0].shader_hash, materials[0].textures[0].opaque_id64), (1, 0x1234_5678, 0x1111));
    let index = vicemodvi_core::texture_links::TextureIndex::new(&catalog);
    let links = vicemodvi_core::texture_links::resolve_model(&catalog, &index, asset, &model).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!((links[0].slot.as_str(), links[0].status.as_str()), ("diffusetex", "confirmed"));
    assert_eq!(links[0].texture_id.as_deref(), Some("1:ttpf:0"));
    assert_eq!(links[0].name_hash.as_deref(), Some("000000000BADF00D"));
    let obj = dir.path().join("prop.obj");
    match xbox_assets::export(&catalog, asset, &obj).unwrap() {
        ExportResult::Mesh(export) => assert_eq!((export.vertices, export.faces, export.decoder.as_str()), (3, 1, "RageModelDecoder")),
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&obj).unwrap();
    assert!(text.contains("f 1/1 2/2 3/3"), "{text}");
    let parsed = vicemodvi_core::obj_reader::parse(&text, "x").unwrap();
    assert_eq!(parsed.meshes[0].positions, mesh.positions);
}
