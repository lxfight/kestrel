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

/// 开发态窗口尺寸覆盖：KESTREL_WINDOW=760x480。
fn window_size() -> [f32; 2] {
    let Some(spec) = std::env::var("KESTREL_WINDOW").ok() else {
        return [1440.0, 900.0];
    };
    match spec.split_once('x') {
        Some((w, h)) => match (w.trim().parse(), h.trim().parse()) {
            (Ok(w), Ok(h)) => [w, h],
            _ => [1440.0, 900.0],
        },
        None => [1440.0, 900.0],
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Kestrel")
            .with_decorations(false)
            .with_inner_size(window_size())
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Kestrel",
        options,
        Box::new(|cc| Ok(Box::new(KestrelApp::new(cc)))),
    )
}
