//! Command palette / quick open / goto line / symbol outline / branch switcher overlay.

use egui::{
    Align2, Area, CornerRadius, Frame, Key, Margin, Order, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use crate::app::{KestrelApp, PaletteItem, PaletteMode};
use crate::icons::{self, Icon};
use crate::ui::UiStyle;

const MAX_ROWS: usize = 12;
const ROW_H: f32 = 30.0;

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
                    "按文件名搜索"
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
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(596.0, 42.0), Sense::click());
                        let input_id = ui.scope_id().with("palette_query");
                        if self.palette.focus {
                            ui.memory_mut(|m| m.request_focus(input_id));
                            self.palette.focus = false;
                        }
                        if resp.clicked() {
                            ui.memory_mut(|m| m.request_focus(input_id));
                        }
                        let focused = ui.memory(|m| m.focused()) == Some(input_id);
                        if focused {
                            let events = ui.input(|i| i.events.clone());
                            for event in events {
                                match event {
                                    egui::Event::Text(text) => {
                                        self.palette.query.push_str(&text);
                                        self.palette.needs_filter = true;
                                        self.palette.selected = 0;
                                    }
                                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                                        self.palette.query.push_str(&text);
                                        self.palette.needs_filter = true;
                                        self.palette.selected = 0;
                                    }
                                    egui::Event::Paste(text) => {
                                        self.palette.query.push_str(&text);
                                        self.palette.needs_filter = true;
                                        self.palette.selected = 0;
                                    }
                                    egui::Event::Key {
                                        key: Key::Backspace,
                                        pressed: true,
                                        ..
                                    } => {
                                        self.palette.query.pop();
                                        self.palette.needs_filter = true;
                                        self.palette.selected = 0;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(rect.left() + 16.0, rect.center().y),
                            Vec2::splat(16.0),
                        );
                        icons::paint(
                            &ui.painter(),
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
                        let shown = if self.palette.query.is_empty() {
                            query_hint.to_string()
                        } else {
                            self.palette.query.clone()
                        };
                        let color = if self.palette.query.is_empty() {
                            theme.text_faint
                        } else {
                            theme.text
                        };
                        let font = style.font(14.5);
                        ui.painter().text(
                            Pos2::new(rect.left() + 34.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &shown,
                            font.clone(),
                            color,
                        );
                        if focused {
                            let t = ui.input(|i| i.time);
                            if ((t - self.palette.blink_base) * 1.8).fract() < 0.62 {
                                let width = if self.palette.query.is_empty() {
                                    0.0
                                } else {
                                    ui.ctx().fonts_mut(|fonts| {
                                        fonts
                                            .layout_job(egui::text::LayoutJob::simple_singleline(
                                                shown.clone(),
                                                font.clone(),
                                                color,
                                            ))
                                            .rect
                                            .width()
                                    })
                                };
                                let caret_x = rect.left() + 34.0 + width + 1.0;
                                ui.painter().rect_filled(
                                    Rect::from_min_size(
                                        Pos2::new(caret_x, rect.center().y - 10.0),
                                        Vec2::new(2.0, 20.0),
                                    ),
                                    CornerRadius::same(1),
                                    theme.accent,
                                );
                            }
                        }
                        if resp.clicked() {
                            self.palette.blink_base = ui.input(|i| i.time);
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
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
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
                        ui.add_space(14.0);
                        return;
                    }

                    ui.add_space(4.0);
                    let start = self.palette.window_start;
                    let end = (start + MAX_ROWS).min(count);
                    for index in start..end {
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
                        let (row_rect, row_resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            Sense::click(),
                        );
                        let hovered = row_resp.hovered();
                        if selected || hovered {
                            ui.painter().rect_filled(
                                row_rect.shrink2(Vec2::new(6.0, 1.0)),
                                CornerRadius::same(6),
                                if selected { theme.active } else { theme.hover },
                            );
                        }
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
                        ui.label(
                            egui::RichText::new("↑↓ 选择    Enter 确认    Esc 关闭")
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

    fn palette_row(
        &self,
        ui: &mut Ui,
        style: &UiStyle,
        rect: Rect,
        item: &PaletteItem,
        selected: bool,
    ) {
        let theme = style.theme;
        let (primary, secondary) = self.palette_text(item);
        let query = self.palette.query.trim().to_string();
        let positions: Vec<usize> = match item {
            PaletteItem::File(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                crate::fuzzy::fuzzy_match(&query, &name)
                    .map(|m| m.positions)
                    .unwrap_or_default()
            }
            _ => crate::fuzzy::fuzzy_match(&query, &primary)
                .map(|m| m.positions)
                .unwrap_or_default(),
        };
        let mut x = rect.left() + 16.0;
        match item {
            PaletteItem::File(path) => {
                icons::paint_path_icon(
                    &ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(15.0)),
                    path,
                    false,
                    false,
                    theme.text_dim,
                );
                x += 24.0;
            }
            PaletteItem::Command(command) => {
                icons::paint(
                    &ui.painter(),
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
                let tag = command.category();
                ui.painter().text(
                    Pos2::new(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    tag,
                    style.tiny(),
                    theme.text_faint,
                );
                x += 44.0;
            }
            PaletteItem::PluginCommand(_) => {
                icons::paint(
                    &ui.painter(),
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
                let id = match item {
                    PaletteItem::PluginCommand(id) => id.as_str(),
                    _ => "",
                };
                let tag = self
                    .plugins
                    .command_category_of(id)
                    .unwrap_or_default()
                    .to_string();
                ui.painter().text(
                    Pos2::new(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    tag,
                    style.tiny(),
                    theme.text_faint,
                );
                x += 44.0;
            }
            PaletteItem::Language(_) => {
                icons::paint(
                    &ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    Icon::FileCode,
                    theme.text_dim,
                    1.3,
                );
                x += 22.0;
            }
            PaletteItem::Symbol(sym) => {
                icons::paint(
                    &ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    sym.kind.icon(),
                    theme.accent,
                    1.2,
                );
                x += 22.0;
            }
            PaletteItem::Branch(_) => {
                icons::paint(
                    &ui.painter(),
                    Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
                    Icon::GitBranch,
                    theme.accent,
                    1.2,
                );
                x += 22.0;
            }
        }

        let base_font = style.font(13.0);
        let primary_clip = Rect::from_min_max(
            Pos2::new(x, rect.top()),
            Pos2::new(rect.right() - 120.0, rect.bottom()),
        );
        let painter = ui.painter().with_clip_rect(primary_clip);
        let mut run_start = 0usize;
        let chars: Vec<char> = primary.chars().collect();
        let mut run_is_match = positions.contains(&0);
        for i in 0..chars.len() {
            let is_match = positions.contains(&i);
            if is_match != run_is_match {
                let text: String = chars[run_start..i].iter().collect();
                let color = if run_is_match {
                    theme.accent
                } else if selected {
                    theme.text
                } else {
                    theme.text_dim
                };
                painter.text(
                    Pos2::new(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &text,
                    base_font.clone(),
                    color,
                );
                x += ui.ctx().fonts_mut(|fonts| {
                    fonts
                        .layout_job(egui::text::LayoutJob::simple_singleline(
                            text,
                            base_font.clone(),
                            color,
                        ))
                        .rect
                        .width()
                });
                run_start = i;
                run_is_match = is_match;
            }
        }
        let text: String = chars[run_start..].iter().collect();
        let color = if run_is_match {
            theme.accent
        } else if selected {
            theme.text
        } else {
            theme.text_dim
        };
        painter.text(
            Pos2::new(x, rect.center().y),
            Align2::LEFT_CENTER,
            &text,
            base_font.clone(),
            color,
        );
        if !secondary.is_empty() {
            ui.painter().text(
                Pos2::new(rect.right() - 16.0, rect.center().y),
                Align2::RIGHT_CENTER,
                &secondary,
                style.tiny(),
                theme.text_faint,
            );
        }
    }
}
