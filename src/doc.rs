//! Text buffer with undo/redo, line math, multi-encoding support, and disk persistence.
//!
//! Text is stored as a rope with `\n` line endings; the original line ending and
//! encoding are remembered and restored on save. All cursor/selection coordinates
//! in the app are char offsets into this rope.

use std::io;
use std::path::{Path, PathBuf};
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
    pub bom: bool,
    pub encoding: Encoding,
    pub version: u64,
    pub saved_version: u64,
    pub modified_time: Option<SystemTime>,
    pub undo: Vec<UndoGroup>,
    pub redo: Vec<UndoGroup>,
    pub readonly: bool,
    /// Set when the file was not valid UTF-8/UTF-16 and got lossy-decoded.
    pub lossy: bool,
    /// Snapshot of saved/HEAD text (capped at 512 KB) used for gutter line-diff markers.
    pub baseline_text: Option<String>,
}

const MAX_UNDO_GROUPS: usize = 800;
const COALESCE_WINDOW_MS: u128 = 600;
const MAX_BASELINE_BYTES: usize = 512 * 1024;

impl Document {
    pub fn from_text(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let baseline = if text.len() <= MAX_BASELINE_BYTES {
            Some(text.to_string())
        } else {
            None
        };
        Self {
            rope,
            path: None,
            eol: Eol::Lf,
            bom: false,
            encoding: Encoding::Utf8,
            version: 1,
            saved_version: 0,
            modified_time: None,
            undo: Vec::new(),
            redo: Vec::new(),
            readonly: false,
            lossy: false,
            baseline_text: baseline,
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
                        false,
                    ),
                }
            }
        } else if bytes.starts_with(&[0xFF, 0xFE]) {
            let u16s: Vec<u16> = bytes[2..]
                .chunks_exact(2)
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
                .chunks_exact(2)
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
                    false,
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
        let baseline = if encoding != Encoding::Binary && normalized.len() <= MAX_BASELINE_BYTES {
            Some(normalized.clone())
        } else {
            None
        };
        let rope = Rope::from_str(&normalized);
        Ok(Self {
            rope,
            path: Some(path.to_path_buf()),
            eol: if crlf { Eol::CrLf } else { Eol::Lf },
            bom,
            encoding,
            version: 1,
            saved_version: 1,
            modified_time,
            undo: Vec::new(),
            redo: Vec::new(),
            readonly,
            lossy,
            baseline_text: baseline,
        })
    }

    pub fn reload(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let fresh = Self::load(&path)?;
        self.rope = fresh.rope;
        self.eol = fresh.eol;
        self.bom = fresh.bom;
        self.encoding = fresh.encoding;
        self.readonly = fresh.readonly;
        self.lossy = fresh.lossy;
        self.baseline_text = fresh.baseline_text;
        self.version += 1;
        self.saved_version = self.version;
        self.modified_time = fresh.modified_time;
        self.undo.clear();
        self.redo.clear();
        Ok(())
    }

    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "文件尚未命名"));
        };
        if self.encoding == Encoding::Binary {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "二进制文件已设为只读保护，无法直接保存",
            ));
        }
        let mut out = String::with_capacity(self.rope.len_bytes() + 2);
        for chunk in self.rope.chunks() {
            out.push_str(chunk);
        }
        if out.len() <= MAX_BASELINE_BYTES {
            self.baseline_text = Some(out.clone());
        }
        if self.eol == Eol::CrLf {
            out = out.replace('\n', "\r\n");
        }
        let mut bytes = Vec::with_capacity(out.len() + 4);
        match self.encoding {
            Encoding::Utf16Le => {
                bytes.extend_from_slice(&[0xFF, 0xFE]);
                for u in out.encode_utf16() {
                    bytes.extend_from_slice(&u.to_le_bytes());
                }
            }
            Encoding::Utf16Be => {
                bytes.extend_from_slice(&[0xFE, 0xFF]);
                for u in out.encode_utf16() {
                    bytes.extend_from_slice(&u.to_be_bytes());
                }
            }
            _ => {
                if self.bom || self.encoding == Encoding::Utf8Bom {
                    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                }
                bytes.extend_from_slice(out.as_bytes());
            }
        }
        std::fs::write(&path, bytes)?;
        self.saved_version = self.version;
        self.modified_time = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> io::Result<()> {
        self.path = Some(path);
        self.save()
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
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
        let now = Instant::now();
        let single = records.len() == 1;
        if single
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
        doc.save().unwrap();
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
        doc.save().unwrap();
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
