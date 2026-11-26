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

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = settings::Settings::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("crash.log");
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let entry = format!(
            "=== Kestrel Panic (unix={ts}) ===\nthread: {thread_name}\nlocation: {location}\nmessage: {payload}\nbacktrace:\n{backtrace}\n\n"
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            use std::io::Write as _;
            let _ = file.write_all(entry.as_bytes());
            let _ = file.flush();
        }
        default_hook(info);
    }));
}

fn main() -> eframe::Result {
    install_panic_hook();
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
