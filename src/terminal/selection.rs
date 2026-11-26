//! Terminal text selection: absolute-row anchored stream and rectangular block modes,
//! wide-character snapping, double-click word / triple-click logical-line selection.

/// Selection geometry mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SelectionMode {
    /// Normal line-wrapping flow selection.
    #[default]
    Stream,
    /// Rectangular column-block selection (`Alt+drag`).
    Block,
}

/// Inclusive cell range anchored by absolute history row indices so selections
/// remain pinned to the same text as new output scrolls in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TerminalSelection {
    pub start: (usize, u16),
    pub end: (usize, u16),
    pub alt_screen: bool,
    pub mode: SelectionMode,
}

impl TerminalSelection {
    pub fn from_endpoints(
        anchor: (usize, u16),
        cursor: (usize, u16),
        alt_screen: bool,
        mode: SelectionMode,
    ) -> Self {
        match mode {
            SelectionMode::Stream => {
                if anchor <= cursor {
                    Self {
                        start: anchor,
                        end: cursor,
                        alt_screen,
                        mode,
                    }
                } else {
                    Self {
                        start: cursor,
                        end: anchor,
                        alt_screen,
                        mode,
                    }
                }
            }
            SelectionMode::Block => Self {
                start: (anchor.0.min(cursor.0), anchor.1.min(cursor.1)),
                end: (anchor.0.max(cursor.0), anchor.1.max(cursor.1)),
                alt_screen,
                mode,
            },
        }
    }

    /// Columns covered on one absolute row, clipped to the screen width.
    pub fn row_span(&self, abs_row: usize, cols: u16) -> Option<(u16, u16)> {
        if abs_row < self.start.0 || abs_row > self.end.0 || cols == 0 {
            return None;
        }
        let max_col = cols - 1;
        let (start, end) = match self.mode {
            SelectionMode::Block => (self.start.1.min(max_col), self.end.1.min(max_col)),
            SelectionMode::Stream => {
                let s = if abs_row == self.start.0 {
                    self.start.1
                } else {
                    0
                };
                let e = if abs_row == self.end.0 {
                    self.end.1
                } else {
                    max_col
                };
                (s.min(max_col), e.min(max_col))
            }
        };
        (start <= end).then_some((start, end))
    }
}

pub(crate) fn snap_start_col(screen: &vt100::Screen, vis_row: u16, col: u16) -> u16 {
    if col > 0
        && screen
            .cell(vis_row, col)
            .is_some_and(vt100::Cell::is_wide_continuation)
    {
        col - 1
    } else {
        col
    }
}

pub(crate) fn snap_end_col(screen: &vt100::Screen, vis_row: u16, col: u16) -> u16 {
    let cols = screen.size().1.get();
    if screen.cell(vis_row, col).is_some_and(vt100::Cell::is_wide) {
        (col + 1).min(cols.saturating_sub(1))
    } else {
        col
    }
}

pub(crate) fn word_bounds_on_row(screen: &vt100::Screen, vis_row: u16, col: u16) -> Option<(u16, u16)> {
    let cols = screen.size().1.get();
    if cols == 0 {
        return None;
    }
    let col = snap_start_col(screen, vis_row, col.min(cols - 1));
    let class_at = |c: u16| -> u8 {
        let Some(cell) = screen.cell(vis_row, c) else {
            return 0;
        };
        let Some(ch) = cell.contents().chars().next() else {
            return 0;
        };
        if ch.is_whitespace() {
            0
        } else if ch.is_alphanumeric()
            || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '@' | '~' | '+' | '=')
        {
            1
        } else {
            2
        }
    };
    let target_class = class_at(col);
    let mut start = col;
    while start > 0 {
        let prev = if start >= 2
            && screen
                .cell(vis_row, start - 1)
                .is_some_and(vt100::Cell::is_wide_continuation)
        {
            start - 2
        } else {
            start - 1
        };
        if class_at(prev) != target_class {
            break;
        }
        start = prev;
    }
    let mut end = snap_end_col(screen, vis_row, col);
    while end + 1 < cols {
        let next = end + 1;
        if class_at(next) != target_class {
            break;
        }
        end = snap_end_col(screen, vis_row, next);
    }
    Some((start, end))
}

pub(crate) fn logical_line_rows(screen: &vt100::Screen, vis_row: u16) -> Option<(u16, u16)> {
    let rows = screen.size().0.get();
    if rows == 0 {
        return None;
    }
    let mut top = vis_row.min(rows - 1);
    while top > 0 && screen.row_wrapped(top - 1) {
        top -= 1;
    }
    let mut bot = vis_row.min(rows - 1);
    while bot + 1 < rows && screen.row_wrapped(bot) {
        bot += 1;
    }
    Some((top, bot))
}

pub(crate) fn extract_selection_text(
    screen: &mut vt100::Screen,
    evicted_rows: usize,
    selection: TerminalSelection,
) -> Option<String> {
    let alt = screen.alternate_screen();
    if selection.alt_screen != alt {
        return None;
    }
    let (rows_nz, cols_nz) = screen.size();
    let screen_rows = rows_nz.get() as usize;
    let cols = cols_nz.get();
    if screen_rows == 0 || cols == 0 {
        return None;
    }

    if alt {
        let start_row = (selection.start.0 as u16).min(rows_nz.get() - 1);
        let end_row = (selection.end.0 as u16).min(rows_nz.get() - 1);
        return extract_visible_range(screen, selection, start_row, end_row, cols);
    }

    let scrollback_len = screen.scrollback_len();
    let total = scrollback_len + screen_rows;
    if total == 0 {
        return None;
    }
    let start_hist = selection.start.0.saturating_sub(evicted_rows).min(total - 1);
    let end_hist = selection.end.0.saturating_sub(evicted_rows).min(total - 1);
    if start_hist > end_hist {
        return None;
    }

    let original_offset = screen.scrollback();
    let mut out = String::new();
    let mut cursor_row = start_hist;

    while cursor_row <= end_hist {
        let window_start = if cursor_row < scrollback_len {
            screen.set_scrollback(scrollback_len - cursor_row);
            cursor_row
        } else {
            screen.set_scrollback(0);
            scrollback_len
        };
        let window_end = (window_start + screen_rows - 1).min(end_hist);
        let local_start = (cursor_row - window_start) as u16;
        let local_end = (window_end - window_start) as u16;

        match selection.mode {
            SelectionMode::Block => {
                for r in local_start..=local_end {
                    let c0 = snap_start_col(screen, r, selection.start.1.min(cols - 1));
                    let c1 = snap_end_col(screen, r, selection.end.1.min(cols - 1));
                    let line = screen.contents_between(r, c0, r, (c1 + 1).min(cols));
                    out.push_str(line.trim_end());
                    let hist_r = window_start + usize::from(r);
                    if hist_r < end_hist {
                        out.push('\n');
                    }
                }
            }
            SelectionMode::Stream => {
                let start_col = if cursor_row == start_hist {
                    snap_start_col(screen, local_start, selection.start.1.min(cols - 1))
                } else {
                    0
                };
                let end_col = if window_end == end_hist {
                    snap_end_col(screen, local_end, selection.end.1.min(cols - 1))
                } else {
                    cols - 1
                };
                let chunk = screen.contents_between(
                    local_start,
                    start_col,
                    local_end,
                    (end_col + 1).min(cols),
                );
                out.push_str(&chunk);
                if window_end < end_hist && !screen.row_wrapped(local_end) {
                    out.push('\n');
                }
            }
        }
        cursor_row = window_end + 1;
    }

    screen.set_scrollback(original_offset);
    clean_copied_text(out)
}

fn extract_visible_range(
    screen: &vt100::Screen,
    selection: TerminalSelection,
    start_row: u16,
    end_row: u16,
    cols: u16,
) -> Option<String> {
    match selection.mode {
        SelectionMode::Block => {
            let mut lines = Vec::new();
            for r in start_row..=end_row {
                let c0 = snap_start_col(screen, r, selection.start.1.min(cols - 1));
                let c1 = snap_end_col(screen, r, selection.end.1.min(cols - 1));
                let text = screen.contents_between(r, c0, r, (c1 + 1).min(cols));
                lines.push(text.trim_end().to_string());
            }
            clean_copied_text(lines.join("\n"))
        }
        SelectionMode::Stream => {
            let start_col = snap_start_col(screen, start_row, selection.start.1.min(cols - 1));
            let end_col = snap_end_col(screen, end_row, selection.end.1.min(cols - 1));
            let text =
                screen.contents_between(start_row, start_col, end_row, (end_col + 1).min(cols));
            clean_copied_text(text)
        }
    }
}

fn clean_copied_text(text: String) -> Option<String> {
    let text = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim_end().to_string();
    (!text.is_empty()).then_some(text)
}
