//! One editor tab: text buffer, multi-cursor/selection model, code folding,
//! completion, diagnostics, line diffs, and find-in-file.

pub mod input;
pub mod syntax;
pub mod view;

use std::collections::HashSet;
use std::time::Instant;

use egui::FontFamily;

use crate::doc::{Document, EditKind};
use crate::git::{LineDiffKind, compute_line_diff};
use crate::highlight::{Language, Span};
use crate::lsp::{CompletionItem, Diagnostic, OutlineSymbol};
use crate::theme::Theme;
use syntax::SyntaxService;

/// Documents larger than this are rendered without syntax highlighting.
pub const MAX_HIGHLIGHT_BYTES: usize = 1_500_000;

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
    /// Synced from settings on every paint.
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub autocomplete_enabled: bool,

    // Layout cache (invalidated on version/style change).
    galleys: std::collections::HashMap<usize, CachedLine>,
    cache_key: (u64, u64, f32, usize),
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
    pub multimodal: Option<crate::viewer::MultimodalViewer>,
}

struct CachedLine {
    galley: std::sync::Arc<egui::Galley>,
    /// `vis_to_doc[visual_col] = doc_col`; present only when the line has tabs.
    vis_to_doc: Option<Vec<u32>>,
    text_width: f32,
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
            autocomplete_enabled: true,
            galleys: std::collections::HashMap::new(),
            cache_key: (0, 0, 0.0, 0),
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
            self.diagnostics.clear();
            self.spans_version = 0;
            self.invalidate_layout();
            self.syntax_due = Some(Instant::now());
        }
    }

    pub fn invalidate_layout(&mut self) {
        self.galleys.clear();
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

    pub fn fold_range_for_line(&self, line: usize) -> Option<(usize, usize)> {
        self.folds.iter().find(|(s, _)| *s == line).copied()
    }

    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        if self.fold_range_for_line(line).is_some() {
            if !self.folded_lines.remove(&line) {
                self.folded_lines.insert(line);
            }
            return true;
        }
        false
    }

    pub fn toggle_fold_caret(&mut self) {
        let caret_line = self.doc.char_to_line(self.caret);
        if self.toggle_fold_at(caret_line) {
            return;
        }
        // Find innermost enclosing foldable range.
        if let Some((start, _)) = self
            .folds
            .iter()
            .filter(|(s, e)| *s <= caret_line && caret_line <= *e)
            .max_by_key(|(s, _)| *s)
            .copied()
        {
            self.toggle_fold_at(start);
        }
    }

    pub fn fold_all(&mut self) {
        for (start, _) in &self.folds {
            self.folded_lines.insert(*start);
        }
    }

    pub fn unfold_all(&mut self) {
        self.folded_lines.clear();
    }

    pub fn is_line_hidden(&self, line: usize) -> bool {
        for &folded_start in &self.folded_lines {
            if let Some((s, e)) = self.fold_range_for_line(folded_start)
                && line > s
                && line <= e
            {
                return true;
            }
        }
        false
    }

    // ---------------------------------------------------------------------
    // Autocomplete & Formatting
    // ---------------------------------------------------------------------

    pub fn update_completion(&mut self) {
        if !self.autocomplete_enabled || !self.extra_cursors.is_empty() {
            self.completion.open = false;
            return;
        }
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = self.caret;
        while start > 0 {
            match self.doc.char_at(start - 1) {
                Some(c) if is_ident(c) => start -= 1,
                _ => break,
            }
        }
        let prefix = self.doc.text_range(start, self.caret);
        if prefix.chars().count() < 2
            || prefix.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            self.completion.open = false;
            return;
        }
        let text = self.doc.text();
        let items = crate::lsp::complete(&prefix, self.language, &self.symbols, &text);
        if items.is_empty() {
            self.completion.open = false;
        } else {
            self.completion.prefix_start = start;
            self.completion.items = items;
            self.completion.selected = 0;
            self.completion.open = true;
        }
    }

    pub fn trigger_completion(&mut self) {
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = self.caret;
        while start > 0 {
            match self.doc.char_at(start - 1) {
                Some(c) if is_ident(c) => start -= 1,
                _ => break,
            }
        }
        let prefix = self.doc.text_range(start, self.caret);
        let query = if prefix.is_empty() { "a" } else { &prefix };
        let text = self.doc.text();
        let items = crate::lsp::complete(query, self.language, &self.symbols, &text);
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
        self.caret = after_pos;
        self.anchor = after_pos;
        self.after_edit();
        true
    }

    pub fn format_document(&mut self) {
        if self.doc.readonly {
            return;
        }
        let current = self.doc.text();
        let formatted =
            crate::lsp::format_document(self.language, &current, self.tab_size, self.insert_spaces);
        if formatted == current {
            return;
        }
        let (line, col) = self.doc.char_to_line_col(self.caret);
        let before = (self.caret, self.anchor);
        let len = self.doc.len_chars();
        let sed = self
            .doc
            .edit(0, len, &formatted, EditKind::Other, before, before);
        self.syntax.apply_edit(sed);
        self.caret = self.doc.line_col_to_char(line, col);
        self.anchor = self.caret;
        self.after_edit();
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
        self.desired_col = Some(clamped_col);
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
        self.caret = caret_after;
        self.anchor = caret_after;
        self.after_edit();
        true
    }

    fn replace_multi_cursors(&mut self, text: &str) -> bool {
        let mut all_cursors = Vec::with_capacity(self.extra_cursors.len() + 1);
        all_cursors.push((self.caret, self.anchor, true));
        for &(c, a) in &self.extra_cursors {
            all_cursors.push((c, a, false));
        }
        all_cursors.sort_by_key(|(c, a, _)| (*c).min(*a));

        let edits: Vec<(usize, usize, String)> = all_cursors
            .iter()
            .map(|(c, a, _)| ((*c).min(*a), (*c).max(*a), text.to_string()))
            .collect();
        let before = (self.caret, self.anchor);
        let seds = self.doc.edit_many(&edits, before, before);
        for sed in seds {
            self.syntax.apply_edit(sed);
        }

        let insert_chars = text.chars().count() as isize;
        let mut delta: isize = 0;
        let mut new_extras = Vec::with_capacity(self.extra_cursors.len());
        for (c, a, is_primary) in all_cursors {
            let start = c.min(a) as isize + delta;
            let removed = c.max(a).saturating_sub(c.min(a)) as isize;
            let new_pos = (start + insert_chars).max(0) as usize;
            delta += insert_chars - removed;
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

    fn after_edit(&mut self) {
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
        let open = matches!(ch, '(' | '[' | '{' | '"' | '\'' | '`');
        let close = matches!(ch, ')' | ']' | '}' | '"' | '\'' | '`');

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
        if ch.is_alphanumeric() || ch == '_' {
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

        let extra = if opens_block || closing_next {
            if self.doc.line_indent(line).contains('\t') {
                "\t".to_string()
            } else {
                " ".repeat(4)
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
        if let (Some(a), Some(b)) = (prev, next)
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
            let tab = 4usize;
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
        self.after_edit();
    }

    fn comment_prefix(&self) -> &'static str {
        match self.language {
            Language::Python | Language::Bash | Language::Yaml | Language::Toml => "# ",
            Language::Html | Language::Markdown => "<!-- ",
            _ => "// ",
        }
    }

    pub fn toggle_comment(&mut self) {
        if self.doc.readonly {
            return;
        }
        let (first, last) = self.selected_lines();
        let prefix = self.comment_prefix();
        let all_commented = (first..=last)
            .filter(|line| !self.doc.line_content(*line).trim().is_empty())
            .all(|line| {
                self.doc
                    .line_content(line)
                    .trim_start()
                    .starts_with(prefix.trim_end())
            });
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for line in first..=last {
            let content = self.doc.line_content(line);
            if content.trim().is_empty() {
                continue;
            }
            let indent_len = content.len() - content.trim_start().len();
            let start = self.doc.line_to_char(line) + content[..indent_len].chars().count();
            if all_commented {
                let rest = &content[indent_len..];
                let trimmed = rest.trim_start();
                if trimmed.starts_with(prefix.trim_end()) {
                    let remove_len = if trimmed.starts_with(prefix) {
                        prefix.len()
                    } else {
                        prefix.trim_end().len()
                    };
                    edits.push((
                        start,
                        start + trimmed[..remove_len].chars().count(),
                        String::new(),
                    ));
                }
            } else {
                edits.push((start, start, prefix.to_string()));
            }
        }
        if edits.is_empty() {
            return;
        }
        let before = (self.caret, self.anchor);
        let syntax_edits = self
            .doc
            .edit_many(&edits, before, (self.caret, self.anchor));
        for se in syntax_edits {
            self.syntax.apply_edit(se);
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
        let syntax_edit = self.doc.edit(
            end,
            end,
            &inserted,
            EditKind::Other,
            before,
            (self.caret, self.anchor),
        );
        self.syntax.apply_edit(syntax_edit);
        self.caret += inserted.chars().count();
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
        self.after_edit();
        self.ensure_visible = true;
    }

    pub fn undo(&mut self) {
        if let Some((caret, anchor)) = self.doc.undo() {
            self.restore_after_history(caret, anchor);
        }
    }

    pub fn redo(&mut self) {
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
        let replacement = self.find.replacement.clone();
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut first_start = None;
        for m in regex.find_iter(&text) {
            if m.start() == m.end() {
                continue;
            }
            let start = self.doc.byte_to_char(m.start());
            let end = self.doc.byte_to_char(m.end());
            first_start.get_or_insert(start);
            edits.push((start, end, replacement.clone()));
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
        self.find.open = false;
        self.find.focus_query = false;
        self.find.focus_replacement = false;
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
        let mut matches = Vec::new();
        for m in regex.find_iter(&text) {
            if matches.len() >= 50_000 {
                break;
            }
            matches.push((
                self.doc.byte_to_char(m.start()),
                self.doc.byte_to_char(m.end()),
            ));
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
        let Some(index) = self.find.active else {
            return;
        };
        let Some((start, end)) = self.find.matches.get(index).copied() else {
            return;
        };
        let replacement = self.find.replacement.clone();
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
    // Syntax & Diff refresh
    // ---------------------------------------------------------------------

    /// Pick up finished background highlights, symbols, folds, and diagnostics; schedule a new one when due.
    pub fn refresh_syntax(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while let Some(analysis) = self.syntax.poll_analysis() {
            if analysis.version >= self.spans_version {
                self.spans = analysis.spans;
                self.symbols = analysis.symbols;
                self.folds = analysis.folds;
                self.diagnostics = analysis.diagnostics;
                self.spans_version = analysis.version;
                self.invalidate_layout();
                changed = true;
            }
        }
        if self.diff_version != self.doc.version {
            self.diff_version = self.doc.version;
            if let Some(base) = &self.doc.baseline_text
                && self.doc.len_bytes() <= 512 * 1024
            {
                let cur = self.doc.text();
                self.line_diffs = compute_line_diff(base, &cur);
            } else {
                self.line_diffs.clear();
            }
        }
        if self.spans_version == self.doc.version {
            return changed;
        }
        if self.doc.len_bytes() > MAX_HIGHLIGHT_BYTES {
            if !self.spans.is_empty() {
                self.spans.clear();
                self.symbols.clear();
                self.folds.clear();
                self.diagnostics.clear();
            }
            self.spans_version = self.doc.version;
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
        self.syntax.request_rope(version, rope);
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
        assert_eq!(ed.doc.char_to_line_col(ed.caret), (2, 2));
    }
}
