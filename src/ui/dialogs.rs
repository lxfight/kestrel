//! Modal dialogs, confirmations, toasts.

use egui::{
    Align2, Area, Color32, CornerRadius, Frame, Id, Modal, Order, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, TextEdit, Ui, Vec2,
};

use crate::app::{Dialog, KestrelApp, ToastKind};
use crate::commands::Command;
use crate::icons::{self, Icon};
use crate::settings::Settings;
use crate::theme::ThemeKind;
use crate::ui::UiStyle;

impl KestrelApp {
    pub fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let style = self.ui_style();
        let theme = style.theme;
        let mut keep = true;
        match &dialog {
            Dialog::About => {
                let modal = Modal::new(Id::unique("about_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(400.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(6.0);
                            let (logo_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(54.0), Sense::hover());
                            ui.painter().rect_filled(
                                logo_rect,
                                CornerRadius::same(14),
                                theme.accent,
                            );
                            ui.painter().text(
                                logo_rect.center(),
                                Align2::CENTER_CENTER,
                                "K",
                                style.font(27.0),
                                theme.accent_text,
                            );
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("Kestrel")
                                    .font(style.font(23.0))
                                    .color(theme.text),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "版本 {} · 使用 Rust 构建",
                                    crate::app::APP_VERSION
                                ))
                                .font(style.small())
                                .color(theme.text_dim),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("轻量、极速的代码编辑器")
                                    .font(style.small())
                                    .color(theme.text_faint),
                            );
                            ui.add_space(16.0);
                            ui.separator();
                            ui.add_space(10.0);
                            for line in [
                                "GPU 渲染 · 虚拟化文本布局 · 6 套精选专业主题",
                                "Rope 缓冲 · 多光标编辑 · 代码折叠 · 粘性滚动",
                                "tree-sitter 16 语言增量高亮 · 智能补全 · 符号大纲",
                                "原生 Git 集成 · 跨文件全局替换 · 多会话内置终端",
                                "MIT License",
                            ] {
                                ui.label(
                                    egui::RichText::new(line)
                                        .font(style.small())
                                        .color(theme.text_dim),
                                );
                            }
                            ui.add_space(18.0);
                            if Self::modal_button(ui, &style, "关闭", true) {
                                keep = false;
                            }
                        });
                    });
                if modal.should_close() {
                    keep = false;
                }
            }
            Dialog::Shortcuts => {
                let modal = Modal::new(Id::unique("shortcuts_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(560.0);
                        ui.label(
                            egui::RichText::new("键盘快捷键")
                                .font(style.font(17.0))
                                .color(theme.text),
                        );
                        ui.add_space(10.0);
                        ScrollArea::vertical()
                            .max_height(440.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut last_category = "";
                                for command in Command::ALL {
                                    let Some(shortcut) = command.shortcut_text() else {
                                        continue;
                                    };
                                    if command.category() != last_category {
                                        last_category = command.category();
                                        ui.add_space(10.0);
                                        ui.label(
                                            egui::RichText::new(last_category)
                                                .font(style.font(11.0))
                                                .color(theme.text_faint),
                                        );
                                        ui.add_space(2.0);
                                    }
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 26.0),
                                        Sense::hover(),
                                    );
                                    if resp.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            CornerRadius::same(4),
                                            theme.hover,
                                        );
                                    }
                                    icons::paint(
                                        ui.painter(),
                                        egui::Rect::from_center_size(
                                            Pos2::new(rect.left() + 14.0, rect.center().y),
                                            Vec2::splat(14.0),
                                        ),
                                        command.icon(),
                                        theme.accent,
                                        1.2,
                                    );
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 30.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        command.label(),
                                        style.small(),
                                        theme.text_dim,
                                    );
                                    let pill_w = (shortcut.len() as f32 * 6.8 + 14.0).max(36.0);
                                    let pill = egui::Rect::from_center_size(
                                        Pos2::new(rect.right() - pill_w * 0.5 - 6.0, rect.center().y),
                                        Vec2::new(pill_w, 19.0),
                                    );
                                    ui.painter().rect_filled(
                                        pill,
                                        CornerRadius::same(4),
                                        theme.input_bg,
                                    );
                                    ui.painter().rect_stroke(
                                        pill,
                                        CornerRadius::same(4),
                                        Stroke::new(1.0, theme.border_strong),
                                        egui::StrokeKind::Inside,
                                    );
                                    ui.painter().text(
                                        pill.center(),
                                        Align2::CENTER_CENTER,
                                        shortcut,
                                        style.tiny(),
                                        theme.text,
                                    );
                                }
                            });
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            if Self::modal_button(ui, &style, "关闭", true) {
                                keep = false;
                            }
                        });
                    });
                if modal.should_close() {
                    keep = false;
                }
            }
            Dialog::Settings => {
                let mut reset_defaults = false;
                let modal = Modal::new(Id::unique("settings_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(600.0);
                        keep &= self.settings_modal_ui(ui, &style, &mut reset_defaults);
                    });
                if reset_defaults {
                    let defaults = Settings::default();
                    self.settings.theme = defaults.theme;
                    self.settings.font_size = defaults.font_size;
                    self.settings.terminal_font_size = defaults.terminal_font_size;
                    self.settings.ui_zoom = defaults.ui_zoom;
                    ctx.set_zoom_factor(self.settings.ui_zoom);
                    self.settings.line_height = defaults.line_height;
                    self.settings.tab_size = defaults.tab_size;
                    self.settings.insert_spaces = defaults.insert_spaces;
                    self.settings.show_minimap = defaults.show_minimap;
                    self.settings.show_line_numbers = defaults.show_line_numbers;
                    self.settings.show_indent_guides = defaults.show_indent_guides;
                    self.settings.show_whitespace = defaults.show_whitespace;
                    self.settings.show_sticky_scroll = defaults.show_sticky_scroll;
                    self.settings.show_git_gutter = defaults.show_git_gutter;
                    self.settings.autocomplete = defaults.autocomplete;
                    self.settings.word_wrap = defaults.word_wrap;
                    self.settings.scroll_past_end = defaults.scroll_past_end;
                    self.settings.show_hidden_files = defaults.show_hidden_files;
                    self.settings.autosave = defaults.autosave;
                    self.workspace.tree.show_hidden = self.settings.show_hidden_files;
                    self.workspace.tree.reload();
                    self.settings.save();
                    self.toast("已恢复默认设置", ToastKind::Info);
                }
                if modal.should_close() {
                    keep = false;
                }
            }
            Dialog::ConfirmClose => {
                let dirty = self.dirty_tabs();
                let mut action: Option<u8> = None;
                Modal::new(Id::unique("confirm_close_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(420.0);
                        ui.label(
                            egui::RichText::new("有未保存的文件")
                                .font(style.font(16.0))
                                .color(theme.text),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("退出前是否保存更改？")
                                .font(style.small())
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        for name in dirty.iter().take(8) {
                            ui.label(
                                egui::RichText::new(format!("•  {name}"))
                                    .font(style.small())
                                    .color(theme.text_dim),
                            );
                        }
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "保存并退出", true) {
                                action = Some(0);
                            }
                            if Self::modal_button(ui, &style, "不保存", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                        });
                    });
                match action {
                    Some(0) => {
                        self.save_all();
                        if self.tabs.iter().any(|t| t.is_dirty()) {
                            self.toast("存在无法自动保存的未命名文件", ToastKind::Warning);
                        } else {
                            self.close_confirmed = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            keep = false;
                        }
                    }
                    Some(1) => {
                        self.close_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        keep = false;
                    }
                    Some(2) => keep = false,
                    None => {}
                    _ => {}
                }
            }
            Dialog::ConfirmCloseAll => {
                let dirty = self.dirty_tabs();
                let mut action: Option<u8> = None;
                Modal::new(Id::unique("confirm_close_all_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(420.0);
                        ui.label(
                            egui::RichText::new("有未保存的文件")
                                .font(style.font(16.0))
                                .color(theme.text),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("关闭所有编辑器不会退出应用。")
                                .font(style.small())
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        for name in dirty.iter().take(8) {
                            ui.label(
                                egui::RichText::new(format!("•  {name}"))
                                    .font(style.small())
                                    .color(theme.text_dim),
                            );
                        }
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "保存并关闭", true) {
                                action = Some(0);
                            }
                            if Self::modal_button(ui, &style, "全部关闭", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                        });
                    });
                match action {
                    Some(0) => {
                        self.save_all();
                        if self.tabs.iter().any(|t| t.is_dirty()) {
                            self.toast("存在无法自动保存的未命名文件", ToastKind::Warning);
                        } else {
                            self.close_all_tabs_force();
                            keep = false;
                        }
                    }
                    Some(1) => {
                        self.close_all_tabs_force();
                        keep = false;
                    }
                    Some(2) => keep = false,
                    None => {}
                    _ => {}
                }
            }
            Dialog::ConfirmCloseTab(group, index) => {
                let group = *group;
                let index = *index;
                let title = self.tabs.get(index).map(|t| t.title()).unwrap_or_default();
                let mut action: Option<u8> = None;
                Modal::new(Id::unique("confirm_close_tab_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(400.0);
                        ui.label(
                            egui::RichText::new(format!("是否保存对 {title} 的更改？"))
                                .font(style.font(15.0))
                                .color(theme.text),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "保存", true) {
                                action = Some(0);
                            }
                            if Self::modal_button(ui, &style, "不保存", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                        });
                    });
                match action {
                    Some(0) => {
                        if index < self.tabs.len() {
                            self.active = index;
                            if self.tabs[index].doc.path.is_some() {
                                self.save_active();
                                if !self.tabs[index].is_dirty() {
                                    self.close_tab_force_in_group(group, index);
                                    keep = false;
                                }
                            } else {
                                self.save_active_as();
                                if self.tabs.get(index).is_some_and(|t| !t.is_dirty()) {
                                    self.close_tab_force_in_group(group, index);
                                    keep = false;
                                }
                            }
                        }
                    }
                    Some(1) => {
                        if index < self.tabs.len() {
                            self.close_tab_force_in_group(group, index);
                        }
                        keep = false;
                    }
                    Some(2) => keep = false,
                    None => {}
                    _ => {}
                }
            }
            Dialog::ConfirmDelete(path) => {
                let path = path.clone();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut action: Option<bool> = None;
                Modal::new(Id::unique("confirm_delete_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(400.0);
                        ui.label(
                            egui::RichText::new(format!("确定要删除 “{name}” 吗？"))
                                .font(style.font(15.0))
                                .color(theme.text),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("此操作不可撤销。")
                                .font(style.small())
                                .color(theme.warning),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "删除", true) {
                                action = Some(true);
                            }
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(false);
                            }
                        });
                    });
                if let Some(confirm) = action {
                    keep = false;
                    if confirm {
                        self.delete_path(path);
                    }
                }
            }
            Dialog::ConfirmOpenFolder(path) => {
                let path = path.clone();
                let mut action: Option<u8> = None;
                Modal::new(Id::unique("confirm_folder_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(420.0);
                        ui.label(
                            egui::RichText::new("当前有未保存的文件")
                                .font(style.font(15.0))
                                .color(theme.text),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("打开其他文件夹会关闭当前的所有编辑器。")
                                .font(style.small())
                                .color(theme.text_dim),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "保存并打开", true) {
                                action = Some(0);
                            }
                            if Self::modal_button(ui, &style, "放弃更改", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                        });
                    });
                match action {
                    Some(0) => {
                        self.save_all();
                        if self.tabs.iter().any(|t| t.is_dirty()) {
                            self.toast("存在无法自动保存的未命名文件", ToastKind::Warning);
                        } else {
                            self.open_folder_quiet(path);
                            keep = false;
                        }
                    }
                    Some(1) => {
                        self.open_folder_quiet(path);
                        keep = false;
                    }
                    Some(2) => keep = false,
                    None => {}
                    _ => {}
                }
            }
        }
        if keep {
            self.dialog = Some(dialog);
        }
    }

    fn modal_frame(style: &UiStyle) -> Frame {
        let theme = style.theme;
        Frame::new()
            .fill(theme.overlay_bg)
            .stroke(Stroke::new(1.0, theme.border_strong))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::same(22))
    }

    fn modal_button(ui: &mut Ui, style: &UiStyle, label: &str, primary: bool) -> bool {
        let theme = style.theme;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(104.0, 30.0), Sense::click());
        let fill = if primary {
            if resp.hovered() {
                theme.accent.gamma_multiply(0.9)
            } else {
                theme.accent
            }
        } else if resp.hovered() {
            theme.hover
        } else {
            theme.input_bg
        };
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        if !primary {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(1.0, theme.border_strong),
                egui::StrokeKind::Inside,
            );
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            style.small(),
            if primary {
                theme.accent_text
            } else {
                theme.text
            },
        );
        resp.clicked()
    }

    fn settings_modal_ui(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        reset_defaults: &mut bool,
    ) -> bool {
        let theme = style.theme;
        let mut keep = true;

        // Window header
        let (hdr, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::hover());
        icons::paint(
            ui.painter(),
            Rect::from_center_size(Pos2::new(hdr.left() + 10.0, hdr.center().y), Vec2::splat(18.0)),
            Icon::Settings,
            theme.accent,
            1.4,
        );
        ui.painter().text(
            Pos2::new(hdr.left() + 26.0, hdr.center().y),
            Align2::LEFT_CENTER,
            "偏好设置",
            style.font(17.0),
            theme.text,
        );
        ui.painter().text(
            Pos2::new(hdr.left() + 104.0, hdr.center().y + 1.0),
            Align2::LEFT_CENTER,
            "所有更改自动保存",
            style.tiny(),
            theme.text_faint,
        );
        let close_rect = Rect::from_center_size(
            Pos2::new(hdr.right() - 14.0, hdr.center().y),
            Vec2::splat(26.0),
        );
        let close_resp = ui.interact(close_rect, ui.scope_id().with("settings_close_x"), Sense::click());
        if close_resp.hovered() {
            ui.painter()
                .rect_filled(close_rect, CornerRadius::same(6), theme.hover);
        }
        icons::paint(
            ui.painter(),
            close_rect.shrink(6.0),
            Icon::Close,
            if close_resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
            1.4,
        );
        if close_resp.on_hover_text("关闭 (Esc)").clicked() {
            keep = false;
        }

        ui.add_space(6.0);
        ui.add_sized(
            Vec2::new(ui.available_width(), 30.0),
            TextEdit::singleline(&mut self.settings_search)
                .hint_text("搜索设置项 (如：主题、字体、小地图、自动保存)…")
                .font(style.small())
                .margin(Vec2::new(10.0, 6.0)),
        );
        ui.add_space(8.0);

        let query = self.settings_search.trim().to_lowercase();
        let matches = |label: &str| -> bool {
            query.is_empty() || label.to_lowercase().contains(&query)
        };

        let mut settings = self.settings.clone();
        let mut changed = false;

        ScrollArea::vertical()
            .max_height(430.0)
            .auto_shrink([false, false])
            .id_salt("settings_window_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                let show_appearance = matches("主题模式 深色 浅色 theme")
                    || matches("字体大小 font_size px")
                    || matches("终端字体 terminal_font_size px")
                    || matches("界面缩放 ui_zoom scale")
                    || matches("行高倍数 line_height")
                    || matches("缩进空格数 tab size");
                if show_appearance {
                    Self::settings_section_title(ui, style, "外观与排版");
                    if matches("主题模式 深色 浅色 theme") {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new("主题模式")
                                    .font(style.small())
                                    .color(theme.text_dim),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(6.0);
                                let current_kind = ThemeKind::from_str(&settings.theme);
                                for (kind, icon, label) in [
                                    (ThemeKind::Light, Icon::Sun, "浅色模式"),
                                    (ThemeKind::Dark, Icon::Moon, "深色模式"),
                                ] {
                                    let active = current_kind == kind;
                                    let (btn_rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(112.0, 28.0),
                                        Sense::click(),
                                    );
                                    let fill = if active {
                                        theme.accent
                                    } else if resp.hovered() {
                                        theme.hover
                                    } else {
                                        theme.input_bg
                                    };
                                    ui.painter()
                                        .rect_filled(btn_rect, CornerRadius::same(6), fill);
                                    if !active {
                                        ui.painter().rect_stroke(
                                            btn_rect,
                                            CornerRadius::same(6),
                                            Stroke::new(1.0, theme.border_strong),
                                            StrokeKind::Inside,
                                        );
                                    }
                                    let fg = if active {
                                        theme.accent_text
                                    } else {
                                        theme.text
                                    };
                                    icons::paint(
                                        ui.painter(),
                                        Rect::from_center_size(
                                            Pos2::new(btn_rect.center().x - 24.0, btn_rect.center().y),
                                            Vec2::splat(13.0),
                                        ),
                                        icon,
                                        fg,
                                        1.3,
                                    );
                                    ui.painter().text(
                                        Pos2::new(btn_rect.center().x + 8.0, btn_rect.center().y),
                                        Align2::CENTER_CENTER,
                                        label,
                                        style.small(),
                                        fg,
                                    );
                                    if resp.clicked() && !active {
                                        settings.theme = kind.as_str().to_string();
                                        changed = true;
                                    }
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                    if matches("字体大小 font_size px") {
                        let mut fsize = settings.font_size.round() as i32;
                        if Self::settings_stepper(
                            ui,
                            style,
                            "编辑器字体大小 (px)",
                            &fsize.to_string(),
                            &mut fsize,
                            9,
                            30,
                        ) {
                            settings.font_size = fsize as f32;
                            changed = true;
                        }
                    }
                    if matches("终端字体 terminal_font_size px") {
                        let mut tsize = settings.terminal_font_size.round() as i32;
                        if Self::settings_stepper(
                            ui,
                            style,
                            "终端字体大小 (px)",
                            &tsize.to_string(),
                            &mut tsize,
                            9,
                            28,
                        ) {
                            settings.terminal_font_size = tsize as f32;
                            changed = true;
                        }
                    }
                    if matches("界面缩放 ui_zoom scale") {
                        let mut zoom_steps = (settings.ui_zoom * 10.0).round() as i32;
                        if Self::settings_stepper(
                            ui,
                            style,
                            "界面缩放比例",
                            &format!("{}%", zoom_steps * 10),
                            &mut zoom_steps,
                            7,
                            18,
                        ) {
                            settings.ui_zoom = zoom_steps as f32 / 10.0;
                            changed = true;
                        }
                    }
                    if matches("行高倍数 line_height") {
                        let mut tenths = (settings.line_height * 10.0).round() as i32;
                        let display = format!("{:.1}x", tenths as f32 / 10.0);
                        if Self::settings_stepper(
                            ui,
                            style,
                            "行高倍数 (Line Height)",
                            &display,
                            &mut tenths,
                            11,
                            22,
                        ) {
                            settings.line_height = tenths as f32 / 10.0;
                            changed = true;
                        }
                    }
                    if matches("缩进空格数 tab size") {
                        let mut tsize = settings.tab_size as i32;
                        if Self::settings_stepper(
                            ui,
                            style,
                            "缩进空格数 (Tab Size)",
                            &tsize.to_string(),
                            &mut tsize,
                            2,
                            8,
                        ) {
                            settings.tab_size = tsize as usize;
                            changed = true;
                        }
                    }
                }

                let editor_items: Vec<(&str, bool)> = vec![
                    ("自动代码补全 (Autocomplete)", true),
                    ("粘性滚动 (Sticky Scroll)", true),
                    ("自动换行 (Word Wrap)", true),
                    ("显示行号 (Line Numbers)", true),
                    ("显示小地图 (Minimap)", true),
                    ("Git 行内差异指示 (Git Gutter)", true),
                    ("显示缩进参考线 (Indent Guides)", true),
                    ("显示空白字符 (Whitespace)", true),
                    ("滚动到行尾之外 (Scroll Past End)", true),
                    ("使用空格缩进 (Insert Spaces)", true),
                ];
                if editor_items.iter().any(|(l, _)| matches(l)) {
                    Self::settings_section_title(ui, style, "编辑器与智能辅助");
                    if matches("自动代码补全 (Autocomplete)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "自动代码补全 (Autocomplete)",
                            &mut settings.autocomplete,
                        );
                    }
                    if matches("粘性滚动 (Sticky Scroll)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "粘性滚动 (Sticky Scroll)",
                            &mut settings.show_sticky_scroll,
                        );
                    }
                    if matches("自动换行 (Word Wrap)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "自动换行 (Word Wrap)",
                            &mut settings.word_wrap,
                        );
                    }
                    if matches("显示行号 (Line Numbers)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "显示行号 (Line Numbers)",
                            &mut settings.show_line_numbers,
                        );
                    }
                    if matches("显示小地图 (Minimap)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "显示小地图 (Minimap)",
                            &mut settings.show_minimap,
                        );
                    }
                    if matches("Git 行内差异指示 (Git Gutter)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "Git 行内差异指示 (Git Gutter)",
                            &mut settings.show_git_gutter,
                        );
                    }
                    if matches("显示缩进参考线 (Indent Guides)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "显示缩进参考线 (Indent Guides)",
                            &mut settings.show_indent_guides,
                        );
                    }
                    if matches("显示空白字符 (Whitespace)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "显示空白字符 (Whitespace)",
                            &mut settings.show_whitespace,
                        );
                    }
                    if matches("滚动到行尾之外 (Scroll Past End)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "滚动到行尾之外 (Scroll Past End)",
                            &mut settings.scroll_past_end,
                        );
                    }
                    if matches("使用空格缩进 (Insert Spaces)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "使用空格缩进 (Insert Spaces)",
                            &mut settings.insert_spaces,
                        );
                    }
                }

                if matches("自动保存 (Autosave)") || matches("显示隐藏文件 (Hidden Files)") {
                    Self::settings_section_title(ui, style, "工作区与文件");
                    if matches("自动保存 (Autosave)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "自动保存 (Autosave)",
                            &mut settings.autosave,
                        );
                    }
                    if matches("显示隐藏文件 (Hidden Files)") {
                        changed |= Self::settings_switch(
                            ui,
                            style,
                            "显示隐藏文件 (Hidden Files)",
                            &mut settings.show_hidden_files,
                        );
                    }
                }

                if matches("配置目录 config about 关于") {
                    Self::settings_section_title(ui, style, "配置与信息");
                    ui.add_space(4.0);
                    let cfg_dir = Settings::config_dir();
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!("配置目录: {}", cfg_dir.display()))
                                .font(style.tiny())
                                .color(theme.text_faint),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(6.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("打开配置目录")
                                            .font(style.tiny())
                                            .color(theme.text_dim),
                                    )
                                    .fill(theme.input_bg)
                                    .stroke(Stroke::new(1.0, theme.border_strong)),
                                )
                                .clicked()
                            {
                                let _ = std::fs::create_dir_all(&cfg_dir);
                                crate::workspace::tree::reveal_in_explorer(&cfg_dir);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Kestrel {} · Rust · GPU Wgpu",
                                crate::app::APP_VERSION
                            ))
                            .font(style.tiny())
                            .color(theme.text_faint),
                        );
                    });
                }
                ui.add_space(8.0);
            });

        if changed {
            if self.settings.show_hidden_files != settings.show_hidden_files {
                self.workspace.tree.show_hidden = settings.show_hidden_files;
                self.workspace.tree.reload();
            }
            self.settings = settings;
            self.settings.save();
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if Self::modal_button(ui, style, "恢复默认", false) {
                *reset_defaults = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Self::modal_button(ui, style, "关闭", true) {
                    keep = false;
                }
            });
        });

        keep
    }

    fn settings_section_title(ui: &mut Ui, style: &UiStyle, title: &str) {
        let theme = style.theme;
        ui.add_space(8.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            style.font(11.5),
            theme.accent,
        );
        ui.painter().line_segment(
            [
                Pos2::new(rect.left() + 6.0, rect.bottom()),
                Pos2::new(rect.right() - 6.0, rect.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );
    }

    fn settings_stepper(
        ui: &mut Ui,
        style: &UiStyle,
        label: &str,
        display_value: &str,
        value: &mut i32,
        min: i32,
        max: i32,
    ) -> bool {
        let theme = style.theme;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            style.small(),
            theme.text_dim,
        );

        let minus_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 78.0, rect.center().y),
            Vec2::new(22.0, 22.0),
        );
        let plus_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 16.0, rect.center().y),
            Vec2::new(22.0, 22.0),
        );
        let m_resp = ui.interact(
            minus_rect,
            ui.scope_id().with(("step_minus", label)),
            Sense::click(),
        );
        let p_resp = ui.interact(
            plus_rect,
            ui.scope_id().with(("step_plus", label)),
            Sense::click(),
        );

        for (r, h) in [(minus_rect, m_resp.hovered()), (plus_rect, p_resp.hovered())] {
            ui.painter().rect_filled(
                r,
                CornerRadius::same(4),
                if h { theme.hover } else { theme.input_bg },
            );
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(1.0, theme.border_strong),
                StrokeKind::Inside,
            );
        }
        ui.painter().text(
            minus_rect.center(),
            Align2::CENTER_CENTER,
            "−",
            style.small(),
            theme.text,
        );
        ui.painter().text(
            Pos2::new(rect.right() - 47.0, rect.center().y),
            Align2::CENTER_CENTER,
            display_value,
            style.small(),
            theme.text,
        );
        ui.painter().text(
            plus_rect.center(),
            Align2::CENTER_CENTER,
            "+",
            style.small(),
            theme.text,
        );

        if m_resp.clicked() && *value > min {
            *value -= 1;
            return true;
        }
        if p_resp.clicked() && *value < max {
            *value += 1;
            return true;
        }
        false
    }

    fn settings_switch(ui: &mut Ui, style: &UiStyle, label: &str, value: &mut bool) -> bool {
        let theme = style.theme;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(5), theme.hover);
        }
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            style.small(),
            theme.text_dim,
        );
        let switch = Rect::from_center_size(
            Pos2::new(rect.right() - 22.0, rect.center().y),
            Vec2::new(32.0, 17.0),
        );
        let on = *value;
        ui.painter().rect_filled(
            switch,
            CornerRadius::same(8),
            if on {
                theme.accent
            } else {
                theme.border_strong
            },
        );
        let knob_x = if on {
            switch.right() - 8.5
        } else {
            switch.left() + 8.5
        };
        ui.painter()
            .circle_filled(Pos2::new(knob_x, switch.center().y), 6.2, Color32::WHITE);
        if resp.clicked() {
            *value = !*value;
            return true;
        }
        false
    }

    pub fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let style = self.ui_style();
        let theme = style.theme;
        let mut dismiss: Option<usize> = None;
        Area::new(Id::unique("toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -36.0))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for (index, toast) in self.toasts.iter().enumerate() {
                        let frame = Frame::new()
                            .fill(theme.overlay_bg)
                            .stroke(Stroke::new(1.0, theme.border_strong))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 6],
                                blur: 18,
                                spread: 0,
                                color: theme.shadow,
                            });
                        let resp = frame
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (icon, color) = match toast.kind {
                                        ToastKind::Info => (Icon::Info, theme.accent),
                                        ToastKind::Success => (Icon::Info, theme.success),
                                        ToastKind::Warning => (Icon::Warning, theme.warning),
                                        ToastKind::Error => (Icon::Error, theme.error),
                                    };
                                    let (r, _) =
                                        ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                                    icons::paint(ui.painter(), r, icon, color, 1.4);
                                    ui.label(
                                        egui::RichText::new(&toast.text)
                                            .font(style.small())
                                            .color(theme.text),
                                    );
                                });
                            })
                            .response;
                        if resp.clicked() {
                            dismiss = Some(index);
                        }
                    }
                });
            });
        if let Some(index) = dismiss {
            self.toasts.remove(index);
        }
    }
}
