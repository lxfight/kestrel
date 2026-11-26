//! Painting, pointer handling, minimap, sticky scroll, completion popup, and find widget for the editor canvas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use super::{Editor, EditorEvent, EditorStyle, MinimapRow};
use crate::git::LineDiffKind;
use crate::highlight::{Span, TokenKind};
use crate::icons::{self, Icon};
use crate::lsp::DiagSeverity;

const PAD_TOP: f32 = 6.0;
const SCROLLBAR_W: f32 = 11.0;
const MINIMAP_W: f32 = 88.0;

impl Editor {
    /// Fractional document line at the top of the viewport.
    pub fn top_visible_line(&self, line_h: f32) -> f32 {
        let row_frac = (self.scroll_y - PAD_TOP).max(0.0) / line_h.max(1.0);
        let row_count = self.line_map.row_count();
        if row_count == 0 || self.line_map.rows.is_none() {
            return row_frac;
        }
        self.line_map
            .line_of_row((row_frac.floor() as usize).min(row_count - 1)) as f32
    }

    /// Scroll so that the given document line sits at the top of the viewport.
    pub fn scroll_to_line(&mut self, line: f32, line_h: f32) {
        self.sync_layout();
        let line = line.max(0.0) as usize;
        let row = match self.line_map.row_of(line) {
            Some(row) => row,
            None => self
                .fold_index
                .enclosing_start(line)
                .and_then(|start| self.line_map.row_of(start))
                .unwrap_or(0),
        };
        self.scroll_y = (row as f32 * line_h.max(1.0) + PAD_TOP).max(0.0);
    }

    /// Ensure `wrap_heights` and `wrap_y` match the current document / fold / width
    /// when `style.word_wrap` is enabled; clears them when word wrap is off.
    fn sync_wrap_metrics(&mut self, style: &EditorStyle, text_width: f32, line_h: f32) {
        if !style.word_wrap {
            if !self.wrap_heights.is_empty() {
                self.wrap_heights.clear();
                self.wrap_y.clear();
                self.wrap_key = (0, 0, 0, 0);
            }
            return;
        }
        let row_count = self.line_map.row_count().max(1);
        let key = (
            self.doc.version,
            self.fold_rev,
            text_width.to_bits(),
            line_h.to_bits(),
        );
        if self.wrap_key == key && self.wrap_heights.len() == row_count && self.wrap_y.len() == row_count + 1 {
            return;
        }
        let wrap_w = (text_width - 12.0).max(40.0);
        let chars_per_row = (wrap_w / self.char_w.max(1.0)).floor().max(1.0) as usize;
        self.wrap_heights.clear();
        self.wrap_heights.reserve(row_count);
        self.wrap_y.clear();
        self.wrap_y.reserve(row_count + 1);
        let mut acc = 0.0f32;
        self.wrap_y.push(0.0);
        for row in 0..row_count {
            let line = self.line_map.line_of_row(row);
            let h = if let Some(cached) = self.galleys.get(&line) {
                (cached.galley.rows.len().max(1) as f32 * line_h).max(cached.galley.rect.height())
            } else {
                let len = self.doc.line_char_len(line);
                let est_rows = len.div_ceil(chars_per_row).max(1);
                est_rows as f32 * line_h
            };
            self.wrap_heights.push(h);
            acc += h;
            self.wrap_y.push(acc);
        }
        self.wrap_key = key;
    }

    /// Update a single visible row's exact wrapped height from its laid-out galley.
    fn sync_row_wrapped_height(&mut self, row: usize, line: usize, line_h: f32) {
        if self.wrap_heights.is_empty() || row >= self.wrap_heights.len() {
            return;
        }
        let Some(cached) = self.galleys.get(&line) else {
            return;
        };
        let exact = (cached.galley.rows.len().max(1) as f32 * line_h).max(cached.galley.rect.height());
        if (exact - self.wrap_heights[row]).abs() > 0.5 {
            self.wrap_heights[row] = exact;
            let mut acc = self.wrap_y[row];
            for r in row..self.wrap_heights.len() {
                acc += self.wrap_heights[r];
                self.wrap_y[r + 1] = acc;
            }
        }
    }

    fn row_top_offset(&self, row: usize, line_h: f32) -> f32 {
        if !self.wrap_y.is_empty() {
            self.wrap_y[row.min(self.wrap_y.len().saturating_sub(1))]
        } else {
            row as f32 * line_h
        }
    }

    fn row_visual_height(&self, row: usize, line_h: f32) -> f32 {
        if !self.wrap_heights.is_empty() {
            self.wrap_heights.get(row).copied().unwrap_or(line_h)
        } else {
            line_h
        }
    }

    fn total_view_height(&mut self, line_h: f32) -> f32 {
        if let Some(&h) = self.wrap_y.last() {
            h.max(line_h)
        } else {
            self.total_content_height(line_h)
        }
    }

    /// First row whose bottom edge is at or below the top of the viewport.
    fn first_visible_row(&self, line_h: f32) -> usize {
        let y = (self.scroll_y - PAD_TOP).max(0.0);
        if !self.wrap_y.is_empty() {
            let row_count = self.wrap_heights.len().max(1);
            let idx = self.wrap_y.partition_point(|&top| top <= y);
            idx.saturating_sub(1).min(row_count - 1)
        } else {
            (y / line_h.max(1.0)).floor() as usize
        }
    }

    /// Row under a screen y coordinate, clamped to the last row.
    fn row_at_y(&self, y: f32, view_top: f32, line_h: f32, row_count: usize) -> usize {
        let doc_y = y - view_top + self.scroll_y - PAD_TOP;
        if !self.wrap_y.is_empty() {
            if doc_y <= 0.0 {
                return 0;
            }
            let idx = self.wrap_y.partition_point(|&top| top <= doc_y);
            idx.saturating_sub(1).min(row_count.saturating_sub(1))
        } else {
            ((doc_y / line_h).floor() as isize).clamp(0, row_count as isize - 1) as usize
        }
    }

    /// Start line of the sticky header floating above the viewport, if any.
    fn sticky_header_line(&self, style: &EditorStyle, line_h: f32) -> Option<usize> {
        if !style.show_sticky_scroll {
            return None;
        }
        let row_count = self.line_map.row_count();
        let first_row = self.first_visible_row(line_h);
        if first_row == 0 || row_count == 0 {
            return None;
        }
        let first_line = self.line_map.line_of_row(first_row.min(row_count - 1));
        self.symbols
            .iter()
            .rfind(|symbol| symbol.start_line < first_line && symbol.end_line > first_line)
            .map(|symbol| symbol.start_line)
    }

    /// Height the sticky header steals from the text viewport.
    fn sticky_height(&self, style: &EditorStyle, line_h: f32) -> f32 {
        if self.sticky_header_line(style, line_h).is_some() {
            line_h
        } else {
            0.0
        }
    }

    fn max_scroll(total_h: f32, view_h: f32, style: &EditorStyle, line_h: f32) -> f32 {
        (total_h + PAD_TOP * 2.0 - view_h).max(0.0)
            + if style.scroll_past_end {
                line_h * 2.0
            } else {
                0.0
            }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &EditorStyle) -> Option<EditorEvent> {
        if let Some((tab_size, insert_spaces)) = self.detected_indent {
            self.tab_size = tab_size.max(1);
            self.insert_spaces = insert_spaces;
        } else {
            self.tab_size = style.tab_size.max(1);
            self.insert_spaces = style.insert_spaces;
        }
        self.autocomplete_enabled = style.autocomplete;
        let mut event = None;

        let available = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(available.x.max(1.0), available.y.max(1.0)),
            Sense::click_and_drag(),
        );
        let focused = response.has_focus();
        self.has_focus = focused;
        if !focused && !self.ime_preedit.is_empty() {
            // Losing focus must end the composition instead of leaving a preedit
            // string behind that would later jump to a new caret.
            self.ime_preedit.clear();
            self.ime_active_range = None;
            ui.ctx().output_mut(|output| {
                if let Some(ime) = &mut output.ime {
                    ime.should_interrupt_composition = true;
                }
            });
        }
        if self.focus_requested {
            response.request_focus();
            self.focus_requested = false;
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }

        let line_h = style.line_h.max(1.0);
        let line_count = self.doc.line_count().max(1);
        let digit_w = self.char_width(ui, style);
        let digits = line_count.to_string().len().max(3) as f32;
        let gutter_w = if style.show_line_numbers {
            digit_w * digits + 34.0
        } else {
            16.0
        };
        let minimap_w = if style.show_minimap { MINIMAP_W } else { 0.0 };
        let text_left = rect.left() + gutter_w;
        let text_right = rect.right() - minimap_w - SCROLLBAR_W;
        let text_width = (text_right - text_left).max(80.0);

        self.refresh_syntax(Instant::now());
        self.sync_layout();
        self.sync_cache(ui, style, text_width);
        self.sync_wrap_metrics(style, text_width, line_h);
        self.snap_caret_out_of_folds();

        if style.word_wrap {
            self.scroll_x = 0.0;
        }

        // ---- scrolling ----
        // The sticky header takes a line off the top of the text viewport; it only
        // appears once the content above the first row has an enclosing scope.
        self.sticky_h = self.sticky_height(style, line_h);
        let mut view_h = (rect.height() - self.sticky_h).max(line_h);
        let mut max_scroll =
            Self::max_scroll(self.total_view_height(line_h), view_h, style, line_h);
        let rows = ((view_h / line_h).ceil() as usize + 1).max(2);

        let (scroll_delta, modifiers) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers));
        if response.hovered() {
            let zoom_step = crate::ui::ctrl_wheel_steps(ui.ctx());
            if zoom_step != 0 {
                event = Some(EditorEvent::Zoom(zoom_step));
            } else if scroll_delta != Vec2::ZERO && !(modifiers.ctrl || modifiers.command) {
                if modifiers.shift && !style.word_wrap {
                    self.scroll_x = (self.scroll_x - scroll_delta.y - scroll_delta.x).max(0.0);
                } else {
                    self.scroll_y -= scroll_delta.y;
                    if !style.word_wrap {
                        self.scroll_x = (self.scroll_x - scroll_delta.x).max(0.0);
                    }
                }
            }
        }
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);

        self.sticky_h = self.sticky_height(style, line_h);
        view_h = (rect.height() - self.sticky_h).max(line_h);
        max_scroll = Self::max_scroll(self.total_view_height(line_h), view_h, style, line_h);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let view_top = rect.top() + self.sticky_h;
        let row_count = self.line_map.row_count().max(1);

        // ---- pointer: fold gutter click, focus, click, selection drag, double/triple click ----
        let pointer_pressed = ui.input(|i| i.pointer.primary_pressed());
        let shift_held = ui.input(|i| i.modifiers.shift);
        let alt_held = ui.input(|i| i.modifiers.alt);
        let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

        if pointer_pressed
            && response.contains_pointer()
            && let Some(pos) = response.interact_pointer_pos()
        {
            // A canvas click always dismisses completion and aborts any composition,
            // so the preedit string never jumps to the new caret.
            self.completion.open = false;
            if !self.ime_preedit.is_empty() {
                self.ime_preedit.clear();
                self.ime_active_range = None;
                self.ime_interrupt = true;
            }
            self.selection_history.clear();
            let in_text = pos.y >= view_top;
            let row = self.row_at_y(pos.y, view_top, line_h, row_count);
            let clicked_line = self.line_of_row(row);
            // Check if user clicked the fold chevron column in the gutter.
            if in_text
                && pos.x >= text_left - 18.0
                && pos.x <= text_left
                && self.fold_range_for_line(clicked_line).is_some()
            {
                self.toggle_fold_at(clicked_line);
            } else if in_text && pos.x >= text_left {
                let ch = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
                let selection = self.selection();
                if ctrl_held && !shift_held {
                    self.caret = ch;
                    self.anchor = ch;
                    let ident = self.identifier_at(ch);
                    if !ident.is_empty() {
                        event = Some(EditorEvent::GotoDefinition(ident));
                    }
                } else if alt_held && !shift_held {
                    // Arm an Alt+drag rectangle; a click without a drag resolves to
                    // the classic Alt+click cursor toggle on release.
                    self.box_anchor = Some(ch);
                    self.box_base = self.extra_cursors.clone();
                    self.box_moved = false;
                } else if !shift_held
                    && selection.is_some_and(|(start, end)| start < ch && ch < end)
                {
                    // Pressing inside the selection arms an in-editor text drag; the
                    // plain-click fallback collapses the selection on release.
                    self.text_drag = Some(super::TextDrag {
                        press_char: ch,
                        press: pos,
                        drop: Some(ch),
                        moved: false,
                    });
                } else {
                    if !shift_held {
                        self.extra_cursors.clear();
                        self.anchor = ch;
                    }
                    self.caret = ch;
                    self.dragging_selection = true;
                }
                self.desired_col = None;
                self.blink_base = ui.input(|i| i.time);
            }
        }
        if let Some(anchor) = self.box_anchor {
            if ui.input(|i| i.pointer.primary_down()) {
                if let Some(pos) = response.interact_pointer_pos()
                    && pos.y >= view_top
                {
                    let current = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
                    // Only rebuild once the pointer actually left the anchor char, so
                    // a plain Alt+click keeps its pre-press caret for the toggle below.
                    if current != anchor || self.box_moved {
                        self.update_box_selection(anchor, current);
                    }
                }
            } else {
                self.box_anchor = None;
                if !self.box_moved {
                    self.toggle_cursor_at(anchor);
                }
            }
        }
        let text_drag = self.text_drag.as_ref().map(|drag| (drag.moved, drag.press));
        if let Some((moved, press)) = text_drag
            && ui.input(|i| i.pointer.primary_down())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let moved = moved || pos.distance(press) > 4.0;
            let drop = (moved && pos.y >= view_top)
                .then(|| self.pos_to_char(ui, style, rect, pos, text_left, text_width));
            if let Some(drag) = &mut self.text_drag {
                drag.moved = moved;
                if let Some(drop) = drop {
                    drag.drop = Some(drop);
                }
            }
        }
        if self.dragging_selection
            && response.is_pointer_button_down_on()
            && let Some(pos) = response.interact_pointer_pos()
            && pos.y >= view_top
        {
            self.caret = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
            self.ensure_visible = true;
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.dragging_selection = false;
            if let Some(drag) = self.text_drag.take() {
                if drag.moved {
                    if let Some(drop) = drag.drop {
                        let copy = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                        if copy {
                            self.duplicate_selection_at(drop);
                        } else {
                            self.move_selection_to(drop);
                        }
                    }
                } else {
                    // Plain click on the selection collapses it to the click point.
                    self.extra_cursors.clear();
                    self.caret = drag.press_char;
                    self.anchor = drag.press_char;
                    self.ensure_visible = true;
                }
            }
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let ch = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
            self.select_word_at(ch);
        }
        if response.triple_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let ch = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
            self.select_line_at(ch);
        }

        // ---- keyboard ----
        if focused {
            if ui.input(|i| i.key_pressed(Key::F12)) {
                let ident = self.identifier_at(self.caret);
                if !ident.is_empty() {
                    event = Some(EditorEvent::GotoDefinition(ident));
                }
            }
            self.handle_input(ui, style, rows);
        }

        // Folding or an edit changed the row count; settle the viewport metrics.
        self.sync_layout();
        self.sync_wrap_metrics(style, text_width, line_h);
        self.sticky_h = self.sticky_height(style, line_h);
        view_h = (rect.height() - self.sticky_h).max(line_h);
        let total_h = self.total_view_height(line_h);
        max_scroll = Self::max_scroll(total_h, view_h, style, line_h);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let view_top = rect.top() + self.sticky_h;

        // Any edit makes the find matches stale; recompute once per frame.
        if self.find.open && self.find.last_version != self.doc.version {
            self.recompute_find();
        }

        // ---- caret visibility ----
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let caret_row = self.row_of_line(caret_line).unwrap_or(0);
        if self.ensure_visible {
            let _ = self.line_galley(ui, style, caret_line, text_width);
            self.sync_row_wrapped_height(caret_row, caret_line, line_h);
            let total_h = self.total_view_height(line_h);
            max_scroll = Self::max_scroll(total_h, view_h, style, line_h);
            let caret_local = match self.galleys.get(&caret_line) {
                Some(cached) => self.pos_of_col_in(cached, caret_col),
                None => Pos2::ZERO,
            };
            let care_y = self.row_top_offset(caret_row, line_h) + caret_local.y - self.scroll_y + PAD_TOP;
            if care_y < 0.0 {
                self.scroll_y += care_y - 2.0;
            } else if care_y + line_h > view_h {
                self.scroll_y += care_y + line_h - view_h + 2.0;
            }
            if !style.word_wrap {
                let x = caret_local.x;
                if x - self.scroll_x < 0.0 {
                    self.scroll_x = (x - 10.0).max(0.0);
                } else if x - self.scroll_x > text_width - 16.0 {
                    self.scroll_x = (x - text_width + 40.0).max(0.0);
                }
            }
            self.ensure_visible = false;
            self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        }

        // ---- paint canvas first so scrollbars render cleanly on top ----
        self.paint(ui, style, rect, line_h, text_left, text_right, text_width);

        // ---- vertical scrollbar (painted on top of canvas) ----
        let total_h = self.total_view_height(line_h);
        max_scroll = Self::max_scroll(total_h, view_h, style, line_h);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let scrollbar_rect = Rect::from_min_size(
            Pos2::new(rect.right() - SCROLLBAR_W, rect.top()),
            Vec2::new(SCROLLBAR_W, rect.height()),
        );
        if total_h > view_h {
            let sb_id = ui.scope_id().with(("v_scroll", self.id));
            let sb = ui.interact(scrollbar_rect, sb_id, Sense::click_and_drag());
            let thumb_h = ((view_h / total_h) * rect.height()).clamp(28.0, rect.height());
            if (sb.dragged() || sb.clicked())
                && let Some(pos) = sb.interact_pointer_pos()
            {
                let travel = (rect.height() - thumb_h).max(1.0);
                let t = ((pos.y - rect.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
                self.scroll_y = t * max_scroll;
            }
            let thumb_y =
                rect.top() + (self.scroll_y / max_scroll.max(1.0)) * (rect.height() - thumb_h);
            let active_sb = sb.hovered() || sb.dragged();
            let thumb_w = if active_sb { 8.0 } else { 5.0 };
            let thumb = Rect::from_min_size(
                Pos2::new(scrollbar_rect.right() - thumb_w - 1.5, thumb_y),
                Vec2::new(thumb_w, thumb_h),
            );
            let thumb_color = if sb.dragged() {
                style.theme.text_dim
            } else if sb.hovered() {
                style.theme.scrollbar_hover
            } else {
                style.theme.scrollbar
            };
            ui.painter().rect_filled(thumb, thumb_w * 0.5, thumb_color);
        }

        // ---- horizontal scrollbar (when word_wrap is off and content overflows) ----
        if !style.word_wrap {
            let max_line_w = self
                .galleys
                .values()
                .map(|c| c.text_width)
                .fold(0.0f32, f32::max)
                .max(self.scroll_x + text_width * 0.5);
            let content_w = max_line_w + 48.0;
            if content_w > text_width + 4.0 {
                let max_scroll_x = (content_w - text_width).max(1.0);
                self.scroll_x = self.scroll_x.clamp(0.0, max_scroll_x);
                let hbar_h = 10.0;
                let hbar_rect = Rect::from_min_max(
                    Pos2::new(text_left, rect.bottom() - hbar_h),
                    Pos2::new(text_right, rect.bottom()),
                );
                let hb_id = ui.scope_id().with(("h_scroll", self.id));
                let hb = ui.interact(hbar_rect, hb_id, Sense::click_and_drag());
                let track_w = hbar_rect.width().max(1.0);
                let thumb_w = ((text_width / content_w) * track_w).clamp(28.0, track_w);
                if (hb.dragged() || hb.clicked())
                    && let Some(pos) = hb.interact_pointer_pos()
                {
                    let travel = (track_w - thumb_w).max(1.0);
                    let t = ((pos.x - hbar_rect.left() - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
                    self.scroll_x = t * max_scroll_x;
                }
                let thumb_x =
                    hbar_rect.left() + (self.scroll_x / max_scroll_x) * (track_w - thumb_w);
                let active_hb = hb.hovered() || hb.dragged();
                let thumb_h = if active_hb { 7.0 } else { 4.5 };
                let thumb = Rect::from_min_size(
                    Pos2::new(thumb_x, hbar_rect.bottom() - thumb_h - 1.5),
                    Vec2::new(thumb_w, thumb_h),
                );
                let thumb_color = if hb.dragged() {
                    style.theme.text_dim
                } else if hb.hovered() {
                    style.theme.scrollbar_hover
                } else {
                    style.theme.scrollbar
                };
                ui.painter().rect_filled(thumb, thumb_h * 0.5, thumb_color);
            }
        }

        self.paint_completion_popup(ui, style, rect, text_left, line_h);
        self.paint_find_widget(ui, style, rect);

        // Right-clicking outside an existing selection moves the caret there before opening the menu.
        if response.secondary_clicked() {
            response.request_focus();
            if let Some(pos) = response.interact_pointer_pos()
                && pos.x >= text_left
                && pos.y >= view_top
            {
                let ch = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
                let in_sel = self
                    .selection()
                    .is_some_and(|(start, end)| ch >= start && ch <= end);
                if !in_sel {
                    self.extra_cursors.clear();
                    self.caret = ch;
                    self.anchor = ch;
                    self.desired_col = None;
                }
            }
        }

        let readonly = self.doc.readonly;
        let can_undo = !readonly && self.doc.can_undo();
        let can_redo = !readonly && self.doc.can_redo();
        let ident = self.identifier_at(self.caret);
        let menu_font = style.ui();
        let sc_font = egui::FontId::new(11.0, style.mono_family.clone());
        let theme = style.theme;
        response.context_menu(|ui| {
            ui.set_width(218.0);
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Close,
                "剪切",
                Some("Ctrl+X"),
                !readonly,
            ) {
                self.copy_selection(ui.ctx());
                self.delete_selection();
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Copy,
                "复制",
                Some("Ctrl+C"),
                true,
            ) {
                self.copy_selection(ui.ctx());
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::File,
                "粘贴",
                Some("Ctrl+V"),
                !readonly,
            ) && let Ok(mut cb) = arboard::Clipboard::new()
                && let Ok(text) = cb.get_text()
                && !text.is_empty()
            {
                self.paste_text(&text);
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::FileCode,
                "全选",
                Some("Ctrl+A"),
                true,
            ) {
                self.select_all();
            }
            crate::ui::widgets::context_menu_sep(ui, &theme);
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Undo,
                "撤销",
                Some("Ctrl+Z"),
                can_undo,
            ) {
                self.undo();
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Redo,
                "重做",
                Some("Ctrl+Y"),
                can_redo,
            ) {
                self.redo();
            }
            crate::ui::widgets::context_menu_sep(ui, &theme);
            if !ident.is_empty()
                && crate::ui::widgets::context_menu_item(
                    ui,
                    &theme,
                    menu_font.clone(),
                    sc_font.clone(),
                    Icon::Goforward,
                    "转到定义",
                    Some("F12"),
                    true,
                )
            {
                event = Some(EditorEvent::GotoDefinition(ident.clone()));
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Search,
                "查找",
                Some("Ctrl+F"),
                true,
            ) {
                self.open_find(false);
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::Replace,
                "替换",
                Some("Ctrl+H"),
                !readonly,
            ) {
                self.open_find(true);
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::FileCode,
                "切换行注释",
                Some("Ctrl+/"),
                !readonly,
            ) {
                self.toggle_comment();
            }
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font.clone(),
                sc_font.clone(),
                Icon::FileCode,
                "格式化文档",
                Some("Shift+Alt+F"),
                !readonly,
            ) {
                let _ = self.format_document();
            }
            crate::ui::widgets::context_menu_sep(ui, &theme);
            if crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                menu_font,
                sc_font,
                Icon::Search,
                "命令面板…",
                Some("Ctrl+Shift+P"),
                true,
            ) {
                event = Some(EditorEvent::OpenPalette);
            }
        });

        // Hover tooltip for diagnostics or outline symbols
        if response.hovered()
            && !self.dragging_selection
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            && pos.x >= text_left
            && pos.x <= text_right
            && pos.y >= view_top
        {
            let ch = self.pos_to_char(ui, style, rect, pos, text_left, text_width);
            let line = self.doc.char_to_line(ch);
            let word = self.identifier_at(ch);
            if let Some(doc_tip) =
                crate::lsp::hover_documentation(&word, line, &self.symbols, &self.diagnostics)
            {
                response.on_hover_text(doc_tip);
            }
        }

        if focused {
            ui.ctx().request_repaint_after(Duration::from_millis(420));
        }
        event
    }

    fn char_width(&mut self, ui: &Ui, style: &EditorStyle) -> f32 {
        if self.char_w_size == style.font_size {
            return self.char_w;
        }
        let font = style.mono();
        let w = ui.ctx().fonts_mut(|fonts| {
            fonts
                .layout_job(LayoutJob::simple_singleline(
                    "0000".to_string(),
                    font,
                    style.theme.text,
                ))
                .rect
                .width()
                / 4.0
        });
        self.char_w = w.max(1.0);
        self.char_w_size = style.font_size;
        self.char_w
    }

    fn style_hash(style: &EditorStyle) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        style.font_size.to_bits().hash(&mut h);
        style.line_h.to_bits().hash(&mut h);
        style.tab_size.hash(&mut h);
        style.show_whitespace.hash(&mut h);
        style.word_wrap.hash(&mut h);
        style.theme.kind.as_u8().hash(&mut h);
        std::mem::discriminant(&style.mono_family).hash(&mut h);
        h.finish()
    }

    /// Hash where a few fixed glyphs landed in the font atlas. A repacked or rebuilt
    /// atlas makes every cached galley point at stale texels, and this probe is the
    /// cheapest reliable signal for it.
    fn atlas_probe(ui: &Ui, style: &EditorStyle) -> u64 {
        use std::hash::{Hash, Hasher};
        let font = style.mono();
        let ppp = ui.ctx().pixels_per_point().to_bits();
        ui.ctx().fonts_mut(|fonts| {
            let galley = fonts.layout_no_wrap("Mg0_".to_owned(), font, Color32::WHITE);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ppp.hash(&mut hasher);
            fonts.font_image_size().hash(&mut hasher);
            for row in &galley.rows {
                for glyph in &row.glyphs {
                    glyph.uv_rect.min.hash(&mut hasher);
                    glyph.uv_rect.max.hash(&mut hasher);
                }
            }
            hasher.finish()
        })
    }

    /// Drop cached galleys only when the document, the style, the wrap width or the
    /// font atlas changed; idle scrolling and caret blinking reuse them.
    fn sync_cache(&mut self, ui: &Ui, style: &EditorStyle, text_width: f32) {
        let atlas_key = Self::atlas_probe(ui, style);
        let atlas_fill = ui.ctx().fonts(|fonts| fonts.font_atlas_fill_ratio());
        let wrap_width = if style.word_wrap {
            text_width.to_bits()
        } else {
            0
        };
        let key = (
            self.doc.version,
            self.spans_version,
            Self::style_hash(style),
            wrap_width,
            self.doc.line_count(),
            atlas_key,
        );
        // A shrinking fill ratio also catches an atlas clear where the probe layout
        // happened to land on identical texels.
        let atlas_rebuilt = (self.atlas_key != 0 && atlas_key != self.atlas_key)
            || atlas_fill + 0.02 < self.atlas_fill;
        if key != self.cache_key || atlas_rebuilt {
            self.galleys.clear();
            self.cache_key = key;
        }
        self.atlas_key = atlas_key;
        self.atlas_fill = atlas_fill;
    }

    /// Lay out one line (with tabs expanded) and cache the galley.
    fn line_galley(
        &mut self,
        ui: &Ui,
        style: &EditorStyle,
        line: usize,
        text_width: f32,
    ) -> Arc<Galley> {
        if let Some(cached) = self.galleys.get(&line) {
            return cached.galley.clone();
        }
        let text = self.doc.line_content(line);
        let line_start_byte = self
            .doc
            .rope
            .line_to_byte(line.min(self.doc.line_count().saturating_sub(1)));
        let default_color = style.theme.syn_text;
        let mut viz = String::with_capacity(text.len() + 8);
        let mut runs: Vec<(usize, usize, Color32)> = Vec::new();
        let mut cur_color = default_color;
        let mut run_start = 0usize;
        let has_tab = text.contains('\t');
        let mut vis_to_doc: Option<Vec<u32>> = if has_tab {
            Some(Vec::with_capacity(text.len() + 8))
        } else {
            None
        };
        let spans = &self.spans;
        let mut span_idx = spans.partition_point(|s: &Span| s.end <= line_start_byte);
        let mut doc_col = 0u32;
        let tab = style.tab_size.max(1) as u32;

        for (byte_idx, ch) in text.char_indices() {
            let abs = line_start_byte + byte_idx;
            while span_idx < spans.len() && spans[span_idx].end <= abs {
                span_idx += 1;
            }
            let color = if spans.is_empty() {
                default_color
            } else {
                match spans.get(span_idx) {
                    Some(span) if span.start <= abs && span.end > abs => {
                        style.theme.token_color(span.kind)
                    }
                    _ => default_color,
                }
            };
            if color != cur_color {
                if viz.len() > run_start {
                    runs.push((run_start, viz.len(), cur_color));
                }
                run_start = viz.len();
                cur_color = color;
            }
            if ch == '\t' {
                let stop = (doc_col / tab + 1) * tab;
                for _ in doc_col..stop {
                    viz.push(' ');
                    if let Some(map) = &mut vis_to_doc {
                        map.push(doc_col);
                    }
                }
            } else {
                viz.push(ch);
                if let Some(map) = &mut vis_to_doc {
                    map.push(doc_col);
                }
            }
            doc_col += 1;
        }
        if viz.len() > run_start {
            runs.push((run_start, viz.len(), cur_color));
        }
        if let Some(map) = &mut vis_to_doc {
            map.push(doc_col);
        }

        let mut job = LayoutJob::default();
        job.wrap.max_width = if style.word_wrap && text_width > 40.0 {
            (text_width - 12.0).max(40.0)
        } else {
            f32::INFINITY
        };
        job.break_on_newline = false;
        job.keep_trailing_whitespace = true;
        let font = style.mono();
        if runs.is_empty() {
            job.append(
                " ",
                0.0,
                TextFormat {
                    font_id: font,
                    color: default_color,
                    line_height: Some(style.line_h),
                    ..Default::default()
                },
            );
        } else {
            for (start, end, color) in runs {
                job.append(
                    &viz[start..end],
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color,
                        line_height: Some(style.line_h),
                        ..Default::default()
                    },
                );
            }
        }
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        let rendered_width = galley.rect.width();
        self.galleys.insert(
            line,
            super::CachedLine {
                galley: galley.clone(),
                vis_to_doc,
                text_width: rendered_width,
            },
        );
        galley
    }

    fn pos_of_col_in(&self, cached: &super::CachedLine, col: usize) -> Pos2 {
        let vis_col = match &cached.vis_to_doc {
            Some(map) => map
                .iter()
                .position(|&d| d as usize >= col)
                .unwrap_or(map.len().saturating_sub(1)),
            None => col,
        };
        let r = cached
            .galley
            .pos_from_cursor(egui::text::CCursor::new(vis_col));
        Pos2::new(r.left(), r.top())
    }

    fn x_of_col_in(&self, cached: &super::CachedLine, col: usize) -> f32 {
        self.pos_of_col_in(cached, col).x
    }

    fn pos_to_char(
        &mut self,
        ui: &Ui,
        style: &EditorStyle,
        rect: Rect,
        pos: Pos2,
        text_left: f32,
        text_width: f32,
    ) -> usize {
        let line_h = style.line_h.max(1.0);
        let row_count = self.line_map.row_count().max(1);
        let view_top = rect.top() + self.sticky_h.max(0.0);
        let row = self.row_at_y(pos.y, view_top, line_h, row_count);
        let line = self.line_map.line_of_row(row);
        let x = pos.x - text_left + self.scroll_x;
        let galley = self.line_galley(ui, style, line, text_width);
        self.sync_row_wrapped_height(row, line, line_h);
        let Some(cached) = self.galleys.get(&line) else {
            return self.doc.line_to_char(line);
        };
        let row_top = view_top + PAD_TOP - self.scroll_y + self.row_top_offset(row, line_h);
        let row_h = self.row_visual_height(row, line_h).max(line_h);
        let local_y = if style.word_wrap {
            (pos.y - row_top).clamp(0.0, (row_h - 1.0).max(0.0))
        } else {
            line_h * 0.5
        };
        let vis_col = if x <= 0.0 && local_y < line_h {
            0
        } else {
            let cc = galley.cursor_from_pos(Vec2::new(x.max(0.0), local_y));
            cc.index.0.min(galley.text().chars().count())
        };
        let doc_col = match &cached.vis_to_doc {
            Some(map) => *map.get(vis_col).unwrap_or(&(map.len() as u32 - 1)) as usize,
            None => vis_col,
        };
        self.doc.line_col_to_char(line, doc_col)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &mut Ui,
        style: &EditorStyle,
        rect: Rect,
        line_h: f32,
        text_left: f32,
        text_right: f32,
        text_width: f32,
    ) {
        let theme = style.theme;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme.bg);

        let sticky_h = self.sticky_h.max(0.0);
        let view_top = rect.top() + sticky_h;
        let row_count = self.line_map.row_count().max(1);
        let first_row = self.first_visible_row(line_h).min(row_count - 1);
        let origin_y = view_top + PAD_TOP - self.scroll_y;
        let origin = Pos2::new(text_left, origin_y);
        let clip = Rect::from_min_max(
            Pos2::new(text_left, view_top),
            Pos2::new(text_right, rect.bottom()),
        );
        let tp = painter.with_clip_rect(clip);

        // Ensure galleys exist for visible rows and refine wrapped heights before painting.
        let mut last_row = first_row;
        while last_row < row_count {
            let line = self.line_map.line_of_row(last_row);
            let _ = self.line_galley(ui, style, line, text_width);
            self.sync_row_wrapped_height(last_row, line, line_h);
            let row_bottom =
                origin_y + self.row_top_offset(last_row, line_h) + self.row_visual_height(last_row, line_h);
            last_row += 1;
            if row_bottom >= rect.bottom() + line_h {
                break;
            }
        }
        let last_row = last_row.max((first_row + 1).min(row_count));
        let first_line = self.line_map.line_of_row(first_row);
        let last_line = self.line_map.line_of_row(last_row.saturating_sub(1));
        let matching_brackets = if self.has_focus {
            self.cached_matching_brackets()
        } else {
            None
        };

        let row_y = |row: usize| origin_y + self.row_top_offset(row, line_h);
        let row_h = |row: usize| self.row_visual_height(row, line_h);

        // Current line highlight (text area; gutter highlight painted after gutter background)
        let caret_line = self.doc.char_to_line(self.caret);
        let caret_col = self.doc.char_to_line_col(self.caret).1;
        if let Some(caret_row) = self.line_map.row_of(caret_line)
            && caret_row >= first_row
            && caret_row < last_row
        {
            let y = row_y(caret_row);
            let h = row_h(caret_row);
            tp.rect_filled(
                Rect::from_min_max(Pos2::new(text_left, y), Pos2::new(text_right, y + h)),
                0.0,
                theme.current_line,
            );
        }

        // Indent guides (respecting scroll_x and clipped to text viewport)
        if style.show_indent_guides {
            let guide_color = theme.indent_guide;
            let active = theme.indent_guide_active;
            let tab = style.tab_size.max(1);
            for row in first_row..last_row {
                let line = self.line_map.line_of_row(row);
                let Some(cached) = self.galleys.get(&line) else {
                    continue;
                };
                let content = self.doc.line_content(line);
                let mut cols = 0usize;
                for ch in content.chars() {
                    if ch == ' ' {
                        cols += 1;
                    } else if ch == '\t' {
                        cols = (cols / tab + 1) * tab;
                    } else {
                        break;
                    }
                }
                let y = row_y(row);
                let h = row_h(row);
                let mut col = tab;
                while col <= cols {
                    let x = origin.x + self.x_of_col_in(cached, col) - self.scroll_x;
                    let color = if line == caret_line && col <= caret_col {
                        active
                    } else {
                        guide_color
                    };
                    tp.line_segment(
                        [Pos2::new(x, y), Pos2::new(x, y + h)],
                        Stroke::new(1.0, color),
                    );
                    col += tab;
                }
            }
        }

        // Find matches (only those intersecting the viewport, clipped & wrap-aware)
        if self.find.open && !self.find.matches.is_empty() {
            let visible_start = self.doc.line_to_char(first_line);
            let visible_end =
                self.doc.line_to_char(last_line) + self.doc.line_char_len(last_line);
            let first_match = self
                .find
                .matches
                .partition_point(|(start, _)| *start < visible_start);
            for (idx, &(start, end)) in self.find.matches.iter().enumerate().skip(first_match) {
                if start > visible_end {
                    break;
                }
                let color = if self.find.active == Some(idx) {
                    theme.find_active
                } else {
                    theme.find_match
                };
                let start_line = self.doc.char_to_line(start);
                let end_line = self.doc.char_to_line(end);
                for line in start_line..=end_line {
                    let Some(row) = self.line_map.row_of(line) else {
                        continue;
                    };
                    if row < first_row || row >= last_row {
                        continue;
                    }
                    let Some(cached) = self.galleys.get(&line) else {
                        continue;
                    };
                    let line_start = self.doc.line_to_char(line);
                    let line_len = self.doc.line_char_len(line);
                    let a = start.saturating_sub(line_start).min(line_len);
                    let b = if end > line_start + line_len {
                        line_len
                    } else {
                        end.saturating_sub(line_start)
                    };
                    let p1 = self.pos_of_col_in(cached, a);
                    let p2 = self.pos_of_col_in(cached, b);
                    let y = row_y(row);
                    if (p1.y - p2.y).abs() < 1.0 {
                        let x1 = origin.x + p1.x - self.scroll_x;
                        let x2 = if b >= line_len && !style.word_wrap {
                            origin.x + cached.text_width.max((b as f32) * self.char_w) - self.scroll_x
                        } else {
                            origin.x + p2.x - self.scroll_x
                        };
                        tp.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(x1 - 1.0, y + p1.y),
                                Pos2::new(x2 + 1.0, y + p1.y + line_h),
                            ),
                            2.0,
                            color,
                        );
                    } else {
                        for prow in &cached.galley.rows {
                            let r = prow.rect();
                            if r.bottom() <= p1.y + 0.5 || r.top() >= p2.y + line_h - 0.5 {
                                continue;
                            }
                            let sx = if (r.top() - p1.y).abs() < 1.0 { p1.x } else { r.left() };
                            let ex = if (r.top() - p2.y).abs() < 1.0 { p2.x } else { r.right() };
                            tp.rect_filled(
                                Rect::from_min_max(
                                    Pos2::new(origin.x + sx - self.scroll_x - 1.0, y + r.top()),
                                    Pos2::new(origin.x + ex - self.scroll_x + 1.0, y + r.top() + line_h),
                                ),
                                2.0,
                                color,
                            );
                        }
                    }
                }
            }
        }

        // Selections (Primary + Extra Multi-Cursors), clipped & wrap-aware.
        let sel_color = if self.has_focus {
            theme.selection
        } else {
            theme.selection_inactive
        };
        let mut all_selections: Vec<(usize, usize)> = Vec::new();
        if let Some(primary) = self.selection() {
            all_selections.push(primary);
        }
        for &(c, a) in &self.extra_cursors {
            if c != a {
                all_selections.push((c.min(a), c.max(a)));
            }
        }
        if !all_selections.is_empty() {
            for row in first_row..last_row {
                let line = self.line_map.line_of_row(row);
                let line_start = self.doc.line_to_char(line);
                let line_len = self.doc.line_char_len(line);
                let Some(cached) = self.galleys.get(&line) else {
                    continue;
                };
                let y = row_y(row);
                for &(sel_start, sel_end) in &all_selections {
                    if sel_end < line_start || sel_start > line_start + line_len {
                        continue;
                    }
                    let a = sel_start.saturating_sub(line_start).min(line_len);
                    let spans_newline = sel_end > line_start + line_len;
                    let b = if spans_newline {
                        line_len
                    } else {
                        sel_end.saturating_sub(line_start)
                    };
                    let p1 = self.pos_of_col_in(cached, a);
                    let p2 = self.pos_of_col_in(cached, b);
                    if (p1.y - p2.y).abs() < 1.0 {
                        let x1 = origin.x + p1.x - self.scroll_x;
                        let x2 = if spans_newline {
                            text_right - 4.0
                        } else {
                            origin.x + p2.x - self.scroll_x
                        };
                        if x2 > x1 || spans_newline {
                            tp.rect_filled(
                                Rect::from_min_max(
                                    Pos2::new(x1, y + p1.y),
                                    Pos2::new(x2.max(x1 + 2.0), y + p1.y + line_h),
                                ),
                                2.0,
                                sel_color,
                            );
                        }
                    } else {
                        let row_len = cached.galley.rows.len();
                        for (idx, prow) in cached.galley.rows.iter().enumerate() {
                            let r = prow.rect();
                            if r.bottom() <= p1.y + 0.5 || r.top() >= p2.y + line_h - 0.5 {
                                continue;
                            }
                            let sx = if (r.top() - p1.y).abs() < 1.0 {
                                origin.x + p1.x - self.scroll_x
                            } else {
                                origin.x + r.left() - self.scroll_x
                            };
                            let ex = if idx + 1 == row_len && spans_newline {
                                text_right - 4.0
                            } else if (r.top() - p2.y).abs() < 1.0 && !spans_newline {
                                origin.x + p2.x - self.scroll_x
                            } else {
                                origin.x + r.right().max(r.left() + 4.0) - self.scroll_x
                            };
                            if ex > sx {
                                tp.rect_filled(
                                    Rect::from_min_max(
                                        Pos2::new(sx, y + r.top()),
                                        Pos2::new(ex.max(sx + 2.0), y + r.top() + line_h),
                                    ),
                                    2.0,
                                    sel_color,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Text + Folded Ellipsis Badges
        let default_color = theme.syn_text;
        for row in first_row..last_row {
            let line = self.line_map.line_of_row(row);
            let Some(cached) = self.galleys.get(&line) else {
                continue;
            };
            let y = row_y(row);
            let pos = Pos2::new(origin.x - self.scroll_x, y);
            tp.galley(pos, cached.galley.clone(), default_color);

            // Folded badge if this line starts a folded block
            if self.folded_lines.contains(&line) {
                let badge_x = origin.x + cached.text_width - self.scroll_x + 8.0;
                let badge_rect = Rect::from_min_size(
                    Pos2::new(badge_x, y + 3.0),
                    Vec2::new(26.0, (line_h - 6.0).max(12.0)),
                );
                tp.rect_filled(badge_rect, CornerRadius::same(4), theme.active);
                tp.rect_stroke(
                    badge_rect,
                    CornerRadius::same(4),
                    Stroke::new(1.0, theme.border_strong),
                    StrokeKind::Inside,
                );
                tp.text(
                    badge_rect.center(),
                    Align2::CENTER_CENTER,
                    "⋯",
                    style.ui(),
                    theme.accent,
                );
            }
        }

        // Diagnostic wavy underlines
        for diag in &self.diagnostics {
            if diag.line < first_line || diag.line > last_line {
                continue;
            }
            let Some(row) = self.line_map.row_of(diag.line) else {
                continue;
            };
            if row < first_row || row >= last_row {
                continue;
            }
            if let Some(cached) = self.galleys.get(&diag.line) {
                let p1 = self.pos_of_col_in(cached, diag.col);
                let p2 = self.pos_of_col_in(cached, diag.end_col);
                let x1 = origin.x + p1.x - self.scroll_x;
                let x2 = if (p1.y - p2.y).abs() < 1.0 {
                    (origin.x + p2.x - self.scroll_x).max(x1 + self.char_w * 2.0)
                } else {
                    (text_right - 6.0).max(x1 + self.char_w * 2.0)
                };
                let y = row_y(row) + p1.y + line_h - 2.0;
                let color = match diag.severity {
                    DiagSeverity::Error => theme.error,
                    DiagSeverity::Warning => theme.warning,
                    DiagSeverity::Info => theme.accent,
                };
                paint_squiggly_line(&tp, x1, x2, y, color);
            }
        }

        // Whitespace markers (clipped to text viewport)
        if style.show_whitespace {
            let dot = theme.text_faint.gamma_multiply(0.6);
            for row in first_row..last_row {
                let line = self.line_map.line_of_row(row);
                let Some(cached) = self.galleys.get(&line) else {
                    continue;
                };
                let content = self.doc.line_content(line);
                let y_base = row_y(row);
                let mut col = 0usize;
                for ch in content.chars() {
                    if ch == ' ' {
                        let p = self.pos_of_col_in(cached, col);
                        let x = origin.x + p.x - self.scroll_x + self.char_w * 0.5;
                        tp.circle_filled(Pos2::new(x, y_base + p.y + line_h * 0.5), 0.9, dot);
                    } else if ch == '\t' {
                        let p = self.pos_of_col_in(cached, col);
                        let x = origin.x + p.x - self.scroll_x + 2.0;
                        let y = y_base + p.y + line_h * 0.5;
                        tp.line_segment(
                            [Pos2::new(x, y), Pos2::new(x + self.char_w * 1.4, y)],
                            Stroke::new(1.0, dot),
                        );
                    } else {
                        break;
                    }
                    col += 1;
                    if col > 400 {
                        break;
                    }
                }
            }
        }

        // Gutter (Line numbers + Fold chevrons + Git diff bars)
        if style.show_line_numbers {
            painter.rect_filled(
                Rect::from_min_max(rect.min, Pos2::new(text_left, rect.bottom())),
                0.0,
                theme.bg,
            );
            // Current-line gutter highlight painted AFTER gutter bg fill so it stays visible.
            if let Some(caret_row) = self.line_map.row_of(caret_line)
                && caret_row >= first_row
                && caret_row < last_row
            {
                let y = row_y(caret_row);
                let h = row_h(caret_row);
                let gutter_clip = painter.with_clip_rect(Rect::from_min_max(
                    Pos2::new(rect.left(), view_top),
                    Pos2::new(text_left, rect.bottom()),
                ));
                gutter_clip.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rect.left(), y),
                        Pos2::new(text_left, y + h),
                    ),
                    0.0,
                    theme.current_line,
                );
            }
            painter.line_segment(
                [
                    Pos2::new(text_left - 0.5, rect.top()),
                    Pos2::new(text_left - 0.5, rect.bottom()),
                ],
                Stroke::new(1.0, theme.border),
            );
            let font = FontId::new(style.font_size - 1.0, style.mono_family.clone());
            let hover_gutter = ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|p| p.x >= rect.left() && p.x <= text_left + 20.0);

            for row in first_row..last_row {
                let line = self.line_map.line_of_row(row);
                let y = row_y(row);
                let color = if line == caret_line {
                    theme.text
                } else {
                    theme.text_muted
                };
                painter.text(
                    Pos2::new(text_left - 18.0, y + line_h * 0.5),
                    Align2::RIGHT_CENTER,
                    (line + 1).to_string(),
                    font.clone(),
                    color,
                );

                // Fold chevron
                let is_folded = self.folded_lines.contains(&line);
                if (hover_gutter || is_folded) && self.fold_range_for_line(line).is_some() {
                    let chev_rect = Rect::from_center_size(
                        Pos2::new(text_left - 9.0, y + line_h * 0.5),
                        Vec2::splat(11.0),
                    );
                    icons::paint(
                        &painter,
                        chev_rect,
                        if is_folded {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        if is_folded {
                            theme.accent
                        } else {
                            theme.text_dim
                        },
                        1.2,
                    );
                }

                // Git / Buffer Line Diff bar
                if style.show_git_gutter
                    && let Some(diff_kind) = self.line_diffs.get(line)
                {
                    match diff_kind {
                        LineDiffKind::Added => {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(text_left - 3.0, y + 1.0),
                                    Vec2::new(2.5, line_h - 2.0),
                                ),
                                1.0,
                                theme.git_added,
                            );
                        }
                        LineDiffKind::Modified => {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(text_left - 3.0, y + 1.0),
                                    Vec2::new(2.5, line_h - 2.0),
                                ),
                                1.0,
                                theme.accent,
                            );
                        }
                        LineDiffKind::DeletedBelow => {
                            let tri = vec![
                                Pos2::new(text_left - 4.0, y + line_h - 3.0),
                                Pos2::new(text_left, y + line_h),
                                Pos2::new(text_left - 4.0, y + line_h + 3.0),
                            ];
                            painter.add(egui::Shape::convex_polygon(
                                tri,
                                theme.git_deleted,
                                Stroke::NONE,
                            ));
                        }
                        LineDiffKind::Unchanged => {}
                    }
                }
            }
        }

        // Bracket match (memoized, clipped & wrap-aware)
        if let Some((open, close)) = matching_brackets {
            for ch_pos in [open, close] {
                let line = self.doc.char_to_line(ch_pos);
                let Some(row) = self.line_map.row_of(line) else {
                    continue;
                };
                if row < first_row || row >= last_row {
                    continue;
                }
                let col = self.doc.char_to_line_col(ch_pos).1;
                if let Some(cached) = self.galleys.get(&line) {
                    let p = self.pos_of_col_in(cached, col);
                    let x = origin.x + p.x - self.scroll_x;
                    let y = row_y(row) + p.y;
                    tp.rect_stroke(
                        Rect::from_min_max(
                            Pos2::new(x - 1.0, y + 1.0),
                            Pos2::new(x + self.char_w + 1.0, y + line_h - 1.0),
                        ),
                        2.0,
                        Stroke::new(1.0, theme.bracket_match),
                        StrokeKind::Inside,
                    );
                }
            }
        }

        // Carets (Primary + Extra Multi-Cursors, clipped & wrap-aware)
        if self.has_focus {
            let t = ui.input(|i| i.time);
            let blinking = ((t - self.blink_base) * 1.8).fract();
            if blinking < 0.62 {
                let mut carets = Vec::with_capacity(self.extra_cursors.len() + 1);
                carets.push(self.caret);
                for &(c, _) in &self.extra_cursors {
                    carets.push(c);
                }
                for c_pos in carets {
                    let c_line = self.doc.char_to_line(c_pos);
                    let Some(c_row) = self.line_map.row_of(c_line) else {
                        continue;
                    };
                    if c_row < first_row || c_row >= last_row {
                        continue;
                    }
                    let c_col = self.doc.char_to_line_col(c_pos).1;
                    if let Some(cached) = self.galleys.get(&c_line) {
                        let p = self.pos_of_col_in(cached, c_col);
                        let x = origin.x + p.x - self.scroll_x;
                        let y = row_y(c_row) + p.y;
                        tp.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(x, y + 1.0),
                                Vec2::new(2.0, line_h - 2.0),
                            ),
                            CornerRadius::same(1),
                            theme.accent,
                        );
                    }
                }
            }
        }

        // Drop caret while dragging selected text inside the canvas (clipped & wrap-aware).
        if let Some(drag) = &self.text_drag
            && drag.moved
            && let Some(drop) = drag.drop
        {
            let line = self.doc.char_to_line(drop);
            let col = self.doc.char_to_line_col(drop).1;
            if let Some(row) = self.line_map.row_of(line)
                && row >= first_row
                && row < last_row
                && let Some(cached) = self.galleys.get(&line)
            {
                let p = self.pos_of_col_in(cached, col);
                let x = origin.x + p.x - self.scroll_x;
                let y = row_y(row) + p.y;
                tp.rect_filled(
                    Rect::from_min_size(Pos2::new(x - 1.0, y), Vec2::new(2.0, line_h)),
                    1.0,
                    theme.accent,
                );
            }
        }

        // IME: keep the OS composition window anchored at the caret and paint
        // the pre-edit string inline while a Chinese/Japanese/Korean IME is
        // composing. Without the output below, egui-winit disables the IME for
        // the whole window and typing falls back to plain ASCII.
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let caret_row = self
            .line_map
            .row_of(caret_line)
            .filter(|row| *row >= first_row && *row < last_row);
        let caret_pos = caret_row.map(|row| {
            let p = match self.galleys.get(&caret_line) {
                Some(cached) => self.pos_of_col_in(cached, caret_col),
                None => Pos2::ZERO,
            };
            Pos2::new(origin.x + p.x - self.scroll_x, row_y(row) + p.y)
        });
        if self.has_focus && !self.doc.readonly {
            // Keep IME enabled for the whole editor even while the caret is
            // scrolled out of view: toggling IMEAllowed resets the OS IME mode.
            let caret = caret_pos.unwrap_or(Pos2::new(text_left, view_top + line_h));
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            let cursor_rect = Rect::from_min_size(caret, Vec2::new(2.0, line_h));
            let editor_rect = to_global * rect;
            let interrupt_composition = std::mem::take(&mut self.ime_interrupt);
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: editor_rect,
                    // The OS candidate window follows the caret rectangle.
                    cursor_rect: to_global * cursor_rect,
                    should_interrupt_composition: interrupt_composition,
                });
            });
            if let Some(caret) = caret_pos
                && !self.ime_preedit.is_empty()
            {
                let font = FontId::new(style.font_size, style.mono_family.clone());
                let job = LayoutJob::single_section(
                    self.ime_preedit.clone(),
                    TextFormat {
                        font_id: font,
                        color: theme.text,
                        underline: Stroke::new(1.0, theme.accent),
                        line_height: Some(line_h),
                        ..Default::default()
                    },
                );
                let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
                painter
                    .with_clip_rect(Rect::from_min_max(
                        Pos2::new(text_left, view_top),
                        Pos2::new(text_right, rect.bottom()),
                    ))
                    .galley(caret, galley, theme.text);
            }
        }

        // Sticky Scroll header (top of editor canvas)
        if let Some(header_line) = self.sticky_header_line(style, line_h) {
            let sticky_rect = Rect::from_min_size(
                Pos2::new(rect.left(), rect.top()),
                Vec2::new(text_right - rect.left(), sticky_h),
            );
            painter.rect_filled(sticky_rect, 0.0, theme.panel_bg);
            painter.line_segment(
                [
                    Pos2::new(sticky_rect.left(), sticky_rect.bottom()),
                    Pos2::new(sticky_rect.right(), sticky_rect.bottom()),
                ],
                Stroke::new(1.0, theme.border_strong),
            );
            let header_line_text = self.doc.line_content(header_line);
            if style.show_line_numbers {
                painter.text(
                    Pos2::new(text_left - 18.0, sticky_rect.center().y),
                    Align2::RIGHT_CENTER,
                    (header_line + 1).to_string(),
                    FontId::new(style.font_size - 1.0, style.mono_family.clone()),
                    theme.text_faint,
                );
            }
            painter.with_clip_rect(sticky_rect).text(
                Pos2::new(text_left, sticky_rect.center().y),
                Align2::LEFT_CENTER,
                header_line_text.trim_end(),
                style.mono(),
                theme.text_dim,
            );
        }

        // Minimap
        if style.show_minimap {
            self.paint_minimap(ui, style, rect, text_right, line_h);
        }

        // Keep the galley cache bounded to the neighbourhood of the viewport.
        let keep_from = first_line.saturating_sub(64);
        let keep_to = last_line + 64;
        self.galleys
            .retain(|line, _| *line >= keep_from && *line <= keep_to);
    }

    fn paint_minimap(
        &mut self,
        ui: &mut Ui,
        style: &EditorStyle,
        rect: Rect,
        text_right: f32,
        line_h: f32,
    ) {
        let theme = style.theme;
        let painter = ui.painter_at(rect);
        let mm = Rect::from_min_max(
            Pos2::new(text_right + 4.0, rect.top() + 2.0),
            Pos2::new(rect.right() - SCROLLBAR_W - 2.0, rect.bottom() - 2.0),
        );
        painter.rect_filled(mm, 0.0, theme.bg);
        painter.line_segment(
            [
                Pos2::new(mm.left(), mm.top()),
                Pos2::new(mm.left(), mm.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );

        let row_count = self.line_map.row_count().max(1);
        let rows = ((mm.height() / 2.2) as usize).clamp(1, 380).min(row_count);
        let step = (row_count as f32 / rows as f32).max(1.0);
        let row_h = (mm.height() / rows as f32).clamp(1.6, 3.2);
        let content_h = (rows as f32 * row_h).min(mm.height()).max(1.0);

        // Rescan the rope only when the document or the minimap geometry changed.
        let key = (
            self.doc.version,
            self.spans_version,
            theme.kind.as_u8(),
            rows,
            mm.height().to_bits(),
            self.fold_rev,
        );
        if self.minimap_cache.key != key || self.minimap_cache.rows.len() != rows {
            let tab = style.tab_size.max(1);
            let mut cached_rows = Vec::with_capacity(rows);
            for i in 0..rows {
                let src_row = (((i as f32) * step) as usize).min(row_count - 1);
                let line = self.line_map.line_of_row(src_row);
                let content = self.doc.line_content(line);
                let mut indent = 0usize;
                for ch in content.chars() {
                    match ch {
                        ' ' => indent += 1,
                        '\t' => indent = (indent / tab + 1) * tab,
                        _ => break,
                    }
                }
                let len = content.trim_end().chars().count();
                let diff = self.line_diffs.get(line).and_then(|kind| match kind {
                    LineDiffKind::Added => Some(theme.git_added),
                    LineDiffKind::Modified => Some(theme.accent),
                    LineDiffKind::DeletedBelow => Some(theme.git_deleted),
                    LineDiffKind::Unchanged => None,
                });
                cached_rows.push(MinimapRow {
                    indent: indent.min(u16::MAX as usize) as u16,
                    len: len.min(u16::MAX as usize) as u16,
                    color: self.minimap_line_color(line, &style.theme),
                    diff,
                });
            }
            self.minimap_cache.rows = cached_rows;
            self.minimap_cache.key = key;
        }

        let caret_line = self.doc.char_to_line(self.caret);
        let caret_row = self.line_map.row_of(caret_line).unwrap_or(0);
        let caret_y = mm.top() + (caret_row as f32 / row_count as f32) * content_h;
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(mm.left() + 1.0, caret_y),
                Vec2::new(mm.width() - 2.0, row_h.max(2.0)),
            ),
            0.0,
            theme.accent_subtle,
        );

        for (i, cached_row) in self.minimap_cache.rows.iter().enumerate() {
            let row = *cached_row;
            let y = mm.top() + i as f32 * row_h;
            if let Some(color) = row.diff {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(mm.left() + 1.0, y), Vec2::new(2.0, row_h)),
                    0.0,
                    color,
                );
            }
            if row.len == 0 {
                continue;
            }
            let x = mm.left() + 5.0 + (row.indent as f32 * 0.45).min(mm.width() * 0.42);
            let width = ((row.len as f32 * 0.45).min(mm.width() - (x - mm.left()) - 4.0)).max(1.5);
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(x, y + row_h * 0.15),
                    Vec2::new(width, (row_h * 0.65).max(1.0)),
                ),
                0.5,
                row.color,
            );
        }

        // Find matches indicator on minimap
        if self.find.open {
            for &(start, _) in self.find.matches.iter().take(200) {
                let Some(row) = self.line_map.row_of(self.doc.char_to_line(start)) else {
                    continue;
                };
                let my = mm.top() + (row as f32 / row_count as f32) * content_h;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(mm.right() - 6.0, my), Vec2::new(5.0, 2.5)),
                    1.0,
                    theme.find_active,
                );
            }
        }

        // Viewport slider indicator
        let view_h = (rect.height() - self.sticky_h.max(0.0)).max(line_h);
        let (slider_offset, vh) =
            minimap_slider_span(content_h, row_count, line_h, view_h, self.scroll_y);
        let vy = mm.top() + slider_offset;
        let vp_rect = Rect::from_min_size(
            Pos2::new(mm.left() + 1.0, vy),
            Vec2::new(mm.width() - 2.0, vh),
        );
        painter.rect_filled(vp_rect, 2.0, theme.minimap_viewport);
        painter.rect_stroke(
            vp_rect,
            2.0,
            Stroke::new(1.0, theme.border_strong),
            StrokeKind::Inside,
        );

        let id = ui.scope_id().with(("minimap", self.id));
        let resp = ui.interact(mm, id, Sense::click_and_drag());
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let total = row_count as f32 * line_h;
            let max_scroll = Self::max_scroll(total, view_h, style, line_h);
            if max_scroll > 0.0 && content_h > vh {
                let t = ((pos.y - mm.top() - vh * 0.5) / (content_h - vh)).clamp(0.0, 1.0);
                self.scroll_y = t * max_scroll;
            }
        }
    }

    fn minimap_line_color(&self, line: usize, theme: &crate::theme::Theme) -> Color32 {
        let start = self
            .doc
            .rope
            .line_to_byte(line.min(self.doc.line_count().saturating_sub(1)));
        let idx = self.spans.partition_point(|s| s.end <= start);
        let kind = self.spans.get(idx).map(|s| s.kind);
        let base = match kind {
            Some(TokenKind::Comment) => theme.syn_comment,
            Some(TokenKind::Keyword) | Some(TokenKind::Macro) => theme.syn_keyword,
            Some(TokenKind::String) => theme.syn_string,
            Some(TokenKind::Function) => theme.syn_function,
            Some(TokenKind::Type) => theme.syn_type,
            _ => theme.text_faint,
        };
        base.gamma_multiply(0.62)
    }

    /// Memoized bracket pair for the paint pass: the caret rarely moves between
    /// frames, so scanning up to 20k chars per repaint was pure overhead.
    fn cached_matching_brackets(&mut self) -> Option<(usize, usize)> {
        if let Some(cache) = self.bracket_cache
            && cache.version == self.doc.version
            && cache.caret == self.caret
        {
            return cache.pair;
        }
        let pair = self.matching_brackets();
        self.bracket_cache = Some(super::BracketCache {
            version: self.doc.version,
            caret: self.caret,
            pair,
        });
        pair
    }

    /// Build the rectangular cursor set for an Alt+drag from `anchor` to `current`.
    /// Zero-width drags become one caret per line, wider drags one selection per line.
    fn update_box_selection(&mut self, anchor: usize, current: usize) {
        if current != anchor {
            self.box_moved = true;
        }
        let (anchor_line, anchor_col) = self.doc.char_to_line_col(anchor);
        let (current_line, current_col) = self.doc.char_to_line_col(current);
        let (first, last) = (
            anchor_line.min(current_line),
            anchor_line.max(current_line),
        );
        // A runaway gesture must not allocate a cursor per document line.
        if last - first > 2_000 {
            return;
        }
        let (col_start, col_end) = (anchor_col.min(current_col), anchor_col.max(current_col));
        let mut extras = self.box_base.clone();
        for line in first..=last {
            let len = self.doc.line_char_len(line);
            let start = self.doc.line_col_to_char(line, col_start.min(len));
            let end = self.doc.line_col_to_char(line, col_end.min(len));
            if line == current_line {
                self.anchor = start;
                self.caret = if col_start == col_end { start } else { end };
            } else if col_start == col_end {
                extras.push((start, start));
            } else {
                extras.push((end, start));
            }
        }
        self.extra_cursors = extras;
        self.ensure_visible = true;
        self.desired_col = None;
        self.selection_history.clear();
    }

    fn paint_completion_popup(
        &mut self,
        ui: &Ui,
        style: &EditorStyle,
        editor_rect: Rect,
        text_left: f32,
        line_h: f32,
    ) {
        if !self.completion.open || self.completion.items.is_empty() {
            return;
        }
        let theme = style.theme;
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let caret_local = match self.galleys.get(&caret_line) {
            Some(cached) => self.pos_of_col_in(cached, caret_col),
            None => Pos2::ZERO,
        };
        let x = text_left + caret_local.x - self.scroll_x;
        let caret_row = self.line_map.row_of(caret_line).unwrap_or(0);
        let caret_top_y = editor_rect.top()
            + self.sticky_h
            + PAD_TOP
            + self.row_top_offset(caret_row, line_h)
            + caret_local.y
            - self.scroll_y;
        let visible_count = self
            .completion
            .items
            .len()
            .min(crate::lsp::COMPLETION_MAX_ITEMS);
        let popup_w = 316.0;
        let popup_h = visible_count as f32 * 24.0 + 10.0;
        let below_y = caret_top_y + line_h + 2.0;
        let y = if below_y + popup_h > editor_rect.bottom() - 8.0
            && caret_top_y - popup_h - 2.0 >= editor_rect.top() + 8.0
        {
            // Flip above the active line when near the bottom of the editor.
            caret_top_y - popup_h - 2.0
        } else {
            below_y.clamp(
                editor_rect.top() + 8.0,
                (editor_rect.bottom() - popup_h - 8.0).max(editor_rect.top() + 8.0),
            )
        };
        let popup_pos = Pos2::new(
            x.clamp(
                editor_rect.left() + 8.0,
                (editor_rect.right() - popup_w - 8.0).max(editor_rect.left() + 8.0),
            ),
            y,
        );

        let mut clicked_idx = None;
        egui::Area::new(ui.scope_id().with(("completion_popup", self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(popup_pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme.overlay_bg)
                    .stroke(Stroke::new(1.0, theme.border_strong))
                    .corner_radius(CornerRadius::same(crate::theme::R_MD))
                    .inner_margin(egui::Margin::same(4))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: theme.shadow,
                    })
                    .show(ui, |ui| {
                        let inner_w = popup_w - 8.0;
                        ui.set_width(inner_w);
                        for (idx, item) in self
                            .completion
                            .items
                            .iter()
                            .take(crate::lsp::COMPLETION_MAX_ITEMS)
                            .enumerate()
                        {
                            let selected = idx == self.completion.selected;
                            let (row_rect, resp) = ui.allocate_exact_size(
                                Vec2::new(inner_w, 24.0),
                                Sense::click(),
                            );
                            if selected || resp.hovered() {
                                ui.painter().rect_filled(
                                    row_rect,
                                    CornerRadius::same(crate::theme::R_SM),
                                    if selected { theme.active } else { theme.hover },
                                );
                            }
                            let icon = item.kind.icon();
                            let icon_rect = Rect::from_center_size(
                                Pos2::new(row_rect.left() + 12.0, row_rect.center().y),
                                Vec2::splat(14.0),
                            );
                            icons::paint(
                                ui.painter(),
                                icon_rect,
                                icon,
                                icons::completion_icon_color(icon, &theme),
                                1.2,
                            );
                            let detail_font = FontId::new(10.5, style.ui_family.clone());
                            let (detail_galley, _) = crate::ui::widgets::ellipsize_mode(
                                ui,
                                &item.detail,
                                detail_font,
                                theme.text_muted,
                                110.0,
                                crate::ui::widgets::EllipsizeMode::End,
                            );
                            let detail_w = if item.detail.is_empty() {
                                0.0
                            } else {
                                detail_galley.rect.width() + 10.0
                            };
                            let label_max = (row_rect.width() - 34.0 - detail_w).max(60.0);
                            let label_color = if selected { theme.text } else { theme.text_dim };
                            let (label_galley, _) = crate::ui::widgets::ellipsize_mode(
                                ui,
                                &item.label,
                                style.mono(),
                                label_color,
                                label_max,
                                crate::ui::widgets::EllipsizeMode::End,
                            );
                            ui.painter().galley(
                                Pos2::new(
                                    row_rect.left() + 26.0,
                                    row_rect.center().y - label_galley.rect.height() * 0.5,
                                ),
                                label_galley,
                                label_color,
                            );
                            if !item.detail.is_empty() {
                                ui.painter().galley(
                                    Pos2::new(
                                        row_rect.right() - 8.0 - detail_galley.rect.width(),
                                        row_rect.center().y - detail_galley.rect.height() * 0.5,
                                    ),
                                    detail_galley,
                                    theme.text_muted,
                                );
                            }
                            if resp.clicked() {
                                clicked_idx = Some(idx);
                            }
                        }
                    });
            });
        if let Some(idx) = clicked_idx {
            self.completion.selected = idx;
            self.accept_completion();
        }
    }

    fn paint_find_widget(&mut self, ui: &Ui, style: &EditorStyle, editor_rect: Rect) {
        if !self.find.open {
            return;
        }
        let theme = style.theme;
        let width = 456.0f32.min((editor_rect.width() - 24.0).max(280.0));
        let minimap_reserve = if style.show_minimap { MINIMAP_W } else { 0.0 };
        let pos = Pos2::new(
            (editor_rect.right() - width - minimap_reserve - SCROLLBAR_W - 12.0)
                .max(editor_rect.left() + 12.0),
            editor_rect.top() + 8.0,
        );
        let mut close = false;
        egui::Area::new(ui.scope_id().with(("find_area", self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme.overlay_bg)
                    .stroke(Stroke::new(1.0, theme.border_strong))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 7))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 18,
                        spread: 0,
                        color: theme.shadow,
                    })
                    .show(ui, |ui| {
                        ui.set_width(width - 16.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            // Expand/Collapse Replace toggle button
                            if find_icon_button(
                                ui,
                                &theme,
                                if self.find.show_replace {
                                    Icon::ChevronDown
                                } else {
                                    Icon::ChevronRight
                                },
                                false,
                                "切换替换模式 (Ctrl+H)",
                            ) {
                                self.find.show_replace = !self.find.show_replace;
                            }

                            let query_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.find.query)
                                    .hint_text("查找")
                                    .desired_width(172.0)
                                    .font(style.ui())
                                    .margin(Vec2::new(8.0, 4.0)),
                            );
                            if self.find.focus_query {
                                query_resp.request_focus();
                                self.find.focus_query = false;
                            }
                            if query_resp.changed() {
                                self.find.history_pos = None;
                                self.recompute_find();
                            }
                            if self.find.error.is_some() {
                                // Invalid regex gets a red ring instead of a silent no-op.
                                ui.painter().rect_stroke(
                                    query_resp.rect.expand(1.0),
                                    CornerRadius::same(4),
                                    Stroke::new(1.0, theme.error),
                                    StrokeKind::Inside,
                                );
                            }
                            // `has_focus` mutates egui state; calling it inside
                            // `ui.input` would take the context lock twice and deadlock.
                            let query_focused = query_resp.has_focus();
                            let history_step = ui.input(|i| {
                                if !query_focused {
                                    return None;
                                }
                                if i.key_pressed(Key::ArrowUp) {
                                    Some(false)
                                } else if i.key_pressed(Key::ArrowDown) {
                                    Some(true)
                                } else {
                                    None
                                }
                            });
                            if let Some(newer) = history_step {
                                self.find_history_step(newer);
                            }
                            let enter =
                                query_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if enter {
                                self.remember_find_query();
                                let back = ui.input(|i| i.modifiers.shift);
                                self.find_next(!back);
                                // Hand the keyboard back to the canvas so F3 keeps working.
                                self.focus_requested = true;
                            }

                            if find_icon_button(
                                ui,
                                &theme,
                                Icon::CaseSensitive,
                                self.find.case_sensitive,
                                "区分大小写 (Aa)",
                            ) {
                                self.find.case_sensitive = !self.find.case_sensitive;
                                self.recompute_find();
                            }
                            if find_icon_button(
                                ui,
                                &theme,
                                Icon::WholeWord,
                                self.find.whole_word,
                                "全词匹配 (W)",
                            ) {
                                self.find.whole_word = !self.find.whole_word;
                                self.recompute_find();
                            }
                            if find_icon_button(
                                ui,
                                &theme,
                                Icon::Regex,
                                self.find.regex,
                                "使用正则表达式 (.*)",
                            ) {
                                self.find.regex = !self.find.regex;
                                self.recompute_find();
                            }
                            if find_icon_button(
                                ui,
                                &theme,
                                Icon::Filter,
                                self.find.in_selection,
                                "仅在选区内查找",
                            ) {
                                self.find.in_selection = !self.find.in_selection;
                                self.find.scope = if self.find.in_selection {
                                    self.selection()
                                } else {
                                    None
                                };
                                self.recompute_find();
                            }

                            let count = if self.find.error.is_some() {
                                "无效".to_string()
                            } else if self.find.matches.is_empty() {
                                "无结果".to_string()
                            } else {
                                format!(
                                    "{}/{}",
                                    self.find.active.map(|a| a + 1).unwrap_or(0),
                                    self.find.matches.len()
                                )
                            };
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(count)
                                    .font(FontId::new(11.0, style.ui_family.clone()))
                                    .color(if self.find.error.is_some() {
                                        theme.error
                                    } else {
                                        theme.text_dim
                                    }),
                            );
                            ui.add_space(2.0);

                            if find_icon_button(ui, &theme, Icon::ArrowUp, false, "上一个匹配 (Shift+F3)") {
                                self.find_next(false);
                            }
                            if find_icon_button(ui, &theme, Icon::ArrowDown, false, "下一个匹配 (F3)") {
                                self.find_next(true);
                            }
                            if find_icon_button(ui, &theme, Icon::Close, false, "关闭 (Esc)") {
                                close = true;
                            }
                        });

                        if self.find.show_replace {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.add_space(26.0); // align with query input
                                let replace_resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.find.replacement)
                                        .hint_text("替换为")
                                        .desired_width(172.0)
                                        .font(style.ui())
                                        .margin(Vec2::new(8.0, 4.0)),
                                );
                                if self.find.focus_replacement {
                                    replace_resp.request_focus();
                                    self.find.focus_replacement = false;
                                }
                                if find_icon_button(ui, &theme, Icon::Replace, false, "替换当前匹配") {
                                    self.remember_find_query();
                                    self.replace_current();
                                }
                                if find_icon_button(ui, &theme, Icon::ReplaceAll, false, "全部替换") {
                                    self.remember_find_query();
                                    self.replace_all();
                                }
                                if let Some(err) = &self.find.error {
                                    ui.label(
                                        egui::RichText::new(err)
                                            .font(FontId::new(11.0, style.ui_family.clone()))
                                            .color(theme.error),
                                    );
                                }
                            });
                        }
                    });
            });
        if close || ui.input(|i| i.key_pressed(Key::Escape) && self.find.open) {
            self.close_find();
        }
    }
}

fn find_icon_button(
    ui: &mut Ui,
    theme: &crate::theme::Theme,
    icon: Icon,
    active: bool,
    tip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if active {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(crate::theme::R_SM),
            theme.accent_subtle,
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(crate::theme::R_SM),
            Stroke::new(1.0, theme.accent),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(crate::theme::R_SM), theme.hover);
    }
    let color = if active {
        theme.accent
    } else if resp.hovered() {
        theme.text
    } else {
        theme.text_dim
    };
    icons::paint(ui.painter(), rect.shrink(4.0), icon, color, 1.3);
    resp.on_hover_text(tip).clicked()
}

fn minimap_slider_span(
    content_h: f32,
    row_count: usize,
    line_h: f32,
    view_h: f32,
    scroll_y: f32,
) -> (f32, f32) {
    let content_h = content_h.max(1.0);
    let row_count = row_count.max(1);
    let line_h = line_h.max(1.0);
    let total_doc_h = (row_count as f32 * line_h).max(1.0);
    let visible_ratio = (view_h.max(1.0) / (total_doc_h + PAD_TOP * 2.0)).clamp(0.0, 1.0);
    let min_h = 12.0_f32.min(content_h);
    let vh = (visible_ratio * content_h).clamp(min_h, content_h);
    let first_line_f = ((scroll_y - PAD_TOP) / line_h).max(0.0);
    let top_ratio = (first_line_f / row_count as f32).clamp(0.0, 1.0);
    let vy = (top_ratio * content_h).clamp(0.0, (content_h - vh).max(0.0));
    (vy, vh)
}

fn paint_squiggly_line(painter: &egui::Painter, x1: f32, x2: f32, y: f32, color: Color32) {
    let mut pts = Vec::new();
    let step = 3.5;
    let mut x = x1;
    let mut up = true;
    while x <= x2 {
        pts.push(Pos2::new(x, if up { y - 1.2 } else { y + 1.2 }));
        x += step;
        up = !up;
    }
    if pts.len() >= 2 {
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::minimap_slider_span;

    #[test]
    fn box_selection_places_one_cursor_per_line() {
        let mut ed = crate::editor::Editor::new(
            crate::doc::Document::from_text("abc\ndef\nghi"),
            1,
            0,
        );
        // Drag from line 0 column 1 to line 2 column 1: one caret per line.
        ed.update_box_selection(1, 9);
        assert_eq!(ed.extra_cursors.len(), 2);
        assert_eq!(ed.doc.char_to_line_col(ed.caret), (2, 1));
        assert_eq!(ed.doc.char_to_line_col(ed.extra_cursors[0].0), (0, 1));
        assert_eq!(ed.doc.char_to_line_col(ed.extra_cursors[1].0), (1, 1));
    }

    #[test]
    fn minimap_slider_scales_to_rendered_content_height() {
        // 74 lines * 3.2px = 236.8px rendered minimap even when panel is 780px tall.
        let content_h = 74.0 * 3.2;
        let (vy, vh) = minimap_slider_span(content_h, 74, 21.0, 780.0, 231.0);
        assert!(vy > 25.0 && vy < 45.0, "unexpected vy={vy}");
        assert!(vh > 110.0 && vh < 130.0, "unexpected vh={vh}");
        assert!(vy + vh <= content_h + 0.01);
    }

    #[test]
    fn minimap_slider_fills_short_file_and_clamps_bottom() {
        let content_h = 20.0 * 3.2;
        let (vy, vh) = minimap_slider_span(content_h, 20, 21.0, 780.0, 0.0);
        assert_eq!(vy, 0.0);
        assert!((vh - content_h).abs() < 0.01);

        let long_h = 700.0;
        let (vy_end, vh_end) = minimap_slider_span(long_h, 500, 21.0, 700.0, 20_000.0);
        assert!((vy_end + vh_end - long_h).abs() < 0.01);
    }
}
