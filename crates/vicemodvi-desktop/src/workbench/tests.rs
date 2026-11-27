use super::*;
use vicemodvi_core::{native::slots::joaat, Fingerprint, Material, Mesh, Source, TextureRef};

fn texture_reference(parameter: &str, key: u64) -> TextureRef {
    TextureRef {
        parameter_hash: joaat(parameter),
        texture_index: (key - 1) as i32,
        system_offset: 0,
        asset_descriptor_offset: 0,
        opaque_id64: key,
        opaque_id32: 0,
        tag: 6,
    }
}

fn decoded_view(ctx: &egui::Context, index: usize, asset: &Asset) -> TextureView {
    let result = TexturePreview {
        info: None,
        original: Some(Pixels { width: 1, height: 1, bgra32: vec![15, 20, 25, 0] }),
        replacement: None,
        confirmed: false,
        details: String::new(),
        edit_details: String::new(),
    };
    TextureView::Ready(Box::new(Workbench::build_preview(ctx, index, asset, result, false, false)))
}

#[test]
fn selecting_a_technical_map_applies_it_to_the_mesh() {
    const DIFFUSE: usize = 1;
    const NORMAL: usize = 2;
    let ctx = egui::Context::default();
    let mut app = Workbench::new(&eframe::CreationContext::_new_kittest(ctx.clone()), None);
    let entry = Asset::new("overlay".into(), "0", "model", "Overlay".into(), "synthetic", "test");
    let mut diffuse = Asset::new("diffuse".into(), "0", "texture", "Diffuse".into(), "synthetic", "test");
    let mut normal = Asset::new("normal".into(), "0", "texture", "Normal".into(), "synthetic", "test");
    diffuse.native_key = xbox_assets::key(Some(1));
    normal.native_key = xbox_assets::key(Some(2));
    let mut catalog = Catalog::new();
    catalog.sources.push(Source {
        id: "0".into(),
        path: "synthetic.rpf".into(),
        format: "rpf9".into(),
        fingerprint: Fingerprint {
            length: 0,
            sha256: String::new(),
            samples_sha256: String::new(),
            last_write_utc: Default::default(),
        },
    });
    catalog.assets.extend([entry, diffuse.clone(), normal.clone()]);

    let mesh = Mesh {
        name: "Triangle".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        indices: vec![0, 1, 2],
        tex_coords: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
        vertex_colors: None,
        shader_index: Some(0),
    };
    let mut model = Model::new("Overlay".into(), vec![mesh], [0.0; 3], [1.0, 1.0, 0.0]);
    model.materials = Some(vec![Material {
        shader_index: 0,
        shader_hash: 0,
        preset_hash: 0,
        textures: vec![texture_reference("diffusetex", 1), texture_reference("bumptex", 2)],
        render_bucket: None,
        render_bucket_mask: None,
    }]);
    let maps = texture_links::resolve_material_maps(&catalog, &TextureIndex::new(&catalog), &catalog.assets[0], &model).unwrap();
    app.catalog = Some(Arc::new(catalog));
    app.texture_views.insert(DIFFUSE, decoded_view(&ctx, DIFFUSE, &diffuse));
    app.model_loaded(0, model, maps);
    assert_eq!(app.mesh_surface(), (Surface::Texture, Some(DIFFUSE)));

    app.texture_views.insert(NORMAL, decoded_view(&ctx, NORMAL, &normal));
    app.select_map(Some(MapChoice { reference: 1, candidate: Some(0) }));
    assert_eq!(app.inspected_preview().map(|preview| preview.asset), Some(NORMAL));
    assert_eq!(app.mesh_surface(), (Surface::DebugTexture, Some(NORMAL)));

    app.select_map(None);
    assert_eq!(app.inspected_preview().map(|preview| preview.asset), Some(DIFFUSE));
    assert_eq!(app.mesh_surface(), (Surface::Texture, Some(DIFFUSE)));
}
