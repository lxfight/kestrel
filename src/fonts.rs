//! Load high-quality system fonts for the UI and code editor, plus embedded VSCode Codicons.
//! CJK fallback is handled automatically by eframe's built-in `system_fonts` provider.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

const CODICON_TTF: &[u8] = include_bytes!("../assets/codicon.ttf");

const UI_FONTS: &[&str] = &[
    "segoeui.ttf",
    "seguisb.ttf",
    "tahoma.ttf",
    "SFNS.ttf",
    "Ubuntu-R.ttf",
    "NotoSans-Regular.ttf",
    "DejaVuSans.ttf",
];

const UI_BOLD_FONTS: &[&str] = &[
    "segoeuib.ttf",
    "seguisb.ttf",
    "NotoSans-Bold.ttf",
    "Ubuntu-B.ttf",
    "DejaVuSans-Bold.ttf",
];

const MONO_FONTS: &[&str] = &[
    "CascadiaMono.ttf",
    "CascadiaCode.ttf",
    "consola.ttf",
    "lucon.ttf",
    "SFNSMono.ttf",
    "Menlo.ttc",
    "JetBrainsMono-Regular.ttf",
    "DejaVuSansMono.ttf",
];

fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        dirs.push(PathBuf::from(windir).join("Fonts"));
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
    } else {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
    }
    dirs
}

fn load_first(dirs: &[PathBuf], names: &[&str]) -> Option<(String, Vec<u8>)> {
    for name in names {
        for dir in dirs {
            let path = dir.join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(((*name).to_string(), bytes));
            }
        }
    }
    None
}

/// The loaded UI and monospace font families.
pub struct FontSet {
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,
}

pub fn codicon_family() -> FontFamily {
    FontFamily::Name("codicon".into())
}

pub fn install(ctx: &egui::Context) -> FontSet {
    let dirs = system_font_dirs();
    let mut defs = FontDefinitions::default();
    let mut ui_family = FontFamily::Proportional;
    let mut ui_bold_family = FontFamily::Proportional;
    let mut mono_family = FontFamily::Monospace;

    // Install embedded VSCode Codicons in an isolated font family so its glyphs
    // never interfere with UI or monospace text layout.
    let icon_key = "codicon.ttf".to_string();
    defs.font_data.insert(
        icon_key.clone(),
        Arc::new(FontData::from_static(CODICON_TTF)),
    );
    defs.families
        .entry(codicon_family())
        .or_default()
        .push(icon_key);

    if let Some((name, bytes)) = load_first(&dirs, UI_FONTS) {
        let family = FontFamily::Name(format!("app-{name}").into());
        defs.font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        defs.families
            .entry(family.clone())
            .or_default()
            .insert(0, name.clone());
        defs.families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name);
        ui_family = family.clone();
        ui_bold_family = family;
    }

    if let Some((name, bytes)) = load_first(&dirs, UI_BOLD_FONTS) {
        let family = FontFamily::Name(format!("app-bold-{name}").into());
        defs.font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        defs.families
            .entry(family.clone())
            .or_default()
            .insert(0, name);
        ui_bold_family = family;
    }

    if let Some((name, bytes)) = load_first(&dirs, MONO_FONTS) {
        let family = FontFamily::Name(format!("app-{name}").into());
        defs.font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        defs.families
            .entry(family.clone())
            .or_default()
            .insert(0, name.clone());
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name);
        mono_family = family;
    }

    ctx.set_fonts(defs);
    FontSet {
        ui_family,
        ui_bold_family,
        mono_family,
    }
}
