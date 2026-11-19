//! Filesystem watcher that coalesces raw OS events into changed paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use notify::{Event, RecursiveMode, Watcher};

pub struct FsWatcher {
    rx: Receiver<Event>,
    _watcher: notify::RecommendedWatcher,
    root: PathBuf,
}

impl FsWatcher {
    pub fn new(root: &Path, repaint: egui::Context) -> notify::Result<Self> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
                // Watcher events arrive on the notify thread; wake egui so the
                // tree, Git badge and index follow without waiting for input.
                repaint.request_repaint();
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            rx,
            _watcher: watcher,
            root: root.to_path_buf(),
        })
    }

    /// Changed paths since the last call, de-duplicated and filtered to the project root.
    /// Ignored: `.git`, `target`, `node_modules`, `.venv`, `__pycache__`.
    pub fn poll(&self) -> Vec<PathBuf> {
        let mut changed = HashSet::new();
        while let Ok(event) = self.rx.try_recv() {
            for path in event.paths {
                if !path.starts_with(&self.root) {
                    continue;
                }
                if path.components().any(|c| {
                    matches!(
                        c.as_os_str().to_str(),
                        Some(".git" | "target" | "node_modules" | ".venv" | "__pycache__")
                    )
                }) {
                    continue;
                }
                changed.insert(path);
            }
        }
        changed.into_iter().collect()
    }
}
