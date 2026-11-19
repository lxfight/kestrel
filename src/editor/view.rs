//! Painting, pointer handling, minimap, sticky scroll, completion popup, and find widget for the editor canvas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use super::{Editor, EditorEvent, EditorStyle};
use crate::git::LineDiffKind;
use crate::highlight::{Span, TokenKind};
use crate::icons::{self, Icon};
use crate::lsp::DiagSeverity;

const PAD_TOP: f32 = 6.0;
const SCROLLBAR_W: f32 = 11.0;
const MINIMAP_W: f32 = 88.0;

impl Editor {
    /// Fractional line number at the top of the viewport.
    pub fn top_visible_line(&self, line_h: f32) -> f32 {
        ((self.scroll_y - PAD_TOP) / line_h.max(1.0)).max(0.0)
    }

    /// Scroll so that `line` sits at the top of the viewport.
    pub fn scroll_to_line(&mut self, line: f32, line_h: f32) {
        self.scroll_y = (line.max(0.0) * line_h + PAD_TOP).max(0.0);
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &EditorStyle) -> Option<EditorEvent> {
        self.tab_size = style.tab_size;
        self.insert_spaces = style.insert_spaces;
        self.autocomplete_enabled = style.autocomplete;
        let mut event = None;

        let available = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(available.x.max(1.0), available.y.max(1.0)),
            Sense::click_and_drag(),
        );
        let focused = response.has_focus();
        self.has_focus = focused;
        if !focused {
            self.ime_preedit.clear();
            self.ime_active_range = None;
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
        let total_h = line_count as f32 * line_h;

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
        let view_h = rect.height();
        let rows = ((view_h / line_h).ceil() as usize + 1).max(2);

        self.refresh_syntax(Instant::now());
        self.sync_cache(ui, style, text_width);

        if style.word_wrap {
            self.scroll_x = 0.0;
        }

        // ---- scrolling ----
        let max_scroll = (total_h + PAD_TOP * 2.0 - view_h).max(0.0)
            + if style.scroll_past_end {
                line_h * 2.0
            } else {
                0.0
            };
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

        // ---- pointer: fold gutter click, focus, click, selection drag, double/triple click ----
        let pointer_pressed = ui.input(|i| i.pointer.primary_pressed());
        let shift_held = ui.input(|i| i.modifiers.shift);
        let alt_held = ui.input(|i| i.modifiers.alt);
        let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

        if pointer_pressed && response.contains_pointer()
            && let Some(pos) = response.interact_pointer_pos() {
                // Check if user clicked the fold chevron column in the gutter.
                let clicked_line = (((pos.y - rect.top() + self.scroll_y - PAD_TOP) / line_h)
                    .floor() as isize)
                    .clamp(0, line_count as isize - 1) as usize;
                if pos.x >= text_left - 18.0
                    && pos.x <= text_left
                    && self.fold_range_for_line(clicked_line).is_some()
                {
                    self.toggle_fold_at(clicked_line);
                } else if pos.x >= text_left {
                    let ch = self.pos_to_char(ui, style, rect, pos, text_left);
                    if alt_held && !shift_held && !ctrl_held {
                        self.toggle_cursor_at(ch);
                    } else if ctrl_held && !shift_held {
                        self.caret = ch;
                        self.anchor = ch;
                        let ident = self.identifier_at(ch);
                        if !ident.is_empty() {
                            event = Some(EditorEvent::GotoDefinition(ident));
                        }
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
                    self.completion.open = false;
                }
            }
        if self.dragging_selection && response.is_pointer_button_down_on()
            && let Some(pos) = response.interact_pointer_pos() {
                self.caret = self.pos_to_char(ui, style, rect, pos, text_left);
                self.ensure_visible = true;
            }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.dragging_selection = false;
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                let ch = self.pos_to_char(ui, style, rect, pos, text_left);
                self.select_word_at(ch);
            }
        if response.triple_clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                let ch = self.pos_to_char(ui, style, rect, pos, text_left);
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

        // ---- caret visibility ----
        let caret_line = self.doc.char_to_line(self.caret);
        let care_y = caret_line as f32 * line_h - self.scroll_y + PAD_TOP;
        if self.ensure_visible {
            if care_y < 0.0 {
                self.scroll_y += care_y - 2.0;
            } else if care_y + line_h > view_h {
                self.scroll_y += care_y + line_h - view_h + 2.0;
            }
            if !style.word_wrap {
                let (caret_col, _) = self.doc.char_to_line_col(self.caret);
                let _ = self.line_galley(ui, style, caret_line, text_width);
                let x = match self.galleys.get(&caret_line) {
                    Some(cached) => self.x_of_col_in(cached, caret_col),
                    None => 0.0,
                };
                if x - self.scroll_x < 0.0 {
                    self.scroll_x = (x - 10.0).max(0.0);
                } else if x - self.scroll_x > text_width - 16.0 {
                    self.scroll_x = (x - text_width + 40.0).max(0.0);
                }
            }
            self.ensure_visible = false;
            self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        }

        // ---- scrollbars and minimap interaction ----
        let scrollbar_rect = Rect::from_min_size(
            Pos2::new(rect.right() - SCROLLBAR_W, rect.top()),
            Vec2::new(SCROLLBAR_W, rect.height()),
        );
        if total_h > view_h {
            let sb_id = ui.scope_id().with(("v_scroll", self.id));
            let sb = ui.interact(scrollbar_rect, sb_id, Sense::click_and_drag());
            let thumb_h = ((view_h / total_h) * view_h).clamp(30.0, view_h);
            if (sb.dragged() || sb.clicked())
                && let Some(pos) = sb.interact_pointer_pos() {
                    let travel = (view_h - thumb_h).max(1.0);
                    let t = ((pos.y - rect.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
                    self.scroll_y = t * max_scroll;
                }
            let thumb_y = rect.top() + (self.scroll_y / max_scroll.max(1.0)) * (view_h - thumb_h);
            let thumb = Rect::from_min_size(
                Pos2::new(scrollbar_rect.right() - 8.0, thumb_y),
                Vec2::new(7.0, thumb_h),
            );
            let thumb_color = if sb.hovered() || sb.dragged() {
                style.theme.border_strong
            } else {
                style.theme.border
            };
            ui.painter().rect_filled(thumb, 3.0, thumb_color);
        }

        // ---- paint ----
        self.paint(ui, style, rect, line_h, text_left, text_right, text_width);
        self.paint_completion_popup(ui, style, rect, text_left, line_h);
        self.paint_find_widget(ui, style, rect);

        // Hover tooltip for diagnostics or outline symbols
        if response.hovered()
            && !self.dragging_selection
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            && pos.x >= text_left
            && pos.x <= text_right
        {
            let ch = self.pos_to_char(ui, style, rect, pos, text_left);
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
        h.finish()
    }

    fn sync_cache(&mut self, _ui: &Ui, style: &EditorStyle, text_width: f32) {
        // Clear per-frame galley map so egui's internal GalleyCache (which tracks
        // font-atlas texture resizes) is always authoritative across frames.
        self.galleys.clear();
        let key = (
            self.doc.version,
            Self::style_hash(style),
            text_width,
            self.doc.line_count(),
        );
        self.cache_key = key;
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

    fn x_of_col_in(&self, cached: &super::CachedLine, col: usize) -> f32 {
        let vis_col = match &cached.vis_to_doc {
            Some(map) => map
                .iter()
                .position(|&d| d as usize >= col)
                .unwrap_or(map.len().saturating_sub(1)),
            None => col,
        };
        cached
            .galley
            .pos_from_cursor(egui::text::CCursor::new(vis_col))
            .left()
    }

    fn pos_to_char(
        &mut self,
        ui: &Ui,
        style: &EditorStyle,
        rect: Rect,
        pos: Pos2,
        text_left: f32,
    ) -> usize {
        let line_h = style.line_h.max(1.0);
        let line_count = self.doc.line_count().max(1);
        let line = (((pos.y - rect.top() + self.scroll_y - PAD_TOP) / line_h).floor() as isize)
            .clamp(0, line_count as isize - 1) as usize;
        let x = pos.x - text_left + self.scroll_x;
        let galley = self.line_galley(ui, style, line, 0.0);
        let cached = self.galleys.get(&line).unwrap();
        let vis_col = if x <= 0.0 {
            0
        } else {
            let cc = galley.cursor_from_pos(Vec2::new(x, line_h * 0.5));
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

        let first = (self.scroll_y / line_h).floor().max(0.0) as usize;
        let visible = ((rect.height() / line_h).ceil() as usize + 2).min(self.doc.line_count());
        let last = (first + visible).min(self.doc.line_count());
        let origin = Pos2::new(text_left, rect.top() + PAD_TOP - self.scroll_y);
        let clip = Rect::from_min_max(
            Pos2::new(text_left, rect.top()),
            Pos2::new(text_right, rect.bottom()),
        );
        let tp = painter.with_clip_rect(clip);

        // Current line highlight
        let caret_line = self.doc.char_to_line(self.caret);
        let caret_col = self.doc.char_to_line_col(self.caret).1;
        if caret_line >= first && caret_line < last && !self.is_line_hidden(caret_line) {
            let y = origin.y + caret_line as f32 * line_h;
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(text_left, y), Pos2::new(text_right, y + line_h)),
                0.0,
                theme.current_line,
            );
            if style.show_line_numbers {
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(rect.left(), y), Pos2::new(text_left, y + line_h)),
                    0.0,
                    theme.current_line,
                );
            }
        }

        // Ensure galleys exist for visible lines.
        for line in first..last {
            if !self.is_line_hidden(line) {
                let _ = self.line_galley(ui, style, line, text_width);
            }
        }

        // Indent guides
        if style.show_indent_guides {
            let guide_color = theme.indent_guide;
            let active = theme.indent_guide_active;
            for line in first..last {
                if self.is_line_hidden(line) {
                    continue;
                }
                let cached = match self.galleys.get(&line) {
                    Some(c) => c,
                    None => continue,
                };
                let content = self.doc.line_content(line);
                let mut cols = 0usize;
                for ch in content.chars() {
                    if ch == ' ' {
                        cols += 1;
                    } else if ch == '\t' {
                        cols = (cols / style.tab_size.max(1) + 1) * style.tab_size.max(1);
                    } else {
                        break;
                    }
                }
                let y = origin.y + line as f32 * line_h;
                let mut col = style.tab_size.max(1);
                while col <= cols {
                    let x = origin.x + self.x_of_col_in(cached, col);
                    let color = if line == caret_line && col <= caret_col {
                        active
                    } else {
                        guide_color
                    };
                    painter.line_segment(
                        [Pos2::new(x, y), Pos2::new(x, y + line_h)],
                        Stroke::new(1.0, color),
                    );
                    col += style.tab_size.max(1);
                }
            }
        }

        // Find matches
        if self.find.open && !self.find.matches.is_empty() {
            for (idx, (start, end)) in self.find.matches.iter().enumerate() {
                let sl = self.doc.char_to_line(*start);
                let el = self.doc.char_to_line(*end);
                if el < first || sl >= last {
                    continue;
                }
                let color = if self.find.active == Some(idx) {
                    theme.find_active
                } else {
                    theme.find_match
                };
                for line in sl.max(first)..=el.min(last.saturating_sub(1)) {
                    if self.is_line_hidden(line) {
                        continue;
                    }
                    let Some(cached) = self.galleys.get(&line) else {
                        continue;
                    };
                    let line_start = self.doc.line_to_char(line);
                    let line_len = self.doc.line_char_len(line);
                    let a = start.saturating_sub(line_start).min(line_len);
                    let b = if *end > line_start + line_len {
                        line_len
                    } else {
                        end.saturating_sub(line_start)
                    };
                    let x1 = origin.x + self.x_of_col_in(cached, a) - self.scroll_x;
                    let x2 = if b >= line_len {
                        origin.x + cached.text_width.max((b as f32) * self.char_w) - self.scroll_x
                    } else {
                        origin.x + self.x_of_col_in(cached, b) - self.scroll_x
                    };
                    let y = origin.y + line as f32 * line_h;
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(x1 - 1.0, y), Pos2::new(x2 + 1.0, y + line_h)),
                        2.0,
                        color,
                    );
                }
            }
        }

        // Selections (Primary + Extra Multi-Cursors)
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
        for (sel_start, sel_end) in all_selections {
            let sl = self.doc.char_to_line(sel_start);
            let el = self.doc.char_to_line(sel_end);
            for line in sl.max(first)..=el.min(last.saturating_sub(1)) {
                if self.is_line_hidden(line) {
                    continue;
                }
                let Some(cached) = self.galleys.get(&line) else {
                    continue;
                };
                let line_start = self.doc.line_to_char(line);
                let line_len = self.doc.line_char_len(line);
                let a = sel_start.saturating_sub(line_start).min(line_len);
                let b = if sel_end > line_start + line_len {
                    line_len + 1
                } else {
                    sel_end.saturating_sub(line_start)
                };
                let x1 = origin.x + self.x_of_col_in(cached, a) - self.scroll_x;
                let x2 = if b > line_len {
                    text_right - 4.0
                } else {
                    origin.x + self.x_of_col_in(cached, b) - self.scroll_x
                };
                let y = origin.y + line as f32 * line_h;
                if x2 > x1 || b > line_len {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x1, y),
                            Pos2::new(x2.max(x1 + 2.0), y + line_h),
                        ),
                        2.0,
                        sel_color,
                    );
                }
            }
        }

        // Text + Folded Ellipsis Badges + Inline Diagnostics
        let default_color = theme.syn_text;
        for line in first..last {
            if self.is_line_hidden(line) {
                continue;
            }
            let cached = match self.galleys.get(&line) {
                Some(c) => c,
                None => continue,
            };
            let y = origin.y + line as f32 * line_h;
            let pos = Pos2::new(origin.x - self.scroll_x, y);
            tp.galley(pos, cached.galley.clone(), default_color);

            // Folded badge `⋯` if this line starts a folded block
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
            if diag.line < first || diag.line >= last || self.is_line_hidden(diag.line) {
                continue;
            }
            if let Some(cached) = self.galleys.get(&diag.line) {
                let x1 = origin.x + self.x_of_col_in(cached, diag.col) - self.scroll_x;
                let x2 = (origin.x + self.x_of_col_in(cached, diag.end_col) - self.scroll_x)
                    .max(x1 + self.char_w * 2.0);
                let y = origin.y + (diag.line as f32 + 1.0) * line_h - 2.0;
                let color = match diag.severity {
                    DiagSeverity::Error => theme.error,
                    DiagSeverity::Warning => theme.warning,
                    DiagSeverity::Info => theme.accent,
                };
                paint_squiggly_line(&tp, x1, x2, y, color);
            }
        }

        // Whitespace markers
        if style.show_whitespace {
            let dot = theme.text_faint.gamma_multiply(0.6);
            for line in first..last {
                if self.is_line_hidden(line) {
                    continue;
                }
                let cached = match self.galleys.get(&line) {
                    Some(c) => c,
                    None => continue,
                };
                let content = self.doc.line_content(line);
                let y = origin.y + line as f32 * line_h + line_h * 0.5;
                let mut col = 0usize;
                for ch in content.chars() {
                    if ch == ' ' {
                        let x = origin.x + self.x_of_col_in(cached, col) - self.scroll_x
                            + self.char_w * 0.5;
                        painter.circle_filled(Pos2::new(x, y), 0.9, dot);
                    } else if ch == '\t' {
                        let x = origin.x + self.x_of_col_in(cached, col) - self.scroll_x + 2.0;
                        painter.line_segment(
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

            for line in first..last {
                if self.is_line_hidden(line) {
                    continue;
                }
                let y = origin.y + line as f32 * line_h;
                let color = if line == caret_line {
                    theme.text
                } else {
                    theme.text_faint
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

        // Bracket match
        if self.has_focus
            && let Some((open, close)) = self.matching_brackets()
        {
            for ch_pos in [open, close] {
                let line = self.doc.char_to_line(ch_pos);
                if line < first || line >= last || self.is_line_hidden(line) {
                    continue;
                }
                let col = self.doc.char_to_line_col(ch_pos).1;
                if let Some(cached) = self.galleys.get(&line) {
                    let x = origin.x + self.x_of_col_in(cached, col) - self.scroll_x;
                    let y = origin.y + line as f32 * line_h;
                    painter.rect_stroke(
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

        // Carets (Primary + Extra Multi-Cursors)
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
                    if c_line < first || c_line >= last || self.is_line_hidden(c_line) {
                        continue;
                    }
                    let c_col = self.doc.char_to_line_col(c_pos).1;
                    if let Some(cached) = self.galleys.get(&c_line) {
                        let x = origin.x + self.x_of_col_in(cached, c_col) - self.scroll_x;
                        let y = origin.y + c_line as f32 * line_h;
                        painter.rect_filled(
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

        // IME: keep the OS composition window anchored at the caret and paint
        // the pre-edit string inline while a Chinese/Japanese/Korean IME is
        // composing. Without the output below, egui-winit disables the IME for
        // the whole window and typing falls back to plain ASCII.
        let (caret_line, caret_col) = self.doc.char_to_line_col(self.caret);
        let caret_visible = caret_line >= first
            && caret_line < last
            && !self.is_line_hidden(caret_line);
        let caret_pos = if caret_visible {
            self.galleys.get(&caret_line).map(|cached| {
                Pos2::new(
                    origin.x + self.x_of_col_in(cached, caret_col) - self.scroll_x,
                    origin.y + caret_line as f32 * line_h,
                )
            })
        } else {
            None
        };
        if self.has_focus && !self.doc.readonly {
            // Keep IME enabled for the whole editor even while the caret is
            // scrolled out of view: toggling IMEAllowed resets the OS IME mode.
            let caret = caret_pos.unwrap_or(Pos2::new(text_left, rect.top() + line_h));
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            let cursor_rect = Rect::from_min_size(caret, Vec2::new(2.0, line_h));
            let editor_rect = to_global * rect;
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: editor_rect,
                    cursor_rect: to_global * cursor_rect,
                    should_interrupt_composition: false,
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
                        Pos2::new(text_left, rect.top()),
                        Pos2::new(text_right, rect.bottom()),
                    ))
                    .galley(caret, galley, theme.text);
            }
        }

        // Sticky Scroll header (top of editor canvas)
        if style.show_sticky_scroll && first > 0
            && let Some(enclosing) = self
                .symbols
                .iter().rfind(|s| s.start_line < first && s.end_line > first)
                .cloned()
            {
                let sticky_rect = Rect::from_min_size(
                    Pos2::new(rect.left(), rect.top()),
                    Vec2::new(text_right - rect.left(), line_h + 2.0),
                );
                painter.rect_filled(sticky_rect, 0.0, theme.panel_bg);
                painter.line_segment(
                    [
                        Pos2::new(sticky_rect.left(), sticky_rect.bottom()),
                        Pos2::new(sticky_rect.right(), sticky_rect.bottom()),
                    ],
                    Stroke::new(1.0, theme.border_strong),
                );
                let header_line = self.doc.line_content(enclosing.start_line);
                if style.show_line_numbers {
                    painter.text(
                        Pos2::new(text_left - 18.0, sticky_rect.center().y),
                        Align2::RIGHT_CENTER,
                        (enclosing.start_line + 1).to_string(),
                        FontId::new(style.font_size - 1.0, style.mono_family.clone()),
                        theme.text_faint,
                    );
                }
                painter.with_clip_rect(sticky_rect).text(
                    Pos2::new(text_left, sticky_rect.center().y),
                    Align2::LEFT_CENTER,
                    header_line.trim_end(),
                    style.mono(),
                    theme.text_dim,
                );
            }

        // Minimap
        if style.show_minimap {
            self.paint_minimap(ui, style, rect, text_right, line_h);
        }
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
            [Pos2::new(mm.left(), mm.top()), Pos2::new(mm.left(), mm.bottom())],
            Stroke::new(1.0, theme.border),
        );

        let line_count = self.doc.line_count().max(1);
        let rows = ((mm.height() / 2.2) as usize).clamp(1, 380).min(line_count);
        let step = (line_count as f32 / rows as f32).max(1.0);
        let row_h = (mm.height() / rows as f32).clamp(1.6, 3.2);
        let content_h = (rows as f32 * row_h).min(mm.height()).max(1.0);
        let tab = style.tab_size.max(1);

        let caret_line = self.doc.char_to_line(self.caret);
        let caret_y = mm.top() + (caret_line as f32 / line_count as f32) * content_h;
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(mm.left() + 1.0, caret_y),
                Vec2::new(mm.width() - 2.0, row_h.max(2.0)),
            ),
            0.0,
            theme.accent.gamma_multiply(0.22),
        );

        for i in 0..rows {
            let line = ((i as f32) * step) as usize;
            if line >= line_count {
                break;
            }
            let y = mm.top() + i as f32 * row_h;

            // Git diff marker on minimap left edge
            if let Some(diff) = self.line_diffs.get(line) {
                let diff_col = match diff {
                    LineDiffKind::Added => Some(theme.git_added),
                    LineDiffKind::Modified => Some(theme.accent),
                    LineDiffKind::DeletedBelow => Some(theme.git_deleted),
                    LineDiffKind::Unchanged => None,
                };
                if let Some(c) = diff_col {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(mm.left() + 1.0, y), Vec2::new(2.0, row_h)),
                        0.0,
                        c,
                    );
                }
            }

            let content = self.doc.line_content(line);
            let trimmed = content.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let mut indent_cols = 0usize;
            for ch in content.chars() {
                match ch {
                    ' ' => indent_cols += 1,
                    '\t' => indent_cols = (indent_cols / tab + 1) * tab,
                    _ => break,
                }
            }
            let color = self.minimap_line_color(line, &style.theme);
            let x = mm.left() + 5.0 + (indent_cols as f32 * 0.45).min(mm.width() * 0.42);
            let width = ((trimmed.chars().count() as f32 * 0.45)
                .min(mm.width() - (x - mm.left()) - 4.0))
            .max(1.5);
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(x, y + row_h * 0.15),
                    Vec2::new(width, (row_h * 0.65).max(1.0)),
                ),
                0.5,
                color,
            );
        }

        // Find matches indicator on minimap
        if self.find.open {
            for &(start, _) in self.find.matches.iter().take(200) {
                let m_line = self.doc.char_to_line(start);
                let ratio = m_line as f32 / line_count as f32;
                let my = mm.top() + ratio * content_h;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(mm.right() - 6.0, my), Vec2::new(5.0, 2.5)),
                    1.0,
                    theme.find_active,
                );
            }
        }

        // Viewport slider indicator
        let view_h = rect.height();
        let (slider_offset, vh) =
            minimap_slider_span(content_h, line_count, line_h, view_h, self.scroll_y);
        let vy = mm.top() + slider_offset;
        let vp_rect = Rect::from_min_size(Pos2::new(mm.left() + 1.0, vy), Vec2::new(mm.width() - 2.0, vh));
        painter.rect_filled(vp_rect, 2.0, Color32::from_white_alpha(15));
        painter.rect_stroke(
            vp_rect,
            2.0,
            Stroke::new(1.0, theme.border_strong.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );

        let id = ui.scope_id().with(("minimap", self.id));
        let resp = ui.interact(mm, id, Sense::click_and_drag());
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let total = line_count as f32 * line_h;
            let max_scroll = (total + PAD_TOP * 2.0 - view_h).max(0.0)
                + if style.scroll_past_end {
                    line_h * 2.0
                } else {
                    0.0
                };
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

    fn matching_brackets(&self) -> Option<(usize, usize)> {
        let candidates = [self.caret.checked_sub(1), Some(self.caret)];
        for pos in candidates.into_iter().flatten() {
            let Some(ch) = self.doc.char_at(pos) else {
                continue;
            };
            let (open, close, forward) = match ch {
                '(' => ('(', ')', true),
                '[' => ('[', ']', true),
                '{' => ('{', '}', true),
                ')' => ('(', ')', false),
                ']' => ('[', ']', false),
                '}' => ('{', '}', false),
                _ => continue,
            };
            let other = if forward { close } else { open };
            let mut depth = 0i32;
            let mut p = pos;
            let steps = 20_000;
            for _ in 0..steps {
                if forward {
                    p += 1;
                    if p >= self.doc.len_chars() {
                        break;
                    }
                } else {
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                }
                let Some(c) = self.doc.char_at(p) else {
                    break;
                };
                if c == ch {
                    depth += 1;
                } else if c == other {
                    if depth == 0 {
                        if !self.is_in_string_or_comment(p) {
                            return if forward {
                                Some((pos, p))
                            } else {
                                Some((p, pos))
                            };
                        }
                        break;
                    }
                    depth -= 1;
                }
            }
        }
        None
    }

    fn is_in_string_or_comment(&self, ch_pos: usize) -> bool {
        let byte = self.doc.char_to_byte(ch_pos);
        let idx = self.spans.partition_point(|s| s.end <= byte);
        matches!(
            self.spans.get(idx).map(|s| s.kind),
            Some(TokenKind::String) | Some(TokenKind::Comment)
        )
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
        let x = match self.galleys.get(&caret_line) {
            Some(cached) => text_left + self.x_of_col_in(cached, caret_col) - self.scroll_x,
            None => text_left,
        };
        let y = editor_rect.top() + PAD_TOP + (caret_line as f32 + 1.0) * line_h - self.scroll_y;
        let popup_pos = Pos2::new(
            x.clamp(editor_rect.left() + 8.0, editor_rect.right() - 320.0),
            y.clamp(editor_rect.top() + 20.0, editor_rect.bottom() - 220.0),
        );

        let mut clicked_idx = None;
        egui::Area::new(ui.scope_id().with(("completion_popup", self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(popup_pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme.overlay_bg)
                    .stroke(Stroke::new(1.0, theme.border_strong))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::same(4))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 20,
                        spread: 0,
                        color: theme.shadow,
                    })
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        for (idx, item) in self.completion.items.iter().take(8).enumerate() {
                            let selected = idx == self.completion.selected;
                            let (row_rect, resp) = ui.allocate_exact_size(
                                Vec2::new(292.0, 24.0),
                                Sense::click(),
                            );
                            if selected || resp.hovered() {
                                ui.painter().rect_filled(
                                    row_rect,
                                    CornerRadius::same(4),
                                    if selected { theme.active } else { theme.hover },
                                );
                            }
                            let icon_rect = Rect::from_center_size(
                                Pos2::new(row_rect.left() + 12.0, row_rect.center().y),
                                Vec2::splat(14.0),
                            );
                            icons::paint(
                                ui.painter(),
                                icon_rect,
                                item.kind.icon(),
                                theme.accent,
                                1.2,
                            );
                            ui.painter().text(
                                Pos2::new(row_rect.left() + 26.0, row_rect.center().y),
                                Align2::LEFT_CENTER,
                                &item.label,
                                style.mono(),
                                if selected { theme.text } else { theme.text_dim },
                            );
                            ui.painter().text(
                                Pos2::new(row_rect.right() - 8.0, row_rect.center().y),
                                Align2::RIGHT_CENTER,
                                &item.detail,
                                FontId::new(10.5, style.ui_family.clone()),
                                theme.text_faint,
                            );
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
        let width = 412.0;
        let pos = Pos2::new(
            (editor_rect.right() - width - MINIMAP_W - SCROLLBAR_W - 14.0)
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
                                self.recompute_find();
                            }
                            let enter =
                                query_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if enter {
                                let back = ui.input(|i| i.modifiers.shift);
                                self.find_next(!back);
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
                                    self.replace_current();
                                }
                                if find_icon_button(ui, &theme, Icon::ReplaceAll, false, "全部替换") {
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
            CornerRadius::same(4),
            theme.accent.gamma_multiply(0.22),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, theme.accent),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), theme.hover);
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
    line_count: usize,
    line_h: f32,
    view_h: f32,
    scroll_y: f32,
) -> (f32, f32) {
    let content_h = content_h.max(1.0);
    let line_count = line_count.max(1);
    let line_h = line_h.max(1.0);
    let total_doc_h = (line_count as f32 * line_h).max(1.0);
    let visible_ratio = (view_h.max(1.0) / (total_doc_h + PAD_TOP * 2.0)).clamp(0.0, 1.0);
    let min_h = 12.0_f32.min(content_h);
    let vh = (visible_ratio * content_h).clamp(min_h, content_h);
    let first_line_f = ((scroll_y - PAD_TOP) / line_h).max(0.0);
    let top_ratio = (first_line_f / line_count as f32).clamp(0.0, 1.0);
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
