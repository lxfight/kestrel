//! Side-by-side / inline file diff view for Git changes.
//!
//! Loading (git blob reads, file read, syntax highlighting) happens on a worker
//! thread; the UI only paints the prepared [`FileDiff`]. Rows are virtualized,
//! so large diffs stay cheap to paint.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, ScrollArea, Sense, Stroke, Ui,
    Vec2,
};

use crate::editor::EditorStyle;
use crate::git::{self, GitFileStatus};
use crate::highlight::{HighlightState, Language, Span};
use crate::icons::{self, Icon};
use crate::theme::Theme;
use crate::ui::UiStyle;

const MAX_DIFF_LINES: usize = 120_000;
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
const MAX_LCS_CELLS: usize = 4_000_000;

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
    pub coarse: bool,
    max_chars: usize,
}

impl FileDiff {
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }
}

struct LoadedText {
    starts: Vec<usize>,
    spans: Vec<Span>,
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
    pub error: Option<String>,
    pub loading: bool,
    pub rx: Option<Receiver<Result<FileDiff, String>>>,
    pub current_change: usize,
    pub pending_scroll_row: Option<usize>,
    pub h_scroll: f32,
    pub tab_size: usize,
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
            error: None,
            loading: false,
            rx: None,
            current_change: 0,
            pending_scroll_row: None,
            h_scroll: 0.0,
            tab_size: 4,
        }
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
}

pub enum DiffAction {
    None,
    OpenFile(PathBuf),
    Refresh,
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
        coarse,
        max_chars,
    })
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

/// Word-level highlight ranges for a pair of replaced lines (character based).
fn word_diff(left: &str, right: &str) -> (Option<(u32, u32)>, Option<(u32, u32)>) {
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
                &ui.painter(),
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
        "打开源文件",
        tab.path.exists(),
    ) {
        action = DiffAction::OpenFile(tab.path.clone());
    }
    if icon_action(ui, &mut ax, Icon::Refresh, "重新计算差异", true) {
        action = DiffAction::Refresh;
    }
    let data = tab.data.as_ref();
    let change_count = data.map(FileDiff::change_count).unwrap_or(0);
    let has_changes = change_count > 0;
    if icon_action(ui, &mut ax, Icon::ArrowDown, "下一个更改", has_changes) {
        if let Some(data) = &tab.data {
            tab.current_change = (tab.current_change + 1) % data.changes.len();
            tab.pending_scroll_row = data.changes.get(tab.current_change).copied();
        }
    }
    if icon_action(ui, &mut ax, Icon::ArrowUp, "上一个更改", has_changes) {
        if let Some(data) = &tab.data {
            tab.current_change = (tab.current_change + data.changes.len() - 1) % data.changes.len();
            tab.pending_scroll_row = data.changes.get(tab.current_change).copied();
        }
    }
    if has_changes {
        ui.painter().text(
            Pos2::new(ax - 6.0, header.center().y),
            Align2::RIGHT_CENTER,
            format!("{}/{}", tab.current_change + 1, change_count),
            style.tiny(),
            theme.text_faint,
        );
        ax -= 48.0;
    }
    // View mode pill
    ax -= 4.0;
    let pill = Rect::from_center_size(Pos2::new(ax - 30.0, header.center().y), Vec2::new(64.0, 20.0));
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
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
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

    let font = FontId::new(editor_style.font_size, editor_style.mono_family.clone());
    let line_h = editor_style.line_h.max(10.0);
    let char_w = ui
        .ctx()
        .fonts_mut(|fonts| fonts.glyph_width(&font, 'M').max(1.0));

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
        let half = width * 0.5;
        ui.painter().text(
            Pos2::new(labels.left() + 12.0, labels.center().y),
            Align2::LEFT_CENTER,
            &data.left_label,
            style.tiny(),
            theme.text_faint,
        );
        ui.painter().text(
            Pos2::new(labels.left() + half + 12.0, labels.center().y),
            Align2::LEFT_CENTER,
            &data.right_label,
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
            Pos2::new(labels.right() - 12.0, labels.center().y),
            Align2::RIGHT_CENTER,
            "差异过大，已粗略对齐",
            style.tiny(),
            theme.text_faint,
        );
    }

    let body = ui.available_rect_before_wrap();
    let max_digits = digit_count(data.rows.len().max(1));
    let gutter_w = (max_digits as f32 * char_w + 22.0).max(46.0);
    let max_scroll = (data.max_chars as f32 * char_w - (body.width() * 0.5 - gutter_w - 12.0))
        .max(0.0);

    let mut scroll = ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt(("diff_scroll", data.rel_path.as_str(), data.staged));
    if let Some(row) = tab.pending_scroll_row.take() {
        scroll = scroll.vertical_scroll_offset((row as f32 - 2.0).max(0.0) * line_h);
    }
    let out = scroll.show(ui, |ui| {
        let width = ui.available_width();
        let rows = if tab.inline {
            data.inline_rows.len()
        } else {
            data.rows.len()
        };
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(width, rows as f32 * line_h),
            Sense::click(),
        );
        resp.context_menu(|ui| {
            if ui.button("打开源文件").clicked() {
                action = DiffAction::OpenFile(tab.path.clone());
                ui.close();
            }
            if ui.button("复制相对路径").clicked() {
                ui.ctx().copy_text(data.rel_path.clone());
                ui.close();
            }
            if ui.button("复制差异内容").clicked() {
                ui.ctx().copy_text(unified_diff(data));
                ui.close();
            }
        });
        // Horizontal scrolling inside the code view.
        if resp.hovered() {
            let (delta, shift) =
                ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
            if delta.x.abs() > 0.0 {
                tab.h_scroll = (tab.h_scroll - delta.x).clamp(0.0, max_scroll);
            } else if shift && delta.y.abs() > 0.0 {
                tab.h_scroll = (tab.h_scroll - delta.y).clamp(0.0, max_scroll);
            }
        }
        tab.h_scroll = tab.h_scroll.clamp(0.0, max_scroll);

        let clip = ui.clip_rect().intersect(rect);
        let first = (((clip.top() - rect.top()) / line_h).floor().max(0.0)) as usize;
        let last = ((((clip.bottom() - rect.top()) / line_h).ceil()) as usize)
            .min(rows);
        if first >= last {
            return;
        }
        let painter = ui.painter().with_clip_rect(clip);
        let line_number_color = theme.text_faint;
        for row_index in first..last {
            let y = rect.top() + row_index as f32 * line_h;
            if tab.inline {
                let layout = InlineLayout { gutter_w };
                paint_inline_row(
                    ui,
                    &painter,
                    data,
                    &tab,
                    &layout,
                    row_index,
                    y,
                    line_h,
                    char_w,
                    &font,
                    style,
                    theme,
                );
            } else {
                let half = width * 0.5;
                let left_rect = Rect::from_min_size(
                    Pos2::new(rect.left(), y),
                    Vec2::new(half, line_h),
                );
                let right_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + half, y),
                    Vec2::new(width - half, line_h),
                );
                paint_side_row(
                    ui,
                    &painter,
                    data,
                    &tab,
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
                    &tab,
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

    // Scroll state is handled by egui; keep navigation target cleared.
    let _ = out;

    // Overview ruler for changed blocks.
    if !tab.inline {
        let ruler = Rect::from_min_max(
            Pos2::new(body.right() - 8.0, body.top()),
            Pos2::new(body.right() - 2.0, body.bottom()),
        );
        ui.painter().rect_filled(ruler, 0.0, theme.bg);
        let total = data.rows.len().max(1) as f32;
        let mut previous: Option<usize> = None;
        for &change in &data.changes {
            if previous.is_some_and(|p| p + 1 == change) {
                previous = Some(change);
                continue;
            }
            previous = Some(change);
            let kind = data.rows[change].kind;
            let color = match kind {
                RowKind::Added => theme.git_added,
                RowKind::Removed => theme.git_deleted,
                _ => theme.git_modified,
            };
            let y = ruler.top() + (change as f32 / total) * ruler.height();
            ui.painter().rect_filled(
                Rect::from_min_max(
                    Pos2::new(ruler.left(), y),
                    Pos2::new(ruler.right(), (y + 3.0).min(ruler.bottom())),
                ),
                1.0,
                color.gamma_multiply(0.8),
            );
        }
    }

    tab.data = data_holder;
    action
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
                ..Default::default()
            },
        );
    }
    job
}

fn row_colors(kind: RowKind, theme: Theme) -> (Option<Color32>, Option<Color32>) {
    match kind {
        RowKind::Equal => (None, None),
        RowKind::Added => (None, Some(theme.git_added.gamma_multiply(0.16))),
        RowKind::Removed => (Some(theme.git_deleted.gamma_multiply(0.16)), None),
        RowKind::Modified => (
            Some(theme.git_modified.gamma_multiply(0.14)),
            Some(theme.git_modified.gamma_multiply(0.14)),
        ),
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
    char_w: f32,
    font: &FontId,
    number_color: &Color32,
    theme: Theme,
) {
    let Some(row) = data.rows.get(row_index) else {
        return;
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
    let Some(line_index) = line_index else {
        painter.rect_filled(rect, 0.0, theme.input_bg.gamma_multiply(0.55));
        return;
    };
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
    if let Some((start, len)) = hl {
        let x = rect.left() + gutter_w - tab.h_scroll + start as f32 * char_w;
        let width = len as f32 * char_w;
        let color = match row.kind {
            RowKind::Modified => theme.git_modified.gamma_multiply(0.28),
            _ => theme.git_added.gamma_multiply(0.24),
        };
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(width, line_h)),
            0.0,
            color,
        );
    }
    let job = line_job(text_line, spans, line_start, font, theme.text, theme, line_h);
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(
        Pos2::new(rect.left() + gutter_w - tab.h_scroll, rect.top()),
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
    let rect = Rect::from_min_size(
        Pos2::new(painter.clip_rect().left(), y),
        Vec2::new(painter.clip_rect().width(), line_h),
    );
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
    if let Some((start, len)) = entry.hl {
        let x = code_left - tab.h_scroll + start as f32 * char_w;
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(len as f32 * char_w, line_h)),
            0.0,
            theme.git_modified.gamma_multiply(0.3),
        );
    }
    let job = line_job(text_line, spans, line_start, font, theme.text, theme, line_h);
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(
        Pos2::new(code_left - tab.h_scroll, y),
        galley,
        theme.text,
    );
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
}
