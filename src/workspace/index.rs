//! Background project file index used by Quick Open and Project Search.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

pub struct FileIndex {
    files: Vec<PathBuf>,
    loading: bool,
    rx: Option<Receiver<(u64, Vec<PathBuf>)>>,
    generation: u64,
}

impl FileIndex {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            loading: false,
            rx: None,
            generation: 0,
        }
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Start (or restart) indexing a project root in a background thread.
    pub fn scan(&mut self, root: &Path) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        let root = root.to_path_buf();
        let (tx, rx): (Sender<(u64, Vec<PathBuf>)>, Receiver<(u64, Vec<PathBuf>)>) = channel();
        std::thread::spawn(move || {
            let files = walk_project(&root);
            let _ = tx.send((generation, files));
        });
        self.rx = Some(rx);
    }

    /// Drain any finished index. Returns true when the file list changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut changed = false;
        while let Ok((generation, files)) = rx.try_recv() {
            if generation == self.generation {
                self.files = files;
                self.loading = false;
                changed = true;
            }
        }
        changed
    }

    pub fn clear(&mut self) {
        self.generation += 1;
        self.files.clear();
        self.loading = false;
        self.rx = None;
    }
}

/// Walk `root` honoring .gitignore/.ignore files, skipping `.git`.
pub fn walk_project(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            files.push(entry.into_path());
        }
    }
    files
}

/// Read a file for search, bailing out on huge or binary files.
pub fn read_searchable(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}
