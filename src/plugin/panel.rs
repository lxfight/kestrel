//! VSCode-style extensions view inside the sidebar: search, installed list,
//! hover actions and a context menu.

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextEdit, Ui, Vec2,
};
use egui::text::LayoutJob;

use crate::app::{KestrelApp, ToastKind};
use crate::icons::{self, Icon};
use crate::plugin::PluginInfo;
use crate::theme::Theme;
use crate::ui::UiStyle;

enum PanelAction {
    Toggle(usize),
    Reload(usize),
    OpenDir,
    Rescan,
}

impl KestrelApp {
    pub(crate) fn plugin_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let mut action: Option<PanelAction> = None;

        // View header, matching the other sidebar panels.
        let (header, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
        ui.painter().text(
            Pos2::new(header.left() + 14.0, header.center().y),
            Align2::LEFT_CENTER,
            "插件",
            style.font(11.5),
            theme.text_dim,
        );
        let mut x = header.right() - 8.0;
        if panel_header_action(ui, &theme, &mut x, header, Icon::Refresh, "重新扫描插件目录") {
            action = Some(PanelAction::Rescan);
        }
        if panel_header_action(
            ui,
            &theme,
            &mut x,
            header,
            Icon::FolderOpen,
            "打开插件目录",
        ) {
            action = Some(PanelAction::OpenDir);
        }

        // Search field.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let resp = ui.add_sized(
                Vec2::new((ui.available_width() - 12.0).max(80.0), 26.0),
                TextEdit::singleline(&mut self.sidebar.plugin_search)
                    .hint_text("搜索插件…")
                    .font(style.small())
                    .margin(Vec2::new(26.0, 4.0)),
            );
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(resp.rect.left() + 14.0, resp.rect.center().y),
                    Vec2::splat(12.0),
                ),
                Icon::Search,
                theme.text_faint,
                1.2,
            );
            if self.sidebar.plugin_search_focus {
                resp.request_focus();
                self.sidebar.plugin_search_focus = false;
            }
        });
        ui.add_space(4.0);

        let infos = self.plugins.plugin_infos();
        let query = self.sidebar.plugin_search.trim().to_lowercase();
        let filtered: Vec<usize> = infos
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                query.is_empty()
                    || info.name.to_lowercase().contains(&query)
                    || info.id.contains(&query)
                    || info.description.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect();

        let load_errors = self.plugins.load_errors().to_vec();
        let mut expanded = self.sidebar.plugin_installed_open;
        let badge = filtered.len().to_string();
        Self::section_header_ui(
            ui,
            style,
            "已安装",
            Some(&badge),
            &mut expanded,
            "plugins_installed",
        );
        self.sidebar.plugin_installed_open = expanded;

        if expanded {
            let root = self.plugins.root().display().to_string();
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("plugin_panel_scroll")
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if !load_errors.is_empty() {
                        load_error_section(ui, style, &load_errors, &mut action);
                    }
                    if infos.is_empty() {
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.label(
                                RichText::new("尚未安装插件")
                                    .font(style.small())
                                    .color(theme.text_dim),
                            );
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            if panel_text_button(ui, style, "打开插件目录") {
                                action = Some(PanelAction::OpenDir);
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            if panel_text_button(ui, style, "重新扫描") {
                                action = Some(PanelAction::Rescan);
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.label(
                                RichText::new(root)
                                    .font(style.tiny())
                                    .color(theme.text_faint),
                            );
                        });
                    } else if filtered.is_empty() {
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.label(
                                RichText::new("没有匹配的插件")
                                    .font(style.small())
                                    .color(theme.text_faint),
                            );
                        });
                    } else {
                        for index in &filtered {
                            plugin_row(ui, style, *index, &infos[*index], &mut action);
                        }
                    }
                });
        }

        if let Some(action) = action {
            self.apply_panel_action(action, &infos);
        }
    }

    fn apply_panel_action(&mut self, action: PanelAction, infos: &[PluginInfo]) {
        match action {
            PanelAction::Toggle(index) => {
                let Some(info) = infos.get(index) else {
                    return;
                };
                if info.enabled {
                    self.plugins.disable(index);
                    self.store_plugin_disabled(&info.id, true);
                    self.toast(format!("已禁用 {}", info.name), ToastKind::Info);
                } else {
                    let config = self
                        .settings
                        .plugin_config
                        .get(&info.id)
                        .cloned()
                        .unwrap_or_default();
                    match self.plugins.enable(index, config) {
                        Ok(runtime) => {
                            self.store_plugin_disabled(&info.id, false);
                            self.startup_plugin(runtime);
                            self.toast(format!("已启用 {}", info.name), ToastKind::Success);
                        }
                        Err(err) => {
                            self.toast(format!("加载 {} 失败: {err}", info.name), ToastKind::Error)
                        }
                    }
                }
            }
            PanelAction::Reload(index) => {
                let Some(info) = infos.get(index) else {
                    return;
                };
                let config = self
                    .settings
                    .plugin_config
                    .get(&info.id)
                    .cloned()
                    .unwrap_or_default();
                match self.plugins.reload(index, config) {
                    Ok(runtime) => {
                        self.startup_plugin(runtime);
                        self.toast(format!("已重载 {}", info.name), ToastKind::Success);
                    }
                    Err(err) => {
                        self.toast(format!("重载 {} 失败: {err}", info.name), ToastKind::Error)
                    }
                }
            }
            PanelAction::OpenDir => {
                crate::workspace::tree::reveal_in_explorer(self.plugins.root());
            }
            PanelAction::Rescan => {
                self.plugins.rescan(&self.settings);
                let configs = self.settings.plugin_config.clone();
                for runtime in self.plugins.load_enabled(&configs) {
                    self.startup_plugin(runtime);
                }
                self.toast("已重新扫描插件目录", ToastKind::Info);
            }
        }
    }
}

/// 「加载失败」区段：即使有正常插件也展示，每个损坏插件一行。
fn load_error_section(
    ui: &mut Ui,
    style: &UiStyle,
    errors: &[String],
    action: &mut Option<PanelAction>,
) {
    let theme = style.theme;
    let (header, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    ui.painter()
        .rect_filled(header, 0.0, theme.surface_subtle);
    ui.painter().text(
        Pos2::new(header.left() + 14.0, header.center().y),
        Align2::LEFT_CENTER,
        format!("加载失败 · {}", errors.len()),
        style.bold(10.5),
        theme.error,
    );
    for error in errors {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(
                rect.shrink2(Vec2::new(6.0, 0.0)),
                CornerRadius::same(crate::theme::R_SM),
                theme.hover,
            );
        }
        let text_clip = Rect::from_min_max(
            Pos2::new(rect.left() + 14.0, rect.top()),
            Pos2::new(rect.right() - 10.0, rect.bottom()),
        );
        ui.painter().with_clip_rect(text_clip).text(
            Pos2::new(rect.left() + 14.0, rect.center().y),
            Align2::LEFT_CENTER,
            load_error_label(error),
            style.tiny(),
            theme.error,
        );
        if resp.on_hover_text(error.as_str()).clicked() {
            *action = Some(PanelAction::OpenDir);
        }
    }
    ui.add_space(4.0);
}

/// `manifest::discover` 的错误形如 `<清单路径>: <原因>`，列表行只取插件目录名。
fn load_error_label(error: &str) -> String {
    let path = error.split_once(": ").map_or(error, |(path, _)| path);
    std::path::Path::new(path)
        .parent()
        .and_then(|parent| parent.file_name())
        .map_or_else(
            || error.lines().next().unwrap_or_default().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
}

fn plugin_row(
    ui: &mut Ui,
    style: &UiStyle,
    index: usize,
    info: &PluginInfo,
    action: &mut Option<PanelAction>,
) {
    let theme = style.theme;
    let height = if info.error.is_some() { 92.0 } else { 78.0 };
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click(),
    );
    if resp.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(6.0, 1.0)),
            CornerRadius::same(crate::theme::R_SM),
            theme.hover,
        );
    }

    let tile = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.top() + 10.0),
        Vec2::splat(36.0),
    );
    ui.painter().rect_filled(
        tile,
        CornerRadius::same(crate::theme::R_MD),
        theme.accent_subtle,
    );
    ui.painter().rect_stroke(
        tile,
        CornerRadius::same(crate::theme::R_MD),
        Stroke::new(1.0, theme.border),
        StrokeKind::Inside,
    );
    icons::paint(
        ui.painter(),
        Rect::from_center_size(tile.center(), Vec2::splat(20.0)),
        Icon::Extensions,
        theme.accent,
        1.2,
    );

    let text_x = rect.left() + 58.0;
    let text_right = rect.right() - 10.0;
    let name_font = style.bold(12.5);
    let name_clip = Rect::from_min_max(
        Pos2::new(text_x, rect.top() + 4.0),
        Pos2::new(text_right, rect.top() + 26.0),
    );
    ui.painter().with_clip_rect(name_clip).text(
        Pos2::new(text_x, rect.top() + 15.0),
        Align2::LEFT_CENTER,
        &info.name,
        name_font.clone(),
        theme.text,
    );
    let name_width = text_width(ui, &info.name, name_font);
    if text_x + name_width + 8.0 < text_right {
        ui.painter().text(
            Pos2::new(text_x + name_width + 8.0, rect.top() + 15.5),
            Align2::LEFT_CENTER,
            &info.version,
            style.tiny(),
            theme.text_muted,
        );
    }

    let action_reserve = if resp.hovered() { 80.0 } else { 4.0 };
    let desc_width = (text_right - action_reserve - text_x).max(40.0);
    if !info.description.is_empty() {
        let (desc_g, desc_cut) = crate::ui::widgets::ellipsize_mode(
            ui,
            &info.description,
            style.small(),
            theme.text_dim,
            desc_width,
            crate::ui::widgets::EllipsizeMode::End,
        );
        ui.painter()
            .galley(Pos2::new(text_x, rect.top() + 30.0), desc_g, theme.text_dim);
        if desc_cut {
            resp.clone().on_hover_text(&info.description);
        }
    }

    let state = if info.error.is_some() {
        ("加载失败", theme.error)
    } else if info.loaded && info.enabled {
        ("已启用", theme.text_dim)
    } else if info.enabled {
        ("未加载", theme.warning)
    } else {
        ("已禁用", theme.text_faint)
    };
    let mut meta = state.0.to_string();
    if info.memory > 0 {
        meta.push_str(&format!(" · {} KB", info.memory.div_ceil(1024)));
    }
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 62.0),
        Align2::LEFT_CENTER,
        &meta,
        style.tiny(),
        state.1,
    );

    if let Some(error) = &info.error {
        let clip = Rect::from_min_max(
            Pos2::new(text_x, rect.top() + 68.0),
            Pos2::new(text_right, rect.top() + 88.0),
        );
        ui.painter().with_clip_rect(clip).text(
            Pos2::new(text_x, rect.top() + 78.0),
            Align2::LEFT_CENTER,
            error,
            style.tiny(),
            theme.error,
        );
    }

    if resp.hovered() {
        let reload = Rect::from_center_size(
            Pos2::new(rect.right() - 22.0, rect.center().y),
            Vec2::splat(24.0),
        );
        if panel_icon_button(ui, &theme, reload, Icon::Refresh, "重载") {
            *action = Some(PanelAction::Reload(index));
        }
        let label = if info.enabled { "禁用" } else { "启用" };
        let pill = Rect::from_min_size(
            Pos2::new(rect.right() - 74.0, rect.center().y - 11.0),
            Vec2::new(44.0, 22.0),
        );
        if panel_text_button_at(ui, style, pill, label) {
            *action = Some(PanelAction::Toggle(index));
        }
    }

    resp.context_menu(|ui| {
        ui.set_width(200.0);
        if KestrelApp::context_item(ui, style, Icon::Refresh, "重载", None) {
            *action = Some(PanelAction::Reload(index));
            ui.close();
        }
        if KestrelApp::context_item(ui, style, Icon::FolderOpen, "在文件资源管理器中显示", None) {
            *action = Some(PanelAction::OpenDir);
            ui.close();
        }
    });
}

fn panel_header_action(
    ui: &mut Ui,
    theme: &Theme,
    x: &mut f32,
    header: Rect,
    icon: Icon,
    tip: &str,
) -> bool {
    *x -= 24.0;
    let rect = Rect::from_center_size(Pos2::new(*x + 11.0, header.center().y), Vec2::splat(22.0));
    let resp = ui.interact(rect, ui.scope_id().with(("plugin_action", tip)), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), theme.hover);
    }
    icons::paint(
        ui.painter(),
        rect.shrink(4.0),
        icon,
        if resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        },
        1.3,
    );
    resp.on_hover_text(tip).clicked()
}

fn panel_icon_button(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    tip: &str,
) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(("plugin_icon", tip)), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), theme.hover);
    }
    icons::paint(
        ui.painter(),
        rect.shrink(6.0),
        icon,
        if resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        },
        1.3,
    );
    resp.on_hover_text(tip).clicked()
}

fn panel_text_button_at(ui: &mut Ui, style: &UiStyle, rect: Rect, label: &str) -> bool {
    let theme = style.theme;
    let id = ui.scope_id().with(("plugin_pill", label));
    let resp = ui.interact(rect, id, Sense::click());
    let fill = if resp.hovered() {
        theme.hover
    } else {
        theme.input_bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        style.tiny(),
        theme.text,
    );
    resp.clicked()
}

fn panel_text_button(ui: &mut Ui, style: &UiStyle, label: &str) -> bool {
    let theme = style.theme;
    let width = text_width(ui, label, style.small()) + 22.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    let fill = if resp.hovered() {
        theme.hover
    } else {
        theme.input_bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        style.small(),
        theme.text,
    );
    resp.clicked()
}

fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_job(LayoutJob::simple_singleline(
                text.to_string(),
                font,
                Color32::WHITE,
            ))
            .rect
            .width()
    })
}
