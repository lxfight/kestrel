//! Bottom workbench panel: Integrated multi-session & N-pane split Terminal,
//! Problems (Diagnostics), and Output logs.

pub mod pane;
pub mod renderer;
pub mod search_bar;

use std::path::PathBuf;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Modifiers, Pos2, Rect, ScrollArea,
    Sense, Stroke, TextEdit, Ui, UiBuilder, Vec2,
};

use crate::app::{BottomTab, KestrelApp};
use crate::icons::{self, Icon};
use crate::lsp::DiagSeverity;
use crate::terminal::ShellKind;
use crate::ui::UiStyle;
use crate::ui::widgets;

use pane::PaneAction;

enum TabAction {
    Focus(usize),
    Close(usize),
    CloseOthers(usize),
    Restart(usize),
    StartRename(u64, String),
    AddToSplit(usize),
}

impl KestrelApp {
    pub fn terminal_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        // Recomputed below when a grid owns keyboard focus; global shortcuts read
        // the previous frame's value so keys the shell owns are not stolen.
        self.bottom.terminal_has_focus = false;

        let (header, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), crate::theme::SP6),
            Sense::hover(),
        );
        ui.painter().rect_filled(header, 0.0, theme.panel_bg);
        ui.painter().line_segment(
            [
                Pos2::new(header.left(), header.top() + 0.5),
                Pos2::new(header.right(), header.top() + 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        let total_problems: usize = self.tabs.iter().map(|t| t.diagnostics.len()).sum();
        let term_count = self.bottom.terminals.len();

        let actions_reserve = if self.bottom.tab == BottomTab::Terminal {
            246.0
        } else {
            96.0
        };
        let actions_left = (header.right() - actions_reserve).max(header.left() + 180.0);
        let mut tab_x = header.left() + crate::theme::SP3;
        let mut draw_tab = |ui: &mut Ui, tab: BottomTab, label: &str, badge: Option<usize>| {
            let active = self.bottom.tab == tab;
            let text_w = label.chars().count() as f32 * 11.0 + 16.0;
            let badge_w = if badge.is_some_and(|b| b > 0) {
                22.0
            } else {
                0.0
            };
            let w = text_w + badge_w;
            if tab_x + w > actions_left {
                return;
            }
            let r = Rect::from_min_size(Pos2::new(tab_x, header.top()), Vec2::new(w, header.height()));
            let resp = widgets::paint_row(
                ui,
                r.shrink2(Vec2::new(0.0, crate::theme::SP1)),
                &theme,
                widgets::RowState::new(active, true),
            );
            ui.painter().text(
                Pos2::new(r.left() + 6.0, r.center().y),
                Align2::LEFT_CENTER,
                label,
                style.font(crate::theme::T_SMALL),
                if active {
                    theme.text
                } else if resp.hovered() {
                    theme.text_dim
                } else {
                    theme.text_faint
                },
            );
            if let Some(count) = badge
                && count > 0
            {
                let pill = Rect::from_center_size(
                    Pos2::new(r.right() - 12.0, r.center().y),
                    Vec2::new(18.0, 14.0),
                );
                ui.painter().rect_filled(
                    pill,
                    CornerRadius::same(7),
                    if tab == BottomTab::Problems {
                        theme.error.gamma_multiply(0.25)
                    } else {
                        theme.active
                    },
                );
                ui.painter().text(
                    pill.center(),
                    Align2::CENTER_CENTER,
                    count.to_string(),
                    style.tiny(),
                    if tab == BottomTab::Problems {
                        theme.error
                    } else {
                        theme.text_dim
                    },
                );
            }
            if active {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 4.0, r.bottom() - 2.0),
                        Pos2::new(r.right() - 4.0, r.bottom()),
                    ),
                    1.0,
                    theme.accent,
                );
            }
            tab_x += w + crate::theme::SP2;
            if resp.clicked() {
                self.bottom.tab = tab;
                if tab == BottomTab::Terminal {
                    self.bottom.terminal_focus = true;
                }
            }
        };

        draw_tab(ui, BottomTab::Terminal, "终端", Some(term_count));
        draw_tab(ui, BottomTab::Problems, "问题", Some(total_problems));
        draw_tab(ui, BottomTab::Output, "输出", None);

        // Terminal session pills (visible in single & N-pane split layouts).
        let mut tab_action: Option<TabAction> = None;
        let mut commit_rename: Option<(u64, String)> = None;
        let mut cancel_rename = false;

        if self.bottom.tab == BottomTab::Terminal && !self.bottom.terminals.is_empty() {
            self.bottom.normalize_panes();
            tab_x += crate::theme::SP2;
            let len = self.bottom.terminals.len();
            let focused_idx = self.bottom.focused_terminal_idx();
            let is_split = self.bottom.is_split();
            let visible_panes = self.bottom.panes.clone();

            for idx in 0..len {
                let term_id = self.bottom.terminals[idx].id;
                let exited = self.bottom.terminals[idx].is_exited()
                    || self.bottom.terminals[idx].failed_error().is_some();
                let unread_bell = self.bottom.terminals[idx].has_unread_bell();
                let title = self.bottom.terminals[idx].title();
                let is_focused_tab = focused_idx == Some(idx);
                let pane_pos = visible_panes.iter().position(|&p| p == idx);

                let is_renaming = self
                    .bottom
                    .renaming_terminal
                    .as_ref()
                    .is_some_and(|(id, _, _)| *id == term_id);

                if is_renaming {
                    let w = 124.0_f32.min((actions_left - tab_x).max(60.0));
                    if tab_x + 48.0 > actions_left {
                        break;
                    }
                    let pill = Rect::from_center_size(
                        Pos2::new(tab_x + w * 0.5, header.center().y),
                        Vec2::new(w, 22.0),
                    );
                    if let Some((_, draft, needs_focus)) = self.bottom.renaming_terminal.as_mut() {
                        let resp = ui.put(
                            pill,
                            TextEdit::singleline(draft)
                                .id(egui::Id::unique(("term_rename", term_id)))
                                .font(style.tiny())
                                .margin(Vec2::new(6.0, 2.0)),
                        );
                        if *needs_focus {
                            resp.request_focus();
                            *needs_focus = false;
                        }
                        let enter = resp.has_focus()
                            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                        let esc = resp.has_focus()
                            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                        if enter || resp.lost_focus() {
                            commit_rename = Some((term_id, draft.clone()));
                        } else if esc {
                            cancel_rename = true;
                        }
                    }
                    tab_x += w + crate::theme::SP1;
                    continue;
                }

                let label = if is_split
                    && let Some(slot) = pane_pos
                {
                    format!("P{}·{}: {}", slot + 1, idx + 1, title)
                } else {
                    format!("{}: {}", idx + 1, title)
                };
                let text_w = ui.ctx().fonts_mut(|f| {
                    f.layout_no_wrap(label.clone(), style.tiny(), theme.text)
                        .rect
                        .width()
                });
                let w = (text_w + 32.0).clamp(68.0, 156.0);
                if tab_x + w > actions_left {
                    break;
                }
                let pill = Rect::from_center_size(
                    Pos2::new(tab_x + w * 0.5, header.center().y),
                    Vec2::new(w, 22.0),
                );
                let resp = ui.interact(
                    pill,
                    egui::Id::unique(("term_tab_pill", term_id)),
                    Sense::click(),
                );
                let hovered = resp.hovered();
                let bg = if is_focused_tab {
                    theme.active
                } else if is_split && pane_pos.is_some() {
                    theme.hover.gamma_multiply(0.85)
                } else if hovered {
                    theme.hover
                } else {
                    Color32::TRANSPARENT
                };
                if bg != Color32::TRANSPARENT {
                    ui.painter().rect_filled(pill, CornerRadius::same(5), bg);
                }
                if is_focused_tab {
                    ui.painter().rect_filled(
                        Rect::from_min_max(
                            Pos2::new(pill.left() + 6.0, pill.bottom() - 2.0),
                            Pos2::new(pill.right() - 6.0, pill.bottom()),
                        ),
                        1.0,
                        theme.accent,
                    );
                }

                let has_dot = exited || (unread_bell && !is_focused_tab);
                if has_dot {
                    let dot_color = if exited { theme.warning } else { theme.accent };
                    ui.painter().circle_filled(
                        Pos2::new(pill.left() + 8.0, pill.center().y),
                        3.0,
                        dot_color,
                    );
                }

                let close_rect = Rect::from_center_size(
                    Pos2::new(pill.right() - 10.0, pill.center().y),
                    Vec2::splat(14.0),
                );
                let close_resp = ui.interact(
                    close_rect,
                    egui::Id::unique(("term_tab_close", term_id)),
                    Sense::click(),
                );
                let show_close = hovered || close_resp.hovered() || is_focused_tab;
                let left_pad = if has_dot { 15.0 } else { 8.0 };
                let right_pad = if show_close { 20.0 } else { 8.0 };
                let text_color = if exited {
                    theme.text_muted
                } else if is_focused_tab {
                    theme.text
                } else {
                    theme.text_dim
                };
                let (galley, cut) = widgets::ellipsize(
                    ui,
                    &label,
                    style.tiny(),
                    text_color,
                    (pill.width() - left_pad - right_pad).max(16.0),
                );
                ui.painter().galley(
                    Pos2::new(
                        pill.left() + left_pad,
                        pill.center().y - galley.rect.height() * 0.5,
                    ),
                    galley,
                    text_color,
                );
                if show_close {
                    if close_resp.hovered() {
                        ui.painter()
                            .rect_filled(close_rect, CornerRadius::same(3), theme.hover);
                    }
                    icons::paint(
                        ui.painter(),
                        close_rect.shrink(3.0),
                        Icon::Close,
                        if close_resp.hovered() {
                            theme.text
                        } else {
                            theme.text_muted
                        },
                        1.1,
                    );
                }
                if close_resp.clicked() || resp.middle_clicked() {
                    tab_action = Some(TabAction::Close(idx));
                } else if resp.clicked() {
                    tab_action = Some(TabAction::Focus(idx));
                }

                if exited {
                    resp.clone()
                        .on_hover_text(format!("{}: {} (已退出) — 中键关闭，右键更多操作", idx + 1, title));
                } else if unread_bell && !is_focused_tab {
                    resp.clone()
                        .on_hover_text(format!("{}: {} (收到响铃提示)", idx + 1, title));
                } else if cut {
                    resp.clone().on_hover_text(format!("{}: {}", idx + 1, title));
                }

                resp.context_menu(|ui| {
                    if ui.button("重命名终端…").clicked() {
                        tab_action = Some(TabAction::StartRename(term_id, title.clone()));
                        ui.close();
                    }
                    if ui.button("重启终端").clicked() {
                        tab_action = Some(TabAction::Restart(idx));
                        ui.close();
                    }
                    if pane_pos.is_none() && visible_panes.len() < 4 && ui.button("并排添加到分屏").clicked() {
                        tab_action = Some(TabAction::AddToSplit(idx));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("关闭此终端").clicked() {
                        tab_action = Some(TabAction::Close(idx));
                        ui.close();
                    }
                    if len > 1 && ui.button("关闭其他终端").clicked() {
                        tab_action = Some(TabAction::CloseOthers(idx));
                        ui.close();
                    }
                });

                tab_x += w + crate::theme::SP1;
            }
        }

        if let Some((term_id, name)) = commit_rename {
            if let Some(term) = self.bottom.terminals.iter_mut().find(|t| t.id == term_id) {
                let trimmed = name.trim();
                term.custom_title = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            self.bottom.renaming_terminal = None;
            self.bottom.terminal_focus = true;
        } else if cancel_rename {
            self.bottom.renaming_terminal = None;
            self.bottom.terminal_focus = true;
        }

        if let Some(action) = tab_action {
            match action {
                TabAction::Focus(idx) => {
                    self.focus_terminal_index(idx);
                }
                TabAction::Close(idx) => {
                    self.close_terminal_at(idx);
                }
                TabAction::CloseOthers(keep_idx) => {
                    if let Some(keep_id) = self.bottom.terminals.get(keep_idx).map(|t| t.id) {
                        self.bottom.terminals.retain(|t| t.id == keep_id);
                        self.bottom.panes = vec![0];
                        self.bottom.pane_weights = vec![1.0];
                        self.bottom.focused_pane = 0;
                        self.bottom.active_terminal = 0;
                        self.bottom.terminal_focus = true;
                    }
                }
                TabAction::Restart(idx) => {
                    if let Some(term) = self.bottom.terminals.get_mut(idx) {
                        term.restart();
                    }
                    self.bottom.terminal_focus = true;
                }
                TabAction::StartRename(term_id, cur_title) => {
                    self.bottom.renaming_terminal = Some((term_id, cur_title, true));
                }
                TabAction::AddToSplit(idx) => {
                    self.add_terminal_to_split(idx);
                }
            }
        }

        // Right-side header actions.
        let mut new_shell: Option<ShellKind> = None;
        let mut split_add = false;
        let mut toggle_follow = false;
        let mut toggle_search = false;
        let mut toggle_maximize = false;
        let mut kill_term = false;
        let mut clear = false;
        let mut close_panel = false;

        let mut x = header.right() - 8.0;
        let action =
            |ui: &mut Ui, x: &mut f32, icon: Icon, tip: &str, active: bool| -> egui::Response {
                *x -= 24.0;
                let r = Rect::from_center_size(
                    Pos2::new(*x + 11.0, header.center().y),
                    Vec2::splat(22.0),
                );
                let resp =
                    ui.interact(r, ui.scope_id().with(("term_action", tip)), Sense::click());
                if active {
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(5), theme.active);
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(5), theme.hover);
                }
                icons::paint(
                    ui.painter(),
                    r.shrink(4.0),
                    icon,
                    if active || resp.hovered() {
                        theme.text
                    } else {
                        theme.text_dim
                    },
                    1.3,
                );
                resp.clone().on_hover_text(tip);
                resp
            };

        if action(ui, &mut x, Icon::Close, "隐藏底部面板 (Ctrl+J)", false).clicked() {
            close_panel = true;
        }
        let max_icon = if self.bottom.maximized {
            Icon::ChevronDown
        } else {
            Icon::ChevronUp
        };
        let max_tip = if self.bottom.maximized {
            "还原底部面板高度"
        } else {
            "最大化底部面板"
        };
        if action(ui, &mut x, max_icon, max_tip, self.bottom.maximized).clicked() {
            toggle_maximize = true;
        }

        x -= 6.0;
        ui.painter().line_segment(
            [
                Pos2::new(x + 2.0, header.top() + 8.0),
                Pos2::new(x + 2.0, header.bottom() - 8.0),
            ],
            Stroke::new(1.0, theme.border),
        );
        x -= 4.0;

        if self.bottom.tab == BottomTab::Terminal {
            if action(ui, &mut x, Icon::Trash, "关闭当前终端会话", false).clicked() {
                kill_term = true;
            }
            if action(ui, &mut x, Icon::ClearAll, "清屏并清空历史", false).clicked() {
                clear = true;
            }
            let search_open = self
                .bottom
                .focused_terminal_idx()
                .and_then(|i| self.bottom.terminals.get(i))
                .is_some_and(|t| t.search.open);
            if action(ui, &mut x, Icon::Search, "搜索终端输出 (Ctrl+F)", search_open).clicked() {
                toggle_search = true;
            }
            let split_tip = if self.bottom.is_split() {
                format!(
                    "向右拆分终端（当前 {} 窗格，上限 4；右键取消分屏）",
                    self.bottom.panes.len()
                )
            } else {
                "拆分终端窗格".to_string()
            };
            let split_resp = action(
                ui,
                &mut x,
                Icon::SplitHorizontal,
                &split_tip,
                self.bottom.is_split(),
            );
            if split_resp.clicked() {
                split_add = true;
            }
            if split_resp.secondary_clicked() && self.bottom.is_split() {
                self.unsplit_terminals();
            }
            if action(
                ui,
                &mut x,
                Icon::FolderOpen,
                "新终端跟随活动文件目录",
                self.bottom.follow_active_dir,
            )
            .clicked()
            {
                toggle_follow = true;
            }

            // Split-button: chevron picks any shell profile (including Custom), `+` spawns default.
            x -= 18.0;
            let drop_r = Rect::from_center_size(
                Pos2::new(x + 8.0, header.center().y),
                Vec2::new(16.0, 22.0),
            );
            let drop_resp = ui.interact(
                drop_r,
                ui.scope_id().with("term_shell_dropdown"),
                Sense::click(),
            );
            if drop_resp.hovered() {
                ui.painter()
                    .rect_filled(drop_r, CornerRadius::same(5), theme.hover);
            }
            icons::paint(
                ui.painter(),
                drop_r.shrink2(Vec2::new(2.0, 5.0)),
                Icon::ChevronDown,
                if drop_resp.hovered() {
                    theme.text
                } else {
                    theme.text_dim
                },
                1.2,
            );
            drop_resp.clone().on_hover_text("选择 Shell 类型新建终端");
            let has_custom = !self.settings.terminal_custom_shell_path.trim().is_empty();
            egui::Popup::menu(&drop_resp).width(164.0).show(|ui| {
                for profile in crate::terminal::available_profiles() {
                    if ui.button(profile.label).clicked() {
                        new_shell = Some(profile.kind);
                    }
                }
                if has_custom && ui.button(ShellKind::Custom.label()).clicked() {
                    new_shell = Some(ShellKind::Custom);
                }
            });

            if action(ui, &mut x, Icon::Plus, "新建默认终端 (Ctrl+Shift+`)", false).clicked() {
                new_shell = Some(ShellKind::Default);
            }
        } else if self.bottom.tab == BottomTab::Output
            && action(ui, &mut x, Icon::ClearAll, "清空输出日志", false).clicked()
        {
            self.git.logs.clear();
            self.plugins.clear_logs();
        }

        ui.painter().line_segment(
            [
                Pos2::new(header.left(), header.bottom() - 0.5),
                Pos2::new(header.right(), header.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        if close_panel {
            self.bottom.visible = false;
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus_requested = true;
            }
            return;
        }
        if toggle_maximize {
            self.bottom.maximized = !self.bottom.maximized;
        }
        if let Some(shell) = new_shell {
            self.new_terminal_with_shell(shell);
        }
        if toggle_follow {
            self.bottom.follow_active_dir = !self.bottom.follow_active_dir;
            self.settings.terminal_follow_active_dir = self.bottom.follow_active_dir;
            self.settings.save();
        }
        if toggle_search
            && let Some(idx) = self.bottom.focused_terminal_idx()
            && let Some(term) = self.bottom.terminals.get_mut(idx)
        {
            if term.search.open {
                term.close_search();
                self.bottom.terminal_focus = true;
            } else {
                term.open_search();
            }
        }
        if split_add {
            self.split_terminal_pane();
        }
        if kill_term {
            self.close_active_terminal();
        }

        match self.bottom.tab {
            BottomTab::Problems => self.problems_body_ui(ui, style),
            BottomTab::Output => self.output_body_ui(ui, style),
            BottomTab::Terminal => self.terminal_body_ui(ui, style, clear),
        }
    }

    fn problems_body_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, theme.terminal_bg);

        let mut entries: Vec<(PathBuf, String, crate::lsp::Diagnostic)> = Vec::new();
        for tab in &self.tabs {
            let title = tab.title();
            let path = tab.doc.path.clone().unwrap_or_default();
            for diag in &tab.diagnostics {
                entries.push((path.clone(), title.clone(), diag.clone()));
            }
        }

        if entries.is_empty() {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    let text = "当前打开的文件中未检测到语法或诊断问题";
                    let text_w = ui.ctx().fonts_mut(|f| {
                        f.layout_no_wrap(text.to_string(), style.small(), theme.text_muted)
                            .rect
                            .width()
                    });
                    let total_w = 18.0 + text_w;
                    ui.add_space(((ui.available_width() - total_w) * 0.5).max(0.0));
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    icons::paint(ui.painter(), ir, Icon::PassFilled, theme.success, 1.2);
                    ui.label(
                        egui::RichText::new(text)
                            .font(style.small())
                            .color(theme.text_muted),
                    );
                });
            });
            return;
        }

        let mut jump_to: Option<(PathBuf, usize)> = None;
        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("problems_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for (path, title, diag) in &entries {
                    let (row, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), crate::theme::H_ROW),
                        Sense::hover(),
                    );
                    let resp = widgets::paint_row(ui, row, &theme, widgets::RowState::idle());
                    let (icon, color) = match diag.severity {
                        DiagSeverity::Error => (Icon::Error, theme.error),
                        DiagSeverity::Warning => (Icon::Warning, theme.warning),
                        DiagSeverity::Info => (Icon::Info, theme.accent),
                    };
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(row.left() + 18.0, row.center().y),
                            Vec2::splat(14.0),
                        ),
                        icon,
                        color,
                        1.2,
                    );
                    let loc = format!("{} [{}:{}]", title, diag.line + 1, diag.col + 1);
                    let (loc_g, _) = widgets::ellipsize_mode(
                        ui,
                        &loc,
                        style.tiny(),
                        theme.text_muted,
                        (row.width() * 0.35).clamp(60.0, 220.0),
                        widgets::EllipsizeMode::Middle,
                    );
                    let loc_w = loc_g.rect.width();
                    let msg_max = (row.width() - 34.0 - loc_w - 28.0).max(40.0);
                    let (msg_g, msg_cut) = widgets::ellipsize_mode(
                        ui,
                        &diag.message,
                        style.small(),
                        theme.text,
                        msg_max,
                        widgets::EllipsizeMode::End,
                    );
                    ui.painter().galley(
                        Pos2::new(row.left() + 34.0, row.center().y - msg_g.rect.height() * 0.5),
                        msg_g,
                        theme.text,
                    );
                    ui.painter().galley(
                        Pos2::new(
                            row.right() - 14.0 - loc_w,
                            row.center().y - loc_g.rect.height() * 0.5,
                        ),
                        loc_g,
                        theme.text_muted,
                    );
                    if msg_cut {
                        resp.clone().on_hover_text(&diag.message);
                    }
                    if resp.clicked() && !path.as_os_str().is_empty() {
                        jump_to = Some((path.clone(), diag.start_byte));
                    }
                }
            });

        if let Some((path, byte_offset)) = jump_to {
            self.open_file_at(path, byte_offset);
        }
    }

    fn output_body_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, theme.terminal_bg);

        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("output_logs_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.add_space(6.0);
                if self.git.logs.is_empty() && self.plugins.logs().is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new("[Kestrel] 工作区与 Git 后台服务就绪。")
                                .font(FontId::new(12.0, style.mono_family.clone()))
                                .color(theme.text_muted),
                        );
                    });
                } else {
                    for line in self.git.logs.iter().chain(self.plugins.logs().iter()) {
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.label(
                                egui::RichText::new(line)
                                    .font(FontId::new(12.0, style.mono_family.clone()))
                                    .color(theme.text_dim),
                            );
                        });
                    }
                }
            });
    }

    fn terminal_body_ui(&mut self, ui: &mut Ui, style: &UiStyle, clear: bool) {
        let theme = style.theme;
        if self.bottom.terminals.is_empty() {
            let (body, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
            ui.painter().rect_filled(body, 0.0, theme.terminal_bg);
            let button = Rect::from_center_size(body.center(), Vec2::new(156.0, 32.0));
            if widgets::paint_button(
                ui,
                button,
                "new_terminal",
                "新建终端会话",
                style.small(),
                &theme,
                widgets::ButtonVariant::Secondary,
                true,
            )
            .clicked()
            {
                self.new_terminal();
            }
            return;
        }

        self.bottom.normalize_panes();
        if clear && let Some(idx) = self.bottom.focused_terminal_idx() {
            self.bottom.terminals[idx].clear_screen();
        }

        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.terminal_bg);
        ui.allocate_rect(area, Sense::hover());

        let panes = self.bottom.panes.clone();
        let pane_count = panes.len();
        let show_headers = pane_count >= 2;
        let gap = 6.0;
        let total_gaps = gap * (pane_count.saturating_sub(1)) as f32;
        let usable = (area.width() - total_gaps).max(16.0);
        let weight_sum: f32 = self
            .bottom
            .pane_weights
            .iter()
            .take(pane_count)
            .copied()
            .sum::<f32>()
            .max(0.01);

        let mut rects = Vec::with_capacity(pane_count);
        let mut cursor_x = area.left();
        for slot in 0..pane_count {
            let w = if slot + 1 == pane_count {
                (area.right() - cursor_x).max(8.0)
            } else {
                let frac = self.bottom.pane_weights[slot] / weight_sum;
                (usable * frac).floor().max(8.0)
            };
            rects.push(Rect::from_min_size(
                Pos2::new(cursor_x, area.top()),
                Vec2::new(w, area.height()),
            ));
            cursor_x += w + gap;
        }

        // Draggable dividers between every adjacent pair of panes.
        for slot in 0..pane_count.saturating_sub(1) {
            let div_rect = Rect::from_min_size(
                Pos2::new(rects[slot].right(), area.top()),
                Vec2::new(gap, area.height()),
            );
            let div_resp = ui.interact(
                div_rect,
                egui::Id::unique(("kestrel_terminal_split_divider", slot)),
                Sense::click_and_drag(),
            );
            if div_resp.hovered() || div_resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if div_resp.double_clicked() {
                self.bottom.pane_weights.fill(1.0);
            } else if div_resp.dragged() {
                let dx = div_resp.drag_delta().x;
                if dx.abs() > 0.1 {
                    let delta_w = (dx / usable) * weight_sum;
                    let pair_sum =
                        self.bottom.pane_weights[slot] + self.bottom.pane_weights[slot + 1];
                    let min_w = (pair_sum * 0.2).max(0.15);
                    let next_left = (self.bottom.pane_weights[slot] + delta_w)
                        .clamp(min_w, (pair_sum - min_w).max(min_w));
                    self.bottom.pane_weights[slot] = next_left;
                    self.bottom.pane_weights[slot + 1] = (pair_sum - next_left).max(0.15);
                }
            }
            let x = div_rect.center().x;
            ui.painter().line_segment(
                [Pos2::new(x, area.top()), Pos2::new(x, area.bottom())],
                Stroke::new(
                    if div_resp.hovered() || div_resp.dragged() {
                        2.0
                    } else {
                        1.0
                    },
                    if div_resp.hovered() || div_resp.dragged() {
                        theme.accent
                    } else {
                        theme.border
                    },
                ),
            );
        }

        // Render each visible pane independently; clicking moves focus without swapping panes.
        let mut any_focused = false;
        let mut new_focused_slot: Option<usize> = None;
        let mut pending_actions: Vec<(usize, PaneAction)> = Vec::new();

        for (slot, (&term_idx, &rect)) in panes.iter().zip(rects.iter()).enumerate() {
            let is_target = slot == self.bottom.focused_pane;
            let res = ui
                .scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    self.terminal_pane_ui(ui, style, term_idx, is_target, show_headers, slot)
                })
                .inner;
            any_focused |= res.focused;
            if res.interacted {
                new_focused_slot = Some(slot);
            }
            if !matches!(res.action, PaneAction::None) {
                pending_actions.push((term_idx, res.action));
            }
        }

        self.bottom.terminal_has_focus = any_focused;
        if let Some(slot) = new_focused_slot
            && slot < self.bottom.panes.len()
        {
            self.bottom.focused_pane = slot;
            self.bottom.active_terminal = self.bottom.panes[slot];
        }

        for (term_idx, action) in pending_actions {
            self.apply_pane_action(term_idx, action);
        }
    }

    fn apply_pane_action(&mut self, idx: usize, action: PaneAction) {
        match action {
            PaneAction::None => {}
            PaneAction::Copy(text) => {
                crate::app::copy_to_clipboard(text);
                self.bottom.terminal_focus = true;
            }
            PaneAction::Paste => {
                if let Ok(mut cb) = arboard::Clipboard::new()
                    && let Ok(text) = cb.get_text()
                    && !text.is_empty()
                    && let Some(term) = self.bottom.terminals.get_mut(idx)
                {
                    term.write_paste(&text);
                }
                self.bottom.terminal_focus = true;
            }
            PaneAction::SelectAll => {
                if let Some(term) = self.bottom.terminals.get_mut(idx) {
                    term.select_all_visible();
                }
                self.bottom.terminal_focus = true;
            }
            PaneAction::Search => {
                if let Some(term) = self.bottom.terminals.get_mut(idx) {
                    term.open_search();
                }
            }
            PaneAction::Clear => {
                if let Some(term) = self.bottom.terminals.get_mut(idx) {
                    term.clear_screen();
                }
                self.bottom.terminal_focus = true;
            }
            PaneAction::NewTerminal => {
                self.new_terminal();
            }
            PaneAction::SplitPane => {
                self.split_terminal_pane();
            }
            PaneAction::Restart => {
                if let Some(term) = self.bottom.terminals.get_mut(idx) {
                    term.restart();
                }
                self.bottom.terminal_focus = true;
            }
            PaneAction::StartRename => {
                if let Some(term) = self.bottom.terminals.get(idx) {
                    self.bottom.renaming_terminal = Some((term.id, term.title(), true));
                }
            }
            PaneAction::Close => {
                self.close_terminal_at(idx);
            }
            PaneAction::OpenUrl(url) => {
                crate::terminal::open_url(&url);
            }
            PaneAction::OpenFile { path, line, col } => {
                if let Some(ln) = line {
                    self.open_file_at_line_col(path, ln, col);
                } else {
                    self.open_file(path);
                }
            }
        }
    }
}
