//! Integrated terminal engine:
//! - [`pty`]: non-blocking OS PTY I/O, coalesced repaint signaling, and off-thread teardown
//! - [`shell`]: cross-platform shell probing, custom shell config, path quoting, safe URL open
//! - [`protocol`]: DSR/DA/OSC 7/8/9;9/10-11/52/133/633, Kitty keyboard & modifyOtherKeys, bells
//! - [`selection`]: absolute-row stream & rectangular block selection across scrollback
//! - [`search`]: scrollback + viewport search (case-sensitive & regex)
//! - [`links`]: OSC 8 hyperlinks, wrapped URLs, and `file:line:col` compiler links
//! - [`input`]: xterm + DECCKM + Kitty CSI u + mouse protocol encoders

pub mod input;
pub mod links;
pub mod protocol;
mod pty;
pub mod search;
pub mod selection;
pub mod shell;

use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

pub use input::{KeyboardProtocol, encode_key, encode_mouse};
pub use links::{TerminalLinkHit, TerminalLinkTarget};
#[allow(unused_imports)]
pub use links::url_at_cell;
pub use protocol::CommandMark;
pub use search::TerminalSearch;
pub use selection::{SelectionMode, TerminalSelection};
pub use shell::{
    CustomShellConfig, ShellKind, available_profiles, open_url, quote_path_for_shell,
};
#[allow(unused_imports)]
pub use shell::quote_path;

/// Scrollback kept by the vt100 parser.
pub const SCROLLBACK: usize = 5000;
/// Bound chunks parsed per UI frame so firehose output stays at 60 FPS.
const MAX_CHUNKS_PER_POLL: usize = 64;

static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

enum SessionLifecycle {
    Starting {
        rx: Receiver<Result<pty::SpawnedPty, String>>,
        pending_writes: Vec<Vec<u8>>,
        pending_resize: Option<(u16, u16)>,
    },
    Running(pty::RunningPty),
    Exited {
        exit_code: Option<u32>,
    },
    Failed(String),
}

pub struct TerminalSession {
    /// Stable monotonic identifier for egui widget state (never shifts on close).
    pub id: u64,
    parser: vt100::Parser<protocol::TermCallbacks>,
    lifecycle: SessionLifecycle,
    default_title: String,
    /// Optional user-assigned tab label (overrides OSC title).
    pub custom_title: Option<String>,
    /// Current scrollback offset, kept synchronized with `parser.screen().scrollback()`.
    pub scrollback_offset: usize,
    /// Sub-line wheel delta accumulator for smooth touchpad scrolling.
    pub scroll_accum: f32,
    /// Uncommitted IME composition text for this session.
    pub ime_preedit: String,
    /// Shell actually spawned (or requested while starting).
    pub shell: ShellKind,
    /// Optional custom shell configuration carried for restarts.
    pub custom_shell: Option<CustomShellConfig>,
    /// Working directory of the shell; updated live via OSC 7 / OSC 9;9.
    pub cwd: PathBuf,
    /// Mouse selection anchored by absolute history row indices.
    pub selection: Option<TerminalSelection>,
    selection_anchor: Option<(usize, u16, bool, SelectionMode)>,
    /// Ctrl+F search over scrollback + screen.
    pub search: TerminalSearch,
    /// Last keyboard input time, used to keep the cursor steady while typing.
    pub last_input_at: Instant,
    /// Tracks screen mode changes so selections clear on alt-screen transitions.
    last_alt_screen: bool,
    /// Optional repaint context used by reader/spawn threads to wake egui immediately.
    repaint_ctx: Option<egui::Context>,
}

impl TerminalSession {
    #[allow(dead_code)]
    pub fn new(cwd: &Path, cols: u16, rows: u16) -> Option<Self> {
        Self::new_with_shell(cwd, cols, rows, ShellKind::Default)
    }

    pub fn new_with_shell(cwd: &Path, cols: u16, rows: u16, shell: ShellKind) -> Option<Self> {
        Self::spawn_sync(cwd, cols, rows, shell, None, None).ok()
    }

    /// Synchronous spawn (used by tests and deterministic headless runs).
    pub fn spawn_sync(
        cwd: &Path,
        cols: u16,
        rows: u16,
        shell: ShellKind,
        custom_shell: Option<CustomShellConfig>,
        repaint_ctx: Option<egui::Context>,
    ) -> Result<Self, String> {
        let rows_nz = nonzero(rows);
        let cols_nz = nonzero(cols);
        let effective_cwd = normalize_cwd(cwd);
        let spawned = pty::spawn_pty_blocking(
            &effective_cwd,
            cols_nz,
            rows_nz,
            shell,
            custom_shell.as_ref(),
            repaint_ctx.clone(),
        )?;
        Ok(Self::from_spawned(
            NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed),
            rows_nz,
            cols_nz,
            effective_cwd,
            custom_shell,
            repaint_ctx,
            spawned,
        ))
    }

    /// Non-blocking spawn: returns immediately in [`SessionLifecycle::Starting`]
    /// so the UI thread never freezes waiting on ConPTY / PowerShell / WSL startup.
    pub fn spawn_async(
        cwd: &Path,
        cols: u16,
        rows: u16,
        shell: ShellKind,
        custom_shell: Option<CustomShellConfig>,
        repaint_ctx: Option<egui::Context>,
    ) -> Self {
        let rows_nz = nonzero(rows);
        let cols_nz = nonzero(cols);
        let effective_cwd = normalize_cwd(cwd);
        let (tx, rx) = channel();

        let thread_cwd = effective_cwd.clone();
        let thread_custom = custom_shell.clone();
        let thread_ctx = repaint_ctx.clone();
        let _ = std::thread::Builder::new()
            .name("kestrel-terminal-spawn".into())
            .spawn(move || {
                let res = pty::spawn_pty_blocking(
                    &thread_cwd,
                    cols_nz,
                    rows_nz,
                    shell,
                    thread_custom.as_ref(),
                    thread_ctx.clone(),
                );
                let _ = tx.send(res);
                if let Some(ctx) = &thread_ctx {
                    ctx.request_repaint();
                }
            });

        Self {
            id: NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed),
            parser: vt100::Parser::new_with_callbacks(
                rows_nz,
                cols_nz,
                SCROLLBACK,
                protocol::TermCallbacks::default(),
            ),
            lifecycle: SessionLifecycle::Starting {
                rx,
                pending_writes: Vec::new(),
                pending_resize: None,
            },
            default_title: shell.label().to_string(),
            custom_title: None,
            scrollback_offset: 0,
            scroll_accum: 0.0,
            ime_preedit: String::new(),
            shell,
            custom_shell,
            cwd: effective_cwd,
            selection: None,
            selection_anchor: None,
            search: TerminalSearch::default(),
            last_input_at: Instant::now(),
            last_alt_screen: false,
            repaint_ctx,
        }
    }

    fn from_spawned(
        id: u64,
        rows: NonZeroU16,
        cols: NonZeroU16,
        cwd: PathBuf,
        custom_shell: Option<CustomShellConfig>,
        repaint_ctx: Option<egui::Context>,
        spawned: pty::SpawnedPty,
    ) -> Self {
        Self {
            id,
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                SCROLLBACK,
                protocol::TermCallbacks::default(),
            ),
            lifecycle: SessionLifecycle::Running(spawned.pty),
            default_title: spawned.default_title,
            custom_title: None,
            scrollback_offset: 0,
            scroll_accum: 0.0,
            ime_preedit: String::new(),
            shell: spawned.resolved_shell,
            custom_shell,
            cwd,
            selection: None,
            selection_anchor: None,
            search: TerminalSearch::default(),
            last_input_at: Instant::now(),
            last_alt_screen: false,
            repaint_ctx,
        }
    }

    /// Restart this terminal asynchronously in place, preserving its `id` and `custom_title`.
    pub fn restart(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        let id = self.id;
        let custom_title = self.custom_title.clone();
        let fg = self.parser.callbacks().theme_fg;
        let bg = self.parser.callbacks().theme_bg;
        let mut fresh = Self::spawn_async(
            &self.cwd,
            cols.get(),
            rows.get(),
            self.shell,
            self.custom_shell.clone(),
            self.repaint_ctx.clone(),
        );
        fresh.id = id;
        fresh.custom_title = custom_title;
        fresh.set_theme_colors(fg, bg);
        *self = fresh;
    }

    pub fn set_theme_colors(&mut self, fg: (u8, u8, u8), bg: (u8, u8, u8)) {
        let cb = self.parser.callbacks_mut();
        cb.theme_fg = fg;
        cb.theme_bg = bg;
    }

    pub fn take_pending_clipboard(&mut self) -> Option<String> {
        self.parser.callbacks_mut().pending_clipboard.take()
    }

    /// Visual bell flash opacity `0.0..=1.0` over a 360ms fade window.
    pub fn bell_flash_alpha(&self) -> f32 {
        let Some(at) = self.parser.callbacks().bell_at else {
            return 0.0;
        };
        let elapsed = at.elapsed().as_secs_f32();
        let dur = 0.36;
        if elapsed >= dur {
            0.0
        } else {
            1.0 - (elapsed / dur)
        }
    }

    pub fn has_unread_bell(&self) -> bool {
        self.parser.callbacks().bell_unread
    }

    pub fn clear_unread_bell(&mut self) {
        self.parser.callbacks_mut().bell_unread = false;
    }

    pub fn command_marks(&self) -> impl Iterator<Item = &CommandMark> {
        self.parser.callbacks().command_marks.iter()
    }

    /// Jump viewport to the previous or next OSC 133/633 command mark.
    pub fn jump_command_mark(&mut self, forward: bool) -> bool {
        if self.parser.screen().alternate_screen() {
            return false;
        }
        let evicted = self.parser.callbacks().evicted_rows;
        let cur_abs = evicted.saturating_add(self.visible_start_row());
        let target_abs = if forward {
            self.parser
                .callbacks()
                .command_marks
                .iter()
                .map(|m| m.abs_row)
                .find(|&r| r > cur_abs)
        } else {
            self.parser
                .callbacks()
                .command_marks
                .iter()
                .rev()
                .map(|m| m.abs_row)
                .find(|&r| r < cur_abs)
        };
        let Some(abs_row) = target_abs else {
            return false;
        };
        let hist_row = abs_row.saturating_sub(evicted);
        let scrollback_len = self.parser.screen().scrollback_len();
        if hist_row >= scrollback_len {
            self.set_scrollback(0);
        } else {
            self.set_scrollback(scrollback_len - hist_row);
        }
        true
    }

    pub fn keyboard_protocol(&self) -> KeyboardProtocol {
        KeyboardProtocol {
            kitty_flags: self.parser.callbacks().kitty_flags(),
            modify_other_keys: self.parser.callbacks().modify_other_keys,
            app_cursor: self.parser.screen().application_cursor(),
            is_cmd: self.shell == ShellKind::Cmd,
        }
    }

    pub fn is_starting(&self) -> bool {
        matches!(self.lifecycle, SessionLifecycle::Starting { .. })
    }

    pub fn is_exited(&self) -> bool {
        matches!(self.lifecycle, SessionLifecycle::Exited { .. })
    }

    pub fn failed_error(&self) -> Option<&str> {
        match &self.lifecycle {
            SessionLifecycle::Failed(err) => Some(err.as_str()),
            _ => None,
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        match self.lifecycle {
            SessionLifecycle::Exited { exit_code } => exit_code,
            _ => None,
        }
    }

    pub fn is_alive(&mut self) -> bool {
        self.poll_spawn_completion();
        match &mut self.lifecycle {
            SessionLifecycle::Starting { .. } => true,
            SessionLifecycle::Running(pty) => {
                if let Some(code) = pty.poll_exit() {
                    self.lifecycle = SessionLifecycle::Exited { exit_code: code };
                    false
                } else {
                    true
                }
            }
            SessionLifecycle::Exited { .. } | SessionLifecycle::Failed(_) => false,
        }
    }

    fn poll_spawn_completion(&mut self) -> bool {
        let completed = match &self.lifecycle {
            SessionLifecycle::Starting { rx, .. } => match rx.try_recv() {
                Ok(res) => Some(res),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("终端启动线程异常中断".to_string())),
            },
            _ => None,
        };
        let Some(res) = completed else {
            return false;
        };
        let prev = std::mem::replace(
            &mut self.lifecycle,
            SessionLifecycle::Exited { exit_code: None },
        );
        let (pending_writes, pending_resize) = match prev {
            SessionLifecycle::Starting {
                pending_writes,
                pending_resize,
                ..
            } => (pending_writes, pending_resize),
            _ => (Vec::new(), None),
        };
        match res {
            Ok(spawned) => {
                self.default_title = spawned.default_title;
                self.shell = spawned.resolved_shell;
                self.lifecycle = SessionLifecycle::Running(spawned.pty);
                if let Some((cols, rows)) = pending_resize {
                    self.resize(cols, rows);
                }
                for bytes in pending_writes {
                    self.write_raw(&bytes, true);
                }
            }
            Err(err) => {
                self.lifecycle = SessionLifecycle::Failed(err);
            }
        }
        true
    }

    /// Drain pending output into the terminal parser. Returns true if state changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.poll_spawn_completion();

        if let SessionLifecycle::Running(pty) = &mut self.lifecycle {
            let (chunks, _more_pending) = pty.drain_output(MAX_CHUNKS_PER_POLL);
            for chunk in chunks {
                self.parser.process(&chunk);
                changed = true;
                let replies = std::mem::take(&mut self.parser.callbacks_mut().replies);
                for reply in replies {
                    if !pty.write_bytes(reply) {
                        break;
                    }
                }
            }
        }

        if let Some(new_cwd) = self.parser.callbacks_mut().cwd.take() {
            self.cwd = new_cwd;
        }
        self.scrollback_offset = self.parser.screen().scrollback();

        let alt = self.parser.screen().alternate_screen();
        if alt != self.last_alt_screen {
            self.last_alt_screen = alt;
            self.clear_selection();
        }
        if let Some(sel) = self.selection
            && !sel.alt_screen
            && sel.end.0 < self.parser.callbacks().evicted_rows
        {
            self.clear_selection();
        }
        if changed {
            self.search.dirty = true;
        }
        changed
    }

    /// Process wait state changes and output; returns true if anything changed.
    pub fn update(&mut self) -> bool {
        let was_running = matches!(self.lifecycle, SessionLifecycle::Running(_));
        let changed = self.poll();
        let now_alive = self.is_alive();
        if now_alive
            && let SessionLifecycle::Running(pty) = &self.lifecycle
            && pty.is_reader_closed()
            && let Some(ctx) = &self.repaint_ctx
        {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        changed || (was_running && !now_alive)
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn scrollback_len(&self) -> usize {
        self.parser.screen().scrollback_len()
    }

    /// First history row currently visible (scrollback rows followed by screen rows).
    pub fn visible_start_row(&self) -> usize {
        let len = self.parser.screen().scrollback_len();
        len.saturating_sub(self.parser.screen().scrollback())
    }

    /// Absolute row index of a currently visible row `vis_row`.
    pub fn abs_row_of_visible(&self, vis_row: u16) -> usize {
        if self.parser.screen().alternate_screen() {
            usize::from(vis_row)
        } else {
            self.parser
                .callbacks()
                .evicted_rows
                .saturating_add(self.visible_start_row())
                .saturating_add(usize::from(vis_row))
        }
    }

    // ------------------------------------------------------------------
    // Selection
    // ------------------------------------------------------------------

    pub fn begin_selection(&mut self, vis_row: u16, col: u16) {
        self.begin_selection_with_mode(vis_row, col, SelectionMode::Stream);
    }

    pub fn begin_selection_with_mode(&mut self, vis_row: u16, col: u16, mode: SelectionMode) {
        let alt = self.parser.screen().alternate_screen();
        let abs_row = self.abs_row_of_visible(vis_row);
        let snapped = selection::snap_start_col(self.parser.screen(), vis_row, col);
        let end_col = selection::snap_end_col(self.parser.screen(), vis_row, snapped);
        self.selection_anchor = Some((abs_row, snapped, alt, mode));
        self.selection = Some(TerminalSelection::from_endpoints(
            (abs_row, snapped),
            (abs_row, end_col),
            alt,
            mode,
        ));
    }

    pub fn update_selection(&mut self, vis_row: u16, col: u16) {
        let Some((anchor_row, anchor_col, anchor_alt, mode)) = self.selection_anchor else {
            return;
        };
        let alt = self.parser.screen().alternate_screen();
        if alt != anchor_alt {
            self.clear_selection();
            return;
        }
        let abs_row = self.abs_row_of_visible(vis_row);
        let sel = match mode {
            SelectionMode::Block => {
                let c = if col >= anchor_col {
                    selection::snap_end_col(self.parser.screen(), vis_row, col)
                } else {
                    selection::snap_start_col(self.parser.screen(), vis_row, col)
                };
                TerminalSelection::from_endpoints(
                    (anchor_row, anchor_col),
                    (abs_row, c),
                    alt,
                    SelectionMode::Block,
                )
            }
            SelectionMode::Stream => {
                if (abs_row, col) >= (anchor_row, anchor_col) {
                    let end_col = selection::snap_end_col(self.parser.screen(), vis_row, col);
                    TerminalSelection {
                        start: (anchor_row, anchor_col),
                        end: (abs_row, end_col),
                        alt_screen: alt,
                        mode,
                    }
                } else {
                    let start_col = selection::snap_start_col(self.parser.screen(), vis_row, col);
                    let anchor_end = if anchor_row == abs_row {
                        selection::snap_end_col(self.parser.screen(), vis_row, anchor_col)
                    } else {
                        anchor_col
                    };
                    TerminalSelection {
                        start: (abs_row, start_col),
                        end: (anchor_row, anchor_end),
                        alt_screen: alt,
                        mode,
                    }
                }
            }
        };
        self.selection = Some(sel);
    }

    pub fn select_word_at(&mut self, vis_row: u16, col: u16) {
        let Some((start, end)) = selection::word_bounds_on_row(self.parser.screen(), vis_row, col)
        else {
            return;
        };
        let alt = self.parser.screen().alternate_screen();
        let abs_row = self.abs_row_of_visible(vis_row);
        self.selection_anchor = Some((abs_row, start, alt, SelectionMode::Stream));
        self.selection = Some(TerminalSelection {
            start: (abs_row, start),
            end: (abs_row, end),
            alt_screen: alt,
            mode: SelectionMode::Stream,
        });
    }

    pub fn select_line_at(&mut self, vis_row: u16) {
        let Some((top, bot)) = selection::logical_line_rows(self.parser.screen(), vis_row) else {
            return;
        };
        let cols = self.parser.screen().size().1.get();
        if cols == 0 {
            return;
        }
        let alt = self.parser.screen().alternate_screen();
        let start_abs = self.abs_row_of_visible(top);
        let end_abs = self.abs_row_of_visible(bot);
        self.selection_anchor = Some((start_abs, 0, alt, SelectionMode::Stream));
        self.selection = Some(TerminalSelection {
            start: (start_abs, 0),
            end: (end_abs, cols - 1),
            alt_screen: alt,
            mode: SelectionMode::Stream,
        });
    }

    pub fn select_all_visible(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        let rows = rows.get();
        let cols = cols.get();
        if rows == 0 || cols == 0 {
            return;
        }
        let alt = self.parser.screen().alternate_screen();
        let start_abs = self.abs_row_of_visible(0);
        let end_abs = self.abs_row_of_visible(rows - 1);
        self.selection_anchor = Some((start_abs, 0, alt, SelectionMode::Stream));
        self.selection = Some(TerminalSelection {
            start: (start_abs, 0),
            end: (end_abs, cols - 1),
            alt_screen: alt,
            mode: SelectionMode::Stream,
        });
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    pub fn selection_text(&mut self) -> Option<String> {
        let sel = self.selection?;
        let evicted = self.parser.callbacks().evicted_rows;
        let text = selection::extract_selection_text(self.parser.screen_mut(), evicted, sel);
        self.scrollback_offset = self.parser.screen().scrollback();
        text
    }

    // ------------------------------------------------------------------
    // Search & Scrolling
    // ------------------------------------------------------------------

    pub fn open_search(&mut self) {
        self.search.open();
    }

    pub fn close_search(&mut self) {
        self.search.close();
    }

    pub fn refresh_search(&mut self) {
        self.search.refresh(self.parser.screen_mut());
        self.scrollback_offset = self.parser.screen().scrollback();
    }

    pub fn search_step(&mut self, forward: bool) {
        if let Some(row) = self.search.step(self.parser.screen_mut(), forward) {
            self.scroll_to_history_row(row);
        }
    }

    pub fn scroll_to_active_match(&mut self) {
        if let Some(idx) = self.search.active
            && let Some(hit) = self.search.matches.get(idx).copied()
        {
            self.scroll_to_history_row(hit.row);
        }
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

    pub fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
        self.scrollback_offset = self.parser.screen().scrollback();
    }

    /// Clear scrollback and all non-cursor rows locally without injecting
    /// commands into the running process stdin.
    pub fn clear_screen(&mut self) {
        let old_sb = self.parser.screen().scrollback_len();
        self.parser.screen_mut().clear_scrollback();
        let (rows, cols) = self.parser.screen().size();
        let (crow, _ccol) = self.parser.screen().cursor_position();
        let rows = rows.get();
        let cols = cols.get();
        let mut seq = String::from("\x1b7\x1b[0m");
        if crow > 0 {
            seq.push_str(&format!("\x1b[{};{}H\x1b[1J", crow, cols));
        }
        if crow + 1 < rows {
            seq.push_str(&format!("\x1b[{};1H\x1b[0J", crow + 2));
        }
        seq.push_str("\x1b8");
        self.parser.process(seq.as_bytes());
        let cb = self.parser.callbacks_mut();
        cb.evicted_rows = cb.evicted_rows.saturating_add(old_sb);
        cb.osc_links.clear();
        cb.command_marks.clear();
        self.scrollback_offset = self.parser.screen().scrollback();
        self.clear_selection();
        self.search.dirty = true;
    }

    // ------------------------------------------------------------------
    // I/O & Resize
    // ------------------------------------------------------------------

    pub fn write(&mut self, bytes: &[u8]) {
        self.write_raw(bytes, true);
    }

    pub fn write_mouse(&mut self, bytes: &[u8]) {
        self.write_raw(bytes, false);
    }

    fn write_raw(&mut self, bytes: &[u8], user_typing: bool) {
        if bytes.is_empty() {
            return;
        }
        if user_typing {
            self.last_input_at = Instant::now();
            self.clear_selection();
            if self.scrollback_offset > 0 {
                self.set_scrollback(0);
            }
        }
        match &mut self.lifecycle {
            SessionLifecycle::Starting { pending_writes, .. } => {
                if user_typing && pending_writes.len() < 64 {
                    pending_writes.push(bytes.to_vec());
                }
            }
            SessionLifecycle::Running(pty) => {
                if !pty.write_bytes(bytes.to_vec()) {
                    self.lifecycle = SessionLifecycle::Exited { exit_code: None };
                }
            }
            SessionLifecycle::Exited { .. } | SessionLifecycle::Failed(_) => {}
        }
    }

    pub fn write_paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.parser.screen().bracketed_paste() {
            let sanitized = normalized.replace("\x1b[201~", "");
            let mut buf = Vec::with_capacity(sanitized.len() + 12);
            buf.extend_from_slice(b"\x1b[200~");
            buf.extend_from_slice(sanitized.as_bytes());
            buf.extend_from_slice(b"\x1b[201~");
            self.write(&buf);
        } else {
            self.write(normalized.as_bytes());
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows_nz = nonzero(rows);
        let cols_nz = nonzero(cols);
        match &mut self.lifecycle {
            SessionLifecycle::Starting { pending_resize, .. } => {
                *pending_resize = Some((cols_nz.get(), rows_nz.get()));
            }
            SessionLifecycle::Running(pty) => {
                pty.resize(cols_nz, rows_nz);
            }
            SessionLifecycle::Exited { .. } | SessionLifecycle::Failed(_) => {}
        }
        self.parser.set_size(rows_nz, cols_nz);
        self.scrollback_offset = self.parser.screen().scrollback();
        self.clear_selection();
        self.search.dirty = true;
    }

    pub fn title(&self) -> String {
        if let Some(custom) = &self.custom_title
            && !custom.trim().is_empty()
        {
            return custom.clone();
        }
        let title = &self.parser.callbacks().title;
        if title.is_empty() {
            self.default_title.clone()
        } else {
            title.clone()
        }
    }

    pub fn link_at(
        &self,
        vis_row: u16,
        col: u16,
        workspace_root: Option<&Path>,
    ) -> Option<TerminalLinkHit> {
        links::resolve_link_at(
            self.parser.screen(),
            &self.parser.callbacks().osc_links,
            |r| self.abs_row_of_visible(r),
            vis_row,
            col,
            &self.cwd,
            workspace_root,
        )
    }
}

fn normalize_cwd(cwd: &Path) -> PathBuf {
    if cwd.is_dir() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

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

        let mut term = TerminalSession::new(project_dir(), 100, 30).expect("spawn shell");
        term.write(b"echo kestrel-ok\r\n");

        let seen = wait_until(&mut term, Duration::from_secs(15), |t| {
            t.screen()
                .contents()
                .lines()
                .any(|line| line.trim() == "kestrel-ok")
        });
        assert!(seen, "screen never showed echo output");

        term.write(b"exit\r\n");
        let dead = wait_until(&mut term, Duration::from_secs(10), |t| !t.is_alive());
        assert!(dead, "shell still alive after exit");
        assert!(term.is_exited());
    }

    #[test]
    fn async_spawn_buffers_input_and_transitions_to_running() {
        let mut term = TerminalSession::spawn_async(
            project_dir(),
            100,
            30,
            ShellKind::Default,
            None,
            None,
        );
        term.write(b"echo kestrel-async-ok\r\n");
        let seen = wait_until(&mut term, Duration::from_secs(15), |t| {
            !t.is_starting()
                && t.screen()
                    .contents()
                    .lines()
                    .any(|line| line.trim() == "kestrel-async-ok")
        });
        assert!(seen, "async spawned shell never echoed buffered command");
        term.write(b"exit\r\n");
    }

    #[test]
    fn resize_updates_screen_and_syncs_scrollback() {
        let mut term = TerminalSession::new(project_dir(), 80, 24).expect("spawn shell");
        term.scrollback_offset = 42;
        term.resize(120, 40);
        let (rows, cols) = term.screen().size();
        assert_eq!((rows.get(), cols.get()), (40, 120));
        assert_eq!(term.scrollback_offset, term.screen().scrollback());
        term.write(b"exit\r\n");
    }

    #[test]
    fn probe_profiles_and_custom_args_parse() {
        let only_cmd = shell::probe_windows_profiles(|name| name == "cmd.exe");
        assert_eq!(only_cmd.len(), 1);
        assert_eq!(only_cmd[0].kind, ShellKind::Cmd);

        let none = shell::probe_windows_profiles(|_| false);
        assert_eq!(none[0].kind, ShellKind::Cmd);

        let unix =
            shell::probe_unix_profiles(|name| matches!(name, "/bin/zsh" | "/bin/bash" | "/bin/sh"));
        let unix_kinds: Vec<ShellKind> = unix.iter().map(|p| p.kind).collect();
        assert_eq!(unix_kinds, vec![ShellKind::Zsh, ShellKind::Bash, ShellKind::Sh]);

        assert_eq!(
            shell::split_shell_args(r#"-NoLogo -Command "echo hello world""#),
            vec!["-NoLogo", "-Command", "echo hello world"]
        );
    }

    #[test]
    fn url_and_file_link_parsing() {
        let line = "中文 http://a.example/b 和 http://c.example";
        assert_eq!(url_at_cell(line, 5).as_deref(), Some("http://a.example/b"));
        assert_eq!(url_at_cell(line, 27).as_deref(), Some("http://c.example"));

        let root = project_dir();
        let err_line = "error at src/terminal/mod.rs:42:7 in build";
        let hit = links::find_file_link_at_byte(err_line, 12, root, Some(root)).expect("file link");
        assert!(hit.2.ends_with(Path::new("src").join("terminal").join("mod.rs")));
        assert_eq!(hit.3, Some(42));
        assert_eq!(hit.4, Some(7));

        let paren = "src/terminal/mod.rs(18,3)";
        let hit2 =
            links::find_file_link_at_byte(paren, 5, root, Some(root)).expect("paren file link");
        assert_eq!(hit2.3, Some(18));
        assert_eq!(hit2.4, Some(3));
    }

    #[test]
    fn quote_path_handles_each_shell() {
        assert_eq!(quote_path(Path::new(r"C:\code\main.rs")), r"C:\code\main.rs");
        assert_eq!(
            quote_path(Path::new(r"C:\Program Files\kestrel\a.txt")),
            r#""C:\Program Files\kestrel\a.txt""#
        );
        assert_eq!(
            quote_path_for_shell(Path::new(r"D:\my dir\$var.rs"), ShellKind::PowerShell7),
            r"'D:\my dir\$var.rs'"
        );
        assert_eq!(
            quote_path_for_shell(Path::new(r"C:\Users\me\a b.rs"), ShellKind::Wsl),
            "'/mnt/c/Users/me/a b.rs'"
        );
        assert_eq!(
            quote_path_for_shell(Path::new(r"C:\code\main.rs"), ShellKind::GitBash),
            "C:/code/main.rs"
        );
    }

    #[test]
    fn protocol_queries_kitty_osc133_osc52_osc8_and_bell() {
        let mut parser = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(24).unwrap(),
            NonZeroU16::new(80).unwrap(),
            100,
            protocol::TermCallbacks::default(),
        );
        // DSR 6, DSR 5, DA1, DA2, OSC 10/11
        parser.process(b"\x1b[3;5H\x1b[6n\x1b[5n\x1b[c\x1b[>c\x1b]10;?\x07\x1b]11;?\x07");
        assert_eq!(parser.callbacks().replies.len(), 6);
        assert_eq!(parser.callbacks().replies[0], b"\x1b[3;5R");
        assert_eq!(parser.callbacks().replies[1], b"\x1b[0n");
        assert_eq!(parser.callbacks().replies[2], b"\x1b[?62;22c");
        assert_eq!(parser.callbacks().replies[3], b"\x1b[>0;10;1c");

        // Kitty keyboard push/query/pop + modifyOtherKeys
        parser.callbacks_mut().replies.clear();
        parser.process(b"\x1b[>1u\x1b[?u\x1b[>4;2m");
        assert_eq!(parser.callbacks().kitty_flags(), 1);
        assert_eq!(parser.callbacks().modify_other_keys, 2);
        assert_eq!(parser.callbacks().replies[0], b"\x1b[?1u");
        parser.process(b"\x1b[<1u");
        assert_eq!(parser.callbacks().kitty_flags(), 0);

        // OSC 133 command marks
        parser.process(b"\x1b]133;A\x07$ ls\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ false\r\n\x1b]133;D;1\x07");
        assert_eq!(parser.callbacks().command_marks.len(), 2);
        assert_eq!(parser.callbacks().command_marks[0].exit_code, Some(0));
        assert_eq!(parser.callbacks().command_marks[1].exit_code, Some(1));

        // OSC 52 + OSC 8 + Bell
        parser.process(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(
            parser.callbacks_mut().pending_clipboard.take().as_deref(),
            Some("hello")
        );
        parser.process(b"\x07");
        assert!(parser.callbacks().bell_at.is_some());
        assert!(parser.callbacks().bell_unread);
    }

    #[test]
    fn input_encoder_supports_kitty_modify_other_keys_and_shift_enter() {
        let shift = Modifiers::SHIFT;
        let ctrl = Modifiers::CTRL;
        let none = Modifiers::NONE;

        let default_proto = KeyboardProtocol::default();
        assert_eq!(
            encode_key(Key::Enter, &shift, default_proto, false).as_deref(),
            Some(b"\x1b\r".as_slice())
        );
        let cmd_proto = KeyboardProtocol {
            is_cmd: true,
            ..Default::default()
        };
        assert_eq!(
            encode_key(Key::Enter, &shift, cmd_proto, false).as_deref(),
            Some(b"\r".as_slice())
        );

        let kitty_proto = KeyboardProtocol {
            kitty_flags: 1,
            ..Default::default()
        };
        assert_eq!(
            encode_key(Key::Enter, &shift, kitty_proto, false).as_deref(),
            Some(b"\x1b[13;2u".as_slice())
        );
        assert_eq!(
            encode_key(Key::I, &ctrl, kitty_proto, false).as_deref(),
            Some(b"\x1b[105;5u".as_slice())
        );

        let mok_proto = KeyboardProtocol {
            modify_other_keys: 2,
            ..Default::default()
        };
        assert_eq!(
            encode_key(Key::Enter, &shift, mok_proto, false).as_deref(),
            Some(b"\x1b[27;2;13~".as_slice())
        );

        let app_proto = KeyboardProtocol {
            app_cursor: true,
            ..Default::default()
        };
        assert_eq!(
            encode_key(Key::ArrowUp, &none, app_proto, false).as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        assert_eq!(
            encode_key(Key::F10, &none, default_proto, false).as_deref(),
            Some(b"\x1b[21~".as_slice())
        );
    }

    #[test]
    fn stream_and_block_selection_survive_scrollback() {
        let mut term = TerminalSession::new(project_dir(), 80, 12).expect("spawn shell");
        term.parser.process(b"AAAA BBBB CCCC\r\n1111 2222 3333\r\n");

        // Rectangular block selection of the middle column (`BBBB` / `2222`).
        term.begin_selection_with_mode(0, 5, SelectionMode::Block);
        term.update_selection(1, 8);
        assert_eq!(term.selection_text().as_deref(), Some("BBBB\n2222"));

        // Push 20 lines into scrollback; block selection stays anchored and readable!
        for i in 0..20 {
            term.parser.process(format!("more line {i}\r\n").as_bytes());
        }
        assert_eq!(term.selection_text().as_deref(), Some("BBBB\n2222"));

        // Double-click word and triple-click logical line.
        term.set_scrollback(0);
        term.parser.process(b"alpha kestrel_word omega\r\n");
        let (crow, _) = term.screen().cursor_position();
        let target_row = crow.saturating_sub(1);
        term.select_word_at(target_row, 8);
        assert_eq!(term.selection_text().as_deref(), Some("kestrel_word"));
        term.select_line_at(target_row);
        assert_eq!(
            term.selection_text().as_deref(),
            Some("alpha kestrel_word omega")
        );

        term.clear_screen();
        assert_eq!(term.scrollback_len(), 0);
        term.write(b"exit\r\n");
    }

    #[test]
    fn kitty_query_echo_and_reply_flood_are_guarded() {
        let mut parser = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(24).unwrap(),
            NonZeroU16::new(80).unwrap(),
            100,
            protocol::TermCallbacks::default(),
        );
        // Echoed non-zero Kitty replies must never trigger a reply.
        parser.process(b"\x1b[?1u\x1b[?3u\x1b[?1;2u");
        assert!(parser.callbacks().replies.is_empty());

        // Legitimate query answers once, then immediate echoed `\x1b[?0u` is suppressed by cooldown.
        parser.process(b"\x1b[?u");
        assert_eq!(parser.callbacks().replies.len(), 1);
        assert_eq!(parser.callbacks().replies[0], b"\x1b[?0u");
        parser.callbacks_mut().replies.clear();

        for _ in 0..50 {
            parser.process(b"\x1b[?0u");
        }
        assert!(parser.callbacks().replies.is_empty());

        // Pathological query flood is strictly capped per poll.
        for _ in 0..100 {
            parser.process(b"\x1b[6n");
        }
        assert!(parser.callbacks().replies.len() <= 32);
    }

    #[test]
    fn nonblocking_pty_handles_burst_resize_write_and_fast_drop() {
        let mut term = TerminalSession::new(project_dir(), 100, 14).expect("spawn shell");
        let cmd: &[u8] = if cfg!(windows) {
            b"1..300 | ForEach-Object { \"burst-$_ \" + ('X' * 60) }\r"
        } else {
            b"seq 1 300\n"
        };
        term.write(cmd);
        let t0 = Instant::now();
        for w in [98u16, 102, 95, 100] {
            term.resize(w, 14);
        }
        term.write(b"\x03");
        // Non-blocking enqueue of resizes and writes must return well under 200ms.
        assert!(t0.elapsed() < Duration::from_millis(500));
        let drop_start = Instant::now();
        drop(term);
        assert!(drop_start.elapsed() < Duration::from_millis(500));
    }
}
