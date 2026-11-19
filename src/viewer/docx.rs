//! Rich Word (`.docx`) viewer powered by `zip_util`, `roxmltree`, and `image`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align as EguiAlign, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Frame,
    Margin, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TextureHandle,
    TextureOptions, Ui, Vec2,
};

use crate::ui::UiStyle;
use crate::viewer::ViewerEvent;
use crate::viewer::image_view::format_bytes;
use crate::viewer::zip_util::ZipArchive;

#[derive(Clone, Debug)]
struct DocxMeta {
    title: Option<String>,
    creator: Option<String>,
    modified: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ParaAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug)]
struct TextRun {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<Color32>,
    link_url: Option<String>,
}

#[derive(Clone, Debug)]
enum DocxBlock {
    Paragraph {
        heading_level: Option<u8>,
        is_list_item: bool,
        list_depth: usize,
        align: ParaAlign,
        runs: Vec<TextRun>,
        images: Vec<String>,
    },
    Table {
        rows: Vec<Vec<Vec<DocxBlock>>>,
    },
}

pub struct DocxViewer {
    path: PathBuf,
    file_bytes: u64,
    meta: DocxMeta,
    blocks: Vec<DocxBlock>,
    media_bytes: HashMap<String, Vec<u8>>,
    textures: HashMap<String, Option<TextureHandle>>,
    search: String,
    plain_text: String,
    error: Option<String>,
}

impl DocxViewer {
    pub fn load(path: &Path) -> Self {
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(err) => {
                return Self::with_error(path, file_bytes, format!("无法读取文件: {err}"));
            }
        };
        match parse_docx_bytes(&bytes) {
            Ok((meta, blocks, media_bytes, plain_text)) => Self {
                path: path.to_path_buf(),
                file_bytes,
                meta,
                blocks,
                media_bytes,
                textures: HashMap::new(),
                search: String::new(),
                plain_text,
                error: None,
            },
            Err(err) => Self::with_error(path, file_bytes, err),
        }
    }

    fn with_error(path: &Path, file_bytes: u64, error: String) -> Self {
        Self {
            path: path.to_path_buf(),
            file_bytes,
            meta: DocxMeta {
                title: None,
                creator: None,
                modified: None,
            },
            blocks: Vec::new(),
            media_bytes: HashMap::new(),
            textures: HashMap::new(),
            search: String::new(),
            plain_text: String::new(),
            error: Some(error),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        let mut events = Vec::new();
        let theme = style.theme;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.bg);

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

        let mut summary = format!("Word 文档 · {}", format_bytes(self.file_bytes));
        if let Some(creator) = &self.meta.creator {
            summary.push_str(&format!(" · 作者: {creator}"));
        }
        ui.painter().text(
            Pos2::new(bar_rect.left() + 14.0, bar_rect.center().y),
            Align2::LEFT_CENTER,
            &summary,
            style.tiny(),
            theme.text_dim,
        );

        // Search input + Copy plain text button on right
        let copy_r = Rect::from_center_size(
            Pos2::new(bar_rect.right() - 48.0, bar_rect.center().y),
            Vec2::new(76.0, 22.0),
        );
        let copy_resp = ui.interact(copy_r, ui.scope_id().with("docx_copy_all"), Sense::click());
        ui.painter().rect_filled(
            copy_r,
            CornerRadius::same(4),
            if copy_resp.hovered() {
                theme.hover
            } else {
                theme.input_bg
            },
        );
        ui.painter().rect_stroke(
            copy_r,
            CornerRadius::same(4),
            Stroke::new(1.0, theme.border),
            StrokeKind::Inside,
        );
        ui.painter().text(
            copy_r.center(),
            Align2::CENTER_CENTER,
            "复制纯文本",
            style.tiny(),
            theme.text_dim,
        );
        if copy_resp.clicked() {
            events.push(ViewerEvent::CopyText(self.plain_text.clone()));
        }

        let search_r = Rect::from_center_size(
            Pos2::new(copy_r.left() - 96.0, bar_rect.center().y),
            Vec2::new(176.0, 22.0),
        );
        ui.put(
            search_r,
            TextEdit::singleline(&mut self.search)
                .hint_text("查找文档内容…")
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

        let needle = self.search.trim().to_lowercase();
        let blocks = self.blocks.clone();
        let meta = self.meta.clone();

        ScrollArea::vertical()
            .id_salt(("docx_scroll", &self.path))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let pad_x = if avail_w > 640.0 {
                    ((avail_w - 820.0) * 0.5).max(28.0)
                } else {
                    18.0
                };
                let content_w = (avail_w - pad_x * 2.0).max(160.0);

                ui.horizontal(|ui| {
                    ui.add_space(pad_x);
                    ui.vertical(|ui| {
                        ui.set_width(content_w);
                        ui.add_space(16.0);

                        if meta.title.is_some() || meta.modified.is_some() {
                            Frame::new()
                                .fill(theme.panel_bg)
                                .stroke(Stroke::new(1.0, theme.border))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width((content_w - 28.0).max(100.0));
                                    if let Some(t) = &meta.title {
                                        ui.label(
                                            egui::RichText::new(t)
                                                .font(FontId::new(
                                                    17.0,
                                                    style.ui_bold_family.clone(),
                                                ))
                                                .color(theme.text),
                                        );
                                    }
                                    if let Some(m) = &meta.modified {
                                        ui.label(
                                            egui::RichText::new(format!("修改时间: {m}"))
                                                .font(style.tiny())
                                                .color(theme.text_faint),
                                        );
                                    }
                                });
                            ui.add_space(12.0);
                        }

                        self.render_blocks(ui, style, &blocks, content_w, &needle, &mut events);
                        ui.add_space(48.0);
                    });
                });
            });

        events
    }

    fn render_blocks(
        &mut self,
        ui: &mut Ui,
        style: &UiStyle,
        blocks: &[DocxBlock],
        max_w: f32,
        needle: &str,
        events: &mut Vec<ViewerEvent>,
    ) {
        let theme = style.theme;
        for block in blocks {
            match block {
                DocxBlock::Paragraph {
                    heading_level,
                    is_list_item,
                    list_depth,
                    align,
                    runs,
                    images,
                } => {
                    let font_size = match heading_level {
                        Some(0) => 24.0,
                        Some(1) => 21.0,
                        Some(2) => 17.5,
                        Some(3) => 15.5,
                        Some(4) => 14.0,
                        Some(_) => 13.5,
                        None => 14.0,
                    };
                    let force_bold = heading_level.is_some();

                    if *is_list_item {
                        let indent = (*list_depth as f32 * 18.0).min(120.0);
                        ui.horizontal_top(|ui| {
                            ui.add_space(indent);
                            ui.label(
                                egui::RichText::new("•")
                                    .font(FontId::new(14.0, style.ui_family.clone()))
                                    .color(theme.accent),
                            );
                            ui.vertical(|ui| {
                                let w = (max_w - indent - 20.0).max(80.0);
                                ui.set_width(w);
                                render_runs(
                                    ui,
                                    style,
                                    runs,
                                    w,
                                    font_size,
                                    force_bold,
                                    *align,
                                    needle,
                                    events,
                                );
                            });
                        });
                    } else if !runs.is_empty() {
                        if let Some(lvl) = heading_level
                            && *lvl <= 2
                        {
                            ui.add_space(8.0);
                        }
                        render_runs(
                            ui, style, runs, max_w, font_size, force_bold, *align, needle, events,
                        );
                        if heading_level.is_some_and(|l| l <= 2) {
                            ui.add_space(3.0);
                            let (r, _) =
                                ui.allocate_exact_size(Vec2::new(max_w, 1.0), Sense::hover());
                            ui.painter().line_segment(
                                [Pos2::new(r.left(), r.center().y), Pos2::new(r.right(), r.center().y)],
                                Stroke::new(1.0, theme.border),
                            );
                        }
                    }

                    for media_target in images {
                        self.render_embedded_image(ui, style, media_target, max_w);
                    }

                    ui.add_space(if *is_list_item { 3.0 } else { 7.0 });
                }
                DocxBlock::Table { rows } => {
                    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
                    let col_w = ((max_w / cols as f32) - 2.0).clamp(80.0, 320.0);
                    ScrollArea::horizontal()
                        .id_salt(("docx_table", rows.len(), cols))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            Frame::new()
                                .stroke(Stroke::new(1.0, theme.border_strong))
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(Margin::ZERO)
                                .show(ui, |ui| {
                                    for (r_idx, row) in rows.iter().enumerate() {
                                        let bg = if r_idx == 0 {
                                            theme.panel_bg
                                        } else if r_idx % 2 == 1 {
                                            theme.bg
                                        } else {
                                            theme.panel_bg.gamma_multiply(0.45)
                                        };
                                        let row_resp = ui
                                            .horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 0.0;
                                                for c_idx in 0..cols {
                                                    Frame::new()
                                                        .fill(bg)
                                                        .inner_margin(Margin::symmetric(8, 6))
                                                        .show(ui, |ui| {
                                                            let cw = (col_w - 16.0).max(50.0);
                                                            ui.set_width(cw);
                                                            if let Some(cell_blocks) = row.get(c_idx)
                                                            {
                                                                self.render_blocks(
                                                                    ui,
                                                                    style,
                                                                    cell_blocks,
                                                                    cw,
                                                                    needle,
                                                                    events,
                                                                );
                                                            }
                                                        });
                                                }
                                            })
                                            .response;
                                        if r_idx + 1 < rows.len() {
                                            ui.painter().line_segment(
                                                [
                                                    Pos2::new(
                                                        row_resp.rect.left(),
                                                        row_resp.rect.bottom(),
                                                    ),
                                                    Pos2::new(
                                                        row_resp.rect.right(),
                                                        row_resp.rect.bottom(),
                                                    ),
                                                ],
                                                Stroke::new(1.0, theme.border),
                                            );
                                        }
                                    }
                                });
                        });
                    ui.add_space(10.0);
                }
            }
        }
    }

    fn render_embedded_image(&mut self, ui: &mut Ui, style: &UiStyle, target: &str, max_w: f32) {
        if !self.textures.contains_key(target) {
            let tex = self.media_bytes.get(target).and_then(|bytes| {
                let img = image::load_from_memory(bytes).ok()?;
                let rgba = img.to_rgba8();
                let sz = [rgba.width() as usize, rgba.height() as usize];
                let color = ColorImage::from_rgba_unmultiplied(sz, rgba.as_raw());
                Some(ui.ctx().load_texture(
                    format!("docx_media:{}:{}", self.path.display(), target),
                    color,
                    TextureOptions::LINEAR,
                ))
            });
            self.textures.insert(target.to_string(), tex);
        }
        if let Some(Some(tex)) = self.textures.get(target) {
            let sz = tex.size();
            let nat_w = sz[0].max(1) as f32;
            let nat_h = sz[1].max(1) as f32;
            let w = nat_w.min(max_w).max(24.0);
            let h = (w * (nat_h / nat_w)).min(540.0);
            let final_w = h * (nat_w / nat_h);
            let (r, _) = ui.allocate_exact_size(Vec2::new(final_w, h), Sense::hover());
            ui.painter().image(
                tex.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(1.0, style.theme.border),
                StrokeKind::Inside,
            );
            ui.add_space(4.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_runs(
    ui: &mut Ui,
    style: &UiStyle,
    runs: &[TextRun],
    max_w: f32,
    font_size: f32,
    force_bold: bool,
    align: ParaAlign,
    needle: &str,
    events: &mut Vec<ViewerEvent>,
) {
    let theme = style.theme;
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_w.max(40.0);
    job.halign = match align {
        ParaAlign::Center => EguiAlign::Center,
        ParaAlign::Right => EguiAlign::RIGHT,
        ParaAlign::Left => EguiAlign::LEFT,
    };

    let mut links: Vec<(usize, usize, String)> = Vec::new();
    let mut char_pos = 0usize;
    let line_h = Some((font_size * 1.48).round());

    for run in runs {
        let family = if force_bold || run.bold {
            style.ui_bold_family.clone()
        } else {
            style.ui_family.clone()
        };
        let color = if run.link_url.is_some() {
            theme.accent
        } else {
            run.color.unwrap_or(theme.text)
        };
        let underline = if run.underline || run.link_url.is_some() {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        };
        let strike = if run.strike {
            Stroke::new(1.1, color.gamma_multiply(0.8))
        } else {
            Stroke::NONE
        };

        let start = char_pos;
        if needle.is_empty() {
            job.append(
                &run.text,
                0.0,
                TextFormat {
                    font_id: FontId::new(font_size, family),
                    line_height: line_h,
                    color,
                    italics: run.italic,
                    underline,
                    strikethrough: strike,
                    ..Default::default()
                },
            );
            char_pos += run.text.chars().count();
        } else {
            // Highlight search matches inside run
            let lower = run.text.to_lowercase();
            let mut byte_idx = 0usize;
            while byte_idx < run.text.len() {
                if let Some(rel) = lower[byte_idx..].find(needle) {
                    let m_start = byte_idx + rel;
                    let m_end = m_start + needle.len();
                    if m_start > byte_idx && run.text.is_char_boundary(m_start) {
                        let seg = &run.text[byte_idx..m_start];
                        job.append(
                            seg,
                            0.0,
                            TextFormat {
                                font_id: FontId::new(font_size, family.clone()),
                                line_height: line_h,
                                color,
                                italics: run.italic,
                                underline,
                                strikethrough: strike,
                                ..Default::default()
                            },
                        );
                        char_pos += seg.chars().count();
                    }
                    if run.text.is_char_boundary(m_start) && run.text.is_char_boundary(m_end) {
                        let matched = &run.text[m_start..m_end];
                        job.append(
                            matched,
                            0.0,
                            TextFormat {
                                font_id: FontId::new(font_size, family.clone()),
                                line_height: line_h,
                                color,
                                background: theme.find_match,
                                italics: run.italic,
                                underline,
                                strikethrough: strike,
                                ..Default::default()
                            },
                        );
                        char_pos += matched.chars().count();
                        byte_idx = m_end;
                    } else {
                        let rest = &run.text[byte_idx..];
                        job.append(
                            rest,
                            0.0,
                            TextFormat {
                                font_id: FontId::new(font_size, family.clone()),
                                line_height: line_h,
                                color,
                                italics: run.italic,
                                underline,
                                strikethrough: strike,
                                ..Default::default()
                            },
                        );
                        char_pos += rest.chars().count();
                        break;
                    }
                } else {
                    let rest = &run.text[byte_idx..];
                    job.append(
                        rest,
                        0.0,
                        TextFormat {
                            font_id: FontId::new(font_size, family.clone()),
                            line_height: line_h,
                            color,
                            italics: run.italic,
                            underline,
                            strikethrough: strike,
                            ..Default::default()
                        },
                    );
                    char_pos += rest.chars().count();
                    break;
                }
            }
        }
        if let Some(url) = &run.link_url
            && char_pos > start
        {
            links.push((start, char_pos, url.clone()));
        }
    }

    if job.text.is_empty() {
        return;
    }

    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let sz = galley.size();
    let alloc_w = if align != ParaAlign::Left {
        max_w.max(sz.x)
    } else {
        sz.x.min(max_w.max(40.0))
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(alloc_w, sz.y), Sense::click());
    let draw_pos = match align {
        ParaAlign::Center => Pos2::new(rect.left() + (alloc_w - sz.x) * 0.5, rect.top()),
        ParaAlign::Right => Pos2::new(rect.right() - sz.x, rect.top()),
        ParaAlign::Left => rect.min,
    };
    ui.painter().galley(draw_pos, galley.clone(), theme.text);

    if !links.is_empty()
        && resp.hovered()
        && let Some(pos) = ui.ctx().input(|i| i.pointer.hover_pos())
    {
        let idx = galley.cursor_from_pos(pos - draw_pos).index.0;
        if let Some((_, _, url)) = links.iter().find(|(s, e, _)| idx >= *s && idx < *e) {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            resp.clone().on_hover_text(url);
            if resp.clicked() {
                events.push(ViewerEvent::OpenUrl(url.clone()));
            }
        }
    }
}

fn parse_docx_bytes(
    bytes: &[u8],
) -> Result<(DocxMeta, Vec<DocxBlock>, HashMap<String, Vec<u8>>, String), String> {
    let archive = ZipArchive::from_bytes(bytes.to_vec())?;
    let doc_xml = archive.read_entry_string("word/document.xml")?;

    let mut rels: HashMap<String, String> = HashMap::new();
    if let Ok(rels_xml) = archive.read_entry_string("word/_rels/document.xml.rels")
        && let Ok(doc) = roxmltree::Document::parse(&rels_xml)
    {
        for node in doc.descendants().filter(|n| n.tag_name().name() == "Relationship") {
            if let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target")) {
                rels.insert(id.to_string(), target.to_string());
            }
        }
    }

    let meta = parse_core_meta(&archive);

    let mut media_bytes = HashMap::new();
    for target in rels.values() {
        let norm = if target.starts_with("word/") {
            target.clone()
        } else {
            format!("word/{}", target.trim_start_matches('/'))
        };
        if norm.contains("media/")
            && let Ok(data) = archive.read_entry(&norm)
        {
            media_bytes.insert(target.clone(), data);
        }
    }

    let xml = roxmltree::Document::parse(&doc_xml).map_err(|e| format!("XML 解析失败: {e}"))?;
    let body = xml
        .descendants()
        .find(|n| n.tag_name().name() == "body")
        .ok_or_else(|| "缺少 word:body 节点".to_string())?;

    let mut blocks = Vec::new();
    let mut plain_text = String::new();

    for child in body.children().filter(|n| n.is_element()) {
        if let Some(block) = parse_body_child(child, &rels, &mut plain_text) {
            blocks.push(block);
        }
    }

    Ok((meta, blocks, media_bytes, plain_text))
}

fn parse_core_meta(archive: &ZipArchive) -> DocxMeta {
    let mut meta = DocxMeta {
        title: None,
        creator: None,
        modified: None,
    };
    let Ok(core_xml) = archive.read_entry_string("docProps/core.xml") else {
        return meta;
    };
    let Ok(doc) = roxmltree::Document::parse(&core_xml) else {
        return meta;
    };
    for node in doc.descendants().filter(|n| n.is_element()) {
        let txt = node.text().map(|s| s.trim()).filter(|s| !s.is_empty());
        match node.tag_name().name() {
            "title" => meta.title = txt.map(str::to_string),
            "creator" => meta.creator = txt.map(str::to_string),
            "modified" => meta.modified = txt.map(str::to_string),
            _ => {}
        }
    }
    meta
}

fn parse_body_child(
    node: roxmltree::Node<'_, '_>,
    rels: &HashMap<String, String>,
    plain_text: &mut String,
) -> Option<DocxBlock> {
    match node.tag_name().name() {
        "p" => Some(parse_paragraph(node, rels, plain_text)),
        "tbl" => Some(parse_table(node, rels, plain_text)),
        _ => None,
    }
}

fn parse_paragraph(
    p: roxmltree::Node<'_, '_>,
    rels: &HashMap<String, String>,
    plain_text: &mut String,
) -> DocxBlock {
    let mut heading_level = None;
    let mut is_list_item = false;
    let mut list_depth = 0usize;
    let mut align = ParaAlign::Left;

    if let Some(ppr) = p.children().find(|n| n.tag_name().name() == "pPr") {
        for prop in ppr.children().filter(|n| n.is_element()) {
            match prop.tag_name().name() {
                "pStyle" => {
                    if let Some(val) = attr_val(prop, "val") {
                        heading_level = heading_from_style(val);
                    }
                }
                "numPr" => {
                    is_list_item = true;
                    if let Some(ilvl) = prop.children().find(|n| n.tag_name().name() == "ilvl")
                        && let Some(v) = attr_val(ilvl, "val")
                    {
                        list_depth = v.parse().unwrap_or(0);
                    }
                }
                "jc" => {
                    if let Some(v) = attr_val(prop, "val") {
                        align = match v {
                            "center" => ParaAlign::Center,
                            "right" | "end" => ParaAlign::Right,
                            _ => ParaAlign::Left,
                        };
                    }
                }
                _ => {}
            }
        }
    }

    let mut runs = Vec::new();
    let mut images = Vec::new();

    for child in p.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "r" => parse_run(child, None, rels, &mut runs, &mut images),
            "hyperlink" => {
                let link_url = attr_val(child, "id").and_then(|id| rels.get(id).cloned());
                for sub in child.children().filter(|n| n.tag_name().name() == "r") {
                    parse_run(sub, link_url.as_deref(), rels, &mut runs, &mut images);
                }
            }
            _ => {}
        }
    }

    for run in &runs {
        plain_text.push_str(&run.text);
    }
    if !runs.is_empty() {
        plain_text.push('\n');
    }

    DocxBlock::Paragraph {
        heading_level,
        is_list_item,
        list_depth,
        align,
        runs,
        images,
    }
}

fn parse_run(
    r: roxmltree::Node<'_, '_>,
    link_url: Option<&str>,
    rels: &HashMap<String, String>,
    runs: &mut Vec<TextRun>,
    images: &mut Vec<String>,
) {
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut strike = false;
    let mut color = None;

    if let Some(rpr) = r.children().find(|n| n.tag_name().name() == "rPr") {
        for prop in rpr.children().filter(|n| n.is_element()) {
            match prop.tag_name().name() {
                "b" => bold = attr_val(prop, "val") != Some("0") && attr_val(prop, "val") != Some("false"),
                "i" => italic = attr_val(prop, "val") != Some("0") && attr_val(prop, "val") != Some("false"),
                "u" => underline = attr_val(prop, "val") != Some("none"),
                "strike" | "dstrike" => {
                    strike = attr_val(prop, "val") != Some("0") && attr_val(prop, "val") != Some("false")
                }
                "color" => {
                    if let Some(hex) = attr_val(prop, "val")
                        && hex != "auto"
                        && hex.len() == 6
                        && let (Ok(r), Ok(g), Ok(b)) = (
                            u8::from_str_radix(&hex[0..2], 16),
                            u8::from_str_radix(&hex[2..4], 16),
                            u8::from_str_radix(&hex[4..6], 16),
                        )
                    {
                        color = Some(Color32::from_rgb(r, g, b));
                    }
                }
                _ => {}
            }
        }
    }

    let mut text = String::new();
    for child in r.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "t" | "delText" | "instrText" => {
                if child.tag_name().name() != "instrText"
                    && let Some(t) = child.text()
                {
                    text.push_str(t);
                }
            }
            "tab" => text.push('\t'),
            "br" | "cr" => text.push('\n'),
            "drawing" | "pict" => {
                for desc in child.descendants().filter(|n| n.is_element()) {
                    if desc.tag_name().name() == "blip"
                        && let Some(rid) = attr_val(desc, "embed")
                        && let Some(target) = rels.get(rid)
                    {
                        images.push(target.clone());
                    }
                }
            }
            _ => {}
        }
    }

    if !text.is_empty() {
        runs.push(TextRun {
            text,
            bold,
            italic,
            underline,
            strike,
            color,
            link_url: link_url.map(str::to_string),
        });
    }
}

fn parse_table(
    tbl: roxmltree::Node<'_, '_>,
    rels: &HashMap<String, String>,
    plain_text: &mut String,
) -> DocxBlock {
    let mut rows = Vec::new();
    for tr in tbl.children().filter(|n| n.tag_name().name() == "tr") {
        let mut row_cells = Vec::new();
        for tc in tr.children().filter(|n| n.tag_name().name() == "tc") {
            let mut cell_blocks = Vec::new();
            for child in tc.children().filter(|n| n.is_element()) {
                if let Some(b) = parse_body_child(child, rels, plain_text) {
                    cell_blocks.push(b);
                }
            }
            row_cells.push(cell_blocks);
        }
        rows.push(row_cells);
    }
    DocxBlock::Table { rows }
}

fn attr_val<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}

fn heading_from_style(style: &str) -> Option<u8> {
    let lower = style.to_ascii_lowercase();
    if lower == "title" {
        return Some(0);
    }
    if lower == "subtitle" {
        return Some(2);
    }
    if let Some(rest) = lower.strip_prefix("heading") {
        return rest.trim().parse::<u8>().ok().filter(|n| (1..=6).contains(n));
    }
    if let Some(rest) = lower.strip_prefix("heading ") {
        return rest.trim().parse::<u8>().ok().filter(|n| (1..=6).contains(n));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::zip_util::build_test_zip;

    #[test]
    fn parses_minimal_docx_paragraphs_and_table() {
        let doc_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Kestrel Docx</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:rPr><w:b/></w:rPr><w:t>Bold</w:t></w:r>
      <w:hyperlink r:id="rId1"><w:r><w:t>Link</w:t></w:r></w:hyperlink>
    </w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>Cell1</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;
        let rels_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Target="https://example.com"/>
</Relationships>"#;
        let zip = build_test_zip(&[
            ("word/document.xml", doc_xml.as_bytes()),
            ("word/_rels/document.xml.rels", rels_xml.as_bytes()),
        ]);
        let (_, blocks, _, plain) = parse_docx_bytes(&zip).unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(plain.contains("Kestrel Docx"));
        assert!(plain.contains("BoldLink"));
        assert!(plain.contains("Cell1"));
    }
}
