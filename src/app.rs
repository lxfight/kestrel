//! Application state, file/tab lifecycle, Git/LSP orchestration, command dispatch, and background work.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::FontFamily;

use crate::commands::{Command, Shortcut};
use crate::devshot;
use crate::doc::{Document, SaveOutcome};
use crate::editor::{Editor, EditorStyle};
use crate::fuzzy;
use crate::git::{GitFileStatus, GitService};
use crate::highlight::Language;
use crate::lsp::OutlineSymbol;
use crate::plugin::{Event, PluginHost};
use crate::settings::{Session, Settings, TabSession, UntitledSession};
use crate::terminal::ShellKind;
use crate::theme::{Theme, ThemeKind};
use crate::ui::UiStyle;
use crate::workspace::{
    FileIndex, FileTree, FsWatcher, ProjectSearch, SearchEvent, SearchQuery, SearchResult,
};

pub const APP_NAME: &str = "Kestrel";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const LANGUAGES: &[Language] = &[
    Language::Rust,
    Language::Python,
    Language::JavaScript,
    Language::TypeScript,
    Language::Tsx,
    Language::Json,
    Language::Toml,
    Language::C,
    Language::Cpp,
    Language::CSharp,
    Language::Go,
    Language::Java,
    Language::Html,
    Language::Css,
    Language::Bash,
    Language::Yaml,
    Language::Markdown,
    Language::Plain,
];

/// Atomic-save temp files are ours; ignore their watcher events.
fn is_save_temp(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "kestrel-tmp")
}

fn language_from_name(name: &str) -> Language {
    LANGUAGES
        .iter()
        .copied()
        .find(|language| language.name() == name)
        .unwrap_or(Language::Plain)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarMode {
    Explorer,
    Search,
    Git,
    Outline,
    Plugins,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeClipboardKind {
    Copy,
    Cut,
}

#[derive(Clone, Debug)]
pub struct TreeClipboard {
    pub path: PathBuf,
    pub kind: TreeClipboardKind,
}

pub struct SidebarUi {
    pub visible: bool,
    pub mode: SidebarMode,
    pub width: f32,
    pub selected: Option<PathBuf>,
    pub editing: Option<TreeEdit>,
    pub clipboard: Option<TreeClipboard>,
    pub open_editors_open: bool,
    pub outline_open: bool,
    /// Collapsed outline nodes, keyed by symbol byte offset.
    pub outline_collapsed: std::collections::HashSet<usize>,
    pub outline_editor: Option<u64>,
    pub outline_saved: std::collections::HashMap<u64, std::collections::HashSet<usize>>,
    pub plugin_search: String,
    pub plugin_search_focus: bool,
    pub plugin_installed_open: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TreeEditKind {
    NewFile,
    NewFolder,
    Rename,
}

pub struct TreeEdit {
    pub kind: TreeEditKind,
    pub parent: PathBuf,
    pub original: Option<PathBuf>,
    pub name: String,
    pub focus: bool,
}

#[derive(Default)]
pub struct SearchPanelState {
    pub query: String,
    pub replacement: String,
    pub show_replace: bool,
    pub include: String,
    pub exclude: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub show_options: bool,
    pub results: Vec<SearchResult>,
    pub scanning: bool,
    pub scanned: usize,
    pub matched_files: usize,
    pub error: Option<String>,
    pub last_query: String,
    pub last_typed: Option<Instant>,
    pub focus: bool,
    pub collapsed: std::collections::HashSet<PathBuf>,
}


pub struct WorkspaceState {
    pub root: Option<PathBuf>,
    pub tree: FileTree,
    pub index: FileIndex,
    pub index_version: u64,
    pub search: ProjectSearch,
    pub watcher: Option<FsWatcher>,
    pub search_panel: SearchPanelState,
    pub pending_search: bool,
}

impl WorkspaceState {
    fn new() -> Self {
        Self {
            root: None,
            tree: FileTree::new(),
            index: FileIndex::new(),
            index_version: 0,
            search: ProjectSearch::new(),
            watcher: None,
            search_panel: SearchPanelState::default(),
            pending_search: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteMode {
    Commands,
    QuickOpen,
    GotoLine,
    Language,
    Symbols,
    Branches,
}

#[derive(Clone)]
pub enum PaletteItem {
    Command(Command),
    PluginCommand(String),
    File(PathBuf),
    Language(Language),
    Symbol(OutlineSymbol),
    Branch(String),
}

pub struct PaletteState {
    pub open: bool,
    pub mode: PaletteMode,
    pub query: String,
    pub selected: usize,
    pub window_start: usize,
    pub blink_base: f64,
    pub focus: bool,
    pub items: Vec<PaletteItem>,
    pub filtered: Vec<usize>,
    pub needs_filter: bool,
    pub list_version: u64,
}

impl PaletteState {
    fn new() -> Self {
        Self {
            open: false,
            mode: PaletteMode::Commands,
            query: String::new(),
            selected: 0,
            window_start: 0,
            blink_base: 0.0,
            focus: false,
            items: Vec::new(),
            filtered: Vec::new(),
            needs_filter: false,
            list_version: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BottomTab {
    Terminal,
    Problems,
    Output,
}

pub struct BottomPanel {
    pub visible: bool,
    pub tab: BottomTab,
    pub height: f32,
    pub terminals: Vec<crate::terminal::TerminalSession>,
    pub active_terminal: usize,
    pub terminal_size: (u16, u16),
    pub terminal_focus: bool,
}

impl BottomPanel {
    fn new() -> Self {
        Self {
            visible: false,
            tab: BottomTab::Terminal,
            height: 240.0,
            terminals: Vec::new(),
            active_terminal: 0,
            terminal_size: (0, 0),
            terminal_focus: false,
        }
    }
}

pub enum Dialog {
    About,
    Shortcuts,
    Settings,
    ConfirmClose,
    ConfirmCloseAll,
    ConfirmCloseTab(usize, usize),
    ConfirmDelete(PathBuf),
    ConfirmOpenFolder(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub enum DragPayload {
    /// Stable editor id plus the group view it was dragged from.
    Tab { id: u64, group: usize },
    ExplorerPath(PathBuf),
}

/// One editor group: a vertical pane with its own tab row. Groups are laid out
/// left-to-right; `weight` is the group's share of the available width.
pub struct EditorGroup {
    pub tabs: Vec<u64>,
    pub active: usize,
    pub weight: f32,
    pub tab_scroll: f32,
}

impl EditorGroup {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            weight: 1.0,
            tab_scroll: 0.0,
        }
    }

    pub fn active_id(&self) -> Option<u64> {
        self.tabs.get(self.active).copied()
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|&tab| tab == id)
    }
}

pub const GROUP_GAP: f32 = 5.0;
pub const TABBAR_H: f32 = 34.0;

fn with_alpha(color: egui::Color32, alpha: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        color.r(),
        color.g(),
        color.b(),
        (alpha.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn lerp_rect(from: egui::Rect, to: egui::Rect, t: f32) -> egui::Rect {
    let lerp = |a: f32, b: f32| a + (b - a) * t.clamp(0.0, 1.0);
    egui::Rect::from_min_max(
        egui::pos2(lerp(from.left(), to.left()), lerp(from.top(), to.top())),
        egui::pos2(lerp(from.right(), to.right()), lerp(from.bottom(), to.bottom())),
    )
}

/// Where a dragged tab/file would land.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DropSpot {
    Center(usize),
    Left(usize),
    Right(usize),
}

/// Smoothly animated split preview shown while dragging.
pub struct DropPreview {
    pub rect: egui::Rect,
    pub alpha: f32,
    pub hint: String,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub created: Instant,
}

pub struct KestrelApp {
    pub settings: Settings,
    pub theme: Theme,
    pub applied_theme: ThemeKind,
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,

    pub tabs: Vec<Editor>,
    pub active: usize,
    pub groups: Vec<EditorGroup>,
    pub active_group: usize,
    pub group_rects: Vec<egui::Rect>,
    pub drop_preview: Option<DropPreview>,
    next_id: u64,
    next_untitled: usize,

    pub workspace: WorkspaceState,
    pub git: GitService,
    pub plugins: PluginHost,
    pub custom_shortcuts: std::collections::HashMap<Command, Shortcut>,
    pub pending_plugin_commands: Vec<Command>,
    pub sidebar: SidebarUi,
    pub palette: PaletteState,
    pub bottom: BottomPanel,
    pub dialog: Option<Dialog>,
    pub settings_search: String,
    pub toasts: Vec<Toast>,
    pub drag_payload: Option<DragPayload>,
    pub editor_area_rect: Option<egui::Rect>,

    pub close_confirmed: bool,
    pub window_focused: bool,
    pub last_title: String,
    pub last_autosave: Instant,
    pub last_reload_check: Instant,
    /// Paths already announced as reloaded / as conflicting (watcher fires in bursts).
    pub reload_notified: std::collections::HashSet<PathBuf>,
    pub conflict_notified: std::collections::HashSet<PathBuf>,
    pub autosave_notice: Option<Instant>,
    session_fingerprint: u64,
    session_pending_since: Option<Instant>,
    pub frame: u64,
    pub shot_frame: u64,
    pub dev_ui: Option<String>,
}

impl KestrelApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut settings = Settings::load();
        if let Ok(theme) = std::env::var("KESTREL_THEME") {
            settings.theme = theme;
        }
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        cc.egui_ctx
            .set_zoom_factor(settings.ui_zoom.clamp(0.7, 1.8));
        let font_set = crate::fonts::install(&cc.egui_ctx);
        let theme = Theme::by_kind(ThemeKind::from_str(&settings.theme));
        theme.apply(&cc.egui_ctx);
        let plugin_started = std::time::Instant::now();
        let plugins = PluginHost::new(&settings);
        let custom_shortcuts = build_custom_shortcuts(&settings);

        let term_h = settings.terminal_height.max(120.0);
        let mut bottom = BottomPanel::new();
        bottom.height = term_h;

        let mut app = Self {
            sidebar: SidebarUi {
                visible: true,
                mode: SidebarMode::Explorer,
                width: settings.sidebar_width.max(180.0),
                selected: None,
                editing: None,
                clipboard: None,
                open_editors_open: true,
                outline_open: true,
                outline_collapsed: std::collections::HashSet::new(),
                outline_editor: None,
                outline_saved: std::collections::HashMap::new(),
                plugin_search: String::new(),
                plugin_search_focus: false,
                plugin_installed_open: true,
            },
            settings,
            theme,
            applied_theme: theme.kind,
            ui_family: font_set.ui_family,
            ui_bold_family: font_set.ui_bold_family,
            mono_family: font_set.mono_family,
            tabs: Vec::new(),
            active: 0,
            groups: vec![EditorGroup::new()],
            active_group: 0,
            group_rects: Vec::new(),
            drop_preview: None,
            next_id: 1,
            next_untitled: 0,
            workspace: WorkspaceState::new(),
            git: GitService::new(),
            plugins,
            custom_shortcuts,
            pending_plugin_commands: Vec::new(),
            palette: PaletteState::new(),
            bottom,
            dialog: None,
            settings_search: String::new(),
            toasts: Vec::new(),
            drag_payload: None,
            editor_area_rect: None,
            close_confirmed: false,
            window_focused: true,
            last_title: String::new(),
            last_autosave: Instant::now(),
            last_reload_check: Instant::now(),
            reload_notified: std::collections::HashSet::new(),
            conflict_notified: std::collections::HashSet::new(),
            autosave_notice: None,
            session_fingerprint: 0,
            session_pending_since: None,
            frame: 0,
            shot_frame: 0,
            dev_ui: None,
        };
        app.apply_dev_env();
        if app.workspace.root.is_none() && app.tabs.is_empty() && app.dev_ui.is_none() {
            app.restore_session();
        }
        app.load_plugins();
        app.apply_dev_ui();
        if std::env::var_os("KESTREL_TRACE").is_some() {
            app.plugins.trace_log(format!(
                "启动插件阶段 {:.2}ms",
                plugin_started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        app
    }

    // ------------------------------------------------------------------
    // Session / dev hooks
    // ------------------------------------------------------------------

    fn restore_session(&mut self) {
        let session = Session::load();
        if let Some(folder) = session.folder {
            let path = PathBuf::from(folder);
            if path.is_dir() {
                self.open_folder_quiet(path);
            }
        }
        for tab in session.tabs {
            let path = PathBuf::from(tab.path);
            if path.is_file() {
                self.open_file_quiet(path, Some((tab.caret, tab.scroll)));
            }
        }
        for entry in session.untitled {
            let text = match entry.text {
                Some(text) => text,
                None => {
                    let Some(path) = entry.path else { continue };
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    text
                }
            };
            let mut doc = Document::from_text(&text);
            if !text.is_empty() {
                doc.mark_dirty();
            }
            self.next_untitled += 1;
            let id = self.next_id;
            self.next_id += 1;
            let mut editor = Editor::new(doc, id, self.next_untitled);
            editor.set_language(language_from_name(&entry.language));
            editor.caret = entry.caret.min(editor.doc.len_chars());
            editor.anchor = editor.caret;
            editor.ensure_visible = false;
            self.tabs.push(editor);
            self.attach_editor_to_active_group(id);
        }
        if !self.tabs.is_empty() {
            let active = session.active.min(self.tabs.len() - 1);
            self.active = active;
            self.active_group = 0;
            self.ensure_groups();
            self.groups[0].active = self
                .groups
                .first()
                .and_then(|group| group.index_of(self.tabs[active].id))
                .unwrap_or(0);
            if let Some(editor) = self.tabs.get_mut(active) {
                editor.focus_requested = true;
            }
        }
    }

    fn apply_dev_env(&mut self) {
        if let Ok(path) = std::env::var("KESTREL_OPEN") {
            let path = PathBuf::from(path);
            if path.is_dir() {
                self.open_folder_quiet(path);
            } else if path.is_file() {
                let parent = path.parent().map(|p| p.to_path_buf());
                if let Some(parent) = parent {
                    self.open_folder_quiet(parent);
                }
                self.open_file_quiet(path, None);
            }
        }
        self.dev_ui = std::env::var("KESTREL_UI").ok();
    }

    fn apply_dev_ui(&mut self) {
        // 先打开文件再注入 UI 状态：split / md_split / settings 等状态依赖已有编辑器。
        // KESTREL_DEV_OPEN 不依赖 KESTREL_UI，单独截图状态也要生效。
        if let Ok(path) = std::env::var("KESTREL_DEV_OPEN") {
            self.open_file(PathBuf::from(path));
        }
        let Some(ui) = self.dev_ui.clone() else {
            return;
        };

        match ui.as_str() {
            "palette" => self.open_palette(PaletteMode::Commands),
            "plugins" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Plugins;
                self.sidebar.plugin_search_focus = true;
            }
            "quickopen" => self.open_palette(PaletteMode::QuickOpen),
            "symbols" => self.open_palette(PaletteMode::Symbols),
            "find" => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.open_find(false);
                    if let Ok(query) = std::env::var("KESTREL_DEV_QUERY") {
                        tab.find.query = query;
                        tab.recompute_find();
                        tab.find_next(true);
                    }
                }
            }
            "replace" => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.open_find(true);
                    if let Ok(query) = std::env::var("KESTREL_DEV_QUERY") {
                        tab.find.query = query;
                        tab.recompute_find();
                        tab.find_next(true);
                    }
                }
            }
            "search" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Search;
                self.workspace.search_panel.query = "fn ".to_string();
                self.workspace.search_panel.show_replace = true;
                self.workspace.search_panel.last_typed = Some(Instant::now());
                self.workspace.pending_search = true;
            }
            "git" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Git;
            }
            "outline" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Outline;
            }
            "settings" => {
                self.open_settings();
            }
            "terminal" => self.toggle_terminal(),
            "output" => {
                self.bottom.visible = true;
                self.bottom.tab = BottomTab::Output;
            }
            "problems" => {
                self.bottom.visible = true;
                self.bottom.tab = BottomTab::Problems;
            }
            "split" => {
                if self.tabs.len() < 2 {
                    let extra = std::env::var("KESTREL_DEV_SPLIT_OPEN")
                        .map(PathBuf::from)
                        .ok()
                        .filter(|path| path.is_file())
                        .or_else(|| {
                            let root = self
                                .workspace
                                .root
                                .clone()
                                .or_else(|| std::env::current_dir().ok())?;
                            ["README.md", "Cargo.toml", "src/main.rs", "src/app.rs"]
                                .iter()
                                .map(|name| root.join(name))
                                .find(|path| path.is_file())
                        });
                    if let Some(path) = extra {
                        self.open_file(path);
                    }
                }
                if self.tabs.len() > 1 {
                    let source = self
                        .group_of(self.tabs[0].id)
                        .unwrap_or(0);
                    let move_id = self.groups[source]
                        .tabs
                        .last()
                        .copied()
                        .unwrap_or(self.tabs[0].id);
                    self.active_group = source;
                    self.split_tab_to_new_group(move_id, source, source + 1);
                }
            }
            "md_preview" | "md_split" => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.preview = false;
                    tab.md_mode = if ui == "md_preview" {
                        crate::markdown::MarkdownViewMode::Preview
                    } else {
                        crate::markdown::MarkdownViewMode::Split
                    };
                }
            }
            "about" => self.dialog = Some(Dialog::About),
            "shortcuts" => self.dialog = Some(Dialog::Shortcuts),
            _ => {}
        }
        if let Ok(query) = std::env::var("KESTREL_DEV_QUERY") {
            self.palette.query = query;
            self.palette.needs_filter = true;
            self.palette.selected = 0;
        }
        if let Ok(id) = std::env::var("KESTREL_DEV_PLUGIN_CMD")
            && let Some(index) = self.plugins.command_index_of_id(&id)
        {
            self.run_plugin_command(index);
        }
    }

    pub fn save_session(&self) {
        let mut visual_order: Vec<usize> = Vec::with_capacity(self.tabs.len());
        for group in &self.groups {
            for id in &group.tabs {
                if let Some(index) = self.tab_index(*id)
                    && !visual_order.contains(&index)
                {
                    visual_order.push(index);
                }
            }
        }
        for index in 0..self.tabs.len() {
            if !visual_order.contains(&index) {
                visual_order.push(index);
            }
        }
        let tabs: Vec<TabSession> = visual_order
            .iter()
            .filter_map(|index| self.tabs.get(*index))
            .filter_map(|editor| {
                editor.doc.path.as_ref().map(|path| TabSession {
                    path: path.to_string_lossy().to_string(),
                    caret: editor.caret,
                    scroll: editor.scroll_y,
                })
            })
            .collect();
        let active = visual_order
            .iter()
            .position(|index| *index == self.active)
            .unwrap_or(0);
        let session = Session {
            folder: self
                .workspace
                .root
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            tabs,
            active,
            untitled: self.untitled_snapshot(),
        };
        session.save();
    }

    /// Capture unsaved buffers; text above `MAX_INLINE_UNTITLED` moves to a recovery file.
    fn untitled_snapshot(&self) -> Vec<UntitledSession> {
        const MAX_INLINE_UNTITLED: usize = 512 * 1024;
        let mut snapshots = Vec::new();
        for (index, editor) in self.tabs.iter().enumerate() {
            if editor.doc.path.is_some() || editor.doc.readonly || editor.virtual_title.is_some() {
                continue;
            }
            let text = editor.doc.text();
            let recovery = if text.len() > MAX_INLINE_UNTITLED {
                let dir = Settings::recovery_dir();
                let path = dir.join(format!("untitled-{index}.txt"));
                std::fs::create_dir_all(&dir)
                    .and_then(|()| std::fs::write(&path, &text))
                    .ok()
                    .map(|()| path.to_string_lossy().to_string())
            } else {
                None
            };
            let entry = match recovery {
                Some(path) => UntitledSession {
                    text: None,
                    path: Some(path),
                    language: editor.language.name().to_string(),
                    caret: editor.caret,
                },
                None => UntitledSession {
                    text: Some(text),
                    path: None,
                    language: editor.language.name().to_string(),
                    caret: editor.caret,
                },
            };
            snapshots.push(entry);
        }
        snapshots
    }

    /// Changes fingerprint over unsaved buffers, so quiet periods trigger a recovery write.
    fn untitled_fingerprint(&self) -> u64 {
        let mut fingerprint = 0u64;
        for tab in &self.tabs {
            if tab.doc.path.is_none() && !tab.doc.readonly && tab.virtual_title.is_none() {
                fingerprint = fingerprint
                    .wrapping_mul(31)
                    .wrapping_add(tab.doc.version)
                    .wrapping_mul(31)
                    .wrapping_add(tab.untitled_index as u64 + 1);
            }
        }
        fingerprint
    }

    /// Write the session 2 seconds after the last unsaved-buffer change.
    fn session_autosave(&mut self) {
        let fingerprint = self.untitled_fingerprint();
        if fingerprint != self.session_fingerprint {
            self.session_fingerprint = fingerprint;
            self.session_pending_since = Some(Instant::now());
        }
        if let Some(pending) = self.session_pending_since
            && pending.elapsed() >= Duration::from_secs(2)
        {
            self.session_pending_since = None;
            self.save_session();
        }
    }

    // ------------------------------------------------------------------
    // Styles
    // ------------------------------------------------------------------

    pub fn ui_style(&self) -> UiStyle {
        UiStyle {
            theme: self.theme,
            ui_family: self.ui_family.clone(),
            ui_bold_family: self.ui_bold_family.clone(),
            mono_family: self.mono_family.clone(),
            ui_size: 13.0,
        }
    }

    pub fn editor_style(&self) -> EditorStyle {
        EditorStyle {
            theme: self.theme,
            font_size: self.settings.font_size,
            line_h: self.settings.line_px(),
            tab_size: self.settings.tab_size,
            insert_spaces: self.settings.insert_spaces,
            show_minimap: self.settings.show_minimap,
            show_line_numbers: self.settings.show_line_numbers,
            show_indent_guides: self.settings.show_indent_guides,
            show_whitespace: self.settings.show_whitespace,
            show_sticky_scroll: self.settings.show_sticky_scroll,
            show_git_gutter: self.settings.show_git_gutter,
            autocomplete: self.settings.autocomplete,
            word_wrap: self.settings.word_wrap,
            scroll_past_end: self.settings.scroll_past_end,
            ui_size: 12.5,
            ui_family: self.ui_family.clone(),
            mono_family: self.mono_family.clone(),
        }
    }

    // ------------------------------------------------------------------
    // Tabs, Split View & Files
    // ------------------------------------------------------------------

    pub fn active_editor(&self) -> Option<&Editor> {
        self.tabs.get(self.active)
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        self.active_editor().and_then(|e| e.doc.path.clone())
    }

    pub fn dirty_tabs(&self) -> Vec<String> {
        self.tabs
            .iter()
            .filter(|e| e.is_dirty())
            .map(|e| e.title())
            .collect()
    }

    pub fn open_settings(&mut self) {
        self.settings_search.clear();
        self.dialog = Some(Dialog::Settings);
    }

    pub fn toggle_settings(&mut self) {
        if matches!(self.dialog, Some(Dialog::Settings)) {
            self.dialog = None;
        } else {
            self.open_settings();
        }
    }

    pub fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    pub fn group_of(&self, id: u64) -> Option<usize> {
        self.groups
            .iter()
            .position(|group| group.index_of(id).is_some())
    }

    /// Keep the flat `active` index pointing at the focused group's tab.
    pub fn sync_active(&mut self) {
        if let Some(index) = self
            .groups
            .get(self.active_group)
            .and_then(|group| group.active_id())
            .and_then(|id| self.tab_index(id))
        {
            self.active = index;
        }
    }

    pub fn focus_group(&mut self, group: usize) {
        if group >= self.groups.len() {
            return;
        }
        self.active_group = group;
        self.sync_active();
    }

    pub fn focus_tab_in_group(&mut self, group: usize, id: u64) {
        if group >= self.groups.len() {
            return;
        }
        let Some(pos) = self.groups[group].index_of(id) else {
            return;
        };
        self.groups[group].active = pos;
        self.active_group = group;
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
            self.sidebar.selected = tab.doc.path.clone();
        }
    }

    pub fn focus_tab(&mut self, id: u64) {
        let Some(group) = self.group_of(id) else { return };
        self.focus_tab_in_group(group, id);
    }

    /// Append a freshly created editor to the focused group.
    fn attach_editor_to_active_group(&mut self, id: u64) {
        self.ensure_groups();
        let group = self.active_group.min(self.groups.len() - 1);
        self.groups[group].tabs.push(id);
        self.groups[group].active = self.groups[group].tabs.len() - 1;
        self.active_group = group;
        self.sync_active();
    }

    pub fn ensure_groups(&mut self) {
        if self.groups.is_empty() {
            self.groups.push(EditorGroup::new());
            self.active_group = 0;
        }
        if self.active_group >= self.groups.len() {
            self.active_group = self.groups.len() - 1;
        }
    }

    /// Drop stale ids, remove empty groups, and forget editors no group shows.
    pub fn normalize_groups(&mut self) {
        let valid: Vec<u64> = self.tabs.iter().map(|tab| tab.id).collect();
        for group in &mut self.groups {
            group.tabs.retain(|id| valid.contains(id));
            if group.active >= group.tabs.len() {
                group.active = group.tabs.len().saturating_sub(1);
            }
        }
        if self.tabs.is_empty() {
            self.groups = vec![EditorGroup::new()];
            self.active_group = 0;
            self.active = 0;
            return;
        }
        let mut index = 0;
        while index < self.groups.len() {
            if self.groups[index].tabs.is_empty() {
                let weight = self.groups[index].weight;
                self.groups.remove(index);
                if index > 0 {
                    self.groups[index - 1].weight += weight;
                } else if let Some(first) = self.groups.first_mut() {
                    first.weight += weight;
                }
                if self.active_group > index {
                    self.active_group -= 1;
                } else if self.active_group == index {
                    self.active_group = self.active_group.saturating_sub(1);
                }
            } else {
                index += 1;
            }
        }
        if self.groups.is_empty() {
            self.groups.push(EditorGroup::new());
        }
        self.prune_editors();
        self.active_group = self.active_group.min(self.groups.len() - 1);
        self.sync_active();
    }

    /// Editors are only kept in `tabs` while some group still shows them.
    fn prune_editors(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let referenced: Vec<u64> = self
            .groups
            .iter()
            .flat_map(|group| group.tabs.iter().copied())
            .collect();
        if self.tabs.iter().all(|tab| referenced.contains(&tab.id)) {
            return;
        }
        let active_id = self.tabs.get(self.active).map(|tab| tab.id);
        self.tabs.retain(|tab| referenced.contains(&tab.id));
        if let Some(id) = active_id
            && self.tab_index(id).is_none()
        {
            self.sync_active();
        }
        if self.tabs.is_empty() {
            self.reset_groups();
        }
    }

    pub fn reset_groups(&mut self) {
        self.groups = vec![EditorGroup::new()];
        self.active_group = 0;
        self.group_rects.clear();
        self.drop_preview = None;
    }

    /// Duplicate a tab view into a new group so both panes can show it (VSCode-style).
    pub fn duplicate_view_in_new_group(&mut self, id: u64, at: usize) {
        let Some(source) = self.group_of(id) else { return };
        let weight = self.groups[source].weight;
        let at = at.min(self.groups.len()).max(1).min(self.groups.len());
        self.groups[source].weight = weight * 0.5;
        let mut group = EditorGroup::new();
        group.tabs.push(id);
        group.active = 0;
        group.weight = weight * 0.5;
        self.groups.insert(at, group);
        self.active_group = at;
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    /// `Ctrl+\`: duplicate the focused tab into a group on the right.
    pub fn split_editor_right(&mut self) {
        let Some(group) = self.groups.get(self.active_group) else {
            return;
        };
        let Some(id) = group.active_id() else {
            return;
        };
        self.duplicate_view_in_new_group(id, self.active_group + 1);
    }

    /// Drag-to-edge split: moves the tab when its group has other tabs, otherwise
    /// duplicates the view so the source pane is not left empty.
    pub fn split_tab_to_new_group(&mut self, id: u64, source_group: usize, at: usize) {
        if source_group >= self.groups.len() {
            return;
        }
        let Some(pos) = self.groups[source_group].index_of(id) else {
            return;
        };
        if self.groups[source_group].tabs.len() == 1 {
            self.duplicate_view_in_new_group(id, at.min(self.groups.len()));
            return;
        }
        let weight = self.groups[source_group].weight;
        self.groups[source_group].tabs.remove(pos);
        if self.groups[source_group].active >= self.groups[source_group].tabs.len() {
            self.groups[source_group].active =
                self.groups[source_group].tabs.len().saturating_sub(1);
        }
        self.groups[source_group].weight = weight * 0.5;
        let at = at.min(self.groups.len());
        let mut group = EditorGroup::new();
        group.tabs.push(id);
        group.active = 0;
        group.weight = weight * 0.5;
        self.groups.insert(at, group);
        self.active_group = at;
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    /// Merge `group` into its left neighbour (or the right one for the first).
    pub fn close_group(&mut self, group: usize) {
        if self.groups.len() < 2 || group >= self.groups.len() {
            return;
        }
        let target = if group > 0 { group - 1 } else { 1 };
        let moved = std::mem::take(&mut self.groups[group].tabs);
        let weight = self.groups[group].weight;
        if group < target {
            self.groups.remove(group);
            let neighbour = target - 1;
            let first = self.groups[neighbour].tabs.len();
            for id in moved {
                if self.groups[neighbour].index_of(id).is_none() {
                    self.groups[neighbour].tabs.push(id);
                }
            }
            self.groups[neighbour].weight += weight;
            if first > 0 && first < self.groups[neighbour].tabs.len() {
                self.groups[neighbour].active = first;
            }
            self.active_group = neighbour;
        } else {
            let first = self.groups[target].tabs.len();
            for id in moved {
                if self.groups[target].index_of(id).is_none() {
                    self.groups[target].tabs.push(id);
                }
            }
            self.groups[target].weight += weight;
            if first > 0 && first < self.groups[target].tabs.len() {
                self.groups[target].active = first;
            }
            self.groups.remove(group);
            self.active_group = target;
        }
        if self.active_group >= self.groups.len() {
            self.active_group = self.groups.len() - 1;
        }
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    pub fn move_tab_to_group(&mut self, id: u64, source: usize, target: usize, pos: usize) {
        if source >= self.groups.len() || target >= self.groups.len() {
            return;
        }
        let Some(source_pos) = self.groups[source].index_of(id) else {
            return;
        };
        if self.groups[target].index_of(id).is_some() && source != target {
            self.groups[source].tabs.remove(source_pos);
        } else {
            self.groups[source].tabs.remove(source_pos);
            if self.groups[source].active >= self.groups[source].tabs.len() {
                self.groups[source].active = self.groups[source].tabs.len().saturating_sub(1);
            }
            let pos = pos.min(self.groups[target].tabs.len());
            self.groups[target].tabs.insert(pos, id);
            self.groups[target].active = pos;
        }
        self.active_group = target;
        if source != target && self.groups[source].tabs.is_empty() {
            let weight = self.groups[source].weight;
            self.groups[target].weight += weight;
            self.groups.remove(source);
            self.active_group = if target > source { target - 1 } else { target };
        }
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    /// Show an already-open document as a tab view in `group`.
    pub fn show_view_in_group(&mut self, id: u64, group: usize) {
        if group >= self.groups.len() || self.tab_index(id).is_none() {
            return;
        }
        if let Some(pos) = self.groups[group].index_of(id) {
            self.groups[group].active = pos;
        } else {
            self.groups[group].tabs.push(id);
            self.groups[group].active = self.groups[group].tabs.len() - 1;
        }
        self.active_group = group;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.preview = false;
        }
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    /// Open a file (or show its view) in the given group.
    pub fn open_path_in_group(&mut self, path: PathBuf, group: usize) {
        self.ensure_groups();
        let group = group.min(self.groups.len() - 1);
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.doc.path.as_deref() == Some(path.as_path()))
        {
            let id = self.tabs[index].id;
            self.show_view_in_group(id, group);
            return;
        }
        self.active_group = group;
        self.open_file(path);
    }

    /// Open a file in a brand new group on the right of the focused group.
    pub fn open_file_to_side(&mut self, path: PathBuf) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.doc.path.as_deref() == Some(path.as_path()))
        {
            let id = self.tabs[index].id;
            self.duplicate_view_in_new_group(id, self.active_group + 1);
            return;
        }
        self.ensure_groups();
        let at = (self.active_group + 1).min(self.groups.len());
        let neighbour = at.saturating_sub(1).min(self.groups.len() - 1);
        let weight = self.groups[neighbour].weight;
        let mut group = EditorGroup::new();
        group.weight = weight * 0.5;
        self.groups[neighbour].weight = weight * 0.5;
        self.groups.insert(at, group);
        self.active_group = at;
        self.sync_active();
        self.open_file(path);
    }

    pub fn open_markdown_preview(&mut self, path: PathBuf) {
        self.open_file(path);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.preview = false;
            if tab.language == Language::Markdown {
                tab.md_mode = crate::markdown::MarkdownViewMode::Split;
                tab.md_preview.split_synced = false;
            }
        }
    }

    pub fn handle_markdown_preview_events(
        &mut self,
        tab_idx: usize,
        events: Vec<crate::markdown::PreviewEvent>,
        ctx: &egui::Context,
    ) {
        use crate::doc::EditKind;
        use crate::markdown::{MarkdownViewMode, PreviewEvent};

        for ev in events {
            match ev {
                PreviewEvent::ToggleTask {
                    byte_offset,
                    checked,
                } => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx)
                        && !tab.doc.readonly
                    {
                        let ch = tab.doc.byte_to_char(byte_offset);
                        if let Some(cur) = tab.doc.char_at(ch)
                            && matches!(cur, ' ' | 'x' | 'X')
                        {
                            let replacement = if checked { "x" } else { " " };
                            let before = (tab.caret, tab.anchor);
                            let edit = tab.doc.edit(
                                ch,
                                ch + 1,
                                replacement,
                                EditKind::Other,
                                before,
                                before,
                            );
                            tab.syntax.apply_edit(edit);
                            tab.last_edit = Instant::now();
                            tab.preview = false;
                            tab.invalidate_layout();
                        }
                    }
                }
                PreviewEvent::OpenUrl(url) => {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &url])
                        .spawn();
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(&url).spawn();
                    #[cfg(all(unix, not(target_os = "macos")))]
                    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                }
                PreviewEvent::OpenFile { path, anchor } => {
                    self.open_file(path);
                    if let Some(slug) = anchor
                        && let Some(tab) = self.tabs.get_mut(self.active)
                        && tab.language == Language::Markdown
                    {
                        if tab.md_mode == MarkdownViewMode::Source {
                            tab.md_mode = MarkdownViewMode::Preview;
                        }
                        tab.md_preview.pending_scroll_to = Some(slug);
                    }
                }
                PreviewEvent::CopyText(text) => {
                    ctx.copy_text(text);
                    self.toast("已复制代码块内容", ToastKind::Success);
                }
                PreviewEvent::JumpSourceLine(line) => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if tab.md_mode == MarkdownViewMode::Preview {
                            tab.md_mode = MarkdownViewMode::Split;
                            tab.md_preview.split_synced = false;
                        }
                        let ch = tab.doc.line_to_char(line.min(tab.doc.line_count().saturating_sub(1)));
                        tab.caret = ch;
                        tab.anchor = ch;
                        tab.ensure_visible = true;
                        tab.focus_requested = true;
                    }
                }
            }
        }
    }

    pub fn handle_viewer_events(
        &mut self,
        events: Vec<crate::viewer::ViewerEvent>,
        ctx: &egui::Context,
    ) {
        use crate::viewer::ViewerEvent;
        for ev in events {
            match ev {
                ViewerEvent::CopyText(text) => {
                    ctx.copy_text(text);
                    self.toast("已复制到剪贴板", ToastKind::Success);
                }
                ViewerEvent::OpenUrl(url) => {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &url])
                        .spawn();
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(&url).spawn();
                    #[cfg(all(unix, not(target_os = "macos")))]
                    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                }
            }
        }
    }

    pub fn reorder_group_tab(&mut self, group: usize, from: usize, to: usize) {
        let Some(g) = self.groups.get_mut(group) else {
            return;
        };
        if from >= g.tabs.len() || to >= g.tabs.len() || from == to {
            return;
        }
        let active_id = g.active_id();
        let item = g.tabs.remove(from);
        g.tabs.insert(to.min(g.tabs.len()), item);
        if let Some(active_id) = active_id
            && let Some(pos) = g.index_of(active_id)
        {
            g.active = pos;
        }
        self.active_group = group;
        self.sync_active();
    }

    pub fn move_tree_path(&mut self, src: PathBuf, dst_dir: PathBuf) {
        let dst_dir = if dst_dir.is_dir() {
            dst_dir
        } else if let Some(p) = dst_dir.parent() {
            p.to_path_buf()
        } else {
            return;
        };
        if src == dst_dir || dst_dir.starts_with(&src) || src.parent() == Some(dst_dir.as_path()) {
            return;
        }
        match crate::workspace::tree::move_path_to_dir(&src, &dst_dir) {
            Ok(new_path) => {
                if let Some(old_parent) = src.parent() {
                    self.workspace.tree.reload_dir(old_parent);
                }
                self.workspace.tree.expand(&dst_dir);
                self.workspace.tree.reload_dir(&dst_dir);
                for tab in &mut self.tabs {
                    if let Some(p) = &tab.doc.path
                        && (p == &src || p.starts_with(&src))
                        && let Ok(rel) = p.strip_prefix(&src)
                    {
                        let updated = if rel.as_os_str().is_empty() {
                            new_path.clone()
                        } else {
                            new_path.join(rel)
                        };
                        tab.doc.path = Some(updated.clone());
                        tab.set_language(Language::from_path(&updated));
                    }
                }
                self.sidebar.selected = Some(new_path);
                if let Some(root) = self.workspace.root.clone() {
                    self.workspace.index.scan(&root);
                    self.git.refresh(&root);
                }
                self.toast("已移动", ToastKind::Success);
            }
            Err(err) => self.toast(format!("移动失败: {err}"), ToastKind::Error),
        }
    }

    pub fn new_untitled(&mut self) {
        self.next_untitled += 1;
        let doc = Document::from_text("");
        let id = self.next_id;
        self.next_id += 1;
        let mut editor = Editor::new(doc, id, self.next_untitled);
        editor.focus_requested = true;
        self.tabs.push(editor);
        self.attach_editor_to_active_group(id);
    }

    pub fn open_file(&mut self, path: PathBuf) {
        self.open_file_internal(path, false);
    }

    pub fn open_file_preview(&mut self, path: PathBuf) {
        self.open_file_internal(path, true);
    }

    fn open_file_internal(&mut self, path: PathBuf, as_preview: bool) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|e| e.doc.path.as_deref() == Some(path.as_path()))
        {
            if !as_preview && let Some(tab) = self.tabs.get_mut(index) {
                tab.preview = false;
            }
            self.activate_tab(index);
            return;
        }
        let mm_kind = crate::viewer::MultimodalKind::from_path(&path);
        if let Some(kind) = mm_kind
            && kind.is_binary()
        {
            if !path.is_file() {
                self.toast(format!("无法打开 {}", path.display()), ToastKind::Error);
                return;
            }
            let mut doc = Document::from_text("");
            doc.path = Some(path.clone());
            doc.readonly = true;
            let id = self.next_id;
            self.next_id += 1;
            let mut editor = Editor::new(doc, id, 0);
            editor.preview = as_preview;
            editor.multimodal = Some(crate::viewer::MultimodalViewer::load_binary(&path, kind));
            if as_preview
                && let Some(prev_idx) = self
                    .tabs
                    .iter()
                    .position(|t| t.preview && !t.is_dirty() && !t.pinned)
            {
                let keep_id = self.tabs[prev_idx].id;
                editor.id = keep_id;
                self.tabs[prev_idx] = editor;
                self.focus_tab(keep_id);
            } else {
                self.tabs.push(editor);
                self.attach_editor_to_active_group(id);
            }
            self.reveal_in_tree(&path);
            return;
        }

        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match Document::load(&path) {
            Ok(mut doc) => {
                // If inside a Git repo, use HEAD blob as the diff baseline when available
                if let Some(root) = &self.workspace.root
                    && let Some(head_text) = crate::git::read_head_blob(root, &path)
                {
                    doc.baseline_text = Some(head_text.replace("\r\n", "\n"));
                }
                if doc.lossy && doc.encoding != crate::doc::Encoding::Binary {
                    self.toast(
                        format!(
                            "文件按 {} 解码显示{}",
                            doc.encoding.as_str(),
                            if doc.readonly { " (只读保护)" } else { "" }
                        ),
                        ToastKind::Warning,
                    );
                }
                let id = self.next_id;
                self.next_id += 1;
                let mut editor = Editor::new(doc, id, 0);
                editor.focus_requested = true;
                editor.ensure_visible = true;
                editor.preview = as_preview;
                if let Some(kind) = mm_kind {
                    let text = editor.doc.text();
                    editor.multimodal = Some(crate::viewer::MultimodalViewer::load_tabular_text(
                        &path, &text, kind,
                    ));
                }
                if size > crate::editor::MAX_HIGHLIGHT_BYTES as u64 {
                    self.toast(
                        format!(
                            "{} 大于 {} MB，已关闭语法高亮以保证流畅",
                            editor.title(),
                            crate::editor::MAX_HIGHLIGHT_BYTES / 1_000_000
                        ),
                        ToastKind::Info,
                    );
                }
                // Reuse existing clean preview tab if opening in preview mode
                if as_preview
                    && let Some(prev_idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.preview && !t.is_dirty() && !t.pinned)
                {
                    let keep_id = self.tabs[prev_idx].id;
                    editor.id = keep_id;
                    self.tabs[prev_idx] = editor;
                    self.focus_tab(keep_id);
                } else {
                    self.tabs.push(editor);
                    self.attach_editor_to_active_group(id);
                }
                self.reveal_in_tree(&path);
            }
            Err(err) => {
                self.toast(
                    format!("无法打开 {}: {err}", path.display()),
                    ToastKind::Error,
                );
            }
        }
    }

    fn open_file_quiet(&mut self, path: PathBuf, state: Option<(usize, f32)>) {
        let path = self.canonical(path);
        let mm_kind = crate::viewer::MultimodalKind::from_path(&path);
        if let Some(kind) = mm_kind
            && kind.is_binary()
        {
            if path.is_file() {
                let mut doc = Document::from_text("");
                doc.path = Some(path.clone());
                doc.readonly = true;
                let id = self.next_id;
                self.next_id += 1;
                let mut editor = Editor::new(doc, id, 0);
                editor.multimodal = Some(crate::viewer::MultimodalViewer::load_binary(&path, kind));
                self.tabs.push(editor);
                self.attach_editor_to_active_group(id);
            }
            return;
        }
        if let Ok(mut doc) = Document::load(&path) {
            if let Some(root) = &self.workspace.root
                && let Some(head_text) = crate::git::read_head_blob(root, &path)
            {
                doc.baseline_text = Some(head_text.replace("\r\n", "\n"));
            }
            let id = self.next_id;
            self.next_id += 1;
            let mut editor = Editor::new(doc, id, 0);
            editor.focus_requested = true;
            if let Some(kind) = mm_kind {
                let text = editor.doc.text();
                editor.multimodal = Some(crate::viewer::MultimodalViewer::load_tabular_text(
                    &path, &text, kind,
                ));
            }
            if let Some((caret, scroll)) = state {
                editor.caret = caret.min(editor.doc.len_chars());
                editor.anchor = editor.caret;
                editor.scroll_y = scroll;
                editor.ensure_visible = false;
            }
            self.tabs.push(editor);
            self.attach_editor_to_active_group(id);
        }
    }

    pub fn open_file_at(&mut self, path: PathBuf, byte_offset: usize) {
        let path = self.canonical(path);
        self.open_file(path);
        if let Some(tab) = self.tabs.get_mut(self.active)
            && tab.doc.path.is_some()
        {
            let ch = tab.doc.byte_to_char(byte_offset);
            tab.caret = ch;
            tab.anchor = ch;
            tab.ensure_visible = true;
            tab.focus_requested = true;
        }
    }

    /// Open a read-only virtual document (git revision, diff, etc.) in the active group.
    pub fn open_virtual_text(&mut self, title: String, text: String, language: Language) {
        let mut doc = Document::from_text(&text);
        doc.readonly = true;
        let id = self.next_id;
        self.next_id += 1;
        let mut editor = Editor::new(doc, id, 0);
        editor.set_language(language);
        editor.virtual_title = Some(title);
        editor.focus_requested = true;
        self.tabs.push(editor);
        self.attach_editor_to_active_group(id);
    }

    /// Browse a file's content at a specific commit (read-only tab).
    pub fn open_git_revision_file(&mut self, commit: &str, rel_path: &str) {
        let Some(root) = self.workspace.root.clone() else {
            self.toast("尚未打开工作区文件夹", ToastKind::Warning);
            return;
        };
        let Some(text) = crate::git::read_blob_at(&root, commit, rel_path) else {
            self.toast(
                format!("无法读取提交中的文件 {rel_path}"),
                ToastKind::Warning,
            );
            return;
        };
        let short = &commit[..commit.len().min(7)];
        let language = Language::from_path(Path::new(rel_path));
        self.open_virtual_text(format!("{rel_path} @{short}"), text, language);
    }

    /// Open a Git diff tab for `path`: working tree vs index when `staged` is
    /// false, index vs HEAD when it is true. Opening the same pair again focuses
    /// the existing tab.
    pub fn open_file_diff(&mut self, path: PathBuf, staged: bool) {
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.diff
                .as_ref()
                .is_some_and(|diff| diff.path == path && diff.staged == staged)
        }) {
            self.activate_tab(index);
            return;
        }
        let rel_path = self
            .workspace
            .root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .map(|rel| rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
            .unwrap_or_else(|| path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"));
        let (status, orig_rel) = self
            .git
            .snapshot
            .changes
            .iter()
            .find(|change| change.path == path && change.staged == staged)
            .map(|change| (change.status, change.orig_path.clone()))
            .unwrap_or((GitFileStatus::Modified, None));
        let language = Language::from_path(&path);
        let mut doc = Document::from_text("");
        doc.readonly = true;
        let id = self.next_id;
        self.next_id += 1;
        let mut editor = Editor::new(doc, id, 0);
        editor.set_language(language);
        editor.virtual_title = Some(format!(
            "{rel_path} ({})",
            if staged { "已暂存" } else { "更改" }
        ));
        let mut diff = crate::diff::DiffTab::new(path.clone(), rel_path, staged, status);
        diff.orig_rel = orig_rel;
        editor.diff = Some(diff);
        self.tabs.push(editor);
        self.attach_editor_to_active_group(id);
        self.spawn_diff_load(id);
    }

    pub fn spawn_diff_load(&mut self, tab_id: u64) {
        let Some(root) = self.workspace.root.clone() else {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id)
                && let Some(diff) = tab.diff.as_mut()
            {
                diff.loading = false;
                diff.error = Some("尚未打开工作区文件夹".to_string());
            }
            return;
        };
        let tab_size = self.settings.tab_size;
        let resolved = self.tabs.iter().find(|tab| tab.id == tab_id).and_then(|tab| {
            let diff = tab.diff.as_ref()?;
            self.git
                .snapshot
                .changes
                .iter()
                .find(|change| change.path == diff.path && change.staged == diff.staged)
                .map(|change| (change.status, change.orig_path.clone()))
        });
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id)
            && let Some(diff) = tab.diff.as_mut()
        {
            match resolved {
                Some((status, orig_rel)) => {
                    diff.status = status;
                    diff.orig_rel = orig_rel;
                }
                None => {
                    if !diff.staged {
                        diff.status = GitFileStatus::Modified;
                        diff.orig_rel = None;
                    }
                }
            }
            diff.start_load(root, tab_size);
        }
    }

    pub fn goto_definition_by_name(&mut self, symbol: &str) {
        let current_path = self.active_path();
        let current_symbols = self
            .active_editor()
            .map(|e| e.symbols.clone())
            .unwrap_or_default();
        let files = self.workspace.index.files().to_vec();
        if let Some((target_path, byte_offset, line)) = crate::lsp::find_definition(
            symbol,
            current_path.as_deref(),
            &current_symbols,
            &files,
        ) {
            if let Some(path) = target_path {
                self.open_file_at(path, byte_offset);
            } else if let Some(tab) = self.tabs.get_mut(self.active) {
                let ch = tab.doc.byte_to_char(byte_offset);
                tab.caret = ch;
                tab.anchor = ch;
                tab.ensure_visible = true;
                tab.focus_requested = true;
            }
            self.toast(format!("已跳转到 `{symbol}` 定义 (第 {line} 行)"), ToastKind::Info);
        } else {
            self.toast(format!("未找到符号 `{symbol}` 的定义"), ToastKind::Warning);
        }
    }

    pub fn activate_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            self.sync_active();
            return;
        }
        let id = self.tabs[index].id;
        if self
            .groups
            .get(self.active_group)
            .is_some_and(|group| group.index_of(id).is_some())
        {
            self.focus_tab_in_group(self.active_group, id);
        } else {
            self.focus_tab(id);
        }
    }

    pub fn cycle_group_tab(&mut self, forward: bool) {
        let Some(group) = self.groups.get(self.active_group) else {
            return;
        };
        let len = group.tabs.len();
        if len < 2 {
            return;
        }
        let next = if forward {
            (group.active + 1) % len
        } else {
            (group.active + len - 1) % len
        };
        if let Some(id) = group.tabs.get(next).copied() {
            self.focus_tab_in_group(self.active_group, id);
        }
    }

    pub fn close_active_tab(&mut self) {
        let Some(group) = self.groups.get(self.active_group) else {
            return;
        };
        let Some(id) = group.active_id() else {
            return;
        };
        let Some(index) = self.tab_index(id) else {
            return;
        };
        self.close_tab_in_group(self.active_group, index);
    }

    /// Close one view of `index` inside `group`. The document itself is only
    /// forgotten when no other group still shows it.
    pub fn close_tab_in_group(&mut self, group: usize, index: usize) {
        if index >= self.tabs.len() || group >= self.groups.len() {
            return;
        }
        let id = self.tabs[index].id;
        let other_view = self
            .groups
            .iter()
            .enumerate()
            .any(|(gi, other)| gi != group && other.index_of(id).is_some());
        if !other_view && self.tabs[index].is_dirty() {
            self.dialog = Some(Dialog::ConfirmCloseTab(group, index));
            return;
        }
        self.close_tab_force_in_group(group, index);
    }

    pub fn close_tab_force_in_group(&mut self, group: usize, index: usize) {
        if index >= self.tabs.len() || group >= self.groups.len() {
            return;
        }
        let id = self.tabs[index].id;
        let mut active_group = self.active_group;
        if let Some(pos) = self.groups[group].index_of(id) {
            self.groups[group].tabs.remove(pos);
        }
        if self.groups[group].active >= self.groups[group].tabs.len() {
            self.groups[group].active = self.groups[group].tabs.len().saturating_sub(1);
        }
        if self.groups[group].tabs.is_empty() && self.groups.len() > 1 {
            let weight = self.groups[group].weight;
            self.groups.remove(group);
            if group > 0 {
                self.groups[group - 1].weight += weight;
            } else if let Some(first) = self.groups.first_mut() {
                first.weight += weight;
            }
            if group < active_group {
                active_group -= 1;
            } else if group == active_group {
                active_group = active_group.saturating_sub(1);
            }
        }
        self.active_group = active_group;
        self.prune_editors();
        if self.tabs.is_empty() {
            self.active = 0;
            self.reset_groups();
            return;
        }
        self.ensure_groups();
        self.active_group = self.active_group.min(self.groups.len() - 1);
        self.sync_active();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    pub fn close_all_tabs(&mut self) {
        if self.tabs.iter().any(|t| t.is_dirty()) {
            self.dialog = Some(Dialog::ConfirmCloseAll);
            return;
        }
        self.close_all_tabs_force();
    }

    /// Close every editor tab, keeping the window and workspace open.
    pub fn close_all_tabs_force(&mut self) {
        self.tabs.clear();
        self.active = 0;
        self.reset_groups();
    }

    pub fn save_active(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.doc.path.is_none() {
            self.save_active_as();
            return;
        }
        let doc_id = tab.id;
        self.emit_plugin_event_for(Event::BeforeSave, doc_id);
        let Some(index) = self.tabs.iter().position(|tab| tab.id == doc_id) else {
            return;
        };
        let result = {
            let tab = &mut self.tabs[index];
            let result = tab.doc.save();
            if matches!(result, SaveOutcome::Saved) {
                tab.preview = false;
                tab.diff_version = 0;
            }
            result
        };
        let name = self.tabs[index].title();
        match result {
            SaveOutcome::Saved => {
                self.toast(format!("已保存 {name}"), ToastKind::Success);
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
                self.emit_plugin_event_for(Event::Saved, doc_id);
            }
            SaveOutcome::DiskChanged => self.toast(
                format!("保存 {name} 已取消: 文件已在磁盘上被修改"),
                ToastKind::Warning,
            ),
            SaveOutcome::ReadOnly => {
                self.toast(format!("{name} 为只读文件，无法保存"), ToastKind::Warning)
            }
            SaveOutcome::Failed(err) => {
                self.toast(format!("保存 {name} 失败: {err}"), ToastKind::Error)
            }
        }
    }

    pub fn save_active_as(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let default_name = if tab.doc.path.is_some() {
            tab.title()
        } else {
            "untitled.txt".to_string()
        };
        let mut dialog = rfd::FileDialog::new().set_file_name(default_name);
        if let Some(root) = &self.workspace.root {
            dialog = dialog.set_directory(root);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let tab = &mut self.tabs[self.active];
        match tab.doc.save_as(path.clone()) {
            SaveOutcome::Saved => {
                tab.preview = false;
                tab.set_language(Language::from_path(&path));
                self.toast(format!("已保存 {}", path.display()), ToastKind::Success);
                self.reveal_in_tree(&path);
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
            }
            SaveOutcome::DiskChanged => self.toast(
                format!("另存为失败: {} 已在磁盘上被修改", path.display()),
                ToastKind::Warning,
            ),
            SaveOutcome::ReadOnly => {
                self.toast(format!("没有权限写入 {}", path.display()), ToastKind::Warning)
            }
            SaveOutcome::Failed(err) => {
                self.toast(format!("另存为失败: {err}"), ToastKind::Error);
            }
        }
    }

    pub fn save_all(&mut self) {
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .filter(|editor| editor.is_dirty() && editor.doc.path.is_some())
            .map(|editor| editor.id)
            .collect();
        let mut saved = 0;
        let mut blocked = 0;
        let mut failed = 0;
        for id in ids {
            self.emit_plugin_event_for(Event::BeforeSave, id);
            let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                continue;
            };
            let outcome = {
                let tab = &mut self.tabs[index];
                let result = tab.doc.save();
                if matches!(result, SaveOutcome::Saved) {
                    tab.preview = false;
                    tab.diff_version = 0;
                }
                result
            };
            match outcome {
                SaveOutcome::Saved => {
                    saved += 1;
                    self.emit_plugin_event_for(Event::Saved, id);
                }
                SaveOutcome::DiskChanged | SaveOutcome::ReadOnly => blocked += 1,
                SaveOutcome::Failed(_) => failed += 1,
            }
        }
        if saved > 0 {
            self.toast(format!("已保存 {saved} 个文件"), ToastKind::Success);
            if let Some(root) = self.workspace.root.clone() {
                self.git.refresh(&root);
            }
        }
        if blocked + failed > 0 {
            self.toast(
                format!("有 {blocked} 个文件磁盘已变更或只读，{failed} 个写入失败"),
                ToastKind::Warning,
            );
        }
    }

    pub fn reload_active(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            match tab.doc.reload() {
                Ok(()) => self.toast("已从磁盘重新加载", ToastKind::Info),
                Err(err) => self.toast(format!("重新加载失败: {err}"), ToastKind::Error),
            }
        }
    }

    /// React to an external change of an open file. Writes done by this app are
    /// recognised by the mtime recorded in the document and ignored.
    fn handle_disk_change(&mut self, path: &Path) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.doc.path.as_deref() == Some(path))
        else {
            return;
        };
        if !self.tabs[index].doc.disk_changed() {
            return;
        }
        let title = self.tabs[index].title();
        if self.tabs[index].is_dirty() {
            if self.conflict_notified.insert(path.to_path_buf()) {
                self.toast(
                    format!("{title} 已在磁盘上被修改，未自动重新加载"),
                    ToastKind::Warning,
                );
            }
        } else if self.tabs[index].doc.reload().is_ok() {
            self.tabs[index].invalidate_layout();
            if self.reload_notified.insert(path.to_path_buf()) {
                self.toast(format!("{title} 已从磁盘重新加载"), ToastKind::Info);
            }
        }
    }

    // ------------------------------------------------------------------
    // Workspace
    // ------------------------------------------------------------------

    pub fn open_folder(&mut self, path: PathBuf) {
        if self.tabs.iter().any(|t| t.is_dirty()) {
            self.dialog = Some(Dialog::ConfirmOpenFolder(path));
            return;
        }
        self.open_folder_quiet(path);
    }

    pub fn open_folder_quiet(&mut self, path: PathBuf) {
        let path = self.canonical(path);
        self.settings.note_folder(&path.to_string_lossy());
        self.workspace.root = Some(path.clone());
        self.workspace.tree.show_hidden = self.settings.show_hidden_files;
        self.workspace.tree.set_root(path.clone());
        self.workspace.tree.reload();
        self.workspace.index.clear();
        self.workspace.index.scan(&path);
        self.workspace.watcher = FsWatcher::new(&path).ok();
        self.workspace.search_panel = SearchPanelState::default();
        self.workspace.search.cancel();
        self.git.prime_fast(&path);
        self.git.log(format!("打开工作区: {}", path.display()));
        self.tabs.clear();
        self.active = 0;
        self.reset_groups();
        self.sidebar.selected = None;
        self.sidebar.editing = None;
    }

    fn canonical(&self, path: PathBuf) -> PathBuf {
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        let text = canonical.to_string_lossy();
        if let Some(stripped) = text.strip_prefix("\\\\?\\") {
            PathBuf::from(stripped)
        } else {
            canonical
        }
    }

    pub fn reveal_in_tree(&mut self, path: &Path) {
        if let Some(root) = self.workspace.root.clone() {
            if !path.starts_with(&root) {
                return;
            }
            let mut dir = path.parent().map(|p| p.to_path_buf());
            while let Some(current) = dir {
                if current == root || current.starts_with(&root) {
                    self.workspace.tree.expand(&current);
                }
                if current == root {
                    break;
                }
                dir = current.parent().map(|p| p.to_path_buf());
            }
            self.sidebar.selected = Some(path.to_path_buf());
        }
    }

    pub fn open_folder_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(root) = &self.workspace.root {
            dialog = dialog.set_directory(root);
        }
        if let Some(path) = dialog.pick_folder() {
            self.open_folder(path);
        }
    }

    pub fn open_file_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(root) = &self.workspace.root {
            dialog = dialog.set_directory(root);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_file(path);
        }
    }

    // ------------------------------------------------------------------
    // Sidebar tree edits
    // ------------------------------------------------------------------

    pub fn start_tree_edit(
        &mut self,
        kind: TreeEditKind,
        parent: PathBuf,
        original: Option<PathBuf>,
    ) {
        let name = match &original {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            None => match kind {
                TreeEditKind::NewFile => "新建文件".to_string(),
                TreeEditKind::NewFolder => "新建文件夹".to_string(),
                TreeEditKind::Rename => String::new(),
            },
        };
        self.sidebar.editing = Some(TreeEdit {
            kind,
            parent,
            original,
            name,
            focus: true,
        });
    }

    pub fn commit_tree_edit(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.sidebar.editing.take() else {
            return;
        };
        let name = edit.name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let result = match edit.kind {
            TreeEditKind::NewFile => {
                crate::workspace::tree::create_file(&edit.parent, &name).map(Some)
            }
            TreeEditKind::NewFolder => {
                crate::workspace::tree::create_dir(&edit.parent, &name).map(Some)
            }
            TreeEditKind::Rename => match &edit.original {
                Some(original) => crate::workspace::tree::rename(original, &name).map(Some),
                None => Ok(None),
            },
        };
        match result {
            Ok(Some(path)) => {
                self.workspace.tree.reload_dir(&edit.parent);
                match edit.kind {
                    TreeEditKind::NewFile => self.open_file(path),
                    TreeEditKind::Rename => {
                        if let Some(original) = &edit.original
                            && let Some(tab) = self
                                .tabs
                                .iter_mut()
                                .find(|t| t.doc.path.as_deref() == Some(original.as_path()))
                        {
                            tab.doc.path = Some(path.clone());
                            tab.set_language(Language::from_path(&path));
                        }
                        self.sidebar.selected = Some(path);
                    }
                    TreeEditKind::NewFolder => {
                        self.workspace.tree.expand(&path);
                    }
                }
                if let Some(root) = self.workspace.root.clone() {
                    self.workspace.index.scan(&root);
                    self.git.refresh(&root);
                }
            }
            Ok(None) => {}
            Err(err) => {
                self.toast(format!("操作失败: {err}"), ToastKind::Error);
            }
        }
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique("tree_edit")));
    }

    pub fn delete_path(&mut self, path: PathBuf) {
        match crate::workspace::tree::delete(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    self.workspace.tree.reload_dir(parent);
                }
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|t| t.doc.path.as_deref() == Some(path.as_path()))
                {
                    self.tabs.remove(index);
                    self.active = self.active.min(self.tabs.len().saturating_sub(1));
                }
                if let Some(root) = &self.workspace.root {
                    self.workspace.index.scan(root);
                    self.git.refresh(root);
                }
                self.toast("已删除", ToastKind::Info);
            }
            Err(err) => self.toast(format!("删除失败: {err}"), ToastKind::Error),
        }
    }

    pub fn duplicate_tree_path(&mut self, path: PathBuf) {
        match crate::workspace::tree::duplicate_path(&path) {
            Ok(new_path) => {
                if let Some(parent) = new_path.parent() {
                    self.workspace.tree.reload_dir(parent);
                }
                self.sidebar.selected = Some(new_path.clone());
                if new_path.is_file() {
                    self.open_file(new_path);
                }
                if let Some(root) = self.workspace.root.clone() {
                    self.workspace.index.scan(&root);
                    self.git.refresh(&root);
                }
                self.toast("已创建副本", ToastKind::Success);
            }
            Err(err) => self.toast(format!("创建副本失败: {err}"), ToastKind::Error),
        }
    }

    pub fn paste_tree_clipboard(&mut self, dst_dir: PathBuf) {
        let Some(clip) = self.sidebar.clipboard.clone() else {
            return;
        };
        if !clip.path.exists() {
            self.sidebar.clipboard = None;
            self.toast("剪贴板中的源文件已不存在", ToastKind::Warning);
            return;
        }
        let dst_dir = if dst_dir.is_dir() {
            dst_dir
        } else if let Some(parent) = dst_dir.parent() {
            parent.to_path_buf()
        } else {
            return;
        };
        let result = match clip.kind {
            TreeClipboardKind::Copy => crate::workspace::tree::copy_path_to_dir(&clip.path, &dst_dir),
            TreeClipboardKind::Cut => crate::workspace::tree::move_path_to_dir(&clip.path, &dst_dir),
        };
        match result {
            Ok(new_path) => {
                if clip.kind == TreeClipboardKind::Cut {
                    if let Some(old_parent) = clip.path.parent() {
                        self.workspace.tree.reload_dir(old_parent);
                    }
                    for tab in &mut self.tabs {
                        if let Some(p) = &tab.doc.path
                            && (p == &clip.path || p.starts_with(&clip.path))
                            && let Ok(rel) = p.strip_prefix(&clip.path)
                        {
                            let updated = if rel.as_os_str().is_empty() {
                                new_path.clone()
                            } else {
                                new_path.join(rel)
                            };
                            tab.doc.path = Some(updated.clone());
                            tab.set_language(Language::from_path(&updated));
                        }
                    }
                    self.sidebar.clipboard = None;
                }
                self.workspace.tree.expand(&dst_dir);
                self.workspace.tree.reload_dir(&dst_dir);
                self.sidebar.selected = Some(new_path);
                if let Some(root) = self.workspace.root.clone() {
                    self.workspace.index.scan(&root);
                    self.git.refresh(&root);
                }
                self.toast("已粘贴", ToastKind::Success);
            }
            Err(err) => self.toast(format!("粘贴失败: {err}"), ToastKind::Error),
        }
    }

    // ------------------------------------------------------------------
    // Search & Project-Wide Replace
    // ------------------------------------------------------------------

    pub fn start_project_search(&mut self) {
        let query = self.workspace.search_panel.query.clone();
        let Some(root) = self.workspace.root.clone() else {
            self.toast("请先打开文件夹", ToastKind::Info);
            return;
        };
        if query.trim().is_empty() {
            self.workspace.search_panel.results.clear();
            self.workspace.search_panel.last_query.clear();
            self.workspace.search.cancel();
            return;
        }
        if self.workspace.index.files().is_empty() {
            self.workspace.pending_search = true;
            self.workspace.index.scan(&root);
            return;
        }
        let search_query = SearchQuery {
            pattern: query.clone(),
            regex: self.workspace.search_panel.regex,
            case_sensitive: self.workspace.search_panel.case_sensitive,
            whole_word: self.workspace.search_panel.whole_word,
            include: self.workspace.search_panel.include.clone(),
            exclude: self.workspace.search_panel.exclude.clone(),
            max_results: 2000,
        };
        let files = self.workspace.index.files().to_vec();
        self.workspace.search_panel.results.clear();
        self.workspace.search_panel.scanning = true;
        self.workspace.search_panel.scanned = 0;
        self.workspace.search_panel.error = None;
        self.workspace.search_panel.last_query = query;
        self.workspace.search_panel.collapsed.clear();
        self.workspace.search.start(root, files, search_query);
    }

    pub fn replace_all_in_project(&mut self) {
        if self.workspace.search_panel.query.trim().is_empty() {
            return;
        }
        let search_query = SearchQuery {
            pattern: self.workspace.search_panel.query.clone(),
            regex: self.workspace.search_panel.regex,
            case_sensitive: self.workspace.search_panel.case_sensitive,
            whole_word: self.workspace.search_panel.whole_word,
            include: self.workspace.search_panel.include.clone(),
            exclude: self.workspace.search_panel.exclude.clone(),
            max_results: 10000,
        };
        let replacement = self.workspace.search_panel.replacement.clone();
        let matched_paths: Vec<PathBuf> = self
            .workspace
            .search_panel
            .results
            .iter()
            .map(|r| r.path.clone())
            .collect();
        let dirty_open: Vec<String> = matched_paths
            .iter()
            .filter_map(|path| {
                self.tabs
                    .iter()
                    .find(|tab| tab.doc.path.as_deref() == Some(path.as_path()) && tab.is_dirty())
                    .map(|tab| tab.title())
            })
            .collect();
        if !dirty_open.is_empty() {
            self.toast(
                format!("{} 有未保存修改，全项目替换已中止", dirty_open.join("、")),
                ToastKind::Warning,
            );
            return;
        }
        let mut total_files = 0usize;
        let mut total_hits = 0usize;

        for path in matched_paths {
            if let Ok(count) =
                crate::workspace::search::replace_in_file(&path, &search_query, &replacement)
                && count > 0
            {
                total_files += 1;
                total_hits += count;
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.doc.path.as_deref() == Some(path.as_path()))
                    && !tab.is_dirty()
                {
                    let _ = tab.doc.reload();
                    tab.invalidate_layout();
                }
            }
        }
        if total_hits > 0 {
            self.toast(
                format!("已在 {total_files} 个文件中替换 {total_hits} 处匹配"),
                ToastKind::Success,
            );
            self.start_project_search();
            if let Some(root) = self.workspace.root.clone() {
                self.git.refresh(&root);
            }
        } else {
            self.toast("没有可替换的匹配项", ToastKind::Info);
        }
    }

    fn poll_search(&mut self) {
        let events = self.workspace.search.poll();
        for event in events {
            match event {
                SearchEvent::Results(files) => {
                    for file in files {
                        if let Some(existing) = self
                            .workspace
                            .search_panel
                            .results
                            .iter_mut()
                            .find(|r| r.path == file.path)
                        {
                            existing.matches.extend(file.matches);
                        } else {
                            self.workspace.search_panel.results.push(file);
                        }
                    }
                    self.workspace
                        .search_panel
                        .results
                        .sort_by(|a, b| a.path.cmp(&b.path));
                }
                SearchEvent::Progress {
                    scanned,
                    matched_files,
                } => {
                    self.workspace.search_panel.scanned = scanned;
                    self.workspace.search_panel.matched_files = matched_files;
                }
                SearchEvent::Done {
                    scanned,
                    matched_files,
                    cancelled,
                } => {
                    self.workspace.search_panel.scanned = scanned;
                    self.workspace.search_panel.matched_files = matched_files;
                    self.workspace.search_panel.scanning = false;
                    if cancelled {
                        self.workspace.search_panel.last_query.clear();
                    }
                }
                SearchEvent::Error(err) => {
                    self.workspace.search_panel.scanning = false;
                    self.workspace.search_panel.error = Some(err);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Palette
    // ------------------------------------------------------------------

    pub fn open_palette(&mut self, mode: PaletteMode) {
        self.palette.open = true;
        self.palette.mode = mode;
        self.palette.query.clear();
        self.palette.selected = 0;
        self.palette.window_start = 0;
        self.palette.focus = true;
        self.palette.items.clear();
        self.palette.filtered.clear();
        self.palette.needs_filter = true;
        match mode {
            PaletteMode::QuickOpen => {
                if let Some(root) = self.workspace.root.clone()
                    && self.workspace.index.files().is_empty()
                {
                    self.workspace.index.scan(&root);
                }
            }
            PaletteMode::Language => {
                for language in LANGUAGES {
                    self.palette.items.push(PaletteItem::Language(*language));
                }
            }
            PaletteMode::Commands => {
                for command in Command::ALL {
                    self.palette.items.push(PaletteItem::Command(*command));
                }
                for command in self.plugins.commands() {
                    self.palette
                        .items
                        .push(PaletteItem::PluginCommand(command.id.clone()));
                }
            }
            PaletteMode::Symbols => {
                let symbols = self
                    .active_editor()
                    .map(|e| e.symbols.clone())
                    .unwrap_or_default();
                for sym in symbols {
                    self.palette.items.push(PaletteItem::Symbol(sym));
                }
            }
            PaletteMode::Branches => {
                for b in &self.git.snapshot.branches {
                    self.palette.items.push(PaletteItem::Branch(b.clone()));
                }
            }
            PaletteMode::GotoLine => {}
        }
    }

    pub fn close_palette(&mut self) {
        self.palette.open = false;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    pub(crate) fn palette_text(&self, item: &PaletteItem) -> (String, String) {
        match item {
            PaletteItem::Command(command) => (
                command.label().to_string(),
                command.shortcut_text().unwrap_or_default(),
            ),
            PaletteItem::File(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let rel = self
                    .workspace
                    .root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                (name, rel)
            }
            PaletteItem::Language(language) => {
                let current = self
                    .active_editor()
                    .is_some_and(|editor| editor.language == *language);
                (
                    language.name().to_string(),
                    if current {
                        "当前".to_string()
                    } else {
                        String::new()
                    },
                )
            }
            PaletteItem::Symbol(sym) => (
                sym.name.clone(),
                format!("{} · 第 {} 行", sym.detail, sym.start_line + 1),
            ),
            PaletteItem::Branch(branch) => (
                branch.clone(),
                if *branch == self.git.snapshot.branch {
                    "当前分支".to_string()
                } else {
                    String::new()
                },
            ),
            PaletteItem::PluginCommand(id) => {
                match self
                    .plugins
                    .commands()
                    .iter()
                    .find(|command| command.id == *id)
                {
                    Some(command) => (
                        command.title.clone(),
                        command.shortcut_text().unwrap_or_default(),
                    ),
                    None => (String::new(), String::new()),
                }
            }
        }
    }

    pub fn filter_palette(&mut self) {
        let query = self.palette.query.trim().to_string();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        if self.palette.mode == PaletteMode::QuickOpen {
            let files = self.workspace.index.files().to_vec();
            for (idx, path) in files.iter().enumerate() {
                let rel = self
                    .workspace
                    .root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                if query.is_empty() {
                    scored.push((0, idx));
                    continue;
                }
                if let Some(score) = fuzzy::quick_score(&query, &rel) {
                    let bonus = if rel
                        .rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|name| fuzzy::quick_score(&query, name).is_some())
                    {
                        20
                    } else {
                        0
                    };
                    scored.push((score + bonus, idx));
                }
            }
            scored.sort_by_key(|item| std::cmp::Reverse(item.0));
            scored.truncate(400);
            self.palette.items = scored
                .iter()
                .map(|(_, idx)| PaletteItem::File(files[*idx].clone()))
                .collect();
            self.palette.filtered = (0..self.palette.items.len()).collect();
        } else {
            let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
            for (idx, item) in self.palette.items.iter().enumerate() {
                let (primary, secondary) = self.palette_text(item);
                let hay = match item {
                    PaletteItem::Command(command) => {
                        format!("{} {} {secondary}", command.category(), primary)
                    }
                    PaletteItem::PluginCommand(id) => {
                        let category = self
                            .plugins
                            .command_category_of(id)
                            .unwrap_or_default();
                        format!("{category} {primary} {secondary}")
                    }
                    _ => {
                        if secondary.is_empty() {
                            primary.clone()
                        } else {
                            format!("{primary} {secondary}")
                        }
                    }
                };
                if query.is_empty() {
                    scored.push((0, idx, Vec::new()));
                    continue;
                }
                if self.palette.mode == PaletteMode::GotoLine {
                    continue;
                }
                match fuzzy::fuzzy_match(&query, &primary) {
                    Some(m) => scored.push((m.score + 30, idx, m.positions)),
                    None => {
                        if let Some(m) = fuzzy::fuzzy_match(&query, &hay) {
                            scored.push((m.score, idx, m.positions));
                        }
                    }
                }
            }
            scored.sort_by_key(|item| std::cmp::Reverse(item.0));
            self.palette.filtered = scored.iter().map(|(_, idx, _)| *idx).collect();
        }
        self.palette.needs_filter = false;
        self.palette.selected = self
            .palette
            .selected
            .min(self.palette.filtered.len().saturating_sub(1));
        self.palette.window_start = 0;
        self.palette.list_version = self.workspace.index_version;
    }

    pub fn palette_activate(&mut self, ctx: &egui::Context) {
        let item = self
            .palette
            .filtered
            .get(self.palette.selected)
            .and_then(|idx| self.palette.items.get(*idx))
            .cloned();
        match item {
            Some(PaletteItem::Command(command)) => {
                self.close_palette();
                self.dispatch(command, ctx);
            }
            Some(PaletteItem::PluginCommand(id)) => {
                self.close_palette();
                if let Some(index) = self.plugins.command_index_of_id(&id) {
                    self.run_plugin_command(index);
                }
            }
            Some(PaletteItem::File(path)) => {
                self.close_palette();
                self.open_file(path);
            }
            Some(PaletteItem::Language(language)) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.set_language(language);
                }
                self.close_palette();
            }
            Some(PaletteItem::Symbol(sym)) => {
                self.close_palette();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    let ch = tab.doc.byte_to_char(sym.byte_offset);
                    tab.caret = ch;
                    tab.anchor = ch;
                    tab.ensure_visible = true;
                    tab.focus_requested = true;
                }
            }
            Some(PaletteItem::Branch(branch)) => {
                self.close_palette();
                if let Some(root) = self.workspace.root.clone() {
                    match self.git.checkout_branch(&root, &branch) {
                        Ok(()) => self.toast(format!("已切换到分支 {branch}"), ToastKind::Success),
                        Err(err) => self.toast(format!("切换分支失败: {err}"), ToastKind::Error),
                    }
                }
            }
            None => {
                if self.palette.mode == PaletteMode::GotoLine {
                    let line: usize = self.palette.query.trim().parse().unwrap_or(1);
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        let line = line.max(1).min(tab.doc.line_count()) - 1;
                        tab.caret = tab.doc.line_to_char(line);
                        tab.anchor = tab.caret;
                        tab.ensure_visible = true;
                        tab.focus_requested = true;
                    }
                }
                self.close_palette();
            }
        }
    }

    // ------------------------------------------------------------------
    // Terminal (Multi-Session)
    // ------------------------------------------------------------------

    pub fn toggle_terminal(&mut self) {
        self.bottom.visible = !self.bottom.visible;
        if self.bottom.visible && self.bottom.terminals.is_empty() {
            self.new_terminal();
        }
    }

    pub fn new_terminal(&mut self) {
        self.new_terminal_with_shell(ShellKind::Default);
    }

    pub fn new_terminal_with_shell(&mut self, shell: ShellKind) {
        let cwd = self
            .workspace
            .root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        self.open_terminal_in(cwd, shell);
    }

    pub fn open_terminal_at(&mut self, path: PathBuf) {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent()
                .map(|p| p.to_path_buf())
                .or_else(|| self.workspace.root.clone())
                .unwrap_or_default()
        };
        self.open_terminal_in(dir, ShellKind::Default);
    }

    fn open_terminal_in(&mut self, cwd: PathBuf, shell: ShellKind) {
        let (cols, rows) = self.bottom.terminal_size;
        let cols = cols.max(20);
        let rows = rows.max(4);
        if let Some(session) =
            crate::terminal::TerminalSession::new_with_shell(&cwd, cols, rows, shell)
        {
            self.bottom.terminals.push(session);
            self.bottom.active_terminal = self.bottom.terminals.len() - 1;
            self.bottom.terminal_size = (cols, rows);
            self.bottom.terminal_focus = true;
            self.bottom.visible = true;
            self.bottom.tab = BottomTab::Terminal;
        }
    }

    pub fn close_active_terminal(&mut self) {
        if self.bottom.terminals.is_empty() {
            return;
        }
        let idx = self
            .bottom
            .active_terminal
            .min(self.bottom.terminals.len() - 1);
        self.bottom.terminals.remove(idx);
        self.bottom.active_terminal = self
            .bottom
            .active_terminal
            .min(self.bottom.terminals.len().saturating_sub(1));
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    pub fn dispatch(&mut self, command: Command, ctx: &egui::Context) {
        if self
            .active_editor()
            .is_some_and(|tab| tab.diff.is_some())
            && (command.is_editor_command()
                || matches!(
                    command,
                    Command::Save
                        | Command::SaveAs
                        | Command::SaveAll
                        | Command::FormatDocument
                ))
        {
            return;
        }
        match command {
            Command::NewFile => self.new_untitled(),
            Command::NewFolder => {
                if let Some(root) = self.workspace.root.clone() {
                    self.start_tree_edit(TreeEditKind::NewFolder, root, None);
                    self.sidebar.visible = true;
                    self.sidebar.mode = SidebarMode::Explorer;
                }
            }
            Command::OpenFile => self.open_file_dialog(),
            Command::OpenFolder => self.open_folder_dialog(),
            Command::Save => self.save_active(),
            Command::SaveAs => self.save_active_as(),
            Command::SaveAll => self.save_all(),
            Command::CloseTab => self.close_active_tab(),
            Command::CloseAllTabs => self.close_all_tabs(),
            Command::ReloadFile => self.reload_active(),
            Command::Undo => {
                if let Some(tab) = self.tabs.get_mut(self.active)
                    && tab.doc.can_undo()
                {
                    tab.undo();
                }
            }
            Command::Redo => {
                if let Some(tab) = self.tabs.get_mut(self.active)
                    && tab.doc.can_redo()
                {
                    tab.redo();
                }
            }
            Command::Cut => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.copy_selection(ctx);
                    tab.delete_selection();
                }
            }
            Command::Copy => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.copy_selection(ctx);
                }
            }
            Command::Paste => {
                if let Ok(mut clipboard) = arboard::Clipboard::new()
                    && let Ok(text) = clipboard.get_text()
                    && let Some(tab) = self.tabs.get_mut(self.active)
                {
                    tab.insert_text(&text);
                }
            }
            Command::SelectAll => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.select_all();
                }
            }
            Command::SelectNextOccurrence => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.select_next_occurrence();
                }
            }
            Command::SelectAllOccurrences => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.select_all_occurrences();
                }
            }
            Command::AddCursorUp => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.add_cursor_vertical(-1);
                }
            }
            Command::AddCursorDown => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.add_cursor_vertical(1);
                }
            }
            Command::Find => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.open_find(false);
                }
            }
            Command::Replace => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.open_find(true);
                }
            }
            Command::FindNext => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.find_next(true);
                }
            }
            Command::FindPrev => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.find_next(false);
                }
            }
            Command::GotoLine => self.open_palette(PaletteMode::GotoLine),
            Command::GotoSymbol => self.open_palette(PaletteMode::Symbols),
            Command::GotoDefinition => {
                if let Some(ident) = self
                    .active_editor()
                    .map(|e| e.identifier_at(e.caret))
                    .filter(|s| !s.is_empty())
                {
                    self.goto_definition_by_name(&ident);
                }
            }
            Command::FormatDocument => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.format_document();
                    self.toast("已格式化文档", ToastKind::Info);
                }
            }
            Command::TriggerSuggest => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.trigger_completion();
                }
            }
            Command::ToggleComment => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.toggle_comment();
                }
            }
            Command::DuplicateLine => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.duplicate_line();
                }
            }
            Command::DeleteLine => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.delete_line();
                }
            }
            Command::MoveLineUp => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.move_lines(true);
                }
            }
            Command::MoveLineDown => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.move_lines(false);
                }
            }
            Command::Indent => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.indent_selection(false);
                }
            }
            Command::Outdent => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.indent_selection(true);
                }
            }
            Command::ToggleFold => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.toggle_fold_caret();
                }
            }
            Command::FoldAll => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.fold_all();
                }
            }
            Command::UnfoldAll => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.unfold_all();
                }
            }
            Command::ShowPalette => self.open_palette(PaletteMode::Commands),
            Command::QuickOpen => self.open_palette(PaletteMode::QuickOpen),
            Command::ToggleSidebar => self.sidebar.visible = !self.sidebar.visible,
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::NewTerminal => self.new_terminal(),
            Command::ShowProblems => {
                self.bottom.visible = true;
                self.bottom.tab = BottomTab::Problems;
            }
            Command::ShowOutput => {
                self.bottom.visible = true;
                self.bottom.tab = BottomTab::Output;
            }
            Command::SplitEditor => self.split_editor_right(),
            Command::ToggleMarkdownPreview => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.language == Language::Markdown {
                        tab.md_mode = match tab.md_mode {
                            crate::markdown::MarkdownViewMode::Preview => {
                                crate::markdown::MarkdownViewMode::Source
                            }
                            _ => crate::markdown::MarkdownViewMode::Preview,
                        };
                    } else {
                        self.toast("当前文件不是 Markdown 文档", ToastKind::Info);
                    }
                }
            }
            Command::MarkdownPreviewSide => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.language == Language::Markdown {
                        let next = match tab.md_mode {
                            crate::markdown::MarkdownViewMode::Split => {
                                crate::markdown::MarkdownViewMode::Source
                            }
                            _ => crate::markdown::MarkdownViewMode::Split,
                        };
                        if next == crate::markdown::MarkdownViewMode::Split {
                            tab.md_preview.split_synced = false;
                        }
                        tab.md_mode = next;
                    } else {
                        self.toast("当前文件不是 Markdown 文档", ToastKind::Info);
                    }
                }
            }
            Command::ToggleWordWrap => {
                self.settings.word_wrap = !self.settings.word_wrap;
                self.settings.save();
            }
            Command::ToggleMinimap => {
                self.settings.show_minimap = !self.settings.show_minimap;
                self.settings.save();
            }
            Command::ToggleStickyScroll => {
                self.settings.show_sticky_scroll = !self.settings.show_sticky_scroll;
                self.settings.save();
            }
            Command::ToggleWhitespace => {
                self.settings.show_whitespace = !self.settings.show_whitespace;
                self.settings.save();
            }
            Command::ToggleIndentGuides => {
                self.settings.show_indent_guides = !self.settings.show_indent_guides;
                self.settings.save();
            }
            Command::ZoomIn => {
                self.set_ui_zoom(self.settings.ui_zoom + 0.1, ctx);
            }
            Command::ZoomOut => {
                self.set_ui_zoom(self.settings.ui_zoom - 0.1, ctx);
            }
            Command::ZoomReset => {
                self.set_ui_zoom(1.0, ctx);
            }
            Command::ThemeDark => self.set_theme_kind(ThemeKind::Dark, ctx),
            Command::ThemeLight => self.set_theme_kind(ThemeKind::Light, ctx),
            Command::OpenSettings => {
                self.open_settings();
            }
            Command::SetLanguage => self.open_palette(PaletteMode::Language),
            Command::ShowExplorer => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Explorer;
            }
            Command::ShowSearch => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Search;
                self.workspace.search_panel.focus = true;
            }
            Command::ShowGit => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Git;
            }
            Command::ShowOutline => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Outline;
            }
            Command::NextTab => self.cycle_group_tab(true),
            Command::PrevTab => self.cycle_group_tab(false),
            Command::CopyPath => {
                if let Some(path) = self.active_path() {
                    ctx.copy_text(path.to_string_lossy().to_string());
                    self.toast("已复制路径", ToastKind::Info);
                }
            }
            Command::RevealInExplorer => {
                if let Some(path) = self.active_path() {
                    crate::workspace::tree::reveal_in_explorer(&path);
                }
            }
            Command::RefreshExplorer => {
                self.workspace.tree.reload();
                if let Some(root) = self.workspace.root.clone() {
                    self.workspace.index.scan(&root);
                    self.git.refresh(&root);
                }
            }
            Command::About => self.dialog = Some(Dialog::About),
            Command::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
            Command::Plugins => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Plugins;
                self.sidebar.plugin_search_focus = true;
            }
        }
    }

    fn set_theme_kind(&mut self, kind: ThemeKind, ctx: &egui::Context) {
        self.settings.theme = kind.as_str().to_string();
        self.settings.save();
        ctx.request_repaint();
    }

    // ------------------------------------------------------------------
    // Toasts
    // ------------------------------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        let text = text.into();
        if let Some(existing) = self.toasts.iter_mut().find(|t| t.text == text) {
            existing.created = Instant::now();
            return;
        }
        self.toasts.push(Toast {
            text,
            kind,
            created: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    // ------------------------------------------------------------------
    // Frame plumbing
    // ------------------------------------------------------------------

    fn apply_settings_theme(&mut self, ctx: &egui::Context) {
        let kind = ThemeKind::from_str(&self.settings.theme);
        if kind != self.applied_theme {
            self.theme = Theme::by_kind(kind);
            self.theme.apply(ctx);
            self.applied_theme = kind;
            for tab in &mut self.tabs {
                tab.invalidate_layout();
            }
        }
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let file = self
            .active_editor()
            .map(|e| {
                let dirty = if e.is_dirty() { " •" } else { "" };
                format!("{}{}", e.title(), dirty)
            })
            .unwrap_or_default();
        let folder = self
            .workspace
            .root
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = match (file.is_empty(), folder.is_empty()) {
            (true, true) => APP_NAME.to_string(),
            (true, false) => format!("{folder} — {APP_NAME}"),
            (false, true) => format!("{file} — {APP_NAME}"),
            (false, false) => format!("{file} — {folder} — {APP_NAME}"),
        };
        if title != self.last_title {
            self.last_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        if self.palette.open || self.dialog.is_some() {
            return;
        }
        // Handle UI zoom shortcuts (Ctrl + = / + / - / 0), including Shift+= for '+'
        let zoom_cmd = ctx.input_mut(|i| {
            let ctrl = (i.modifiers.ctrl || i.modifiers.command) && !i.modifiers.alt;
            if !ctrl {
                return None;
            }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::Equals)
                || i.consume_key(egui::Modifiers::CTRL, egui::Key::Plus)
                || (i.modifiers.shift
                    && (i.consume_key(
                        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                        egui::Key::Plus,
                    ) || i.consume_key(
                        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                        egui::Key::Equals,
                    )))
            {
                Some(Command::ZoomIn)
            } else if i.consume_key(egui::Modifiers::CTRL, egui::Key::Minus) {
                Some(Command::ZoomOut)
            } else if i.consume_key(egui::Modifiers::CTRL, egui::Key::Num0) {
                Some(Command::ZoomReset)
            } else {
                None
            }
        });
        if let Some(cmd) = zoom_cmd {
            self.dispatch(cmd, ctx);
            return;
        }
        let mut candidates: Vec<(Command, crate::commands::Shortcut)> = Command::ALL
            .iter()
            .filter(|command| !command.is_editor_command())
            .filter_map(|command| {
                self.custom_shortcuts
                    .get(command)
                    .copied()
                    .or_else(|| command.shortcut())
                    .map(|shortcut| (*command, shortcut))
            })
            .collect();
        candidates.sort_by_key(|(_, s)| {
            (
                std::cmp::Reverse((s.mods.shift as u8) * 2 + (s.mods.alt as u8)),
                0,
            )
        });
        // Read modifiers from the key events themselves: the current modifier
        // state is already cleared when a quick tap is processed in the next
        // frame, which used to swallow Ctrl shortcuts. Exact matching keeps
        // Ctrl+Shift+X from firing a Ctrl+X binding.
        let pressed: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        let mut handled = false;
        for (command, shortcut) in candidates {
            if !pressed
                .iter()
                .any(|(key, mods)| *key == shortcut.key && mods.matches_exact(shortcut.mods))
            {
                continue;
            }
            if ctx.input_mut(|i| i.consume_key(shortcut.mods, shortcut.key)) {
                self.dispatch(command, ctx);
                handled = true;
                break;
            }
        }
        if handled {
            return;
        }
        for (index, shortcut) in self.plugins.shortcut_candidates() {
            if !pressed
                .iter()
                .any(|(key, mods)| *key == shortcut.key && mods.matches_exact(shortcut.mods))
            {
                continue;
            }
            if ctx.input_mut(|i| i.consume_key(shortcut.mods, shortcut.key)) {
                self.run_plugin_command(index);
                break;
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested {
            return;
        }
        if self.close_confirmed || !self.tabs.iter().any(|t| t.is_dirty()) {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.dialog = Some(Dialog::ConfirmClose);
    }

    fn pump_background(&mut self, ctx: &egui::Context) {
        if self.workspace.index.poll() {
            self.workspace.index_version += 1;
            self.palette.needs_filter = true;
            if self.workspace.pending_search {
                self.workspace.pending_search = false;
                self.start_project_search();
            }
            if !self.workspace.search_panel.last_query.is_empty()
                && self.workspace.search_panel.last_typed.is_some()
            {
                self.workspace.pending_search = true;
            }
        }
        self.poll_search();
        self.git.poll();

        if let Some(watcher) = &self.workspace.watcher {
            let events = watcher.poll();
            if !events.is_empty() {
                for path in &events {
                    if is_save_temp(path) {
                        continue;
                    }
                    if let Some(parent) = path.parent() {
                        self.workspace.tree.reload_dir(parent);
                    }
                }
                for path in events {
                    if is_save_temp(&path) {
                        continue;
                    }
                    self.handle_disk_change(&path);
                    let diff_tabs: Vec<u64> = self
                        .tabs
                        .iter()
                        .filter(|tab| {
                            tab.diff
                                .as_ref()
                                .is_some_and(|diff| diff.path == path)
                        })
                        .map(|tab| tab.id)
                        .collect();
                    for id in diff_tabs {
                        self.spawn_diff_load(id);
                    }
                }
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
            }
        }

        for term in &mut self.bottom.terminals {
            term.update();
        }

        let mut diff_loading = false;
        for tab in &mut self.tabs {
            if let Some(diff) = &mut tab.diff {
                if diff.poll() {
                    ctx.request_repaint();
                }
                diff_loading |= diff.loading;
            }
        }
        if diff_loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        if let Some(typed) = self.workspace.search_panel.last_typed
            && typed.elapsed() > Duration::from_millis(320)
            && self.workspace.search_panel.query != self.workspace.search_panel.last_query
            && !self.workspace.search_panel.scanning
        {
            self.workspace.search_panel.last_typed = None;
            self.start_project_search();
        }

        if self.settings.autosave && self.last_autosave.elapsed() > Duration::from_millis(900) {
            self.last_autosave = Instant::now();
            let mut any_saved = false;
            let mut blocked = 0usize;
            for editor in &mut self.tabs {
                if !editor.is_dirty()
                    || editor.doc.path.is_none()
                    || editor.last_edit.elapsed() <= Duration::from_millis(1200)
                {
                    continue;
                }
                match editor.doc.save() {
                    SaveOutcome::Saved => {
                        editor.preview = false;
                        editor.diff_version = 0;
                        any_saved = true;
                    }
                    SaveOutcome::DiskChanged | SaveOutcome::ReadOnly | SaveOutcome::Failed(_) => {
                        blocked += 1;
                    }
                }
            }
            if any_saved && let Some(root) = self.workspace.root.clone() {
                self.git.refresh(&root);
            }
            if blocked > 0 && self.autosave_notice.is_none_or(|at| at.elapsed() > Duration::from_secs(10))
            {
                self.autosave_notice = Some(Instant::now());
                self.toast(
                    format!("自动保存跳过 {blocked} 个文件: 磁盘已变更、只读或写入失败"),
                    ToastKind::Warning,
                );
            }
        }

        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if focused
            && !self.window_focused
            && self.last_reload_check.elapsed() > Duration::from_millis(500)
        {
            self.last_reload_check = Instant::now();
            let paths: Vec<PathBuf> = self
                .tabs
                .iter()
                .filter_map(|tab| tab.doc.path.clone())
                .collect();
            for path in paths {
                self.handle_disk_change(&path);
            }
            if let Some(root) = self.workspace.root.clone() {
                self.git.refresh(&root);
            }
        }
        self.session_autosave();
        self.window_focused = focused;

        self.toasts
            .retain(|toast| toast.created.elapsed() < Duration::from_millis(3600));
    }

    pub fn set_ui_zoom(&mut self, zoom: f32, ctx: &egui::Context) {
        let clamped = ((zoom.clamp(0.7, 1.8) * 10.0).round() / 10.0).clamp(0.7, 1.8);
        self.settings.ui_zoom = clamped;
        ctx.set_zoom_factor(clamped);
        self.settings.save();
        self.toast(format!("界面缩放: {:.0}%", clamped * 100.0), ToastKind::Info);
    }

    pub fn open_in_default_app(&mut self, path: &Path) {
        #[cfg(target_os = "windows")]
        let res = std::process::Command::new("explorer").arg(path).spawn();
        #[cfg(target_os = "macos")]
        let res = std::process::Command::new("open").arg(path).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let res = std::process::Command::new("xdg-open").arg(path).spawn();

        if let Err(err) = res {
            self.toast(format!("无法调用系统程序打开: {err}"), ToastKind::Error);
        }
    }
    fn handle_window_resize(&self, ctx: &egui::Context) {
        let (maximized, fullscreen) = ctx.input(|i| {
            let vp = i.viewport();
            (
                vp.maximized.unwrap_or(false),
                vp.fullscreen.unwrap_or(false),
            )
        });
        if maximized || fullscreen {
            return;
        }
        let rect = ctx.content_rect();
        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
            let edge = 5.0;
            let corner = 10.0;
            let left = pos.x - rect.left() <= edge;
            let right = rect.right() - pos.x <= edge;
            let top = pos.y - rect.top() <= edge;
            let bottom = rect.bottom() - pos.y <= edge;
            let corner_left = pos.x - rect.left() <= corner;
            let corner_right = rect.right() - pos.x <= corner;
            let corner_top = pos.y - rect.top() <= corner;
            let corner_bottom = rect.bottom() - pos.y <= corner;

            let dir = if (top && corner_left) || (left && corner_top) {
                Some(egui::ResizeDirection::NorthWest)
            } else if (top && corner_right) || (right && corner_top) {
                Some(egui::ResizeDirection::NorthEast)
            } else if (bottom && corner_left) || (left && corner_bottom) {
                Some(egui::ResizeDirection::SouthWest)
            } else if (bottom && corner_right) || (right && corner_bottom) {
                Some(egui::ResizeDirection::SouthEast)
            } else if top {
                Some(egui::ResizeDirection::North)
            } else if bottom {
                Some(egui::ResizeDirection::South)
            } else if left {
                Some(egui::ResizeDirection::West)
            } else if right {
                Some(egui::ResizeDirection::East)
            } else {
                None
            };

            if let Some(dir) = dir {
                let cursor = match dir {
                    egui::ResizeDirection::North | egui::ResizeDirection::South => {
                        egui::CursorIcon::ResizeVertical
                    }
                    egui::ResizeDirection::East | egui::ResizeDirection::West => {
                        egui::CursorIcon::ResizeHorizontal
                    }
                    egui::ResizeDirection::NorthWest | egui::ResizeDirection::SouthEast => {
                        egui::CursorIcon::ResizeNwSe
                    }
                    egui::ResizeDirection::NorthEast | egui::ResizeDirection::SouthWest => {
                        egui::CursorIcon::ResizeNeSw
                    }
                };
                ctx.set_cursor_icon(cursor);
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                }
            }
        }
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("kestrel_window_border"),
        ))
        .rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, self.theme.border_strong),
            egui::StrokeKind::Inside,
        );
    }
}

impl eframe::App for KestrelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame += 1;
        let ctx = ui.ctx().clone();
        devshot::maybe_capture(&ctx, &mut self.shot_frame);
        if cfg!(debug_assertions) && ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            devshot::capture_now(&ctx);
        }
        if cfg!(debug_assertions) && ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if self.frame == 15 {
            match self.dev_ui.as_deref() {
                Some("light") => {
                    self.settings.theme = "light".to_string();
                }
                Some("light_collapsed") => {
                    self.settings.theme = "light".to_string();
                    self.workspace.tree.collapse_all();
                    self.workspace.tree.collapse_all();
                }
                _ => {}
            }
        }
        self.apply_settings_theme(&ctx);
        let target_zoom = self.settings.ui_zoom.clamp(0.7, 1.8);
        if (ctx.zoom_factor() - target_zoom).abs() > 0.001 {
            ctx.set_zoom_factor(target_zoom);
        }
        self.pump_background(&ctx);
        self.plugin_tick(&ctx);
        self.handle_global_shortcuts(&ctx);
        self.handle_close_request(&ctx);
        self.update_window_title(&ctx);

        self.root_ui(ui);
        self.handle_drag_and_drop_overlay(&ctx);
        self.handle_os_file_drops(&ctx);
        self.palette_ui(&ctx);
        self.dialogs_ui(&ctx);
        self.toasts_ui(&ctx);
        self.handle_window_resize(&ctx);
    }

    fn on_exit(&mut self) {
        if self.plugins.has_handlers(Event::Shutdown) {
            let ctx = self.plugin_context(None);
            self.plugins.emit(Event::Shutdown, &ctx);
        }
        if std::env::var("KESTREL_SHOT").is_err() {
            self.settings.sidebar_width = self.sidebar.width;
            self.settings.terminal_height = self.bottom.height;
            self.settings.save();
            self.save_session();
        }
    }
}

impl KestrelApp {
    fn drop_spot_at(&self, pos: egui::Pos2) -> Option<DropSpot> {
        let area = self.editor_area_rect?;
        if !area.contains(pos) {
            return None;
        }
        if self.tabs.is_empty() {
            return Some(DropSpot::Center(0));
        }
        for (index, rect) in self.group_rects.iter().enumerate() {
            if !rect.contains(pos) {
                continue;
            }
            if pos.y < rect.top() + TABBAR_H {
                // Tab rows handle their own drops.
                return None;
            }
            let edge = (rect.width() * 0.22).clamp(48.0, 150.0);
            if pos.x - rect.left() < edge {
                return Some(DropSpot::Left(index));
            }
            if rect.right() - pos.x < edge {
                return Some(DropSpot::Right(index));
            }
            return Some(DropSpot::Center(index));
        }
        None
    }

    fn drop_preview_target(&self, spot: DropSpot) -> Option<(egui::Rect, String)> {
        let rect = *self.group_rects.get(match spot {
            DropSpot::Center(g) | DropSpot::Left(g) | DropSpot::Right(g) => g,
        })?;
        let top = rect.top() + TABBAR_H;
        let content = egui::Rect::from_min_max(
            egui::pos2(rect.left(), top),
            egui::pos2(rect.right(), rect.bottom()),
        );
        Some(match spot {
            DropSpot::Center(_) => (content, "释放以在该分屏中打开".to_string()),
            DropSpot::Left(_) => {
                let half = egui::Rect::from_min_max(
                    content.min,
                    egui::pos2(content.center().x, content.bottom()),
                );
                (half, "释放以在左侧新建分屏".to_string())
            }
            DropSpot::Right(_) => {
                let half = egui::Rect::from_min_max(
                    egui::pos2(content.center().x, content.top()),
                    content.max,
                );
                (half, "释放以在右侧新建分屏".to_string())
            }
        })
    }

    fn update_drop_preview(&mut self, ctx: &egui::Context, target: Option<(egui::Rect, String)>) {
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
        let k = 1.0 - (-dt * 20.0).exp();
        match (&mut self.drop_preview, target) {
            (Some(preview), Some((rect, hint))) => {
                preview.rect = lerp_rect(preview.rect, rect, k);
                preview.alpha += (1.0 - preview.alpha) * k;
                preview.hint = hint;
            }
            (None, Some((rect, hint))) => {
                self.drop_preview = Some(DropPreview {
                    rect,
                    alpha: 0.0,
                    hint,
                });
            }
            (Some(preview), None) => {
                preview.alpha -= preview.alpha * k;
                if preview.alpha < 0.02 {
                    self.drop_preview = None;
                }
            }
            (None, None) => {}
        }
    }

    fn paint_drop_preview(&self, ctx: &egui::Context) {
        let Some(preview) = &self.drop_preview else {
            return;
        };
        let a = preview.alpha.clamp(0.0, 1.0);
        if a <= 0.01 {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drag_drop_zone"),
        ));
        painter.rect_filled(
            preview.rect,
            4.0,
            with_alpha(self.theme.accent, 0.14 * a),
        );
        painter.rect_stroke(
            preview.rect,
            4.0,
            egui::Stroke::new(1.5, with_alpha(self.theme.accent, a)),
            egui::StrokeKind::Inside,
        );
        let pill = egui::Rect::from_center_size(
            preview.rect.center(),
            egui::Vec2::new(196.0, 30.0),
        );
        painter.rect_filled(pill, 6.0, with_alpha(self.theme.overlay_bg, a));
        painter.rect_stroke(
            pill,
            6.0,
            egui::Stroke::new(1.0, with_alpha(self.theme.accent, a)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            pill.center(),
            egui::Align2::CENTER_CENTER,
            &preview.hint,
            egui::FontId::new(12.0, self.ui_family.clone()),
            with_alpha(self.theme.text, a),
        );
    }

    fn open_file_in_new_group(&mut self, path: PathBuf, at: usize) {
        self.ensure_groups();
        let at = at.min(self.groups.len());
        let neighbour = at.saturating_sub(1).min(self.groups.len() - 1);
        let weight = self.groups[neighbour].weight;
        let mut group = EditorGroup::new();
        group.weight = weight * 0.5;
        self.groups[neighbour].weight = weight * 0.5;
        self.groups.insert(at, group);
        self.active_group = at;
        self.sync_active();
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.doc.path.as_deref() == Some(path.as_path()))
        {
            let id = self.tabs[index].id;
            self.show_view_in_group(id, at);
            return;
        }
        self.open_file(path);
    }

    fn apply_editor_drop(&mut self, payload: DragPayload, spot: DropSpot) {
        match payload {
            DragPayload::Tab { id, group } => match spot {
                DropSpot::Center(target) => {
                    if group != target {
                        self.move_tab_to_group(id, group, target, usize::MAX);
                    }
                }
                DropSpot::Left(target) => self.split_tab_to_new_group(id, group, target),
                DropSpot::Right(target) => {
                    self.split_tab_to_new_group(id, group, target + 1)
                }
            },
            DragPayload::ExplorerPath(path) if path.is_dir() => {
                if matches!(spot, DropSpot::Center(_)) {
                    self.open_folder(path);
                }
            }
            DragPayload::ExplorerPath(path) => match spot {
                DropSpot::Center(group) => self.open_path_in_group(path, group),
                DropSpot::Left(group) => self.open_file_in_new_group(path, group),
                DropSpot::Right(group) => self.open_file_in_new_group(path, group + 1),
            },
        }
    }

    fn handle_drag_and_drop_overlay(&mut self, ctx: &egui::Context) {
        let payload = self.drag_payload.clone();
        let pointer_pos = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let primary_down = ctx.input(|i| i.pointer.primary_down());

        let spot = payload
            .as_ref()
            .and(pointer_pos)
            .and_then(|pos| self.drop_spot_at(pos));
        let target = spot.and_then(|spot| self.drop_preview_target(spot));
        self.update_drop_preview(ctx, target);
        self.paint_drop_preview(ctx);

        let Some(payload) = payload else {
            return;
        };
        let label = match &payload {
            DragPayload::Tab { id, .. } => self
                .tab_index(*id)
                .and_then(|index| self.tabs.get(index))
                .map(|tab| tab.title())
                .unwrap_or_else(|| "标签页".to_string()),
            DragPayload::ExplorerPath(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        };

        if let Some(pos) = pointer_pos {
            let pill_rect = egui::Rect::from_min_size(
                pos + egui::Vec2::new(12.0, 12.0),
                egui::Vec2::new(
                    (label.chars().count() as f32 * 9.0 + 28.0).clamp(72.0, 220.0),
                    26.0,
                ),
            );
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::unique("drag_floating_pill"),
            ));
            painter.rect_filled(pill_rect, 6.0, self.theme.overlay_bg);
            painter.rect_stroke(
                pill_rect,
                6.0,
                egui::Stroke::new(1.0, self.theme.accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                pill_rect.center(),
                egui::Align2::CENTER_CENTER,
                &label,
                egui::FontId::new(11.5, self.ui_family.clone()),
                self.theme.text,
            );
        }

        if !primary_down {
            if let Some(spot) = spot {
                self.apply_editor_drop(payload, spot);
            }
            self.drag_payload = None;
        }
    }

    fn handle_os_file_drops(&mut self, ctx: &egui::Context) {
        let hovered = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let pointer_pos = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let spot = pointer_pos.and_then(|pos| self.drop_spot_at(pos));
        if hovered {
            let target = spot.and_then(|spot| self.drop_preview_target(spot));
            self.update_drop_preview(ctx, target);
            self.paint_drop_preview(ctx);
        }

        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        for file in dropped {
            let path = file.path().to_path_buf();
            if path.is_dir() {
                self.open_folder(path);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            match spot {
                Some(DropSpot::Center(group)) => self.open_path_in_group(path, group),
                Some(DropSpot::Left(group)) => self.open_file_in_new_group(path, group),
                Some(DropSpot::Right(group)) => self.open_file_in_new_group(path, group + 1),
                None => self.open_file(path),
            }
        }
    }

}

/// Parse user keybinding overrides from settings into command shortcuts.
fn build_custom_shortcuts(settings: &Settings) -> std::collections::HashMap<Command, Shortcut> {
    let mut map = std::collections::HashMap::new();
    for (id, text) in &settings.keybindings {
        let Some(command) = Command::from_id(id) else {
            continue;
        };
        for shortcut in Shortcut::parse_list(text) {
            map.insert(command, shortcut);
        }
    }
    map
}
