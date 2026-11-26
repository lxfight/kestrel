//! Terminal grid painter: merged backgrounds (including empty cells), foreground
//! text runs with bold/dim/italic/underline/inverse, seamless box-drawing & block
//! glyphs, OSC 133 command gutter dots, selection/search/link overlays, visual
//! bell edge flash, DECSCUSR cursor styles, and IME preedit preview.

use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Pos2, Rect, Stroke, Ui, Vec2};

use crate::terminal::TerminalSession;
use crate::theme::Theme;
use crate::ui::UiStyle;

#[derive(Clone, Copy)]
pub(crate) struct GridMetrics {
    pub origin: Pos2,
    pub cell_w: f32,
    pub cell_h: f32,
    pub text_h: f32,
    pub y_text_off: f32,
    pub screen_rows: u16,
    pub screen_cols: u16,
}

/// Map a pointer position to a terminal cell, clamped to the visible grid.
pub(crate) fn cell_at(
    pos: Pos2,
    origin: Pos2,
    cell_w: f32,
    cell_h: f32,
    rows: u16,
    cols: u16,
) -> (u16, u16) {
    let col = ((pos.x - origin.x) / cell_w)
        .floor()
        .clamp(0.0, cols.saturating_sub(1) as f32) as u16;
    let row = ((pos.y - origin.y) / cell_h)
        .floor()
        .clamp(0.0, rows.saturating_sub(1) as f32) as u16;
    (row, col)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_terminal_grid(
    ui: &Ui,
    painter: &egui::Painter,
    style: &UiStyle,
    font: &FontId,
    terminal: &TerminalSession,
    m: GridMetrics,
) {
    let theme = style.theme;
    let bold_font = style.term_mono_bold(font.size);
    let screen = terminal.screen();

    for row in 0..m.screen_rows {
        let y = m.origin.y + row as f32 * m.cell_h;

        // Pass 1: Merged background rectangles (handles empty cells with bg / inverse).
        let mut bg_run: Option<(u16, u16, Color32)> = None;
        let mut c = 0u16;
        while c < m.screen_cols {
            let Some(cell) = screen.cell(row, c) else {
                c += 1;
                continue;
            };
            if cell.is_wide_continuation() {
                c += 1;
                continue;
            }
            let span = if cell.is_wide() { 2u16 } else { 1u16 };
            let fg_raw = color_to_ui(cell.fgcolor(), theme, true);
            let bg_raw = color_to_ui(cell.bgcolor(), theme, false);
            let bg = if cell.inverse() { fg_raw } else { bg_raw };
            if bg != theme.terminal_bg {
                if let Some((start, end, cur_bg)) = bg_run
                    && cur_bg == bg
                    && end == c
                {
                    bg_run = Some((start, c + span, cur_bg));
                } else {
                    if let Some((start, end, cur_bg)) = bg_run {
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(m.origin.x + start as f32 * m.cell_w, y),
                                Vec2::new((end - start) as f32 * m.cell_w, m.cell_h),
                            ),
                            0.0,
                            cur_bg,
                        );
                    }
                    bg_run = Some((c, c + span, bg));
                }
            } else if let Some((start, end, cur_bg)) = bg_run.take() {
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(m.origin.x + start as f32 * m.cell_w, y),
                        Vec2::new((end - start) as f32 * m.cell_w, m.cell_h),
                    ),
                    0.0,
                    cur_bg,
                );
            }
            c += span;
        }
        if let Some((start, end, cur_bg)) = bg_run {
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(m.origin.x + start as f32 * m.cell_w, y),
                    Vec2::new((end - start) as f32 * m.cell_w, m.cell_h),
                ),
                0.0,
                cur_bg,
            );
        }

        // Pass 2: Foreground text & seamless geometric box-drawing / block glyphs.
        let mut run_start = 0u16;
        let mut run_text = String::new();
        let mut run_fg = theme.text;
        let mut run_underline = false;
        let mut run_bold = false;
        let mut run_italic = false;

        let flush_run = |text: &mut String,
                         start: u16,
                         fg: Color32,
                         underline: bool,
                         bold: bool,
                         italic: bool| {
            if !text.is_empty() {
                paint_terminal_run(
                    ui,
                    painter,
                    Pos2::new(m.origin.x + start as f32 * m.cell_w, y + m.y_text_off),
                    text,
                    if bold { &bold_font } else { font },
                    fg,
                    underline,
                    italic,
                );
                text.clear();
            }
        };

        let mut col = 0u16;
        while col < m.screen_cols {
            let Some(cell) = screen.cell(row, col) else {
                flush_run(
                    &mut run_text,
                    run_start,
                    run_fg,
                    run_underline,
                    run_bold,
                    run_italic,
                );
                col += 1;
                continue;
            };
            if cell.is_wide_continuation() {
                col += 1;
                continue;
            }
            let span = if cell.is_wide() { 2u16 } else { 1u16 };
            let contents = cell.contents();
            if contents.is_empty() {
                flush_run(
                    &mut run_text,
                    run_start,
                    run_fg,
                    run_underline,
                    run_bold,
                    run_italic,
                );
                col += span;
                continue;
            }

            let fg_raw = color_to_ui(cell.fgcolor(), theme, true);
            let bg_raw = color_to_ui(cell.bgcolor(), theme, false);
            let (mut fg, bg) = if cell.inverse() {
                (bg_raw, fg_raw)
            } else {
                (fg_raw, bg_raw)
            };
            if cell.dim() {
                fg = blend_color(bg, fg, 0.55);
            }
            let bold = cell.bold();
            let italic = cell.italic();
            let underline = cell.underline();

            if let Some(ch) = single_char(contents)
                && paint_box_or_block(
                    painter,
                    Rect::from_min_size(
                        Pos2::new(m.origin.x + col as f32 * m.cell_w, y),
                        Vec2::new(m.cell_w * span as f32, m.cell_h),
                    ),
                    ch,
                    fg,
                )
            {
                flush_run(
                    &mut run_text,
                    run_start,
                    run_fg,
                    run_underline,
                    run_bold,
                    run_italic,
                );
                col += span;
                continue;
            }

            if !contents.is_ascii() || span > 1 {
                flush_run(
                    &mut run_text,
                    run_start,
                    run_fg,
                    run_underline,
                    run_bold,
                    run_italic,
                );
                paint_terminal_run(
                    ui,
                    painter,
                    Pos2::new(m.origin.x + col as f32 * m.cell_w, y + m.y_text_off),
                    contents,
                    if bold { &bold_font } else { font },
                    fg,
                    underline,
                    italic,
                );
                col += span;
                continue;
            }

            if run_text.is_empty() {
                run_start = col;
                run_fg = fg;
                run_underline = underline;
                run_bold = bold;
                run_italic = italic;
            } else if fg != run_fg
                || underline != run_underline
                || bold != run_bold
                || italic != run_italic
            {
                flush_run(
                    &mut run_text,
                    run_start,
                    run_fg,
                    run_underline,
                    run_bold,
                    run_italic,
                );
                run_start = col;
                run_fg = fg;
                run_underline = underline;
                run_bold = bold;
                run_italic = italic;
            }
            run_text.push_str(contents);
            col += span;
        }
        flush_run(
            &mut run_text,
            run_start,
            run_fg,
            run_underline,
            run_bold,
            run_italic,
        );
    }
}

/// Paint OSC 133 / 633 Shell Integration command status dots in the left gutter.
pub(crate) fn paint_command_marks(
    painter: &egui::Painter,
    theme: Theme,
    terminal: &TerminalSession,
    m: GridMetrics,
) {
    if terminal.screen().alternate_screen() || m.screen_rows == 0 {
        return;
    }
    let min_abs = terminal.abs_row_of_visible(0);
    let max_abs = min_abs + usize::from(m.screen_rows);
    let dot_x = m.origin.x - 4.5;
    for mark in terminal.command_marks() {
        if mark.abs_row < min_abs || mark.abs_row >= max_abs {
            continue;
        }
        let vis_row = (mark.abs_row - min_abs) as f32;
        let cy = m.origin.y + vis_row * m.cell_h + m.cell_h * 0.5;
        let color = if !mark.finished {
            theme.accent.gamma_multiply(0.8)
        } else if mark.exit_code.unwrap_or(0) == 0 {
            theme.success
        } else {
            theme.error
        };
        painter.circle_filled(Pos2::new(dot_x, cy), 2.5, color);
    }
}

/// Paint selection wash, search match highlights, Ctrl+hovered link underlines,
/// and subtle visual-bell pulse.
pub(crate) fn paint_overlays(
    ui: &Ui,
    painter: &egui::Painter,
    grid_rect: Rect,
    theme: Theme,
    terminal: &TerminalSession,
    hovered_link_spans: &[(u16, u16, u16)],
    m: GridMetrics,
) {
    let screen = terminal.screen();
    let visible_start = terminal.visible_start_row();
    let text_right = m.origin.x + m.screen_cols as f32 * m.cell_w;

    if let Some(selection) = terminal.selection
        && selection.alt_screen == screen.alternate_screen()
    {
        let wash = Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            68,
        );
        for row in 0..m.screen_rows {
            let abs_row = terminal.abs_row_of_visible(row);
            if let Some((start, end)) = selection.row_span(abs_row, m.screen_cols) {
                let x0 = m.origin.x + start as f32 * m.cell_w;
                let x1 = (m.origin.x + (end as f32 + 1.0) * m.cell_w).min(text_right);
                let y = m.origin.y + row as f32 * m.cell_h;
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + m.cell_h)),
                    0.0,
                    wash,
                );
            }
        }
    }

    if terminal.search.open {
        let active = terminal
            .search
            .active
            .and_then(|index| terminal.search.matches.get(index))
            .copied();
        let hit = Color32::from_rgba_unmultiplied(
            theme.find_match.r(),
            theme.find_match.g(),
            theme.find_match.b(),
            80,
        );
        let hot = Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            125,
        );
        for hit_ref in &terminal.search.matches {
            if hit_ref.row < visible_start
                || hit_ref.row >= visible_start + m.screen_rows as usize
            {
                continue;
            }
            let row = (hit_ref.row - visible_start) as f32;
            let x0 = m.origin.x + hit_ref.start_col as f32 * m.cell_w;
            let x1 = (m.origin.x + (hit_ref.end_col as f32 + 1.0) * m.cell_w).min(text_right);
            let y = m.origin.y + row * m.cell_h;
            let is_active =
                active.is_some_and(|a| a.row == hit_ref.row && a.start_col == hit_ref.start_col);
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + m.cell_h)),
                0.0,
                if is_active { hot } else { hit },
            );
        }
    }

    for &(r, c0, c1) in hovered_link_spans {
        if r < m.screen_rows {
            let y = m.origin.y + r as f32 * m.cell_h + m.y_text_off + m.text_h;
            let x0 = m.origin.x + c0 as f32 * m.cell_w;
            let x1 = (m.origin.x + (c1 as f32 + 1.0) * m.cell_w).min(text_right);
            painter.line_segment(
                [Pos2::new(x0, y), Pos2::new(x1, y)],
                Stroke::new(1.2, theme.accent),
            );
        }
    }

    // Visual bell subtle border pulse.
    let bell_alpha = terminal.bell_flash_alpha();
    if bell_alpha > 0.01 {
        let pulse = Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            (bell_alpha * 160.0) as u8,
        );
        painter.rect_stroke(
            grid_rect.shrink(1.0),
            2.0,
            Stroke::new(2.0, pulse),
            egui::StrokeKind::Inside,
        );
        ui.ctx().request_repaint_after(Duration::from_millis(24));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_cursor_and_ime(
    ui: &mut Ui,
    painter: &egui::Painter,
    grid_rect: Rect,
    theme: Theme,
    font: &FontId,
    terminal: &TerminalSession,
    focused: bool,
    m: GridMetrics,
) {
    let screen = terminal.screen();
    let (crow, ccol_raw) = screen.cursor_position();
    let cursor_in_grid = crow < m.screen_rows && m.screen_cols > 0;
    let ccol = ccol_raw.min(m.screen_cols.saturating_sub(1));
    let cursor_wide = screen.cell(crow, ccol).is_some_and(vt100::Cell::is_wide);
    let cursor_w = if cursor_wide { m.cell_w * 2.0 } else { m.cell_w };
    let cursor_cell = cursor_in_grid.then(|| {
        Rect::from_min_size(
            Pos2::new(
                m.origin.x + ccol as f32 * m.cell_w,
                m.origin.y + crow as f32 * m.cell_h,
            ),
            Vec2::new(cursor_w, m.cell_h),
        )
    });

    if !screen.hide_cursor()
        && !terminal.is_exited()
        && !terminal.is_starting()
        && terminal.failed_error().is_none()
        && terminal.scrollback_offset == 0
        && let Some(cell) = cursor_cell
    {
        let cursor_style = screen.cursor_style();
        let blinks = matches!(
            cursor_style,
            vt100::CursorStyle::Default
                | vt100::CursorStyle::BlinkBlock
                | vt100::CursorStyle::BlinkUnderline
                | vt100::CursorStyle::BlinkVerticalBar
        );
        let elapsed_ms = terminal.last_input_at.elapsed().as_millis();
        let phase_visible = !blinks || elapsed_ms < 500 || ((elapsed_ms / 550) % 2 == 0);
        if focused && blinks {
            let next_ms = if elapsed_ms < 500 {
                (500 - elapsed_ms) as u64
            } else {
                (550 - (elapsed_ms % 550)) as u64
            };
            ui.ctx()
                .request_repaint_after(Duration::from_millis(next_ms.max(40)));
        }
        let r = Rect::from_min_size(
            Pos2::new(cell.left(), cell.top() + m.y_text_off),
            Vec2::new(cell.width(), m.text_h),
        );
        if !focused {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, theme.accent.gamma_multiply(0.45)),
                egui::StrokeKind::Inside,
            );
        } else if phase_visible {
            match cursor_style {
                vt100::CursorStyle::BlinkUnderline | vt100::CursorStyle::SteadyUnderline => {
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(r.left(), r.bottom() - 2.0), r.right_bottom()),
                        0.0,
                        theme.accent,
                    );
                }
                vt100::CursorStyle::BlinkVerticalBar | vt100::CursorStyle::SteadyVerticalBar => {
                    painter.rect_filled(
                        Rect::from_min_size(r.left_top(), Vec2::new(2.0, r.height())),
                        0.0,
                        theme.accent,
                    );
                }
                _ => {
                    painter.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.32));
                    painter.rect_stroke(
                        r,
                        0.0,
                        Stroke::new(1.5, theme.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }

    if focused {
        let cell = cursor_cell
            .unwrap_or_else(|| Rect::from_min_size(m.origin, Vec2::new(m.cell_w, m.cell_h)));
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        ui.output_mut(|output| {
            output.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: to_global * grid_rect,
                cursor_rect: to_global * cell,
                should_interrupt_composition: false,
            });
        });
        if cursor_cell.is_some() && !terminal.ime_preedit.is_empty() {
            let job = LayoutJob::single_section(
                terminal.ime_preedit.clone(),
                TextFormat {
                    font_id: font.clone(),
                    color: theme.text,
                    underline: Stroke::new(1.5, theme.accent),
                    line_height: Some(m.cell_h),
                    ..Default::default()
                },
            );
            let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
            let bg_rect = Rect::from_min_size(
                cell.left_top(),
                Vec2::new(galley.rect.width().max(m.cell_w), m.cell_h),
            );
            painter.rect_filled(bg_rect, 2.0, theme.overlay_bg);
            painter.galley(cell.left_top(), galley, theme.text);
        }
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn blend_color(bg: Color32, fg: Color32, alpha: f32) -> Color32 {
    let inv = 1.0 - alpha;
    Color32::from_rgb(
        (bg.r() as f32 * inv + fg.r() as f32 * alpha).round() as u8,
        (bg.g() as f32 * inv + fg.g() as f32 * alpha).round() as u8,
        (bg.b() as f32 * inv + fg.b() as f32 * alpha).round() as u8,
    )
}

fn paint_box_or_block(painter: &egui::Painter, rect: Rect, ch: char, color: Color32) -> bool {
    let cx = rect.center().x.round() + 0.5;
    let cy = rect.center().y.round() + 0.5;
    let thin = Stroke::new(1.0, color);
    let thick = Stroke::new(2.0, color);

    match ch {
        '█' => {
            painter.rect_filled(rect, 0.0, color);
            return true;
        }
        '▀' => {
            painter.rect_filled(
                Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.center().y)),
                0.0,
                color,
            );
            return true;
        }
        '▄' => {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(rect.left(), rect.center().y), rect.max),
                0.0,
                color,
            );
            return true;
        }
        '▌' => {
            painter.rect_filled(
                Rect::from_min_max(rect.min, Pos2::new(rect.center().x, rect.bottom())),
                0.0,
                color,
            );
            return true;
        }
        '▐' => {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(rect.center().x, rect.top()), rect.max),
                0.0,
                color,
            );
            return true;
        }
        '░' => {
            painter.rect_filled(rect, 0.0, color.gamma_multiply(0.25));
            return true;
        }
        '▒' => {
            painter.rect_filled(rect, 0.0, color.gamma_multiply(0.5));
            return true;
        }
        '▓' => {
            painter.rect_filled(rect, 0.0, color.gamma_multiply(0.75));
            return true;
        }
        '▁'..='▇' => {
            let frac = ((ch as u32 - '▁' as u32) + 1) as f32 / 8.0;
            let h = rect.height() * frac;
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - h), rect.max),
                0.0,
                color,
            );
            return true;
        }
        _ => {}
    }

    let dirs = match ch {
        '─' => Some((false, true, false, true, thin)),
        '━' => Some((false, true, false, true, thick)),
        '│' => Some((true, false, true, false, thin)),
        '┃' => Some((true, false, true, false, thick)),
        '┌' | '╭' => Some((false, true, true, false, thin)),
        '┏' => Some((false, true, true, false, thick)),
        '┐' | '╮' => Some((false, false, true, true, thin)),
        '┓' => Some((false, false, true, true, thick)),
        '└' | '╰' => Some((true, true, false, false, thin)),
        '┗' => Some((true, true, false, false, thick)),
        '┘' | '╯' => Some((true, false, false, true, thin)),
        '┛' => Some((true, false, false, true, thick)),
        '├' => Some((true, true, true, false, thin)),
        '┣' => Some((true, true, true, false, thick)),
        '┤' => Some((true, false, true, true, thin)),
        '┫' => Some((true, false, true, true, thick)),
        '┬' => Some((false, true, true, true, thin)),
        '┳' => Some((false, true, true, true, thick)),
        '┴' => Some((true, true, false, true, thin)),
        '┻' => Some((true, true, false, true, thick)),
        '┼' => Some((true, true, true, true, thin)),
        '╋' => Some((true, true, true, true, thick)),
        '╴' => Some((false, false, false, true, thin)),
        '╵' => Some((true, false, false, false, thin)),
        '╶' => Some((false, true, false, false, thin)),
        '╷' => Some((false, false, true, false, thin)),
        _ => None,
    };
    if let Some((up, right, down, left, stroke)) = dirs {
        let center = Pos2::new(cx, cy);
        if up {
            painter.line_segment([Pos2::new(cx, rect.top()), center], stroke);
        }
        if down {
            painter.line_segment([center, Pos2::new(cx, rect.bottom())], stroke);
        }
        if left {
            painter.line_segment([Pos2::new(rect.left(), cy), center], stroke);
        }
        if right {
            painter.line_segment([center, Pos2::new(rect.right(), cy)], stroke);
        }
        return true;
    }

    false
}

#[allow(clippy::too_many_arguments)]
fn paint_terminal_run(
    ui: &Ui,
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    font: &FontId,
    color: Color32,
    underline: bool,
    italic: bool,
) {
    let job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font.clone(),
            color,
            italics: italic,
            underline: if underline {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        },
    );
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(pos, galley, color);
}

fn color_to_ui(color: vt100::Color, theme: Theme, foreground: bool) -> Color32 {
    let default = if foreground {
        theme.text
    } else {
        theme.terminal_bg
    };
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(idx) => ansi_color(idx, theme.kind.is_dark(), foreground),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn ansi_color(idx: u8, dark: bool, foreground: bool) -> Color32 {
    const DARK: [Color32; 16] = [
        Color32::from_rgb(0x22, 0x24, 0x2B),
        Color32::from_rgb(0xE0, 0x6C, 0x75),
        Color32::from_rgb(0x98, 0xC3, 0x79),
        Color32::from_rgb(0xE5, 0xC0, 0x7B),
        Color32::from_rgb(0x61, 0xAF, 0xEF),
        Color32::from_rgb(0xC6, 0x78, 0xDD),
        Color32::from_rgb(0x56, 0xB6, 0xC2),
        Color32::from_rgb(0xBE, 0xC1, 0xC8),
        Color32::from_rgb(0x5C, 0x63, 0x72),
        Color32::from_rgb(0xE8, 0x8A, 0x92),
        Color32::from_rgb(0xB5, 0xD8, 0x9A),
        Color32::from_rgb(0xF0, 0xD0, 0x95),
        Color32::from_rgb(0x82, 0xBE, 0xF5),
        Color32::from_rgb(0xD8, 0x9C, 0xE8),
        Color32::from_rgb(0x7A, 0xCF, 0xD8),
        Color32::from_rgb(0xEE, 0xEF, 0xF2),
    ];
    const LIGHT: [Color32; 16] = [
        Color32::from_rgb(0x1F, 0x24, 0x2D),
        Color32::from_rgb(0xB3, 0x26, 0x1E),
        Color32::from_rgb(0x1F, 0x6E, 0x2E),
        Color32::from_rgb(0x8A, 0x5A, 0x00),
        Color32::from_rgb(0x1F, 0x5F, 0xBF),
        Color32::from_rgb(0x82, 0x26, 0xA8),
        Color32::from_rgb(0x0B, 0x6E, 0x80),
        Color32::from_rgb(0xD8, 0xDD, 0xE6),
        Color32::from_rgb(0x64, 0x6E, 0x80),
        Color32::from_rgb(0xC6, 0x28, 0x28),
        Color32::from_rgb(0x2E, 0x7D, 0x32),
        Color32::from_rgb(0x9A, 0x54, 0x00),
        Color32::from_rgb(0x19, 0x76, 0xD2),
        Color32::from_rgb(0xAB, 0x47, 0xBC),
        Color32::from_rgb(0x00, 0x83, 0x8F),
        Color32::from_rgb(0xF4, 0xF6, 0xFA),
    ];
    if idx < 16 {
        if !dark && foreground && (idx == 7 || idx == 15) {
            return Color32::from_rgb(0x4A, 0x53, 0x64);
        }
        return if dark {
            DARK[idx as usize]
        } else {
            LIGHT[idx as usize]
        };
    }
    if idx < 232 {
        let i = idx - 16;
        let r = i / 36;
        let g = (i % 36) / 6;
        let b = i % 6;
        let scale = |v: u8| -> u8 {
            if v == 0 {
                0
            } else {
                (55 + v as u16 * 40).min(255) as u8
            }
        };
        return Color32::from_rgb(scale(r), scale(g), scale(b));
    }
    let gray = 8 + (idx - 232) * 10;
    Color32::from_rgb(gray, gray, gray)
}
