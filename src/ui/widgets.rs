//! Shared drawing primitives for hand-painted rows, icon buttons, action buttons,
//! keycap chips, and cached ellipsized text.
//!
//! Every list-ish surface (file tree, tabs, palette rows, status bar hit targets)
//! paints through these primitives so hover / selected / pressed / focus / disabled
//! and smooth 120ms transitions behave consistently everywhere.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{
    Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::icons::{self, Icon};
use crate::theme::{Theme, R_SM};

/// What a row wants painted on top of the pointer state it derives itself.
#[derive(Clone, Copy, Debug)]
pub struct RowState {
    /// The row is the current selection (active tab, selected file, ...).
    pub selected: bool,
    /// Disabled rows stay visible but drop hover, click and focus feedback.
    pub enabled: bool,
}

impl RowState {
    pub const fn new(selected: bool, enabled: bool) -> Self {
        Self { selected, enabled }
    }

    pub const fn idle() -> Self {
        Self::new(false, true)
    }

    pub const fn selected_row() -> Self {
        Self::new(true, true)
    }
}

/// Visual variants for unified action buttons across welcome, dialogs, and panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// Ellipsis placement when truncating text to fit a maximum width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EllipsizeMode {
    Middle,
    End,
}

/// Hover fade duration shared by hand-painted chrome. Only paint colors read it,
/// so layout never shifts while a control animates in or out.
pub const HOVER_FADE: f32 = 0.12;

/// 0..=1 hover progress for `hovered`, animated over [`HOVER_FADE`].
pub fn hover_fade(
    ui: &Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    hovered: bool,
) -> f32 {
    ui.ctx()
        .animate_bool_with_time(ui.scope_id().with(id_salt), hovered, HOVER_FADE)
}

/// Linear RGBA blend between two colors; `t` is clamped to `0..=1`.
pub fn blend(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
        mix(from.a(), to.a()),
    )
}

/// Paint hover / selected / pressed / focus ring / disabled for a clickable row
/// with a caller-supplied stable ID salt.
pub fn paint_row_with_id(
    ui: &mut Ui,
    rect: Rect,
    id_salt: impl Hash + std::fmt::Debug,
    theme: &Theme,
    state: RowState,
) -> Response {
    let id = ui.scope_id().with(("kestrel_row", &id_salt));
    let sense = if state.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let radius = CornerRadius::same(R_SM);
    let pressed = state.enabled && response.is_pointer_button_down_on();
    let hover_t = hover_fade(
        ui,
        ("row_hover", &id_salt),
        state.enabled && response.hovered() && !state.selected && !pressed,
    );
    if pressed || state.selected {
        ui.painter().rect_filled(rect, radius, theme.active);
    } else if hover_t > 0.01 {
        let fill = blend(
            Color32::from_rgba_unmultiplied(theme.hover.r(), theme.hover.g(), theme.hover.b(), 0),
            theme.hover,
            hover_t,
        );
        ui.painter().rect_filled(rect, radius, fill);
    }
    if state.enabled && response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme.accent),
            StrokeKind::Inside,
        );
    }
    response
}

/// Paint hover / selected / pressed / focus ring / disabled for a clickable row.
pub fn paint_row(ui: &mut Ui, rect: Rect, theme: &Theme, state: RowState) -> Response {
    let salt = (rect.min.x.round() as i32, rect.min.y.round() as i32);
    paint_row_with_id(ui, rect, salt, theme, state)
}

/// Paint a square icon button with smooth hover/press/focus states and a stable ID.
pub fn icon_button_with_id(
    ui: &mut Ui,
    rect: Rect,
    id_salt: impl Hash + std::fmt::Debug,
    theme: &Theme,
    icon: Icon,
    enabled: bool,
) -> Response {
    let id = ui.scope_id().with(("kestrel_icon_button", &id_salt));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let radius = CornerRadius::same(R_SM);
    let hover_t = hover_fade(
        ui,
        ("icon_btn_hover", &id_salt),
        enabled && response.hovered(),
    );
    if enabled {
        if response.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, radius, theme.active);
        } else if hover_t > 0.01 {
            let fill = blend(
                Color32::from_rgba_unmultiplied(
                    theme.hover.r(),
                    theme.hover.g(),
                    theme.hover.b(),
                    0,
                ),
                theme.hover,
                hover_t,
            );
            ui.painter().rect_filled(rect, radius, fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, theme.accent),
                StrokeKind::Inside,
            );
        }
    }
    let color = if !enabled {
        theme.text_muted
    } else if response.has_focus() {
        theme.text
    } else {
        blend(theme.text_dim, theme.text, hover_t)
    };
    let glyph = Rect::from_center_size(
        rect.center(),
        Vec2::splat(rect.width().min(rect.height()) * 0.62),
    );
    icons::paint(ui.painter(), glyph, icon, color, 1.3);
    response
}

/// Paint a square icon button with the same interaction states as [`paint_row`].
pub fn icon_button(ui: &mut Ui, rect: Rect, theme: &Theme, icon: Icon, enabled: bool) -> Response {
    let salt = (rect.min.x.round() as i32, rect.min.y.round() as i32, icon);
    icon_button_with_id(ui, rect, salt, theme, icon, enabled)
}

/// Paint a unified action button (Primary, Secondary, Ghost, or Danger) inside `rect`.
#[allow(clippy::too_many_arguments)]
pub fn paint_button(
    ui: &mut Ui,
    rect: Rect,
    id_salt: impl Hash + std::fmt::Debug,
    label: &str,
    font: FontId,
    theme: &Theme,
    variant: ButtonVariant,
    enabled: bool,
) -> Response {
    let id = ui.scope_id().with(("kestrel_btn", &id_salt));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let pressed = enabled && response.is_pointer_button_down_on();
    let hover_t = hover_fade(ui, ("btn_hover", &id_salt), enabled && response.hovered());
    let radius = CornerRadius::same(R_SM);

    let (fill, stroke, text_color) = match variant {
        ButtonVariant::Primary => {
            if !enabled {
                (theme.surface_subtle, Stroke::new(1.0, theme.border), theme.text_muted)
            } else {
                let bg = if pressed {
                    theme.accent_fill_active
                } else {
                    blend(theme.accent_fill, theme.accent_fill_hover, hover_t)
                };
                (bg, Stroke::NONE, theme.accent_text)
            }
        }
        ButtonVariant::Danger => {
            if !enabled {
                (theme.surface_subtle, Stroke::new(1.0, theme.border), theme.text_muted)
            } else {
                let bg = if pressed {
                    theme.danger_fill
                } else {
                    blend(theme.danger_fill, theme.danger_fill_hover, hover_t)
                };
                (bg, Stroke::NONE, theme.danger_text)
            }
        }
        ButtonVariant::Secondary => {
            if !enabled {
                (theme.surface_subtle, Stroke::new(1.0, theme.border), theme.text_muted)
            } else {
                let bg = if pressed {
                    theme.active
                } else {
                    blend(theme.surface_subtle, theme.hover, hover_t)
                };
                let bd = blend(theme.border, theme.border_strong, hover_t);
                (bg, Stroke::new(1.0, bd), theme.text)
            }
        }
        ButtonVariant::Ghost => {
            if !enabled {
                (Color32::TRANSPARENT, Stroke::NONE, theme.text_muted)
            } else {
                let bg = if pressed {
                    theme.active
                } else {
                    blend(
                        Color32::from_rgba_unmultiplied(
                            theme.hover.r(),
                            theme.hover.g(),
                            theme.hover.b(),
                            0,
                        ),
                        theme.hover,
                        hover_t,
                    )
                };
                (bg, Stroke::NONE, blend(theme.text_dim, theme.text, hover_t))
            }
        }
    };

    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, radius, fill);
    }
    if stroke != Stroke::NONE {
        ui.painter()
            .rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    }
    if enabled && response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            CornerRadius::same(R_SM + 1),
            Stroke::new(1.5, theme.accent),
            StrokeKind::Inside,
        );
    }
    let (galley, _) = ellipsize_mode(
        ui,
        label,
        font,
        text_color,
        (rect.width() - 16.0).max(16.0),
        EllipsizeMode::End,
    );
    let pos = Pos2::new(
        rect.center().x - galley.rect.width() * 0.5,
        rect.center().y - galley.rect.height() * 0.5,
    );
    ui.painter().galley(pos, galley, text_color);
    response
}

/// Paint a crisp `<kbd>` shortcut keycap right-aligned to `right_x` and vertically
/// centered at `center_y`. Returns the total pixel width occupied.
pub fn paint_keycap_right(
    ui: &Ui,
    painter: &Painter,
    right_x: f32,
    center_y: f32,
    shortcut: &str,
    font: FontId,
    theme: &Theme,
) -> f32 {
    if shortcut.is_empty() {
        return 0.0;
    }
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_no_wrap(shortcut.to_string(), font, theme.text_dim)
    });
    let w = (galley.rect.width() + 12.0).max(22.0);
    let h = (galley.rect.height() + 5.0).max(18.0);
    let rect = Rect::from_min_size(Pos2::new(right_x - w, center_y - h * 0.5), Vec2::new(w, h));
    painter.rect_filled(rect, CornerRadius::same(R_SM), theme.surface_subtle);
    painter.rect_stroke(
        rect,
        CornerRadius::same(R_SM),
        Stroke::new(1.0, theme.border),
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(
            rect.center().x - galley.rect.width() * 0.5,
            rect.center().y - galley.rect.height() * 0.5,
        ),
        galley,
        theme.text_dim,
    );
    w
}

/// Measure keycap width without painting (useful for reserving right-side space before ellipsizing).
pub fn measure_keycap_width(ui: &Ui, shortcut: &str, font: FontId) -> f32 {
    if shortcut.is_empty() {
        return 0.0;
    }
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_no_wrap(shortcut.to_string(), font, Color32::WHITE)
    });
    (galley.rect.width() + 12.0).max(22.0)
}

/// Cache key for elided display strings. Never caches `Arc<Galley>` across frames
/// because egui's font atlas can grow or repack (e.g. when CJK glyphs are loaded),
/// which invalidates baked glyph UV coordinates inside previously cached galleys.
#[derive(Clone, PartialEq, Eq, Hash)]
struct EllipsizeKey {
    text: String,
    size_bits: u32,
    family: egui::FontFamily,
    max_width_px: i32,
    mode: EllipsizeMode,
    ppp_bits: u32,
}

thread_local! {
    static ELLIPSIZE_CACHE: RefCell<HashMap<EllipsizeKey, (String, bool)>> =
        RefCell::new(HashMap::with_capacity(256));
}

/// Lay out `text`, cutting out the middle with an ellipsis until it fits `max_width`.
///
/// Returns the galley plus whether anything was cut; call sites attach the
/// tooltip so overflow always explains itself.
pub fn ellipsize(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> (Arc<Galley>, bool) {
    ellipsize_mode(ui, text, font, color, max_width, EllipsizeMode::Middle)
}

/// Lay out `text` truncated either at the end (`abc…`) or in the middle (`a…z`)
/// until it fits `max_width`, caching only the elided string so every painted
/// galley uses up-to-date font-atlas UVs.
pub fn ellipsize_mode(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    mode: EllipsizeMode,
) -> (Arc<Galley>, bool) {
    let max_width = max_width.max(4.0);
    let key = EllipsizeKey {
        text: text.to_owned(),
        size_bits: font.size.to_bits(),
        family: font.family.clone(),
        max_width_px: max_width.round() as i32,
        mode,
        ppp_bits: ui.ctx().pixels_per_point().to_bits(),
    };

    let layout = |ui: &mut Ui, s: &str| -> Arc<Galley> {
        let mut job = LayoutJob::simple_singleline(s.to_owned(), font.clone(), color);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = false;
        ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
    };

    let cached = ELLIPSIZE_CACHE.with(|cell| cell.borrow().get(&key).cloned());
    if let Some((elided, cut)) = cached {
        return (layout(ui, &elided), cut);
    }

    let (elided_str, galley, cut) = {
        let full = layout(ui, text);
        if full.rect.width() <= max_width {
            (text.to_owned(), full, false)
        } else {
            let chars: Vec<char> = text.chars().collect();
            let mut lo = 0usize;
            let mut hi = chars.len();
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let candidate = match mode {
                    EllipsizeMode::Middle => {
                        let head = mid.div_ceil(2);
                        let tail = mid - head;
                        middle_elided(&chars, head, tail)
                    }
                    EllipsizeMode::End => end_elided(&chars, mid),
                };
                if layout(ui, &candidate).rect.width() <= max_width {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            if lo == 0 {
                let s = "\u{2026}".to_owned();
                let g = layout(ui, &s);
                (s, g, true)
            } else {
                let final_str = match mode {
                    EllipsizeMode::Middle => {
                        let head = lo.div_ceil(2);
                        let tail = lo - head;
                        middle_elided(&chars, head, tail)
                    }
                    EllipsizeMode::End => end_elided(&chars, lo),
                };
                let g = layout(ui, &final_str);
                (final_str, g, true)
            }
        }
    };

    ELLIPSIZE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, (elided_str, cut));
    });
    (galley, cut)
}

fn middle_elided(chars: &[char], head: usize, tail: usize) -> String {
    let mut out = String::with_capacity(chars.len() + 1);
    out.extend(chars.iter().take(head));
    out.push('\u{2026}');
    out.extend(chars.iter().skip(chars.len() - tail));
    out
}

fn end_elided(chars: &[char], keep: usize) -> String {
    let mut out = String::with_capacity(keep + 1);
    out.extend(chars.iter().take(keep));
    out.push('\u{2026}');
    out
}
