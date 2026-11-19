#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod devshot;
mod diff;
mod doc;
mod editor;
mod fonts;
mod fuzzy;
mod git;
mod highlight;
mod icons;
mod lsp;
mod markdown;
mod plugin;
mod settings;
mod terminal;
mod theme;
mod ui;
mod viewer;
mod workspace;

use app::KestrelApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Kestrel")
            .with_decorations(false)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Kestrel",
        options,
        Box::new(|cc| Ok(Box::new(KestrelApp::new(cc)))),
    )
}
