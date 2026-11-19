//! Window chrome: top bar, activity bar, tabs, breadcrumbs, split editor, status bar, welcome.

pub mod dialogs;
pub mod palette;
pub mod sidebar;
pub mod terminal;

use std::path::PathBuf;

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Panel, Pos2, Rect, Sense,
    Stroke, Ui, UiBuilder, Vec2,
};

use crate::app::{BottomTab, DragPayload, KestrelApp, PaletteMode, SidebarMode};
use crate::commands::Command;
use crate::git::GitFileStatus;
use crate::icons::{self, Icon};
use crate::lsp::DiagSeverity;
use crate::theme::{Theme, ThemeKind};

#[derive(Clone)]
pub struct UiStyle {
    pub theme: Theme,
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,
    pub ui_size: f32,
}

impl UiStyle {
    pub fn font(&self, size: f32) -> FontId {
        FontId::new(size, self.ui_family.clone())
    }
    pub fn ui(&self) -> FontId {
        self.font(self.ui_size)
    }
    pub fn small(&self) -> FontId {
        self.font(11.5)
    }
    pub fn tiny(&self) -> FontId {
        self.font(10.5)
    }
}

impl KestrelApp {
    pub fn root_ui(&mut self, ui: &mut Ui) {
        let style = self.ui_style();
        let theme = style.theme;

        Panel::top("kestrel_topbar")
            .exact_size(40.0)
            .frame(Frame::new().fill(theme.panel_bg))
            .show(ui, |ui| {
                self.topbar_ui(ui, &style);
            });

        Panel::left("kestrel_activity")
            .exact_size(48.0)
            .frame(Frame::new().fill(theme.activity_bg))
            .show(ui, |ui| {
                self.activity_ui(ui, &style);
            });

        if self.sidebar.visible {
            let width = self.sidebar.width;
            let sb_resp = Panel::left("kestrel_sidebar")
                .default_size(width)
                .size_range(180.0..=640.0)
                .resizable(true)
                .frame(
                    Frame::new()
                        .fill(theme.sidebar_bg)
                        .inner_margin(Margin::ZERO),
                )
                .show(ui, |ui| {
                    self.sidebar_ui(ui, &style);
                });
            let new_w = sb_resp.response.rect.width();
            if new_w >= 180.0 {
                self.sidebar.width = new_w;
            }
        }

        Panel::bottom("kestrel_status")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(theme.status_bg)
                    .inner_margin(Margin::ZERO),
            )
            .show(ui, |ui| {
                self.statusbar_ui(ui, &style);
            });

        if self.bottom.visible {
            let bp_resp = Panel::bottom("kestrel_panel")
                .default_size(self.bottom.height)
                .size_range(100.0..=640.0)
                .resizable(true)
                .frame(Frame::new().fill(theme.panel_bg).inner_margin(Margin::ZERO))
                .show(ui, |ui| {
                    self.terminal_panel_ui(ui, &style);
                });
            let new_h = bp_resp.response.rect.height();
            if new_h >= 100.0 {
                self.bottom.height = new_h;
            }
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme.bg).inner_margin(Margin::ZERO))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                if self.tabs.is_empty() {
                    self.editor_area_rect = Some(ui.max_rect());
                    self.group_rects.clear();
                    self.welcome_ui(ui, &style);
                    return;
                }
                self.groups_ui(ui, &style);
            });
    }

    fn topbar_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.panel_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom() - 0.5),
                Pos2::new(rect.right(), rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        // Branded Logo
        let logo_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 13.0, rect.center().y - 11.0),
            Vec2::splat(22.0),
        );
        ui.painter()
            .rect_filled(logo_rect, CornerRadius::same(6), theme.accent);
        ui.painter().text(
            logo_rect.center(),
            Align2::CENTER_CENTER,
            "K",
            style.font(13.5),
            theme.accent_text,
        );

        // Menu bar
        let menu_rect = Rect::from_min_max(
            Pos2::new(logo_rect.right() + 10.0, rect.top()),
            Pos2::new(logo_rect.right() + 260.0, rect.bottom()),
        );
        let mut menu_end_x = logo_rect.right() + 10.0;
        ui.scope_builder(UiBuilder::new().max_rect(menu_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mb = egui::MenuBar::new().ui(ui, |ui| {
                    self.topbar_menus(ui, style);
                });
                menu_end_x = mb.response.rect.right();
            });
        });

        // Command palette search pill (centered)
        let pill_size = Vec2::new((rect.width() * 0.32).clamp(240.0, 440.0), 26.0);
        let pill_rect =
            Rect::from_center_size(Pos2::new(rect.center().x, rect.center().y), pill_size);
        let pill_resp = ui.interact(
            pill_rect,
            ui.scope_id().with("command_pill"),
            Sense::click(),
        );
        let hovered = pill_resp.hovered();
        ui.painter().rect_filled(
            pill_rect,
            CornerRadius::same(7),
            if hovered { theme.hover } else { theme.input_bg },
        );
        ui.painter().rect_stroke(
            pill_rect,
            CornerRadius::same(7),
            Stroke::new(
                1.0,
                if hovered {
                    theme.border_strong
                } else {
                    theme.border
                },
            ),
            egui::StrokeKind::Inside,
        );
        let search_icon = Rect::from_center_size(
            Pos2::new(pill_rect.left() + 15.0, pill_rect.center().y),
            Vec2::splat(14.0),
        );
        icons::paint(
            ui.painter(),
            search_icon,
            Icon::Search,
            theme.text_dim,
            1.3,
        );
        ui.painter().text(
            Pos2::new(pill_rect.left() + 29.0, pill_rect.center().y),
            Align2::LEFT_CENTER,
            "搜索文件、符号或命令",
            style.small(),
            theme.text_dim,
        );
        ui.painter().text(
            Pos2::new(pill_rect.right() - 10.0, pill_rect.center().y),
            Align2::RIGHT_CENTER,
            "Ctrl+P",
            style.tiny(),
            theme.text_faint,
        );
        if pill_resp.clicked() {
            self.open_palette(PaletteMode::QuickOpen);
        }

        // Window controls (Minimize, Maximize/Restore, Close) on far right
        let win_btn_w = 38.0;
        let is_maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));

        let close_rect = Rect::from_min_size(
            Pos2::new(rect.right() - win_btn_w, rect.top()),
            Vec2::new(win_btn_w, rect.height() - 1.0),
        );
        let max_rect = Rect::from_min_size(
            Pos2::new(close_rect.left() - win_btn_w, rect.top()),
            Vec2::new(win_btn_w, rect.height() - 1.0),
        );
        let min_rect = Rect::from_min_size(
            Pos2::new(max_rect.left() - win_btn_w, rect.top()),
            Vec2::new(win_btn_w, rect.height() - 1.0),
        );

        let close_resp = ui.interact(close_rect, ui.scope_id().with("win_close"), Sense::click());
        if close_resp.hovered() {
            ui.painter()
                .rect_filled(close_rect, 0.0, Color32::from_rgb(0xC4, 0x2B, 0x1C));
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(close_rect.center(), Vec2::splat(13.0)),
            Icon::Close,
            if close_resp.hovered() {
                Color32::WHITE
            } else {
                theme.text_dim
            },
            1.4,
        );
        if close_resp.on_hover_text("关闭").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let max_resp = ui.interact(max_rect, ui.scope_id().with("win_max"), Sense::click());
        if max_resp.hovered() {
            ui.painter().rect_filled(max_rect, 0.0, theme.hover);
        }
        let max_fg = if max_resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        };
        if is_maximized {
            let back = Rect::from_center_size(
                Pos2::new(max_rect.center().x + 1.5, max_rect.center().y - 1.5),
                Vec2::splat(8.0),
            );
            let front = Rect::from_center_size(
                Pos2::new(max_rect.center().x - 1.5, max_rect.center().y + 1.5),
                Vec2::splat(8.0),
            );
            ui.painter().rect_stroke(
                back,
                1.0,
                Stroke::new(1.1, max_fg),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(front, 1.0, theme.panel_bg);
            ui.painter().rect_stroke(
                front,
                1.0,
                Stroke::new(1.1, max_fg),
                egui::StrokeKind::Inside,
            );
        } else {
            ui.painter().rect_stroke(
                Rect::from_center_size(max_rect.center(), Vec2::splat(9.5)),
                1.0,
                Stroke::new(1.1, max_fg),
                egui::StrokeKind::Inside,
            );
        }
        if max_resp
            .on_hover_text(if is_maximized { "向下还原" } else { "最大化" })
            .clicked()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
        }

        let min_resp = ui.interact(min_rect, ui.scope_id().with("win_min"), Sense::click());
        if min_resp.hovered() {
            ui.painter().rect_filled(min_rect, 0.0, theme.hover);
        }
        let min_fg = if min_resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        };
        ui.painter().line_segment(
            [
                Pos2::new(min_rect.center().x - 5.0, min_rect.center().y),
                Pos2::new(min_rect.center().x + 5.0, min_rect.center().y),
            ],
            Stroke::new(1.2, min_fg),
        );
        if min_resp.on_hover_text("最小化").clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        // Subtle divider before editor actions
        let div_x = min_rect.left() - 6.0;
        ui.painter().line_segment(
            [
                Pos2::new(div_x, rect.top() + 10.0),
                Pos2::new(div_x, rect.bottom() - 10.0),
            ],
            Stroke::new(1.0, theme.border),
        );

        // Right side workbench layout & theme toggles
        let mut x = div_x - 8.0;
        let size = 26.0;
        let action = |ui: &mut Ui, x: &mut f32, icon: Icon, active: bool, tip: &str| -> bool {
            *x -= size + 4.0;
            let r = Rect::from_center_size(
                Pos2::new(*x + size * 0.5, rect.center().y),
                Vec2::splat(size),
            );
            let resp = ui.interact(r, ui.scope_id().with(("top_btn", tip)), Sense::click());
            if resp.hovered() || active {
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(6),
                    if resp.hovered() {
                        theme.hover
                    } else {
                        theme.active.gamma_multiply(0.75)
                    },
                );
                if active {
                    ui.painter().rect_stroke(
                        r,
                        CornerRadius::same(6),
                        Stroke::new(1.0, theme.border),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            icons::paint(
                ui.painter(),
                r.shrink(5.5),
                icon,
                if active { theme.accent } else { theme.text_dim },
                1.4,
            );
            resp.on_hover_text(tip).clicked()
        };
        if action(
            ui,
            &mut x,
            Icon::PanelBottom,
            self.bottom.visible,
            "切换底部面板 (Ctrl+J)",
        ) {
            self.toggle_terminal();
        }
        if action(
            ui,
            &mut x,
            Icon::PanelLeft,
            self.sidebar.visible,
            "切换侧边栏 (Ctrl+B)",
        ) {
            self.sidebar.visible = !self.sidebar.visible;
        }
        let theme_icon = if theme.kind.is_dark() {
            Icon::Moon
        } else {
            Icon::Sun
        };
        if action(
            ui,
            &mut x,
            theme_icon,
            false,
            &format!("切换深色/浅色主题 (当前: {})", theme.kind.label()),
        ) {
            self.settings.theme = if theme.kind == ThemeKind::Dark {
                "light".to_string()
            } else {
                "dark".to_string()
            };
            self.settings.save();
            ui.ctx().request_repaint();
        }

        // Drag / double-click to move or maximize on empty titlebar regions
        let drag_zones = [
            Rect::from_min_max(rect.min, Pos2::new(logo_rect.right() + 6.0, rect.bottom())),
            Rect::from_min_max(
                Pos2::new(menu_end_x + 4.0, rect.top()),
                Pos2::new(pill_rect.left() - 4.0, rect.bottom()),
            ),
            Rect::from_min_max(
                Pos2::new(pill_rect.right() + 4.0, rect.top()),
                Pos2::new(x - 4.0, rect.bottom()),
            ),
        ];
        for (idx, zone) in drag_zones.into_iter().enumerate() {
            if zone.width() <= 4.0 {
                continue;
            }
            let drag_resp = ui.interact(
                zone,
                ui.scope_id().with(("topbar_drag", idx)),
                Sense::click_and_drag(),
            );
            if drag_resp.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag_resp.double_clicked() {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
            }
        }
    }

    fn topbar_menus(&mut self, ui: &mut Ui, style: &UiStyle) {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.menu_button("文件", |ui| {
            self.menu_items(
                ui,
                style,
                &[
                    Command::NewFile,
                    Command::OpenFile,
                    Command::OpenFolder,
                    Command::Save,
                    Command::SaveAs,
                    Command::SaveAll,
                    Command::CloseTab,
                    Command::CloseAllTabs,
                ],
            );
            ui.separator();
            if Self::context_item(ui, style, Icon::Close, "退出", Some("Alt+F4")) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.menu_button("编辑", |ui| {
            self.menu_items(
                ui,
                style,
                &[
                    Command::Undo,
                    Command::Redo,
                    Command::Cut,
                    Command::Copy,
                    Command::Paste,
                    Command::Find,
                    Command::Replace,
                    Command::FormatDocument,
                    Command::ToggleComment,
                    Command::DuplicateLine,
                    Command::DeleteLine,
                ],
            );
        });
        ui.menu_button("选择", |ui| {
            self.menu_items(
                ui,
                style,
                &[
                    Command::SelectAll,
                    Command::SelectNextOccurrence,
                    Command::SelectAllOccurrences,
                    Command::AddCursorUp,
                    Command::AddCursorDown,
                    Command::Indent,
                    Command::Outdent,
                ],
            );
        });
        ui.menu_button("查看", |ui| {
            self.menu_items(
                ui,
                style,
                &[
                    Command::ShowPalette,
                    Command::QuickOpen,
                    Command::ShowExplorer,
                    Command::ShowSearch,
                    Command::ShowGit,
                    Command::ShowOutline,
                    Command::SplitEditor,
                    Command::ToggleMarkdownPreview,
                    Command::MarkdownPreviewSide,
                    Command::ToggleSidebar,
                    Command::ToggleTerminal,
                    Command::ShowProblems,
                    Command::ToggleWordWrap,
                    Command::ToggleStickyScroll,
                    Command::ToggleMinimap,
                    Command::ToggleFold,
                    Command::FoldAll,
                    Command::UnfoldAll,
                    Command::ZoomIn,
                    Command::ZoomOut,
                    Command::ZoomReset,
                    Command::SetLanguage,
                    Command::OpenSettings,
                ],
            );
        });
        ui.menu_button("转到", |ui| {
            self.menu_items(
                ui,
                style,
                &[
                    Command::QuickOpen,
                    Command::GotoSymbol,
                    Command::GotoDefinition,
                    Command::GotoLine,
                    Command::NextTab,
                    Command::PrevTab,
                    Command::FindNext,
                    Command::FindPrev,
                ],
            );
        });
        ui.menu_button("帮助", |ui| {
            self.menu_items(ui, style, &[Command::Shortcuts, Command::About]);
        });
    }

    fn menu_items(&mut self, ui: &mut Ui, style: &UiStyle, commands: &[Command]) {
        for command in commands {
            self.menu_item(ui, style, *command);
        }
    }

    fn menu_item(&mut self, ui: &mut Ui, style: &UiStyle, command: Command) {
        let theme = style.theme;
        let shortcut = command.shortcut_text().unwrap_or_default();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(244.0, 25.0), Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(4), theme.hover);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(Pos2::new(rect.left() + 13.0, rect.center().y), Vec2::splat(14.0)),
            command.icon(),
            if response.hovered() {
                theme.accent
            } else {
                theme.text_dim
            },
            1.2,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            command.label(),
            style.small(),
            theme.text,
        );
        if !shortcut.is_empty() {
            ui.painter().text(
                Pos2::new(rect.right() - 8.0, rect.center().y),
                Align2::RIGHT_CENTER,
                shortcut,
                style.tiny(),
                theme.text_faint,
            );
        }
        if response.clicked() {
            ui.close();
            let ctx = ui.ctx().clone();
            self.dispatch(command, &ctx);
        }
    }

    fn activity_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.activity_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.right() - 0.5, rect.top()),
                Pos2::new(rect.right() - 0.5, rect.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );

        let item = |ui: &mut Ui,
                    y: f32,
                    icon: Icon,
                    selected: bool,
                    badge: usize,
                    tip: &str|
         -> bool {
            let r = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), 44.0));
            let resp = ui.interact(r, ui.scope_id().with(("activity", tip)), Sense::click());
            if selected {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 6.0, r.top() + 5.0),
                        Pos2::new(r.right() - 6.0, r.bottom() - 5.0),
                    ),
                    CornerRadius::same(7),
                    theme.hover,
                );
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        Pos2::new(r.left(), r.top() + 10.0),
                        Vec2::new(2.5, r.height() - 20.0),
                    ),
                    CornerRadius::same(1),
                    theme.accent,
                );
            } else if resp.hovered() {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 6.0, r.top() + 5.0),
                        Pos2::new(r.right() - 6.0, r.bottom() - 5.0),
                    ),
                    CornerRadius::same(7),
                    theme.hover.gamma_multiply(0.6),
                );
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(r.center(), Vec2::splat(20.0)),
                icon,
                if selected {
                    theme.text
                } else if resp.hovered() {
                    theme.text_dim
                } else {
                    theme.text_faint
                },
                1.45,
            );
            if badge > 0 {
                let badge_pos = Pos2::new(r.center().x + 9.0, r.center().y + 7.0);
                ui.painter().circle_filled(badge_pos, 7.5, theme.accent);
                ui.painter().text(
                    badge_pos,
                    Align2::CENTER_CENTER,
                    if badge > 99 {
                        "99".to_string()
                    } else {
                        badge.to_string()
                    },
                    style.font(9.0),
                    theme.accent_text,
                );
            }
            resp.on_hover_text(tip).clicked()
        };

        let mut y = rect.top() + 6.0;
        if item(
            ui,
            y,
            Icon::Explorer,
            self.sidebar.visible && self.sidebar.mode == SidebarMode::Explorer,
            0,
            "资源管理器 (Ctrl+Shift+E)",
        ) {
            if self.sidebar.visible && self.sidebar.mode == SidebarMode::Explorer {
                self.sidebar.visible = false;
            } else {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Explorer;
            }
        }
        y += 44.0;
        if item(
            ui,
            y,
            Icon::Search,
            self.sidebar.visible && self.sidebar.mode == SidebarMode::Search,
            0,
            "全局搜索与替换 (Ctrl+Shift+F)",
        ) {
            self.sidebar.visible = true;
            self.sidebar.mode = SidebarMode::Search;
            self.workspace.search_panel.focus = true;
        }
        y += 44.0;
        let git_changes = self.git.snapshot.changes.len();
        if item(
            ui,
            y,
            Icon::SourceControl,
            self.sidebar.visible && self.sidebar.mode == SidebarMode::Git,
            git_changes,
            "源代码管理 (Git)",
        ) {
            self.sidebar.visible = true;
            self.sidebar.mode = SidebarMode::Git;
            if let Some(root) = self.workspace.root.clone() {
                self.git.refresh(&root);
            }
        }
        y += 44.0;
        if item(
            ui,
            y,
            Icon::Outline,
            self.sidebar.visible && self.sidebar.mode == SidebarMode::Outline,
            0,
            "代码大纲 (Outline)",
        ) {
            self.sidebar.visible = true;
            self.sidebar.mode = SidebarMode::Outline;
        }
        y += 44.0;
        if item(
            ui,
            y,
            Icon::Extensions,
            self.sidebar.visible && self.sidebar.mode == SidebarMode::Plugins,
            0,
            "插件 (Ctrl+Shift+X)",
        ) {
            if self.sidebar.visible && self.sidebar.mode == SidebarMode::Plugins {
                self.sidebar.visible = false;
            } else {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Plugins;
                self.sidebar.plugin_search_focus = true;
            }
        }

        // Bottom: Shortcuts & Settings
        let y_kb = rect.bottom() - 94.0;
        if item(ui, y_kb, Icon::Keyboard, false, 0, "键盘快捷键 (Ctrl+K)") {
            self.dialog = Some(crate::app::Dialog::Shortcuts);
        }
        let y_set = rect.bottom() - 48.0;
        if item(
            ui,
            y_set,
            Icon::Settings,
            matches!(self.dialog, Some(crate::app::Dialog::Settings)),
            0,
            "偏好设置 (Ctrl+,)",
        ) {
            self.toggle_settings();
        }
    }

    fn groups_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let avail = ui.available_rect_before_wrap();
        self.editor_area_rect = Some(avail);
        self.normalize_groups();
        self.ensure_groups();
        let count = self.groups.len();
        let weights: Vec<f32> = self
            .groups
            .iter()
            .map(|group| group.weight.max(0.05))
            .collect();
        let weight_sum: f32 = weights.iter().sum();
        let usable = (avail.width() - crate::app::GROUP_GAP * (count as f32 - 1.0)).max(80.0);
        let mut rects = Vec::with_capacity(count);
        let mut x = avail.left();
        for weight in &weights {
            let width = usable * weight / weight_sum;
            rects.push(Rect::from_min_size(
                Pos2::new(x, avail.top()),
                Vec2::new(width, avail.height()),
            ));
            x += width + crate::app::GROUP_GAP;
        }
        if let Some(last) = rects.last_mut() {
            last.max.x = avail.right();
        }
        self.group_rects = rects.clone();

        // Clicking anywhere in a group focuses it for commands / shortcuts.
        let overlay_open = self.palette.open || self.dialog.is_some();
        if !overlay_open
            && ui.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            for (index, rect) in rects.iter().enumerate() {
                if rect.contains(pos) {
                    if self.active_group != index {
                        self.focus_group(index);
                    }
                    break;
                }
            }
        }

        let editor_style = self.editor_style();
        let mut event = None;
        for group in 0..count {
            if group >= self.groups.len() {
                break;
            }
            if group > 0 {
                let sep = Rect::from_min_max(
                    Pos2::new(rects[group - 1].right(), avail.top()),
                    Pos2::new(rects[group].left(), avail.bottom()),
                );
                let resp = ui.interact(
                    sep,
                    ui.scope_id().with(("group_sep", group)),
                    Sense::click_and_drag(),
                );
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().rect_filled(
                    sep,
                    0.0,
                    if resp.hovered() || resp.dragged() {
                        theme.accent
                    } else {
                        theme.border_strong
                    },
                );
                if resp.dragged()
                    && let Some(pos) = resp.interact_pointer_pos()
                {
                    let left = rects[group - 1].left();
                    let right = rects[group].right();
                    let fraction =
                        ((pos.x - left) / (right - left).max(1.0)).clamp(0.12, 0.88);
                    let total = self.groups[group - 1].weight.max(0.05)
                        + self.groups[group].weight.max(0.05);
                    self.groups[group - 1].weight = total * fraction;
                    self.groups[group].weight = total * (1.0 - fraction);
                } else if resp.double_clicked() {
                    self.groups[group - 1].weight = 1.0;
                    self.groups[group].weight = 1.0;
                }
            }
            let Some(rect) = rects.get(group).copied() else {
                break;
            };
            let group_event = self.group_ui(ui, style, &editor_style, group, rect);
            if event.is_none() {
                event = group_event;
            }
        }
        ui.advance_cursor_after_rect(avail);

        match event {
            Some(crate::editor::EditorEvent::Zoom(direction)) => {
                if direction > 0 {
                    self.settings.font_size = (self.settings.font_size + 1.0).min(30.0);
                } else {
                    self.settings.font_size = (self.settings.font_size - 1.0).max(9.0);
                }
                self.settings.save();
            }
            Some(crate::editor::EditorEvent::GotoDefinition(symbol)) => {
                self.goto_definition_by_name(&symbol);
            }
            None => {}
        }
    }

    fn group_ui(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        editor_style: &crate::editor::EditorStyle,
        group_idx: usize,
        rect: Rect,
    ) -> Option<crate::editor::EditorEvent> {
        let mut event = None;
        let tag = format!("g{group_idx}");
        ui.scope_builder(
            UiBuilder::new().max_rect(rect).id_salt(("editor_group", group_idx)),
            |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let (bar, _) = ui.allocate_exact_size(
                Vec2::new(rect.width(), crate::app::TABBAR_H),
                Sense::hover(),
            );
            self.group_tabbar_ui(ui, style, group_idx, bar);
            let Some(active_id) = self.groups.get(group_idx).and_then(|group| group.active_id())
            else {
                if self.groups.get(group_idx).is_some() {
                    self.empty_group_ui(ui, style, group_idx);
                }
                return;
            };
            let id = active_id;
            let Some(tab_idx) = self.tab_index(id) else {
                return;
            };
            self.breadcrumb_ui(ui, style, tab_idx, group_idx);
            // Scope the pane below the tab bar / breadcrumb so viewers that paint
            // `ui.max_rect()` keep the tab bar visible.
            let pane_rect = ui.available_rect_before_wrap();
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(pane_rect)
                    .id_salt(("editor_pane", group_idx)),
                |ui| {
                    event = self.render_pane(ui, style, editor_style, tab_idx, &tag);
                },
            );
        });
        event
    }

    fn group_tabbar_ui(&mut self, ui: &mut Ui, style: &UiStyle, group_idx: usize, rect: Rect) {
        let theme = style.theme;
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme.panel_bg);
        painter.line_segment(
            [
                Pos2::new(rect.left(), rect.bottom() - 0.5),
                Pos2::new(rect.right(), rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        let bar_resp = ui.interact(
            rect,
            ui.scope_id().with(("group_bar", group_idx)),
            Sense::hover(),
        );
        if bar_resp.hovered() {
            let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
            if dx != 0.0 || dy != 0.0 {
                self.groups[group_idx].tab_scroll =
                    (self.groups[group_idx].tab_scroll - dx - dy).max(0.0);
            }
        }

        let group_active = group_idx == self.active_group;
        let active_id = self.groups[group_idx].active_id();
        let active_tab_idx = active_id.and_then(|id| self.tab_index(id));
        let is_active_text = active_tab_idx
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| {
                tab.doc.encoding != crate::doc::Encoding::Binary
                    && tab.multimodal.as_ref().is_none_or(|m| m.is_showing_source())
            });
        let mut actions_reserve = 10.0 + 26.0;
        if group_active && is_active_text {
            actions_reserve += 26.0;
        }
        if self.groups.len() > 1 {
            actions_reserve += 26.0;
        }
        let tabs_clip = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.right() - actions_reserve, rect.bottom()),
        );
        let scroll = self.groups[group_idx].tab_scroll;
        let ids: Vec<u64> = self.groups[group_idx].tabs.clone();
        let pointer_pos = ui
            .ctx()
            .input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let dragging_tab = match &self.drag_payload {
            Some(DragPayload::Tab { id, group }) => Some((*id, *group)),
            _ => None,
        };
        let mut x = rect.left() - scroll;
        let mut active_tab_line_x = x;
        let mut activate: Option<u64> = None;
        let mut close: Option<u64> = None;
        let mut toggle_pin: Option<u64> = None;
        let mut start_drag: Option<u64> = None;
        let mut reorder: Option<(u64, usize, usize, f32)> = None;
        let mut context_action: Option<TabAction> = None;

        for (pos_in_group, id) in ids.iter().enumerate() {
            let Some(index) = self.tab_index(*id) else {
                continue;
            };
            let tab = &self.tabs[index];
            let title = tab.title();
            let dirty = tab.is_dirty();
            let preview = tab.preview;
            let pinned = tab.pinned;
            let display_title = if preview {
                format!("{title} · 预览")
            } else {
                title.clone()
            };
            let text_width = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        display_title.clone(),
                        style.small(),
                        theme.text,
                    ))
                    .rect
                    .width()
            });
            let width = (text_width + 52.0).clamp(116.0, 236.0);
            let tab_rect = Rect::from_min_size(
                Pos2::new(x, rect.top()),
                Vec2::new(width, rect.height()),
            );
            x += width;
            active_tab_line_x = x;
            if tab_rect.right() < tabs_clip.left() - 40.0
                || tab_rect.left() > tabs_clip.right() + 40.0
            {
                continue;
            }
            let clip_rect = tab_rect.intersect(tabs_clip);
            let tab_resp = ui.interact(
                clip_rect,
                ui.scope_id().with(("gtab", group_idx, index)),
                Sense::click_and_drag(),
            );
            if tab_resp.drag_started_by(egui::PointerButton::Primary) {
                start_drag = Some(*id);
            }
            if let Some((src, src_group)) = dragging_tab
                && (src != *id || src_group != group_idx)
                && let Some(pos) = pointer_pos
                && clip_rect.contains(pos)
            {
                let after = pos.x >= clip_rect.center().x;
                let line_x = if after {
                    clip_rect.right()
                } else {
                    clip_rect.left()
                };
                let mut to = if after {
                    pos_in_group + 1
                } else {
                    pos_in_group
                };
                if src_group == group_idx
                    && let Some(src_pos) = self.groups[group_idx].index_of(src)
                    && src_pos < to
                {
                    to = to.saturating_sub(1);
                }
                reorder = Some((src, src_group, to, line_x));
            }

            let active = active_id == Some(*id);
            let focused_tab = active && group_active;
            let bg = if active {
                theme.bg
            } else if tab_resp.hovered() {
                theme.hover
            } else {
                theme.panel_bg
            };
            ui.painter().rect_filled(clip_rect, 0.0, bg);
            ui.painter().line_segment(
                [
                    Pos2::new(tab_rect.right(), tab_rect.top() + 6.0),
                    Pos2::new(tab_rect.right(), tab_rect.bottom() - 6.0),
                ],
                Stroke::new(1.0, theme.border),
            );
            if focused_tab {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(clip_rect.left(), clip_rect.top()),
                        Pos2::new(clip_rect.right(), clip_rect.top() + 2.0),
                    ),
                    0.0,
                    theme.accent,
                );
            }

            let icon_rect = Rect::from_center_size(
                Pos2::new(tab_rect.left() + 18.0, tab_rect.center().y),
                Vec2::splat(15.0),
            );
            if tab.diff.is_some() {
                icons::paint(
                    &ui.painter().with_clip_rect(clip_rect),
                    icon_rect,
                    Icon::GitCompare,
                    theme.accent,
                    1.2,
                );
            } else if let Some(p) = &tab.doc.path {
                icons::paint_path_icon(
                    &ui.painter().with_clip_rect(clip_rect),
                    icon_rect,
                    p,
                    false,
                    false,
                    theme.text_dim,
                );
            } else {
                icons::file_glyph(
                    &ui.painter().with_clip_rect(clip_rect),
                    icon_rect,
                    theme.text_dim,
                    false,
                    false,
                );
            }

            let git_status = tab
                .doc
                .path
                .as_deref()
                .and_then(|p| self.git.status_of(p));
            let title_color = match git_status {
                Some(GitFileStatus::Modified) => theme.git_modified,
                Some(GitFileStatus::Added | GitFileStatus::Untracked) => theme.git_added,
                Some(GitFileStatus::Deleted) => theme.git_deleted,
                _ if focused_tab => theme.text,
                _ => theme.text_dim,
            };
            let title_rect = Rect::from_min_max(
                Pos2::new(tab_rect.left() + 31.0, tab_rect.top()),
                Pos2::new(tab_rect.right() - 26.0, tab_rect.bottom()),
            );
            ui.painter().with_clip_rect(clip_rect).text(
                Pos2::new(title_rect.left(), title_rect.center().y),
                Align2::LEFT_CENTER,
                &display_title,
                style.small(),
                title_color,
            );

            let right_icon_center = Pos2::new(tab_rect.right() - 16.0, tab_rect.center().y);
            if tab_resp.hovered() && !pinned {
                let close_rect = Rect::from_center_size(right_icon_center, Vec2::splat(16.0));
                icons::paint(
                    &ui.painter().with_clip_rect(clip_rect),
                    close_rect.shrink(3.5),
                    Icon::Close,
                    theme.text_dim,
                    1.3,
                );
            } else if pinned {
                let pin_rect = Rect::from_center_size(right_icon_center, Vec2::splat(14.0));
                icons::paint(
                    &ui.painter().with_clip_rect(clip_rect),
                    pin_rect.shrink(1.5),
                    Icon::Pinned,
                    theme.accent,
                    1.2,
                );
            } else if dirty {
                ui.painter()
                    .with_clip_rect(clip_rect)
                    .circle_filled(right_icon_center, 3.2, theme.accent);
            }

            let close_hit = ui.input(|i| i.pointer.primary_clicked())
                && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pos| {
                    Rect::from_center_size(right_icon_center, Vec2::splat(18.0)).contains(pos)
                })
                && tab_resp.hovered();
            if close_hit {
                if pinned {
                    toggle_pin = Some(*id);
                } else {
                    close = Some(*id);
                }
            } else if tab_resp.double_clicked() {
                toggle_pin = Some(*id);
            } else if tab_resp.clicked() {
                activate = Some(*id);
            }
            if tab_resp.middle_clicked() {
                close = Some(*id);
            }
            tab_resp.context_menu(|ui| {
                ui.set_width(212.0);
                if Self::context_item(
                    ui,
                    style,
                    Icon::Pin,
                    if pinned { "取消固定标签" } else { "固定标签 (Pin)" },
                    None,
                ) {
                    context_action = Some(TabAction::TogglePin(*id));
                    ui.close();
                }
                if Self::context_item(
                    ui,
                    style,
                    Icon::SplitHorizontal,
                    "在右侧新建分屏",
                    Some("Ctrl+\\"),
                ) {
                    context_action = Some(TabAction::Split(group_idx, *id));
                    ui.close();
                }
                ui.separator();
                if Self::context_item(ui, style, Icon::Close, "关闭", Some("Ctrl+W")) {
                    context_action = Some(TabAction::Close(group_idx, *id));
                    ui.close();
                }
                if Self::context_item(ui, style, Icon::Close, "关闭组内其他标签", None) {
                    context_action = Some(TabAction::CloseOthers(group_idx, *id));
                    ui.close();
                }
                ui.separator();
                if Self::context_item(ui, style, Icon::Copy, "复制文件路径", None) {
                    context_action = Some(TabAction::CopyPath(*id));
                    ui.close();
                }
            });
        }

        // Dragging over the empty part of the tab row appends to this group.
        if reorder.is_none()
            && let Some((src, src_group)) = dragging_tab
            && let Some(pos) = pointer_pos
            && rect.contains(pos)
            && pos.x >= active_tab_line_x - 4.0
            && pos.x < tabs_clip.right()
        {
            let mut to = ids.len();
            if src_group == group_idx
                && let Some(src_pos) = self.groups[group_idx].index_of(src)
                && src_pos < to
            {
                to = to.saturating_sub(1);
            }
            reorder = Some((
                src,
                src_group,
                to,
                tabs_clip.right().min(rect.right() - actions_reserve),
            ));
        }

        if let Some((src, src_group, to, line_x)) = reorder {
            ui.painter().line_segment(
                [
                    Pos2::new(line_x, rect.top() + 4.0),
                    Pos2::new(line_x, rect.bottom() - 4.0),
                ],
                Stroke::new(2.5, theme.accent),
            );
            if !ui.ctx().input(|i| i.pointer.primary_down()) {
                if src_group == group_idx {
                    self.reorder_group_tab(
                        group_idx,
                        self.groups[group_idx].index_of(src).unwrap_or(0),
                        to,
                    );
                } else {
                    self.move_tab_to_group(src, src_group, group_idx, to);
                }
                self.drag_payload = None;
            }
        }

        // Dropping an Explorer file on the tab row opens it in this group.
        if let Some(DragPayload::ExplorerPath(path)) = &self.drag_payload
            && let Some(pos) = pointer_pos
            && rect.contains(pos)
            && !ui.ctx().input(|i| i.pointer.primary_down())
            && path.is_file()
        {
            let path = path.clone();
            self.open_path_in_group(path, group_idx);
            self.drag_payload = None;
        }

        // Right side actions: close group, split, word wrap.
        let mut ax = rect.right() - 5.0;
        let action = |ui: &mut Ui, ax: &mut f32, icon: Icon, tip: &str| -> bool {
            *ax -= 24.0;
            let r = Rect::from_center_size(
                Pos2::new(*ax + 11.0, rect.center().y),
                Vec2::splat(22.0),
            );
            let resp = ui.interact(r, ui.scope_id().with(("group_act", group_idx, tip)), Sense::click());
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
                1.25,
            );
            resp.on_hover_text(tip).clicked()
        };
        let mut close_group = false;
        let close_group_tip = if group_idx == 0 {
            "关闭分屏（标签并入右侧）"
        } else {
            "关闭分屏（标签并入左侧）"
        };
        if self.groups.len() > 1 && action(ui, &mut ax, Icon::Close, close_group_tip) {
            close_group = true;
        }
        let mut split_group = false;
        if active_id.is_some()
            && action(ui, &mut ax, Icon::SplitHorizontal, "向右拆分编辑器 (Ctrl+\\)")
        {
            split_group = true;
        }
        if group_active && is_active_text
            && action(
                ui,
                &mut ax,
                Icon::WordWrap,
                "切换自动换行 (Alt+Z)",
            )
        {
            self.settings.word_wrap = !self.settings.word_wrap;
            self.settings.save();
        }

        if let Some(id) = start_drag {
            self.drag_payload = Some(DragPayload::Tab {
                id,
                group: group_idx,
            });
        }
        if let Some(id) = activate {
            self.focus_tab_in_group(group_idx, id);
        }
        if let Some(id) = toggle_pin
            && let Some(index) = self.tab_index(id)
            && let Some(tab) = self.tabs.get_mut(index)
        {
            if tab.preview {
                tab.preview = false;
            } else {
                tab.pinned = !tab.pinned;
            }
        }
        if let Some(id) = close
            && let Some(index) = self.tab_index(id)
        {
            self.close_tab_in_group(group_idx, index);
        }
        if split_group && let Some(id) = active_id {
            self.duplicate_view_in_new_group(id, group_idx + 1);
        }
        if close_group {
            self.close_group(group_idx);
        }
        if let Some(action) = context_action {
            match action {
                TabAction::TogglePin(id) => {
                    if let Some(index) = self.tab_index(id)
                        && let Some(tab) = self.tabs.get_mut(index)
                    {
                        tab.preview = false;
                        tab.pinned = !tab.pinned;
                    }
                }
                TabAction::Split(group, id) => {
                    self.duplicate_view_in_new_group(id, group + 1);
                }
                TabAction::Close(group, id) => {
                    if let Some(index) = self.tab_index(id) {
                        self.close_tab_in_group(group, index);
                    }
                }
                TabAction::CloseOthers(group, keep) => {
                    let ids: Vec<u64> = self
                        .groups
                        .get(group)
                        .map(|tabs| {
                            tabs.tabs
                                .iter()
                                .copied()
                                .filter(|id| *id != keep)
                                .collect()
                        })
                        .unwrap_or_default();
                    for id in ids {
                        if let Some(index) = self.tab_index(id)
                            && !self.tabs[index].is_dirty()
                            && !self.tabs[index].pinned
                        {
                            self.close_tab_force_in_group(group, index);
                        }
                    }
                    self.focus_tab_in_group(group, keep);
                }
                TabAction::CopyPath(id) => {
                    if let Some(index) = self.tab_index(id)
                        && let Some(path) = &self.tabs[index].doc.path
                    {
                        ui.ctx().copy_text(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    fn empty_group_ui(&mut self, ui: &mut Ui, style: &UiStyle, group_idx: usize) {
        let theme = style.theme;
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, theme.bg);
        let button = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.center().y - 10.0),
            Vec2::new(150.0, 30.0),
        );
        let resp = ui.interact(
            button,
            ui.scope_id().with(("empty_group_open", group_idx)),
            Sense::click(),
        );
        ui.painter().rect_filled(
            button,
            CornerRadius::same(6),
            if resp.hovered() { theme.hover } else { theme.active },
        );
        ui.painter().text(
            button.center(),
            Align2::CENTER_CENTER,
            "打开文件 (Ctrl+P)",
            style.small(),
            theme.text,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, button.bottom() + 16.0),
            Align2::CENTER_CENTER,
            "拖入标签页或文件以在此分屏打开",
            style.tiny(),
            theme.text_faint,
        );
        if resp.clicked() {
            self.open_palette(PaletteMode::QuickOpen);
        }
    }

    fn breadcrumb_ui(&mut self, ui: &mut Ui, style: &UiStyle, tab_idx: usize, group_idx: usize) {
        let theme = style.theme;
        let height = 26.0;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom() - 0.5),
                Pos2::new(rect.right(), rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );
        let Some(editor) = self.tabs.get(tab_idx) else {
            return;
        };
        let mut segments: Vec<(String, bool)> = Vec::new();
        if let Some(path) = &editor.doc.path {
            if let Some(root) = &self.workspace.root {
                if let Ok(rel) = path.strip_prefix(root) {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect();
                    if parts.is_empty() {
                        if let Some(name) = root.file_name() {
                            segments.push((name.to_string_lossy().to_string(), true));
                        }
                    } else {
                        for (i, part) in parts.iter().enumerate() {
                            segments.push((part.clone(), i + 1 == parts.len()));
                        }
                    }
                } else {
                    segments.push((path.to_string_lossy().to_string(), true));
                }
            } else {
                segments.push((path.to_string_lossy().to_string(), true));
            }
        } else {
            segments.push((editor.title(), true));
        }

        let caret_line = editor.doc.char_to_line(editor.caret);
        let active_symbol = editor
            .symbols
            .iter().rfind(|s| caret_line >= s.start_line && caret_line <= s.end_line)
            .cloned();

        let mut x = rect.left() + 14.0;
        for (index, (segment, is_last)) in segments.iter().enumerate() {
            if index > 0 {
                let chevron =
                    Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(11.0));
                icons::paint(
                    ui.painter(),
                    chevron,
                    Icon::ChevronRight,
                    theme.text_faint,
                    1.2,
                );
                x += 15.0;
            }
            if *is_last {
                let icon_rect = Rect::from_center_size(
                    Pos2::new(x + 6.0, rect.center().y),
                    Vec2::splat(13.0),
                );
                if let Some(p) = &editor.doc.path {
                    icons::paint_path_icon(ui.painter(), icon_rect, p, false, false, theme.text_dim);
                } else {
                    icons::file_glyph(ui.painter(), icon_rect, theme.text_dim, false, false);
                }
                x += 16.0;
            }
            let width = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        segment.clone(),
                        style.small(),
                        theme.text,
                    ))
                    .rect
                    .width()
            });
            ui.painter().text(
                Pos2::new(x, rect.center().y),
                Align2::LEFT_CENTER,
                segment,
                style.small(),
                if *is_last { theme.text } else { theme.text_dim },
            );
            x += width + 4.0;
        }

        // Enclosing Symbol Segment (clickable to open Goto Symbol palette)
        let mut open_symbols = false;
        if let Some(sym) = active_symbol {
            let chevron =
                Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(11.0));
            icons::paint(
                ui.painter(),
                chevron,
                Icon::ChevronRight,
                theme.text_faint,
                1.2,
            );
            x += 15.0;
            let sym_w = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        sym.name.clone(),
                        style.small(),
                        theme.text,
                    ))
                    .rect
                    .width()
            }) + 20.0;
            let sym_rect = Rect::from_min_size(Pos2::new(x, rect.top() + 2.0), Vec2::new(sym_w, height - 4.0));
            let s_resp = ui.interact(sym_rect, ui.scope_id().with("bc_symbol"), Sense::click());
            if s_resp.hovered() {
                ui.painter()
                    .rect_filled(sym_rect, CornerRadius::same(4), theme.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(Pos2::new(x + 8.0, rect.center().y), Vec2::splat(13.0)),
                sym.kind.icon(),
                theme.accent,
                1.2,
            );
            ui.painter().text(
                Pos2::new(x + 18.0, rect.center().y),
                Align2::LEFT_CENTER,
                &sym.name,
                style.small(),
                theme.text_dim,
            );
            if s_resp.on_hover_text("点击跳转到文档符号 (Ctrl+Shift+.)").clicked() {
                open_symbols = true;
            }
        }

        // Language & Markdown view mode pills on the right of the breadcrumb
        let is_diff = editor.diff.is_some();
        let is_md = !is_diff && editor.language == crate::highlight::Language::Markdown;
        let cur_md_mode = editor.md_mode;
        let is_tabular = !is_diff
            && editor.multimodal.as_ref().is_some_and(|m| {
                matches!(m, crate::viewer::MultimodalViewer::Spreadsheet(_))
                    && editor.doc.path.as_deref().is_some_and(|p| {
                        matches!(
                            crate::viewer::MultimodalKind::from_path(p),
                            Some(
                                crate::viewer::MultimodalKind::Csv
                                    | crate::viewer::MultimodalKind::Tsv
                            )
                        )
                    })
            });
        let cur_tabular_src = editor
            .multimodal
            .as_ref()
            .is_some_and(|m| m.is_showing_source());
        let language = if is_diff {
            "Git 差异"
        } else if let Some(mm) = &editor.multimodal {
            mm.kind().label()
        } else if editor.doc.encoding == crate::doc::Encoding::Binary {
            "二进制文件"
        } else {
            editor.language.name()
        };
        ui.painter().text(
            Pos2::new(rect.right() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            language,
            style.tiny(),
            theme.text_faint,
        );
        let mut next_tabular_src: Option<bool> = None;
        if is_tabular && rect.width() > 360.0 {
            let items = [(false, "表格"), (true, "源码")];
            let salt = format!("bc_tab_{group_idx}");
            if let Some(sel) = segmented_capsule(
                ui,
                style,
                &salt,
                rect.right() - 78.0,
                rect.center().y,
                &items,
                &cur_tabular_src,
            ) {
                next_tabular_src = Some(sel);
            }
        }
        if let Some(flag) = next_tabular_src
            && let Some(tab) = self.tabs.get_mut(tab_idx)
            && let Some(mm) = &mut tab.multimodal
        {
            if !flag {
                let text = tab.doc.text();
                if let Some(path) = tab.doc.path.as_deref()
                    && let Some(kind) = crate::viewer::MultimodalKind::from_path(path)
                {
                    mm.sync_tabular_text(&text, kind);
                }
            }
            mm.set_show_source(flag);
        }
        let mut next_md_mode = None;
        if is_md && rect.width() > 360.0 {
            let items = [
                (crate::markdown::MarkdownViewMode::Preview, "预览"),
                (crate::markdown::MarkdownViewMode::Split, "分屏"),
                (crate::markdown::MarkdownViewMode::Source, "源码"),
            ];
            let salt = format!("bc_md_{group_idx}");
            if let Some(sel) = segmented_capsule(
                ui,
                style,
                &salt,
                rect.right() - 78.0,
                rect.center().y,
                &items,
                &cur_md_mode,
            ) {
                next_md_mode = Some(sel);
            }
        }
        if let Some(mode) = next_md_mode
            && let Some(tab) = self.tabs.get_mut(tab_idx)
        {
            if mode == crate::markdown::MarkdownViewMode::Split {
                tab.md_preview.split_synced = false;
            }
            tab.md_mode = mode;
        }

        if open_symbols {
            self.open_palette(PaletteMode::Symbols);
        }
    }

    fn render_pane(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        editor_style: &crate::editor::EditorStyle,
        tab_idx: usize,
        pane_tag: &str,
    ) -> Option<crate::editor::EditorEvent> {
        let tab = self.tabs.get(tab_idx)?;
        if let Some(diff_id) = tab.diff.as_ref().map(|_| tab.id) {
            let mut action = crate::diff::DiffAction::None;
            if let Some(ed) = self.tabs.get_mut(tab_idx)
                && let Some(diff) = ed.diff.as_mut()
            {
                action = crate::diff::diff_ui(ui, style, editor_style, diff);
            }
            match action {
                crate::diff::DiffAction::OpenFile(path) => self.open_file(path),
                crate::diff::DiffAction::Refresh => self.spawn_diff_load(diff_id),
                crate::diff::DiffAction::None => {}
            }
            return None;
        }
        if let Some(mm) = &tab.multimodal
            && !mm.is_showing_source()
        {
            let mut viewer_events = Vec::new();
            if let Some(ed) = self.tabs.get_mut(tab_idx)
                && let Some(mm) = &mut ed.multimodal
            {
                viewer_events = mm.ui(ui, style);
            }
            if !viewer_events.is_empty() {
                let ctx = ui.ctx().clone();
                self.handle_viewer_events(viewer_events, &ctx);
            }
            return None;
        }
        if tab.doc.encoding == crate::doc::Encoding::Binary {
            let path = tab.doc.path.clone();
            let title = tab.title();
            self.unreadable_binary_ui(ui, style, path.as_deref(), &title);
            return None;
        }
        let is_md = tab.language == crate::highlight::Language::Markdown;
        let md_mode = if is_md {
            tab.md_mode
        } else {
            crate::markdown::MarkdownViewMode::Source
        };

        // Support Ctrl + scroll wheel font zoom on Markdown preview areas too
        if md_mode != crate::markdown::MarkdownViewMode::Source {
            let pane_rect = ui.available_rect_before_wrap();
            let hover_pos = ui.ctx().input(|i| i.pointer.hover_pos());
            let steps = ctrl_wheel_steps(ui.ctx());
            if steps != 0 && hover_pos.is_some_and(|p| pane_rect.contains(p)) {
                self.settings.font_size =
                    (self.settings.font_size + steps as f32).clamp(9.0, 30.0);
                self.settings.save();
            }
        }

        match md_mode {
            crate::markdown::MarkdownViewMode::Source => {
                self.tabs.get_mut(tab_idx).and_then(|ed| ed.ui(ui, editor_style))
            }
            crate::markdown::MarkdownViewMode::Preview => {
                let mut prev_events = Vec::new();
                let workspace_root = self.workspace.root.clone();
                if let Some(ed) = self.tabs.get_mut(tab_idx) {
                    let version = ed.doc.version;
                    let text = ed.doc.text();
                    let path = ed.doc.path.clone();
                    prev_events = crate::markdown::render::preview_ui(
                        ui,
                        style,
                        &mut ed.md_preview,
                        &mut ed.md_mode,
                        version,
                        &text,
                        path.as_deref(),
                        workspace_root.as_deref(),
                        self.settings.font_size,
                    );
                }
                if !prev_events.is_empty() {
                    let ctx = ui.ctx().clone();
                    self.handle_markdown_preview_events(tab_idx, prev_events, &ctx);
                }
                None
            }
            crate::markdown::MarkdownViewMode::Split => {
                let avail = ui.available_rect_before_wrap();
                let ratio = self
                    .tabs
                    .get(tab_idx)
                    .map(|t| t.md_split_ratio.clamp(0.15, 0.85))
                    .unwrap_or(0.5);
                let usable_w = (avail.width() - 5.0).max(240.0);
                let left_w = (usable_w * ratio).clamp(120.0, usable_w - 120.0);
                let left_rect = Rect::from_min_size(avail.min, Vec2::new(left_w, avail.height()));
                let sep_rect = Rect::from_min_size(
                    Pos2::new(left_rect.right(), avail.top()),
                    Vec2::new(5.0, avail.height()),
                );
                let right_rect =
                    Rect::from_min_max(Pos2::new(sep_rect.right(), avail.top()), avail.max);

                let mut ev = None;
                let mut source_before = (0.0f32, 0usize, false);
                let mut source_after = (0.0f32, 0usize, false);
                ui.scope_builder(UiBuilder::new().max_rect(left_rect), |ui| {
                    if let Some(ed) = self.tabs.get_mut(tab_idx) {
                        source_before = (ed.scroll_y, ed.caret, ed.has_focus);
                        ev = ed.ui(ui, editor_style);
                        source_after = (ed.scroll_y, ed.caret, ed.has_focus);
                    }
                });

                let sep_resp = ui.interact(
                    sep_rect,
                    ui.scope_id().with(("md_split_sep", pane_tag, tab_idx)),
                    Sense::click_and_drag(),
                );
                let sep_dragging = sep_resp.dragged();
                if sep_resp.hovered() || sep_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().rect_filled(
                    sep_rect,
                    0.0,
                    if sep_resp.hovered() || sep_resp.dragged() {
                        style.theme.accent
                    } else {
                        style.theme.border_strong
                    },
                );
                if sep_dragging
                    && let Some(pos) = sep_resp.interact_pointer_pos()
                    && let Some(ed) = self.tabs.get_mut(tab_idx)
                {
                    ed.md_split_ratio = ((pos.x - avail.left()) / avail.width().max(1.0))
                        .clamp(0.15, 0.85);
                } else if sep_resp.double_clicked()
                    && let Some(ed) = self.tabs.get_mut(tab_idx)
                {
                    ed.md_split_ratio = 0.5;
                }

                let preview_before = self
                    .tabs
                    .get(tab_idx)
                    .map(|ed| ed.md_preview.scroll_y)
                    .unwrap_or(0.0);
                let mut prev_events = Vec::new();
                let workspace_root = self.workspace.root.clone();
                ui.scope_builder(UiBuilder::new().max_rect(right_rect), |ui| {
                    if let Some(ed) = self.tabs.get_mut(tab_idx) {
                        let version = ed.doc.version;
                        let text = ed.doc.text();
                        let path = ed.doc.path.clone();
                        prev_events = crate::markdown::render::preview_ui(
                            ui,
                            style,
                            &mut ed.md_preview,
                            &mut ed.md_mode,
                            version,
                            &text,
                            path.as_deref(),
                            workspace_root.as_deref(),
                            self.settings.font_size,
                        );
                    }
                });

                // Keep source scrolling and rendered preview in sync.
                if !sep_dragging
                    && let Some(ed) = self.tabs.get(tab_idx)
                {
                    let line_h = editor_style.line_h.max(1.0);
                    let total_lines = ed.doc.line_count().max(1) as f32;
                    let preview_after = ed.md_preview.scroll_y;
                    let source_moved = (source_after.0 - source_before.0).abs() > 0.5;
                    let preview_moved = (preview_after - preview_before).abs() > 0.5;
                    let caret_moved = source_after.1 != source_before.1;
                    let hover = ui.ctx().input(|i| i.pointer.hover_pos());
                    let over_source = hover.is_some_and(|pos| left_rect.contains(pos));
                    let over_preview = hover.is_some_and(|pos| right_rect.contains(pos));
                    let hold_source = ed.md_preview.hold_source_sync;
                    let force_sync = !ed.md_preview.split_synced;
                    let source_has_focus = source_after.2;
                    let drive_source = !hold_source
                        && (force_sync
                            || (source_moved && !preview_moved)
                            || (source_moved && preview_moved && over_source)
                            || (source_moved && over_source)
                            || (!source_moved
                                && !preview_moved
                                && caret_moved
                                && source_has_focus));
                    let drive_preview = !drive_source
                        && ((preview_moved && !source_moved)
                            || (preview_moved && over_preview));

                    if drive_source {
                        let top_line = ed.top_visible_line(line_h);
                        let target = preview_y_for_source_line(
                            &ed.md_preview,
                            top_line,
                            total_lines,
                            self.settings.font_size,
                        );
                        if (target - preview_after).abs() > 2.0
                            && let Some(ed) = self.tabs.get_mut(tab_idx)
                        {
                            ed.md_preview.pending_scroll_y = Some(target);
                            ed.md_preview.split_synced = true;
                            ui.ctx().request_repaint();
                        }
                    } else if drive_preview {
                        let line = source_line_for_preview_y(
                            &ed.md_preview,
                            preview_after,
                            total_lines,
                            self.settings.font_size,
                        );
                        if let Some(ed) = self.tabs.get_mut(tab_idx) {
                            let before = ed.scroll_y;
                            ed.scroll_to_line(line, line_h);
                            ed.md_preview.split_synced = true;
                            if (ed.scroll_y - before).abs() > 0.5 {
                                ed.md_preview.hold_source_sync = true;
                                ui.ctx().request_repaint();
                            }
                        }
                    } else if hold_source
                        && let Some(ed) = self.tabs.get_mut(tab_idx) {
                            ed.md_preview.hold_source_sync = false;
                        }
                }

                ui.advance_cursor_after_rect(avail);
                if !prev_events.is_empty() {
                    let ctx = ui.ctx().clone();
                    self.handle_markdown_preview_events(tab_idx, prev_events, &ctx);
                }
                ev
            }
        }
    }

    fn statusbar_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.status_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.top() + 0.5),
                Pos2::new(rect.right(), rect.top() + 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );
        let mut x = rect.left() + 10.0;
        let text = |ui: &Ui, x: &mut f32, s: &str, color: Color32, font: FontId| -> Rect {
            let width = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        s.to_string(),
                        font.clone(),
                        color,
                    ))
                    .rect
                    .width()
            });
            let r = Rect::from_min_size(Pos2::new(*x, rect.center().y - 9.0), Vec2::new(width, 18.0));
            ui.painter().text(
                Pos2::new(*x, rect.center().y),
                Align2::LEFT_CENTER,
                s,
                font,
                color,
            );
            *x += width;
            r
        };

        // 1. Git Branch indicator (clickable to switch branch)
        if self.git.snapshot.is_repo && !self.git.snapshot.branch.is_empty() {
            let icon_r = Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(13.0));
            icons::paint(ui.painter(), icon_r, Icon::GitBranch, theme.accent, 1.2);
            x += 16.0;
            let branch_label = if self.git.snapshot.changes.is_empty() {
                self.git.snapshot.branch.clone()
            } else {
                format!("{}*", self.git.snapshot.branch)
            };
            let br_rect = text(ui, &mut x, &branch_label, theme.text, style.small());
            let mut hit = Rect::from_min_max(icon_r.min, br_rect.max);
            if self.git.snapshot.ahead > 0 || self.git.snapshot.behind > 0 {
                x += 6.0;
                let counts = format!(
                    "↑{} ↓{}",
                    self.git.snapshot.ahead, self.git.snapshot.behind
                );
                let counts_rect = text(ui, &mut x, &counts, theme.text_faint, style.tiny());
                hit = hit.union(counts_rect);
            }
            let hit = hit.expand2(Vec2::new(4.0, 2.0));
            let resp = ui.interact(hit, ui.scope_id().with("status_git_branch"), Sense::click());
            if resp.on_hover_text("点击切换 Git 分支").clicked() {
                self.open_palette(PaletteMode::Branches);
            }
            x += 14.0;
        }

        // 2. Folder name
        if let Some(folder) = self.workspace.root.as_ref().and_then(|p| p.file_name()) {
            text(
                ui,
                &mut x,
                &folder.to_string_lossy(),
                theme.text_dim,
                style.small(),
            );
        } else {
            text(
                ui,
                &mut x,
                "没有打开文件夹",
                theme.text_faint,
                style.small(),
            );
        }
        x += 14.0;

        // 3. Error & Warning Diagnostic Counters (clickable to open Problems panel)
        let mut err_count = 0usize;
        let mut warn_count = 0usize;
        for tab in &self.tabs {
            for d in &tab.diagnostics {
                match d.severity {
                    DiagSeverity::Error => err_count += 1,
                    DiagSeverity::Warning => warn_count += 1,
                    DiagSeverity::Info => {}
                }
            }
        }
        let diag_start_x = x;
        let er_r = Rect::from_center_size(Pos2::new(x + 6.0, rect.center().y), Vec2::splat(12.0));
        icons::paint(
            ui.painter(),
            er_r,
            Icon::Error,
            if err_count > 0 {
                theme.error
            } else {
                theme.text_faint
            },
            1.2,
        );
        x += 14.0;
        text(
            ui,
            &mut x,
            &err_count.to_string(),
            if err_count > 0 {
                theme.error
            } else {
                theme.text_faint
            },
            style.tiny(),
        );
        x += 8.0;
        let wn_r = Rect::from_center_size(Pos2::new(x + 6.0, rect.center().y), Vec2::splat(12.0));
        icons::paint(
            ui.painter(),
            wn_r,
            Icon::Warning,
            if warn_count > 0 {
                theme.warning
            } else {
                theme.text_faint
            },
            1.2,
        );
        x += 14.0;
        text(
            ui,
            &mut x,
            &warn_count.to_string(),
            if warn_count > 0 {
                theme.warning
            } else {
                theme.text_faint
            },
            style.tiny(),
        );
        let diag_hit = Rect::from_min_max(
            Pos2::new(diag_start_x - 2.0, rect.top()),
            Pos2::new(x + 2.0, rect.bottom()),
        );
        if ui
            .interact(diag_hit, ui.scope_id().with("status_problems"), Sense::click())
            .on_hover_text("显示问题面板 (Ctrl+Shift+M)")
            .clicked()
        {
            self.bottom.visible = true;
            self.bottom.tab = BottomTab::Problems;
        }
        x += 14.0;

        if let Some(editor) = self.tabs.get(self.active)
            && editor.is_dirty()
        {
            text(ui, &mut x, "● 未保存", theme.accent, style.small());
        }

        // Right side
        let mut rx = rect.right() - 12.0;
        let mut item = |ui: &mut Ui, text_str: &str, color: Color32| -> Rect {
            let width = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        text_str.to_string(),
                        style.small(),
                        color,
                    ))
                    .rect
                    .width()
            });
            rx -= width;
            let item_rect =
                Rect::from_min_size(Pos2::new(rx, rect.center().y - 8.0), Vec2::new(width, 16.0));
            ui.painter().text(
                Pos2::new(rx, rect.center().y),
                Align2::LEFT_CENTER,
                text_str,
                style.small(),
                color,
            );
            rx -= 16.0;
            item_rect
        };
        let mut zoom_rect = None;
        if (self.settings.ui_zoom - 1.0).abs() > 0.01 {
            zoom_rect = Some(item(
                ui,
                &format!("缩放 {:.0}%", self.settings.ui_zoom * 100.0),
                theme.accent,
            ));
        }
        let mut language_rect = None;
        let mut active_language = None;
        let mut pos_rect = None;
        if let Some(editor) = self.tabs.get(self.active) {
            if let Some(mm) = &editor.multimodal
                && !mm.is_showing_source()
            {
                let size_str = editor
                    .doc
                    .path
                    .as_deref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| crate::viewer::image_view::format_bytes(m.len()))
                    .unwrap_or_default();
                item(ui, mm.kind().label(), theme.text_dim);
                if !size_str.is_empty() {
                    item(ui, &size_str, theme.text_faint);
                }
            } else if editor.doc.encoding == crate::doc::Encoding::Binary {
                let size_str = editor
                    .doc
                    .path
                    .as_deref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| crate::viewer::image_view::format_bytes(m.len()))
                    .unwrap_or_default();
                item(ui, "二进制文件 (不可读取)", theme.warning);
                if !size_str.is_empty() {
                    item(ui, &size_str, theme.text_faint);
                }
            } else if editor.diff.is_some() {
                item(ui, "Git 差异", theme.text_dim);
            } else {
                language_rect = Some(item(ui, editor.language.name(), theme.text_dim));
                active_language = Some(editor.language);
                item(ui, editor.doc.eol.as_str(), theme.text_faint);
                item(ui, editor.doc.encoding.as_str(), theme.text_faint);
                item(
                    ui,
                    &format!(
                        "{}:{}",
                        self.settings.tab_size,
                        if self.settings.insert_spaces {
                            "空格"
                        } else {
                            "制表符"
                        }
                    ),
                    theme.text_faint,
                );
                let (line, col) = editor.doc.char_to_line_col(editor.caret);
                let pos = if editor.extra_cursors.is_empty() {
                    format!("行 {}, 列 {}", line + 1, col + 1)
                } else {
                    format!(
                        "行 {}, 列 {} ({} 个光标)",
                        line + 1,
                        col + 1,
                        editor.extra_cursors.len() + 1
                    )
                };
                pos_rect = Some(item(ui, &pos, theme.text_dim));
            }
        }
        if let Some(z_rect) = zoom_rect
            && ui
                .interact(
                    z_rect.expand2(Vec2::new(4.0, 4.0)),
                    ui.scope_id().with("status_zoom_reset"),
                    Sense::click(),
                )
                .on_hover_text("点击重置界面缩放为 100% (Ctrl+0)")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.set_ui_zoom(1.0, &ctx);
            }
        if let Some(p_rect) = pos_rect
            && ui
                .interact(
                    p_rect.expand2(Vec2::new(4.0, 4.0)),
                    ui.scope_id().with("status_goto_line"),
                    Sense::click(),
                )
                .on_hover_text("转到行 (Ctrl+G)")
                .clicked()
            {
                self.open_palette(PaletteMode::GotoLine);
            }
        if let Some(lang_rect) = language_rect
            && let Some(language) = active_language
        {
            let resp = ui.interact(
                lang_rect.expand2(Vec2::new(4.0, 4.0)),
                ui.scope_id().with("status_language"),
                Sense::click(),
            );
            if resp.hovered() {
                ui.painter().rect_filled(
                    lang_rect.expand2(Vec2::new(4.0, 3.0)),
                    CornerRadius::same(4),
                    theme.hover,
                );
                ui.painter().text(
                    Pos2::new(lang_rect.left(), rect.center().y),
                    Align2::LEFT_CENTER,
                    language.name(),
                    style.small(),
                    theme.text,
                );
            }
            if resp.on_hover_text("点击切换语言模式").clicked() {
                self.open_palette(PaletteMode::Language);
                self.palette.focus = true;
            }
        }

        let plugin_items: Vec<(String, Option<String>)> = self
            .plugins
            .status_items()
            .iter()
            .map(|item| (item.text.clone(), item.command.clone()))
            .collect();
        for (index, (text, command)) in plugin_items.into_iter().enumerate() {
            let rect_item = item(ui, &text, theme.text_dim);
            if let Some(command) = command {
                let resp = ui.interact(
                    rect_item.expand2(Vec2::new(4.0, 4.0)),
                    ui.scope_id().with(("status_plugin", index)),
                    Sense::click(),
                );
                if resp.clicked()
                    && let Some(command_index) = self.plugins.command_index_of_id(&command) {
                        self.run_plugin_command(command_index);
                    }
            }
        }
    }

    fn welcome_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.bg);
        let center_x = rect.center().x;
        let mut y = rect.top() + (rect.height() * 0.16).max(36.0);

        let logo = Rect::from_center_size(Pos2::new(center_x, y + 30.0), Vec2::splat(60.0));
        ui.painter()
            .rect_filled(logo, CornerRadius::same(14), theme.accent);
        ui.painter().text(
            logo.center(),
            Align2::CENTER_CENTER,
            "K",
            style.font(31.0),
            theme.accent_text,
        );
        y += 80.0;
        ui.painter().text(
            Pos2::new(center_x, y),
            Align2::CENTER_CENTER,
            "Kestrel",
            style.font(26.0),
            theme.text,
        );
        y += 28.0;
        ui.painter().text(
            Pos2::new(center_x, y),
            Align2::CENTER_CENTER,
            "轻量 · 极速 · 现代的 Rust 代码编辑器",
            style.font(13.0),
            theme.text_dim,
        );
        y += 42.0;

        let footer = |ui: &mut Ui| {
            ui.painter().text(
                Pos2::new(center_x, rect.bottom() - 24.0),
                Align2::CENTER_CENTER,
                format!(
                    "Kestrel {} · 使用 Rust 构建 · 主题: {}",
                    crate::app::APP_VERSION,
                    theme.kind.label()
                ),
                style.tiny(),
                theme.text_faint,
            );
        };

        let has_folder = self.workspace.root.is_some();
        if has_folder {
            let folder = self
                .workspace
                .root
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            ui.painter().text(
                Pos2::new(center_x, y),
                Align2::CENTER_CENTER,
                format!("{folder} 已打开，选择一个文件开始编辑"),
                style.small(),
                theme.text_faint,
            );
            y += 34.0;
            let button =
                Rect::from_center_size(Pos2::new(center_x - 102.0, y), Vec2::new(186.0, 32.0));
            if self.primary_button(ui, button, "快速打开文件  Ctrl+P", style) {
                self.open_palette(PaletteMode::QuickOpen);
            }
            let button2 =
                Rect::from_center_size(Pos2::new(center_x + 102.0, y), Vec2::new(186.0, 32.0));
            if self.secondary_button(ui, button2, "命令面板  Ctrl+Shift+P", style) {
                self.open_palette(PaletteMode::Commands);
            }
            y += 46.0;
            self.shortcut_hints(ui, style, center_x, y);
            footer(ui);
            return;
        }

        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(210.0, 32.0));
        if self.primary_button(ui, button, "打开文件夹  Ctrl+Shift+O", style) {
            self.open_folder_dialog();
        }
        y += 40.0;
        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(210.0, 30.0));
        if self.secondary_button(ui, button, "打开文件  Ctrl+O", style) {
            self.open_file_dialog();
        }
        y += 40.0;
        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(210.0, 30.0));
        if self.secondary_button(ui, button, "新建文件  Ctrl+N", style) {
            self.new_untitled();
        }

        let recents: Vec<String> = self
            .settings
            .recent_folders
            .iter()
            .filter(|f| std::path::Path::new(f).is_dir())
            .take(6)
            .cloned()
            .collect();
        if !recents.is_empty() {
            y += 48.0;
            ui.painter().text(
                Pos2::new(center_x, y),
                Align2::CENTER_CENTER,
                "最近打开的项目",
                style.tiny(),
                theme.text_faint,
            );
            y += 22.0;
            for recent in &recents {
                let name = std::path::Path::new(recent)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| recent.clone());
                let row = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(330.0, 26.0));
                let resp = ui.interact(row, ui.scope_id().with(("recent", recent)), Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(row, CornerRadius::same(5), theme.hover);
                }
                icons::paint_path_icon(
                    ui.painter(),
                    Rect::from_center_size(
                        Pos2::new(row.left() + 14.0, row.center().y),
                        Vec2::splat(14.0),
                    ),
                    std::path::Path::new(recent),
                    true,
                    false,
                    theme.accent,
                );
                ui.painter().text(
                    Pos2::new(row.left() + 28.0, row.center().y),
                    Align2::LEFT_CENTER,
                    &name,
                    style.small(),
                    theme.text,
                );
                let path_label = elide_middle(recent, 42);
                ui.painter().with_clip_rect(row).text(
                    Pos2::new(row.right() - 10.0, row.center().y),
                    Align2::RIGHT_CENTER,
                    path_label,
                    style.tiny(),
                    theme.text_faint,
                );
                if resp.clicked() {
                    let path = PathBuf::from(recent);
                    self.open_folder(path);
                }
                y += 28.0;
            }
        }

        footer(ui);
    }

    fn shortcut_hints(&self, ui: &mut Ui, style: &UiStyle, center_x: f32, y: f32) {
        let theme = style.theme;
        let hints = [
            ("快速打开", "Ctrl+P"),
            ("命令面板", "Ctrl+Shift+P"),
            ("转到符号", "Ctrl+Shift+."),
            ("多光标选择", "Ctrl+D"),
        ];
        let mut x = center_x - 260.0;
        for (label, key) in hints {
            ui.painter().text(
                Pos2::new(x, y),
                Align2::LEFT_CENTER,
                key,
                style.tiny(),
                theme.text_dim,
            );
            let kw = ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        key.to_string(),
                        style.tiny(),
                        theme.text_dim,
                    ))
                    .rect
                    .width()
            });
            ui.painter().text(
                Pos2::new(x + kw + 6.0, y),
                Align2::LEFT_CENTER,
                label,
                style.tiny(),
                theme.text_faint,
            );
            x += 136.0;
        }
    }

    fn unreadable_binary_ui(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        path: Option<&std::path::Path>,
        title: &str,
    ) {
        let theme = style.theme;
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.bg);

        let card_w = (rect.width() - 48.0).clamp(280.0, 460.0);
        let card_h = 224.0;
        let card = Rect::from_center_size(rect.center(), Vec2::new(card_w, card_h));
        ui.painter()
            .rect_filled(card, CornerRadius::same(10), theme.panel_bg);
        ui.painter().rect_stroke(
            card,
            CornerRadius::same(10),
            Stroke::new(1.0, theme.border_strong),
            egui::StrokeKind::Inside,
        );

        let badge = Rect::from_center_size(
            Pos2::new(card.center().x, card.top() + 38.0),
            Vec2::splat(38.0),
        );
        ui.painter().rect_filled(
            badge,
            CornerRadius::same(9),
            theme.warning.gamma_multiply(0.16),
        );
        icons::paint(
            ui.painter(),
            badge.shrink(9.0),
            Icon::Warning,
            theme.warning,
            1.6,
        );

        ui.painter().text(
            Pos2::new(card.center().x, card.top() + 74.0),
            Align2::CENTER_CENTER,
            "无法在编辑器中读取此文件",
            style.font(15.0),
            theme.text,
        );
        ui.painter().text(
            Pos2::new(card.center().x, card.top() + 98.0),
            Align2::CENTER_CENTER,
            "该文件为不支持直接预览的二进制格式",
            style.small(),
            theme.text_dim,
        );

        let meta_line = if let Some(p) = path {
            let sig = crate::doc::detect_binary_signature(p);
            let size = std::fs::metadata(p)
                .map(|m| crate::viewer::image_view::format_bytes(m.len()))
                .unwrap_or_default();
            if size.is_empty() {
                format!("{title}  ·  {sig}")
            } else {
                format!("{title}  ·  {sig}  ·  {size}")
            }
        } else {
            title.to_string()
        };
        ui.painter().text(
            Pos2::new(card.center().x, card.top() + 124.0),
            Align2::CENTER_CENTER,
            elide_middle(&meta_line, 54),
            style.tiny(),
            theme.text_faint,
        );

        if let Some(p) = path {
            let btn_w = 136.0;
            let btn_h = 30.0;
            let gap = 12.0;
            let total_w = btn_w * 2.0 + gap;
            let y = card.bottom() - 44.0;
            let open_r = Rect::from_min_size(
                Pos2::new(card.center().x - total_w * 0.5, y),
                Vec2::new(btn_w, btn_h),
            );
            let reveal_r = Rect::from_min_size(
                Pos2::new(open_r.right() + gap, y),
                Vec2::new(btn_w, btn_h),
            );
            if self.primary_button(ui, open_r, "用默认应用打开", style) {
                self.open_in_default_app(p);
            }
            if self.secondary_button(ui, reveal_r, "在资源管理器中显示", style) {
                crate::workspace::tree::reveal_in_explorer(p);
            }
        }
    }

    pub(crate) fn primary_button(
        &self,
        ui: &mut Ui,
        rect: Rect,
        label: &str,
        style: &UiStyle,
    ) -> bool {
        let theme = style.theme;
        let resp = ui.interact(rect, ui.scope_id().with(("primary", label)), Sense::click());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(7),
            if resp.hovered() {
                theme.accent.gamma_multiply(0.92)
            } else {
                theme.accent
            },
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            style.small(),
            theme.accent_text,
        );
        resp.clicked()
    }

    pub(crate) fn secondary_button(
        &self,
        ui: &mut Ui,
        rect: Rect,
        label: &str,
        style: &UiStyle,
    ) -> bool {
        let theme = style.theme;
        let resp = ui.interact(
            rect,
            ui.scope_id().with(("secondary", label)),
            Sense::click(),
        );
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(7),
            if resp.hovered() {
                theme.hover
            } else {
                theme.input_bg
            },
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.0, theme.border_strong),
            egui::StrokeKind::Inside,
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
}

enum TabAction {
    TogglePin(u64),
    Split(usize, u64),
    Close(usize, u64),
    CloseOthers(usize, u64),
    CopyPath(u64),
}

/// Returns discrete Ctrl+wheel zoom steps (+1 / -1) for the current frame from MouseWheel events.
pub fn ctrl_wheel_steps(ctx: &egui::Context) -> i32 {
    ctx.input(|i| {
        if !(i.modifiers.ctrl || i.modifiers.command) {
            return 0;
        }
        let raw_y: f32 = i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                _ => None,
            })
            .sum();
        if raw_y > 0.05 {
            1
        } else if raw_y < -0.05 {
            -1
        } else {
            0
        }
    })
}

/// Sleek pill-capsule segmented toggle used on the breadcrumb bar.
fn sorted_line_anchors(
    state: &crate::markdown::MarkdownPreviewState,
) -> Vec<(f32, f32)> {
    let mut anchors: Vec<(f32, f32)> = state
        .line_anchors
        .iter()
        .map(|(line, y)| (*line as f32, *y))
        .collect();
    anchors.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    anchors.dedup_by(|a, b| (a.0 - b.0).abs() < 0.5);
    anchors
}

fn preview_y_for_source_line(
    state: &crate::markdown::MarkdownPreviewState,
    line: f32,
    total_lines: f32,
    font_size: f32,
) -> f32 {
    let anchors = sorted_line_anchors(state);
    if anchors.is_empty() {
        let height = state.content_height.max(1.0);
        return (line / total_lines.max(1.0) * height).max(0.0);
    }
    let estimated = (font_size * 1.7).max(10.0);
    let first = anchors[0];
    if line <= first.0 {
        if first.0 <= 0.5 {
            return first.1;
        }
        return (line / first.0 * first.1).max(0.0);
    }
    for index in 0..anchors.len() {
        if index + 1 == anchors.len() {
            let last = anchors[index];
            let slope = if index > 0 {
                let prev = anchors[index - 1];
                let delta = last.0 - prev.0;
                if delta > 0.5 {
                    (last.1 - prev.1) / delta
                } else {
                    estimated
                }
            } else {
                estimated
            };
            return (last.1 + (line - last.0) * slope).max(0.0);
        }
        let a = anchors[index];
        let b = anchors[index + 1];
        if line >= a.0 && line <= b.0 {
            let delta = b.0 - a.0;
            let t = if delta > 0.5 {
                (line - a.0) / delta
            } else {
                0.0
            };
            return (a.1 + (b.1 - a.1) * t).max(0.0);
        }
    }
    state.content_height.max(0.0)
}

fn source_line_for_preview_y(
    state: &crate::markdown::MarkdownPreviewState,
    y: f32,
    total_lines: f32,
    font_size: f32,
) -> f32 {
    let anchors = sorted_line_anchors(state);
    if anchors.is_empty() {
        let height = state.content_height.max(1.0);
        let fraction = (y / height).clamp(0.0, 1.0);
        return fraction * total_lines;
    }
    let estimated = (font_size * 1.7).max(10.0);
    let first = anchors[0];
    if y <= first.1 {
        if first.1 <= 0.5 {
            return first.0;
        }
        return (y / first.1 * first.0).max(0.0);
    }
    for index in 0..anchors.len() {
        if index + 1 == anchors.len() {
            let last = anchors[index];
            let slope = if index > 0 {
                let prev = anchors[index - 1];
                let delta = last.0 - prev.0;
                if delta > 0.5 {
                    delta / (last.1 - prev.1).max(1.0)
                } else {
                    1.0 / estimated
                }
            } else {
                1.0 / estimated
            };
            return (last.0 + (y - last.1) * slope).clamp(0.0, total_lines);
        }
        let a = anchors[index];
        let b = anchors[index + 1];
        if y >= a.1 && y <= b.1 {
            let delta = b.1 - a.1;
            let t = if delta > 0.5 {
                (y - a.1) / delta
            } else {
                0.0
            };
            return (a.0 + (b.0 - a.0) * t).clamp(0.0, total_lines);
        }
    }
    total_lines
}

fn segmented_capsule<T: Copy + PartialEq>(
    ui: &mut Ui,
    style: &UiStyle,
    id_salt: &str,
    right_x: f32,
    center_y: f32,
    items: &[(T, &str)],
    current: &T,
) -> Option<T> {
    let theme = style.theme;
    let seg_w = 38.0;
    let pad = 2.0;
    let total_w = items.len() as f32 * seg_w + pad * 2.0;
    let cap_rect = Rect::from_min_size(
        Pos2::new(right_x - total_w, center_y - 10.0),
        Vec2::new(total_w, 20.0),
    );
    ui.painter()
        .rect_filled(cap_rect, CornerRadius::same(6), theme.input_bg);
    ui.painter().rect_stroke(
        cap_rect,
        CornerRadius::same(6),
        Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );

    let mut chosen = None;
    for (idx, (val, label)) in items.iter().enumerate() {
        let seg_r = Rect::from_min_size(
            Pos2::new(cap_rect.left() + pad + idx as f32 * seg_w, cap_rect.top() + pad),
            Vec2::new(seg_w, cap_rect.height() - pad * 2.0),
        );
        let active = *current == *val;
        let resp = ui.interact(seg_r, ui.scope_id().with((id_salt, *label)), Sense::click());
        if active {
            ui.painter()
                .rect_filled(seg_r, CornerRadius::same(4), theme.accent);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(seg_r, CornerRadius::same(4), theme.hover);
        }
        ui.painter().text(
            seg_r.center(),
            Align2::CENTER_CENTER,
            *label,
            style.tiny(),
            if active {
                theme.accent_text
            } else if resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
        );
        if resp.clicked() {
            chosen = Some(*val);
        }
    }
    chosen
}

fn elide_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3) / 2;
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::{preview_y_for_source_line, source_line_for_preview_y};
    use crate::markdown::MarkdownPreviewState;

    fn state() -> MarkdownPreviewState {
        let mut state = MarkdownPreviewState::default();
        state.line_anchors = vec![(0, 0.0), (10, 200.0), (20, 500.0)];
        state.content_height = 800.0;
        state
    }

    #[test]
    fn markdown_split_scroll_mapping_round_trips() {
        let state = state();
        assert!((preview_y_for_source_line(&state, 10.0, 100.0, 14.0) - 200.0).abs() < 1.0);
        let y = preview_y_for_source_line(&state, 15.0, 100.0, 14.0);
        assert!((y - 350.0).abs() < 1.0);
        assert!((source_line_for_preview_y(&state, 350.0, 100.0, 14.0) - 15.0).abs() < 0.5);
        assert!(preview_y_for_source_line(&state, 40.0, 100.0, 14.0) > 500.0);
        assert!(source_line_for_preview_y(&state, 700.0, 100.0, 14.0) > 20.0);
    }

    #[test]
    fn markdown_split_scroll_mapping_without_anchors_uses_ratios() {
        let state = MarkdownPreviewState {
            content_height: 900.0,
            ..Default::default()
        };
        let y = preview_y_for_source_line(&state, 50.0, 100.0, 14.0);
        assert!((y - 450.0).abs() < 1.0);
        let line = source_line_for_preview_y(&state, 450.0, 100.0, 14.0);
        assert!((line - 50.0).abs() < 0.5);
    }
}
