//! Native Git integration: branch detection, working-tree status, staging, commits, and line diffs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

/// Number of commits the SCM history section loads.
pub const COMMIT_HISTORY_LIMIT: usize = 20;
const COMMIT_HISTORY_REFRESH: Duration = Duration::from_secs(5);
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl GitFileStatus {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitChange {
    pub path: PathBuf,
    pub rel_path: String,
    /// Previous path for renames (used as the diff base).
    pub orig_path: Option<String>,
    pub status: GitFileStatus,
    pub staged: bool,
}

/// One entry of the SCM history list.
#[derive(Clone, Debug)]
pub struct GitCommit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub time: String,
    pub subject: String,
}

/// One entry of the SCM stash list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStash {
    /// Stash reflog index, i.e. the `n` in `stash@{n}`.
    pub index: usize,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl CommitFileStatus {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
        }
    }
}

/// One file touched by a commit (`git show --name-status`).
#[derive(Clone, Debug)]
pub struct CommitFile {
    pub rel_path: String,
    pub status: CommitFileStatus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineDiffKind {
    Unchanged,
    Added,
    Modified,
    DeletedBelow,
}

#[derive(Clone, Debug, Default)]
pub struct GitSnapshot {
    pub is_repo: bool,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub branches: Vec<String>,
    pub changes: Vec<GitChange>,
    pub stashes: Vec<GitStash>,
    pub by_path: HashMap<PathBuf, GitFileStatus>,
}

pub struct GitService {
    pub snapshot: GitSnapshot,
    pub commit_message: String,
    pub logs: Vec<String>,
    rx: Option<Receiver<GitSnapshot>>,
    last_refresh: Instant,

    // SCM panel state lives here so `KestrelApp` needs no Git-specific fields.
    pub amend: bool,
    pub branch_panel_open: bool,
    pub branch_filter: String,
    pub new_branch: String,
    pub commits_open: bool,
    pub staged_open: bool,
    pub changes_open: bool,
    pub stashes_open: bool,
    pub message_picker_open: bool,
    pub commits: Vec<GitCommit>,
    pub expanded_commit: Option<String>,
    pub commit_files: HashMap<String, Vec<CommitFile>>,
    commits_rx: Option<Receiver<Vec<GitCommit>>>,
    commit_files_rx: Option<Receiver<(String, Vec<CommitFile>)>>,
    loading_commit_files: Option<String>,
    discard_confirm_until: Option<Instant>,
    undo_confirm_until: Option<Instant>,
    last_commits_load: Instant,
}

impl GitService {
    pub fn new() -> Self {
        Self {
            snapshot: GitSnapshot::default(),
            commit_message: String::new(),
            logs: Vec::new(),
            rx: None,
            last_refresh: Instant::now() - Duration::from_secs(60),
            amend: false,
            branch_panel_open: false,
            branch_filter: String::new(),
            new_branch: String::new(),
            commits_open: true,
            staged_open: true,
            changes_open: true,
            stashes_open: false,
            message_picker_open: false,
            commits: Vec::new(),
            expanded_commit: None,
            commit_files: HashMap::new(),
            commits_rx: None,
            commit_files_rx: None,
            loading_commit_files: None,
            discard_confirm_until: None,
            undo_confirm_until: None,
            last_commits_load: Instant::now() - Duration::from_secs(60),
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.snapshot = GitSnapshot::default();
        self.rx = None;
        self.commits.clear();
        self.commit_files.clear();
        self.expanded_commit = None;
        self.commits_rx = None;
        self.commit_files_rx = None;
        self.loading_commit_files = None;
        self.branch_panel_open = false;
        self.message_picker_open = false;
        self.discard_confirm_until = None;
        self.undo_confirm_until = None;
    }

    pub fn log(&mut self, message: impl Into<String>) {
        let msg = message.into();
        self.logs.push(msg);
        if self.logs.len() > 200 {
            self.logs.remove(0);
        }
    }

    /// Fast synchronous branch read from `.git/HEAD` before background scan completes.
    pub fn prime_fast(&mut self, root: &Path) {
        let git_dir = root.join(".git");
        if !git_dir.exists() {
            self.snapshot.is_repo = false;
            return;
        }
        self.snapshot.is_repo = true;
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
            let trimmed = head.trim();
            if let Some(rest) = trimmed.strip_prefix("ref: refs/heads/") {
                self.snapshot.branch = rest.to_string();
            } else if trimmed.len() >= 7 {
                self.snapshot.branch = trimmed[..7].to_string();
            }
        }
        self.refresh(root);
    }

    /// Trigger a background `git status` & branch scan.
    pub fn refresh(&mut self, root: &Path) {
        let root = root.to_path_buf();
        self.last_refresh = Instant::now();
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("kestrel-git".into())
            .spawn(move || {
                let snap = collect_git_snapshot(&root);
                // A repo whose status scan failed (e.g. transient index lock) must not
                // replace the live snapshot with an empty "not a repo" one.
                if snap.is_repo || !root.join(".git").exists() {
                    let _ = tx.send(snap);
                }
            })
            .ok();
        self.rx = Some(rx);
    }

    /// Poll for finished background status snapshot.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut updated = false;
        while let Ok(snap) = rx.try_recv() {
            self.snapshot = snap;
            updated = true;
        }
        updated
    }

    pub fn status_of(&self, path: &Path) -> Option<GitFileStatus> {
        self.snapshot.by_path.get(path).copied()
    }

    // ------------------------------------------------------------------
    // Commit history (background, UI thread only polls)
    // ------------------------------------------------------------------

    /// Kick off a background `git log` (at most [`COMMIT_HISTORY_LIMIT`] commits).
    /// Rate limited: no reload within 5 s of the previous one.
    pub fn load_commits(&mut self, root: &Path) {
        if self.commits_rx.is_some() || self.last_commits_load.elapsed() < COMMIT_HISTORY_REFRESH {
            return;
        }
        self.last_commits_load = Instant::now();
        let root = root.to_path_buf();
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("kestrel-git-log".into())
            .spawn(move || {
                let _ = tx.send(collect_commits(&root));
            })
            .ok();
        self.commits_rx = Some(rx);
    }

    pub fn poll_commits(&mut self) -> bool {
        let Some(rx) = self.commits_rx.take() else {
            return false;
        };
        let mut updated = false;
        loop {
            match rx.try_recv() {
                Ok(commits) => {
                    self.commits = commits;
                    updated = true;
                }
                Err(TryRecvError::Empty) => {
                    self.commits_rx = Some(rx);
                    break;
                }
                Err(TryRecvError::Disconnected) => break,
            }
        }
        updated
    }

    /// Kick off a background `git show --name-status` for one commit and cache the result.
    pub fn load_commit_files(&mut self, root: &Path, hash: &str) {
        if self.commit_files.contains_key(hash) {
            return;
        }
        if self.loading_commit_files.as_deref() == Some(hash) || self.commit_files_rx.is_some() {
            return;
        }
        self.loading_commit_files = Some(hash.to_string());
        let root = root.to_path_buf();
        let hash_owned = hash.to_string();
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("kestrel-git-show".into())
            .spawn(move || {
                let files = collect_commit_files(&root, &hash_owned);
                let _ = tx.send((hash_owned, files));
            })
            .ok();
        self.commit_files_rx = Some(rx);
    }

    pub fn poll_commit_files(&mut self) -> bool {
        let Some(rx) = self.commit_files_rx.take() else {
            return false;
        };
        let mut updated = false;
        loop {
            match rx.try_recv() {
                Ok((hash, files)) => {
                    self.commit_files.insert(hash, files);
                    self.loading_commit_files = None;
                    updated = true;
                }
                Err(TryRecvError::Empty) => {
                    self.commit_files_rx = Some(rx);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    self.loading_commit_files = None;
                    break;
                }
            }
        }
        updated
    }

    pub fn commits_loading(&self) -> bool {
        self.commits_rx.is_some()
    }

    pub fn commit_files_loading(&self) -> bool {
        self.commit_files_rx.is_some()
    }

    /// Forget the cached history position so the next panel frame reloads it.
    fn invalidate_commits(&mut self) {
        self.last_commits_load = Instant::now() - Duration::from_secs(60);
    }

    // ------------------------------------------------------------------
    // Two-step confirmations for destructive actions (UI state kept here)
    // ------------------------------------------------------------------

    pub fn discard_confirm_armed(&self) -> bool {
        confirm_armed(self.discard_confirm_until)
    }

    pub fn arm_discard_confirm(&mut self) {
        self.discard_confirm_until = Some(Instant::now() + CONFIRM_WINDOW);
    }

    pub fn clear_discard_confirm(&mut self) {
        self.discard_confirm_until = None;
    }

    pub fn undo_confirm_armed(&self) -> bool {
        confirm_armed(self.undo_confirm_until)
    }

    pub fn arm_undo_confirm(&mut self) {
        self.undo_confirm_until = Some(Instant::now() + CONFIRM_WINDOW);
    }

    pub fn clear_undo_confirm(&mut self) {
        self.undo_confirm_until = None;
    }

    pub fn stage_file(&mut self, root: &Path, path: &Path) {
        let rel = path.strip_prefix(root).unwrap_or(path);
        // `-A` also stages deletions, which plain `git add` skips.
        let output = git_cmd(root)
            .arg("add")
            .arg("-A")
            .arg("--")
            .arg(rel)
            .output();
        if let Ok(out) = output {
            self.log(format!("git add -A {} -> {}", rel.display(), out.status));
        }
        self.refresh(root);
    }

    pub fn unstage_file(&mut self, root: &Path, path: &Path) {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let out = git_cmd(root)
            .arg("reset")
            .arg("HEAD")
            .arg("--")
            .arg(rel)
            .output();
        let reset_ok = out.as_ref().is_ok_and(|o| o.status.success());
        if reset_ok {
            if let Ok(out) = out {
                self.log(format!(
                    "git reset HEAD {} -> {}",
                    rel.display(),
                    out.status
                ));
            }
        } else {
            // No HEAD yet (fresh repo): drop the path from the index instead.
            let _ = git_cmd(root)
                .arg("rm")
                .arg("--cached")
                .arg("-r")
                .arg("--")
                .arg(rel)
                .output();
            self.log(format!("git rm --cached -r {}", rel.display()));
        }
        self.refresh(root);
    }

    pub fn stage_all(&mut self, root: &Path) {
        if let Ok(out) = git_cmd(root).arg("add").arg("-A").output() {
            self.log(format!("git add -A -> {}", out.status));
        }
        self.refresh(root);
    }

    pub fn unstage_all(&mut self, root: &Path) {
        if let Ok(out) = git_cmd(root).arg("reset").output() {
            self.log(format!("git reset -> {}", out.status));
        }
        self.refresh(root);
    }

    pub fn discard_file(&mut self, root: &Path, path: &Path) {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let status = self.status_of(path);
        if status == Some(GitFileStatus::Untracked) {
            let _ = std::fs::remove_file(path);
            self.log(format!("removed untracked {}", rel.display()));
        } else if let Ok(out) = git_cmd(root).arg("checkout").arg("--").arg(rel).output() {
            self.log(format!(
                "git checkout -- {} -> {}",
                rel.display(),
                out.status
            ));
        }
        self.refresh(root);
    }

    /// Discard every working-tree change: unstage, restore tracked files, delete the
    /// untracked files listed in the current snapshot (never a `git clean` scan).
    pub fn discard_all(&mut self, root: &Path) -> Result<(), String> {
        let untracked: Vec<PathBuf> = self
            .snapshot
            .changes
            .iter()
            .filter(|c| {
                c.status == GitFileStatus::Untracked
                    || (c.staged && c.status == GitFileStatus::Added)
            })
            .map(|c| c.path.clone())
            .collect();
        let _ = git_cmd(root).arg("reset").output();
        let checkout = git_cmd(root)
            .arg("checkout")
            .arg("--")
            .arg(".")
            .output()
            .map_err(|e| format!("无法执行 git checkout: {e}"))?;
        let mut removed = 0usize;
        for path in untracked {
            if path.is_file() && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        if !checkout.status.success() {
            self.refresh(root);
            return Err(git_error(&checkout));
        }
        self.log(format!("丢弃全部更改，删除 {removed} 个未跟踪文件"));
        self.refresh(root);
        Ok(())
    }

    pub fn commit(&mut self, root: &Path) -> Result<String, String> {
        let msg = self.commit_message.trim().to_string();
        if msg.is_empty() && !self.amend {
            return Err("请输入提交说明 (Commit Message)".to_string());
        }
        let amend = self.amend;
        // Plain commits keep the existing convenience: auto-stage when nothing is staged.
        // Amending must not silently pull in unrelated files.
        if !amend && !self.snapshot.changes.iter().any(|c| c.staged) {
            let _ = git_cmd(root).arg("add").arg("-A").output();
        }
        let mut cmd = git_cmd(root);
        cmd.arg("commit");
        if amend {
            if msg.is_empty() {
                cmd.arg("--amend").arg("--no-edit");
            } else {
                cmd.arg("--amend").arg("-m").arg(&msg);
            }
        } else {
            cmd.arg("-m").arg(&msg);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("无法执行 git commit: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        self.log(format!(
            "git commit{} {:?}: {} {}",
            if amend { " --amend" } else { " -m" },
            msg,
            stdout,
            stderr
        ));
        if out.status.success() {
            self.commit_message.clear();
            self.amend = false;
            self.invalidate_commits();
            self.refresh(root);
            Ok(stdout.lines().next().unwrap_or("提交成功").to_string())
        } else {
            Err(if stderr.is_empty() { stdout } else { stderr })
        }
    }

    pub fn checkout_branch(&mut self, root: &Path, branch: &str) -> Result<(), String> {
        let out = git_cmd(root)
            .arg("checkout")
            .arg(branch)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            self.log(format!("git checkout {branch}"));
            self.invalidate_commits();
            self.refresh(root);
            Ok(())
        } else {
            Err(git_error(&out))
        }
    }

    pub fn create_branch(&mut self, root: &Path, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("请输入新分支名称".to_string());
        }
        let out = git_cmd(root)
            .arg("checkout")
            .arg("-b")
            .arg(name)
            .output()
            .map_err(|e| format!("无法执行 git checkout: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log(format!("git checkout -b {name}"));
        self.new_branch.clear();
        self.branch_filter.clear();
        self.invalidate_commits();
        self.refresh(root);
        Ok(format!("已创建并切换到分支 {name}"))
    }

    pub fn delete_branch(&mut self, root: &Path, branch: &str) -> Result<String, String> {
        if branch == self.snapshot.branch {
            return Err("不能删除当前分支".to_string());
        }
        let out = git_cmd(root)
            .arg("branch")
            .arg("-d")
            .arg(branch)
            .output()
            .map_err(|e| format!("无法执行 git branch: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log(format!("git branch -d {branch}"));
        self.refresh(root);
        Ok(format!("已删除分支 {branch}"))
    }

    pub fn merge_branch(&mut self, root: &Path, branch: &str) -> Result<String, String> {
        if branch == self.snapshot.branch {
            return Err("不能合并当前分支".to_string());
        }
        let out = git_cmd(root)
            .arg("merge")
            .arg("--no-edit")
            .arg(branch)
            .output()
            .map_err(|e| format!("无法执行 git merge: {e}"))?;
        // A conflicted merge still changed the working tree, so always refresh.
        self.invalidate_commits();
        self.refresh(root);
        if out.status.success() {
            self.log(format!("git merge {branch}"));
            Ok(format!("已合并分支 {branch}"))
        } else {
            Err(git_error(&out))
        }
    }

    /// `git reset --soft HEAD~1`: drop the last commit, keep its changes staged.
    pub fn undo_last_commit(&mut self, root: &Path) -> Result<String, String> {
        let out = git_cmd(root)
            .args(["reset", "--soft", "HEAD~1"])
            .output()
            .map_err(|e| format!("无法执行 git reset: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log("git reset --soft HEAD~1");
        self.invalidate_commits();
        self.refresh(root);
        Ok("已撤销上次提交，改动保留在暂存区".to_string())
    }

    /// Stash the working tree (including untracked files). The commit message box
    /// doubles as the stash description when non-empty.
    pub fn stash_save(&mut self, root: &Path) -> Result<String, String> {
        let draft = self.commit_message.trim();
        let message = if draft.is_empty() {
            "Kestrel 自动储藏".to_string()
        } else {
            draft.to_string()
        };
        let out = git_cmd(root)
            .args(["stash", "push", "-u", "-m", &message])
            .output()
            .map_err(|e| format!("无法执行 git stash: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log(format!("git stash push -u -m {message:?}"));
        self.refresh(root);
        Ok(format!("已储藏当前更改：{message}"))
    }

    pub fn stash_pop(&mut self, root: &Path, index: usize) -> Result<String, String> {
        let out = git_cmd(root)
            .args(["stash", "pop", &stash_ref(index)])
            .output()
            .map_err(|e| format!("无法执行 git stash: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log(format!("git stash pop {}", stash_ref(index)));
        self.refresh(root);
        Ok(format!("已应用 stash@{{{index}}}"))
    }

    pub fn stash_drop(&mut self, root: &Path, index: usize) -> Result<String, String> {
        let out = git_cmd(root)
            .args(["stash", "drop", &stash_ref(index)])
            .output()
            .map_err(|e| format!("无法执行 git stash: {e}"))?;
        if !out.status.success() {
            return Err(git_error(&out));
        }
        self.log(format!("git stash drop {}", stash_ref(index)));
        self.refresh(root);
        Ok(format!("已删除 stash@{{{index}}}"))
    }
}

fn confirm_armed(until: Option<Instant>) -> bool {
    until.is_some_and(|until| Instant::now() < until)
}

fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

fn git_cmd(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Human readable message from a failed git invocation.
fn git_error(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        "git 命令执行失败".to_string()
    } else {
        stdout
    }
}

/// Read `file_path` as it existed in `commit`.
pub fn read_blob_at(root: &Path, commit: &str, rel_path: &str) -> Option<String> {
    let rel_slash = rel_path.replace(std::path::MAIN_SEPARATOR, "/");
    let out = git_cmd(root)
        .arg("show")
        .arg(format!("{commit}:{rel_slash}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Read the index (staged) version of a repo-relative path.
pub fn read_index_rel(root: &Path, rel_path: &str) -> Option<String> {
    let out = git_cmd(root)
        .arg("show")
        .arg(format!(":{rel_path}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Read a repo-relative path at `HEAD`.
pub fn read_head_rel(root: &Path, rel_path: &str) -> Option<String> {
    read_blob_at(root, "HEAD", rel_path)
}

/// Read the `HEAD` version of a file from Git, if available.
pub fn read_head_blob(root: &Path, file_path: &Path) -> Option<String> {
    let rel = file_path.strip_prefix(root).ok()?;
    let rel_slash = rel.to_string_lossy().replace('\\', "/");
    let out = git_cmd(root)
        .arg("show")
        .arg(format!("HEAD:{rel_slash}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn collect_git_snapshot(root: &Path) -> GitSnapshot {
    let mut snap = GitSnapshot::default();
    let Ok(out) = git_cmd(root)
        .args(["status", "--porcelain=v1", "-b", "-uall"])
        .output()
    else {
        return snap;
    };
    if !out.status.success() {
        return snap;
    }
    snap.is_repo = true;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_porcelain_status(&text, root, &mut snap);

    if let Ok(b_out) = git_cmd(root)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        && b_out.status.success()
    {
        snap.branches = String::from_utf8_lossy(&b_out.stderr)
            .lines()
            .chain(String::from_utf8_lossy(&b_out.stdout).lines())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Ok(s_out) = git_cmd(root)
        .args(["stash", "list", "--pretty=format:%gd%x1f%s"])
        .output()
        && s_out.status.success()
    {
        snap.stashes = parse_stash_list(&String::from_utf8_lossy(&s_out.stdout));
    }
    snap
}

fn collect_commits(root: &Path) -> Vec<GitCommit> {
    let Ok(out) = git_cmd(root)
        .args([
            "log",
            "--max-count=20",
            "--pretty=format:%H%x1f%h%x1f%an%x1f%ar%x1f%s",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_commit_log(&String::from_utf8_lossy(&out.stdout))
}

fn collect_commit_files(root: &Path, hash: &str) -> Vec<CommitFile> {
    let Ok(out) = git_cmd(root)
        .args(["show", "--name-status", "--format=", hash])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_commit_files(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `git log --pretty=format:%H%x1f%h%x1f%an%x1f%ar%x1f%s` output (one commit per line).
pub fn parse_commit_log(text: &str) -> Vec<GitCommit> {
    let mut commits = Vec::with_capacity(COMMIT_HISTORY_LIMIT);
    for line in text.lines() {
        let mut fields = line.split('\u{1f}');
        let hash = fields.next().unwrap_or("").trim();
        if hash.is_empty() || hash.len() < 7 {
            continue;
        }
        commits.push(GitCommit {
            hash: hash.to_string(),
            short: fields.next().unwrap_or("").trim().to_string(),
            author: fields.next().unwrap_or("").trim().to_string(),
            time: fields.next().unwrap_or("").trim().to_string(),
            subject: fields.next().unwrap_or("").trim().to_string(),
        });
    }
    commits
}

/// Parse `git stash list --pretty=format:%gd%x1f%s` output.
pub fn parse_stash_list(text: &str) -> Vec<GitStash> {
    let mut stashes = Vec::new();
    for (position, line) in text.lines().enumerate() {
        let Some((selector, message)) = line.split_once('\u{1f}') else {
            continue;
        };
        let index = selector
            .rsplit_once('{')
            .and_then(|(_, rest)| rest.trim_end_matches('}').parse().ok())
            .unwrap_or(position);
        stashes.push(GitStash {
            index,
            message: message.trim().to_string(),
        });
    }
    stashes
}

/// Parse `git show --name-status --format=` output; rename lines carry `old<TAB>new`.
pub fn parse_commit_files(text: &str) -> Vec<CommitFile> {
    let mut files = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let code = fields.next().unwrap_or("");
        let Some(flag) = code.chars().next() else {
            continue;
        };
        let status = match flag {
            'A' | 'C' => CommitFileStatus::Added,
            'D' => CommitFileStatus::Deleted,
            'R' => CommitFileStatus::Renamed,
            'M' | 'T' => CommitFileStatus::Modified,
            _ => continue,
        };
        let Some(path) = fields.next_back().map(str::trim).filter(|p| !p.is_empty()) else {
            continue;
        };
        files.push(CommitFile {
            rel_path: path.to_string(),
            status,
        });
    }
    files
}

pub fn parse_porcelain_status(text: &str, root: &Path, snap: &mut GitSnapshot) {
    for line in text.lines() {
        if let Some(branch_line) = line.strip_prefix("## ") {
            // e.g. "main...origin/main [ahead 1, behind 2]" or "No commits yet on main"
            let branch_part = branch_line
                .split("...")
                .next()
                .unwrap_or(branch_line)
                .split(' ')
                .next_back()
                .unwrap_or("HEAD");
            snap.branch = branch_part.trim().to_string();
            if let Some(bracket_start) = branch_line.find('[')
                && let Some(bracket_end) = branch_line[bracket_start..].find(']')
            {
                let inside = &branch_line[bracket_start + 1..bracket_start + bracket_end];
                for part in inside.split(',') {
                    let part = part.trim();
                    if let Some(n) = part.strip_prefix("ahead ") {
                        snap.ahead = n.trim().parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        snap.behind = n.trim().parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let raw_path = line[3..].trim();
        let (orig_path, rel_path) = if let Some((old_name, new_name)) = raw_path.split_once(" -> ") {
            (
                Some(old_name.trim_matches('"').to_string()),
                new_name.trim_matches('"').to_string(),
            )
        } else {
            (None, raw_path.trim_matches('"').to_string())
        };
        let full_path = root.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if x == '?' && y == '?' {
            snap.by_path
                .insert(full_path.clone(), GitFileStatus::Untracked);
            snap.changes.push(GitChange {
                path: full_path,
                rel_path,
                orig_path: None,
                status: GitFileStatus::Untracked,
                staged: false,
            });
            continue;
        }

        // Staged status (index X)
        if x != ' ' && x != '?' {
            let status = match x {
                'A' => GitFileStatus::Added,
                'D' => GitFileStatus::Deleted,
                'R' => GitFileStatus::Renamed,
                'U' => GitFileStatus::Conflicted,
                _ => GitFileStatus::Modified,
            };
            snap.by_path.insert(full_path.clone(), status);
            snap.changes.push(GitChange {
                path: full_path.clone(),
                rel_path: rel_path.clone(),
                orig_path: orig_path.clone(),
                status,
                staged: true,
            });
        }

        // Working tree status (Y)
        if y != ' ' && y != '?' {
            let status = match y {
                'D' => GitFileStatus::Deleted,
                'U' => GitFileStatus::Conflicted,
                _ => GitFileStatus::Modified,
            };
            snap.by_path.insert(full_path.clone(), status);
            snap.changes.push(GitChange {
                path: full_path,
                rel_path,
                orig_path,
                status,
                staged: false,
            });
        }
    }
}

/// Compute per-line diff markers between `baseline` (saved / HEAD text) and `current` text.
/// Returns a vector of length `current_lines.len()`.
pub fn compute_line_diff(baseline: &str, current: &str) -> Vec<LineDiffKind> {
    let old_lines: Vec<&str> = baseline.split('\n').collect();
    let new_lines: Vec<&str> = current.split('\n').collect();
    let n = old_lines.len();
    let m = new_lines.len();
    if m == 0 {
        return Vec::new();
    }
    if baseline == current {
        return vec![LineDiffKind::Unchanged; m];
    }

    // Trim common prefix and suffix for O(delta) performance on large files.
    let mut prefix = 0;
    while prefix < n && prefix < m && old_lines[prefix] == new_lines[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < (n - prefix)
        && suffix < (m - prefix)
        && old_lines[n - 1 - suffix] == new_lines[m - 1 - suffix]
    {
        suffix += 1;
    }

    let mut result = vec![LineDiffKind::Unchanged; m];
    let old_mid = &old_lines[prefix..n - suffix];
    let new_mid = &new_lines[prefix..m - suffix];

    if old_mid.is_empty() && !new_mid.is_empty() {
        for i in 0..new_mid.len() {
            result[prefix + i] = LineDiffKind::Added;
        }
        return result;
    }
    if !old_mid.is_empty() && new_mid.is_empty() {
        let mark_idx = prefix.min(m - 1);
        result[mark_idx] = LineDiffKind::DeletedBelow;
        return result;
    }

    // Bounded DP LCS for the changed window; if window is huge, use fast hash-set matching.
    if old_mid.len() * new_mid.len() <= 160_000 {
        let a = old_mid.len();
        let b = new_mid.len();
        let mut dp = vec![vec![0u16; b + 1]; a + 1];
        for i in (0..a).rev() {
            for j in (0..b).rev() {
                dp[i][j] = if old_mid[i] == new_mid[j] {
                    dp[i + 1][j + 1] + 1
                } else {
                    dp[i + 1][j].max(dp[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        let mut pending_deleted = 0usize;
        while i < a || j < b {
            if i < a && j < b && old_mid[i] == new_mid[j] {
                pending_deleted = 0;
                i += 1;
                j += 1;
            } else if i < a && (j == b || dp[i + 1][j] >= dp[i][j + 1]) {
                pending_deleted += 1;
                if j == b && prefix + b > 0 {
                    let idx = (prefix + b - 1).min(m - 1);
                    if result[idx] == LineDiffKind::Unchanged {
                        result[idx] = LineDiffKind::DeletedBelow;
                    }
                }
                i += 1;
            } else {
                result[prefix + j] = if pending_deleted > 0 {
                    pending_deleted -= 1;
                    LineDiffKind::Modified
                } else {
                    LineDiffKind::Added
                };
                j += 1;
            }
        }
    } else {
        for (j, line) in new_mid.iter().enumerate() {
            if !old_mid.contains(line) {
                result[prefix + j] = LineDiffKind::Modified;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parser_extracts_branch_and_changes() {
        let sample =
            "## main...origin/main [ahead 2, behind 1]\n M src/app.rs\nA  src/git.rs\n?? new.txt\n";
        let mut snap = GitSnapshot::default();
        parse_porcelain_status(sample, Path::new("/repo"), &mut snap);
        assert_eq!(snap.branch, "main");
        assert_eq!(snap.ahead, 2);
        assert_eq!(snap.behind, 1);
        assert_eq!(snap.changes.len(), 3);
        assert_eq!(snap.changes[0].status, GitFileStatus::Modified);
        assert!(!snap.changes[0].staged);
        assert_eq!(snap.changes[1].status, GitFileStatus::Added);
        assert!(snap.changes[1].staged);
        assert_eq!(snap.changes[2].status, GitFileStatus::Untracked);
    }

    #[test]
    fn line_diff_identifies_added_and_modified_lines() {
        let base = "a\nb\nc";
        let edited = "a\nB\nc\nd";
        let diff = compute_line_diff(base, edited);
        assert_eq!(
            diff,
            vec![
                LineDiffKind::Unchanged,
                LineDiffKind::Modified,
                LineDiffKind::Unchanged,
                LineDiffKind::Added,
            ]
        );
    }

    #[test]
    fn commit_log_parser_splits_fields() {
        let sample = "0123456789abcdef\u{1f}0123456\u{1f}Alice\u{1f}2 hours ago\u{1f}fix: parser\n\
                      fedcba9876543210\u{1f}fedcba9\u{1f}Bob\u{1f}3 days ago\u{1f}feat: ui\n";
        let commits = parse_commit_log(sample);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "0123456789abcdef");
        assert_eq!(commits[0].short, "0123456");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].time, "2 hours ago");
        assert_eq!(commits[0].subject, "fix: parser");
        assert_eq!(commits[1].subject, "feat: ui");
    }

    #[test]
    fn commit_files_parser_handles_rename_and_delete() {
        let sample = "\nA\tsrc/new.rs\nM\tsrc/app.rs\nD\told.txt\nR100\tsrc/a.rs\tsrc/b.rs\n";
        let files = parse_commit_files(sample);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].rel_path, "src/new.rs");
        assert_eq!(files[0].status, CommitFileStatus::Added);
        assert_eq!(files[1].status, CommitFileStatus::Modified);
        assert_eq!(files[2].status, CommitFileStatus::Deleted);
        assert_eq!(files[3].status, CommitFileStatus::Renamed);
        assert_eq!(files[3].rel_path, "src/b.rs");
    }

    #[test]
    fn stash_list_parser_reads_index_and_message() {
        let sample = "stash@{0}\u{1f}WIP on main: 1234567 editor split\n\
                      stash@{1}\u{1f}Kestrel 自动储藏\n";
        let stashes = parse_stash_list(sample);
        assert_eq!(stashes.len(), 2);
        assert_eq!(
            stashes[0],
            GitStash {
                index: 0,
                message: "WIP on main: 1234567 editor split".to_string(),
            }
        );
        assert_eq!(stashes[1].index, 1);
        assert_eq!(stashes[1].message, "Kestrel 自动储藏");
    }

    #[test]
    fn stash_list_parser_skips_malformed_lines() {
        let stashes = parse_stash_list("no separator here\nstash@{2}\u{1f}\n");
        assert_eq!(
            stashes,
            vec![GitStash {
                index: 2,
                message: String::new()
            }]
        );
    }

    #[test]
    fn discard_confirm_state_is_two_step() {
        let mut git = GitService::new();
        assert!(!git.discard_confirm_armed());
        git.arm_discard_confirm();
        assert!(git.discard_confirm_armed());
        git.clear_discard_confirm();
        assert!(!git.discard_confirm_armed());
    }
}
