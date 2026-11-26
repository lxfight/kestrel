//! One editor tab: text buffer, multi-cursor/selection model, code folding,
//! completion, diagnostics, line diffs, and find-in-file.

pub mod input;
pub mod syntax;
pub mod view;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use egui::FontFamily;

use crate::doc::{Document, EditKind, Encoding, Eol, SaveOutcome};
use crate::git::LineDiffKind;
use crate::highlight::{Language, Span, TokenKind};
use crate::lsp::{CompletionItem, Diagnostic, OutlineSymbol};
use crate::theme::Theme;
use syntax::SyntaxService;

/// Documents larger than this are rendered without syntax highlighting.
pub const MAX_HIGHLIGHT_BYTES: usize = 1_500_000;

/// Document bytes before the caret handed to the completion matcher. Completion
/// must never copy the whole document on every keystroke.
const COMPLETION_CONTEXT_BYTES: usize = 64 * 1024;

/// Snapshot of the user settings the editor needs while painting/handling keys.
#[derive(Clone)]
pub struct EditorStyle {
    pub theme: Theme,
    pub font_size: f32,
    pub line_h: f32,
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub show_minimap: bool,
    pub show_line_numbers: bool,
    pub show_indent_guides: bool,
    pub show_whitespace: bool,
    pub show_sticky_scroll: bool,
    pub show_git_gutter: bool,
    pub autocomplete: bool,
    pub word_wrap: bool,
    pub scroll_past_end: bool,
    pub ui_size: f32,
    pub ui_family: FontFamily,
    pub mono_family: FontFamily,
}

impl EditorStyle {
    pub fn mono(&self) -> egui::FontId {
        egui::FontId::new(self.font_size, self.mono_family.clone())
    }
    pub fn ui(&self) -> egui::FontId {
        egui::FontId::new(self.ui_size, self.ui_family.clone())
    }
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub show_replace: bool,
    pub query: String,
    pub replacement: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub matches: Vec<(usize, usize)>,
    pub active: Option<usize>,
    pub error: Option<String>,
    pub focus_query: bool,
    pub focus_replacement: bool,
    pub last_version: u64,
    /// When `in_selection` is set, matches outside this char range are dropped.
    pub scope: Option<(usize, usize)>,
    pub in_selection: bool,
    /// Recently used queries, oldest first; `history_pos` walks them with ↑/↓.
    pub history: Vec<String>,
    pub history_pos: Option<usize>,
}


#[derive(Default)]
pub struct CompletionState {
    pub open: bool,
    pub items: Vec<CompletionItem>,
    pub selected: usize,
    pub prefix_start: usize,
}

pub struct Editor {
    pub id: u64,
    pub doc: Document,
    pub language: Language,
    pub syntax: SyntaxService,
    /// Latest highlight spans for the current document version.
    pub spans: Vec<Span>,
    pub spans_version: u64,
    pub symbols: Vec<OutlineSymbol>,
    pub folds: Vec<(usize, usize)>,
    pub folded_lines: HashSet<usize>,
    pub diagnostics: Vec<Diagnostic>,
    pub line_diffs: Vec<LineDiffKind>,
    pub diff_version: u64,
    syntax_due: Option<Instant>,

    pub caret: usize,
    pub anchor: usize,
    /// Additional `(caret, anchor)` pairs for multi-cursor editing.
    pub extra_cursors: Vec<(usize, usize)>,
    pub desired_col: Option<usize>,
    pub scroll_y: f32,
    pub scroll_x: f32,

    pub find: FindState,
    pub completion: CompletionState,

    /// Set when the next paint should scroll the caret into view.
    pub ensure_visible: bool,
    pub focus_requested: bool,
    pub last_edit: Instant,
    pub untitled_index: usize,
    /// Preview tabs are reused on single-click in Explorer until edited or pinned.
    pub preview: bool,
    pub pinned: bool,
    /// Synced from settings on every paint (or overridden by `detected_indent`).
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Per-file indentation detected on load when `settings.detect_indentation` is on.
    pub detected_indent: Option<(usize, bool)>,
    pub autocomplete_enabled: bool,
    /// Mirrors `Settings::auto_close_pairs`; off inserts brackets literally.
    pub auto_close: bool,

    // Layout cache (invalidated on version/style change).
    galleys: std::collections::HashMap<usize, CachedLine>,
    /// (doc version, spans version, style hash, wrap width bits, line count, atlas probe).
    cache_key: (u64, u64, u64, u32, usize, u64),
    /// Font-atlas probe hash and fill ratio; an atlas rebuild invalidates every galley.
    atlas_key: u64,
    atlas_fill: f32,
    /// Memo for bracket highlighting: the scan used to run on every painted frame,
    /// it now runs only when the caret or text moves.
    bracket_cache: Option<BracketCache>,
    /// Alt+drag box selection: anchor char and the cursor list the gesture started from.
    box_anchor: Option<usize>,
    box_base: Vec<(usize, usize)>,
    box_moved: bool,
    /// In-editor drag of the selected text: move by default, copy while Ctrl is held.
    pub text_drag: Option<TextDrag>,
    /// Expanding selections remember their history; shrinking pops it.
    selection_history: Vec<(usize, usize)>,
    selection_history_version: u64,
    /// Visible-row mapping (folded lines removed) plus the sorted fold index.
    line_map: LineMap,
    fold_index: FoldIndex,
    fold_rev: u64,
    /// Per-visible-row wrapped heights and prefix Y offsets (only populated when `word_wrap` is on).
    wrap_heights: Vec<f32>,
    wrap_y: Vec<f32>,
    wrap_key: (u64, u64, u32, u32),
    /// Height of the sticky-scroll header currently covering the top of the canvas.
    sticky_h: f32,
    minimap_cache: MinimapCache,
    char_w: f32,
    char_w_size: f32,
    blink_base: f64,
    pub dragging_selection: bool,
    pub has_focus: bool,
    pub md_mode: crate::markdown::MarkdownViewMode,
    pub md_preview: crate::markdown::MarkdownPreviewState,
    pub md_split_ratio: f32,
    /// Display title for virtual documents (git revisions, diffs, etc.).
    pub virtual_title: Option<String>,
    /// When set, this tab renders a Git file diff instead of the text editor.
    pub diff: Option<crate::diff::DiffTab>,
    /// Uncommitted IME composition text, painted at the caret.
    pub ime_preedit: String,
    /// Active (underlined) character range inside `ime_preedit`.
    pub ime_active_range: Option<(usize, usize)>,
    /// Set when the next IME output must abort the OS composition (click / Esc).
    ime_interrupt: bool,
    pub multimodal: Option<crate::viewer::MultimodalViewer>,
}

/// Key for the bracket-pair memo: document version plus caret char offset.
#[derive(Clone, Copy)]
struct BracketCache {
    version: u64,
    caret: usize,
    pair: Option<(usize, usize)>,
}

/// Drag of the current selection inside the canvas.
pub struct TextDrag {
    /// Char under the press point, used to collapse a plain click.
    pub press_char: usize,
    pub press: egui::Pos2,
    /// Caret position the text would be dropped at, `None` while outside the text.
    pub drop: Option<usize>,
    pub moved: bool,
}

struct CachedLine {
    galley: std::sync::Arc<egui::Galley>,
    /// `vis_to_doc[visual_col] = doc_col`; present only when the line has tabs.
    vis_to_doc: Option<Vec<u32>>,
    text_width: f32,
}

/// Maps document lines to visual rows, with folded-away lines removed.
///
/// `rows: None` is the identity mapping, so files without folds allocate nothing.
/// Rebuilt only when the document version or the fold revision changes.
#[derive(Default)]
struct LineMap {
    version: u64,
    fold_rev: u64,
    line_count: usize,
    rows: Option<Vec<u32>>,
}

impl LineMap {
    fn rebuild(
        &mut self,
        line_count: usize,
        fold_rev: u64,
        version: u64,
        hidden: impl Fn(usize) -> bool,
    ) {
        let mut rows = Vec::with_capacity(line_count);
        for line in 0..line_count {
            if !hidden(line) {
                rows.push(line as u32);
            }
        }
        self.rows = (rows.len() != line_count).then_some(rows);
        self.line_count = line_count;
        self.fold_rev = fold_rev;
        self.version = version;
    }

    fn row_count(&self) -> usize {
        match &self.rows {
            Some(rows) => rows.len(),
            None => self.line_count,
        }
    }

    fn row_of(&self, line: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) => rows.binary_search(&(line as u32)).ok(),
            None => (line < self.line_count).then_some(line),
        }
    }

    fn line_of_row(&self, row: usize) -> usize {
        match &self.rows {
            Some(rows) => rows[row.min(rows.len().saturating_sub(1))] as usize,
            None => row.min(self.line_count.saturating_sub(1)),
        }
    }
}

/// Foldable ranges sorted by start line, plus a prefix maximum of their ends, so
/// hidden-line lookups are one binary search instead of a scan over every fold.
#[derive(Default)]
struct FoldIndex {
    rev: u64,
    /// Every foldable range, sorted by start line.
    all: Vec<(usize, usize)>,
    /// Only the folded ranges, sorted by start line.
    ranges: Vec<(usize, usize)>,
    /// `max_end[i] = ranges[..=i].end.max()`.
    max_end: Vec<usize>,
}

impl FoldIndex {
    fn rebuild(&mut self, folds: &[(usize, usize)], folded: &HashSet<usize>, rev: u64) {
        self.all.clear();
        self.ranges.clear();
        for &(start, end) in folds {
            self.all.push((start, end));
            if folded.contains(&start) && start < end {
                self.ranges.push((start, end));
            }
        }
        self.all.sort_unstable_by_key(|(start, _)| *start);
        self.ranges.sort_unstable_by_key(|(start, _)| *start);
        self.max_end = self
            .ranges
            .iter()
            .map(|(_, end)| *end)
            .scan(0, |max, end| {
                *max = (*max).max(end);
                Some(*max)
            })
            .collect();
        self.rev = rev;
    }

    /// Lines strictly inside a folded block are hidden; its start line stays visible.
    fn is_hidden(&self, line: usize) -> bool {
        let idx = self.ranges.partition_point(|(start, _)| *start < line);
        idx > 0 && line <= self.max_end[idx - 1]
    }

    /// Start line of the folded block hiding `line`, if any.
    fn enclosing_start(&self, line: usize) -> Option<usize> {
        let idx = self.max_end.partition_point(|end| *end < line);
        let (start, _) = *self.ranges.get(idx)?;
        (start < line).then_some(start)
    }

    fn range_starting_at(&self, line: usize) -> Option<(usize, usize)> {
        let idx = self
            .all
            .binary_search_by_key(&line, |(start, _)| *start)
            .ok()?;
        self.all.get(idx).copied()
    }
}

/// One sampled line of the minimap, cached per (version, rows) so scrolling does
/// not rescan the rope.
#[derive(Clone, Copy)]
struct MinimapRow {
    indent: u16,
    len: u16,
    color: egui::Color32,
    diff: Option<egui::Color32>,
}

#[derive(Default)]
struct MinimapCache {
    /// (doc version, spans version, theme kind, sampled rows, pixel height, fold revision).
    key: (u64, u64, u8, usize, u32, u64),
    rows: Vec<MinimapRow>,
}

/// Outcome of an explicit format request, so callers can report what actually happened.
pub enum FormatOutcome {
    /// Document is read-only; nothing was formatted.
    ReadOnly,
    /// Text changed.
    Formatted { external: bool },
    /// Text was already normalized.
    Unchanged { external: bool },
}

impl Editor {
    pub fn new(doc: Document, id: u64, untitled_index: usize) -> Self {
        let language = doc
            .path
            .as_deref()
            .map(Language::from_path)
            .unwrap_or(Language::Plain);
        let mut editor = Self {
            id,
            doc,
            language,
            syntax: SyntaxService::new(language),
            spans: Vec::new(),
            spans_version: 0,
            symbols: Vec::new(),
            folds: Vec::new(),
            folded_lines: HashSet::new(),
            diagnostics: Vec::new(),
            line_diffs: Vec::new(),
            diff_version: 0,
            syntax_due: Some(Instant::now()),
            caret: 0,
            anchor: 0,
            extra_cursors: Vec::new(),
            desired_col: None,
            scroll_y: 0.0,
            scroll_x: 0.0,
            find: FindState::default(),
            completion: CompletionState::default(),
            ensure_visible: true,
            focus_requested: true,
            last_edit: Instant::now(),
            untitled_index,
            preview: false,
            pinned: false,
            tab_size: 4,
            insert_spaces: true,
            detected_indent: None,
            autocomplete_enabled: true,
            auto_close: true,
            galleys: std::collections::HashMap::new(),
            cache_key: (0, 0, 0, 0, 0, 0),
            atlas_key: 0,
            atlas_fill: 0.0,
            bracket_cache: None,
            box_anchor: None,
            box_base: Vec::new(),
            box_moved: false,
            text_drag: None,
            selection_history: Vec::new(),
            selection_history_version: 0,
            line_map: LineMap::default(),
            fold_index: FoldIndex::default(),
            fold_rev: 0,
            wrap_heights: Vec::new(),
            wrap_y: Vec::new(),
            wrap_key: (0, 0, 0, 0),
            sticky_h: 0.0,
            minimap_cache: MinimapCache::default(),
            char_w: 8.0,
            char_w_size: 0.0,
            blink_base: 0.0,
            dragging_selection: false,
            has_focus: false,
            md_mode: crate::markdown::MarkdownViewMode::default(),
            md_preview: crate::markdown::MarkdownPreviewState::default(),
            md_split_ratio: 0.5,
            virtual_title: None,
            diff: None,
            ime_preedit: String::new(),
            ime_active_range: None,
            ime_interrupt: false,
            multimodal: None,
        };
        editor.refresh_find_matches();
        editor
    }

    pub fn title(&self) -> String {
        if let Some(title) = &self.virtual_title {
            return title.clone();
        }
        match self.doc.path {
            Some(_) => self.doc.file_name(),
            None => {
                if self.untitled_index == 0 {
                    "未命名".to_string()
                } else {
                    format!("未命名-{}", self.untitled_index)
                }
            }
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.is_dirty()
    }

    pub fn set_language(&mut self, language: Language) {
        if self.language != language {
            self.language = language;
            self.syntax = SyntaxService::new(language);
            self.spans.clear();
            self.symbols.clear();
            self.folds.clear();
            self.folded_lines.clear();
            self.fold_rev += 1;
            self.diagnostics.clear();
            self.spans_version = 0;
            self.invalidate_layout();
            self.syntax_due = Some(Instant::now());
        }
    }

    pub fn invalidate_layout(&mut self) {
        self.galleys.clear();
        self.wrap_key = (0, 0, 0, 0);
    }

    /// Delay until this tab needs another frame for in-flight or debounced syntax work.
    pub fn syntax_wakeup_after(&self) -> Option<Duration> {
        if self.syntax.is_busy() {
            return Some(Duration::from_millis(50));
        }
        self.syntax_due.map(|due| {
            due.saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1))
        })
    }

    /// Effective `(tab_size, insert_spaces)` for this buffer, honoring detected indentation when enabled.
    pub fn effective_indent(
        &self,
        default_tab_size: usize,
        default_insert_spaces: bool,
        detect_enabled: bool,
    ) -> (usize, bool) {
        if detect_enabled
            && let Some((tab_size, insert_spaces)) = self.detected_indent
        {
            return (tab_size.max(1), insert_spaces);
        }
        (default_tab_size.max(1), default_insert_spaces)
    }

    /// Inspect the leading whitespace of the first ~200 lines to detect tabs vs 2/4 spaces.
    pub fn detect_indentation(&mut self) {
        let mut tab_lines = 0usize;
        let mut space_lines = 0usize;
        let mut two_space_diffs = 0usize;
        let mut four_space_diffs = 0usize;
        let mut prev_spaces = 0usize;
        let limit = self.doc.line_count().min(200);
        for line in 0..limit {
            let content = self.doc.line_content(line);
            if content.trim().is_empty() {
                continue;
            }
            if content.starts_with('\t') {
                tab_lines += 1;
                prev_spaces = 0;
            } else {
                let spaces = content.bytes().take_while(|&b| b == b' ').count();
                if spaces >= 2 {
                    space_lines += 1;
                    let diff = spaces.abs_diff(prev_spaces);
                    if diff == 2 || (spaces % 2 == 0 && spaces % 4 != 0) {
                        two_space_diffs += 1;
                    } else if diff == 4 || (spaces >= 4 && spaces % 4 == 0) {
                        four_space_diffs += 1;
                    }
                }
                prev_spaces = spaces;
            }
        }
        if tab_lines > space_lines && tab_lines >= 2 {
            self.detected_indent = Some((self.tab_size.max(1), false));
            self.insert_spaces = false;
        } else if space_lines >= 2 {
            let size = if two_space_diffs > four_space_diffs {
                2
            } else {
                4
            };
            self.detected_indent = Some((size, true));
            self.tab_size = size;
            self.insert_spaces = true;
        }
    }

    /// Reload the underlying document from disk and resync cursors, folds,
    /// syntax tree, and layout caches cleanly.
    pub fn reload(&mut self) -> std::io::Result<()> {
        self.doc.reload()?;
        let len = self.doc.len_chars();
        self.caret = self.caret.min(len);
        self.anchor = self.anchor.min(len);
        self.extra_cursors.clear();
        self.completion.open = false;
        self.ime_preedit.clear();
        self.ime_active_range = None;
        self.syntax.reset(self.language);
        self.spans.clear();
        self.spans_version = 0;
        self.diagnostics.clear();
        self.folds.clear();
        self.folded_lines.clear();
        self.fold_rev += 1;
        self.diff_version = 0;
        self.syntax_due = Some(Instant::now());
        self.invalidate_layout();
        self.refresh_find_matches();
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Selection & Multi-cursor helpers
    // ---------------------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        if self.caret == self.anchor {
            None
        } else {
            Some((self.caret.min(self.anchor), self.caret.max(self.anchor)))
        }
    }

    pub fn selected_text(&self) -> String {
        match self.selection() {
            Some((start, end)) => self.doc.text_range(start, end),
            None => String::new(),
        }
    }

    pub fn clear_extra_cursors(&mut self) {
        self.extra_cursors.clear();
    }

    pub fn toggle_cursor_at(&mut self, pos: usize) {
        let pos = pos.min(self.doc.len_chars());
        if pos == self.caret {
            return;
        }
        // Alt+click inside an existing selection must not drop a cursor into it.
        let inside_primary = self
            .selection()
            .is_some_and(|(start, end)| start < pos && pos < end);
        let inside_extra = self
            .extra_cursors
            .iter()
            .any(|(c, a)| c.min(a) < &pos && &pos < c.max(a));
        if inside_primary || inside_extra {
            return;
        }
        if let Some(idx) = self.extra_cursors.iter().position(|(c, _)| *c == pos) {
            self.extra_cursors.remove(idx);
        } else {
            self.extra_cursors.push((pos, pos));
        }
    }

    /// `Ctrl+D`: Select word at caret, or add the next occurrence of the current selection as a cursor.
    pub fn select_next_occurrence(&mut self) {
        if !self.has_selection() {
            self.select_word_at(self.caret);
            return;
        }
        let needle = self.selected_text();
        if needle.is_empty() {
            return;
        }
        let text = self.doc.text();
        let search_start_char = self
            .extra_cursors
            .last()
            .map(|(c, a)| (*c).max(*a))
            .unwrap_or_else(|| self.caret.max(self.anchor));
        let start_byte = self.doc.char_to_byte(search_start_char);

        let found_byte = text[start_byte..]
            .find(&needle)
            .map(|off| start_byte + off)
            .or_else(|| text[..start_byte].find(&needle));

        if let Some(byte_pos) = found_byte {
            let ch_start = self.doc.byte_to_char(byte_pos);
            let ch_end = ch_start + needle.chars().count();
            let primary = (self.caret.min(self.anchor), self.caret.max(self.anchor));
            if (ch_start, ch_end) != primary
                && !self
                    .extra_cursors
                    .iter()
                    .any(|(c, a)| ((*c).min(*a), (*c).max(*a)) == (ch_start, ch_end))
            {
                self.extra_cursors.push((ch_end, ch_start));
                self.ensure_visible = true;
            }
        }
    }

    /// `Ctrl+Shift+L`: Select all occurrences of the current selection or word under caret.
    pub fn select_all_occurrences(&mut self) {
        if !self.has_selection() {
            self.select_word_at(self.caret);
        }
        let needle = self.selected_text();
        if needle.is_empty() {
            return;
        }
        let text = self.doc.text();
        let needle_chars = needle.chars().count();
        let primary = (self.caret.min(self.anchor), self.caret.max(self.anchor));
        self.extra_cursors.clear();
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(&needle) {
            let byte_pos = from + rel;
            let ch_start = self.doc.byte_to_char(byte_pos);
            let ch_end = ch_start + needle_chars;
            if (ch_start, ch_end) != primary {
                self.extra_cursors.push((ch_end, ch_start));
            }
            from = byte_pos + needle.len().max(1);
            if self.extra_cursors.len() >= 500 {
                break;
            }
        }
    }

    pub fn add_cursor_vertical(&mut self, delta_lines: isize) {
        let base_caret = self
            .extra_cursors
            .last()
            .map(|(c, _)| *c)
            .unwrap_or(self.caret);
        let (line, col) = self.doc.char_to_line_col(base_caret);
        let target_line = (line as isize + delta_lines)
            .clamp(0, self.doc.line_count().saturating_sub(1) as isize)
            as usize;
        if target_line == line {
            return;
        }
        let target_col = col.min(self.doc.line_char_len(target_line));
        let pos = self.doc.line_col_to_char(target_line, target_col);
        if pos != self.caret && !self.extra_cursors.iter().any(|(c, _)| *c == pos) {
            self.extra_cursors.push((pos, pos));
        }
    }

    fn set_caret(&mut self, caret: usize, extend: bool) {
        let caret = caret.min(self.doc.len_chars());
        self.caret = caret;
        if !extend {
            self.anchor = caret;
        }
        self.selection_history.clear();
        self.ensure_visible = true;
        self.completion.open = false;
    }

    pub fn select_all(&mut self) {
        self.extra_cursors.clear();
        self.anchor = 0;
        self.caret = self.doc.len_chars();
        self.ensure_visible = true;
        self.desired_col = None;
    }

    pub fn select_line_at(&mut self, pos: usize) {
        let line = self.doc.char_to_line(pos);
        let start = self.doc.line_to_char(line);
        let end = start + self.doc.line_char_len(line);
        self.anchor = start;
        self.caret = if end < self.doc.len_chars() {
            end + 1
        } else {
            end
        };
        self.ensure_visible = true;
    }

    pub fn select_word_at(&mut self, pos: usize) {
        let (start, end) = self.word_at(pos);
        self.anchor = start;
        self.caret = end;
    }

    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let len = self.doc.len_chars();
        if len == 0 {
            return (0, 0);
        }
        let pos = pos.min(len);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = pos;
        while start > 0 {
            match self.doc.char_at(start - 1) {
                Some(c) if is_word(c) => start -= 1,
                _ => break,
            }
        }
        let mut end = pos;
        while end < len {
            match self.doc.char_at(end) {
                Some(c) if is_word(c) => end += 1,
                _ => break,
            }
        }
        if start == end && end < len {
            end += 1;
        }
        (start, end)
    }

    pub fn identifier_at(&self, pos: usize) -> String {
        let len = self.doc.len_chars();
        if len == 0 {
            return String::new();
        }
        let pos = pos.min(len);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = pos;
        while start > 0 {
            match self.doc.char_at(start - 1) {
                Some(c) if is_word(c) => start -= 1,
                _ => break,
            }
        }
        let mut end = pos;
        while end < len {
            match self.doc.char_at(end) {
                Some(c) if is_word(c) => end += 1,
                _ => break,
            }
        }
        if start < end {
            self.doc.text_range(start, end)
        } else {
            String::new()
        }
    }

    pub fn prev_word(&self, pos: usize) -> usize {
        let mut p = pos.min(self.doc.len_chars());
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        while p > 0 {
            match self.doc.char_at(p - 1) {
                Some(c) if !is_word(c) => p -= 1,
                _ => break,
            }
        }
        while p > 0 {
            match self.doc.char_at(p - 1) {
                Some(c) if is_word(c) => p -= 1,
                _ => break,
            }
        }
        p
    }

    pub fn next_word(&self, pos: usize) -> usize {
        let len = self.doc.len_chars();
        let mut p = pos.min(len);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        while p < len {
            match self.doc.char_at(p) {
                Some(c) if is_word(c) => p += 1,
                _ => break,
            }
        }
        while p < len {
            match self.doc.char_at(p) {
                Some(c) if !is_word(c) => p += 1,
                _ => break,
            }
        }
        p
    }

    // ---------------------------------------------------------------------
    // Code Folding
    // ---------------------------------------------------------------------

    /// Bring the visible-row map and the sorted fold index up to date with the
    /// document version and fold state. Cheap to call every frame.
    fn sync_layout(&mut self) {
        let folds_stale = self.fold_index.rev != self.fold_rev
            || self.fold_index.all.len() != self.folds.len();
        if folds_stale {
            let rev = self.fold_rev;
            let Editor {
                fold_index,
                folds,
                folded_lines,
                ..
            } = self;
            fold_index.rebuild(folds, folded_lines, rev);
        }
        if folds_stale
            || self.line_map.version != self.doc.version
            || self.line_map.fold_rev != self.fold_rev
            || self.line_map.line_count == 0
        {
            let version = self.doc.version;
            let rev = self.fold_rev;
            let line_count = self.doc.line_count().max(1);
            let Editor {
                line_map,
                fold_index,
                ..
            } = self;
            line_map.rebuild(line_count, rev, version, |line| fold_index.is_hidden(line));
        }
    }

    /// Number of rows actually rendered (document lines minus folded-away lines).
    pub fn visible_row_count(&mut self) -> usize {
        self.sync_layout();
        self.line_map.row_count()
    }

    /// Height of the scrollable text area; folded lines occupy no space.
    pub fn total_content_height(&mut self, line_h: f32) -> f32 {
        self.visible_row_count() as f32 * line_h.max(1.0)
    }

    /// Visual row of a document line, `None` while it is folded away.
    pub fn row_of_line(&mut self, line: usize) -> Option<usize> {
        self.sync_layout();
        self.line_map.row_of(line)
    }

    /// Document line rendered at `row` (clamped to the last row).
    pub fn line_of_row(&mut self, row: usize) -> usize {
        self.sync_layout();
        self.line_map.line_of_row(row)
    }

    /// Move a caret that ended up inside a folded block back to the block's start line.
    fn snap_caret_out_of_folds(&mut self) {
        let mut caret_line = self.doc.char_to_line(self.caret);
        let mut anchor_line = self.doc.char_to_line(self.anchor);
        let mut snapped = false;
        if let Some(start) = self.fold_index.enclosing_start(caret_line) {
            caret_line = start;
            snapped = true;
        }
        if let Some(start) = self.fold_index.enclosing_start(anchor_line) {
            anchor_line = start;
            snapped = true;
        }
        if snapped {
            self.caret = self.doc.line_to_char(caret_line) + self.doc.line_char_len(caret_line);
            self.anchor = self.doc.line_to_char(anchor_line) + self.doc.line_char_len(anchor_line);
            self.desired_col = None;
            self.ensure_visible = true;
        }
    }

    pub fn fold_range_for_line(&self, line: usize) -> Option<(usize, usize)> {
        self.fold_index.range_starting_at(line)
    }

    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        self.sync_layout();
        if self.fold_range_for_line(line).is_none() {
            return false;
        }
        if !self.folded_lines.remove(&line) {
            self.folded_lines.insert(line);
        }
        self.fold_rev += 1;
        self.sync_layout();
        self.snap_caret_out_of_folds();
        self.ensure_visible = true;
        true
    }

    pub fn toggle_fold_caret(&mut self) {
        self.sync_layout();
        let caret_line = self.doc.char_to_line(self.caret);
        if self.toggle_fold_at(caret_line) {
            return;
        }
        // Find innermost enclosing foldable range.
        if let Some((start, _)) = self
            .fold_index
            .all
            .iter()
            .filter(|(s, e)| *s <= caret_line && caret_line <= *e)
            .max_by_key(|(s, _)| *s)
            .copied()
        {
            self.toggle_fold_at(start);
        }
    }

    pub fn fold_all(&mut self) {
        self.sync_layout();
        for (start, _) in &self.folds {
            self.folded_lines.insert(*start);
        }
        self.fold_rev += 1;
        self.sync_layout();
        self.snap_caret_out_of_folds();
    }

    pub fn unfold_all(&mut self) {
        self.folded_lines.clear();
        self.fold_rev += 1;
        self.sync_layout();
    }

    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.fold_index.is_hidden(line)
    }

    // ---------------------------------------------------------------------
    // Autocomplete & Formatting
    // ---------------------------------------------------------------------

    /// Start of the identifier being typed at the caret.
    fn ident_start(&self) -> usize {
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = self.caret;
        while start > 0 {
            match self.doc.char_at(start - 1) {
                Some(c) if is_ident(c) => start -= 1,
                _ => break,
            }
        }
        start
    }

    /// True when `ident_start` sits right after `.` or `::` — a member access.
    fn member_access_before(&self, ident_start: usize) -> bool {
        ident_start > 0 && matches!(self.doc.char_at(ident_start - 1), Some('.' | ':'))
    }

    /// Text handed to the completion matcher: up to 64 KB before the caret plus
    /// the rest of the caret line. Never copies the whole document.
    fn completion_context(&self) -> String {
        let line = self.doc.char_to_line(self.caret);
        let line_start = self.doc.line_to_char(line);
        let line_end = line_start + self.doc.line_char_len(line);
        let start_byte = self
            .doc
            .char_to_byte(self.caret)
            .saturating_sub(COMPLETION_CONTEXT_BYTES);
        let start = self.doc.byte_to_char(start_byte).min(line_start);
        self.doc.text_range(start, line_end)
    }

    pub fn update_completion(&mut self) {
        if !self.autocomplete_enabled
            || !self.extra_cursors.is_empty()
            || !self.ime_preedit.is_empty()
        {
            self.completion.open = false;
            return;
        }
        let start = self.ident_start();
        let prefix = self.doc.text_range(start, self.caret);
        let member = self.member_access_before(start);
        if member && prefix.is_empty() {
            self.open_member_completion();
            return;
        }
        // Member access filters from the first typed character; plain identifier
        // completion waits for two to keep the popup quiet.
        let min_prefix = if member { 1 } else { 2 };
        if prefix.chars().count() < min_prefix
            || prefix.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            self.completion.open = false;
            return;
        }
        let context = self.completion_context();
        let items = crate::lsp::complete(&prefix, self.language, &self.symbols, &context);
        if items.is_empty() {
            self.completion.open = false;
        } else {
            self.completion.prefix_start = start;
            self.completion.items = items;
            self.completion.selected = 0;
            self.completion.open = true;
        }
    }

    /// Open the member list right after `.` or `::`: current file symbols plus
    /// language keywords, before the user narrows it down by typing.
    fn open_member_completion(&mut self) {
        if !self.autocomplete_enabled
            || !self.extra_cursors.is_empty()
            || !self.ime_preedit.is_empty()
        {
            self.completion.open = false;
            return;
        }
        let items = crate::lsp::member_candidates(self.language, &self.symbols);
        if items.is_empty() {
            self.completion.open = false;
            return;
        }
        self.completion.prefix_start = self.caret;
        self.completion.items = items;
        self.completion.selected = 0;
        self.completion.open = true;
    }

    pub fn trigger_completion(&mut self) {
        if !self.autocomplete_enabled
            || !self.extra_cursors.is_empty()
            || !self.ime_preedit.is_empty()
        {
            self.completion.open = false;
            return;
        }
        let start = self.ident_start();
        let prefix = self.doc.text_range(start, self.caret);
        if prefix.is_empty() && self.member_access_before(start) {
            self.open_member_completion();
            return;
        }
        let query = if prefix.is_empty() { "a" } else { prefix.as_str() };
        let context = self.completion_context();
        let items = crate::lsp::complete(query, self.language, &self.symbols, &context);
        if !items.is_empty() {
            self.completion.prefix_start = start;
            self.completion.items = items;
            self.completion.selected = 0;
            self.completion.open = true;
        }
    }

    pub fn accept_completion(&mut self) -> bool {
        if !self.completion.open || self.completion.items.is_empty() {
            return false;
        }
        let idx = self
            .completion
            .selected
            .min(self.completion.items.len() - 1);
        let insert = self.completion.items[idx].insert_text.clone();
        let start = self.completion.prefix_start.min(self.caret);
        let end = self.caret;
        self.completion.open = false;
        let before = (self.caret, self.anchor);
        let after_pos = start + insert.chars().count();
        let sed = self.doc.edit(
            start,
            end,
            &insert,
            EditKind::Other,
            before,
            (after_pos, after_pos),
        );
        self.syntax.apply_edit(sed);
        self.caret = after_pos.min(self.doc.len_chars());
        self.anchor = self.caret;
        self.after_edit();
        true
    }

    pub fn format_document(&mut self) -> FormatOutcome {
        if self.doc.readonly {
            return FormatOutcome::ReadOnly;
        }
        let current = self.doc.text();
        let result =
            crate::lsp::format_document(self.language, &current, self.tab_size, self.insert_spaces);
        if result.text == current {
            return FormatOutcome::Unchanged {
                external: result.external,
            };
        }
        let (line, col) = self.doc.char_to_line_col(self.caret);
        let before = (self.caret, self.anchor);
        let len = self.doc.len_chars();
        let sed = self
            .doc
            .edit(0, len, &result.text, EditKind::Other, before, before);
        self.syntax.apply_edit(sed);
        self.caret = self.doc.line_col_to_char(line, col);
        self.anchor = self.caret;
        self.after_edit();
        FormatOutcome::Formatted {
            external: result.external,
        }
    }

    // ---------------------------------------------------------------------
    // Caret movement
    // ---------------------------------------------------------------------

    pub fn move_horizontal(&mut self, forward: bool, extend: bool, by_word: bool) {
        if !extend && let Some((start, end)) = self.selection() {
            self.set_caret(if forward { end } else { start }, false);
            self.desired_col = None;
            return;
        }
        let new = if by_word {
            if forward {
                self.next_word(self.caret)
            } else {
                self.prev_word(self.caret)
            }
        } else if forward {
            (self.caret + 1).min(self.doc.len_chars())
        } else {
            self.caret.saturating_sub(1)
        };
        self.set_caret(new, extend);
        self.desired_col = None;
    }

    pub fn move_vertical(&mut self, delta_lines: isize, extend: bool) {
        self.sync_layout();
        let (line, col) = self.doc.char_to_line_col(self.caret);
        let target_col = self.desired_col.unwrap_or(col);
        let mut target_line = (line as isize + delta_lines).max(0) as usize;
        let max_line = self.doc.line_count().saturating_sub(1);
        target_line = target_line.min(max_line);
        // Skip over folded hidden lines.
        while self.is_line_hidden(target_line) {
            if delta_lines >= 0 {
                if target_line >= max_line {
                    break;
                }
                target_line += 1;
            } else {
                if target_line == 0 {
                    break;
                }
                target_line -= 1;
            }
        }
        let clamped_col = target_col.min(self.doc.line_char_len(target_line));
        let target = self.doc.line_col_to_char(target_line, clamped_col);
        self.set_caret(target, extend);
        self.desired_col = Some(target_col);
    }

    pub fn move_home(&mut self, extend: bool) {
        let line = self.doc.char_to_line(self.caret);
        let start = self.doc.line_to_char(line);
        let indent = self.doc.line_indent(line).chars().count();
        let first_non_space = start + indent;
        let target = if self.caret == first_non_space {
            start
        } else {
            first_non_space
        };
        self.set_caret(target, extend);
        self.desired_col = None;
    }

    pub fn move_end(&mut self, extend: bool) {
        let line = self.doc.char_to_line(self.caret);
        let target = self.doc.line_to_char(line) + self.doc.line_char_len(line);
        self.set_caret(target, extend);
        self.desired_col = None;
    }

    pub fn move_doc(&mut self, to_end: bool, extend: bool) {
        self.set_caret(if to_end { self.doc.len_chars() } else { 0 }, extend);
        self.desired_col = None;
    }

    pub fn move_page(&mut self, pages: isize, extend: bool, rows: usize) {
        self.move_vertical(pages * rows.max(1) as isize, extend);
    }

    // ---------------------------------------------------------------------
    // Editing
    // ---------------------------------------------------------------------

    /// Replace the current selection with `text`.
    fn replace_selection(
        &mut self,
        text: &str,
        kind: EditKind,
        caret_after_override: Option<usize>,
    ) -> bool {
        if self.doc.readonly {
            return false;
        }
        if !self.extra_cursors.is_empty() {
            return self.replace_multi_cursors(text);
        }
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let before = (self.caret, self.anchor);
        let caret_after = caret_after_override.unwrap_or(start + text.chars().count());
        let syntax_edit = self
            .doc
            .edit(start, end, text, kind, before, (caret_after, caret_after));
        self.syntax.apply_edit(syntax_edit);
        self.caret = caret_after.min(self.doc.len_chars());
        self.anchor = self.caret;
        self.after_edit();
        true
    }

    fn replace_multi_cursors(&mut self, text: &str) -> bool {
        let texts = vec![text.to_string(); self.extra_cursors.len() + 1];
        self.replace_multi_cursors_each(&texts)
    }

    /// Replace every cursor's selection with its own text (cursors ordered by position).
    fn replace_multi_cursors_each(&mut self, texts: &[String]) -> bool {
        let mut cursors = Vec::with_capacity(self.extra_cursors.len() + 1);
        cursors.push((self.caret, self.anchor, true));
        for &(c, a) in &self.extra_cursors {
            cursors.push((c, a, false));
        }
        cursors.sort_by_key(|(c, a, _)| (*c).min(*a));

        let edits: Vec<(usize, usize, String)> = cursors
            .iter()
            .enumerate()
            .map(|(i, (c, a, _))| {
                (
                    (*c).min(*a),
                    (*c).max(*a),
                    texts.get(i).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let before = (self.caret, self.anchor);
        let seds = self.doc.edit_many(&edits, before, before);
        for sed in seds {
            self.syntax.apply_edit(sed);
        }

        let mut delta: isize = 0;
        let mut new_extras = Vec::with_capacity(self.extra_cursors.len());
        for (i, (c, a, is_primary)) in cursors.into_iter().enumerate() {
            let start = c.min(a) as isize + delta;
            let removed = c.max(a).saturating_sub(c.min(a)) as isize;
            let inserted = texts.get(i).map_or(0, |text| text.chars().count()) as isize;
            let new_pos = (start + inserted).max(0) as usize;
            delta += inserted - removed;
            if is_primary {
                self.caret = new_pos;
                self.anchor = new_pos;
            } else {
                new_extras.push((new_pos, new_pos));
            }
        }
        self.extra_cursors = new_extras;
        self.after_edit();
        true
    }

    /// Paste, distributing lines across cursors when their counts match.
    pub fn paste_text(&mut self, text: &str) {
        if self.doc.readonly {
            return;
        }
        let cursor_count = self.extra_cursors.len() + 1;
        if cursor_count > 1 {
            let lines: Vec<&str> = text
                .split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .collect();
            if lines.len() == cursor_count {
                let texts: Vec<String> = lines.into_iter().map(str::to_string).collect();
                self.replace_multi_cursors_each(&texts);
                self.completion.open = false;
                return;
            }
        }
        self.insert_text(text);
    }

    fn after_edit(&mut self) {
        self.selection_history.clear();
        self.preview = false;
        self.ensure_visible = true;
        self.desired_col = None;
        self.last_edit = Instant::now();
        let debounce = if self.doc.len_bytes() > 600_000 {
            90
        } else {
            22
        };
        self.syntax_due = Some(Instant::now() + std::time::Duration::from_millis(debounce));
        self.invalidate_layout();
    }

    /// Apply one edit requested by a plugin as a single undo step.
    /// `caret_after` is a char offset; when absent the caret keeps its relative position.
    /// Returns false for read-only documents.
    pub fn apply_plugin_edit(
        &mut self,
        start: usize,
        end: usize,
        text: &str,
        caret_after: Option<usize>,
    ) -> bool {
        if self.doc.readonly {
            return false;
        }
        let len = self.doc.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        let insert_len = text.chars().count();
        let removed = end - start;
        let new_len = len - removed + insert_len;
        let caret = match caret_after {
            Some(caret) => caret,
            None if self.caret <= start => self.caret,
            None if self.caret >= end => self.caret + insert_len - removed,
            None => start + insert_len,
        }
        .min(new_len);
        let before = (self.caret, self.anchor);
        self.extra_cursors.clear();
        self.completion.open = false;
        let syntax_edit = self
            .doc
            .edit(start, end, text, EditKind::Other, before, (caret, caret));
        self.syntax.apply_edit(syntax_edit);
        self.caret = caret;
        self.anchor = caret;
        self.after_edit();
        true
    }

    pub fn insert_text(&mut self, text: &str) {
        if self.doc.readonly {
            return;
        }
        if !self.extra_cursors.is_empty() {
            self.replace_multi_cursors(text);
            self.completion.open = false;
            return;
        }
        // Multi-character inserts (paste, IME) are never auto-paired.
        if text.chars().count() != 1 {
            self.replace_selection(text, EditKind::Other, None);
            self.completion.open = false;
            return;
        }
        let ch = text.chars().next().unwrap();
        // The settings toggle disables both pairing and the skip-over/backspace
        // helpers, so brackets then behave as plain characters.
        let open = self.auto_close && matches!(ch, '(' | '[' | '{' | '"' | '\'' | '`');
        let close = self.auto_close && matches!(ch, ')' | ']' | '}' | '"' | '\'' | '`');

        // Surround an existing selection with a pair.
        if open && self.has_selection() {
            let (start, end) = self.selection().unwrap();
            let selected = self.doc.text_range(start, end);
            let closing = match ch {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                c => c,
            };
            let inserted = format!("{ch}{selected}{closing}");
            let caret = start + inserted.chars().count() - 1;
            self.replace_selection(&inserted, EditKind::Other, Some(caret));
            self.completion.open = false;
            return;
        }

        // Skip over an auto-closed closing char.
        if close && !self.has_selection() && self.doc.char_at(self.caret) == Some(ch) {
            self.update_caret(self.caret + 1);
            self.completion.open = false;
            return;
        }

        if open && self.doc.char_at(self.caret) != Some(ch) {
            let pair: String = match ch {
                '(' => "()".to_string(),
                '[' => "[]".to_string(),
                '{' => "{}".to_string(),
                _ => {
                    let next = self.doc.char_at(self.caret);
                    if next.is_some_and(|c| c.is_alphanumeric()) {
                        self.replace_selection(text, EditKind::Typing, None);
                        self.update_completion();
                        return;
                    }
                    return_quote_pair(ch).to_string()
                }
            };
            let caret = self.caret + 1;
            self.replace_selection(&pair, EditKind::Other, Some(caret));
            self.completion.open = false;
            return;
        }

        self.replace_selection(text, EditKind::Typing, None);
        // `.` / `::` open the member list; other punctuation dismisses it.
        let member_access = ch == '.'
            || (ch == ':' && self.caret >= 2 && self.doc.char_at(self.caret - 2) == Some(':'));
        if member_access {
            self.open_member_completion();
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            self.update_completion();
        } else {
            self.completion.open = false;
        }
    }

    fn update_caret(&mut self, caret: usize) {
        self.caret = caret.min(self.doc.len_chars());
        self.anchor = self.caret;
        self.ensure_visible = true;
        self.desired_col = None;
    }

    pub fn newline(&mut self) {
        if self.doc.readonly {
            return;
        }
        self.completion.open = false;
        if !self.extra_cursors.is_empty() {
            self.replace_multi_cursors("\n");
            self.refresh_find_matches();
            return;
        }
        if self.has_selection() {
            self.replace_selection("", EditKind::Other, None);
        }
        let line = self.doc.char_to_line(self.caret);
        let indent = self.doc.line_indent(line);
        let line_start = self.doc.line_to_char(line);
        if self.caret == line_start && !indent.is_empty() {
            let caret = self.caret + 1 + indent.chars().count();
            self.replace_selection("\n", EditKind::Other, Some(caret));
            return;
        }
        let before = self.doc.text_range(line_start, self.caret);
        let trimmed = before.trim_end();
        let opens_block = trimmed.ends_with('{')
            || trimmed.ends_with('(')
            || trimmed.ends_with('[')
            || trimmed.ends_with(':');
        let next_char = self.doc.char_at(self.caret);
        let closing_next = next_char.is_some_and(|c| matches!(c, '}' | ')' | ']'));

        let extra = if opens_block {
            if self.doc.line_indent(line).contains('\t') {
                "\t".to_string()
            } else {
                " ".repeat(self.tab_size.max(1))
            }
        } else {
            String::new()
        };

        let insert = if opens_block && closing_next {
            format!("\n{indent}{extra}\n{indent}")
        } else if opens_block {
            format!("\n{indent}{extra}")
        } else {
            format!("\n{indent}")
        };
        let caret_after = self.caret + 1 + indent.chars().count() + extra.chars().count();
        self.replace_selection(&insert, EditKind::Other, Some(caret_after));
        self.refresh_find_matches();
    }

    pub fn backspace(&mut self) {
        if self.doc.readonly {
            return;
        }
        if !self.extra_cursors.is_empty() {
            let any_sel = self.has_selection() || self.extra_cursors.iter().any(|(c, a)| c != a);
            if any_sel {
                self.replace_multi_cursors("");
            } else {
                let mut all = Vec::with_capacity(self.extra_cursors.len() + 1);
                all.push((self.caret, true));
                for &(c, _) in &self.extra_cursors {
                    all.push((c, false));
                }
                all.sort_by_key(|(c, _)| *c);
                let edits: Vec<(usize, usize, String)> = all
                    .iter()
                    .filter(|(c, _)| *c > 0)
                    .map(|(c, _)| (c - 1, *c, String::new()))
                    .collect();
                let before = (self.caret, self.anchor);
                let seds = self.doc.edit_many(&edits, before, before);
                for sed in seds {
                    self.syntax.apply_edit(sed);
                }
                let mut delta = 0usize;
                let mut new_extras = Vec::new();
                for (c, is_primary) in all {
                    if c > 0 {
                        delta += 1;
                    }
                    let new_c = c.saturating_sub(delta);
                    if is_primary {
                        self.caret = new_c;
                        self.anchor = new_c;
                    } else {
                        new_extras.push((new_c, new_c));
                    }
                }
                self.extra_cursors = new_extras;
                self.after_edit();
            }
            self.completion.open = false;
            return;
        }
        if self.has_selection() {
            self.replace_selection("", EditKind::Other, None);
            self.refresh_find_matches();
            self.completion.open = false;
            return;
        }
        if self.caret == 0 {
            return;
        }
        // Delete auto-closed pair as one unit.
        let prev = self.doc.char_at(self.caret - 1);
        let next = self.doc.char_at(self.caret);
        if self.auto_close
            && let (Some(a), Some(b)) = (prev, next)
            && matches!(
                (a, b),
                ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'') | ('`', '`')
            )
        {
            let before = (self.caret, self.anchor);
            let sed = self.doc.edit(
                self.caret - 1,
                self.caret + 1,
                "",
                EditKind::Other,
                before,
                (self.caret - 1, self.caret - 1),
            );
            self.syntax.apply_edit(sed);
            self.caret -= 1;
            self.anchor = self.caret;
            self.after_edit();
            self.refresh_find_matches();
            self.completion.open = false;
            return;
        }
        // Backspace over indentation snaps to the previous tab stop.
        let line = self.doc.char_to_line(self.caret);
        let line_start = self.doc.line_to_char(line);
        let col = self.caret - line_start;
        let prefix: String = (line_start..self.caret)
            .filter_map(|c| self.doc.char_at(c))
            .collect();
        if prefix.chars().all(|c| c == ' ')
            && col > 0
            && self.doc.line_indent(line).chars().count() == col
        {
            let tab = self.tab_size.max(1);
            let remove = ((col - 1) / tab) * tab;
            let target = line_start + remove;
            let n = self.caret - target;
            if n > 1 {
                let before = (self.caret, self.anchor);
                let sed = self.doc.edit(
                    target,
                    self.caret,
                    "",
                    EditKind::Backspace,
                    before,
                    (target, target),
                );
                self.syntax.apply_edit(sed);
                self.caret = target;
                self.anchor = target;
                self.after_edit();
                self.refresh_find_matches();
                self.completion.open = false;
                return;
            }
        }
        let before = (self.caret, self.anchor);
        let sed = self.doc.edit(
            self.caret - 1,
            self.caret,
            "",
            EditKind::Backspace,
            before,
            (self.caret - 1, self.caret - 1),
        );
        self.syntax.apply_edit(sed);
        self.caret -= 1;
        self.anchor = self.caret;
        self.after_edit();
        self.refresh_find_matches();
        self.update_completion();
    }

    pub fn delete_forward(&mut self) {
        if self.doc.readonly {
            return;
        }
        self.completion.open = false;
        if !self.extra_cursors.is_empty() {
            let any_sel = self.has_selection() || self.extra_cursors.iter().any(|(c, a)| c != a);
            if any_sel {
                self.replace_multi_cursors("");
            } else {
                let doc_len = self.doc.len_chars();
                let mut all = Vec::with_capacity(self.extra_cursors.len() + 1);
                all.push((self.caret, true));
                for &(c, _) in &self.extra_cursors {
                    all.push((c, false));
                }
                all.sort_by_key(|(c, _)| *c);
                let edits: Vec<(usize, usize, String)> = all
                    .iter()
                    .filter(|(c, _)| *c < doc_len)
                    .map(|(c, _)| (*c, c + 1, String::new()))
                    .collect();
                if !edits.is_empty() {
                    let before = (self.caret, self.anchor);
                    let mut delta = 0usize;
                    let mut new_extras = Vec::new();
                    let mut primary_after = self.caret;
                    for (c, is_primary) in all {
                        let new_c = c.saturating_sub(delta);
                        if c < doc_len {
                            delta += 1;
                        }
                        if is_primary {
                            primary_after = new_c;
                        } else {
                            new_extras.push((new_c, new_c));
                        }
                    }
                    let seds = self
                        .doc
                        .edit_many(&edits, before, (primary_after, primary_after));
                    for sed in seds {
                        self.syntax.apply_edit(sed);
                    }
                    self.caret = primary_after.min(self.doc.len_chars());
                    self.anchor = self.caret;
                    self.extra_cursors = new_extras;
                    self.after_edit();
                }
            }
            self.refresh_find_matches();
            return;
        }
        if self.has_selection() {
            self.replace_selection("", EditKind::Other, None);
            self.refresh_find_matches();
            return;
        }
        if self.caret >= self.doc.len_chars() {
            return;
        }
        let before = (self.caret, self.anchor);
        let sed = self.doc.edit(
            self.caret,
            self.caret + 1,
            "",
            EditKind::Delete,
            before,
            (self.caret, self.caret),
        );
        self.syntax.apply_edit(sed);
        self.after_edit();
        self.refresh_find_matches();
    }

    pub fn delete_word(&mut self, backward: bool) {
        if self.doc.readonly {
            return;
        }
        self.completion.open = false;
        if !self.extra_cursors.is_empty() {
            let mut all = Vec::with_capacity(self.extra_cursors.len() + 1);
            all.push((self.caret, self.anchor, true));
            for &(c, a) in &self.extra_cursors {
                all.push((c, a, false));
            }
            all.sort_by_key(|(c, a, _)| (*c).min(*a));
            let mut ranges: Vec<(usize, usize, bool)> = Vec::with_capacity(all.len());
            for (c, a, is_primary) in all {
                let (s, e) = if c != a {
                    (c.min(a), c.max(a))
                } else if backward {
                    (self.prev_word(c), c)
                } else {
                    (c, self.next_word(c))
                };
                if let Some(last) = ranges.last_mut()
                    && s <= last.1
                {
                    last.1 = last.1.max(e);
                    last.2 |= is_primary;
                } else {
                    ranges.push((s, e, is_primary));
                }
            }
            let edits: Vec<(usize, usize, String)> = ranges
                .iter()
                .filter(|(s, e, _)| s < e)
                .map(|(s, e, _)| (*s, *e, String::new()))
                .collect();
            if !edits.is_empty() {
                let before = (self.caret, self.anchor);
                let mut delta = 0usize;
                let mut primary_after = self.caret;
                let mut new_extras = Vec::new();
                for (s, e, is_primary) in ranges {
                    let pos = s.saturating_sub(delta);
                    delta += e - s;
                    if is_primary {
                        primary_after = pos;
                    } else {
                        new_extras.push((pos, pos));
                    }
                }
                let seds = self
                    .doc
                    .edit_many(&edits, before, (primary_after, primary_after));
                for sed in seds {
                    self.syntax.apply_edit(sed);
                }
                self.caret = primary_after.min(self.doc.len_chars());
                self.anchor = self.caret;
                self.extra_cursors = new_extras;
                self.after_edit();
                self.refresh_find_matches();
            }
            return;
        }
        if self.has_selection() {
            self.replace_selection("", EditKind::Other, None);
            self.refresh_find_matches();
            return;
        }
        let (start, end) = if backward {
            (self.prev_word(self.caret), self.caret)
        } else {
            (self.caret, self.next_word(self.caret))
        };
        if start == end {
            return;
        }
        let before = (self.caret, self.anchor);
        let sed = self
            .doc
            .edit(start, end, "", EditKind::Other, before, (start, start));
        self.syntax.apply_edit(sed);
        self.caret = start;
        self.anchor = start;
        self.after_edit();
        self.refresh_find_matches();
    }

    fn selected_lines(&self) -> (usize, usize) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let first = self.doc.char_to_line(start);
        let mut last = self.doc.char_to_line(end);
        if last > first && self.doc.line_to_char(last) == end {
            last -= 1;
        }
        (first, last)
    }

    pub fn indent_selection(&mut self, outdent: bool) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        let uses_tabs = self.doc.line_indent(first).contains('\t');
        let unit = if uses_tabs {
            "\t".to_string()
        } else {
            " ".repeat(self.tab_size)
        };
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for line in first..=last {
            let start = self.doc.line_to_char(line);
            if outdent {
                let indent = self.doc.line_indent(line);
                let remove = if indent.starts_with('\t') {
                    1
                } else {
                    indent.len().min(self.tab_size)
                };
                if remove > 0 {
                    edits.push((start, start + remove, String::new()));
                }
            } else if !self.doc.line_content(line).is_empty() {
                edits.push((start, start, unit.clone()));
            }
        }
        if edits.is_empty() {
            return;
        }
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let (anchor_line, anchor_col) = self.doc.char_to_line_col(self.anchor);
        let before = (self.caret, self.anchor);
        let syntax_edits = self
            .doc
            .edit_many(&edits, before, (self.caret, self.anchor));
        for se in syntax_edits {
            self.syntax.apply_edit(se);
        }
        let delta = if outdent {
            -(unit.chars().count() as isize)
        } else {
            unit.chars().count() as isize
        };
        let clamp = |doc: &Document, line: usize, col: isize| -> usize {
            let start = doc.line_to_char(line) as isize;
            let len = doc.line_char_len(line) as isize;
            (start + (col + delta).max(0)).min(start + len) as usize
        };
        let caret_at = self.caret;
        let anchor_at = self.anchor;
        self.caret = clamp(&self.doc, caret_line, caret_col as isize);
        let new_anchor_line = if anchor_at == caret_at {
            self.caret
        } else {
            clamp(&self.doc, anchor_line, anchor_col as isize)
        };
        self.anchor = new_anchor_line;
        if let Some(last_undo) = self.doc.undo.last_mut() {
            last_undo.after = (self.caret, self.anchor);
        }
        self.after_edit();
    }

    fn comment_delimiters(&self) -> (&'static str, &'static str) {
        match self.language {
            Language::Python | Language::Bash | Language::Yaml | Language::Toml => ("# ", ""),
            Language::Html | Language::Markdown => ("<!-- ", " -->"),
            Language::Css => ("/* ", " */"),
            _ => ("// ", ""),
        }
    }

    pub fn toggle_comment(&mut self) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        let (prefix, suffix) = self.comment_delimiters();
        let prefix_bare = prefix.trim_end();
        let suffix_bare = suffix.trim_start();
        let all_commented = (first..=last)
            .filter(|line| !self.doc.line_content(*line).trim().is_empty())
            .all(|line| {
                let content = self.doc.line_content(line);
                let trimmed = content.trim();
                trimmed.starts_with(prefix_bare)
                    && (suffix_bare.is_empty() || trimmed.ends_with(suffix_bare))
            });
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for line in first..=last {
            let content = self.doc.line_content(line);
            if content.trim().is_empty() {
                continue;
            }
            let indent_len = content.len() - content.trim_start().len();
            let line_start = self.doc.line_to_char(line);
            let start = line_start + content[..indent_len].chars().count();
            let trimmed_right_end = line_start + content.trim_end().chars().count();
            if all_commented {
                let rest = &content[indent_len..];
                let trimmed = rest.trim_start();
                if trimmed.starts_with(prefix_bare) {
                    let remove_len = if trimmed.starts_with(prefix) {
                        prefix.len()
                    } else {
                        prefix_bare.len()
                    };
                    edits.push((
                        start,
                        start + trimmed[..remove_len].chars().count(),
                        String::new(),
                    ));
                }
                if !suffix_bare.is_empty() {
                    let trimmed_r = content.trim_end();
                    if trimmed_r.ends_with(suffix_bare) {
                        let remove_bytes = if trimmed_r.ends_with(suffix) {
                            suffix.len()
                        } else {
                            suffix_bare.len()
                        };
                        let remove_chars =
                            trimmed_r[trimmed_r.len() - remove_bytes..].chars().count();
                        edits.push((
                            trimmed_right_end.saturating_sub(remove_chars),
                            trimmed_right_end,
                            String::new(),
                        ));
                    }
                }
            } else {
                edits.push((start, start, prefix.to_string()));
                if !suffix.is_empty() {
                    edits.push((trimmed_right_end, trimmed_right_end, suffix.to_string()));
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let (anchor_line, anchor_col) = self.doc.char_to_line_col(self.anchor);
        let before = (self.caret, self.anchor);
        let syntax_edits = self
            .doc
            .edit_many(&edits, before, (self.caret, self.anchor));
        for se in syntax_edits {
            self.syntax.apply_edit(se);
        }
        let shift = if all_commented {
            -(prefix.chars().count() as isize)
        } else {
            prefix.chars().count() as isize
        };
        let clamp = |doc: &Document, line: usize, col: usize| -> usize {
            let new_col = (col as isize + shift).max(0) as usize;
            doc.line_col_to_char(line, new_col.min(doc.line_char_len(line)))
        };
        self.caret = clamp(&self.doc, caret_line, caret_col);
        self.anchor = if before.0 == before.1 {
            self.caret
        } else {
            clamp(&self.doc, anchor_line, anchor_col)
        };
        if let Some(last_undo) = self.doc.undo.last_mut() {
            last_undo.after = (self.caret, self.anchor);
        }
        self.after_edit();
        self.refresh_find_matches();
    }

    pub fn duplicate_line(&mut self) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        let start = self.doc.line_to_char(first);
        let end = self.doc.line_to_char(last) + self.doc.line_char_len(last);
        let block = self.doc.text_range(start, end);
        let inserted = format!("\n{block}");
        let before = (self.caret, self.anchor);
        let after_caret = self.caret + inserted.chars().count();
        let syntax_edit = self.doc.edit(
            end,
            end,
            &inserted,
            EditKind::Other,
            before,
            (after_caret, after_caret),
        );
        self.syntax.apply_edit(syntax_edit);
        self.caret = after_caret.min(self.doc.len_chars());
        self.anchor = self.caret;
        self.after_edit();
        self.ensure_visible = true;
    }

    pub fn delete_line(&mut self) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        let start = self.doc.line_to_char(first);
        let line_after = last + 1;
        let end = if line_after < self.doc.line_count() {
            self.doc.line_to_char(line_after)
        } else {
            self.doc.line_to_char(last) + self.doc.line_char_len(last)
        };
        if start >= end {
            return;
        }
        let before = (self.caret, self.anchor);
        let sed = self
            .doc
            .edit(start, end, "", EditKind::Other, before, (start, start));
        self.syntax.apply_edit(sed);
        self.caret = start;
        self.anchor = start;
        self.after_edit();
        self.refresh_find_matches();
    }

    pub fn move_lines(&mut self, up: bool) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        if up && first == 0 {
            return;
        }
        if !up && last + 1 >= self.doc.line_count() {
            return;
        }
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let (anchor_line, anchor_col) = self.doc.char_to_line_col(self.anchor);

        let block_start = self.doc.line_to_char(first);
        let block_end = self.doc.line_to_char(last) + self.doc.line_char_len(last);
        let block = self.doc.text_range(block_start, block_end);
        let (start, end, replacement) = if up {
            let prev_start = self.doc.line_to_char(first - 1);
            let prev_len = self.doc.line_char_len(first - 1);
            let prev = self.doc.text_range(prev_start, prev_start + prev_len);
            (prev_start, block_end, format!("{block}\n{prev}"))
        } else {
            let next_start = self.doc.line_to_char(last + 1);
            let next_len = self.doc.line_char_len(last + 1);
            let next = self.doc.text_range(next_start, next_start + next_len);
            (
                block_start,
                next_start + next_len,
                format!("{next}\n{block}"),
            )
        };
        let before = (self.caret, self.anchor);
        let syntax_edit = self.doc.edit(
            start,
            end,
            &replacement,
            EditKind::Other,
            before,
            (self.caret, self.anchor),
        );
        self.syntax.apply_edit(syntax_edit);

        let shift = if up { -1isize } else { 1 };
        let new_caret_line =
            (caret_line as isize + shift).clamp(0, self.doc.line_count() as isize - 1) as usize;
        let new_anchor_line =
            (anchor_line as isize + shift).clamp(0, self.doc.line_count() as isize - 1) as usize;
        self.caret = self.doc.line_col_to_char(
            new_caret_line,
            caret_col.min(self.doc.line_char_len(new_caret_line)),
        );
        self.anchor = self.doc.line_col_to_char(
            new_anchor_line,
            anchor_col.min(self.doc.line_char_len(new_anchor_line)),
        );
        if let Some(last_undo) = self.doc.undo.last_mut() {
            last_undo.after = (self.caret, self.anchor);
        }
        self.after_edit();
        self.ensure_visible = true;
    }

    pub fn undo(&mut self) {
        if self.doc.readonly {
            return;
        }
        if let Some((caret, anchor)) = self.doc.undo() {
            self.restore_after_history(caret, anchor);
        }
    }

    pub fn redo(&mut self) {
        if self.doc.readonly {
            return;
        }
        if let Some((caret, anchor)) = self.doc.redo() {
            self.restore_after_history(caret, anchor);
        }
    }

    fn restore_after_history(&mut self, caret: usize, anchor: usize) {
        self.extra_cursors.clear();
        self.completion.open = false;
        self.caret = caret.min(self.doc.len_chars());
        self.anchor = anchor.min(self.doc.len_chars());
        self.syntax.reset(self.language);
        self.spans.clear();
        self.spans_version = 0;
        self.syntax_due = Some(Instant::now());
        self.invalidate_layout();
        self.ensure_visible = true;
        self.refresh_find_matches();
    }

    pub fn replace_all(&mut self) -> usize {
        if self.doc.readonly {
            return 0;
        }
        let regex = match self.build_find_regex() {
            Ok(regex) => regex,
            Err(err) => {
                self.find.error = Some(err);
                return 0;
            }
        };
        let text = self.doc.text();
        let regex_mode = self.find.regex;
        let (scope_start, scope_end) = self.find.scope.unwrap_or((0, usize::MAX));
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut first_start = None;
        let mut byte = 0usize;
        let mut ch = 0usize;
        for captures in regex.captures_iter(&text) {
            let Some(whole) = captures.get(0) else {
                continue;
            };
            if whole.start() == whole.end() {
                continue;
            }
            ch += text[byte..whole.start()].chars().count();
            let start = ch;
            ch += text[whole.start()..whole.end()].chars().count();
            byte = whole.end();
            if start < scope_start || ch > scope_end {
                continue;
            }
            let mut replacement = String::new();
            if regex_mode {
                captures.expand(&self.find.replacement, &mut replacement);
            } else {
                replacement.push_str(&self.find.replacement);
            }
            first_start.get_or_insert(start);
            edits.push((start, ch, replacement));
            if edits.len() >= 50_000 {
                break;
            }
        }
        let count = edits.len();
        if edits.is_empty() {
            return 0;
        }
        let before = (self.caret, self.anchor);
        let syntax_edits = self
            .doc
            .edit_many(&edits, before, (self.caret, self.anchor));
        for se in syntax_edits {
            self.syntax.apply_edit(se);
        }
        if let Some(start) = first_start {
            self.caret = start.min(self.doc.len_chars());
            self.anchor = self.caret;
            if let Some(last_undo) = self.doc.undo.last_mut() {
                last_undo.after = (self.caret, self.anchor);
            }
        }
        self.after_edit();
        self.recompute_find();
        self.ensure_visible = true;
        count
    }

    fn build_find_regex(&self) -> Result<regex::Regex, String> {
        let mut pattern = if self.find.regex {
            self.find.query.clone()
        } else {
            regex::escape(&self.find.query)
        };
        if self.find.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.find.case_sensitive)
            .size_limit(8 * 1024 * 1024)
            .build()
            .map_err(|err| format!("正则表达式无效: {err}"))
    }

    // ---------------------------------------------------------------------
    // Find
    // ---------------------------------------------------------------------

    pub fn open_find(&mut self, with_replace: bool) {
        self.find.open = true;
        self.find.show_replace = with_replace;
        self.find.focus_query = true;
        if let Some((start, end)) = self.selection()
            && !self.doc.text_range(start, end).contains('\n')
        {
            let text = self.doc.text_range(start, end);
            if !text.is_empty() {
                self.find.query = text;
            }
        }
        self.recompute_find();
        if self.find.active.is_none() {
            self.find_next(true);
        }
    }

    pub fn close_find(&mut self) {
        self.remember_find_query();
        self.find.open = false;
        self.find.focus_query = false;
        self.find.focus_replacement = false;
        self.focus_requested = true;
    }

    pub fn refresh_find_matches(&mut self) {
        if self.find.open {
            self.recompute_find();
        }
    }

    pub fn recompute_find(&mut self) {
        self.find.last_version = self.doc.version;
        if !self.find.open && self.find.query.is_empty() {
            self.find.matches.clear();
            return;
        }
        if self.find.query.is_empty() {
            self.find.matches.clear();
            self.find.error = None;
            self.find.active = None;
            return;
        }
        // "仅在选区内查找" snapshots the selection when the toggle flips; if the
        // selection vanished the search falls back to the whole document.
        if self.find.in_selection && self.find.scope.is_none() {
            self.find.scope = self.selection();
        }
        let mut pattern = if self.find.regex {
            self.find.query.clone()
        } else {
            regex::escape(&self.find.query)
        };
        if self.find.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let regex = match regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.find.case_sensitive)
            .size_limit(8 * 1024 * 1024)
            .build()
        {
            Ok(regex) => regex,
            Err(err) => {
                self.find.error = Some(format!("正则表达式无效: {err}"));
                self.find.matches.clear();
                self.find.active = None;
                return;
            }
        };
        self.find.error = None;
        let text = self.doc.text();
        let (scope_start, scope_end) = self.find.scope.unwrap_or((0, usize::MAX));
        let mut matches = Vec::new();
        let mut byte = 0usize;
        let mut ch = 0usize;
        for m in regex.find_iter(&text) {
            if matches.len() >= 50_000 {
                break;
            }
            ch += text[byte..m.start()].chars().count();
            let start = ch;
            ch += text[m.start()..m.end()].chars().count();
            byte = m.end();
            if start < scope_start || ch > scope_end {
                continue;
            }
            matches.push((start, ch));
        }
        self.find.matches = matches;
        if self.find.matches.is_empty() {
            self.find.active = None;
        } else {
            let active = self
                .find
                .matches
                .iter()
                .position(|(start, end)| *start <= self.caret && self.caret <= *end)
                .or_else(|| {
                    self.find
                        .matches
                        .iter()
                        .position(|(start, _)| *start >= self.caret)
                })
                .unwrap_or(self.find.matches.len() - 1);
            self.find.active = Some(active);
        }
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.find.matches.is_empty() || self.find.last_version != self.doc.version {
            self.recompute_find();
        }
        if self.find.matches.is_empty() {
            return;
        }
        let count = self.find.matches.len();
        let next = match self.find.active {
            Some(active) => {
                if forward {
                    (active + 1) % count
                } else {
                    (active + count - 1) % count
                }
            }
            None => 0,
        };
        self.activate_match(next);
    }

    fn activate_match(&mut self, index: usize) {
        if let Some((start, end)) = self.find.matches.get(index).copied() {
            self.find.active = Some(index);
            self.anchor = start;
            self.caret = end;
            self.ensure_visible = true;
            self.desired_col = None;
        }
    }

    pub fn replace_current(&mut self) {
        if self.doc.readonly {
            return;
        }
        let Some(index) = self.find.active else {
            return;
        };
        let Some((start, end)) = self.find.matches.get(index).copied() else {
            return;
        };
        let replacement = self.replacement_for(start, end);
        let before = (self.caret, self.anchor);
        let caret_after = start + replacement.chars().count();
        let sed = self.doc.edit(
            start,
            end,
            &replacement,
            EditKind::Other,
            before,
            (caret_after, caret_after),
        );
        self.syntax.apply_edit(sed);
        self.caret = caret_after;
        self.anchor = caret_after;
        self.after_edit();
        self.recompute_find();
        if !self.find.matches.is_empty() {
            let next = self
                .find
                .active
                .unwrap_or(0)
                .min(self.find.matches.len() - 1);
            self.activate_match(next);
        }
    }

    // ---------------------------------------------------------------------
    // Bracket matching, selection expansion & navigation
    // ---------------------------------------------------------------------

    /// Bracket pair around the caret. The bracket right before the caret wins, so
    /// typing after `(` immediately shows its mate.
    pub fn matching_brackets(&self) -> Option<(usize, usize)> {
        for pos in [self.caret.checked_sub(1), Some(self.caret)]
            .into_iter()
            .flatten()
        {
            if let Some(pair) = self.matching_pair_at(pos) {
                return Some(pair);
            }
        }
        None
    }

    /// Move the caret to the bracket matching the one next to it (Ctrl+Shift+\).
    pub fn jump_to_matching_bracket(&mut self) -> bool {
        let Some((open, close)) = self.matching_brackets() else {
            return false;
        };
        let mut target = None;
        for pos in [self.caret.checked_sub(1), Some(self.caret)]
            .into_iter()
            .flatten()
        {
            if pos == open {
                target = Some(close);
                break;
            }
            if pos == close {
                target = Some(open);
                break;
            }
        }
        let Some(target) = target else {
            return false;
        };
        self.set_caret(target, false);
        self.desired_col = None;
        true
    }

    fn matching_pair_at(&self, pos: usize) -> Option<(usize, usize)> {
        let bracket = self.doc.char_at(pos)?;
        let (open, close, forward) = match bracket {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return None,
        };
        let other = if forward { close } else { open };
        let origin_in_lit = self.is_in_string_or_comment(pos);
        let mut depth = 0i32;
        let mut p = pos;
        // Bounded scan: a broken file must not stall the key handler.
        for _ in 0..20_000 {
            if forward {
                p += 1;
                if p >= self.doc.len_chars() {
                    return None;
                }
            } else {
                p = p.checked_sub(1)?;
            }
            let ch = self.doc.char_at(p)?;
            if ch != bracket && ch != other {
                continue;
            }
            if !origin_in_lit && self.is_in_string_or_comment(p) {
                continue;
            }
            if ch == bracket {
                depth += 1;
            } else if ch == other {
                if depth == 0 {
                    return if forward { Some((pos, p)) } else { Some((p, pos)) };
                }
                depth -= 1;
            }
        }
        None
    }

    /// True when the char sits inside a string literal or a comment span.
    pub fn is_in_string_or_comment(&self, ch_pos: usize) -> bool {
        let byte = self.doc.char_to_byte(ch_pos);
        let idx = self.spans.partition_point(|s| s.end <= byte);
        matches!(
            self.spans.get(idx).map(|s| s.kind),
            Some(TokenKind::String) | Some(TokenKind::Comment)
        )
    }

    /// Grow the selection: word → bracket interior → bracket → line → enclosing block.
    pub fn expand_selection(&mut self) -> bool {
        let current = self.selection().unwrap_or((self.caret, self.caret));
        let Some(next) = self.selection_candidates(current).into_iter().find(|candidate| {
            *candidate != current && candidate.0 <= current.0 && candidate.1 >= current.1
        }) else {
            return false;
        };
        if current.0 != current.1 {
            self.selection_history.push(current);
            self.selection_history_version = self.doc.version;
        }
        self.anchor = next.0;
        self.caret = next.1;
        self.ensure_visible = true;
        self.desired_col = None;
        true
    }

    /// Shrink back through the ranges `expand_selection` produced.
    pub fn shrink_selection(&mut self) -> bool {
        if self.selection_history_version == self.doc.version
            && let Some(previous) = self.selection_history.pop()
        {
            self.anchor = previous.0;
            self.caret = previous.1;
            self.ensure_visible = true;
            self.desired_col = None;
            return true;
        }
        let current = self.selection().unwrap_or((self.caret, self.caret));
        if current.0 == current.1 {
            return false;
        }
        let word = self.word_at(current.0);
        if word.0 < word.1 && word.0 >= current.0 && word.1 <= current.1 && word != current {
            self.anchor = word.0;
            self.caret = word.1;
            self.ensure_visible = true;
            self.desired_col = None;
            return true;
        }
        false
    }

    /// Selections containing `sel`, smallest first. Ranges nest, so length order is
    /// dependency order; build it only when the user presses expand/shrink.
    fn selection_candidates(&self, sel: (usize, usize)) -> Vec<(usize, usize)> {
        let len = self.doc.len_chars();
        let (start, end) = sel;
        let mut candidates: Vec<(usize, usize)> = Vec::new();
        if start == end {
            let word = self.word_at(start.min(len.saturating_sub(1)));
            if word.0 < word.1 {
                candidates.push(word);
            }
        }
        for (open, close) in self.enclosing_bracket_pairs(start, end) {
            if open + 1 < close {
                candidates.push((open + 1, close));
            }
            candidates.push((open, (close + 1).min(len)));
        }
        let first = self.doc.char_to_line(start);
        let last = self.doc.char_to_line(end);
        let line_start = self.doc.line_to_char(first);
        let line_end = self.doc.line_to_char(last) + self.doc.line_char_len(last);
        candidates.push((line_start, line_end));
        if line_end < len {
            candidates.push((line_start, line_end + 1));
        }
        candidates.push((0, len));
        candidates
            .retain(|candidate| candidate.0 <= start && candidate.1 >= end && candidate.0 < candidate.1);
        candidates.sort_unstable_by_key(|candidate| (candidate.1 - candidate.0, candidate.0));
        candidates.dedup();
        candidates
    }

    /// Enclosing bracket pairs, innermost first. One bounded left scan collects the
    /// ancestors, then each mate is found forward.
    fn enclosing_bracket_pairs(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let len = self.doc.len_chars();
        let mut ancestors: Vec<usize> = Vec::new();
        let mut depth = 0i32;
        let mut p = start.min(len);
        let floor = start.saturating_sub(200_000);
        while p > floor {
            p -= 1;
            match self.doc.char_at(p) {
                Some(')' | ']' | '}') if !self.is_in_string_or_comment(p) => depth += 1,
                Some('(' | '[' | '{') if !self.is_in_string_or_comment(p) => {
                    if depth == 0 {
                        ancestors.push(p);
                    } else {
                        depth -= 1;
                    }
                }
                _ => {}
            }
        }
        let mut pairs = Vec::new();
        for open in ancestors {
            let Some(open_ch) = self.doc.char_at(open) else {
                break;
            };
            let close_ch = match open_ch {
                '(' => ')',
                '[' => ']',
                _ => '}',
            };
            let mut depth = 0i32;
            let mut mate = None;
            let mut q = open;
            while q + 1 < len {
                q += 1;
                match self.doc.char_at(q) {
                    Some(ch) if ch == open_ch && !self.is_in_string_or_comment(q) => depth += 1,
                    Some(ch) if ch == close_ch && !self.is_in_string_or_comment(q) => {
                        if depth == 0 {
                            mate = Some(q);
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            let Some(close) = mate else {
                break;
            };
            if close < end {
                break;
            }
            pairs.push((open, close));
        }
        pairs
    }

    // ---------------------------------------------------------------------
    // Line endings, encoding & cleanup
    // ---------------------------------------------------------------------

    /// Switch the buffer between LF and CRLF; the toggle takes effect on the next save.
    /// Read-only previews have no save path, so they report `None` instead.
    pub fn toggle_eol(&mut self) -> Option<Eol> {
        if self.doc.readonly {
            return None;
        }
        let next = match self.doc.eol {
            Eol::Lf => Eol::CrLf,
            Eol::CrLf => Eol::Lf,
        };
        self.doc.set_eol(next);
        Some(next)
    }

    /// Strip trailing spaces/tabs from every line as one undo step.
    pub fn trim_trailing_whitespace(&mut self) -> usize {
        if self.doc.readonly {
            return 0;
        }
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for line in 0..self.doc.line_count() {
            let content = self.doc.line_content(line);
            let trimmed = content.trim_end_matches([' ', '\t']);
            if trimmed.len() == content.len() {
                continue;
            }
            let line_start = self.doc.line_to_char(line);
            let end = line_start + content.chars().count();
            let start = line_start + trimmed.chars().count();
            edits.push((start, end, String::new()));
            if edits.len() >= 100_000 {
                break;
            }
        }
        let count = edits.len();
        if count == 0 {
            return 0;
        }
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let before = (self.caret, self.anchor);
        let syntax_edits = self.doc.edit_many(&edits, before, (self.caret, self.anchor));
        for sed in syntax_edits {
            self.syntax.apply_edit(sed);
        }
        let last_line = self.doc.line_count().saturating_sub(1);
        let line_len = self.doc.line_char_len(caret_line.min(last_line));
        self.caret = self.doc.line_col_to_char(caret_line, caret_col.min(line_len));
        self.anchor = self.caret;
        if let Some(last_undo) = self.doc.undo.last_mut() {
            last_undo.after = (self.caret, self.anchor);
        }
        self.after_edit();
        self.refresh_find_matches();
        count
    }

    /// Re-decode the file on disk with an explicitly chosen encoding.
    pub fn reopen_with_encoding(&mut self, encoding: Encoding) -> std::io::Result<()> {
        if self.doc.is_dirty() {
            return Err(std::io::Error::other("有未保存的修改，请先保存再重新解码"));
        }
        self.doc.reopen_with_encoding(encoding)?;
        self.extra_cursors.clear();
        self.caret = self.caret.min(self.doc.len_chars());
        self.anchor = self.anchor.min(self.doc.len_chars());
        self.syntax.reset(self.language);
        self.spans.clear();
        self.spans_version = 0;
        self.diagnostics.clear();
        self.folds.clear();
        self.folded_lines.clear();
        self.fold_rev += 1;
        self.syntax_due = Some(Instant::now());
        self.invalidate_layout();
        self.ensure_visible = true;
        self.refresh_find_matches();
        Ok(())
    }

    /// Save to a new path, converting to the requested encoding.
    pub fn save_as_with_encoding(
        &mut self,
        path: std::path::PathBuf,
        encoding: Encoding,
    ) -> SaveOutcome {
        let language = Language::from_path(&path);
        let outcome = self.doc.save_as_with_encoding(path, encoding);
        if matches!(outcome, SaveOutcome::Saved) {
            self.preview = false;
            self.diff_version = 0;
            self.set_language(language);
        }
        outcome
    }

    // ---------------------------------------------------------------------
    // In-editor drag & drop of the selected text
    // ---------------------------------------------------------------------

    /// Move the selected text to `target` as one undo step (plain drag).
    pub fn move_selection_to(&mut self, target: usize) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        if self.doc.readonly {
            return false;
        }
        let target = target.min(self.doc.len_chars());
        if target >= start && target <= end {
            return false;
        }
        let selected = self.doc.text_range(start, end);
        let insert_len = selected.chars().count();
        let (edit_start, edit_end, insert, caret_after) = if target < start {
            let between = self.doc.text_range(target, start);
            (target, end, format!("{selected}{between}"), target + insert_len)
        } else {
            let between = self.doc.text_range(end, target);
            (start, target, format!("{between}{selected}"), target)
        };
        let anchor_after = if target < start { target } else { target - insert_len };
        let before = (self.caret, self.anchor);
        let sed = self.doc.edit(
            edit_start,
            edit_end,
            &insert,
            EditKind::Other,
            before,
            (caret_after, anchor_after),
        );
        self.syntax.apply_edit(sed);
        self.caret = caret_after.min(self.doc.len_chars());
        self.anchor = anchor_after.min(self.doc.len_chars());
        self.after_edit();
        self.refresh_find_matches();
        true
    }

    /// Copy the selected text to `target` as one undo step (Ctrl+drag).
    pub fn duplicate_selection_at(&mut self, target: usize) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        if self.doc.readonly {
            return false;
        }
        let selected = self.doc.text_range(start, end);
        let insert_len = selected.chars().count();
        let target = target.min(self.doc.len_chars());
        let before = (self.caret, self.anchor);
        let sed = self.doc.edit(
            target,
            target,
            &selected,
            EditKind::Other,
            before,
            (target + insert_len, target),
        );
        self.syntax.apply_edit(sed);
        self.caret = target + insert_len;
        self.anchor = target;
        self.after_edit();
        self.refresh_find_matches();
        true
    }

    // ---------------------------------------------------------------------
    // Find history & replacement expansion
    // ---------------------------------------------------------------------

    /// Record the current query for the ↑/↓ history walk.
    pub fn remember_find_query(&mut self) {
        let query = self.find.query.trim().to_string();
        if query.is_empty() || self.find.history.last() == Some(&query) {
            return;
        }
        self.find.history.push(query);
        if self.find.history.len() > 50 {
            self.find.history.remove(0);
        }
        self.find.history_pos = None;
    }

    /// Walk recorded queries; `newer` moves towards the most recent entry.
    pub fn find_history_step(&mut self, newer: bool) -> bool {
        if self.find.history.is_empty() {
            return false;
        }
        let next = match self.find.history_pos {
            Some(pos) if newer => (pos + 1).min(self.find.history.len() - 1),
            Some(0) if !newer => return false,
            Some(pos) => pos - 1,
            None if !newer => self.find.history.len() - 1,
            None => return false,
        };
        self.find.history_pos = Some(next);
        self.find.query = self.find.history[next].clone();
        self.recompute_find();
        true
    }

    /// Replacement text for one match; `$1`/`${1}` expand only in regex mode.
    fn replacement_for(&self, start: usize, end: usize) -> String {
        if !self.find.regex {
            return self.find.replacement.clone();
        }
        let Ok(regex) = self.build_find_regex() else {
            return self.find.replacement.clone();
        };
        let text = self.doc.text();
        let byte_start = self.doc.char_to_byte(start);
        let byte_end = self.doc.char_to_byte(end);
        let Some(captures) = regex.captures_at(&text, byte_start) else {
            return self.find.replacement.clone();
        };
        let Some(whole) = captures.get(0) else {
            return self.find.replacement.clone();
        };
        if whole.start() != byte_start || whole.end() != byte_end {
            return self.find.replacement.clone();
        }
        let mut expanded = String::new();
        captures.expand(&self.find.replacement, &mut expanded);
        expanded
    }

    // ---------------------------------------------------------------------
    // Syntax & Diff refresh
    // ---------------------------------------------------------------------

    /// Pick up finished background highlights, symbols, folds, diagnostics, and Git gutter diffs;
    /// schedule a new worker job when due.
    pub fn refresh_syntax(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while let Some(analysis) = self.syntax.poll_analysis() {
            if analysis.version >= self.spans_version {
                self.spans = analysis.spans;
                self.symbols = analysis.symbols;
                self.folds = analysis.folds;
                self.fold_rev += 1;
                self.diagnostics = analysis.diagnostics;
                self.spans_version = analysis.version;
                if let Some(diffs) = analysis.line_diffs {
                    self.line_diffs = diffs;
                    self.diff_version = analysis.version;
                }
                self.invalidate_layout();
                changed = true;
            }
        }
        let needs_diff = self.diff_version != self.doc.version
            && self.doc.baseline_text.is_some()
            && self.doc.len_bytes() <= 512 * 1024;
        if self.diff_version != self.doc.version && !needs_diff {
            self.line_diffs.clear();
            self.diff_version = self.doc.version;
        }
        let needs_syntax = self.spans_version != self.doc.version;
        if !needs_syntax && !needs_diff {
            return changed;
        }
        if self.doc.len_bytes() > MAX_HIGHLIGHT_BYTES {
            if !self.spans.is_empty() {
                self.spans.clear();
                self.symbols.clear();
                self.folds.clear();
                self.folded_lines.clear();
                self.fold_rev += 1;
                self.diagnostics.clear();
            }
            self.line_diffs.clear();
            self.spans_version = self.doc.version;
            self.diff_version = self.doc.version;
            return true;
        }
        if self.syntax.is_busy() {
            return changed;
        }
        match self.syntax_due {
            Some(due) if now < due => return changed,
            _ => {}
        }
        let version = self.doc.version;
        let rope = self.doc.rope.clone();
        let baseline = needs_diff
            .then(|| self.doc.baseline_text.clone())
            .flatten();
        self.syntax.request_rope(version, rope, baseline);
        self.syntax_due = None;
        changed
    }
}

fn return_quote_pair(ch: char) -> &'static str {
    match ch {
        '"' => "\"\"",
        '\'' => "''",
        '`' => "``",
        _ => "",
    }
}

/// Message from the editor to the app that needs app-level state.
pub enum EditorEvent {
    Zoom(i32),
    GotoDefinition(String),
    OpenPalette,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        Editor::new(Document::from_text(text), 1, 0)
    }

    #[test]
    fn plugin_edit_is_a_single_undo_step() {
        let mut ed = editor("hello world");
        ed.caret = 11;
        ed.anchor = 11;
        assert!(ed.apply_plugin_edit(0, 5, "HELLO", None));
        assert_eq!(ed.doc.text(), "HELLO world");
        assert_eq!(ed.caret, 11);
        ed.undo();
        assert_eq!(ed.doc.text(), "hello world");
    }

    #[test]
    fn plugin_whole_document_edit_keeps_caret_position() {
        let mut ed = editor("one
two
");
        ed.caret = 5;
        ed.anchor = 5;
        assert!(ed.apply_plugin_edit(
            0,
            ed.doc.len_chars(),
            "aa
bb
cc
",
            Some(ed.caret)
        ));
        assert_eq!(ed.doc.text(), "aa
bb
cc
");
        assert_eq!(ed.caret, 5);
    }

    #[test]
    fn plugin_edit_refuses_readonly_documents() {
        let mut ed = editor("locked");
        ed.doc.readonly = true;
        assert!(!ed.apply_plugin_edit(0, 6, "open", None));
        assert_eq!(ed.doc.text(), "locked");
    }

    #[test]
    fn typing_and_undo() {
        let mut ed = editor("");
        ed.insert_text("let x = 1;");
        assert_eq!(ed.doc.text(), "let x = 1;");
        assert_eq!(ed.caret, 10);
        ed.undo();
        assert_eq!(ed.doc.text(), "");
        assert_eq!(ed.caret, 0);
        ed.redo();
        assert_eq!(ed.doc.text(), "let x = 1;");
    }

    #[test]
    fn auto_pairs_and_skip_over() {
        let mut ed = editor("");
        ed.insert_text("(");
        assert_eq!(ed.doc.text(), "()");
        assert_eq!(ed.caret, 1);
        ed.insert_text(")");
        assert_eq!(ed.doc.text(), "()");
        assert_eq!(ed.caret, 2);
        ed.backspace();
        assert_eq!(ed.doc.text(), "(");
        assert_eq!(ed.caret, 1);
        ed.backspace();
        assert_eq!(ed.doc.text(), "");
    }

    #[test]
    fn auto_close_toggle_disables_pairing() {
        let mut ed = editor("");
        assert!(ed.auto_close);
        ed.insert_text("(");
        assert_eq!(ed.doc.text(), "()");
        ed.auto_close = false;
        // With pairing off the closing char is inserted instead of skipping over.
        ed.insert_text(")");
        assert_eq!(ed.doc.text(), "())");
        // Backspace deletes one char, not the whole pair.
        ed.backspace();
        assert_eq!(ed.doc.text(), "()");
        ed.insert_text("]");
        assert_eq!(ed.doc.text(), "(])");
    }

    #[test]
    fn bracket_matching_and_jump() {
        let mut ed = editor("fn main() { let x = (1 + 2); }");
        let text = ed.doc.text();
        let open = ed.doc.byte_to_char(text.find('{').unwrap());
        let close = ed.doc.byte_to_char(text.rfind('}').unwrap());
        ed.caret = open + 1;
        ed.anchor = ed.caret;
        assert_eq!(ed.matching_brackets(), Some((open, close)));
        assert!(ed.jump_to_matching_bracket());
        assert_eq!(ed.caret, close);
        assert!(ed.jump_to_matching_bracket());
        assert_eq!(ed.caret, open);
        // Unbalanced input must not claim a pair.
        let mut ed = editor("fn broken( {");
        ed.caret = 4;
        ed.anchor = 4;
        assert_eq!(ed.matching_brackets(), None);
    }

    #[test]
    fn selection_expands_and_shrinks() {
        let mut ed = editor("fn run() {\n    let value = compute(arg);\n}");
        let caret = ed.doc.byte_to_char(ed.doc.text().find("arg").unwrap()) + 1;
        ed.caret = caret;
        ed.anchor = caret;
        assert!(ed.expand_selection());
        assert_eq!(ed.selected_text(), "arg");
        assert!(ed.expand_selection());
        assert_eq!(ed.selected_text(), "(arg)");
        assert!(ed.expand_selection());
        assert_eq!(ed.selected_text(), "    let value = compute(arg);");
        assert!(ed.shrink_selection());
        assert_eq!(ed.selected_text(), "(arg)");
        assert!(ed.shrink_selection());
        assert_eq!(ed.selected_text(), "arg");
    }

    #[test]
    fn regex_replace_expands_capture_groups() {
        let mut ed = editor("key = value; other = thing;");
        ed.open_find(true);
        ed.find.regex = true;
        ed.find.query = r"(\w+) = (\w+)".to_string();
        ed.find.replacement = "$2 = $1".to_string();
        ed.recompute_find();
        assert_eq!(ed.find.matches.len(), 2);
        assert_eq!(ed.replace_all(), 2);
        assert_eq!(ed.doc.text(), "value = key; thing = other;");

        // The single-match path expands the same groups.
        let mut ed = editor("a1 b2");
        ed.open_find(true);
        ed.find.regex = true;
        ed.find.query = r"(\w)(\d)".to_string();
        ed.find.replacement = "$2$1".to_string();
        ed.recompute_find();
        ed.find.active = Some(0);
        ed.replace_current();
        assert_eq!(ed.doc.text(), "1a b2");
    }

    #[test]
    fn find_in_selection_limits_matches() {
        let mut ed = editor("foo bar foo");
        ed.anchor = 4;
        ed.caret = 7;
        ed.open_find(false);
        ed.find.in_selection = true;
        ed.find.scope = ed.selection();
        ed.find.query = "foo".to_string();
        ed.recompute_find();
        assert!(ed.find.matches.is_empty());
        ed.find.in_selection = false;
        ed.find.scope = None;
        ed.recompute_find();
        assert_eq!(ed.find.matches.len(), 2);
    }

    #[test]
    fn find_history_walks_recent_queries() {
        let mut ed = editor("foo bar foo");
        ed.open_find(false);
        ed.find.query = "foo".to_string();
        ed.remember_find_query();
        ed.find.query = "bar".to_string();
        ed.remember_find_query();
        assert!(ed.find_history_step(false));
        assert_eq!(ed.find.query, "bar");
        assert!(ed.find_history_step(false));
        assert_eq!(ed.find.query, "foo");
        assert!(!ed.find_history_step(false));
        assert!(ed.find_history_step(true));
        assert_eq!(ed.find.query, "bar");
    }

    #[test]
    fn invalid_regex_reports_error_instead_of_matches() {
        let mut ed = editor("abc");
        ed.open_find(false);
        ed.find.regex = true;
        ed.find.query = "(".to_string();
        ed.recompute_find();
        assert!(ed.find.error.is_some());
        assert!(ed.find.matches.is_empty());
    }

    #[test]
    fn trim_trailing_whitespace_and_eol_toggle() {
        let mut ed = editor("let a = 1;   \nlet b = 2;\t\n");
        assert_eq!(ed.trim_trailing_whitespace(), 2);
        assert_eq!(ed.doc.text(), "let a = 1;\nlet b = 2;\n");
        assert_eq!(ed.toggle_eol(), Some(Eol::CrLf));
        assert_eq!(ed.toggle_eol(), Some(Eol::Lf));
    }

    #[test]
    fn drag_moves_and_copies_selection() {
        let mut ed = editor("alpha beta");
        ed.anchor = 0;
        ed.caret = 5;
        assert!(ed.move_selection_to(10));
        assert_eq!(ed.doc.text(), " betaalpha");

        let mut ed = editor("one two");
        ed.anchor = 0;
        ed.caret = 3;
        assert!(ed.duplicate_selection_at(7));
        assert_eq!(ed.doc.text(), "one twoone");
    }

    #[test]
    fn newline_keeps_indent_and_opens_blocks() {
        let mut ed = editor("fn main() {\n    let x = 1;\n}");
        let pos = ed.doc.line_to_char(1);
        ed.caret = pos;
        ed.anchor = pos;
        ed.newline();
        let text = ed.doc.text();
        assert_eq!(text, "fn main() {\n\n    let x = 1;\n}");
    }

    #[test]
    fn selection_word_and_replace() {
        let mut ed = editor("hello world");
        ed.select_word_at(7);
        assert_eq!(ed.selected_text(), "world");
        ed.insert_text("rust");
        assert_eq!(ed.doc.text(), "hello rust");
        assert_eq!(ed.caret, 10);
    }

    #[test]
    fn multi_cursor_select_and_edit() {
        let mut ed = editor("foo = 1;\nfoo = 2;\nfoo = 3;");
        ed.caret = 1;
        ed.anchor = 1;
        ed.select_next_occurrence(); // selects first "foo"
        assert_eq!(ed.selected_text(), "foo");
        ed.select_next_occurrence(); // adds second "foo"
        assert_eq!(ed.extra_cursors.len(), 1);
        ed.insert_text("bar");
        assert_eq!(ed.doc.text(), "bar = 1;\nbar = 2;\nfoo = 3;");
    }

    #[test]
    fn code_folding_hides_inner_lines() {
        let mut ed = editor("fn a() {\n    let x = 1;\n}\nfn b() {}");
        ed.folds = vec![(0, 2)];
        assert!(!ed.is_line_hidden(1));
        ed.toggle_fold_at(0);
        assert!(ed.is_line_hidden(1));
        assert!(ed.is_line_hidden(2));
        assert!(!ed.is_line_hidden(3));
    }

    #[test]
    fn indent_and_outdent_selection() {
        let mut ed = editor("a\nb\nc");
        ed.select_all();
        ed.indent_selection(false);
        assert_eq!(ed.doc.text(), "    a\n    b\n    c");
        ed.select_all();
        ed.indent_selection(true);
        assert_eq!(ed.doc.text(), "a\nb\nc");
    }

    #[test]
    fn toggle_comment_lines() {
        let mut ed = editor("let a = 1;\nlet b = 2;");
        ed.select_all();
        ed.toggle_comment();
        assert_eq!(ed.doc.text(), "// let a = 1;\n// let b = 2;");
        ed.toggle_comment();
        assert_eq!(ed.doc.text(), "let a = 1;\nlet b = 2;");
    }

    #[test]
    fn duplicate_move_delete_line() {
        let mut ed = editor("one\ntwo\nthree");
        ed.caret = ed.doc.line_col_to_char(1, 0);
        ed.anchor = ed.caret;
        ed.duplicate_line();
        assert_eq!(ed.doc.text(), "one\ntwo\ntwo\nthree");
        ed.caret = ed.doc.line_col_to_char(1, 0);
        ed.anchor = ed.caret;
        ed.move_lines(true);
        assert_eq!(ed.doc.text(), "two\none\ntwo\nthree");
        ed.delete_line();
        assert_eq!(ed.doc.text(), "one\ntwo\nthree");
    }

    #[test]
    fn find_matches_and_replace_all() {
        let mut ed = editor("let value = 1;\nlet value2 = value;");
        ed.open_find(false);
        ed.find.query = "value".to_string();
        ed.recompute_find();
        assert_eq!(ed.find.matches.len(), 3);
        ed.find.replacement = "v".to_string();
        let replaced = ed.replace_all();
        assert_eq!(replaced, 3);
        assert_eq!(ed.doc.text(), "let v = 1;\nlet v2 = v;");
    }

    #[test]
    fn whole_word_and_regex_find() {
        let mut ed = editor("cat category cat");
        ed.open_find(false);
        ed.find.query = "cat".to_string();
        ed.find.whole_word = true;
        ed.recompute_find();
        assert_eq!(ed.find.matches.len(), 2);
        ed.find.whole_word = false;
        ed.find.regex = true;
        ed.find.query = r"cat\w*".to_string();
        ed.recompute_find();
        assert_eq!(ed.find.matches.len(), 3);
    }

    #[test]
    fn vertical_movement_keeps_column() {
        let mut ed = editor("abcdef\nab\nabcdef");
        ed.caret = 4;
        ed.anchor = 4;
        ed.move_vertical(1, false);
        assert_eq!(ed.doc.char_to_line_col(ed.caret), (1, 2));
        ed.move_vertical(1, false);
        assert_eq!(ed.doc.char_to_line_col(ed.caret), (2, 4));
    }

    #[test]
    fn phase1_editor_core_bug_regressions() {
        // 1. Readonly guards prevent mutation on replace_selection / replace_current / undo.
        let mut ro = editor("hello world");
        ro.doc.readonly = true;
        ro.select_all();
        assert!(!ro.replace_selection("x", EditKind::Other, None));
        assert_eq!(ro.doc.text(), "hello world");

        // 2. newline() before closing paren without opening block does not jump past ')'.
        let mut nl = editor("foo(bar)");
        nl.caret = 7; // before ')'
        nl.anchor = 7;
        nl.newline();
        assert_eq!(nl.doc.text(), "foo(bar\n)");
        assert_eq!(nl.doc.char_at(nl.caret), Some(')'));

        // 3. toggle_comment closes HTML and CSS comments properly.
        let mut html = editor("<div>hi</div>");
        html.language = Language::Html;
        html.toggle_comment();
        assert_eq!(html.doc.text(), "<!-- <div>hi</div> -->");
        html.toggle_comment();
        assert_eq!(html.doc.text(), "<div>hi</div>");

        let mut css = editor("color: red;");
        css.language = Language::Css;
        css.toggle_comment();
        assert_eq!(css.doc.text(), "/* color: red; */");
        css.toggle_comment();
        assert_eq!(css.doc.text(), "color: red;");

        // 4. Multi-cursor delete_forward deletes at every cursor.
        let mut mc = editor("ab\ncd");
        mc.caret = 0;
        mc.anchor = 0;
        mc.extra_cursors.push((3, 3));
        mc.delete_forward();
        assert_eq!(mc.doc.text(), "b\nd");
    }

    #[test]
    fn phase1_security_and_doc_safety() {
        assert!(crate::app::is_safe_external_url("https://example.com/path?a=1"));
        assert!(crate::app::is_safe_external_url("mailto:user@example.com"));
        assert!(!crate::app::is_safe_external_url("https://example.com & calc.exe"));
        assert!(!crate::app::is_safe_external_url("javascript:alert(1)"));
        assert!(!crate::app::is_safe_external_url("file:///C:/Windows/cmd.exe"));

        // Toggling EOL twice clears dirty flag instead of leaving inverted hash.
        let mut doc = Document::from_text("a\nb\n");
        assert!(!doc.is_dirty());
        doc.set_eol(Eol::CrLf);
        assert!(doc.is_dirty());
        doc.set_eol(Eol::Lf);
        assert!(!doc.is_dirty());

        // Git baseline survives Document::save().
        let tmp = std::env::temp_dir().join(format!(
            "kestrel_git_baseline_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "line1\n").unwrap();
        let mut gdoc = Document::load(&tmp).unwrap();
        gdoc.set_git_baseline("head_version\n");
        let _ = gdoc.edit(0, 5, "edited", EditKind::Other, (0, 0), (6, 6));
        assert!(matches!(gdoc.save(), SaveOutcome::Saved));
        assert_eq!(gdoc.baseline_text.as_deref(), Some("head_version\n"));
        let _ = std::fs::remove_file(&tmp);

        // Non-UTF-8 / Lossy file is opened readonly to prevent silent corruption.
        let bad = std::env::temp_dir().join(format!(
            "kestrel_lossy_ro_{}.txt",
            std::process::id()
        ));
        std::fs::write(&bad, b"hello \xC4\xE3\xBA\xC3").unwrap();
        let ldoc = Document::load(&bad).unwrap();
        assert_eq!(ldoc.encoding, Encoding::LossyUtf8);
        assert!(ldoc.readonly);
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn folding_compresses_rows_and_total_height() {
        let mut ed = editor("fn a() {\n    1\n    2\n    3\n}\nfn b() {}");
        ed.folds = vec![(0, 4)];
        assert_eq!(ed.visible_row_count(), 6);
        assert!(ed.toggle_fold_at(0));
        assert_eq!(ed.visible_row_count(), 2);
        assert_eq!(ed.total_content_height(20.0), 40.0);
        assert_eq!(ed.row_of_line(0), Some(0));
        assert_eq!(ed.row_of_line(1), None);
        assert_eq!(ed.row_of_line(5), Some(1));
        assert_eq!(ed.line_of_row(1), 5);
    }

    #[test]
    fn folding_moves_caret_to_the_fold_start_line() {
        let mut ed = editor("a\nb\nc\nd");
        ed.folds = vec![(1, 3)];
        ed.caret = ed.doc.line_col_to_char(2, 0);
        ed.anchor = ed.caret;
        assert!(ed.toggle_fold_at(1));
        assert_eq!(ed.doc.char_to_line(ed.caret), 1);
        assert!(ed.is_line_hidden(2));
    }

    #[test]
    fn hidden_lookup_handles_nested_folds() {
        let mut ed = editor("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11");
        ed.folds = vec![(0, 10), (2, 4)];
        ed.toggle_fold_at(0);
        ed.toggle_fold_at(2);
        let hidden: Vec<bool> = (0..12).map(|line| ed.is_line_hidden(line)).collect();
        // Line 2 is the inner fold start but the outer block hides it; line 7 is past
        // the inner block and must still be hidden by the outer one.
        assert_eq!(
            hidden,
            [
                false, true, true, true, true, true, true, true, true, true, true, false
            ]
        );
    }

    #[test]
    fn fold_layout_tracks_document_version() {
        let mut ed = editor("a\nb\nc\nd");
        ed.folds = vec![(0, 3)];
        ed.toggle_fold_at(0);
        assert_eq!(ed.visible_row_count(), 1);
        assert_eq!(ed.total_content_height(10.0), 10.0);
        // A new edit bumps doc.version and must rebuild the cached row map.
        ed.doc
            .edit(0, 0, "x\n", EditKind::Other, (0, 0), (0, 0));
        assert_eq!(ed.visible_row_count(), 2);
        assert_eq!(ed.line_of_row(1), 4);
    }

    #[test]
    fn newline_and_backspace_follow_tab_size() {
        let mut ed = editor("fn main() {");
        ed.tab_size = 2;
        ed.caret = ed.doc.len_chars();
        ed.anchor = ed.caret;
        ed.newline();
        assert_eq!(ed.doc.text(), "fn main() {\n  ");

        let mut ed = editor("x\n    ");
        ed.tab_size = 2;
        ed.caret = ed.doc.len_chars();
        ed.anchor = ed.caret;
        ed.backspace();
        assert_eq!(ed.doc.text(), "x\n  ");
    }

    #[test]
    fn multi_cursor_paste_distributes_lines() {
        let mut ed = editor("a\nb");
        ed.caret = ed.doc.line_col_to_char(1, 1);
        ed.anchor = ed.caret;
        ed.extra_cursors
            .push((ed.doc.line_col_to_char(0, 1), ed.doc.line_col_to_char(0, 1)));
        ed.paste_text("X\nY");
        assert_eq!(ed.doc.text(), "aX\nbY");
    }

    fn function_symbol(name: &str) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_string(),
            detail: "fn".to_string(),
            kind: crate::lsp::SymbolKind::Function,
            start_line: 0,
            end_line: 0,
            byte_offset: 0,
            depth: 0,
        }
    }

    #[test]
    fn continuous_typing_is_a_single_undo_step() {
        let mut ed = editor("");
        for ch in "abc".chars() {
            ed.insert_text(&ch.to_string());
        }
        ed.undo();
        assert_eq!(ed.doc.text(), "");
        assert_eq!(ed.caret, 0);
        ed.redo();
        assert_eq!(ed.doc.text(), "abc");
    }

    #[test]
    fn dot_and_double_colon_open_member_completion() {
        let mut ed = editor("");
        ed.symbols = vec![function_symbol("render_view"), function_symbol("reload")];
        ed.insert_text(".");
        assert!(ed.completion.open);
        assert!(
            ed.completion
                .items
                .iter()
                .any(|item| item.label == "render_view")
        );
        ed.insert_text("r");
        ed.insert_text("e");
        assert!(ed.completion.open);
        let selected = ed
            .completion
            .items
            .iter()
            .position(|item| item.label == "render_view")
            .expect("render_view candidate");
        ed.completion.selected = selected;
        assert!(ed.accept_completion());
        assert_eq!(ed.doc.text(), ".render_view");

        let mut ed = editor("");
        ed.symbols = vec![function_symbol("render_view")];
        ed.insert_text(":");
        assert!(!ed.completion.open);
        ed.insert_text(":");
        assert!(ed.completion.open);
        assert_eq!(ed.completion.prefix_start, 2);
    }

    #[test]
    fn member_completion_rows_match_the_popup_cap() {
        let mut ed = editor("");
        ed.symbols = (0..20)
            .map(|index| function_symbol(&format!("symbol_{index}")))
            .collect();
        ed.insert_text(".");
        assert_eq!(ed.completion.items.len(), crate::lsp::COMPLETION_MAX_ITEMS);
    }

    #[test]
    fn completion_context_stays_bounded() {
        let body = "let identifier = 1;\n".repeat(80_000);
        let mut ed = editor(&body);
        ed.caret = ed.doc.len_chars();
        ed.anchor = ed.caret;
        let context = ed.completion_context();
        assert!(
            context.len() <= COMPLETION_CONTEXT_BYTES + 1024,
            "context grew to {} bytes",
            context.len()
        );
        assert!(body.ends_with(&context));
    }

    #[test]
    fn format_reports_builtin_cleanup_without_formatter() {
        let mut ed = editor("let x = 1;  \nlet y = 2;\t\n");
        match ed.format_document() {
            FormatOutcome::Formatted { external } => assert!(!external),
            _ => panic!("expected a change"),
        }
        assert_eq!(ed.doc.text(), "let x = 1;\nlet y = 2;\n");
        match ed.format_document() {
            FormatOutcome::Unchanged { external } => assert!(!external),
            _ => panic!("expected no change"),
        }
    }

    #[test]
    fn format_reports_read_only() {
        let mut ed = editor("x\n");
        ed.doc.readonly = true;
        assert!(matches!(ed.format_document(), FormatOutcome::ReadOnly));
    }
}
