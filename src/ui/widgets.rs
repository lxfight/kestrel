//! Shared drawing primitives for hand-painted rows, icon buttons and clipped text.
//!
//! Every list-ish surface (file tree, tabs, palette rows, status bar hit targets)
//! paints through these three so hover / selected / pressed / focus / disabled are
//! resolved once instead of per call site.

use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{
    Color32, CornerRadius, FontId, Galley, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
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

/// Paint hover / selected / pressed / focus ring / disabled for a clickable row.
pub fn paint_row(ui: &mut Ui, rect: Rect, theme: &Theme, state: RowState) -> Response {
    let id = ui
        .scope_id()
        .with(("kestrel_row", rect.min.x.to_bits(), rect.min.y.to_bits()));
    let sense = if state.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let radius = CornerRadius::same(R_SM);
    let pressed = state.enabled && response.is_pointer_button_down_on();
    let fill = if pressed || state.selected {
        Some(theme.active)
    } else if state.enabled && response.hovered() {
        Some(theme.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
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

/// Paint a square icon button with the same interaction states as [`paint_row`].
pub fn icon_button(ui: &mut Ui, rect: Rect, theme: &Theme, icon: Icon, enabled: bool) -> Response {
    let id = ui.scope_id().with((
        "kestrel_icon_button",
        rect.min.x.to_bits(),
        rect.min.y.to_bits(),
    ));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let radius = CornerRadius::same(R_SM);
    if enabled {
        if response.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, radius, theme.active);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, radius, theme.hover);
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
        theme.text_faint
    } else if response.hovered() || response.has_focus() {
        theme.text
    } else {
        theme.text_dim
    };
    let glyph = Rect::from_center_size(
        rect.center(),
        Vec2::splat(rect.width().min(rect.height()) * 0.62),
    );
    icons::paint(ui.painter(), glyph, icon, color, 1.3);
    response
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
    let layout = |ui: &mut Ui, s: &str| -> Arc<Galley> {
        let mut job = LayoutJob::simple_singleline(s.to_owned(), font.clone(), color);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = false;
        ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
    };

    let full = layout(ui, text);
    if full.rect.width() <= max_width {
        return (full, false);
    }

    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let head = mid.div_ceil(2);
        let tail = mid - head;
        if layout(ui, &middle_elided(&chars, head, tail)).rect.width() <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return (layout(ui, "\u{2026}"), true);
    }
    let head = lo.div_ceil(2);
    let tail = lo - head;
    (layout(ui, &middle_elided(&chars, head, tail)), true)
}

fn middle_elided(chars: &[char], head: usize, tail: usize) -> String {
    let mut out = String::with_capacity(chars.len() + 1);
    out.extend(chars.iter().take(head));
    out.push('\u{2026}');
    out.extend(chars.iter().skip(chars.len() - tail));
    out
}
