//! Text buffer with undo/redo, line math, multi-encoding support, and disk persistence.
//!
//! Text is stored as a rope with `\n` line endings; the original line ending and
//! encoding are remembered and restored on save. All cursor/selection coordinates
//! in the app are char offsets into this rope.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use ropey::Rope;

use crate::highlight::SyntaxEdit;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    LossyUtf8,
    Binary,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::LossyUtf8 => "ANSI / GBK",
            Self::Binary => "Binary",
        }
    }
}

/// Result of writing a document back to its file.
#[derive(Debug)]
pub enum SaveOutcome {
    Saved,
    /// The file changed on disk since it was loaded/saved; nothing was written.
    DiskChanged,
    ReadOnly,
    Failed(io::Error),
}

/// Coalescing class for undo grouping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Typing,
    Backspace,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
pub struct EditRecord {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

#[derive(Clone, Debug)]
pub struct UndoGroup {
    pub edits: Vec<EditRecord>,
    pub before: (usize, usize),
    pub after: (usize, usize),
    pub kind: EditKind,
    pub time: Instant,
}

pub struct Document {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub eol: Eol,
    /// Line ending as of the last load/save; toggling EOL back clears dirty state.
    pub saved_eol: Eol,
    pub bom: bool,
    pub encoding: Encoding,
    pub version: u64,
    /// FNV-1a of the text at the last successful load/save; dirty means hash mismatch.
    pub saved_text_hash: u64,
    /// Document version as of the last load/save (O(1) clean fast-path).
    saved_version: u64,
    /// Undo stack depth at the last load/save (O(1) dirty check on >256 KB files).
    saved_undo_depth: Option<usize>,
    /// mtime of the file as of the last load/save, used to spot external edits.
    pub disk_modified_at: Option<SystemTime>,
    /// `(version, hash)` cache so `content_hash` only walks the rope after an edit.
    hash_cache: std::cell::Cell<(u64, u64)>,
    pub undo: Vec<UndoGroup>,
    pub redo: Vec<UndoGroup>,
    pub readonly: bool,
    /// Set when the file was not valid UTF-8/UTF-16 and got lossy-decoded.
    pub lossy: bool,
    /// Snapshot of saved/HEAD text (capped at 512 KB) used for gutter line-diff markers.
    /// Shared so a loaded/saved buffer can hand its text over instead of cloning it.
    pub baseline_text: Option<Arc<str>>,
    /// True when `baseline_text` came from Git `HEAD`, so saving to disk preserves gutter diffs.
    pub baseline_is_git: bool,
    /// Scratch buffers reused by `save`, so repeated autosaves do not allocate the
    /// whole document (and its CRLF/encoding copy) again every time.
    save_text_buf: String,
    save_bytes_buf: Vec<u8>,
}

const MAX_UNDO_GROUPS: usize = 800;
const COALESCE_WINDOW_MS: u128 = 600;
const MAX_BASELINE_BYTES: usize = 512 * 1024;
const FAST_DIRTY_THRESHOLD_BYTES: usize = 256 * 1024;

/// Append `text` with `\n` rewritten to `\r\n`, straight into `out`.
/// Saves the second full-size string that `str::replace` would allocate.
fn push_crlf(out: &mut Vec<u8>, text: &str) {
    let mut rest = text;
    while let Some(index) = rest.find('\n') {
        out.extend_from_slice(&rest.as_bytes()[..index]);
        out.extend_from_slice(b"\r\n");
        rest = &rest[index + 1..];
    }
    out.extend_from_slice(rest.as_bytes());
}

impl Document {
    pub fn from_text(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let baseline = if text.len() <= MAX_BASELINE_BYTES {
            Some(Arc::from(text))
        } else {
            None
        };
        let saved_text_hash = hash_bytes(text.as_bytes());
        Self {
            rope,
            path: None,
            eol: Eol::Lf,
            saved_eol: Eol::Lf,
            bom: false,
            encoding: Encoding::Utf8,
            version: 1,
            saved_text_hash,
            saved_version: 1,
            saved_undo_depth: Some(0),
            disk_modified_at: None,
            hash_cache: std::cell::Cell::new((1, saved_text_hash)),
            undo: Vec::new(),
            redo: Vec::new(),
            readonly: false,
            lossy: false,
            baseline_text: baseline,
            baseline_is_git: false,
            save_text_buf: String::new(),
            save_bytes_buf: Vec::new(),
        }
    }

    pub fn set_git_baseline(&mut self, text: impl Into<Arc<str>>) {
        let text: Arc<str> = text.into();
        if text.len() <= MAX_BASELINE_BYTES {
            self.baseline_text = Some(text);
            self.baseline_is_git = true;
        }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let modified_time = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());

        let (text, bom, encoding, lossy, readonly) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            let body = &bytes[3..];
            if is_binary_bytes(body) {
                (String::new(), false, Encoding::Binary, true, true)
            } else {
                match std::str::from_utf8(body) {
                    Ok(s) => (s.to_string(), true, Encoding::Utf8Bom, false, false),
                    Err(_) => (
                        String::from_utf8_lossy(body).into_owned(),
                        true,
                        Encoding::LossyUtf8,
                        true,
                        true,
                    ),
                }
            }
        } else if bytes.starts_with(&[0xFF, 0xFE]) {
            let u16s: Vec<u16> = bytes[2..]
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            (
                String::from_utf16_lossy(&u16s),
                false,
                Encoding::Utf16Le,
                false,
                false,
            )
        } else if bytes.starts_with(&[0xFE, 0xFF]) {
            let u16s: Vec<u16> = bytes[2..]
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            (
                String::from_utf16_lossy(&u16s),
                false,
                Encoding::Utf16Be,
                false,
                false,
            )
        } else if is_binary_bytes(&bytes) {
            (String::new(), false, Encoding::Binary, true, true)
        } else {
            match std::str::from_utf8(&bytes) {
                Ok(s) => (s.to_string(), false, Encoding::Utf8, false, false),
                Err(_) => (
                    String::from_utf8_lossy(&bytes).into_owned(),
                    false,
                    Encoding::LossyUtf8,
                    true,
                    true,
                ),
            }
        };

        let crlf = text.contains("\r\n");
        let normalized = if encoding == Encoding::Binary {
            String::new()
        } else if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text
        };
        let rope = Rope::from_str(&normalized);
        // The rope owns its own copy, so hand the decoded string to the baseline
        // instead of cloning the file a second time.
        let baseline: Option<Arc<str>> =
            (encoding != Encoding::Binary && normalized.len() <= MAX_BASELINE_BYTES)
                .then(|| Arc::from(normalized));
        let saved_text_hash = hash_rope(&rope);
        let eol = if crlf { Eol::CrLf } else { Eol::Lf };
        Ok(Self {
            rope,
            path: Some(path.to_path_buf()),
            eol,
            saved_eol: eol,
            bom,
            encoding,
            version: 1,
            saved_text_hash,
            saved_version: 1,
            saved_undo_depth: Some(0),
            disk_modified_at: modified_time,
            hash_cache: std::cell::Cell::new((1, saved_text_hash)),
            undo: Vec::new(),
            redo: Vec::new(),
            readonly,
            lossy,
            baseline_text: baseline,
            baseline_is_git: false,
            save_text_buf: String::new(),
            save_bytes_buf: Vec::new(),
        })
    }

    pub fn reload(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let fresh = Self::load(&path)?;
        self.rope = fresh.rope;
        self.eol = fresh.eol;
        self.saved_eol = fresh.saved_eol;
        self.bom = fresh.bom;
        self.encoding = fresh.encoding;
        self.readonly = fresh.readonly;
        self.lossy = fresh.lossy;
        if !self.baseline_is_git {
            self.baseline_text = fresh.baseline_text;
        }
        self.version += 1;
        self.saved_text_hash = fresh.saved_text_hash;
        self.saved_version = self.version;
        self.saved_undo_depth = Some(0);
        self.disk_modified_at = fresh.disk_modified_at;
        self.hash_cache.set((self.version, fresh.saved_text_hash));
        self.undo.clear();
        self.redo.clear();
        Ok(())
    }

    pub fn save(&mut self) -> SaveOutcome {
        let Some(path) = self.path.clone() else {
            return SaveOutcome::Failed(io::Error::new(io::ErrorKind::NotFound, "文件尚未命名"));
        };
        if self.readonly || self.encoding == Encoding::Binary {
            return SaveOutcome::ReadOnly;
        }
        if self.disk_changed() {
            return SaveOutcome::DiskChanged;
        }
        // Take the scratch buffers out of self so the rope can be walked while
        // they are written; they go back at the end and are reused next save.
        let mut text = std::mem::take(&mut self.save_text_buf);
        text.clear();
        text.reserve(self.rope.len_bytes() + 2);
        for chunk in self.rope.chunks() {
            text.push_str(chunk);
        }
        if !self.baseline_is_git && text.len() <= MAX_BASELINE_BYTES {
            self.baseline_text = Some(Arc::from(text.as_str()));
        }
        let crlf = self.eol == Eol::CrLf;
        let mut bytes = std::mem::take(&mut self.save_bytes_buf);
        bytes.clear();
        match self.encoding {
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let little_endian = self.encoding == Encoding::Utf16Le;
                bytes.reserve(text.len() * 2 + 2);
                bytes.extend_from_slice(if little_endian {
                    &[0xFF, 0xFE]
                } else {
                    &[0xFE, 0xFF]
                });
                for unit in text.encode_utf16() {
                    // Newlines are single UTF-16 units, so CRLF needs no second pass.
                    if crlf && unit == u16::from(b'\n') {
                        let cr: [u8; 2] = if little_endian {
                            0x0Du16.to_le_bytes()
                        } else {
                            0x0Du16.to_be_bytes()
                        };
                        bytes.extend_from_slice(&cr);
                    }
                    let raw: [u8; 2] = if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    };
                    bytes.extend_from_slice(&raw);
                }
            }
            _ => {
                if self.bom || self.encoding == Encoding::Utf8Bom {
                    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                }
                bytes.reserve(if crlf {
                    text.len() + self.rope.len_lines()
                } else {
                    text.len()
                });
                if crlf {
                    push_crlf(&mut bytes, &text);
                } else {
                    bytes.extend_from_slice(text.as_bytes());
                }
            }
        }
        let tmp = temp_path(&path);
        let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &path));
        self.save_text_buf = text;
        self.save_bytes_buf = bytes;
        if let Err(err) = written {
            let _ = std::fs::remove_file(&tmp);
            return if err.kind() == io::ErrorKind::PermissionDenied {
                SaveOutcome::ReadOnly
            } else {
                SaveOutcome::Failed(err)
            };
        }
        self.saved_text_hash = self.content_hash();
        self.saved_version = self.version;
        self.saved_undo_depth = Some(self.undo.len());
        self.saved_eol = self.eol;
        self.disk_modified_at = disk_mtime(&path);
        SaveOutcome::Saved
    }

    pub fn save_as(&mut self, path: PathBuf) -> SaveOutcome {
        let prev_path = self.path.clone();
        let prev_mtime = self.disk_modified_at;
        let prev_readonly = self.readonly;
        self.disk_modified_at = disk_mtime(&path);
        self.path = Some(path);
        // Explicit Save As to a new path is allowed even from a lossy/readonly preview
        // unless the encoding is Binary.
        if self.encoding != Encoding::Binary {
            self.readonly = false;
        }
        let outcome = self.save();
        if !matches!(outcome, SaveOutcome::Saved) {
            self.path = prev_path;
            self.disk_modified_at = prev_mtime;
            self.readonly = prev_readonly;
        }
        outcome
    }

    /// Re-decode the file on disk with an explicitly chosen encoding.
    pub fn reopen_with_encoding(&mut self, encoding: Encoding) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "文件尚未命名"));
        };
        let bytes = std::fs::read(&path)?;
        let text = decode_bytes(&bytes, encoding)?;
        let crlf = text.contains("\r\n");
        let normalized = if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text
        };
        let rope = Rope::from_str(&normalized);
        let hash = hash_rope(&rope);
        let eol = if crlf { Eol::CrLf } else { Eol::Lf };
        self.rope = rope;
        self.eol = eol;
        self.saved_eol = eol;
        self.encoding = encoding;
        self.bom = encoding == Encoding::Utf8Bom;
        self.lossy = encoding == Encoding::LossyUtf8;
        self.readonly = encoding == Encoding::LossyUtf8;
        self.version += 1;
        self.saved_text_hash = hash;
        self.saved_version = self.version;
        self.saved_undo_depth = Some(0);
        self.hash_cache.set((self.version, hash));
        if !self.baseline_is_git {
            self.baseline_text =
                (normalized.len() <= MAX_BASELINE_BYTES).then(|| Arc::from(normalized));
        }
        self.disk_modified_at = disk_mtime(&path);
        self.undo.clear();
        self.redo.clear();
        Ok(())
    }

    /// Write the buffer with a new line ending; the file itself changes on save.
    pub fn set_eol(&mut self, eol: Eol) {
        self.eol = eol;
    }

    /// Apply an explicit encoding and save under a new path.
    pub fn save_as_with_encoding(&mut self, path: PathBuf, encoding: Encoding) -> SaveOutcome {
        let prev_encoding = self.encoding;
        let prev_bom = self.bom;
        let prev_lossy = self.lossy;
        self.encoding = encoding;
        self.bom = encoding == Encoding::Utf8Bom;
        self.lossy = encoding == Encoding::LossyUtf8;
        let outcome = self.save_as(path);
        if !matches!(outcome, SaveOutcome::Saved) {
            self.encoding = prev_encoding;
            self.bom = prev_bom;
            self.lossy = prev_lossy;
        }
        outcome
    }

    /// FNV-1a fingerprint of the whole buffer, cached by document version.
    pub fn content_hash(&self) -> u64 {
        let (version, hash) = self.hash_cache.get();
        if version == self.version {
            return hash;
        }
        let hash = hash_rope(&self.rope);
        self.hash_cache.set((self.version, hash));
        hash
    }

    pub fn is_dirty(&self) -> bool {
        if self.eol != self.saved_eol {
            return true;
        }
        if self.version == self.saved_version {
            return false;
        }
        if self.rope.len_bytes() > FAST_DIRTY_THRESHOLD_BYTES {
            return self.saved_undo_depth != Some(self.undo.len());
        }
        self.content_hash() != self.saved_text_hash
    }

    /// Mark the buffer as unsaved content that has no file to fall back to.
    pub fn mark_dirty(&mut self) {
        self.saved_version = 0;
        self.saved_undo_depth = None;
        self.saved_text_hash = !self.content_hash();
    }

    /// True when the file on disk no longer matches the mtime recorded at load/save.
    pub fn disk_changed(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        let Some(known) = self.disk_modified_at else {
            return false;
        };
        disk_mtime(path) != Some(known)
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".to_string())
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Line content without the trailing newline.
    pub fn line_content(&self, line: usize) -> String {
        if line >= self.line_count() {
            return String::new();
        }
        let slice = self.rope.line(line);
        let mut s = slice.to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    pub fn line_char_len(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
        }
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
        len
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        self.rope.line_to_char(line)
    }

    pub fn char_to_line(&self, ch: usize) -> usize {
        self.rope.char_to_line(ch.min(self.len_chars()))
    }

    pub fn char_to_line_col(&self, ch: usize) -> (usize, usize) {
        let ch = ch.min(self.len_chars());
        let line = self.rope.char_to_line(ch);
        (line, ch - self.rope.line_to_char(line))
    }

    pub fn line_col_to_char(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        start + col.min(self.line_char_len(line))
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn char_to_byte(&self, ch: usize) -> usize {
        self.rope.char_to_byte(ch.min(self.len_chars()))
    }

    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.len_bytes()))
    }

    /// 0-based line and byte column, the coordinate system tree-sitter uses.
    pub fn char_to_point(&self, ch: usize) -> (usize, usize) {
        let ch = ch.min(self.len_chars());
        let line = self.rope.char_to_line(ch);
        let line_byte = self.rope.line_to_byte(line);
        (line, self.rope.char_to_byte(ch) - line_byte)
    }

    pub fn text_range(&self, start: usize, end: usize) -> String {
        let start = start.min(self.len_chars());
        let end = end.clamp(start, self.len_chars());
        self.rope.slice(start..end).to_string()
    }

    pub fn char_at(&self, ch: usize) -> Option<char> {
        if ch < self.len_chars() {
            self.rope.get_char(ch)
        } else {
            None
        }
    }

    pub fn line_indent(&self, line: usize) -> String {
        self.line_content(line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// Apply one edit, recording undo information. Returns the tree-sitter edit
    /// description of what changed.
    pub fn edit(
        &mut self,
        start: usize,
        end: usize,
        insert: &str,
        kind: EditKind,
        before: (usize, usize),
        after: (usize, usize),
    ) -> SyntaxEdit {
        let (record, syntax_edit) = self.apply_edit_raw(start, end, insert);
        self.push_undo(vec![record], kind, before, after);
        syntax_edit
    }

    /// Apply several non-overlapping edits (in original document coordinates) as
    /// one undo step. Returns syntax edits in application order.
    pub fn edit_many(
        &mut self,
        edits: &[(usize, usize, String)],
        before: (usize, usize),
        after: (usize, usize),
    ) -> Vec<SyntaxEdit> {
        let mut sorted: Vec<&(usize, usize, String)> = edits.iter().collect();
        sorted.sort_by_key(|e| e.0);
        let mut delta: isize = 0;
        let mut records = Vec::with_capacity(sorted.len());
        let mut syntax_edits = Vec::with_capacity(sorted.len());
        for (start, end, insert) in sorted {
            let cur_start = (*start as isize + delta).max(0) as usize;
            let cur_end = (*end as isize + delta).max(cur_start as isize) as usize;
            let (record, syntax_edit) = self.apply_edit_raw(cur_start, cur_end, insert);
            delta += insert.chars().count() as isize - end.saturating_sub(*start) as isize;
            records.push(record);
            syntax_edits.push(syntax_edit);
        }
        self.push_undo(records, EditKind::Other, before, after);
        syntax_edits
    }

    fn apply_edit_raw(
        &mut self,
        start: usize,
        end: usize,
        insert: &str,
    ) -> (EditRecord, SyntaxEdit) {
        let start = start.min(self.len_chars());
        let end = end.clamp(start, self.len_chars());
        let removed = self.text_range(start, end);

        let old_start_byte = self.rope.char_to_byte(start);
        let old_end_byte = self.rope.char_to_byte(end);
        let (start_row, start_col) = self.char_to_point(start);
        let (old_end_row, old_end_col) = self.char_to_point(end);

        self.rope.remove(start..end);
        self.rope.insert(start, insert);
        self.version += 1;

        let new_end_byte = old_start_byte + insert.len();
        let (new_end_row, new_end_col) = match insert.rfind('\n') {
            None => (start_row, start_col + insert.len()),
            Some(nl) => (
                start_row + insert.matches('\n').count(),
                insert[nl + 1..].len(),
            ),
        };

        let syntax_edit = SyntaxEdit {
            start_byte: old_start_byte,
            old_end_byte,
            new_end_byte,
            start_row,
            start_column: start_col,
            old_end_row,
            old_end_column: old_end_col,
            new_end_row,
            new_end_column: new_end_col,
        };
        let record = EditRecord {
            start,
            removed,
            inserted: insert.to_string(),
        };
        (record, syntax_edit)
    }

    fn push_undo(
        &mut self,
        records: Vec<EditRecord>,
        kind: EditKind,
        before: (usize, usize),
        after: (usize, usize),
    ) {
        self.redo.clear();
        if self
            .saved_undo_depth
            .is_some_and(|depth| depth > self.undo.len())
        {
            self.saved_undo_depth = None;
        }
        let now = Instant::now();
        let single = records.len() == 1;
        if single
            && self.saved_undo_depth != Some(self.undo.len())
            && let Some(group) = self.undo.last_mut()
            && group.kind == kind
            && kind != EditKind::Other
            && now.duration_since(group.time).as_millis() < COALESCE_WINDOW_MS
            && Self::try_merge(group, &records[0])
        {
            group.after = after;
            group.time = now;
            return;
        }
        self.undo.push(UndoGroup {
            edits: records,
            before,
            after,
            kind,
            time: now,
        });
        if self.undo.len() > MAX_UNDO_GROUPS {
            self.undo.remove(0);
            self.saved_undo_depth = self.saved_undo_depth.and_then(|d| d.checked_sub(1));
        }
    }

    fn try_merge(group: &mut UndoGroup, record: &EditRecord) -> bool {
        let Some(last) = group.edits.last_mut() else {
            return false;
        };
        let last_end = last.start + last.inserted.chars().count();

        if record.removed.is_empty()
            && !record.inserted.is_empty()
            && group.kind == EditKind::Typing
            && record.start == last_end
        {
            last.inserted.push_str(&record.inserted);
            return true;
        }

        if record.inserted.is_empty()
            && !record.removed.is_empty()
            && group.kind == EditKind::Backspace
            && record.start + record.removed.chars().count() == last.start
        {
            let first = group.edits.first_mut().unwrap();
            first.start = record.start;
            first.removed = format!("{}{}", record.removed, first.removed);
            return true;
        }

        if record.inserted.is_empty()
            && !record.removed.is_empty()
            && group.kind == EditKind::Delete
            && record.start == last.start
            && last.inserted.is_empty()
        {
            last.removed.push_str(&record.removed);
            return true;
        }
        false
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> Option<(usize, usize)> {
        let group = self.undo.pop()?;
        for edit in group.edits.iter().rev() {
            let inserted_len = edit.inserted.chars().count();
            let end = (edit.start + inserted_len).min(self.len_chars());
            self.rope.remove(edit.start.min(end)..end);
            self.rope
                .insert(edit.start.min(self.len_chars()), &edit.removed);
        }
        self.version += 1;
        let before = group.before;
        self.redo.push(group);
        Some(before)
    }

    pub fn redo(&mut self) -> Option<(usize, usize)> {
        let group = self.redo.pop()?;
        for edit in group.edits.iter() {
            let removed_len = edit.removed.chars().count();
            let end = (edit.start + removed_len).min(self.len_chars());
            self.rope.remove(edit.start.min(end)..end);
            self.rope
                .insert(edit.start.min(self.len_chars()), &edit.inserted);
        }
        self.version += 1;
        let after = group.after;
        self.undo.push(group);
        Some(after)
    }
}

/// Decode file bytes using an explicitly requested encoding.
fn decode_bytes(bytes: &[u8], encoding: Encoding) -> io::Result<String> {
    match encoding {
        Encoding::Utf8 | Encoding::LossyUtf8 => match std::str::from_utf8(bytes) {
            Ok(text) => Ok(text.to_string()),
            Err(_) if encoding == Encoding::LossyUtf8 => {
                Ok(String::from_utf8_lossy(bytes).into_owned())
            }
            Err(err) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UTF-8 解码失败: {err}"),
            )),
        },
        Encoding::Utf8Bom => {
            let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            std::str::from_utf8(body)
                .map(str::to_string)
                .map_err(|err| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("UTF-8 BOM 解码失败: {err}"))
                })
        }
        Encoding::Utf16Le => decode_utf16(bytes, true),
        Encoding::Utf16Be => decode_utf16(bytes, false),
        Encoding::Binary => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "二进制文件不能按文本编码重新打开",
        )),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> io::Result<String> {
    let body = if little_endian {
        bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes)
    } else {
        bytes.strip_prefix(&[0xFE, 0xFF]).unwrap_or(bytes)
    };
    if body.len() % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UTF-16 字节长度不是偶数",
        ));
    }
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes(*pair)
            } else {
                u16::from_be_bytes(*pair)
            }
        })
        .collect();
    String::from_utf16(&units)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "UTF-16 包含无效代理项"))
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn hash_rope(rope: &Rope) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for chunk in rope.chunks() {
        for byte in chunk.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

fn disk_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

/// Sibling temp file so the final rename stays on one filesystem and is atomic.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".kestrel-tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kestrel_doc_{}_{name}", std::process::id()))
    }

    #[test]
    fn line_math() {
        let doc = Document::from_text("abc\ndefg\n\nhi");
        assert_eq!(doc.line_count(), 4);
        assert_eq!(doc.line_content(1), "defg");
        assert_eq!(doc.line_char_len(1), 4);
        assert_eq!(doc.char_to_line_col(5), (1, 1));
        assert_eq!(doc.line_col_to_char(1, 4), 8);
        assert_eq!(doc.text_range(4, 8), "defg");
    }

    #[test]
    fn typing_coalesces_and_undoes_as_one_step() {
        let mut doc = Document::from_text("");
        for (i, ch) in ["h", "e", "l", "l", "o"].iter().enumerate() {
            doc.edit(i, i, ch, EditKind::Typing, (i, i), (i + 1, i + 1));
        }
        assert_eq!(doc.text(), "hello");
        assert!(doc.is_dirty());
        assert_eq!(doc.undo().unwrap(), (0, 0));
        assert_eq!(doc.text(), "");
        assert_eq!(doc.redo().unwrap(), (5, 5));
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn save_roundtrip_preserves_crlf_and_bom() {
        let path = temp_path("crlf.txt");
        std::fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\r\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert_eq!(doc.eol, Eol::CrLf);
        assert_eq!(doc.encoding, Encoding::Utf8Bom);
        assert_eq!(doc.text(), "one\ntwo\n");
        doc.edit(0, 0, "x", EditKind::Other, (0, 0), (1, 1));
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"\xEF\xBB\xBFxone\r\ntwo\r\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn utf16le_roundtrip() {
        let path = temp_path("utf16.txt");
        let mut raw = vec![0xFF, 0xFE];
        for u in "hello\r\n世界".encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&path, &raw).unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert_eq!(doc.encoding, Encoding::Utf16Le);
        assert_eq!(doc.text(), "hello\n世界");
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        let saved = std::fs::read(&path).unwrap();
        assert_eq!(saved, raw);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_many_undo_is_atomic() {
        let mut doc = Document::from_text("a\nb\nc");
        doc.edit_many(
            &[
                (0, 1, "A".to_string()),
                (2, 3, "B".to_string()),
                (4, 5, "C".to_string()),
            ],
            (0, 0),
            (0, 0),
        );
        assert_eq!(doc.text(), "A\nB\nC");
        doc.undo();
        assert_eq!(doc.text(), "a\nb\nc");
    }

    #[test]
    fn undo_clears_at_redo_boundary() {
        let mut doc = Document::from_text("");
        doc.edit(0, 0, "a", EditKind::Other, (0, 0), (1, 1));
        doc.edit(1, 1, "b", EditKind::Other, (1, 1), (2, 2));
        doc.undo();
        assert!(doc.can_redo());
        doc.edit(0, 0, "z", EditKind::Typing, (0, 0), (1, 1));
        assert!(!doc.can_redo());
        assert_eq!(doc.text(), "za");
    }

    #[test]
    fn save_clears_dirty_and_undo_restores_clean_state() {
        let path = temp_path("dirty.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert!(!doc.is_dirty());
        doc.edit(0, 0, "x", EditKind::Other, (0, 0), (1, 1));
        assert!(doc.is_dirty());
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        assert!(!doc.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xone\n");
        doc.edit(0, 1, "y", EditKind::Other, (0, 0), (1, 1));
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn own_save_does_not_look_like_an_external_change() {
        let path = temp_path("selfwrite.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        doc.edit(0, 0, "x", EditKind::Other, (0, 0), (1, 1));
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        assert!(!doc.disk_changed());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn external_edit_makes_save_report_disk_changed() {
        let path = temp_path("conflict.txt");
        std::fs::write(&path, "mine\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        doc.edit(0, 0, "x", EditKind::Other, (0, 0), (1, 1));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "theirs\n").unwrap();
        assert!(matches!(doc.save(), SaveOutcome::DiskChanged));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");
        assert!(doc.is_dirty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn binary_file_loads_empty_readonly_instead_of_garbled_text() {
        let path = temp_path("sample.bin");
        std::fs::write(&path, b"\x4D\x5A\x90\x00\x03\x00\xFF\xFE\x00\x12\x03\x04").unwrap();
        let doc = Document::load(&path).unwrap();
        assert_eq!(doc.encoding, Encoding::Binary);
        assert!(doc.readonly);
        assert!(doc.lossy);
        assert_eq!(doc.len_chars(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_reuses_buffers_and_refreshes_baseline() {
        let path = temp_path("reuse.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert_eq!(doc.baseline_text.as_deref(), Some("one\ntwo\n"));
        doc.edit(0, 0, "x", EditKind::Other, (0, 0), (1, 1));
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        let text_capacity = doc.save_text_buf.capacity();
        let bytes_capacity = doc.save_bytes_buf.capacity();
        assert_eq!(doc.baseline_text.as_deref(), Some("xone\ntwo\n"));
        // Same-length edit: the next save must reuse the same scratch allocations.
        doc.edit(0, 1, "y", EditKind::Other, (0, 0), (1, 1));
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "yone\ntwo\n");
        // The scratch allocations survive the save instead of being rebuilt.
        assert_eq!(doc.save_text_buf.capacity(), text_capacity);
        assert_eq!(doc.save_bytes_buf.capacity(), bytes_capacity);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopen_with_encoding_decodes_requested_utf16() {
        let path = temp_path("reopen_utf16.txt");
        let mut raw = vec![0xFF, 0xFE];
        for unit in "第一行\r\n第二行".encode_utf16() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&path, &raw).unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert_eq!(doc.encoding, Encoding::Utf16Le);
        // Reopening as UTF-8 must fail instead of garbling the buffer.
        assert!(doc.reopen_with_encoding(Encoding::Utf8).is_err());
        assert!(doc.reopen_with_encoding(Encoding::Utf16Le).is_ok());
        assert_eq!(doc.text(), "第一行\n第二行");
        assert_eq!(doc.eol, Eol::CrLf);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_as_with_encoding_writes_utf16_and_sets_eol() {
        let path = temp_path("save_utf16le.txt");
        let mut doc = Document::from_text("one\ntwo\n");
        doc.set_eol(Eol::CrLf);
        assert!(doc.is_dirty());
        assert!(matches!(
            doc.save_as_with_encoding(path.clone(), Encoding::Utf16Le),
            SaveOutcome::Saved
        ));
        assert_eq!(doc.encoding, Encoding::Utf16Le);
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "one\r\ntwo\r\n");
        assert!(!doc.is_dirty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn crlf_save_rewrites_newlines_in_place() {
        let path = temp_path("crlf_inplace.txt");
        std::fs::write(&path, b"a\r\nb\r\n").unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert_eq!(doc.eol, Eol::CrLf);
        doc.edit(1, 1, "X", EditKind::Other, (1, 1), (2, 2));
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        assert_eq!(std::fs::read(&path).unwrap(), b"aX\r\nb\r\n");
        assert_eq!(doc.save_bytes_buf, b"aX\r\nb\r\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn large_document_fast_dirty_tracks_savepoint_and_undo() {
        let big = "a".repeat(FAST_DIRTY_THRESHOLD_BYTES + 1024);
        let path = temp_path("large_dirty.txt");
        std::fs::write(&path, &big).unwrap();
        let mut doc = Document::load(&path).unwrap();
        assert!(!doc.is_dirty());
        doc.edit(0, 0, "X", EditKind::Typing, (0, 0), (1, 1));
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        doc.redo();
        assert!(doc.is_dirty());
        assert!(matches!(doc.save(), SaveOutcome::Saved));
        assert!(!doc.is_dirty());
        // Typing right after save must not coalesce across the savepoint.
        doc.edit(1, 1, "Y", EditKind::Typing, (1, 1), (2, 2));
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        let _ = std::fs::remove_file(&path);
    }
}

/// Heuristic detection for binary (non-text) files by inspecting NUL and control bytes.
pub fn is_binary_bytes(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample = &bytes[..bytes.len().min(16384)];
    if sample.contains(&0) {
        return true;
    }
    let ctrl_count = sample
        .iter()
        .filter(|&&b| {
            (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x08 | 0x0C | 0x1B)) || b == 0x7F
        })
        .count();
    ctrl_count >= 4 && ctrl_count * 100 > sample.len() * 2
}

/// Best-effort magic-header identification for common unparseable binary formats.
pub fn detect_binary_signature(path: &Path) -> &'static str {
    let Ok(mut file) = std::fs::File::open(path) else {
        return "二进制文件";
    };
    let mut buf = [0u8; 16];
    let n = io::Read::read(&mut file, &mut buf).unwrap_or(0);
    let head = &buf[..n];
    if head.starts_with(b"MZ") {
        return "Windows 可执行文件 / 动态库 (PE)";
    }
    if head.starts_with(b"\x7FELF") {
        return "Linux / Unix 可执行文件 (ELF)";
    }
    if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return "旧版 Office 复合文档 (OLE2 .doc/.xls/.ppt)";
    }
    if head.starts_with(b"SQLite format 3") {
        return "SQLite 数据库文件";
    }
    if head.starts_with(b"Rar!") {
        return "RAR 压缩归档";
    }
    if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return "7-Zip 压缩归档";
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return "GZIP 压缩文件";
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return "Zstandard 压缩文件";
    }
    if head.starts_with(b"\x00asm") {
        return "WebAssembly 二进制模块 (.wasm)";
    }
    if head.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]) {
        return "Java 字节码 / Mach-O 通用二进制";
    }
    if head.starts_with(b"ID3") || head.starts_with(b"fLaC") || head.starts_with(b"OggS") {
        return "音频媒体文件";
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return "视频 / MP4 媒体文件";
    }
    if head.starts_with(b"wOFF") || head.starts_with(b"wOF2") || head.starts_with(b"OTTO") {
        return "字体二进制文件";
    }
    "二进制文件"
}
