//! Modal dialogs, confirmations, toasts.

use egui::{
    Align2, Area, Color32, CornerRadius, Frame, Id, Key, Layout, Modal, Order, Pos2, Rect,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2,
};

use crate::app::{Dialog, KestrelApp, SidebarMode, ToastKind};
use crate::commands::{Command, Shortcut};
use crate::icons::{self, Icon};
use crate::settings::Settings;
use crate::theme::ThemeKind;
use crate::ui::UiStyle;

fn dialog_kind(dialog: &Dialog) -> &'static str {
    match dialog {
        Dialog::About => "about",
        Dialog::Shortcuts => "shortcuts",
        Dialog::Settings => "settings",
        Dialog::ConfirmClose => "confirm_close",
        Dialog::ConfirmCloseAll => "confirm_close_all",
        Dialog::ConfirmCloseTab(_, _) => "confirm_close_tab",
        Dialog::ConfirmDelete(_) => "confirm_delete",
        Dialog::ConfirmDeleteBatch(_) => "confirm_delete_batch",
        Dialog::ConfirmOpenFolder(_) => "confirm_open_folder",
    }
}

impl KestrelApp {
    pub fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else {
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::unique("kestrel_dialog_kind"), String::new());
            });
            return;
        };
        // A freshly opened modal owns the keyboard: drop focus left behind by the
        // editor so Enter activates the primary button.
        let kind = dialog_kind(&dialog).to_string();
        let previous = ctx.data_mut(|data| {
            let id = egui::Id::unique("kestrel_dialog_kind");
            let previous = data.get_temp::<String>(id).unwrap_or_default();
            data.insert_temp(id, kind.clone());
            previous
        });
        if previous != kind
            && let Some(focused) = ctx.memory(|memory| memory.focused())
        {
            ctx.memory_mut(|memory| memory.surrender_focus(focused));
        }
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
                let filter_id = egui::Id::unique("kestrel_shortcuts_filter");
                let recording_id = egui::Id::unique("kestrel_shortcuts_recording");
                let error_id = egui::Id::unique("kestrel_shortcuts_error");
                let mut filter =
                    ctx.data(|data| data.get_temp::<String>(filter_id).unwrap_or_default());
                let mut recording =
                    ctx.data(|data| data.get_temp::<String>(recording_id).unwrap_or_default());
                if recording.is_empty() {
                    recording.clear();
                }
                let mut reset_command: Option<Command> = None;
                let mut start_record: Option<Command> = None;
                let mut cancel_record = false;
                let error = ctx.data(|data| data.get_temp::<String>(error_id).unwrap_or_default());
                let modal = Modal::new(Id::unique("shortcuts_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        let width = (ctx.content_rect().width() - 140.0).clamp(430.0, 640.0);
                        let height = (ctx.content_rect().height() - 260.0).clamp(220.0, 430.0);
                        ui.set_width(width);
                        // Give the auto-sized modal a stable size: egui sizing passes
                        // otherwise clamp the scroll area to the previous frame's box.
                        ui.set_min_height(30.0 + 8.0 + 28.0 + 4.0 + 13.0 + 6.0 + height);
                        ui.horizontal(|ui| {
                            icons::paint(
                                ui.painter(),
                                egui::Rect::from_center_size(
                                    Pos2::new(ui.cursor().left() + 9.0, ui.cursor().center().y),
                                    Vec2::splat(17.0),
                                ),
                                Icon::Keyboard,
                                theme.accent,
                                1.3,
                            );
                            ui.add_space(26.0);
                            ui.label(
                                egui::RichText::new("键盘快捷键")
                                    .font(style.font(17.0))
                                    .color(theme.text),
                            );
                            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                if Self::modal_button(ui, &style, "关闭", true) {
                                    keep = false;
                                }
                            });
                        });
                        ui.add_space(8.0);
                        let filter_resp = ui.add_sized(
                            Vec2::new(ui.available_width(), 28.0),
                            TextEdit::singleline(&mut filter)
                                .hint_text("搜索命令、分类或快捷键…")
                                .font(style.small())
                                .margin(Vec2::new(8.0, 5.0)),
                        );
                        if filter_resp.changed() {
                            ctx.data_mut(|data| {
                                data.insert_temp(filter_id, filter.clone());
                            });
                        }
                        if !recording.is_empty()
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, Key::Escape)
                            })
                        {
                            cancel_record = true;
                        }
                        if !error.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(&error)
                                    .font(style.tiny())
                                    .color(theme.warning),
                            );
                        } else {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(if recording.is_empty() {
                                    "点击任意命令行，然后按下新的组合键"
                                } else {
                                    "正在录制：按下组合键，Esc 取消"
                                })
                                .font(style.tiny())
                                .color(theme.text_faint),
                            );
                        }
                        ui.add_space(6.0);
                        let query = filter.trim().to_lowercase();
                        ScrollArea::vertical()
                            .max_height(height)
                            .auto_shrink([false, false])
                            .id_salt("shortcuts_scroll")
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                let mut last_category = "";
                                for command in Command::ALL {
                                    let shortcut = self.effective_shortcut(*command);
                                    let text = shortcut.map(|s| s.display()).unwrap_or_default();
                                    let overridden = self.custom_shortcuts.contains_key(command);
                                    if !query.is_empty()
                                        && !command.label().to_lowercase().contains(&query)
                                        && !command.category().to_lowercase().contains(&query)
                                        && !text.to_lowercase().contains(&query)
                                    {
                                        continue;
                                    }
                                    if command.category() != last_category {
                                        last_category = command.category();
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(last_category)
                                                .font(style.font(11.0))
                                                .color(theme.text_faint),
                                        );
                                        ui.add_space(2.0);
                                    }
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 30.0),
                                        Sense::click(),
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
                                        if resp.hovered() {
                                            theme.accent
                                        } else {
                                            theme.text_dim
                                        },
                                        1.2,
                                    );
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 30.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        command.label(),
                                        style.small(),
                                        theme.text_dim,
                                    );
                                    let is_recording = recording == command.id();
                                    let pill_text = if is_recording {
                                        "按下新的组合键…".to_string()
                                    } else if text.is_empty() {
                                        "未设置".to_string()
                                    } else {
                                        text.clone()
                                    };
                                    let pill_w =
                                        (pill_text.chars().count() as f32 * 6.8 + 16.0).max(48.0);
                                    let pill = egui::Rect::from_center_size(
                                        Pos2::new(
                                            rect.right() - 56.0 - pill_w * 0.5,
                                            rect.center().y,
                                        ),
                                        Vec2::new(pill_w, 20.0),
                                    );
                                    ui.painter().rect_filled(
                                        pill,
                                        CornerRadius::same(4),
                                        if is_recording {
                                            theme.accent.gamma_multiply(0.22)
                                        } else {
                                            theme.input_bg
                                        },
                                    );
                                    ui.painter().rect_stroke(
                                        pill,
                                        CornerRadius::same(4),
                                        Stroke::new(
                                            1.0,
                                            if is_recording {
                                                theme.accent
                                            } else {
                                                theme.border_strong
                                            },
                                        ),
                                        egui::StrokeKind::Inside,
                                    );
                                    ui.painter().text(
                                        pill.center(),
                                        Align2::CENTER_CENTER,
                                        pill_text,
                                        style.tiny(),
                                        if is_recording {
                                            theme.accent
                                        } else {
                                            theme.text
                                        },
                                    );
                                    if overridden {
                                        let reset_rect = egui::Rect::from_center_size(
                                            Pos2::new(rect.right() - 32.0, rect.center().y),
                                            Vec2::new(44.0, 20.0),
                                        );
                                        let reset_resp = ui.interact(
                                            reset_rect,
                                            ui.scope_id()
                                                .with(("shortcut_reset", command.id())),
                                            Sense::click(),
                                        );
                                        if reset_resp.hovered() {
                                            ui.painter().rect_filled(
                                                reset_rect,
                                                CornerRadius::same(4),
                                                theme.hover,
                                            );
                                        }
                                        ui.painter().text(
                                            reset_rect.center(),
                                            Align2::CENTER_CENTER,
                                            "重置",
                                            style.tiny(),
                                            if reset_resp.hovered() {
                                                theme.text
                                            } else {
                                                theme.text_faint
                                            },
                                        );
                                        if reset_resp.clicked() {
                                            reset_command = Some(*command);
                                        }
                                    }
                                    if resp.clicked() {
                                        start_record = Some(*command);
                                    }
                                }
                            });
                    });
                if let Some(command) = reset_command {
                    self.custom_shortcuts.remove(&command);
                    self.settings.keybindings.remove(command.id());
                    self.settings.flush();
                    ctx.data_mut(|data| {
                        data.insert_temp(recording_id, String::new());
                        data.insert_temp(error_id, String::new());
                    });
                    self.toast(
                        format!("已恢复「{}」的默认快捷键", command.label()),
                        ToastKind::Info,
                    );
                }
                if let Some(command) = start_record {
                    ctx.data_mut(|data| {
                        data.insert_temp(recording_id, command.id().to_string());
                        data.insert_temp(error_id, String::new());
                    });
                } else if !recording.is_empty() && !cancel_record {
                    // The click that started recording must not be read as a chord.
                    let captured = ctx.input(|input| {
                        input.events.iter().find_map(|event| {
                            if let egui::Event::Key {
                                key,
                                pressed: true,
                                modifiers,
                                ..
                            } = event
                                && *key != Key::Escape
                            {
                                return Some((*key, *modifiers));
                            }
                            None
                        })
                    });
                    if let Some((key, mods)) = captured {
                        let shortcut = Shortcut::new(mods, key);
                        if let Some(command) = Command::from_id(&recording) {
                            if !(mods.command || mods.ctrl || mods.alt) {
                                ctx.data_mut(|data| {
                                    data.insert_temp(
                                        error_id,
                                        "快捷键需要包含 Ctrl 或 Alt".to_string(),
                                    );
                                    data.insert_temp(recording_id, String::new());
                                });
                            } else if let Some(other) = self.shortcut_conflict(command, shortcut) {
                                ctx.data_mut(|data| {
                                    data.insert_temp(
                                        error_id,
                                        format!("该组合键已被「{}」占用", other.label()),
                                    );
                                    data.insert_temp(recording_id, String::new());
                                });
                            } else {
                                self.settings
                                    .keybindings
                                    .insert(command.id().to_string(), shortcut.display());
                                self.custom_shortcuts.insert(command, shortcut);
                                self.settings.flush();
                                ctx.data_mut(|data| {
                                    data.insert_temp(recording_id, String::new());
                                    data.insert_temp(error_id, String::new());
                                });
                                self.toast(
                                    format!(
                                        "已将「{}」绑定到 {}",
                                        command.label(),
                                        shortcut.display()
                                    ),
                                    ToastKind::Success,
                                );
                            }
                        }
                    }
                }
                if cancel_record {
                    ctx.data_mut(|data| {
                        data.insert_temp(recording_id, String::new());
                    });
                }
                if modal.should_close() {
                    keep = false;
                }
            }
            Dialog::Settings => {
                let mut reset_defaults = false;
                let modal = Modal::new(Id::unique("settings_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        keep &= self.settings_modal_ui(ui, &style, &mut reset_defaults);
                    });
                if reset_defaults {
                    let defaults = Settings::default();
                    let recent_folders = std::mem::take(&mut self.settings.recent_folders);
                    let last_folder = self.settings.last_folder.take();
                    let keybindings = self.settings.keybindings.clone();
                    self.settings = defaults;
                    self.settings.recent_folders = recent_folders;
                    self.settings.last_folder = last_folder;
                    self.settings.keybindings = keybindings;
                    ctx.set_zoom_factor(self.settings.ui_zoom);
                    self.workspace.tree.show_hidden = self.settings.show_hidden_files;
                    self.workspace.tree.reload();
                    self.settings.flush();
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
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                            if Self::modal_button(ui, &style, "不保存", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "保存并退出", true) {
                                action = Some(0);
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
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                            if Self::modal_button(ui, &style, "全部关闭", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "保存并关闭", true) {
                                action = Some(0);
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
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                            if Self::modal_button(ui, &style, "不保存", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "保存", true) {
                                action = Some(0);
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
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(false);
                            }
                            if Self::modal_button(ui, &style, "删除", true) {
                                action = Some(true);
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
            Dialog::ConfirmDeleteBatch(paths) => {
                let paths = paths.clone();
                let mut action: Option<bool> = None;
                // Confirmation dialogs ignore backdrop/Esc so a stray click cannot delete.
                Modal::new(Id::unique("confirm_delete_batch_modal"))
                    .frame(Self::modal_frame(&style))
                    .show(ctx, |ui| {
                        ui.set_width(430.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "确定要删除选中的 {} 个项目吗？",
                                paths.len()
                            ))
                            .font(style.font(15.0))
                            .color(theme.text),
                        );
                        ui.add_space(6.0);
                        for path in paths.iter().take(8) {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!("•  {name}"))
                                    .font(style.small())
                                    .color(theme.text_dim),
                            );
                        }
                        if paths.len() > 8 {
                            ui.label(
                                egui::RichText::new(format!("…以及另外 {} 个项目", paths.len() - 8))
                                    .font(style.small())
                                    .color(theme.text_faint),
                            );
                        }
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("此操作不可撤销。")
                                .font(style.small())
                                .color(theme.warning),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(false);
                            }
                            if Self::modal_button(ui, &style, "删除", true) {
                                action = Some(true);
                            }
                        });
                    });
                if let Some(confirm) = action {
                    keep = false;
                    if confirm {
                        self.delete_paths(&paths);
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
                            if Self::modal_button(ui, &style, "取消", false) {
                                action = Some(2);
                            }
                            if Self::modal_button(ui, &style, "放弃更改", false) {
                                action = Some(1);
                            }
                            if Self::modal_button(ui, &style, "保存并打开", true) {
                                action = Some(0);
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
        // The primary action is the dialog default target: it takes focus when the
        // modal opens, so Enter confirms without reaching for the mouse.
        if primary && ui.memory(|memory| memory.focused()).is_none() {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        let fill = if primary {
            if resp.hovered() || focused {
                theme.accent.gamma_multiply(0.9)
            } else {
                theme.accent
            }
        } else if resp.hovered() || focused {
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
        if primary && focused {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(7),
                Stroke::new(1.0, theme.accent),
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
        if focused && primary && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
        {
            return true;
        }
        resp.clicked()
    }

    fn effective_shortcut(&self, command: Command) -> Option<Shortcut> {
        self.custom_shortcuts
            .get(&command)
            .copied()
            .or_else(|| command.shortcut())
    }

    fn shortcut_conflict(&self, target: Command, shortcut: Shortcut) -> Option<Command> {
        // Compare the display form: recorded modifiers carry the physical ctrl bit
        // while built-in bindings use egui's platform command alias.
        let text = shortcut.display();
        Command::ALL.iter().copied().find(|command| {
            *command != target
                && self
                    .effective_shortcut(*command)
                    .is_some_and(|existing| existing.display() == text)
        })
    }

    fn settings_modal_ui(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        reset_defaults: &mut bool,
    ) -> bool {
        let theme = style.theme;
        let mut keep = true;
        let width = (ui.ctx().content_rect().width() - 80.0).clamp(460.0, 720.0);
        let body_height = (ui.ctx().content_rect().height() - 250.0).clamp(240.0, 460.0);
        ui.set_width(width);
        // Fixed body size keeps the auto-sized modal stable across sizing passes.
        ui.set_min_height(30.0 + 6.0 + 30.0 + 6.0 + body_height + 8.0 + 30.0);

        let (hdr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        icons::paint(
            ui.painter(),
            Rect::from_center_size(Pos2::new(hdr.left() + 9.0, hdr.center().y), Vec2::splat(18.0)),
            Icon::Settings,
            theme.accent,
            1.4,
        );
        ui.painter().text(
            Pos2::new(hdr.left() + 24.0, hdr.center().y),
            Align2::LEFT_CENTER,
            "设置",
            style.font(17.0),
            theme.text,
        );
        let close_rect = Rect::from_center_size(
            Pos2::new(hdr.right() - 14.0, hdr.center().y),
            Vec2::splat(26.0),
        );
        let close_resp =
            ui.interact(close_rect, ui.scope_id().with("settings_close_x"), Sense::click());
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
                .hint_text("搜索设置项…")
                .font(style.small())
                .margin(Vec2::new(10.0, 6.0)),
        );
        ui.add_space(6.0);

        let query = self.settings_search.trim().to_lowercase();
        let searching = !query.is_empty();
        let matches = |labels: &str| -> bool { !searching || labels.to_lowercase().contains(&query) };
        let section_id = egui::Id::unique("kestrel_settings_section");
        let mut section =
            ui.ctx().data(|data| data.get_temp::<String>(section_id).unwrap_or_default());
        if section.is_empty() {
            section = "appearance".to_string();
        }
        let mut settings = self.settings.clone();
        let mut changed = false;
        let mut open_plugins = false;

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(128.0);
                for (key, label, icon) in [
                    ("appearance", "外观", Icon::Sun),
                    ("editor", "编辑器", Icon::FileCode),
                    ("terminal", "终端", Icon::Terminal),
                    ("files", "文件", Icon::Folder),
                    ("git", "Git", Icon::SourceControl),
                    ("plugins", "插件", Icon::Extensions),
                ] {
                    let active = !searching && section == key;
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 30.0),
                        Sense::click(),
                    );
                    if active {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(5),
                            theme.accent.gamma_multiply(0.18),
                        );
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(5), theme.hover);
                    }
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(rect.left() + 14.0, rect.center().y),
                            Vec2::splat(14.0),
                        ),
                        icon,
                        if active { theme.accent } else { theme.text_dim },
                        1.2,
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 30.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        label,
                        style.small(),
                        if active { theme.text } else { theme.text_dim },
                    );
                    if resp.clicked() {
                        section = key.to_string();
                    }
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.set_min_width(ui.available_width());
                ScrollArea::vertical()
                    .max_height(body_height)
                    .auto_shrink([false, false])
                    .id_salt("settings_window_scroll")
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());

                        if searching || section == "appearance" {
                            let mut shown = false;
                            if matches("主题 深色 浅色 theme") {
                                Self::settings_section_header(ui, style, &mut shown, "外观");
                                changed |= Self::settings_row(
                                    ui,
                                    style,
                                    "主题",
                                    "深色 / 浅色界面",
                                    |ui| Self::settings_theme_picker(ui, style, &mut settings.theme),
                                );
                            }
                            if matches("编辑器字号 字体大小 font size") {
                                Self::settings_section_header(ui, style, &mut shown, "外观");
                                let mut value = settings.font_size.round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "编辑器字号",
                                    "代码文本的像素大小",
                                    &format!("{value} px"),
                                    &mut value,
                                    9,
                                    30,
                                ) {
                                    settings.font_size = value as f32;
                                    changed = true;
                                }
                            }
                            if matches("界面缩放 ui zoom scale") {
                                Self::settings_section_header(ui, style, &mut shown, "外观");
                                let mut value = (settings.ui_zoom * 10.0).round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "界面缩放",
                                    "整个界面的显示比例",
                                    &format!("{}%", value * 10),
                                    &mut value,
                                    7,
                                    18,
                                ) {
                                    settings.ui_zoom = value as f32 / 10.0;
                                    changed = true;
                                }
                            }
                            if matches("行高 line height") {
                                Self::settings_section_header(ui, style, &mut shown, "外观");
                                let mut value = (settings.line_height * 10.0).round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "行高倍数",
                                    "代码行间距相对字号的倍数",
                                    &format!("{:.1}x", value as f32 / 10.0),
                                    &mut value,
                                    11,
                                    22,
                                ) {
                                    settings.line_height = value as f32 / 10.0;
                                    changed = true;
                                }
                            }
                        }

                        if searching || section == "editor" {
                            let mut shown = false;
                            if matches("缩进空格数 tab size") {
                                Self::settings_section_header(ui, style, &mut shown, "编辑器");
                                let mut value = settings.tab_size as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "缩进空格数",
                                    "按 Tab 插入的空格数量",
                                    &value.to_string(),
                                    &mut value,
                                    2,
                                    8,
                                ) {
                                    settings.tab_size = value as usize;
                                    changed = true;
                                }
                            }
                            let toggles: [(&str, &str, String, &mut bool); 13] = [
                                ("使用空格缩进", "关闭时插入制表符", "空格 制表符".to_string(), &mut settings.insert_spaces),
                                ("自动补全", "输入时弹出补全建议", "补全 autocomplete".to_string(), &mut settings.autocomplete),
                                ("自动换行", "长行按面板宽度折行", "换行 wrap".to_string(), &mut settings.word_wrap),
                                ("显示行号", "编辑器左侧行号栏", "行号".to_string(), &mut settings.show_line_numbers),
                                ("显示小地图", "右侧代码缩略图", "小地图 minimap".to_string(), &mut settings.show_minimap),
                                ("缩进参考线", "竖向缩进辅助线", "缩进 guides".to_string(), &mut settings.show_indent_guides),
                                ("显示空白字符", "用点线标出空格与制表符", "空白 whitespace".to_string(), &mut settings.show_whitespace),
                                ("粘性滚动", "顶部固定当前作用域", "粘性 sticky".to_string(), &mut settings.show_sticky_scroll),
                                ("滚动到行尾之外", "允许最后一行停在顶部", "滚动 scroll".to_string(), &mut settings.scroll_past_end),
                                ("保存时格式化", "保存后自动格式化文档", "格式化 format".to_string(), &mut settings.format_on_save),
                                ("去除行尾空白", "保存时清掉行尾空格", "行尾空白 trim".to_string(), &mut settings.trim_trailing_whitespace),
                                ("自动闭合括号", "输入左括号时补右括号", "括号 pairs".to_string(), &mut settings.auto_close_pairs),
                                ("检测缩进", "按文件内容推断缩进宽度", "缩进 detect".to_string(), &mut settings.detect_indentation),
                            ];
                            for (label, desc, keywords, value) in toggles {
                                if matches(&keywords) {
                                    Self::settings_section_header(ui, style, &mut shown, "编辑器");
                                    changed |= Self::settings_toggle_row(ui, style, label, desc, value);
                                }
                            }
                        }

                        if searching || section == "terminal" {
                            let mut shown = false;
                            if matches("终端字号 terminal font") {
                                Self::settings_section_header(ui, style, &mut shown, "终端");
                                let mut value = settings.terminal_font_size.round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "终端字号",
                                    "终端文本的像素大小",
                                    &format!("{value} px"),
                                    &mut value,
                                    9,
                                    28,
                                ) {
                                    settings.terminal_font_size = value as f32;
                                    changed = true;
                                }
                            }
                            if matches("终端高度 面板 terminal height") {
                                Self::settings_section_header(ui, style, &mut shown, "终端");
                                let mut value = (settings.terminal_height / 10.0).round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "终端面板高度",
                                    "底部终端面板的默认高度",
                                    &format!("{} px", value * 10),
                                    &mut value,
                                    12,
                                    64,
                                ) {
                                    settings.terminal_height = value as f32 * 10.0;
                                    changed = true;
                                }
                            }
                        }

                        if searching || section == "files" {
                            let mut shown = false;
                            if matches("自动保存 autosave") {
                                Self::settings_section_header(ui, style, &mut shown, "文件");
                                changed |= Self::settings_toggle_row(
                                    ui,
                                    style,
                                    "自动保存",
                                    "停止输入后自动写盘，冲突时跳过",
                                    &mut settings.autosave,
                                );
                            }
                            if matches("隐藏文件 hidden") {
                                Self::settings_section_header(ui, style, &mut shown, "文件");
                                changed |= Self::settings_toggle_row(
                                    ui,
                                    style,
                                    "显示隐藏文件",
                                    "资源管理器中显示点开头的文件",
                                    &mut settings.show_hidden_files,
                                );
                            }
                            if matches("配置目录 config") {
                                Self::settings_section_header(ui, style, &mut shown, "文件");
                                let cfg = Settings::config_dir();
                                changed |= Self::settings_row(
                                    ui,
                                    style,
                                    "配置目录",
                                    "设置、快捷键与会话的存放位置",
                                    |ui| {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(84.0, 26.0),
                                            Sense::click(),
                                        );
                                        if resp.hovered() {
                                            ui.painter().rect_filled(
                                                rect,
                                                CornerRadius::same(5),
                                                style.theme.hover,
                                            );
                                        }
                                        ui.painter().text(
                                            rect.center(),
                                            Align2::CENTER_CENTER,
                                            "打开目录",
                                            style.tiny(),
                                            style.theme.text_dim,
                                        );
                                        if resp.clicked() {
                                            let _ = std::fs::create_dir_all(&cfg);
                                            crate::workspace::tree::reveal_in_explorer(&cfg);
                                            true
                                        } else {
                                            false
                                        }
                                    },
                                );
                            }
                        }

                        if searching || section == "git" {
                            let mut shown = false;
                            if matches("git 行内差异 gutter") {
                                Self::settings_section_header(ui, style, &mut shown, "Git");
                                changed |= Self::settings_toggle_row(
                                    ui,
                                    style,
                                    "Git 行内差异",
                                    "行号栏显示新增与修改标记",
                                    &mut settings.show_git_gutter,
                                );
                            }
                            if matches("git 历史高度 history") {
                                Self::settings_section_header(ui, style, &mut shown, "Git");
                                let mut value = (settings.git_history_height / 10.0).round() as i32;
                                if Self::settings_stepper_row(
                                    ui,
                                    style,
                                    "Git 历史高度",
                                    "提交历史列表的默认高度",
                                    &format!("{} px", value * 10),
                                    &mut value,
                                    12,
                                    42,
                                ) {
                                    settings.git_history_height = value as f32 * 10.0;
                                    changed = true;
                                }
                            }
                        }

                        if searching || section == "plugins" {
                            let mut shown = false;
                            if matches("插件 plugins") {
                                Self::settings_section_header(ui, style, &mut shown, "插件");
                                let disabled = settings.disabled_plugins.len();
                                changed |= Self::settings_row(
                                    ui,
                                    style,
                                    "插件管理",
                                    &format!("已禁用 {disabled} 个插件"),
                                    |ui| {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(84.0, 26.0),
                                            Sense::click(),
                                        );
                                        if resp.hovered() {
                                            ui.painter().rect_filled(
                                                rect,
                                                CornerRadius::same(5),
                                                style.theme.hover,
                                            );
                                        }
                                        ui.painter().text(
                                            rect.center(),
                                            Align2::CENTER_CENTER,
                                            "打开面板",
                                            style.tiny(),
                                            style.theme.text_dim,
                                        );
                                        if resp.clicked() {
                                            open_plugins = true;
                                        }
                                        false
                                    },
                                );
                            }
                        }

                        ui.add_space(8.0);
                    });
            });
        });
        ui.ctx()
            .data_mut(|data| data.insert_temp(section_id, section.clone()));

        if changed {
            if self.settings.show_hidden_files != settings.show_hidden_files {
                self.workspace.tree.show_hidden = settings.show_hidden_files;
                self.workspace.tree.reload();
            }
            let zoom = settings.ui_zoom.clamp(0.7, 1.8);
            self.settings = settings;
            ui.ctx().set_zoom_factor(zoom);
            self.settings.save();
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let now = ui.input(|i| i.time);
            let armed_id = egui::Id::unique("kestrel_settings_restore_armed");
            let armed_until =
                ui.ctx()
                    .data(|data| data.get_temp::<f64>(armed_id).unwrap_or(0.0));
            let armed = now < armed_until;
            if Self::modal_button(
                ui,
                style,
                if armed { "确认恢复默认" } else { "恢复默认" },
                false,
            ) {
                if armed {
                    *reset_defaults = true;
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(armed_id, 0.0));
                } else {
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(armed_id, now + 4.0));
                }
            }
            if armed {
                ui.label(
                    egui::RichText::new("再次点击确认")
                        .font(style.tiny())
                        .color(theme.warning),
                );
            }
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if Self::modal_button(ui, style, "关闭", true) {
                    keep = false;
                }
            });
        });
        if open_plugins {
            self.sidebar.visible = true;
            self.sidebar.mode = SidebarMode::Plugins;
            keep = false;
        }
        keep
    }

    fn settings_row(
        ui: &mut Ui,
        style: &UiStyle,
        label: &str,
        desc: &str,
        control: impl FnOnce(&mut Ui) -> bool,
    ) -> bool {
        let theme = style.theme;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.top() + 14.0),
            Align2::LEFT_CENTER,
            label,
            style.small(),
            theme.text,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 6.0, rect.top() + 30.0),
            Align2::LEFT_CENTER,
            desc,
            style.tiny(),
            theme.text_faint,
        );
        let control_rect = Rect::from_min_max(
            Pos2::new(rect.right() - 168.0, rect.top() + 7.0),
            Pos2::new(rect.right() - 2.0, rect.bottom() - 7.0),
        );
        ui.scope_builder(
            UiBuilder::new().max_rect(control_rect).id_salt(label),
            |ui| control(ui),
        )
        .inner
    }

    fn settings_toggle_row(
        ui: &mut Ui,
        style: &UiStyle,
        label: &str,
        desc: &str,
        value: &mut bool,
    ) -> bool {
        Self::settings_row(ui, style, label, desc, |ui| {
            Self::settings_switch(ui, style, "", value)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn settings_stepper_row(
        ui: &mut Ui,
        style: &UiStyle,
        label: &str,
        desc: &str,
        display: &str,
        value: &mut i32,
        min: i32,
        max: i32,
    ) -> bool {
        Self::settings_row(ui, style, label, desc, |ui| {
            Self::settings_stepper(ui, style, "", display, value, min, max)
        })
    }

    fn settings_theme_picker(ui: &mut Ui, style: &UiStyle, current: &mut String) -> bool {
        let theme = style.theme;
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (kind, icon, label) in [
                (ThemeKind::Light, Icon::Sun, "浅色"),
                (ThemeKind::Dark, Icon::Moon, "深色"),
            ] {
                let active = ThemeKind::from_str(current) == kind;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(74.0, 26.0), Sense::click());
                let fill = if active {
                    theme.accent
                } else if resp.hovered() {
                    theme.hover
                } else {
                    theme.input_bg
                };
                ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                let fg = if active { theme.accent_text } else { theme.text };
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(
                        Pos2::new(rect.left() + 18.0, rect.center().y),
                        Vec2::splat(12.0),
                    ),
                    icon,
                    fg,
                    1.2,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 30.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    style.tiny(),
                    fg,
                );
                if resp.clicked() && !active {
                    *current = kind.as_str().to_string();
                    changed = true;
                }
            }
        });
        changed
    }

    fn settings_section_header(ui: &mut Ui, style: &UiStyle, shown: &mut bool, title: &str) {
        if *shown {
            return;
        }
        Self::settings_section_title(ui, style, title);
        *shown = true;
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
            ui.scope_id()
                .with(("step_minus", label, rect.top().to_bits())),
            Sense::click(),
        );
        let p_resp = ui.interact(
            plus_rect,
            ui.scope_id()
                .with(("step_plus", label, rect.top().to_bits())),
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
