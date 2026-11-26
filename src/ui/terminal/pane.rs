//! Single terminal pane controller: input routing, mouse protocol, stream & Alt-block
//! selection with optional copy-on-select, interactive scrollbar, drag-and-drop,
//! Ctrl+click links, async starting / exited / failed overlays, and context menu.

use std::path::PathBuf;
use std::time::Duration;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Event, ImeEvent, Key, Modifiers, PointerButton,
    Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use crate::app::{DragPayload, KestrelApp};
use crate::icons::{self, Icon};
use crate::terminal::{
    SelectionMode, TerminalLinkTarget, TerminalSession, encode_key, encode_mouse,
};
use crate::ui::UiStyle;
use crate::ui::widgets;

use super::renderer::{
    GridMetrics, cell_at, paint_command_marks, paint_cursor_and_ime, paint_overlays,
    paint_terminal_grid,
};
use super::search_bar::terminal_search_bar_ui;

pub(crate) enum PaneAction {
    None,
    Copy(String),
    Paste,
    SelectAll,
    Search,
    Clear,
    NewTerminal,
    SplitPane,
    Restart,
    StartRename,
    Close,
    OpenUrl(String),
    OpenFile {
        path: PathBuf,
        line: Option<usize>,
        col: Option<usize>,
    },
}

pub(crate) struct TerminalPaneResult {
    pub focused: bool,
    pub interacted: bool,
    pub action: PaneAction,
}

impl Default for TerminalPaneResult {
    fn default() -> Self {
        Self {
            focused: false,
            interacted: false,
            action: PaneAction::None,
        }
    }
}

impl KestrelApp {
    /// Paint and drive one terminal pane inside a single or multi-pane split layout.
    pub(crate) fn terminal_pane_ui(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        idx: usize,
        is_target_pane: bool,
        show_pane_header: bool,
        pane_slot: usize,
    ) -> TerminalPaneResult {
        let theme = style.theme;
        let mut result = TerminalPaneResult::default();
        if idx >= self.bottom.terminals.len() {
            return result;
        }
        let term_id = self.bottom.terminals[idx].id;

        // Keep theme colors synced for OSC 10/11 queries.
        self.bottom.terminals[idx].set_theme_colors(
            (theme.text.r(), theme.text.g(), theme.text.b()),
            (
                theme.terminal_bg.r(),
                theme.terminal_bg.g(),
                theme.terminal_bg.b(),
            ),
        );

        // Mini header strip in multi-pane mode so every pane shows its title and focus bar.
        if show_pane_header {
            let (strip, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
            let pane_active = self.bottom.focused_pane == pane_slot;
            ui.painter().rect_filled(
                strip,
                0.0,
                if pane_active {
                    theme.panel_bg
                } else {
                    theme.terminal_bg
                },
            );
            if pane_active {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        strip.left_top(),
                        Pos2::new(strip.right(), strip.top() + 2.0),
                    ),
                    0.0,
                    theme.accent,
                );
            }
            let title = self.bottom.terminals[idx].title();
            let dir_hint = self.bottom.terminals[idx]
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let caption = if dir_hint.is_empty() {
                format!("{}: {}", idx + 1, title)
            } else {
                format!("{}: {} — {}", idx + 1, title, dir_hint)
            };
            let (galley, _) = widgets::ellipsize(
                ui,
                &caption,
                style.tiny(),
                if pane_active {
                    theme.text
                } else {
                    theme.text_muted
                },
                (strip.width() - 32.0).max(24.0),
            );
            ui.painter().galley(
                Pos2::new(
                    strip.left() + 8.0,
                    strip.center().y - galley.rect.height() * 0.5,
                ),
                galley,
                theme.text,
            );
            let close_r = Rect::from_center_size(
                Pos2::new(strip.right() - 12.0, strip.center().y),
                Vec2::splat(16.0),
            );
            let close_resp = ui.interact(
                close_r,
                egui::Id::unique(("split_pane_close", term_id)),
                Sense::click(),
            );
            if close_resp.hovered() {
                ui.painter()
                    .rect_filled(close_r, CornerRadius::same(3), theme.hover);
            }
            icons::paint(
                ui.painter(),
                close_r.shrink(3.5),
                Icon::Close,
                if close_resp.hovered() {
                    theme.text
                } else {
                    theme.text_muted
                },
                1.1,
            );
            if close_resp.clicked() {
                result.action = PaneAction::Close;
            }
            ui.painter().line_segment(
                [
                    Pos2::new(strip.left(), strip.bottom() - 0.5),
                    Pos2::new(strip.right(), strip.bottom() - 0.5),
                ],
                Stroke::new(1.0, theme.border),
            );
        }

        if self.bottom.terminals[idx].search.open {
            let dismissed = terminal_search_bar_ui(ui, style, &mut self.bottom.terminals[idx]);
            if dismissed {
                self.bottom.terminal_focus = true;
            }
        }

        let body = ui.available_rect_before_wrap();
        if body.width() < 16.0 || body.height() < 16.0 {
            return result;
        }

        // Always reserve the slim scrollbar track so grid column count never jumps
        // and triggers a mid-output PTY resize the first time scrollback appears.
        let sb_track_w = 10.0;
        let grid_rect = Rect::from_min_max(
            body.min,
            Pos2::new(
                (body.right() - sb_track_w).max(body.left() + 12.0),
                body.bottom(),
            ),
        );
        let sb_rect = Rect::from_min_max(Pos2::new(grid_rect.right(), body.top()), body.max);

        let resp = ui.interact(
            grid_rect,
            egui::Id::unique(("kestrel_terminal_grid", term_id)),
            Sense::click_and_drag(),
        );
        ui.painter().rect_filled(body, 0.0, theme.terminal_bg);

        let pressed_inside = resp.hovered() && ui.input(|i| i.pointer.primary_pressed());
        if pressed_inside || resp.clicked() || resp.secondary_clicked() {
            resp.request_focus();
            result.interacted = true;
        }
        if is_target_pane && self.bottom.terminal_focus {
            resp.request_focus();
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus_requested = false;
            }
            self.bottom.terminal_focus = false;
        }
        let focused = resp.has_focus();
        result.focused = focused;
        if focused {
            self.bottom.terminals[idx].clear_unread_bell();
        }

        // Font metrics & grid dimensions (uses dedicated terminal font when configured).
        let term_font_size = self.settings.terminal_font_size.clamp(9.0, 28.0);
        let font = style.term_mono(term_font_size);
        let (cell_w, text_h) = ui.ctx().fonts_mut(|fonts| {
            (
                fonts.glyph_width(&font, 'M').max(1.0),
                fonts.row_height(&font),
            )
        });
        let cell_h = (term_font_size * 1.45).max(12.0);
        let y_text_off = (cell_h - text_h) * 0.5;
        let has_status_banner = self.bottom.terminals[idx].is_exited()
            || self.bottom.terminals[idx].failed_error().is_some();
        let banner_h = if has_status_banner { 30.0 } else { 0.0 };
        let usable_h = (grid_rect.height() - 6.0 - banner_h).max(cell_h);
        // Leave 10px left padding so OSC 133 gutter dots sit cleanly beside column 0.
        let origin = Pos2::new(grid_rect.left() + 10.0, grid_rect.top() + 3.0);
        let cols = ((grid_rect.width() - 16.0) / cell_w).floor().max(2.0) as u16;
        let rows = (usable_h / cell_h).floor().max(2.0) as u16;

        // Ctrl+wheel adjusts terminal font size; regular wheel scrolls history / TUI.
        if resp.hovered() {
            let zoom_step = crate::ui::ctrl_wheel_steps(ui.ctx());
            if zoom_step != 0 {
                self.settings.terminal_font_size =
                    (self.settings.terminal_font_size + zoom_step as f32).clamp(9.0, 28.0);
                self.settings.save();
            } else {
                let (modifiers, smooth_dy, hover_pos) =
                    ui.input(|i| (i.modifiers, i.smooth_scroll_delta.y, i.pointer.hover_pos()));
                if !(modifiers.ctrl || modifiers.command)
                    && smooth_dy.abs() > 0.01
                    && let Some(term) = self.bottom.terminals.get_mut(idx)
                {
                    term.scroll_accum += smooth_dy;
                    let step_px = cell_h.clamp(12.0, 22.0);
                    let lines = (term.scroll_accum / step_px).trunc() as isize;
                    if lines != 0 {
                        term.scroll_accum -= lines as f32 * step_px;
                        let mouse_mode = term.screen().mouse_protocol_mode();
                        let alt_screen = term.screen().alternate_screen();
                        let interactive = !term.is_exited() && term.failed_error().is_none();
                        if !modifiers.shift
                            && mouse_mode != vt100::MouseProtocolMode::None
                            && interactive
                        {
                            let (sr, sc) = term.screen().size();
                            let pos = hover_pos.unwrap_or(origin);
                            let (mr, mc) =
                                cell_at(pos, origin, cell_w, cell_h, sr.get(), sc.get());
                            let btn = if lines > 0 { 64u8 } else { 65u8 };
                            let encoding = term.screen().mouse_protocol_encoding();
                            for _ in 0..lines.unsigned_abs().min(10) {
                                if let Some(bytes) =
                                    encode_mouse(encoding, btn, mc, mr, true, &modifiers)
                                {
                                    term.write_mouse(&bytes);
                                }
                            }
                        } else if alt_screen && interactive {
                            let app_cursor = term.screen().application_cursor();
                            let seq: &[u8] = match (lines > 0, app_cursor) {
                                (true, true) => b"\x1bOA",
                                (true, false) => b"\x1b[A",
                                (false, true) => b"\x1bOB",
                                (false, false) => b"\x1b[B",
                            };
                            for _ in 0..lines.unsigned_abs().min(12) {
                                term.write_mouse(seq);
                            }
                        } else {
                            let max_sb = term.scrollback_len() as isize;
                            let next = (term.scrollback_offset as isize + lines).clamp(0, max_sb)
                                as usize;
                            term.set_scrollback(next);
                        }
                    }
                }
            }
        }

        let copy_on_select = self.settings.terminal_copy_on_select;
        let workspace_root = self.workspace.root.clone();
        let terminal = &mut self.bottom.terminals[idx];
        if !focused {
            terminal.ime_preedit.clear();
        }
        let (cur_rows, cur_cols) = terminal.screen().size();
        if (rows, cols) != (cur_rows.get(), cur_cols.get()) && cols > 0 && rows > 0 {
            terminal.resize(cols, rows);
        }
        if is_target_pane && cols > 0 && rows > 0 {
            self.bottom.terminal_size = (cols, rows);
        }

        let (screen_rows, screen_cols) = {
            let (rows_nz, cols_nz) = terminal.screen().size();
            (rows_nz.get().min(rows), cols_nz.get().min(cols))
        };
        let metrics = GridMetrics {
            origin,
            cell_w,
            cell_h,
            text_h,
            y_text_off,
            screen_rows,
            screen_cols,
        };

        // Ctrl+hover / Ctrl+click links (OSC 8, wrapped URLs, `file:line:col`).
        let mut hovered_link_spans: Vec<(u16, u16, u16)> = Vec::new();
        let mut clicked_link = false;
        if let Some(pos) = resp.hover_pos()
            && ui.input(|i| i.modifiers.ctrl || i.modifiers.command)
        {
            let (row, col) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
            if let Some(hit) = terminal.link_at(row, col, workspace_root.as_deref()) {
                hovered_link_spans = hit.spans;
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                resp.clone().on_hover_text(&hit.label);
                if resp.clicked() {
                    clicked_link = true;
                    result.action = match hit.target {
                        TerminalLinkTarget::Url(url) => PaneAction::OpenUrl(url),
                        TerminalLinkTarget::File { path, line, col } => {
                            PaneAction::OpenFile { path, line, col }
                        }
                    };
                }
            }
        }

        // External OS file drop onto this terminal pane.
        let mut drop_highlight = false;
        if resp.hovered() {
            let (os_hovered, os_dropped): (bool, Vec<PathBuf>) = ui.input(|i| {
                (
                    !i.raw.hovered_files.is_empty(),
                    i.raw
                        .dropped_files
                        .iter()
                        .map(|file| file.path().to_path_buf())
                        .collect(),
                )
            });
            drop_highlight |= os_hovered;
            if !os_dropped.is_empty() {
                let shell = terminal.shell;
                let mut text = String::new();
                for path in &os_dropped {
                    text.push_str(&crate::terminal::quote_path_for_shell(path, shell));
                    text.push(' ');
                }
                terminal.write(text.as_bytes());
                ui.input_mut(|i| i.raw.dropped_files.clear());
                resp.request_focus();
                result.interacted = true;
            }
        }

        // Mouse protocol forwarding vs local text selection (Shift forces local selection;
        // Alt+drag performs rectangular column-block selection).
        let modifiers = ui.input(|i| i.modifiers);
        let mouse_mode = terminal.screen().mouse_protocol_mode();
        let mouse_encoding = terminal.screen().mouse_protocol_encoding();
        let forward_mouse = !modifiers.shift
            && !clicked_link
            && mouse_mode != vt100::MouseProtocolMode::None
            && !terminal.is_exited()
            && terminal.failed_error().is_none();

        if forward_mouse {
            let (press_pos, hover_pos, pointer_events) = ui.input(|i| {
                let evs: Vec<(PointerButton, bool, Pos2)> = i
                    .events
                    .iter()
                    .filter_map(|e| match *e {
                        Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            ..
                        } if grid_rect.contains(pos) => Some((button, pressed, pos)),
                        _ => None,
                    })
                    .collect();
                (i.pointer.interact_pos(), i.pointer.hover_pos(), evs)
            });
            for (button, pressed, pos) in pointer_events {
                let code = match button {
                    PointerButton::Primary => 0u8,
                    PointerButton::Middle => 1u8,
                    PointerButton::Secondary => 2u8,
                    _ => continue,
                };
                if pressed
                    || matches!(
                        mouse_mode,
                        vt100::MouseProtocolMode::PressRelease
                            | vt100::MouseProtocolMode::ButtonMotion
                            | vt100::MouseProtocolMode::AnyMotion
                    )
                {
                    let (mr, mc) =
                        cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                    if let Some(bytes) =
                        encode_mouse(mouse_encoding, code, mc, mr, pressed, &modifiers)
                    {
                        terminal.write_mouse(&bytes);
                    }
                }
            }
            if matches!(mouse_mode, vt100::MouseProtocolMode::ButtonMotion)
                && resp.dragged()
                && let Some(pos) = press_pos
            {
                let (mr, mc) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                if let Some(bytes) = encode_mouse(mouse_encoding, 32, mc, mr, true, &modifiers) {
                    terminal.write_mouse(&bytes);
                }
            } else if matches!(mouse_mode, vt100::MouseProtocolMode::AnyMotion)
                && let Some(pos) = hover_pos
                && grid_rect.contains(pos)
            {
                let delta = ui.input(|i| i.pointer.delta());
                if delta.length_sq() > 0.5 {
                    let (mr, mc) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                    if let Some(bytes) =
                        encode_mouse(mouse_encoding, 35, mc, mr, true, &modifiers)
                    {
                        terminal.write_mouse(&bytes);
                    }
                }
            }
        } else if !clicked_link {
            let mut selection_completed = false;
            if resp.triple_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let (row, _) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                terminal.select_line_at(row);
                selection_completed = true;
            } else if resp.double_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let (row, col) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                terminal.select_word_at(row, col);
                selection_completed = true;
            } else {
                if resp.drag_started_by(PointerButton::Primary)
                    && let Some(pos) = ui
                        .input(|i| i.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                {
                    let (row, col) =
                        cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                    let mode = if modifiers.alt {
                        SelectionMode::Block
                    } else {
                        SelectionMode::Stream
                    };
                    terminal.begin_selection_with_mode(row, col, mode);
                    result.interacted = true;
                }
                if resp.dragged_by(PointerButton::Primary)
                    && let Some(pos) = resp.interact_pointer_pos()
                {
                    if pos.y < origin.y {
                        let max_sb = terminal.scrollback_len();
                        let next = (terminal.scrollback_offset + 1).min(max_sb);
                        terminal.set_scrollback(next);
                        ui.ctx().request_repaint_after(Duration::from_millis(40));
                    } else if pos.y > origin.y + screen_rows as f32 * cell_h
                        && terminal.scrollback_offset > 0
                    {
                        terminal.set_scrollback(terminal.scrollback_offset.saturating_sub(1));
                        ui.ctx().request_repaint_after(Duration::from_millis(40));
                    }
                    let (row, col) =
                        cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                    terminal.update_selection(row, col);
                }
                if resp.drag_stopped_by(PointerButton::Primary) {
                    selection_completed = true;
                }
                if resp.clicked_by(PointerButton::Primary) {
                    terminal.clear_selection();
                }
            }
            if copy_on_select
                && selection_completed
                && let Some(text) = terminal.selection_text()
            {
                ui.ctx().copy_text(text);
            }
        }

        // Right-click context menu.
        if !forward_mouse {
            let has_selection = terminal.selection.is_some();
            let mut copied_text: Option<String> = None;
            resp.context_menu(|ui| {
                let copy_btn = egui::Button::new("复制  (Ctrl+C / Ctrl+Shift+C)");
                if ui.add_enabled(has_selection, copy_btn).clicked() {
                    copied_text = terminal.selection_text();
                    terminal.clear_selection();
                    ui.close();
                }
                if ui.button("粘贴  (Ctrl+V / Ctrl+Shift+V)").clicked() {
                    result.action = PaneAction::Paste;
                    ui.close();
                }
                if ui.button("全选可见内容").clicked() {
                    result.action = PaneAction::SelectAll;
                    ui.close();
                }
                ui.separator();
                if ui.button("查找…  (Ctrl+F)").clicked() {
                    result.action = PaneAction::Search;
                    ui.close();
                }
                if ui.button("清屏").clicked() {
                    result.action = PaneAction::Clear;
                    ui.close();
                }
                ui.separator();
                if ui.button("新建终端").clicked() {
                    result.action = PaneAction::NewTerminal;
                    ui.close();
                }
                if ui.button("向右拆分新终端").clicked() {
                    result.action = PaneAction::SplitPane;
                    ui.close();
                }
                if ui.button("重命名终端…").clicked() {
                    result.action = PaneAction::StartRename;
                    ui.close();
                }
                if ui.button("重启终端").clicked() {
                    result.action = PaneAction::Restart;
                    ui.close();
                }
                if ui.button("关闭当前终端").clicked() {
                    result.action = PaneAction::Close;
                    ui.close();
                }
            });
            if let Some(text) = copied_text {
                result.action = PaneAction::Copy(text);
            }
        }

        // Keyboard input for this pane.
        if focused {
            if terminal.is_exited() || terminal.failed_error().is_some() {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    result.action = PaneAction::Restart;
                }
            } else {
                handle_terminal_keyboard(ui, terminal);
            }
        }

        // Render grid, OSC 133 marks, overlays, scrollbar, cursor, and status banners.
        let painter = ui.painter_at(grid_rect);
        paint_terminal_grid(ui, &painter, style, &font, terminal, metrics);
        paint_command_marks(&painter, theme, terminal, metrics);
        paint_overlays(
            ui,
            &painter,
            grid_rect,
            theme,
            terminal,
            &hovered_link_spans,
            metrics,
        );

        // Interactive scrollbar using real `scrollback_len()`.
        let sb_len = terminal.scrollback_len();
        if sb_len > 0 && sb_rect.width() >= 6.0 && sb_rect.height() >= 20.0 {
            let sb_resp = ui.interact(
                sb_rect,
                egui::Id::unique(("kestrel_terminal_sb", term_id)),
                Sense::click_and_drag(),
            );
            let track_top = sb_rect.top() + 4.0;
            let track_h = (sb_rect.height() - 8.0).max(20.0);
            let total_rows = (sb_len + screen_rows as usize) as f32;
            let thumb_h = ((screen_rows as f32 / total_rows) * track_h).clamp(24.0, track_h);
            let travel = (track_h - thumb_h).max(1.0);

            if (sb_resp.dragged() || sb_resp.clicked())
                && let Some(pos) = sb_resp.interact_pointer_pos()
            {
                let frac = ((pos.y - track_top - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
                let next_offset = ((1.0 - frac) * sb_len as f32).round() as usize;
                terminal.set_scrollback(next_offset);
            }

            let frac =
                (1.0 - (terminal.scrollback_offset as f32 / sb_len as f32)).clamp(0.0, 1.0);
            let thumb_y = track_top + frac * travel;
            let thumb_color = if sb_resp.hovered() || sb_resp.dragged() {
                theme.scrollbar_hover
            } else if terminal.scrollback_offset > 0 {
                theme.scrollbar_hover.gamma_multiply(0.75)
            } else {
                theme.scrollbar.gamma_multiply(0.6)
            };
            ui.painter().rect_filled(
                Rect::from_min_size(
                    Pos2::new(sb_rect.left() + 2.0, thumb_y),
                    Vec2::new(6.0, thumb_h),
                ),
                3.0,
                thumb_color,
            );
        }

        // Clickable scrollback pill.
        if terminal.scrollback_offset > 0 {
            let label = format!("↑{} 行 · 回到底部", terminal.scrollback_offset);
            let pill_w = 116.0;
            let badge = Rect::from_min_size(
                Pos2::new(grid_rect.right() - pill_w - 8.0, grid_rect.top() + 6.0),
                Vec2::new(pill_w, 22.0),
            );
            let badge_resp = ui.interact(
                badge,
                egui::Id::unique(("term_scroll_bottom", term_id)),
                Sense::click(),
            );
            ui.painter().rect_filled(
                badge,
                CornerRadius::same(crate::theme::R_SM),
                if badge_resp.hovered() {
                    theme.hover
                } else {
                    theme.active
                },
            );
            ui.painter().rect_stroke(
                badge,
                CornerRadius::same(crate::theme::R_SM),
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                badge.center(),
                Align2::CENTER_CENTER,
                label,
                style.tiny(),
                theme.accent,
            );
            if badge_resp.clicked() {
                terminal.set_scrollback(0);
            }
        }

        paint_cursor_and_ime(ui, &painter, grid_rect, theme, &font, terminal, focused, metrics);

        // Starting placeholder overlay (non-blocking async spawn).
        if terminal.is_starting() {
            let msg = format!("正在启动 {}…", terminal.title());
            painter.text(
                Pos2::new(origin.x, origin.y + 4.0),
                Align2::LEFT_TOP,
                msg,
                style.small(),
                theme.text_muted,
            );
        }

        // Exited / Failed bottom status banner.
        if has_status_banner {
            let banner = Rect::from_min_max(
                Pos2::new(grid_rect.left(), grid_rect.bottom() - banner_h),
                grid_rect.max,
            );
            ui.painter().rect_filled(banner, 0.0, theme.panel_bg);
            ui.painter().line_segment(
                [banner.left_top(), banner.right_top()],
                Stroke::new(1.0, theme.border),
            );
            let (msg, msg_color) = if let Some(err) = terminal.failed_error() {
                (format!("启动失败: {err}"), theme.error)
            } else {
                let text = match terminal.exit_code() {
                    Some(code) => format!("终端进程已退出（退出码 {code}）"),
                    None => "终端进程已退出".to_string(),
                };
                (text, theme.warning)
            };
            let (g, _) = widgets::ellipsize(
                ui,
                &msg,
                style.tiny(),
                msg_color,
                (banner.width() - 170.0).max(40.0),
            );
            ui.painter().galley(
                Pos2::new(banner.left() + 10.0, banner.center().y - g.rect.height() * 0.5),
                g,
                msg_color,
            );
            let close_btn = Rect::from_center_size(
                Pos2::new(banner.right() - 34.0, banner.center().y),
                Vec2::new(52.0, 20.0),
            );
            if widgets::paint_button(
                ui,
                close_btn,
                ("term_exit_close", term_id),
                "关闭",
                style.tiny(),
                &theme,
                widgets::ButtonVariant::Secondary,
                true,
            )
            .clicked()
            {
                result.action = PaneAction::Close;
            }
            let restart_btn = Rect::from_center_size(
                Pos2::new(close_btn.left() - 52.0, banner.center().y),
                Vec2::new(88.0, 20.0),
            );
            if widgets::paint_button(
                ui,
                restart_btn,
                ("term_exit_restart", term_id),
                "重启 (Enter)",
                style.tiny(),
                &theme,
                widgets::ButtonVariant::Primary,
                true,
            )
            .clicked()
            {
                result.action = PaneAction::Restart;
            }
        }

        // Internal Explorer drag-and-drop into this terminal pane.
        if let Some(DragPayload::ExplorerPath(drag_path)) = self.drag_payload.clone()
            && grid_rect.contains(
                ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()))
                    .unwrap_or(Pos2::new(-1.0, -1.0)),
            )
        {
            drop_highlight = true;
            let primary_down = ui.input(|i| i.pointer.primary_down());
            if !primary_down {
                let shell = self.bottom.terminals[idx].shell;
                let quoted = crate::terminal::quote_path_for_shell(&drag_path, shell);
                self.bottom.terminals[idx].write(format!("{quoted} ").as_bytes());
                self.drag_payload = None;
                resp.request_focus();
                result.interacted = true;
            }
        }

        if drop_highlight {
            ui.painter().rect_filled(
                grid_rect,
                0.0,
                Color32::from_rgba_unmultiplied(
                    theme.accent.r(),
                    theme.accent.g(),
                    theme.accent.b(),
                    28,
                ),
            );
            ui.painter().rect_stroke(
                grid_rect.shrink(1.0),
                2.0,
                Stroke::new(1.5, theme.accent),
                egui::StrokeKind::Inside,
            );
            let pill = Rect::from_center_size(grid_rect.center(), Vec2::new(168.0, 26.0));
            ui.painter().rect_filled(pill, 6.0, theme.overlay_bg);
            ui.painter().rect_stroke(
                pill,
                6.0,
                Stroke::new(1.0, theme.accent),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                "松开以插入文件路径",
                style.tiny(),
                theme.text,
            );
        }

        result
    }
}

fn handle_terminal_keyboard(ui: &Ui, terminal: &mut TerminalSession) {
    let ctx = ui.ctx();
    let events = ctx.input(|i| i.events.clone());
    let has_paste_event = events.iter().any(|e| matches!(e, Event::Paste(_)));
    let has_text_event = events.iter().any(|e| matches!(e, Event::Text(_)));

    for event in events {
        match event {
            Event::Text(text) => {
                terminal.ime_preedit.clear();
                terminal.write(text.as_bytes());
            }
            Event::Paste(text) => {
                terminal.write_paste(&text);
            }
            Event::Copy => {
                if let Some(text) = terminal.selection_text() {
                    ctx.copy_text(text);
                    terminal.clear_selection();
                }
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => {
                terminal.ime_preedit = text;
            }
            Event::Ime(ImeEvent::Commit(text)) => {
                terminal.ime_preedit.clear();
                if !text.is_empty() && text.bytes().any(|b| b != 10 && b != 13) {
                    terminal.write(text.as_bytes());
                }
            }
            Event::Ime(_) => {
                terminal.ime_preedit.clear();
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if !terminal.ime_preedit.is_empty() {
                    if key == Key::Escape {
                        terminal.ime_preedit.clear();
                    }
                    continue;
                }

                let command = modifiers.ctrl || modifiers.command;

                // Shift+PageUp/PageDown/Home/End scroll terminal history.
                if modifiers.shift && !command && !modifiers.alt {
                    let screen_rows = terminal.screen().size().0.get() as usize;
                    let page = (screen_rows / 2).max(1);
                    let max_sb = terminal.scrollback_len();
                    match key {
                        Key::PageUp => {
                            terminal.set_scrollback(
                                terminal.scrollback_offset.saturating_add(page).min(max_sb),
                            );
                            continue;
                        }
                        Key::PageDown => {
                            terminal.set_scrollback(terminal.scrollback_offset.saturating_sub(page));
                            continue;
                        }
                        Key::Home => {
                            terminal.set_scrollback(max_sb);
                            continue;
                        }
                        Key::End => {
                            terminal.set_scrollback(0);
                            continue;
                        }
                        _ => {}
                    }
                }

                // Ctrl+Shift+Up/Down jump between OSC 133/633 Shell Integration command marks.
                if command && modifiers.shift && !modifiers.alt {
                    match key {
                        Key::ArrowUp if terminal.jump_command_mark(false) => continue,
                        Key::ArrowDown if terminal.jump_command_mark(true) => continue,
                        _ => {}
                    }
                }

                // Copy / Paste / Search shortcuts.
                if command && !modifiers.alt {
                    if modifiers.shift {
                        match key {
                            Key::C => {
                                if let Some(text) = terminal.selection_text() {
                                    ctx.copy_text(text);
                                    terminal.clear_selection();
                                }
                                continue;
                            }
                            Key::V => {
                                if !has_paste_event
                                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                                    && let Ok(text) = clipboard.get_text()
                                    && !text.is_empty()
                                {
                                    terminal.write_paste(&text);
                                }
                                continue;
                            }
                            _ => {}
                        }
                    } else {
                        match key {
                            Key::C if terminal.selection.is_some() => {
                                if let Some(text) = terminal.selection_text() {
                                    ctx.copy_text(text);
                                    terminal.clear_selection();
                                    continue;
                                }
                            }
                            Key::V => {
                                if !has_paste_event
                                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                                    && let Ok(text) = clipboard.get_text()
                                    && !text.is_empty()
                                {
                                    terminal.write_paste(&text);
                                }
                                continue;
                            }
                            Key::F => {
                                terminal.open_search();
                                continue;
                            }
                            _ => {}
                        }
                    }
                }

                let proto = terminal.keyboard_protocol();
                if let Some(bytes) = encode_key(key, &modifiers, proto, has_text_event) {
                    terminal.write(&bytes);
                }
            }
            _ => {}
        }
    }
}
