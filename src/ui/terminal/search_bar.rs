//! Ctrl+F terminal search bar UI component.

use std::time::Duration;

use egui::{Align2, CornerRadius, Key, Modifiers, Pos2, Rect, Sense, Stroke, TextEdit, Ui, Vec2};

use crate::icons::{self, Icon};
use crate::terminal::TerminalSession;
use crate::ui::UiStyle;

/// Render the search bar above a terminal grid. Returns `true` if the user closed
/// the search bar and focus should return to the terminal grid.
pub(crate) fn terminal_search_bar_ui(
    ui: &mut Ui,
    style: &UiStyle,
    terminal: &mut TerminalSession,
) -> bool {
    let theme = style.theme;
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, theme.panel_bg);
    ui.painter().line_segment(
        [
            Pos2::new(bar.left(), bar.bottom() - 0.5),
            Pos2::new(bar.right(), bar.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme.border),
    );

    let term_id = terminal.id;
    let input_w = (bar.width() * 0.38).clamp(130.0, 300.0);
    let input_rect = Rect::from_min_size(
        Pos2::new(bar.left() + 10.0, bar.top() + 4.0),
        Vec2::new(input_w, 22.0),
    );
    let response = ui.put(
        input_rect,
        TextEdit::singleline(&mut terminal.search.query)
            .id(egui::Id::unique(("terminal_search", term_id)))
            .hint_text("搜索终端输出")
            .font(style.small())
            .margin(Vec2::new(8.0, 3.0)),
    );
    if terminal.search.focus {
        response.request_focus();
        terminal.search.focus = false;
    }
    if terminal.search.regex_error.is_some() {
        ui.painter().rect_stroke(
            input_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, theme.error),
            egui::StrokeKind::Inside,
        );
        if let Some(err) = &terminal.search.regex_error {
            response
                .clone()
                .on_hover_text(format!("正则表达式无效: {err}"));
        }
    }

    let mut close = false;
    let mut forward = false;
    let mut backward = false;

    // Consume navigation keys ONLY while the search input itself owns focus.
    if response.has_focus() {
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            close = true;
        } else if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter)) {
            backward = true;
        } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            forward = true;
        }
    }

    let mut bx = input_rect.right() + 6.0;
    let mut small_btn = |icon: Icon, tip: &str, active: bool, id_salt: &str| -> bool {
        let r = Rect::from_min_size(Pos2::new(bx, bar.top() + 4.0), Vec2::splat(22.0));
        bx += 24.0;
        let resp = ui.interact(
            r,
            egui::Id::unique(("term_search_btn", term_id, id_salt)),
            Sense::click(),
        );
        if active {
            ui.painter()
                .rect_filled(r, CornerRadius::same(4), theme.active);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, CornerRadius::same(4), theme.hover);
        }
        icons::paint(
            ui.painter(),
            r.shrink(4.0),
            icon,
            if active {
                theme.accent
            } else if resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
            1.2,
        );
        resp.on_hover_text(tip).clicked()
    };

    let mut options_changed = false;
    if small_btn(
        Icon::CaseSensitive,
        "区分大小写",
        terminal.search.case_sensitive,
        "case",
    ) {
        terminal.search.case_sensitive = !terminal.search.case_sensitive;
        options_changed = true;
    }
    if small_btn(
        Icon::Regex,
        "使用正则表达式",
        terminal.search.use_regex,
        "regex",
    ) {
        terminal.search.use_regex = !terminal.search.use_regex;
        options_changed = true;
    }
    if small_btn(Icon::ArrowUp, "上一个匹配 (Shift+Enter)", false, "prev") {
        backward = true;
    }
    if small_btn(Icon::ArrowDown, "下一个匹配 (Enter)", false, "next") {
        forward = true;
    }

    let close_r = Rect::from_center_size(
        Pos2::new(bar.right() - 16.0, bar.center().y),
        Vec2::splat(22.0),
    );
    let close_resp = ui.interact(
        close_r,
        egui::Id::unique(("term_search_close", term_id)),
        Sense::click(),
    );
    if close_resp.hovered() {
        ui.painter()
            .rect_filled(close_r, CornerRadius::same(4), theme.hover);
    }
    icons::paint(
        ui.painter(),
        close_r.shrink(4.0),
        Icon::Close,
        if close_resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        },
        1.2,
    );
    if close_resp.on_hover_text("关闭搜索 (Esc)").clicked() {
        close = true;
    }

    if close {
        terminal.close_search();
        return true;
    }

    let query_changed = response.changed() || options_changed;
    if query_changed {
        terminal.search.dirty = true;
        terminal.search.active = None;
        terminal.search.last_refresh = None;
    }
    terminal.refresh_search();
    if query_changed {
        terminal.scroll_to_active_match();
    }
    if terminal.search.dirty {
        ui.ctx().request_repaint_after(Duration::from_millis(160));
    }

    let counter = if terminal.search.query.is_empty() {
        String::new()
    } else if terminal.search.regex_error.is_some() {
        "正则错误".to_string()
    } else if terminal.search.matches.is_empty() {
        "0 项".to_string()
    } else if terminal.search.truncated {
        format!("{}+ 项", terminal.search.matches.len())
    } else {
        match terminal.search.active {
            Some(index) => format!("{}/{} 项", index + 1, terminal.search.matches.len()),
            None => format!("{} 项", terminal.search.matches.len()),
        }
    };
    if !counter.is_empty() && bx + 40.0 < close_r.left() {
        ui.painter().text(
            Pos2::new(bx + 6.0, bar.center().y),
            Align2::LEFT_CENTER,
            &counter,
            style.tiny(),
            if terminal.search.regex_error.is_some() {
                theme.error
            } else if terminal.search.truncated {
                theme.warning
            } else {
                theme.text_dim
            },
        );
    }
    if forward {
        terminal.search_step(true);
    } else if backward {
        terminal.search_step(false);
    }
    false
}
