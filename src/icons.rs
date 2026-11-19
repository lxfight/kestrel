//! Professional icon system powered by embedded VSCode Codicons and Material/Seti file badges.

use std::path::Path;

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

use crate::fonts::codicon_family;

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Icon {
    // Activity Bar & Panels
    Explorer,
    Search,
    SourceControl,
    Outline,
    Settings,
    Extensions,
    Terminal,
    Problems,
    Output,
    Keyboard,
    // Navigation & Chrome
    ChevronRight,
    ChevronDown,
    ChevronUp,
    ChevronLeft,
    Close,
    Plus,
    NewFile,
    NewFolder,
    Folder,
    FolderOpen,
    File,
    FileCode,
    Refresh,
    CollapseAll,
    ExpandAll,
    More,
    Filter,
    ClearAll,
    Sun,
    Moon,
    PanelLeft,
    PanelBottom,
    SplitHorizontal,
    SplitVertical,
    Preview,
    Pin,
    Pinned,
    WordWrap,
    // Find & Search toggles
    CaseSensitive,
    WholeWord,
    Regex,
    Replace,
    ReplaceAll,
    ArrowUp,
    ArrowDown,
    // Status & Diagnostics
    Error,
    Warning,
    Info,
    Check,
    PassFilled,
    Lightbulb,
    // Actions & Git
    Trash,
    Goforward,
    Copy,
    Save,
    SaveAll,
    Undo,
    Redo,
    GitBranch,
    GitCommit,
    GitCompare,
    DiffAdded,
    DiffModified,
    DiffRemoved,
    Discard,
    Sync,
    Stage,
    Unstage,
    // Symbols (Outline / LSP / Completion)
    SymbolFunction,
    SymbolMethod,
    SymbolStruct,
    SymbolClass,
    SymbolInterface,
    SymbolEnum,
    SymbolConstant,
    SymbolVariable,
    SymbolField,
    SymbolProperty,
    SymbolModule,
    SymbolKeyword,
    SymbolSnippet,
}

impl Icon {
    /// Returns the VSCode Codicon Unicode character for this icon, if mapped.
    pub const fn codicon_char(self) -> Option<char> {
        Some(match self {
            Icon::Explorer => '\u{eaf0}',        // files
            Icon::Search => '\u{ea6d}',          // search
            Icon::SourceControl => '\u{ea68}',   // source-control / git-branch
            Icon::Outline => '\u{eb86}',         // list-tree
            Icon::Settings => '\u{eb51}',        // settings-gear
            Icon::Extensions => '\u{eae6}',      // extensions
            Icon::Terminal => '\u{ea85}',        // terminal
            Icon::Problems => '\u{ea6c}',        // warning
            Icon::Output => '\u{eb9d}',          // output
            Icon::Keyboard => '\u{ea65}',        // record-keys
            Icon::ChevronRight => '\u{eab6}',    // chevron-right
            Icon::ChevronDown => '\u{eab4}',     // chevron-down
            Icon::ChevronUp => '\u{eab7}',       // chevron-up
            Icon::ChevronLeft => '\u{eab5}',     // chevron-left
            Icon::Close => '\u{ea76}',           // close
            Icon::Plus => '\u{ea60}',            // add / plus
            Icon::NewFile => '\u{ea7f}',         // new-file
            Icon::NewFolder => '\u{ea80}',       // new-folder
            Icon::Folder => '\u{ea83}',          // folder
            Icon::FolderOpen => '\u{eaf7}',      // folder-opened
            Icon::File => '\u{ea7b}',            // file
            Icon::FileCode => '\u{eae9}',        // file-code
            Icon::Refresh => '\u{eb37}',         // refresh
            Icon::CollapseAll => '\u{eac5}',     // collapse-all
            Icon::ExpandAll => '\u{eb95}',       // expand-all
            Icon::More => '\u{ea7c}',            // ellipsis / more
            Icon::Filter => '\u{eaf1}',          // filter
            Icon::ClearAll => '\u{eabf}',        // clear-all
            Icon::Sun | Icon::Moon => return None,
            Icon::PanelLeft => '\u{ebf3}',       // layout-sidebar-left
            Icon::PanelBottom => '\u{ebf2}',     // layout-panel
            Icon::SplitHorizontal => '\u{eb56}', // split-horizontal
            Icon::SplitVertical => '\u{eb57}',   // split-vertical
            Icon::Preview => '\u{eb2f}',         // open-preview
            Icon::Pin => '\u{eb2b}',             // pin
            Icon::Pinned => '\u{eba0}',          // pinned
            Icon::WordWrap => '\u{eb80}',        // word-wrap
            Icon::CaseSensitive => '\u{eab1}',   // case-sensitive
            Icon::WholeWord => '\u{eb7e}',       // whole-word
            Icon::Regex => '\u{eb38}',           // regex
            Icon::Replace => '\u{eb3d}',         // replace
            Icon::ReplaceAll => '\u{eb3c}',      // replace-all
            Icon::ArrowUp => '\u{eaa1}',         // arrow-up
            Icon::ArrowDown => '\u{ea9a}',       // arrow-down
            Icon::Error => '\u{ea87}',           // error
            Icon::Warning => '\u{ea6c}',         // warning
            Icon::Info => '\u{ea74}',            // info
            Icon::Check => '\u{eab2}',           // check
            Icon::PassFilled => '\u{ebb3}',      // pass-filled
            Icon::Lightbulb => '\u{ea61}',       // lightbulb
            Icon::Trash => '\u{ea81}',           // trash
            Icon::Goforward => '\u{ea9c}',       // arrow-right
            Icon::Copy => '\u{ebcc}',            // copy
            Icon::Save => '\u{eb4b}',            // save
            Icon::SaveAll => '\u{eb49}',         // save-all
            Icon::Undo => '\u{ea7d}',            // reply / undo
            Icon::Redo => '\u{ebb0}',            // redo
            Icon::GitBranch => '\u{ea68}',       // git-branch
            Icon::GitCommit => '\u{eafc}',       // git-commit
            Icon::GitCompare => '\u{eafd}',      // git-compare
            Icon::DiffAdded => '\u{eadc}',       // diff-added
            Icon::DiffModified => '\u{eade}',    // diff-modified
            Icon::DiffRemoved => '\u{eadf}',     // diff-removed
            Icon::Discard => '\u{eae2}',         // discard
            Icon::Sync => '\u{ea77}',            // sync
            Icon::Stage => '\u{ea60}',           // add
            Icon::Unstage => '\u{eb3b}',         // remove
            Icon::SymbolFunction => '\u{ea8c}',  // symbol-method / function
            Icon::SymbolMethod => '\u{ea8c}',    // symbol-method
            Icon::SymbolStruct => '\u{ea91}',    // symbol-structure
            Icon::SymbolClass => '\u{eb5b}',     // symbol-class
            Icon::SymbolInterface => '\u{eb61}', // symbol-interface
            Icon::SymbolEnum => '\u{ea95}',      // symbol-enum
            Icon::SymbolConstant => '\u{eb5d}',  // symbol-constant
            Icon::SymbolVariable => '\u{ea88}',  // symbol-variable
            Icon::SymbolField => '\u{eb5f}',     // symbol-field
            Icon::SymbolProperty => '\u{eb65}',  // symbol-property
            Icon::SymbolModule => '\u{ea8b}',    // symbol-module
            Icon::SymbolKeyword => '\u{eb62}',   // symbol-keyword
            Icon::SymbolSnippet => '\u{eb66}',   // symbol-snippet
        })
    }
}

/// Paint an [`Icon`] inside `rect` with `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32, stroke: f32) {
    let s = rect.width().min(rect.height()).max(8.0);
    let r = Rect::from_center_size(rect.center(), Vec2::splat(s));

    if let Some(ch) = icon.codicon_char() {
        let font_size = (s * 1.06).round().max(10.0);
        let mut buf = [0u8; 4];
        let text = ch.encode_utf8(&mut buf);
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::new(font_size, codicon_family()),
            color,
        );
        return;
    }

    let p = |x: f32, y: f32| -> Pos2 { Pos2::new(r.left() + x * s, r.top() + y * s) };
    let st = Stroke::new(stroke.max(1.2), color);
    match icon {
        Icon::Sun => {
            painter.circle_stroke(p(0.5, 0.5), s * 0.21, st);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let inner = 0.31;
                let outer = 0.44;
                painter.add(Shape::line_segment(
                    [
                        p(0.5 + a.cos() * inner, 0.5 + a.sin() * inner),
                        p(0.5 + a.cos() * outer, 0.5 + a.sin() * outer),
                    ],
                    st,
                ));
            }
        }
        Icon::Moon => {
            // Crisp crescent moon using outer arc + inner offset arc.
            let mut pts = Vec::with_capacity(34);
            let c1 = p(0.50, 0.50);
            let r1 = s * 0.36;
            let c2 = p(0.64, 0.38);
            let r2 = s * 0.30;
            for i in 0..=18 {
                let t = i as f32 / 18.0;
                let a = 0.65 + t * 4.35;
                pts.push(Pos2::new(c1.x + a.cos() * r1, c1.y + a.sin() * r1));
            }
            for i in (0..=14).rev() {
                let t = i as f32 / 14.0;
                let a = 1.15 + t * 3.35;
                pts.push(Pos2::new(c2.x + a.cos() * r2, c2.y + a.sin() * r2));
            }
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            painter.add(Shape::line(pts, st));
        }
        _ => {}
    }
}

/// Paint a raw Codicon glyph by codepoint.
pub fn paint_codicon(painter: &Painter, rect: Rect, ch: char, color: Color32) {
    let s = rect.width().min(rect.height()).max(8.0);
    let font_size = (s * 1.05).round().max(10.0);
    let mut buf = [0u8; 4];
    let text = ch.encode_utf8(&mut buf);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::new(font_size, codicon_family()),
        color,
    );
}

/// Semantic color for a file extension.
pub fn file_color(ext: &str) -> Color32 {
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Color32::from_rgb(0xE4, 0x7B, 0x45),
        "py" | "pyi" | "pyw" => Color32::from_rgb(0x4B, 0x9C, 0xD3),
        "js" | "mjs" | "cjs" => Color32::from_rgb(0xF0, 0xDB, 0x4F),
        "jsx" => Color32::from_rgb(0x61, 0xDA, 0xFB),
        "ts" | "mts" | "cts" => Color32::from_rgb(0x31, 0x78, 0xC6),
        "tsx" => Color32::from_rgb(0x58, 0xC4, 0xDC),
        "json" | "jsonc" | "json5" => Color32::from_rgb(0xF0, 0xC6, 0x54),
        "toml" => Color32::from_rgb(0xC8, 0x8A, 0xF0),
        "md" | "markdown" | "mdx" => Color32::from_rgb(0x6C, 0xB6, 0xFF),
        "html" | "htm" | "vue" | "svelte" => Color32::from_rgb(0xE4, 0x4D, 0x26),
        "css" | "scss" | "less" => Color32::from_rgb(0x56, 0x9C, 0xD6),
        "go" => Color32::from_rgb(0x00, 0xAD, 0xD8),
        "c" => Color32::from_rgb(0x65, 0x9A, 0xD2),
        "h" | "hpp" | "hh" => Color32::from_rgb(0xB0, 0x8A, 0xE0),
        "cpp" | "cc" | "cxx" => Color32::from_rgb(0x5B, 0x9B, 0xE5),
        "java" | "jar" | "class" => Color32::from_rgb(0xE8, 0x7B, 0x35),
        "cs" => Color32::from_rgb(0x9B, 0x6F, 0xE0),
        "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" => Color32::from_rgb(0x73, 0xC9, 0x91),
        "yml" | "yaml" => Color32::from_rgb(0xE0, 0x6C, 0x75),
        "lock" => Color32::from_rgb(0xD1, 0x9A, 0x66),
        "gitignore" | "gitattributes" | "gitmodules" => Color32::from_rgb(0xF1, 0x4E, 0x32),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp" | "bmp" | "tif" | "tiff" => {
            Color32::from_rgb(0xC6, 0x78, 0xDD)
        }
        "pdf" => Color32::from_rgb(0xE0, 0x52, 0x52),
        "docx" | "doc" => Color32::from_rgb(0x2B, 0x7C, 0xD3),
        "xlsx" | "xlsm" | "xls" | "csv" | "tsv" => Color32::from_rgb(0x21, 0xA3, 0x66),
        "zip" | "tar" | "gz" | "7z" | "rar" => Color32::from_rgb(0xD1, 0x9A, 0x66),
        "exe" | "dll" | "so" | "dylib" | "bin" => Color32::from_rgb(0x98, 0xA2, 0xB3),
        _ => Color32::from_rgb(0x8A, 0x96, 0xAA),
    }
}

/// Semantic accent color for well-known directory names.
pub fn folder_color(name: &str, default_color: Color32) -> Color32 {
    match name.to_ascii_lowercase().as_str() {
        "src" | "source" | "lib" => Color32::from_rgb(0x5A, 0xA3, 0xF2),
        "ui" | "views" | "components" | "theme" => Color32::from_rgb(0xC8, 0x8A, 0xF0),
        "editor" | "core" | "engine" => Color32::from_rgb(0x67, 0xD6, 0xC6),
        "workspace" | "project" => Color32::from_rgb(0xEF, 0xBF, 0x7D),
        "highlight" | "syntax" | "lsp" => Color32::from_rgb(0x8F, 0xD3, 0xA0),
        "docs" | "doc" => Color32::from_rgb(0x7B, 0xB6, 0xFF),
        "scripts" | "tools" | "bin" => Color32::from_rgb(0x56, 0xB6, 0xC2),
        "tests" | "test" | "benches" => Color32::from_rgb(0x6D, 0xD4, 0x9A),
        "assets" | "resources" | "icons" | "images" => Color32::from_rgb(0xE5, 0xC0, 0x7B),
        ".git" | ".github" | ".vscode" => Color32::from_rgb(0xF0, 0x88, 0x3E),
        "target" | "dist" | "build" | "out" | "node_modules" => Color32::from_rgb(0x76, 0x80, 0x94),
        _ => {
            if default_color == Color32::TRANSPARENT {
                Color32::from_rgb(0x8E, 0x9E, 0xB8)
            } else {
                Color32::from_rgb(0x8E, 0x9E, 0xB8)
            }
        }
    }
}

/// Paint a duotone folder or file icon when only color + `is_dir` is known.
pub fn file_glyph(painter: &Painter, rect: Rect, color: Color32, is_dir: bool, expanded: bool) {
    if is_dir {
        paint_folder_duotone(painter, rect, color, expanded);
        return;
    }
    paint_codicon(painter, rect, '\u{eae9}', color);
}

/// Paint a rich Material/Seti-style icon for a specific file or directory path.
pub fn paint_path_icon(
    painter: &Painter,
    rect: Rect,
    path: &Path,
    is_dir: bool,
    expanded: bool,
    fallback_color: Color32,
) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_dir {
        let color = folder_color(&name, fallback_color);
        paint_folder_duotone(painter, rect, color, expanded);
        return;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Special filenames first.
    match name.as_str() {
        ".gitignore" | ".gitattributes" | ".gitmodules" => {
            paint_codicon(painter, rect, '\u{ea68}', Color32::from_rgb(0xF1, 0x4E, 0x32));
            return;
        }
        "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "uv.lock" => {
            paint_codicon(painter, rect, '\u{ea75}', Color32::from_rgb(0xD1, 0x9A, 0x66));
            return;
        }
        "cargo.toml" => {
            paint_badge(painter, rect, "CG", Color32::from_rgb(0xE4, 0x7B, 0x45));
            return;
        }
        "license" | "license.md" | "license.txt" => {
            paint_codicon(painter, rect, '\u{eb12}', Color32::from_rgb(0xE5, 0xC0, 0x7B));
            return;
        }
        "readme.md" | "readme" => {
            paint_codicon(painter, rect, '\u{eb1d}', Color32::from_rgb(0x6C, 0xB6, 0xFF));
            return;
        }
        _ => {}
    }

    let color = file_color(&ext);
    match ext.as_str() {
        "rs" => paint_badge(painter, rect, "RS", color),
        "ts" | "mts" | "cts" => paint_badge(painter, rect, "TS", color),
        "tsx" => paint_badge(painter, rect, "TX", color),
        "js" | "mjs" | "cjs" => paint_badge(painter, rect, "JS", color),
        "jsx" => paint_badge(painter, rect, "JX", color),
        "py" | "pyi" | "pyw" => paint_codicon(painter, rect, '\u{ec16}', color), // snake
        "json" | "jsonc" | "json5" => paint_codicon(painter, rect, '\u{eb0f}', color), // json braces
        "toml" | "ini" | "cfg" | "conf" => paint_codicon(painter, rect, '\u{eb52}', color), // settings
        "md" | "markdown" | "mdx" => paint_codicon(painter, rect, '\u{eb1d}', color), // markdown
        "html" | "htm" | "vue" | "svelte" => paint_codicon(painter, rect, '\u{eac4}', color), // code
        "css" | "scss" | "less" => paint_codicon(painter, rect, '\u{eb5c}', color), // symbol-color
        "go" => paint_badge(painter, rect, "GO", color),
        "c" => paint_badge(painter, rect, "C", color),
        "cpp" | "cc" | "cxx" => paint_badge(painter, rect, "C+", color),
        "h" | "hpp" | "hh" => paint_badge(painter, rect, "H", color),
        "cs" => paint_badge(painter, rect, "C#", color),
        "java" | "jar" => paint_codicon(painter, rect, '\u{ec15}', color), // coffee
        "sh" | "bash" | "zsh" | "ps1" | "bat" => paint_codicon(painter, rect, '\u{ebca}', color),
        "yml" | "yaml" => paint_codicon(painter, rect, '\u{eb86}', color),
        "lock" => paint_codicon(painter, rect, '\u{ea75}', color),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp" | "bmp" | "tif" | "tiff" => {
            paint_codicon(painter, rect, '\u{eaea}', color)
        }
        "zip" | "tar" | "gz" | "7z" => paint_codicon(painter, rect, '\u{eaef}', color),
        "exe" | "dll" | "so" | "bin" => paint_codicon(painter, rect, '\u{eae8}', color),
        "pdf" => paint_badge(painter, rect, "PD", color),
        "docx" | "doc" => paint_badge(painter, rect, "WD", color),
        "xlsx" | "xlsm" | "xls" => paint_badge(painter, rect, "XL", color),
        "csv" | "tsv" => paint_badge(painter, rect, "CS", color),
        _ => paint_codicon(painter, rect, '\u{ea7b}', color),
    }
}

fn paint_folder_duotone(painter: &Painter, rect: Rect, color: Color32, expanded: bool) {
    let s = rect.width().min(rect.height()).max(10.0);
    let r = Rect::from_center_size(rect.center(), Vec2::splat(s));
    let p = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y * s);

    // Back tab plate
    let tab_pts = vec![
        p(0.10, 0.24),
        p(0.42, 0.24),
        p(0.50, 0.34),
        p(0.88, 0.34),
        p(0.88, 0.78),
        p(0.10, 0.78),
    ];
    painter.add(Shape::convex_polygon(
        tab_pts,
        color.gamma_multiply(0.28),
        Stroke::new(1.15, color),
    ));

    // Front folder flap
    let top_y = if expanded { 0.44 } else { 0.36 };
    let body = Rect::from_min_max(p(0.10, top_y), p(0.90, 0.80));
    painter.rect_filled(body, CornerRadius::same(2), color.gamma_multiply(0.45));
    painter.rect_stroke(
        body,
        CornerRadius::same(2),
        Stroke::new(1.15, color),
        egui::StrokeKind::Inside,
    );
}

fn paint_badge(painter: &Painter, rect: Rect, label: &str, color: Color32) {
    let s = rect.width().min(rect.height()).max(12.0);
    let badge = Rect::from_center_size(rect.center(), Vec2::new(s * 0.94, s * 0.82));
    painter.rect_filled(badge, CornerRadius::same(3), color.gamma_multiply(0.22));
    painter.rect_stroke(
        badge,
        CornerRadius::same(3),
        Stroke::new(1.1, color.gamma_multiply(0.88)),
        egui::StrokeKind::Inside,
    );
    let font_size = if label.len() == 1 {
        (s * 0.56).round().max(8.5)
    } else {
        (s * 0.48).round().max(7.5)
    };
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(font_size, egui::FontFamily::Monospace),
        color,
    );
}
