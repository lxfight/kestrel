//! Command palette / quick open / goto line / symbol outline / branch switcher overlay.

use egui::text::LayoutJob;
use egui::{
    Align2, Area, CornerRadius, Frame, Key, Margin, Order, Pos2, Rect, Sense, Stroke, Ui, UiBuilder,
    Vec2,
};

use crate::app::{split_file_line, KestrelApp, PaletteItem, PaletteMode};
use crate::icons::{self, Icon};
use crate::ui::{widgets, UiStyle};

const MAX_ROWS: usize = 12;
const ROW_H: f32 = 30.0;
const GROUP_H: f32 = 22.0;

impl KestrelApp {
    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        let style = self.ui_style();
        let theme = style.theme;

        let (ctrl, shift) = ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        if ctrl && ctx.input(|i| i.key_pressed(Key::P)) {
            let mode = if shift {
                PaletteMode::Commands
            } else {
                PaletteMode::QuickOpen
            };
            if mode != self.palette.mode {
                self.open_palette(mode);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.close_palette();
            return;
        }

        if self.palette.needs_filter
            || (self.palette.mode == PaletteMode::QuickOpen
                && self.workspace.index_version != self.palette.list_version)
        {
            self.filter_palette();
        }
        let count = self.palette.filtered.len();
        if count > 0 {
            self.palette.selected = self.palette.selected.min(count - 1);
        } else {
            self.palette.selected = 0;
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowDown)) && count > 0 {
            self.palette.selected = (self.palette.selected + 1).min(count - 1);
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
            self.palette.selected = self.palette.selected.saturating_sub(1);
        }
        if self.palette.selected < self.palette.window_start {
            self.palette.window_start = self.palette.selected;
        } else if self.palette.selected >= self.palette.window_start + MAX_ROWS {
            self.palette.window_start = self.palette.selected + 1 - MAX_ROWS;
        }

        let mut activate = ctx.input(|i| i.key_pressed(Key::Enter));

        if self.palette.mode == PaletteMode::GotoLine {
            let line_text = self.palette.query.trim().to_string();
            let line = line_text.parse::<usize>().unwrap_or(0);
            if activate && line == 0 {
                activate = false;
            }
        }

        let query_hint = match self.palette.mode {
            PaletteMode::Commands => "输入命令名称…",
            PaletteMode::QuickOpen => {
                if self.workspace.index.is_loading() {
                    "正在索引文件…"
                } else {
                    "按文件名搜索，或输入 文件:行号 直接跳转"
                }
            }
            PaletteMode::GotoLine => "输入行号…",
            PaletteMode::Language => "选择语言模式…",
            PaletteMode::Symbols => "搜索当前文件中的符号 (@)…",
            PaletteMode::Branches => "选择要切换的 Git 分支…",
        };

        let area = Area::new(egui::Id::unique("kestrel_palette"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 82.0))
            .interactable(true);

        let mut clicked_row: Option<usize> = None;
        area.show(ctx, |ui| {
            Frame::new()
                .fill(theme.overlay_bg)
                .stroke(Stroke::new(1.0, theme.border_strong))
                .corner_radius(CornerRadius::same(10))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 32,
                    spread: 0,
                    color: theme.shadow,
                })
                .inner_margin(Margin::same(0))
                .show(ui, |ui| {
                    ui.set_width(620.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(596.0, 42.0), Sense::hover());
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(rect.left() + 16.0, rect.center().y),
                            Vec2::splat(16.0),
                        );
                        icons::paint(
                            ui.painter(),
                            icon_rect,
                            match self.palette.mode {
                                PaletteMode::Commands => Icon::Goforward,
                                PaletteMode::QuickOpen => Icon::Search,
                                PaletteMode::GotoLine => Icon::Goforward,
                                PaletteMode::Language => Icon::FileCode,
                                PaletteMode::Symbols => Icon::Outline,
                                PaletteMode::Branches => Icon::GitBranch,
                            },
                            theme.accent,
                            1.4,
                        );
                        // A real text editor: IME preedit, selection, caret and
                        // horizontal clipping all come from egui instead of hand-rolled state.
                        let edit_rect = Rect::from_min_max(
                            Pos2::new(rect.left() + 34.0, rect.top() + 9.0),
                            Pos2::new(rect.right() - 8.0, rect.bottom() - 9.0),
                        );
                        let output = ui
                            .scope_builder(UiBuilder::new().max_rect(edit_rect), |ui| {
                                egui::TextEdit::singleline(&mut self.palette.query)
                                    .frame(Frame::NONE)
                                    .font(style.emph())
                                    .text_color(theme.text)
                                    .hint_text(query_hint)
                                    .desired_width(edit_rect.width())
                                    .margin(Margin::ZERO)
                                    .show(ui)
                            })
                            .inner;
                        if self.palette.focus {
                            output.response.request_focus();
                            self.palette.focus = false;
                        }
                        self.palette.blink_base = ui.input(|i| i.time);
                        if output.response.changed() {
                            self.palette.needs_filter = true;
                            self.palette.selected = 0;
                        }
                    });
                    ui.painter().line_segment(
                        [
                            Pos2::new(ui.min_rect().left(), ui.min_rect().top() + 40.0),
                            Pos2::new(ui.min_rect().right(), ui.min_rect().top() + 40.0),
                        ],
                        Stroke::new(1.0, theme.border),
                    );

                    if self.palette.mode == PaletteMode::GotoLine {
                        ui.add_space(8.0);
                        let line = self.palette.query.trim().to_string();
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            let text = if line.is_empty() {
                                "输入要跳转的行号，然后按 Enter".to_string()
                            } else {
                                format!("转到第 {line} 行")
                            };
                            ui.label(egui::RichText::new(text).font(style.ui()).color(
                                if line.is_empty() {
                                    theme.text_faint
                                } else {
                                    theme.text
                                },
                            ));
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new("Enter 跳转 · Esc 关闭")
                                    .font(style.tiny())
                                    .color(theme.text_faint),
                            );
                        });
                        ui.add_space(8.0);
                        return;
                    }

                    if count == 0 {
                        ui.add_space(18.0);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            let (icon_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                            icons::paint(
                                ui.painter(),
                                icon_rect,
                                Icon::Search,
                                theme.text_faint,
                                1.2,
                            );
                            ui.add_space(2.0);
                            let text = if self.palette.mode == PaletteMode::QuickOpen
                                && self.workspace.index.is_loading()
                            {
                                "正在索引项目文件…"
                            } else {
                                "没有匹配结果"
                            };
                            ui.label(
                                egui::RichText::new(text)
                                    .font(style.small())
                                    .color(theme.text_faint),
                            );
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(40.0);
                            let hint = match self.palette.mode {
                                PaletteMode::QuickOpen => {
                                    "换个关键词，或输入 文件:行号 直接跳转"
                                }
                                PaletteMode::Commands => "换个关键词试试，Esc 关闭",
                                _ => "Esc 关闭",
                            };
                            ui.label(
                                egui::RichText::new(hint)
                                    .font(style.tiny())
                                    .color(theme.text_faint),
                            );
                        });
                        ui.add_space(16.0);
                        return;
                    }

                    ui.add_space(2.0);
                    let start = self.palette.window_start;
                    let end = (start + MAX_ROWS).min(count);
                    for index in start..end {
                        if let Some(header) = self.palette_group_header(index) {
                            let (header_rect, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), GROUP_H),
                                Sense::hover(),
                            );
                            ui.painter().text(
                                Pos2::new(header_rect.left() + 16.0, header_rect.bottom() - 5.0),
                                Align2::LEFT_BOTTOM,
                                header,
                                style.font(10.5),
                                theme.text_faint,
                            );
                        }
                        let selected = index == self.palette.selected;
                        let item = self
                            .palette
                            .filtered
                            .get(index)
                            .and_then(|idx| self.palette.items.get(*idx))
                            .cloned();
                        let Some(item) = item else {
                            continue;
                        };
                        let (row_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            Sense::hover(),
                        );
                        let state = if selected {
                            widgets::RowState::selected_row()
                        } else {
                            widgets::RowState::idle()
                        };
                        let row_resp = widgets::paint_row(
                            ui,
                            row_rect.shrink2(Vec2::new(6.0, 1.0)),
                            &theme,
                            state,
                        );
                        if selected {
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(row_rect.left() + 6.0, row_rect.top() + 6.0),
                                    Vec2::new(2.0, row_rect.height() - 12.0),
                                ),
                                CornerRadius::same(1),
                                theme.accent,
                            );
                        }
                        self.palette_row(ui, &style, row_rect, &item, selected);
                        if row_resp.clicked() {
                            clicked_row = Some(index);
                        }
                    }

                    ui.painter().line_segment(
                        [
                            Pos2::new(ui.min_rect().left(), ui.min_rect().bottom()),
                            Pos2::new(ui.min_rect().right(), ui.min_rect().bottom()),
                        ],
                        Stroke::new(1.0, theme.border),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let hint = match self.palette.mode {
                            PaletteMode::QuickOpen => {
                                "↑↓ 选择    Enter 打开    文件:行号 直接跳转    Esc 关闭"
                            }
                            _ => "↑↓ 选择    Enter 确认    Esc 关闭",
                        };
                        ui.label(
                            egui::RichText::new(hint)
                                .font(style.tiny())
                                .color(theme.text_faint),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            let mode_label = match self.palette.mode {
                                PaletteMode::Commands => "命令",
                                PaletteMode::QuickOpen => "文件",
                                PaletteMode::GotoLine => "行",
                                PaletteMode::Language => "语言",
                                PaletteMode::Symbols => "符号",
                                PaletteMode::Branches => "Git 分支",
                            };
                            ui.label(
                                egui::RichText::new(mode_label)
                                    .font(style.tiny())
                                    .color(theme.text_faint),
                            );
                        });
                    });
                    ui.add_space(4.0);
                });
        });

        if let Some(index) = clicked_row {
            self.palette.selected = index;
            activate = true;
        }
        if !activate
            && std::env::var("KESTREL_DEV_ENTER").is_ok()
            && self.frame > 20
            && !self.palette.filtered.is_empty()
        {
            static DEV_ENTERED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !DEV_ENTERED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                activate = true;
            }
        }
        if activate {
            self.palette_activate(ctx);
        }
    }

    /// Group title shown above a palette row, or None when the row joins the
    /// current group. Only the empty-query command list is grouped.
    fn palette_group_header(&self, index: usize) -> Option<String> {
        if self.palette.mode != PaletteMode::Commands || !self.palette.query.trim().is_empty() {
            return None;
        }
        let item = self
            .palette
            .filtered
            .get(index)
            .and_then(|idx| self.palette.items.get(*idx))?;
        let (is_recent, category) = self.palette_item_group(item);
        if index == 0 {
            return Some(if is_recent { "最近使用".to_string() } else { category.to_string() });
        }
        let previous = self
            .palette
            .filtered
            .get(index - 1)
            .and_then(|idx| self.palette.items.get(*idx))?;
        let (previous_recent, previous_category) = self.palette_item_group(previous);
        if is_recent && !previous_recent {
            Some("最近使用".to_string())
        } else if !is_recent && (previous_recent || previous_category != category) {
            Some(category.to_string())
        } else {
            None
        }
    }

    fn palette_item_group<'a>(&'a self, item: &'a PaletteItem) -> (bool, &'a str) {
        match item {
            PaletteItem::Command(command) => (
                self.settings
                    .recent_commands
                    .iter()
                    .any(|id| id == command.id()),
                command.category(),
            ),
            PaletteItem::PluginCommand(id) => (
                self.settings.recent_commands.iter().any(|entry| entry == id),
                self.plugins.command_category_of(id).unwrap_or("插件"),
            ),
            _ => (false, ""),
        }
    }

    fn palette_row(
        &self,
        ui: &mut Ui,
        style: &UiStyle,
        rect: Rect,
        item: &PaletteItem,
        selected: bool,
    ) {
        let theme = style.theme;
        let raw_query = self.palette.query.trim().to_string();
        let (query, _) = split_file_line(&raw_query);
        let query = query.to_string();
        let mut x = rect.left() + 16.0;
        let mut shortcut_text = String::new();
        match item {
            PaletteItem::File(path) => {
                icons::paint_path_icon(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(15.0)),
                    path,
                    false,
                    false,
                    theme.text_dim,
                    theme.kind.is_dark(),
                );
                x += 24.0;
                let rel = self
                    .workspace
                    .root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let dir_chars = rel
                    .rfind(['/', '\\'])
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let split: String = rel.chars().take(dir_chars).collect();
                let name: String = rel.chars().skip(dir_chars).collect();
                let dir_max = (rect.right() - x - 140.0).max(0.0);
                if !split.is_empty() {
                    let (galley, cut) =
                        widgets::ellipsize(ui, &split, style.small(), theme.text_faint, dir_max);
                    let width = galley.rect.width();
                    ui.painter().galley(
                        Pos2::new(x, rect.center().y - galley.rect.height() * 0.5),
                        galley,
                        theme.text_faint,
                    );
                    x += width;
                    if cut {
                        x -= 2.0;
                    }
                }
                let positions: Vec<usize> = crate::fuzzy::fuzzy_match(&query, &rel)
                    .map(|m| m.positions)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|index| *index >= dir_chars)
                    .map(|index| index - dir_chars)
                    .collect();
                let name_color = if selected { theme.text } else { theme.text_dim };
                let name_clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    &name,
                    &positions,
                    x,
                    rect.center().y,
                    name_clip,
                    name_color,
                    theme.accent,
                );
            }
            PaletteItem::Command(command) => {
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    command.icon(),
                    if selected {
                        theme.accent
                    } else {
                        theme.text_dim
                    },
                    1.2,
                );
                x += 22.0;
                // The empty-query list is grouped by category headers, so the inline
                // tag would only repeat the header.
                if !self.palette.query.trim().is_empty() {
                    let tag = command.category();
                    let tag_width =
                        paint_tag(ui, style, tag, x, rect.center().y, theme.text_faint);
                    x += tag_width + 8.0;
                }
                shortcut_text = self
                    .custom_shortcuts
                    .get(command)
                    .map(|shortcut| shortcut.display())
                    .or_else(|| command.shortcut_text())
                    .unwrap_or_default();
                let positions = crate::fuzzy::fuzzy_match(&query, command.label())
                    .map(|m| m.positions)
                    .unwrap_or_default();
                let color = if selected { theme.text } else { theme.text_dim };
                let clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    command.label(),
                    &positions,
                    x,
                    rect.center().y,
                    clip,
                    color,
                    theme.accent,
                );
            }
            PaletteItem::PluginCommand(id) => {
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    Icon::Extensions,
                    if selected {
                        theme.accent
                    } else {
                        theme.text_dim
                    },
                    1.2,
                );
                x += 22.0;
                if !self.palette.query.trim().is_empty() {
                    let tag = self
                        .plugins
                        .command_category_of(id)
                        .unwrap_or_default()
                        .to_string();
                    let tag_width =
                        paint_tag(ui, style, &tag, x, rect.center().y, theme.text_faint);
                    x += tag_width + 8.0;
                }
                let (primary, secondary) = self.palette_text(item);
                shortcut_text = secondary;
                let positions = crate::fuzzy::fuzzy_match(&query, &primary)
                    .map(|m| m.positions)
                    .unwrap_or_default();
                let color = if selected { theme.text } else { theme.text_dim };
                let clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    &primary,
                    &positions,
                    x,
                    rect.center().y,
                    clip,
                    color,
                    theme.accent,
                );
            }
            PaletteItem::Language(_) => {
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    Icon::FileCode,
                    theme.text_dim,
                    1.3,
                );
                x += 22.0;
                let (primary, secondary) = self.palette_text(item);
                let positions = crate::fuzzy::fuzzy_match(&query, &primary)
                    .map(|m| m.positions)
                    .unwrap_or_default();
                let color = if selected { theme.text } else { theme.text_dim };
                let clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    &primary,
                    &positions,
                    x,
                    rect.center().y,
                    clip,
                    color,
                    theme.accent,
                );
                shortcut_text = secondary;
            }
            PaletteItem::Symbol(sym) => {
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    sym.kind.icon(),
                    theme.accent,
                    1.2,
                );
                x += 22.0;
                let positions = crate::fuzzy::fuzzy_match(&query, &sym.name)
                    .map(|m| m.positions)
                    .unwrap_or_default();
                let color = if selected { theme.text } else { theme.text_dim };
                let clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    &sym.name,
                    &positions,
                    x,
                    rect.center().y,
                    clip,
                    color,
                    theme.accent,
                );
                shortcut_text = format!("第 {} 行", sym.start_line + 1);
            }
            PaletteItem::Branch(branch) => {
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    Icon::GitBranch,
                    theme.accent,
                    1.2,
                );
                x += 22.0;
                let positions = crate::fuzzy::fuzzy_match(&query, branch)
                    .map(|m| m.positions)
                    .unwrap_or_default();
                let color = if selected { theme.text } else { theme.text_dim };
                let clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(rect.right() - 12.0, rect.bottom()),
                );
                paint_highlighted(
                    ui,
                    style,
                    branch,
                    &positions,
                    x,
                    rect.center().y,
                    clip,
                    color,
                    theme.accent,
                );
                if *branch == self.git.snapshot.branch {
                    shortcut_text = "当前分支".to_string();
                }
            }
        }

        if !shortcut_text.is_empty() {
            let galley = ui.ctx().fonts_mut(|fonts| {
                fonts.layout_job(LayoutJob::simple_singleline(
                    shortcut_text,
                    style.tiny(),
                    theme.text_faint,
                ))
            });
            let width = galley.rect.width();
            ui.painter().galley(
                Pos2::new(
                    rect.right() - 14.0 - width,
                    rect.center().y - galley.rect.height() * 0.5,
                ),
                galley,
                theme.text_faint,
            );
        }
    }
}

/// Paint a faint category tag and return its pixel width.
fn paint_tag(
    ui: &mut Ui,
    style: &UiStyle,
    text: &str,
    x: f32,
    center_y: f32,
    color: egui::Color32,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_job(LayoutJob::simple_singleline(
            text.to_string(),
            style.tiny(),
            color,
        ))
    });
    let width = galley.rect.width();
    ui.painter().galley(
        Pos2::new(x, center_y - galley.rect.height() * 0.5),
        galley,
        color,
    );
    width
}

/// Paint one string, tinting fuzzy-match characters with the accent colour.
#[allow(clippy::too_many_arguments)]
fn paint_highlighted(
    ui: &Ui,
    style: &UiStyle,
    text: &str,
    positions: &[usize],
    start_x: f32,
    center_y: f32,
    clip: Rect,
    base_color: egui::Color32,
    match_color: egui::Color32,
) {
    let painter = ui.painter().with_clip_rect(clip);
    let font = style.font(13.0);
    let chars: Vec<char> = text.chars().collect();
    let mut x = start_x;
    let mut run_start = 0usize;
    let mut run_is_match = positions.contains(&0);
    for index in 0..chars.len() {
        let is_match = positions.contains(&index);
        if is_match != run_is_match {
            let run: String = chars[run_start..index].iter().collect();
            let color = if run_is_match { match_color } else { base_color };
            let width = paint_run(ui, &painter, &run, &font, x, center_y, color);
            x += width;
            run_start = index;
            run_is_match = is_match;
        }
    }
    let run: String = chars[run_start..].iter().collect();
    let color = if run_is_match { match_color } else { base_color };
    let _ = paint_run(ui, &painter, &run, &font, x, center_y, color);
}

fn paint_run(
    ui: &Ui,
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
    x: f32,
    center_y: f32,
    color: egui::Color32,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_job(LayoutJob::simple_singleline(
            text.to_string(),
            font.clone(),
            color,
        ))
    });
    let width = galley.rect.width();
    painter.galley(
        Pos2::new(x, center_y - galley.rect.height() * 0.5),
        galley,
        color,
    );
    width
}
