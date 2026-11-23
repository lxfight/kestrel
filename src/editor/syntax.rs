//! Background syntax-highlighting, code-intelligence, and line-diff service.
//!
//! All open tabs share a fixed 2-thread worker pool instead of spawning one OS
//! thread per document. Each tab owns its incremental `HighlightState` while
//! idle, lends it to a pool worker alongside an O(1) `Rope` snapshot clone, and
//! receives it back with the finished spans, symbols, folds, diagnostics, and
//! Git gutter line diffs.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use ropey::Rope;

use crate::git::{LineDiffKind, compute_line_diff};
use crate::highlight::{HighlightState, Language, Span, SyntaxEdit};
use crate::lsp::{Diagnostic, OutlineSymbol};

/// tree-sitter parses recursively. 1 MB covers deep nesting on the 2 shared workers.
const WORKER_STACK_BYTES: usize = 1024 * 1024;
const POOL_WORKERS: usize = 2;

struct SyntaxJob {
    generation: u64,
    language: Language,
    state: Option<HighlightState>,
    pending_edits: Vec<SyntaxEdit>,
    version: u64,
    rope: Rope,
    baseline: Option<Arc<str>>,
    reply: Sender<SyntaxReply>,
}

struct SyntaxReply {
    generation: u64,
    state: HighlightState,
    analysis: SyntaxAnalysis,
}

pub struct SyntaxAnalysis {
    pub version: u64,
    pub spans: Vec<Span>,
    pub symbols: Vec<OutlineSymbol>,
    pub folds: Vec<(usize, usize)>,
    pub diagnostics: Vec<Diagnostic>,
    pub line_diffs: Option<Vec<LineDiffKind>>,
}

struct SyntaxPool {
    queue: Arc<(Mutex<VecDeque<SyntaxJob>>, Condvar)>,
}

impl SyntaxPool {
    fn global() -> &'static Self {
        static POOL: LazyLock<SyntaxPool> = LazyLock::new(SyntaxPool::new);
        &POOL
    }

    fn new() -> Self {
        let queue = Arc::new((Mutex::new(VecDeque::<SyntaxJob>::new()), Condvar::new()));
        for idx in 0..POOL_WORKERS {
            let shared = Arc::clone(&queue);
            if let Err(err) = std::thread::Builder::new()
                .name(format!("kestrel-syntax-{idx}"))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || pool_worker(shared))
            {
                eprintln!("kestrel: 语法高亮线程池启动失败({idx}): {err}");
            }
        }
        Self { queue }
    }

    fn submit(&self, job: SyntaxJob) {
        let (lock, cvar) = &*self.queue;
        if let Ok(mut guard) = lock.lock() {
            guard.push_back(job);
            cvar.notify_one();
        }
    }
}

fn pool_worker(shared: Arc<(Mutex<VecDeque<SyntaxJob>>, Condvar)>) {
    let (lock, cvar) = &*shared;
    loop {
        let job = {
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            while guard.is_empty() {
                match cvar.wait(guard) {
                    Ok(next) => guard = next,
                    Err(_) => return,
                }
            }
            guard.pop_front()
        };
        let Some(job) = job else {
            continue;
        };
        let mut state = job
            .state
            .unwrap_or_else(|| HighlightState::new(job.language));
        for edit in job.pending_edits {
            state.apply_edit(edit);
        }
        let text = job.rope.to_string();
        state.parse(&text, job.version);
        let spans = state.spans(&text, job.version).to_vec();
        let (symbols, folds, diagnostics) = state.analyze(&text, job.language);
        let line_diffs = job.baseline.map(|base| compute_line_diff(&base, &text));
        let _ = job.reply.send(SyntaxReply {
            generation: job.generation,
            state,
            analysis: SyntaxAnalysis {
                version: job.version,
                spans,
                symbols,
                folds,
                diagnostics,
                line_diffs,
            },
        });
    }
}

pub struct SyntaxService {
    language: Language,
    generation: u64,
    state: Option<HighlightState>,
    pending_edits: Vec<SyntaxEdit>,
    tx: Sender<SyntaxReply>,
    rx: Receiver<SyntaxReply>,
    busy: bool,
    latest_version: u64,
}

impl SyntaxService {
    pub fn new(language: Language) -> Self {
        let (tx, rx) = channel();
        Self {
            language,
            generation: 1,
            state: None,
            pending_edits: Vec::new(),
            tx,
            rx,
            busy: false,
            latest_version: 0,
        }
    }

    pub fn apply_edit(&mut self, edit: SyntaxEdit) {
        if let Some(state) = &mut self.state {
            state.apply_edit(edit);
        } else if self.busy {
            // Apply once the in-flight worker hands `HighlightState` back.
            self.pending_edits.push(edit);
        }
    }

    pub fn reset(&mut self, language: Language) {
        self.generation = self.generation.wrapping_add(1);
        self.language = language;
        self.state = None;
        self.pending_edits.clear();
        self.busy = false;
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Request background syntax analysis (and optional line diff) using an O(1) `Rope` clone.
    pub fn request_rope(&mut self, version: u64, rope: Rope, baseline: Option<Arc<str>>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.latest_version = version;
        let state = self.state.take();
        let pending_edits = std::mem::take(&mut self.pending_edits);
        SyntaxPool::global().submit(SyntaxJob {
            generation: self.generation,
            language: self.language,
            state,
            pending_edits,
            version,
            rope,
            baseline,
            reply: self.tx.clone(),
        });
    }

    /// Latest finished analysis, if ready.
    pub fn poll_analysis(&mut self) -> Option<SyntaxAnalysis> {
        let mut latest: Option<SyntaxAnalysis> = None;
        loop {
            match self.rx.try_recv() {
                Ok(mut reply) => {
                    if reply.generation != self.generation {
                        continue;
                    }
                    self.busy = false;
                    for edit in std::mem::take(&mut self.pending_edits) {
                        reply.state.apply_edit(edit);
                    }
                    self.state = Some(reply.state);
                    if latest
                        .as_ref()
                        .is_none_or(|prev| reply.analysis.version >= prev.version)
                    {
                        latest = Some(reply.analysis);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    break;
                }
            }
        }
        latest
    }
}
