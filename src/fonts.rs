//! Load high-quality system fonts for the UI, code editor, and integrated terminal
//! (including user-installed Nerd Fonts), plus embedded VSCode Codicons.
//! CJK fallback is handled automatically by eframe's built-in `system_fonts` provider.

use std::path::{Path, PathBuf};
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
    "seguisb.ttf",
    "segoeuib.ttf",
    "NotoSans-SemiBold.ttf",
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

const MONO_BOLD_FONTS: &[&str] = &[
    "CascadiaMono-Bold.ttf",
    "CascadiaCode-Bold.ttf",
    "consolab.ttf",
    "JetBrainsMono-Bold.ttf",
    "DejaVuSansMono-Bold.ttf",
];

fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        dirs.push(PathBuf::from(windir).join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library").join("Fonts"));
        }
        dirs.push(PathBuf::from("/Library/Fonts"));
        dirs.push(PathBuf::from("/System/Library/Fonts"));
    } else {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local/share/fonts"));
        }
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

fn normalize_font_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Find a custom font (by absolute path, exact filename, or fuzzy family stem such as
/// `"CaskaydiaCove Nerd Font"` / `"JetBrainsMono"`) together with an optional Bold match.
fn load_custom_font(
    dirs: &[PathBuf],
    spec: &str,
) -> Option<(String, Vec<u8>, Option<(String, Vec<u8>)>)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let direct = Path::new(spec);
    if direct.is_file()
        && let Ok(bytes) = std::fs::read(direct)
    {
        let name = direct
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "custom-terminal-font".to_string());
        return Some((name, bytes, None));
    }

    let needle = normalize_font_key(spec.trim_end_matches(".ttf").trim_end_matches(".otf"));
    if needle.is_empty() {
        return None;
    }

    let mut regular_best: Option<(u8, PathBuf)> = None;
    let mut bold_best: Option<(u8, PathBuf)> = None;

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("ttf") && !ext.eq_ignore_ascii_case("otf") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let norm = normalize_font_key(stem);
            if !norm.contains(&needle) {
                continue;
            }
            let is_italic = norm.contains("italic") || norm.contains("oblique");
            if is_italic {
                continue;
            }
            let is_bold = norm.contains("bold") || norm.ends_with('b');
            if is_bold {
                let score = if norm == format!("{needle}bold")
                    || norm == format!("{needle}regularbold")
                {
                    0
                } else {
                    1
                };
                if bold_best.as_ref().is_none_or(|(s, _)| score < *s) {
                    bold_best = Some((score, path));
                }
            } else {
                let score = if norm == needle {
                    0
                } else if norm.contains("regular") || norm.contains("mono") {
                    1
                } else {
                    2
                };
                if regular_best.as_ref().is_none_or(|(s, _)| score < *s) {
                    regular_best = Some((score, path));
                }
            }
        }
    }

    let (_, reg_path) = regular_best?;
    let reg_bytes = std::fs::read(&reg_path).ok()?;
    let reg_name = format!(
        "term-{}",
        reg_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "custom.ttf".to_string())
    );
    let bold = bold_best.and_then(|(_, bp)| {
        let bytes = std::fs::read(&bp).ok()?;
        let name = format!(
            "term-bold-{}",
            bp.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "custom-bold.ttf".to_string())
        );
        Some((name, bytes))
    });
    Some((reg_name, reg_bytes, bold))
}

/// Discover installed monospace & Nerd Font candidates for the terminal settings dropdown.
pub fn discover_monospace_fonts() -> Vec<String> {
    let mut found = Vec::new();
    let hints = [
        "nerd", "mono", "code", "consola", "cascadia", "fira", "hack", "iosevka", "meslo",
        "menlo", "courier", "ubuntu", "sourcecode", "victor", "sarasa", "maple",
    ];
    for dir in system_font_dirs() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("ttf") && !ext.eq_ignore_ascii_case("otf") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower = stem.to_ascii_lowercase();
            if lower.contains("italic")
                || lower.contains("bold")
                || lower.contains("light")
                || lower.contains("thin")
                || lower.contains("oblique")
            {
                continue;
            }
            if hints.iter().any(|h| lower.contains(h)) {
                found.push(stem.to_string());
            }
        }
    }
    found.sort_by_key(|s| s.to_ascii_lowercase());
    found.dedup();
    found.truncate(24);
    found
}

/// The loaded UI, editor monospace, and terminal monospace font families.
pub struct FontSet {
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,
    pub mono_bold_family: FontFamily,
    pub term_mono_family: FontFamily,
    pub term_mono_bold_family: FontFamily,
}

pub fn codicon_family() -> FontFamily {
    FontFamily::Name("codicon".into())
}

#[allow(dead_code)]
pub fn install(ctx: &egui::Context) -> FontSet {
    install_with_terminal_font(ctx, "")
}

pub fn install_with_terminal_font(ctx: &egui::Context, custom_terminal_font: &str) -> FontSet {
    let dirs = system_font_dirs();
    let mut defs = FontDefinitions::default();
    let default_prop_fallbacks = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let default_mono_fallbacks = defs
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mut ui_family = FontFamily::Proportional;
    let mut ui_bold_family = FontFamily::Proportional;
    let mut mono_family = FontFamily::Monospace;
    let mut mono_bold_family = FontFamily::Monospace;

    // Install embedded VSCode Codicons in an isolated font family.
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
        let mut chain = Vec::with_capacity(1 + default_prop_fallbacks.len());
        chain.push(name.clone());
        chain.extend(default_prop_fallbacks.iter().cloned());
        defs.families.insert(family.clone(), chain);
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
            .entry(name.clone())
            .or_insert_with(|| Arc::new(FontData::from_owned(bytes)));
        let mut chain = Vec::with_capacity(1 + default_prop_fallbacks.len());
        chain.push(name);
        chain.extend(default_prop_fallbacks.iter().cloned());
        defs.families.insert(family.clone(), chain);
        ui_bold_family = family;
    }

    let mut base_mono_chain = default_mono_fallbacks.clone();
    if let Some((name, bytes)) = load_first(&dirs, MONO_FONTS) {
        let family = FontFamily::Name(format!("app-{name}").into());
        defs.font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        let mut chain = Vec::with_capacity(1 + default_mono_fallbacks.len());
        chain.push(name.clone());
        chain.extend(default_mono_fallbacks.iter().cloned());
        base_mono_chain = chain.clone();
        defs.families.insert(family.clone(), chain);
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name);
        mono_family = family.clone();
        mono_bold_family = family;
    }

    if let Some((name, bytes)) = load_first(&dirs, MONO_BOLD_FONTS) {
        let family = FontFamily::Name(format!("app-mono-bold-{name}").into());
        defs.font_data
            .entry(name.clone())
            .or_insert_with(|| Arc::new(FontData::from_owned(bytes)));
        let mut chain = Vec::with_capacity(1 + default_mono_fallbacks.len());
        chain.push(name);
        chain.extend(default_mono_fallbacks.iter().cloned());
        defs.families.insert(family.clone(), chain);
        mono_bold_family = family;
    }

    let mut term_mono_family = mono_family.clone();
    let mut term_mono_bold_family = mono_bold_family.clone();

    if let Some((reg_name, reg_bytes, bold_opt)) = load_custom_font(&dirs, custom_terminal_font) {
        let family = FontFamily::Name(format!("term-mono-{reg_name}").into());
        defs.font_data
            .insert(reg_name.clone(), Arc::new(FontData::from_owned(reg_bytes)));
        let mut chain = Vec::with_capacity(1 + base_mono_chain.len());
        chain.push(reg_name);
        chain.extend(base_mono_chain.iter().cloned());
        defs.families.insert(family.clone(), chain);
        term_mono_family = family.clone();
        term_mono_bold_family = family;

        if let Some((bold_name, bold_bytes)) = bold_opt {
            let bold_family = FontFamily::Name(format!("term-mono-bold-{bold_name}").into());
            defs.font_data
                .insert(bold_name.clone(), Arc::new(FontData::from_owned(bold_bytes)));
            let mut bchain = Vec::with_capacity(1 + base_mono_chain.len());
            bchain.push(bold_name);
            bchain.extend(base_mono_chain.iter().cloned());
            defs.families.insert(bold_family.clone(), bchain);
            term_mono_bold_family = bold_family;
        }
    }

    ctx.set_fonts(defs);
    FontSet {
        ui_family,
        ui_bold_family,
        mono_family,
        mono_bold_family,
        term_mono_family,
        term_mono_bold_family,
    }
}
