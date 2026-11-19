//! Bottom workbench panel: Integrated multi-session Terminal, Problems (Diagnostics), and Output logs.

use std::path::PathBuf;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, Event, FontId, ImeEvent, Key, Modifiers, Pos2, Rect, ScrollArea,
    Sense, Stroke, TextEdit, Ui, Vec2,
};

use crate::app::{BottomTab, KestrelApp};
use crate::icons::{self, Icon};
use crate::lsp::DiagSeverity;
use crate::terminal::ShellKind;
use crate::ui::widgets;
use crate::ui::UiStyle;

impl KestrelApp {
    pub fn terminal_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        // Recomputed below when the grid owns the keyboard; global shortcuts read
        // the previous frame's value so transient keys are not stolen mid-frame.
        self.bottom.terminal_has_focus = false;
        // Header with tabs: PROBLEMS | OUTPUT | TERMINAL
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

        // Tabs keep out of the fixed right-hand action cluster.
        let actions_left = header.right() - 168.0;
        let mut tab_x = header.left() + crate::theme::SP3;
        let mut draw_tab = |ui: &mut Ui, tab: BottomTab, label: &str, badge: Option<usize>| {
            let active = self.bottom.tab == tab;
            let text_w = label.chars().count() as f32 * 11.0 + 16.0;
            let badge_w = if badge.is_some_and(|b| b > 0) { 22.0 } else { 0.0 };
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
            }
        };

        draw_tab(ui, BottomTab::Terminal, "终端", Some(term_count));
        draw_tab(ui, BottomTab::Problems, "问题", Some(total_problems));
        draw_tab(ui, BottomTab::Output, "输出", None);

        // Terminal instance selector pills when on Terminal tab
        let mut switch_term = None;
        if self.bottom.tab == BottomTab::Terminal && !self.bottom.terminals.is_empty() {
            tab_x += crate::theme::SP2;
            for (idx, term) in self.bottom.terminals.iter().enumerate() {
                let active = idx == self.bottom.active_terminal;
                let label = format!("{}: {}", idx + 1, term.title());
                let w = (label.len() as f32 * 6.5 + 18.0).clamp(64.0, 130.0);
                if tab_x + w > actions_left {
                    break;
                }
                let pill = Rect::from_center_size(
                    Pos2::new(tab_x + w * 0.5, header.center().y),
                    Vec2::new(w, 20.0),
                );
                let resp = widgets::paint_row(
                    ui,
                    pill,
                    &theme,
                    widgets::RowState::new(active, true),
                );
                let (galley, cut) = widgets::ellipsize(
                    ui,
                    &label,
                    style.tiny(),
                    if active { theme.text } else { theme.text_dim },
                    pill.width() - 8.0,
                );
                ui.painter().galley(
                    Pos2::new(
                        pill.center().x - galley.rect.width() * 0.5,
                        pill.center().y - galley.rect.height() * 0.5,
                    ),
                    galley,
                    theme.text,
                );
                if cut {
                    resp.clone()
                        .on_hover_text(format!("{} {}", idx + 1, term.title()));
                }
                if resp.clicked() {
                    switch_term = Some(idx);
                }
                tab_x += w + crate::theme::SP1;
            }
        }
        if let Some(idx) = switch_term {
            self.bottom.active_terminal = idx;
            self.bottom.terminal_focus = true;
            for term in &mut self.bottom.terminals {
                term.ime_preedit.clear();
            }
        }

        // Right side header actions
        let mut new_pwsh = false;
        let mut new_cmd = false;
        let mut kill_term = false;
        let mut clear = false;
        let mut close_panel = false;
        let mut x = header.right() - 8.0;
        let action = |ui: &mut Ui, x: &mut f32, icon: Icon, tip: &str| -> bool {
            *x -= 24.0;
            let r =
                Rect::from_center_size(Pos2::new(*x + 11.0, header.center().y), Vec2::splat(22.0));
            let resp = ui.interact(r, ui.scope_id().with(("term_action", tip)), Sense::click());
            if resp.hovered() {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(5), theme.hover);
            }
            icons::paint(
                ui.painter(),
                r.shrink(4.0),
                icon,
                if resp.hovered() {
                    theme.text
                } else {
                    theme.text_dim
                },
                1.3,
            );
            resp.on_hover_text(tip).clicked()
        };
        if action(ui, &mut x, Icon::Close, "隐藏底部面板 (Ctrl+J)") {
            close_panel = true;
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
            if action(ui, &mut x, Icon::Trash, "关闭当前终端会话") {
                kill_term = true;
            }
            if action(ui, &mut x, Icon::ClearAll, "清屏 (Clear)") {
                clear = true;
            }
            // Compact "CMD" pill button to distinguish from the PowerShell '+' icon
            x -= 36.0;
            let cmd_rect = Rect::from_center_size(
                Pos2::new(x + 17.0, header.center().y),
                Vec2::new(32.0, 20.0),
            );
            let cmd_resp = ui.interact(cmd_rect, ui.scope_id().with("term_cmd_btn"), Sense::click());
            ui.painter().rect_filled(
                cmd_rect,
                CornerRadius::same(4),
                if cmd_resp.hovered() {
                    theme.hover
                } else {
                    theme.input_bg
                },
            );
            ui.painter().rect_stroke(
                cmd_rect,
                CornerRadius::same(4),
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                cmd_rect.center(),
                Align2::CENTER_CENTER,
                "CMD",
                style.tiny(),
                if cmd_resp.hovered() {
                    theme.text
                } else {
                    theme.text_dim
                },
            );
            if cmd_resp.on_hover_text("新建 CMD 终端").clicked() {
                new_cmd = true;
            }
            if action(
                ui,
                &mut x,
                Icon::Plus,
                "新建 PowerShell 终端 (Ctrl+Shift+`)",
            ) {
                new_pwsh = true;
            }
        } else if self.bottom.tab == BottomTab::Output
            && action(ui, &mut x, Icon::ClearAll, "清空输出日志")
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
            return;
        }
        if new_pwsh {
            self.new_terminal_with_shell(ShellKind::PowerShell);
        }
        if new_cmd {
            self.new_terminal_with_shell(ShellKind::Cmd);
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
        ui.painter().rect_filled(rect, 0.0, theme.input_bg);

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
                ui.label(
                    egui::RichText::new("✓ 当前打开的文件中未检测到语法或诊断问题")
                        .font(style.small())
                        .color(theme.text_faint),
                );
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
                    ui.painter().text(
                        Pos2::new(row.left() + 34.0, row.center().y),
                        Align2::LEFT_CENTER,
                        &diag.message,
                        style.small(),
                        theme.text,
                    );
                    let loc = format!("{} [{}:{}]", title, diag.line + 1, diag.col + 1);
                    ui.painter().text(
                        Pos2::new(row.right() - 14.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        loc,
                        style.tiny(),
                        theme.text_faint,
                    );
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
        ui.painter().rect_filled(rect, 0.0, theme.input_bg);

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
                                .color(theme.text_faint),
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
            ui.painter().rect_filled(body, 0.0, theme.input_bg);
            let button = Rect::from_center_size(body.center(), Vec2::new(150.0, 30.0));
            let bresp = ui.interact(button, ui.scope_id().with("new_terminal"), Sense::click());
            ui.painter().rect_filled(
                button,
                CornerRadius::same(6),
                if bresp.hovered() {
                    theme.hover
                } else {
                    theme.active
                },
            );
            ui.painter().text(
                button.center(),
                Align2::CENTER_CENTER,
                "+ 新建终端会话",
                style.small(),
                theme.text,
            );
            if bresp.clicked() {
                self.new_terminal();
            }
            return;
        }

        let active_idx = self
            .bottom
            .active_terminal
            .min(self.bottom.terminals.len() - 1);
        self.bottom.active_terminal = active_idx;

        if clear {
            self.bottom.terminals[active_idx].clear_screen();
        }

        if self.bottom.terminals[active_idx].search.open {
            self.terminal_search_bar_ui(ui, style, active_idx);
        }

        let (body, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.painter().rect_filled(body, 0.0, theme.input_bg);

        if resp.clicked() {
            resp.request_focus();
        }
        if self.bottom.terminal_focus {
            resp.request_focus();
            // 终端面板先于 CentralPanel 绘制，编辑器本帧仍会按 focus_requested 抢焦点，这里提前清掉。
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus_requested = false;
            }
            self.bottom.terminal_focus = false;
        }
        let focused = resp.has_focus();
        self.bottom.terminal_has_focus = focused;

        // Ctrl + wheel adjusts terminal font size; normal wheel scrolls history
        if resp.hovered() {
            let zoom_step = crate::ui::ctrl_wheel_steps(ui.ctx());
            if zoom_step != 0 {
                self.settings.terminal_font_size =
                    (self.settings.terminal_font_size + zoom_step as f32).clamp(9.0, 28.0);
                self.settings.save();
            } else {
                let (modifiers, smooth_dy) =
                    ui.input(|i| (i.modifiers, i.smooth_scroll_delta.y));
                if !(modifiers.ctrl || modifiers.command)
                    && smooth_dy.abs() > 0.5
                    && let Some(term) = self.bottom.terminals.get_mut(active_idx)
                {
                    let lines = (smooth_dy / 16.0).round() as isize;
                    let next_offset =
                        (term.scrollback_offset as isize + lines).clamp(0, 5000) as usize;
                    term.set_scrollback(next_offset);
                }
            }
        }

        let term_font_size = self.settings.terminal_font_size.clamp(9.0, 28.0);
        let font = FontId::new(term_font_size, style.mono_family.clone());
        let (cell_w, text_h) = ui
            .ctx()
            .fonts_mut(|fonts| (fonts.glyph_width(&font, 'M').max(1.0), fonts.row_height(&font)));
        let cell_h = (term_font_size * 1.45).max(12.0);
        let y_text_off = (cell_h - text_h) * 0.5;
        let cols = ((body.width() - 14.0) / cell_w).floor().max(2.0) as u16;
        let rows = ((body.height() - 6.0) / cell_h).floor().max(2.0) as u16;

        let terminal = &mut self.bottom.terminals[active_idx];
        if !focused {
            terminal.ime_preedit.clear();
        }
        let (cur_rows, cur_cols) = terminal.screen().size();
        if (rows, cols) != (cur_rows.get(), cur_cols.get()) && cols > 0 && rows > 0 {
            terminal.resize(cols, rows);
        }
        if cols > 0 && rows > 0 {
            self.bottom.terminal_size = (cols, rows);
        }
        terminal.poll();

        let origin = Pos2::new(body.left() + 8.0, body.top() + 3.0);
        let (screen_rows, screen_cols) = {
            let (rows_nz, cols_nz) = terminal.screen().size();
            (rows_nz.get().min(rows), cols_nz.get().min(cols))
        };

        // Mouse selection maps the press point (not the post-threshold drag point)
        // to a cell so the first cell stays inside the range.
        if !terminal.search.open && focused {
            if resp.drag_started()
                && let Some(pos) = ui
                    .input(|i| i.pointer.press_origin())
                    .or_else(|| resp.interact_pointer_pos())
            {
                let (row, col) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                terminal.begin_selection(row, col);
            }
            if resp.dragged()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let (row, col) = cell_at(pos, origin, cell_w, cell_h, screen_rows, screen_cols);
                terminal.update_selection(row, col);
            }
            if resp.clicked() {
                terminal.clear_selection();
            }
        }
        if terminal.search.open && resp.clicked() {
            terminal.search.focus = true;
        }

        if focused && !terminal.search.open {
            terminal_input(ui, terminal);
        }

        let screen = terminal.screen();
        let painter = ui.painter_at(body);
        for row in 0..screen_rows {
            let y = origin.y + row as f32 * cell_h;
            let mut run_start = 0u16;
            let mut run_text = String::new();
            let mut run_fg = theme.text;
            let mut run_underline = false;
            let mut run_bold = false;
            let mut idx = 0u16;
            while idx < screen_cols {
                let cell = screen.cell(row, idx);
                let contents = cell.map(|c| c.contents().to_string()).unwrap_or_default();
                if contents.is_empty() {
                    if !run_text.is_empty() {
                        paint_terminal_run(
                            ui,
                            &painter,
                            Pos2::new(origin.x + run_start as f32 * cell_w, y + y_text_off),
                            &run_text,
                            &font,
                            run_fg,
                            run_underline,
                        );
                        run_text.clear();
                    }
                    idx += 1;
                    continue;
                }
                let (fg, bg, bold, underline, inverse) = match cell {
                    Some(cell) => (
                        color_to_ui(cell.fgcolor(), theme, true),
                        color_to_ui(cell.bgcolor(), theme, false),
                        cell.bold(),
                        cell.underline(),
                        cell.inverse(),
                    ),
                    None => (theme.text, theme.input_bg, false, false, false),
                };
                let (fg, bg) = if inverse { (bg, fg) } else { (fg, bg) };
                let ch = contents.chars().next();
                let is_wide = contents.chars().count() == 1 && ch.is_some_and(is_wide_char);
                let is_non_ascii = !contents.is_ascii();
                let cell_cols = if is_wide { 2.0 } else { 1.0 };

                if bg != theme.input_bg {
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(origin.x + idx as f32 * cell_w, y),
                            Vec2::new(cell_w * cell_cols, cell_h),
                        ),
                        0.0,
                        bg,
                    );
                }
                if is_non_ascii {
                    if !run_text.is_empty() {
                        paint_terminal_run(
                            ui,
                            &painter,
                            Pos2::new(origin.x + run_start as f32 * cell_w, y + y_text_off),
                            &run_text,
                            &font,
                            run_fg,
                            run_underline,
                        );
                        run_text.clear();
                    }
                    paint_terminal_run(
                        ui,
                        &painter,
                        Pos2::new(origin.x + idx as f32 * cell_w, y + y_text_off),
                        &contents,
                        &font,
                        fg,
                        underline,
                    );
                    if is_wide {
                        idx += 1;
                    }
                    idx += 1;
                    continue;
                }
                if run_text.is_empty() {
                    run_start = idx;
                    run_fg = fg;
                    run_underline = underline;
                    run_bold = bold;
                } else if fg != run_fg || underline != run_underline || bold != run_bold {
                    paint_terminal_run(
                        ui,
                        &painter,
                        Pos2::new(origin.x + run_start as f32 * cell_w, y + y_text_off),
                        &run_text,
                        &font,
                        run_fg,
                        run_underline,
                    );
                    run_text.clear();
                    run_start = idx;
                    run_fg = fg;
                    run_underline = underline;
                    run_bold = bold;
                }
                run_text.push_str(&contents);
                idx += 1;
            }
            if !run_text.is_empty() {
                paint_terminal_run(
                    ui,
                    &painter,
                    Pos2::new(origin.x + run_start as f32 * cell_w, y + y_text_off),
                    &run_text,
                    &font,
                    run_fg,
                    run_underline,
                );
            }
        }

        // Selection and search hits sit above the glyphs as translucent washes so the
        // ANSI foreground colors keep their contrast.
        let visible_start = terminal.visible_start_row();
        let text_right = origin.x + screen_cols as f32 * cell_w;
        if let Some(selection) = terminal.selection {
            let wash = Color32::from_rgba_unmultiplied(
                theme.accent.r(),
                theme.accent.g(),
                theme.accent.b(),
                56,
            );
            for row in 0..screen_rows {
                if let Some((start, end)) = selection.row_span(row, screen_cols) {
                    let x0 = origin.x + start as f32 * cell_w;
                    let x1 = (origin.x + (end as f32 + 1.0) * cell_w).min(text_right);
                    let y = origin.y + row as f32 * cell_h;
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + cell_h)),
                        0.0,
                        wash,
                    );
                }
            }
        }
        if terminal.search.open {
            let active = terminal
                .search
                .active
                .and_then(|index| terminal.search.matches.get(index))
                .copied();
            let hit = Color32::from_rgba_unmultiplied(
                theme.find_match.r(),
                theme.find_match.g(),
                theme.find_match.b(),
                80,
            );
            let hot = Color32::from_rgba_unmultiplied(
                theme.accent.r(),
                theme.accent.g(),
                theme.accent.b(),
                110,
            );
            for hit_ref in &terminal.search.matches {
                if hit_ref.row < visible_start || hit_ref.row >= visible_start + screen_rows as usize {
                    continue;
                }
                let row = (hit_ref.row - visible_start) as f32;
                let x0 = origin.x + hit_ref.start_col as f32 * cell_w;
                let x1 = (origin.x + (hit_ref.end_col as f32 + 1.0) * cell_w).min(text_right);
                let y = origin.y + row * cell_h;
                let is_active =
                    active.is_some_and(|a| a.row == hit_ref.row && a.start_col == hit_ref.start_col);
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + cell_h)),
                    0.0,
                    if is_active { hot } else { hit },
                );
            }
        }

        // Scrollback indicator pill if viewing history
        if terminal.scrollback_offset > 0 {
            let badge = Rect::from_min_size(
                Pos2::new(body.right() - 120.0, body.top() + 6.0),
                Vec2::new(110.0, 20.0),
            );
            painter.rect_filled(badge, CornerRadius::same(4), theme.active);
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("历史回滚 ↑{} 行", terminal.scrollback_offset),
                style.tiny(),
                theme.accent,
            );
        }

        // Cursor + IME composition anchor.
        let (crow, ccol) = screen.cursor_position();
        let cursor_in_grid = crow < screen_rows && ccol < screen_cols;
        let cursor_wide = screen
            .cell(crow, ccol)
            .map(|c| c.contents().chars().next().is_some_and(is_wide_char))
            .unwrap_or(false);
        let cursor_w = if cursor_wide { cell_w * 2.0 } else { cell_w };
        let cursor_cell = cursor_in_grid.then(|| {
            Rect::from_min_size(
                Pos2::new(origin.x + ccol as f32 * cell_w, origin.y + crow as f32 * cell_h),
                Vec2::new(cursor_w, cell_h),
            )
        });
        if !screen.hide_cursor()
            && focused
            && terminal.scrollback_offset == 0
            && let Some(cell) = cursor_cell
        {
            let r = Rect::from_min_size(
                Pos2::new(cell.left(), cell.top() + y_text_off),
                Vec2::new(cell.width(), text_h),
            );
            painter.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.32));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.5, theme.accent),
                egui::StrokeKind::Inside,
            );
        }
        if focused {
            let cell = cursor_cell.unwrap_or_else(|| {
                Rect::from_min_size(origin, Vec2::new(cell_w, cell_h))
            });
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: to_global * body,
                    cursor_rect: to_global * cell,
                    should_interrupt_composition: false,
                });
            });
            if cursor_cell.is_some() && !terminal.ime_preedit.is_empty() {
                let job = LayoutJob::single_section(
                    terminal.ime_preedit.clone(),
                    TextFormat {
                        font_id: font.clone(),
                        color: theme.text,
                        underline: Stroke::new(1.0, theme.accent),
                        line_height: Some(cell_h),
                        ..Default::default()
                    },
                );
                let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
                painter.galley(cell.left_top(), galley, theme.text);
            }
        }

        if focused {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
    /// Ctrl+F row above the grid: query field, live match counter, and
    /// Enter / Shift+Enter navigation. Esc closes and returns focus to the grid.
    fn terminal_search_bar_ui(&mut self, ui: &mut Ui, style: &UiStyle, idx: usize) {
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

        let dismissed = {
            let Some(terminal) = self.bottom.terminals.get_mut(idx) else {
                return;
            };
            let mut close = false;
            let mut forward = false;
            let mut backward = false;
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                close = true;
            } else if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter)) {
                backward = true;
            } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                forward = true;
            }

            if close {
                terminal.close_search();
            } else {
                let input_w = (bar.width() * 0.42).clamp(150.0, 340.0);
                let input_rect = Rect::from_min_size(
                    Pos2::new(bar.left() + 10.0, bar.top() + 4.0),
                    Vec2::new(input_w, 22.0),
                );
                let response = ui.put(
                    input_rect,
                    TextEdit::singleline(&mut terminal.search.query)
                        .id(egui::Id::unique(("terminal_search", idx)))
                        .hint_text("搜索终端输出")
                        .font(style.small())
                        .margin(Vec2::new(8.0, 3.0)),
                );
                if response.changed() {
                    terminal.search.dirty = true;
                    terminal.search.active = None;
                }
                if terminal.search.focus {
                    response.request_focus();
                    terminal.search.focus = false;
                }
                terminal.refresh_search();
                if terminal.search.dirty {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(160));
                }
                let counter = if terminal.search.query.is_empty() {
                    String::new()
                } else if terminal.search.matches.is_empty() {
                    "0 项".to_string()
                } else if terminal.search.truncated {
                    format!(
                        "{}+ 项（上限 {}）",
                        terminal.search.matches.len(),
                        crate::terminal::MAX_SEARCH_MATCHES
                    )
                } else {
                    match terminal.search.active {
                        Some(index) => {
                            format!("{}/{} 项", index + 1, terminal.search.matches.len())
                        }
                        None => format!("{} 项", terminal.search.matches.len()),
                    }
                };
                if !counter.is_empty() {
                    ui.painter().text(
                        Pos2::new(input_rect.right() + 12.0, bar.center().y),
                        Align2::LEFT_CENTER,
                        &counter,
                        style.tiny(),
                        if terminal.search.truncated {
                            theme.warning
                        } else {
                            theme.text_dim
                        },
                    );
                }
                if forward || backward {
                    terminal.search_step(forward);
                }
            }
            !terminal.search.open
        };
        if dismissed {
            self.bottom.terminal_focus = true;
        }
    }
}

fn paint_terminal_run(
    ui: &Ui,
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    font: &FontId,
    color: Color32,
    underline: bool,
) {
    let job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font.clone(),
            color,
            underline: if underline {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        },
    );
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(pos, galley, color);
}

fn is_wide_char(ch: char) -> bool {
    crate::terminal::char_width(ch) == 2
}

/// Map a pointer position to a terminal cell, clamped to the visible grid.
fn cell_at(pos: Pos2, origin: Pos2, cell_w: f32, cell_h: f32, rows: u16, cols: u16) -> (u16, u16) {
    let col = ((pos.x - origin.x) / cell_w)
        .floor()
        .clamp(0.0, cols.saturating_sub(1) as f32) as u16;
    let row = ((pos.y - origin.y) / cell_h)
        .floor()
        .clamp(0.0, rows.saturating_sub(1) as f32) as u16;
    (row, col)
}
fn color_to_ui(color: vt100::Color, theme: crate::theme::Theme, foreground: bool) -> Color32 {
    let default = if foreground {
        theme.text
    } else {
        theme.input_bg
    };
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(idx) => ansi_color(idx, theme.kind.is_dark()),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// ANSI base 16. The light set is dark-on-light: reusing the dark pastels on a
/// white terminal leaves red/green/blue at 1.1-1.8:1.
fn ansi_color(idx: u8, dark: bool) -> Color32 {
    const DARK: [Color32; 16] = [
        Color32::from_rgb(0x22, 0x24, 0x2B),
        Color32::from_rgb(0xE0, 0x6C, 0x75),
        Color32::from_rgb(0x98, 0xC3, 0x79),
        Color32::from_rgb(0xE5, 0xC0, 0x7B),
        Color32::from_rgb(0x61, 0xAF, 0xEF),
        Color32::from_rgb(0xC6, 0x78, 0xDD),
        Color32::from_rgb(0x56, 0xB6, 0xC2),
        Color32::from_rgb(0xBE, 0xC1, 0xC8),
        Color32::from_rgb(0x5C, 0x63, 0x72),
        Color32::from_rgb(0xE8, 0x8A, 0x92),
        Color32::from_rgb(0xB5, 0xD8, 0x9A),
        Color32::from_rgb(0xF0, 0xD0, 0x95),
        Color32::from_rgb(0x82, 0xBE, 0xF5),
        Color32::from_rgb(0xD8, 0x9C, 0xE8),
        Color32::from_rgb(0x7A, 0xCF, 0xD8),
        Color32::from_rgb(0xEE, 0xEF, 0xF2),
    ];
    const LIGHT: [Color32; 16] = [
        Color32::from_rgb(0x24, 0x29, 0x2F),
        Color32::from_rgb(0xB3, 0x26, 0x1E),
        Color32::from_rgb(0x1F, 0x6E, 0x2E),
        Color32::from_rgb(0x8A, 0x5A, 0x00),
        Color32::from_rgb(0x1F, 0x5F, 0xBF),
        Color32::from_rgb(0x82, 0x26, 0xA8),
        Color32::from_rgb(0x0B, 0x6E, 0x80),
        Color32::from_rgb(0x5A, 0x62, 0x72),
        Color32::from_rgb(0x5A, 0x62, 0x72),
        Color32::from_rgb(0xC6, 0x28, 0x28),
        Color32::from_rgb(0x2E, 0x7D, 0x32),
        Color32::from_rgb(0x9A, 0x54, 0x00),
        Color32::from_rgb(0x19, 0x76, 0xD2),
        Color32::from_rgb(0xAB, 0x47, 0xBC),
        Color32::from_rgb(0x00, 0x83, 0x8F),
        Color32::from_rgb(0x3E, 0x46, 0x52),
    ];
    if idx < 16 {
        return if dark {
            DARK[idx as usize]
        } else {
            LIGHT[idx as usize]
        };
    }
    if idx < 232 {
        let i = idx - 16;
        let r = i / 36;
        let g = (i % 36) / 6;
        let b = i % 6;
        let scale = |v: u8| -> u8 {
            if v == 0 {
                0
            } else {
                (55 + v as u16 * 40).min(255) as u8
            }
        };
        return Color32::from_rgb(scale(r), scale(g), scale(b));
    }
    let gray = 8 + (idx - 232) * 10;
    Color32::from_rgb(gray, gray, gray)
}

fn terminal_input(ui: &Ui, terminal: &mut crate::terminal::TerminalSession) {
    let ctx = ui.ctx();
    let events = ctx.input(|i| i.events.clone());
    for event in events {
        match event {
            Event::Text(text) => {
                terminal.ime_preedit.clear();
                terminal.write(text.as_bytes());
            }
            Event::Paste(text) => {
                terminal.write(b"\x1b[200~");
                terminal.write(text.as_bytes());
                terminal.write(b"\x1b[201~");
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => {
                terminal.ime_preedit = text;
            }
            Event::Ime(ImeEvent::Commit(text)) => {
                terminal.ime_preedit.clear();
                if !text.is_empty() {
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
                let command = modifiers.ctrl || modifiers.command;
                // Clipboard/search shortcuts are consumed before key_bytes, which
                // would otherwise send Ctrl+Shift+C/V as 0x03/0x16 and Ctrl+F as 0x06.
                if command && modifiers.shift {
                    match key {
                        Key::C => {
                            if let Some(text) = terminal.selection_text() {
                                ctx.copy_text(text);
                            }
                            continue;
                        }
                        Key::V => {
                            if let Ok(mut clipboard) = arboard::Clipboard::new()
                                && let Ok(text) = clipboard.get_text()
                                && !text.is_empty()
                            {
                                terminal.write(b"\x1b[200~");
                                terminal.write(text.as_bytes());
                                terminal.write(b"\x1b[201~");
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
                if command && key == Key::F {
                    terminal.open_search();
                    continue;
                }
                if let Some(bytes) = key_bytes(key, &modifiers) {
                    terminal.write(&bytes);
                }
            }
            _ => {}
        }
    }
}

fn key_bytes(key: Key, modifiers: &Modifiers) -> Option<Vec<u8>> {
    let ctrl = modifiers.ctrl || modifiers.command;
    if ctrl && let Some(byte) = ctrl_letter(key) {
        return Some(vec![byte]);
    }
    let bytes: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Tab => b"\t",
        Key::Backspace => b"\x7f",
        Key::Escape => b"\x1b",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        _ => return None,
    };
    Some(bytes.to_vec())
}

fn ctrl_letter(key: Key) -> Option<u8> {
    Some(match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0A,
        Key::K => 0x0B,
        Key::L => 0x0C,
        Key::M => 0x0D,
        Key::N => 0x0E,
        Key::O => 0x0F,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1A,
        _ => return None,
    })
}
