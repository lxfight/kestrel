//! Sidebar panels: Explorer (with Open Editors & Outline), Project Search & Replace,
//! Source Control (Git), and Symbol Outline.

use std::path::PathBuf;
use std::time::Instant;

use egui::text::LayoutJob;
use egui::{
    Align2, CornerRadius, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};

use crate::app::{KestrelApp, SidebarMode, ToastKind, TreeClipboard, TreeClipboardKind, TreeEditKind};
use crate::git::GitFileStatus;
use crate::icons::{self, Icon};
use crate::ui::UiStyle;

impl KestrelApp {
    pub fn sidebar_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        ui.spacing_mut().item_spacing.y = 0.0;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.sidebar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.right() - 0.5, rect.top()),
                Pos2::new(rect.right() - 0.5, rect.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );
        match self.sidebar.mode {
            SidebarMode::Explorer => self.explorer_ui(ui, style),
            SidebarMode::Search => self.search_panel_ui(ui, style),
            SidebarMode::Git => self.git_panel_ui(ui, style),
            SidebarMode::Outline => self.outline_panel_ui(ui, style),
            SidebarMode::Plugins => self.plugin_panel_ui(ui, style),
        }
    }

    fn explorer_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let mut new_file = false;
        let mut new_folder = false;
        let mut refresh = false;

        // Top Panel Header
        {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
            ui.painter().text(
                Pos2::new(rect.left() + 14.0, rect.center().y),
                Align2::LEFT_CENTER,
                "资源管理器",
                style.font(11.5),
                theme.text_dim,
            );
            let mut x = rect.right() - 8.0;
            let mut action_ui = |ui: &mut Ui, icon: Icon, tip: &str| -> bool {
                x -= 24.0;
                let r =
                    Rect::from_center_size(Pos2::new(x + 11.0, rect.center().y), Vec2::splat(22.0));
                let resp = ui.interact(
                    r,
                    ui.scope_id().with(("explorer_action", tip)),
                    Sense::click(),
                );
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(5), theme.hover);
                }
                icons::paint(
                    &ui.painter(),
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
            if action_ui(ui, Icon::CollapseAll, "全部折叠") {
                self.workspace.tree.collapse_all();
            }
            if action_ui(ui, Icon::Refresh, "刷新资源管理器") {
                refresh = true;
            }
            if action_ui(ui, Icon::NewFolder, "新建文件夹") {
                new_folder = true;
            }
            if action_ui(ui, Icon::NewFile, "新建文件") {
                new_file = true;
            }
        }

        if refresh {
            self.workspace.tree.reload();
            if let Some(root) = self.workspace.root.clone() {
                self.workspace.index.scan(&root);
                self.git.refresh(&root);
            }
        }
        if let Some(root) = self.workspace.root.clone() {
            if new_file {
                let dir = self
                    .sidebar
                    .selected
                    .as_ref()
                    .and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|x| x.to_path_buf())
                        }
                    })
                    .unwrap_or(root.clone());
                self.workspace.tree.expand(&dir);
                self.start_tree_edit(TreeEditKind::NewFile, dir, None);
            }
            if new_folder {
                let dir = self
                    .sidebar
                    .selected
                    .as_ref()
                    .and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|x| x.to_path_buf())
                        }
                    })
                    .unwrap_or(root.clone());
                self.workspace.tree.expand(&dir);
                self.start_tree_edit(TreeEditKind::NewFolder, dir, None);
            }
        }

        // Section 1: Open Editors (with proper collapsible section header)
        if !self.tabs.is_empty() {
            self.open_editors_ui(ui, style);
        }

        if self.workspace.root.is_none() {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 120.0), Sense::hover());
            ui.painter().text(
                Pos2::new(rect.center().x, rect.top() + 26.0),
                Align2::CENTER_CENTER,
                "尚未打开文件夹",
                style.small(),
                theme.text_faint,
            );
            let button = Rect::from_center_size(
                Pos2::new(rect.center().x, rect.top() + 58.0),
                Vec2::new(148.0, 28.0),
            );
            let resp = ui.interact(
                button,
                ui.scope_id().with("open_folder_welcome"),
                Sense::click(),
            );
            ui.painter().rect_filled(
                button,
                CornerRadius::same(6),
                if resp.hovered() {
                    theme.accent.gamma_multiply(0.9)
                } else {
                    theme.accent
                },
            );
            ui.painter().text(
                button.center(),
                Align2::CENTER_CENTER,
                "打开文件夹",
                style.small(),
                theme.accent_text,
            );
            if resp.clicked() {
                self.open_folder_dialog();
            }
            return;
        }

        // Section 2: Workspace Folder Header (clickable to collapse/expand root directory)
        let root_path = self.workspace.root.clone().unwrap();
        let root_name = root_path
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "WORKSPACE".to_string());
        let mut root_expanded = self.workspace.tree.is_expanded(&root_path);
        let prev_root_expanded = root_expanded;
        let root_hdr_resp = Self::section_header_ui(
            ui,
            style,
            &root_name,
            Some(&self.workspace.index.files().len().to_string()),
            &mut root_expanded,
            "workspace_root_hdr",
        );
        if root_expanded != prev_root_expanded {
            self.workspace.tree.toggle_dir(&root_path);
        }

        // File Tree
        let rows = self.workspace.tree.rows();
        let mut toggle: Option<PathBuf> = None;
        let mut open_preview: Option<PathBuf> = None;
        let mut open_pinned: Option<PathBuf> = None;
        let mut select: Option<PathBuf> = None;
        let mut context_action: Option<TreeContext> = None;
        let mut drag_start_path: Option<PathBuf> = None;
        let mut move_tree: Option<(PathBuf, PathBuf)> = None;
        let dragging_tree_src = match &self.drag_payload {
            Some(crate::app::DragPayload::ExplorerPath(p)) => Some(p.clone()),
            _ => None,
        };
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_pos = ui.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()));

        if let Some(ref src) = dragging_tree_src
            && pointer_pos.is_some_and(|p| root_hdr_resp.rect.contains(p))
            && src.parent() != Some(root_path.as_path())
            && !root_path.starts_with(src)
        {
            ui.painter().rect_stroke(
                root_hdr_resp.rect.shrink(1.0),
                2.0,
                egui::Stroke::new(1.5, theme.accent),
                egui::StrokeKind::Inside,
            );
            if !pointer_down {
                move_tree = Some((src.clone(), root_path.clone()));
            }
        }
        let mut commit_edit = false;
        let mut cancel_edit = false;
        let editing = self.sidebar.editing.is_some();
        let has_clipboard = self.sidebar.clipboard.is_some();

        let editor_focused = self.tabs.get(self.active).is_some_and(|t| t.has_focus);
        let term_focused = self.bottom.visible && self.bottom.terminal_focus;
        if !editing
            && !editor_focused
            && !term_focused
            && self.dialog.is_none()
            && !self.palette.open
            && let Some(selected_path) = self.sidebar.selected.clone()
        {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2)) {
                context_action = Some(TreeContext::Rename(selected_path.clone()));
            } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)) {
                context_action = Some(TreeContext::Delete(selected_path));
            }
        }

        root_hdr_resp.context_menu(|ui| {
            Self::root_context_menu(ui, style, &root_path, has_clipboard, &mut context_action);
        });

        let avail_h = ui.available_height();
        let outline_h = if self.sidebar.outline_open {
            self.settings
                .outline_height
                .clamp(70.0, (avail_h - 110.0).max(70.0))
        } else {
            28.0
        };
        let tree_h = (avail_h - outline_h).max(100.0);

        ScrollArea::vertical()
            .max_height(tree_h)
            .auto_shrink([false, false])
            .id_salt("explorer_tree")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let edit = self.sidebar.editing.as_ref().map(|e| {
                    (
                        e.kind,
                        e.parent.clone(),
                        e.original.clone(),
                        e.name.clone(),
                        e.focus,
                    )
                });
                if let Some((_, parent, _, _, _)) = &edit {
                    self.workspace.tree.expand(parent);
                }
                let mut edit_drawn = false;
                let mut editing_active = editing;
                for row in &rows {
                    let is_selected = self.sidebar.selected.as_ref() == Some(&row.path);
                    let is_cut = self.sidebar.clipboard.as_ref().is_some_and(|c| {
                        c.kind == TreeClipboardKind::Cut && c.path == row.path
                    });
                    let resp = ui
                        .push_id(&row.path, |ui| {
                            ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 24.0),
                                Sense::click_and_drag(),
                            )
                        })
                        .inner
                        .1;
                    let rect = resp.rect;
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, theme.hover);
                    }
                    if let Some(ref src) = dragging_tree_src
                        && row.is_dir
                        && pointer_pos.is_some_and(|p| rect.contains(p))
                        && !row.path.starts_with(src)
                        && src.parent() != Some(row.path.as_path())
                    {
                        ui.painter().rect_stroke(
                            rect.shrink(1.0),
                            2.0,
                            egui::Stroke::new(1.5, theme.accent),
                            egui::StrokeKind::Inside,
                        );
                        if !pointer_down {
                            move_tree = Some((src.clone(), row.path.clone()));
                        }
                    }
                    if is_selected {
                        ui.painter().rect_filled(rect, 0.0, theme.active);
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                Pos2::new(rect.left(), rect.top() + 3.0),
                                Vec2::new(2.0, rect.height() - 6.0),
                            ),
                            1.0,
                            theme.accent,
                        );
                    }
                    let indent = 10.0 + row.depth as f32 * 14.0;
                    for level in 0..row.depth {
                        let gx = rect.left() + 10.0 + level as f32 * 14.0 + 6.0;
                        ui.painter().line_segment(
                            [Pos2::new(gx, rect.top()), Pos2::new(gx, rect.bottom())],
                            Stroke::new(1.0, theme.indent_guide),
                        );
                    }
                    let chevron_rect = Rect::from_center_size(
                        Pos2::new(rect.left() + indent + 5.0, rect.center().y),
                        Vec2::splat(13.0),
                    );
                    let expanded = self.workspace.tree.is_expanded(&row.path);
                    if row.is_dir {
                        icons::paint(
                            &ui.painter(),
                            chevron_rect,
                            if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            theme.text_dim,
                            1.2,
                        );
                    }
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(rect.left() + indent + 21.0, rect.center().y),
                        Vec2::splat(15.0),
                    );
                    icons::paint_path_icon(
                        &ui.painter(),
                        icon_rect,
                        &row.path,
                        row.is_dir,
                        expanded,
                        theme.text_dim,
                    );

                    let git_status = if !row.is_dir {
                        self.git.status_of(&row.path)
                    } else {
                        None
                    };
                    let mut name_color = match git_status {
                        Some(GitFileStatus::Modified) => theme.git_modified,
                        Some(GitFileStatus::Added | GitFileStatus::Untracked) => theme.git_added,
                        Some(GitFileStatus::Deleted | GitFileStatus::Conflicted) => {
                            theme.git_deleted
                        }
                        _ if row.is_dir || is_selected => theme.text,
                        _ => theme.text_dim,
                    };
                    if is_cut {
                        name_color = name_color.gamma_multiply(0.5);
                    }

                    let is_edit_row = edit.as_ref().is_some_and(|(kind, _, original, _, _)| {
                        *kind == TreeEditKind::Rename && original.as_ref() == Some(&row.path)
                    });
                    if is_edit_row {
                        if let Some((_, _, _, name, focus)) = edit.as_ref() {
                            let mut name = name.clone();
                            let te_rect = Rect::from_min_max(
                                Pos2::new(rect.left() + indent + 33.0, rect.top() + 2.0),
                                Pos2::new(rect.right() - 8.0, rect.bottom() - 2.0),
                            );
                            let te_resp = ui.put(
                                te_rect,
                                TextEdit::singleline(&mut name)
                                    .font(style.small())
                                    .margin(Vec2::new(4.0, 2.0)),
                            );
                            if *focus {
                                te_resp.request_focus();
                                if let Some(edit) = self.sidebar.editing.as_mut() {
                                    edit.focus = false;
                                }
                            }
                            if te_resp.changed()
                                && let Some(edit) = self.sidebar.editing.as_mut()
                            {
                                edit.name = name.clone();
                            }
                            let enter = te_resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if enter {
                                commit_edit = true;
                            } else if escape || (!*focus && te_resp.lost_focus()) {
                                cancel_edit = true;
                            }
                            edit_drawn = true;
                        }
                    } else {
                        ui.painter().text(
                            Pos2::new(rect.left() + indent + 34.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &row.name,
                            style.small(),
                            name_color,
                        );
                        // Dirty dot or Git status badge on the right
                        let is_dirty_tab = self
                            .tabs
                            .iter()
                            .any(|t| t.doc.path.as_deref() == Some(row.path.as_path()) && t.is_dirty());
                        if is_dirty_tab {
                            ui.painter().circle_filled(
                                Pos2::new(rect.right() - 12.0, rect.center().y),
                                3.0,
                                theme.accent,
                            );
                        } else if let Some(st) = git_status {
                            ui.painter().text(
                                Pos2::new(rect.right() - 10.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                st.badge(),
                                style.tiny(),
                                name_color,
                            );
                        }
                    }

                    if !is_edit_row {
                        if resp.drag_started_by(egui::PointerButton::Primary) {
                            drag_start_path = Some(row.path.clone());
                        }
                        if resp.double_clicked() && !row.is_dir {
                            select = Some(row.path.clone());
                            open_pinned = Some(row.path.clone());
                        } else if resp.clicked() {
                            select = Some(row.path.clone());
                            if row.is_dir {
                                toggle = Some(row.path.clone());
                            } else {
                                open_preview = Some(row.path.clone());
                            }
                        } else if resp.secondary_clicked() {
                            select = Some(row.path.clone());
                        }
                    }

                    let row_path = row.path.clone();
                    let is_dir = row.is_dir;
                    let parent_dir = row_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| root_path.clone());
                    let target_dir = if is_dir {
                        row_path.clone()
                    } else {
                        parent_dir.clone()
                    };
                    resp.context_menu(|ui| {
                        ui.set_width(212.0);
                        if !is_dir {
                            if Self::context_item(ui, style, Icon::FileCode, "打开文件", None) {
                                context_action = Some(TreeContext::OpenPinned(row_path.clone()));
                                ui.close();
                            }
                            if crate::highlight::Language::from_path(&row_path)
                                == crate::highlight::Language::Markdown
                                && Self::context_item(
                                    ui,
                                    style,
                                    Icon::Preview,
                                    "打开 Markdown 预览",
                                    None,
                                )
                            {
                                context_action =
                                    Some(TreeContext::OpenMarkdownPreview(row_path.clone()));
                                ui.close();
                            }
                            if Self::context_item(
                                ui,
                                style,
                                Icon::SplitHorizontal,
                                "在侧边分屏中打开",
                                None,
                            ) {
                                context_action = Some(TreeContext::OpenToSide(row_path.clone()));
                                ui.close();
                            }
                            ui.separator();
                        }
                        if Self::context_item(ui, style, Icon::NewFile, "新建文件", None) {
                            context_action = Some(TreeContext::NewFile(target_dir.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::NewFolder, "新建文件夹", None) {
                            context_action = Some(TreeContext::NewFolder(target_dir.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::Terminal, "在集成终端中打开", None)
                        {
                            context_action = Some(TreeContext::OpenInTerminal(target_dir.clone()));
                            ui.close();
                        }
                        if is_dir
                            && Self::context_item(ui, style, Icon::Search, "在文件夹中查找", None)
                        {
                            context_action = Some(TreeContext::SearchInFolder(row_path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if Self::context_item(ui, style, Icon::Discard, "剪切", None) {
                            context_action = Some(TreeContext::Cut(row_path.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::Copy, "复制", None) {
                            context_action = Some(TreeContext::Copy(row_path.clone()));
                            ui.close();
                        }
                        if has_clipboard
                            && Self::context_item(ui, style, Icon::Plus, "粘贴", None)
                        {
                            context_action = Some(TreeContext::Paste(target_dir.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::NewFile, "创建副本", None) {
                            context_action = Some(TreeContext::Duplicate(row_path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if Self::context_item(ui, style, Icon::Replace, "重命名", Some("F2")) {
                            context_action = Some(TreeContext::Rename(row_path.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::Trash, "删除", Some("Delete")) {
                            context_action = Some(TreeContext::Delete(row_path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if Self::context_item(ui, style, Icon::Copy, "复制路径", None) {
                            context_action = Some(TreeContext::CopyPath(row_path.clone()));
                            ui.close();
                        }
                        if Self::context_item(ui, style, Icon::Copy, "复制相对路径", None) {
                            context_action = Some(TreeContext::CopyRelativePath(row_path.clone()));
                            ui.close();
                        }
                        if Self::context_item(
                            ui,
                            style,
                            Icon::FolderOpen,
                            "在资源管理器中显示",
                            None,
                        ) {
                            context_action = Some(TreeContext::Reveal(row_path.clone()));
                            ui.close();
                        }
                    });

                    // Inline new file/folder input row
                    if let Some((kind, parent, original, name, focus)) = edit.as_ref()
                        && original.is_none()
                        && *kind != TreeEditKind::Rename
                        && parent == &row.path
                        && row.is_dir
                        && !edit_drawn
                        && !editing_active
                    {
                        editing_active = true;
                        let (edit_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            Sense::hover(),
                        );
                        ui.painter().rect_filled(edit_rect, 0.0, theme.active);
                        let mut name = name.clone();
                        let te_rect = Rect::from_min_max(
                            Pos2::new(edit_rect.left() + indent + 34.0, edit_rect.top() + 2.0),
                            Pos2::new(edit_rect.right() - 8.0, edit_rect.bottom() - 2.0),
                        );
                        let te_resp = ui.put(
                            te_rect,
                            TextEdit::singleline(&mut name)
                                .font(style.small())
                                .margin(Vec2::new(4.0, 2.0)),
                        );
                        if *focus {
                            te_resp.request_focus();
                            if let Some(edit) = self.sidebar.editing.as_mut() {
                                edit.focus = false;
                            }
                        }
                        if te_resp.changed()
                            && let Some(edit) = self.sidebar.editing.as_mut()
                        {
                            edit.name = name.clone();
                        }
                        let enter =
                            te_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if enter {
                            commit_edit = true;
                        } else if escape || (!*focus && te_resp.lost_focus()) {
                            cancel_edit = true;
                        }
                        edit_drawn = true;
                    }
                }
                if let Some((kind, parent, None, name, focus)) = edit
                    && kind != TreeEditKind::Rename
                    && !edit_drawn
                {
                    self.workspace.tree.expand(&parent);
                    let (rect, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, theme.active);
                    let mut name = name.clone();
                    let te_rect = Rect::from_min_max(
                        Pos2::new(rect.left() + 20.0, rect.top() + 2.0),
                        Pos2::new(rect.right() - 8.0, rect.bottom() - 2.0),
                    );
                    let te_resp =
                        ui.put(te_rect, TextEdit::singleline(&mut name).font(style.small()));
                    if focus {
                        te_resp.request_focus();
                        if let Some(edit) = self.sidebar.editing.as_mut() {
                            edit.focus = false;
                        }
                    }
                    if te_resp.changed()
                        && let Some(edit) = self.sidebar.editing.as_mut()
                    {
                        edit.name = name;
                    }
                    let enter =
                        te_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if enter {
                        commit_edit = true;
                    } else if escape || (!focus && te_resp.lost_focus()) {
                        cancel_edit = true;
                    }
                }

                // Empty space below tree rows also supports right-click context menu
                let rem_h = ui.available_height().max(48.0);
                let (_, empty_resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), rem_h),
                    Sense::click(),
                );
                empty_resp.context_menu(|ui| {
                    Self::root_context_menu(
                        ui,
                        style,
                        &root_path,
                        has_clipboard,
                        &mut context_action,
                    );
                });
            });

        // Section 3: Collapsible Outline at the bottom of Explorer
        if self.sidebar.outline_open {
            let (split_rect, split_resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 5.0), Sense::drag());
            let split_active = split_resp.hovered() || split_resp.dragged();
            if split_active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                ui.painter().line_segment(
                    [
                        Pos2::new(split_rect.left(), split_rect.center().y),
                        Pos2::new(split_rect.right(), split_rect.center().y),
                    ],
                    egui::Stroke::new(2.0, theme.accent),
                );
            } else {
                ui.painter().line_segment(
                    [
                        Pos2::new(split_rect.left(), split_rect.center().y),
                        Pos2::new(split_rect.right(), split_rect.center().y),
                    ],
                    egui::Stroke::new(1.0, theme.border),
                );
            }
            if split_resp.dragged() {
                let max_h = (avail_h - 110.0).max(70.0);
                self.settings.outline_height =
                    (self.settings.outline_height - split_resp.drag_delta().y).clamp(70.0, max_h);
            }
            if split_resp.double_clicked() {
                self.settings.outline_height = 170.0;
            }
        }
        let symbol_count = self
            .active_editor()
            .map(|e| e.symbols.len())
            .unwrap_or(0);
        let badge = symbol_count.to_string();
        let mut outline_open = self.sidebar.outline_open;
        Self::section_header_ui(
            ui,
            style,
            "大纲 (OUTLINE)",
            Some(&badge),
            &mut outline_open,
            "explorer_outline_hdr",
        );
        self.sidebar.outline_open = outline_open;
        if self.sidebar.outline_open {
            self.outline_list_ui(ui, style, "explorer_outline_scroll");
        }

        if let Some(path) = drag_start_path {
            self.drag_payload = Some(crate::app::DragPayload::ExplorerPath(path));
        }
        if let Some((src, dst_dir)) = move_tree {
            self.drag_payload = None;
            self.move_tree_path(src, dst_dir);
        }

        if let Some(path) = toggle {
            self.workspace.tree.toggle_dir(&path);
        }
        if let Some(path) = open_pinned {
            self.open_file(path);
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.preview = false;
            }
        } else if let Some(path) = open_preview {
            self.open_file_preview(path);
        }
        if let Some(path) = select {
            self.sidebar.selected = Some(path);
        }
        match context_action {
            Some(TreeContext::OpenPinned(path)) => {
                self.open_file(path);
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.preview = false;
                }
            }
            Some(TreeContext::OpenMarkdownPreview(path)) => {
                self.open_markdown_preview(path);
            }
            Some(TreeContext::OpenToSide(path)) => {
                self.open_file_to_side(path);
            }
            Some(TreeContext::NewFile(dir)) => {
                self.workspace.tree.expand(&dir);
                self.start_tree_edit(TreeEditKind::NewFile, dir, None);
            }
            Some(TreeContext::NewFolder(dir)) => {
                self.workspace.tree.expand(&dir);
                self.start_tree_edit(TreeEditKind::NewFolder, dir, None);
            }
            Some(TreeContext::OpenInTerminal(dir)) => {
                self.open_terminal_at(dir);
            }
            Some(TreeContext::SearchInFolder(dir)) => {
                let rel = dir
                    .strip_prefix(&root_path)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Search;
                self.workspace.search_panel.show_options = true;
                self.workspace.search_panel.include = if rel.is_empty() {
                    String::new()
                } else {
                    format!("{rel}/*")
                };
                self.workspace.search_panel.focus = true;
            }
            Some(TreeContext::Cut(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.sidebar.clipboard = Some(TreeClipboard {
                    path,
                    kind: TreeClipboardKind::Cut,
                });
                self.toast(format!("已剪切 {name}"), ToastKind::Info);
            }
            Some(TreeContext::Copy(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.sidebar.clipboard = Some(TreeClipboard {
                    path,
                    kind: TreeClipboardKind::Copy,
                });
                self.toast(format!("已复制 {name}"), ToastKind::Info);
            }
            Some(TreeContext::Paste(dir)) => {
                self.paste_tree_clipboard(dir);
            }
            Some(TreeContext::Duplicate(path)) => {
                self.duplicate_tree_path(path);
            }
            Some(TreeContext::Rename(path)) => {
                let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                self.start_tree_edit(TreeEditKind::Rename, parent, Some(path));
            }
            Some(TreeContext::Delete(path)) => {
                self.dialog = Some(crate::app::Dialog::ConfirmDelete(path));
            }
            Some(TreeContext::CopyPath(path)) => {
                ui.ctx().copy_text(path.to_string_lossy().to_string());
                self.toast("已复制路径", ToastKind::Info);
            }
            Some(TreeContext::CopyRelativePath(path)) => {
                let rel = path
                    .strip_prefix(&root_path)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.to_string_lossy().to_string());
                ui.ctx().copy_text(rel);
                self.toast("已复制相对路径", ToastKind::Info);
            }
            Some(TreeContext::Reveal(path)) => {
                crate::workspace::tree::reveal_in_explorer(&path);
            }
            Some(TreeContext::Refresh) => {
                self.workspace.tree.reload();
                self.workspace.index.scan(&root_path);
                self.git.refresh(&root_path);
            }
            Some(TreeContext::CollapseAll) => {
                self.workspace.tree.collapse_all();
            }
            None => {}
        }
        if commit_edit {
            let ctx = ui.ctx().clone();
            self.commit_tree_edit(&ctx);
        }
        if cancel_edit {
            self.sidebar.editing = None;
        }
    }

    fn root_context_menu(
        ui: &mut Ui,
        style: &UiStyle,
        root_path: &PathBuf,
        has_clipboard: bool,
        context_action: &mut Option<TreeContext>,
    ) {
        ui.set_width(212.0);
        if Self::context_item(ui, style, Icon::NewFile, "新建文件", None) {
            *context_action = Some(TreeContext::NewFile(root_path.clone()));
            ui.close();
        }
        if Self::context_item(ui, style, Icon::NewFolder, "新建文件夹", None) {
            *context_action = Some(TreeContext::NewFolder(root_path.clone()));
            ui.close();
        }
        if has_clipboard && Self::context_item(ui, style, Icon::Plus, "粘贴", None) {
            *context_action = Some(TreeContext::Paste(root_path.clone()));
            ui.close();
        }
        ui.separator();
        if Self::context_item(ui, style, Icon::Terminal, "在集成终端中打开", None) {
            *context_action = Some(TreeContext::OpenInTerminal(root_path.clone()));
            ui.close();
        }
        if Self::context_item(ui, style, Icon::Refresh, "刷新资源管理器", None) {
            *context_action = Some(TreeContext::Refresh);
            ui.close();
        }
        if Self::context_item(ui, style, Icon::CollapseAll, "全部折叠", None) {
            *context_action = Some(TreeContext::CollapseAll);
            ui.close();
        }
        ui.separator();
        if Self::context_item(ui, style, Icon::Copy, "复制工作区路径", None) {
            *context_action = Some(TreeContext::CopyPath(root_path.clone()));
            ui.close();
        }
        if Self::context_item(ui, style, Icon::FolderOpen, "在资源管理器中显示", None) {
            *context_action = Some(TreeContext::Reveal(root_path.clone()));
            ui.close();
        }
    }

    pub(crate) fn section_header_ui(
        ui: &mut Ui,
        style: &UiStyle,
        title: &str,
        badge: Option<&str>,
        expanded: &mut bool,
        id_salt: &str,
    ) -> egui::Response {
        let theme = style.theme;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
        let bg = if resp.hovered() {
            theme.hover
        } else {
            theme.panel_bg.gamma_multiply(0.7)
        };
        ui.painter().rect_filled(rect, 0.0, bg);
        ui.painter().line_segment(
            [Pos2::new(rect.left(), rect.top()), Pos2::new(rect.right(), rect.top())],
            Stroke::new(1.0, theme.border),
        );
        icons::paint(
            &ui.painter(),
            Rect::from_center_size(Pos2::new(rect.left() + 12.0, rect.center().y), Vec2::splat(12.0)),
            if *expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            theme.text_dim,
            1.2,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 24.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            style.font(10.5),
            theme.text_dim,
        );
        if let Some(badge_text) = badge {
            ui.painter().text(
                Pos2::new(rect.right() - 12.0, rect.center().y),
                Align2::RIGHT_CENTER,
                badge_text,
                style.tiny(),
                theme.text_faint,
            );
        }
        let _ = id_salt;
        if resp.clicked() {
            *expanded = !*expanded;
        }
        resp
    }

    pub(crate) fn context_item(
        ui: &mut Ui,
        style: &UiStyle,
        icon: Icon,
        label: &str,
        shortcut: Option<&str>,
    ) -> bool {
        let theme = style.theme;
        let width = ui.min_rect().width().max(212.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 25.0), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(4), theme.hover);
        }
        icons::paint(
            &ui.painter(),
            Rect::from_center_size(Pos2::new(rect.left() + 13.0, rect.center().y), Vec2::splat(14.0)),
            icon,
            if resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
            1.2,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            style.small(),
            theme.text,
        );
        if let Some(sc) = shortcut {
            ui.painter().text(
                Pos2::new(rect.right() - 8.0, rect.center().y),
                Align2::RIGHT_CENTER,
                sc,
                style.tiny(),
                theme.text_faint,
            );
        }
        resp.clicked()
    }

    fn open_editors_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let count_str = self.tabs.len().to_string();
        let mut open = self.sidebar.open_editors_open;
        Self::section_header_ui(
            ui,
            style,
            "打开的编辑器",
            Some(&count_str),
            &mut open,
            "open_editors_hdr",
        );
        self.sidebar.open_editors_open = open;

        let mut activate: Option<(usize, u64)> = None;
        let mut close: Option<(usize, usize)> = None;
        let mut save_idx: Option<(usize, u64)> = None;
        let mut split_id: Option<(usize, u64)> = None;
        let mut copy_path: Option<PathBuf> = None;
        let mut reveal_path: Option<PathBuf> = None;
        let mut drag_start: Option<(usize, u64)> = None;
        if self.sidebar.open_editors_open {
            let groups: Vec<(usize, Vec<u64>, usize)> = self
                .groups
                .iter()
                .enumerate()
                .map(|(index, group)| (index, group.tabs.clone(), group.active))
                .collect();
            let active_group = self.active_group;
            let multi_group = groups.len() > 1;
            let mut render_rows = |ui: &mut Ui| {
                for (group_index, ids, group_active) in &groups {
                    if multi_group {
                        let (header, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 22.0),
                            Sense::hover(),
                        );
                        let active = *group_index == active_group;
                        if active {
                            ui.painter().rect_filled(
                                Rect::from_min_max(
                                    Pos2::new(header.left() + 6.0, header.top() + 6.0),
                                    Pos2::new(header.left() + 8.0, header.bottom() - 6.0),
                                ),
                                1.0,
                                theme.accent,
                            );
                        }
                        ui.painter().text(
                            Pos2::new(header.left() + 16.0, header.center().y),
                            Align2::LEFT_CENTER,
                            format!("第 {} 组", group_index + 1),
                            style.tiny(),
                            if active { theme.text_dim } else { theme.text_faint },
                        );
                    }
                    for (pos, id) in ids.iter().enumerate() {
                        let Some(index) = self.tab_index(*id) else {
                            continue;
                        };
                        let (tab_id, title, dirty, path, preview, pinned, is_diff) = {
                            let tab = &self.tabs[index];
                            (
                                tab.id,
                                tab.title(),
                                tab.is_dirty(),
                                tab.doc.path.clone(),
                                tab.preview,
                                tab.pinned,
                                tab.diff.is_some(),
                            )
                        };
                        let resp = ui
                            .push_id(("open_editor", tab_id), |ui| {
                                ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 24.0),
                                    Sense::click_and_drag(),
                                )
                            })
                            .inner
                            .1;
                        if resp.drag_started_by(egui::PointerButton::Primary) {
                            drag_start = Some((*group_index, tab_id));
                        }
                        let rect = resp.rect;
                        let is_active = *group_index == active_group && pos == *group_active;
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 0.0, theme.hover);
                        }
                        if is_active {
                            ui.painter().rect_filled(rect, 0.0, theme.active);
                        }
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(rect.left() + 24.0, rect.center().y),
                            Vec2::splat(14.0),
                        );
                        if is_diff {
                            icons::paint(
                                &ui.painter(),
                                icon_rect,
                                Icon::GitCompare,
                                theme.accent,
                                1.2,
                            );
                        } else if let Some(p) = &path {
                            icons::paint_path_icon(
                                &ui.painter(),
                                icon_rect,
                                p,
                                false,
                                false,
                                theme.text_dim,
                            );
                        } else {
                            icons::file_glyph(
                                &ui.painter(),
                                icon_rect,
                                theme.text_dim,
                                false,
                                false,
                            );
                        }
                        let label = if preview {
                            format!("{title} (预览)")
                        } else {
                            title
                        };
                        ui.painter().text(
                            Pos2::new(rect.left() + 38.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &label,
                            style.small(),
                            if is_active { theme.text } else { theme.text_dim },
                        );
                        if pinned {
                            icons::paint(
                                &ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(rect.right() - 28.0, rect.center().y),
                                    Vec2::splat(12.0),
                                ),
                                Icon::Pinned,
                                theme.accent,
                                1.2,
                            );
                        } else if dirty {
                            ui.painter().circle_filled(
                                Pos2::new(rect.right() - 26.0, rect.center().y),
                                3.0,
                                theme.accent,
                            );
                        }
                        if resp.hovered() {
                            let close_rect = Rect::from_center_size(
                                Pos2::new(rect.right() - 13.0, rect.center().y),
                                Vec2::splat(16.0),
                            );
                            icons::paint(
                                &ui.painter(),
                                close_rect.shrink(3.0),
                                Icon::Close,
                                theme.text_dim,
                                1.2,
                            );
                        }
                        if resp.clicked()
                            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pos| {
                                Rect::from_center_size(
                                    Pos2::new(rect.right() - 13.0, rect.center().y),
                                    Vec2::splat(18.0),
                                )
                                .contains(pos)
                            })
                        {
                            close = Some((*group_index, index));
                        } else if resp.clicked() {
                            activate = Some((*group_index, tab_id));
                        }
                        let path_clone = path.clone();
                        resp.context_menu(|ui| {
                            ui.set_width(212.0);
                            if dirty
                                && Self::context_item(ui, style, Icon::Save, "保存", Some("Ctrl+S"))
                            {
                                save_idx = Some((*group_index, tab_id));
                                ui.close();
                            }
                            if Self::context_item(
                                ui,
                                style,
                                Icon::SplitHorizontal,
                                "在右侧新建分屏",
                                None,
                            ) {
                                split_id = Some((*group_index, tab_id));
                                ui.close();
                            }
                            if Self::context_item(ui, style, Icon::Close, "关闭", Some("Ctrl+W")) {
                                close = Some((*group_index, index));
                                ui.close();
                            }
                            if let Some(p) = &path_clone {
                                ui.separator();
                                if Self::context_item(ui, style, Icon::Copy, "复制路径", None) {
                                    copy_path = Some(p.clone());
                                    ui.close();
                                }
                                if Self::context_item(
                                    ui,
                                    style,
                                    Icon::FolderOpen,
                                    "在资源管理器中显示",
                                    None,
                                ) {
                                    reveal_path = Some(p.clone());
                                    ui.close();
                                }
                            }
                        });
                    }
                }
            };
            let max_h = (ui.available_height() - 120.0).max(60.0);
            let stored = self.settings.open_editors_height.clamp(24.0, max_h);
            ScrollArea::vertical()
                .max_height(stored)
                .auto_shrink([false, true])
                .id_salt("open_editors_scroll")
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    render_rows(ui);
                });
            let (split_rect, split_resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 5.0), Sense::drag());
            let split_active = split_resp.hovered() || split_resp.dragged();
            ui.painter().line_segment(
                [
                    Pos2::new(split_rect.left(), split_rect.center().y),
                    Pos2::new(split_rect.right(), split_rect.center().y),
                ],
                egui::Stroke::new(
                    if split_active { 2.0 } else { 1.0 },
                    if split_active { theme.accent } else { theme.border },
                ),
            );
            if split_active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if split_resp.dragged() {
                self.settings.open_editors_height =
                    (self.settings.open_editors_height + split_resp.drag_delta().y)
                        .clamp(24.0, max_h);
            }
            if split_resp.double_clicked() {
                self.settings.open_editors_height = 150.0;
            }
        }
        if let Some((group, id)) = drag_start {
            self.drag_payload = Some(crate::app::DragPayload::Tab { id, group });
        }
        if let Some((group, id)) = activate {
            self.focus_tab_in_group(group, id);
        }
        if let Some((group, id)) = save_idx {
            self.focus_tab_in_group(group, id);
            self.save_active();
        }
        if let Some((group, id)) = split_id {
            self.duplicate_view_in_new_group(id, group + 1);
        }
        if let Some((group, index)) = close {
            self.close_tab_in_group(group, index);
        }
        if let Some(p) = copy_path {
            ui.ctx().copy_text(p.to_string_lossy().to_string());
            self.toast("已复制路径", ToastKind::Info);
        }
        if let Some(p) = reveal_path {
            crate::workspace::tree::reveal_in_explorer(&p);
        }
    }

    // ------------------------------------------------------------------
    // Search & Project-Wide Replace Panel
    // ------------------------------------------------------------------

    fn search_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let (header, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
        ui.painter().text(
            Pos2::new(header.left() + 14.0, header.center().y),
            Align2::LEFT_CENTER,
            "全局搜索与替换",
            style.font(11.5),
            theme.text_dim,
        );

        let mut start_search = false;
        let mut do_replace_all = false;
        let mut query_changed = false;

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            if sidebar_icon_toggle(
                ui,
                &theme,
                if self.workspace.search_panel.show_replace {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                false,
                "展开/收起跨文件替换",
            ) {
                self.workspace.search_panel.show_replace =
                    !self.workspace.search_panel.show_replace;
            }
            let query_resp = ui.add_sized(
                Vec2::new(ui.available_width() - 12.0, 26.0),
                TextEdit::singleline(&mut self.workspace.search_panel.query)
                    .hint_text("在工作区中搜索…")
                    .font(style.small())
                    .margin(Vec2::new(8.0, 4.0)),
            );
            if self.workspace.search_panel.focus {
                query_resp.request_focus();
                self.workspace.search_panel.focus = false;
            }
            if query_resp.changed() {
                self.workspace.search_panel.last_typed = Some(Instant::now());
                self.workspace.search_panel.scanning = false;
                query_changed = true;
            }
            if query_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                start_search = true;
            }
        });

        if self.workspace.search_panel.show_replace {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(34.0);
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_sized(
                    Vec2::new((ui.available_width() - 38.0).max(80.0), 26.0),
                    TextEdit::singleline(&mut self.workspace.search_panel.replacement)
                        .hint_text("替换为…")
                        .font(style.small())
                        .margin(Vec2::new(8.0, 4.0)),
                );
                if sidebar_icon_toggle(
                    ui,
                    &theme,
                    Icon::ReplaceAll,
                    false,
                    "在所有匹配文件中全部替换",
                ) {
                    do_replace_all = true;
                }
            });
        }

        if query_changed {
            self.workspace.search_panel.last_query = String::new();
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(34.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut changed = false;
            if sidebar_icon_toggle(
                ui,
                &theme,
                Icon::CaseSensitive,
                self.workspace.search_panel.case_sensitive,
                "区分大小写 (Aa)",
            ) {
                self.workspace.search_panel.case_sensitive =
                    !self.workspace.search_panel.case_sensitive;
                changed = true;
            }
            if sidebar_icon_toggle(
                ui,
                &theme,
                Icon::WholeWord,
                self.workspace.search_panel.whole_word,
                "全词匹配 (W)",
            ) {
                self.workspace.search_panel.whole_word = !self.workspace.search_panel.whole_word;
                changed = true;
            }
            if sidebar_icon_toggle(
                ui,
                &theme,
                Icon::Regex,
                self.workspace.search_panel.regex,
                "正则表达式 (.*)",
            ) {
                self.workspace.search_panel.regex = !self.workspace.search_panel.regex;
                changed = true;
            }
            if sidebar_icon_toggle(
                ui,
                &theme,
                Icon::Filter,
                self.workspace.search_panel.show_options,
                "包含/排除文件过滤",
            ) {
                self.workspace.search_panel.show_options =
                    !self.workspace.search_panel.show_options;
            }
            if changed {
                self.workspace.search_panel.last_typed = Some(Instant::now());
                self.workspace.search_panel.last_query = String::new();
            }
        });

        if self.workspace.search_panel.show_options {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.add_sized(
                    Vec2::new((ui.available_width() - 20.0) / 2.0, 24.0),
                    TextEdit::singleline(&mut self.workspace.search_panel.include)
                        .hint_text("包含: *.rs")
                        .font(style.tiny()),
                );
                ui.add_sized(
                    Vec2::new((ui.available_width() - 10.0).max(80.0), 24.0),
                    TextEdit::singleline(&mut self.workspace.search_panel.exclude)
                        .hint_text("排除: *test*")
                        .font(style.tiny()),
                );
            });
        }

        if start_search {
            self.start_project_search();
        }
        if do_replace_all {
            self.replace_all_in_project();
        }

        let panel = &self.workspace.search_panel;
        let status = if let Some(err) = &panel.error {
            err.clone()
        } else if panel.scanning || self.workspace.search.is_running() {
            format!(
                "正在搜索… 已扫描 {} 个文件，{} 处结果",
                panel.scanned,
                panel.results.iter().map(|r| r.matches.len()).sum::<usize>()
            )
        } else if !panel.last_query.is_empty() {
            format!(
                "{} 个文件，共 {} 处匹配",
                panel.matched_files.max(panel.results.len()),
                panel.results.iter().map(|r| r.matches.len()).sum::<usize>()
            )
        } else {
            String::new()
        };
        if !status.is_empty() {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
            ui.painter().text(
                Pos2::new(rect.left() + 14.0, rect.center().y),
                Align2::LEFT_CENTER,
                &status,
                style.tiny(),
                if self.workspace.search_panel.error.is_some() {
                    theme.error
                } else {
                    theme.text_faint
                },
            );
        }
        ui.add_space(2.0);

        let mut open_at: Option<(PathBuf, usize)> = None;
        let mut toggle_file: Option<PathBuf> = None;
        let root = self.workspace.root.clone();
        let needle = self.workspace.search_panel.query.trim().to_string();
        let case_sensitive = self.workspace.search_panel.case_sensitive;

        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("search_results")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let mut rendered = 0usize;
                for result in &self.workspace.search_panel.results {
                    if rendered > 400 {
                        break;
                    }
                    rendered += 1;
                    let rel = root
                        .as_ref()
                        .and_then(|r| result.path.strip_prefix(r).ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| {
                            result
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default()
                        });
                    let collapsed = self.workspace.search_panel.collapsed.contains(&result.path);
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, theme.hover);
                    }
                    icons::paint(
                        &ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(rect.left() + 15.0, rect.center().y),
                            Vec2::splat(12.0),
                        ),
                        if collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        theme.text_dim,
                        1.1,
                    );
                    icons::paint_path_icon(
                        &ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(rect.left() + 31.0, rect.center().y),
                            Vec2::splat(14.0),
                        ),
                        &result.path,
                        false,
                        false,
                        theme.text_dim,
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 44.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &rel,
                        style.small(),
                        theme.text,
                    );
                    let count = result.matches.len();
                    let count_label = if result.truncated {
                        format!("{count}+")
                    } else {
                        count.to_string()
                    };
                    let pill_w = (count_label.len() as f32 * 6.5 + 10.0).max(20.0);
                    let pill_rect = Rect::from_center_size(
                        Pos2::new(rect.right() - 16.0, rect.center().y),
                        Vec2::new(pill_w, 16.0),
                    );
                    ui.painter()
                        .rect_filled(pill_rect, CornerRadius::same(8), theme.active);
                    ui.painter().text(
                        pill_rect.center(),
                        Align2::CENTER_CENTER,
                        count_label,
                        style.tiny(),
                        theme.text_dim,
                    );
                    if resp.clicked() {
                        toggle_file = Some(result.path.clone());
                    }
                    if collapsed {
                        continue;
                    }
                    for m in result.matches.iter().take(40) {
                        let (mrect, mresp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 22.0),
                            Sense::click(),
                        );
                        if mresp.hovered() {
                            ui.painter().rect_filled(mrect, 0.0, theme.hover);
                        }
                        ui.painter().text(
                            Pos2::new(mrect.left() + 32.0, mrect.center().y),
                            Align2::RIGHT_CENTER,
                            m.line.to_string(),
                            style.tiny(),
                            theme.text_faint,
                        );
                        paint_highlighted_preview(
                            ui,
                            style,
                            mrect,
                            mrect.left() + 40.0,
                            &m.preview,
                            &needle,
                            case_sensitive,
                        );
                        let mresp =
                            mresp.on_hover_text(format!("第 {} 行，第 {} 列", m.line, m.column));
                        if mresp.clicked() {
                            open_at = Some((result.path.clone(), m.byte_offset));
                        }
                    }
                }
            });
        if let Some(path) = toggle_file {
            if !self.workspace.search_panel.collapsed.remove(&path) {
                self.workspace.search_panel.collapsed.insert(path);
            }
        }
        if let Some((path, offset)) = open_at {
            self.open_file_at(path, offset);
        }
    }

    // ------------------------------------------------------------------
    // Source Control (Git SCM) Panel
    // ------------------------------------------------------------------

    fn git_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        self.git.poll_commits();
        self.git.poll_commit_files();

        let mut refresh_git = false;
        let mut stage_all = false;
        let mut unstage_all = false;
        let mut discard_all_now = false;
        let mut executed = false;
        let discard_armed = self.git.discard_confirm_armed();

        {
            let (header, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
            ui.painter().text(
                Pos2::new(header.left() + 14.0, header.center().y),
                Align2::LEFT_CENTER,
                "源代码管理 (Git)",
                style.font(11.5),
                theme.text_dim,
            );
            let mut x = header.right() - 8.0;
            let mut action = |ui: &mut Ui, icon: Icon, tip: &str, color: egui::Color32| -> bool {
                x -= 24.0;
                let r = Rect::from_center_size(
                    Pos2::new(x + 11.0, header.center().y),
                    Vec2::splat(22.0),
                );
                let resp = ui.interact(r, ui.scope_id().with(("git_act", tip)), Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(5), theme.hover);
                }
                icons::paint(
                    ui.painter(),
                    r.shrink(4.0),
                    icon,
                    if resp.hovered() { theme.text } else { color },
                    1.2,
                );
                resp.on_hover_text(tip).clicked()
            };
            if action(ui, Icon::Refresh, "刷新 Git 状态", theme.text_dim) {
                refresh_git = true;
            }
            if action(ui, Icon::Unstage, "取消暂存全部更改", theme.text_dim) {
                unstage_all = true;
            }
            if action(ui, Icon::Stage, "暂存全部更改 (git add -A)", theme.text_dim) {
                stage_all = true;
            }
            let discard_tip = if discard_armed {
                "确认丢弃全部更改（3 秒内再次点击）"
            } else {
                "丢弃全部更改"
            };
            let discard_color = if discard_armed {
                theme.git_deleted
            } else {
                theme.text_dim
            };
            if action(ui, Icon::Discard, discard_tip, discard_color) {
                if discard_armed {
                    discard_all_now = true;
                } else {
                    self.git.arm_discard_confirm();
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(3100));
                }
            }
        }

        let Some(root) = self.workspace.root.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("尚未打开工作区文件夹")
                        .font(style.small())
                        .color(theme.text_faint),
                );
            });
            return;
        };

        if refresh_git {
            self.git.refresh(&root);
        }
        if stage_all {
            self.git.stage_all(&root);
        }
        if unstage_all {
            self.git.unstage_all(&root);
        }
        if refresh_git || stage_all || unstage_all {
            executed = true;
        }

        if !self.git.snapshot.is_repo {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("当前目录不是 Git 仓库")
                        .font(style.small())
                        .color(theme.text_faint),
                );
            });
            return;
        }

        self.git.load_commits(&root);

        // Branch bar: click toggles the inline branch panel.
        let branch_panel_open = self.git.branch_panel_open;
        {
            let (bar, resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), 26.0),
                Sense::click(),
            );
            if resp.hovered() {
                ui.painter().rect_filled(bar, 0.0, theme.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(bar.left() + 18.0, bar.center().y),
                    Vec2::splat(14.0),
                ),
                Icon::GitBranch,
                theme.accent,
                1.2,
            );
            let label_painter = ui.painter().with_clip_rect(Rect::from_min_max(
                bar.min,
                Pos2::new(bar.right() - 26.0, bar.max.y),
            ));
            let branch_galley = label_painter.layout_no_wrap(
                self.git.snapshot.branch.clone(),
                style.small(),
                theme.text,
            );
            let branch_width = branch_galley.size().x;
            label_painter.galley(
                Pos2::new(bar.left() + 34.0, bar.center().y - branch_galley.size().y * 0.5),
                branch_galley,
                theme.text,
            );
            if self.git.snapshot.ahead > 0 || self.git.snapshot.behind > 0 {
                ui.painter().text(
                    Pos2::new(bar.left() + 40.0 + branch_width, bar.center().y),
                    Align2::LEFT_CENTER,
                    format!(
                        "↑{} ↓{}",
                        self.git.snapshot.ahead, self.git.snapshot.behind
                    ),
                    style.tiny(),
                    theme.text_faint,
                );
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(bar.right() - 14.0, bar.center().y),
                    Vec2::splat(12.0),
                ),
                if branch_panel_open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                theme.text_faint,
                1.2,
            );
            let resp = resp.on_hover_text("分支：切换 / 新建 / 合并 / 删除");
            if resp.clicked() {
                self.git.branch_panel_open = !self.git.branch_panel_open;
            }
        }

        let mut checkout_branch: Option<String> = None;
        let mut merge_branch: Option<String> = None;
        let mut delete_branch: Option<String> = None;
        let mut create_branch = false;

        if self.git.branch_panel_open {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.add_sized(
                    Vec2::new((ui.available_width() - 24.0).max(60.0), 24.0),
                    TextEdit::singleline(&mut self.git.branch_filter)
                        .hint_text("过滤分支")
                        .font(style.small())
                        .margin(Vec2::new(8.0, 4.0)),
                );
            });
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let btn_w = 26.0;
                let input_w = (ui.available_width() - 24.0 - btn_w - 4.0).max(60.0);
                let resp = ui.add_sized(
                    Vec2::new(input_w, 24.0),
                    TextEdit::singleline(&mut self.git.new_branch)
                        .hint_text("新建分支名称")
                        .font(style.small())
                        .margin(Vec2::new(8.0, 4.0)),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    create_branch = true;
                }
                let add_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(btn_w, 24.0));
                let add_resp = ui.interact(
                    add_rect,
                    ui.scope_id().with("git_new_branch_btn"),
                    Sense::click(),
                );
                if add_resp.hovered() {
                    ui.painter()
                        .rect_filled(add_rect, CornerRadius::same(5), theme.hover);
                }
                icons::paint(
                    ui.painter(),
                    add_rect.shrink(6.0),
                    Icon::Plus,
                    if add_resp.hovered() {
                        theme.text
                    } else {
                        theme.text_dim
                    },
                    1.2,
                );
                if add_resp.on_hover_text("创建并切换分支").clicked() {
                    create_branch = true;
                }
                ui.advance_cursor_after_rect(add_rect);
            });
            ui.add_space(3.0);

            let filter = self.git.branch_filter.trim().to_lowercase();
            let branches = self.git.snapshot.branches.clone();
            let current = self.git.snapshot.branch.clone();
            let mut branch_rows = 0usize;
            ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .id_salt("git_branch_list")
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for (index, branch) in branches.iter().enumerate() {
                        if !filter.is_empty() && !branch.to_lowercase().contains(&filter) {
                            continue;
                        }
                        branch_rows += 1;
                        let is_current = *branch == current;
                        let (row, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 22.0),
                            Sense::hover(),
                        );
                        let resp = ui.interact(
                            row,
                            ui.scope_id().with(("git_branch_row", index)),
                            Sense::click(),
                        );
                        let hovered = ui.rect_contains_pointer(row);
                        if hovered {
                            ui.painter().rect_filled(row, 0.0, theme.hover);
                        }
                        let label_painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            Pos2::new(row.left() + 30.0, row.top()),
                            Pos2::new(row.right() - 48.0, row.bottom()),
                        ));
                        label_painter.text(
                            Pos2::new(row.left() + 30.0, row.center().y),
                            Align2::LEFT_CENTER,
                            branch.as_str(),
                            style.small(),
                            if is_current { theme.accent } else { theme.text },
                        );
                        if is_current {
                            icons::paint(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(row.right() - 16.0, row.center().y),
                                    Vec2::splat(12.0),
                                ),
                                Icon::Check,
                                theme.accent,
                                1.2,
                            );
                        } else {
                            let merge_rect = Rect::from_center_size(
                                Pos2::new(row.right() - 40.0, row.center().y),
                                Vec2::splat(18.0),
                            );
                            let delete_rect = Rect::from_center_size(
                                Pos2::new(row.right() - 20.0, row.center().y),
                                Vec2::splat(18.0),
                            );
                            let merge_resp = ui.interact(
                                merge_rect,
                                ui.scope_id().with(("git_merge_branch", index)),
                                Sense::click(),
                            );
                            let delete_resp = ui.interact(
                                delete_rect,
                                ui.scope_id().with(("git_delete_branch", index)),
                                Sense::click(),
                            );
                            if hovered || merge_resp.hovered() || delete_resp.hovered() {
                                icons::paint(
                                    ui.painter(),
                                    merge_rect.shrink(3.0),
                                    Icon::GitCompare,
                                    if merge_resp.hovered() {
                                        theme.text
                                    } else {
                                        theme.text_dim
                                    },
                                    1.2,
                                );
                                icons::paint(
                                    ui.painter(),
                                    delete_rect.shrink(3.0),
                                    Icon::Trash,
                                    if delete_resp.hovered() {
                                        theme.text
                                    } else {
                                        theme.text_dim
                                    },
                                    1.2,
                                );
                            }
                            if merge_resp
                                .on_hover_text(format!("合并 {branch} 到 {current}"))
                                .clicked()
                            {
                                merge_branch = Some(branch.clone());
                            }
                            if delete_resp
                                .on_hover_text(format!("删除分支 {branch}"))
                                .clicked()
                            {
                                delete_branch = Some(branch.clone());
                            }
                        }
                        if resp.clicked() && !is_current {
                            checkout_branch = Some(branch.clone());
                        }
                    }
                });
            if branch_rows == 0 {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("没有匹配的分支")
                            .font(style.tiny())
                            .color(theme.text_faint),
                    );
                });
            }
            ui.add_space(4.0);
        } else {
            ui.add_space(4.0);
        }

        // Commit message + amend + commit button.
        let mut do_commit = false;
        let mut pick_message: Option<String> = None;
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let picker_size = 20.0;
            let resp = ui.add_sized(
                Vec2::new(
                    (ui.available_width() - 24.0 - picker_size - 6.0).max(60.0),
                    58.0,
                ),
                TextEdit::multiline(&mut self.git.commit_message)
                    .hint_text("提交说明 (Ctrl+Enter 提交)")
                    .font(style.small())
                    .desired_rows(3)
                    .margin(Vec2::new(8.0, 6.0)),
            );
            if resp.has_focus()
                && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter))
            {
                do_commit = true;
            }
            let picker_rect = Rect::from_min_size(
                Pos2::new(resp.rect.right() + 6.0, resp.rect.top()),
                Vec2::splat(picker_size),
            );
            let picker_resp = ui.interact(
                picker_rect,
                ui.scope_id().with("git_message_picker_btn"),
                Sense::click(),
            );
            if picker_resp.hovered() {
                ui.painter()
                    .rect_filled(picker_rect, CornerRadius::same(4), theme.hover);
            }
            icons::paint(
                ui.painter(),
                picker_rect.shrink(3.0),
                Icon::GitCommit,
                if self.git.message_picker_open || picker_resp.hovered() {
                    theme.accent
                } else {
                    theme.text_dim
                },
                1.2,
            );
            if picker_resp.on_hover_text("复用历史提交说明").clicked() {
                self.git.message_picker_open = !self.git.message_picker_open;
            }
        });
        if self.git.message_picker_open {
            let recent: Vec<String> = self
                .git
                .commits
                .iter()
                .take(8)
                .map(|commit| commit.subject.clone())
                .collect();
            if recent.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("暂无提交记录")
                            .font(style.tiny())
                            .color(theme.text_faint),
                    );
                });
            } else {
                ScrollArea::vertical()
                    .max_height(140.0)
                    .auto_shrink([false, true])
                    .id_salt("git_message_picker")
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for subject in &recent {
                            let (row, row_resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 20.0),
                                Sense::click(),
                            );
                            if row_resp.hovered() {
                                ui.painter().rect_filled(row, 0.0, theme.hover);
                            }
                            ui.painter()
                                .with_clip_rect(Rect::from_min_max(
                                    Pos2::new(row.left() + 16.0, row.top()),
                                    Pos2::new(row.right() - 8.0, row.bottom()),
                                ))
                                .text(
                                    Pos2::new(row.left() + 16.0, row.center().y),
                                    Align2::LEFT_CENTER,
                                    subject.as_str(),
                                    style.tiny(),
                                    theme.text_dim,
                                );
                            if row_resp.clicked() {
                                pick_message = Some(subject.clone());
                            }
                        }
                    });
            }
        }
        if let Some(subject) = pick_message {
            self.git.commit_message = subject;
            self.git.message_picker_open = false;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let (cb_rect, cb_resp) =
                ui.allocate_exact_size(Vec2::new(74.0, 24.0), Sense::click());
            let box_rect = Rect::from_center_size(
                Pos2::new(cb_rect.left() + 7.0, cb_rect.center().y),
                Vec2::splat(13.0),
            );
            if self.git.amend {
                ui.painter()
                    .rect_filled(box_rect, CornerRadius::same(3), theme.accent);
                icons::paint(
                    ui.painter(),
                    box_rect.shrink(2.0),
                    Icon::Check,
                    theme.accent_text,
                    1.2,
                );
            } else {
                ui.painter().rect_stroke(
                    box_rect,
                    CornerRadius::same(3),
                    Stroke::new(
                        1.0,
                        if cb_resp.hovered() {
                            theme.text_dim
                        } else {
                            theme.border
                        },
                    ),
                    StrokeKind::Inside,
                );
            }
            ui.painter().text(
                Pos2::new(box_rect.right() + 6.0, cb_rect.center().y),
                Align2::LEFT_CENTER,
                "Amend",
                style.tiny(),
                if self.git.amend {
                    theme.text
                } else {
                    theme.text_dim
                },
            );
            if cb_resp.clicked() {
                self.git.amend = !self.git.amend;
            }
            let btn_rect = Rect::from_min_size(
                ui.cursor().min,
                Vec2::new((ui.available_width() - 4.0).max(60.0), 24.0),
            );
            let label = if self.git.amend {
                "✓ 修订提交 (Amend)"
            } else {
                "✓ 提交更改 (Commit)"
            };
            if self.primary_button(ui, btn_rect, label, style) {
                do_commit = true;
            }
            ui.advance_cursor_after_rect(btn_rect);
        });
        ui.add_space(8.0);

        let staged: Vec<_> = self
            .git
            .snapshot
            .changes
            .iter()
            .filter(|c| c.staged)
            .cloned()
            .collect();
        let unstaged: Vec<_> = self
            .git
            .snapshot
            .changes
            .iter()
            .filter(|c| !c.staged)
            .cloned()
            .collect();
        let commits = self.git.commits.clone();

        let mut open_file = None;
        let mut open_diff: Option<(PathBuf, bool)> = None;
        let mut stage_path = None;
        let mut unstage_path = None;
        let mut discard_path = None;
        let mut group_unstage_all = false;
        let mut group_stage_all = false;
        let mut group_discard_all = false;
        let mut arm_group_discard = false;
        let mut toggle_commit: Option<String> = None;
        let mut open_commit_file: Option<(String, String)> = None;
        let mut stash_save_now = false;
        let mut stash_pop: Option<usize> = None;
        let mut stash_drop: Option<usize> = None;
        let mut undo_now = false;
        let mut arm_undo = false;

        // Split the lower panel: change sections take the remaining height, the commit
        // history keeps `settings.git_history_height` and is resizable via the separator.
        let history_open = self.git.commits_open;
        let avail_h = ui.available_height();
        let splitter_h = if history_open { 5.0 } else { 0.0 };
        let history_header_h = 24.0;
        let max_history = (avail_h - splitter_h - history_header_h - 80.0).max(80.0);
        let history_h = if history_open {
            self.settings.git_history_height.clamp(80.0, max_history)
        } else {
            0.0
        };
        let changes_h = (avail_h - splitter_h - history_header_h - history_h).max(80.0);

        ScrollArea::vertical()
            .max_height(changes_h)
            .auto_shrink(false)
            .id_salt("git_changes_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if !staged.is_empty() {
                    let count = staged.len().to_string();
                    let mut expanded = self.git.staged_open;
                    if Self::git_section_ui(
                        ui,
                        style,
                        "暂存的更改",
                        Some(&count),
                        &mut expanded,
                        &[(Icon::Unstage, "取消暂存全部更改")],
                        None,
                    ) == Some(0)
                    {
                        group_unstage_all = true;
                    }
                    self.git.staged_open = expanded;
                    if expanded {
                        for item in &staged {
                            let (row, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 24.0),
                                Sense::click(),
                            );
                            if resp.hovered() {
                                ui.painter().rect_filled(row, 0.0, theme.hover);
                            }
                            icons::paint_path_icon(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(row.left() + 20.0, row.center().y),
                                    Vec2::splat(14.0),
                                ),
                                &item.path,
                                false,
                                false,
                                theme.text_dim,
                            );
                            ui.painter().text(
                                Pos2::new(row.left() + 34.0, row.center().y),
                                Align2::LEFT_CENTER,
                                &item.rel_path,
                                style.small(),
                                theme.text,
                            );
                            if resp.hovered() {
                                let unstage_r = Rect::from_center_size(
                                    Pos2::new(row.right() - 28.0, row.center().y),
                                    Vec2::splat(18.0),
                                );
                                icons::paint(
                                    ui.painter(),
                                    unstage_r.shrink(3.0),
                                    Icon::Unstage,
                                    theme.text_dim,
                                    1.2,
                                );
                                if resp.clicked()
                                    && ui
                                        .input(|i| i.pointer.interact_pos())
                                        .is_some_and(|p| unstage_r.contains(p))
                                {
                                    unstage_path = Some(item.path.clone());
                                } else if resp.clicked() {
                                    open_diff = Some((item.path.clone(), true));
                                }
                            } else if resp.clicked() {
                                open_diff = Some((item.path.clone(), true));
                            }
                            resp.context_menu(|ui| {
                                ui.set_width(200.0);
                                if Self::context_item(ui, style, Icon::GitCompare, "查看更改", None) {
                                    open_diff = Some((item.path.clone(), true));
                                    ui.close();
                                }
                                if Self::context_item(ui, style, Icon::File, "打开文件", None) {
                                    open_file = Some(item.path.clone());
                                    ui.close();
                                }
                                ui.separator();
                                if Self::context_item(ui, style, Icon::Unstage, "取消暂存", None) {
                                    unstage_path = Some(item.path.clone());
                                    ui.close();
                                }
                                if Self::context_item(ui, style, Icon::Copy, "复制相对路径", None) {
                                    ui.ctx().copy_text(item.rel_path.clone());
                                    ui.close();
                                }
                            });
                            ui.painter().text(
                                Pos2::new(row.right() - 10.0, row.center().y),
                                Align2::RIGHT_CENTER,
                                item.status.badge(),
                                style.tiny(),
                                theme.git_added,
                            );
                        }
                    }
                }

                let count = unstaged.len().to_string();
                let mut expanded = self.git.changes_open;
                let group_actions = [
                    (Icon::Stage, "暂存全部更改"),
                    (
                        Icon::Discard,
                        if discard_armed {
                            "确认丢弃全部更改（3 秒内再次点击）"
                        } else {
                            "丢弃全部更改"
                        },
                    ),
                ];
                match Self::git_section_ui(
                    ui,
                    style,
                    "更改 (CHANGES)",
                    Some(&count),
                    &mut expanded,
                    &group_actions,
                    if discard_armed { Some(1) } else { None },
                ) {
                    Some(0) => group_stage_all = true,
                    Some(1) => {
                        if discard_armed {
                            group_discard_all = true;
                        } else {
                            arm_group_discard = true;
                        }
                    }
                    _ => {}
                }
                self.git.changes_open = expanded;
                if expanded {
                    if unstaged.is_empty() && staged.is_empty() {
                        ui.add_space(16.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("工作区干净，没有未提交的更改")
                                    .font(style.small())
                                    .color(theme.text_faint),
                            );
                        });
                    }
                    for item in &unstaged {
                        let (row, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            Sense::click(),
                        );
                        if resp.hovered() {
                            ui.painter().rect_filled(row, 0.0, theme.hover);
                        }
                        icons::paint_path_icon(
                            ui.painter(),
                            Rect::from_center_size(
                                Pos2::new(row.left() + 20.0, row.center().y),
                                Vec2::splat(14.0),
                            ),
                            &item.path,
                            false,
                            false,
                            theme.text_dim,
                        );
                        let st_color = match item.status {
                            GitFileStatus::Modified => theme.git_modified,
                            GitFileStatus::Added | GitFileStatus::Untracked => theme.git_added,
                            GitFileStatus::Deleted | GitFileStatus::Conflicted => theme.git_deleted,
                            GitFileStatus::Renamed => theme.accent,
                        };
                        ui.painter().text(
                            Pos2::new(row.left() + 34.0, row.center().y),
                            Align2::LEFT_CENTER,
                            &item.rel_path,
                            style.small(),
                            theme.text,
                        );
                        if resp.hovered() {
                            let discard_r = Rect::from_center_size(
                                Pos2::new(row.right() - 46.0, row.center().y),
                                Vec2::splat(18.0),
                            );
                            let stage_r = Rect::from_center_size(
                                Pos2::new(row.right() - 28.0, row.center().y),
                                Vec2::splat(18.0),
                            );
                            icons::paint(
                                ui.painter(),
                                discard_r.shrink(3.0),
                                Icon::Discard,
                                theme.text_dim,
                                1.2,
                            );
                            icons::paint(
                                ui.painter(),
                                stage_r.shrink(3.0),
                                Icon::Stage,
                                theme.text_dim,
                                1.2,
                            );
                            if resp.clicked()
                                && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                            {
                                if discard_r.contains(pos) {
                                    discard_path = Some(item.path.clone());
                                } else if stage_r.contains(pos) {
                                    stage_path = Some(item.path.clone());
                                } else {
                                    open_diff = Some((item.path.clone(), false));
                                }
                            }
                        } else if resp.clicked() {
                            open_diff = Some((item.path.clone(), false));
                        }
                        resp.context_menu(|ui| {
                            ui.set_width(200.0);
                            if Self::context_item(ui, style, Icon::GitCompare, "查看更改", None) {
                                open_diff = Some((item.path.clone(), false));
                                ui.close();
                            }
                            if Self::context_item(ui, style, Icon::File, "打开文件", None) {
                                open_file = Some(item.path.clone());
                                ui.close();
                            }
                            ui.separator();
                            if Self::context_item(ui, style, Icon::Stage, "暂存更改", None) {
                                stage_path = Some(item.path.clone());
                                ui.close();
                            }
                            if Self::context_item(ui, style, Icon::Discard, "丢弃更改", None) {
                                discard_path = Some(item.path.clone());
                                ui.close();
                            }
                            if Self::context_item(ui, style, Icon::Copy, "复制相对路径", None) {
                                ui.ctx().copy_text(item.rel_path.clone());
                                ui.close();
                            }
                        });
                        ui.painter().text(
                            Pos2::new(row.right() - 10.0, row.center().y),
                            Align2::RIGHT_CENTER,
                            item.status.badge(),
                            style.tiny(),
                            st_color,
                        );
                    }
                }

                // Stashes
                let stashes = self.git.snapshot.stashes.clone();
                let stash_count = stashes.len().to_string();
                let mut stashes_open = self.git.stashes_open;
                if Self::git_section_ui(
                    ui,
                    style,
                    "储藏 (STASHES)",
                    Some(&stash_count),
                    &mut stashes_open,
                    &[(Icon::Save, "储藏当前更改")],
                    None,
                ) == Some(0)
                {
                    stash_save_now = true;
                }
                self.git.stashes_open = stashes_open;
                if stashes_open {
                    if stashes.is_empty() {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new("暂无储藏")
                                    .font(style.tiny())
                                    .color(theme.text_faint),
                            );
                        });
                        ui.add_space(8.0);
                    }
                    for (index, stash) in stashes.iter().enumerate() {
                        let (row, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 22.0),
                            Sense::hover(),
                        );
                        let hovered = ui.rect_contains_pointer(row);
                        if hovered {
                            ui.painter().rect_filled(row, 0.0, theme.hover);
                        }
                        ui.painter()
                            .with_clip_rect(Rect::from_min_max(
                                Pos2::new(row.left() + 16.0, row.top()),
                                Pos2::new(row.right() - 100.0, row.bottom()),
                            ))
                            .text(
                                Pos2::new(row.left() + 16.0, row.center().y),
                                Align2::LEFT_CENTER,
                                stash.message.as_str(),
                                style.tiny(),
                                theme.text_dim,
                            );
                        let pop_rect = Rect::from_center_size(
                            Pos2::new(row.right() - 86.0, row.center().y),
                            Vec2::splat(18.0),
                        );
                        let drop_rect = Rect::from_center_size(
                            Pos2::new(row.right() - 66.0, row.center().y),
                            Vec2::splat(18.0),
                        );
                        let pop_resp = ui.interact(
                            pop_rect,
                            ui.scope_id().with(("git_stash_pop", index)),
                            Sense::click(),
                        );
                        let drop_resp = ui.interact(
                            drop_rect,
                            ui.scope_id().with(("git_stash_drop", index)),
                            Sense::click(),
                        );
                        ui.painter().text(
                            Pos2::new(row.right() - 8.0, row.center().y),
                            Align2::RIGHT_CENTER,
                            format!("stash@{{{}}}", stash.index),
                            style.tiny(),
                            theme.text_faint,
                        );
                        if hovered || pop_resp.hovered() || drop_resp.hovered() {
                            icons::paint(
                                ui.painter(),
                                pop_rect.shrink(3.0),
                                Icon::Goforward,
                                if pop_resp.hovered() {
                                    theme.text
                                } else {
                                    theme.text_dim
                                },
                                1.2,
                            );
                            icons::paint(
                                ui.painter(),
                                drop_rect.shrink(3.0),
                                Icon::Trash,
                                if drop_resp.hovered() {
                                    theme.text
                                } else {
                                    theme.text_dim
                                },
                                1.2,
                            );
                        }
                        if pop_resp
                            .on_hover_text(format!("应用 stash@{{{}}}", stash.index))
                            .clicked()
                        {
                            stash_pop = Some(stash.index);
                        }
                        if drop_resp
                            .on_hover_text(format!("删除 stash@{{{}}}", stash.index))
                            .clicked()
                        {
                            stash_drop = Some(stash.index);
                        }
                    }
                }
            });

        if history_open {
            let (split_rect, split_resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), 5.0),
                Sense::click_and_drag(),
            );
            let split_active = split_resp.hovered() || split_resp.dragged();
            if split_active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                ui.painter().line_segment(
                    [
                        Pos2::new(split_rect.left(), split_rect.center().y),
                        Pos2::new(split_rect.right(), split_rect.center().y),
                    ],
                    Stroke::new(2.0, theme.accent),
                );
            } else {
                ui.painter().line_segment(
                    [
                        Pos2::new(split_rect.left(), split_rect.center().y),
                        Pos2::new(split_rect.right(), split_rect.center().y),
                    ],
                    Stroke::new(1.0, theme.border),
                );
            }
            if split_resp.dragged() {
                let resized = self.settings.git_history_height - split_resp.drag_delta().y;
                self.settings.git_history_height = resized.clamp(80.0, max_history);
            }
            if split_resp.double_clicked() {
                self.settings.git_history_height = 210.0;
            }
        }

        // Commit history
        let history_count = commits.len().to_string();
        let undo_armed = self.git.undo_confirm_armed();
        let undo_tip = if undo_armed {
            "确认撤销上次提交（3 秒内再次点击）"
        } else {
            "撤销上次提交 (soft reset)"
        };
        let mut history_open_next = self.git.commits_open;
        if Self::git_section_ui(
            ui,
            style,
            "提交历史 (HISTORY)",
            Some(&history_count),
            &mut history_open_next,
            &[(Icon::Undo, undo_tip)],
            if undo_armed { Some(0) } else { None },
        ) == Some(0)
        {
            if undo_armed {
                undo_now = true;
            } else {
                arm_undo = true;
            }
        }
        self.git.commits_open = history_open_next;
        if history_open {
            ScrollArea::vertical()
                .max_height(history_h)
                .auto_shrink(false)
                .id_salt("git_history_scroll")
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if commits.is_empty() {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(if self.git.commits_loading() {
                                    "正在加载提交历史…"
                                } else {
                                    "暂无提交记录"
                                })
                                .font(style.tiny())
                                .color(theme.text_faint),
                            );
                        });
                        ui.add_space(8.0);
                    }
                    for (index, commit) in commits.iter().enumerate() {
                        let expanded =
                            self.git.expanded_commit.as_deref() == Some(commit.hash.as_str());
                        let (row, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 34.0),
                            Sense::hover(),
                        );
                        let resp = ui.interact(
                            row,
                            ui.scope_id().with(("git_commit_row", index)),
                            Sense::click(),
                        );
                        if resp.hovered() {
                            ui.painter().rect_filled(row, 0.0, theme.hover);
                        }
                        icons::paint(
                            ui.painter(),
                            Rect::from_center_size(
                                Pos2::new(row.left() + 12.0, row.center().y),
                                Vec2::splat(10.0),
                            ),
                            if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            theme.text_faint,
                            1.2,
                        );
                        let text_painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            Pos2::new(row.left() + 24.0, row.top()),
                            Pos2::new(row.right() - 62.0, row.bottom()),
                        ));
                        text_painter.text(
                            Pos2::new(row.left() + 24.0, row.top() + 11.0),
                            Align2::LEFT_CENTER,
                            commit.subject.as_str(),
                            style.small(),
                            theme.text,
                        );
                        text_painter.text(
                            Pos2::new(row.left() + 24.0, row.bottom() - 9.0),
                            Align2::LEFT_CENTER,
                            format!("{} · {}", commit.author, commit.time),
                            style.tiny(),
                            theme.text_faint,
                        );
                        ui.painter().text(
                            Pos2::new(row.right() - 8.0, row.center().y),
                            Align2::RIGHT_CENTER,
                            &commit.short,
                            style.tiny(),
                            theme.accent,
                        );
                        if resp.clicked() {
                            toggle_commit = Some(commit.hash.clone());
                        }
                        if !expanded {
                            continue;
                        }
                        match self.git.commit_files.get(&commit.hash) {
                            None => {
                                ui.horizontal(|ui| {
                                    ui.add_space(34.0);
                                    ui.label(
                                        egui::RichText::new("正在加载改动…")
                                            .font(style.tiny())
                                            .color(theme.text_faint),
                                    );
                                });
                            }
                            Some(files) if files.is_empty() => {
                                ui.horizontal(|ui| {
                                    ui.add_space(34.0);
                                    ui.label(
                                        egui::RichText::new("无文件改动")
                                            .font(style.tiny())
                                            .color(theme.text_faint),
                                    );
                                });
                            }
                            Some(files) => {
                                for (file_index, file) in files.iter().enumerate() {
                                    let (file_row, _) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 20.0),
                                        Sense::hover(),
                                    );
                                    let file_resp = ui.interact(
                                        file_row,
                                        ui.scope_id()
                                            .with(("git_commit_file", index, file_index)),
                                        Sense::click(),
                                    );
                                    if file_resp.hovered() {
                                        ui.painter().rect_filled(file_row, 0.0, theme.hover);
                                    }
                                    let file_color = match file.status {
                                        crate::git::CommitFileStatus::Added => theme.git_added,
                                        crate::git::CommitFileStatus::Modified => {
                                            theme.git_modified
                                        }
                                        crate::git::CommitFileStatus::Deleted => {
                                            theme.git_deleted
                                        }
                                        crate::git::CommitFileStatus::Renamed => theme.accent,
                                    };
                                    ui.painter().text(
                                        Pos2::new(file_row.left() + 34.0, file_row.center().y),
                                        Align2::LEFT_CENTER,
                                        file.status.badge(),
                                        style.tiny(),
                                        file_color,
                                    );
                                    ui.painter()
                                        .with_clip_rect(file_row)
                                        .text(
                                            Pos2::new(
                                                file_row.left() + 48.0,
                                                file_row.center().y,
                                            ),
                                            Align2::LEFT_CENTER,
                                            file.rel_path.as_str(),
                                            style.tiny(),
                                            theme.text_dim,
                                        );
                                    if file_resp.clicked() {
                                        // A deleted file no longer exists in that commit
                                        // tree, so read the version it had just before.
                                        let deleted =
                                            file.status == crate::git::CommitFileStatus::Deleted;
                                        let revision = if deleted {
                                            format!("{}^", commit.hash)
                                        } else {
                                            commit.hash.clone()
                                        };
                                        open_commit_file = Some((revision, file.rel_path.clone()));
                                    }
                                }
                            }
                        }
                        ui.add_space(2.0);
                    }
                });
        }

        if let Some(hash) = toggle_commit {
            if self.git.expanded_commit.as_deref() == Some(hash.as_str()) {
                self.git.expanded_commit = None;
            } else {
                self.git.expanded_commit = Some(hash);
            }
        }
        if let Some(hash) = self.git.expanded_commit.clone()
            && !self.git.commit_files.contains_key(&hash)
        {
            self.git.load_commit_files(&root, &hash);
        }

        if let Some((hash, rel)) = open_commit_file {
            self.open_git_revision_file(&hash, &rel);
        }
        if let Some(path) = stage_path {
            self.git.stage_file(&root, &path);
            executed = true;
        }
        if let Some(path) = unstage_path {
            self.git.unstage_file(&root, &path);
            executed = true;
        }
        if let Some(path) = discard_path {
            self.git.discard_file(&root, &path);
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|t| t.doc.path.as_deref() == Some(path.as_path()))
            {
                let _ = tab.doc.reload();
            }
            executed = true;
        }
        if let Some((path, staged)) = open_diff {
            self.open_file_diff(path, staged);
        }
        if let Some(path) = open_file {
            self.open_file(path);
        }
        if let Some(branch) = checkout_branch {
            match self.git.checkout_branch(&root, &branch) {
                Ok(()) => self.toast(format!("已切换到分支 {branch}"), ToastKind::Success),
                Err(err) => self.toast(format!("切换分支失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if create_branch {
            let name = self.git.new_branch.trim().to_string();
            match self.git.create_branch(&root, &name) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("新建分支失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if let Some(branch) = merge_branch {
            match self.git.merge_branch(&root, &branch) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("合并失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if let Some(branch) = delete_branch {
            match self.git.delete_branch(&root, &branch) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("删除分支失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if stash_save_now {
            match self.git.stash_save(&root) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("储藏失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if let Some(index) = stash_pop {
            match self.git.stash_pop(&root, index) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("应用储藏失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if let Some(index) = stash_drop {
            match self.git.stash_drop(&root, index) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("删除储藏失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if arm_undo {
            self.git.arm_undo_confirm();
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(3100));
        }
        if undo_now {
            self.git.clear_undo_confirm();
            match self.git.undo_last_commit(&root) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(format!("撤销提交失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if group_unstage_all {
            self.git.unstage_all(&root);
            executed = true;
        }
        if group_stage_all {
            self.git.stage_all(&root);
            executed = true;
        }
        if arm_group_discard {
            self.git.arm_discard_confirm();
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(3100));
        }
        if discard_all_now || group_discard_all {
            self.git.clear_discard_confirm();
            let touched: Vec<PathBuf> = self
                .git
                .snapshot
                .changes
                .iter()
                .map(|c| c.path.clone())
                .collect();
            match self.git.discard_all(&root) {
                Ok(()) => {
                    for tab in &mut self.tabs {
                        if let Some(path) = tab.doc.path.clone()
                            && touched.contains(&path)
                        {
                            let _ = tab.doc.reload();
                        }
                    }
                    self.toast("已丢弃全部更改", ToastKind::Success);
                }
                Err(err) => self.toast(format!("丢弃失败: {err}"), ToastKind::Warning),
            }
            executed = true;
        }
        if do_commit {
            match self.git.commit(&root) {
                Ok(msg) => self.toast(msg, ToastKind::Success),
                Err(err) => self.toast(err, ToastKind::Warning),
            }
            executed = true;
        }

        if executed || self.git.commits_loading() || self.git.commit_files_loading() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// SCM section header: chevron + title + count badge + right-side icon actions.
    /// Returns the index of the clicked action, if any.
    fn git_section_ui(
        ui: &mut Ui,
        style: &UiStyle,
        title: &str,
        badge: Option<&str>,
        expanded: &mut bool,
        actions: &[(Icon, &str)],
        emphasis: Option<usize>,
    ) -> Option<usize> {
        let theme = style.theme;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
        let bg = if resp.hovered() {
            theme.hover
        } else {
            theme.panel_bg.gamma_multiply(0.7)
        };
        ui.painter().rect_filled(rect, 0.0, bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.top()),
                Pos2::new(rect.right(), rect.top()),
            ],
            Stroke::new(1.0, theme.border),
        );
        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                Pos2::new(rect.left() + 12.0, rect.center().y),
                Vec2::splat(12.0),
            ),
            if *expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            theme.text_dim,
            1.2,
        );
        let title_painter = ui.painter().with_clip_rect(rect);
        let title_galley =
            title_painter.layout_no_wrap(title.to_string(), style.font(10.5), theme.text_dim);
        let title_width = title_galley.size().x;
        title_painter.galley(
            Pos2::new(
                rect.left() + 24.0,
                rect.center().y - title_galley.size().y * 0.5,
            ),
            title_galley,
            theme.text_dim,
        );
        if let Some(badge) = badge {
            ui.painter().text(
                Pos2::new(rect.left() + 30.0 + title_width, rect.center().y),
                Align2::LEFT_CENTER,
                badge,
                style.tiny(),
                theme.text_faint,
            );
        }
        let mut clicked = None;
        let mut x = rect.right() - 4.0;
        for (index, (icon, tip)) in actions.iter().enumerate().rev() {
            x -= 20.0;
            let action_rect =
                Rect::from_center_size(Pos2::new(x + 10.0, rect.center().y), Vec2::splat(18.0));
            let action_resp = ui.interact(
                action_rect,
                ui.scope_id().with((title, "hdr_action", index)),
                Sense::click(),
            );
            if action_resp.hovered() {
                ui.painter()
                    .rect_filled(action_rect, CornerRadius::same(4), theme.hover);
            }
            let color = if emphasis == Some(index) {
                theme.git_deleted
            } else if action_resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            };
            icons::paint(ui.painter(), action_rect.shrink(3.5), *icon, color, 1.2);
            if action_resp.on_hover_text(*tip).clicked() {
                clicked = Some(index);
            }
        }
        if clicked.is_some() {
            return clicked;
        }
        if resp.clicked() {
            *expanded = !*expanded;
        }
        None
    }

    // ------------------------------------------------------------------
    // Symbol Outline Panel
    // ------------------------------------------------------------------

    fn outline_panel_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        let (header, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
        ui.painter().text(
            Pos2::new(header.left() + 14.0, header.center().y),
            Align2::LEFT_CENTER,
            "代码大纲 (Outline)",
            style.font(11.5),
            theme.text_dim,
        );
        self.outline_list_ui(ui, style, "dedicated_outline_scroll");
    }

    fn outline_list_ui(&mut self, ui: &mut Ui, style: &UiStyle, scroll_id: &str) {
        let theme = style.theme;
        let editor = self.active_editor();
        let editor_id = editor.map(|e| e.id);
        let symbols = editor.map(|e| e.symbols.clone()).unwrap_or_default();
        if self.sidebar.outline_editor != editor_id {
            if let Some(previous) = self.sidebar.outline_editor {
                let collapsed = std::mem::take(&mut self.sidebar.outline_collapsed);
                self.sidebar.outline_saved.insert(previous, collapsed);
            }
            self.sidebar.outline_editor = editor_id;
            self.sidebar.outline_collapsed = editor_id
                .and_then(|id| self.sidebar.outline_saved.get(&id).cloned())
                .unwrap_or_default();
        }
        if symbols.is_empty() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new("当前文档暂无符号大纲")
                        .font(style.tiny())
                        .color(theme.text_faint),
                );
            });
            return;
        }
        let caret_line = self
            .active_editor()
            .map(|e| e.doc.char_to_line(e.caret))
            .unwrap_or(0);
        let mut jump_to = None;
        let mut toggle: Option<usize> = None;
        let pointer_pos = ui
            .ctx()
            .input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));

        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt(scroll_id)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let mut index = 0usize;
                while index < symbols.len() {
                    let sym = &symbols[index];
                    let child_end = outline_child_end(&symbols, index);
                    let has_children = child_end > index + 1;
                    let collapsed = has_children
                        && self.sidebar.outline_collapsed.contains(&sym.byte_offset);
                    let is_current =
                        caret_line >= sym.start_line && caret_line <= sym.end_line;
                    let (row, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 23.0),
                        Sense::click(),
                    );
                    if resp.hovered() {
                        ui.painter().rect_filled(row, 0.0, theme.hover);
                    } else if is_current {
                        ui.painter()
                            .rect_filled(row, 0.0, theme.active.gamma_multiply(0.55));
                    }
                    let indent = 10.0 + sym.depth as f32 * 14.0;
                    let chevron_rect = Rect::from_center_size(
                        Pos2::new(row.left() + indent + 5.0, row.center().y),
                        Vec2::splat(14.0),
                    );
                    if has_children {
                        let chevron = if collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        };
                        let chevron_hovered =
                            pointer_pos.is_some_and(|pos| chevron_rect.contains(pos));
                        icons::paint(
                            &ui.painter(),
                            chevron_rect.shrink(2.0),
                            chevron,
                            if chevron_hovered {
                                theme.text
                            } else {
                                theme.text_faint
                            },
                            1.2,
                        );
                    }
                    icons::paint(
                        &ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(row.left() + indent + 19.0, row.center().y),
                            Vec2::splat(14.0),
                        ),
                        sym.kind.icon(),
                        theme.accent,
                        1.2,
                    );
                    ui.painter().text(
                        Pos2::new(row.left() + indent + 32.0, row.center().y),
                        Align2::LEFT_CENTER,
                        &sym.name,
                        style.small(),
                        if is_current {
                            theme.text
                        } else {
                            theme.text_dim
                        },
                    );
                    ui.painter().text(
                        Pos2::new(row.right() - 10.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        format!(":{}", sym.start_line + 1),
                        style.tiny(),
                        theme.text_faint,
                    );
                    let on_chevron = has_children
                        && pointer_pos.is_some_and(|pos| chevron_rect.contains(pos));
                    if resp.clicked() {
                        if on_chevron {
                            toggle = Some(sym.byte_offset);
                        } else {
                            jump_to = Some(sym.byte_offset);
                        }
                    }
                    if collapsed {
                        index = child_end;
                    } else {
                        index += 1;
                    }
                }
            });

        if let Some(offset) = toggle {
            if !self.sidebar.outline_collapsed.remove(&offset) {
                self.sidebar.outline_collapsed.insert(offset);
            }
        }
        if let Some(byte_offset) = jump_to
            && let Some(editor) = self.tabs.get_mut(self.active)
        {
            let ch = editor.doc.byte_to_char(byte_offset);
            editor.caret = ch;
            editor.anchor = ch;
            editor.ensure_visible = true;
            editor.focus_requested = true;
        }
    }

}

/// Index one past the last descendant of `index` (children are contiguous).
fn outline_child_end(symbols: &[crate::lsp::OutlineSymbol], index: usize) -> usize {
    let Some(root) = symbols.get(index) else {
        return symbols.len();
    };
    let depth = root.depth;
    let mut end = index + 1;
    while end < symbols.len() && symbols[end].depth > depth {
        end += 1;
    }
    end
}

fn sidebar_icon_toggle(
    ui: &mut Ui,
    theme: &crate::theme::Theme,
    icon: Icon,
    active: bool,
    tip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if active {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            theme.accent.gamma_multiply(0.22),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, theme.accent),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), theme.hover);
    }
    icons::paint(
        &ui.painter(),
        rect.shrink(4.0),
        icon,
        if active {
            theme.accent
        } else if resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        },
        1.2,
    );
    resp.on_hover_text(tip).clicked()
}

fn paint_highlighted_preview(
    ui: &Ui,
    style: &UiStyle,
    row_rect: Rect,
    start_x: f32,
    preview: &str,
    needle: &str,
    case_sensitive: bool,
) {
    let theme = style.theme;
    let painter = ui.painter().with_clip_rect(row_rect);
    let font = style.tiny();
    if needle.is_empty() {
        painter.text(
            Pos2::new(start_x, row_rect.center().y),
            Align2::LEFT_CENTER,
            preview,
            font,
            theme.text_dim,
        );
        return;
    }

    let match_range = if case_sensitive {
        preview.find(needle).map(|s| (s, s + needle.len()))
    } else {
        preview
            .to_lowercase()
            .find(&needle.to_lowercase())
            .and_then(|s| {
                preview
                    .get(s..s + needle.len())
                    .map(|_| (s, s + needle.len()))
            })
    };

    let Some((m_start, m_end)) = match_range else {
        painter.text(
            Pos2::new(start_x, row_rect.center().y),
            Align2::LEFT_CENTER,
            preview,
            font,
            theme.text_dim,
        );
        return;
    };

    let prefix = &preview[..m_start];
    let matched = &preview[m_start..m_end];
    let suffix = &preview[m_end..];

    let measure = |s: &str| -> f32 {
        if s.is_empty() {
            return 0.0;
        }
        ui.ctx().fonts_mut(|fonts| {
            fonts
                .layout_job(LayoutJob::simple_singleline(
                    s.to_string(),
                    font.clone(),
                    theme.text,
                ))
                .rect
                .width()
        })
    };

    let mut x = start_x;
    if !prefix.is_empty() {
        painter.text(
            Pos2::new(x, row_rect.center().y),
            Align2::LEFT_CENTER,
            prefix,
            font.clone(),
            theme.text_dim,
        );
        x += measure(prefix);
    }
    let mw = measure(matched);
    let hl_rect = Rect::from_min_size(
        Pos2::new(x - 1.0, row_rect.center().y - 7.0),
        Vec2::new(mw + 2.0, 14.0),
    );
    painter.rect_filled(hl_rect, CornerRadius::same(2), theme.find_match);
    painter.text(
        Pos2::new(x, row_rect.center().y),
        Align2::LEFT_CENTER,
        matched,
        font.clone(),
        theme.accent,
    );
    x += mw;
    if !suffix.is_empty() {
        painter.text(
            Pos2::new(x, row_rect.center().y),
            Align2::LEFT_CENTER,
            suffix,
            font,
            theme.text_dim,
        );
    }
}

enum TreeContext {
    OpenPinned(PathBuf),
    OpenMarkdownPreview(PathBuf),
    OpenToSide(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    OpenInTerminal(PathBuf),
    SearchInFolder(PathBuf),
    Cut(PathBuf),
    Copy(PathBuf),
    Paste(PathBuf),
    Duplicate(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    CopyPath(PathBuf),
    CopyRelativePath(PathBuf),
    Reveal(PathBuf),
    Refresh,
    CollapseAll,
}

#[cfg(test)]
mod tests {
    use super::outline_child_end;
    use crate::lsp::{OutlineSymbol, SymbolKind};

    fn sym(name: &str, line: usize, depth: usize) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_string(),
            detail: String::new(),
            kind: SymbolKind::Function,
            start_line: line,
            end_line: line,
            byte_offset: line * 10,
            depth,
        }
    }

    #[test]
    fn outline_child_end_groups_contiguous_descendants() {
        let symbols = vec![
            sym("a", 0, 0),
            sym("b", 1, 1),
            sym("c", 2, 2),
            sym("d", 3, 1),
            sym("e", 4, 0),
        ];
        assert_eq!(outline_child_end(&symbols, 0), 4);
        assert_eq!(outline_child_end(&symbols, 1), 3);
        assert_eq!(outline_child_end(&symbols, 2), 3);
        assert_eq!(outline_child_end(&symbols, 3), 4);
        assert_eq!(outline_child_end(&symbols, 4), 5);
    }
}
