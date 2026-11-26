//! Application state, file/tab lifecycle, Git/LSP orchestration, command dispatch, and background work.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::FontFamily;

use crate::commands::{Command, Shortcut};
use crate::devshot;
use crate::doc::{Document, Encoding, SaveOutcome};
use crate::editor::{Editor, EditorStyle, FormatOutcome};
use crate::fuzzy;
use crate::git::{GitFileStatus, GitService};
use crate::highlight::Language;
use crate::lsp::{DiagSeverity, OutlineSymbol};
use crate::plugin::{Event, PluginHost};
use crate::settings::{
    GroupSession, Session, Settings, TabSession, TerminalSessionState, UntitledSession,
};
use crate::terminal::ShellKind;
use crate::theme::{Theme, ThemeKind};
use crate::ui::UiStyle;
use crate::workspace::{
    FileIndex, FileTree, FsWatcher, ProjectSearch, SearchEvent, SearchQuery, SearchResult,
};

pub const APP_NAME: &str = "Kestrel";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Watcher bursts wait this long before touching the file index.
const INDEX_DEBOUNCE: Duration = Duration::from_millis(500);

/// Files above this size open as a read-only preview instead of an editable buffer.
pub const MAX_OPEN_FILE_BYTES: u64 = 8 * 1024 * 1024;
const LARGE_FILE_TOAST: &str =
    "文件超过 8MB，已用只读模式打开，语法高亮与 minimap 已关闭";

fn is_large_file(size: u64) -> bool {
    size > MAX_OPEN_FILE_BYTES
}
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
impl SidebarMode {

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explorer => "explorer",
            Self::Search => "search",
            Self::Git => "git",
            Self::Outline => "outline",
            Self::Plugins => "plugins",
        }
    }

    pub fn from_str(name: &str) -> Self {
        match name {
            "search" => Self::Search,
            "git" => Self::Git,
            "outline" => Self::Outline,
            "plugins" => Self::Plugins,
            _ => Self::Explorer,
        }
    }
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
    /// Multi-selection for batch file operations; holds `selected` when non-empty.
    pub selected_paths: std::collections::HashSet<PathBuf>,
    /// Anchor row for Shift+click range selection.
    pub selection_anchor: Option<PathBuf>,
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
    /// Match file names only; file contents are never read.
    pub filename_only: bool,
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
    /// Watcher paths waiting for the 500 ms index debounce.
    pub index_dirty: Vec<PathBuf>,
    pub index_dirty_at: Option<Instant>,
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
            index_dirty: Vec::new(),
            index_dirty_at: None,
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

impl BottomTab {

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Problems => "problems",
            Self::Output => "output",
        }
    }

    pub fn from_str(name: &str) -> Self {
        match name {
            "problems" => Self::Problems,
            "output" => Self::Output,
            _ => Self::Terminal,
        }
    }
}
/// Maximum simultaneous side-by-side split terminal panes.
pub const MAX_TERMINAL_PANES: usize = 4;

pub struct BottomPanel {
    pub visible: bool,
    pub tab: BottomTab,
    pub height: f32,
    pub terminals: Vec<crate::terminal::TerminalSession>,

    /// Saved shell specs for a panel that was closed on exit; spawned when reopened.
    pub pending_terminals: Vec<(ShellKind, PathBuf)>,
    pub active_terminal: usize,
    pub terminal_size: (u16, u16),
    pub terminal_focus: bool,

    /// Kept from the last frame so global shortcuts skip keys the terminal owns.
    pub terminal_has_focus: bool,
    /// Indices into `terminals` currently visible side-by-side (1..=MAX_TERMINAL_PANES).
    pub panes: Vec<usize>,
    /// Relative horizontal weights matching `panes`.
    pub pane_weights: Vec<f32>,
    /// Index into `panes` identifying the currently focused pane slot.
    pub focused_pane: usize,
    /// Maximize the bottom panel height to fill most of the window.
    pub maximized: bool,
    /// Active inline rename state: `(terminal_id, draft_name, needs_focus)`.
    pub renaming_terminal: Option<(u64, String, bool)>,
    /// New terminals start in the active editor's directory instead of the project root.
    pub follow_active_dir: bool,
}

impl BottomPanel {
    fn new() -> Self {
        Self {
            visible: false,
            tab: BottomTab::Terminal,
            height: 240.0,
            terminals: Vec::new(),
            pending_terminals: Vec::new(),
            active_terminal: 0,
            terminal_size: (0, 0),
            terminal_focus: false,
            terminal_has_focus: false,
            panes: Vec::new(),
            pane_weights: Vec::new(),
            focused_pane: 0,
            maximized: false,
            renaming_terminal: None,
            follow_active_dir: false,
        }
    }

    pub fn is_split(&self) -> bool {
        self.panes.len() >= 2
    }

    /// Ensure `panes`, `pane_weights`, `focused_pane`, and `active_terminal` stay consistent.
    pub fn normalize_panes(&mut self) {
        let len = self.terminals.len();
        if len == 0 {
            self.panes.clear();
            self.pane_weights.clear();
            self.focused_pane = 0;
            self.active_terminal = 0;
            return;
        }
        let mut seen = Vec::new();
        let mut next_panes = Vec::new();
        let mut next_weights = Vec::new();
        for (slot, &idx) in self.panes.iter().enumerate() {
            if idx < len && !seen.contains(&idx) && next_panes.len() < MAX_TERMINAL_PANES {
                seen.push(idx);
                next_panes.push(idx);
                let w = self.pane_weights.get(slot).copied().unwrap_or(1.0);
                next_weights.push(if w.is_finite() && w > 0.1 { w } else { 1.0 });
            }
        }
        if next_panes.is_empty() {
            let fallback = self.active_terminal.min(len - 1);
            next_panes.push(fallback);
            next_weights.push(1.0);
        }
        self.panes = next_panes;
        self.pane_weights = next_weights;
        self.focused_pane = self.focused_pane.min(self.panes.len() - 1);
        self.active_terminal = self.panes[self.focused_pane];
    }

    /// Index of the terminal session that currently owns focus.
    pub fn focused_terminal_idx(&self) -> Option<usize> {
        let len = self.terminals.len();
        if len == 0 {
            return None;
        }
        if let Some(&idx) = self.panes.get(self.focused_pane)
            && idx < len
        {
            return Some(idx);
        }
        Some(self.active_terminal.min(len - 1))
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
    ConfirmDeleteBatch(Vec<PathBuf>),
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
    pub pane_scroll: std::collections::HashMap<u64, (f32, f32)>,
}

impl EditorGroup {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            weight: 1.0,
            tab_scroll: 0.0,
            pane_scroll: std::collections::HashMap::new(),
        }
    }

    pub fn active_id(&self) -> Option<u64> {
        self.tabs.get(self.active).copied()
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|&tab| tab == id)
    }
}

pub const GROUP_GAP: f32 = 1.0;
pub const TABBAR_H: f32 = 34.0;

/// Write text to the real OS system clipboard via `arboard` (needed because `eframe` is
/// built with `default-features = false` without its own clipboard feature).
pub(crate) fn copy_to_clipboard(text: impl Into<String>) {
    let text = text.into();
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text);
    }
}

/// Mirror any `egui::OutputCommand::CopyText` emitted by built-in egui widgets
/// (`TextEdit`, selectable `Label`, etc.) into the real OS clipboard.
fn sync_egui_copy_to_os_clipboard(ctx: &egui::Context) {
    let copied: Vec<String> = ctx.output(|o| {
        o.commands
            .iter()
            .filter_map(|cmd| match cmd {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    });
    if let Some(last) = copied.into_iter().last() {
        copy_to_clipboard(last);
    }
}

/// When an egui `TextEdit` owns keyboard focus and `eframe` lacks its built-in
/// clipboard feature, synthesize `Event::Paste` on `Ctrl+V` from `arboard`.
fn inject_os_paste_for_text_edits(ctx: &egui::Context, editor_focused: bool, terminal_focused: bool) {
    if editor_focused || terminal_focused || !ctx.egui_wants_keyboard_input() {
        return;
    }
    let needs_paste = ctx.input_mut(|i| {
        let already_pasting = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
        if already_pasting {
            return false;
        }
        let mods = i.modifiers;
        let ctrl = (mods.ctrl || mods.command) && !mods.alt && !mods.shift;
        ctrl && i.consume_key(egui::Modifiers::COMMAND, egui::Key::V)
    });
    if needs_paste
        && let Ok(mut cb) = arboard::Clipboard::new()
        && let Ok(text) = cb.get_text()
        && !text.is_empty()
    {
        ctx.input_mut(|i| i.events.push(egui::Event::Paste(text)));
    }
}

/// Validate and open an external URL via the OS handler without invoking a shell.
pub(crate) fn is_safe_external_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
}

pub(crate) fn open_external_url(url: &str) -> bool {
    if !is_safe_external_url(url) {
        return false;
    }
    let trimmed = url.trim();
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                hwnd: *mut std::ffi::c_void,
                lp_operation: *const u16,
                lp_file: *const u16,
                lp_parameters: *const u16,
                lp_directory: *const u16,
                n_show_cmd: i32,
            ) -> isize;
        }
        let op: Vec<u16> = OsStr::new("open").encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = OsStr::new(trimmed).encode_wide().chain(Some(0)).collect();
        // SAFETY: `op` and `target` are NUL-terminated UTF-16 slices live for the call.
        let rc = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1, // SW_SHOWNORMAL
            )
        };
        rc > 32
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(trimmed).spawn().is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(trimmed).spawn().is_ok()
    }
}

/// First line of a Git failure for a toast; the full text already went to the log.
fn error_summary(error: &str) -> String {
    let mut lines = error.lines().filter(|line| !line.trim().is_empty());
    match (lines.next(), lines.next()) {
        (Some(first), Some(_)) => format!("{} …", first.trim()),
        (Some(first), None) => first.trim().to_string(),
        (None, _) => "未知错误".to_string(),
    }
}

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

/// Status-bar strings, rebuilt only when the value behind them changes.
///
/// The status bar repaints every frame, but the cursor position, diagnostics
/// totals, branch label, indent and file size change only on edits and background
/// updates. Formatting them per frame allocated and re-laid-out text that was
/// immediately thrown away.
#[derive(Default)]
pub struct StatusCache {
    indent_key: (usize, bool),
    indent_text: String,
    pos_key: (u64, usize, usize, usize),
    pos_text: String,
    zoom_key: i32,
    zoom_text: String,
    size_key: (u64, u64),
    size_text: String,
    branch_key: (String, bool, usize, usize),
    branch_label: String,
    branch_counts: String,
    diag_key: (usize, usize),
    diag_errors: usize,
    diag_warnings: usize,
    diag_error_text: String,
    diag_warning_text: String,
}

impl StatusCache {
    /// Indent label, for example 4空格 or 2制表符.
    pub fn indent(&mut self, tab_size: usize, insert_spaces: bool) -> &str {
        if self.indent_key != (tab_size, insert_spaces) {
            self.indent_key = (tab_size, insert_spaces);
            self.indent_text.clear();
            let _ = write!(self.indent_text, "{tab_size}");
            self.indent_text
                .push_str(if insert_spaces { "空格" } else { "制表符" });
        }
        &self.indent_text
    }

    /// Cursor label: line/column, plus the cursor count with extra cursors.
    pub fn cursor(&mut self, doc_version: u64, line: usize, col: usize, cursors: usize) -> &str {
        let key = (doc_version, line, col, cursors);
        if self.pos_key != key {
            self.pos_key = key;
            self.pos_text.clear();
            if cursors <= 1 {
                let _ = write!(self.pos_text, "行 {}, 列 {}", line + 1, col + 1);
            } else {
                let _ = write!(
                    self.pos_text,
                    "行 {}, 列 {} ({} 个光标)",
                    line + 1,
                    col + 1,
                    cursors
                );
            }
        }
        &self.pos_text
    }

    pub fn zoom(&mut self, zoom: f32) -> &str {
        let key = (zoom * 1000.0).round() as i32;
        if self.zoom_key != key {
            self.zoom_key = key;
            self.zoom_text.clear();
            let _ = write!(self.zoom_text, "缩放 {:.0}%", zoom * 100.0);
        }
        &self.zoom_text
    }

    /// File size label; avoids disk metadata syscalls when buffer size is already known.
    pub fn file_size(
        &mut self,
        editor_id: u64,
        doc_version: u64,
        buf_bytes: usize,
        path: Option<&Path>,
    ) -> &str {
        if self.size_key != (editor_id, doc_version) {
            self.size_key = (editor_id, doc_version);
            self.size_text.clear();
            if buf_bytes > 0 {
                self.size_text = crate::viewer::image_view::format_bytes(buf_bytes as u64);
            } else if let Some(path) = path
                && let Ok(meta) = std::fs::metadata(path)
            {
                self.size_text = crate::viewer::image_view::format_bytes(meta.len());
            }
        }
        &self.size_text
    }

    /// Branch label and ahead/behind counters, formatted only when Git state changes.
    pub fn branch(
        &mut self,
        branch: &str,
        modified: bool,
        ahead: usize,
        behind: usize,
    ) -> (&str, &str) {
        let (cached_branch, cached_modified, cached_ahead, cached_behind) = &self.branch_key;
        if cached_branch != branch
            || *cached_modified != modified
            || *cached_ahead != ahead
            || *cached_behind != behind
        {
            self.branch_key = (branch.to_string(), modified, ahead, behind);
            self.branch_label.clear();
            self.branch_label.push_str(branch);
            if modified {
                self.branch_label.push('*');
            }
            self.branch_counts.clear();
            if ahead > 0 || behind > 0 {
                let _ = write!(self.branch_counts, "↑{ahead} ↓{behind}");
            }
        }
        (&self.branch_label, &self.branch_counts)
    }

    /// Document-wide error/warning totals, formatted only when `(errors, warnings)` changes.
    pub fn diagnostics(&mut self, tabs: &[Editor]) -> (usize, usize, &str, &str) {
        let (mut errors, mut warnings) = (0usize, 0usize);
        for tab in tabs {
            for diagnostic in &tab.diagnostics {
                match diagnostic.severity {
                    DiagSeverity::Error => errors += 1,
                    DiagSeverity::Warning => warnings += 1,
                    DiagSeverity::Info => {}
                }
            }
        }
        let key = (errors, warnings);
        if self.diag_key != key || self.diag_error_text.is_empty() {
            self.diag_key = key;
            self.diag_errors = errors;
            self.diag_warnings = warnings;
            self.diag_error_text = errors.to_string();
            self.diag_warning_text = warnings.to_string();
        }
        (
            self.diag_errors,
            self.diag_warnings,
            &self.diag_error_text,
            &self.diag_warning_text,
        )
    }
}

/// Resolve every active command shortcut (including custom bindings on commands
/// that have no built-in default), sorted so more specific chords match first.
pub(crate) fn effective_shortcut_bindings(
    custom: &std::collections::HashMap<Command, Shortcut>,
) -> Vec<(Command, Shortcut)> {
    let mut bindings: Vec<(Command, Shortcut)> = Command::ALL
        .iter()
        .filter_map(|&command| {
            custom
                .get(&command)
                .copied()
                .or_else(|| command.shortcut())
                .map(|shortcut| (command, shortcut))
        })
        .collect();
    bindings.sort_by_key(|(_, shortcut)| {
        let count = (shortcut.mods.command as u8)
            + (shortcut.mods.ctrl as u8)
            + (shortcut.mods.shift as u8)
            + (shortcut.mods.alt as u8);
        std::cmp::Reverse((count, shortcut.mods.shift as u8, shortcut.mods.alt as u8))
    });
    bindings
}

pub struct KestrelApp {
    pub settings: Settings,
    pub theme: Theme,
    pub applied_theme: ThemeKind,
    pub ui_family: FontFamily,
    pub ui_bold_family: FontFamily,
    pub mono_family: FontFamily,
    pub mono_bold_family: FontFamily,
    pub term_mono_family: FontFamily,
    pub term_mono_bold_family: FontFamily,

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
    /// Recently opened files, newest first; persisted in the session and shown
    /// on the welcome page.
    pub recent_files: Vec<PathBuf>,

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
    /// Kept so background producers (watcher) can wake egui without a UI call site.
    pub egui_ctx: egui::Context,
    /// Cross-frame status-bar strings.
    pub status_cache: StatusCache,
    /// Key events collected this frame; reused so a keystroke does not allocate.
    pressed_keys: Vec<(egui::Key, egui::Modifiers)>,
    title_key: (Option<u64>, u64, bool),
    title_path: Option<PathBuf>,
    title_root: Option<PathBuf>,

    /// Heavy multimodal viewers parsing on worker threads, keyed by editor id.
    viewer_loads: Vec<(u64, crate::viewer::ViewerLoader)>,

    /// Last periodic session write (layout, panels, terminals; not just buffers).
    last_session_save: Instant,
    /// Navigation history behind / ahead of the caret (Alt+← / Alt+→).
    jump_back: Vec<JumpPoint>,
    jump_forward: Vec<JumpPoint>,
    /// Background Git `HEAD` baseline loader so file open never blocks on `git.exe`.
    baseline_tx: Option<std::sync::mpsc::Sender<(u64, PathBuf, PathBuf)>>,
    baseline_rx: std::sync::mpsc::Receiver<(u64, Option<String>)>,
    pending_baselines: usize,
    /// Defer ConPTY spawning for restored visible terminals until after frame 1 paints.
    deferred_terminal_restore: bool,
    deferred_terminal_active: usize,
}

/// A navigation target recorded by goto-definition / goto-line / symbol jumps.
#[derive(Clone)]
struct JumpPoint {
    tab_id: u64,
    path: Option<PathBuf>,
    byte: usize,
}

impl KestrelApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let startup_started = Instant::now();
        let mut settings = Settings::load();
        if let Ok(theme) = std::env::var("KESTREL_THEME") {
            settings.theme = theme;
        }
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        cc.egui_ctx
            .set_zoom_factor(settings.ui_zoom.clamp(0.7, 1.8));
        let font_set =
            crate::fonts::install_with_terminal_font(&cc.egui_ctx, &settings.terminal_font_family);
        let theme = Theme::by_kind(ThemeKind::from_str(&settings.theme));
        theme.apply(&cc.egui_ctx);
        let ui_ready_ms = startup_started.elapsed().as_secs_f64() * 1000.0;
        let plugin_started = std::time::Instant::now();
        let plugins = PluginHost::new(&settings);
        let custom_shortcuts = build_custom_shortcuts(&settings);
        let (_, baseline_rx) = std::sync::mpsc::channel();

        let term_h = settings.terminal_height.max(120.0);
        let mut bottom = BottomPanel::new();
        bottom.height = term_h;

        let mut app = Self {
            sidebar: SidebarUi {
                visible: true,
                mode: SidebarMode::Explorer,
                width: settings.sidebar_width.max(180.0),
                selected: None,
                selected_paths: std::collections::HashSet::new(),
                selection_anchor: None,
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
            mono_bold_family: font_set.mono_bold_family,
            term_mono_family: font_set.term_mono_family,
            term_mono_bold_family: font_set.term_mono_bold_family,
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
            recent_files: Vec::new(),
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
            egui_ctx: cc.egui_ctx.clone(),
            status_cache: StatusCache::default(),
            pressed_keys: Vec::new(),
            title_key: (None, 0, false),
            title_path: None,
            title_root: None,
            viewer_loads: Vec::new(),
            last_session_save: Instant::now(),
            jump_back: Vec::new(),
            jump_forward: Vec::new(),
            baseline_tx: None,
            baseline_rx,
            pending_baselines: 0,
            deferred_terminal_restore: false,
            deferred_terminal_active: 0,
        };
        app.bottom.follow_active_dir = app.settings.terminal_follow_active_dir;
        app.workspace.search_panel.include = app.settings.search_include.clone();
        app.workspace.search_panel.exclude = app.settings.search_exclude.clone();
        app.apply_dev_env();
        let session_started = Instant::now();
        if app.workspace.root.is_none() && app.tabs.is_empty() && app.dev_ui.is_none() {
            app.restore_session();
        }
        let session_ms = session_started.elapsed().as_secs_f64() * 1000.0;
        app.load_plugins();
        app.apply_dev_ui();
        if std::env::var_os("KESTREL_TRACE").is_some() {
            app.plugins.trace_log(format!(
                "启动插件阶段 {:.2}ms",
                plugin_started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if std::env::var_os("KESTREL_PROFILE").is_some() {
            eprintln!(
                "[kestrel profile] startup total={:.1}ms (fonts/theme={:.1}ms, session={:.1}ms)",
                startup_started.elapsed().as_secs_f64() * 1000.0,
                ui_ready_ms,
                session_ms
            );
        }
        app
    }

    // ------------------------------------------------------------------
    // Session / dev hooks
    // ------------------------------------------------------------------

    fn restore_session(&mut self) {
        let mut session = Session::load();
        if let Some(folder) = &session.folder {
            let path = PathBuf::from(folder);
            if path.is_dir() {
                self.open_folder_quiet(path);
            }
        }

        // Split layout is rebuilt from the saved groups; the flat tab list is the
        // fallback for sessions written before groups existed.
        let mut restored_groups: Vec<(Vec<u64>, usize)> = Vec::new();
        if session.groups.is_empty() {
            for tab in &session.tabs {
                let path = PathBuf::from(&tab.path);
                if path.is_file() {
                    self.open_file_quiet(path, Some((tab.caret, tab.scroll)));
                }
            }
        } else {
            let mut opened_by_path: std::collections::HashMap<PathBuf, u64> =
                std::collections::HashMap::new();
            let mut restored_scrolls: Vec<std::collections::HashMap<u64, (f32, f32)>> = Vec::new();
            for saved in &session.groups {
                let mut ids = Vec::new();
                let mut active = 0usize;
                let mut pane_scroll = std::collections::HashMap::new();
                for (position, tab) in saved.tabs.iter().enumerate() {
                    let path = self.canonical(PathBuf::from(&tab.path));
                    if !path.is_file() {
                        continue;
                    }
                    let id = if let Some(&existing_id) = opened_by_path.get(&path) {
                        Some(existing_id)
                    } else {
                        let before = self.tabs.len();
                        self.open_file_quiet(path.clone(), Some((tab.caret, tab.scroll)));
                        if self.tabs.len() > before {
                            let new_id = self.tabs[self.tabs.len() - 1].id;
                            opened_by_path.insert(path, new_id);
                            Some(new_id)
                        } else {
                            None
                        }
                    };
                    if let Some(id) = id {
                        pane_scroll.insert(id, (tab.scroll, 0.0));
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                        if position == saved.active {
                            active = ids.iter().position(|&t| t == id).unwrap_or(0);
                        }
                    }
                }
                if !ids.is_empty() {
                    restored_groups.push((ids, active));
                    restored_scrolls.push(pane_scroll);
                }
            }
            if !restored_groups.is_empty() {
                self.groups = restored_groups
                    .iter()
                    .zip(restored_scrolls)
                    .map(|((ids, active), pane_scroll)| {
                        let mut group = EditorGroup::new();
                        group.tabs = ids.clone();
                        group.active = *active;
                        group.pane_scroll = pane_scroll;
                        group
                    })
                    .collect();
                self.active_group = session.active_group.min(self.groups.len() - 1);
                self.sync_active();
            }
        }
        for entry in std::mem::take(&mut session.untitled) {
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
        if restored_groups.is_empty() {
            if !self.tabs.is_empty() {
                let active = session.active.min(self.tabs.len() - 1);
                self.active = active;
                self.active_group = 0;
                self.ensure_groups();
                let id = self.tabs[active].id;
                if let Some(group) = self.groups.first_mut() {
                    group.active = group.index_of(id).unwrap_or(0);
                }
                self.sync_active();
            }
        } else {
            // Untitled buffers joined the groups; restore each group's active tab.
            for (index, (_, active)) in restored_groups.iter().enumerate() {
                if let Some(group) = self.groups.get_mut(index) {
                    group.active = (*active).min(group.tabs.len().saturating_sub(1));
                }
            }
            self.active_group = session.active_group.min(self.groups.len() - 1);
            self.sync_active();
        }
        self.restore_panel_state(&session);
        // Avoid synchronous `is_file()` syscalls on startup (can stall on dead UNC/network paths).
        self.recent_files = session
            .recent_files
            .iter()
            .take(12)
            .map(PathBuf::from)
            .collect();
        if let Some(editor) = self.tabs.get_mut(self.active) {
            editor.focus_requested = true;
        }
    }

    /// Restore sidebar/bottom-panel geometry, the search query, and terminal shells.
    fn restore_panel_state(&mut self, session: &Session) {
        self.sidebar.visible = session.sidebar_visible;
        if session.sidebar_width >= 180.0 {
            self.sidebar.width = session.sidebar_width;
        }
        self.sidebar.mode = SidebarMode::from_str(&session.sidebar_mode);
        self.bottom.height = session.bottom_height.max(120.0);
        self.bottom.tab = BottomTab::from_str(&session.bottom_tab);
        if !session.bottom_panes.is_empty() {
            self.bottom.panes = session.bottom_panes.clone();
            self.bottom.pane_weights = session.bottom_pane_weights.clone();
        } else if session.bottom_split {
            let ratio = session.bottom_split_ratio.clamp(0.2, 0.8);
            self.bottom.panes = vec![session.active_terminal, session.secondary_terminal];
            self.bottom.pane_weights = vec![ratio, 1.0 - ratio];
        } else {
            self.bottom.panes = vec![session.active_terminal];
            self.bottom.pane_weights = vec![1.0];
        }
        let specs: Vec<(ShellKind, PathBuf)> = session
            .terminals
            .iter()
            .take(4)
            .filter_map(|entry| {
                let cwd = PathBuf::from(&entry.cwd);
                let cwd = if cwd.is_dir() {
                    cwd
                } else {
                    self.workspace
                        .root
                        .clone()
                        .or_else(|| std::env::current_dir().ok())?
                };
                Some((ShellKind::from_str(&entry.shell), cwd))
            })
            .collect();
        self.bottom.pending_terminals = specs;
        if session.bottom_visible {
            self.bottom.visible = true;
            self.bottom.tab = BottomTab::from_str(&session.bottom_tab);
            // Defer ConPTY spawn until frame 2 so frame 1 paints without waiting on PowerShell.
            self.deferred_terminal_restore = !self.bottom.pending_terminals.is_empty();
            self.deferred_terminal_active = session.active_terminal;
        }
        if !session.search_query.is_empty() {
            self.workspace.search_panel.query = session.search_query.clone();
            if self.sidebar.mode == SidebarMode::Search {
                self.workspace.search_panel.last_typed = Some(Instant::now());
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
            "diff" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Git;
                if let Some(root) = self.workspace.root.clone() {
                    let rel = std::env::var("KESTREL_DEV_DIFF")
                        .unwrap_or_else(|_| "src/app.rs".to_string());
                    self.open_file_diff(root.join(rel), false);
                }
            }
            "outline" => {
                self.sidebar.visible = true;
                self.sidebar.mode = SidebarMode::Outline;
            }
            "settings" => {
                self.open_settings();
            }
            "terminal" => {
                self.toggle_terminal();
                // Dev-only screenshot hook: expose the Ctrl+F search row and a cell
                // selection without driving the mouse.
                if let Ok(query) = std::env::var("KESTREL_DEV_QUERY")
                    && let Some(term) = self.bottom.terminals.first_mut()
                {
                    term.open_search();
                    term.search.query = query;
                    term.search.dirty = true;
                    term.begin_selection(1, 0);
                    term.update_selection(2, 18);
                }
            }
            "terminal_split" => {
                self.toggle_terminal();
                if self.bottom.terminals.len() < 2 {
                    self.new_terminal();
                }
                if self.bottom.terminals.len() >= 2 {
                    self.bottom.panes = vec![0, 1];
                    self.bottom.pane_weights = vec![1.0, 1.0];
                    self.bottom.focused_pane = 0;
                    self.bottom.active_terminal = 0;
                }
            }
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
        let mut groups = Vec::new();
        for group in &self.groups {
            let mut tabs = Vec::new();
            let mut active = 0usize;
            for (position, id) in group.tabs.iter().enumerate() {
                let Some(index) = self.tab_index(*id) else {
                    continue;
                };
                let Some(path) = self.tabs[index].doc.path.as_ref() else {
                    continue;
                };
                if position == group.active {
                    active = tabs.len();
                }
                let editor = &self.tabs[index];
                tabs.push(TabSession {
                    path: path.to_string_lossy().to_string(),
                    caret: editor.caret,
                    scroll: editor.scroll_y,
                });
            }
            if !tabs.is_empty() {
                groups.push(GroupSession { tabs, active });
            }
        }
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

        // Closed panels keep their terminals as pending specs so the layout returns
        // intact even when no shell was ever painted this run.
        let terminals: Vec<TerminalSessionState> = if self.bottom.terminals.is_empty() {
            self.bottom
                .pending_terminals
                .iter()
                .take(4)
                .map(|(shell, cwd)| TerminalSessionState {
                    shell: shell.as_str().to_string(),
                    cwd: cwd.to_string_lossy().to_string(),
                })
                .collect()
        } else {
            self.bottom
                .terminals
                .iter()
                .take(4)
                .map(|term| TerminalSessionState {
                    shell: term.shell.as_str().to_string(),
                    cwd: term.cwd.to_string_lossy().to_string(),
                })
                .collect()
        };
        let session = Session {
            folder: self
                .workspace
                .root
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            tabs,
            active,
            untitled: self.untitled_snapshot(),
            groups,
            active_group: self.active_group,
            sidebar_visible: self.sidebar.visible,
            sidebar_width: self.sidebar.width,
            sidebar_mode: self.sidebar.mode.as_str().to_string(),
            bottom_visible: self.bottom.visible,
            bottom_tab: self.bottom.tab.as_str().to_string(),
            bottom_height: self.bottom.height,
            terminals,
            active_terminal: self.bottom.active_terminal,
            bottom_split: self.bottom.is_split(),
            secondary_terminal: self.bottom.panes.get(1).copied().unwrap_or(1),
            bottom_split_ratio: {
                let sum: f32 = self.bottom.pane_weights.iter().take(2).sum();
                if sum > 0.01 {
                    (self.bottom.pane_weights.first().copied().unwrap_or(0.5) / sum)
                        .clamp(0.2, 0.8)
                } else {
                    0.5
                }
            },
            bottom_panes: self.bottom.panes.clone(),
            bottom_pane_weights: self.bottom.pane_weights.clone(),
            search_query: self.workspace.search_panel.query.clone(),
            recent_files: self
                .recent_files
                .iter()
                .take(12)
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
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

    /// Kick off a background parse for a heavy viewer; the tab paints a Loading
    /// placeholder until poll_viewer_loads swaps in the finished viewer.
    fn spawn_viewer_load(&mut self, id: u64, path: PathBuf, kind: crate::viewer::MultimodalKind) {
        // A preview tab can be reused for another file before the first parse lands;
        // drop the stale loader so its result cannot overwrite the new tab.
        self.viewer_loads.retain(|(load_id, _)| *load_id != id);
        self.viewer_loads
            .push((id, crate::viewer::ViewerLoader::spawn(path, kind)));
    }

    /// Collect finished viewer parses and drop loaders for tabs the user closed.
    fn poll_viewer_loads(&mut self, ctx: &egui::Context) {
        if self.viewer_loads.is_empty() {
            return;
        }
        let mut finished: Vec<(u64, Option<crate::viewer::MultimodalViewer>)> = Vec::new();
        for (id, loader) in &mut self.viewer_loads {
            match loader.poll() {
                Some(Ok(viewer)) => finished.push((*id, Some(viewer))),
                Some(Err(())) => finished.push((*id, None)),
                None => {}
            }
        }
        if finished.is_empty() {
            return;
        }
        let mut failed = false;
        let mut done: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for (id, viewer) in finished {
            done.insert(id);
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                failed |= viewer.is_none();
                tab.multimodal = viewer;
                tab.invalidate_layout();
            }
        }
        // Finished loaders must be dropped or the next poll reports the closed
        // channel and clears the viewer that just arrived.
        let open: std::collections::HashSet<u64> = self.tabs.iter().map(|tab| tab.id).collect();
        self.viewer_loads
            .retain(|(id, _)| open.contains(id) && !done.contains(id));
        if failed {
            self.toast("文件渲染加载失败", ToastKind::Error);
        }
        ctx.request_repaint();
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

        // Periodic layout write: a crash should not lose splits, panel state, or the
        // terminal set just because no untitled buffer changed. Screenshot runs must
        // never overwrite the real session file.
        if self.last_session_save.elapsed() >= Duration::from_secs(30)
            && std::env::var_os("KESTREL_SHOT").is_none()
        {
            self.last_session_save = Instant::now();
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
            mono_bold_family: self.mono_bold_family.clone(),
            term_mono_family: self.term_mono_family.clone(),
            term_mono_bold_family: self.term_mono_bold_family.clone(),
            ui_size: 13.0,
        }
    }

    /// Hot-reload UI, editor, and custom terminal font definitions.
    pub fn reload_fonts(&mut self, ctx: &egui::Context) {
        let font_set = crate::fonts::install_with_terminal_font(
            ctx,
            &self.settings.terminal_font_family,
        );
        self.ui_family = font_set.ui_family;
        self.ui_bold_family = font_set.ui_bold_family;
        self.mono_family = font_set.mono_family;
        self.mono_bold_family = font_set.mono_bold_family;
        self.term_mono_family = font_set.term_mono_family;
        self.term_mono_bold_family = font_set.term_mono_bold_family;
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

    /// Fetch the Git `HEAD` blob for `path` on a background thread so opening files
    /// and restoring sessions never block on `git.exe`.
    fn queue_git_baseline(&mut self, tab_id: u64, path: PathBuf) {
        let Some(root) = self.workspace.root.clone() else {
            return;
        };
        if self.baseline_tx.is_none() {
            let (req_tx, req_rx) = std::sync::mpsc::channel::<(u64, PathBuf, PathBuf)>();
            let (res_tx, res_rx) = std::sync::mpsc::channel::<(u64, Option<String>)>();
            let ctx = self.egui_ctx.clone();
            if std::thread::Builder::new()
                .name("kestrel-git-baseline".to_string())
                .spawn(move || {
                    while let Ok((id, repo_root, file_path)) = req_rx.recv() {
                        let blob = crate::git::read_head_blob(&repo_root, &file_path);
                        if res_tx.send((id, blob)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                })
                .is_ok()
            {
                self.baseline_tx = Some(req_tx);
                self.baseline_rx = res_rx;
            } else {
                return;
            }
        }
        if let Some(tx) = &self.baseline_tx
            && tx.send((tab_id, root, path)).is_ok()
        {
            self.pending_baselines += 1;
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
        _ctx: &egui::Context,
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
                    if !open_external_url(&url) {
                        self.toast("已阻止打开不安全或无效的外部链接", ToastKind::Warning);
                    }
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
                    copy_to_clipboard(text);
                    self.toast("已复制代码块内容", ToastKind::Success);
                }
                PreviewEvent::CopySource => {
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        copy_to_clipboard(tab.doc.text());
                        self.toast("已复制文档源码到剪贴板", ToastKind::Success);
                    }
                }
                PreviewEvent::SwitchMode(mode) => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if mode == MarkdownViewMode::Split {
                            tab.md_preview.split_synced = false;
                        }
                        tab.md_mode = mode;
                    }
                }
                PreviewEvent::OpenInBrowser => {
                    self.preview_tab_in_browser(tab_idx);
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
                ViewerEvent::OpenPath(path) => self.open_file(path),
                ViewerEvent::OpenUrl(url) => {
                    if !open_external_url(&url) {
                        self.toast("已阻止打开不安全或无效的外部链接", ToastKind::Warning);
                    }
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
        let path = self.canonical(path);
        if path.is_file() {
            self.record_recent_file(&path);
        }
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

            // Heavy formats parse on a worker thread; paint a placeholder meanwhile.
            editor.multimodal = Some(crate::viewer::MultimodalViewer::Loading(kind));
            let final_id = if as_preview
                && let Some(prev_idx) = self
                    .tabs
                    .iter()
                    .position(|t| t.preview && !t.is_dirty() && !t.pinned)
            {
                let keep_id = self.tabs[prev_idx].id;
                editor.id = keep_id;
                self.tabs[prev_idx] = editor;
                self.focus_tab(keep_id);
                keep_id
            } else {
                self.tabs.push(editor);
                self.attach_editor_to_active_group(id);
                id
            };
            self.spawn_viewer_load(final_id, path.clone(), kind);
            self.reveal_in_tree(&path);
            self.record_recent_file(&path);
            return;
        }

        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let large = is_large_file(size);
        match Document::load(&path) {
            Ok(mut doc) => {
                if large {
                    // >8 MB: read-only preview. Syntax analysis is already gated at
                    // 1.5 MB and the minimap is disabled per pane, so the tab
                    // stays cheap to paint.
                    doc.readonly = true;
                }
                if large {
                    self.toast(LARGE_FILE_TOAST, ToastKind::Warning);
                } else if doc.lossy && doc.encoding != crate::doc::Encoding::Binary {
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
                if self.settings.detect_indentation {
                    editor.detect_indentation();
                }
                editor.focus_requested = true;
                editor.ensure_visible = true;
                editor.preview = as_preview;
                if let Some(kind) = mm_kind {
                    let text = editor.doc.text();
                    editor.multimodal = Some(crate::viewer::MultimodalViewer::load_tabular_text(
                        &path, &text, kind,
                    ));
                }
                if !large && size > crate::editor::MAX_HIGHLIGHT_BYTES as u64 {
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
                let final_id = if as_preview
                    && let Some(prev_idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.preview && !t.is_dirty() && !t.pinned)
                {
                    let keep_id = self.tabs[prev_idx].id;
                    editor.id = keep_id;
                    self.tabs[prev_idx] = editor;
                    self.focus_tab(keep_id);
                    keep_id
                } else {
                    self.tabs.push(editor);
                    self.attach_editor_to_active_group(id);
                    id
                };
                if !large {
                    self.queue_git_baseline(final_id, path.clone());
                }
                self.reveal_in_tree(&path);
                self.record_recent_file(&path);
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
                editor.multimodal = Some(crate::viewer::MultimodalViewer::Loading(kind));
                self.tabs.push(editor);
                self.attach_editor_to_active_group(id);
                self.spawn_viewer_load(id, path.clone(), kind);
            }
            return;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let large = is_large_file(size);
        if let Ok(mut doc) = Document::load(&path) {
            if large {
                doc.readonly = true;
            }
            let id = self.next_id;
            self.next_id += 1;
            let mut editor = Editor::new(doc, id, 0);
            if self.settings.detect_indentation {
                editor.detect_indentation();
            }
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
            if !large {
                self.queue_git_baseline(id, path);
            }
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

    /// Open `path` and jump directly to a 1-based line number.
    pub fn open_file_at_line(&mut self, path: PathBuf, line: usize) {
        self.open_file_at_line_col(path, line, None);
    }

    /// Open `path` and jump directly to a 1-based line and optional 1-based column.
    pub fn open_file_at_line_col(&mut self, path: PathBuf, line: usize, col: Option<usize>) {
        let path = self.canonical(path);
        self.open_file(path);
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let line_idx = line
            .max(1)
            .min(tab.doc.line_count().max(1))
            .saturating_sub(1);
        let line_start = tab.doc.line_to_char(line_idx);
        let line_len = tab.doc.line_content(line_idx).chars().count();
        let col_off = col.unwrap_or(1).saturating_sub(1).min(line_len);
        let caret = (line_start + col_off).min(tab.doc.len_chars());
        tab.caret = caret;
        tab.anchor = caret;
        tab.ensure_visible = true;
        tab.focus_requested = true;
    }

    /// Synchronize open editor tabs, Git status, and the diff view after saving
    /// an inline working-tree edit from inside a Diff tab.
    pub fn on_diff_working_file_saved(&mut self, diff_id: u64, path: PathBuf, line: usize) {
        let canon = self.canonical(path.clone());
        for tab in &mut self.tabs {
            if tab.diff.is_none()
                && tab.doc.path.as_deref() == Some(canon.as_path())
                && !tab.is_dirty()
            {
                let _ = tab.reload();
            }
        }
        if let Some(root) = self.workspace.root.clone() {
            self.git.refresh(&root);
        }
        self.spawn_diff_load(diff_id);
        self.toast(
            format!("已保存第 {line} 行附近更改并刷新差异"),
            ToastKind::Success,
        );
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

    /// Apply hunk-level Git actions parked by the diff view's hunk buttons. Git
    /// runs here, outside the paint pass; each action reloads its diff tab and
    /// the Git panel through the regular refresh path.
    fn apply_diff_commands(&mut self) {
        let Some(root) = self.workspace.root.clone() else {
            return;
        };
        let mut commands = Vec::new();
        for tab in &mut self.tabs {
            let Some(diff) = tab.diff.as_mut() else {
                continue;
            };
            let Some(command) = diff.pending.take() else {
                continue;
            };
            let patch = diff
                .data
                .as_ref()
                .and_then(|data| data.hunk_patch(command.hunk()));
            diff.pending_scroll_y = Some(diff.last_scroll_y);
            commands.push((tab.id, command, patch, diff.staged));
        }
        for (tab_id, command, patch, staged) in commands {
            let label = command.success_label();
            let Some(patch) = patch else {
                self.toast(
                    format!("{label}失败: 差异块已失效，请刷新差异"),
                    ToastKind::Error,
                );
                continue;
            };
            let result = match command {
                crate::diff::DiffCommand::Stage { .. } => crate::git::stage_patch(&root, &patch),
                crate::diff::DiffCommand::Unstage { .. } => {
                    crate::git::unstage_patch(&root, &patch)
                }
                crate::diff::DiffCommand::Discard { .. } => {
                    crate::git::discard_patch(&root, &patch, staged)
                }
            };
            match result {
                Ok(()) => self.toast(label, ToastKind::Success),
                Err(error) => self.toast(
                    format!("{label}失败: {}", error_summary(&error)),
                    ToastKind::Error,
                ),
            }
            // A partially applied discard already moved the index, so reload
            // the diff and the panel after a failure too.
            self.git.refresh(&root);
            self.spawn_diff_load(tab_id);
        }
    }

    /// Remember the current caret as a jump target for Alt+←.
    fn record_jump_point(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        self.jump_back.push(JumpPoint {
            tab_id: tab.id,
            path: tab.doc.path.clone(),
            byte: tab.doc.char_to_byte(tab.caret),
        });
        if self.jump_back.len() > 100 {
            self.jump_back.remove(0);
        }
        self.jump_forward.clear();
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
            self.record_jump_point();
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

    /// Alt+← / Alt+→ walk the recorded navigation history.
    fn jump_history(&mut self, back: bool) {
        let target = if back {
            self.jump_back.pop()
        } else {
            self.jump_forward.pop()
        };
        let Some(target) = target else {
            return;
        };
        if let Some(tab) = self.tabs.get(self.active) {
            let current = JumpPoint {
                tab_id: tab.id,
                path: tab.doc.path.clone(),
                byte: tab.doc.char_to_byte(tab.caret),
            };
            if back {
                self.jump_forward.push(current);
            } else {
                self.jump_back.push(current);
            }
        }
        self.navigate_to_jump(target);
    }

    fn navigate_to_jump(&mut self, target: JumpPoint) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == target.tab_id) {
            if index != self.active {
                self.activate_tab(index);
            }
            if let Some(tab) = self.tabs.get_mut(index) {
                let ch = tab.doc.byte_to_char(target.byte);
                tab.caret = ch;
                tab.anchor = ch;
                tab.ensure_visible = true;
                tab.focus_requested = true;
            }
            return;
        }
        if let Some(path) = target.path {
            self.open_file_at(path, target.byte);
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
        let format_on_save = self.settings.format_on_save;
        let trim_ws = self.settings.trim_trailing_whitespace;
        if let Some(tab) = self.tabs.get_mut(self.active)
            && !tab.doc.readonly
            && tab.doc.encoding != crate::doc::Encoding::Binary
        {
            if format_on_save {
                let _ = tab.format_document();
            }
            if trim_ws {
                tab.trim_trailing_whitespace();
            }
        }
        self.emit_plugin_event_for(Event::BeforeSave, doc_id);
        let Some(index) = self.tabs.iter().position(|tab| tab.id == doc_id) else {
            return;
        };
        let saved_path = self.tabs[index].doc.path.clone();
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
                if let Some(path) = saved_path {
                    self.reload_notified.remove(&path);
                    self.conflict_notified.remove(&path);
                }
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

    fn reopen_active_with_encoding(&mut self, encoding: Encoding) {
        let result = self
            .tabs
            .get_mut(self.active)
            .map(|tab| tab.reopen_with_encoding(encoding));
        match result {
            Some(Ok(())) => self.toast(
                format!("已按 {} 重新解码当前文件", encoding.as_str()),
                ToastKind::Info,
            ),
            Some(Err(err)) => self.toast(format!("重新打开失败: {err}"), ToastKind::Error),
            None => {}
        }
    }

    fn save_active_as_encoding(&mut self, encoding: Encoding) {
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
        let result = self
            .tabs
            .get_mut(self.active)
            .map(|tab| tab.save_as_with_encoding(path.clone(), encoding));
        let name = path.display().to_string();
        match result {
            Some(SaveOutcome::Saved) => {
                self.toast(format!("已保存为 {name}"), ToastKind::Success);
                self.reveal_in_tree(&path);
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
            }
            Some(SaveOutcome::DiskChanged) => self.toast(
                format!("另存为失败: {name} 已在磁盘上被修改"),
                ToastKind::Warning,
            ),
            Some(SaveOutcome::ReadOnly) => {
                self.toast(format!("没有权限写入 {name}"), ToastKind::Warning)
            }
            Some(SaveOutcome::Failed(err)) => {
                self.toast(format!("另存为失败: {err}"), ToastKind::Error)
            }
            None => {}
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
        let trim_ws = self.settings.trim_trailing_whitespace;
        for id in ids {
            if trim_ws
                && let Some(index) = self.tabs.iter().position(|tab| tab.id == id)
                && !self.tabs[index].doc.readonly
                && self.tabs[index].doc.encoding != crate::doc::Encoding::Binary
            {
                self.tabs[index].trim_trailing_whitespace();
            }
            self.emit_plugin_event_for(Event::BeforeSave, id);
            let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                continue;
            };
            let saved_path = self.tabs[index].doc.path.clone();
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
                    if let Some(path) = saved_path {
                        self.reload_notified.remove(&path);
                        self.conflict_notified.remove(&path);
                    }
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
        let reloaded_path = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.doc.path.clone());
        if let Some(tab) = self.tabs.get_mut(self.active) {
            match tab.reload() {
                Ok(()) => {
                    if let Some(path) = reloaded_path {
                        self.reload_notified.remove(&path);
                        self.conflict_notified.remove(&path);
                    }
                    self.toast("已从磁盘重新加载", ToastKind::Info);
                }
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
        } else if self.tabs[index].reload().is_ok()
            && self.reload_notified.insert(path.to_path_buf())
        {
            self.toast(format!("{title} 已从磁盘重新加载"), ToastKind::Info);
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
        self.workspace.watcher = FsWatcher::new(&path, self.egui_ctx.clone()).ok();
        self.workspace.search_panel = SearchPanelState::default();
        self.workspace.search.cancel();
        self.workspace.index_dirty.clear();
        self.workspace.index_dirty_at = None;
        // Terminals and the Git snapshot belong to the old project: drop them so
        // the next shell spawns in the new root instead of the old directory.
        self.bottom.terminals.clear();
        self.bottom.pending_terminals.clear();
        self.bottom.active_terminal = 0;
        self.bottom.terminal_focus = false;
        self.bottom.terminal_has_focus = false;
        self.git.clear();
        self.git.prime_fast(&path);
        self.git.log(format!("打开工作区: {}", path.display()));
        self.tabs.clear();
        self.active = 0;
        self.reset_groups();
        self.sidebar.selected = None;
        self.sidebar.editing = None;
        // An open terminal panel starts a fresh shell in the new root.
        if self.bottom.visible {
            self.new_terminal();
        }
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
        match crate::workspace::tree::delete_to_trash_or_permanent(&path) {
            Ok(outcome) => {
                if let Some(parent) = path.parent() {
                    self.workspace.tree.reload_dir(parent);
                }
                let canonical = self.canonical(path.clone());
                self.tabs.retain(|t| {
                    t.doc.path.as_deref().is_none_or(|p| {
                        p != path.as_path()
                            && !p.starts_with(&path)
                            && p != canonical.as_path()
                            && !p.starts_with(&canonical)
                    })
                });
                self.normalize_groups();
                if let Some(root) = &self.workspace.root {
                    self.workspace.index.scan(root);
                    self.git.refresh(root);
                }
                match outcome {
                    crate::workspace::tree::DeleteOutcome::Trashed => {
                        self.toast("已移至回收站", ToastKind::Info);
                    }
                    crate::workspace::tree::DeleteOutcome::Permanent => {
                        self.toast("无法移至回收站，已永久删除", ToastKind::Warning);
                    }
                }
            }
            Err(err) => self.toast(format!("删除失败: {err}"), ToastKind::Error),
        }
    }

    /// Batch delete for the Explorer multi-selection context menu.
    pub fn delete_paths(&mut self, paths: &[PathBuf]) {
        let mut trashed = 0usize;
        let mut permanent = 0usize;
        let mut failed = 0usize;
        let mut parents: Vec<PathBuf> = Vec::new();
        let mut removed_roots: Vec<(PathBuf, PathBuf)> = Vec::new();
        for path in paths {
            match crate::workspace::tree::delete_to_trash_or_permanent(path) {
                Ok(outcome) => {
                    match outcome {
                        crate::workspace::tree::DeleteOutcome::Trashed => trashed += 1,
                        crate::workspace::tree::DeleteOutcome::Permanent => permanent += 1,
                    }
                    if let Some(parent) = path.parent() {
                        parents.push(parent.to_path_buf());
                    }
                    removed_roots.push((path.clone(), self.canonical(path.clone())));
                }
                Err(_) => failed += 1,
            }
        }
        if !removed_roots.is_empty() {
            self.tabs.retain(|t| {
                t.doc.path.as_deref().is_none_or(|p| {
                    !removed_roots.iter().any(|(raw, canon)| {
                        p == raw.as_path()
                            || p.starts_with(raw)
                            || p == canon.as_path()
                            || p.starts_with(canon)
                    })
                })
            });
            self.normalize_groups();
        }
        for parent in &parents {
            self.workspace.tree.reload_dir(parent);
        }
        self.sidebar.selected = None;
        self.sidebar.selected_paths.clear();
        if let Some(root) = self.workspace.root.clone() {
            self.workspace.index.scan(&root);
            self.git.refresh(&root);
        }
        if failed == 0 {
            if permanent == 0 {
                self.toast(format!("已将 {trashed} 个项目移至回收站"), ToastKind::Info);
            } else if trashed == 0 {
                self.toast(
                    format!("无法移至回收站，已永久删除 {permanent} 个项目"),
                    ToastKind::Warning,
                );
            } else {
                self.toast(
                    format!("已移至回收站 {trashed} 个，永久删除 {permanent} 个"),
                    ToastKind::Warning,
                );
            }
        } else {
            let deleted = trashed + permanent;
            self.toast(
                format!("已删除 {deleted} 个项目（回收站 {trashed} / 永久 {permanent}），{failed} 个失败"),
                ToastKind::Error,
            );
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
        let filters_changed = self.settings.search_include
            != self.workspace.search_panel.include
            || self.settings.search_exclude != self.workspace.search_panel.exclude;
        if filters_changed {
            self.settings.search_include = self.workspace.search_panel.include.clone();
            self.settings.search_exclude = self.workspace.search_panel.exclude.clone();
            self.settings.save();
        }
        let search_query = SearchQuery {
            pattern: query.clone(),
            regex: self.workspace.search_panel.regex,
            filename_only: self.workspace.search_panel.filename_only,
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

    /// Start a search the user explicitly submitted, remembering the query.
    pub fn submit_project_search(&mut self) {
        let query = self.workspace.search_panel.query.trim().to_string();
        if !query.is_empty() {
            self.settings.push_search_history(&query);
        }
        self.start_project_search();
    }

    pub fn replace_all_in_project(&mut self) {
        if self.workspace.search_panel.query.trim().is_empty() {
            return;
        }
        if self.workspace.search_panel.filename_only {
            self.toast("仅文件名模式下不支持替换", ToastKind::Info);
            return;
        }
        let search_query = SearchQuery {
            pattern: self.workspace.search_panel.query.clone(),
            regex: self.workspace.search_panel.regex,
            filename_only: false,
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
                    let _ = tab.reload();
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

    /// Drain search events; returns true when the panel needs a repaint.
    fn poll_search(&mut self) -> bool {
        let events = self.workspace.search.poll();
        let changed = !events.is_empty();
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
        changed
    }

    /// Watcher paths wait for the index debounce before touching the file index,
    /// so a save burst costs one incremental update instead of one per event.
    fn queue_index_changes(&mut self, paths: &[PathBuf]) {
        self.workspace
            .index_dirty
            .extend(paths.iter().filter(|path| !is_save_temp(path)).cloned());
        if !self.workspace.index_dirty.is_empty() {
            self.workspace.index_dirty_at = Some(Instant::now() + INDEX_DEBOUNCE);
        }
    }

    fn flush_index_changes(&mut self) {
        self.workspace.index_dirty_at = None;
        let Some(root) = self.workspace.root.clone() else {
            self.workspace.index_dirty.clear();
            return;
        };
        let paths = std::mem::take(&mut self.workspace.index_dirty);
        if paths.is_empty() {
            return;
        }
        if self.workspace.index.apply_changes(&root, &paths) {
            self.on_index_changed();
        }
    }

    /// Quick Open and Project Search read the index, so both follow it here.
    fn on_index_changed(&mut self) {
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

    // ------------------------------------------------------------------
    // Palette
    // ------------------------------------------------------------------

    /// Move the caret to a 1-based line in the active editor.
    fn goto_line(&mut self, line: usize) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let line = line.max(1).min(tab.doc.line_count()) - 1;
        tab.caret = tab.doc.line_to_char(line);
        tab.anchor = tab.caret;
        tab.ensure_visible = true;
        tab.focus_requested = true;
    }

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

    /// Empty-query command list: recent commands first, then grouped by category.
    fn command_palette_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = Vec::new();
        for id in &self.settings.recent_commands {
            if let Some(index) = self
                .palette
                .items
                .iter()
                .position(|item| palette_item_id(item) == Some(id.as_str()))
            {
                order.push(index);
            }
        }
        for category in PALETTE_CATEGORIES {
            for (index, item) in self.palette.items.iter().enumerate() {
                if order.contains(&index) {
                    continue;
                }
                let item_category = match item {
                    PaletteItem::Command(command) => command.category(),
                    PaletteItem::PluginCommand(id) => {
                        self.plugins.command_category_of(id).unwrap_or_default()
                    }
                    _ => continue,
                };
                if item_category == *category {
                    order.push(index);
                }
            }
        }
        for index in 0..self.palette.items.len() {
            if !order.contains(&index) {
                order.push(index);
            }
        }
        order
    }

    pub fn filter_palette(&mut self) {
        let raw_query = self.palette.query.trim().to_string();
        let (query_text, _) = split_file_line(&raw_query);
        let query = query_text.to_string();
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
        } else if self.palette.mode == PaletteMode::Commands && query.is_empty() {
            self.palette.filtered = self.command_palette_order();
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
                self.settings.push_recent_command(command.id());
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
                let (_, line) = split_file_line(self.palette.query.trim());
                self.close_palette();
                self.open_file(path);
                if let Some(line) = line {
                    self.goto_line(line);
                }
            }
            Some(PaletteItem::Language(language)) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.set_language(language);
                }
                self.close_palette();
            }
            Some(PaletteItem::Symbol(sym)) => {
                self.close_palette();
                self.record_jump_point();
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
                    self.record_jump_point();
                    self.goto_line(self.palette.query.trim().parse().unwrap_or(1));
                }
                self.close_palette();
            }
        }
    }

    // ------------------------------------------------------------------
    // Terminal (Multi-Session)
    // ------------------------------------------------------------------

    fn ensure_terminal_ready(&mut self) {
        if self.bottom.terminals.is_empty() {
            let pending = std::mem::take(&mut self.bottom.pending_terminals);
            for (shell, cwd) in pending.into_iter().take(4) {
                self.open_terminal_in(cwd, shell);
            }
            if self.bottom.terminals.is_empty() {
                self.new_terminal();
            }
        }
    }

    pub fn toggle_terminal(&mut self) {
        if self.bottom.visible && self.bottom.tab != BottomTab::Terminal {
            self.bottom.tab = BottomTab::Terminal;
            self.ensure_terminal_ready();
            self.bottom.terminal_focus = true;
            return;
        }
        self.bottom.visible = !self.bottom.visible;
        if self.bottom.visible {
            self.bottom.tab = BottomTab::Terminal;
            self.ensure_terminal_ready();
            self.bottom.terminal_focus = true;
        } else if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus_requested = true;
        }
    }

    /// Focus the integrated terminal (opening the panel/session if needed), or
    /// return focus to the editor if the terminal is already focused.
    pub fn focus_terminal(&mut self) {
        if self.bottom.visible && self.bottom.tab == BottomTab::Terminal && self.bottom.terminal_has_focus {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus_requested = true;
                self.bottom.terminal_focus = false;
                return;
            }
        }
        self.bottom.visible = true;
        self.bottom.tab = BottomTab::Terminal;
        self.ensure_terminal_ready();
        self.bottom.terminal_focus = true;
    }

    pub fn new_terminal(&mut self) {
        self.new_terminal_with_shell(ShellKind::Default);
    }

    pub fn new_terminal_with_shell(&mut self, shell: ShellKind) {
        let follow_dir = if self.bottom.follow_active_dir {
            self.active_path()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        } else {
            None
        };
        let cwd = follow_dir
            .or_else(|| self.workspace.root.clone())
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

    fn estimated_initial_terminal_size(&self) -> (u16, u16) {
        let (cols, rows) = self.bottom.terminal_size;
        if cols >= 20 && rows >= 4 {
            return (cols, rows);
        }
        let font_size = self.settings.terminal_font_size.clamp(9.0, 28.0);
        let cell_h = (font_size * 1.45).max(12.0);
        let est_rows = (((self.bottom.height - 36.0) / cell_h).floor() as u16).clamp(8, 60);
        let est_cols = 100u16;
        (est_cols, est_rows)
    }

    fn open_terminal_in(&mut self, cwd: PathBuf, shell: ShellKind) {
        self.open_terminal_internal(cwd, shell, false);
    }

    fn open_terminal_internal(&mut self, cwd: PathBuf, shell: ShellKind, add_pane: bool) {
        let resolved_shell = if shell == ShellKind::Default {
            ShellKind::from_str(&self.settings.terminal_default_shell)
        } else {
            shell
        };
        let custom_shell = crate::terminal::CustomShellConfig::from_settings(
            &self.settings.terminal_custom_shell_path,
            &self.settings.terminal_custom_shell_args,
        );
        let (cols, rows) = self.estimated_initial_terminal_size();
        let mut session = crate::terminal::TerminalSession::spawn_async(
            &cwd,
            cols,
            rows,
            resolved_shell,
            custom_shell,
            Some(self.egui_ctx.clone()),
        );
        let fg = self.theme.text;
        let bg = self.theme.terminal_bg;
        session.set_theme_colors((fg.r(), fg.g(), fg.b()), (bg.r(), bg.g(), bg.b()));
        self.bottom.terminals.push(session);
        let new_idx = self.bottom.terminals.len() - 1;

        if add_pane && self.bottom.panes.len() < MAX_TERMINAL_PANES {
            let insert_at = (self.bottom.focused_pane + 1).min(self.bottom.panes.len());
            self.bottom.panes.insert(insert_at, new_idx);
            let avg_weight = if self.bottom.pane_weights.is_empty() {
                1.0
            } else {
                self.bottom.pane_weights.iter().sum::<f32>()
                    / self.bottom.pane_weights.len() as f32
            };
            self.bottom
                .pane_weights
                .insert(insert_at, avg_weight.max(0.25));
            self.bottom.focused_pane = insert_at;
        } else if self.bottom.panes.is_empty() {
            self.bottom.panes.push(new_idx);
            self.bottom.pane_weights.push(1.0);
            self.bottom.focused_pane = 0;
        } else {
            let slot = self.bottom.focused_pane.min(self.bottom.panes.len() - 1);
            self.bottom.panes[slot] = new_idx;
        }
        self.bottom.active_terminal = new_idx;
        self.bottom.normalize_panes();
        self.bottom.terminal_size = (cols, rows);
        self.bottom.terminal_focus = true;
        self.bottom.visible = true;
        self.bottom.tab = BottomTab::Terminal;
    }

    pub fn focus_terminal_index(&mut self, idx: usize) {
        if idx >= self.bottom.terminals.len() {
            return;
        }
        self.bottom.normalize_panes();
        if let Some(slot) = self.bottom.panes.iter().position(|&p| p == idx) {
            self.bottom.focused_pane = slot;
        } else if !self.bottom.panes.is_empty() {
            let slot = self.bottom.focused_pane.min(self.bottom.panes.len() - 1);
            self.bottom.panes[slot] = idx;
        } else {
            self.bottom.panes = vec![idx];
            self.bottom.pane_weights = vec![1.0];
            self.bottom.focused_pane = 0;
        }
        self.bottom.active_terminal = idx;
        self.bottom.terminal_focus = true;
        for term in &mut self.bottom.terminals {
            term.ime_preedit.clear();
        }
    }

    pub fn add_terminal_to_split(&mut self, idx: usize) {
        if idx >= self.bottom.terminals.len() {
            return;
        }
        self.bottom.normalize_panes();
        if let Some(slot) = self.bottom.panes.iter().position(|&p| p == idx) {
            self.bottom.focused_pane = slot;
            self.bottom.active_terminal = idx;
            self.bottom.terminal_focus = true;
            return;
        }
        if self.bottom.panes.len() >= MAX_TERMINAL_PANES {
            self.toast(
                format!("最多同时并排显示 {MAX_TERMINAL_PANES} 个终端窗格"),
                ToastKind::Info,
            );
            return;
        }
        let insert_at = (self.bottom.focused_pane + 1).min(self.bottom.panes.len());
        self.bottom.panes.insert(insert_at, idx);
        self.bottom.pane_weights.insert(insert_at, 1.0);
        self.bottom.focused_pane = insert_at;
        self.bottom.active_terminal = idx;
        self.bottom.terminal_focus = true;
    }

    pub fn close_active_terminal(&mut self) {
        if let Some(idx) = self.bottom.focused_terminal_idx() {
            self.close_terminal_at(idx);
        }
    }

    pub fn close_terminal_at(&mut self, idx: usize) {
        if idx >= self.bottom.terminals.len() {
            return;
        }
        let removed_id = self.bottom.terminals[idx].id;
        if self
            .bottom
            .renaming_terminal
            .as_ref()
            .is_some_and(|(id, _, _)| *id == removed_id)
        {
            self.bottom.renaming_terminal = None;
        }
        self.bottom.terminals.remove(idx);

        let mut next_panes = Vec::new();
        let mut next_weights = Vec::new();
        for (slot, &pane_idx) in self.bottom.panes.iter().enumerate() {
            if pane_idx == idx {
                continue;
            }
            let shifted = if pane_idx > idx { pane_idx - 1 } else { pane_idx };
            next_panes.push(shifted);
            next_weights.push(self.bottom.pane_weights.get(slot).copied().unwrap_or(1.0));
        }
        self.bottom.panes = next_panes;
        self.bottom.pane_weights = next_weights;
        self.bottom.normalize_panes();
        if !self.bottom.terminals.is_empty() {
            self.bottom.terminal_focus = true;
        }
    }

    /// Split a new terminal pane side-by-side (up to MAX_TERMINAL_PANES).
    pub fn split_terminal_pane(&mut self) {
        self.bottom.visible = true;
        self.bottom.tab = BottomTab::Terminal;
        if self.bottom.terminals.is_empty() {
            self.new_terminal();
        }
        self.bottom.normalize_panes();
        if self.bottom.panes.len() >= MAX_TERMINAL_PANES {
            self.toast(
                format!("最多同时并排显示 {MAX_TERMINAL_PANES} 个终端窗格"),
                ToastKind::Info,
            );
            return;
        }
        let follow_dir = if self.bottom.follow_active_dir {
            self.active_path()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        } else {
            None
        };
        let cwd = follow_dir
            .or_else(|| {
                self.bottom
                    .focused_terminal_idx()
                    .and_then(|i| self.bottom.terminals.get(i))
                    .map(|t| t.cwd.clone())
            })
            .or_else(|| self.workspace.root.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        self.open_terminal_internal(cwd, ShellKind::Default, true);
    }

    /// Collapse multi-pane split back to a single pane showing the focused terminal.
    pub fn unsplit_terminals(&mut self) {
        if let Some(focused) = self.bottom.focused_terminal_idx() {
            self.bottom.panes = vec![focused];
            self.bottom.pane_weights = vec![1.0];
            self.bottom.focused_pane = 0;
            self.bottom.active_terminal = focused;
            self.bottom.terminal_focus = true;
        }
    }

    pub fn switch_terminal_step(&mut self, forward: bool) {
        let len = self.bottom.terminals.len();
        if len <= 1 {
            return;
        }
        let cur = self.bottom.focused_terminal_idx().unwrap_or(0);
        let next = if forward {
            (cur + 1) % len
        } else {
            (cur + len - 1) % len
        };
        self.focus_terminal_index(next);
        self.bottom.visible = true;
        self.bottom.tab = BottomTab::Terminal;
    }

    pub fn clear_focused_terminal(&mut self) {
        if let Some(idx) = self.bottom.focused_terminal_idx()
            && let Some(term) = self.bottom.terminals.get_mut(idx)
        {
            term.clear_screen();
        }
    }

    pub fn toggle_terminal_maximize(&mut self) {
        self.bottom.visible = true;
        self.bottom.maximized = !self.bottom.maximized;
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    pub fn dispatch(&mut self, command: Command, ctx: &egui::Context) {
        if let Some(diff_tab_id) = self
            .active_editor()
            .filter(|tab| tab.diff.is_some())
            .map(|tab| tab.id)
        {
            match command {
                Command::Save => {
                    let saved = self
                        .tabs
                        .get_mut(self.active)
                        .and_then(|tab| tab.diff.as_mut())
                        .and_then(|diff| {
                            let path = diff.path.clone();
                            diff.save_inline_edit().map(|res| (path, res))
                        });
                    if let Some((path, res)) = saved {
                        match res {
                            Ok(line) => self.on_diff_working_file_saved(diff_tab_id, path, line),
                            Err(err) => self.toast(err, ToastKind::Error),
                        }
                    }
                    return;
                }
                Command::Copy => {
                    if let Some(text) = self
                        .active_editor()
                        .and_then(|tab| tab.diff.as_ref())
                        .and_then(|diff| diff.copy_selection())
                    {
                        copy_to_clipboard(text);
                    }
                    return;
                }
                Command::SelectAll => {
                    if let Some(diff) = self
                        .tabs
                        .get_mut(self.active)
                        .and_then(|tab| tab.diff.as_mut())
                    {
                        diff.select_all_active_side();
                    }
                    return;
                }
                Command::Find => {
                    if let Some(diff) = self
                        .tabs
                        .get_mut(self.active)
                        .and_then(|tab| tab.diff.as_mut())
                    {
                        diff.open_find();
                    }
                    return;
                }
                Command::FindNext => {
                    if let Some(diff) = self
                        .tabs
                        .get_mut(self.active)
                        .and_then(|tab| tab.diff.as_mut())
                    {
                        diff.find_step(true);
                    }
                    return;
                }
                Command::FindPrev => {
                    if let Some(diff) = self
                        .tabs
                        .get_mut(self.active)
                        .and_then(|tab| tab.diff.as_mut())
                    {
                        diff.find_step(false);
                    }
                    return;
                }
                _ if command.is_editor_command()
                    || matches!(
                        command,
                        Command::SaveAs
                            | Command::SaveAll
                            | Command::FormatDocument
                            | Command::ToggleEol
                            | Command::TrimTrailingWhitespace
                            | Command::ReopenUtf8
                            | Command::ReopenUtf8Bom
                            | Command::ReopenUtf16Le
                            | Command::ReopenUtf16Be
                            | Command::SaveAsUtf8
                            | Command::SaveAsUtf8Bom
                            | Command::SaveAsUtf16Le
                            | Command::SaveAsUtf16Be
                    ) =>
                {
                    return;
                }
                _ => {}
            }
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
            Command::ToggleEol => {
                let next = self.tabs.get_mut(self.active).and_then(Editor::toggle_eol);
                if let Some(eol) = next {
                    self.toast(format!("行尾已切换为 {}", eol.as_str()), ToastKind::Info);
                } else if self.active_editor().is_some() {
                    self.toast("只读文件无法切换行尾", ToastKind::Warning);
                }
            }
            Command::ReopenUtf8 => self.reopen_active_with_encoding(Encoding::Utf8),
            Command::ReopenUtf8Bom => self.reopen_active_with_encoding(Encoding::Utf8Bom),
            Command::ReopenUtf16Le => self.reopen_active_with_encoding(Encoding::Utf16Le),
            Command::ReopenUtf16Be => self.reopen_active_with_encoding(Encoding::Utf16Be),
            Command::SaveAsUtf8 => self.save_active_as_encoding(Encoding::Utf8),
            Command::SaveAsUtf8Bom => self.save_active_as_encoding(Encoding::Utf8Bom),
            Command::SaveAsUtf16Le => self.save_active_as_encoding(Encoding::Utf16Le),
            Command::SaveAsUtf16Be => self.save_active_as_encoding(Encoding::Utf16Be),
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
                    tab.paste_text(&text);
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
                let outcome = self.tabs.get_mut(self.active).map(Editor::format_document);
                match outcome {
                    Some(FormatOutcome::ReadOnly) => {
                        self.toast("文档已锁定，无法格式化", ToastKind::Warning);
                    }
                    Some(FormatOutcome::Formatted { external: true }) => {
                        self.toast("已格式化文档", ToastKind::Success);
                    }
                    Some(FormatOutcome::Unchanged { external: true }) => {
                        self.toast("文档已是格式化状态", ToastKind::Info);
                    }
                    Some(FormatOutcome::Formatted { external: false }) => {
                        self.toast("当前语言没有格式化器，已清理行尾空白", ToastKind::Info);
                    }
                    Some(FormatOutcome::Unchanged { external: false }) => {
                        self.toast("当前语言没有格式化器，无需整理", ToastKind::Info);
                    }
                    None => {}
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
            Command::SelectExpand => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.expand_selection();
                }
            }
            Command::SelectShrink => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.shrink_selection();
                }
            }
            Command::JumpToBracket => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.jump_to_matching_bracket();
                }
            }
            Command::TrimTrailingWhitespace => {
                let removed = self
                    .tabs
                    .get_mut(self.active)
                    .map(Editor::trim_trailing_whitespace);
                match removed {
                    Some(0) => self.toast("没有行尾空白需要清理", ToastKind::Info),
                    Some(count) => self.toast(
                        format!("已清理 {count} 行的行尾空白"),
                        ToastKind::Success,
                    ),
                    None => {}
                }
            }
            Command::ToggleAutoClose => {
                self.settings.auto_close_pairs = !self.settings.auto_close_pairs;
                self.settings.save();
                self.toast(
                    if self.settings.auto_close_pairs {
                        "已开启自动闭合括号"
                    } else {
                        "已关闭自动闭合括号"
                    },
                    ToastKind::Info,
                );
            }
            Command::JumpBack => self.jump_history(true),
            Command::JumpForward => self.jump_history(false),
            Command::ShowPalette => self.open_palette(PaletteMode::Commands),
            Command::QuickOpen => self.open_palette(PaletteMode::QuickOpen),
            Command::ToggleSidebar => self.sidebar.visible = !self.sidebar.visible,
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::FocusTerminal => self.focus_terminal(),
            Command::NewTerminal => self.new_terminal(),
            Command::TerminalClose => self.close_active_terminal(),
            Command::TerminalClear => self.clear_focused_terminal(),
            Command::TerminalSplit => self.split_terminal_pane(),
            Command::TerminalUnsplit => self.unsplit_terminals(),
            Command::TerminalNext => self.switch_terminal_step(true),
            Command::TerminalPrev => self.switch_terminal_step(false),
            Command::TerminalMaximize => self.toggle_terminal_maximize(),
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
                    if matches!(tab.language, Language::Markdown | Language::Html) {
                        tab.md_mode = match tab.md_mode {
                            crate::markdown::MarkdownViewMode::Preview => {
                                crate::markdown::MarkdownViewMode::Source
                            }
                            _ => crate::markdown::MarkdownViewMode::Preview,
                        };
                    } else {
                        self.toast("当前文件不是 Markdown 或 HTML 文档", ToastKind::Info);
                    }
                }
            }
            Command::MarkdownPreviewSide => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if matches!(tab.language, Language::Markdown | Language::Html) {
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
                        self.toast("当前文件不是 Markdown 或 HTML 文档", ToastKind::Info);
                    }
                }
            }
            Command::PreviewInBrowser => {
                self.preview_active_in_browser();
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
            Command::ToggleIndent => self.toggle_indent(),
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
        // Background writes return an empty message when another toast covers the result.
        if text.is_empty() {
            return;
        }
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
        let active_id = self.tabs.get(self.active).map(|tab| tab.id);
        let version = self.tabs.get(self.active).map_or(0, |tab| tab.doc.version);
        let dirty = self.tabs.get(self.active).is_some_and(|tab| tab.is_dirty());
        // The title only changes with the active document, its dirty flag, or the
        // folder. Every other frame used to format three strings and drop them.
        if self.title_key == (active_id, version, dirty)
            && self.title_path.as_deref()
                == self
                    .tabs
                    .get(self.active)
                    .and_then(|tab| tab.doc.path.as_deref())
            && self.title_root.as_deref() == self.workspace.root.as_deref()
        {
            return;
        }
        self.title_key = (active_id, version, dirty);
        self.title_path = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.doc.path.clone());
        self.title_root = self.workspace.root.clone();
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
        // Reuse the scratch buffer: typing pushes key events every frame, and a
        // fresh Vec per keystroke is pure churn.
        let mut pressed = std::mem::take(&mut self.pressed_keys);
        pressed.clear();
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                {
                    pressed.push((*key, *modifiers));
                }
            }
        });
        if !pressed.is_empty() {
            self.route_shortcuts(ctx, &pressed);
        }
        self.pressed_keys = pressed;
    }

    /// Dispatch this frame's key events. Frames without keys return before the
    /// shortcut table is touched.
    fn route_shortcuts(&mut self, ctx: &egui::Context, pressed: &[(egui::Key, egui::Modifiers)]) {
        if self.palette.open || self.dialog.is_some() {
            return;
        }
        let terminal_focused = self.bottom.visible && self.bottom.terminal_has_focus;
        // Handle UI zoom shortcuts (Ctrl + = / + / - / 0), including Shift+= for '+',
        // unless the terminal owns keyboard focus (where Ctrl+-/=/0 belong to the shell).
        if !terminal_focused {
            let zoom_cmd = ctx.input_mut(|i| {
                let command = (i.modifiers.ctrl || i.modifiers.command) && !i.modifiers.alt;
                if !command {
                    return None;
                }
                if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Equals)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Plus)
                    || (i.modifiers.shift
                        && (i.consume_key(
                            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                            egui::Key::Plus,
                        ) || i.consume_key(
                            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                            egui::Key::Equals,
                        )))
                {
                    Some(Command::ZoomIn)
                } else if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Minus) {
                    Some(Command::ZoomOut)
                } else if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0) {
                    Some(Command::ZoomReset)
                } else {
                    None
                }
            });
            if let Some(cmd) = zoom_cmd {
                self.dispatch(cmd, ctx);
                return;
            }
        }
        // Editor commands usually run inside the canvas. While the terminal owns
        // keyboard focus, only explicit workbench-level commands bypass the shell.
        let editor_focused = self
            .active_editor()
            .is_some_and(|tab| tab.has_focus && tab.ime_preedit.is_empty());
        let mut handled = false;
        let bindings = effective_shortcut_bindings(&self.custom_shortcuts);
        for (command, shortcut) in bindings {
            if command.is_editor_command() && !editor_focused {
                continue;
            }
            if terminal_focused && !command.skip_shell_in_terminal() {
                continue;
            }
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
        if handled || terminal_focused {
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
        // Settings edits only mark the file dirty; one write per quiet second.
        if Settings::save_due() {
            self.settings.flush();
        }
        // Poll returns are the only wake-up for background work: without these
        // repaints a finished search or Git scan waited for the next input event.
        let mut repaint = false;
        if self.deferred_terminal_restore && self.frame >= 1 {
            self.deferred_terminal_restore = false;
            let specs = std::mem::take(&mut self.bottom.pending_terminals);
            let want_panes = self.bottom.panes.clone();
            let want_weights = self.bottom.pane_weights.clone();
            for (shell, cwd) in specs {
                self.open_terminal_in(cwd, shell);
            }
            let len = self.bottom.terminals.len();
            self.bottom.active_terminal = self
                .deferred_terminal_active
                .min(len.saturating_sub(1));
            if !want_panes.is_empty() && len > 0 {
                self.bottom.panes = want_panes;
                self.bottom.pane_weights = want_weights;
                self.bottom.focused_pane = self
                    .bottom
                    .panes
                    .iter()
                    .position(|&idx| idx == self.bottom.active_terminal)
                    .unwrap_or(0);
            }
            self.bottom.normalize_panes();
            repaint = true;
        }
        while let Ok((tab_id, head_text)) = self.baseline_rx.try_recv() {
            self.pending_baselines = self.pending_baselines.saturating_sub(1);
            if let Some(text) = head_text
                && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            {
                tab.doc.set_git_baseline(text.replace("\r\n", "\n"));
                tab.diff_version = 0;
                repaint = true;
            }
        }
        if self.workspace.index.poll() {
            self.on_index_changed();
            repaint = true;
        }
        if self.poll_search() {
            repaint = true;
        }
        let updates = self.git.poll();
        repaint |= updates.snapshot;
        repaint |= !updates.ops.is_empty();
        for op in updates.ops {
            for path in &op.touched {
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.doc.path.as_deref() == Some(path.as_path()))
                {
                    let _ = tab.reload();
                }
            }
            match op.result {
                Ok(message) => {
                    if !message.is_empty() {
                        self.toast(message, ToastKind::Success);
                    }
                }
                Err(error) => self.toast(
                    format!("{}失败: {}", op.label, error_summary(&error)),
                    ToastKind::Error,
                ),
            }
        }

        if self
            .workspace
            .index_dirty_at
            .is_some_and(|at| Instant::now() >= at)
        {
            self.flush_index_changes();
        }

        let events = self
            .workspace
            .watcher
            .as_ref()
            .map(|watcher| watcher.poll())
            .unwrap_or_default();
        if !events.is_empty() {
            for path in &events {
                if is_save_temp(path) {
                    continue;
                }
                if let Some(parent) = path.parent() {
                    self.workspace.tree.reload_dir(parent);
                }
            }
            self.queue_index_changes(&events);
            for path in events {
                if is_save_temp(&path) {
                    continue;
                }
                self.handle_disk_change(&path);
                let diff_tabs: Vec<u64> = self
                    .tabs
                    .iter()
                    .filter(|tab| tab.diff.as_ref().is_some_and(|diff| diff.path == path))
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

        for term in &mut self.bottom.terminals {
            if term.update() {
                repaint = true;
            }
            if let Some(clip) = term.take_pending_clipboard() {
                copy_to_clipboard(clip);
            }
        }

        // Hunk buttons in diff tabs park a Git command on the tab; apply it
        // outside the paint pass, then reload the diff and the Git panel.
        self.apply_diff_commands();

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
        self.poll_viewer_loads(ctx);

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

        if repaint {
            ctx.request_repaint();
        }
    }

    /// Wake the frame loop for work still in flight. Runs at the end of the frame
    /// so a refresh requested later in the same frame still gets its timer.
    fn schedule_background_wakeup(&self, ctx: &egui::Context) {
        if let Some(after) = self.git.repaint_after() {
            ctx.request_repaint_after(after);
        }
        if self.workspace.index.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        if let Some(at) = self.workspace.index_dirty_at {
            ctx.request_repaint_after(
                at.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            );
        }
        if self.workspace.search.is_running() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        if let Some(delay) = Settings::save_delay() {
            ctx.request_repaint_after(delay);
        }
        if !self.viewer_loads.is_empty() || self.pending_baselines > 0 {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        if self.deferred_terminal_restore {
            ctx.request_repaint();
        }
        if let Some(delay) = self
            .tabs
            .iter()
            .filter_map(Editor::syntax_wakeup_after)
            .min()
        {
            ctx.request_repaint_after(delay);
        }
        if self.plugins.has_pending_exec() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        if self.settings.autosave {
            let mut next_autosave: Option<Duration> = None;
            for tab in &self.tabs {
                if tab.is_dirty() && tab.doc.path.is_some() {
                    let wait = Duration::from_millis(1250)
                        .saturating_sub(tab.last_edit.elapsed())
                        .max(
                            Duration::from_millis(950)
                                .saturating_sub(self.last_autosave.elapsed()),
                        )
                        .max(Duration::from_millis(50));
                    next_autosave = Some(match next_autosave {
                        Some(best) => best.min(wait),
                        None => wait,
                    });
                }
            }
            if let Some(delay) = next_autosave {
                ctx.request_repaint_after(delay);
            }
        }
        if let Some(delay) = self
            .toasts
            .iter()
            .map(|toast| {
                Duration::from_millis(3650)
                    .saturating_sub(toast.created.elapsed())
                    .max(Duration::from_millis(50))
            })
            .min()
        {
            ctx.request_repaint_after(delay);
        }
        ctx.request_repaint_after(
            Duration::from_secs(30).saturating_sub(self.last_session_save.elapsed()),
        );
    }

    pub fn set_ui_zoom(&mut self, zoom: f32, ctx: &egui::Context) {
        let clamped = ((zoom.clamp(0.7, 1.8) * 10.0).round() / 10.0).clamp(0.7, 1.8);
        self.settings.ui_zoom = clamped;
        ctx.set_zoom_factor(clamped);
        self.settings.save();
        self.toast(format!("界面缩放: {:.0}%", clamped * 100.0), ToastKind::Info);
    }

    pub fn preview_active_in_browser(&mut self) {
        self.preview_tab_in_browser(self.active);
    }

    pub fn preview_tab_in_browser(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        match tab.language {
            Language::Html => {
                if !tab.is_dirty()
                    && let Some(path) = tab.doc.path.clone()
                {
                    self.open_in_default_app(&path);
                    return;
                }
                let text = tab.doc.text();
                let title = tab.title();
                let dir = std::env::temp_dir().join("kestrel-preview");
                let safe: String = title
                    .chars()
                    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                    .take(48)
                    .collect();
                let out = dir.join(format!("{}.html", if safe.is_empty() { "preview" } else { &safe }));
                match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&out, text)) {
                    Ok(()) => self.open_in_default_app(&out),
                    Err(err) => self.toast(format!("生成网页预览失败: {err}"), ToastKind::Error),
                }
            }
            Language::Markdown => {
                let text = tab.doc.text();
                let title = tab.title();
                let path = tab.doc.path.clone();
                let dark = self.theme.kind.is_dark();
                match crate::markdown::export_html::write_temp_html_preview(
                    &title,
                    &text,
                    dark,
                    path.as_deref(),
                ) {
                    Ok(out) => {
                        self.open_in_default_app(&out);
                        self.toast("已在浏览器中打开网页预览", ToastKind::Info);
                    }
                    Err(err) => self.toast(format!("生成网页预览失败: {err}"), ToastKind::Error),
                }
            }
            _ => {
                self.toast("当前文件不是 Markdown 或 HTML 文档", ToastKind::Info);
            }
        }
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
            // Keep the top-right window control buttons (Min/Max/Close, 3*38px) clickable
            // right up to the top edge; only the outermost corner/right edge resizes there.
            let over_win_controls =
                pos.y - rect.top() <= edge && pos.x >= rect.right() - 116.0 && !right;
            let top = pos.y - rect.top() <= edge && !over_win_controls;
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
        // The canvas reads pairing from the focused editor; keep it in step with
        // the settings toggle (which the command palette can flip at runtime).
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.auto_close = self.settings.auto_close_pairs;
        }
        self.pump_background(&ctx);
        self.plugin_tick(&ctx);
        self.handle_global_shortcuts(&ctx);
        let editor_focused = self.active_editor().is_some_and(|tab| tab.has_focus);
        let terminal_focused = self.bottom.visible && self.bottom.terminal_has_focus;
        inject_os_paste_for_text_edits(&ctx, editor_focused, terminal_focused);
        self.handle_close_request(&ctx);
        self.update_window_title(&ctx);

        self.root_ui(ui);
        self.handle_drag_and_drop_overlay(&ctx);
        self.handle_os_file_drops(&ctx);
        self.palette_ui(&ctx);
        self.dialogs_ui(&ctx);
        self.toasts_ui(&ctx);
        self.handle_window_resize(&ctx);
        sync_egui_copy_to_os_clipboard(&ctx);
        self.schedule_background_wakeup(&ctx);
    }

    fn on_exit(&mut self) {
        if self.plugins.has_handlers(Event::Shutdown) {
            let ctx = self.plugin_context(None);
            self.plugins.emit(Event::Shutdown, &ctx);
        }
        if !devshot::enabled() {
            self.settings.sidebar_width = self.sidebar.width;
            self.settings.terminal_height = self.bottom.height;
            self.settings.terminal_follow_active_dir = self.bottom.follow_active_dir;
            // Settings writes are debounced; exit must land them immediately.
            self.settings.flush();
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
        let (pointer_pos, primary_down) = ctx.input(|i| {
            (
                i.pointer.interact_pos().or(i.pointer.hover_pos()),
                i.pointer.primary_down(),
            )
        });

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
        // One input lock per frame instead of three; dragging files clones the
        // list once, but the common no-drop frame stays allocation-free.
        let (hovered, pointer_pos, dropped) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.pointer.interact_pos().or(i.pointer.hover_pos()),
                if i.raw.dropped_files.is_empty() {
                    Vec::new()
                } else {
                    i.raw.dropped_files.clone()
                },
            )
        });
        let spot = pointer_pos.and_then(|pos| self.drop_spot_at(pos));
        if hovered {
            let target = spot.and_then(|spot| self.drop_preview_target(spot));
            self.update_drop_preview(ctx, target);
            self.paint_drop_preview(ctx);
        }

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

/// Fixed category order used by the command palette's grouped list.
const PALETTE_CATEGORIES: &[&str] = &["文件", "编辑", "视图", "工作区", "帮助", "插件"];

/// Stable id of a palette entry that maps to a command.
fn palette_item_id(item: &PaletteItem) -> Option<&str> {
    match item {
        PaletteItem::Command(command) => Some(command.id()),
        PaletteItem::PluginCommand(id) => Some(id.as_str()),
        _ => None,
    }
}

/// Split a Quick Open query like src/app.rs:120 into the path part and line number.
pub(crate) fn split_file_line(query: &str) -> (&str, Option<usize>) {
    let trimmed = query.trim();
    if let Some((head, tail)) = trimmed.rsplit_once(':')
        && !head.is_empty()
        && let Ok(line) = tail.trim().parse::<usize>()
        && line > 0
    {
        return (head.trim_end(), Some(line));
    }
    (trimmed, None)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_file_line_parses_trailing_line_number() {
        assert_eq!(split_file_line("src/app.rs:120"), ("src/app.rs", Some(120)));
        assert_eq!(split_file_line("  app.rs:7  "), ("app.rs", Some(7)));
        assert_eq!(split_file_line("app.rs"), ("app.rs", None));
        assert_eq!(split_file_line("C:/work/x:42"), ("C:/work/x", Some(42)));
        assert_eq!(split_file_line("::"), ("::", None));
    }
    use crate::doc::Document;
    use crate::editor::Editor;
    use crate::lsp::Diagnostic;

    fn editor_with(id: u64, severities: &[DiagSeverity]) -> Editor {
        let mut editor = Editor::new(Document::from_text(""), id, 0);
        editor.diagnostics = severities
            .iter()
            .map(|severity| Diagnostic {
                line: 0,
                col: 0,
                end_col: 1,
                start_byte: 0,
                end_byte: 1,
                severity: *severity,
                message: String::new(),
                source: String::new(),
            })
            .collect();
        editor
    }

    #[test]
    fn status_cache_rebuilds_strings_when_values_change() {
        let mut cache = StatusCache::default();
        assert_eq!(cache.cursor(3, 1, 2, 1), "行 2, 列 3");
        assert_eq!(cache.cursor(3, 1, 2, 1), "行 2, 列 3");
        assert_eq!(cache.cursor(3, 4, 2, 1), "行 5, 列 3");
        assert_eq!(cache.cursor(3, 4, 6, 3), "行 5, 列 7 (3 个光标)");
        assert_eq!(cache.cursor(9, 4, 6, 3), "行 5, 列 7 (3 个光标)");
        assert_eq!(cache.indent(4, true), "4空格");
        assert_eq!(cache.indent(2, false), "2制表符");
        assert_eq!(cache.zoom(1.5), "缩放 150%");
        let (label, counts) = cache.branch("main", false, 0, 0);
        assert_eq!((label, counts), ("main", ""));
        let (label, counts) = cache.branch("main", true, 2, 1);
        assert_eq!((label, counts), ("main*", "↑2 ↓1"));
        // Unchanged Git state must not rebuild the label or the counters.
        let label_ptr = cache.branch_label.as_ptr();
        let counts_ptr = cache.branch_counts.as_ptr();
        let (label, counts) = cache.branch("main", true, 2, 1);
        assert_eq!((label, counts), ("main*", "↑2 ↓1"));
        assert_eq!(cache.branch_label.as_ptr(), label_ptr);
        assert_eq!(cache.branch_counts.as_ptr(), counts_ptr);
    }

    #[test]
    fn status_cache_counts_diagnostics_until_the_set_changes() {
        let mut cache = StatusCache::default();
        let tabs = [editor_with(1, &[DiagSeverity::Error, DiagSeverity::Warning])];
        let (errors, warnings, error_text, warning_text) = cache.diagnostics(&tabs);
        assert_eq!(
            (errors, warnings, error_text, warning_text),
            (1, 1, "1", "1")
        );
        let key_before = cache.diag_key;
        let tabs = [editor_with(1, &[DiagSeverity::Error, DiagSeverity::Warning])];
        let (errors, warnings, ..) = cache.diagnostics(&tabs);
        assert_eq!((errors, warnings), (1, 1));
        assert_eq!(cache.diag_key, key_before);
        let tabs = [editor_with(
            1,
            &[
                DiagSeverity::Error,
                DiagSeverity::Error,
                DiagSeverity::Error,
            ],
        )];
        let (errors, warnings, error_text, _) = cache.diagnostics(&tabs);
        assert_eq!((errors, warnings, error_text), (3, 0, "3"));
    }

    #[test]
    fn default_shortcut_table_covers_every_bound_command_once() {
        let empty = std::collections::HashMap::new();
        let bindings = super::effective_shortcut_bindings(&empty);
        let mut seen = std::collections::HashSet::new();
        for &(command, shortcut) in &bindings {
            assert_eq!(command.shortcut(), Some(shortcut), "{}", command.id());
            assert!(seen.insert(command), "duplicate {}", command.id());
        }
        for command in Command::ALL {
            assert_eq!(
                seen.contains(command),
                command.shortcut().is_some(),
                "{}",
                command.id()
            );
        }
        // More specific chords sort before less specific ones so they win dispatch.
        let priority = |shortcut: Shortcut| {
            let count = (shortcut.mods.command as u8)
                + (shortcut.mods.ctrl as u8)
                + (shortcut.mods.shift as u8)
                + (shortcut.mods.alt as u8);
            (count, shortcut.mods.shift as u8, shortcut.mods.alt as u8)
        };
        for pair in bindings.windows(2) {
            assert!(priority(pair[0].1) >= priority(pair[1].1));
        }
    }

    #[test]
    fn phase4_reliability_and_settings_regressions() {
        // 1. Custom shortcut on a previously unbound command is now resolved & sorted properly.
        let unbound = Command::ALL
            .iter()
            .copied()
            .find(|cmd| cmd.shortcut().is_none())
            .expect("expected at least one unbound command");
        let chord = Shortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::ALT,
            egui::Key::F9,
        );
        let mut custom = std::collections::HashMap::new();
        custom.insert(unbound, chord);
        let bindings = super::effective_shortcut_bindings(&custom);
        assert!(bindings.iter().any(|&(cmd, sc)| cmd == unbound && sc == chord));

        // 2. StatusCache::diagnostics updates even when total diagnostic count stays equal.
        let mut cache = StatusCache::default();
        let tabs_err = [editor_with(1, &[DiagSeverity::Error])];
        assert_eq!(cache.diagnostics(&tabs_err), (1, 0, "1", "0"));
        let tabs_warn = [editor_with(1, &[DiagSeverity::Warning])];
        assert_eq!(cache.diagnostics(&tabs_warn), (0, 1, "0", "1"));

        // 3. Editor indentation detection distinguishes 2 spaces, 4 spaces, and tabs.
        let mut ed2 = Editor::new(
            Document::from_text("fn a() {\n  let x = 1;\n  let y = 2;\n}\n"),
            1,
            0,
        );
        ed2.detect_indentation();
        assert_eq!(ed2.detected_indent, Some((2, true)));

        let mut ed_tab = Editor::new(
            Document::from_text("func main() {\n\tfmt.Println()\n\treturn\n}\n"),
            2,
            0,
        );
        ed_tab.detect_indentation();
        assert_eq!(ed_tab.detected_indent, Some((4, false)));
    }

    #[test]
    fn custom_shortcut_overrides_default_when_resolved() {
        let mut overrides = std::collections::HashMap::new();
        let save = Shortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        overrides.insert(Command::Save, save);
        let default = Shortcut::new(egui::Modifiers::COMMAND, egui::Key::D);
        let resolve = |command: Command, fallback: Shortcut| -> Shortcut {
            overrides.get(&command).copied().unwrap_or(fallback)
        };
        assert_eq!(resolve(Command::Save, default), save);
        assert_eq!(resolve(Command::Copy, default), default);
    }

    use super::{MAX_OPEN_FILE_BYTES, is_large_file};
    #[test]
    fn large_file_gate_uses_eight_megabyte_boundary() {
        assert!(!is_large_file(0));
        assert!(!is_large_file(MAX_OPEN_FILE_BYTES));
        assert!(is_large_file(MAX_OPEN_FILE_BYTES + 1));
    }
}
