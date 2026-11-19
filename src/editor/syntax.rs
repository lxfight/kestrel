//! Background syntax-highlighting and code-intelligence service.
//!
//! Parsing and querying tree-sitter on the UI thread would stall typing on
//! large files, so each document ships an O(1) `Rope` clone to the worker thread,
//! which computes highlight spans, outline symbols, fold ranges, and syntax diagnostics.

use std::sync::mpsc::{Receiver, Sender, channel};

use ropey::Rope;

use crate::highlight::{HighlightState, Language, Span, SyntaxEdit};
use crate::lsp::{Diagnostic, OutlineSymbol};

/// tree-sitter parses recursively. 1 MB covers deep nesting; reserving the old
/// 4 MB per document bought nothing and cost address space per open tab.
const WORKER_STACK_BYTES: usize = 1024 * 1024;

enum Request {
    Edit(SyntaxEdit),
    ComputeRope { version: u64, rope: Rope },
    Reset(Language),
    Shutdown,
}

pub struct SyntaxAnalysis {
    pub version: u64,
    pub spans: Vec<Span>,
    pub symbols: Vec<OutlineSymbol>,
    pub folds: Vec<(usize, usize)>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct SyntaxService {
    language: Language,
    /// Created on the first parse request: restored tabs nobody looks at must not
    /// own a thread. `None` also means the worker failed to start.
    tx: Option<Sender<Request>>,
    rx: Receiver<SyntaxAnalysis>,
    busy: bool,
    latest_version: u64,
}

impl SyntaxService {
    pub fn new(language: Language) -> Self {
        let (_, rx) = channel::<SyntaxAnalysis>();
        Self {
            language,
            tx: None,
            rx,
            busy: false,
            latest_version: 0,
        }
    }

    /// Start the worker on first use. A spawn failure disables background
    /// highlighting for this document instead of panicking the UI thread.
    fn ensure_worker(&mut self) -> bool {
        if self.tx.is_some() {
            return true;
        }
        let (request_tx, request_rx): (Sender<Request>, Receiver<Request>) = channel();
        let (analysis_tx, analysis_rx): (Sender<SyntaxAnalysis>, Receiver<SyntaxAnalysis>) =
            channel();
        let language = self.language;
        match std::thread::Builder::new()
            .name("kestrel-highlight".to_string())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || worker(language, request_rx, analysis_tx))
        {
            Ok(_) => {
                self.tx = Some(request_tx);
                self.rx = analysis_rx;
                true
            }
            Err(err) => {
                eprintln!("kestrel: 语法高亮线程启动失败: {err}");
                false
            }
        }
    }

    pub fn apply_edit(&mut self, edit: SyntaxEdit) {
        // Edits only feed the incremental parser state, which the first request
        // rebuilds from scratch; drop them until a worker exists.
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request::Edit(edit));
        }
    }

    pub fn reset(&mut self, language: Language) {
        self.busy = false;
        self.language = language;
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request::Reset(language));
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Request background analysis using an O(1) `Rope` snapshot clone.
    pub fn request_rope(&mut self, version: u64, rope: Rope) {
        if self.busy {
            return;
        }
        if !self.ensure_worker() {
            return;
        }
        self.busy = true;
        self.latest_version = version;
        let send_failed = match &self.tx {
            Some(tx) => tx.send(Request::ComputeRope { version, rope }).is_err(),
            None => false,
        };
        if send_failed {
            self.busy = false;
        }
    }

    /// Latest finished analysis, if ready.
    pub fn poll_analysis(&mut self) -> Option<SyntaxAnalysis> {
        let mut latest: Option<SyntaxAnalysis> = None;
        loop {
            match self.rx.try_recv() {
                Ok(analysis) => {
                    self.busy = false;
                    if latest.as_ref().is_none_or(|prev| analysis.version >= prev.version) {
                        latest = Some(analysis);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Worker is gone; never wedge the editor in "always busy".
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    break;
                }
            }
        }
        latest
    }
}

impl Drop for SyntaxService {
    fn drop(&mut self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request::Shutdown);
        }
    }
}

fn worker(mut language: Language, rx: Receiver<Request>, tx: Sender<SyntaxAnalysis>) {
    let mut state = HighlightState::new(language);
    while let Ok(request) = rx.recv() {
        match request {
            Request::Edit(edit) => state.apply_edit(edit),
            Request::ComputeRope { version, rope } => {
                let text = rope.to_string();
                state.parse(&text, version);
                let spans = state.spans(&text, version).to_vec();
                let (symbols, folds, diagnostics) = state.analyze(&text, language);
                if tx
                    .send(SyntaxAnalysis {
                        version,
                        spans,
                        symbols,
                        folds,
                        diagnostics,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Request::Reset(new_lang) => {
                language = new_lang;
                state = HighlightState::new(language);
            }
            Request::Shutdown => break,
        }
    }
}
