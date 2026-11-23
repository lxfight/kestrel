//! Window chrome: top bar, activity bar, tabs, breadcrumbs, split editor, status bar, welcome.

pub mod dialogs;
pub mod palette;
pub mod sidebar;
pub mod terminal;
pub mod widgets;

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
use crate::theme::{Theme, ThemeKind, R_MD, R_SM};

#[derive(Clone)]
pub struct UiStyle {
    pub theme: Theme,
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,
    pub mono_bold_family: FontFamily,
    pub ui_size: f32,
}

#[allow(dead_code)]
impl UiStyle {
    pub fn font(&self, size: f32) -> FontId {
        FontId::new(size, self.ui_family.clone())
    }
    pub fn bold(&self, size: f32) -> FontId {
        FontId::new(size, self.ui_bold_family.clone())
    }
    pub fn ui(&self) -> FontId {
        self.font(self.ui_size)
    }
    pub fn ui_bold(&self) -> FontId {
        self.bold(self.ui_size)
    }
    pub fn small(&self) -> FontId {
        self.font(crate::theme::T_SMALL)
    }
    pub fn small_bold(&self) -> FontId {
        self.bold(crate::theme::T_SMALL)
    }
    pub fn tiny(&self) -> FontId {
        self.font(crate::theme::T_TINY)
    }
    /// Emphasis size, used by dialog and palette headline rows.
    pub fn emph(&self) -> FontId {
        self.font(crate::theme::T_EMPH)
    }
    pub fn emph_bold(&self) -> FontId {
        self.bold(crate::theme::T_EMPH)
    }
    pub fn title_bold(&self) -> FontId {
        self.bold(crate::theme::T_TITLE)
    }
    pub fn mono(&self, size: f32) -> FontId {
        FontId::new(size, self.mono_family.clone())
    }
    pub fn mono_bold(&self, size: f32) -> FontId {
        FontId::new(size, self.mono_bold_family.clone())
    }
}

impl KestrelApp {
    pub fn root_ui(&mut self, ui: &mut Ui) {
        // Runs before the palette draws: the first Esc clears its query, the
        // next one closes it (see `palette_ui`).
        self.handle_palette_escape(ui.ctx());
        let style = self.ui_style();
        let theme = style.theme;
        let win_w = ui.max_rect().width();

        Panel::top("kestrel_topbar")
            .exact_size(crate::theme::H_TOPBAR)
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
            // Never let the sidebar squeeze the editor below a usable width.
            let max_w = (win_w - 48.0 - 320.0).clamp(180.0, 640.0);
            let width = self.sidebar.width.clamp(180.0, max_w);
            self.sidebar.width = width;
            let sb_resp = Panel::left("kestrel_sidebar")
                .default_size(width)
                .size_range(180.0..=max_w)
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
            .exact_size(crate::theme::H_STATUS)
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

        // Branded Logo (geometric falcon-wing emblem)
        let logo_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 13.0, rect.center().y - 11.0),
            Vec2::splat(22.0),
        );
        icons::paint_kestrel_logo(ui.painter(), logo_rect, &theme);

        // The right cluster (window controls + workbench actions) has a fixed
        // width; the menu and search pill share whatever is left of it.
        let win_btn_w = 38.0;
        let action_zone_w = 3.0 * 30.0 + crate::theme::SP2 + 6.0;
        let menu_rect = Rect::from_min_max(
            Pos2::new(logo_rect.right() + crate::theme::SP2, rect.top()),
            Pos2::new(
                (rect.right() - win_btn_w * 3.0 - action_zone_w - crate::theme::SP2)
                    .max(logo_rect.right() + 80.0),
                rect.bottom(),
            ),
        );
        let mut menu_end_x = logo_rect.right() + 10.0;
        ui.scope_builder(UiBuilder::new().max_rect(menu_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mb = egui::MenuBar::new().ui(ui, |ui| {
                    menu_end_x = self.topbar_menus(ui, style);
                });
                menu_end_x = menu_end_x.max(mb.response.rect.left()) + crate::theme::SP1;
            });
        });

        // Command palette entry: the full pill needs the gap between the menu and
        // the right cluster, so narrow windows fall back to an icon-only button.
        let pill_left = menu_end_x + crate::theme::SP2;
        let pill_right = rect.right()
            - win_btn_w * 3.0
            - action_zone_w
            - crate::theme::SP2;
        let gap = (pill_right - pill_left).max(0.0);
        let pill_compact = gap < 260.0;
        let pill_rect = if pill_compact {
            Rect::from_min_size(
                Pos2::new(pill_right - 26.0, rect.center().y - 13.0),
                Vec2::splat(26.0),
            )
        } else {
            let width = (rect.width() * 0.32).clamp(200.0, 440.0).min(gap);
            // Prefer the window centre; slide only as far as the gap requires.
            let left = (rect.center().x - width * 0.5)
                .clamp(pill_left, (pill_right - width).max(pill_left));
            Rect::from_min_size(
                Pos2::new(left, rect.center().y - 13.0),
                Vec2::new(width, 26.0),
            )
        };
        if pill_compact {
            if widgets::icon_button(ui, pill_rect, &theme, Icon::Search, true)
                .on_hover_text("快速打开 (Ctrl+P)，Esc 清空")
                .clicked()
            {
                self.focus_or_open_quick_open();
            }
        } else {
            let pill_resp = ui.interact(
                pill_rect,
                ui.scope_id().with("command_pill"),
                Sense::click(),
            );
            let radius = CornerRadius::same(R_MD);
            let fade = widgets::hover_fade(ui, "command_pill_hover", pill_resp.hovered());
            let fill = if pill_resp.is_pointer_button_down_on() {
                theme.active
            } else {
                widgets::blend(theme.input_bg, theme.hover, fade)
            };
            ui.painter().rect_filled(pill_rect, radius, fill);
            ui.painter().rect_stroke(
                pill_rect,
                radius,
                Stroke::new(1.0, widgets::blend(theme.border, theme.border_strong, fade)),
                egui::StrokeKind::Inside,
            );
            if pill_resp.has_focus() {
                ui.painter().rect_stroke(
                    pill_rect,
                    radius,
                    Stroke::new(1.0, theme.accent),
                    egui::StrokeKind::Inside,
                );
            }
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
            let (hint, _) = widgets::ellipsize(
                ui,
                "搜索文件、符号或命令",
                style.small(),
                theme.text_dim,
                (pill_rect.width() - 96.0).max(0.0),
            );
            ui.painter().galley(
                Pos2::new(pill_rect.left() + 29.0, pill_rect.center().y - hint.rect.height() * 0.5),
                hint,
                theme.text_dim,
            );
            ui.painter().text(
                Pos2::new(pill_rect.right() - 10.0, pill_rect.center().y),
                Align2::RIGHT_CENTER,
                "Ctrl+P",
                style.tiny(),
                theme.text_faint,
            );
            if pill_resp
                .on_hover_text("搜索文件、符号或命令 (Ctrl+P)，Esc 清空")
                .clicked()
            {
                self.focus_or_open_quick_open();
            }
        }

        // Window controls (Minimize, Maximize/Restore, Close) on far right
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
        let close_fade = widgets::hover_fade(ui, "win_close_hover", close_resp.hovered());
        if close_fade > 0.001 {
            let fill = if close_resp.is_pointer_button_down_on() {
                theme.danger_fill
            } else {
                widgets::blend(theme.panel_bg, theme.danger_fill, close_fade)
            };
            ui.painter().rect_filled(close_rect, 0.0, fill);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(close_rect.center(), Vec2::splat(13.0)),
            Icon::Close,
            if close_resp.is_pointer_button_down_on() || close_fade > 0.5 {
                theme.danger_text
            } else {
                theme.text_dim
            },
            1.4,
        );
        if close_resp.on_hover_text("关闭").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let max_resp = ui.interact(max_rect, ui.scope_id().with("win_max"), Sense::click());
        let max_fade = widgets::hover_fade(ui, "win_max_hover", max_resp.hovered());
        let max_bg = if max_resp.is_pointer_button_down_on() {
            theme.active
        } else if max_fade > 0.001 {
            widgets::blend(theme.panel_bg, theme.hover, max_fade)
        } else {
            theme.panel_bg
        };
        if max_bg != theme.panel_bg {
            ui.painter().rect_filled(max_rect, 0.0, max_bg);
        }
        let max_fg = if max_resp.hovered() || max_resp.is_pointer_button_down_on() {
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
            ui.painter().rect_filled(front, 1.0, max_bg);
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
        let min_fade = widgets::hover_fade(ui, "win_min_hover", min_resp.hovered());
        if min_resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(min_rect, 0.0, theme.active);
        } else if min_fade > 0.001 {
            ui.painter().rect_filled(
                min_rect,
                0.0,
                widgets::blend(theme.panel_bg, theme.hover, min_fade),
            );
        }
        let min_fg = if min_resp.hovered() || min_resp.is_pointer_button_down_on() {
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
            let fade = widgets::hover_fade(ui, ("top_btn_hover", tip), resp.hovered());
            if fade > 0.001 || active {
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(6),
                    if active {
                        theme.active.gamma_multiply(0.75)
                    } else {
                        widgets::blend(theme.panel_bg, theme.hover, fade)
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

    /// Draws the menu buttons and returns the right edge actually used, so the
    /// search pill can be placed from measurement instead of a guessed width.
    fn topbar_menus(&mut self, ui: &mut Ui, style: &UiStyle) -> f32 {
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
                    Command::ToggleIndent,
                    Command::ToggleEol,
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
        })
        .response
        .rect
        .right()
    }

    fn menu_items(&mut self, ui: &mut Ui, style: &UiStyle, commands: &[Command]) {
        for command in commands {
            self.menu_item(ui, style, *command);
        }
    }

    fn menu_item(&mut self, ui: &mut Ui, style: &UiStyle, command: Command) {
        let theme = style.theme;
        let enabled = self.command_enabled(command);
        let shortcut = command.shortcut_text().unwrap_or_default();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(244.0, 25.0), Sense::click());
        let fade = widgets::hover_fade(ui, ("menu_hover", command.id()), response.hovered());
        if enabled && fade > 0.001 {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(R_SM),
                widgets::blend(theme.overlay_bg, theme.hover, fade),
            );
        }
        let text_color = if enabled { theme.text } else { theme.text_faint };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(Pos2::new(rect.left() + 13.0, rect.center().y), Vec2::splat(14.0)),
            command.icon(),
            if !enabled {
                theme.text_faint
            } else if response.hovered() {
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
            text_color,
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
        if enabled && response.clicked() {
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
            let fade = widgets::hover_fade(ui, ("activity_hover", tip), resp.hovered());
            if selected {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 6.0, r.top() + 5.0),
                        Pos2::new(r.right() - 6.0, r.bottom() - 5.0),
                    ),
                    CornerRadius::same(R_MD),
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
            } else if fade > 0.001 {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 6.0, r.top() + 5.0),
                        Pos2::new(r.right() - 6.0, r.bottom() - 5.0),
                    ),
                    CornerRadius::same(R_MD),
                    widgets::blend(theme.activity_bg, theme.hover, fade * 0.7),
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
                    theme.text_muted
                },
                1.45,
            );
            if badge > 0 {
                let label = if badge > 99 {
                    "99+".to_string()
                } else {
                    badge.to_string()
                };
                let pill_w = if label.len() > 2 {
                    22.0
                } else if label.len() == 2 {
                    18.0
                } else {
                    15.0
                };
                let badge_rect = Rect::from_center_size(
                    Pos2::new(r.center().x + 10.0, r.center().y + 8.0),
                    Vec2::new(pill_w, 15.0),
                );
                ui.painter().rect_filled(badge_rect, CornerRadius::same(8), theme.accent_fill);
                ui.painter().rect_stroke(
                    badge_rect.expand(1.0),
                    CornerRadius::same(9),
                    Stroke::new(1.5, theme.activity_bg),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    badge_rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    style.bold(9.0),
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
            if self.sidebar.visible && self.sidebar.mode == SidebarMode::Search {
                self.sidebar.visible = false;
            } else {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Search;
                self.workspace.search_panel.focus = true;
            }
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
            if self.sidebar.visible && self.sidebar.mode == SidebarMode::Git {
                self.sidebar.visible = false;
            } else {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Git;
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
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
            if self.sidebar.visible && self.sidebar.mode == SidebarMode::Outline {
                self.sidebar.visible = false;
            } else {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Outline;
            }
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
                // Sleek 1px hairline seam with a generous 6px invisible drag hitzone.
                let sep_hit = sep.expand2(Vec2::new(2.5, 0.0));
                let resp = ui.interact(
                    sep_hit,
                    ui.scope_id().with(("group_sep", group)),
                    Sense::click_and_drag(),
                );
                let active = resp.hovered() || resp.dragged();
                if active {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let fade = widgets::hover_fade(ui, ("group_sep_hover", group), active);
                ui.painter().rect_filled(sep, 0.0, theme.border);
                if fade > 0.01 {
                    let accent_bar = Rect::from_center_size(
                        sep.center(),
                        Vec2::new(2.0, sep.height()),
                    );
                    ui.painter().rect_filled(
                        accent_bar,
                        0.0,
                        widgets::blend(theme.border, theme.accent, fade),
                    );
                }
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
                    let total = self.groups[group - 1].weight.max(0.05)
                        + self.groups[group].weight.max(0.05);
                    self.groups[group - 1].weight = total * 0.5;
                    self.groups[group].weight = total * 0.5;
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
                Vec2::new(rect.width(), crate::theme::H_TAB),
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
            let shared_groups = self
                .groups
                .iter()
                .filter(|g| g.tabs.contains(&id))
                .count()
                > 1;
            let saved_scroll = if shared_groups {
                let prev = (self.tabs[tab_idx].scroll_y, self.tabs[tab_idx].scroll_x);
                if let Some(&(sy, sx)) = self.groups[group_idx].pane_scroll.get(&id) {
                    self.tabs[tab_idx].scroll_y = sy;
                    self.tabs[tab_idx].scroll_x = sx;
                }
                Some(prev)
            } else {
                None
            };
            let prev_focus = self.tabs[tab_idx].has_focus;
            let large_pane =
                self.tabs[tab_idx].doc.len_bytes() as u64 > crate::app::MAX_OPEN_FILE_BYTES;
            let pane_style;
            let effective_style = if large_pane && editor_style.show_minimap {
                pane_style = crate::editor::EditorStyle {
                    show_minimap: false,
                    ..editor_style.clone()
                };
                &pane_style
            } else {
                editor_style
            };
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(pane_rect)
                    .id_salt(("editor_pane", group_idx)),
                |ui| {
                    event = self.render_pane(ui, style, effective_style, tab_idx, &tag);
                },
            );
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if let Some((prev_y, prev_x)) = saved_scroll {
                    let next = (tab.scroll_y, tab.scroll_x);
                    if let Some(group) = self.groups.get_mut(group_idx) {
                        group.pane_scroll.insert(id, next);
                    }
                    // Keep the active group's scroll position as the buffer's primary view.
                    if group_idx != self.active_group {
                        tab.scroll_y = prev_y;
                        tab.scroll_x = prev_x;
                    }
                }
                if shared_groups && group_idx != self.active_group && prev_focus {
                    tab.has_focus = true;
                }
            }
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
            Sense::click(),
        );

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
        let tabs_zone = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.right() - actions_reserve, rect.bottom()),
        );
        let ids: Vec<u64> = self.groups[group_idx].tabs.clone();
        // One measuring pass per frame; the scroll bounds and the paint pass
        // below reuse these widths instead of laying every title out twice.
        let mut widths: Vec<f32> = Vec::with_capacity(ids.len());
        let mut total_w = 0.0;
        for id in &ids {
            let Some(index) = self.tab_index(*id) else {
                widths.push(0.0);
                continue;
            };
            let width = tab_width(ui, style, &self.tabs[index]);
            widths.push(width);
            total_w += width;
        }
        let pointer_pos = ui
            .ctx()
            .input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let overflow = total_w > tabs_zone.width() - 0.5;
        let scroll_btn_w = if overflow { 18.0 } else { 0.0 };
        let tabs_clip = Rect::from_min_max(
            Pos2::new(tabs_zone.left() + scroll_btn_w, tabs_zone.top()),
            Pos2::new(tabs_zone.right() - scroll_btn_w, tabs_zone.bottom()),
        );
        let max_scroll = (total_w - tabs_clip.width()).max(0.0);
        let mut scroll = if max_scroll > 0.0 {
            self.groups[group_idx].tab_scroll.max(0.0)
        } else {
            0.0
        };
        // Wheel over the strip scrolls the tabs, even when the pointer sits on a
        // tab (that tab is the top-most hover target, but the wheel belongs here).
        if max_scroll > 0.0
            && pointer_pos.is_some_and(|pos| tabs_zone.contains(pos))
        {
            let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
            if dx != 0.0 || dy != 0.0 {
                scroll -= dx + dy;
            }
        }
        if overflow {
            let step = (tabs_clip.width() * 0.7).max(120.0);
            let button =
                |ui: &mut Ui, x: f32, salt: &'static str, icon: Icon, enabled: bool| -> f32 {
                    let r = Rect::from_min_size(
                        Pos2::new(x, rect.top() + 2.0),
                        Vec2::new(scroll_btn_w, rect.height() - 4.0),
                    );
                    let resp = ui.interact(
                        r,
                        ui.scope_id().with(("gtab_scroll", group_idx, salt)),
                        if enabled { Sense::click() } else { Sense::hover() },
                    );
                    let fade = widgets::hover_fade(
                        ui,
                        ("gtab_scroll_hover", group_idx, salt),
                        resp.hovered() && enabled,
                    );
                    if fade > 0.001 {
                        ui.painter().rect_filled(
                            r.shrink2(Vec2::new(0.0, 3.0)),
                            CornerRadius::same(4),
                            widgets::blend(theme.panel_bg, theme.hover, fade),
                        );
                    }
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(r.center(), Vec2::splat(12.0)),
                        icon,
                        if !enabled {
                            theme.text_faint
                        } else if resp.hovered() {
                            theme.text
                        } else {
                            theme.text_dim
                        },
                        1.3,
                    );
                    if enabled && resp.clicked() {
                        if salt == "left" { -step } else { step }
                    } else {
                        0.0
                    }
                };
            let delta = button(ui, tabs_zone.left(), "left", Icon::ChevronLeft, scroll > 0.5)
                + button(
                    ui,
                    tabs_clip.right(),
                    "right",
                    Icon::ChevronRight,
                    scroll < max_scroll - 0.5,
                );
            scroll += delta;
        }
        if let Some(active) = active_id
            && let Some(pos) = ids.iter().position(|id| *id == active)
        {
            // Only react to an actual active-tab change: manual scrolling away
            // from the active tab must not snap back on every frame.
            let reveal_key = ui.scope_id().with(("gtab_reveal", group_idx));
            let previous: Option<u64> = ui.ctx().data(|data| data.get_temp(reveal_key));
            ui.ctx().data_mut(|data| data.insert_temp(reveal_key, active));
            if previous != Some(active) {
                let left: f32 = widths[..pos].iter().sum();
                let right = left + widths[pos];
                if right - scroll > tabs_clip.width() - 2.0 {
                    scroll = right - tabs_clip.width() + 2.0;
                } else if left - scroll < 2.0 {
                    scroll = left - 2.0;
                }
            }
        }
        scroll = scroll.clamp(0.0, max_scroll);
        self.groups[group_idx].tab_scroll = scroll;
        let dragging_tab = match &self.drag_payload {
            Some(DragPayload::Tab { id, group }) => Some((*id, *group)),
            _ => None,
        };
        let mut x = tabs_clip.left() - scroll;
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
            let dirty = tab.is_dirty();
            let pinned = tab.pinned;
            let display_title = tab_display_title(tab);
            let width = widths[pos_in_group];
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
            let hover_t = if active {
                0.0
            } else {
                widgets::hover_fade(ui, ("gtab_hover", group_idx, *id), tab_resp.hovered())
            };
            let bg = if tab_resp.is_pointer_button_down_on() {
                theme.active
            } else if active {
                theme.bg
            } else {
                widgets::blend(theme.panel_bg, theme.hover, hover_t)
            };
            // Inactive tabs stop 1px above the bottom so the tab bar's 1px bottom
            // hairline stays intact; the active tab connects seamlessly into the canvas.
            let fill_rect = if active {
                clip_rect
            } else {
                Rect::from_min_max(
                    clip_rect.min,
                    Pos2::new(clip_rect.max.x, (clip_rect.max.y - 1.0).max(clip_rect.min.y)),
                )
            };
            ui.painter().rect_filled(fill_rect, 0.0, bg);
            if tab_resp.has_focus() && !active {
                ui.painter().rect_stroke(
                    fill_rect.shrink(1.0),
                    0.0,
                    Stroke::new(1.0, theme.accent),
                    egui::StrokeKind::Inside,
                );
            }
            ui.painter().with_clip_rect(tabs_clip).line_segment(
                [
                    Pos2::new(tab_rect.right(), tab_rect.top() + 7.0),
                    Pos2::new(tab_rect.right(), tab_rect.bottom() - 7.0),
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
                    theme.kind.is_dark(),
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
            let title_font = if focused_tab {
                style.small_bold()
            } else {
                style.small()
            };
            let title_rect = Rect::from_min_max(
                Pos2::new(tab_rect.left() + 31.0, tab_rect.top()),
                Pos2::new(tab_rect.right() - 26.0, tab_rect.bottom()),
            );
            let (title_galley, title_cut) = widgets::ellipsize(
                ui,
                &display_title,
                title_font,
                title_color,
                title_rect.width(),
            );
            ui.painter().with_clip_rect(clip_rect).galley(
                Pos2::new(
                    title_rect.left(),
                    title_rect.center().y - title_galley.rect.height() * 0.5,
                ),
                title_galley,
                title_color,
            );
            if title_cut {
                tab_resp.clone().on_hover_text(&display_title);
            }

            let right_icon_center = Pos2::new(tab_rect.right() - 16.0, tab_rect.center().y);
            let close_rect = Rect::from_center_size(right_icon_center, Vec2::splat(16.0));
            let close_hovered =
                tab_resp.hovered() && pointer_pos.is_some_and(|pos| close_rect.contains(pos));
            let close_fade = widgets::hover_fade(ui, ("gtab_close", group_idx, *id), close_hovered);
            let tab_surface = if active { theme.bg } else { theme.panel_bg };
            if close_fade > 0.001 {
                ui.painter().with_clip_rect(clip_rect).rect_filled(
                    close_rect,
                    CornerRadius::same(R_SM),
                    widgets::blend(tab_surface, theme.active, close_fade),
                );
            }
            if close_hovered {
                icons::paint(
                    &ui.painter().with_clip_rect(clip_rect),
                    close_rect.shrink(3.5),
                    Icon::Close,
                    theme.text,
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
            } else if tab_resp.hovered() {
                icons::paint(
                    &ui.painter().with_clip_rect(clip_rect),
                    close_rect.shrink(3.5),
                    Icon::Close,
                    theme.text_dim,
                    1.3,
                );
            }

            let close_hit = close_hovered && ui.input(|i| i.pointer.primary_clicked());
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
                if Self::context_item(ui, style, Icon::Close, "关闭右侧标签", None) {
                    context_action = Some(TabAction::CloseRight(group_idx, *id));
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

        // Double-clicking the empty end of the tab strip creates a new file,
        // the convention users expect from the tab row.
        if bar_resp.double_clicked()
            && let Some(pos) = pointer_pos
            && pos.x >= rect.left() - scroll + total_w
            && pos.x < tabs_clip.right()
        {
            self.new_untitled();
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
            let is_on = icon == Icon::WordWrap && self.settings.word_wrap;
            let fade = widgets::hover_fade(ui, ("group_act_hover", group_idx, tip), resp.hovered());
            if is_on {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(R_SM), theme.accent_subtle);
            } else if fade > 0.001 {
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(R_SM),
                    widgets::blend(theme.panel_bg, theme.hover, fade),
                );
            }
            icons::paint(
                ui.painter(),
                r.shrink(4.0),
                icon,
                if is_on {
                    theme.accent
                } else if resp.hovered() {
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
                TabAction::CloseRight(group, keep) => {
                    let ids: Vec<u64> = self
                        .groups
                        .get(group)
                        .map(|tabs| {
                            let start = tabs.index_of(keep).map(|pos| pos + 1).unwrap_or(0);
                            tabs.tabs.iter().copied().skip(start).collect()
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
            Vec2::new(168.0, 32.0),
        );
        let resp = widgets::paint_button(
            ui,
            button,
            ("empty_group_open", group_idx),
            "打开文件 (Ctrl+P)",
            style.small(),
            &theme,
            widgets::ButtonVariant::Secondary,
            true,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, button.bottom() + 18.0),
            Align2::CENTER_CENTER,
            "拖入标签页或文件以在此分屏打开",
            style.tiny(),
            theme.text_muted,
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
        // Each crumb carries its own absolute path: clicking one reveals that
        // directory in the file tree, hovering shows the full path.
        let full_path: Option<PathBuf> = editor.doc.path.clone();
        let mut segments: Vec<(String, Option<PathBuf>)> = Vec::new();
        if let Some(path) = &full_path {
            if let Some(root) = &self.workspace.root {
                if let Ok(rel) = path.strip_prefix(root) {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect();
                    if parts.is_empty() {
                        if let Some(name) = root.file_name() {
                            segments.push((name.to_string_lossy().to_string(), Some(path.clone())));
                        }
                    } else {
                        let mut acc = root.clone();
                        for part in parts {
                            acc.push(&part);
                            segments.push((part, Some(acc.clone())));
                        }
                    }
                } else {
                    segments.push((path.to_string_lossy().to_string(), Some(path.clone())));
                }
            } else {
                segments.push((path.to_string_lossy().to_string(), Some(path.clone())));
            }
        } else {
            segments.push((editor.title(), None));
        }
        if segments.is_empty() {
            segments.push((editor.title(), None));
        }

        let caret_line = editor.doc.char_to_line(editor.caret);
        let active_symbol = editor
            .symbols
            .iter().rfind(|s| caret_line >= s.start_line && caret_line <= s.end_line)
            .cloned();

        // The language label and the view-mode capsule keep their spots on the
        // right; the crumbs may only use what is left of the row.
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
        let measure = |ui: &Ui, text: &str| -> f32 {
            ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        text.to_string(),
                        style.small(),
                        theme.text,
                    ))
                    .rect
                    .width()
            })
        };
        let language_w = measure(ui, language);
        let has_capsule = (is_tabular || is_md) && rect.width() > 360.0;
        let right_reserve = if has_capsule {
            language_w.max(120.0) + 92.0
        } else {
            language_w + 24.0
        };
        let available = (rect.right() - right_reserve - (rect.left() + 14.0)).max(0.0);
        let widths: Vec<f32> = segments
            .iter()
            .map(|(label, _)| measure(ui, label))
            .collect();
        let last_icon_w = 16.0;
        let ellipsis_label = "…";
        let ellipsis_w = measure(ui, ellipsis_label) + 19.0;
        // Drop leading crumbs until the row fits; the marker that replaces them
        // explains the hidden prefix on hover.
        let mut start = 0usize;
        loop {
            let mut total = widths[start..].iter().sum::<f32>()
                + 4.0 * (segments.len() - start) as f32
                + 15.0 * (segments.len() - start - 1) as f32
                + last_icon_w;
            if start > 0 {
                total += ellipsis_w;
            }
            if total <= available || start + 1 >= segments.len() {
                break;
            }
            start += 1;
        }
        // Crumbs never paint over the language label or the view-mode capsule.
        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(
                (rect.right() - right_reserve + 4.0).max(rect.left() + 40.0),
                rect.bottom(),
            ),
        ));
        let mut reveal: Option<PathBuf> = None;
        let mut x = rect.left() + 14.0;
        if start > 0
            && let Some(path) = &full_path
        {
            let chip = Rect::from_min_size(
                Pos2::new(x, rect.center().y - 9.0),
                Vec2::new(ellipsis_w, 18.0),
            );
            let resp = ui.interact(
                chip,
                ui.scope_id().with(("bc_ellipsis", tab_idx)),
                Sense::click(),
            );
            let fade = widgets::hover_fade(ui, ("bc_ellipsis_hover", tab_idx), resp.hovered());
            if fade > 0.001 {
                painter.rect_filled(
                    chip,
                    CornerRadius::same(4),
                    widgets::blend(theme.bg, theme.hover, fade),
                );
            }
            painter.text(
                Pos2::new(chip.left() + 3.0, rect.center().y),
                Align2::LEFT_CENTER,
                ellipsis_label,
                style.small(),
                theme.text_dim,
            );
            if resp.clicked() {
                reveal = Some(path.clone());
            }
            resp.on_hover_text(path.to_string_lossy().to_string());
            x += ellipsis_w;
        }
        for (index, (label, path)) in segments.iter().enumerate().skip(start) {
            if index > start {
                let chevron =
                    Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(11.0));
                icons::paint(&painter, chevron, Icon::ChevronRight, theme.text_faint, 1.2);
                x += 15.0;
            }
            let is_last = index + 1 == segments.len();
            if is_last {
                let icon_rect = Rect::from_center_size(
                    Pos2::new(x + 6.0, rect.center().y),
                    Vec2::splat(13.0),
                );
                if let Some(p) = &full_path {
                    icons::paint_path_icon(
                        &painter,
                        icon_rect,
                        p,
                        false,
                        false,
                        theme.text_dim,
                        theme.kind.is_dark(),
                    );
                } else {
                    icons::file_glyph(&painter, icon_rect, theme.text_dim, false, false);
                }
                x += last_icon_w;
            }
            let width = widths[index];
            let hit = Rect::from_min_size(
                Pos2::new(x - 3.0, rect.center().y - 9.0),
                Vec2::new(width + 6.0, 18.0),
            );
            let resp = ui.interact(
                hit,
                ui.scope_id().with(("bc_seg", tab_idx, index)),
                Sense::click(),
            );
            let fade = widgets::hover_fade(ui, ("bc_seg_hover", tab_idx, index), resp.hovered());
            if fade > 0.001 {
                painter.rect_filled(
                    hit,
                    CornerRadius::same(4),
                    widgets::blend(theme.bg, theme.hover, fade),
                );
            }
            painter.text(
                Pos2::new(x, rect.center().y),
                Align2::LEFT_CENTER,
                label.as_str(),
                style.small(),
                if is_last { theme.text } else { theme.text_dim },
            );
            if let Some(path) = path {
                let resp = resp.on_hover_text(path.to_string_lossy().to_string());
                if resp.clicked() {
                    reveal = Some(path.clone());
                }
            }
            x += width + 4.0;
        }

        // Measure the right-side language label + optional capsule so neither the
        // symbol segment nor the capsule ever collides on narrow panes.
        let lang_galley = ui.ctx().fonts_mut(|fonts| {
            fonts.layout_job(LayoutJob::simple_singleline(
                language.to_string(),
                style.tiny(),
                theme.text_muted,
            ))
        });
        let lang_w = lang_galley.rect.width();
        let capsule_right = rect.right() - 14.0 - lang_w - 10.0;
        let capsule_w = if (is_md || is_tabular) && rect.width() > 360.0 {
            if is_md { 116.0 } else { 82.0 }
        } else {
            0.0
        };
        let symbol_max_x = (capsule_right - capsule_w - 8.0).max(x);

        // Enclosing Symbol Segment (clickable to open Goto Symbol palette), ellipsized against right chrome.
        let mut open_symbols = false;
        if let Some(sym) = active_symbol
            && symbol_max_x - x > 44.0
        {
            let chevron =
                Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(11.0));
            icons::paint(
                ui.painter(),
                chevron,
                Icon::ChevronRight,
                theme.text_muted,
                1.2,
            );
            x += 15.0;
            let text_max = (symbol_max_x - x - 24.0).max(16.0);
            let (sym_galley, sym_cut) = widgets::ellipsize_mode(
                ui,
                &sym.name,
                style.small(),
                theme.text_dim,
                text_max,
                widgets::EllipsizeMode::End,
            );
            let sym_w = sym_galley.rect.width() + 24.0;
            let sym_rect = Rect::from_min_size(
                Pos2::new(x, rect.top() + 2.0),
                Vec2::new(sym_w, height - 4.0),
            );
            let s_resp = ui.interact(
                sym_rect,
                ui.scope_id().with(("bc_symbol", group_idx)),
                Sense::click(),
            );
            let s_fade = widgets::hover_fade(ui, ("bc_symbol_hover", group_idx), s_resp.hovered());
            if s_fade > 0.001 {
                ui.painter().rect_filled(
                    sym_rect,
                    CornerRadius::same(R_SM),
                    widgets::blend(theme.bg, theme.hover, s_fade),
                );
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(Pos2::new(x + 8.0, rect.center().y), Vec2::splat(13.0)),
                sym.kind.icon(),
                icons::symbol_color(sym.kind, &theme),
                1.2,
            );
            ui.painter().galley(
                Pos2::new(x + 18.0, rect.center().y - sym_galley.rect.height() * 0.5),
                sym_galley,
                theme.text_dim,
            );
            let tip = if sym_cut {
                format!("{}\n点击跳转到文档符号 (Ctrl+Shift+.)", sym.name)
            } else {
                "点击跳转到文档符号 (Ctrl+Shift+.)".to_string()
            };
            if s_resp.on_hover_text(tip).clicked() {
                open_symbols = true;
            }
        }

        // Language label on the right of the breadcrumb.
        ui.painter().galley(
            Pos2::new(
                rect.right() - 14.0 - lang_w,
                rect.center().y - lang_galley.rect.height() * 0.5,
            ),
            lang_galley,
            theme.text_muted,
        );
        let mut next_tabular_src: Option<bool> = None;
        if is_tabular && rect.width() > 360.0 {
            let items = [(false, "表格"), (true, "源码")];
            let salt = format!("bc_tab_{group_idx}");
            if let Some(sel) = segmented_capsule(
                ui,
                style,
                &salt,
                capsule_right,
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
                capsule_right,
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
        if let Some(path) = reveal {
            self.sidebar.visible = true;
            self.sidebar.mode = SidebarMode::Explorer;
            self.reveal_in_tree(&path);
            if path.is_dir() {
                self.workspace.tree.expand(&path);
            }
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
                    if ed.md_preview.needs_parse(version) {
                        let text = ed.doc.text();
                        ed.md_preview.ensure_parsed(version, &text);
                    }
                    let path = ed.doc.path.clone();
                    prev_events = crate::markdown::render::preview_ui(
                        ui,
                        style,
                        &mut ed.md_preview,
                        &mut ed.md_mode,
                        version,
                        "",
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
                let usable_w = (avail.width() - 1.0).max(240.0);
                let left_w = (usable_w * ratio).clamp(120.0, usable_w - 120.0);
                let left_rect = Rect::from_min_size(avail.min, Vec2::new(left_w, avail.height()));
                let sep_rect = Rect::from_min_size(
                    Pos2::new(left_rect.right(), avail.top()),
                    Vec2::new(1.0, avail.height()),
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
                    sep_rect.expand2(Vec2::new(2.5, 0.0)),
                    ui.scope_id().with(("md_split_sep", pane_tag, tab_idx)),
                    Sense::click_and_drag(),
                );
                let sep_dragging = sep_resp.dragged();
                if sep_resp.hovered() || sep_dragging {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let sep_fade = widgets::hover_fade(
                    ui,
                    ("md_split_sep_hover", pane_tag, tab_idx),
                    sep_resp.hovered() || sep_dragging,
                );
                ui.painter().rect_filled(sep_rect, 0.0, style.theme.border);
                if sep_fade > 0.01 {
                    let bar = Rect::from_center_size(
                        sep_rect.center(),
                        Vec2::new(2.0, sep_rect.height()),
                    );
                    ui.painter().rect_filled(
                        bar,
                        0.0,
                        widgets::blend(style.theme.border, style.theme.accent, sep_fade),
                    );
                }
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
                        if ed.md_preview.needs_parse(version) {
                            let text = ed.doc.text();
                            ed.md_preview.ensure_parsed(version, &text);
                        }
                        let path = ed.doc.path.clone();
                        prev_events = crate::markdown::render::preview_ui(
                            ui,
                            style,
                            &mut ed.md_preview,
                            &mut ed.md_mode,
                            version,
                            "",
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
        let center_y = rect.center().y;

        // Right cluster first: every entry is measured once, then painted with
        // its hover plate underneath. Its left edge bounds the left cluster so
        // the two can never overlap on narrow windows.
        let mut right: Vec<StatusSegment> = Vec::new();
        if (self.settings.ui_zoom - 1.0).abs() > 0.01 {
            right.push(StatusSegment {
                text: self.status_cache.zoom(self.settings.ui_zoom).to_owned(),
                color: theme.accent,
                font: style.small(),
                action: Some(StatusAction::Zoom),
                tip: "点击重置界面缩放为 100% (Ctrl+0)",
            });
        }
        if let Some(editor) = self.tabs.get(self.active) {
            if let Some(mm) = &editor.multimodal
                && !mm.is_showing_source()
            {
                let size = self
                    .status_cache
                    .file_size(
                        editor.id,
                        editor.doc.version,
                        editor.doc.len_bytes(),
                        editor.doc.path.as_deref(),
                    )
                    .to_owned();
                right.push(StatusSegment {
                    text: mm.kind().label().to_string(),
                    color: theme.text_dim,
                    font: style.small(),
                    action: None,
                    tip: "",
                });
                if !size.is_empty() {
                    right.push(StatusSegment {
                        text: size,
                        color: theme.text_faint,
                        font: style.small(),
                        action: None,
                        tip: "",
                    });
                }
            } else if editor.doc.encoding == crate::doc::Encoding::Binary {
                let size = self
                    .status_cache
                    .file_size(
                        editor.id,
                        editor.doc.version,
                        editor.doc.len_bytes(),
                        editor.doc.path.as_deref(),
                    )
                    .to_owned();
                right.push(StatusSegment {
                    text: "二进制文件 (不可读取)".to_string(),
                    color: theme.warning,
                    font: style.small(),
                    action: None,
                    tip: "",
                });
                if !size.is_empty() {
                    right.push(StatusSegment {
                        text: size,
                        color: theme.text_faint,
                        font: style.small(),
                        action: None,
                        tip: "",
                    });
                }
            } else if editor.diff.is_some() {
                right.push(StatusSegment {
                    text: "Git 差异".to_string(),
                    color: theme.text_dim,
                    font: style.small(),
                    action: None,
                    tip: "",
                });
            } else {
                right.push(StatusSegment {
                    text: editor.language.name().to_string(),
                    color: theme.text_dim,
                    font: style.small(),
                    action: Some(StatusAction::Language),
                    tip: "点击切换语言模式",
                });
                right.push(StatusSegment {
                    text: editor.doc.eol.as_str().to_string(),
                    color: theme.text_faint,
                    font: style.small(),
                    action: Some(StatusAction::Eol),
                    tip: "点击切换行尾 (LF / CRLF)",
                });
                right.push(StatusSegment {
                    text: editor.doc.encoding.as_str().to_string(),
                    color: theme.text_faint,
                    font: style.small(),
                    action: Some(StatusAction::Encoding),
                    tip: "点击选择保存编码",
                });
                let (tab_size, insert_spaces) = editor.effective_indent(
                    self.settings.tab_size,
                    self.settings.insert_spaces,
                    self.settings.detect_indentation,
                );
                let indent = self
                    .status_cache
                    .indent(tab_size, insert_spaces)
                    .to_owned();
                right.push(StatusSegment {
                    text: indent,
                    color: theme.text_faint,
                    font: style.small(),
                    action: Some(StatusAction::Indent),
                    tip: "点击切换缩进：空格 / 制表符",
                });
                let (line, col) = editor.doc.char_to_line_col(editor.caret);
                let cursors = editor.extra_cursors.len() + 1;
                let position = self
                    .status_cache
                    .cursor(editor.doc.version, line, col, cursors)
                    .to_owned();
                right.push(StatusSegment {
                    text: position,
                    color: theme.text_dim,
                    font: style.small(),
                    action: Some(StatusAction::Cursor),
                    tip: "转到行 (Ctrl+G)",
                });
            }
        }
        for (index, status) in self.plugins.status_items().iter().enumerate() {
            right.push(StatusSegment {
                text: status.text.clone(),
                color: theme.text_dim,
                font: style.small(),
                action: status.command.as_ref().map(|_| StatusAction::Plugin(index)),
                tip: "",
            });
        }

        let mut rx = rect.right() - 12.0;
        let mut right_hits: Vec<(StatusAction, egui::Response)> = Vec::new();
        // The encoding popup must be shown every frame while it is open; only
        // the toggle decision comes from the click on the segment.
        let mut encoding_response: Option<egui::Response> = None;
        for (index, segment) in right.into_iter().enumerate() {
            let galley = ui.ctx().fonts_mut(|fonts| {
                fonts.layout_job(LayoutJob::simple_singleline(
                    segment.text,
                    segment.font,
                    segment.color,
                ))
            });
            let width = galley.rect.width();
            let pos = Pos2::new(rx - width, center_y - galley.rect.height() * 0.5);
            if let Some(action) = segment.action {
                let hit = Rect::from_min_max(
                    Pos2::new(rx - width - 6.0, rect.top() + 3.0),
                    Pos2::new(rx + 6.0, rect.bottom() - 3.0),
                );
                let resp = ui.interact(
                    hit,
                    ui.scope_id().with(("status_seg", index)),
                    Sense::click(),
                );
                let fade = widgets::hover_fade(ui, ("status_seg_hover", index), resp.hovered());
                if fade > 0.001 {
                    ui.painter().rect_filled(
                        hit,
                        CornerRadius::same(4),
                        widgets::blend(theme.status_bg, theme.hover, fade),
                    );
                }
                let resp = if segment.tip.is_empty() {
                    resp
                } else {
                    resp.on_hover_text(segment.tip)
                };
                ui.painter().galley(pos, galley, segment.color);
                if matches!(action, StatusAction::Encoding) {
                    encoding_response = Some(resp.clone());
                }
                right_hits.push((action, resp));
            } else {
                ui.painter().galley(pos, galley, segment.color);
            }
            rx -= width + 16.0;
        }

        let left_limit = rx.max(rect.left() + 96.0);
        let mut x = rect.left() + 10.0;
        let mut left_hits: Vec<(StatusAction, egui::Response)> = Vec::new();

        // Git branch: click opens the branch switcher.
        if self.git.snapshot.is_repo && !self.git.snapshot.branch.is_empty() {
            let (label, counts) = self.status_cache.branch(
                &self.git.snapshot.branch,
                !self.git.snapshot.changes.is_empty(),
                self.git.snapshot.ahead,
                self.git.snapshot.behind,
            );
            let label = label.to_owned();
            let counts = counts.to_owned();
            let counts_galley_pre = (!counts.is_empty()).then(|| {
                ui.ctx().fonts_mut(|fonts| {
                    fonts.layout_job(LayoutJob::simple_singleline(
                        counts.clone(),
                        style.tiny(),
                        theme.text_muted,
                    ))
                })
            });
            let counts_reserve = counts_galley_pre
                .as_ref()
                .map(|g| g.rect.width() + 6.0)
                .unwrap_or(0.0);
            let start_x = x;
            let icon_rect =
                Rect::from_center_size(Pos2::new(x + 7.0, center_y), Vec2::splat(13.0));
            x += 16.0;
            let avail = (left_limit - x - counts_reserve - 8.0).max(20.0);
            let (label_galley, label_cut) =
                widgets::ellipsize(ui, &label, style.small(), theme.text, avail);
            let label_width = label_galley.rect.width();
            let label_pos = Pos2::new(x, center_y - label_galley.rect.height() * 0.5);
            x += label_width;
            let mut hit = Rect::from_min_max(
                Pos2::new(start_x - 2.0, rect.top() + 3.0),
                Pos2::new(x + 2.0, rect.bottom() - 3.0),
            );
            let counts_galley = counts_galley_pre.and_then(|galley| {
                if x + 6.0 + galley.rect.width() <= left_limit {
                    x += 6.0;
                    let pos = Pos2::new(x, center_y - galley.rect.height() * 0.5);
                    x += galley.rect.width();
                    hit.max.x = x + 2.0;
                    Some((pos, galley))
                } else {
                    None
                }
            });
            let resp = ui.interact(hit, ui.scope_id().with("status_git_branch"), Sense::click());
            let fade = widgets::hover_fade(ui, "status_git_branch_hover", resp.hovered());
            if fade > 0.001 {
                ui.painter().rect_filled(
                    hit,
                    CornerRadius::same(R_SM),
                    widgets::blend(theme.status_bg, theme.hover, fade),
                );
            }
            icons::paint(ui.painter(), icon_rect, Icon::GitBranch, theme.accent, 1.2);
            ui.painter().galley(label_pos, label_galley, theme.text);
            if let Some((pos, galley)) = counts_galley {
                ui.painter().galley(pos, galley, theme.text_muted);
            }
            let resp = if label_cut {
                resp.on_hover_text(format!("{label}\n点击切换 Git 分支"))
            } else {
                resp.on_hover_text("点击切换 Git 分支")
            };
            left_hits.push((StatusAction::Branch, resp));
            x += 12.0;
        }

        // Workspace folder label.
        let folder = self
            .workspace
            .root
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "没有打开文件夹".to_string());
        let folder_color = if self.workspace.root.is_some() {
            theme.text_dim
        } else {
            theme.text_muted
        };
        let avail = (left_limit - x - 64.0).max(0.0);
        if avail > 24.0 {
            let (galley, cut) =
                widgets::ellipsize(ui, &folder, style.small(), folder_color, avail);
            let width = galley.rect.width();
            ui.painter().galley(
                Pos2::new(x, center_y - galley.rect.height() * 0.5),
                galley,
                folder_color,
            );
            if cut {
                let hit =
                    Rect::from_min_size(Pos2::new(x, center_y - 9.0), Vec2::new(width, 18.0));
                ui.interact(hit, ui.scope_id().with("status_folder"), Sense::hover())
                    .on_hover_text(&folder);
            }
            x += width + 12.0;
        }

        // Diagnostics: click opens the problems panel (only when left cluster has room).
        if x + 52.0 <= left_limit {
            let (err_count, warn_count, err_text, warn_text) =
                self.status_cache.diagnostics(&self.tabs);
            let err_text = err_text.to_owned();
            let warn_text = warn_text.to_owned();
            let start_x = x;
            let error_icon =
                Rect::from_center_size(Pos2::new(x + 6.0, center_y), Vec2::splat(12.0));
            x += 14.0;
            let err_galley = ui.ctx().fonts_mut(|fonts| {
                fonts.layout_job(LayoutJob::simple_singleline(
                    err_text,
                    style.tiny(),
                    theme.text_faint,
                ))
            });
            let err_pos = Pos2::new(x, center_y - err_galley.rect.height() * 0.5);
            x += err_galley.rect.width() + 8.0;
            let warn_icon =
                Rect::from_center_size(Pos2::new(x + 6.0, center_y), Vec2::splat(12.0));
            x += 14.0;
            let warn_galley = ui.ctx().fonts_mut(|fonts| {
                fonts.layout_job(LayoutJob::simple_singleline(
                    warn_text,
                    style.tiny(),
                    theme.text_faint,
                ))
            });
            let warn_pos = Pos2::new(x, center_y - warn_galley.rect.height() * 0.5);
            x += warn_galley.rect.width();
            let hit = Rect::from_min_max(
                Pos2::new(start_x - 2.0, rect.top() + 3.0),
                Pos2::new(x + 2.0, rect.bottom() - 3.0),
            );
            let resp = ui.interact(hit, ui.scope_id().with("status_problems"), Sense::click());
            let fade = widgets::hover_fade(ui, "status_problems_hover", resp.hovered());
            if fade > 0.001 {
                ui.painter().rect_filled(
                    hit,
                    CornerRadius::same(4),
                    widgets::blend(theme.status_bg, theme.hover, fade),
                );
            }
            let (err_color, warn_color) = (
                if err_count > 0 {
                    theme.error
                } else {
                    theme.text_faint
                },
                if warn_count > 0 {
                    theme.warning
                } else {
                    theme.text_faint
                },
            );
            icons::paint(ui.painter(), error_icon, Icon::Error, err_color, 1.2);
            ui.painter().galley(err_pos, err_galley, err_color);
            icons::paint(ui.painter(), warn_icon, Icon::Warning, warn_color, 1.2);
            ui.painter().galley(warn_pos, warn_galley, warn_color);
            let resp = resp.on_hover_text("显示问题面板 (Ctrl+Shift+M)");
            left_hits.push((StatusAction::Problems, resp));
            x += 14.0;
        }

        if let Some(editor) = self.tabs.get(self.active)
            && editor.is_dirty()
        {
            let avail = (left_limit - x).max(0.0);
            let (galley, _) =
                widgets::ellipsize(ui, "● 未保存", style.small(), theme.accent, avail);
            ui.painter().galley(
                Pos2::new(x, center_y - galley.rect.height() * 0.5),
                galley,
                theme.accent,
            );
        }

        let ctx = ui.ctx().clone();
        for (action, response) in right_hits {
            if !response.clicked() {
                continue;
            }
            match action {
                StatusAction::Zoom => self.set_ui_zoom(1.0, &ctx),
                StatusAction::Language => {
                    self.open_palette(PaletteMode::Language);
                    self.palette.focus = true;
                }
                StatusAction::Cursor => self.open_palette(PaletteMode::GotoLine),
                StatusAction::Indent => self.dispatch(Command::ToggleIndent, &ctx),
                StatusAction::Eol => self.dispatch(Command::ToggleEol, &ctx),
                StatusAction::Encoding => {}
                StatusAction::Plugin(index) => {
                    if let Some(command) = self
                        .plugins
                        .status_items()
                        .get(index)
                        .and_then(|status| status.command.clone())
                        && let Some(command_index) = self.plugins.command_index_of_id(&command)
                    {
                        self.run_plugin_command(command_index);
                    }
                }
                StatusAction::Branch | StatusAction::Problems => {}
            }
        }
        if let Some(response) = encoding_response {
            let current = self.tabs.get(self.active).map(|editor| editor.doc.encoding);
            egui::Popup::menu(&response).show(|ui| {
                for encoding in ENCODINGS {
                    if ui
                        .selectable_label(current == Some(*encoding), encoding.as_str())
                        .clicked()
                    {
                        self.set_active_encoding(*encoding);
                        ui.close();
                    }
                }
            });
        }
        for (action, response) in left_hits {
            if !response.clicked() {
                continue;
            }
            match action {
                StatusAction::Branch => self.open_palette(PaletteMode::Branches),
                StatusAction::Problems => {
                    self.bottom.visible = true;
                    self.bottom.tab = BottomTab::Problems;
                }
                _ => {}
            }
        }
    }


    fn welcome_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.bg);
        let center_x = rect.center().x;
        let mut y = rect.top() + (rect.height() * 0.16).max(36.0);
        // The footer sits at bottom - SP5; nothing may be laid out below it.
        let bottom_limit = rect.bottom() - crate::theme::SP5 - crate::theme::SP4;

        let logo = Rect::from_center_size(Pos2::new(center_x, y + 32.0), Vec2::splat(64.0));
        icons::paint_kestrel_logo(ui.painter(), logo, &theme);
        y += 84.0;
        ui.painter().text(
            Pos2::new(center_x, y),
            Align2::CENTER_CENTER,
            "Kestrel",
            style.bold(24.0),
            theme.text,
        );
        y += 28.0;
        ui.painter().text(
            Pos2::new(center_x, y),
            Align2::CENTER_CENTER,
            "轻量 · 极速 · 现代的 Rust 代码编辑器",
            style.ui(),
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
                theme.text_muted,
            );
        };

        // Helper for crisp welcome action row with left-aligned label and right-aligned keycap chip.
        let welcome_cta = |ui: &mut Ui, rect: Rect, id_salt: &str, label: &str, shortcut: &str, primary: bool| -> bool {
            let resp = ui.interact(rect, ui.scope_id().with(("welcome_cta", id_salt)), Sense::click());
            let pressed = resp.is_pointer_button_down_on();
            let fade = widgets::hover_fade(ui, ("welcome_cta_h", id_salt), resp.hovered());
            let radius = CornerRadius::same(R_MD);
            if primary {
                let fill = if pressed {
                    theme.accent_fill_active
                } else {
                    widgets::blend(theme.accent_fill, theme.accent_fill_hover, fade)
                };
                ui.painter().rect_filled(rect, radius, fill);
                ui.painter().text(
                    Pos2::new(rect.left() + 14.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    style.small_bold(),
                    theme.accent_text,
                );
                if !shortcut.is_empty() {
                    let g = ui.ctx().fonts_mut(|f| {
                        f.layout_no_wrap(shortcut.to_string(), style.tiny(), theme.accent_text)
                    });
                    let chip_w = g.rect.width() + 12.0;
                    let chip = Rect::from_min_size(
                        Pos2::new(rect.right() - chip_w - 10.0, rect.center().y - 9.0),
                        Vec2::new(chip_w, 18.0),
                    );
                    ui.painter().rect_filled(
                        chip,
                        CornerRadius::same(R_SM),
                        Color32::from_white_alpha(38),
                    );
                    ui.painter().galley(
                        Pos2::new(
                            chip.center().x - g.rect.width() * 0.5,
                            chip.center().y - g.rect.height() * 0.5,
                        ),
                        g,
                        theme.accent_text,
                    );
                }
            } else {
                let fill = if pressed {
                    theme.active
                } else {
                    widgets::blend(theme.surface_subtle, theme.hover, fade)
                };
                let stroke = widgets::blend(theme.border, theme.border_strong, fade);
                ui.painter().rect_filled(rect, radius, fill);
                ui.painter().rect_stroke(
                    rect,
                    radius,
                    Stroke::new(1.0, stroke),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 14.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    style.small(),
                    theme.text,
                );
                widgets::paint_keycap_right(
                    ui,
                    ui.painter(),
                    rect.right() - 10.0,
                    rect.center().y,
                    shortcut,
                    style.tiny(),
                    &theme,
                );
            }
            resp.clicked()
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
                theme.text_muted,
            );
            y += 34.0;
            // Two side-by-side buttons: shrink to the pane instead of running past it.
            let gap = crate::theme::SP3;
            let btn_w =
                ((rect.width() - crate::theme::SP6 * 2.0 - gap) * 0.5).clamp(170.0, 214.0);
            let total = btn_w * 2.0 + gap;
            let button = Rect::from_center_size(
                Pos2::new(center_x - total * 0.5 + btn_w * 0.5, y),
                Vec2::new(btn_w, 34.0),
            );
            if welcome_cta(ui, button, "quick_open", "快速打开文件", "Ctrl+P", true) {
                self.open_palette(PaletteMode::QuickOpen);
            }
            let button2 = Rect::from_center_size(
                Pos2::new(center_x + total * 0.5 - btn_w * 0.5, y),
                Vec2::new(btn_w, 34.0),
            );
            if welcome_cta(ui, button2, "cmd_palette", "命令面板", "Ctrl+Shift+P", false) {
                self.open_palette(PaletteMode::Commands);
            }
            y += 48.0;
            if y + 12.0 <= bottom_limit {
                self.shortcut_hints(ui, style, center_x, y);
            }
            footer(ui);
            return;
        }

        let cta_w = 248.0;
        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(cta_w, 34.0));
        if welcome_cta(ui, button, "open_folder", "打开文件夹", "Ctrl+Shift+O", true) {
            self.open_folder_dialog();
        }
        y += 42.0;
        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(cta_w, 34.0));
        if welcome_cta(ui, button, "open_file", "打开文件", "Ctrl+O", false) {
            self.open_file_dialog();
        }
        y += 42.0;
        let button = Rect::from_center_size(Pos2::new(center_x, y), Vec2::new(cta_w, 34.0));
        if welcome_cta(ui, button, "new_file", "新建文件", "Ctrl+N", false) {
            self.new_untitled();
        }

        let projects: Vec<String> = self
            .settings
            .recent_folders
            .iter()
            .filter(|folder| std::path::Path::new(folder).is_dir())
            .take(6)
            .cloned()
            .collect();
        let files: Vec<PathBuf> = self
            .recent_files
            .iter()
            .filter(|path| path.is_file())
            .take(12)
            .cloned()
            .collect();
        if !projects.is_empty() || !files.is_empty() {
            // Row budget: the footer stays clear on 760x480, and a cramped
            // window still gets the recent-files list instead of nothing.
            let mut header_gap = 48.0;
            if bottom_limit - (y + header_gap) < 48.0 {
                header_gap = 24.0;
            }
            let start_y = y + header_gap;
            let rows_single = ((bottom_limit - start_y - 22.0) / 28.0).floor().max(0.0) as usize;
            let rows_pair = ((bottom_limit - start_y - 12.0 - 44.0) / 56.0)
                .floor()
                .max(0.0) as usize;
            let two_columns = !projects.is_empty()
                && !files.is_empty()
                && rect.width() >= crate::theme::SP6 * 2.0 + 2.0 * 330.0 + crate::theme::SP5;
            let (project_rows, file_rows) = if two_columns
                || projects.is_empty()
                || files.is_empty()
            {
                (
                    rows_single.min(projects.len()),
                    rows_single.min(files.len()),
                )
            } else if rows_pair > 0 {
                (
                    rows_pair.min(projects.len()),
                    rows_pair.min(files.len()),
                )
            } else {
                // One section only: the new recent-files list wins the space.
                (0, rows_single.min(files.len()))
            };
            let column = if two_columns {
                ((rect.width() - crate::theme::SP6 * 2.0 - crate::theme::SP5) * 0.5).min(330.0)
            } else {
                330.0
            };
            let gap = crate::theme::SP5;
            let (projects_x, files_x) = if two_columns {
                (
                    center_x - gap * 0.5 - column * 0.5,
                    center_x + gap * 0.5 + column * 0.5,
                )
            } else {
                (center_x, center_x)
            };
            let mut next_y = start_y;
            if project_rows > 0 {
                ui.painter().text(
                    Pos2::new(projects_x - column * 0.5 + 8.0, start_y),
                    Align2::LEFT_CENTER,
                    "最近打开的项目",
                    style.small_bold(),
                    theme.text_dim,
                );
                let mut row_y = start_y + 24.0;
                for recent in projects.iter().take(project_rows) {
                    let name = std::path::Path::new(recent)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| recent.clone());
                    let row =
                        Rect::from_center_size(Pos2::new(projects_x, row_y), Vec2::new(column, 28.0));
                    let resp = widgets::paint_row_with_id(
                        ui,
                        row,
                        ("recent_proj", recent.as_str()),
                        &theme,
                        widgets::RowState::idle(),
                    );
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
                        theme.kind.is_dark(),
                    );
                    let name_max = (column * 0.42).clamp(72.0, 140.0);
                    let (name_galley, _) = widgets::ellipsize_mode(
                        ui,
                        &name,
                        style.small(),
                        theme.text,
                        name_max,
                        widgets::EllipsizeMode::End,
                    );
                    let name_w = name_galley.rect.width();
                    ui.painter().galley(
                        Pos2::new(
                            row.left() + 28.0,
                            row.center().y - name_galley.rect.height() * 0.5,
                        ),
                        name_galley,
                        theme.text,
                    );
                    let path_max = (column - 28.0 - name_w - 22.0).max(36.0);
                    let (path_galley, _) = widgets::ellipsize_mode(
                        ui,
                        recent,
                        style.tiny(),
                        theme.text_muted,
                        path_max,
                        widgets::EllipsizeMode::Middle,
                    );
                    ui.painter().galley(
                        Pos2::new(
                            row.right() - 10.0 - path_galley.rect.width(),
                            row.center().y - path_galley.rect.height() * 0.5,
                        ),
                        path_galley,
                        theme.text_muted,
                    );
                    if resp.on_hover_text(recent).clicked() {
                        let path = PathBuf::from(recent);
                        self.open_folder(path);
                    }
                    row_y += 28.0;
                }
                next_y = row_y;
            }
            if file_rows > 0 {
                let section_y = if project_rows > 0 && !two_columns {
                    next_y + 12.0
                } else {
                    start_y
                };
                ui.painter().text(
                    Pos2::new(files_x - column * 0.5 + 8.0, section_y),
                    Align2::LEFT_CENTER,
                    "最近打开的文件",
                    style.small_bold(),
                    theme.text_dim,
                );
                let mut row_y = section_y + 24.0;
                for path in files.iter().take(file_rows) {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let row =
                        Rect::from_center_size(Pos2::new(files_x, row_y), Vec2::new(column, 28.0));
                    let resp = widgets::paint_row_with_id(
                        ui,
                        row,
                        ("recent_file", path),
                        &theme,
                        widgets::RowState::idle(),
                    );
                    icons::paint_path_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(row.left() + 14.0, row.center().y),
                            Vec2::splat(14.0),
                        ),
                        path,
                        false,
                        false,
                        theme.text_dim,
                        theme.kind.is_dark(),
                    );
                    let name_max = (column * 0.48).clamp(76.0, 156.0);
                    let (name_galley, _) = widgets::ellipsize_mode(
                        ui,
                        &name,
                        style.small(),
                        theme.text,
                        name_max,
                        widgets::EllipsizeMode::End,
                    );
                    let name_w = name_galley.rect.width();
                    ui.painter().galley(
                        Pos2::new(
                            row.left() + 28.0,
                            row.center().y - name_galley.rect.height() * 0.5,
                        ),
                        name_galley,
                        theme.text,
                    );
                    let dir_label = path
                        .parent()
                        .map(|dir| {
                            self.workspace
                                .root
                                .as_ref()
                                .and_then(|root| dir.strip_prefix(root).ok())
                                .filter(|rel| !rel.as_os_str().is_empty())
                                .map(|rel| rel.to_string_lossy().to_string())
                                .unwrap_or_else(|| dir.to_string_lossy().to_string())
                        })
                        .unwrap_or_default();
                    let dir_max = (column - 28.0 - name_w - 22.0).max(36.0);
                    let (dir_galley, _) = widgets::ellipsize_mode(
                        ui,
                        &dir_label,
                        style.tiny(),
                        theme.text_muted,
                        dir_max,
                        widgets::EllipsizeMode::Middle,
                    );
                    ui.painter().galley(
                        Pos2::new(
                            row.right() - 10.0 - dir_galley.rect.width(),
                            row.center().y - dir_galley.rect.height() * 0.5,
                        ),
                        dir_galley,
                        theme.text_muted,
                    );
                    if resp.on_hover_text(path.to_string_lossy().to_string()).clicked() {
                        self.open_file(path.clone());
                    }
                    row_y += 28.0;
                }
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
        let measure = |s: &str| -> f32 {
            ui.ctx().fonts_mut(|fonts| {
                fonts
                    .layout_job(LayoutJob::simple_singleline(
                        s.to_string(),
                        style.tiny(),
                        theme.text_dim,
                    ))
                    .rect
                    .width()
            })
        };
        let avail = ui.max_rect().width() - crate::theme::SP6;
        let mut widths = Vec::new();
        let mut total = 0.0;
        for (label, key) in hints {
            let key_w = widgets::measure_keycap_width(ui, key, style.tiny());
            let w = key_w + 6.0 + measure(label) + crate::theme::SP4;
            if total + w > avail {
                break;
            }
            total += w;
            widths.push((key_w, w));
        }
        let mut x = center_x - total * 0.5;
        for ((label, key), (key_w, w)) in hints.into_iter().zip(widths) {
            widgets::paint_keycap_right(ui, ui.painter(), x + key_w, y, key, style.tiny(), &theme);
            ui.painter().text(
                Pos2::new(x + key_w + 6.0, y),
                Align2::LEFT_CENTER,
                label,
                style.tiny(),
                theme.text_muted,
            );
            x += w;
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
        widgets::paint_button(
            ui,
            rect,
            ("primary", label),
            label,
            style.small_bold(),
            &style.theme,
            widgets::ButtonVariant::Primary,
            true,
        )
        .clicked()
    }

    pub(crate) fn secondary_button(
        &self,
        ui: &mut Ui,
        rect: Rect,
        label: &str,
        style: &UiStyle,
    ) -> bool {
        widgets::paint_button(
            ui,
            rect,
            ("secondary", label),
            label,
            style.small(),
            &style.theme,
            widgets::ButtonVariant::Secondary,
            true,
        )
        .clicked()
    }
}


impl KestrelApp {
    /// Newest-first recent file list, capped at 12 entries (welcome page).
    pub(crate) fn record_recent_file(&mut self, path: &std::path::Path) {
        push_recent_file(&mut self.recent_files, path.to_path_buf());
    }

    /// Toggle the indent unit between spaces and tab characters.
    pub(crate) fn toggle_indent(&mut self) {
        self.settings.insert_spaces = !self.settings.insert_spaces;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.detected_indent = None;
        }
        self.settings.save();
    }

    /// Re-encode the buffer on the next save (text itself stays UTF-8 in memory).
    fn set_active_encoding(&mut self, encoding: crate::doc::Encoding) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab_is_editable_text(tab) || tab.doc.encoding == encoding {
            return;
        }
        tab.doc.encoding = encoding;
        tab.doc.bom = encoding == crate::doc::Encoding::Utf8Bom;
        if tab.doc.path.is_some() || tab.doc.len_chars() > 0 {
            tab.doc.mark_dirty();
        }
        self.toast(
            format!("保存时按 {} 写入", encoding.as_str()),
            crate::app::ToastKind::Info,
        );
    }

    /// First Esc clears the open palette query, the next one closes the overlay.
    fn handle_palette_escape(&mut self, ctx: &egui::Context) {
        if !self.palette.open || self.palette.query.is_empty() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.palette.query.clear();
            self.palette.selected = 0;
            self.palette.needs_filter = true;
        }
    }

    /// Clicking the search pill focuses an already open quick-open overlay
    /// instead of resetting its query.
    fn focus_or_open_quick_open(&mut self) {
        if self.palette.open && self.palette.mode == PaletteMode::QuickOpen {
            self.palette.focus = true;
        } else {
            self.open_palette(PaletteMode::QuickOpen);
        }
    }

    /// Whether a menu entry can run right now; disabled entries paint grey.
    fn command_enabled(&self, command: Command) -> bool {
        let editor = self.tabs.get(self.active);
        let text_editable = editor.is_some_and(tab_is_editable_text);
        let markdown = editor.is_some_and(|tab| tab.language == crate::highlight::Language::Markdown);
        match command {
            Command::Save | Command::SaveAs | Command::FormatDocument => text_editable,
            Command::SaveAll => self
                .tabs
                .iter()
                .any(|tab| tab.is_dirty() && tab.diff.is_none()),
            Command::CloseTab => editor.is_some(),
            Command::CloseAllTabs => !self.tabs.is_empty(),
            Command::ReloadFile => self.active_path().is_some(),
            Command::Undo => editor.is_some_and(|tab| tab.doc.can_undo()),
            Command::Redo => editor.is_some_and(|tab| tab.doc.can_redo()),
            Command::Cut
            | Command::Paste
            | Command::ToggleComment
            | Command::DuplicateLine
            | Command::DeleteLine
            | Command::MoveLineUp
            | Command::MoveLineDown
            | Command::Indent
            | Command::Outdent
            | Command::ToggleFold
            | Command::FoldAll
            | Command::UnfoldAll
            | Command::TriggerSuggest
            | Command::ToggleEol
            | Command::ToggleWordWrap
            | Command::SplitEditor
            | Command::SetLanguage => text_editable,
            Command::Copy
            | Command::SelectAll
            | Command::SelectNextOccurrence
            | Command::SelectAllOccurrences
            | Command::Find
            | Command::Replace
            | Command::FindNext
            | Command::FindPrev
            | Command::GotoLine
            | Command::GotoDefinition
            | Command::GotoSymbol => editor.is_some(),
            Command::ToggleMarkdownPreview | Command::MarkdownPreviewSide => markdown,
            Command::ZoomReset => (self.settings.ui_zoom - 1.0).abs() > 0.001,
            Command::ThemeDark => self.settings.theme != "dark",
            Command::ThemeLight => self.settings.theme != "light",
            Command::NextTab | Command::PrevTab => self
                .groups
                .get(self.active_group)
                .is_some_and(|group| group.tabs.len() > 1),
            Command::CopyPath | Command::RevealInExplorer => self.active_path().is_some(),
            Command::RefreshExplorer | Command::ShowGit => self.workspace.root.is_some(),
            _ => true,
        }
    }
}

/// Text the editor may modify: no diff view, no binary buffer, not read-only.
fn tab_is_editable_text(tab: &crate::editor::Editor) -> bool {
    tab.diff.is_none()
        && tab.doc.encoding != crate::doc::Encoding::Binary
        && !tab.doc.readonly
        && tab
            .multimodal
            .as_ref()
            .is_none_or(|viewer| viewer.is_showing_source())
}

/// Newest-first recent file list; duplicates move to the front (capped at 12).
fn push_recent_file(list: &mut Vec<PathBuf>, path: PathBuf) {
    list.retain(|known| known != &path);
    list.insert(0, path);
    list.truncate(12);
}

/// Encodings the status bar can switch to; the next save converts the buffer.
const ENCODINGS: &[crate::doc::Encoding] = &[
    crate::doc::Encoding::Utf8,
    crate::doc::Encoding::Utf8Bom,
    crate::doc::Encoding::Utf16Le,
    crate::doc::Encoding::Utf16Be,
];

/// Tab label as painted, including the preview suffix.
fn tab_display_title(tab: &crate::editor::Editor) -> String {
    let title = tab.title();
    if tab.preview {
        format!("{title} · 预览")
    } else {
        title
    }
}

/// Measured tab width; the tab bar and its scroll bounds share it.
/// Reserves full horizontal chrome (31px left icon + 26px right close + 8px breathing room)
/// using the bold active-tab font so normal filenames never prematurely ellipsize.
fn tab_width(ui: &Ui, style: &UiStyle, tab: &crate::editor::Editor) -> f32 {
    let title = tab_display_title(tab);
    let text_width = ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_job(LayoutJob::simple_singleline(
                title,
                style.small_bold(),
                style.theme.text,
            ))
            .rect
            .width()
    });
    (text_width + 66.0).clamp(116.0, 248.0)
}

/// One painted status-bar segment; `action` is `None` for plain text.
struct StatusSegment {
    text: String,
    color: Color32,
    font: FontId,
    action: Option<StatusAction>,
    tip: &'static str,
}

/// What clicking a status-bar segment does.
enum StatusAction {
    Zoom,
    Language,
    Cursor,
    Indent,
    Encoding,
    Eol,
    Branch,
    Problems,
    Plugin(usize),
}

enum TabAction {
    TogglePin(u64),
    Split(usize, u64),
    Close(usize, u64),
    CloseOthers(usize, u64),
    CloseRight(usize, u64),
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
        let fade = widgets::hover_fade(ui, (id_salt, *label, "capsule"), resp.hovered());
        if active {
            ui.painter()
                .rect_filled(seg_r, CornerRadius::same(4), theme.accent);
        } else if fade > 0.001 {
            ui.painter().rect_filled(
                seg_r,
                CornerRadius::same(4),
                widgets::blend(theme.input_bg, theme.hover, fade),
            );
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
    use super::{
        preview_y_for_source_line, push_recent_file, source_line_for_preview_y,
        tab_is_editable_text,
    };
    use crate::commands::Command;
    use std::path::PathBuf;
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

    #[test]
    fn recent_files_stay_newest_first_deduped_and_capped() {
        let mut list = Vec::new();
        for index in 0..14 {
            push_recent_file(&mut list, PathBuf::from(format!("file-{index}.rs")));
        }
        assert_eq!(list.len(), 12);
        assert_eq!(list[0], PathBuf::from("file-13.rs"));
        assert_eq!(list[11], PathBuf::from("file-2.rs"));
        // Re-opening a known file moves it to the front instead of duplicating.
        push_recent_file(&mut list, PathBuf::from("file-5.rs"));
        assert_eq!(list.len(), 12);
        assert_eq!(list[0], PathBuf::from("file-5.rs"));
        assert_eq!(
            list.iter()
                .filter(|path| path.ends_with("file-5.rs"))
                .count(),
            1
        );
    }

    #[test]
    fn session_roundtrips_recent_files() {
        let session = crate::settings::Session {
            recent_files: vec!["D:/work/a.rs".to_string(), "D:/work/b.md".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: crate::settings::Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.recent_files, session.recent_files);
    }

    #[test]
    fn document_settings_commands_are_registered() {
        assert_eq!(Command::from_id("file.toggleEol"), Some(Command::ToggleEol));
        assert_eq!(
            Command::from_id("edit.toggleIndent"),
            Some(Command::ToggleIndent)
        );
        assert_eq!(Command::ToggleEol.category(), "文件");
        assert_eq!(Command::ToggleIndent.category(), "编辑");
    }

    #[test]
    fn read_only_and_binary_tabs_are_not_editable_text() {
        use crate::doc::{Document, Encoding};
        use crate::editor::Editor;
        let mut editor = Editor::new(Document::from_text("fn main() {}"), 1, 0);
        assert!(tab_is_editable_text(&editor));
        editor.doc.readonly = true;
        assert!(!tab_is_editable_text(&editor));
        editor.doc.readonly = false;
        editor.doc.encoding = Encoding::Binary;
        assert!(!tab_is_editable_text(&editor));
    }
}
