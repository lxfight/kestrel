//! Clickable terminal links: OSC 8 hyperlinks, soft-wrapped URLs, and compiler
//! `file:line:col` / `file(line,col)` / Python traceback file locations.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use super::protocol::{OscLink, parse_file_uri};

/// Target opened by Ctrl+click on terminal output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalLinkTarget {
    Url(String),
    File {
        path: PathBuf,
        line: Option<usize>,
        col: Option<usize>,
    },
}

/// Matched link plus its visible screen highlight spans `(vis_row, start_col, end_col)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalLinkHit {
    pub target: TerminalLinkTarget,
    pub spans: Vec<(u16, u16, u16)>,
    pub label: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_link_at(
    screen: &vt100::Screen,
    osc_links: &VecDeque<OscLink>,
    abs_row_of_visible: impl Fn(u16) -> usize,
    vis_row: u16,
    col: u16,
    cwd: &Path,
    workspace_root: Option<&Path>,
) -> Option<TerminalLinkHit> {
    let (rows_nz, cols_nz) = screen.size();
    let rows = rows_nz.get();
    let cols = cols_nz.get();
    if vis_row >= rows || col >= cols {
        return None;
    }

    // 1. Explicit OSC 8 hyperlinks take precedence.
    let alt = screen.alternate_screen();
    let abs_row = abs_row_of_visible(vis_row);
    if let Some(link) = osc_links.iter().rev().find(|l| {
        l.alt_screen == alt
            && (abs_row, col) >= (l.abs_row_start, l.col_start)
            && (abs_row, col) <= (l.abs_row_end, l.col_end)
    }) {
        let mut spans = Vec::new();
        for r in 0..rows {
            let r_abs = abs_row_of_visible(r);
            if r_abs < link.abs_row_start || r_abs > link.abs_row_end {
                continue;
            }
            let c0 = if r_abs == link.abs_row_start {
                link.col_start
            } else {
                0
            };
            let c1 = if r_abs == link.abs_row_end {
                link.col_end.min(cols - 1)
            } else {
                cols - 1
            };
            if c0 <= c1 {
                spans.push((r, c0, c1));
            }
        }
        if let Some(file_hit) = parse_file_uri(&link.url) {
            return Some(TerminalLinkHit {
                label: format!("Ctrl+点击在编辑器打开 {}", file_hit.display()),
                target: TerminalLinkTarget::File {
                    path: file_hit,
                    line: None,
                    col: None,
                },
                spans,
            });
        }
        return Some(TerminalLinkHit {
            label: format!("Ctrl+点击在浏览器打开 {}", link.url),
            target: TerminalLinkTarget::Url(link.url.clone()),
            spans,
        });
    }

    // 2. Join soft-wrapped visible rows into one logical line with cell mapping.
    let mut top = vis_row;
    while top > 0 && screen.row_wrapped(top - 1) {
        top -= 1;
    }
    let mut bot = vis_row;
    while bot + 1 < rows && screen.row_wrapped(bot) {
        bot += 1;
    }

    let mut logical = String::new();
    let mut pieces: Vec<(usize, usize, u16, u16, u16)> = Vec::new();
    let mut target_byte: Option<usize> = None;

    for r in top..=bot {
        let mut c = 0u16;
        let mut last_non_empty_len = logical.len();
        let mut last_non_empty_pieces = pieces.len();
        while c < cols {
            let Some(cell) = screen.cell(r, c) else {
                c += 1;
                continue;
            };
            if cell.is_wide_continuation() {
                c += 1;
                continue;
            }
            let wide = cell.is_wide();
            let c_end = if wide { (c + 1).min(cols - 1) } else { c };
            let s = cell.contents();
            let byte_start = logical.len();
            if s.is_empty() {
                logical.push(' ');
            } else {
                logical.push_str(s);
            }
            let byte_end = logical.len();
            if r == vis_row && col >= c && col <= c_end {
                target_byte = Some(byte_start);
            }
            pieces.push((byte_start, byte_end, r, c, c_end));
            if !s.is_empty() && s != " " {
                last_non_empty_len = logical.len();
                last_non_empty_pieces = pieces.len();
            }
            c = c_end + 1;
        }
        if r == bot {
            logical.truncate(last_non_empty_len);
            pieces.truncate(last_non_empty_pieces);
        }
    }

    let target_byte = target_byte?;
    if target_byte >= logical.len() {
        return None;
    }

    // 2a. HTTP / HTTPS / file:// URLs.
    if let Some((b_start, b_end, url)) = find_url_at_byte(&logical, target_byte) {
        let spans = spans_for_byte_range(&pieces, b_start, b_end);
        if let Some(path) = parse_file_uri(&url) {
            return Some(TerminalLinkHit {
                label: format!("Ctrl+点击在编辑器打开 {}", path.display()),
                target: TerminalLinkTarget::File {
                    path,
                    line: None,
                    col: None,
                },
                spans,
            });
        }
        return Some(TerminalLinkHit {
            label: format!("Ctrl+点击在浏览器打开 {url}"),
            target: TerminalLinkTarget::Url(url),
            spans,
        });
    }

    // 2b. Compiler / CLI file path references (`path:line:col`, `path(line,col)`, etc.).
    if let Some((b_start, b_end, path, line, col_num)) =
        find_file_link_at_byte(&logical, target_byte, cwd, workspace_root)
    {
        let spans = spans_for_byte_range(&pieces, b_start, b_end);
        let loc = match (line, col_num) {
            (Some(l), Some(c)) => format!("{}:{}:{}", path.display(), l, c),
            (Some(l), None) => format!("{}:{}", path.display(), l),
            _ => path.display().to_string(),
        };
        return Some(TerminalLinkHit {
            label: format!("Ctrl+点击跳转到 {loc}"),
            target: TerminalLinkTarget::File {
                path,
                line,
                col: col_num,
            },
            spans,
        });
    }

    None
}

fn spans_for_byte_range(
    pieces: &[(usize, usize, u16, u16, u16)],
    b_start: usize,
    b_end: usize,
) -> Vec<(u16, u16, u16)> {
    let mut spans: Vec<(u16, u16, u16)> = Vec::new();
    for &(ps, pe, row, c0, c1) in pieces {
        if pe <= b_start || ps >= b_end {
            continue;
        }
        if let Some(last) = spans.last_mut()
            && last.0 == row
        {
            last.2 = last.2.max(c1);
        } else {
            spans.push((row, c0, c1));
        }
    }
    spans
}

fn find_url_at_byte(line: &str, target: usize) -> Option<(usize, usize, String)> {
    let mut index = 0usize;
    while index < line.len() {
        let rest = &line[index..];
        if rest.starts_with("https://")
            || rest.starts_with("http://")
            || rest.starts_with("file://")
        {
            let end = rest
                .char_indices()
                .find(|(_, ch)| url_boundary(*ch))
                .map_or(rest.len(), |(offset, _)| offset);
            let raw = &rest[..end];
            let trimmed = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            let b_start = index;
            let b_end = index + trimmed.len();
            if target >= b_start && target < b_end {
                return Some((b_start, b_end, trimmed.to_string()));
            }
            index += end;
            continue;
        }
        let ch = rest.chars().next()?;
        index += ch.len_utf8();
    }
    None
}

fn is_path_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '<' | '>' | '|' | '`' | '[' | ']' | '{' | '}' | ';' | ',' | '!' | '?'
        )
        || ('\u{2500}'..='\u{257F}').contains(&ch)
}

pub(crate) fn find_file_link_at_byte(
    line: &str,
    target: usize,
    cwd: &Path,
    workspace_root: Option<&Path>,
) -> Option<(usize, usize, PathBuf, Option<usize>, Option<usize>)> {
    let mut start = target;
    let mut crossed_paren = false;
    while start > 0 {
        let prev = line[..start].chars().next_back()?;
        if prev == '(' {
            let before = &line[..start - 1];
            let has_path_before = before
                .chars()
                .next_back()
                .is_some_and(|ch| !is_path_delimiter(ch) && ch != '(');
            if has_path_before && !crossed_paren {
                crossed_paren = true;
                start -= 1;
                continue;
            }
            break;
        }
        if prev == ',' {
            let before = &line[..start - 1];
            if before.chars().next_back().is_some_and(|c| c.is_ascii_digit())
                && line[start..].starts_with(|c: char| c.is_ascii_digit())
            {
                start -= 1;
                continue;
            }
            break;
        }
        if is_path_delimiter(prev) {
            break;
        }
        start -= prev.len_utf8();
    }
    let mut end = target;
    let mut in_paren = line[start..target].contains('(') && !line[start..target].contains(')');
    while end < line.len() {
        let ch = line[end..].chars().next()?;
        if ch == '(' && end > start {
            in_paren = true;
            end += 1;
            continue;
        }
        if ch == ')' && in_paren {
            end += 1;
            break;
        }
        if in_paren && (ch.is_ascii_digit() || ch == ',') {
            end += 1;
            continue;
        }
        if is_path_delimiter(ch) {
            break;
        }
        end += ch.len_utf8();
    }

    let mut token = &line[start..end];
    while let Some(last) = token.chars().next_back() {
        if matches!(last, '.' | ':' | ')' | ',') {
            if last == ')' && token.contains('(') {
                break;
            }
            token = &token[..token.len() - last.len_utf8()];
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    if token.len() < 2 || token.starts_with("http://") || token.starts_with("https://") {
        return None;
    }

    let (raw_path, line_num, col_num) = split_path_line_col(token);
    if raw_path.is_empty() {
        return None;
    }

    let extra_line = if line_num.is_none() {
        let after = line[end..].trim_start_matches(['"', '\'', ' ']);
        after.strip_prefix(", line ").and_then(|rest| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<usize>().ok()
        })
    } else {
        None
    };

    let resolved = resolve_existing_file(raw_path, cwd, workspace_root)?;
    Some((start, end, resolved, line_num.or(extra_line), col_num))
}

fn split_path_line_col(token: &str) -> (&str, Option<usize>, Option<usize>) {
    if let Some(open) = token.rfind('(')
        && token.ends_with(')')
        && open > 0
    {
        let inside = &token[open + 1..token.len() - 1];
        if let Some((l, c)) = inside.split_once(',') {
            if let (Ok(ln), Ok(cn)) = (l.trim().parse::<usize>(), c.trim().parse::<usize>()) {
                return (&token[..open], Some(ln), Some(cn));
            }
        } else if let Ok(ln) = inside.trim().parse::<usize>() {
            return (&token[..open], Some(ln), None);
        }
    }

    let min_colon = if token.as_bytes().get(1) == Some(&b':') {
        2
    } else {
        0
    };
    if let Some(last_colon) = token[min_colon..].rfind(':').map(|i| i + min_colon)
        && let Ok(num1) = token[last_colon + 1..].parse::<usize>()
    {
        let head = &token[..last_colon];
        if let Some(prev_colon) = head[min_colon..].rfind(':').map(|i| i + min_colon)
            && let Ok(num0) = head[prev_colon + 1..].parse::<usize>()
        {
            return (&head[..prev_colon], Some(num0), Some(num1));
        }
        return (head, Some(num1), None);
    }

    (token, None, None)
}

fn resolve_existing_file(
    raw: &str,
    cwd: &Path,
    workspace_root: Option<&Path>,
) -> Option<PathBuf> {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let trimmed = raw
        .strip_prefix("./")
        .or_else(|| raw.strip_prefix(".\\"))
        .unwrap_or(raw);
    let mut bases: Vec<&Path> = vec![cwd];
    if let Some(root) = workspace_root
        && root != cwd
    {
        bases.push(root);
    }
    for base in &bases {
        let joined = base.join(trimmed);
        if joined.is_file() {
            return Some(joined);
        }
    }
    if let Some(git_rel) = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
    {
        for base in &bases {
            let joined = base.join(git_rel);
            if joined.is_file() {
                return Some(joined);
            }
        }
    }
    None
}

/// Display width of one character in terminal cells.
pub fn char_width(ch: char) -> u16 {
    let wide = matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x231A..=0x231B
            | 0x2329..=0x232A
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    );
    if wide { 2 } else { 1 }
}

fn url_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\''
                | '<'
                | '>'
                | '|'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
                | '、'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '…'
                | '—'
        )
        || ('\u{2500}'..='\u{257F}').contains(&ch)
}

/// URL under a Ctrl+click on a single row string (kept for pure-function unit tests).
#[allow(dead_code)]
pub fn url_at_cell(line: &str, cell: u16) -> Option<String> {
    let mut col = 0u16;
    let mut index = 0usize;
    while index < line.len() {
        let rest = &line[index..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest
                .char_indices()
                .find(|(_, ch)| url_boundary(*ch))
                .map_or(rest.len(), |(offset, _)| offset);
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            let width: u16 = url.chars().map(char_width).sum();
            if cell >= col && cell < col.saturating_add(width) {
                return Some(url.to_string());
            }
            col = col.saturating_add(rest[..end].chars().map(char_width).sum::<u16>());
            index += end;
            continue;
        }
        let ch = rest.chars().next()?;
        col = col.saturating_add(char_width(ch));
        index += ch.len_utf8();
    }
    None
}
