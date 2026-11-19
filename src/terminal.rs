//! Integrated terminal: ConPTY session + vt100 screen state.
//!
//! The UI layer paints [`TerminalSession::screen`]; input goes back through
//! [`TerminalSession::write`]. Output is drained on the UI thread from a reader
//! thread channel, so painting never blocks on the shell.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};

/// Scrollback kept by the vt100 parser.
const SCROLLBACK: usize = 5000;
/// Reader thread buffer; also the size of one channel chunk.
const READ_CHUNK: usize = 8 * 1024;
/// Bound the chunks parsed per frame so a firehose command cannot stall painting.
const MAX_CHUNKS_PER_POLL: usize = 256;

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
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
}

#[allow(dead_code)]
impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default | Self::PowerShell => "PowerShell",
            Self::Cmd => "Command Prompt",
            Self::GitBash => "Git Bash",
            Self::Wsl => "WSL",
        }
    }
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
        let (child, default_title) = spawn_shell_kind(&*pair.slave, cwd, shell)?;

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

    pub fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
        self.scrollback_offset = self.parser.screen().scrollback();
    }

    /// Send raw keyboard bytes to the shell.
    pub fn write(&mut self, bytes: &[u8]) {
        if !self.alive {
            return;
        }
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

fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

fn spawn_shell_kind(
    slave: &dyn SlavePty,
    cwd: &Path,
    kind: ShellKind,
) -> Option<(Box<dyn Child + Send + Sync>, &'static str)> {
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
        ShellKind::Cmd => try_spawn("cmd.exe", None)
            .map(|child| (child, "Command Prompt"))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::GitBash => try_spawn("C:\\Program Files\\Git\\bin\\bash.exe", Some("--login"))
            .or_else(|| try_spawn("bash", None))
            .map(|child| (child, "Git Bash"))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::Wsl => try_spawn("wsl.exe", None)
            .map(|child| (child, "WSL"))
            .or_else(|| spawn_shell_default(slave, cwd)),
        ShellKind::Default | ShellKind::PowerShell => spawn_shell_default(slave, cwd),
    }
}

fn spawn_shell_default(
    slave: &dyn SlavePty,
    cwd: &Path,
) -> Option<(Box<dyn Child + Send + Sync>, &'static str)> {
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
        try_spawn("powershell.exe", Some("-NoLogo"))
            .map(|child| (child, "PowerShell"))
            .or_else(|| try_spawn("cmd.exe", None).map(|child| (child, "Command Prompt")))
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        try_spawn(&shell, None)
            .map(|child| (child, "Shell"))
            .or_else(|| try_spawn("/bin/sh", None).map(|child| (child, "sh")))
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
}
