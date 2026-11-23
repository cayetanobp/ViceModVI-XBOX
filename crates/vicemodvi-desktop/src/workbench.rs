//! The ViceModVI workbench window: browse, preview, export and stage edits.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use eframe::egui::{
    self, text::LayoutJob, Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, TextFormat, TextureHandle, TextureOptions,
};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use vicemodvi_core::{
    catalog::group_digits,
    dds, files,
    project::{ReplacementProject, StagedTexture},
    texture_conversion::{self, TextureShape, IMAGE_EXTENSIONS},
    texture_links::{self, TextureIndex, TextureLink},
    xbox_assets::{self, BuildOptions, TextureExport},
    Asset, Cancel, Catalog, Error, IndexProgress, Model, Pixels, Result,
};

use crate::{
    browse::{self, export_name, Node, Row, Tree},
    exports,
    jobs::{Job, Poll},
    theme,
    viewer::{Camera, Surface, SurfaceImage, Viewer},
};

const TYPES: [(&str, Option<&str>); 5] = [
    ("All assets", None),
    ("Models", Some("model")),
    ("Textures", Some("texture")),
    ("Dictionaries", Some("dictionary")),
    ("Unknown ranges", Some("unknown")),
];
const REPOSITORY: &str = "https://github.com/cayetanobp/ViceModVI-XBOX";
/// Name hash of the white diffuse used with vertex colors.
const WHITE_DIFFUSE: &str = "0000000082079169";

/// Results of work started from the UI.
enum Done {
    Workspace(Catalog, Option<PathBuf>),
    Staged { asset: usize, path: PathBuf, dds: Vec<u8> },
    Confirmed { asset: usize },
    Status(String),
    Batch { exported: usize, total: usize, failures: Vec<String> },
}

struct Busy {
    job: Job<Result<Done>>,
    /// Opening work reports progress and can be canceled on close.
    progress: Option<Arc<Mutex<Option<(PathBuf, IndexProgress)>>>>,
}

struct LoadedModel {
    asset: usize,
    model: Arc<Model>,
    links: Vec<TextureLink>,
}

/// Decoded preview pixels and their egui texture.
struct Picture {
    pixels: Pixels,
    handle: TextureHandle,
    key: String,
}

struct TexturePreview {
    info: Option<TextureExport>,
    original: Option<Pixels>,
    replacement: Option<Pixels>,
    confirmed: bool,
    details: String,
    edit_details: String,
}

struct Preview {
    asset: usize,
    info: Option<TextureExport>,
    original: Option<Picture>,
    replacement: Option<Picture>,
    confirmed: bool,
    details: String,
    edit_details: String,
    staged: bool,
}

pub struct Workbench {
    catalog: Option<Arc<Catalog>>,
    index: Option<Arc<TextureIndex>>,
    project: ReplacementProject,
    tree: Option<Tree>,
    rows: Vec<Row>,
    shown: Vec<usize>,
    selected: HashSet<usize>,
    anchor: Option<usize>,
    scroll_to: Option<usize>,
    scope: Option<usize>,
    visible: HashSet<String>,
    scope_path: String,
    scope_tooltip: String,
    force_open: HashSet<usize>,
    type_filter: usize,
    search: String,

    selected_asset: Option<usize>,
    model: Option<LoadedModel>,
    model_job: Option<(usize, Job<Result<(Model, Vec<TextureLink>)>>)>,
    model_error: Option<String>,
    mesh: Option<usize>,
    material: Option<i32>,
    texture_choices: Vec<(Option<String>, String)>,
    texture_choice: Option<String>,
    preview_choices: HashMap<i32, Option<String>>,

    texture: Option<usize>,
    preview: Option<Preview>,
    texture_job: Option<(usize, Job<TexturePreview>)>,
    show_replacement: bool,
    reveal_replacement: bool,

    busy: Option<Busy>,
    status: String,
    close_after_opening: bool,
    closing: bool,
    pending_open: Option<PathBuf>,

    viewer: Option<Viewer>,
    camera: Camera,
    logo: Option<TextureHandle>,
    ctx: egui::Context,
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn ask(title: &str, text: &str) -> bool {
    MessageDialog::new()
        .set_title(title)
        .set_description(text)
        .set_level(MessageLevel::Info)
        .set_buttons(MessageButtons::YesNo)
        .show()
        == MessageDialogResult::Yes
}

fn inform(title: &str, text: &str) {
    MessageDialog::new().set_title(title).set_description(text).set_level(MessageLevel::Info).set_buttons(MessageButtons::Ok).show();
}

/// Truncated single-line text with an ellipsis.
fn line(text: &str, size: f32, color: Color32, width: f32, family: Option<egui::FontFamily>) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat { font_id: FontId::new(size, family.unwrap_or(egui::FontFamily::Proportional)), color, ..Default::default() },
    );
    job.wrap.max_width = width.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

impl Workbench {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let logo = image::load_from_memory(include_bytes!("../../../assets/leonida-logo-transparent.png")).ok().map(|image| {
            let rgba = image.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            cc.egui_ctx.load_texture("logo", egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), TextureOptions::LINEAR)
        });
        Self {
            catalog: None,
            index: None,
            project: ReplacementProject::new(),
            tree: None,
            rows: Vec::new(),
            shown: Vec::new(),
            selected: HashSet::new(),
            anchor: None,
            scroll_to: None,
            scope: None,
            visible: HashSet::new(),
            scope_path: String::new(),
            scope_tooltip: String::new(),
            force_open: HashSet::new(),
            type_filter: 0,
            search: String::new(),
            selected_asset: None,
            model: None,
            model_job: None,
            model_error: None,
            mesh: None,
            material: None,
            texture_choices: Vec::new(),
            texture_choice: None,
            preview_choices: HashMap::new(),
            texture: None,
            preview: None,
            texture_job: None,
            show_replacement: false,
            reveal_replacement: false,
            busy: None,
            status: "Open a folder or an Xbox Series RPF or TTPF file.".into(),
            close_after_opening: false,
            closing: false,
            pending_open: open,
            viewer: cc.wgpu_render_state.as_ref().map(Viewer::new),
            camera: Camera::default(),
            logo,
            ctx: cc.egui_ctx.clone(),
        }
    }

    // ----- Opening -----------------------------------------------------

    fn can_discard(&self) -> bool {
        self.project.replacements.is_empty() || ask("ViceModVI", "Discard staged texture edits?")
    }

    fn run(&mut self, ctx: &egui::Context, message: &str, work: impl FnOnce(&Cancel) -> Result<Done> + Send + 'static) {
        if self.busy.is_some() {
            return;
        }
        self.status = message.to_string();
        self.busy = Some(Busy { job: Job::spawn(ctx, work), progress: None });
    }

    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.busy.is_some() || !self.can_discard() {
            return;
        }
        let path = std::path::absolute(&path).unwrap_or(path);
        if vicemodvi_core::util::extension(&path) == ".json" && path.is_file() {
            self.run(ctx, "Opening…", move |_| Ok(Done::Workspace(Catalog::load(&path)?, None)));
            return;
        }
        let progress = Arc::new(Mutex::new(None::<(PathBuf, IndexProgress)>));
        let shared = progress.clone();
        let repaint = ctx.clone();
        self.status = "Opening…".into();
        let job = Job::spawn(ctx, move |cancel| {
            let folder = path.is_dir();
            if folder {
                let saved = path.join("vicemodvi-catalog.json");
                if saved.is_file() {
                    return Ok(Done::Workspace(Catalog::load_for_folder(&saved, &path, cancel)?, Some(path)));
                }
            }
            let sources = if folder { xbox_assets::source_files(&path)? } else { vec![path.clone()] };
            let report = move |source: &Path, entry: &IndexProgress| {
                if let Ok(mut slot) = shared.lock() {
                    *slot = Some((source.to_path_buf(), entry.clone()));
                }
                repaint.request_repaint();
            };
            let options = BuildOptions { inventory: None, use_cache: true, progress: Some(&report) };
            let catalog = xbox_assets::build(&sources, options, cancel)?;
            Ok(Done::Workspace(catalog, folder.then_some(path)))
        });
        self.busy = Some(Busy { job, progress: Some(progress) });
    }

    fn set_workspace(&mut self, catalog: Catalog, folder: Option<PathBuf>) {
        self.preview_choices.clear();
        self.project = ReplacementProject::new();
        self.selected_asset = None;
        self.search.clear();
        self.type_filter = 0;
        self.selected.clear();
        self.anchor = None;
        let (tree, rows) = browse::build(&catalog, folder.as_deref());
        let ranges = catalog.assets.iter().filter(|a| a.kind == "unknown").count() as i64;
        let models = catalog.assets.iter().filter(|a| a.kind == "model").count() as i64;
        let identified = catalog.assets.len() as i64 - ranges;
        self.index = Some(Arc::new(TextureIndex::new(&catalog)));
        self.catalog = Some(Arc::new(catalog));
        self.rows = rows;
        let root = tree.root.id;
        self.tree = Some(tree);
        self.set_scope(root);
        self.refilter();
        self.load_asset(None);
        self.status = format!(
            "{} models · {} identified entries and {} unknown ranges.",
            group_digits(models),
            group_digits(identified),
            group_digits(ranges)
        );
    }

    // ----- Browsing ------------------------------------------------------

    fn set_scope(&mut self, node: usize) {
        let Some(found) = self.tree.as_ref().and_then(|t| t.find(node)) else { return };
        self.visible = found.sources.clone();
        self.scope_path = found.display.clone();
        self.scope_tooltip = found.path.clone();
        self.scope = Some(node);
    }

    fn refilter(&mut self) {
        let Some(catalog) = self.catalog.clone() else { return };
        self.shown = browse::filter(&catalog, &self.rows, &self.visible, TYPES[self.type_filter].1, &self.search);
        let still: HashSet<usize> = self.shown.iter().map(|&r| self.rows[r].asset).filter(|a| self.selected.contains(a)).collect();
        self.selected = still;
        self.anchor = None;
        let primary = match self.selected_asset {
            Some(asset) if self.selected.contains(&asset) => Some(asset),
            _ => self.shown.iter().map(|&r| self.rows[r].asset).find(|a| self.selected.contains(a)),
        };
        if primary != self.selected_asset {
            self.load_asset(primary);
        }
    }

    fn click_row(&mut self, position: usize, modifiers: egui::Modifiers) {
        let asset = self.rows[self.shown[position]].asset;
        if modifiers.shift {
            let from = self.anchor.unwrap_or(position);
            let (lo, hi) = (from.min(position), from.max(position));
            if !modifiers.command {
                self.selected.clear();
            }
            for p in lo..=hi {
                self.selected.insert(self.rows[self.shown[p]].asset);
            }
        } else if modifiers.command {
            if !self.selected.remove(&asset) {
                self.selected.insert(asset);
            }
            self.anchor = Some(position);
        } else {
            self.selected.clear();
            self.selected.insert(asset);
            self.anchor = Some(position);
        }
        let primary = if self.selected.contains(&asset) {
            Some(asset)
        } else {
            self.shown.iter().map(|&r| self.rows[r].asset).find(|a| self.selected.contains(a))
        };
        if primary != self.selected_asset {
            self.load_asset(primary);
        }
    }

    fn selection(&self) -> Vec<Asset> {
        let Some(catalog) = &self.catalog else { return Vec::new() };
        self.shown.iter().map(|&r| self.rows[r].asset).filter(|a| self.selected.contains(a)).map(|a| catalog.assets[a].clone()).collect()
    }

    fn view_linked_texture(&mut self) {
        let (Some(catalog), Some(texture)) = (self.catalog.clone(), self.texture) else { return };
        let asset = &catalog.assets[texture];
        let Some(node) = self.tree.as_ref().and_then(|t| t.source_nodes.get(&asset.source_id).copied()) else { return };
        if let Some(tree) = &self.tree {
            self.force_open.extend(tree.ancestors(node));
        }
        self.set_scope(node);
        self.type_filter = 0;
        self.search.clear();
        self.selected = HashSet::from([texture]);
        self.selected_asset = Some(texture);
        self.refilter();
        self.selected = HashSet::from([texture]);
        let position = self.shown.iter().position(|&r| self.rows[r].asset == texture);
        self.anchor = position;
        self.scroll_to = position;
        self.load_asset(Some(texture));
    }

    // ----- Assets ------------------------------------------------------

    fn load_asset(&mut self, asset: Option<usize>) {
        if self.selected_asset != asset {
            self.preview_choices.clear();
        }
        self.selected_asset = asset;
        self.model = None;
        self.model_job = None;
        self.model_error = None;
        self.mesh = None;
        self.material = None;
        self.texture_choices.clear();
        self.texture_choice = None;
        self.load_texture(None);
        let (Some(asset), Some(catalog), Some(index)) = (asset, self.catalog.clone(), self.index.clone()) else { return };
        match catalog.assets[asset].kind.as_str() {
            "model" => {
                self.model_job = Some((asset, Job::spawn(&self.ctx, move |_| {
                    let entry = &catalog.assets[asset];
                    let model = xbox_assets::read_model(&catalog, entry)?;
                    let links = texture_links::resolve_model(&catalog, &index, entry, &model)?;
                    Ok((model, links))
                })));
            }
            "texture" => self.load_texture(Some(asset)),
            _ => {}
        }
    }

    fn model_loaded(&mut self, asset: usize, model: Model, links: Vec<TextureLink>) {
        let model = Arc::new(model);
        let first = model
            .meshes
            .iter()
            .position(|mesh| links.iter().any(|l| l.status == "confirmed" && Some(l.shader) == mesh.shader_index))
            .or(if model.meshes.is_empty() { None } else { Some(0) });
        self.model = Some(LoadedModel { asset, model, links });
        if let Some(mesh) = first {
            self.select_mesh(mesh);
        }
    }

    fn materials(&self) -> Vec<(i32, String)> {
        self.model
            .as_ref()
            .and_then(|m| m.model.materials.as_ref())
            .map(|materials| materials.iter().map(|m| (m.shader_index, format!("Shader {} ({:08X})", m.shader_index, m.shader_hash))).collect())
            .unwrap_or_default()
    }

    fn select_mesh(&mut self, mesh: usize) {
        let Some(loaded) = &self.model else { return };
        let shader = loaded.model.meshes[mesh].shader_index;
        self.mesh = Some(mesh);
        self.material = shader.filter(|s| self.materials().iter().any(|(m, _)| m == s));
        self.refresh_link();
        self.fit_mesh();
    }

    fn select_material(&mut self, shader: i32) {
        let Some(loaded) = &self.model else { return };
        self.material = Some(shader);
        let current = self.mesh.and_then(|m| loaded.model.meshes[m].shader_index);
        if current != Some(shader) {
            self.mesh = loaded.model.meshes.iter().position(|m| m.shader_index == Some(shader));
        }
        self.refresh_link();
        self.fit_mesh();
    }

    fn fit_mesh(&mut self) {
        if let (Some(loaded), Some(mesh)) = (&self.model, self.mesh) {
            self.camera.fit(&loaded.model.meshes[mesh]);
        }
    }

    fn current_link(&self) -> Option<&TextureLink> {
        let shader = self.material?;
        self.model.as_ref()?.links.iter().find(|l| l.shader == shader)
    }

    fn refresh_link(&mut self) {
        let Some(catalog) = self.catalog.clone() else { return };
        let link = self.current_link().cloned();
        let mut chosen = link.as_ref().and_then(|l| l.texture_id.clone());
        if let Some(manual) = self.material.and_then(|s| self.preview_choices.get(&s)) {
            chosen = manual.clone();
        }
        let mut choices: Vec<(Option<String>, String)> = link
            .as_ref()
            .map(|l| {
                l.candidates
                    .iter()
                    .filter_map(|c| catalog.get(&c.asset_id).ok().map(|a| (Some(a.id.clone()), format!("{} · {} · {}", a.name, file_name(&c.source), a.id))))
                    .collect()
            })
            .unwrap_or_default();
        if self.material.is_some_and(|shader| !self.preview_choices.contains_key(&shader)) {
            if let Some((model, shader)) = self.model.as_ref().zip(self.material) {
                let saved = material_texture_path(&catalog, &catalog.assets[model.asset], shader)
                    .ok().and_then(|path| std::fs::read_to_string(path).ok());
                if let Some(saved) = saved {
                    let matched = choices.iter().filter_map(|(id, _)| id.as_ref()).find(|id| {
                        catalog.get(id).ok().and_then(|entry| confirmed_texture_path(&catalog, entry).ok())
                            .is_some_and(|path| path.file_name().is_some_and(|name| name == saved.as_str()) && path.is_file())
                    });
                    if let Some(id) = matched {
                        chosen = Some(id.clone());
                    }
                }
            }
        }
        if !choices.is_empty() && link.as_ref().is_some_and(|l| l.texture_id.is_none()) {
            choices.insert(0, (None, "Choose a texture for preview".into()));
        }
        self.texture_choice = choices.iter().find(|(id, _)| *id == chosen).and_then(|(id, _)| id.clone());
        self.texture_choices = choices;
        let choice = self.texture_choice.clone();
        self.select_texture(choice);
    }

    fn select_texture(&mut self, id: Option<String>) {
        if let Some(shader) = self.material {
            self.preview_choices.insert(shader, id.clone());
        }
        self.texture_choice = id.clone();
        let asset = id.and_then(|id| self.catalog.as_ref().and_then(|c| c.position(&id)));
        self.load_texture(asset);
    }

    fn load_texture(&mut self, asset: Option<usize>) {
        if let Some((_, job)) = self.texture_job.take() {
            job.cancel.cancel();
        }
        self.texture = asset;
        self.preview = None;
        self.show_replacement = false;
        let (Some(asset), Some(catalog)) = (asset, self.catalog.clone()) else { return };
        let stage = self.project.find(&catalog.assets[asset]).cloned();
        let job = Job::spawn(&self.ctx, move |cancel| read_preview(&catalog, asset, stage.as_ref(), cancel));
        self.texture_job = Some((asset, job));
    }

    fn texture_loaded(&mut self, ctx: &egui::Context, asset: usize, result: TexturePreview) {
        let Some(catalog) = &self.catalog else { return };
        let entry = &catalog.assets[asset];
        let staged = self.project.find(entry).is_some();
        let picture = |pixels: Pixels, label: &str| {
            let display = fit_pixels(&pixels, ctx.input(|i| i.max_texture_side));
            let size = [display.width as usize, display.height as usize];
            let mut rgba = display.bgra32.clone();
            for p in rgba.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
            let name = format!("{}|{label}", entry.id);
            let handle = ctx.load_texture(name.clone(), egui::ColorImage::from_rgba_unmultiplied(size, &rgba), TextureOptions::LINEAR);
            let key = format!("{name}|{:?}", handle.id());
            Picture { handle, pixels, key }
        };
        let original = result.original.map(|p| picture(p, "original"));
        let replacement = result.replacement.map(|p| picture(p, "replacement"));
        let reveal = self.reveal_replacement && replacement.is_some();
        self.reveal_replacement = false;
        self.preview = Some(Preview {
            asset,
            info: result.info,
            original,
            replacement,
            confirmed: result.confirmed,
            details: result.details,
            edit_details: result.edit_details,
            staged,
        });
        if reveal {
            self.show_replacement = true;
        }
    }

    // ----- Edits and exports -------------------------------------------

    fn import_replacement(&mut self, ctx: &egui::Context) {
        let Some(texture) = self.texture else { return };
        let mut extensions = vec!["dds"];
        extensions.extend_from_slice(IMAGE_EXTENSIONS);
        let Some(path) = FileDialog::new().add_filter("Texture images", &extensions).add_filter("All files", &["*"]).pick_file() else { return };
        self.run(ctx, "Importing texture…", move |_| {
            let dds = texture_conversion::import(&path)?;
            Ok(Done::Staged { asset: texture, path, dds })
        });
    }

    fn revert_replacement(&mut self) {
        let (Some(texture), Some(catalog)) = (self.texture, self.catalog.clone()) else { return };
        if self.project.revert(&catalog.assets[texture]) {
            self.load_texture(Some(texture));
            self.status = "Pending replacement discarded.".into();
        }
    }

    fn confirm_replacement(&mut self, ctx: &egui::Context) {
        let (Some(asset), Some(catalog)) = (self.texture, self.catalog.clone()) else { return };
        let Some(stage) = self.project.find(&catalog.assets[asset]).cloned() else { return };
        let material = self.model.as_ref().zip(self.material).map(|(model, shader)| (model.asset, shader));
        self.run(ctx, "Saving texture replacement…", move |cancel| {
            let entry = &catalog.assets[asset];
            let source = catalog.source_of(entry)?;
            let mut file = source.open()?;
            source.fingerprint.check(source.path(), &mut file, false)?;
            dds::decode_texture_pixels(&stage.dds, 0)?;
            let path = confirmed_texture_path(&catalog, entry)?;
            cancel.check()?;
            write_confirmed(&path, &stage.dds)?;
            if let Some((model, shader)) = material {
                let binding = material_texture_path(&catalog, &catalog.assets[model], shader)?;
                let file_name = path.file_name().ok_or_else(|| Error::io("Replacement has no file name."))?;
                write_confirmed(&binding, file_name.to_string_lossy().as_bytes())?;
            }
            Ok(Done::Confirmed { asset })
        });
    }

    fn export_one(&mut self, ctx: &egui::Context, asset: usize, dds_only: bool) {
        let Some(catalog) = self.catalog.clone() else { return };
        let entry = catalog.assets[asset].clone();
        let mut dialog = FileDialog::new().set_file_name(export_name(&entry.name, &entry.id));
        dialog = match entry.kind.as_str() {
            "model" => dialog.add_filter("Wavefront mesh", &["obj"]),
            "texture" if dds_only => dialog.add_filter("DirectDraw surface", &["dds"]),
            "texture" => dialog.add_filter("DirectDraw surface", &["dds"]).add_filter("PNG preview", &["png"]),
            _ => dialog.add_filter("Stored bytes", &["bin"]),
        };
        let Some(path) = dialog.save_file() else { return };
        self.run(ctx, "Exporting…", move |cancel| {
            if entry.kind == "texture" && vicemodvi_core::util::extension(&path) == ".png" {
                let (bytes, _) = xbox_assets::read_texture(&catalog, &entry, cancel)?;
                exports::write_png(&dds::decode_texture_pixels(&bytes, 0)?, &path)?;
            } else {
                exports::export_native(&catalog, &entry, &path)?;
            }
            Ok(Done::Status(format!("Exported {}.", file_name(&path.to_string_lossy()))))
        });
    }

    fn export_selection(&mut self, ctx: &egui::Context) {
        let Some(catalog) = self.catalog.clone() else { return };
        let selection = self.selection();
        if selection.is_empty() || selection.iter().any(|a| !exports::can_export(a)) {
            return;
        }
        if let [single] = selection.as_slice() {
            if let Some(index) = catalog.position(&single.id) {
                self.export_one(ctx, index, false);
            }
            return;
        }
        let Some(folder) = FileDialog::new().set_title("Choose a folder for the selected exports").pick_folder() else { return };
        let total = selection.len();
        self.run(ctx, &format!("Exporting {total} assets…"), move |_| {
            let (exported, failures) = exports::export_batch(&catalog, &selection, &folder);
            Ok(Done::Batch { exported, total, failures })
        });
    }

    fn export_staged(&mut self, ctx: &egui::Context) {
        let (Some(asset), Some(catalog)) = (self.selected_asset, self.catalog.clone()) else { return };
        let Some(staged) = self.project.find(&catalog.assets[asset]).cloned() else { return };
        let stem = Path::new(&staged.original_file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(path) = FileDialog::new().add_filter("DirectDraw surface", &["dds"]).set_file_name(format!("{stem}-staged.dds")).save_file() else {
            return;
        };
        self.run(ctx, "Exporting staged DDS…", move |_| {
            files::write_bytes(&path, &staged.dds, false)?;
            Ok(Done::Status(format!("Exported {}.", file_name(&path.to_string_lossy()))))
        });
    }

    fn export_uv_layout(&mut self, ctx: &egui::Context) {
        let (Some(loaded), Some(mesh), Some(catalog)) = (&self.model, self.mesh, &self.catalog) else { return };
        let asset = &catalog.assets[loaded.asset];
        let model = loaded.model.clone();
        let Some(path) = FileDialog::new()
            .set_title("Export UV layout")
            .add_filter("PNG image", &["png"])
            .set_file_name(format!("{}_mesh{}_uv.png", export_name(&asset.name, &asset.id), mesh + 1))
            .save_file()
        else {
            return;
        };
        self.run(ctx, "Exporting UV layout…", move |_| {
            exports::write_uv_layout(&model.meshes[mesh], &path)?;
            Ok(Done::Status(format!("Exported {}.", file_name(&path.to_string_lossy()))))
        });
    }

    // ----- Frame updates -------------------------------------------------

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(busy) = &self.busy {
            if let Some(progress) = &busy.progress {
                if let Some((path, entry)) = progress.lock().ok().and_then(|p| p.clone()) {
                    if !busy.job.cancel.is_cancelled() {
                        let count = if entry.total <= 0 {
                            String::new()
                        } else {
                            format!(" · {}/{} {}", group_digits(entry.completed), group_digits(entry.total), entry.unit)
                        };
                        self.status = format!("{} · {}{count}", file_name(&path.to_string_lossy()), entry.phase);
                    }
                }
            }
            let outcome = busy.job.poll();
            let opening = busy.progress.is_some();
            match outcome {
                Poll::Pending => {}
                Poll::Failed => {
                    self.busy = None;
                    self.status = "The operation failed unexpectedly.".into();
                }
                Poll::Done(result) => {
                    self.busy = None;
                    match result {
                        Ok(done) => self.finish(done),
                        Err(Error::Cancelled) if opening => self.status = "Opening canceled.".into(),
                        Err(error) => self.status = error.to_string(),
                    }
                    if self.close_after_opening {
                        self.close_after_opening = false;
                        self.closing = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        if let Some((asset, job)) = &self.model_job {
            let asset = *asset;
            match job.poll() {
                Poll::Pending => {}
                Poll::Failed => {
                    self.model_job = None;
                    self.status = "The model reader failed unexpectedly.".into();
                }
                Poll::Done(result) => {
                    self.model_job = None;
                    if self.selected_asset == Some(asset) {
                        match result {
                            Ok((model, links)) => self.model_loaded(asset, model, links),
                            Err(error) => {
                                self.status = error.to_string();
                                self.model_error = Some(error.to_string());
                            }
                        }
                    }
                }
            }
        }
        if let Some((asset, job)) = &self.texture_job {
            let asset = *asset;
            match job.poll() {
                Poll::Pending => {}
                Poll::Failed => {
                    self.texture_job = None;
                    self.status = "The texture reader failed unexpectedly.".into();
                }
                Poll::Done(result) => {
                    self.texture_job = None;
                    if self.texture == Some(asset) {
                        self.texture_loaded(ctx, asset, result);
                    }
                }
            }
        }
    }

    fn finish(&mut self, done: Done) {
        match done {
            Done::Workspace(catalog, folder) => self.set_workspace(catalog, folder),
            Done::Status(text) => self.status = text,
            Done::Confirmed { asset } => {
                if let Some(catalog) = &self.catalog {
                    self.project.revert(&catalog.assets[asset]);
                }
                if self.texture == Some(asset) {
                    self.load_texture(Some(asset));
                }
                self.status = "Texture replacement saved.".into();
            }
            Done::Batch { exported, total, failures } => {
                self.status = format!("Exported {exported} of {total} assets.");
                if !failures.is_empty() {
                    inform("Exports needing attention", &failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
                }
            }
            Done::Staged { asset, path, dds } => {
                let Some(catalog) = self.catalog.clone() else { return };
                match self.project.stage(&catalog, &catalog.assets[asset], &path.to_string_lossy(), dds) {
                    Ok(()) => {
                        self.reveal_replacement = true;
                        self.load_texture(Some(asset));
                        self.status = "Replacement staged for this session.".into();
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.closing {
            return;
        }
        if let Some(busy) = &self.busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if busy.progress.is_some() && !self.close_after_opening {
                self.close_after_opening = true;
                self.status = "Canceling the current read…".into();
                busy.job.cancel.cancel();
            }
            return;
        }
        if !self.can_discard() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }
        if let Some((_, job)) = &self.texture_job {
            job.cancel.cancel();
        }
        self.closing = true;
    }
}

/// Downscales preview pixels to fit the GPU texture limit.
fn fit_pixels(pixels: &Pixels, max_side: usize) -> Pixels {
    let mut current = pixels.clone();
    while current.width as usize > max_side || current.height as usize > max_side {
        let (w, h) = (current.width as usize, current.height as usize);
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                let at = ((y * 2).min(h - 1) * w + (x * 2).min(w - 1)) * 4;
                out[(y * nw + x) * 4..(y * nw + x) * 4 + 4].copy_from_slice(&current.bgra32[at..at + 4]);
            }
        }
        current = Pixels { width: nw as i32, height: nh as i32, bgra32: out };
    }
    current
}

fn confirmed_texture_path(catalog: &Catalog, entry: &Asset) -> Result<PathBuf> {
    let source = catalog.source_of(entry)?;
    let directory = source.path().parent().ok_or_else(|| Error::io("Source has no parent folder."))?;
    let file_name = source.path().file_name().ok_or_else(|| Error::io("Source has no file name."))?;
    let locator = entry.id.strip_prefix(&source.id).and_then(|tail| tail.strip_prefix(':')).unwrap_or(&entry.id);
    let key = format!("{}|{}|{locator}", file_name.to_string_lossy(), source.fingerprint.sha256);
    Ok(directory.join(".vicemodvi-textures").join(format!("{}.dds", vicemodvi_core::util::sha256_hex(key.as_bytes()))))
}

fn material_texture_path(catalog: &Catalog, model: &Asset, shader: i32) -> Result<PathBuf> {
    Ok(confirmed_texture_path(catalog, model)?.with_extension(format!("{shader}.binding")))
}

fn write_confirmed(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or_else(|| Error::io("Replacement has no parent folder."))?;
    std::fs::create_dir_all(directory)?;
    files::write_bytes(path, bytes, true)
}

fn read_preview(catalog: &Catalog, asset: usize, stage: Option<&StagedTexture>, cancel: &Cancel) -> TexturePreview {
    let entry = &catalog.assets[asset];
    let mut preview = TexturePreview { info: None, original: None, replacement: None, confirmed: false, details: String::new(), edit_details: String::new() };
    let read = xbox_assets::read_texture(catalog, entry, cancel).and_then(|(bytes, export)| Ok((dds::decode_texture_pixels(&bytes, 0)?, export)));
    match read {
        Ok((pixels, export)) => {
            preview.details = format!(
                "{} × {} · DXGI {}\n{}/{} mips · starts at {}",
                export.source_width, export.source_height, export.source_format, export.exported_mips, export.source_mips, export.base_mip
            );
            preview.original = Some(pixels);
            preview.info = Some(export);
        }
        Err(error) => preview.details = error.to_string(),
    }
    if cancel.is_cancelled() {
        return preview;
    }
    let saved = confirmed_texture_path(catalog, entry)
        .and_then(|path| std::fs::read(path).or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound { Ok(Vec::new()) } else { Err(error) }
        }).map_err(Error::from));
    match saved {
        Ok(bytes) if !bytes.is_empty() => match dds::decode_texture_pixels(&bytes, 0) {
            Ok(pixels) => {
                preview.original = Some(pixels);
                preview.confirmed = true;
                preview.details.push_str("\nSaved replacement applied.");
            }
            Err(error) => preview.details.push_str(&format!("\nSaved replacement unavailable: {error}")),
        },
        Err(error) => preview.details.push_str(&format!("\nSaved replacement unavailable: {error}")),
        _ => {}
    }
    if let Some(stage) = stage {
        match dds::inspect(&stage.dds) {
            Ok(info) => {
                preview.edit_details = format!("{}\n{}", stage.original_file_name, TextureShape::from_dds(&info));
                if preview.info.as_ref().is_some_and(|original| !ReplacementProject::compare(original, &info).is_empty()) {
                    preview.edit_details.push_str("\nPreview can differ from the final material.");
                }
                match dds::decode_texture_pixels(&stage.dds, 0) {
                    Ok(pixels) => preview.replacement = Some(pixels),
                    Err(error) => preview.edit_details.push_str(&format!("\nPreview unavailable. {error}")),
                }
            }
            Err(error) => preview.edit_details.push_str(&format!("\nPreview unavailable. {error}")),
        }

    }
    preview
}

// ----- Drawing ---------------------------------------------------------------

impl eframe::App for Workbench {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(path) = self.pending_open.take() {
            self.open_path(&ctx, path);
        }
        self.poll(&ctx);
        self.handle_close(&ctx);
        let enabled = self.busy.is_none();
        self.header(ui, &ctx, enabled);
        self.status_bar(ui);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::CANVAS).inner_margin(14)).show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                self.assets_panel(ui, &ctx);
                self.inspector_panel(ui, &ctx);
                self.preview_panel(ui, frame);
            });
        });
        if self.busy.is_some() || self.model_job.is_some() || self.texture_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn card() -> egui::Frame {
    egui::Frame::new().fill(theme::PAPER).stroke(Stroke::new(1.0, theme::LINE)).inner_margin(12)
}

impl Workbench {
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, enabled: bool) {
        let frame = egui::Frame::new().fill(theme::HEADER).inner_margin(egui::Margin::symmetric(20, 14));
        let response = egui::Panel::top("header").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(42.0, 42.0)));
                    ui.add_space(4.0);
                }
                ui.label(RichText::new("ViceModVI for Xbox").size(24.0).family(theme::semibold()).color(theme::HEADER_INK));
                ui.add_space(28.0);
                ui.add_enabled_ui(enabled, |ui| {
                    theme::header_buttons(ui);
                    if ui.button("Open folder").clicked() {
                        if let Some(folder) = FileDialog::new().set_title("Open Xbox Series asset folder").pick_folder() {
                            self.open_path(ctx, folder);
                        }
                    }
                    if ui.button("Open file").clicked() {
                        if let Some(file) = FileDialog::new()
                            .add_filter("Xbox Series assets", &["rpf", "ttpf"])
                            .add_filter("RPF archives", &["rpf"])
                            .add_filter("TTPF textures", &["ttpf"])
                            .pick_file()
                        {
                            self.open_path(ctx, file);
                        }
                    }
                });
            });
        });
        let rect = response.response.rect;
        ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(2.0, theme::GOLD));
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(theme::CANVAS).inner_margin(egui::Margin::symmetric(20, 10));
        let response = egui::Panel::bottom("status").frame(frame).show(ui, |ui| {
            ui.columns(3, |columns| {
                let width = columns[0].available_width();
                columns[0].label(line(&self.status, 13.0, theme::INK, width, None)).on_hover_text(&self.status);
                columns[1].vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("©").color(theme::MUTED));
                        ui.hyperlink_to("ViceModVI-XBOX", REPOSITORY);
                    });
                });
                if let Some(progress) = self.busy.as_ref().and_then(|b| b.progress.as_ref()) {
                    let entry = progress.lock().ok().and_then(|p| p.clone());
                    columns[2].with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let bar = match entry {
                            Some((_, e)) if e.total > 0 => egui::ProgressBar::new((e.completed as f32 / e.total as f32).clamp(0.0, 1.0)),
                            _ => egui::ProgressBar::new(0.0).animate(true),
                        };
                        ui.add(bar.desired_width(160.0).desired_height(12.0));
                    });
                }
            });
        });
        let rect = response.response.rect;
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::LINE));
    }

    fn assets_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let frame = card().outer_margin(egui::Margin { right: 8, ..Default::default() });
        egui::Panel::left("assets").frame(frame).resizable(true).default_size(285.0).size_range(190.0..=600.0).show(ui, |ui| {
            ui.label(RichText::new("Assets").size(17.0).family(theme::semibold()));
            ui.add_space(4.0);
            let mut clicked_node = None;
            egui::ScrollArea::vertical().id_salt("tree").max_height(170.0).min_scrolled_height(62.0).auto_shrink([false, true]).show(ui, |ui| {
                if let Some(tree) = &self.tree {
                    draw_node(ui, &tree.root, self.scope, &mut self.force_open, &mut clicked_node);
                }
            });
            if let Some(node) = clicked_node {
                self.set_scope(node);
                self.refilter();
            }
            let width = ui.available_width();
            ui.label(line(&self.scope_path, 11.0, theme::MUTED, width, None)).on_hover_text(&self.scope_tooltip);
            let before = self.type_filter;
            egui::ComboBox::from_id_salt("type").width(width).selected_text(TYPES[self.type_filter].0).show_ui(ui, |ui| {
                for (i, (label, _)) in TYPES.iter().enumerate() {
                    ui.selectable_value(&mut self.type_filter, i, *label);
                }
            });
            ui.label(RichText::new("Name, source, hash, or offset").color(theme::MUTED));
            let search = ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(f32::INFINITY).margin(egui::vec2(7.0, 5.0)));
            if before != self.type_filter || search.changed() {
                self.refilter();
            }
            ui.add_space(4.0);
            self.asset_list(ui, ctx, search.has_focus());
        });
    }

    fn asset_list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, typing: bool) {
        let Some(catalog) = self.catalog.clone() else { return };
        let row_height = 40.0;
        let mut area = egui::ScrollArea::vertical().id_salt("asset list").auto_shrink([false, false]);
        if let Some(position) = self.scroll_to.take() {
            area = area.vertical_scroll_offset((position as f32 * row_height - row_height * 2.0).max(0.0));
        }
        let mut click = None;
        let mut menu_action = None;
        let selection_count = self.selected.len();
        let staged_single = selection_count == 1 && self.selected_asset.is_some_and(|a| self.project.find(&catalog.assets[a]).is_some());
        let all_exportable = self.selection().iter().all(exports::can_export);
        area.show_rows(ui, row_height, self.shown.len(), |ui, range| {
            for position in range {
                let row = &self.rows[self.shown[position]];
                let asset = &catalog.assets[row.asset];
                let width = ui.available_width();
                let (rect, response) = ui.allocate_exact_size(egui::vec2(width, row_height), Sense::click());
                let selected = self.selected.contains(&row.asset);
                if selected {
                    ui.painter().rect_filled(rect, 3.0, theme::SELECTION);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 3.0, Color32::from_rgb(0xEE, 0xF2, 0xFA));
                }
                let name = ui.painter().layout_job(line(&asset.name, 13.0, theme::INK, width - 8.0, None));
                ui.painter().galley(rect.min + egui::vec2(4.0, 4.0), name, theme::INK);
                let summary = ui.painter().layout_job(line(&row.summary, 11.0, theme::MUTED, width - 8.0, None));
                ui.painter().galley(rect.min + egui::vec2(4.0, 22.0), summary, theme::MUTED);
                let response = response.on_hover_text(format!("{}\n{}", asset.name, row.source_path));
                if response.clicked() {
                    click = Some((position, ui.input(|i| i.modifiers)));
                }
                if response.secondary_clicked() && !selected {
                    click = Some((position, egui::Modifiers::NONE));
                }
                response.context_menu(|ui| {
                    let label = if selection_count > 1 { format!("Export {selection_count} selected assets…") } else { "Export selection…".into() };
                    if ui.add_enabled(selection_count > 0 && all_exportable, egui::Button::new(label)).clicked() {
                        menu_action = Some(false);
                        ui.close();
                    }
                    if staged_single && ui.button("Export staged DDS…").clicked() {
                        menu_action = Some(true);
                        ui.close();
                    }
                });
            }
        });
        if !typing && !self.shown.is_empty() {
            let (down, up) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp)));
            if down || up {
                let current = self.anchor.unwrap_or(0);
                let next = if down { (current + 1).min(self.shown.len() - 1) } else { current.saturating_sub(1) };
                click = Some((next, egui::Modifiers::NONE));
                self.scroll_to = Some(next);
            }
        }
        if let Some((position, modifiers)) = click {
            self.click_row(position, modifiers);
        }
        match menu_action {
            Some(false) => self.export_selection(ctx),
            Some(true) => self.export_staged(ctx),
            None => {}
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let panel = egui::Frame::new().fill(theme::PREVIEW_PANEL).stroke(Stroke::new(1.0, theme::LINE)).inner_margin(14);
        egui::CentralPanel::default().frame(panel).show(ui, |ui| {
            let catalog = self.catalog.clone();
            let asset = self.selected_asset.and_then(|a| catalog.as_ref().map(|c| &c.assets[a]));
            ui.horizontal(|ui| {
                let heading = asset.map_or("Preview", |a| a.name.as_str());
                let width = (ui.available_width() - 40.0).max(40.0);
                ui.label(line(heading, 20.0, theme::INK, width, Some(theme::semibold())));
                let copy = ui.add_enabled(asset.is_some(), egui::Button::new("").min_size(egui::vec2(27.0, 27.0))).on_hover_text("Copy name");
                let color = if asset.is_some() { theme::INK } else { theme::MUTED };
                let r = copy.rect.shrink(7.0);
                let back = Rect::from_min_max(r.min, r.min + egui::vec2(r.width() * 0.66, r.height() * 0.66));
                let front = Rect::from_min_max(r.min + egui::vec2(r.width() * 0.34, r.height() * 0.34), r.max);
                ui.painter().rect_stroke(back, 1.0, Stroke::new(1.35, color), egui::StrokeKind::Middle);
                ui.painter().rect_filled(front, 1.0, theme::PAPER);
                ui.painter().rect_stroke(front, 1.0, Stroke::new(1.35, color), egui::StrokeKind::Middle);
                if copy.clicked() {
                    if let Some(asset) = asset {
                        ui.ctx().copy_text(asset.name.clone());
                        self.status = "Name copied to clipboard.".into();
                    }
                }
            });
            ui.add_space(10.0);
            let has_replacement = self.preview.as_ref().is_some_and(|p| p.replacement.is_some());
            let compare_height = if has_replacement { 34.0 } else { 0.0 };
            let area = Rect::from_min_max(ui.cursor().min, ui.max_rect().max - egui::vec2(0.0, compare_height));
            ui.painter().rect_filled(area, 0.0, theme::PREVIEW_BACKDROP);
            self.draw_preview(ui, frame, area);
            if has_replacement {
                let controls = Rect::from_min_max(egui::pos2(area.left(), area.bottom() + 12.0), ui.max_rect().max);
                ui.scope_builder(egui::UiBuilder::new().max_rect(controls), |ui| {
                    ui.horizontal(|ui| {
                        let baseline = if self.preview.as_ref().is_some_and(|p| p.confirmed) { "Saved" } else { "Original" };
                        ui.radio_value(&mut self.show_replacement, false, baseline);
                        ui.add_space(12.0);
                        ui.radio_value(&mut self.show_replacement, true, "Replacement");
                    });
                });
            }
        });
    }

    fn draw_preview(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, area: Rect) {
        let picture = self.preview.as_ref().and_then(|p| if self.show_replacement { p.replacement.as_ref() } else { p.original.as_ref() });
        let mesh = self.model.as_ref().zip(self.mesh);
        if let (Some((loaded, index)), Some(state), Some(viewer)) = (mesh, frame.wgpu_render_state(), self.viewer.as_mut()) {
            let mesh = &loaded.model.meshes[index];
            let white = loaded.links.iter().find(|l| Some(l.shader) == mesh.shader_index).and_then(|l| l.name_hash.as_deref()) == Some(WHITE_DIFFUSE);
            let colors = white && !self.show_replacement && !self.preview.as_ref().is_some_and(|p| p.confirmed) && mesh.vertex_colors.as_ref().is_some_and(|c| c.len() == mesh.positions.len());
            let has_uv = mesh.tex_coords.as_ref().is_some_and(|uv| uv.len() == mesh.positions.len());
            let key = format!("{}#{index}", loaded.asset);
            viewer.set_mesh(state, &key, mesh, colors);
            let image = picture.filter(|_| has_uv && !colors).map(|p| {
                let fitted = fit_pixels(&p.pixels, viewer.max_texture_side as usize);
                (p.key.clone(), fitted)
            });
            viewer.set_surface(
                state,
                image.as_ref().map(|(key, pixels)| SurfaceImage { key, width: pixels.width as u32, height: pixels.height as u32, bgra: &pixels.bgra32 }),
            );
            let surface = if colors {
                Surface::VertexColors
            } else if image.is_some() {
                Surface::Texture
            } else {
                Surface::Plain
            };
            viewer.show(ui, state, area, &mut self.camera, surface);
            return;
        }
        if let (Some(picture), None) = (picture, mesh) {
            let inner = area.shrink(12.0);
            let size = picture.handle.size_vec2();
            let scale = (inner.width() / size.x).min(inner.height() / size.y).max(0.0);
            let rect = Rect::from_center_size(inner.center(), size * scale);
            ui.painter().image(picture.handle.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            return;
        }
        let text = if self.model_job.is_some() {
            "Reading model…"
        } else if mesh.is_none() && self.model.is_some() {
            "Select a mesh"
        } else {
            "Select a model or texture"
        };
        ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(13.0), theme::INK);
    }

    fn inspector_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let frame = card().inner_margin(11).outer_margin(egui::Margin { left: 8, ..Default::default() });
        egui::Panel::right("inspector").frame(frame).resizable(true).default_size(224.0).size_range(192.0..=480.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Inspector").size(16.0).family(theme::semibold()));
                self.inspector(ui, ctx);
            });
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(catalog) = self.catalog.clone() else { return };
        let small = |text: &str| RichText::new(text).size(11.0);
        let full = ui.available_width();
        if let Some(asset) = self.selected_asset.map(|a| &catalog.assets[a]) {
            let source = catalog.source_of(asset).map(|s| s.path.clone()).unwrap_or_default();
            ui.label(small(&format!("{}\n{} · {}", file_name(&source), asset.id, asset.name_origin)).color(theme::MUTED))
                .on_hover_text(format!("{source}\n{}", asset.evidence));
        }
        if let Some(error) = &self.model_error {
            ui.label(small(error).color(theme::MUTED));
        }
        let mut action: Option<Box<dyn FnOnce(&mut Self)>> = None;
        if let Some(loaded) = &self.model {
            ui.label(small("Mesh"));
            let meshes: Vec<String> = loaded.model.meshes.iter().map(|m| m.name.clone()).collect();
            let current = self.mesh.map(|m| meshes[m].clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("mesh").width(full).selected_text(current).show_ui(ui, |ui| {
                for (i, name) in meshes.iter().enumerate() {
                    if ui.selectable_label(self.mesh == Some(i), name).clicked() {
                        action = Some(Box::new(move |s: &mut Self| s.select_mesh(i)));
                    }
                }
            });
            ui.label(small("Material"));
            let materials = self.materials();
            let current = materials.iter().find(|(s, _)| Some(*s) == self.material).map(|(_, l)| l.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("material").width(full).selected_text(current).show_ui(ui, |ui| {
                for (shader, label) in &materials {
                    if ui.selectable_label(self.material == Some(*shader), label).clicked() {
                        let shader = *shader;
                        action = Some(Box::new(move |s: &mut Self| s.select_material(shader)));
                    }
                }
            });
            match self.current_link() {
                None => {
                    ui.label(small("No recognized diffuse reference."));
                }
                Some(link) => {
                    let mut text = format!("{} · {}", link.slot, link.status);
                    if link.texture_id.is_some() {
                        text.push_str(&format!("\n{}", link.candidates.first().map(|c| file_name(&c.source)).unwrap_or_default()));
                    } else {
                        text.push_str(&format!("\n{}", link.reason));
                        for candidate in &link.candidates {
                            text.push_str(&format!("\n{} · {}", file_name(&candidate.source), candidate.asset_id));
                        }
                    }
                    let mut tip = format!("{}\n{}", link.reason, link.key.as_deref().unwrap_or("Key unavailable"));
                    for candidate in &link.candidates {
                        tip.push_str(&format!("\n{} · {}", candidate.source, candidate.asset_id));
                    }
                    ui.label(small(&text)).on_hover_text(tip);
                }
            }
            if let Some(index) = self.mesh {
                let mesh = &loaded.model.meshes[index];
                let white = loaded.links.iter().find(|l| Some(l.shader) == mesh.shader_index).and_then(|l| l.name_hash.as_deref()) == Some(WHITE_DIFFUSE);
                if white && !self.show_replacement && !self.preview.as_ref().is_some_and(|p| p.confirmed) && mesh.vertex_colors.as_ref().is_some_and(|c| c.len() == mesh.positions.len()) {
                    ui.label(small("Preview uses vertex colors with the white diffuse."));
                }
            }
            if !self.texture_choices.is_empty() {
                ui.label(small("Texture"));
                let current = self.texture_choices.iter().find(|(id, _)| *id == self.texture_choice).map(|(_, l)| l.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("texture").width(full).selected_text(line(&current, 13.0, theme::INK, full - 30.0, None)).show_ui(ui, |ui| {
                    for (id, label) in &self.texture_choices {
                        if ui.selectable_label(*id == self.texture_choice, label).clicked() {
                            let id = id.clone();
                            action = Some(Box::new(move |s: &mut Self| s.select_texture(id)));
                        }
                    }
                });
            }
            let wide = |text: &str| egui::Button::new(text).min_size(egui::vec2(full, 0.0));
            if self.texture_choice.is_some() && ui.add(wide("View linked texture")).clicked() {
                action = Some(Box::new(|s: &mut Self| s.view_linked_texture()));
            }
            if ui.add(wide("Export model as OBJ…")).clicked() {
                let asset = loaded.asset;
                let ctx = ctx.clone();
                action = Some(Box::new(move |s: &mut Self| s.export_one(&ctx, asset, false)));
            }
            let uv = self.mesh.is_some_and(|m| {
                let mesh = &loaded.model.meshes[m];
                mesh.tex_coords.as_ref().is_some_and(|uv| !uv.is_empty() && uv.len() == mesh.positions.len())
            });
            if ui.add_enabled(uv, wide("Export UV layout…")).clicked() {
                let ctx = ctx.clone();
                action = Some(Box::new(move |s: &mut Self| s.export_uv_layout(&ctx)));
            }
        }
        let wide = |text: &str| egui::Button::new(text).min_size(egui::vec2(full, 0.0));
        if let Some(preview) = &self.preview {
            if preview.info.is_some() && ui.add(wide("Export texture as DDS…")).clicked() {
                let asset = preview.asset;
                let ctx = ctx.clone();
                action = Some(Box::new(move |s: &mut Self| s.export_one(&ctx, asset, true)));
            }
            if ui.add(wide("Import replacement…")).clicked() {
                let ctx = ctx.clone();
                action = Some(Box::new(move |s: &mut Self| s.import_replacement(&ctx)));
            }
            if preview.staged {
                ui.label(small(&preview.edit_details).color(theme::MUTED));
                if ui.add_enabled(self.busy.is_none() && preview.replacement.is_some(), wide("Confirm")).clicked() {
                    let ctx = ctx.clone();
                    action = Some(Box::new(move |s: &mut Self| s.confirm_replacement(&ctx)));
                }
                if ui.add_enabled(self.busy.is_none(), wide("Revert replacement")).clicked() {
                    action = Some(Box::new(|s: &mut Self| s.revert_replacement()));
                }
            }
            ui.label(small(&preview.details));
        } else if self.texture.is_some() {
            ui.label(small("Reading texture…"));
        }
        if let Some(action) = action {
            action(self);
        }
    }
}

fn draw_node(ui: &mut egui::Ui, node: &Node, selected: Option<usize>, force_open: &mut HashSet<usize>, clicked: &mut Option<usize>) {
    let mut label = |ui: &mut egui::Ui| {
        let response = ui.selectable_label(selected == Some(node.id), &node.title).on_hover_text(&node.path);
        if response.clicked() {
            *clicked = Some(node.id);
        }
    };
    if node.children.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            label(ui);
        });
        return;
    }
    let id = ui.make_persistent_id(("source node", node.id));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, node.parent.is_none());
    if force_open.remove(&node.id) {
        state.set_open(true);
    }
    state
        .show_header(ui, |ui| label(ui))
        .body(|ui| {
            for child in &node.children {
                draw_node(ui, child, selected, force_open, clicked);
            }
        });
}
