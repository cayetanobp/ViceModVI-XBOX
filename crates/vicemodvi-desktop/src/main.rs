//! ViceModVI desktop viewer for Xbox Series assets.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browse;
mod exports;
mod jobs;
mod theme;
mod viewer;
mod workbench;

use eframe::egui;

fn icon() -> Option<egui::IconData> {
    let image = image::load_from_memory_with_format(include_bytes!("../../../assets/vicemodvi.ico"), image::ImageFormat::Ico).ok()?;
    let rgba = image.to_rgba8();
    Some(egui::IconData { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let open = (args.len() == 1).then(|| std::path::PathBuf::from(&args[0]));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ViceModVI for Xbox")
        .with_inner_size([1280.0, 830.0])
        .with_min_inner_size([850.0, 580.0]);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Wgpu, ..Default::default() };
    eframe::run_native("ViceModVI for Xbox", options, Box::new(move |cc| Ok(Box::new(workbench::Workbench::new(cc, open)))))
}
