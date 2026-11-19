//! Background syntax-highlighting and code-intelligence service.
//!
//! Parsing and querying tree-sitter on the UI thread would stall typing on
//! large files, so each document ships an O(1) `Rope` clone to the worker thread,
//! which computes highlight spans, outline symbols, fold ranges, and syntax diagnostics.

use std::sync::mpsc::{Receiver, Sender, channel};

use ropey::Rope;

use crate::highlight::{HighlightState, Language, Span, SyntaxEdit};
use crate::lsp::{Diagnostic, OutlineSymbol};

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
    tx: Sender<Request>,
    rx: Receiver<SyntaxAnalysis>,
    busy: bool,
    latest_version: u64,
}

impl SyntaxService {
    pub fn new(language: Language) -> Self {
        let (tx, req_rx) = channel();
        let (out_tx, rx) = channel();
        std::thread::Builder::new()
            .name("kestrel-highlight".to_string())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || worker(language, req_rx, out_tx))
            .expect("spawn highlight thread");
        Self {
            tx,
            rx,
            busy: false,
            latest_version: 0,
        }
    }

    pub fn apply_edit(&mut self, edit: SyntaxEdit) {
        let _ = self.tx.send(Request::Edit(edit));
    }

    pub fn reset(&mut self, language: Language) {
        self.busy = false;
        let _ = self.tx.send(Request::Reset(language));
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Request background analysis using an O(1) `Rope` snapshot clone.
    pub fn request_rope(&mut self, version: u64, rope: Rope) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.latest_version = version;
        let _ = self.tx.send(Request::ComputeRope { version, rope });
    }

    /// Latest finished analysis, if ready.
    pub fn poll_analysis(&mut self) -> Option<SyntaxAnalysis> {
        let mut latest: Option<SyntaxAnalysis> = None;
        while let Ok(analysis) = self.rx.try_recv() {
            self.busy = false;
            if latest.as_ref().is_none_or(|prev| analysis.version >= prev.version) {
                latest = Some(analysis);
            }
        }
        latest
    }
}

impl Drop for SyntaxService {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Shutdown);
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
