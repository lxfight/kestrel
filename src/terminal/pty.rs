//! Non-blocking, deadlock-free OS PTY backend.
//!
//! Architecture invariants:
//! 1. **Never block the UI thread**: PTY spawning, stdin writes, protocol replies,
//!    `ResizePseudoConsole`, and `ClosePseudoConsole` teardown all execute off the
//!    UI thread.
//! 2. **Never block the reader thread on the UI**: ConPTY can deadlock on resize,
//!    stdin writes, or `ClosePseudoConsole` if its output pipe is not continuously
//!    drained. Incoming chunks land in a memory-bounded ring queue (`MAX_QUEUED_BYTES`),
//!    dropping the oldest unread chunks only under extreme UI stalls instead of
//!    stalling OS pipe reads.
//! 3. **Coalesced repaints**: The reader thread raises at most one pending
//!    `egui::Context::request_repaint()` until the UI drains the queue, preventing
//!    Windows `PostMessageW` queue saturation (`ERROR_NOT_ENOUGH_QUOTA` / winit panic).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, MasterPty, PtySize, native_pty_system};

use super::shell::{self, CustomShellConfig, ShellKind};

/// Reader thread chunk size.
const READ_CHUNK: usize = 8 * 1024;
/// Upper bound on unread PTY output retained in memory (4 MiB).
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound on queued outbound stdin/reply bytes (2 MiB).
const MAX_WRITE_QUEUED_BYTES: usize = 2 * 1024 * 1024;
/// Minimum interval between `child.try_wait()` OS handle probes while reader is open.
const ALIVE_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Memory-bounded, lock-brief output queue shared between the reader thread and UI.
pub(crate) struct OutputBuffer {
    inner: Mutex<OutputBufferInner>,
    repaint_pending: AtomicBool,
    repaint_ctx: Option<egui::Context>,
}

struct OutputBufferInner {
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    reader_closed: bool,
    discarding: bool,
}

impl OutputBuffer {
    fn new(repaint_ctx: Option<egui::Context>) -> Self {
        Self {
            inner: Mutex::new(OutputBufferInner {
                chunks: VecDeque::new(),
                queued_bytes: 0,
                reader_closed: false,
                discarding: false,
            }),
            repaint_pending: AtomicBool::new(false),
            repaint_ctx,
        }
    }

    pub(crate) fn request_repaint(&self) {
        if let Some(ctx) = &self.repaint_ctx
            && !self.repaint_pending.swap(true, Ordering::AcqRel)
        {
            ctx.request_repaint();
        }
    }

    fn push(&self, chunk: Vec<u8>) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.discarding {
                return;
            }
            inner.queued_bytes = inner.queued_bytes.saturating_add(chunk.len());
            inner.chunks.push_back(chunk);
            while inner.queued_bytes > MAX_QUEUED_BYTES && inner.chunks.len() > 1 {
                if let Some(dropped) = inner.chunks.pop_front() {
                    inner.queued_bytes = inner.queued_bytes.saturating_sub(dropped.len());
                }
            }
        }
        self.request_repaint();
    }

    fn mark_reader_closed(&self) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.reader_closed = true;
        }
        self.request_repaint();
    }

    pub(crate) fn begin_discard(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.discarding = true;
        inner.chunks.clear();
        inner.queued_bytes = 0;
    }

    /// Pop up to `max_chunks` without blocking. Returns `(chunks, more_pending, reader_closed)`.
    pub(crate) fn drain_up_to(&self, max_chunks: usize) -> (Vec<Vec<u8>>, bool, bool) {
        // Clear before acquiring the mutex so any chunk pushed concurrently after we
        // release the lock is guaranteed to trigger a fresh repaint.
        self.repaint_pending.store(false, Ordering::Release);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let count = inner.chunks.len().min(max_chunks);
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(chunk) = inner.chunks.pop_front() {
                inner.queued_bytes = inner.queued_bytes.saturating_sub(chunk.len());
                out.push(chunk);
            }
        }
        let more_pending = !inner.chunks.is_empty();
        let reader_closed = inner.reader_closed;
        (out, more_pending, reader_closed)
    }
}

struct IoQueue {
    writes: VecDeque<Vec<u8>>,
    queued_write_bytes: usize,
    pending_resize: Option<PtySize>,
    shutdown: bool,
    writer_failed: bool,
}

/// Non-blocking handle to the background PTY writer & resize control thread.
struct PtyIoHandle {
    state: Arc<(Mutex<IoQueue>, Condvar)>,
}

impl PtyIoHandle {
    fn spawn(
        master: Box<dyn MasterPty + Send>,
        mut writer: Box<dyn Write + Send>,
        output: Arc<OutputBuffer>,
    ) -> Result<Self, String> {
        let state = Arc::new((
            Mutex::new(IoQueue {
                writes: VecDeque::new(),
                queued_write_bytes: 0,
                pending_resize: None,
                shutdown: false,
                writer_failed: false,
            }),
            Condvar::new(),
        ));
        let worker_state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("kestrel-terminal-io".into())
            .spawn(move || {
                let (lock, cvar) = &*worker_state;
                loop {
                    let (resize, write_chunk, should_stop) = {
                        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while !q.shutdown
                            && q.pending_resize.is_none()
                            && q.writes.is_empty()
                        {
                            q = cvar.wait(q).unwrap_or_else(|e| e.into_inner());
                        }
                        if q.shutdown {
                            (None, None, true)
                        } else {
                            let resize = q.pending_resize.take();
                            let chunk = if resize.is_none() {
                                let c = q.writes.pop_front();
                                if let Some(ref bytes) = c {
                                    q.queued_write_bytes =
                                        q.queued_write_bytes.saturating_sub(bytes.len());
                                }
                                c
                            } else {
                                None
                            };
                            (resize, chunk, false)
                        }
                    };
                    if should_stop {
                        break;
                    }
                    if let Some(size) = resize {
                        let _ = master.resize(size);
                    }
                    if let Some(bytes) = write_chunk
                        && writer
                            .write_all(&bytes)
                            .and_then(|()| writer.flush())
                            .is_err()
                    {
                        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        q.writer_failed = true;
                        q.writes.clear();
                        q.queued_write_bytes = 0;
                        drop(q);
                        output.request_repaint();
                        // Keep the worker alive only until shutdown so `master`
                        // stays owned until teardown (and `ClosePseudoConsole`
                        // happens cleanly on shutdown).
                    }
                }
                // Drop `writer` before `master` (`ClosePseudoConsole`) off the UI thread.
                drop(writer);
                drop(master);
            })
            .map_err(|err| format!("无法启动终端写入线程: {err}"))?;

        Ok(Self { state })
    }

    fn enqueue_write(&self, bytes: Vec<u8>) -> bool {
        let (lock, cvar) = &*self.state;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown || q.writer_failed {
            return false;
        }
        if q.queued_write_bytes.saturating_add(bytes.len()) > MAX_WRITE_QUEUED_BYTES {
            // Drop excess write bursts instead of blocking the UI or exhausting RAM.
            return true;
        }
        q.queued_write_bytes = q.queued_write_bytes.saturating_add(bytes.len());
        q.writes.push_back(bytes);
        cvar.notify_one();
        true
    }

    fn enqueue_resize(&self, size: PtySize) {
        let (lock, cvar) = &*self.state;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown {
            return;
        }
        q.pending_resize = Some(size);
        cvar.notify_one();
    }

    fn writer_failed(&self) -> bool {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .writer_failed
    }

    fn shutdown(&self) {
        let (lock, cvar) = &*self.state;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.shutdown = true;
        q.writes.clear();
        q.queued_write_bytes = 0;
        q.pending_resize = None;
        cvar.notify_one();
    }
}

/// Active running PTY session; all blocking I/O and teardown happen on worker threads.
pub(crate) struct RunningPty {
    child: Box<dyn Child + Send + Sync>,
    io: PtyIoHandle,
    output: Arc<OutputBuffer>,
    reader_closed: bool,
    last_alive_check: Instant,
}

/// Newly spawned PTY metadata returned from the async spawn worker.
pub(crate) struct SpawnedPty {
    pub(crate) pty: RunningPty,
    pub(crate) default_title: String,
    pub(crate) resolved_shell: ShellKind,
}

impl RunningPty {
    /// Drain up to `max_chunks` output buffers without blocking.
    pub(crate) fn drain_output(&mut self, max_chunks: usize) -> (Vec<Vec<u8>>, bool) {
        let (chunks, more_pending, reader_closed) = self.output.drain_up_to(max_chunks);
        if reader_closed {
            self.reader_closed = true;
        }
        if more_pending {
            self.output.request_repaint();
        }
        (chunks, more_pending)
    }

    /// Enqueue bytes for non-blocking write on the I/O worker.
    pub(crate) fn write_bytes(&mut self, bytes: Vec<u8>) -> bool {
        if !self.io.enqueue_write(bytes) {
            self.reader_closed = true;
            return false;
        }
        true
    }

    /// Enqueue a coalesced resize for the I/O worker.
    pub(crate) fn resize(&mut self, cols: NonZeroU16, rows: NonZeroU16) {
        self.io.enqueue_resize(PtySize {
            rows: rows.get(),
            cols: cols.get(),
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub(crate) fn is_reader_closed(&self) -> bool {
        self.reader_closed || self.io.writer_failed()
    }

    /// Check whether the child process has exited, throttling OS probes while active.
    pub(crate) fn poll_exit(&mut self) -> Option<Option<u32>> {
        let force = self.is_reader_closed();
        if !force && self.last_alive_check.elapsed() < ALIVE_POLL_INTERVAL {
            return None;
        }
        self.last_alive_check = Instant::now();
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(Some(status.exit_code())),
            Err(_) => Some(None),
        }
    }

    /// Terminate the child and schedule non-blocking `ClosePseudoConsole` teardown.
    pub(crate) fn shutdown(&mut self) {
        // Let the reader thread drain ConPTY freely so `ClosePseudoConsole` never blocks.
        self.output.begin_discard();
        let _ = self.child.kill();
        self.io.shutdown();
    }
}

impl Drop for RunningPty {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn a shell inside a native PTY together with its reader and I/O worker threads.
pub(crate) fn spawn_pty_blocking(
    cwd: &Path,
    cols: NonZeroU16,
    rows: NonZeroU16,
    shell: ShellKind,
    custom_shell: Option<&CustomShellConfig>,
    repaint_ctx: Option<egui::Context>,
) -> Result<SpawnedPty, String> {
    let size = PtySize {
        rows: rows.get(),
        cols: cols.get(),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(size)
        .map_err(|err| format!("无法创建 PTY: {err}"))?;
    let (child, default_title, resolved_shell) =
        shell::spawn_shell_kind(&*pair.slave, cwd, shell, custom_shell)?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| format!("无法获取 PTY 读取句柄: {err}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|err| format!("无法获取 PTY 写入句柄: {err}"))?;

    let output = Arc::new(OutputBuffer::new(repaint_ctx));
    let io = PtyIoHandle::spawn(pair.master, writer, Arc::clone(&output))?;

    let reader_output = Arc::clone(&output);
    std::thread::Builder::new()
        .name("kestrel-terminal-reader".into())
        .spawn(move || {
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        reader_output.mark_reader_closed();
                        break;
                    }
                    Ok(n) => {
                        reader_output.push(buf[..n].to_vec());
                    }
                }
            }
        })
        .map_err(|err| format!("无法启动终端读取线程: {err}"))?;

    Ok(SpawnedPty {
        pty: RunningPty {
            child,
            io,
            output,
            reader_closed: false,
            last_alive_check: Instant::now()
                .checked_sub(ALIVE_POLL_INTERVAL)
                .unwrap_or_else(Instant::now),
        },
        default_title,
        resolved_shell,
    })
}
