//! Background project file index used by Quick Open and Project Search.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

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
        type IndexMessage = (u64, Vec<PathBuf>);
        let (tx, rx): (Sender<IndexMessage>, Receiver<IndexMessage>) = channel();
        std::thread::spawn(move || {
            let mut files = walk_project(&root);
            // Sorted so watcher batches can insert/remove with a binary search.
            files.sort();
            let _ = tx.send((generation, files));
        });
        self.rx = Some(rx);
    }

    /// Fold one watcher batch into the index. Returns true when the list changed.
    /// A directory path means the child set is unknown, so that batch falls back
    /// to a full rescan instead of guessing.
    pub fn apply_changes(&mut self, root: &Path, paths: &[PathBuf]) -> bool {
        if self.files.is_empty() {
            self.scan(root);
            return false;
        }
        let mut changed = false;
        for path in paths {
            if path.is_dir() {
                self.scan(root);
                return true;
            }
            match self.files.binary_search(path) {
                Ok(index) => {
                    if !path.is_file() {
                        self.files.remove(index);
                        changed = true;
                    }
                }
                Err(index) => {
                    if path.is_file() {
                        self.files.insert(index, path.clone());
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Drain any finished index. Returns true when the file list changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok((generation, files)) => {
                    if generation == self.generation {
                        self.files = files;
                        changed = true;
                    }
                    self.loading = false;
                }
                Err(TryRecvError::Empty) => break,
                // Scan thread gone without a result; stop reporting "loading".
                Err(TryRecvError::Disconnected) => {
                    self.loading = false;
                    break;
                }
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
