//! vt100 callback layer: terminal queries (DSR/DA/OSC 10-11), OSC 7/9;9 CWD,
//! OSC 8 hyperlinks, OSC 52 clipboard, OSC 133/633 Shell Integration marks,
//! Kitty Keyboard Protocol / modifyOtherKeys negotiation, and visual bells.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_OSC_LINKS: usize = 512;
const MAX_COMMAND_MARKS: usize = 512;
const MAX_KITTY_STACK: usize = 16;
const MAX_REPLIES_PER_POLL: usize = 32;
const MAX_TITLE_CHARS: usize = 128;
const MAX_OSC52_RAW_BYTES: usize = 1_400_000;
const KITTY_QUERY_COOLDOWN: Duration = Duration::from_millis(150);

#[derive(Clone, Debug)]
pub(crate) struct OscLink {
    pub abs_row_start: usize,
    pub col_start: u16,
    pub abs_row_end: usize,
    pub col_end: u16,
    pub url: String,
    pub alt_screen: bool,
}

/// One command recorded via OSC 133 (FinalTerm) or OSC 633 (VS Code) Shell Integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandMark {
    /// Absolute primary-screen history row of the command prompt / start.
    pub abs_row: usize,
    /// Exit status reported by `OSC 133;D;<code>`, or `None` if still running / unspecified.
    pub exit_code: Option<i32>,
    /// True once `D` (command finished) has been observed.
    pub finished: bool,
}

pub(crate) struct TermCallbacks {
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub replies: Vec<Vec<u8>>,
    pub pending_clipboard: Option<String>,
    pub theme_fg: (u8, u8, u8),
    pub theme_bg: (u8, u8, u8),
    pub evicted_rows: usize,
    pub active_link: Option<(usize, u16, bool, String)>,
    pub osc_links: VecDeque<OscLink>,
    pub command_marks: VecDeque<CommandMark>,
    /// Stack of Kitty Keyboard Protocol enhancement flags (per primary/alt screen).
    pub kitty_kbd_stack: Vec<u8>,
    /// xterm `modifyOtherKeys` level (`0`, `1`, or `2`).
    pub modify_other_keys: u8,
    /// Timestamp of the last audible/visual bell for subtle UI flash.
    pub bell_at: Option<Instant>,
    /// Unread bell indicator shown on inactive terminal tabs.
    pub bell_unread: bool,
    /// Cooldown timestamp preventing echoed `CSI ? 0 u` replies from forming a query storm.
    last_kitty_query: Option<Instant>,
}

impl Default for TermCallbacks {
    fn default() -> Self {
        Self {
            title: String::new(),
            cwd: None,
            replies: Vec::new(),
            pending_clipboard: None,
            theme_fg: (0xD4, 0xD7, 0xDF),
            theme_bg: (0x14, 0x17, 0x1E),
            evicted_rows: 0,
            active_link: None,
            osc_links: VecDeque::new(),
            command_marks: VecDeque::new(),
            kitty_kbd_stack: Vec::new(),
            modify_other_keys: 0,
            bell_at: None,
            bell_unread: false,
            last_kitty_query: None,
        }
    }
}

impl TermCallbacks {
    pub fn kitty_flags(&self) -> u8 {
        self.kitty_kbd_stack.last().copied().unwrap_or(0)
    }

    fn push_reply(&mut self, reply: Vec<u8>) {
        if self.replies.len() < MAX_REPLIES_PER_POLL {
            self.replies.push(reply);
        }
    }

    pub fn current_abs_cursor(&self, screen: &vt100::Screen) -> (usize, u16, bool) {
        let (row, col) = screen.cursor_position();
        if screen.alternate_screen() {
            (usize::from(row), col, true)
        } else {
            (
                self.evicted_rows
                    .saturating_add(screen.scrollback_len())
                    .saturating_add(usize::from(row)),
                col,
                false,
            )
        }
    }

    fn finish_active_link(&mut self, screen: &vt100::Screen) {
        let Some((abs_row_start, col_start, alt_screen, url)) = self.active_link.take() else {
            return;
        };
        let (abs_row_end, col_end_excl, cur_alt) = self.current_abs_cursor(screen);
        if cur_alt != alt_screen {
            return;
        }
        let cols = screen.size().1.get();
        let (end_row, end_col) = if abs_row_end > abs_row_start && col_end_excl == 0 {
            (abs_row_end - 1, cols.saturating_sub(1))
        } else if (abs_row_end, col_end_excl) > (abs_row_start, col_start) {
            (abs_row_end, col_end_excl.saturating_sub(1))
        } else {
            return;
        };
        self.osc_links.push_back(OscLink {
            abs_row_start,
            col_start,
            abs_row_end: end_row,
            col_end: end_col,
            url,
            alt_screen,
        });
        while self.osc_links.len() > MAX_OSC_LINKS {
            self.osc_links.pop_front();
        }
    }

    fn record_shell_integration(&mut self, screen: &vt100::Screen, kind: &[u8], extra: Option<&[u8]>) {
        if screen.alternate_screen() {
            return;
        }
        let (abs_row, _, _) = self.current_abs_cursor(screen);
        match kind {
            // A = Prompt start, C = Command execution start
            b"A" => {
                if self
                    .command_marks
                    .back()
                    .is_some_and(|m| m.abs_row == abs_row && !m.finished)
                {
                    return;
                }
                self.command_marks.push_back(CommandMark {
                    abs_row,
                    exit_code: None,
                    finished: false,
                });
                while self.command_marks.len() > MAX_COMMAND_MARKS {
                    self.command_marks.pop_front();
                }
            }
            b"C" => {
                if let Some(last) = self.command_marks.back_mut() {
                    if !last.finished {
                        // Keep the prompt start row so jumping lands on the command line.
                        return;
                    }
                }
                self.command_marks.push_back(CommandMark {
                    abs_row,
                    exit_code: None,
                    finished: false,
                });
                while self.command_marks.len() > MAX_COMMAND_MARKS {
                    self.command_marks.pop_front();
                }
            }
            // D [; exit_code] = Command finished
            b"D" => {
                let code = extra
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.trim().parse::<i32>().ok());
                if let Some(last) = self.command_marks.back_mut()
                    && !last.finished
                {
                    last.exit_code = code;
                    last.finished = true;
                }
            }
            _ => {}
        }
    }
}

impl vt100::Callbacks for TermCallbacks {
    fn audible_bell(&mut self, _screen: &mut vt100::Screen) {
        self.bell_at = Some(Instant::now());
        self.bell_unread = true;
    }

    fn visual_bell(&mut self, _screen: &mut vt100::Screen) {
        self.bell_at = Some(Instant::now());
        self.bell_unread = true;
    }

    fn set_window_title(&mut self, _screen: &mut vt100::Screen, title: &[u8]) {
        let title = String::from_utf8_lossy(title);
        let title = title.trim();
        if title.is_empty() || title.to_ascii_lowercase().ends_with(".exe") {
            return;
        }
        self.title = title.chars().take(MAX_TITLE_CHARS).collect();
    }

    fn copy_to_clipboard(&mut self, _screen: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        if data.len() > MAX_OSC52_RAW_BYTES {
            return;
        }
        if let Some(decoded) = decode_base64(data)
            && let Ok(text) = String::from_utf8(decoded)
            && !text.is_empty()
        {
            self.pending_clipboard = Some(text);
        }
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i2.is_some() {
            return;
        }
        let p0 = params.first().and_then(|p| p.first().copied()).unwrap_or(0);
        let p1 = params.get(1).and_then(|p| p.first().copied()).unwrap_or(0);

        match (i1, c) {
            // DSR 6: Cursor Position Report.
            (None, 'n') if params.len() == 1 && p0 == 6 => {
                let (row, col) = screen.cursor_position();
                self.push_reply(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            }
            // DSR 5: Operating Status Report -> OK.
            (None, 'n') if params.len() == 1 && p0 == 5 => {
                self.push_reply(b"\x1b[0n".to_vec());
            }
            // DA1: Primary Device Attributes -> VT220 + ANSI color.
            (None, 'c') if params.is_empty() || (params.len() == 1 && p0 == 0) => {
                self.push_reply(b"\x1b[?62;22c".to_vec());
            }
            // DA2: Secondary Device Attributes.
            (Some(b'>'), 'c') if params.is_empty() || (params.len() == 1 && p0 == 0) => {
                self.push_reply(b"\x1b[>0;10;1c".to_vec());
            }
            // Kitty Keyboard Protocol: Query current flags (`CSI ? u`).
            // vte normalizes empty params to `[0]`, which matches an echoed `CSI ? 0 u`
            // reply; reject non-zero params (e.g. `CSI ? 1 u`) and enforce a cooldown
            // so an echoed reply in cooked mode can never form a feedback storm.
            (Some(b'?'), 'u') if params.len() <= 1 && p0 == 0 => {
                let cooling_down = self
                    .last_kitty_query
                    .is_some_and(|at| at.elapsed() < KITTY_QUERY_COOLDOWN);
                if !cooling_down {
                    self.last_kitty_query = Some(Instant::now());
                    let flags = self.kitty_flags();
                    self.push_reply(format!("\x1b[?{flags}u").into_bytes());
                }
            }
            // Kitty Keyboard Protocol: Push flags (`CSI > flags u`).
            (Some(b'>'), 'u') => {
                let flags = (p0 & 0x1F) as u8;
                if self.kitty_kbd_stack.len() >= MAX_KITTY_STACK {
                    self.kitty_kbd_stack.remove(0);
                }
                self.kitty_kbd_stack.push(flags);
            }
            // Kitty Keyboard Protocol: Pop `n` entries (`CSI < n u`).
            (Some(b'<'), 'u') => {
                let count = (p0.max(1) as usize).min(self.kitty_kbd_stack.len());
                let keep = self.kitty_kbd_stack.len() - count;
                self.kitty_kbd_stack.truncate(keep);
            }
            // Kitty Keyboard Protocol: Set/OR/AND top flags (`CSI = flags ; mode u`).
            (Some(b'='), 'u') => {
                let flags = (p0 & 0x1F) as u8;
                let mode = if params.len() >= 2 { p1 } else { 1 };
                let cur = self.kitty_flags();
                let next = match mode {
                    2 => cur | flags,
                    3 => cur & !flags,
                    _ => flags,
                };
                if let Some(top) = self.kitty_kbd_stack.last_mut() {
                    *top = next;
                } else {
                    self.kitty_kbd_stack.push(next);
                }
            }
            // xterm `modifyOtherKeys`: `CSI > 4 ; m` or reset `CSI > 4 m`.
            (Some(b'>'), 'm') if p0 == 4 => {
                self.modify_other_keys = if params.len() >= 2 {
                    (p1.min(2)) as u8
                } else {
                    0
                };
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            // OSC 7: Current working directory (`file://host/path`).
            [b"7", uri] => {
                if let Some(path) = parse_osc7_cwd(uri) {
                    self.cwd = Some(path);
                }
            }
            // OSC 9;9;<path>: ConPTY / Windows Terminal CWD notification.
            [b"9", b"9", path] => {
                let text = String::from_utf8_lossy(path).trim().to_string();
                if !text.is_empty() {
                    let p = PathBuf::from(&text);
                    if p.is_dir() {
                        self.cwd = Some(p);
                    }
                }
            }
            // OSC 8;<params>;<uri>: Explicit terminal hyperlinks.
            [b"8", _link_params, uri] => {
                self.finish_active_link(screen);
                let url = String::from_utf8_lossy(uri).trim().to_string();
                if !url.is_empty() {
                    let (abs_row, col, alt) = self.current_abs_cursor(screen);
                    self.active_link = Some((abs_row, col, alt, url));
                }
            }
            // OSC 10 / 11: Dynamic foreground / background color queries.
            [b"10", b"?"] => {
                let (r, g, b) = self.theme_fg;
                self.push_reply(
                    format!("\x1b]10;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\")
                        .into_bytes(),
                );
            }
            [b"11", b"?"] => {
                let (r, g, b) = self.theme_bg;
                self.push_reply(
                    format!("\x1b]11;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\")
                        .into_bytes(),
                );
            }
            // OSC 133 / OSC 633: Shell Integration command marks.
            [b"133" | b"633", kind] => {
                self.record_shell_integration(screen, kind, None);
            }
            [b"133" | b"633", kind, extra, ..] => {
                self.record_shell_integration(screen, kind, Some(extra));
            }
            _ => {}
        }
    }

    fn on_scroll(&mut self, _contents: vt100::capture::RowContents<'_>, alternate_screen: bool) {
        if !alternate_screen {
            self.evicted_rows = self.evicted_rows.saturating_add(1);
            while self
                .osc_links
                .front()
                .is_some_and(|link| !link.alt_screen && link.abs_row_end < self.evicted_rows)
            {
                self.osc_links.pop_front();
            }
            while self
                .command_marks
                .front()
                .is_some_and(|mark| mark.abs_row < self.evicted_rows)
            {
                self.command_marks.pop_front();
            }
        }
    }
}

pub(crate) fn parse_osc7_cwd(uri: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(uri).ok()?.trim();
    let path = parse_file_uri(text)?;
    path.is_dir().then_some(path)
}

pub(crate) fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let raw_path = percent_decode(&rest[slash..]);
    let path_str = if cfg!(windows)
        && raw_path.starts_with('/')
        && raw_path.as_bytes().get(2) == Some(&b':')
    {
        &raw_path[1..]
    } else {
        raw_path.as_str()
    };
    let path = PathBuf::from(path_str);
    (!path.as_os_str().is_empty()).then_some(path)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for &b in input {
        if b == b'=' || b.is_ascii_whitespace() {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}
