//! Long-lived bidirectional JSON-RPC child processes for plugins (MCP servers,
//! external AI agent sidecars, custom language/analysis tools).
//!
//! Supports two standard stdio framings:
//! - `ndjson` (default): one JSON value per newline (used by MCP stdio servers).
//! - `lsp`: `Content-Length: N\r\n\r\n<json>` framing (used by LSP/MCP/ACP).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};

/// Maximum concurrent long-lived RPC processes across all plugins.
pub(crate) const MAX_CONCURRENT_RPC: usize = 4;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RpcFraming {
    Ndjson,
    Lsp,
}

impl RpcFraming {
    pub(crate) fn parse(name: Option<&str>) -> Result<Self, String> {
        match name.unwrap_or("ndjson") {
            "ndjson" | "jsonl" | "line" => Ok(Self::Ndjson),
            "lsp" | "content-length" => Ok(Self::Lsp),
            other => Err(format!("不支持的 RPC 帧协议: {other}（可选 ndjson / lsp）")),
        }
    }
}

pub(crate) enum RpcEvent {
    Message {
        runtime: u64,
        id: u64,
        payload: serde_json::Value,
    },
    Stderr {
        runtime: u64,
        id: u64,
        chunk: String,
    },
    Exit {
        runtime: u64,
        id: u64,
        code: i32,
        error: Option<String>,
    },
}

enum Outgoing {
    Payload(Vec<u8>),
    Close,
}

#[derive(Clone)]
pub(crate) struct RpcControl {
    pub id: u64,
    framing: RpcFraming,
    writer_tx: Sender<Outgoing>,
    alive: Arc<AtomicBool>,
    kill_flag: Arc<AtomicBool>,
}

impl RpcControl {
    pub(crate) fn send_json(&self, value: &serde_json::Value) -> Result<(), String> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err("RPC 进程已退出".to_string());
        }
        let encoded = serde_json::to_string(value)
            .map_err(|err| format!("JSON 序列化失败: {err}"))?;
        let bytes = match self.framing {
            RpcFraming::Ndjson => format!("{encoded}\n").into_bytes(),
            RpcFraming::Lsp => format!("Content-Length: {}\r\n\r\n{encoded}", encoded.len())
                .into_bytes(),
        };
        self.writer_tx
            .send(Outgoing::Payload(bytes))
            .map_err(|_| "RPC 写入通道已关闭".to_string())
    }

    pub(crate) fn send_raw(&self, text: &str) -> Result<(), String> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err("RPC 进程已退出".to_string());
        }
        self.writer_tx
            .send(Outgoing::Payload(text.as_bytes().to_vec()))
            .map_err(|_| "RPC 写入通道已关闭".to_string())
    }

    pub(crate) fn kill(&self) {
        self.kill_flag.store(true, Ordering::Relaxed);
        let _ = self.writer_tx.send(Outgoing::Close);
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

struct RpcSlot(Arc<AtomicUsize>);
impl Drop for RpcSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) struct RpcSpawnSpec {
    pub runtime: u64,
    pub id: u64,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub framing: RpcFraming,
}

pub(crate) fn spawn_rpc(
    spec: RpcSpawnSpec,
    event_tx: Sender<RpcEvent>,
    running: Arc<AtomicUsize>,
    plugin_cancel: Arc<AtomicBool>,
    repaint_ctx: Option<egui::Context>,
) -> Result<RpcControl, String> {
    if running.fetch_add(1, Ordering::Relaxed) >= MAX_CONCURRENT_RPC {
        running.fetch_sub(1, Ordering::Relaxed);
        return Err(format!(
            "kestrel.spawn_rpc 并发上限 {MAX_CONCURRENT_RPC}，本次调用被拒绝"
        ));
    }

    let mut cmd = Process::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            running.fetch_sub(1, Ordering::Relaxed);
            return Err(format!("启动 RPC 进程 {} 失败: {err}", spec.program));
        }
    };

    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill();
        running.fetch_sub(1, Ordering::Relaxed);
        return Err("无法获取子进程 stdin".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        running.fetch_sub(1, Ordering::Relaxed);
        return Err("无法获取子进程 stdout".to_string());
    };
    let stderr = child.stderr.take();

    let alive = Arc::new(AtomicBool::new(true));
    let kill_flag = Arc::new(AtomicBool::new(false));
    let (writer_tx, writer_rx) = channel::<Outgoing>();
    let runtime = spec.runtime;
    let id = spec.id;
    let framing = spec.framing;

    // Writer thread: drains outbound messages without blocking UI.
    std::thread::Builder::new()
        .name(format!("kestrel-rpc-in-{id}"))
        .spawn(move || {
            let mut stdin = stdin;
            while let Ok(msg) = writer_rx.recv() {
                match msg {
                    Outgoing::Payload(bytes) => {
                        if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                            break;
                        }
                    }
                    Outgoing::Close => break,
                }
            }
        })
        .ok();

    // Stderr reader thread.
    let stderr_handle = stderr.and_then(|mut stderr| {
        let tx = event_tx.clone();
        let repaint = repaint_ctx.clone();
        let p_cancel = plugin_cancel.clone();
        let k_flag = kill_flag.clone();
        std::thread::Builder::new()
            .name(format!("kestrel-rpc-err-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if p_cancel.load(Ordering::Relaxed) || k_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let _ = tx.send(RpcEvent::Stderr { runtime, id, chunk });
                    if let Some(ctx) = &repaint {
                        ctx.request_repaint();
                    }
                }
            })
            .ok()
    });

    // Stdout framing reader thread.
    let stdout_handle = {
        let tx = event_tx.clone();
        let repaint = repaint_ctx.clone();
        let p_cancel = plugin_cancel.clone();
        let k_flag = kill_flag.clone();
        std::thread::Builder::new()
            .name(format!("kestrel-rpc-out-{id}"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                match framing {
                    RpcFraming::Ndjson => {
                        let mut line = String::new();
                        loop {
                            line.clear();
                            match reader.read_line(&mut line) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    if p_cancel.load(Ordering::Relaxed)
                                        || k_flag.load(Ordering::Relaxed)
                                    {
                                        break;
                                    }
                                    let trimmed = line.trim();
                                    if trimmed.is_empty() {
                                        continue;
                                    }
                                    if let Ok(payload) =
                                        serde_json::from_str::<serde_json::Value>(trimmed)
                                    {
                                        let _ = tx.send(RpcEvent::Message {
                                            runtime,
                                            id,
                                            payload,
                                        });
                                        if let Some(ctx) = &repaint {
                                            ctx.request_repaint();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    RpcFraming::Lsp => loop {
                        if p_cancel.load(Ordering::Relaxed) || k_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(len) = read_content_length(&mut reader) else {
                            break;
                        };
                        if len == 0 || len > MAX_FRAME_BYTES {
                            break;
                        }
                        let mut body = vec![0u8; len];
                        if reader.read_exact(&mut body).is_err() {
                            break;
                        }
                        if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) {
                            let _ = tx.send(RpcEvent::Message {
                                runtime,
                                id,
                                payload,
                            });
                            if let Some(ctx) = &repaint {
                                ctx.request_repaint();
                            }
                        }
                    },
                }
            })
            .ok()
    };

    // Lifecycle supervisor thread: watches kill / plugin_cancel, joins stdout/stderr
    // readers so all trailing messages arrive strictly before Exit, then reports exit.
    {
        let alive_sup = alive.clone();
        let k_flag = kill_flag.clone();
        let p_cancel = plugin_cancel.clone();
        let slot = RpcSlot(running);
        std::thread::Builder::new()
            .name(format!("kestrel-rpc-wait-{id}"))
            .spawn(move || {
                let _slot = slot;
                let mut killed = false;
                let code = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break status.code().unwrap_or(-1),
                        Ok(None) => {}
                        Err(_) => {
                            let _ = child.kill();
                            break -1;
                        }
                    }
                    if !killed
                        && (k_flag.load(Ordering::Relaxed) || p_cancel.load(Ordering::Relaxed))
                    {
                        killed = true;
                        let _ = child.kill();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                };
                if let Some(h) = stdout_handle {
                    let _ = h.join();
                }
                if let Some(h) = stderr_handle {
                    let _ = h.join();
                }
                alive_sup.store(false, Ordering::Relaxed);
                let _ = event_tx.send(RpcEvent::Exit {
                    runtime,
                    id,
                    code,
                    error: killed.then(|| "已终止".to_string()),
                });
                if let Some(ctx) = &repaint_ctx {
                    ctx.request_repaint();
                }
            })
            .ok();
    }

    Ok(RpcControl {
        id,
        framing,
        writer_tx,
        alive,
        kill_flag,
    })
}

fn read_content_length(reader: &mut impl BufRead) -> Option<usize> {
    let mut content_len = None;
    let mut header = String::new();
    loop {
        header.clear();
        let n = reader.read_line(&mut header).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            return content_len;
        }
        if let Some((k, v)) = trimmed.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            content_len = v.trim().parse::<usize>().ok();
        }
    }
}
