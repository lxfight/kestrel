//! Keyboard handling for the editor canvas.

use egui::{Event, ImeEvent, Key, Modifiers};

use super::{Editor, EditorStyle};

impl Editor {
    /// Handle all pending keyboard/IME events. Called only while the canvas owns focus.
    pub fn handle_input(&mut self, ui: &egui::Ui, _style: &EditorStyle, rows: usize) {
        let ctx = ui.ctx().clone();
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::Text(text) => {
                    // Completion must not react to text delivered while a composition
                    // is active; the IME owns the keystrokes until it commits.
                    let composing = !self.ime_preedit.is_empty();
                    self.ime_preedit.clear();
                    self.ime_active_range = None;
                    self.insert_text(&text);
                    if composing {
                        self.completion.open = false;
                    }
                }
                Event::Paste(text) => {
                    self.ime_preedit.clear();
                    self.ime_active_range = None;
                    self.paste_text(&text);
                }
                Event::Copy => {
                    self.copy_selection(&ctx);
                }
                Event::Cut => {
                    self.ime_preedit.clear();
                    self.copy_selection(&ctx);
                    self.delete_selection();
                }
                Event::Ime(ImeEvent::Preedit {
                    text,
                    active_range_chars,
                }) => {
                    if text.is_empty() {
                        self.ime_preedit.clear();
                        self.ime_active_range = None;
                    } else {
                        self.ime_preedit = text;
                        self.ime_active_range =
                            active_range_chars.map(|range| (range.start, range.end));
                    }
                }
                Event::Ime(ImeEvent::Commit(text)) => {
                    self.ime_preedit.clear();
                    self.ime_active_range = None;
                    if !text.is_empty() && text.bytes().any(|b| b != 10 && b != 13) {
                        self.insert_text(&text);
                    }
                }
                Event::Ime(ImeEvent::DeleteSurrounding {
                    before_chars,
                    after_chars,
                }) => {
                    self.ime_preedit.clear();
                    self.ime_active_range = None;
                    self.ime_delete_surrounding(before_chars, after_chars);
                }
                Event::Ime(_) => {
                    self.ime_preedit.clear();
                    self.ime_active_range = None;
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // While an IME composition is active keys belong to the IME,
                    // except Escape: it aborts the composition and still reaches the
                    // editor so it can dismiss completion / find.
                    if !self.ime_preedit.is_empty() {
                        if key != Key::Escape {
                            continue;
                        }
                        self.ime_preedit.clear();
                        self.ime_active_range = None;
                        self.ime_interrupt = true;
                    }
                    self.handle_key(&ctx, key, modifiers, rows);
                }
                _ => {}
            }
        }
    }

    /// Copy every selection, joined by newlines, so multi-cursor paste round-trips.
    pub fn copy_selection(&mut self, ctx: &egui::Context) {
        let mut selections: Vec<(usize, usize)> = Vec::new();
        if let Some(primary) = self.selection() {
            selections.push(primary);
        }
        for &(c, a) in &self.extra_cursors {
            if c != a {
                selections.push((c.min(a), c.max(a)));
            }
        }
        if selections.is_empty() {
            let line = self.doc.char_to_line(self.caret);
            let text = self.doc.line_content(line);
            if !text.is_empty() {
                ctx.copy_text(format!("{text}\n"));
            }
            return;
        }
        selections.sort_unstable();
        let parts: Vec<String> = selections
            .iter()
            .map(|(start, end)| self.doc.text_range(*start, *end))
            .collect();
        ctx.copy_text(parts.join("\n"));
    }

    pub fn delete_selection(&mut self) {
        if self.has_selection() || !self.extra_cursors.is_empty() {
            self.replace_selection("", crate::doc::EditKind::Other, None);
        }
    }

    fn ime_delete_surrounding(&mut self, before_chars: usize, after_chars: usize) {
        if self.doc.readonly {
            return;
        }
        let start = self.caret.saturating_sub(before_chars);
        let end = (self.caret + after_chars).min(self.doc.len_chars());
        if end <= start {
            return;
        }
        let before = (self.caret, self.anchor);
        let edit = self
            .doc
            .edit(start, end, "", crate::doc::EditKind::Other, before, (start, start));
        self.syntax.apply_edit(edit);
        self.caret = start;
        self.anchor = start;
        self.after_edit();
    }

    fn insert_tab(&mut self) {
        let (_, col) = self.doc.char_to_line_col(self.caret);
        let text = if self.insert_spaces {
            let size = self.tab_size.max(1);
            " ".repeat(size - (col % size))
        } else {
            "\t".to_string()
        };
        self.replace_selection(&text, crate::doc::EditKind::Other, None);
    }

    fn handle_key(&mut self, ctx: &egui::Context, key: Key, modifiers: Modifiers, rows: usize) {
        let shift = modifiers.shift;
        let ctrl = modifiers.ctrl || modifiers.command;
        let alt = modifiers.alt;

        // Completion popup navigation takes priority when open.
        if self.completion.open && !self.completion.items.is_empty() && !ctrl && !alt {
            match key {
                Key::ArrowDown => {
                    self.completion.selected =
                        (self.completion.selected + 1) % self.completion.items.len();
                    return;
                }
                Key::ArrowUp => {
                    self.completion.selected =
                        (self.completion.selected + self.completion.items.len() - 1)
                            % self.completion.items.len();
                    return;
                }
                Key::Tab | Key::Enter if !shift => {
                    self.accept_completion();
                    return;
                }
                Key::Escape => {
                    self.completion.open = false;
                    return;
                }
                _ => {}
            }
        }

        match key {
            Key::Escape => {
                if self.completion.open {
                    self.completion.open = false;
                } else if self.find.open {
                    self.close_find();
                } else if !self.extra_cursors.is_empty() {
                    self.clear_extra_cursors();
                } else if self.has_selection() {
                    self.anchor = self.caret;
                }
            }
            Key::Enter => {
                if modifiers.ctrl {
                    return;
                }
                self.newline();
            }
            Key::Tab => {
                if shift {
                    self.indent_selection(true);
                } else if self.has_selection() && self.selected_text().contains('\n') {
                    self.indent_selection(false);
                } else if self.has_selection() {
                    let text = if self.insert_spaces {
                        " ".repeat(self.tab_size.max(1))
                    } else {
                        "\t".to_string()
                    };
                    self.replace_selection(&text, crate::doc::EditKind::Other, None);
                } else {
                    self.insert_tab();
                }
            }
            Key::Backspace => {
                if ctrl {
                    self.delete_word(true);
                } else {
                    self.backspace();
                }
            }
            Key::Delete => {
                if ctrl {
                    self.delete_word(false);
                } else {
                    self.delete_forward();
                }
            }
            // Alt+Shift+←/→ (shrink/expand selection) is dispatched by the app so
            // the chord stays user-rebindable; the canvas only sees plain moves.
            Key::ArrowLeft => self.move_horizontal(false, shift, ctrl),
            Key::ArrowRight => self.move_horizontal(true, shift, ctrl),
            Key::ArrowUp => {
                if ctrl && alt {
                    self.add_cursor_vertical(-1);
                } else if alt {
                    self.move_lines(true);
                } else if ctrl {
                    self.scroll_y = (self.scroll_y - 60.0).max(0.0);
                } else {
                    self.move_vertical(-1, shift);
                }
            }
            Key::ArrowDown => {
                if ctrl && alt {
                    self.add_cursor_vertical(1);
                } else if alt {
                    self.move_lines(false);
                } else if ctrl {
                    self.scroll_y += 60.0;
                } else {
                    self.move_vertical(1, shift);
                }
            }
            Key::Home => {
                if ctrl {
                    self.move_doc(false, shift);
                } else {
                    self.move_home(shift);
                }
            }
            Key::End => {
                if ctrl {
                    self.move_doc(true, shift);
                } else {
                    self.move_end(shift);
                }
            }
            Key::PageUp => self.move_page(-1, shift, rows),
            Key::PageDown => self.move_page(1, shift, rows),
            Key::Space if ctrl => self.trigger_completion(),
            Key::A if ctrl => self.select_all(),
            Key::D if ctrl && !shift => self.select_next_occurrence(),
            Key::D if ctrl && shift => self.duplicate_line(),
            Key::L if ctrl && shift => self.select_all_occurrences(),
            Key::L if ctrl && !shift => self.select_line_at(self.caret),
            Key::Z if ctrl => {
                if shift {
                    self.redo();
                } else {
                    self.undo();
                }
            }
            Key::Y if ctrl => self.redo(),
            Key::K if ctrl && shift => self.delete_line(),
            Key::Slash if ctrl => self.toggle_comment(),
            Key::OpenBracket if ctrl && shift => self.toggle_fold_caret(),
            Key::Backslash if ctrl && shift => {
                self.jump_to_matching_bracket();
            }
            Key::F if ctrl && !shift => {
                self.open_find(false);
            }
            Key::H if ctrl && !shift => {
                self.open_find(true);
            }
            Key::C if ctrl => self.copy_selection(ctx),
            Key::X if ctrl => {
                self.copy_selection(ctx);
                self.delete_selection();
            }
            Key::V if ctrl => {
                if let Ok(mut clipboard) = arboard::Clipboard::new()
                    && let Ok(text) = clipboard.get_text()
                    && !text.is_empty()
                {
                    self.paste_text(&text);
                }
            }
            Key::F3 => self.find_next(!shift),
            _ => {}
        }
    }
}
