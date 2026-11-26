//! Terminal scrollback + viewport search with case-sensitive and regex support.

use std::time::{Duration, Instant};

use regex::RegexBuilder;

use super::links::char_width;

/// Search results are capped so a pathological match count cannot stall painting.
pub const MAX_SEARCH_MATCHES: usize = 1000;

/// One search hit; rows are history rows (scrollback first, then screen).
#[derive(Clone, Copy, Debug)]
pub struct SearchMatch {
    pub row: usize,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Default)]
pub struct TerminalSearch {
    pub open: bool,
    pub query: String,
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub regex_error: Option<String>,
    pub matches: Vec<SearchMatch>,
    pub active: Option<usize>,
    pub truncated: bool,
    pub dirty: bool,
    pub focus: bool,
    /// Last expensive history scan; output bursts must not rescan every frame.
    pub last_refresh: Option<Instant>,
}

impl TerminalSearch {
    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
        self.dirty = true;
        self.last_refresh = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focus = false;
        self.matches.clear();
        self.active = None;
        self.truncated = false;
        self.regex_error = None;
        self.last_refresh = None;
    }

    /// Recompute matches over scrollback + screen when dirty and throttle window elapsed.
    pub fn refresh(&mut self, screen: &mut vt100::Screen) {
        if !self.open || !self.dirty {
            return;
        }
        if self
            .last_refresh
            .is_some_and(|at| at.elapsed() < Duration::from_millis(150))
        {
            return;
        }
        self.recompute(screen);
    }

    fn recompute(&mut self, screen: &mut vt100::Screen) {
        self.dirty = false;
        self.last_refresh = Some(Instant::now());
        self.matches.clear();
        self.truncated = false;
        self.regex_error = None;
        if self.query.is_empty() {
            self.active = None;
            return;
        }

        let compiled_regex = if self.use_regex {
            match RegexBuilder::new(&self.query)
                .case_insensitive(!self.case_sensitive)
                .build()
            {
                Ok(re) => Some(re),
                Err(err) => {
                    self.regex_error = Some(err.to_string());
                    self.active = None;
                    return;
                }
            }
        } else {
            None
        };

        let needle = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };

        let (rows, cols) = screen.size();
        let rows = rows.get() as usize;
        let cols = cols.get();
        let history = history_rows_with_cols(screen, rows, cols);

        'history: for (row, (line, byte_to_col)) in history.iter().enumerate() {
            if let Some(re) = &compiled_regex {
                for m in re.find_iter(line) {
                    if m.start() == m.end() {
                        continue;
                    }
                    let start_col = map_byte_to_col(byte_to_col, m.start(), line, cols);
                    let end_col = map_byte_to_col(byte_to_col, m.end(), line, cols)
                        .saturating_sub(1)
                        .max(start_col)
                        .min(cols.saturating_sub(1));
                    self.matches.push(SearchMatch {
                        row,
                        start_col,
                        end_col,
                    });
                    if self.matches.len() >= MAX_SEARCH_MATCHES {
                        self.truncated = true;
                        break 'history;
                    }
                }
            } else {
                let owned_lower;
                let hay = if self.case_sensitive {
                    line.as_str()
                } else {
                    owned_lower = line.to_lowercase();
                    owned_lower.as_str()
                };
                let same_len = hay.len() == line.len();
                let mut from = 0;
                while let Some(found) = hay[from..].find(&needle) {
                    let start_byte = from + found;
                    let end_byte = start_byte + needle.len();
                    let (start_col, end_col) = if same_len {
                        let s = map_byte_to_col(byte_to_col, start_byte, line, cols);
                        let e = map_byte_to_col(byte_to_col, end_byte, line, cols)
                            .saturating_sub(1)
                            .max(s)
                            .min(cols.saturating_sub(1));
                        (s, e)
                    } else {
                        let s = column_of_byte(hay, start_byte);
                        let e = column_of_byte(hay, end_byte)
                            .saturating_sub(1)
                            .max(s)
                            .min(cols.saturating_sub(1));
                        (s, e)
                    };
                    self.matches.push(SearchMatch {
                        row,
                        start_col,
                        end_col,
                    });
                    if self.matches.len() >= MAX_SEARCH_MATCHES {
                        self.truncated = true;
                        break 'history;
                    }
                    from = end_byte;
                }
            }
        }

        if self.matches.is_empty() {
            self.active = None;
        } else if let Some(active) = self.active {
            self.active = Some(active.min(self.matches.len() - 1));
        } else {
            self.active = Some(self.matches.len() - 1);
        }
    }

    /// Advance active match and return its history row to scroll into view.
    pub fn step(&mut self, screen: &mut vt100::Screen, forward: bool) -> Option<usize> {
        self.last_refresh = None;
        self.refresh(screen);
        let count = self.matches.len();
        if count == 0 {
            self.active = None;
            return None;
        }
        let next = match self.active {
            Some(index) => {
                if forward {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                }
            }
            None if forward => 0,
            None => count - 1,
        };
        self.active = Some(next);
        Some(self.matches[next].row)
    }
}

fn history_rows_with_cols(
    screen: &mut vt100::Screen,
    rows: usize,
    cols: u16,
) -> Vec<(String, Vec<(usize, u16)>)> {
    let scrollback_len = screen.scrollback_len();
    let total = scrollback_len + rows;
    let original = screen.scrollback();
    let mut history = Vec::with_capacity(total);
    let mut start = 0;
    while start < total {
        let skip = if start < scrollback_len {
            screen.set_scrollback(scrollback_len - start);
            0
        } else {
            screen.set_scrollback(0);
            start - scrollback_len
        };
        for vis_row in skip..rows {
            history.push(extract_row_with_cols(screen, vis_row as u16, cols));
        }
        if start < scrollback_len {
            start += rows;
        } else {
            start = total;
        }
    }
    screen.set_scrollback(original);
    history
}

fn extract_row_with_cols(
    screen: &vt100::Screen,
    vis_row: u16,
    cols: u16,
) -> (String, Vec<(usize, u16)>) {
    let mut text = String::new();
    let mut map = Vec::new();
    let mut col = 0u16;
    let mut last_non_blank_len = 0usize;
    let mut last_non_blank_map = 0usize;
    while col < cols {
        let Some(cell) = screen.cell(vis_row, col) else {
            col += 1;
            continue;
        };
        if cell.is_wide_continuation() {
            col += 1;
            continue;
        }
        let s = cell.contents();
        let byte_pos = text.len();
        map.push((byte_pos, col));
        if s.is_empty() {
            text.push(' ');
        } else {
            text.push_str(s);
            last_non_blank_len = text.len();
            last_non_blank_map = map.len();
        }
        col += if cell.is_wide() { 2 } else { 1 };
    }
    text.truncate(last_non_blank_len);
    map.truncate(last_non_blank_map);
    (text, map)
}

fn map_byte_to_col(map: &[(usize, u16)], byte: usize, line: &str, cols: u16) -> u16 {
    if map.is_empty() {
        return 0;
    }
    if byte >= line.len() {
        if let Some(&(last_b, last_c)) = map.last() {
            let tail = &line[last_b..];
            let w: u16 = tail.chars().next().map_or(1, char_width);
            return last_c.saturating_add(w).min(cols);
        }
        return 0;
    }
    match map.binary_search_by_key(&byte, |&(b, _)| b) {
        Ok(idx) => map[idx].1,
        Err(0) => 0,
        Err(idx) => map[idx - 1].1,
    }
}

fn column_of_byte(line: &str, byte: usize) -> u16 {
    let mut col = 0u16;
    for (offset, ch) in line.char_indices() {
        if offset >= byte {
            break;
        }
        col = col.saturating_add(char_width(ch));
    }
    col
}
