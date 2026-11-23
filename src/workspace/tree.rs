//! Lazily-loaded, cached directory tree for the Explorer panel.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One visible line of the tree.
#[derive(Clone, Debug)]
pub struct FileRow {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
}

pub struct FileTree {
    root: Option<PathBuf>,
    expanded: HashSet<PathBuf>,
    /// Cached directory listings: sorted (name, is_dir).
    dirs: HashMap<PathBuf, Vec<(String, bool)>>,
    pub show_hidden: bool,
    cached_rows: Arc<[FileRow]>,
    cached_show_hidden: bool,
    rows_dirty: bool,
}

impl FileTree {
    pub fn new() -> Self {
        Self {
            root: None,
            expanded: HashSet::new(),
            dirs: HashMap::new(),
            show_hidden: false,
            cached_rows: Arc::from(Vec::new()),
            cached_show_hidden: false,
            rows_dirty: true,
        }
    }

    /// Point the tree at a new folder. Expanding the root by default.
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = Some(root.clone());
        self.expanded.clear();
        self.dirs.clear();
        self.expanded.insert(root);
        self.rows_dirty = true;
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn expand(&mut self, path: &Path) {
        if self.expanded.insert(path.to_path_buf()) {
            self.rows_dirty = true;
        }
    }

    pub fn toggle_dir(&mut self, path: &Path) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rows_dirty = true;
    }

    /// Collapse all expanded subdirectories; if none are expanded, collapse the root directory itself.
    pub fn collapse_all(&mut self) {
        let Some(root) = self.root.clone() else {
            self.expanded.clear();
            self.rows_dirty = true;
            return;
        };
        let has_subdirs = self.expanded.iter().any(|p| p != &root);
        self.expanded.clear();
        if has_subdirs {
            self.expanded.insert(root);
        }
        self.rows_dirty = true;
    }

    /// Flattened list of currently visible rows, cached across frames until mutated.
    pub fn rows(&mut self) -> Arc<[FileRow]> {
        if self.cached_show_hidden != self.show_hidden {
            self.cached_show_hidden = self.show_hidden;
            self.dirs.clear();
            self.rows_dirty = true;
        }
        if !self.rows_dirty {
            return Arc::clone(&self.cached_rows);
        }
        let mut out = Vec::new();
        if let Some(root) = self.root.clone() {
            self.walk(&root, 0, &mut out);
        }
        self.cached_rows = Arc::from(out);
        self.rows_dirty = false;
        Arc::clone(&self.cached_rows)
    }

    fn walk(&mut self, dir: &Path, depth: usize, out: &mut Vec<FileRow>) {
        if !self.expanded.contains(dir) {
            return;
        }
        let entries = self.children(dir);
        for (name, is_dir) in entries {
            let path = dir.join(&name);
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(FileRow {
                path: path.clone(),
                name,
                is_dir,
                depth,
            });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    fn children(&mut self, dir: &Path) -> Vec<(String, bool)> {
        if let Some(cached) = self.dirs.get(dir) {
            return cached.clone();
        }
        let mut entries = Vec::new();
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !self.show_hidden && name.starts_with('.') {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };

                // Avoid an extra stat syscall on regular files/dirs; only follow symlinks.
                let is_dir = if file_type.is_symlink() {
                    entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
                } else {
                    file_type.is_dir()
                };
                entries.push((name, is_dir));
            }
        }
        entries.sort_by_cached_key(|(name, is_dir)| (!*is_dir, name.to_lowercase()));
        self.dirs.insert(dir.to_path_buf(), entries.clone());
        entries
    }

    /// Drop cached listings so the next paint re-reads the disk.
    pub fn reload(&mut self) {
        self.dirs.clear();
        self.rows_dirty = true;
    }

    pub fn reload_dir(&mut self, dir: &Path) {
        self.dirs.remove(dir);
        self.rows_dirty = true;
    }
}

/// Create an empty file; fails if it already exists.
pub fn create_file(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let path = parent.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok(path)
}

pub fn create_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let path = parent.join(name);
    std::fs::create_dir(&path)?;
    Ok(path)
}

pub fn rename(path: &Path, new_name: &str) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let new_path = parent.join(new_name);
    if new_path.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "目标已存在"));
    }
    std::fs::rename(path, &new_path)?;
    Ok(new_path)
}

/// Recycle/move to trash is overkill here: delete recursively.
pub fn delete(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

pub fn reveal_in_explorer(path: &Path) {
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

fn unique_dest_path(dir: &Path, base_name: &str, is_dir: bool) -> PathBuf {
    let candidate = dir.join(base_name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = if is_dir {
        (base_name.to_string(), String::new())
    } else if let Some(dot) = base_name.rfind('.')
        && dot > 0
    {
        (
            base_name[..dot].to_string(),
            base_name[dot..].to_string(),
        )
    } else {
        (base_name.to_string(), String::new())
    };
    let first = dir.join(format!("{stem} - 副本{ext}"));
    if !first.exists() {
        return first;
    }
    let mut n = 2usize;
    loop {
        let next = dir.join(format!("{stem} - 副本 ({n}){ext}"));
        if !next.exists() {
            return next;
        }
        n += 1;
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn copy_path_to_dir(src: &Path, dst_dir: &Path) -> io::Result<PathBuf> {
    if !dst_dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "目标目录不存在"));
    }
    if src.is_dir() && dst_dir.starts_with(src) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "不能将文件夹复制到自身子目录中",
        ));
    }
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "无效路径"))?
        .to_string_lossy();
    let dst = unique_dest_path(dst_dir, &name, src.is_dir());
    if src.is_dir() {
        copy_dir_recursive(src, &dst)?;
    } else {
        std::fs::copy(src, &dst)?;
    }
    Ok(dst)
}

pub fn duplicate_path(src: &Path) -> io::Result<PathBuf> {
    let parent = src
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "无效父目录"))?;
    copy_path_to_dir(src, parent)
}

pub fn move_path_to_dir(src: &Path, dst_dir: &Path) -> io::Result<PathBuf> {
    if !dst_dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "目标目录不存在"));
    }
    if src.is_dir() && dst_dir.starts_with(src) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "不能将文件夹移动到自身子目录中",
        ));
    }
    if src.parent() == Some(dst_dir) {
        return Ok(src.to_path_buf());
    }
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "无效路径"))?;
    let dst = dst_dir.join(name);
    if dst.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "目标已存在"));
    }
    if std::fs::rename(src, &dst).is_err() {
        if src.is_dir() {
            copy_dir_recursive(src, &dst)?;
            std::fs::remove_dir_all(src)?;
        } else {
            std::fs::copy(src, &dst)?;
            std::fs::remove_file(src)?;
        }
    }
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_subdirectories_collapse_properly() {
        let mut tree = FileTree::new();
        let root = PathBuf::from("workspace_root");
        let sub = root.join("src");
        tree.set_root(root.clone());
        assert!(tree.is_expanded(&root));

        tree.expand(&sub);
        assert!(tree.is_expanded(&sub));

        // First collapse_all collapses expanded subdirectories while keeping root open.
        tree.collapse_all();
        assert!(tree.is_expanded(&root));
        assert!(!tree.is_expanded(&sub));

        // Second collapse_all (when no subdirs are open) collapses the root itself.
        tree.collapse_all();
        assert!(!tree.is_expanded(&root));

        // Clicking the root header toggles it back open.
        tree.toggle_dir(&root);
        assert!(tree.is_expanded(&root));
    }

    #[test]
    fn duplicate_copy_and_move_paths() {
        let base = std::env::temp_dir().join(format!("kestrel_tree_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        let file = base.join("demo.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let dup = duplicate_path(&file).unwrap();
        assert_eq!(dup.file_name().unwrap().to_string_lossy(), "demo - 副本.rs");
        assert!(dup.exists());

        let moved = move_path_to_dir(&dup, &base.join("sub")).unwrap();
        assert!(!dup.exists());
        assert!(moved.exists());

        assert!(copy_path_to_dir(&base.join("sub"), &base.join("sub")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rows_cache_reuses_arc_until_mutated() {
        let base = std::env::temp_dir().join(format!("kestrel_tree_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), "hello").unwrap();
        std::fs::write(base.join("sub/b.txt"), "world").unwrap();

        let mut tree = FileTree::new();
        tree.set_root(base.clone());
        let r1 = tree.rows();
        let r2 = tree.rows();
        assert!(std::sync::Arc::ptr_eq(&r1, &r2));
        assert_eq!(r1.len(), 2);

        tree.expand(&base.join("sub"));
        let r3 = tree.rows();
        assert!(!std::sync::Arc::ptr_eq(&r1, &r3));
        assert_eq!(r3.len(), 3);

        let _ = std::fs::remove_dir_all(&base);
    }
}
