//! Side-by-side / inline file diff view for Git changes.
//!
//! Loading (git blob reads, file read, syntax highlighting) happens on a worker
//! thread; the UI only paints the prepared [`FileDiff`]. Rows are virtualized,
//! so large diffs stay cheap to paint.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Modifiers, Pos2, Rect, ScrollArea,
    Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2,
};

use crate::doc::{Document, EditKind, Encoding, SaveOutcome};
use crate::editor::EditorStyle;
use crate::git::{self, GitFileStatus, HunkRange};
use crate::highlight::{HighlightState, Language, Span, TokenKind};
use crate::icons::{self, Icon};
use crate::theme::Theme;
use crate::ui::{UiStyle, widgets};

const MAX_DIFF_LINES: usize = 120_000;
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
const MAX_LCS_CELLS: usize = 4_000_000;
const DIFF_MINIMAP_W: f32 = 76.0;
const DIFF_RULER_W: f32 = 12.0;
const H_SCROLLBAR_H: f32 = 10.0;

// ---------------------------------------------------------------------------
// Diff data
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Equal,
    Modified,
    Removed,
    Added,
}

#[derive(Clone, Copy, Debug)]
pub struct DiffRow {
    pub left: Option<u32>,
    pub right: Option<u32>,
    pub kind: RowKind,
    /// Character range that changed inside the line, for word-level highlight.
    pub left_hl: Option<(u32, u32)>,
    pub right_hl: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug)]
pub struct InlineRow {
    pub kind: RowKind,
    pub line: u32,
    pub left_side: bool,
    pub hl: Option<(u32, u32)>,
}

/// One Git hunk placed in the view: the header row it introduces and how many
/// lines it touches.
pub struct HunkMeta {
    pub header: String,
    pub added: usize,
    pub removed: usize,
    pub first_row: usize,
    pub inline_row: usize,
}

impl HunkMeta {
    pub fn start(&self, inline: bool) -> usize {
        if inline {
            self.inline_row
        } else {
            self.first_row
        }
    }
}

pub struct FileDiff {
    pub rel_path: String,
    pub staged: bool,
    pub left_label: String,
    pub right_label: String,
    pub left: String,
    pub right: String,
    pub left_starts: Vec<usize>,
    pub right_starts: Vec<usize>,
    pub left_spans: Vec<Span>,
    pub right_spans: Vec<Span>,
    pub rows: Vec<DiffRow>,
    pub inline_rows: Vec<InlineRow>,
    /// Row indices where a change block starts (navigation targets).
    pub changes: Vec<usize>,
    /// File header and raw hunks of the backing Git patch; the per-hunk buttons
    /// apply exactly these and nothing for untracked/binary files.
    pub patch_header: String,
    pub patch_hunks: Vec<String>,
    pub hunks: Vec<HunkMeta>,
    pub coarse: bool,
    max_chars: usize,
}

impl FileDiff {
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    /// File header plus one hunk only: the patch handed to `git apply`.
    pub fn hunk_patch(&self, index: usize) -> Option<String> {
        git::hunk_patch(&self.patch_header, &self.patch_hunks, index)
    }

    /// View row of a content row once the hunk header rows above it are counted.
    pub fn display_row(&self, row: usize, inline: bool) -> usize {
        row + self
            .hunks
            .iter()
            .filter(|hunk| hunk.start(inline) <= row)
            .count()
    }
}

struct LoadedText {
    starts: Vec<usize>,
    spans: Vec<Span>,
}

// ---------------------------------------------------------------------------
// Selection, Find, Inline Edit, Minimap State
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffSide {
    Left,
    Right,
    Inline,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DiffPos {
    /// Content row index (`0..rows.len()` or `0..inline_rows.len()`).
    pub row: usize,
    /// Character column offset inside that row's line text.
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiffSelection {
    pub side: DiffSide,
    pub anchor: DiffPos,
    pub caret: DiffPos,
}

impl DiffSelection {
    pub fn ordered(self) -> (DiffPos, DiffPos) {
        if self.anchor <= self.caret {
            (self.anchor, self.caret)
        } else {
            (self.caret, self.anchor)
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.caret
    }
}

#[derive(Clone, Copy, Debug)]
struct DiffHit {
    side: DiffSide,
    row: usize,
    col: usize,
    source_line: Option<usize>,
    hunk_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct DiffInlineEdit {
    /// 0-based inclusive line range in the working-tree file on disk.
    pub start_line: usize,
    pub end_line: usize,
    pub original: String,
    pub buffer: String,
    pub focus_requested: bool,
    pub error: Option<String>,
}

impl DiffInlineEdit {
    pub fn is_dirty(&self) -> bool {
        self.buffer != self.original
    }
}

#[derive(Clone, Debug, Default)]
pub struct DiffFindState {
    pub open: bool,
    pub query: String,
    pub case_sensitive: bool,
    pub focus_requested: bool,
    /// `(side, content_row, start_char, len_chars)`
    pub matches: Vec<(DiffSide, usize, usize, usize)>,
    pub active: Option<usize>,
    cache_key: (String, bool, bool, u64),
}

#[derive(Clone, Copy)]
struct DiffMinimapBucket {
    left_kind: RowKind,
    right_kind: RowKind,
    inline_kind: RowKind,
    is_hunk: bool,
    left_indent: u16,
    left_len: u16,
    left_color: Color32,
    right_indent: u16,
    right_len: u16,
    right_color: Color32,
}

#[derive(Default)]
struct DiffMinimapCache {
    key: (u64, bool, u8, usize, u32),
    buckets: Vec<DiffMinimapBucket>,
}

// ---------------------------------------------------------------------------
// Tab state
// ---------------------------------------------------------------------------

pub struct DiffTab {
    pub path: PathBuf,
    pub rel_path: String,
    pub staged: bool,
    pub status: GitFileStatus,
    pub orig_rel: Option<String>,
    pub inline: bool,
    pub data: Option<FileDiff>,
    pub data_rev: u64,
    pub error: Option<String>,
    pub loading: bool,
    pub rx: Option<Receiver<Result<FileDiff, String>>>,
    pub current_change: usize,
    pub pending_scroll_row: Option<usize>,
    pub pending_scroll_y: Option<f32>,
    pub last_scroll_y: f32,
    pub h_scroll: f32,
    pub tab_size: usize,
    /// Hunk button command for `KestrelApp` to apply outside the paint pass.
    pub pending: Option<DiffCommand>,
    pub selection: Option<DiffSelection>,
    pub selecting: bool,
    pub has_focus: bool,
    context_hit: Option<DiffHit>,
    pub find: DiffFindState,
    pub inline_edit: Option<DiffInlineEdit>,
    minimap_cache: DiffMinimapCache,
}

impl DiffTab {
    pub fn new(path: PathBuf, rel_path: String, staged: bool, status: GitFileStatus) -> Self {
        Self {
            path,
            rel_path,
            staged,
            status,
            orig_rel: None,
            inline: false,
            data: None,
            data_rev: 0,
            error: None,
            loading: false,
            rx: None,
            current_change: 0,
            pending_scroll_row: None,
            pending_scroll_y: None,
            last_scroll_y: 0.0,
            h_scroll: 0.0,
            tab_size: 4,
            pending: None,
            selection: None,
            selecting: false,
            has_focus: false,
            context_hit: None,
            find: DiffFindState::default(),
            inline_edit: None,
            minimap_cache: DiffMinimapCache::default(),
        }
    }

    pub fn can_edit_working_tree(&self) -> bool {
        !self.staged && self.status != GitFileStatus::Deleted && self.path.exists()
    }

    /// Start computing the diff on a worker thread.
    pub fn start_load(&mut self, root: PathBuf, tab_size: usize) {
        let path = self.path.clone();
        let staged = self.staged;
        let status = self.status;
        let orig_rel = self.orig_rel.clone();
        self.tab_size = tab_size;
        self.loading = true;
        self.data = None;
        self.error = None;
        self.current_change = 0;
        self.h_scroll = 0.0;
        self.pending = None;
        self.selection = None;
        self.selecting = false;
        self.context_hit = None;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("kestrel-diff".into())
            .spawn(move || {
                let result = load_file_diff(
                    &root,
                    &path,
                    staged,
                    status,
                    orig_rel.as_deref(),
                    tab_size,
                );
                let _ = tx.send(result);
            })
            .ok();
        self.rx = Some(rx);
    }

    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.loading = false;
                self.rx = None;
                match result {
                    Ok(data) => {
                        self.changes_scroll_reset(&data);
                        self.data = Some(data);
                        self.data_rev = self.data_rev.wrapping_add(1);
                        self.error = None;
                    }
                    Err(err) => {
                        self.error = Some(err);
                        self.data = None;
                    }
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.loading = false;
                self.rx = None;
                self.error = Some("差异计算线程异常退出".to_string());
                true
            }
        }
    }

    fn changes_scroll_reset(&mut self, _data: &FileDiff) {
        self.current_change = 0;
    }

    /// Return selected text on the active side, or the caret line if selection is empty.
    pub fn copy_selection(&self) -> Option<String> {
        let data = self.data.as_ref()?;
        let sel = self.selection?;
        extract_selection_text(data, sel, self.inline)
            .or_else(|| row_line_text(data, sel.side, sel.caret.row, self.inline).map(str::to_string))
    }

    /// Select all lines on the active side (Right by default in side-by-side, Inline in inline).
    pub fn select_all_active_side(&mut self) {
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let side = if self.inline {
            DiffSide::Inline
        } else {
            self.selection
                .map(|s| s.side)
                .filter(|s| *s != DiffSide::Inline)
                .unwrap_or(DiffSide::Right)
        };
        let total = if self.inline {
            data.inline_rows.len()
        } else {
            data.rows.len()
        };
        if total == 0 {
            return;
        }
        let last_row = total - 1;
        let last_col = row_line_text(data, side, last_row, self.inline)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        self.selection = Some(DiffSelection {
            side,
            anchor: DiffPos { row: 0, col: 0 },
            caret: DiffPos {
                row: last_row,
                col: last_col,
            },
        });
        self.has_focus = true;
    }

    /// Open the in-diff find bar, pre-populating a single-line selection if any.
    pub fn open_find(&mut self) {
        if let (Some(data), Some(sel)) = (self.data.as_ref(), self.selection)
            && !sel.is_empty()
            && sel.anchor.row == sel.caret.row
            && let Some(text) = extract_selection_text(data, sel, self.inline)
            && !text.is_empty()
            && !text.contains('\n')
        {
            self.find.query = text;
        }
        self.find.open = true;
        self.find.focus_requested = true;
    }

    pub fn find_step(&mut self, forward: bool) {
        let Some(data) = self.data.as_ref() else {
            return;
        };
        refresh_diff_find(&mut self.find, data, self.inline, self.data_rev);
        let count = self.find.matches.len();
        if count == 0 {
            self.find.active = None;
            return;
        }
        let next = match self.find.active {
            Some(i) if forward => (i + 1) % count,
            Some(i) => (i + count - 1) % count,
            None => 0,
        };
        self.find.active = Some(next);
        let (side, row, start_col, len) = self.find.matches[next];
        self.selection = Some(DiffSelection {
            side,
            anchor: DiffPos {
                row,
                col: start_col,
            },
            caret: DiffPos {
                row,
                col: start_col + len,
            },
        });
        self.pending_scroll_row = Some(row);
    }

    /// Return the 1-based working-tree (or fallback left) line number best representing
    /// where the user is currently looking in the diff.
    pub fn active_source_line(&self) -> Option<usize> {
        let data = self.data.as_ref()?;
        if let Some(edit) = &self.inline_edit {
            return Some(edit.start_line + 1);
        }
        if let Some(sel) = self.selection
            && let Some(line) = source_line_for_row(data, sel.caret.row, self.inline)
        {
            return Some(line);
        }
        if let Some(&change_row) = data.changes.get(self.current_change)
            && let Some(line) = source_line_for_row(data, change_row, false)
        {
            return Some(line);
        }
        None
    }

    /// Open the in-place working-tree editor for 0-based inclusive line range `start_line..=end_line`.
    pub fn open_inline_edit(&mut self, start_line: usize, end_line: usize) {
        if !self.can_edit_working_tree() {
            return;
        }
        let (start, end) = if start_line <= end_line {
            (start_line, end_line)
        } else {
            (end_line, start_line)
        };
        match load_working_lines(&self.path, start, end) {
            Ok((clamped_start, clamped_end, text)) => {
                self.inline_edit = Some(DiffInlineEdit {
                    start_line: clamped_start,
                    end_line: clamped_end,
                    original: text.clone(),
                    buffer: text,
                    focus_requested: true,
                    error: None,
                });
            }
            Err(err) => {
                self.inline_edit = Some(DiffInlineEdit {
                    start_line: start,
                    end_line: end,
                    original: String::new(),
                    buffer: String::new(),
                    focus_requested: false,
                    error: Some(err),
                });
            }
        }
    }

    /// Save the active inline edit to disk, preserving encoding & EOL.
    /// Returns `Some(Ok(line_1based))` on save, `Some(Err(msg))` on failure, `None` if no edit is open.
    pub fn save_inline_edit(&mut self) -> Option<Result<usize, String>> {
        let edit = self.inline_edit.as_mut()?;
        match replace_working_lines(&self.path, edit.start_line, edit.end_line, &edit.buffer) {
            Ok(()) => {
                let line = edit.start_line + 1;
                self.pending_scroll_y = Some(self.last_scroll_y);
                self.inline_edit = None;
                Some(Ok(line))
            }
            Err(err) => {
                edit.error = Some(err.clone());
                Some(Err(err))
            }
        }
    }
}

pub enum DiffAction {
    None,
    OpenFile(PathBuf),
    OpenFileAtLine(PathBuf, usize),
    Refresh,
    SavedWorkingFile { path: PathBuf, line: usize },
}

/// A Git hunk command raised by a hunk header button. The `DiffAction` dispatch
/// lives in `ui/mod.rs`, which this batch does not touch, so the view parks the
/// command on the tab and `KestrelApp::pump_background` drains it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffCommand {
    Stage { hunk: usize },
    Unstage { hunk: usize },
    Discard { hunk: usize },
}

impl DiffCommand {
    pub fn hunk(self) -> usize {
        match self {
            Self::Stage { hunk } | Self::Unstage { hunk } | Self::Discard { hunk } => hunk,
        }
    }

    pub fn success_label(self) -> &'static str {
        match self {
            Self::Stage { .. } => "已暂存所选差异块",
            Self::Unstage { .. } => "已取消暂存所选差异块",
            Self::Discard { .. } => "已丢弃所选差异块",
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn normalize_source(text: Option<String>) -> String {
    text.map(|text| text.replace("\r\n", "\n"))
        .unwrap_or_default()
}

fn build_loaded(text: &str, language: Language, highlight: bool) -> LoadedText {
    let starts = line_starts(text);
    let spans = if highlight && language != Language::Plain && text.len() <= 1_000_000 {
        let mut state = HighlightState::new(language);
        state.parse(text, 1);
        state.spans(text, 1).to_vec()
    } else {
        Vec::new()
    };
    LoadedText { starts, spans }
}

/// Normalize tabs to spaces so diff rows and word spans share one coordinate
/// system, and strip trailing CR from line ends.
fn normalize_text(text: &str, tab_size: usize) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let tab_size = tab_size.max(1);
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            for _ in 0..tab_size {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn line_starts(text: &str) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::with_capacity(text.len() / 32 + 1);
    starts.push(0);
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' && index + 1 < text.len() {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_text<'a>(text: &'a str, starts: &[usize], index: usize) -> &'a str {
    let Some(&start) = starts.get(index) else {
        return "";
    };
    let mut end = starts
        .get(index + 1)
        .map(|next| next.saturating_sub(1))
        .unwrap_or(text.len());
    if text.as_bytes().get(end.saturating_sub(1)) == Some(&10u8) {
        end = end.saturating_sub(1);
    }
    text.get(start..end).unwrap_or("")
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.strip_suffix('\n').unwrap_or(text).split('\n').collect()
    }
}

fn working_text(path: &Path) -> Option<String> {
    let doc = crate::doc::Document::load(path).ok()?;
    if doc.encoding == crate::doc::Encoding::Binary {
        return None;
    }
    Some(doc.text().replace("\r\n", "\n"))
}

#[allow(clippy::too_many_arguments)]
pub fn load_file_diff(
    root: &Path,
    path: &Path,
    staged: bool,
    status: GitFileStatus,
    orig_rel: Option<&str>,
    tab_size: usize,
) -> Result<FileDiff, String> {
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let language = Language::from_path(path);
    let (left_label, right_label, left_blob, right_blob, right_text) = match (staged, status) {
        (true, GitFileStatus::Untracked) => (
            "空".to_string(),
            "索引".to_string(),
            None,
            git::read_index_rel(root, &rel_path),
            None,
        ),
        (true, GitFileStatus::Added) => (
            "HEAD".to_string(),
            "索引".to_string(),
            git::read_head_rel(root, &rel_path),
            git::read_index_rel(root, &rel_path),
            None,
        ),
        (true, _) => (
            "HEAD".to_string(),
            "索引".to_string(),
            orig_rel
                .and_then(|old| git::read_head_rel(root, old))
                .or_else(|| git::read_head_rel(root, &rel_path)),
            git::read_index_rel(root, &rel_path),
            None,
        ),
        (false, GitFileStatus::Untracked) => (
            "空".to_string(),
            "工作区".to_string(),
            None,
            None,
            working_text(path),
        ),
        (false, GitFileStatus::Deleted) => (
            "索引".to_string(),
            "已删除".to_string(),
            git::read_index_rel(root, &rel_path)
                .or_else(|| git::read_head_rel(root, &rel_path)),
            None,
            Some(String::new()),
        ),
        (false, _) => (
            "索引".to_string(),
            "工作区".to_string(),
            git::read_index_rel(root, &rel_path)
                .or_else(|| git::read_head_rel(root, &rel_path)),
            None,
            working_text(path),
        ),
    };

    let left_text = normalize_text(&normalize_source(left_blob), tab_size);
    let right_text = normalize_text(&normalize_source(right_text.or(right_blob)), tab_size);

    let total_bytes = left_text.len() + right_text.len();
    if total_bytes > MAX_DIFF_BYTES {
        return Err("文件差异过大（超过 8 MB），无法显示逐行对比".to_string());
    }
    let left_lines = split_lines(&left_text);
    let right_lines = split_lines(&right_text);
    if left_lines.len() + right_lines.len() > MAX_DIFF_LINES {
        return Err("文件行数过多，无法显示逐行对比".to_string());
    }

    let (rows, changes, coarse) = compute_rows(&left_lines, &right_lines);
    let left_loaded = build_loaded(&left_text, language, true);
    let right_loaded = build_loaded(&right_text, language, true);
    let max_chars = left_lines
        .iter()
        .chain(right_lines.iter())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let inline_rows = build_inline_rows(&rows);
    let patch = if staged {
        git::staged_patch(root, &rel_path)
    } else {
        git::worktree_patch(root, &rel_path)
    };
    let (patch_header, patch_hunks) = git::split_hunks(&patch);
    let hunks = place_hunks(&patch_hunks, &rows, &inline_rows);

    Ok(FileDiff {
        rel_path,
        staged,
        left_label,
        right_label,
        left: left_text,
        right: right_text,
        left_starts: left_loaded.starts,
        right_starts: right_loaded.starts,
        left_spans: left_loaded.spans,
        right_spans: right_loaded.spans,
        rows,
        inline_rows,
        changes,
        patch_header,
        patch_hunks,
        hunks,
        coarse,
        max_chars,
    })
}

/// Place every raw patch hunk on the view row where its header belongs. The UI
/// diff algorithm may pair lines differently than git, but line numbers stay
/// exact, so a hunk starts at the first row whose old/new line falls inside it.
fn place_hunks(raw: &[String], rows: &[DiffRow], inline_rows: &[InlineRow]) -> Vec<HunkMeta> {
    let mut hunks = Vec::with_capacity(raw.len());
    let mut row_cursor = 0;
    let mut inline_cursor = 0;
    for text in raw {
        let header = text
            .lines()
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .to_string();
        let mut added = 0;
        let mut removed = 0;
        for line in text.lines().skip(1) {
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
        }
        if let Some(range) = git::parse_hunk_header(&header) {
            row_cursor = find_hunk_row(rows, range, row_cursor);
            inline_cursor = find_hunk_inline_row(inline_rows, range, inline_cursor);
        }
        hunks.push(HunkMeta {
            header,
            added,
            removed,
            first_row: row_cursor,
            inline_row: inline_cursor,
        });
    }
    hunks
}

fn find_hunk_row(rows: &[DiffRow], range: HunkRange, from: usize) -> usize {
    let old_end = range.old_start as u64 + range.old_count as u64;
    let new_end = range.new_start as u64 + range.new_count as u64;
    let hit = |row: &DiffRow| {
        row.left.is_some_and(|line| {
            let line = line as u64 + 1;
            line >= range.old_start as u64 && line < old_end
        }) || row.right.is_some_and(|line| {
            let line = line as u64 + 1;
            line >= range.new_start as u64 && line < new_end
        })
    };
    let start = from.min(rows.len());
    rows[start..]
        .iter()
        .position(hit)
        .map_or(start, |offset| start + offset)
}

fn find_hunk_inline_row(rows: &[InlineRow], range: HunkRange, from: usize) -> usize {
    let old_end = range.old_start as u64 + range.old_count as u64;
    let new_end = range.new_start as u64 + range.new_count as u64;
    let hit = |row: &InlineRow| {
        let line = row.line as u64 + 1;
        if row.left_side {
            line >= range.old_start as u64 && line < old_end
        } else {
            line >= range.new_start as u64 && line < new_end
        }
    };
    let start = from.min(rows.len());
    rows[start..]
        .iter()
        .position(hit)
        .map_or(start, |offset| start + offset)
}

fn build_inline_rows(rows: &[DiffRow]) -> Vec<InlineRow> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match row.kind {
            RowKind::Equal => {
                if let Some(line) = row.right {
                    out.push(InlineRow {
                        kind: RowKind::Equal,
                        line,
                        left_side: false,
                        hl: None,
                    });
                }
            }
            RowKind::Modified => {
                if let Some(line) = row.left {
                    out.push(InlineRow {
                        kind: RowKind::Removed,
                        line,
                        left_side: true,
                        hl: row.left_hl,
                    });
                }
                if let Some(line) = row.right {
                    out.push(InlineRow {
                        kind: RowKind::Added,
                        line,
                        left_side: false,
                        hl: row.right_hl,
                    });
                }
            }
            RowKind::Removed => {
                if let Some(line) = row.left {
                    out.push(InlineRow {
                        kind: RowKind::Removed,
                        line,
                        left_side: true,
                        hl: None,
                    });
                }
            }
            RowKind::Added => {
                if let Some(line) = row.right {
                    out.push(InlineRow {
                        kind: RowKind::Added,
                        line,
                        left_side: false,
                        hl: None,
                    });
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Line diff (Myers with bounded edit distance)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn compute_rows(
    left: &[&str],
    right: &[&str],
) -> (Vec<DiffRow>, Vec<usize>, bool) {
    // Trim the common prefix/suffix so Myers only runs on the changed window.
    let mut prefix = 0;
    while prefix < left.len() && prefix < right.len() && left[prefix] == right[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < left.len() - prefix
        && suffix < right.len() - prefix
        && left[left.len() - 1 - suffix] == right[right.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let left_mid = &left[prefix..left.len() - suffix];
    let right_mid = &right[prefix..right.len() - suffix];

    let mut rows: Vec<DiffRow> = Vec::with_capacity(left.len().max(right.len()));
    for index in 0..prefix {
        rows.push(DiffRow {
            left: Some(index as u32),
            right: Some(index as u32),
            kind: RowKind::Equal,
            left_hl: None,
            right_hl: None,
        });
    }
    let mut coarse = false;
    let mut ops = lcs_ops(left_mid, right_mid);
    if ops.is_none() {
        coarse = true;
        ops = Some(fallback_ops(left_mid, right_mid));
    }
    let ops = ops.unwrap_or_default();
    let mut pending_deleted: Vec<usize> = Vec::new();
    let mut pending_inserted: Vec<usize> = Vec::new();
    let flush = |deleted: &mut Vec<usize>,
                     inserted: &mut Vec<usize>,
                     rows: &mut Vec<DiffRow>| {
        let paired = deleted.len().min(inserted.len());
        for i in 0..paired {
            let old_line = left[prefix + deleted[i]];
            let new_line = right[prefix + inserted[i]];
            let (left_hl, right_hl) = word_diff(old_line, new_line);
            rows.push(DiffRow {
                left: Some((prefix + deleted[i]) as u32),
                right: Some((prefix + inserted[i]) as u32),
                kind: RowKind::Modified,
                left_hl,
                right_hl,
            });
        }
        for &deleted_index in &deleted[paired..] {
            rows.push(DiffRow {
                left: Some((prefix + deleted_index) as u32),
                right: None,
                kind: RowKind::Removed,
                left_hl: None,
                right_hl: None,
            });
        }
        for &inserted_index in &inserted[paired..] {
            rows.push(DiffRow {
                left: None,
                right: Some((prefix + inserted_index) as u32),
                kind: RowKind::Added,
                left_hl: None,
                right_hl: None,
            });
        }
        deleted.clear();
        inserted.clear();
    };

    let mut changes: Vec<usize> = Vec::new();
    for op in ops {
        match op {
            Op::Equal(left_index, right_index) => {
                flush(&mut pending_deleted, &mut pending_inserted, &mut rows);
                rows.push(DiffRow {
                    left: Some((prefix + left_index) as u32),
                    right: Some((prefix + right_index) as u32),
                    kind: RowKind::Equal,
                    left_hl: None,
                    right_hl: None,
                });
            }
            Op::Delete(index) => pending_deleted.push(index),
            Op::Insert(index) => pending_inserted.push(index),
        }
    }
    flush(&mut pending_deleted, &mut pending_inserted, &mut rows);
    for index in 0..suffix {
        rows.push(DiffRow {
            left: Some((left.len() - suffix + index) as u32),
            right: Some((right.len() - suffix + index) as u32),
            kind: RowKind::Equal,
            left_hl: None,
            right_hl: None,
        });
    }
    for (index, row) in rows.iter().enumerate() {
        if row.kind != RowKind::Equal
            && (changes.is_empty() || *changes.last().unwrap() + 1 < index)
        {
            changes.push(index);
        }
    }
    (rows, changes, coarse)
}

fn lcs_ops(left: &[&str], right: &[&str]) -> Option<Vec<Op>> {
    let n = left.len();
    let m = right.len();
    if n == 0 {
        return Some((0..m).map(Op::Insert).collect());
    }
    if m == 0 {
        return Some((0..n).map(Op::Delete).collect());
    }
    if n.saturating_mul(m) > MAX_LCS_CELLS {
        return None;
    }
    let stride = m + 1;
    let mut dp = vec![0u16; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * stride + j] = if left[i] == right[j] {
                dp[(i + 1) * stride + j + 1] + 1
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n || j < m {
        if i < n && j < m && left[i] == right[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if j >= m || (i < n && dp[(i + 1) * stride + j] >= dp[i * stride + j + 1]) {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    Some(ops)
}

fn fallback_ops(left: &[&str], right: &[&str]) -> Vec<Op> {
    // Coarse but predictable: try to resync on identical lines within a small
    // window, otherwise treat the region as one replacement block.
    let mut ops = Vec::with_capacity(left.len().max(right.len()));
    let mut i = 0usize;
    let mut j = 0usize;
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
            continue;
        }
        let window = 48usize;
        let mut matched = None;
        for distance in 1..=window {
            if i + distance < left.len() && left[i + distance] == right[j] {
                matched = Some((distance, 0usize));
                break;
            }
            if j + distance < right.len() && left[i] == right[j + distance] {
                matched = Some((0usize, distance));
                break;
            }
        }
        match matched {
            Some((di, dj)) => {
                for k in 0..di {
                    ops.push(Op::Delete(i + k));
                }
                for k in 0..dj {
                    ops.push(Op::Insert(j + k));
                }
                i += di;
                j += dj;
            }
            None => {
                let block = 64usize;
                let di = block.min(left.len() - i);
                let dj = block.min(right.len() - j);
                for k in 0..di {
                    ops.push(Op::Delete(i + k));
                }
                for k in 0..dj {
                    ops.push(Op::Insert(j + k));
                }
                i += di;
                j += dj;
            }
        }
    }
    while i < left.len() {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < right.len() {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

/// Highlight ranges for the changed middle of a replaced line pair.
type WordRanges = (Option<(u32, u32)>, Option<(u32, u32)>);

fn word_diff(left: &str, right: &str) -> WordRanges {
    let a: Vec<char> = left.chars().collect();
    let b: Vec<char> = right.chars().collect();
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let left_len = a.len() - prefix - suffix;
    let right_len = b.len() - prefix - suffix;
    let left_hl = (left_len > 0).then_some((prefix as u32, left_len as u32));
    let right_hl = (right_len > 0).then_some((prefix as u32, right_len as u32));
    (left_hl, right_hl)
}

fn unified_diff(data: &FileDiff) -> String {
    let mut out = String::new();
    out.push_str("--- a/");
    out.push_str(&data.rel_path);
    out.push('\n');
    out.push_str("+++ b/");
    out.push_str(&data.rel_path);
    out.push('\n');
    for row in &data.rows {
        match row.kind {
            RowKind::Equal => {
                if let Some(line) = row.left {
                    out.push(' ');
                    out.push_str(line_text(&data.left, &data.left_starts, line as usize));
                    out.push('\n');
                }
            }
            RowKind::Modified | RowKind::Removed => {
                if let Some(line) = row.left {
                    out.push('-');
                    out.push_str(line_text(&data.left, &data.left_starts, line as usize));
                    out.push('\n');
                }
                if row.kind == RowKind::Modified
                    && let Some(line) = row.right
                {
                    out.push('+');
                    out.push_str(line_text(&data.right, &data.right_starts, line as usize));
                    out.push('\n');
                }
            }
            RowKind::Added => {
                if let Some(line) = row.right {
                    out.push('+');
                    out.push_str(line_text(&data.right, &data.right_starts, line as usize));
                    out.push('\n');
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Selection, Find, Working-Tree Edit & Minimap Helpers
// ---------------------------------------------------------------------------

fn row_line_text(data: &FileDiff, side: DiffSide, row: usize, inline: bool) -> Option<&str> {
    if inline || side == DiffSide::Inline {
        let entry = data.inline_rows.get(row)?;
        if entry.left_side {
            Some(line_text(&data.left, &data.left_starts, entry.line as usize))
        } else {
            Some(line_text(
                &data.right,
                &data.right_starts,
                entry.line as usize,
            ))
        }
    } else {
        let r = data.rows.get(row)?;
        match side {
            DiffSide::Left => {
                let idx = r.left?;
                Some(line_text(&data.left, &data.left_starts, idx as usize))
            }
            DiffSide::Right => {
                let idx = r.right?;
                Some(line_text(&data.right, &data.right_starts, idx as usize))
            }
            DiffSide::Inline => None,
        }
    }
}

fn slice_chars(s: &str, from: usize, to: usize) -> &str {
    if from >= to {
        return "";
    }
    let mut byte_start = s.len();
    let mut byte_end = s.len();
    for (ci, (bi, _)) in s.char_indices().enumerate() {
        if ci == from {
            byte_start = bi;
        }
        if ci == to {
            byte_end = bi;
            break;
        }
    }
    if byte_start > byte_end {
        ""
    } else {
        &s[byte_start..byte_end]
    }
}

fn extract_selection_text(data: &FileDiff, sel: DiffSelection, inline: bool) -> Option<String> {
    let (start, end) = sel.ordered();
    if start == end {
        return None;
    }
    let mut out = String::new();
    let mut any = false;
    for row in start.row..=end.row {
        let Some(line) = row_line_text(data, sel.side, row, inline) else {
            continue;
        };
        if any {
            out.push('\n');
        }
        any = true;
        if start.row == end.row {
            out.push_str(slice_chars(line, start.col, end.col));
        } else if row == start.row {
            out.push_str(slice_chars(line, start.col, usize::MAX));
        } else if row == end.row {
            out.push_str(slice_chars(line, 0, end.col));
        } else {
            out.push_str(line);
        }
    }
    if any { Some(out) } else { None }
}

fn char_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_ascii_alphanumeric() || ch == '_' {
        1
    } else if ch.is_alphanumeric() {
        2
    } else {
        3
    }
}

fn word_bounds_at(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let idx = col.min(chars.len().saturating_sub(1));
    let cls = char_class(chars[idx]);
    let mut start = idx;
    while start > 0 && char_class(chars[start - 1]) == cls {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && char_class(chars[end]) == cls {
        end += 1;
    }
    (start, end)
}

/// Best 1-based working-tree (or fallback left) line number for a content row.
fn source_line_for_row(data: &FileDiff, row: usize, inline: bool) -> Option<usize> {
    if inline {
        let entry = data.inline_rows.get(row)?;
        if !entry.left_side {
            return Some(entry.line as usize + 1);
        }
        // Walk nearby inline rows to find the closest working-tree line.
        for delta in 1..=8 {
            if let Some(next) = data.inline_rows.get(row + delta)
                && !next.left_side
            {
                return Some(next.line as usize + 1);
            }
            if row >= delta
                && let Some(prev) = data.inline_rows.get(row - delta)
                && !prev.left_side
            {
                return Some(prev.line as usize + 1);
            }
        }
        Some(entry.line as usize + 1)
    } else {
        let r = data.rows.get(row)?;
        if let Some(line) = r.right {
            return Some(line as usize + 1);
        }
        for delta in 1..=8 {
            if let Some(next) = data.rows.get(row + delta)
                && let Some(line) = next.right
            {
                return Some(line as usize + 1);
            }
            if row >= delta
                && let Some(prev) = data.rows.get(row - delta)
                && let Some(line) = prev.right
            {
                return Some(line as usize + 1);
            }
        }
        r.left.map(|l| l as usize + 1)
    }
}

/// 0-based inclusive working-tree line range covered by `sel`, if any.
fn working_line_range_for_selection(
    data: &FileDiff,
    sel: DiffSelection,
    inline: bool,
) -> Option<(usize, usize)> {
    let (start, end) = sel.ordered();
    let mut min_line: Option<usize> = None;
    let mut max_line: Option<usize> = None;
    for row in start.row..=end.row {
        let line = if inline {
            data.inline_rows
                .get(row)
                .filter(|e| !e.left_side)
                .map(|e| e.line as usize)
        } else {
            data.rows.get(row).and_then(|r| r.right.map(|l| l as usize))
        };
        if let Some(l) = line {
            min_line = Some(min_line.map_or(l, |m| m.min(l)));
            max_line = Some(max_line.map_or(l, |m| m.max(l)));
        }
    }
    match (min_line, max_line) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => source_line_for_row(data, start.row, inline).map(|l| {
            let l0 = l.saturating_sub(1);
            (l0, l0)
        }),
    }
}

/// 0-based inclusive working-tree line range for a Git hunk header.
fn hunk_working_line_range(data: &FileDiff, hunk_index: usize) -> Option<(usize, usize)> {
    let hunk = data.hunks.get(hunk_index)?;
    if let Some(range) = git::parse_hunk_header(&hunk.header) {
        let start = (range.new_start as usize).saturating_sub(1);
        let end = if range.new_count == 0 {
            start
        } else {
            start + range.new_count as usize - 1
        };
        return Some((start, end));
    }
    source_line_for_row(data, hunk.first_row, false).map(|l| {
        let l0 = l.saturating_sub(1);
        (l0, l0)
    })
}

/// Return the governing Git hunk index if `content_row` falls within that hunk.
fn hunk_index_for_row(data: &FileDiff, content_row: usize, inline: bool) -> Option<usize> {
    if data.hunks.is_empty() {
        return None;
    }
    let idx = data.hunks.iter().rposition(|h| h.start(inline) <= content_row)?;
    let hunk = &data.hunks[idx];
    if let Some(range) = git::parse_hunk_header(&hunk.header) {
        let old_end = range.old_start as u64 + range.old_count as u64;
        let new_end = range.new_start as u64 + range.new_count as u64;
        let inside = if inline {
            data.inline_rows.get(content_row).is_some_and(|e| {
                let l = e.line as u64 + 1;
                if e.left_side {
                    l >= range.old_start as u64 && l <= old_end.max(range.old_start as u64)
                } else {
                    l >= range.new_start as u64 && l <= new_end.max(range.new_start as u64)
                }
            })
        } else {
            data.rows.get(content_row).is_some_and(|r| {
                r.left.is_some_and(|l| {
                    let l = l as u64 + 1;
                    l >= range.old_start as u64 && l <= old_end.max(range.old_start as u64)
                }) || r.right.is_some_and(|l| {
                    let l = l as u64 + 1;
                    l >= range.new_start as u64 && l <= new_end.max(range.new_start as u64)
                })
            })
        };
        if inside {
            return Some(idx);
        }
    }
    None
}

fn load_working_lines(
    path: &Path,
    start_line: usize,
    end_line: usize,
) -> Result<(usize, usize, String), String> {
    let doc = Document::load(path).map_err(|e| format!("读取工作区文件失败: {e}"))?;
    if doc.encoding == Encoding::Binary {
        return Err("二进制文件不支持直接编辑".to_string());
    }
    let max_line = doc.line_count().saturating_sub(1);
    let start = start_line.min(max_line);
    let end = end_line.clamp(start, max_line);
    let mut lines = Vec::with_capacity(end - start + 1);
    for line in start..=end {
        lines.push(doc.line_content(line));
    }
    Ok((start, end, lines.join("\n")))
}

fn replace_working_lines(
    path: &Path,
    start_line: usize,
    end_line: usize,
    replacement: &str,
) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("读取工作区文件失败: {e}"))?;
    if doc.readonly || doc.encoding == Encoding::Binary {
        return Err("目标文件为只读或二进制文件，无法保存".to_string());
    }
    let max_line = doc.line_count().saturating_sub(1);
    let start = start_line.min(max_line);
    let end = end_line.clamp(start, max_line);
    let start_char = doc.line_to_char(start);
    let end_char = doc.line_to_char(end) + doc.line_char_len(end);
    let normalized = replacement.replace("\r\n", "\n");
    doc.edit(
        start_char,
        end_char,
        &normalized,
        EditKind::Other,
        (start_char, start_char),
        (start_char, start_char),
    );
    match doc.save() {
        SaveOutcome::Saved => Ok(()),
        SaveOutcome::DiskChanged => Err("文件已在外部被修改，请刷新差异后重试".to_string()),
        SaveOutcome::ReadOnly => Err("文件只读，无法写入".to_string()),
        SaveOutcome::Failed(err) => Err(format!("写入文件失败: {err}")),
    }
}

fn find_occurrences_in_line(line: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() || line.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let q_chars = query.chars().count();
    if case_sensitive {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(query) {
            let byte_pos = search_from + rel;
            let col = line[..byte_pos].chars().count();
            out.push((col, q_chars));
            search_from = byte_pos + query.len().max(1);
        }
    } else {
        let l_chars: Vec<char> = line.chars().collect();
        let q_lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        if q_lower.is_empty() {
            return out;
        }
        let mut i = 0usize;
        while i + q_lower.len() <= l_chars.len() {
            let matched = l_chars[i..i + q_lower.len()]
                .iter()
                .zip(q_lower.iter())
                .all(|(a, b)| a.to_lowercase().next() == Some(*b));
            if matched {
                out.push((i, q_lower.len()));
                i += q_lower.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

fn refresh_diff_find(find: &mut DiffFindState, data: &FileDiff, inline: bool, data_rev: u64) {
    let key = (find.query.clone(), find.case_sensitive, inline, data_rev);
    if find.cache_key == key {
        return;
    }
    find.cache_key = key;
    find.matches.clear();
    if find.query.is_empty() {
        find.active = None;
        return;
    }
    const MAX_MATCHES: usize = 5000;
    if inline {
        for (row, entry) in data.inline_rows.iter().enumerate() {
            let text = if entry.left_side {
                line_text(&data.left, &data.left_starts, entry.line as usize)
            } else {
                line_text(&data.right, &data.right_starts, entry.line as usize)
            };
            for (col, len) in find_occurrences_in_line(text, &find.query, find.case_sensitive) {
                find.matches.push((DiffSide::Inline, row, col, len));
                if find.matches.len() >= MAX_MATCHES {
                    break;
                }
            }
            if find.matches.len() >= MAX_MATCHES {
                break;
            }
        }
    } else {
        for (row, entry) in data.rows.iter().enumerate() {
            if let Some(left_idx) = entry.left {
                let text = line_text(&data.left, &data.left_starts, left_idx as usize);
                for (col, len) in find_occurrences_in_line(text, &find.query, find.case_sensitive) {
                    find.matches.push((DiffSide::Left, row, col, len));
                    if find.matches.len() >= MAX_MATCHES {
                        break;
                    }
                }
            }
            if let Some(right_idx) = entry.right
                && find.matches.len() < MAX_MATCHES
            {
                let text = line_text(&data.right, &data.right_starts, right_idx as usize);
                for (col, len) in find_occurrences_in_line(text, &find.query, find.case_sensitive) {
                    find.matches.push((DiffSide::Right, row, col, len));
                    if find.matches.len() >= MAX_MATCHES {
                        break;
                    }
                }
            }
            if find.matches.len() >= MAX_MATCHES {
                break;
            }
        }
    }
    find.active = if find.matches.is_empty() {
        None
    } else {
        Some(find.active.unwrap_or(0).min(find.matches.len() - 1))
    };
}

/// Convert a display row index (which includes hunk headers) into either `None`
/// (if it's a hunk header) or `Some(content_row)`.
fn display_row_to_content(display_index: usize, hunk_starts: &[usize]) -> Option<usize> {
    let header_cursor = hunk_starts.partition_point(|&start| start < display_index);
    if hunk_starts.get(header_cursor) == Some(&display_index) {
        None
    } else {
        Some(display_index.saturating_sub(header_cursor))
    }
}

/// Compute `(slider_offset, slider_height)` inside the diff minimap track.
fn diff_minimap_slider_span(
    content_h: f32,
    total_rows: usize,
    line_h: f32,
    viewport_h: f32,
    scroll_y: f32,
) -> (f32, f32) {
    let total_h = (total_rows.max(1) as f32) * line_h;
    if total_h <= viewport_h + 0.5 || content_h <= 1.0 {
        return (0.0, content_h.max(1.0));
    }
    let vh = ((viewport_h / total_h) * content_h).clamp(18.0, content_h);
    let max_scroll = (total_h - viewport_h).max(1.0);
    let frac = (scroll_y / max_scroll).clamp(0.0, 1.0);
    let offset = frac * (content_h - vh);
    (offset, vh)
}

/// Map a pointer Y position on the diff minimap to the target vertical scroll offset.
fn diff_minimap_scroll_from_y(
    pointer_y: f32,
    mm_top: f32,
    content_h: f32,
    slider_h: f32,
    max_v_scroll: f32,
) -> f32 {
    if max_v_scroll <= 0.0 || content_h <= slider_h + 0.5 {
        return 0.0;
    }
    let t = ((pointer_y - mm_top - slider_h * 0.5) / (content_h - slider_h)).clamp(0.0, 1.0);
    t * max_v_scroll
}

fn diff_minimap_line_color(spans: &[Span], starts: &[usize], line: usize, theme: Theme) -> Color32 {
    let Some(&byte_start) = starts.get(line) else {
        return theme.text_faint.gamma_multiply(0.55);
    };
    let idx = spans.partition_point(|s| s.end <= byte_start);
    let kind = spans.get(idx).map(|s| s.kind);
    let base = match kind {
        Some(TokenKind::Comment) => theme.syn_comment,
        Some(TokenKind::Keyword) | Some(TokenKind::Macro) => theme.syn_keyword,
        Some(TokenKind::String) => theme.syn_string,
        Some(TokenKind::Function) => theme.syn_function,
        Some(TokenKind::Type) => theme.syn_type,
        _ => theme.text_faint,
    };
    base.gamma_multiply(0.62)
}

fn line_indent_and_len(line: &str) -> (u16, u16) {
    let mut indent = 0usize;
    for ch in line.chars() {
        if ch == ' ' {
            indent += 1;
        } else {
            break;
        }
    }
    let len = line.trim_end().chars().count().saturating_sub(indent);
    (
        indent.min(u16::MAX as usize) as u16,
        len.min(u16::MAX as usize) as u16,
    )
}

fn priority_kind(current: RowKind, next: RowKind) -> RowKind {
    match (current, next) {
        (RowKind::Equal, x) => x,
        (RowKind::Added, RowKind::Removed) | (RowKind::Removed, RowKind::Added) => {
            RowKind::Modified
        }
        (x, RowKind::Equal) => x,
        (RowKind::Modified, _) | (_, RowKind::Modified) => RowKind::Modified,
        (x, _) => x,
    }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

pub fn diff_ui(
    ui: &mut Ui,
    style: &UiStyle,
    editor_style: &EditorStyle,
    tab: &mut DiffTab,
) -> DiffAction {
    let theme = style.theme;
    let mut action = DiffAction::None;
    let width = ui.available_width();

    // Toolbar
    let (header, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
    ui.painter().rect_filled(header, 0.0, theme.panel_bg);
    ui.painter().line_segment(
        [
            Pos2::new(header.left(), header.bottom() - 0.5),
            Pos2::new(header.right(), header.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme.border),
    );
    let title_color = match tab.status {
        GitFileStatus::Modified => theme.git_modified,
        GitFileStatus::Added | GitFileStatus::Untracked => theme.git_added,
        GitFileStatus::Deleted | GitFileStatus::Conflicted => theme.git_deleted,
        GitFileStatus::Renamed => theme.accent,
    };
    let badge = tab.status.badge();
    ui.painter().text(
        Pos2::new(header.left() + 12.0, header.center().y),
        Align2::LEFT_CENTER,
        &tab.rel_path,
        style.small(),
        theme.text,
    );
    let badge_x = header.left() + 20.0 + measure_width(ui, style, &tab.rel_path, style.small());
    ui.painter().text(
        Pos2::new(badge_x, header.center().y),
        Align2::LEFT_CENTER,
        badge,
        style.tiny(),
        title_color,
    );

    let mut ax = header.right() - 5.0;
    let icon_action =
        |ui: &mut Ui, ax: &mut f32, icon: Icon, tip: &str, enabled: bool| -> bool {
            *ax -= 24.0;
            let rect =
                Rect::from_center_size(Pos2::new(*ax + 11.0, header.center().y), Vec2::splat(22.0));
            let resp = ui.interact(
                rect,
                ui.scope_id().with(("diff_action", tip)),
                Sense::click(),
            );
            if resp.hovered() && enabled {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(5), theme.hover);
            }
            icons::paint(
                ui.painter(),
                rect.shrink(4.0),
                icon,
                if !enabled {
                    theme.text_faint
                } else if resp.hovered() {
                    theme.text
                } else {
                    theme.text_dim
                },
                1.2,
            );
            enabled && resp.on_hover_text(tip).clicked()
        };
    if icon_action(
        ui,
        &mut ax,
        Icon::File,
        "在编辑器中打开当前行",
        tab.path.exists(),
    ) {
        if let Some(line) = tab.active_source_line() {
            action = DiffAction::OpenFileAtLine(tab.path.clone(), line);
        } else {
            action = DiffAction::OpenFile(tab.path.clone());
        }
    }
    if icon_action(ui, &mut ax, Icon::Refresh, "重新计算差异", true) {
        action = DiffAction::Refresh;
    }
    if icon_action(ui, &mut ax, Icon::Search, "在差异中查找 (Ctrl+F)", true) {
        if tab.find.open {
            tab.find.open = false;
        } else {
            tab.open_find();
        }
    }
    let change_count = tab.data.as_ref().map(FileDiff::change_count).unwrap_or(0);
    let has_changes = change_count > 0;
    if icon_action(
        ui,
        &mut ax,
        Icon::ArrowDown,
        "下一个更改 (F7 / Alt+F5)",
        has_changes,
    ) && let Some(data) = &tab.data
    {
        tab.current_change = (tab.current_change + 1) % data.changes.len();
        tab.pending_scroll_row = data.changes.get(tab.current_change).copied();
    }
    if icon_action(
        ui,
        &mut ax,
        Icon::ArrowUp,
        "上一个更改 (Shift+F7)",
        has_changes,
    ) && let Some(data) = &tab.data
    {
        tab.current_change = (tab.current_change + data.changes.len() - 1) % data.changes.len();
        tab.pending_scroll_row = data.changes.get(tab.current_change).copied();
    }
    if has_changes {
        let counter = format!("{}/{}", tab.current_change + 1, change_count);
        let counter_w = measure_width(ui, style, &counter, style.tiny());
        ui.painter().text(
            Pos2::new(ax - 6.0, header.center().y),
            Align2::RIGHT_CENTER,
            counter,
            style.tiny(),
            theme.text_faint,
        );
        ax -= counter_w + 16.0;
    }
    // View mode pill
    ax -= 4.0;
    let pill = Rect::from_center_size(
        Pos2::new(ax - 28.0, header.center().y),
        Vec2::new(56.0, 20.0),
    );
    let pill_resp = ui.interact(pill, ui.scope_id().with("diff_mode"), Sense::click());
    ui.painter().rect_filled(
        pill,
        CornerRadius::same(5),
        if pill_resp.hovered() {
            theme.hover
        } else {
            theme.input_bg
        },
    );
    ui.painter().text(
        pill.center(),
        Align2::CENTER_CENTER,
        if tab.inline { "内联" } else { "并排" },
        style.tiny(),
        theme.text_dim,
    );
    if pill_resp.on_hover_text("切换并排 / 内联差异").clicked() {
        tab.inline = !tab.inline;
        tab.selection = None;
    }
    ax -= 62.0;

    // Working-tree inline edit toggle pill (only for unstaged non-deleted files)
    if tab.can_edit_working_tree() && tab.data.is_some() {
        let edit_open = tab.inline_edit.is_some();
        let edit_label = if edit_open { "关闭编辑" } else { "编辑工作区" };
        let pill_w = measure_width(ui, style, edit_label, style.tiny()) + 16.0;
        let edit_pill = Rect::from_center_size(
            Pos2::new(ax - pill_w * 0.5, header.center().y),
            Vec2::new(pill_w, 20.0),
        );
        let edit_resp = ui.interact(edit_pill, ui.scope_id().with("diff_edit_btn"), Sense::click());
        ui.painter().rect_filled(
            edit_pill,
            CornerRadius::same(5),
            if edit_open {
                theme.accent_subtle
            } else if edit_resp.hovered() {
                theme.hover
            } else {
                theme.input_bg
            },
        );
        ui.painter().text(
            edit_pill.center(),
            Align2::CENTER_CENTER,
            edit_label,
            style.tiny(),
            if edit_open { theme.accent } else { theme.text_dim },
        );
        if edit_resp
            .on_hover_text("在差异视图中直接编辑工作区当前选中行或更改块 (Ctrl+S 保存)")
            .clicked()
        {
            if edit_open {
                tab.inline_edit = None;
            } else if let Some(data) = tab.data.as_ref() {
                let (s, e) = tab
                    .selection
                    .and_then(|sel| working_line_range_for_selection(data, sel, tab.inline))
                    .or_else(|| {
                        data.changes
                            .get(tab.current_change)
                            .and_then(|&row| hunk_index_for_row(data, row, false))
                            .and_then(|h| hunk_working_line_range(data, h))
                    })
                    .or_else(|| {
                        tab.active_source_line().map(|l| {
                            let l0 = l.saturating_sub(1);
                            (l0, l0)
                        })
                    })
                    .unwrap_or((0, 0));
                tab.open_inline_edit(s, e);
            }
        }
    }

    if tab.loading {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.spinner();
            ui.label(
                egui::RichText::new("正在计算差异…")
                    .font(style.small())
                    .color(theme.text_faint),
            );
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(80));
        return action;
    }
    if let Some(error) = &tab.error {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(error.as_str())
                    .font(style.small())
                    .color(theme.git_deleted),
            );
        });
        return action;
    }

    let mut data_holder = tab.data.take();
    let Some(data) = data_holder.as_mut() else {
        return action;
    };

    // In-diff Find Bar (Ctrl+F)
    if tab.find.open {
        diff_find_bar_ui(ui, style, tab, data, width);
    }

    let font = FontId::new(editor_style.font_size, editor_style.mono_family.clone());
    let line_h = editor_style.line_h.max(10.0);
    let char_w = ui
        .ctx()
        .fonts_mut(|fonts| fonts.glyph_width(&font, 'M').max(1.0));

    // Reserve minimap / overview ruler width cleanly on the right edge so code never
    // paints underneath the minimap and the left/right header divider aligns with the body.
    let strip_w = if editor_style.show_minimap && width > 280.0 {
        DIFF_MINIMAP_W
    } else {
        DIFF_RULER_W
    };
    let code_area_w = (width - strip_w).max(120.0);

    // Column labels bar
    let (labels, _) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::hover());
    ui.painter().rect_filled(labels, 0.0, theme.input_bg);
    if tab.inline {
        ui.painter().text(
            Pos2::new(labels.left() + 12.0, labels.center().y),
            Align2::LEFT_CENTER,
            format!("统一差异  {} → {}", data.left_label, data.right_label),
            style.tiny(),
            theme.text_faint,
        );
    } else {
        let half = code_area_w * 0.5;
        ui.painter().text(
            Pos2::new(labels.left() + 12.0, labels.center().y),
            Align2::LEFT_CENTER,
            &data.left_label,
            style.tiny(),
            theme.text_faint,
        );
        let right_heading = if tab.can_edit_working_tree() {
            format!("{}  (可编辑)", data.right_label)
        } else {
            data.right_label.clone()
        };
        ui.painter().text(
            Pos2::new(labels.left() + half + 12.0, labels.center().y),
            Align2::LEFT_CENTER,
            right_heading,
            style.tiny(),
            theme.text_faint,
        );
        ui.painter().line_segment(
            [
                Pos2::new(labels.left() + half, labels.top()),
                Pos2::new(labels.left() + half, labels.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );
    }
    if data.coarse {
        ui.painter().text(
            Pos2::new(labels.left() + code_area_w - 10.0, labels.center().y),
            Align2::RIGHT_CENTER,
            "差异过大，已粗略对齐",
            style.tiny(),
            theme.text_faint,
        );
    }
    ui.painter().line_segment(
        [
            Pos2::new(labels.left() + code_area_w, labels.top()),
            Pos2::new(labels.left() + code_area_w, labels.bottom()),
        ],
        Stroke::new(1.0, theme.border),
    );

    // Refresh find matches whenever open.
    if tab.find.open {
        refresh_diff_find(&mut tab.find, data, tab.inline, tab.data_rev);
    }

    // Reserve space at the bottom if inline working-tree editor panel is active.
    let total_avail = ui.available_rect_before_wrap();
    let edit_panel_h = if tab.inline_edit.is_some() {
        (total_avail.height() * 0.36).clamp(118.0, 260.0)
    } else {
        0.0
    };
    let body = Rect::from_min_max(
        total_avail.min,
        Pos2::new(
            total_avail.right(),
            (total_avail.bottom() - edit_panel_h).max(total_avail.top() + 48.0),
        ),
    );

    let max_digits = digit_count(data.rows.len().max(1));
    let gutter_w = (max_digits as f32 * char_w + 22.0).max(46.0);
    let pane_code_w = if tab.inline {
        (code_area_w - gutter_w - 30.0).max(40.0)
    } else {
        (code_area_w * 0.5 - gutter_w - 18.0).max(40.0)
    };
    let max_h_scroll = (data.max_chars as f32 * char_w - pane_code_w).max(0.0);
    let hbar_h = if max_h_scroll > 0.5 {
        H_SCROLLBAR_H
    } else {
        0.0
    };

    let code_rect = Rect::from_min_max(
        body.min,
        Pos2::new(body.left() + code_area_w, (body.bottom() - hbar_h).max(body.top() + 24.0)),
    );
    let hbar_rect = Rect::from_min_max(
        Pos2::new(body.left(), code_rect.bottom()),
        Pos2::new(body.left() + code_area_w, body.bottom()),
    );
    let minimap_rect = Rect::from_min_max(
        Pos2::new(body.left() + code_area_w, body.top()),
        body.max,
    );

    let inline = tab.inline;
    let content_rows = if inline {
        data.inline_rows.len()
    } else {
        data.rows.len()
    };
    // Each hunk header takes a row of its own.
    let hunk_starts: Vec<usize> = data
        .hunks
        .iter()
        .enumerate()
        .map(|(index, hunk)| hunk.start(inline) + index)
        .collect();
    let total_rows = content_rows + hunk_starts.len();

    // Handle keyboard shortcuts when the diff canvas is active (and no TextEdit owns keyboard).
    if !ui.ctx().egui_wants_keyboard_input() {
        let mut copy_req = false;
        let mut select_all_req = false;
        let mut find_req = false;
        let mut find_next = false;
        let mut find_prev = false;
        let mut next_change = false;
        let mut prev_change = false;
        let mut escape_req = false;
        let mut enter_edit_req = false;
        ui.ctx().input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::C) {
                copy_req = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::A) {
                select_all_req = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::F) {
                find_req = true;
            }
            if i.consume_key(Modifiers::SHIFT, Key::F3) {
                find_prev = true;
            } else if i.consume_key(Modifiers::NONE, Key::F3) {
                find_next = true;
            }
            if i.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::F5)
                || i.consume_key(Modifiers::SHIFT, Key::F7)
            {
                prev_change = true;
            } else if i.consume_key(Modifiers::ALT, Key::F5)
                || i.consume_key(Modifiers::NONE, Key::F7)
            {
                next_change = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                escape_req = true;
            }
            if tab.has_focus && i.consume_key(Modifiers::NONE, Key::Enter) {
                enter_edit_req = true;
            }
        });
        if copy_req
            && let Some(sel) = tab.selection
            && let Some(text) = extract_selection_text(data, sel, inline).or_else(|| {
                row_line_text(data, sel.side, sel.caret.row, inline).map(str::to_string)
            })
        {
            crate::app::copy_to_clipboard(text);
        }
        if select_all_req {
            let side = if inline {
                DiffSide::Inline
            } else {
                tab.selection
                    .map(|s| s.side)
                    .filter(|s| *s != DiffSide::Inline)
                    .unwrap_or(DiffSide::Right)
            };
            if content_rows > 0 {
                let last_row = content_rows - 1;
                let last_col = row_line_text(data, side, last_row, inline)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                tab.selection = Some(DiffSelection {
                    side,
                    anchor: DiffPos { row: 0, col: 0 },
                    caret: DiffPos {
                        row: last_row,
                        col: last_col,
                    },
                });
                tab.has_focus = true;
            }
        }
        if find_req {
            if let Some(sel) = tab.selection
                && !sel.is_empty()
                && sel.anchor.row == sel.caret.row
                && let Some(text) = extract_selection_text(data, sel, inline)
                && !text.is_empty()
                && !text.contains('\n')
            {
                tab.find.query = text;
            }
            tab.find.open = true;
            tab.find.focus_requested = true;
        }
        if find_next || find_prev {
            if !tab.find.open {
                tab.find.open = true;
            }
            refresh_diff_find(&mut tab.find, data, inline, tab.data_rev);
            let count = tab.find.matches.len();
            if count > 0 {
                let next = match tab.find.active {
                    Some(idx) if find_next => (idx + 1) % count,
                    Some(idx) => (idx + count - 1) % count,
                    None => 0,
                };
                tab.find.active = Some(next);
                let (side, row, start_col, len) = tab.find.matches[next];
                tab.selection = Some(DiffSelection {
                    side,
                    anchor: DiffPos {
                        row,
                        col: start_col,
                    },
                    caret: DiffPos {
                        row,
                        col: start_col + len,
                    },
                });
                tab.pending_scroll_row = Some(row);
            }
        }
        if (next_change || prev_change) && !data.changes.is_empty() {
            let n = data.changes.len();
            tab.current_change = if next_change {
                (tab.current_change + 1) % n
            } else {
                (tab.current_change + n - 1) % n
            };
            tab.pending_scroll_row = data.changes.get(tab.current_change).copied();
        }
        if escape_req {
            if tab.find.open {
                tab.find.open = false;
            } else {
                tab.selection = None;
            }
        }
        if enter_edit_req
            && tab.can_edit_working_tree()
            && let Some(sel) = tab.selection
            && let Some((s, e)) = working_line_range_for_selection(data, sel, inline)
        {
            tab.open_inline_edit(s, e);
        }
    }

    let mut scroll_offset_y = tab.last_scroll_y;
    let mut scroll_viewport_h = code_rect.height().max(line_h);
    let mut open_inline_edit_range: Option<(usize, usize)> = None;

    // Main virtualized diff scroll area strictly inside `code_rect`.
    ui.scope_builder(UiBuilder::new().max_rect(code_rect), |ui| {
        let mut scroll = ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt(("diff_scroll", data.rel_path.as_str(), data.staged));
        if let Some(y) = tab.pending_scroll_y.take() {
            scroll = scroll.vertical_scroll_offset(y.max(0.0));
        } else if let Some(row) = tab.pending_scroll_row.take() {
            let display_row = data.display_row(row, inline);
            scroll = scroll.vertical_scroll_offset((display_row as f32 - 2.0).max(0.0) * line_h);
        }
        let out = scroll.show(ui, |ui| {
            let canvas_w = ui.available_width().max(60.0);
            let total_h = (total_rows as f32 * line_h).max(code_rect.height());
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(canvas_w, total_h),
                Sense::click_and_drag(),
            );

            // Hit-test helper: map screen pointer to (side, content_row, char_col, source_line, hunk_index, in_gutter).
            let hit_at = |pos: Pos2| -> Option<(DiffHit, bool)> {
                if total_rows == 0 || content_rows == 0 {
                    return None;
                }
                let raw_disp = ((pos.y - rect.top()) / line_h).floor();
                let disp = raw_disp.clamp(0.0, (total_rows - 1) as f32) as usize;
                let content_row = display_row_to_content(disp, &hunk_starts)?;
                let (side, code_left, in_gutter) = if inline {
                    let g_end = rect.left() + gutter_w + 18.0;
                    (DiffSide::Inline, g_end, pos.x < rect.left() + gutter_w)
                } else {
                    let half = canvas_w * 0.5;
                    if pos.x < rect.left() + half {
                        (
                            DiffSide::Left,
                            rect.left() + gutter_w,
                            pos.x < rect.left() + gutter_w,
                        )
                    } else {
                        (
                            DiffSide::Right,
                            rect.left() + half + gutter_w,
                            pos.x < rect.left() + half + gutter_w,
                        )
                    }
                };
                let col = if let Some(text) = row_line_text(data, side, content_row, inline) {
                    let local_x = (pos.x - code_left + tab.h_scroll).max(0.0);
                    let job = LayoutJob::simple_singleline(
                        text.to_string(),
                        font.clone(),
                        theme.text,
                    );
                    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
                    galley
                        .cursor_from_pos(Vec2::new(local_x, line_h * 0.5))
                        .index
                        .0
                        .min(text.chars().count())
                } else {
                    0
                };
                let source_line = source_line_for_row(data, content_row, inline);
                let hunk_index = hunk_index_for_row(data, content_row, inline);
                Some((
                    DiffHit {
                        side,
                        row: content_row,
                        col,
                        source_line,
                        hunk_index,
                    },
                    in_gutter,
                ))
            };

            // Hover cursor: I-beam over code, pointing hand over gutter when Ctrl held.
            if resp.hovered()
                && let Some(pos) = ui.ctx().pointer_hover_pos()
                && let Some((_, in_gutter)) = hit_at(pos)
            {
                let ctrl = ui.input(|i| i.modifiers.command);
                if in_gutter && ctrl {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                } else if !in_gutter {
                    ui.ctx().set_cursor_icon(CursorIcon::Text);
                }
            }

            // Mouse selection interactions (single click, shift+click, drag, double/triple click, gutter Ctrl+click).
            let (primary_pressed, primary_down, shift, ctrl, secondary_pressed, pointer_pos) =
                ui.input(|i| {
                    (
                        i.pointer.primary_pressed(),
                        i.pointer.primary_down(),
                        i.modifiers.shift,
                        i.modifiers.command,
                        i.pointer.secondary_pressed(),
                        i.pointer.interact_pos(),
                    )
                });

            if resp.hovered() && secondary_pressed
                && let Some(pos) = pointer_pos
                && let Some((hit, _)) = hit_at(pos)
            {
                tab.context_hit = Some(hit);
                // If right-click is outside any active selection, place caret on the clicked row.
                let keep_sel = tab.selection.is_some_and(|sel| {
                    !sel.is_empty() && sel.side == hit.side && {
                        let (a, b) = sel.ordered();
                        let p = DiffPos {
                            row: hit.row,
                            col: hit.col,
                        };
                        p >= a && p <= b
                    }
                });
                if !keep_sel {
                    tab.selection = Some(DiffSelection {
                        side: hit.side,
                        anchor: DiffPos {
                            row: hit.row,
                            col: hit.col,
                        },
                        caret: DiffPos {
                            row: hit.row,
                            col: hit.col,
                        },
                    });
                }
                tab.has_focus = true;
            }

            if resp.triple_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some((hit, _)) = hit_at(pos)
            {
                let len = row_line_text(data, hit.side, hit.row, inline)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                tab.selection = Some(DiffSelection {
                    side: hit.side,
                    anchor: DiffPos {
                        row: hit.row,
                        col: 0,
                    },
                    caret: DiffPos {
                        row: hit.row,
                        col: len,
                    },
                });
                tab.selecting = false;
                tab.has_focus = true;
            } else if resp.double_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some((hit, in_gutter)) = hit_at(pos)
            {
                if in_gutter {
                    if let Some(line) = hit.source_line {
                        action = DiffAction::OpenFileAtLine(tab.path.clone(), line);
                    }
                } else if let Some(text) = row_line_text(data, hit.side, hit.row, inline) {
                    let (a, b) = word_bounds_at(text, hit.col);
                    tab.selection = Some(DiffSelection {
                        side: hit.side,
                        anchor: DiffPos {
                            row: hit.row,
                            col: a,
                        },
                        caret: DiffPos {
                            row: hit.row,
                            col: b,
                        },
                    });
                }
                tab.selecting = false;
                tab.has_focus = true;
            } else if resp.hovered() && primary_pressed
                && let Some(pos) = pointer_pos
                && let Some((hit, in_gutter)) = hit_at(pos)
            {
                tab.has_focus = true;
                if in_gutter && ctrl {
                    if let Some(line) = hit.source_line {
                        action = DiffAction::OpenFileAtLine(tab.path.clone(), line);
                    }
                } else if in_gutter {
                    let len = row_line_text(data, hit.side, hit.row, inline)
                        .map(|s| s.chars().count())
                        .unwrap_or(0);
                    tab.selection = Some(DiffSelection {
                        side: hit.side,
                        anchor: DiffPos {
                            row: hit.row,
                            col: 0,
                        },
                        caret: DiffPos {
                            row: hit.row,
                            col: len,
                        },
                    });
                    tab.selecting = false;
                } else if shift
                    && let Some(prev) = tab.selection
                    && prev.side == hit.side
                {
                    tab.selection = Some(DiffSelection {
                        side: hit.side,
                        anchor: prev.anchor,
                        caret: DiffPos {
                            row: hit.row,
                            col: hit.col,
                        },
                    });
                    tab.selecting = true;
                } else {
                    tab.selection = Some(DiffSelection {
                        side: hit.side,
                        anchor: DiffPos {
                            row: hit.row,
                            col: hit.col,
                        },
                        caret: DiffPos {
                            row: hit.row,
                            col: hit.col,
                        },
                    });
                    tab.selecting = true;
                }
            } else if tab.selecting && primary_down {
                if let Some(pos) = pointer_pos
                    && let Some(prev) = tab.selection
                {
                    // Clamp drag to the starting side so VSCode-style single-pane selection
                    // stays strictly within Left or Right even if the mouse drifts horizontally.
                    let clamped_x = if inline {
                        pos.x
                    } else {
                        let half = canvas_w * 0.5;
                        match prev.side {
                            DiffSide::Left => pos.x.min(rect.left() + half - 2.0),
                            DiffSide::Right => pos.x.max(rect.left() + half + 2.0),
                            DiffSide::Inline => pos.x,
                        }
                    };
                    let probe = Pos2::new(clamped_x, pos.y.clamp(rect.top(), rect.bottom() - 1.0));
                    if let Some((hit, _)) = hit_at(probe) {
                        tab.selection = Some(DiffSelection {
                            side: prev.side,
                            anchor: prev.anchor,
                            caret: DiffPos {
                                row: hit.row,
                                col: hit.col,
                            },
                        });
                    }
                    // Auto-scroll when dragging near top or bottom viewport edge.
                    if pos.y < code_rect.top() + 18.0 {
                        tab.pending_scroll_y = Some((tab.last_scroll_y - line_h * 1.5).max(0.0));
                        ui.ctx().request_repaint();
                    } else if pos.y > code_rect.bottom() - 18.0 {
                        tab.pending_scroll_y = Some(tab.last_scroll_y + line_h * 1.5);
                        ui.ctx().request_repaint();
                    }
                }
            } else if !primary_down {
                tab.selecting = false;
            }

            // VSCode-style right-click context menu.
            resp.context_menu(|ui| {
                let ctx_hit = tab.context_hit;
                let has_sel = tab.selection.is_some_and(|s| !s.is_empty());
                let copy_label = if has_sel { "复制选中内容" } else { "复制当前行" };
                let copy_text = tab.selection.and_then(|sel| {
                    extract_selection_text(data, sel, inline).or_else(|| {
                        row_line_text(data, sel.side, sel.caret.row, inline).map(str::to_string)
                    })
                });
                let menu_font = style.small();
                let sc_font = style.tiny();
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::Copy,
                    copy_label,
                    Some("Ctrl+C"),
                    copy_text.is_some(),
                ) && let Some(text) = copy_text
                {
                    crate::app::copy_to_clipboard(text);
                }
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::Check,
                    "全选当前侧",
                    Some("Ctrl+A"),
                    content_rows > 0,
                ) {
                    let side = if inline {
                        DiffSide::Inline
                    } else {
                        ctx_hit
                            .map(|h| h.side)
                            .or_else(|| tab.selection.map(|s| s.side))
                            .filter(|s| *s != DiffSide::Inline)
                            .unwrap_or(DiffSide::Right)
                    };
                    if content_rows > 0 {
                        let last_row = content_rows - 1;
                        let last_col = row_line_text(data, side, last_row, inline)
                            .map(|s| s.chars().count())
                            .unwrap_or(0);
                        tab.selection = Some(DiffSelection {
                            side,
                            anchor: DiffPos { row: 0, col: 0 },
                            caret: DiffPos {
                                row: last_row,
                                col: last_col,
                            },
                        });
                    }
                }
                widgets::context_menu_sep(ui, &theme);

                let target_line = ctx_hit
                    .and_then(|h| h.source_line)
                    .or_else(|| {
                        tab.selection
                            .and_then(|s| source_line_for_row(data, s.caret.row, inline))
                    });
                if tab.can_edit_working_tree() {
                    let edit_range = tab
                        .selection
                        .filter(|s| !s.is_empty())
                        .and_then(|s| working_line_range_for_selection(data, s, inline))
                        .or_else(|| {
                            target_line.map(|l| {
                                let l0 = l.saturating_sub(1);
                                (l0, l0)
                            })
                        });
                    let edit_label = if has_sel {
                        "编辑选中行 (工作区)"
                    } else {
                        "编辑此行 (工作区)"
                    };
                    if widgets::context_menu_item(
                        ui,
                        &theme,
                        menu_font.clone(),
                        sc_font.clone(),
                        Icon::FileCode,
                        edit_label,
                        Some("Enter"),
                        edit_range.is_some(),
                    ) {
                        open_inline_edit_range = edit_range;
                    }
                }
                let open_label = match target_line {
                    Some(line) => format!("在编辑器中打开第 {line} 行"),
                    None => "在编辑器中打开源文件".to_string(),
                };
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::File,
                    &open_label,
                    None,
                    tab.path.exists(),
                ) {
                    if let Some(line) = target_line {
                        action = DiffAction::OpenFileAtLine(tab.path.clone(), line);
                    } else {
                        action = DiffAction::OpenFile(tab.path.clone());
                    }
                }

                if let Some(hunk_idx) = ctx_hit.and_then(|h| h.hunk_index) {
                    widgets::context_menu_sep(ui, &theme);
                    if tab.staged {
                        if widgets::context_menu_item(
                            ui,
                            &theme,
                            menu_font.clone(),
                            sc_font.clone(),
                            Icon::Unstage,
                            "取消暂存此差异块",
                            None,
                            true,
                        ) {
                            tab.pending = Some(DiffCommand::Unstage { hunk: hunk_idx });
                        }
                    } else if widgets::context_menu_item(
                        ui,
                        &theme,
                        menu_font.clone(),
                        sc_font.clone(),
                        Icon::Stage,
                        "暂存此差异块",
                        None,
                        true,
                    ) {
                        tab.pending = Some(DiffCommand::Stage { hunk: hunk_idx });
                    }
                    if widgets::context_menu_item(
                        ui,
                        &theme,
                        menu_font.clone(),
                        sc_font.clone(),
                        Icon::Discard,
                        "丢弃此差异块更改",
                        None,
                        true,
                    ) {
                        tab.pending = Some(DiffCommand::Discard { hunk: hunk_idx });
                    }
                }

                widgets::context_menu_sep(ui, &theme);
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::Search,
                    "在差异中查找…",
                    Some("Ctrl+F"),
                    true,
                ) {
                    tab.find.open = true;
                    tab.find.focus_requested = true;
                }
                let mode_label = if tab.inline {
                    "切换到并排差异视图"
                } else {
                    "切换到内联差异视图"
                };
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::GitCompare,
                    mode_label,
                    None,
                    true,
                ) {
                    tab.inline = !tab.inline;
                    tab.selection = None;
                }
                widgets::context_menu_sep(ui, &theme);
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::Copy,
                    "复制相对路径",
                    None,
                    true,
                ) {
                    crate::app::copy_to_clipboard(data.rel_path.clone());
                }
                if widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font,
                    sc_font,
                    Icon::Copy,
                    "复制完整差异补丁",
                    None,
                    true,
                ) {
                    crate::app::copy_to_clipboard(unified_diff(data));
                }
            });

            // Horizontal scrolling inside the code view.
            if resp.hovered() {
                let (delta, shift_mod) =
                    ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
                if delta.x.abs() > 0.0 {
                    tab.h_scroll = (tab.h_scroll - delta.x).clamp(0.0, max_h_scroll);
                } else if shift_mod && delta.y.abs() > 0.0 {
                    tab.h_scroll = (tab.h_scroll - delta.y).clamp(0.0, max_h_scroll);
                }
            }
            tab.h_scroll = tab.h_scroll.clamp(0.0, max_h_scroll);

            let clip = ui.clip_rect().intersect(rect);
            let first = (((clip.top() - rect.top()) / line_h).floor().max(0.0)) as usize;
            let last = ((((clip.bottom() - rect.top()) / line_h).ceil()) as usize).min(total_rows);
            if first >= last {
                return;
            }
            let painter = ui.painter().with_clip_rect(clip);
            let line_number_color = theme.text_faint;
            let mut header_cursor = hunk_starts.partition_point(|&start| start < first);
            for display_index in first..last {
                let y = rect.top() + display_index as f32 * line_h;
                if header_cursor < hunk_starts.len() && hunk_starts[header_cursor] == display_index
                {
                    hunk_header_ui(
                        ui,
                        &painter,
                        data,
                        tab,
                        header_cursor,
                        rect.left(),
                        y,
                        canvas_w,
                        line_h,
                        style,
                        theme,
                        &mut action,
                        &mut open_inline_edit_range,
                    );
                    header_cursor += 1;
                    continue;
                }
                let row_index = display_index - header_cursor;
                if tab.inline {
                    let layout = InlineLayout { gutter_w };
                    paint_inline_row(
                        ui,
                        &painter,
                        data,
                        tab,
                        &layout,
                        row_index,
                        rect.left(),
                        canvas_w,
                        y,
                        line_h,
                        char_w,
                        &font,
                        style,
                        theme,
                    );
                } else {
                    let half = canvas_w * 0.5;
                    let left_rect =
                        Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(half, line_h));
                    let right_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + half, y),
                        Vec2::new(canvas_w - half, line_h),
                    );
                    paint_side_row(
                        ui,
                        &painter,
                        data,
                        tab,
                        left_rect,
                        gutter_w,
                        row_index,
                        true,
                        line_h,
                        char_w,
                        &font,
                        &line_number_color,
                        theme,
                    );
                    paint_side_row(
                        ui,
                        &painter,
                        data,
                        tab,
                        right_rect,
                        gutter_w,
                        row_index,
                        false,
                        line_h,
                        char_w,
                        &font,
                        &line_number_color,
                        theme,
                    );
                    painter.line_segment(
                        [
                            Pos2::new(rect.left() + half, y),
                            Pos2::new(rect.left() + half, y + line_h),
                        ],
                        Stroke::new(1.0, theme.border),
                    );
                }
            }
        });
        scroll_offset_y = out.state.clamped_offset().y;
        scroll_viewport_h = out.inner_rect.height().max(line_h);
    });

    tab.last_scroll_y = scroll_offset_y;

    if let Some((start_line, end_line)) = open_inline_edit_range {
        tab.open_inline_edit(start_line, end_line);
    }

    // Interactive horizontal scrollbar along bottom of code pane when lines overflow.
    if hbar_h > 0.0 && max_h_scroll > 0.5 {
        paint_diff_h_scrollbar(ui, hbar_rect, pane_code_w, max_h_scroll, &mut tab.h_scroll, theme);
    }

    // Diff Minimap (or clickable Overview Ruler when minimap is disabled in settings).
    paint_diff_minimap(
        ui,
        minimap_rect,
        data,
        tab,
        &hunk_starts,
        total_rows,
        line_h,
        scroll_offset_y,
        scroll_viewport_h,
        editor_style.show_minimap,
        theme,
    );

    // Docked In-Place Working-Tree Editor Panel (when active).
    if tab.inline_edit.is_some() {
        let edit_rect = Rect::from_min_max(
            Pos2::new(total_avail.left(), body.bottom()),
            total_avail.max,
        );
        if let Some(saved_line) = inline_edit_panel_ui(ui, edit_rect, style, editor_style, tab) {
            action = DiffAction::SavedWorkingFile {
                path: tab.path.clone(),
                line: saved_line,
            };
        }
    }

    tab.data = data_holder;
    action
}

/// Compact VSCode-style find bar docked below the diff toolbar.
fn diff_find_bar_ui(
    ui: &mut Ui,
    style: &UiStyle,
    tab: &mut DiffTab,
    data: &FileDiff,
    width: f32,
) {
    let theme = style.theme;
    let (bar, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, theme.panel_bg);
    ui.painter().line_segment(
        [
            Pos2::new(bar.left(), bar.bottom() - 0.5),
            Pos2::new(bar.right(), bar.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme.border),
    );

    let input_rect = Rect::from_min_size(
        Pos2::new(bar.left() + 10.0, bar.top() + 4.0),
        Vec2::new((width * 0.42).clamp(150.0, 320.0), 22.0),
    );
    ui.painter()
        .rect_filled(input_rect, CornerRadius::same(4), theme.input_bg);
    ui.painter().rect_stroke(
        input_rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme.border_strong),
        StrokeKind::Inside,
    );

    let mut step_dir: Option<bool> = None;
    let mut close_find = false;
    ui.scope_builder(UiBuilder::new().max_rect(input_rect.shrink2(Vec2::new(6.0, 2.0))), |ui| {
        let resp = ui.add(
            TextEdit::singleline(&mut tab.find.query)
                .frame(egui::Frame::NONE)
                .font(style.small())
                .text_color(theme.text)
                .hint_text("在差异中查找…")
                .desired_width(input_rect.width() - 12.0),
        );
        if tab.find.focus_requested {
            resp.request_focus();
            tab.find.focus_requested = false;
        }
        if resp.has_focus() {
            ui.ctx().input_mut(|i| {
                if i.consume_key(Modifiers::SHIFT, Key::Enter)
                    || i.consume_key(Modifiers::SHIFT, Key::F3)
                {
                    step_dir = Some(false);
                } else if i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::F3)
                {
                    step_dir = Some(true);
                } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                    close_find = true;
                }
            });
        }
    });

    refresh_diff_find(&mut tab.find, data, tab.inline, tab.data_rev);

    let mut x = input_rect.right() + 6.0;
    let case_rect = Rect::from_min_size(Pos2::new(x, bar.top() + 4.0), Vec2::splat(22.0));
    let case_resp = ui.interact(case_rect, ui.scope_id().with("diff_find_case"), Sense::click());
    if tab.find.case_sensitive || case_resp.hovered() {
        ui.painter().rect_filled(
            case_rect,
            CornerRadius::same(4),
            if tab.find.case_sensitive {
                theme.accent_subtle
            } else {
                theme.hover
            },
        );
    }
    icons::paint(
        ui.painter(),
        case_rect.shrink(4.0),
        Icon::CaseSensitive,
        if tab.find.case_sensitive {
            theme.accent
        } else {
            theme.text_dim
        },
        1.2,
    );
    if case_resp.on_hover_text("区分大小写").clicked() {
        tab.find.case_sensitive = !tab.find.case_sensitive;
        refresh_diff_find(&mut tab.find, data, tab.inline, tab.data_rev);
    }
    x += 28.0;

    let count = tab.find.matches.len();
    let status = if tab.find.query.is_empty() {
        String::new()
    } else if count == 0 {
        "无结果".to_string()
    } else {
        format!("{}/{}", tab.find.active.unwrap_or(0) + 1, count)
    };
    if !status.is_empty() {
        ui.painter().text(
            Pos2::new(x + 4.0, bar.center().y),
            Align2::LEFT_CENTER,
            &status,
            style.tiny(),
            if count == 0 {
                theme.git_deleted
            } else {
                theme.text_faint
            },
        );
        x += measure_width(ui, style, &status, style.tiny()) + 14.0;
    }

    let mut small_btn = |icon: Icon, tip: &str, enabled: bool| -> bool {
        let r = Rect::from_min_size(Pos2::new(x, bar.top() + 4.0), Vec2::splat(22.0));
        x += 24.0;
        let resp = ui.interact(r, ui.scope_id().with(("diff_find_btn", tip)), Sense::click());
        if enabled && resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(4), theme.hover);
        }
        icons::paint(
            ui.painter(),
            r.shrink(4.0),
            icon,
            if !enabled {
                theme.text_faint
            } else if resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
            1.2,
        );
        enabled && resp.on_hover_text(tip).clicked()
    };

    if small_btn(Icon::ArrowUp, "上一个匹配 (Shift+Enter)", count > 0) {
        step_dir = Some(false);
    }
    if small_btn(Icon::ArrowDown, "下一个匹配 (Enter)", count > 0) {
        step_dir = Some(true);
    }
    if small_btn(Icon::Close, "关闭查找 (Esc)", true) {
        close_find = true;
    }

    if let Some(forward) = step_dir
        && count > 0
    {
        let next = match tab.find.active {
            Some(idx) if forward => (idx + 1) % count,
            Some(idx) => (idx + count - 1) % count,
            None => 0,
        };
        tab.find.active = Some(next);
        let (side, row, start_col, len) = tab.find.matches[next];
        tab.selection = Some(DiffSelection {
            side,
            anchor: DiffPos {
                row,
                col: start_col,
            },
            caret: DiffPos {
                row,
                col: start_col + len,
            },
        });
        tab.pending_scroll_row = Some(row);
    }
    if close_find {
        tab.find.open = false;
    }
}

/// Interactive horizontal scrollbar rendered along the bottom of the diff code area.
fn paint_diff_h_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    pane_w: f32,
    max_h_scroll: f32,
    h_scroll: &mut f32,
    theme: Theme,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme.bg);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, theme.border),
    );
    let track = rect.shrink2(Vec2::new(4.0, 2.0));
    if track.width() <= 24.0 {
        return;
    }
    let total_w = pane_w + max_h_scroll;
    let thumb_w = ((pane_w / total_w) * track.width()).clamp(28.0, track.width());
    let frac = (*h_scroll / max_h_scroll).clamp(0.0, 1.0);
    let thumb_x = track.left() + frac * (track.width() - thumb_w);
    let thumb = Rect::from_min_size(
        Pos2::new(thumb_x, track.top()),
        Vec2::new(thumb_w, track.height()),
    );
    let resp = ui.interact(rect, ui.scope_id().with("diff_h_scroll"), Sense::click_and_drag());
    let color = if resp.dragged() || resp.hovered() {
        theme.text_dim.gamma_multiply(0.7)
    } else {
        theme.text_faint.gamma_multiply(0.45)
    };
    painter.rect_filled(thumb, CornerRadius::same(3), color);
    if (resp.clicked() || resp.dragged())
        && let Some(pos) = resp.interact_pointer_pos()
        && track.width() > thumb_w + 0.5
    {
        let t = ((pos.x - track.left() - thumb_w * 0.5) / (track.width() - thumb_w)).clamp(0.0, 1.0);
        *h_scroll = t * max_h_scroll;
    }
}

/// Diff Minimap with syntax-tinted miniature code bars, green/red change bands,
/// find match markers, active viewport slider, and click/drag smooth jumping.
#[allow(clippy::too_many_arguments)]
fn paint_diff_minimap(
    ui: &mut Ui,
    mm: Rect,
    data: &FileDiff,
    tab: &mut DiffTab,
    hunk_starts: &[usize],
    total_rows: usize,
    line_h: f32,
    scroll_offset_y: f32,
    viewport_h: f32,
    full_minimap: bool,
    theme: Theme,
) {
    if mm.width() <= 2.0 || mm.height() <= 4.0 {
        return;
    }
    let painter = ui.painter_at(mm);
    painter.rect_filled(mm, 0.0, theme.bg);
    painter.line_segment(
        [mm.left_top(), mm.left_bottom()],
        Stroke::new(1.0, theme.border),
    );

    let total_rows_nz = total_rows.max(1);
    let mm_rows = ((mm.height() / 2.2) as usize)
        .clamp(1, 420)
        .min(total_rows_nz);
    let step = (total_rows_nz as f32 / mm_rows as f32).max(1.0);
    let mm_row_h = (mm.height() / mm_rows as f32).clamp(1.6, 3.2);
    let content_h = (mm_rows as f32 * mm_row_h).min(mm.height()).max(1.0);

    // Rebuild downsampled minimap cache only when diff data, view mode, theme, or height changes.
    let cache_key = (
        tab.data_rev,
        tab.inline,
        theme.kind.as_u8(),
        mm_rows,
        mm.height().to_bits(),
    );
    if tab.minimap_cache.key != cache_key || tab.minimap_cache.buckets.len() != mm_rows {
        let mut buckets = Vec::with_capacity(mm_rows);
        for i in 0..mm_rows {
            let d0 = ((i as f32 * step).floor() as usize).min(total_rows_nz - 1);
            let d1 = ((((i + 1) as f32 * step).ceil()) as usize)
                .max(d0 + 1)
                .min(total_rows_nz);
            let mut left_kind = RowKind::Equal;
            let mut right_kind = RowKind::Equal;
            let mut inline_kind = RowKind::Equal;
            let mut is_hunk = false;
            let mut sample_content: Option<usize> = None;

            for disp in d0..d1 {
                if let Some(c_row) = display_row_to_content(disp, hunk_starts) {
                    if sample_content.is_none() {
                        sample_content = Some(c_row);
                    }
                    if tab.inline {
                        if let Some(e) = data.inline_rows.get(c_row) {
                            inline_kind = priority_kind(inline_kind, e.kind);
                        }
                    } else if let Some(r) = data.rows.get(c_row) {
                        match r.kind {
                            RowKind::Removed => {
                                left_kind = priority_kind(left_kind, RowKind::Removed);
                            }
                            RowKind::Added => {
                                right_kind = priority_kind(right_kind, RowKind::Added);
                            }
                            RowKind::Modified => {
                                left_kind = priority_kind(left_kind, RowKind::Removed);
                                right_kind = priority_kind(right_kind, RowKind::Added);
                            }
                            RowKind::Equal => {}
                        }
                    }
                } else {
                    is_hunk = true;
                }
            }

            let (
                mut left_indent,
                mut left_len,
                mut left_color,
                mut right_indent,
                mut right_len,
                mut right_color,
            ) = (
                0u16,
                0u16,
                theme.text_faint.gamma_multiply(0.5),
                0u16,
                0u16,
                theme.text_faint.gamma_multiply(0.5),
            );
            if let Some(c_row) = sample_content {
                if tab.inline {
                    if let Some(e) = data.inline_rows.get(c_row) {
                        let (text, starts, spans) = if e.left_side {
                            (&data.left, &data.left_starts, &data.left_spans)
                        } else {
                            (&data.right, &data.right_starts, &data.right_spans)
                        };
                        let l = line_text(text, starts, e.line as usize);
                        let (ind, len) = line_indent_and_len(l);
                        left_indent = ind;
                        left_len = len;
                        left_color =
                            diff_minimap_line_color(spans, starts, e.line as usize, theme);
                    }
                } else if let Some(r) = data.rows.get(c_row) {
                    if let Some(li) = r.left {
                        let l = line_text(&data.left, &data.left_starts, li as usize);
                        let (ind, len) = line_indent_and_len(l);
                        left_indent = ind;
                        left_len = len;
                        left_color = diff_minimap_line_color(
                            &data.left_spans,
                            &data.left_starts,
                            li as usize,
                            theme,
                        );
                    }
                    if let Some(ri) = r.right {
                        let l = line_text(&data.right, &data.right_starts, ri as usize);
                        let (ind, len) = line_indent_and_len(l);
                        right_indent = ind;
                        right_len = len;
                        right_color = diff_minimap_line_color(
                            &data.right_spans,
                            &data.right_starts,
                            ri as usize,
                            theme,
                        );
                    }
                }
            }

            buckets.push(DiffMinimapBucket {
                left_kind,
                right_kind,
                inline_kind,
                is_hunk,
                left_indent,
                left_len,
                left_color,
                right_indent,
                right_len,
                right_color,
            });
        }
        tab.minimap_cache.buckets = buckets;
        tab.minimap_cache.key = cache_key;
    }

    let inner_left = mm.left() + 2.0;
    let inner_w = (mm.width() - 4.0).max(4.0);
    let mid_x = inner_left + inner_w * 0.5;

    if full_minimap {
        // Subtle center split for side-by-side minimap.
        if !tab.inline {
            painter.line_segment(
                [Pos2::new(mid_x, mm.top()), Pos2::new(mid_x, mm.top() + content_h)],
                Stroke::new(1.0, theme.border.gamma_multiply(0.5)),
            );
        }

        for (i, b) in tab.minimap_cache.buckets.iter().enumerate() {
            let y = mm.top() + i as f32 * mm_row_h;
            let bar_y = y + mm_row_h * 0.16;
            let bar_h = (mm_row_h * 0.68).max(1.0);

            if b.is_hunk {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(inner_w, mm_row_h.max(1.5))),
                    0.0,
                    theme.diff_hunk_bg.gamma_multiply(0.85),
                );
            }

            if tab.inline {
                let band_color = match b.inline_kind {
                    RowKind::Added => Some((theme.diff_added_bg, theme.git_added)),
                    RowKind::Removed => Some((theme.diff_removed_bg, theme.git_deleted)),
                    RowKind::Modified => Some((theme.diff_added_bg, theme.git_modified)),
                    RowKind::Equal => None,
                };
                if let Some((bg, edge)) = band_color {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(inner_w, mm_row_h)),
                        0.0,
                        bg,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(2.5, mm_row_h)),
                        0.0,
                        edge,
                    );
                }
                if b.left_len > 0 {
                    let x = inner_left + 4.0 + (b.left_indent as f32 * 0.4).min(inner_w * 0.35);
                    let w = ((b.left_len as f32 * 0.42)
                        .min(inner_left + inner_w - x - 2.0))
                        .max(1.5);
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(x, bar_y), Vec2::new(w, bar_h)),
                        0.5,
                        b.left_color,
                    );
                }
            } else {
                let half_w = (inner_w * 0.5 - 1.0).max(2.0);
                // Left (old) half diff band + mini code bar
                if b.left_kind != RowKind::Equal {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(half_w, mm_row_h)),
                        0.0,
                        theme.diff_removed_bg,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(2.0, mm_row_h)),
                        0.0,
                        theme.git_deleted,
                    );
                }
                if b.left_len > 0 {
                    let x = inner_left + 3.0 + (b.left_indent as f32 * 0.25).min(half_w * 0.35);
                    let w = ((b.left_len as f32 * 0.25)
                        .min(inner_left + half_w - x - 1.0))
                        .max(1.2);
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(x, bar_y), Vec2::new(w, bar_h)),
                        0.5,
                        b.left_color,
                    );
                }
                // Right (new) half diff band + mini code bar
                let rx0 = mid_x + 1.0;
                if b.right_kind != RowKind::Equal {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(rx0, y), Vec2::new(half_w, mm_row_h)),
                        0.0,
                        theme.diff_added_bg,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(rx0 + half_w - 2.0, y),
                            Vec2::new(2.0, mm_row_h),
                        ),
                        0.0,
                        theme.git_added,
                    );
                }
                if b.right_len > 0 {
                    let x = rx0 + 1.5 + (b.right_indent as f32 * 0.25).min(half_w * 0.35);
                    let w = ((b.right_len as f32 * 0.25)
                        .min(rx0 + half_w - x - 3.0))
                        .max(1.2);
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(x, bar_y), Vec2::new(w, bar_h)),
                        0.5,
                        b.right_color,
                    );
                }
            }
        }
    } else {
        // Compact clickable overview ruler when minimap is disabled in settings.
        for (i, b) in tab.minimap_cache.buckets.iter().enumerate() {
            let y = mm.top() + i as f32 * mm_row_h;
            let color = if tab.inline {
                match b.inline_kind {
                    RowKind::Added => Some(theme.git_added),
                    RowKind::Removed => Some(theme.git_deleted),
                    RowKind::Modified => Some(theme.git_modified),
                    RowKind::Equal => None,
                }
            } else {
                match (b.left_kind, b.right_kind) {
                    (RowKind::Equal, RowKind::Equal) => None,
                    (RowKind::Equal, _) => Some(theme.git_added),
                    (_, RowKind::Equal) => Some(theme.git_deleted),
                    _ => Some(theme.git_modified),
                }
            };
            if let Some(c) = color {
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(inner_left, y),
                        Vec2::new(inner_w, mm_row_h.max(2.5)),
                    ),
                    1.0,
                    c.gamma_multiply(0.85),
                );
            }
        }
    }

    // Active cursor/selection line marker on minimap.
    if let Some(sel) = tab.selection {
        let disp = data.display_row(sel.caret.row, tab.inline);
        let cy = mm.top() + (disp as f32 / total_rows_nz as f32) * content_h;
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(mm.left() + 1.0, cy),
                Vec2::new(mm.width() - 2.0, mm_row_h.max(2.0)),
            ),
            0.0,
            theme.accent_subtle,
        );
    }

    // Find match tick marks on minimap.
    if tab.find.open && !tab.find.matches.is_empty() {
        for &(_, row, _, _) in tab.find.matches.iter().take(240) {
            let disp = data.display_row(row, tab.inline);
            let my = mm.top() + (disp as f32 / total_rows_nz as f32) * content_h;
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(mm.right() - 6.0, my), Vec2::new(5.0, 2.5)),
                1.0,
                theme.find_active,
            );
        }
    }

    // Translucent viewport slider indicator.
    let (slider_offset, vh) =
        diff_minimap_slider_span(content_h, total_rows_nz, line_h, viewport_h, scroll_offset_y);
    let vp_rect = Rect::from_min_size(
        Pos2::new(mm.left() + 1.0, mm.top() + slider_offset),
        Vec2::new(mm.width() - 2.0, vh),
    );
    let id = ui
        .scope_id()
        .with(("diff_minimap", data.rel_path.as_str(), data.staged));
    let resp = ui.interact(mm, id, Sense::click_and_drag());
    let vp_fill = if resp.hovered() || resp.dragged() {
        theme.minimap_viewport.gamma_multiply(1.35)
    } else {
        theme.minimap_viewport
    };
    painter.rect_filled(vp_rect, 2.0, vp_fill);
    painter.rect_stroke(
        vp_rect,
        2.0,
        Stroke::new(1.0, theme.border_strong),
        StrokeKind::Inside,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    // Click or drag to jump/scrub smoothly to that location in the diff.
    if (resp.clicked() || resp.dragged())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let total_content_h = total_rows_nz as f32 * line_h;
        let max_v_scroll = (total_content_h - viewport_h).max(0.0);
        let target_y =
            diff_minimap_scroll_from_y(pos.y, mm.top(), content_h, vh, max_v_scroll);
        tab.pending_scroll_y = Some(target_y);
        ui.ctx().request_repaint();
    }
}

/// Docked working-tree editor panel allowing VSCode-like edits right inside the diff view.
/// Returns `Some(saved_1based_line)` when the user saves changes to disk.
fn inline_edit_panel_ui(
    ui: &mut Ui,
    rect: Rect,
    style: &UiStyle,
    editor_style: &EditorStyle,
    tab: &mut DiffTab,
) -> Option<usize> {
    let theme = style.theme;
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme.panel_bg);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.5, theme.accent),
    );

    let bar_h = 28.0;
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), bar_h));
    painter.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, theme.border),
    );

    let (start_1, end_1, dirty, err_msg) = {
        let edit = tab.inline_edit.as_ref()?;
        (
            edit.start_line + 1,
            edit.end_line + 1,
            edit.is_dirty(),
            edit.error.clone(),
        )
    };
    let range_text = if start_1 == end_1 {
        format!("编辑工作区  {} · 第 {start_1} 行", tab.rel_path)
    } else {
        format!("编辑工作区  {} · 第 {start_1}–{end_1} 行", tab.rel_path)
    };
    painter.text(
        Pos2::new(bar.left() + 12.0, bar.center().y),
        Align2::LEFT_CENTER,
        if dirty {
            format!("● {range_text}")
        } else {
            range_text
        },
        style.tiny(),
        if dirty { theme.accent } else { theme.text_dim },
    );

    let mut save_clicked = false;
    let mut cancel_clicked = false;
    let mut right = bar.right() - 8.0;
    if hunk_button(
        ui,
        painter,
        &mut right,
        bar,
        style,
        theme,
        ("inline_edit_cancel", tab.rel_path.as_str(), tab.staged, 0),
        "取消 (Esc)",
        theme.text_dim,
    ) {
        cancel_clicked = true;
    }
    if hunk_button(
        ui,
        painter,
        &mut right,
        bar,
        style,
        theme,
        ("inline_edit_save", tab.rel_path.as_str(), tab.staged, 1),
        "保存并刷新 (Ctrl+S)",
        if dirty { theme.git_added } else { theme.text_dim },
    ) {
        save_clicked = true;
    }

    let err_h = if err_msg.is_some() { 20.0 } else { 0.0 };
    if let Some(err) = err_msg {
        painter.text(
            Pos2::new(rect.left() + 12.0, bar.bottom() + 10.0),
            Align2::LEFT_CENTER,
            err,
            style.tiny(),
            theme.git_deleted,
        );
    }

    let editor_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, bar.bottom() + err_h + 6.0),
        Pos2::new(rect.right() - 8.0, rect.bottom() - 8.0),
    );
    painter.rect_filled(editor_rect, CornerRadius::same(4), theme.bg);
    painter.rect_stroke(
        editor_rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme.border_strong),
        StrokeKind::Inside,
    );

    if let Some(edit) = tab.inline_edit.as_mut() {
        let mono = FontId::new(editor_style.font_size, editor_style.mono_family.clone());
        ui.scope_builder(UiBuilder::new().max_rect(editor_rect.shrink(4.0)), |ui| {
            ScrollArea::both()
                .auto_shrink([false, false])
                .id_salt(("diff_inline_edit_scroll", tab.rel_path.as_str()))
                .show(ui, |ui| {
                    let resp = ui.add(
                        TextEdit::multiline(&mut edit.buffer)
                            .frame(egui::Frame::NONE)
                            .code_editor()
                            .font(mono)
                            .text_color(theme.text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(4),
                    );
                    if edit.focus_requested {
                        resp.request_focus();
                        edit.focus_requested = false;
                    }
                    if resp.has_focus() {
                        ui.ctx().input_mut(|i| {
                            if i.consume_key(Modifiers::COMMAND, Key::S) {
                                save_clicked = true;
                            } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                                cancel_clicked = true;
                            }
                        });
                    }
                });
        });
    }

    if cancel_clicked {
        tab.inline_edit = None;
        return None;
    }
    if save_clicked && let Some(Ok(saved_line)) = tab.save_inline_edit() {
        return Some(saved_line);
    }
    None
}

/// Hunk header row: `@@ … @@` summary plus the per-hunk Git & edit buttons.
#[allow(clippy::too_many_arguments)]
fn hunk_header_ui(
    ui: &Ui,
    painter: &egui::Painter,
    data: &FileDiff,
    tab: &mut DiffTab,
    index: usize,
    x: f32,
    y: f32,
    width: f32,
    line_h: f32,
    style: &UiStyle,
    theme: Theme,
    action: &mut DiffAction,
    open_edit_range: &mut Option<(usize, usize)>,
) {
    let Some(hunk) = data.hunks.get(index) else {
        return;
    };
    let line = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, line_h));
    painter.rect_filled(line, 0.0, theme.diff_hunk_bg);
    painter.line_segment(
        [line.left_top(), line.right_top()],
        Stroke::new(1.0, theme.border),
    );
    painter.text(
        Pos2::new(line.left() + 14.0, line.center().y),
        Align2::LEFT_CENTER,
        format!("{}  +{} -{}", hunk.header, hunk.added, hunk.removed),
        style.tiny(),
        theme.text_faint,
    );

    // Right-to-left: discard, stage/unstage, jump-to-source, edit hunk (working tree).
    let mut right = line.right() - 6.0;
    if hunk_button(
        ui,
        painter,
        &mut right,
        line,
        style,
        theme,
        ("hunk_discard", data.rel_path.as_str(), data.staged, index),
        "丢弃",
        theme.git_deleted,
    ) {
        tab.pending = Some(DiffCommand::Discard { hunk: index });
    }
    let (label, color, command) = if tab.staged {
        (
            "取消暂存",
            theme.accent,
            DiffCommand::Unstage { hunk: index },
        )
    } else {
        (
            "暂存此块",
            theme.git_added,
            DiffCommand::Stage { hunk: index },
        )
    };
    if hunk_button(
        ui,
        painter,
        &mut right,
        line,
        style,
        theme,
        ("hunk_apply", data.rel_path.as_str(), data.staged, index),
        label,
        color,
    ) {
        tab.pending = Some(command);
    }
    if tab.path.exists()
        && hunk_button(
            ui,
            painter,
            &mut right,
            line,
            style,
            theme,
            ("hunk_goto", data.rel_path.as_str(), data.staged, index),
            "转到",
            theme.text_dim,
        )
    {
        let target_line = hunk_working_line_range(data, index)
            .map(|(s, _)| s + 1)
            .unwrap_or(1);
        *action = DiffAction::OpenFileAtLine(tab.path.clone(), target_line);
    }
    if tab.can_edit_working_tree()
        && hunk_button(
            ui,
            painter,
            &mut right,
            line,
            style,
            theme,
            ("hunk_edit", data.rel_path.as_str(), data.staged, index),
            "编辑",
            theme.accent,
        )
    {
        *open_edit_range = hunk_working_line_range(data, index);
    }
}

/// Small text button laid out right-to-left inside a hunk header row.
#[allow(clippy::too_many_arguments)]
fn hunk_button(
    ui: &Ui,
    painter: &egui::Painter,
    right: &mut f32,
    line: Rect,
    style: &UiStyle,
    theme: Theme,
    salt: (&str, &str, bool, usize),
    label: &str,
    color: Color32,
) -> bool {
    let width = measure_width(ui, style, label, style.tiny()) + 14.0;
    let rect = Rect::from_min_size(
        Pos2::new(*right - width, line.top() + 2.0),
        Vec2::new(width, line.height() - 4.0),
    );
    *right -= width + 6.0;
    let response = ui.interact(rect, ui.scope_id().with(salt), Sense::click());
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme.hover);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        style.tiny(),
        if response.hovered() {
            theme.text
        } else {
            color
        },
    );
    response.clicked()
}

struct InlineLayout {
    gutter_w: f32,
}

fn digit_count(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn measure_width(ui: &Ui, style: &UiStyle, text: &str, font: FontId) -> f32 {
    ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_job(LayoutJob::simple_singleline(
                text.to_string(),
                font,
                style.theme.text,
            ))
            .rect
            .width()
    })
}

fn line_job(
    text: &str,
    spans: &[Span],
    line_start: usize,
    font: &FontId,
    default_color: Color32,
    theme: Theme,
    line_height: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let line_end = line_start + text.len();
    let mut cursor = 0usize;
    for span in spans {
        if span.end <= line_start || span.start >= line_end {
            continue;
        }
        let start = span.start.max(line_start) - line_start;
        let end = span.end.min(line_end) - line_start;
        if start >= text.len() || end > text.len() || start >= end {
            continue;
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if start > cursor {
            job.append(
                &text[cursor..start],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: default_color,
                    line_height: Some(line_height),
                    ..Default::default()
                },
            );
        }
        if start >= cursor {
            job.append(
                &text[start..end],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: theme.token_color(span.kind),
                    line_height: Some(line_height),
                    ..Default::default()
                },
            );
            cursor = end;
        }
    }
    if cursor < text.len() {
        job.append(
            &text[cursor..],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: default_color,
                line_height: Some(line_height),
                ..Default::default()
            },
        );
    }
    job
}

fn row_colors(kind: RowKind, theme: Theme) -> (Option<Color32>, Option<Color32>) {
    match kind {
        RowKind::Equal => (None, None),
        RowKind::Added => (None, Some(theme.diff_added_bg)),
        RowKind::Removed => (Some(theme.diff_removed_bg), None),
        // Standard red-removed (left) / green-added (right) cue for modified lines.
        RowKind::Modified => (Some(theme.diff_removed_bg), Some(theme.diff_added_bg)),
    }
}

/// Paint selection highlight, find matches, and active caret on a diff code row.
#[allow(clippy::too_many_arguments)]
fn paint_row_overlays(
    cp: &egui::Painter,
    galley: &egui::Galley,
    tab: &DiffTab,
    side: DiffSide,
    row_index: usize,
    line_char_len: usize,
    code_left: f32,
    code_right: f32,
    y: f32,
    line_h: f32,
    theme: Theme,
) {
    // Find match highlights on this row & side.
    if tab.find.open && !tab.find.matches.is_empty() {
        for (idx, &(m_side, m_row, m_col, m_len)) in tab.find.matches.iter().enumerate() {
            if m_side != side || m_row != row_index {
                continue;
            }
            let x0 = galley
                .pos_from_cursor(egui::text::CCursor::new(m_col.min(line_char_len)))
                .left();
            let x1 = galley
                .pos_from_cursor(egui::text::CCursor::new(
                    (m_col + m_len).min(line_char_len),
                ))
                .left();
            let color = if tab.find.active == Some(idx) {
                theme.find_active
            } else {
                theme.find_match
            };
            cp.rect_filled(
                Rect::from_min_size(
                    Pos2::new(code_left - tab.h_scroll + x0, y),
                    Vec2::new((x1 - x0).max(2.0), line_h),
                ),
                2.0,
                color,
            );
        }
    }

    // Selection highlight & caret on this row & side.
    if let Some(sel) = tab.selection
        && sel.side == side
    {
        let (start, end) = sel.ordered();
        if !sel.is_empty() && row_index >= start.row && row_index <= end.row {
            let a = if row_index == start.row {
                start.col.min(line_char_len)
            } else {
                0
            };
            let spans_newline = row_index < end.row;
            let b = if spans_newline {
                line_char_len
            } else {
                end.col.min(line_char_len)
            };
            let x0 = code_left - tab.h_scroll
                + galley
                    .pos_from_cursor(egui::text::CCursor::new(a))
                    .left();
            let x1 = if spans_newline {
                (code_left - tab.h_scroll + galley.rect.width() + 8.0).min(code_right)
            } else {
                code_left - tab.h_scroll
                    + galley
                        .pos_from_cursor(egui::text::CCursor::new(b))
                        .left()
            };
            let sel_color = if tab.has_focus {
                theme.selection
            } else {
                theme.selection_inactive
            };
            if x1 > x0 || spans_newline {
                cp.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, y),
                        Pos2::new(x1.max(x0 + 4.0), y + line_h),
                    ),
                    2.0,
                    sel_color,
                );
            }
        }
        if tab.has_focus && sel.caret.row == row_index {
            let cx = code_left - tab.h_scroll
                + galley
                    .pos_from_cursor(egui::text::CCursor::new(
                        sel.caret.col.min(line_char_len),
                    ))
                    .left();
            cp.rect_filled(
                Rect::from_min_size(Pos2::new(cx, y + 1.0), Vec2::new(1.5, line_h - 2.0)),
                CornerRadius::same(1),
                theme.accent,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_side_row(
    ui: &Ui,
    painter: &egui::Painter,
    data: &FileDiff,
    tab: &DiffTab,
    rect: Rect,
    gutter_w: f32,
    row_index: usize,
    left_side: bool,
    line_h: f32,
    _char_w: f32,
    font: &FontId,
    number_color: &Color32,
    theme: Theme,
) {
    let Some(row) = data.rows.get(row_index) else {
        return;
    };
    let side = if left_side {
        DiffSide::Left
    } else {
        DiffSide::Right
    };
    let (left_tint, right_tint) = row_colors(row.kind, theme);
    let tint = if left_side { left_tint } else { right_tint };
    let (line_index, hl, text, starts, spans) = if left_side {
        (
            row.left,
            row.left_hl,
            data.left.as_str(),
            data.left_starts.as_slice(),
            data.left_spans.as_slice(),
        )
    } else {
        (
            row.right,
            row.right_hl,
            data.right.as_str(),
            data.right_starts.as_slice(),
            data.right_spans.as_slice(),
        )
    };
    if let Some(tint) = tint {
        painter.rect_filled(rect, 0.0, tint);
    }
    if tab
        .selection
        .is_some_and(|s| s.side == side && s.caret.row == row_index)
    {
        painter.rect_filled(rect, 0.0, theme.current_line.gamma_multiply(0.55));
    }
    let Some(line_index) = line_index else {
        painter.rect_filled(rect, 0.0, theme.diff_filler_bg);
        return;
    };
    // Highlight lines currently open in the inline working-tree editor.
    if !left_side
        && let Some(edit) = &tab.inline_edit
        && (line_index as usize) >= edit.start_line
        && (line_index as usize) <= edit.end_line
    {
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, line_h)),
            0.0,
            theme.accent,
        );
    }
    let number = format!("{}", line_index + 1);
    painter.text(
        Pos2::new(rect.left() + gutter_w - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        &number,
        FontId::new(font.size - 1.0, font.family.clone()),
        *number_color,
    );
    let text_line = line_text(text, starts, line_index as usize);
    let line_start = starts.get(line_index as usize).copied().unwrap_or(0);
    let job = line_job(text_line, spans, line_start, font, theme.text, theme, line_h);
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    let code_left = rect.left() + gutter_w;
    let code_clip = Rect::from_min_max(
        Pos2::new(code_left, rect.top()),
        Pos2::new(rect.right(), rect.bottom()),
    );
    let cp = painter.with_clip_rect(code_clip);
    if let Some((start, len)) = hl {
        let x0 = galley
            .pos_from_cursor(egui::text::CCursor::new(start as usize))
            .left();
        let x1 = galley
            .pos_from_cursor(egui::text::CCursor::new((start + len) as usize))
            .left();
        let x = code_left - tab.h_scroll + x0;
        let width = (x1 - x0).max(2.0);
        let color = if left_side {
            Color32::from_rgba_unmultiplied(
                theme.git_deleted.r(),
                theme.git_deleted.g(),
                theme.git_deleted.b(),
                64,
            )
        } else {
            Color32::from_rgba_unmultiplied(
                theme.git_added.r(),
                theme.git_added.g(),
                theme.git_added.b(),
                64,
            )
        };
        cp.rect_filled(
            Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(width, line_h)),
            2.0,
            color,
        );
    }
    paint_row_overlays(
        &cp,
        &galley,
        tab,
        side,
        row_index,
        text_line.chars().count(),
        code_left,
        rect.right(),
        rect.top(),
        line_h,
        theme,
    );
    cp.galley(
        Pos2::new(code_left - tab.h_scroll, rect.top()),
        galley,
        theme.text,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_inline_row(
    ui: &Ui,
    painter: &egui::Painter,
    data: &FileDiff,
    tab: &DiffTab,
    layout: &InlineLayout,
    row_index: usize,
    left_x: f32,
    row_width: f32,
    y: f32,
    line_h: f32,
    char_w: f32,
    font: &FontId,
    style: &UiStyle,
    theme: Theme,
) {
    let Some(entry) = data.inline_rows.get(row_index) else {
        return;
    };
    let rect = Rect::from_min_size(Pos2::new(left_x, y), Vec2::new(row_width, line_h));
    let (tint, marker) = match entry.kind {
        RowKind::Added => (
            Some(theme.git_added.gamma_multiply(0.16)),
            ("+", theme.git_added),
        ),
        RowKind::Removed => (
            Some(theme.git_deleted.gamma_multiply(0.16)),
            ("-", theme.git_deleted),
        ),
        _ => (None, ("", theme.text_faint)),
    };
    if let Some(tint) = tint {
        painter.rect_filled(rect, 0.0, tint);
    }
    if tab
        .selection
        .is_some_and(|s| s.side == DiffSide::Inline && s.caret.row == row_index)
    {
        painter.rect_filled(rect, 0.0, theme.current_line.gamma_multiply(0.55));
    }
    if !entry.left_side
        && let Some(edit) = &tab.inline_edit
        && (entry.line as usize) >= edit.start_line
        && (entry.line as usize) <= edit.end_line
    {
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, line_h)),
            0.0,
            theme.accent,
        );
    }
    if !marker.0.is_empty() {
        painter.text(
            Pos2::new(rect.left() + layout.gutter_w + 6.0, y + line_h * 0.5),
            Align2::LEFT_CENTER,
            marker.0,
            style.tiny(),
            marker.1,
        );
    }
    painter.text(
        Pos2::new(rect.left() + layout.gutter_w - 6.0, y + line_h * 0.5),
        Align2::RIGHT_CENTER,
        format!("{}", entry.line + 1),
        FontId::new(font.size - 1.0, font.family.clone()),
        theme.text_faint,
    );
    let (text, starts, spans) = if entry.left_side {
        (
            data.left.as_str(),
            data.left_starts.as_slice(),
            data.left_spans.as_slice(),
        )
    } else {
        (
            data.right.as_str(),
            data.right_starts.as_slice(),
            data.right_spans.as_slice(),
        )
    };
    let text_line = line_text(text, starts, entry.line as usize);
    let line_start = starts.get(entry.line as usize).copied().unwrap_or(0);
    let code_left = rect.left() + layout.gutter_w + 18.0;
    let job = line_job(text_line, spans, line_start, font, theme.text, theme, line_h);
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    let code_clip = Rect::from_min_max(
        Pos2::new(code_left, y),
        Pos2::new(rect.right(), y + line_h),
    );
    let cp = painter.with_clip_rect(code_clip);
    if let Some((start, len)) = entry.hl {
        let x0 = galley
            .pos_from_cursor(egui::text::CCursor::new(start as usize))
            .left();
        let x1 = galley
            .pos_from_cursor(egui::text::CCursor::new((start + len) as usize))
            .left();
        let x = code_left - tab.h_scroll + x0;
        let color = if entry.left_side {
            Color32::from_rgba_unmultiplied(
                theme.git_deleted.r(),
                theme.git_deleted.g(),
                theme.git_deleted.b(),
                64,
            )
        } else {
            Color32::from_rgba_unmultiplied(
                theme.git_added.r(),
                theme.git_added.g(),
                theme.git_added.b(),
                64,
            )
        };
        cp.rect_filled(
            Rect::from_min_size(Pos2::new(x, y), Vec2::new((x1 - x0).max(2.0), line_h)),
            2.0,
            color,
        );
    }
    paint_row_overlays(
        &cp,
        &galley,
        tab,
        DiffSide::Inline,
        row_index,
        text_line.chars().count(),
        code_left,
        rect.right(),
        y,
        line_h,
        theme,
    );
    cp.galley(Pos2::new(code_left - tab.h_scroll, y), galley, theme.text);
    let _ = char_w;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<&str> {
        if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').collect()
        }
    }

    fn rows(left: &str, right: &str) -> Vec<DiffRow> {
        compute_rows(&split(left), &split(right)).0
    }

    #[test]
    fn identical_text_is_all_equal() {
        let rows = rows("a\nb\nc", "a\nb\nc");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.kind == RowKind::Equal));
    }

    #[test]
    fn replacement_is_paired_as_modified() {
        let rows = rows("a\nb\nc", "a\nB\nc");
        assert_eq!(rows[1].kind, RowKind::Modified);
        assert_eq!(rows[1].left, Some(1));
        assert_eq!(rows[1].right, Some(1));
        assert!(rows[1].left_hl.is_some());
        assert!(rows[1].right_hl.is_some());
    }

    #[test]
    fn insert_delete_and_word_ranges() {
        let modified = rows("let x = 1;", "let x = 2;");
        assert_eq!(modified[0].kind, RowKind::Modified);
        assert_eq!(modified[0].left_hl, Some((8, 1)));
        assert_eq!(modified[0].right_hl, Some((8, 1)));

        let added = rows("a\nc", "a\nb\nc");
        assert_eq!(added[1].kind, RowKind::Added);
        assert_eq!(added[1].left, None);
        assert_eq!(added[1].right, Some(1));
    }

    #[test]
    fn inline_rows_expand_changes() {
        let (rows, changes, _) = compute_rows(&["a", "b"], &["a", "B"]);
        let inline = build_inline_rows(&rows);
        assert_eq!(inline.len(), 3);
        assert_eq!(inline[1].kind, RowKind::Removed);
        assert!(inline[1].left_side);
        assert_eq!(inline[2].kind, RowKind::Added);
        assert!(!inline[2].left_side);
        assert_eq!(changes, vec![1]);
    }

    #[test]
    fn large_edit_falls_back_to_coarse_alignment() {
        let mut left = String::new();
        let mut right = String::new();
        for index in 0..4000 {
            left.push_str(&format!("old line {index}\n"));
            right.push_str(&format!("new line {index}\n"));
        }
        let left_lines: Vec<&str> = split(left.trim_end());
        let right_lines: Vec<&str> = split(right.trim_end());
        let (rows, _, coarse) = compute_rows(&left_lines, &right_lines);
        assert!(coarse);
        assert!(rows.iter().any(|row| row.kind == RowKind::Modified));
    }

    #[test]
    fn word_diff_trims_common_edges() {
        let (left, right) = word_diff("abcXYZdef", "abc123def");
        assert_eq!(left, Some((3, 3)));
        assert_eq!(right, Some((3, 3)));
    }

    #[test]
    fn line_helpers_match_split_lines() {
        let text = "a\nb\n";
        assert_eq!(line_starts(text), vec![0, 2]);
        assert_eq!(line_text(text, &line_starts(text), 0), "a");
        assert_eq!(line_text(text, &line_starts(text), 1), "b");
        assert_eq!(split_lines(text), vec!["a", "b"]);
    }

    #[test]
    fn patch_hunks_are_placed_on_their_rows() {
        let (rows, _, _) = compute_rows(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"], &["a", "b", "C", "d", "e", "f", "g", "h", "I", "j"]);
        let inline_rows = build_inline_rows(&rows);
        let patch = "--- a/x\n+++ b/x\n@@ -2,3 +2,3 @@\n b\n-c\n+C\n d\n@@ -8,3 +8,3 @@\n h\n-i\n+I\n j\n";
        let (header, raw) = git::split_hunks(patch);
        assert_eq!(header, "--- a/x\n+++ b/x\n");
        let hunks = place_hunks(&raw, &rows, &inline_rows);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].header, "@@ -2,3 +2,3 @@");
        assert_eq!((hunks[0].added, hunks[0].removed), (1, 1));
        assert_eq!((hunks[0].first_row, hunks[0].inline_row), (1, 1));
        assert_eq!((hunks[1].first_row, hunks[1].inline_row), (7, 8));
        assert_eq!(hunks[1].start(false), 7);
        assert_eq!(hunks[1].start(true), 8);
    }

    fn git_test(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(root)
            .args([
                "-c",
                "user.name=Kestrel Test",
                "-c",
                "user.email=test@kestrel.invalid",
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git 不可用");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Temp repo with two far-apart edits in file.txt, i.e. two hunks.
    fn two_change_repo(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("kestrel-diff-{tag}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_test(&root, &["init", "-q"]);
        let original: String = (1..=40).map(|line| format!("line {line}\n")).collect();
        std::fs::write(root.join("file.txt"), &original).unwrap();
        git_test(&root, &["add", "file.txt"]);
        git_test(&root, &["commit", "-q", "-m", "base"]);
        let edited = original
            .replace("line 3\n", "line 3 changed\n")
            .replace("line 33\n", "line 33 changed\n");
        std::fs::write(root.join("file.txt"), &edited).unwrap();
        root
    }

    #[test]
    fn load_file_diff_attaches_applicable_hunks() {
        let root = two_change_repo("load");
        let path = root.join("file.txt");
        let data = load_file_diff(&root, &path, false, GitFileStatus::Modified, None, 4).unwrap();
        assert_eq!(data.hunks.len(), 2);
        assert!(data.hunk_patch(0).unwrap().contains("+line 3 changed"));
        assert!(data.hunk_patch(1).unwrap().contains("+line 33 changed"));
        assert!(data.hunk_patch(2).is_none());
        assert_eq!(data.hunks[0].added, 1);
        assert!(data.hunks[1].first_row > data.hunks[0].first_row);

        // The staged view reads the cached patch the same way.
        git_test(&root, &["add", "file.txt"]);
        let staged = load_file_diff(&root, &path, true, GitFileStatus::Modified, None, 4).unwrap();
        assert_eq!(staged.hunks.len(), 2);
        assert!(staged.hunk_patch(1).unwrap().contains("+line 33 changed"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diff_ui_paints_hunk_headers_without_panicking() {
        let root = two_change_repo("ui");
        let path = root.join("file.txt");
        let data = load_file_diff(&root, &path, false, GitFileStatus::Modified, None, 4).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(data.hunks.len(), 2);

        let theme = Theme::dark();
        let style = UiStyle {
            theme,
            ui_family: egui::FontFamily::Proportional,
            ui_bold_family: egui::FontFamily::Proportional,
            mono_family: egui::FontFamily::Monospace,
            mono_bold_family: egui::FontFamily::Monospace,
            ui_size: 13.0,
        };
        let editor_style = EditorStyle {
            theme,
            font_size: 13.0,
            line_h: 18.0,
            tab_size: 4,
            insert_spaces: true,
            show_minimap: false,
            show_line_numbers: true,
            show_indent_guides: false,
            show_whitespace: false,
            show_sticky_scroll: false,
            show_git_gutter: false,
            autocomplete: false,
            word_wrap: false,
            scroll_past_end: false,
            ui_size: 13.0,
            ui_family: egui::FontFamily::Proportional,
            mono_family: egui::FontFamily::Monospace,
        };
        let mut tab = DiffTab::new(
            path,
            "file.txt".to_string(),
            false,
            GitFileStatus::Modified,
        );
        tab.data = Some(data);
        let ctx = egui::Context::default();
        // Two passes: egui resolves widget interaction against the previous
        // frame, so the second pass exercises the real button rects.
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let action = diff_ui(ui, &style, &editor_style, &mut tab);
                    assert!(matches!(action, DiffAction::None));
                });
            });
            output.textures_delta.clear();
        }
        assert!(tab.pending.is_none());
    }

    #[test]
    fn diff_minimap_slider_and_click_jump_mapping() {
        let (offset_top, vh) = diff_minimap_slider_span(400.0, 500, 18.0, 540.0, 0.0);
        assert_eq!(offset_top, 0.0);
        assert!(vh >= 18.0 && vh < 400.0);

        let max_v = 500.0 * 18.0 - 540.0;
        let (offset_end, vh_end) = diff_minimap_slider_span(400.0, 500, 18.0, 540.0, max_v);
        assert!((offset_end + vh_end - 400.0).abs() < 0.01);

        // Clicking near top scrolls to 0, clicking near bottom scrolls to max_v,
        // and clicking in the middle scrolls to ~halfway.
        assert_eq!(diff_minimap_scroll_from_y(10.0, 10.0, 400.0, vh, max_v), 0.0);
        assert!((diff_minimap_scroll_from_y(410.0, 10.0, 400.0, vh, max_v) - max_v).abs() < 0.01);
        let mid = diff_minimap_scroll_from_y(210.0, 10.0, 400.0, vh, max_v);
        assert!((mid - max_v * 0.5).abs() < 1.0);
    }

    #[test]
    fn side_selection_extracts_single_side_and_skips_fillers() {
        let left = "alpha\nremoved_only\n你好世界\nomega";
        let right = "alpha\n你好世界\nadded_line\nomega";
        let left_lines = split(left);
        let right_lines = split(right);
        let (rows, changes, coarse) = compute_rows(&left_lines, &right_lines);
        let inline_rows = build_inline_rows(&rows);
        let data = FileDiff {
            rel_path: "demo.rs".to_string(),
            staged: false,
            left_label: "索引".to_string(),
            right_label: "工作区".to_string(),
            left: left.to_string(),
            right: right.to_string(),
            left_starts: line_starts(left),
            right_starts: line_starts(right),
            left_spans: Vec::new(),
            right_spans: Vec::new(),
            rows,
            inline_rows,
            changes,
            patch_header: String::new(),
            patch_hunks: Vec::new(),
            hunks: Vec::new(),
            coarse,
            max_chars: 20,
        };
        // Select all rows on the Right side: must skip the removed_only filler row!
        let sel = DiffSelection {
            side: DiffSide::Right,
            anchor: DiffPos { row: 0, col: 0 },
            caret: DiffPos {
                row: data.rows.len() - 1,
                col: 5,
            },
        };
        let copied = extract_selection_text(&data, sel, false).unwrap();
        assert_eq!(copied, "alpha\n你好世界\nadded_line\nomega");

        // CJK word/slice extraction on single row
        let cjk_row = data
            .rows
            .iter()
            .position(|r| r.right == Some(1))
            .unwrap();
        let cjk_sel = DiffSelection {
            side: DiffSide::Right,
            anchor: DiffPos {
                row: cjk_row,
                col: 2,
            },
            caret: DiffPos {
                row: cjk_row,
                col: 4,
            },
        };
        assert_eq!(
            extract_selection_text(&data, cjk_sel, false).as_deref(),
            Some("世界")
        );
    }

    #[test]
    fn working_tree_inline_edit_preserves_crlf_and_tabs() {
        let dir = std::env::temp_dir().join(format!(
            "kestrel-diff-edit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.txt");
        std::fs::write(&file, "line1\r\n\tline2\r\nline3\r\n").unwrap();

        let (s, e, loaded) = load_working_lines(&file, 1, 1).unwrap();
        assert_eq!((s, e), (1, 1));
        assert_eq!(loaded, "\tline2");

        replace_working_lines(&file, 1, 1, "\tline2_edited\n\tline2b").unwrap();
        let raw = std::fs::read_to_string(&file).unwrap();
        assert_eq!(raw, "line1\r\n\tline2_edited\r\n\tline2b\r\nline3\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_ui_paints_minimap_selection_find_and_inline_edit() {
        let root = two_change_repo("ui-rich");
        let path = root.join("file.txt");
        let data = load_file_diff(&root, &path, false, GitFileStatus::Modified, None, 4).unwrap();

        let theme = Theme::light();
        let style = UiStyle {
            theme,
            ui_family: egui::FontFamily::Proportional,
            ui_bold_family: egui::FontFamily::Proportional,
            mono_family: egui::FontFamily::Monospace,
            mono_bold_family: egui::FontFamily::Monospace,
            ui_size: 13.0,
        };
        let editor_style = EditorStyle {
            theme,
            font_size: 13.0,
            line_h: 18.0,
            tab_size: 4,
            insert_spaces: true,
            show_minimap: true,
            show_line_numbers: true,
            show_indent_guides: false,
            show_whitespace: false,
            show_sticky_scroll: false,
            show_git_gutter: false,
            autocomplete: false,
            word_wrap: false,
            scroll_past_end: false,
            ui_size: 13.0,
            ui_family: egui::FontFamily::Proportional,
            mono_family: egui::FontFamily::Monospace,
        };
        let mut tab = DiffTab::new(
            path.clone(),
            "file.txt".to_string(),
            false,
            GitFileStatus::Modified,
        );
        tab.data = Some(data);
        tab.data_rev = 1;
        tab.selection = Some(DiffSelection {
            side: DiffSide::Right,
            anchor: DiffPos { row: 2, col: 0 },
            caret: DiffPos { row: 3, col: 6 },
        });
        tab.has_focus = true;
        tab.find.open = true;
        tab.find.query = "changed".to_string();
        tab.open_inline_edit(2, 3);

        let ctx = egui::Context::default();
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = diff_ui(ui, &style, &editor_style, &mut tab);
                });
            });
            output.textures_delta.clear();
        }
        assert_eq!(tab.find.matches.len(), 2);
        assert!(!tab.minimap_cache.buckets.is_empty());
        assert!(tab.inline_edit.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}

