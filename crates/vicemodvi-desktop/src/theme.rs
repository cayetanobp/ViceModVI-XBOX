//! Logo palette and fonts, matching the WPF resources of the C# viewer.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub const INK: Color32 = Color32::from_rgb(0x17, 0x26, 0x45);
pub const MUTED: Color32 = Color32::from_rgb(0x4C, 0x56, 0x71);
pub const CANVAS: Color32 = Color32::from_rgb(0xF4, 0xEF, 0xE7);
pub const PAPER: Color32 = Color32::from_rgb(0xFF, 0xFC, 0xF6);
pub const LINE: Color32 = Color32::from_rgb(0xC8, 0xCE, 0xDC);
pub const HEADER: Color32 = Color32::from_rgb(0x16, 0x3E, 0x78);
pub const HEADER_INK: Color32 = Color32::from_rgb(0xFF, 0xF7, 0xE6);
pub const GOLD: Color32 = Color32::from_rgb(0xF5, 0xBD, 0x48);
pub const LINK: Color32 = Color32::from_rgb(0x21, 0x58, 0xA2);
pub const PREVIEW_PANEL: Color32 = Color32::from_rgb(0xE5, 0xEA, 0xF5);
pub const PREVIEW_BACKDROP: Color32 = Color32::from_rgb(0xD2, 0xDD, 0xF0);
pub const BUTTON_OUTLINE: Color32 = Color32::from_rgb(0x53, 0x74, 0xA4);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(0xF9, 0xDE, 0xF0);
pub const BUTTON_FOCUS: Color32 = Color32::from_rgb(0x9B, 0x29, 0x6F);
pub const SELECTION: Color32 = Color32::from_rgb(0xD6, 0xE2, 0xF6);

pub const SEMIBOLD: &str = "semibold";

pub fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

/// Uses Segoe UI from the Windows font folder when it is present. The font
/// is read at run time and never bundled.
fn load_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let folder = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"));
    let read = |name: &str| folder.as_ref().and_then(|f| std::fs::read(f.join(name)).ok());
    let mut bold_family = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(regular) = read("segoeui.ttf") {
        fonts.font_data.insert("Segoe UI".into(), FontData::from_owned(regular).into());
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "Segoe UI".into());
        }
        bold_family.insert(0, "Segoe UI".into());
    }
    if let Some(bold) = read("seguisb.ttf") {
        fonts.font_data.insert("Segoe UI Semibold".into(), FontData::from_owned(bold).into());
        bold_family.insert(0, "Segoe UI Semibold".into());
    }
    fonts.families.insert(semibold(), bold_family);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    load_fonts(ctx);
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(13.0, semibold())),
            (TextStyle::Heading, FontId::new(17.0, semibold())),
        ]
        .into();
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.item_spacing = egui::vec2(7.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        let mut visuals = Visuals::light();
        visuals.panel_fill = CANVAS;
        visuals.window_fill = PAPER;
        visuals.extreme_bg_color = Color32::WHITE;
        visuals.faint_bg_color = PAPER;
        visuals.hyperlink_color = LINK;
        visuals.selection.bg_fill = SELECTION;
        visuals.selection.stroke = Stroke::new(1.0, INK);
        visuals.override_text_color = None;
        let radius = CornerRadius::same(5);
        let w = &mut visuals.widgets;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, INK);
        w.noninteractive.corner_radius = radius;
        w.inactive.bg_fill = PAPER;
        w.inactive.weak_bg_fill = PAPER;
        w.inactive.bg_stroke = Stroke::new(1.0, BUTTON_OUTLINE);
        w.inactive.fg_stroke = Stroke::new(1.0, INK);
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = BUTTON_HOVER;
        w.hovered.weak_bg_fill = BUTTON_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, BUTTON_FOCUS);
        w.hovered.fg_stroke = Stroke::new(1.0, INK);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = GOLD;
        w.active.weak_bg_fill = GOLD;
        w.active.bg_stroke = Stroke::new(2.0, BUTTON_FOCUS);
        w.active.fg_stroke = Stroke::new(1.0, INK);
        w.active.corner_radius = radius;
        w.open.bg_fill = PAPER;
        w.open.weak_bg_fill = PAPER;
        w.open.bg_stroke = Stroke::new(1.0, BUTTON_FOCUS);
        w.open.fg_stroke = Stroke::new(1.0, INK);
        w.open.corner_radius = radius;
        style.visuals = visuals;
    });
}

/// Gold buttons for the header bar.
pub fn header_buttons(ui: &mut egui::Ui) {
    let inactive = &mut ui.visuals_mut().widgets.inactive;
    inactive.weak_bg_fill = GOLD;
    inactive.bg_fill = GOLD;
    inactive.bg_stroke = Stroke::new(1.0, GOLD);
}
