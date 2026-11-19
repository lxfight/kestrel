//! Project-wide text search running on a background thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Search options as entered in the Search panel.
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Match file names only; file contents are never read or searched.
    pub filename_only: bool,
    /// Glob filter applied to relative paths, e.g. `*.rs` (empty = no filter).
    pub include: String,
    /// Glob exclusion, e.g. `*test*` (empty = none).
    pub exclude: String,
    pub max_results: usize,
}

impl SearchQuery {
    pub fn build(&self) -> Result<regex::Regex, String> {
        let mut pattern = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        let pattern = if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
            pattern
        } else {
            pattern
        };
        let mut builder = regex::RegexBuilder::new(&pattern);
        builder.case_insensitive(!self.case_sensitive);
        builder.size_limit(4 * 1024 * 1024);
        builder.build().map_err(|e| format!("正则表达式无效: {e}"))
    }
}

/// One match inside a file. All offsets are byte offsets into the file.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub path: PathBuf,
    pub matches: Vec<SearchMatch>,
    /// Set when the per-file match cap was hit.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct SearchMatch {
    /// 1-based line number.
    pub line: u32,
    /// 1-based character column.
    pub column: u32,
    /// Byte offset of the match start, used to place the caret exactly.
    pub byte_offset: usize,
    /// Trimmed preview of the matching line, capped at ~160 chars.
    pub preview: String,
}

#[derive(Clone, Debug)]
pub enum SearchEvent {
    /// A batch of per-file results (sent incrementally while searching).
    Results(Vec<SearchResult>),
    /// Progress numbers, roughly every 100 scanned files.
    Progress {
        scanned: usize,
        matched_files: usize,
    },
    /// Search finished (or was cancelled).
    Done {
        scanned: usize,
        matched_files: usize,
        cancelled: bool,
    },
    /// Fatal error (bad regex is reported by `SearchQuery::build`).
    Error(String),
}

pub struct ProjectSearch {
    rx: Option<Receiver<SearchEvent>>,
    cancel: Arc<AtomicBool>,
    running: bool,
}

impl ProjectSearch {
    pub fn new() -> Self {
        Self {
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
        }
    }

    /// Start a search over `files` (already collected by the file index).
    pub fn start(&mut self, root: PathBuf, files: Vec<PathBuf>, query: SearchQuery) {
        self.cancel();
        let regex = match query.build() {
            Ok(regex) => regex,
            Err(err) => {
                let (tx, rx) = channel();
                let _ = tx.send(SearchEvent::Error(err));
                drop(tx);
                self.rx = Some(rx);
                self.running = false;
                return;
            }
        };
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let include = query.include.trim().to_string();
        let exclude = query.exclude.trim().to_string();
        let filename_only = query.filename_only;
        let max_results = if query.max_results == 0 {
            2000
        } else {
            query.max_results
        };
        std::thread::spawn(move || {
            run_search(
                &root,
                &files,
                &regex,
                &include,
                &exclude,
                filename_only,
                max_results,
                &cancel_thread,
                &tx,
            );
        });
        self.rx = Some(rx);
        self.cancel = cancel;
        self.running = true;
    }

    /// Drain all events produced so far.
    pub fn poll(&mut self) -> Vec<SearchEvent> {
        let mut events = Vec::new();
        let Some(rx) = &self.rx else {
            return events;
        };
        while let Ok(event) = rx.try_recv() {
            if matches!(event, SearchEvent::Done { .. } | SearchEvent::Error(_)) {
                self.running = false;
            }
            events.push(event);
        }
        events
    }

    /// Whether a search is currently running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn cancel(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.rx = None;
        self.running = false;
    }
}

/// Matches collected per file before the file is marked truncated.
const PER_FILE_LIMIT: usize = 200;
/// Flush a `Results` batch once it holds this many files...
const BATCH_FILES: usize = 16;
/// ...or this many matches, whichever comes first.
const BATCH_MATCHES: usize = 64;
/// Preview is clipped to this many characters plus an ellipsis.
const PREVIEW_CHARS: usize = 160;
/// Cancellation is re-checked every this many matches inside a file.
const CANCEL_CHECK_INTERVAL: usize = 200;

/// One file's match set; content scanning is shared by the project search loop.
struct ContentScan {
    hits: Vec<SearchMatch>,
    truncated: bool,
    cancelled: bool,
}

#[allow(clippy::too_many_arguments)]
fn run_search(
    root: &Path,
    files: &[PathBuf],
    regex: &regex::Regex,
    include: &str,
    exclude: &str,
    filename_only: bool,
    max_results: usize,
    cancel: &AtomicBool,
    tx: &Sender<SearchEvent>,
) {
    let include = parse_filters(include);
    let exclude = parse_filters(exclude);
    let mut scanned = 0usize;
    let mut matched_files = 0usize;
    let mut total_matches = 0usize;
    let mut batch: Vec<SearchResult> = Vec::new();
    let mut batch_matches = 0usize;
    let mut cancelled = false;

    for path in files {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let rel = relative_path(path, root);
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if !path_allowed(&rel, &name, &include, &exclude) {
            continue;
        }
        scanned += 1;
        if scanned.is_multiple_of(100) {
            let _ = tx.send(SearchEvent::Progress {
                scanned,
                matched_files,
            });
        }
        // Name-only mode never opens the file: binary or oversized files still
        // match by name.
        let (hits, truncated) = if filename_only {
            if !regex.is_match(&name) {
                continue;
            }
            (
                vec![SearchMatch {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                    preview: rel,
                }],
                false,
            )
        } else {
            let Some(content) = crate::workspace::index::read_searchable(path) else {
                continue;
            };
            let scan = scan_content(&content, regex, max_results - total_matches, cancel);
            if scan.cancelled {
                cancelled = true;
                break;
            }
            (scan.hits, scan.truncated)
        };
        if hits.is_empty() {
            continue;
        }
        matched_files += 1;
        total_matches += hits.len();
        batch_matches += hits.len();
        batch.push(SearchResult {
            path: path.clone(),
            matches: hits,
            truncated,
        });
        if batch.len() >= BATCH_FILES || batch_matches >= BATCH_MATCHES {
            let _ = tx.send(SearchEvent::Results(std::mem::take(&mut batch)));
            batch_matches = 0;
        }
        if total_matches >= max_results {
            break;
        }
    }

    if !batch.is_empty() {
        let _ = tx.send(SearchEvent::Results(batch));
    }
    let _ = tx.send(SearchEvent::Done {
        scanned,
        matched_files,
        cancelled,
    });
}

/// Scan one file's text, capping at `cap` total hits and `PER_FILE_LIMIT` per file.
fn scan_content(
    content: &str,
    regex: &regex::Regex,
    cap: usize,
    cancel: &AtomicBool,
) -> ContentScan {
    let mut hits: Vec<SearchMatch> = Vec::new();
    let mut truncated = false;
    let mut line = 1u32;
    let mut line_start = 0usize;
    let mut pos = 0usize;
    for m in regex.find_iter(content) {
        if hits.len() >= cap {
            break;
        }
        if hits.len() >= PER_FILE_LIMIT {
            truncated = true;
            break;
        }
        if hits.len().is_multiple_of(CANCEL_CHECK_INTERVAL) && cancel.load(Ordering::Relaxed) {
            return ContentScan {
                hits,
                truncated,
                cancelled: true,
            };
        }
        let start = m.start();
        for (i, byte) in content.as_bytes()[pos..start].iter().enumerate() {
            if *byte == b'\n' {
                line += 1;
                line_start = pos + i + 1;
            }
        }
        pos = start;
        let column = content[line_start..start].chars().count() as u32 + 1;
        hits.push(SearchMatch {
            line,
            column,
            byte_offset: start,
            preview: line_preview(content, line_start),
        });
    }
    ContentScan {
        hits,
        truncated,
        cancelled: false,
    }
}

/// Split a comma-separated glob list, dropping empty entries.
fn parse_filters(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn path_allowed(rel: &str, name: &str, include: &[String], exclude: &[String]) -> bool {
    (include.is_empty() || filter_matches(include, rel, name))
        && !filter_matches(exclude, rel, name)
}

/// A filter matches either the root-relative path or the bare file name.
fn filter_matches(filters: &[String], rel: &str, name: &str) -> bool {
    filters
        .iter()
        .any(|filter| wildcard_match(filter, rel) || wildcard_match(filter, name))
}

/// Simple glob match supporting `*` (any sequence, including `/`) and `?` (one character).
/// Case-insensitive on Windows.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (pattern, text) = if cfg!(windows) {
        (pattern.to_lowercase(), text.to_lowercase())
    } else {
        (pattern.to_string(), text.to_string())
    };
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut backtrack: Option<(usize, usize)> = None;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            backtrack = Some((pi, ti));
            pi += 1;
        } else if let Some((star, matched)) = backtrack {
            pi = star + 1;
            ti = matched + 1;
            backtrack = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn relative_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Trimmed, tab-expanded, length-capped rendering of the line starting at `line_start`.
fn line_preview(content: &str, line_start: usize) -> String {
    let line_end = content[line_start..]
        .find('\n')
        .map_or(content.len(), |i| line_start + i);
    let expanded = content[line_start..line_end].replace('\t', "    ");
    let line = expanded.trim();
    let mut preview: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

/// Replace all matches of `query` in `path` on disk. Returns the number of replacements made.
pub fn replace_in_file(path: &Path, query: &SearchQuery, replacement: &str) -> Result<usize, String> {
    if query.filename_only {
        return Ok(0);
    }
    let regex = query.build()?;
    let Some(content) = crate::workspace::index::read_searchable(path) else {
        return Ok(0);
    };
    let mut count = 0usize;
    let mut out = String::with_capacity(content.len());
    let mut last = 0usize;
    for m in regex.find_iter(&content) {
        if m.start() == m.end() {
            continue;
        }
        out.push_str(&content[last..m.start()]);
        if query.regex {
            let slice = &content[m.start()..m.end()];
            out.push_str(&regex.replace(slice, replacement));
        } else {
            out.push_str(replacement);
        }
        last = m.end();
        count += 1;
    }
    if count == 0 {
        return Ok(0);
    }
    out.push_str(&content[last..]);
    std::fs::write(path, out.as_bytes()).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(count)
}

/// Replace only the match that starts at `byte_offset` in `path`.
///
/// The file is re-read and the regex re-run so a stale search result cannot
/// splice the wrong span; a moved/missing match returns `Ok(false)`.
pub fn replace_match_at(
    path: &Path,
    query: &SearchQuery,
    replacement: &str,
    byte_offset: usize,
) -> Result<bool, String> {
    let regex = query.build()?;
    let Some(content) = crate::workspace::index::read_searchable(path) else {
        return Ok(false);
    };
    let Some(found) = regex
        .find_iter(&content)
        .find(|m| m.start() == byte_offset && m.start() != m.end())
    else {
        return Ok(false);
    };
    let mut out = String::with_capacity(content.len() + replacement.len());
    out.push_str(&content[..found.start()]);
    if query.regex {
        out.push_str(&regex.replace(&content[found.start()..found.end()], replacement));
    } else {
        out.push_str(replacement);
    }
    out.push_str(&content[found.end()..]);
    std::fs::write(path, out.as_bytes())
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::index::walk_project;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn replace_match_at_replaces_only_the_target_hit() {
        let path =
            std::env::temp_dir().join(format!("kestrel_replace_one_{}.txt", std::process::id()));
        fs::write(&path, "needle one\nneedle two\n").unwrap();
        let query = query("needle");
        assert!(replace_match_at(&path, &query, "X", 11).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "needle one\nX two\n");
        // A stale offset no longer matches and must not touch the file.
        assert!(!replace_match_at(&path, &query, "X", 999).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "needle one\nX two\n");
        let _ = fs::remove_file(&path);
    }

    fn project_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!("kestrel_search_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src")).unwrap();
        fs::create_dir_all(base.join("tools")).unwrap();
        fs::write(
            base.join("src/lib.rs"),
            "let needle = 1;\nlet needle = 2;\n",
        )
        .unwrap();
        fs::write(base.join("tools/tool.rs"), "//\tneedle tool\n").unwrap();
        fs::write(
            base.join("notes.txt"),
            "needle one\nxx needle two\nNEEDLE upper\nneedles plural\n",
        )
        .unwrap();
        fs::write(base.join("blob.bin"), b"needle\0binary").unwrap();
        base
    }

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery {
            pattern: pattern.to_string(),
            ..Default::default()
        }
    }

    fn run_with(
        root: &Path,
        files: Vec<PathBuf>,
        query: SearchQuery,
    ) -> (Vec<SearchResult>, usize, usize) {
        let mut search = ProjectSearch::new();
        search.start(root.to_path_buf(), files, query);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut results = Vec::new();
        loop {
            for event in search.poll() {
                match event {
                    SearchEvent::Results(batch) => results.extend(batch),
                    SearchEvent::Progress { .. } => {}
                    SearchEvent::Done {
                        scanned,
                        matched_files,
                        cancelled,
                    } => {
                        assert!(!cancelled);
                        return (results, scanned, matched_files);
                    }
                    SearchEvent::Error(err) => panic!("search error: {err}"),
                }
            }
            assert!(Instant::now() < deadline, "search timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run(root: &Path, query: SearchQuery) -> (Vec<SearchResult>, usize, usize) {
        let mut files = walk_project(root);
        files.sort();
        run_with(root, files, query)
    }

    fn find<'a>(results: &'a [SearchResult], path_suffix: &str) -> &'a SearchResult {
        results
            .iter()
            .find(|r| r.path.ends_with(path_suffix))
            .unwrap_or_else(|| panic!("no result for {path_suffix}"))
    }

    fn check(m: &SearchMatch, line: u32, column: u32, byte_offset: usize, preview: &str) {
        assert_eq!(
            (m.line, m.column, m.byte_offset, m.preview.as_str()),
            (line, column, byte_offset, preview)
        );
    }

    #[test]
    fn project_search_matches_public_behavior() {
        let root = project_root();

        // Literal, case-insensitive by default.
        let (results, scanned, matched_files) = run(&root, query("needle"));
        assert_eq!(scanned, 4); // blob.bin is scanned, then skipped as binary
        assert_eq!(matched_files, 3);
        assert_eq!(results.len(), 3);

        let lib = find(&results, "src/lib.rs");
        assert_eq!(lib.matches.len(), 2);
        assert!(!lib.truncated);
        check(&lib.matches[0], 1, 5, 4, "let needle = 1;");
        check(&lib.matches[1], 2, 5, 20, "let needle = 2;");

        let notes = find(&results, "notes.txt");
        assert_eq!(notes.matches.len(), 4);
        check(&notes.matches[0], 1, 1, 0, "needle one");
        check(&notes.matches[1], 2, 4, 14, "xx needle two");
        check(&notes.matches[2], 3, 1, 25, "NEEDLE upper");
        check(&notes.matches[3], 4, 1, 38, "needles plural");

        let tool = find(&results, "tools/tool.rs");
        assert_eq!(tool.matches.len(), 1);
        check(&tool.matches[0], 1, 4, 3, "//    needle tool");

        // Regex search.
        let mut regex_query = query(r"needle\s*=\s*\d");
        regex_query.regex = true;
        let (results, _, matched_files) = run(&root, regex_query);
        assert_eq!(matched_files, 1);
        let lib = find(&results, "src/lib.rs");
        assert_eq!(lib.matches.len(), 2);
        check(&lib.matches[0], 1, 5, 4, "let needle = 1;");
        check(&lib.matches[1], 2, 5, 20, "let needle = 2;");

        // Case-sensitive and insensitive.
        let mut sensitive = query("needle");
        sensitive.case_sensitive = true;
        let (results, _, matched_files) = run(&root, sensitive);
        assert_eq!(matched_files, 3);
        assert_eq!(find(&results, "notes.txt").matches.len(), 3);
        assert_eq!(find(&results, "src/lib.rs").matches.len(), 2);

        let (results, _, _) = run(&root, query("NEEDLE"));
        assert_eq!(find(&results, "notes.txt").matches.len(), 4);

        // Whole word: "needles" no longer matches.
        let mut whole_word = query("needle");
        whole_word.whole_word = true;
        let (results, _, matched_files) = run(&root, whole_word);
        assert_eq!(matched_files, 3);
        let notes = find(&results, "notes.txt");
        assert_eq!(notes.matches.len(), 3);
        assert!(notes.matches.iter().all(|m| m.preview != "needles plural"));

        // Include filter.
        let mut include = query("needle");
        include.include = "*.rs".to_string();
        let (results, scanned, matched_files) = run(&root, include);
        assert_eq!((matched_files, scanned, results.len()), (2, 2, 2));
        assert!(
            results
                .iter()
                .all(|r| r.path.extension().is_some_and(|ext| ext == "rs"))
        );

        // Exclude filter, comma-separated alternatives.
        let mut exclude = query("needle");
        exclude.exclude = "*.txt,*.bin".to_string();
        let (_, scanned, matched_files) = run(&root, exclude);
        assert_eq!((matched_files, scanned), (2, 2));

        // Global result cap stops the search early.
        let mut capped = query("needle");
        capped.max_results = 3;
        let (results, _, matched_files) = run(&root, capped);
        assert_eq!(matched_files, 1);
        assert_eq!(results.iter().map(|r| r.matches.len()).sum::<usize>(), 3);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn per_file_cap_marks_truncated() {
        let root =
            std::env::temp_dir().join(format!("kestrel_search_test_cap_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("many.txt"), "needle\n".repeat(250)).unwrap();

        let (results, _, matched_files) = run(&root, query("needle"));
        assert_eq!(matched_files, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 200);
        assert!(results[0].truncated);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancel_stops_search() {
        let missing = std::env::temp_dir().join("kestrel_search_missing_root");
        let mut search = ProjectSearch::new();
        search.start(missing.clone(), vec![missing.join("x.rs")], query("needle"));
        assert!(search.is_running());
        search.cancel();
        assert!(!search.is_running());
        assert!(search.poll().is_empty());
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("*", "src/main.rs"));
        assert!(wildcard_match("*.rs", "src/main.rs"));
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "src/main.py"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match("src/?ain.rs", "src/main.rs"));
        assert!(wildcard_match("src/*.rs", "src/deep/lib.rs"));
        assert!(wildcard_match("", ""));
        assert!(!wildcard_match("", "x"));
        assert!(!wildcard_match("*.rs", ""));
        if cfg!(windows) {
            assert!(wildcard_match("*.RS", "src/main.rs"));
        } else {
            assert!(!wildcard_match("*.RS", "src/main.rs"));
        }
    }

    #[test]
    fn filename_only_searches_names_without_reading_content() {
        let root = std::env::temp_dir().join(format!("kestrel_search_names_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("needle_one.txt"), "nothing here").unwrap();
        fs::write(root.join("contents.txt"), "needle in content").unwrap();
        fs::write(root.join("sub/needle_two.rs"), "x").unwrap();
        fs::write(root.join("blob.bin"), b"needle\x00binary").unwrap();

        let mut name_only = query("needle");
        name_only.filename_only = true;
        let (results, scanned, matched_files) = run(&root, name_only);
        assert_eq!(scanned, 4);
        assert_eq!(matched_files, 2);
        let names: Vec<String> = results
            .iter()
            .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["needle_one.txt", "needle_two.rs"]);
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].byte_offset, 0);
        assert!(!results[0].truncated);

        // Content mode still sees only the file body, proving the mode switch.
        let (content_results, _, content_files) = run(&root, query("needle"));
        assert_eq!(content_files, 1);
        assert!(content_results[0].path.ends_with("contents.txt"));

        // Include filters still constrain names.
        let mut filtered = query("needle");
        filtered.filename_only = true;
        filtered.include = "*.txt".to_string();
        let (results, _, matched_files) = run(&root, filtered);
        assert_eq!(matched_files, 1);
        assert!(results[0].path.ends_with("needle_one.txt"));

        fs::remove_dir_all(&root).unwrap();
    }
}
