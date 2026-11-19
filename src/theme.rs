//! Color tokens and egui style for Kestrel's dark and light themes.

use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

use crate::highlight::TokenKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn from_str(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("light") {
            Self::Light
        } else {
            Self::Dark
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }

    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Dark => 0,
            Self::Light => 1,
        }
    }
}

/// Every color the editor UI paints with.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,

    pub bg: Color32,
    pub sidebar_bg: Color32,
    pub panel_bg: Color32,
    pub activity_bg: Color32,
    pub status_bg: Color32,
    pub overlay_bg: Color32,
    pub hover: Color32,
    pub active: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub input_bg: Color32,

    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_text: Color32,

    pub selection: Color32,
    pub selection_inactive: Color32,
    pub current_line: Color32,
    pub find_match: Color32,
    pub find_active: Color32,
    pub bracket_match: Color32,
    pub indent_guide: Color32,
    pub indent_guide_active: Color32,

    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,

    // Git status colors
    pub git_added: Color32,
    pub git_modified: Color32,
    pub git_deleted: Color32,
    pub git_untracked: Color32,

    pub shadow: Color32,

    // Syntax
    pub syn_text: Color32,
    pub syn_keyword: Color32,
    pub syn_string: Color32,
    pub syn_comment: Color32,
    pub syn_number: Color32,
    pub syn_function: Color32,
    pub syn_type: Color32,
    pub syn_variable: Color32,
    pub syn_constant: Color32,
    pub syn_property: Color32,
    pub syn_operator: Color32,
    pub syn_punctuation: Color32,
    pub syn_macro: Color32,
    pub syn_attribute: Color32,
    pub syn_heading: Color32,
    pub syn_link: Color32,
    pub syn_tag: Color32,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            bg: Color32::from_rgb(0x14, 0x16, 0x1B),
            sidebar_bg: Color32::from_rgb(0x18, 0x1B, 0x21),
            panel_bg: Color32::from_rgb(0x1A, 0x1D, 0x24),
            activity_bg: Color32::from_rgb(0x11, 0x13, 0x18),
            status_bg: Color32::from_rgb(0x0E, 0x10, 0x14),
            overlay_bg: Color32::from_rgb(0x1F, 0x22, 0x2A),
            hover: Color32::from_rgb(0x23, 0x27, 0x30),
            active: Color32::from_rgb(0x2A, 0x2F, 0x3A),
            border: Color32::from_rgb(0x22, 0x25, 0x2D),
            border_strong: Color32::from_rgb(0x2E, 0x33, 0x3E),
            input_bg: Color32::from_rgb(0x0F, 0x11, 0x15),

            text: Color32::from_rgb(0xCF, 0xD5, 0xE1),
            text_dim: Color32::from_rgb(0x84, 0x8D, 0xA0),
            text_faint: Color32::from_rgb(0x5B, 0x63, 0x74),
            accent: Color32::from_rgb(0x5A, 0xA3, 0xF2),
            accent_text: Color32::from_rgb(0x0D, 0x12, 0x1A),

            selection: Color32::from_rgb(0x28, 0x3A, 0x5C),
            selection_inactive: Color32::from_rgb(0x25, 0x29, 0x33),
            current_line: Color32::from_rgb(0x19, 0x1C, 0x23),
            find_match: Color32::from_rgba_premultiplied(0x5C, 0x49, 0x12, 0xC8),
            find_active: Color32::from_rgb(0x8A, 0x6D, 0x1E),
            bracket_match: Color32::from_rgba_premultiplied(0x2A, 0x44, 0x2E, 0xB0),
            indent_guide: Color32::from_rgb(0x25, 0x28, 0x31),
            indent_guide_active: Color32::from_rgb(0x3A, 0x40, 0x4E),

            error: Color32::from_rgb(0xF2, 0x73, 0x6B),
            warning: Color32::from_rgb(0xE6, 0xB5, 0x5C),
            success: Color32::from_rgb(0x6D, 0xD4, 0x9A),

            git_added: Color32::from_rgb(0x6D, 0xD4, 0x9A),
            git_modified: Color32::from_rgb(0xE6, 0xB5, 0x5C),
            git_deleted: Color32::from_rgb(0xF2, 0x73, 0x6B),
            git_untracked: Color32::from_rgb(0x73, 0xC9, 0x91),

            shadow: Color32::from_black_alpha(90),

            syn_text: Color32::from_rgb(0xCD, 0xD3, 0xDF),
            syn_keyword: Color32::from_rgb(0xC8, 0x8A, 0xF0),
            syn_string: Color32::from_rgb(0x8F, 0xD3, 0xA0),
            syn_comment: Color32::from_rgb(0x5F, 0x6A, 0x7D),
            syn_number: Color32::from_rgb(0xEF, 0xBF, 0x7D),
            syn_function: Color32::from_rgb(0x7B, 0xB6, 0xFF),
            syn_type: Color32::from_rgb(0x67, 0xD6, 0xC6),
            syn_variable: Color32::from_rgb(0xCD, 0xD3, 0xDF),
            syn_constant: Color32::from_rgb(0xEF, 0xBF, 0x7D),
            syn_property: Color32::from_rgb(0xA9, 0xC4, 0xE8),
            syn_operator: Color32::from_rgb(0x8F, 0xD0, 0xE8),
            syn_punctuation: Color32::from_rgb(0x93, 0x9C, 0xAE),
            syn_macro: Color32::from_rgb(0xF0, 0x9A, 0x7A),
            syn_attribute: Color32::from_rgb(0xE8, 0xC4, 0x7E),
            syn_heading: Color32::from_rgb(0x7B, 0xB6, 0xFF),
            syn_link: Color32::from_rgb(0x67, 0xD6, 0xC6),
            syn_tag: Color32::from_rgb(0xF0, 0x8F, 0x8F),
        }
    }

    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            bg: Color32::from_rgb(0xFA, 0xFA, 0xFC),
            sidebar_bg: Color32::from_rgb(0xF3, 0xF4, 0xF8),
            panel_bg: Color32::from_rgb(0xEC, 0xEE, 0xF3),
            activity_bg: Color32::from_rgb(0xE8, 0xEA, 0xF0),
            status_bg: Color32::from_rgb(0xE7, 0xE9, 0xEF),
            overlay_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            hover: Color32::from_rgb(0xE6, 0xE9, 0xF0),
            active: Color32::from_rgb(0xDA, 0xDE, 0xE8),
            border: Color32::from_rgb(0xDC, 0xE0, 0xE8),
            border_strong: Color32::from_rgb(0xC8, 0xCE, 0xDA),
            input_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),

            text: Color32::from_rgb(0x23, 0x26, 0x2E),
            text_dim: Color32::from_rgb(0x68, 0x70, 0x80),
            text_faint: Color32::from_rgb(0x92, 0x99, 0xA8),
            accent: Color32::from_rgb(0x2F, 0x6F, 0xDE),
            accent_text: Color32::from_rgb(0xFF, 0xFF, 0xFF),

            selection: Color32::from_rgb(0xD3, 0xE3, 0xFC),
            selection_inactive: Color32::from_rgb(0xE6, 0xE9, 0xF0),
            current_line: Color32::from_rgb(0xF2, 0xF4, 0xF8),
            find_match: Color32::from_rgb(0xFF, 0xEB, 0xA8),
            find_active: Color32::from_rgb(0xFF, 0xC9, 0x6B),
            bracket_match: Color32::from_rgb(0xC4, 0xE8, 0xCE),
            indent_guide: Color32::from_rgb(0xE3, 0xE6, 0xEC),
            indent_guide_active: Color32::from_rgb(0xBF, 0xC6, 0xD4),

            error: Color32::from_rgb(0xD1, 0x3A, 0x3A),
            warning: Color32::from_rgb(0xA8, 0x6A, 0x00),
            success: Color32::from_rgb(0x1F, 0x8A, 0x4C),

            git_added: Color32::from_rgb(0x1F, 0x8A, 0x4C),
            git_modified: Color32::from_rgb(0xA8, 0x6A, 0x00),
            git_deleted: Color32::from_rgb(0xD1, 0x3A, 0x3A),
            git_untracked: Color32::from_rgb(0x1F, 0x8A, 0x4C),

            shadow: Color32::from_black_alpha(30),

            syn_text: Color32::from_rgb(0x2A, 0x2E, 0x37),
            syn_keyword: Color32::from_rgb(0xA6, 0x2A, 0xA4),
            syn_string: Color32::from_rgb(0x2E, 0x7D, 0x32),
            syn_comment: Color32::from_rgb(0x8C, 0x93, 0xA1),
            syn_number: Color32::from_rgb(0xB5, 0x62, 0x00),
            syn_function: Color32::from_rgb(0x2F, 0x6F, 0xDE),
            syn_type: Color32::from_rgb(0x0E, 0x74, 0x90),
            syn_variable: Color32::from_rgb(0x2A, 0x2E, 0x37),
            syn_constant: Color32::from_rgb(0xB5, 0x62, 0x00),
            syn_property: Color32::from_rgb(0x39, 0x5A, 0x8A),
            syn_operator: Color32::from_rgb(0x1F, 0x77, 0xA8),
            syn_punctuation: Color32::from_rgb(0x5A, 0x61, 0x70),
            syn_macro: Color32::from_rgb(0xC0, 0x5A, 0x1F),
            syn_attribute: Color32::from_rgb(0x8A, 0x6D, 0x1F),
            syn_heading: Color32::from_rgb(0x1F, 0x56, 0xB8),
            syn_link: Color32::from_rgb(0x0E, 0x74, 0x90),
            syn_tag: Color32::from_rgb(0xB3, 0x3A, 0x3A),
        }
    }

    pub fn by_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    /// Push this theme into egui's global style.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.global_style()).clone();
        let mut v = if self.kind == ThemeKind::Dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        v.panel_fill = self.bg;
        v.window_fill = self.overlay_bg;
        v.extreme_bg_color = self.input_bg;
        v.faint_bg_color = self.hover;
        v.code_bg_color = self.input_bg;
        v.window_stroke = Stroke::new(1.0, self.border_strong);
        v.window_corner_radius = CornerRadius::same(10);
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: self.shadow,
        };
        v.popup_shadow = egui::epaint::Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: self.shadow,
        };
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.warning;
        v.error_fg_color = self.error;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.text_cursor.stroke = Stroke::new(1.5, self.accent);
        v.text_cursor.preview = true;
        v.widgets.noninteractive.bg_fill = self.bg;
        v.widgets.noninteractive.weak_bg_fill = self.bg;
        v.widgets.noninteractive.bg_stroke = Stroke::NONE;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.noninteractive.corner_radius = CornerRadius::same(6);

        v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.inactive.corner_radius = CornerRadius::same(6);

        v.widgets.hovered.bg_fill = self.hover;
        v.widgets.hovered.weak_bg_fill = self.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.hovered.corner_radius = CornerRadius::same(6);

        v.widgets.active.bg_fill = self.active;
        v.widgets.active.weak_bg_fill = self.active;
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.active.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.active.corner_radius = CornerRadius::same(6);

        v.widgets.open.bg_fill = self.active;
        v.widgets.open.weak_bg_fill = self.active;
        v.widgets.open.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.open.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.open.corner_radius = CornerRadius::same(6);

        style.visuals = v;
        style.animation_time = 0.10;

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(9.0, 4.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.window_margin = egui::Margin::same(10);
        style.spacing.indent = 16.0;
        style.spacing.interact_size = egui::vec2(26.0, 24.0);
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.bar_width = 10.0;
        style.spacing.scroll.floating = true;
        style.spacing.scroll.bar_inner_margin = 3.0;
        style.spacing.scroll.bar_outer_margin = 2.0;
        style.spacing.tooltip_width = 420.0;

        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(11.5, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(17.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, egui::FontFamily::Monospace),
        );

        let egui_theme = match self.kind {
            ThemeKind::Dark => egui::Theme::Dark,
            ThemeKind::Light => egui::Theme::Light,
        };
        ctx.set_theme(egui_theme);
        ctx.set_style_of(egui_theme, style.clone());
        ctx.set_global_style(style);
        ctx.request_repaint();
    }

    /// Color used for a syntax token category.
    pub fn token_color(&self, kind: TokenKind) -> Color32 {
        match kind {
            TokenKind::Keyword => self.syn_keyword,
            TokenKind::String => self.syn_string,
            TokenKind::Comment => self.syn_comment,
            TokenKind::Number => self.syn_number,
            TokenKind::Function => self.syn_function,
            TokenKind::Type => self.syn_type,
            TokenKind::Variable => self.syn_variable,
            TokenKind::Constant => self.syn_constant,
            TokenKind::Property => self.syn_property,
            TokenKind::Operator => self.syn_operator,
            TokenKind::Punctuation => self.syn_punctuation,
            TokenKind::Macro => self.syn_macro,
            TokenKind::Attribute => self.syn_attribute,
            TokenKind::Heading => self.syn_heading,
            TokenKind::Link => self.syn_link,
            TokenKind::Tag => self.syn_tag,
        }
    }
}
