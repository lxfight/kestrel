//! Integrated terminal: ConPTY session + vt100 screen state.
//!
//! The UI layer paints [`TerminalSession::screen`]; input goes back through
//! [`TerminalSession::write`]. Output is drained on the UI thread from a reader
//! thread channel, so painting never blocks on the shell.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};

/// Scrollback kept by the vt100 parser.
const SCROLLBACK: usize = 5000;
/// Reader thread buffer; also the size of one channel chunk.
const READ_CHUNK: usize = 8 * 1024;
/// Bound the chunks parsed per frame so a firehose command cannot stall painting.
const MAX_CHUNKS_PER_POLL: usize = 256;
/// Search results are capped so a pathological match count cannot stall painting.
pub const MAX_SEARCH_MATCHES: usize = 1000;

/// vt100 0.18 has no `Screen::title`; titles arrive as OSC 0/2 escape
/// sequences, so capture them through parser callbacks.
#[derive(Default)]
struct TermCallbacks {
    title: String,
    /// Set when the pty host asks for the cursor position (`ESC[6n`).
    cursor_report: bool,
}

impl vt100::Callbacks for TermCallbacks {
    fn set_window_title(&mut self, _screen: &mut vt100::Screen, title: &[u8]) {
        let title = String::from_utf8_lossy(title);
        let title = title.trim();
        // Windows PowerShell reports its own executable path as the title; keep
        // the friendly default instead of showing "C:\...\powershell.exe".
        if title.is_empty() || title.to_ascii_lowercase().ends_with(".exe") {
            return;
        }
        self.title = title.to_string();
    }

    fn unhandled_csi(
        &mut self,
        _screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        // DSR 6: ConPTY sends this while starting up and waits for the pty
        // host to answer with a cursor position report; without the reply the
        // shell's output is never flushed to us.
        if i1.is_none() && c == 'n' && params.len() == 1 && params[0].first() == Some(&6) {
            self.cursor_report = true;
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    Default,
    PowerShell7,
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
}

#[allow(dead_code)]
impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "PowerShell",
            Self::PowerShell7 => "PowerShell 7",
            Self::PowerShell => "Windows PowerShell",
            Self::Cmd => "Command Prompt",
            Self::GitBash => "Git Bash",
            Self::Wsl => "WSL",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::PowerShell7 => "pwsh",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
            Self::GitBash => "gitbash",
            Self::Wsl => "wsl",
        }
    }

    pub fn from_str(name: &str) -> Self {
        match name {
            "pwsh" => Self::PowerShell7,
            "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            "gitbash" => Self::GitBash,
            "wsl" => Self::Wsl,
            _ => Self::Default,
        }
    }

    /// cmd.exe has no clear applet; every other supported shell accepts it.
    fn clear_command(self) -> &'static [u8] {
        match self {
            Self::Cmd => b"cls\r",
            _ => b"clear\r",
        }
    }
}

/// One shell installed on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellProfile {
    pub kind: ShellKind,
    pub label: &'static str,
}

/// Known Git for Windows locations, most common first.
const GIT_BASH_PATHS: &[&str] = &[
    "C:/Program Files/Git/bin/bash.exe",
    "C:/Program Files (x86)/Git/bin/bash.exe",
    "C:/Program Files/Git/usr/bin/bash.exe",
];

fn profile(kind: ShellKind) -> ShellProfile {
    ShellProfile {
        kind,
        label: kind.label(),
    }
}

/// Detect which shells are installed. Pure: the exists callback decides
/// availability, so tests never depend on the host. Always returns at least
/// one profile.
pub fn probe_profiles(exists: impl Fn(&str) -> bool) -> Vec<ShellProfile> {
    let mut profiles = Vec::new();
    if exists("pwsh.exe") {
        profiles.push(profile(ShellKind::PowerShell7));
    }
    if exists("powershell.exe") {
        profiles.push(profile(ShellKind::PowerShell));
    }
    if exists("cmd.exe") {
        profiles.push(profile(ShellKind::Cmd));
    }
    let git_bash = GIT_BASH_PATHS.iter().any(|path| exists(path)) || exists("bash.exe");
    if git_bash {
        profiles.push(profile(ShellKind::GitBash));
    }
    if exists("wsl.exe") {
        profiles.push(profile(ShellKind::Wsl));
    }
    if profiles.is_empty() {
        profiles.push(profile(ShellKind::Cmd));
    }
    profiles
}

/// Cached probe over the real file system; no process is spawned.
pub fn available_profiles() -> &'static [ShellProfile] {
    static PROFILES: LazyLock<Vec<ShellProfile>> = LazyLock::new(|| probe_profiles(shell_exists));
    PROFILES.as_slice()
}

fn shell_exists(name: &str) -> bool {
    // COMSPEC names cmd.exe even when the app PATH cannot resolve it.
    if name.eq_ignore_ascii_case("cmd.exe") && std::env::var_os("COMSPEC").is_some() {
        return true;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file();
    }
    command_in_path(name).is_some()
}

/// Resolve a bare command name through PATH and PATHEXT.
fn command_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) && Path::new(name).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path_var).find_map(|dir| {
        exts.iter().find_map(|ext| {
            let mut file = name.to_string();
            file.push_str(ext);
            let candidate = dir.join(file);
            candidate.is_file().then_some(candidate)
        })
    })
}

fn git_bash_path() -> Option<PathBuf> {
    GIT_BASH_PATHS
        .iter()
        .map(|path| Path::new(*path))
        .find(|path| path.is_file())
        .map(Path::to_path_buf)
        .or_else(|| command_in_path("bash.exe"))
}

/// Inclusive cell range over the visible terminal grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TerminalSelection {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl TerminalSelection {
    fn normalized(anchor: (u16, u16), cursor: (u16, u16)) -> Self {
        if anchor <= cursor {
            Self { start: anchor, end: cursor }
        } else {
            Self { start: cursor, end: anchor }
        }
    }

    /// Columns covered on one row, clipped to the screen width.
    pub fn row_span(&self, row: u16, cols: u16) -> Option<(u16, u16)> {
        if row < self.start.0 || row > self.end.0 || cols == 0 {
            return None;
        }
        let start = if row == self.start.0 { self.start.1 } else { 0 };
        let end = if row == self.end.0 { self.end.1 } else { cols - 1 };
        (start <= end).then_some((start.min(cols - 1), end))
    }
}

/// One case-insensitive hit; rows are history rows (scrollback first, then screen).
#[derive(Clone, Copy, Debug)]
pub struct SearchMatch {
    pub row: usize,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Default)]
pub struct TerminalSearch {
    pub open: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub active: Option<usize>,
    pub truncated: bool,
    pub dirty: bool,
    pub focus: bool,
    /// Last expensive history scan; output bursts must not rescan every frame.
    pub last_refresh: Option<Instant>,
}

pub struct TerminalSession {
    parser: vt100::Parser<TermCallbacks>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    /// Cleared when the writer fails or `try_wait` reaps the child.
    alive: bool,
    default_title: &'static str,
    pub scrollback_offset: usize,
    /// Uncommitted IME composition text for this session.
    pub ime_preedit: String,
    /// Shell actually spawned; fallbacks can differ from the requested kind.
    pub shell: ShellKind,
    /// Working directory the shell was started in; persisted in the session.
    pub cwd: PathBuf,
    /// Mouse selection over the visible grid, inclusive cells.
    pub selection: Option<TerminalSelection>,
    selection_anchor: Option<(u16, u16)>,
    /// Ctrl+F search over scrollback + screen.
    pub search: TerminalSearch,
}

impl TerminalSession {
    #[allow(dead_code)]
    pub fn new(cwd: &Path, cols: u16, rows: u16) -> Option<Self> {
        Self::new_with_shell(cwd, cols, rows, ShellKind::Default)
    }

    pub fn new_with_shell(cwd: &Path, cols: u16, rows: u16, shell: ShellKind) -> Option<Self> {
        let rows = nonzero(rows);
        let cols = nonzero(cols);
        let size = PtySize {
            rows: rows.get(),
            cols: cols.get(),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).ok()?;
        let (child, default_title, resolved_shell) =
            spawn_shell_kind(&*pair.slave, cwd, shell)?;

        // ConPTY: the slave only shares the pty state, drop it so the shell's
        // handles are the ones that decide when the pty reports EOF.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().ok()?;
        let writer = pair.master.take_writer().ok()?;

        let (tx, output) = channel();
        std::thread::Builder::new()
            .name("kestrel-terminal-reader".into())
            .spawn(move || {
                let mut buf = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .ok()?;

        Some(Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                SCROLLBACK,
                TermCallbacks::default(),
            ),
            master: pair.master,
            child,
            writer,
            output,
            alive: true,
            default_title,
            scrollback_offset: 0,
            ime_preedit: String::new(),
            shell: resolved_shell,
            cwd: cwd.to_path_buf(),
            selection: None,
            selection_anchor: None,
            search: TerminalSearch::default(),
        })
    }

    /// Drain pending output into the terminal parser. Returns true if the screen changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..MAX_CHUNKS_PER_POLL {
            match self.output.try_recv() {
                Ok(chunk) => {
                    self.parser.process(&chunk);
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if changed {
            // New output shifts the grid: stale selection coordinates would point
            // at unrelated cells, and search hits must be recomputed.
            self.clear_selection();
            self.search.dirty = true;
        }
        while std::mem::take(&mut self.parser.callbacks_mut().cursor_report) {
            let (row, col) = self.parser.screen().cursor_position();
            let report = format!("\x1b[{};{}R", row + 1, col + 1);
            if self
                .writer
                .write_all(report.as_bytes())
                .and_then(|()| self.writer.flush())
                .is_err()
            {
                self.alive = false;
                break;
            }
        }
        changed
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Plain text of one visible row; the scrollback offset is not applied.
    pub fn row_text(&self, row: u16) -> String {
        let cols = self.parser.screen().size().1.get();
        self.parser
            .screen()
            .rows(0, cols)
            .nth(usize::from(row))
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Selection, copy, and screen search
    // ------------------------------------------------------------------

    /// Start a mouse selection at a screen cell.
    pub fn begin_selection(&mut self, row: u16, col: u16) {
        self.selection_anchor = Some((row, col));
        self.selection = Some(TerminalSelection::normalized((row, col), (row, col)));
    }

    pub fn update_selection(&mut self, row: u16, col: u16) {
        let Some(anchor) = self.selection_anchor else {
            return;
        };
        self.selection = Some(TerminalSelection::normalized(anchor, (row, col)));
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    /// Plain text of the selected cells. Trailing padding is trimmed per line so a
    /// selection that reaches the right edge does not copy blanks.
    pub fn selection_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (rows, cols) = self.parser.screen().size();
        let rows = rows.get();
        let cols = cols.get();
        let start_row = selection.start.0.min(rows.saturating_sub(1));
        let end_row = selection.end.0.min(rows.saturating_sub(1));
        let start_col = selection.start.1.min(cols.saturating_sub(1));
        let end_col = selection.end.1.min(cols.saturating_sub(1));
        let text = self.parser.screen().contents_between(
            start_row,
            start_col,
            end_row,
            (end_col + 1).min(cols),
        );
        let text = text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        let text = text.trim_end().to_string();
        (!text.is_empty()).then_some(text)
    }

    /// Clear the visible screen with the command the running shell understands.
    pub fn clear_screen(&mut self) {
        let command = self.shell.clear_command();
        self.write(command);
    }

    /// First history row currently visible: history is scrollback rows followed by
    /// screen rows, so the visible window starts at scrollback_len minus the offset.
    pub fn visible_start_row(&self) -> usize {
        let len = self.parser.screen().scrollback_len();
        len.saturating_sub(self.parser.screen().scrollback())
    }

    pub fn open_search(&mut self) {
        self.search.open = true;
        self.search.focus = true;
        self.search.dirty = true;
        self.search.last_refresh = None;
    }

    pub fn close_search(&mut self) {
        self.search.open = false;
        self.search.focus = false;
        self.search.matches.clear();
        self.search.active = None;
        self.search.truncated = false;
        self.search.last_refresh = None;
    }

    /// Recompute case-insensitive matches over scrollback + screen when the query or
    /// the terminal contents changed. Hits are capped at MAX_SEARCH_MATCHES.
    pub fn refresh_search(&mut self) {
        if !self.search.open || !self.search.dirty {
            return;
        }
        // Output-heavy shells mark the search dirty every frame; rescanning up to
        // 5000 scrollback rows per frame would stall painting, so coalesce bursts.
        if self
            .search
            .last_refresh
            .is_some_and(|at| at.elapsed() < Duration::from_millis(150))
        {
            return;
        }
        self.recompute_search();
    }

    fn recompute_search(&mut self) {
        self.search.dirty = false;
        self.search.last_refresh = Some(Instant::now());
        self.search.matches.clear();
        self.search.truncated = false;
        let needle = self.search.query.to_lowercase();
        if needle.is_empty() {
            self.search.active = None;
            return;
        }
        let (rows, cols) = self.parser.screen().size();
        let rows = rows.get() as usize;
        let cols = cols.get();
        let history = self.history_rows(rows, cols);
        'history: for (row, line) in history.iter().enumerate() {
            let lower = line.to_lowercase();
            // Case folding can change byte lengths for a few scripts; fall back to
            // the folded text for column mapping then (display columns may be off).
            let col_source = if lower.len() == line.len() {
                line.as_str()
            } else {
                lower.as_str()
            };
            let mut from = 0;
            while let Some(found) = lower[from..].find(&needle) {
                let start_byte = from + found;
                let end_byte = start_byte + needle.len();
                let start_col = column_of_byte(col_source, start_byte);
                let end_col = column_of_byte(col_source, end_byte)
                    .saturating_sub(1)
                    .max(start_col)
                    .min(cols.saturating_sub(1));
                self.search.matches.push(SearchMatch {
                    row,
                    start_col,
                    end_col,
                });
                if self.search.matches.len() >= MAX_SEARCH_MATCHES {
                    self.search.truncated = true;
                    break 'history;
                }
                from = end_byte;
            }
        }
        if let Some(active) = self.search.active {
            self.search.active = (!self.search.matches.is_empty())
                .then(|| active.min(self.search.matches.len() - 1));
        }
    }

    /// Jump to the next search hit (previous when forward is false) and scroll the
    /// grid so the hit is on screen.
    pub fn search_step(&mut self, forward: bool) {
        self.search.last_refresh = None;
        self.refresh_search();
        let count = self.search.matches.len();
        if count == 0 {
            self.search.active = None;
            return;
        }
        let next = match self.search.active {
            Some(index) => {
                if forward {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                }
            }
            None if forward => 0,
            None => count - 1,
        };
        self.search.active = Some(next);
        self.scroll_to_history_row(self.search.matches[next].row);
    }

    fn scroll_to_history_row(&mut self, row: usize) {
        let screen_rows = self.parser.screen().size().0.get() as usize;
        let scrollback_len = self.parser.screen().scrollback_len();
        if row >= scrollback_len {
            self.set_scrollback(0);
        } else {
            let start = row.saturating_sub(screen_rows / 2);
            self.set_scrollback(scrollback_len - start);
        }
    }

    /// Snapshot every history row (scrollback then screen). vt100 only exposes the
    /// visible window, so page backwards through the scrollback and restore the
    /// original offset afterwards.
    fn history_rows(&mut self, rows: usize, cols: u16) -> Vec<String> {
        let scrollback_len = self.parser.screen().scrollback_len();
        let total = scrollback_len + rows;
        let original = self.parser.screen().scrollback();
        let mut history = Vec::with_capacity(total);
        let mut start = 0;
        while start < total {
            if start < scrollback_len {
                self.parser.screen_mut().set_scrollback(scrollback_len - start);
                history.extend(self.parser.screen().rows(0, cols));
                start += rows;
            } else {
                self.parser.screen_mut().set_scrollback(0);
                history.extend(self.parser.screen().rows(0, cols).skip(start - scrollback_len));
                start = total;
            }
        }
        self.parser.screen_mut().set_scrollback(original);
        history
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
        self.scrollback_offset = self.parser.screen().scrollback();
        self.clear_selection();
    }

    /// Send raw keyboard bytes to the shell.
    pub fn write(&mut self, bytes: &[u8]) {
        if !self.alive {
            return;
        }
        self.clear_selection();
        if self.scrollback_offset > 0 {
            self.set_scrollback(0);
        }
        if self
            .writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .is_err()
        {
            self.alive = false;
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = nonzero(rows);
        let cols = nonzero(cols);
        let size = PtySize {
            rows: rows.get(),
            cols: cols.get(),
            pixel_width: 0,
            pixel_height: 0,
        };
        let _ = self.master.resize(size);
        self.parser.set_size(rows, cols);
        self.clear_selection();
        self.search.dirty = true;
    }

    pub fn is_alive(&mut self) -> bool {
        if !self.alive {
            return false;
        }
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                self.alive = false;
                false
            }
        }
    }

    pub fn title(&self) -> String {
        let title = &self.parser.callbacks().title;
        if title.is_empty() {
            self.default_title.to_string()
        } else {
            title.clone()
        }
    }

    /// Process wait state changes and output; call once per frame.
    pub fn update(&mut self) {
        self.poll();
        self.is_alive();
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // The child would otherwise keep running after its terminal tab closes.
        let _ = self.child.kill();
    }
}

/// Display width of one character in terminal cells.
pub fn char_width(ch: char) -> u16 {
    let wide = matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    );
    if wide { 2 } else { 1 }
}

/// Characters that end a clickable URL in terminal output.
fn url_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\''
                | '<'
                | '>'
                | '|'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
                | '、'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '…'
                | '—'
        )
        || ('\u{2500}'..='\u{257F}').contains(&ch)
}

/// URL under a Ctrl+click. The cell counts display columns from the left edge
/// of the row, so wide characters shift the URL start exactly like the grid.
pub fn url_at_cell(line: &str, cell: u16) -> Option<String> {
    let mut col = 0u16;
    let mut index = 0usize;
    while index < line.len() {
        let rest = &line[index..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest
                .char_indices()
                .find(|(_, ch)| url_boundary(*ch))
                .map_or(rest.len(), |(offset, _)| offset);
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            let width: u16 = url.chars().map(char_width).sum();
            if cell >= col && cell < col.saturating_add(width) {
                return Some(url.to_string());
            }
            col = col.saturating_add(rest[..end].chars().map(char_width).sum::<u16>());
            index += end;
            continue;
        }
        let ch = rest.chars().next()?;
        col = col.saturating_add(char_width(ch));
        index += ch.len_utf8();
    }
    None
}

/// Quote a path for the running shell. Only paths with shell-special
/// characters need the double quotes cmd and PowerShell both honour.
pub fn quote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let needs_quotes = text.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\''
                    | '&'
                    | '|'
                    | '<'
                    | '>'
                    | '^'
                    | '('
                    | ')'
                    | '%'
                    | ';'
                    | ','
                    | '='
                    | '!'
                    | '*'
                    | '?'
            )
    });
    if needs_quotes {
        format!("\"{text}\"")
    } else {
        text.into_owned()
    }
}

/// Open a URL with the platform default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: cmd /C start would otherwise flash a console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Cell column (0-based) of a byte offset inside a row string.
fn column_of_byte(line: &str, byte: usize) -> u16 {
    let mut col = 0u16;
    for (offset, ch) in line.char_indices() {
        if offset >= byte {
            break;
        }
        col = col.saturating_add(char_width(ch));
    }
    col
}

fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

fn spawn_shell_kind(
    slave: &dyn SlavePty,
    cwd: &Path,
    kind: ShellKind,
) -> Option<(Box<dyn Child + Send + Sync>, &'static str, ShellKind)> {
    let try_spawn = |program: &str, arg: Option<&str>| {
        let mut cmd = CommandBuilder::new(program);
        if let Some(arg) = arg {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        slave.spawn_command(cmd).ok()
    };

    match kind {
        ShellKind::PowerShell7 => try_spawn("pwsh.exe", Some("-NoLogo"))
            .map(|child| (child, "PowerShell 7", ShellKind::PowerShell7))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::PowerShell => try_spawn("powershell.exe", Some("-NoLogo"))
            .map(|child| (child, "Windows PowerShell", ShellKind::PowerShell))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::Cmd => try_spawn("cmd.exe", None)
            .map(|child| (child, "Command Prompt", ShellKind::Cmd))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::GitBash => git_bash_path()
            .and_then(|path| try_spawn(path.to_string_lossy().as_ref(), Some("--login")))
            .or_else(|| try_spawn("bash.exe", None))
            .map(|child| (child, "Git Bash", ShellKind::GitBash))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::Wsl => try_spawn("wsl.exe", None)
            .map(|child| (child, "WSL", ShellKind::Wsl))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::Default => spawn_shell_default(slave, cwd),
    }
}

fn spawn_shell_default(
    slave: &dyn SlavePty,
    cwd: &Path,
) -> Option<(Box<dyn Child + Send + Sync>, &'static str, ShellKind)> {
    let try_spawn = |program: &str, arg: Option<&str>| {
        let mut cmd = CommandBuilder::new(program);
        if let Some(arg) = arg {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        slave.spawn_command(cmd).ok()
    };

    if cfg!(windows) {
        // Default resolves to the first PowerShell the probe found.
        let first_powershell = available_profiles()
            .iter()
            .find(|profile| matches!(profile.kind, ShellKind::PowerShell7 | ShellKind::PowerShell))
            .map(|profile| profile.kind);
        if first_powershell == Some(ShellKind::PowerShell7)
            && let Some(child) = try_spawn("pwsh.exe", Some("-NoLogo"))
        {
            return Some((child, "PowerShell 7", ShellKind::PowerShell7));
        }
        if first_powershell == Some(ShellKind::PowerShell)
            && let Some(child) = try_spawn("powershell.exe", Some("-NoLogo"))
        {
            return Some((child, "Windows PowerShell", ShellKind::PowerShell));
        }
        try_spawn("cmd.exe", None).map(|child| (child, "Command Prompt", ShellKind::Cmd))
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        try_spawn(&shell, None)
            .map(|child| (child, "Shell", ShellKind::Default))
            .or_else(|| {
                try_spawn("/bin/sh", None).map(|child| (child, "sh", ShellKind::Default))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn project_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn wait_until(
        session: &mut TerminalSession,
        limit: Duration,
        mut done: impl FnMut(&mut TerminalSession) -> bool,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            session.update();
            if done(session) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        session.update();
        done(session)
    }

    #[test]
    fn spawn_and_echo() {
        fn assert_send<T: Send>() {}
        assert_send::<TerminalSession>();

        let mut term = TerminalSession::new(project_dir(), 100, 30).expect("spawn shell in ConPTY");
        // PSReadLine may still be starting; ConPTY buffers the input bytes.
        term.write(b"echo kestrel-ok\r\n");

        // The echoed command line also contains the string, so wait for the
        // command's own output line to prove the shell actually ran it.
        let seen = wait_until(&mut term, Duration::from_secs(15), |t| {
            t.screen()
                .contents()
                .lines()
                .any(|line| line.trim() == "kestrel-ok")
        });
        println!(
            "echo output visible: {seen}; screen:\n{}\n---",
            term.screen().contents()
        );
        assert!(seen, "screen never showed the echo output");

        term.write(b"exit\r\n");
        let dead = wait_until(&mut term, Duration::from_secs(10), |t| !t.is_alive());
        println!("shell reported dead after `exit`: {dead}");
        assert!(dead, "shell still alive after `exit`");
    }

    #[test]
    fn resize_updates_screen() {
        let mut term = TerminalSession::new(project_dir(), 80, 24).expect("spawn shell in ConPTY");
        term.resize(120, 40);
        let (rows, cols) = term.screen().size();
        assert_eq!((rows.get(), cols.get()), (40, 120));
    }

    #[test]
    fn clear_command_matches_shell_kind() {
        assert_eq!(ShellKind::Cmd.clear_command(), b"cls\r");
        assert_eq!(ShellKind::PowerShell.clear_command(), b"clear\r");
        assert_eq!(ShellKind::PowerShell7.clear_command(), b"clear\r");
        assert_eq!(ShellKind::GitBash.clear_command(), b"clear\r");
    }

    #[test]
    fn probe_profiles_falls_back_to_cmd() {
        let only_cmd = probe_profiles(|name| name == "cmd.exe");
        let kinds: Vec<ShellKind> = only_cmd.iter().map(|profile| profile.kind).collect();
        assert_eq!(kinds, vec![ShellKind::Cmd]);

        let none = probe_profiles(|_| false);
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].kind, ShellKind::Cmd);
        assert_eq!(none[0].label, "Command Prompt");
    }

    #[test]
    fn probe_profiles_orders_available_shells() {
        let profiles = probe_profiles(|name| {
            matches!(name, "pwsh.exe" | "powershell.exe" | "bash.exe" | "wsl.exe")
        });
        let kinds: Vec<ShellKind> = profiles.iter().map(|profile| profile.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ShellKind::PowerShell7,
                ShellKind::PowerShell,
                ShellKind::GitBash,
                ShellKind::Wsl
            ]
        );
        let labels: Vec<&str> = profiles.iter().map(|profile| profile.label).collect();
        assert_eq!(
            labels,
            vec!["PowerShell 7", "Windows PowerShell", "Git Bash", "WSL"]
        );

        // Known Git for Windows install paths satisfy the Git Bash probe.
        let git = probe_profiles(|name| name == "C:/Program Files/Git/bin/bash.exe");
        assert_eq!(git.len(), 1);
        assert_eq!(git[0].kind, ShellKind::GitBash);
    }

    #[test]
    fn url_at_cell_maps_columns_to_url() {
        let line = "visit https://example.com/docs now";
        assert_eq!(url_at_cell(line, 0), None);
        assert_eq!(
            url_at_cell(line, 6).as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(
            url_at_cell(line, 20).as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(
            url_at_cell(line, 29).as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(url_at_cell(line, 30), None);
    }

    #[test]
    fn url_at_cell_handles_tokens_and_wide_prefix() {
        let line = "中文 http://a.example/b 和 http://c.example";
        assert_eq!(url_at_cell(line, 4), None);
        assert_eq!(url_at_cell(line, 5).as_deref(), Some("http://a.example/b"));
        assert_eq!(url_at_cell(line, 26), None);
        assert_eq!(url_at_cell(line, 27).as_deref(), Some("http://c.example"));
    }

    #[test]
    fn quote_path_quotes_only_when_needed() {
        assert_eq!(quote_path(Path::new(r"C:\code\main.rs")), r"C:\code\main.rs");
        assert_eq!(
            quote_path(Path::new(r"C:\Program Files\kestrel\a.txt")),
            r#""C:\Program Files\kestrel\a.txt""#
        );
    }

    #[test]
    fn selection_copy_and_search_over_screen() {
        let mut term = TerminalSession::new(project_dir(), 100, 30).expect("spawn shell in ConPTY");
        term.write(b"echo kestrel-b5b-selection\r\n");
        let seen = wait_until(&mut term, Duration::from_secs(15), |t| {
            t.screen()
                .contents()
                .to_lowercase()
                .contains("kestrel-b5b-selection")
        });
        assert!(seen, "shell output never appeared; screen:\n{}", term.screen().contents());

        // Search is case-insensitive and sees the echoed output.
        term.open_search();
        term.search.query = "KESTREL-B5B".to_string();
        term.search.dirty = true;
        term.refresh_search();
        assert!(!term.search.matches.is_empty(), "search found no output match");
        assert!(!term.search.truncated);
        assert!(term.visible_start_row() < 30);

        // A full-screen selection copies the output; right-edge padding is trimmed.
        let (rows, cols) = term.screen().size();
        term.begin_selection(0, 0);
        term.update_selection(rows.get() - 1, cols.get() - 1);
        let text = term.selection_text().expect("selection text");
        assert!(
            text.to_lowercase().contains("kestrel-b5b-selection"),
            "selection text: {text:?}"
        );
        assert!(
            !text.lines().any(|line| line.ends_with(' ')),
            "trailing blanks not trimmed: {text:?}"
        );
        term.write(b"exit\r\n");
    }
}
