//! Interactive Excel (`.xlsx` / `.xlsm`) and CSV/TSV spreadsheet viewer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{
    Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2,
};

use crate::ui::UiStyle;
use crate::viewer::ViewerEvent;
use crate::viewer::image_view::format_bytes;
use crate::viewer::zip_util::ZipArchive;

/// Hard row cap per sheet, so a huge workbook cannot allocate millions of cells.
pub const MAX_SHEET_ROWS: usize = 5000;
/// Hard cap on columns kept per sheet (Excel allows 16384).
pub const MAX_SHEET_COLS: usize = 512;
/// Hard cap on parsed worksheets per workbook.
pub const MAX_SHEETS: usize = 20;

#[derive(Clone, Debug)]
pub struct SheetData {
    pub name: String,
    pub rows: Vec<Vec<CellValue>>,
    pub col_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CellValue {
    pub display: String,
    pub formula: Option<String>,
    pub is_number: bool,
}

pub struct SpreadsheetViewer {
    path: PathBuf,
    file_bytes: u64,
    is_text_tabular: bool,
    pub show_source: bool,
    sheets: Vec<SheetData>,
    active_sheet: usize,
    selected: Option<(usize, usize)>,
    search: String,
    error: Option<String>,
    /// Human-readable truncation notice when the parser hit a cap.
    truncated: Option<String>,
}

impl SpreadsheetViewer {
    pub fn load_xlsx(path: &Path) -> Self {
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(err) => return Self::with_error(path, file_bytes, false, format!("读取失败: {err}")),
        };
        match parse_xlsx_bytes(&bytes) {
            Ok((sheets, truncated)) => Self {
                path: path.to_path_buf(),
                file_bytes,
                is_text_tabular: false,
                show_source: false,
                sheets,
                active_sheet: 0,
                selected: None,
                search: String::new(),
                error: None,
                truncated,
            },
            Err(err) => Self::with_error(path, file_bytes, false, err),
        }
    }

    pub fn load_csv(path: &Path, text: &str, delimiter: char) -> Self {
        let file_bytes = text.len() as u64;
        let rows = parse_delimited(text, delimiter);
        let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Sheet1".to_string());
        Self {
            path: path.to_path_buf(),
            file_bytes,
            is_text_tabular: true,
            show_source: false,
            sheets: vec![SheetData {
                name,
                rows,
                col_count,
            }],
            active_sheet: 0,
            selected: None,
            search: String::new(),
            error: None,
            truncated: None,
        }
    }

    pub fn refresh_csv(&mut self, text: &str, delimiter: char) {
        self.file_bytes = text.len() as u64;
        let rows = parse_delimited(text, delimiter);
        let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        if let Some(sheet) = self.sheets.get_mut(0) {
            sheet.rows = rows;
            sheet.col_count = col_count;
        }
    }

    fn with_error(path: &Path, file_bytes: u64, is_text_tabular: bool, error: String) -> Self {
        Self {
            path: path.to_path_buf(),
            file_bytes,
            is_text_tabular,
            show_source: false,
            sheets: Vec::new(),
            active_sheet: 0,
            selected: None,
            search: String::new(),
            error: Some(error),
            truncated: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        let mut events = Vec::new();
        let theme = style.theme;
        let full_rect = ui.max_rect();
        ui.painter().rect_filled(full_rect, 0.0, theme.bg);

        // Top toolbar
        let bar_h = 34.0;
        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), bar_h), Sense::hover());
        ui.painter().rect_filled(bar_rect, 0.0, theme.panel_bg);
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.left(), bar_rect.bottom() - 0.5),
                Pos2::new(bar_rect.right(), bar_rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        let sheet_info = self
            .sheets
            .get(self.active_sheet)
            .map(|s| format!("{} 行 × {} 列", s.rows.len(), s.col_count))
            .unwrap_or_default();
        let truncation = self
            .truncated
            .as_ref()
            .map(|note| format!(" · {note}"))
            .unwrap_or_default();
        ui.painter().text(
            Pos2::new(bar_rect.left() + 14.0, bar_rect.center().y),
            Align2::LEFT_CENTER,
            format!(
                "表格视图 · {} · {}{}",
                sheet_info,
                format_bytes(self.file_bytes),
                truncation
            ),
            style.tiny(),
            if self.truncated.is_some() {
                theme.warning
            } else {
                theme.text_dim
            },
        );

        let mut rx = bar_rect.right() - 10.0;
        let mut top_btn = |label: &str, w: f32| -> bool {
            rx -= w + 4.0;
            let r = Rect::from_center_size(
                Pos2::new(rx + w * 0.5, bar_rect.center().y),
                Vec2::new(w, 22.0),
            );
            let resp = ui.interact(r, ui.scope_id().with(("xlsx_btn", label)), Sense::click());
            ui.painter().rect_filled(
                r,
                CornerRadius::same(4),
                if resp.hovered() {
                    theme.hover
                } else {
                    theme.input_bg
                },
            );
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(1.0, theme.border),
                StrokeKind::Inside,
            );
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                label,
                style.tiny(),
                theme.text_dim,
            );
            resp.clicked()
        };

        if self.is_text_tabular && top_btn("编辑源码", 62.0) {
            self.show_source = true;
        }
        if top_btn("复制 CSV", 62.0)
            && let Some(sheet) = self.sheets.get(self.active_sheet)
        {
            events.push(ViewerEvent::CopyText(sheet_to_csv(sheet)));
        }
        if self.selected.is_some()
            && top_btn("复制单元格", 72.0)
            && let Some((sr, sc)) = self.selected
            && let Some(cell) = self
                .sheets
                .get(self.active_sheet)
                .and_then(|s| s.rows.get(sr))
                .and_then(|r| r.get(sc))
        {
            events.push(ViewerEvent::CopyText(cell.display.clone()));
        }

        rx -= 170.0;
        let search_r = Rect::from_center_size(
            Pos2::new(rx + 82.0, bar_rect.center().y),
            Vec2::new(164.0, 22.0),
        );
        ui.put(
            search_r,
            TextEdit::singleline(&mut self.search)
                .hint_text("筛选/高亮单元格…")
                .font(style.tiny())
                .margin(Vec2::new(8.0, 3.0)),
        );

        if let Some(err) = &self.error {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(err)
                        .font(style.ui())
                        .color(theme.error),
                );
            });
            return events;
        }

        // Formula / selected cell bar
        let formula_h = 26.0;
        let (f_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), formula_h), Sense::hover());
        ui.painter().rect_filled(f_rect, 0.0, theme.sidebar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(f_rect.left(), f_rect.bottom() - 0.5),
                Pos2::new(f_rect.right(), f_rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );
        if let Some((sr, sc)) = self.selected {
            let coord = format!("{}{}", col_name(sc), sr + 1);
            let cell_info = self
                .sheets
                .get(self.active_sheet)
                .and_then(|s| s.rows.get(sr))
                .and_then(|r| r.get(sc))
                .map(|c| {
                    if let Some(f) = &c.formula {
                        format!("{coord}:  ={f}   ({})", c.display)
                    } else {
                        format!("{coord}:  {}", c.display)
                    }
                })
                .unwrap_or_else(|| format!("{coord}:  (空)"));
            ui.painter().with_clip_rect(f_rect).text(
                Pos2::new(f_rect.left() + 14.0, f_rect.center().y),
                Align2::LEFT_CENTER,
                &cell_info,
                style.small(),
                theme.text,
            );
        } else {
            ui.painter().text(
                Pos2::new(f_rect.left() + 14.0, f_rect.center().y),
                Align2::LEFT_CENTER,
                "点击任意单元格查看完整内容或公式",
                style.tiny(),
                theme.text_faint,
            );
        }

        let sheet_bar_h = if self.sheets.len() > 1 { 30.0 } else { 0.0 };
        let grid_h = (ui.available_height() - sheet_bar_h).max(80.0);

        let needle = self.search.trim().to_lowercase();
        let mut new_selection = self.selected;

        if let Some(sheet) = self.sheets.get(self.active_sheet) {
            let row_h = 25.0;
            let row_num_w = 54.0;
            let col_w = 136.0;
            let total_rows = sheet.rows.len().max(1);
            let total_cols = sheet.col_count.max(1);
            let total_w = row_num_w + total_cols as f32 * col_w;
            let total_h = (total_rows + 1) as f32 * row_h;

            ScrollArea::both()
                .id_salt(("xlsx_grid", &self.path, self.active_sheet))
                .max_height(grid_h)
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    let (grid_rect, _) =
                        ui.allocate_exact_size(Vec2::new(total_w, total_h), Sense::hover());
                    let painter = ui.painter_at(grid_rect);
                    // 冻结表头：表头钉在可视区顶端，数据区裁剪到表头下沿，最后再画表头压在最上层。
                    let clip = painter.clip_rect();
                    let header_rect = Rect::from_min_size(
                        Pos2::new(clip.left(), clip.top()),
                        Vec2::new(clip.width(), row_h),
                    );
                    let data_painter = painter.with_clip_rect(Rect::from_min_max(
                        Pos2::new(clip.left(), header_rect.bottom()),
                        clip.max,
                    ));

                    let first_row = ((viewport.top() / row_h).floor() as isize - 1).max(0) as usize;
                    let last_row =
                        ((viewport.bottom() / row_h).ceil() as usize + 1).min(total_rows);
                    let first_col = (((viewport.left() - row_num_w) / col_w).floor() as isize)
                        .max(0) as usize;
                    let last_col = (((viewport.right() - row_num_w) / col_w).ceil() as usize + 1)
                        .min(total_cols);

                    // Visible data rows
                    for r in first_row..last_row {
                        let y = grid_rect.top() + (r + 1) as f32 * row_h;
                        let row_bg = if r % 2 == 1 {
                            theme.panel_bg.gamma_multiply(0.35)
                        } else {
                            theme.bg
                        };

                        // Row number cell
                        let rn_rect = Rect::from_min_size(
                            Pos2::new(grid_rect.left(), y),
                            Vec2::new(row_num_w, row_h),
                        );
                        data_painter.rect_filled(rn_rect, 0.0, theme.panel_bg);
                        data_painter.rect_stroke(
                            rn_rect,
                            0.0,
                            Stroke::new(1.0, theme.border),
                            StrokeKind::Inside,
                        );
                        data_painter.text(
                            rn_rect.center(),
                            Align2::CENTER_CENTER,
                            (r + 1).to_string(),
                            style.tiny(),
                            theme.text_faint,
                        );

                        let row_cells = sheet.rows.get(r);
                        for c in first_col..last_col {
                            let x = grid_rect.left() + row_num_w + c as f32 * col_w;
                            let cell_rect =
                                Rect::from_min_size(Pos2::new(x, y), Vec2::new(col_w, row_h));
                            let cell = row_cells.and_then(|rc| rc.get(c));
                            let val = cell.map(|v| v.display.as_str()).unwrap_or("");
                            let is_num = cell.is_some_and(|v| v.is_number);
                            let is_sel = new_selection == Some((r, c));
                            let is_match =
                                !needle.is_empty() && val.to_lowercase().contains(&needle);

                            let bg = if is_sel {
                                theme.selection
                            } else if is_match {
                                theme.find_match
                            } else {
                                row_bg
                            };
                            data_painter.rect_filled(cell_rect, 0.0, bg);
                            data_painter.rect_stroke(
                                cell_rect,
                                0.0,
                                Stroke::new(
                                    if is_sel { 1.5 } else { 1.0 },
                                    if is_sel { theme.accent } else { theme.border },
                                ),
                                StrokeKind::Inside,
                            );

                            if !val.is_empty() {
                                let text_painter =
                                    data_painter.with_clip_rect(cell_rect.shrink2(Vec2::new(6.0, 2.0)));
                                if is_num {
                                    text_painter.text(
                                        Pos2::new(cell_rect.right() - 8.0, cell_rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        val,
                                        style.small(),
                                        theme.syn_number,
                                    );
                                } else {
                                    text_painter.text(
                                        Pos2::new(cell_rect.left() + 8.0, cell_rect.center().y),
                                        Align2::LEFT_CENTER,
                                        val,
                                        style.small(),
                                        theme.text,
                                    );
                                }
                            }

                            let resp = ui.interact(
                                cell_rect.intersect(data_painter.clip_rect()),
                                ui.scope_id().with(("cell", r, c)),
                                Sense::click(),
                            );
                            if resp.clicked() {
                                new_selection = Some((r, c));
                            }
                        }
                    }

                    // 表头最后绘制，始终压在数据行之上。
                    painter.rect_filled(header_rect, 0.0, theme.panel_bg);
                    for c in first_col..last_col {
                        let x = grid_rect.left() + row_num_w + c as f32 * col_w;
                        let cr = Rect::from_min_size(
                            Pos2::new(x, header_rect.top()),
                            Vec2::new(col_w, row_h),
                        );
                        painter.rect_stroke(
                            cr,
                            0.0,
                            Stroke::new(1.0, theme.border),
                            StrokeKind::Inside,
                        );
                        painter.text(
                            cr.center(),
                            Align2::CENTER_CENTER,
                            col_name(c),
                            FontId::new(11.5, style.ui_bold_family.clone()),
                            theme.text_dim,
                        );
                    }
                });
        }
        self.selected = new_selection;

        // Bottom Sheet Tabs
        if self.sheets.len() > 1 {
            let (sb_rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), sheet_bar_h), Sense::hover());
            ui.painter().rect_filled(sb_rect, 0.0, theme.panel_bg);
            ui.painter().line_segment(
                [
                    Pos2::new(sb_rect.left(), sb_rect.top() + 0.5),
                    Pos2::new(sb_rect.right(), sb_rect.top() + 0.5),
                ],
                Stroke::new(1.0, theme.border),
            );
            let mut x = sb_rect.left() + 8.0;
            for (idx, sheet) in self.sheets.iter().enumerate() {
                let w = (sheet.name.chars().count() as f32 * 9.5 + 24.0).clamp(64.0, 180.0);
                let r = Rect::from_min_size(
                    Pos2::new(x, sb_rect.top() + 4.0),
                    Vec2::new(w, sheet_bar_h - 8.0),
                );
                let active = idx == self.active_sheet;
                let resp = ui.interact(r, ui.scope_id().with(("sheet_tab", idx)), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(4),
                    if active {
                        theme.accent
                    } else if resp.hovered() {
                        theme.hover
                    } else {
                        theme.input_bg
                    },
                );
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    &sheet.name,
                    style.tiny(),
                    if active {
                        theme.accent_text
                    } else {
                        theme.text_dim
                    },
                );
                if resp.clicked() {
                    self.active_sheet = idx;
                    self.selected = None;
                }
                x += w + 6.0;
            }
        }

        events
    }
}

pub fn col_name(mut col: usize) -> String {
    let mut name = String::new();
    loop {
        let rem = col % 26;
        name.insert(0, (b'A' + rem as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name
}

fn parse_cell_ref(r: &str) -> Option<(usize, usize)> {
    let split = r.bytes().position(|b| b.is_ascii_digit())?;
    let col_str = &r[..split];
    let row_str = &r[split..];
    if col_str.is_empty() || row_str.is_empty() {
        return None;
    }
    let mut col = 0usize;
    for b in col_str.bytes() {
        let up = b.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        col = col * 26 + (up - b'A') as usize + 1;
    }
    let row: usize = row_str.parse().ok()?;
    Some((row.saturating_sub(1), col.saturating_sub(1)))
}

fn parse_xlsx_bytes(bytes: &[u8]) -> Result<(Vec<SheetData>, Option<String>), String> {
    let archive = ZipArchive::from_bytes(bytes.to_vec())?;

    // Shared strings
    let mut shared_strings = Vec::new();
    if let Ok(ss_xml) = archive.read_entry_string("xl/sharedStrings.xml")
        && let Ok(doc) = roxmltree::Document::parse(&ss_xml)
    {
        for si in doc.descendants().filter(|n| n.tag_name().name() == "si") {
            let mut text = String::new();
            for t in si.descendants().filter(|n| n.tag_name().name() == "t") {
                if let Some(s) = t.text() {
                    text.push_str(s);
                }
            }
            shared_strings.push(text);
        }
    }

    // Workbook relationships
    let mut rels: HashMap<String, String> = HashMap::new();
    if let Ok(rels_xml) = archive.read_entry_string("xl/_rels/workbook.xml.rels")
        && let Ok(doc) = roxmltree::Document::parse(&rels_xml)
    {
        for node in doc.descendants().filter(|n| n.tag_name().name() == "Relationship") {
            if let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target")) {
                rels.insert(id.to_string(), target.to_string());
            }
        }
    }

    // Sheet list from workbook.xml
    let mut sheet_entries: Vec<(String, String)> = Vec::new();
    if let Ok(wb_xml) = archive.read_entry_string("xl/workbook.xml")
        && let Ok(doc) = roxmltree::Document::parse(&wb_xml)
    {
        for node in doc.descendants().filter(|n| n.tag_name().name() == "sheet") {
            let name = node.attribute("name").unwrap_or("Sheet").to_string();
            let rid = node
                .attributes()
                .find(|a| a.name() == "id")
                .map(|a| a.value());
            if let Some(id) = rid
                && let Some(target) = rels.get(id)
            {
                // Targets are absolute (`/xl/worksheets/sheet1.xml`, openpyxl) or
                // package-relative (`worksheets/sheet1.xml`, Excel).
                let target = target.trim_start_matches('/');
                let path = if target.starts_with("xl/") {
                    target.to_string()
                } else {
                    format!("xl/{target}")
                };
                sheet_entries.push((name, path));
            }
        }
    }

    if sheet_entries.is_empty() {
        for name in archive.entry_names() {
            if name.starts_with("xl/worksheets/") && name.ends_with(".xml") {
                sheet_entries.push((name.clone(), name));
            }
        }
        sheet_entries.sort_by(|a, b| a.1.cmp(&b.1));
    }

    let mut sheets = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut rows_truncated = false;
    let mut cols_truncated = false;
    for (name, path) in sheet_entries {
        if sheets.len() >= MAX_SHEETS {
            notes.push(format!("仅显示前 {MAX_SHEETS} 个工作表"));
            break;
        }
        let Ok(xml_str) = archive.read_entry_string(&path) else {
            continue;
        };
        let Ok(doc) = roxmltree::Document::parse(&xml_str) else {
            continue;
        };
        let mut rows: Vec<Vec<CellValue>> = Vec::new();
        let mut max_col = 1usize;

        for row_node in doc.descendants().filter(|n| n.tag_name().name() == "row") {
            let row_idx = row_node
                .attribute("r")
                .and_then(|r| r.parse::<usize>().ok())
                .map(|r| r.saturating_sub(1))
                .unwrap_or(rows.len());
            if row_idx >= MAX_SHEET_ROWS {
                rows_truncated = true;
                break;
            }
            if rows.len() <= row_idx {
                rows.resize_with(row_idx + 1, Vec::new);
            }
            let mut next_col = 0usize;
            for c_node in row_node.children().filter(|n| n.tag_name().name() == "c") {
                let (r_idx, col_idx) = c_node
                    .attribute("r")
                    .and_then(parse_cell_ref)
                    .unwrap_or((row_idx, next_col));
                next_col = col_idx + 1;
                if r_idx >= MAX_SHEET_ROWS {
                    rows_truncated = true;
                    continue;
                }
                if col_idx >= MAX_SHEET_COLS {
                    cols_truncated = true;
                    continue;
                }
                max_col = max_col.max(col_idx + 1);
                if rows.len() <= r_idx {
                    rows.resize_with(r_idx + 1, Vec::new);
                }
                if rows[r_idx].len() <= col_idx {
                    rows[r_idx].resize_with(col_idx + 1, CellValue::default);
                }

                let cell_type = c_node.attribute("t").unwrap_or("");
                let formula = c_node
                    .children()
                    .find(|n| n.tag_name().name() == "f")
                    .and_then(|n| n.text())
                    .map(str::to_string);
                let v_text = c_node
                    .children()
                    .find(|n| n.tag_name().name() == "v")
                    .and_then(|n| n.text())
                    .unwrap_or("");

                let (display, is_number) = match cell_type {
                    "s" => {
                        let s = v_text
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|idx| shared_strings.get(idx))
                            .cloned()
                            .unwrap_or_else(|| v_text.to_string());
                        (s, false)
                    }
                    "inlineStr" => {
                        let mut s = String::new();
                        if let Some(is_node) =
                            c_node.children().find(|n| n.tag_name().name() == "is")
                        {
                            for t in is_node.descendants().filter(|n| n.tag_name().name() == "t") {
                                if let Some(txt) = t.text() {
                                    s.push_str(txt);
                                }
                            }
                        }
                        (s, false)
                    }
                    "b" => (
                        if v_text.trim() == "1" {
                            "TRUE".to_string()
                        } else {
                            "FALSE".to_string()
                        },
                        false,
                    ),
                    "str" | "e" => (v_text.to_string(), false),
                    _ => {
                        let num = !v_text.is_empty() && v_text.trim().parse::<f64>().is_ok();
                        (v_text.to_string(), num)
                    }
                };
                rows[r_idx][col_idx] = CellValue {
                    display,
                    formula,
                    is_number,
                };
            }
        }

        sheets.push(SheetData {
            name,
            rows,
            col_count: max_col,
        });
    }

    if sheets.is_empty() {
        return Err("未能从 Excel 文件中找到工作表".to_string());
    }
    if rows_truncated {
        notes.push(format!("表格超过 {MAX_SHEET_ROWS} 行，仅显示前 {MAX_SHEET_ROWS} 行"));
    }
    if cols_truncated {
        notes.push(format!("列数超过 {MAX_SHEET_COLS} 列，仅显示前 {MAX_SHEET_COLS} 列"));
    }
    let truncated = (!notes.is_empty()).then(|| notes.join("；"));
    Ok((sheets, truncated))
}

fn parse_delimited(text: &str, delim: char) -> Vec<Vec<CellValue>> {
    let mut rows = Vec::new();
    let mut cur_row = Vec::new();
    let mut cur_field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    let push_cell = |row: &mut Vec<CellValue>, field: &mut String| {
        let s = std::mem::take(field);
        let is_number = !s.trim().is_empty() && s.trim().parse::<f64>().is_ok();
        row.push(CellValue {
            display: s,
            formula: None,
            is_number,
        });
    };

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    cur_field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur_field.push(ch);
            }
        } else if ch == '"' && cur_field.is_empty() {
            in_quotes = true;
        } else if ch == delim {
            push_cell(&mut cur_row, &mut cur_field);
        } else if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            push_cell(&mut cur_row, &mut cur_field);
            rows.push(std::mem::take(&mut cur_row));
        } else if ch == '\n' {
            push_cell(&mut cur_row, &mut cur_field);
            rows.push(std::mem::take(&mut cur_row));
        } else {
            cur_field.push(ch);
        }
    }
    if !cur_field.is_empty() || !cur_row.is_empty() {
        push_cell(&mut cur_row, &mut cur_field);
        rows.push(cur_row);
    }
    rows
}

fn sheet_to_csv(sheet: &SheetData) -> String {
    let mut out = String::new();
    for (r_idx, row) in sheet.rows.iter().enumerate() {
        if r_idx > 0 {
            out.push('\n');
        }
        for (c_idx, cell) in row.iter().enumerate() {
            if c_idx > 0 {
                out.push(',');
            }
            let s = &cell.display;
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                out.push('"');
                out.push_str(&s.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(s);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::zip_util::build_test_zip;

    #[test]
    fn parses_csv_with_quotes() {
        let csv = "Name,Score,Note\n\"Alice, B\",95,\"Hello \"\"World\"\"\"\nBob,88.5,OK";
        let rows = parse_delimited(csv, ',');
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1][0].display, "Alice, B");
        assert!(rows[1][1].is_number);
        assert_eq!(rows[1][2].display, "Hello \"World\"");
    }

    #[test]
    fn parses_minimal_xlsx() {
        let wb = r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
</Relationships>"#;
        let ss = r#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <si><t>Item</t></si>
  <si><t>Apple</t></si>
</sst>"#;
        let s1 = r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row>
    <row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><f>B1*2</f><v>84</v></c></row>
  </sheetData>
</worksheet>"#;
        let zip = build_test_zip(&[
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/sharedStrings.xml", ss.as_bytes()),
            ("xl/worksheets/sheet1.xml", s1.as_bytes()),
        ]);
        let (sheets, truncated) = parse_xlsx_bytes(&zip).unwrap();
        assert!(truncated.is_none());
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].name, "Sales");
        assert_eq!(sheets[0].rows[0][0].display, "Item");
        assert_eq!(sheets[0].rows[1][1].display, "84");
        assert_eq!(sheets[0].rows[1][1].formula.as_deref(), Some("B1*2"));
    }

    #[test]
    fn parses_openpyxl_sheet_with_absolute_target_and_inline_strings() {
        let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets><sheet xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" name="Data" sheetId="1" state="visible" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet1.xml" Id="rId1"/></Relationships>"#;
        let s1 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>R1C1</t></is></c><c r="B1"><v>42</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>R2C1</t></is></c><c r="B2"><v>3.5</v></c></row></sheetData></worksheet>"#;
        let zip = build_test_zip(&[
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/worksheets/sheet1.xml", s1.as_bytes()),
        ]);
        let (sheets, truncated) = parse_xlsx_bytes(&zip).unwrap();
        assert!(truncated.is_none());
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].name, "Data");
        assert_eq!(sheets[0].rows[0][0].display, "R1C1");
        assert_eq!(sheets[0].rows[0][1].display, "42");
        assert_eq!(sheets[0].rows[1][0].display, "R2C1");
        assert_eq!(sheets[0].rows[1][1].display, "3.5");
        assert_eq!(sheets[0].col_count, 2);
    }

    #[test]
    fn xlsx_row_limit_truncates_with_notice() {
        let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets><sheet xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" name="Big" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let mut sheet = String::from(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for row in 1..=(MAX_SHEET_ROWS + 5) {
            sheet.push_str(&format!(
                r#"<row r="{row}"><c r="A{row}"><v>{row}</v></c></row>"#
            ));
        }
        sheet.push_str("</sheetData></worksheet>");
        let zip = build_test_zip(&[
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ]);
        let (sheets, truncated) = parse_xlsx_bytes(&zip).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].rows.len(), MAX_SHEET_ROWS);
        let note = truncated.expect("row cap must surface a truncation notice");
        assert!(note.contains("5000"), "note: {note}");
    }

}
