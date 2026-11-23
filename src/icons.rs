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
    let st = Stroke::new(stroke.max(1.25), color);
    match icon {
        Icon::Sun => {
            painter.circle_stroke(p(0.5, 0.5), s * 0.19, st);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let inner = 0.31;
                let outer = 0.43;
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
            // Mathematically closed crescent: solve exact intersection of two circles
            // so the outer and inner arcs meet cleanly with zero stray gap.
            let c1 = Vec2::new(0.49, 0.51);
            let r1 = 0.34f32;
            let c2 = Vec2::new(0.65, 0.36);
            let r2 = 0.27f32;
            let d_vec = c2 - c1;
            let d = d_vec.length();
            let base = d_vec.y.atan2(d_vec.x);
            let cos1 = ((r1 * r1 + d * d - r2 * r2) / (2.0 * r1 * d)).clamp(-1.0, 1.0);
            let a1 = cos1.acos();
            let start1 = base + a1;
            let end1 = base + std::f32::consts::TAU - a1;

            let cos2 = ((r2 * r2 + d * d - r1 * r1) / (2.0 * r2 * d)).clamp(-1.0, 1.0);
            let a2 = cos2.acos();
            let rev_base = (c1 - c2).y.atan2((c1 - c2).x);
            let start2 = rev_base + a2;
            let end2 = rev_base - a2;

            let mut pts = Vec::with_capacity(34);
            for i in 0..=18 {
                let t = i as f32 / 18.0;
                let a = start1 + t * (end1 - start1);
                pts.push(p(c1.x + a.cos() * r1, c1.y + a.sin() * r1));
            }
            for i in 1..=14 {
                let t = i as f32 / 14.0;
                let a = start2 + t * (end2 - start2);
                pts.push(p(c2.x + a.cos() * r2, c2.y + a.sin() * r2));
            }
            painter.add(Shape::closed_line(pts, st));
        }
        _ => {}
    }
}

/// Paint Kestrel's signature geometric falcon-wing brand mark inside `rect`.
pub fn paint_kestrel_logo(painter: &Painter, rect: Rect, theme: &crate::theme::Theme) {
    let s = rect.width().min(rect.height()).max(16.0);
    let r = Rect::from_center_size(rect.center(), Vec2::splat(s));
    let radius = (s * 0.24).round().clamp(4.0, 14.0) as u8;
    // Deep cobalt badge with crisp subtle rim highlight.
    painter.rect_filled(r, CornerRadius::same(radius), theme.accent_fill);
    painter.rect_stroke(
        r,
        CornerRadius::same(radius),
        Stroke::new(1.0, Color32::from_white_alpha(if theme.kind.is_dark() { 45 } else { 60 })),
        egui::StrokeKind::Inside,
    );
    let p = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y * s);
    // Clean geometric K-wing emblem (two crisp convex facets, no font dependency).
    let stem = vec![p(0.27, 0.23), p(0.41, 0.23), p(0.41, 0.77), p(0.27, 0.77)];
    painter.add(Shape::convex_polygon(stem, theme.accent_text, Stroke::NONE));

    let upper_wing = vec![p(0.43, 0.50), p(0.62, 0.23), p(0.77, 0.23), p(0.55, 0.52)];
    painter.add(Shape::convex_polygon(upper_wing, theme.accent_text, Stroke::NONE));

    let lower_wing = vec![
        p(0.45, 0.48),
        p(0.57, 0.48),
        p(0.77, 0.77),
        p(0.61, 0.77),
    ];
    painter.add(Shape::convex_polygon(
        lower_wing,
        Color32::from_rgba_unmultiplied(255, 255, 255, 215),
        Stroke::NONE,
    ));
}

/// Semantic color for outline / autocomplete symbol icons.
pub fn symbol_color(kind: crate::lsp::SymbolKind, theme: &crate::theme::Theme) -> Color32 {
    use crate::lsp::SymbolKind;
    match kind {
        SymbolKind::Function | SymbolKind::Method => theme.syn_function,
        SymbolKind::Struct | SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => {
            theme.syn_type
        }
        SymbolKind::Constant => theme.syn_constant,
        SymbolKind::Module | SymbolKind::Heading | SymbolKind::Keyword => theme.syn_keyword,
        SymbolKind::Snippet => theme.syn_macro,
        SymbolKind::Variable | SymbolKind::Field | SymbolKind::Property => theme.syn_property,
    }
}

/// Semantic color for completion icons.
pub fn completion_icon_color(icon: Icon, theme: &crate::theme::Theme) -> Color32 {
    match icon {
        Icon::SymbolFunction | Icon::SymbolMethod => theme.syn_function,
        Icon::SymbolStruct | Icon::SymbolClass | Icon::SymbolInterface | Icon::SymbolEnum => {
            theme.syn_type
        }
        Icon::SymbolConstant => theme.syn_constant,
        Icon::SymbolKeyword => theme.syn_keyword,
        Icon::SymbolSnippet => theme.syn_macro,
        Icon::SymbolModule => theme.syn_attribute,
        Icon::SymbolField | Icon::SymbolProperty | Icon::SymbolVariable => theme.syn_property,
        _ => theme.accent,
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

/// Pick the dark- or light-theme variant of a badge color.
const fn tint(dark: bool, dark_rgb: [u8; 3], light_rgb: [u8; 3]) -> Color32 {
    let c = if dark { dark_rgb } else { light_rgb };
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Semantic color for a file extension.
///
/// The light set is the darkened variant: the pastel badges tuned for a dark
/// background sit at 1.5-2.5:1 on the light editor surface.
pub fn file_color(ext: &str, dark: bool) -> Color32 {
    match ext.to_ascii_lowercase().as_str() {
        "rs" => tint(dark, [0xE4, 0x7B, 0x45], [0xB5, 0x45, 0x1A]),
        "py" | "pyi" | "pyw" => tint(dark, [0x4B, 0x9C, 0xD3], [0x1F, 0x5F, 0xBF]),
        "js" | "mjs" | "cjs" => tint(dark, [0xF0, 0xDB, 0x4F], [0x8A, 0x6A, 0x00]),
        "jsx" => tint(dark, [0x61, 0xDA, 0xFB], [0x0B, 0x6E, 0x80]),
        "ts" | "mts" | "cts" => tint(dark, [0x31, 0x78, 0xC6], [0x2A, 0x5F, 0xA8]),
        "tsx" => tint(dark, [0x58, 0xC4, 0xDC], [0x0E, 0x6E, 0x80]),
        "json" | "jsonc" | "json5" => tint(dark, [0xF0, 0xC6, 0x54], [0x8A, 0x5A, 0x00]),
        "toml" => tint(dark, [0xC8, 0x8A, 0xF0], [0x7A, 0x34, 0xA8]),
        "md" | "markdown" | "mdx" => tint(dark, [0x6C, 0xB6, 0xFF], [0x1F, 0x5F, 0xBF]),
        "html" | "htm" | "vue" | "svelte" => tint(dark, [0xE4, 0x4D, 0x26], [0xB3, 0x36, 0x1A]),
        "css" | "scss" | "less" => tint(dark, [0x56, 0x9C, 0xD6], [0x2A, 0x5F, 0xA8]),
        "go" => tint(dark, [0x00, 0xAD, 0xD8], [0x00, 0x69, 0x7A]),
        "c" => tint(dark, [0x65, 0x9A, 0xD2], [0x2A, 0x5F, 0xA8]),
        "h" | "hpp" | "hh" => tint(dark, [0xB0, 0x8A, 0xE0], [0x6B, 0x3F, 0xA0]),
        "cpp" | "cc" | "cxx" => tint(dark, [0x5B, 0x9B, 0xE5], [0x1F, 0x5F, 0xBF]),
        "java" | "jar" | "class" => tint(dark, [0xE8, 0x7B, 0x35], [0xA8, 0x50, 0x1A]),
        "cs" => tint(dark, [0x9B, 0x6F, 0xE0], [0x6B, 0x3F, 0xA0]),
        "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" => {
            tint(dark, [0x73, 0xC9, 0x91], [0x1F, 0x6E, 0x2E])
        }
        "yml" | "yaml" => tint(dark, [0xE0, 0x6C, 0x75], [0xB3, 0x26, 0x1E]),
        "lock" => tint(dark, [0xD1, 0x9A, 0x66], [0x8A, 0x5A, 0x00]),
        "gitignore" | "gitattributes" | "gitmodules" => {
            tint(dark, [0xF1, 0x4E, 0x32], [0xB3, 0x26, 0x1E])
        }
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp" | "bmp" | "tif" | "tiff" => {
            tint(dark, [0xC6, 0x78, 0xDD], [0x82, 0x26, 0xA8])
        }
        "pdf" => tint(dark, [0xE0, 0x52, 0x52], [0xB3, 0x26, 0x1E]),
        "docx" | "doc" => tint(dark, [0x2B, 0x7C, 0xD3], [0x1F, 0x5F, 0xBF]),
        "xlsx" | "xlsm" | "xls" | "csv" | "tsv" => {
            tint(dark, [0x21, 0xA3, 0x66], [0x1F, 0x6E, 0x2E])
        }
        "zip" | "tar" | "gz" | "7z" | "rar" => {
            tint(dark, [0xD1, 0x9A, 0x66], [0x8A, 0x5A, 0x00])
        }
        "exe" | "dll" | "so" | "dylib" | "bin" => {
            tint(dark, [0x98, 0xA2, 0xB3], [0x5A, 0x62, 0x72])
        }
        _ => tint(dark, [0x8A, 0x96, 0xAA], [0x5A, 0x62, 0x72]),
    }
}

/// Semantic accent color for well-known directory names.
///
/// Unknown names fall back to the caller-supplied color, so the tree inherits
/// the theme text color instead of a fixed blue-grey.
pub fn folder_color(name: &str, default_color: Color32, dark: bool) -> Color32 {
    match name.to_ascii_lowercase().as_str() {
        "src" | "source" | "lib" => tint(dark, [0x5A, 0xA3, 0xF2], [0x2F, 0x6F, 0xDE]),
        "ui" | "views" | "components" | "theme" => {
            tint(dark, [0xC8, 0x8A, 0xF0], [0x7A, 0x34, 0xA8])
        }
        "editor" | "core" | "engine" => tint(dark, [0x67, 0xD6, 0xC6], [0x0E, 0x74, 0x90]),
        "workspace" | "project" => tint(dark, [0xEF, 0xBF, 0x7D], [0x8A, 0x5A, 0x00]),
        "highlight" | "syntax" | "lsp" => tint(dark, [0x8F, 0xD3, 0xA0], [0x2E, 0x7D, 0x32]),
        "docs" | "doc" => tint(dark, [0x7B, 0xB6, 0xFF], [0x1F, 0x5F, 0xBF]),
        "scripts" | "tools" | "bin" => tint(dark, [0x56, 0xB6, 0xC2], [0x0B, 0x6E, 0x80]),
        "tests" | "test" | "benches" => tint(dark, [0x6D, 0xD4, 0x9A], [0x1F, 0x6E, 0x2E]),
        "assets" | "resources" | "icons" | "images" => {
            tint(dark, [0xE5, 0xC0, 0x7B], [0x8A, 0x6A, 0x1F])
        }
        ".git" | ".github" | ".vscode" => tint(dark, [0xF0, 0x88, 0x3E], [0xB0, 0x4A, 0x18]),
        "target" | "dist" | "build" | "out" | "node_modules" => {
            tint(dark, [0x76, 0x80, 0x94], [0x5A, 0x62, 0x72])
        }
        _ => default_color,
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
    dark: bool,
) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_dir {
        let color = folder_color(&name, fallback_color, dark);
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
            paint_codicon(painter, rect, '\u{ea68}', file_color("gitignore", dark));
            return;
        }
        "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "uv.lock" => {
            paint_codicon(painter, rect, '\u{ea75}', file_color("lock", dark));
            return;
        }
        "cargo.toml" => {
            paint_codicon(painter, rect, '\u{eb29}', file_color("rs", dark));
            return;
        }
        "license" | "license.md" | "license.txt" => {
            paint_codicon(painter, rect, '\u{eb12}', folder_color("assets", Color32::TRANSPARENT, dark));
            return;
        }
        "readme.md" | "readme" => {
            paint_codicon(painter, rect, '\u{eb1d}', file_color("md", dark));
            return;
        }
        _ => {}
    }

    let color = file_color(&ext, dark);
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

    let tab_fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 155);
    let body_fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 215);
    let rim = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 255);

    // Strictly convex top folder tab + back plate (zero concave polygon artifacts).
    let tab = Rect::from_min_max(p(0.10, 0.22), p(0.48, 0.40));
    painter.rect_filled(
        tab,
        CornerRadius {
            nw: 2,
            ne: 2,
            sw: 0,
            se: 0,
        },
        tab_fill,
    );
    let back = Rect::from_min_max(p(0.10, 0.30), p(0.88, 0.78));
    painter.rect_filled(back, CornerRadius::same(2), tab_fill);

    // Front folder flap (drops open slightly when expanded).
    let top_y = if expanded { 0.43 } else { 0.35 };
    let body = Rect::from_min_max(p(0.10, top_y), p(0.90, 0.80));
    painter.rect_filled(body, CornerRadius::same(2), body_fill);
    painter.rect_stroke(
        body,
        CornerRadius::same(2),
        Stroke::new(0.85, rim),
        egui::StrokeKind::Inside,
    );
}

fn paint_badge(painter: &Painter, rect: Rect, label: &str, color: Color32) {
    let s = rect.width().min(rect.height()).max(12.0);
    let badge = Rect::from_center_size(rect.center(), Vec2::new(s * 0.96, s * 0.80));
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38);
    let border = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 140);
    painter.rect_filled(badge, CornerRadius::same(3), fill);
    painter.rect_stroke(
        badge,
        CornerRadius::same(3),
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let font_size = if label.len() == 1 {
        (s * 0.56).round().max(8.5)
    } else {
        (s * 0.47).round().max(7.5)
    };
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(font_size, egui::FontFamily::Proportional),
        color,
    );
}
