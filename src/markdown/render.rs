//! Rich egui renderer for parsed Markdown documents.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align as EguiAlign, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontFamily,
    FontId, Frame, Margin, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextureOptions, Ui,
    Vec2,
};

use crate::icons::{self, Icon};
use crate::markdown::parser::{format_latex_unicode, plain_text_of_inlines};
use crate::markdown::{
    Align, Block, CachedImage, CalloutKind, FrontMatter, FrontMatterKind, Inline,
    MarkdownPreviewState, MarkdownViewMode, PreviewEvent,
};
use crate::theme::Theme;
use crate::ui::UiStyle;

struct LinkSpan {
    char_start: usize,
    char_end: usize,
    url: String,
    title: Option<String>,
    is_footnote: Option<String>,
}

struct RenderContext<'a> {
    style: &'a UiStyle,
    theme: Theme,
    base_size: f32,
    doc_dir: Option<&'a Path>,
    workspace_root: Option<&'a Path>,
    state: &'a mut MarkdownPreviewState,
    events: &'a mut Vec<PreviewEvent>,
    scroll_origin_y: f32,
    inherited_align: Option<Align>,
}

#[allow(clippy::too_many_arguments)]
pub fn preview_ui(
    ui: &mut Ui,
    style: &UiStyle,
    state: &mut MarkdownPreviewState,
    _view_mode: &mut MarkdownViewMode,
    doc_version: u64,
    doc_text: &str,
    doc_path: Option<&Path>,
    workspace_root: Option<&Path>,
    base_font_size: f32,
) -> Vec<PreviewEvent> {
    state.ensure_parsed(doc_version, doc_text);
    // Collect any remote images / SVGs finished by the background fetch pool.
    for done in state.remote_images.drain() {
        let entry = match done.result {
            Ok(crate::markdown::net_img::DecodedImage::Raster { image, size }) => {
                let texture = ui.ctx().load_texture(
                    format!("md_net:{}", done.url),
                    image,
                    TextureOptions::LINEAR,
                );
                CachedImage::Loaded {
                    texture,
                    size,
                    modified: None,
                }
            }
            Ok(crate::markdown::net_img::DecodedImage::Svg(graphic)) => CachedImage::Svg {
                cache: crate::markdown::svg::SvgRenderCache::new(ui.ctx(), &done.url, graphic),
                modified: None,
            },
            Err(err) => CachedImage::Failed(err),
        };
        state.textures.insert(done.url, entry);
    }
    let mut events = Vec::new();
    let theme = style.theme;
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, theme.bg);

    let doc = std::sync::Arc::clone(&state.doc);
    let doc_dir = doc_path.and_then(|p| p.parent());
    let base_size = base_font_size.clamp(11.0, 24.0);

    let mut scroll = ScrollArea::vertical()
        .id_salt(("md_preview_scroll", doc_path))
        .auto_shrink([false, false]);
    if let Some(target_slug) = state.pending_scroll_to.take() {
        if let Some(&y_offset) = state.anchor_y.get(&target_slug) {
            scroll = scroll.vertical_scroll_offset(y_offset.max(0.0));
        } else {
            // Keep pending if anchors weren't measured yet
            state.pending_scroll_to = Some(target_slug);
        }
    } else if let Some(y) = state.pending_scroll_y.take() {
        scroll = scroll.vertical_scroll_offset(y.max(0.0));
    }

    let out = scroll.show(ui, |ui| {
        let avail_w = ui.available_width();
        let pad_x = if avail_w > 640.0 {
            ((avail_w - 820.0) * 0.5).max(28.0)
        } else {
            18.0
        };
        let content_w = (avail_w - pad_x * 2.0).max(160.0);
        let origin_y = ui.cursor().top();
        state.anchor_y.clear();
        state.line_anchors.clear();

        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.vertical(|ui| {
                ui.set_width(content_w);
                ui.add_space(16.0);

                let mut rctx = RenderContext {
                    style,
                    theme,
                    base_size,
                    doc_dir,
                    workspace_root,
                    state,
                    events: &mut events,
                    scroll_origin_y: origin_y,
                    inherited_align: None,
                };

                if let Some(fm) = &doc.front_matter {
                    render_front_matter(ui, &mut rctx, fm, content_w);
                    ui.add_space(12.0);
                }

                render_blocks(ui, &mut rctx, &doc.blocks, content_w, false);

                if !doc.footnotes.is_empty() {
                    ui.add_space(16.0);
                    render_thematic_break(ui, &rctx, content_w);
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("脚注 (Footnotes)")
                            .font(FontId::new(base_size * 0.9, style.ui_bold_family.clone()))
                            .color(theme.text_dim),
                    );
                    ui.add_space(6.0);
                    for fn_def in &doc.footnotes {
                        let y = ui.cursor().top() - rctx.scroll_origin_y;
                        rctx.state
                            .anchor_y
                            .insert(format!("fn:{}", fn_def.id), y.max(0.0));
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                egui::RichText::new(format!("[{}]", fn_def.number))
                                    .font(FontId::new(base_size * 0.85, style.mono_family.clone()))
                                    .color(theme.accent),
                            );
                            ui.vertical(|ui| {
                                let w = (content_w - 36.0).max(120.0);
                                ui.set_width(w);
                                render_blocks(ui, &mut rctx, &fn_def.blocks, w, true);
                            });
                        });
                        ui.add_space(4.0);
                    }
                }

                ui.add_space(48.0);
            });
        });
    });

    state.scroll_y = out.state.clamped_offset().y;
    state.content_height = out.content_size.y;
    state.viewport_height = out.inner_rect.height();

    // Right-click context menu on the Markdown / HTML preview surface.
    let bg_resp = ui.interact(
        rect,
        ui.scope_id().with(("md_preview_ctx", doc_path)),
        Sense::click(),
    );
    let cur_mode = *_view_mode;
    bg_resp.context_menu(|ui| {
        ui.set_width(204.0);
        let font = style.small();
        let sc_font = FontId::new(11.0, style.mono_family.clone());
        if crate::ui::widgets::context_menu_item(
            ui,
            &theme,
            font.clone(),
            sc_font.clone(),
            Icon::Copy,
            "复制文档源码",
            None,
            true,
        ) {
            events.push(PreviewEvent::CopySource);
        }
        crate::ui::widgets::context_menu_sep(ui, &theme);
        if cur_mode != MarkdownViewMode::Split
            && crate::ui::widgets::context_menu_item(
                ui,
                &theme,
                font.clone(),
                sc_font.clone(),
                Icon::SplitHorizontal,
                "侧边分屏预览",
                Some("Alt+V"),
                true,
            )
        {
            events.push(PreviewEvent::SwitchMode(MarkdownViewMode::Split));
        }
        if crate::ui::widgets::context_menu_item(
            ui,
            &theme,
            font.clone(),
            sc_font.clone(),
            Icon::FileCode,
            "切换到源码编辑",
            Some("Ctrl+Shift+V"),
            true,
        ) {
            events.push(PreviewEvent::SwitchMode(MarkdownViewMode::Source));
        }
        crate::ui::widgets::context_menu_sep(ui, &theme);
        if crate::ui::widgets::context_menu_item(
            ui,
            &theme,
            font,
            sc_font,
            Icon::Globe,
            "在浏览器中预览网页",
            None,
            true,
        ) {
            events.push(PreviewEvent::OpenInBrowser);
        }
    });

    events
}

/// Render Markdown blocks inline inside an existing container (such as a plugin
/// view or chat bubble) without painting a full-page backdrop or outer ScrollArea.
pub fn embedded_ui(
    ui: &mut Ui,
    style: &UiStyle,
    state: &mut MarkdownPreviewState,
    version: u64,
    text: &str,
    workspace_root: Option<&Path>,
    base_font_size: f32,
) -> Vec<PreviewEvent> {
    state.ensure_parsed(version, text);
    for done in state.remote_images.drain() {
        let entry = match done.result {
            Ok(crate::markdown::net_img::DecodedImage::Raster { image, size }) => {
                let texture = ui.ctx().load_texture(
                    format!("md_net:{}", done.url),
                    image,
                    TextureOptions::LINEAR,
                );
                CachedImage::Loaded {
                    texture,
                    size,
                    modified: None,
                }
            }
            Ok(crate::markdown::net_img::DecodedImage::Svg(graphic)) => CachedImage::Svg {
                cache: crate::markdown::svg::SvgRenderCache::new(ui.ctx(), &done.url, graphic),
                modified: None,
            },
            Err(err) => CachedImage::Failed(err),
        };
        state.textures.insert(done.url, entry);
    }
    let mut events = Vec::new();
    let doc = std::sync::Arc::clone(&state.doc);
    let content_w = ui.available_width().max(120.0);
    let origin_y = ui.cursor().top();
    let mut rctx = RenderContext {
        style,
        theme: style.theme,
        base_size: base_font_size.clamp(11.0, 20.0),
        doc_dir: workspace_root,
        workspace_root,
        state,
        events: &mut events,
        scroll_origin_y: origin_y,
        inherited_align: None,
    };
    render_blocks(ui, &mut rctx, &doc.blocks, content_w, true);
    events
}

fn render_front_matter(ui: &mut Ui, rctx: &mut RenderContext<'_>, fm: &FrontMatter, width: f32) {
    let theme = rctx.theme;
    let kind_label = match fm.kind {
        FrontMatterKind::Yaml => "YAML FRONT MATTER",
        FrontMatterKind::Toml => "TOML FRONT MATTER",
    };
    Frame::new()
        .fill(theme.panel_bg)
        .stroke(Stroke::new(1.0, theme.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width((width - 28.0).max(100.0));
            ui.label(
                egui::RichText::new(kind_label)
                    .font(FontId::new(10.5, rctx.style.ui_bold_family.clone()))
                    .color(theme.text_faint),
            );
            ui.add_space(6.0);
            if fm.entries.is_empty() {
                ui.label(
                    egui::RichText::new(&fm.raw)
                        .font(FontId::new(rctx.base_size * 0.88, rctx.style.mono_family.clone()))
                        .color(theme.text_dim),
                );
            } else {
                for (k, v) in &fm.entries {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{k}:"))
                                .font(FontId::new(
                                    rctx.base_size * 0.88,
                                    rctx.style.ui_bold_family.clone(),
                                ))
                                .color(theme.accent),
                        );
                        ui.label(
                            egui::RichText::new(v)
                                .font(FontId::new(
                                    rctx.base_size * 0.88,
                                    rctx.style.ui_family.clone(),
                                ))
                                .color(theme.text),
                        );
                    });
                }
            }
        });
}

fn render_blocks(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    blocks: &[Block],
    max_w: f32,
    tight: bool,
) {
    for (idx, block) in blocks.iter().enumerate() {
        match block {
            Block::Heading {
                level,
                slug,
                inlines,
                source_line,
                align,
            } => {
                if idx > 0 {
                    ui.add_space(if *level <= 2 { 14.0 } else { 8.0 });
                }
                let y = ui.cursor().top() - rctx.scroll_origin_y;
                rctx.state.anchor_y.insert(slug.clone(), y.max(0.0));
                rctx.state.line_anchors.push((*source_line, y.max(0.0)));
                let eff_align = (*align).or(rctx.inherited_align);
                render_heading(ui, rctx, *level, slug, inlines, *source_line, max_w, eff_align);
                ui.add_space(if *level <= 2 { 8.0 } else { 5.0 });
            }
            Block::Paragraph { inlines, align } => {
                let eff_align = (*align).or(rctx.inherited_align);
                render_inlines_flow(
                    ui,
                    rctx,
                    inlines,
                    max_w,
                    rctx.base_size,
                    false,
                    rctx.theme.text,
                    eff_align,
                );
                if !tight {
                    ui.add_space(8.0);
                } else if idx + 1 < blocks.len() {
                    ui.add_space(4.0);
                }
            }
            Block::Container {
                align,
                blocks: inner,
                caption,
            } => {
                let prev_align = rctx.inherited_align;
                rctx.inherited_align = (*align).or(prev_align);
                render_blocks(ui, rctx, inner, max_w, tight);
                if let Some(cap) = caption {
                    render_inlines_flow(
                        ui,
                        rctx,
                        cap,
                        max_w,
                        (rctx.base_size * 0.88).round().max(11.0),
                        false,
                        rctx.theme.text_dim,
                        rctx.inherited_align.or(Some(Align::Center)),
                    );
                    ui.add_space(6.0);
                }
                rctx.inherited_align = prev_align;
                if !tight {
                    ui.add_space(6.0);
                }
            }
            Block::CodeBlock {
                lang,
                meta,
                code,
                spans,
                source_line,
            } => {
                let y = ui.cursor().top() - rctx.scroll_origin_y;
                rctx.state.line_anchors.push((*source_line, y.max(0.0)));
                render_code_block(ui, rctx, lang, meta, code, spans, *source_line, max_w);
                ui.add_space(if tight { 6.0 } else { 10.0 });
            }
            Block::MathBlock { tex, source_line } => {
                let y = ui.cursor().top() - rctx.scroll_origin_y;
                rctx.state.line_anchors.push((*source_line, y.max(0.0)));
                render_math_block(ui, rctx, tex, *source_line, max_w);
                ui.add_space(if tight { 6.0 } else { 10.0 });
            }
            Block::Blockquote {
                callout,
                blocks: inner,
            } => {
                render_blockquote(ui, rctx, callout.as_ref(), inner, max_w);
                ui.add_space(if tight { 6.0 } else { 10.0 });
            }
            Block::List {
                ordered,
                start,
                tight: list_tight,
                items,
            } => {
                render_list(ui, rctx, *ordered, *start, *list_tight, items, max_w, 0);
                ui.add_space(if tight { 4.0 } else { 8.0 });
            }
            Block::Table {
                alignments,
                headers,
                rows,
                source_line,
            } => {
                let y = ui.cursor().top() - rctx.scroll_origin_y;
                rctx.state.line_anchors.push((*source_line, y.max(0.0)));
                render_table(ui, rctx, alignments, headers, rows, *source_line, max_w);
                ui.add_space(if tight { 6.0 } else { 10.0 });
            }
            Block::DefinitionList { items } => {
                for item in items {
                    render_inlines_flow(
                        ui,
                        rctx,
                        &item.term,
                        max_w,
                        rctx.base_size,
                        true,
                        rctx.theme.text,
                        None,
                    );
                    ui.add_space(3.0);
                    for def_blocks in &item.definitions {
                        ui.horizontal_top(|ui| {
                            ui.add_space(16.0);
                            ui.vertical(|ui| {
                                let w = (max_w - 20.0).max(100.0);
                                ui.set_width(w);
                                render_blocks(ui, rctx, def_blocks, w, true);
                            });
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_space(4.0);
                }
                ui.add_space(6.0);
            }
            Block::Details {
                summary,
                blocks: inner,
                open_by_default,
                source_line,
            } => {
                let y = ui.cursor().top() - rctx.scroll_origin_y;
                rctx.state.line_anchors.push((*source_line, y.max(0.0)));
                let title = plain_text_of_inlines(summary);
                Frame::new()
                    .fill(rctx.theme.panel_bg.gamma_multiply(0.55))
                    .stroke(Stroke::new(1.0, rctx.theme.border))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width((max_w - 24.0).max(100.0));
                        egui::CollapsingHeader::new(
                            egui::RichText::new(title)
                                .font(FontId::new(
                                    rctx.base_size,
                                    rctx.style.ui_bold_family.clone(),
                                ))
                                .color(rctx.theme.text),
                        )
                        .id_salt(("md_details", *source_line))
                        .default_open(*open_by_default)
                        .show(ui, |ui| {
                            ui.add_space(4.0);
                            render_blocks(ui, rctx, inner, (max_w - 38.0).max(100.0), false);
                        });
                    });
                ui.add_space(8.0);
            }
            Block::ThematicBreak => {
                ui.add_space(4.0);
                render_thematic_break(ui, rctx, max_w);
                ui.add_space(8.0);
            }
            Block::HtmlBlock { html } => {
                Frame::new()
                    .fill(rctx.theme.input_bg)
                    .stroke(Stroke::new(1.0, rctx.theme.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width((max_w - 20.0).max(100.0));
                        ui.label(
                            egui::RichText::new(html)
                                .font(FontId::new(
                                    rctx.base_size * 0.88,
                                    rctx.style.mono_family.clone(),
                                ))
                                .color(rctx.theme.text_dim),
                        );
                    });
                ui.add_space(8.0);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_heading(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    level: u8,
    slug: &str,
    inlines: &[Inline],
    source_line: usize,
    max_w: f32,
    align: Option<Align>,
) {
    let scale = match level {
        1 => 1.85,
        2 => 1.5,
        3 => 1.28,
        4 => 1.12,
        5 => 1.0,
        _ => 0.92,
    };
    let size = (rctx.base_size * scale).round();
    let color = if level == 6 {
        rctx.theme.text_dim
    } else {
        rctx.theme.text
    };

    if matches!(align, Some(Align::Center | Align::Right)) {
        render_inlines_flow(ui, rctx, inlines, max_w, size, true, color, align);
    } else {
        let row_resp = ui.horizontal(|ui| {
            ui.set_max_width(max_w);
            render_inlines_flow(ui, rctx, inlines, max_w - 24.0, size, true, color, None);
            let (hash_rect, h_resp) =
                ui.allocate_exact_size(Vec2::new(18.0, size * 1.15), Sense::click());
            let pointer_on_row = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
                let band = Rect::from_min_max(
                    Pos2::new(ui.min_rect().left(), hash_rect.top()),
                    Pos2::new(hash_rect.right() + 4.0, hash_rect.bottom()),
                );
                band.contains(p)
            });
            if pointer_on_row || h_resp.hovered() {
                ui.painter().text(
                    hash_rect.center(),
                    Align2::CENTER_CENTER,
                    "#",
                    FontId::new(size * 0.78, rctx.style.mono_family.clone()),
                    if h_resp.hovered() {
                        rctx.theme.accent
                    } else {
                        rctx.theme.text_muted
                    },
                );
            }
            if h_resp.on_hover_text("定位到源码行").clicked() {
                rctx.events.push(PreviewEvent::JumpSourceLine(source_line));
            }
        });
        let _ = row_resp;
    }
    let _ = slug;

    if level <= 2 {
        ui.add_space(4.0);
        let (r, _) = ui.allocate_exact_size(Vec2::new(max_w, 1.0), Sense::hover());
        ui.painter().line_segment(
            [Pos2::new(r.left(), r.center().y), Pos2::new(r.right(), r.center().y)],
            Stroke::new(1.0, rctx.theme.border),
        );
    }
}

fn render_thematic_break(ui: &mut Ui, rctx: &RenderContext<'_>, max_w: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(max_w, 6.0), Sense::hover());
    ui.painter().line_segment(
        [Pos2::new(r.left(), r.center().y), Pos2::new(r.right(), r.center().y)],
        Stroke::new(1.5, rctx.theme.border_strong),
    );
}

#[allow(clippy::too_many_arguments)]
fn render_code_block(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    lang: &str,
    meta: &str,
    code: &str,
    spans: &[crate::highlight::Span],
    source_line: usize,
    max_w: f32,
) {
    let theme = rctx.theme;
    let header_label = if lang.is_empty() {
        "代码".to_string()
    } else if meta.is_empty() {
        lang.to_string()
    } else {
        format!("{lang} · {meta}")
    };

    Frame::new()
        .fill(theme.code_bg)
        .stroke(Stroke::new(1.0, theme.border))
        .corner_radius(CornerRadius::same(crate::theme::R_LG))
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            ui.set_width(max_w);
            // Code block header bar
            let (hdr, _) =
                ui.allocate_exact_size(Vec2::new(max_w, 26.0), Sense::hover());
            ui.painter().rect_filled(
                Rect::from_min_max(hdr.min, hdr.max),
                CornerRadius {
                    nw: 8,
                    ne: 8,
                    sw: 0,
                    se: 0,
                },
                theme.panel_bg,
            );
            ui.painter().line_segment(
                [
                    Pos2::new(hdr.left(), hdr.bottom() - 0.5),
                    Pos2::new(hdr.right(), hdr.bottom() - 0.5),
                ],
                Stroke::new(1.0, theme.border),
            );
            ui.painter().text(
                Pos2::new(hdr.left() + 12.0, hdr.center().y),
                Align2::LEFT_CENTER,
                &header_label,
                rctx.style.tiny(),
                theme.text_dim,
            );

            let copy_rect = Rect::from_center_size(
                Pos2::new(hdr.right() - 28.0, hdr.center().y),
                Vec2::new(42.0, 18.0),
            );
            let copy_resp = ui.interact(
                copy_rect,
                ui.scope_id().with(("md_copy_code", source_line)),
                Sense::click(),
            );
            if copy_resp.hovered() {
                ui.painter()
                    .rect_filled(copy_rect, CornerRadius::same(4), theme.hover);
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().text(
                copy_rect.center(),
                Align2::CENTER_CENTER,
                "复制",
                rctx.style.tiny(),
                if copy_resp.hovered() {
                    theme.accent
                } else {
                    theme.text_dim
                },
            );
            if copy_resp.clicked() {
                rctx.events.push(PreviewEvent::CopyText(code.to_string()));
            }

            // Highlighted code body
            let mut job = LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            let mono = FontId::new((rctx.base_size * 0.92).round(), rctx.style.mono_family.clone());
            let line_h = Some((rctx.base_size * 1.4).round());

            if code.is_empty() {
                job.append(
                    " ",
                    0.0,
                    TextFormat {
                        font_id: mono,
                        color: theme.syn_text,
                        line_height: line_h,
                        ..Default::default()
                    },
                );
            } else if spans.is_empty() {
                job.append(
                    code,
                    0.0,
                    TextFormat {
                        font_id: mono,
                        color: theme.syn_text,
                        line_height: line_h,
                        ..Default::default()
                    },
                );
            } else {
                let mut cursor = 0usize;
                for span in spans {
                    let s = span.start.min(code.len());
                    let e = span.end.min(code.len());
                    if s > cursor {
                        job.append(
                            &code[cursor..s],
                            0.0,
                            TextFormat {
                                font_id: mono.clone(),
                                color: theme.syn_text,
                                line_height: line_h,
                                ..Default::default()
                            },
                        );
                    }
                    if e > s {
                        job.append(
                            &code[s..e],
                            0.0,
                            TextFormat {
                                font_id: mono.clone(),
                                color: theme.token_color(span.kind),
                                line_height: line_h,
                                ..Default::default()
                            },
                        );
                    }
                    cursor = cursor.max(e);
                }
                if cursor < code.len() {
                    job.append(
                        &code[cursor..],
                        0.0,
                        TextFormat {
                            font_id: mono,
                            color: theme.syn_text,
                            line_height: line_h,
                            ..Default::default()
                        },
                    );
                }
            }

            Frame::new()
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ScrollArea::horizontal()
                        .id_salt(("md_code_hscroll", source_line))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(job).selectable(true));
                        });
                });
        });
}

fn render_math_block(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    tex: &str,
    source_line: usize,
    max_w: f32,
) {
    let theme = rctx.theme;
    let formatted = format_latex_unicode(tex);
    Frame::new()
        .fill(theme.panel_bg.gamma_multiply(0.75))
        .stroke(Stroke::new(1.0, theme.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width((max_w - 32.0).max(100.0));
            ui.vertical_centered(|ui| {
                let resp = ui.add(
                    egui::Label::new(
                        egui::RichText::new(&formatted)
                            .font(FontId::new(
                                rctx.base_size * 1.12,
                                rctx.style.mono_family.clone(),
                            ))
                            .color(theme.text),
                    )
                    .selectable(true),
                );
                if resp.hovered() {
                    resp.on_hover_text(format!("LaTeX: {tex}"));
                }
            });
            let _ = source_line;
        });
}

fn render_blockquote(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    callout: Option<&(CalloutKind, Option<String>)>,
    inner: &[Block],
    max_w: f32,
) {
    let theme = rctx.theme;
    if let Some((kind, custom_title)) = callout {
        let (accent_col, icon) = match kind {
            CalloutKind::Note | CalloutKind::Info => (theme.accent, Icon::Info),
            CalloutKind::Tip => (theme.success, Icon::Lightbulb),
            CalloutKind::Important => (theme.syn_keyword, Icon::Info),
            CalloutKind::Warning | CalloutKind::Caution => (theme.warning, Icon::Warning),
            CalloutKind::Danger | CalloutKind::Bug => (theme.error, Icon::Error),
            CalloutKind::Example => (theme.syn_type, Icon::FileCode),
            CalloutKind::Quote => (theme.text_dim, Icon::Output),
        };
        let bg = Color32::from_rgba_unmultiplied(
            accent_col.r(),
            accent_col.g(),
            accent_col.b(),
            if theme.kind.is_dark() { 26 } else { 20 },
        );
        let resp = Frame::new()
            .fill(bg)
            .stroke(Stroke::new(1.0, accent_col.gamma_multiply(0.35)))
            .corner_radius(CornerRadius::same(7))
            .inner_margin(Margin {
                left: 16,
                right: 12,
                top: 10,
                bottom: 8,
            })
            .show(ui, |ui| {
                let inner_w = (max_w - 28.0).max(100.0);
                ui.set_width(inner_w);
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                    icons::paint(ui.painter(), ir, icon, accent_col, 1.4);
                    let title = custom_title
                        .as_deref()
                        .unwrap_or_else(|| kind.default_title());
                    ui.label(
                        egui::RichText::new(title)
                            .font(FontId::new(
                                rctx.base_size,
                                rctx.style.ui_bold_family.clone(),
                            ))
                            .color(accent_col),
                    );
                });
                if !inner.is_empty() {
                    ui.add_space(4.0);
                    render_blocks(ui, rctx, inner, inner_w, true);
                }
            })
            .response;
        let bar = Rect::from_min_size(resp.rect.min, Vec2::new(3.5, resp.rect.height()));
        ui.painter().rect_filled(
            bar,
            CornerRadius {
                nw: 7,
                sw: 7,
                ne: 0,
                se: 0,
            },
            accent_col,
        );
    } else {
        let bg = theme.panel_bg.gamma_multiply(0.5);
        let resp = Frame::new()
            .fill(bg)
            .inner_margin(Margin {
                left: 16,
                right: 12,
                top: 8,
                bottom: 6,
            })
            .show(ui, |ui| {
                let inner_w = (max_w - 28.0).max(100.0);
                ui.set_width(inner_w);
                render_blocks(ui, rctx, inner, inner_w, true);
            })
            .response;
        let bar = Rect::from_min_size(resp.rect.min, Vec2::new(3.0, resp.rect.height()));
        ui.painter()
            .rect_filled(bar, CornerRadius::same(1), theme.border_strong);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_list(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    ordered: bool,
    start: usize,
    tight: bool,
    items: &[crate::markdown::ListItem],
    max_w: f32,
    depth: usize,
) {
    let theme = rctx.theme;
    for (idx, item) in items.iter().enumerate() {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let marker_w = if ordered { 26.0 } else { 20.0 };
            let (m_rect, m_resp) =
                ui.allocate_exact_size(Vec2::new(marker_w, rctx.base_size * 1.45), Sense::click());

            if let Some(checked) = item.checked {
                let box_r = Rect::from_center_size(m_rect.center(), Vec2::splat(14.0));
                if checked {
                    ui.painter()
                        .rect_filled(box_r, CornerRadius::same(3), theme.accent);
                    icons::paint(
                        ui.painter(),
                        box_r.shrink(2.0),
                        Icon::Check,
                        theme.accent_text,
                        1.5,
                    );
                } else {
                    ui.painter().rect_filled(
                        box_r,
                        CornerRadius::same(3),
                        if m_resp.hovered() {
                            theme.hover
                        } else {
                            theme.input_bg
                        },
                    );
                    ui.painter().rect_stroke(
                        box_r,
                        CornerRadius::same(3),
                        Stroke::new(1.2, theme.border_strong),
                        StrokeKind::Inside,
                    );
                }
                if m_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if m_resp.clicked()
                    && let Some(byte_offset) = item.checkbox_byte
                {
                    rctx.events.push(PreviewEvent::ToggleTask {
                        byte_offset,
                        checked: !checked,
                    });
                }
            } else if ordered {
                ui.painter().text(
                    Pos2::new(m_rect.right() - 2.0, m_rect.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{}.", start + idx),
                    FontId::new(rctx.base_size, rctx.style.ui_family.clone()),
                    theme.text_dim,
                );
            } else {
                let bullet = match depth % 3 {
                    0 => "•",
                    1 => "◦",
                    _ => "▪",
                };
                ui.painter().text(
                    m_rect.center(),
                    Align2::CENTER_CENTER,
                    bullet,
                    FontId::new(rctx.base_size * 1.05, rctx.style.ui_family.clone()),
                    theme.accent,
                );
            }

            ui.vertical(|ui| {
                let item_w = (max_w - marker_w - 8.0).max(100.0);
                ui.set_width(item_w);
                for (b_idx, b) in item.blocks.iter().enumerate() {
                    match b {
                        Block::List {
                            ordered: sub_ord,
                            start: sub_start,
                            tight: sub_tight,
                            items: sub_items,
                            ..
                        } => {
                            if b_idx > 0 {
                                ui.add_space(2.0);
                            }
                            render_list(
                                ui,
                                rctx,
                                *sub_ord,
                                *sub_start,
                                *sub_tight,
                                sub_items,
                                item_w,
                                depth + 1,
                            );
                        }
                        _ => {
                            render_blocks(ui, rctx, std::slice::from_ref(b), item_w, true);
                        }
                    }
                }
            });
        });
        ui.add_space(if tight { 3.0 } else { 7.0 });
    }
}

fn render_table(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    alignments: &[Align],
    headers: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    source_line: usize,
    max_w: f32,
) {
    let theme = rctx.theme;
    let cols = alignments.len().max(headers.len()).max(1);
    let min_col_w = 110.0;
    let natural_w = (cols as f32 * min_col_w).max(max_w);
    let col_w = ((natural_w / cols as f32) - 2.0).max(80.0);

    ScrollArea::horizontal()
        .id_salt(("md_table", source_line))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new()
                .stroke(Stroke::new(1.0, theme.border_strong))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::ZERO)
                .show(ui, |ui| {
                    // Header row
                    let hdr_resp = ui
                        .horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for c in 0..cols {
                                let align = alignments.get(c).copied().unwrap_or(Align::Left);
                                Frame::new()
                                    .fill(theme.panel_bg)
                                    .inner_margin(Margin::symmetric(10, 7))
                                    .show(ui, |ui| {
                                        ui.set_width((col_w - 20.0).max(60.0));
                                        let inlines =
                                            headers.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
                                        render_inlines_flow(
                                            ui,
                                            rctx,
                                            inlines,
                                            (col_w - 20.0).max(60.0),
                                            rctx.base_size * 0.94,
                                            true,
                                            theme.text,
                                            Some(align),
                                        );
                                    });
                            }
                        })
                        .response;

                    ui.painter().line_segment(
                        [
                            Pos2::new(hdr_resp.rect.left(), hdr_resp.rect.bottom()),
                            Pos2::new(hdr_resp.rect.right(), hdr_resp.rect.bottom()),
                        ],
                        Stroke::new(1.0, theme.border_strong),
                    );

                    // Body rows
                    for (r_idx, row) in rows.iter().enumerate() {
                        let bg = if r_idx % 2 == 1 {
                            theme.panel_bg.gamma_multiply(0.45)
                        } else {
                            theme.bg
                        };
                        let row_resp = ui
                            .horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for c in 0..cols {
                                    let align = alignments.get(c).copied().unwrap_or(Align::Left);
                                    Frame::new()
                                        .fill(bg)
                                        .inner_margin(Margin::symmetric(10, 6))
                                        .show(ui, |ui| {
                                            ui.set_width((col_w - 20.0).max(60.0));
                                            let inlines =
                                                row.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
                                            render_inlines_flow(
                                                ui,
                                                rctx,
                                                inlines,
                                                (col_w - 20.0).max(60.0),
                                                rctx.base_size * 0.93,
                                                false,
                                                theme.text,
                                                Some(align),
                                            );
                                        });
                                }
                            })
                            .response;

                        if r_idx + 1 < rows.len() {
                            ui.painter().line_segment(
                                [
                                    Pos2::new(row_resp.rect.left(), row_resp.rect.bottom()),
                                    Pos2::new(row_resp.rect.right(), row_resp.rect.bottom()),
                                ],
                                Stroke::new(1.0, theme.border),
                            );
                        }
                    }

                    // Column dividers
                    let full_r = ui.min_rect();
                    for c in 1..cols {
                        let x = full_r.left() + c as f32 * col_w;
                        if x < full_r.right() - 2.0 {
                            ui.painter().line_segment(
                                [Pos2::new(x, full_r.top()), Pos2::new(x, full_r.bottom())],
                                Stroke::new(1.0, theme.border),
                            );
                        }
                    }
                });
        });
}

#[allow(clippy::too_many_arguments)]
fn render_inlines_flow(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    inlines: &[Inline],
    max_w: f32,
    font_size: f32,
    default_bold: bool,
    default_color: Color32,
    align: Option<Align>,
) {
    // Split into contiguous text chunks vs image groups so consecutive inline badges
    // (`<p align="center"><a><img/></a> <a><img/></a></p>`) flow horizontally instead of stacking.
    struct PendingImg<'a> {
        alt: &'a str,
        url: &'a str,
        title: Option<&'a str>,
        width: Option<f32>,
        link_url: Option<&'a str>,
    }

    let mut text_chunk: Vec<Inline> = Vec::new();
    let mut img_row: Vec<PendingImg<'_>> = Vec::new();

    let flush_text = |ui: &mut Ui, rctx: &mut RenderContext<'_>, chunk: &mut Vec<Inline>| {
        if chunk.is_empty() {
            return;
        }
        // Skip whitespace-only separators between consecutive badge images.
        let only_ws = chunk.iter().all(|item| {
            matches!(item, Inline::SoftBreak)
                || matches!(item, Inline::Text(t) if t.trim().is_empty())
        });
        if !only_ws {
            render_text_chunk(
                ui,
                rctx,
                chunk,
                max_w,
                font_size,
                default_bold,
                default_color,
                align,
            );
        }
        chunk.clear();
    };

    let flush_images = |ui: &mut Ui, rctx: &mut RenderContext<'_>, imgs: &mut Vec<PendingImg<'_>>| {
        if imgs.is_empty() {
            return;
        }
        if imgs.len() == 1 {
            let img = &imgs[0];
            if matches!(align, Some(Align::Center)) {
                let est_w = estimate_image_width(rctx, img.url, img.width, max_w, false);
                let pad = ((max_w - est_w) * 0.5).max(0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.5 {
                        ui.add_space(pad);
                    }
                    render_image_card(
                        ui,
                        rctx,
                        img.alt,
                        img.url,
                        img.title,
                        img.width,
                        img.link_url,
                        max_w,
                        false,
                    );
                });
            } else {
                render_image_card(
                    ui,
                    rctx,
                    img.alt,
                    img.url,
                    img.title,
                    img.width,
                    img.link_url,
                    max_w,
                    false,
                );
            }
            ui.add_space(6.0);
        } else {
            // Pack consecutive badges/images into wrapped lines <= max_w, centering each line when aligned.
            let gap = 6.0f32;
            let mut rows: Vec<(Vec<usize>, f32)> = Vec::new();
            let mut cur_row: Vec<usize> = Vec::new();
            let mut cur_w = 0.0f32;
            for (idx, img) in imgs.iter().enumerate() {
                let w = estimate_image_width(rctx, img.url, img.width, max_w, true);
                let add = if cur_row.is_empty() { w } else { gap + w };
                if !cur_row.is_empty() && cur_w + add > max_w {
                    rows.push((std::mem::take(&mut cur_row), cur_w));
                    cur_w = w;
                    cur_row.push(idx);
                } else {
                    cur_w += add;
                    cur_row.push(idx);
                }
            }
            if !cur_row.is_empty() {
                rows.push((cur_row, cur_w));
            }
            for (row_indices, row_w) in rows {
                let pad = match align {
                    Some(Align::Center) => ((max_w - row_w) * 0.5).max(0.0),
                    Some(Align::Right) => (max_w - row_w).max(0.0),
                    _ => 0.0,
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(gap, 4.0);
                    if pad > 0.5 {
                        ui.add_space((pad - gap).max(0.0));
                    }
                    for idx in row_indices {
                        let img = &imgs[idx];
                        render_image_card(
                            ui,
                            rctx,
                            img.alt,
                            img.url,
                            img.title,
                            img.width,
                            img.link_url,
                            max_w,
                            true,
                        );
                    }
                });
                ui.add_space(4.0);
            }
            ui.add_space(4.0);
        }
        imgs.clear();
    };

    for item in inlines {
        match item {
            Inline::Image {
                alt,
                url,
                title,
                width,
            } => {
                flush_text(ui, rctx, &mut text_chunk);
                img_row.push(PendingImg {
                    alt,
                    url,
                    title: title.as_deref(),
                    width: *width,
                    link_url: None,
                });
            }
            Inline::Link { text, url, title }
                if text.len() == 1 && matches!(text.first(), Some(Inline::Image { .. })) =>
            {
                flush_text(ui, rctx, &mut text_chunk);
                if let Some(Inline::Image {
                    alt,
                    url: img_url,
                    title: img_title,
                    width,
                }) = text.first()
                {
                    img_row.push(PendingImg {
                        alt,
                        url: img_url,
                        title: img_title.as_deref().or(title.as_deref()),
                        width: *width,
                        link_url: Some(url.as_str()),
                    });
                }
            }
            Inline::SoftBreak if !img_row.is_empty() => {}
            Inline::Text(t) if !img_row.is_empty() && t.trim().is_empty() => {}
            other => {
                flush_images(ui, rctx, &mut img_row);
                text_chunk.push(other.clone());
            }
        }
    }
    flush_images(ui, rctx, &mut img_row);
    flush_text(ui, rctx, &mut text_chunk);
}

#[derive(Clone, Copy)]
struct InlineStyle {
    size: f32,
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    highlight: bool,
    sup: bool,
    sub: bool,
    color: Color32,
    bg: Option<Color32>,
}

#[allow(clippy::too_many_arguments)]
fn render_text_chunk(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    inlines: &[Inline],
    max_w: f32,
    font_size: f32,
    default_bold: bool,
    default_color: Color32,
    align: Option<Align>,
) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_w.max(40.0);
    job.halign = match align {
        Some(Align::Center) => EguiAlign::Center,
        Some(Align::Right) => EguiAlign::RIGHT,
        _ => EguiAlign::LEFT,
    };

    let mut links: Vec<LinkSpan> = Vec::new();
    let mut char_count = 0usize;

    let base_style = InlineStyle {
        size: font_size,
        bold: default_bold,
        italic: false,
        strike: false,
        underline: false,
        highlight: false,
        sup: false,
        sub: false,
        color: default_color,
        bg: None,
    };

    append_inlines(
        inlines,
        base_style,
        rctx,
        &mut job,
        &mut links,
        &mut char_count,
    );

    if job.text.is_empty() {
        return;
    }

    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let size = galley.size();
    let alloc_w = if align.is_some() {
        max_w.max(40.0)
    } else {
        size.x.min(max_w.max(40.0))
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(alloc_w, size.y), Sense::click());
    // In egui, `job.halign = Center` places the galley origin at its horizontal center
    // (`[-w/2, +w/2]`), and `RIGHT` places it at its right edge (`[-w, 0]`).
    let draw_pos = match align {
        Some(Align::Center) => Pos2::new(rect.center().x, rect.top()),
        Some(Align::Right) => Pos2::new(rect.right(), rect.top()),
        _ => rect.min,
    };
    ui.painter().galley(draw_pos, galley.clone(), default_color);

    if !links.is_empty() && resp.hovered()
        && let Some(pos) = ui.ctx().input(|i| i.pointer.hover_pos())
    {
        let local = pos - draw_pos;
        let cc = galley.cursor_from_pos(local);
        let idx = cc.index.0;
        if let Some(link) = links
            .iter()
            .find(|l| idx >= l.char_start && idx < l.char_end)
        {
            if !link.url.is_empty() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let tip = match (&link.title, link.url.is_empty()) {
                (Some(t), true) => t.clone(),
                (Some(t), false) => format!("{t}\n{}", link.url),
                (None, _) => link.url.clone(),
            };
            if !tip.is_empty() {
                resp.clone().on_hover_text(tip);
            }
            if resp.clicked() && !link.url.is_empty() {
                if let Some(fn_id) = &link.is_footnote {
                    rctx.state.pending_scroll_to = Some(format!("fn:{fn_id}"));
                } else {
                    dispatch_link(rctx, &link.url);
                }
            }
        }
    }
}

fn append_inlines(
    inlines: &[Inline],
    st: InlineStyle,
    rctx: &RenderContext<'_>,
    job: &mut LayoutJob,
    links: &mut Vec<LinkSpan>,
    char_count: &mut usize,
) {
    let theme = rctx.theme;
    let line_h = Some((st.size * 1.48).round());
    let make_fmt = |s: InlineStyle, family: FontFamily, bg: Color32| TextFormat {
        font_id: FontId::new(
            if s.sup || s.sub {
                (s.size * 0.78).round().max(9.0)
            } else {
                s.size
            },
            family,
        ),
        line_height: line_h,
        color: s.color,
        background: bg,
        expand_bg: if bg == Color32::TRANSPARENT { 1.0 } else { 2.5 },
        italics: s.italic,
        underline: if s.underline {
            Stroke::new(1.0, s.color)
        } else {
            Stroke::NONE
        },
        strikethrough: if s.strike {
            Stroke::new(1.1, s.color.gamma_multiply(0.85))
        } else {
            Stroke::NONE
        },
        valign: if s.sup {
            EguiAlign::TOP
        } else {
            EguiAlign::Center
        },
        ..Default::default()
    };

    for item in inlines {
        match item {
            Inline::Text(text) => {
                let family = if st.bold {
                    rctx.style.ui_bold_family.clone()
                } else {
                    rctx.style.ui_family.clone()
                };
                let bg = st.bg.unwrap_or(if st.highlight {
                    theme.find_match
                } else {
                    Color32::TRANSPARENT
                });
                job.append(text, 0.0, make_fmt(st, family, bg));
                *char_count += text.chars().count();
            }
            Inline::SoftBreak => {
                let family = rctx.style.ui_family.clone();
                job.append(" ", 0.0, make_fmt(st, family, Color32::TRANSPARENT));
                *char_count += 1;
            }
            Inline::HardBreak => {
                let family = rctx.style.ui_family.clone();
                job.append("\n", 0.0, make_fmt(st, family, Color32::TRANSPARENT));
                *char_count += 1;
            }
            Inline::Code(code) => {
                let padded = format!(" {code} ");
                let mut cs = st;
                cs.size = (st.size * 0.92).round().max(10.0);
                cs.color = theme.syn_string;
                let bg = if theme.kind.is_dark() {
                    theme.active
                } else {
                    theme.panel_bg
                };
                job.append(&padded, 0.0, make_fmt(cs, rctx.style.mono_family.clone(), bg));
                *char_count += padded.chars().count();
            }
            Inline::Kbd(key) => {
                let padded = format!(" {key} ");
                let mut ks = st;
                ks.size = (st.size * 0.86).round().max(10.0);
                ks.bold = true;
                ks.color = theme.text;
                job.append(
                    &padded,
                    0.0,
                    make_fmt(ks, rctx.style.mono_family.clone(), theme.active),
                );
                *char_count += padded.chars().count();
            }
            Inline::Math(tex) => {
                let formatted = format!(" {} ", format_latex_unicode(tex));
                let mut ms = st;
                ms.italic = true;
                ms.color = theme.syn_type;
                job.append(
                    &formatted,
                    0.0,
                    make_fmt(ms, rctx.style.mono_family.clone(), theme.panel_bg),
                );
                *char_count += formatted.chars().count();
            }
            Inline::Strong(children) => {
                let mut next = st;
                next.bold = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Emphasis(children) => {
                let mut next = st;
                next.italic = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Strikethrough(children) => {
                let mut next = st;
                next.strike = true;
                next.color = st.color.gamma_multiply(0.72);
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Highlight(children) => {
                let mut next = st;
                next.highlight = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Underline(children) => {
                let mut next = st;
                next.underline = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Superscript(children) => {
                let mut next = st;
                next.sup = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Subscript(children) => {
                let mut next = st;
                next.sub = true;
                append_inlines(children, next, rctx, job, links, char_count);
            }
            Inline::Link { text, url, title } => {
                let start = *char_count;
                let mut next = st;
                next.color = theme.accent;
                next.underline = true;
                append_inlines(text, next, rctx, job, links, char_count);
                let end = *char_count;
                if end > start {
                    links.push(LinkSpan {
                        char_start: start,
                        char_end: end,
                        url: url.clone(),
                        title: title.clone(),
                        is_footnote: None,
                    });
                }
            }
            Inline::FootnoteRef { id, number } => {
                let start = *char_count;
                let badge = format!("[{number}]");
                let mut fs = st;
                fs.sup = true;
                fs.color = theme.accent;
                job.append(&badge, 0.0, make_fmt(fs, rctx.style.mono_family.clone(), Color32::TRANSPARENT));
                *char_count += badge.chars().count();
                let end = *char_count;
                links.push(LinkSpan {
                    char_start: start,
                    char_end: end,
                    url: format!("跳转到脚注 [{number}]"),
                    title: None,
                    is_footnote: Some(id.clone()),
                });
            }
            Inline::Image { alt, .. } => {
                let label = if alt.is_empty() { "[图片]" } else { alt.as_str() };
                let family = rctx.style.ui_family.clone();
                job.append(label, 0.0, make_fmt(st, family, Color32::TRANSPARENT));
                *char_count += label.chars().count();
            }
            Inline::Styled {
                children,
                color,
                bg,
                scale,
                bold,
                italic,
                underline,
                strike,
                title,
            } => {
                let start = *char_count;
                let mut next = st;
                if let Some(c) = color {
                    next.color = *c;
                }
                if let Some(b) = bg {
                    next.bg = Some(*b);
                }
                if let Some(s) = scale {
                    next.size = (st.size * *s).round().max(9.0);
                }
                next.bold |= *bold;
                next.italic |= *italic;
                next.underline |= *underline;
                next.strike |= *strike;
                append_inlines(children, next, rctx, job, links, char_count);
                let end = *char_count;
                if let Some(tip) = title
                    && end > start
                {
                    links.push(LinkSpan {
                        char_start: start,
                        char_end: end,
                        url: String::new(),
                        title: Some(tip.clone()),
                        is_footnote: None,
                    });
                }
            }
        }
    }
}

fn fit_image_dims(nat_w: f32, nat_h: f32, explicit_width: Option<f32>, max_w: f32) -> (f32, f32) {
    let nat_w = nat_w.max(1.0);
    let nat_h = nat_h.max(1.0);
    let target_w = explicit_width.unwrap_or(nat_w).min(max_w).max(16.0);
    let target_h = (target_w * (nat_h / nat_w)).clamp(12.0, 1800.0);
    let final_w = (target_h * (nat_w / nat_h)).min(max_w);
    (final_w, target_h)
}

fn cache_key_for_image(rctx: &RenderContext<'_>, url: &str) -> Option<(String, bool)> {
    let is_remote =
        url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:");
    if is_remote {
        Some((url.to_string(), true))
    } else {
        resolve_local_path(url, rctx.doc_dir, rctx.workspace_root)
            .map(|p| (p.to_string_lossy().to_string(), false))
    }
}

fn estimate_image_width(
    rctx: &RenderContext<'_>,
    url: &str,
    explicit_width: Option<f32>,
    max_w: f32,
    inline_row: bool,
) -> f32 {
    if let Some((key, _)) = cache_key_for_image(rctx, url)
        && let Some(cached) = rctx.state.textures.get(&key)
    {
        match cached {
            CachedImage::Loaded { size, .. } => {
                return fit_image_dims(size[0] as f32, size[1] as f32, explicit_width, max_w).0;
            }
            CachedImage::Svg { cache, .. } => {
                return fit_image_dims(
                    cache.graphic.width,
                    cache.graphic.height,
                    explicit_width,
                    max_w,
                )
                .0;
            }
            CachedImage::Failed(_) => {}
        }
    }
    if let Some(w) = explicit_width {
        return w.min(max_w);
    }
    if inline_row {
        150.0f32.min(max_w)
    } else {
        260.0f32.min(max_w)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_image_card(
    ui: &mut Ui,
    rctx: &mut RenderContext<'_>,
    alt: &str,
    url: &str,
    title: Option<&str>,
    explicit_width: Option<f32>,
    link_url: Option<&str>,
    max_w: f32,
    inline_row: bool,
) {
    let theme = rctx.theme;
    let is_remote =
        url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:");

    let mut fail_reason: Option<String> = None;
    let mut is_loading = false;

    if is_remote {
        if !rctx.state.textures.contains_key(url) {
            rctx.state.remote_images.request(url, ui.ctx());
            is_loading = rctx.state.remote_images.is_loading(url);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
    } else if let Some(resolved) = resolve_local_path(url, rctx.doc_dir, rctx.workspace_root) {
        let key = resolved.to_string_lossy().to_string();
        let disk_mtime = std::fs::metadata(&resolved).and_then(|m| m.modified()).ok();
        let needs_load = match rctx.state.textures.get(&key) {
            Some(CachedImage::Loaded { modified, .. } | CachedImage::Svg { modified, .. }) => {
                *modified != disk_mtime
            }
            Some(CachedImage::Failed(_)) => false,
            None => true,
        };
        if needs_load {
            let entry = load_local_texture(ui.ctx(), &resolved, disk_mtime);
            rctx.state.textures.insert(key, entry);
        }
    }

    if let Some((key, _)) = cache_key_for_image(rctx, url) {
        match rctx.state.textures.get(&key) {
            Some(CachedImage::Failed(err)) => {
                fail_reason = Some(err.clone());
            }
            Some(CachedImage::Loaded { texture, size, .. }) => {
                let (final_w, target_h) =
                    fit_image_dims(size[0] as f32, size[1] as f32, explicit_width, max_w);
                let (img_rect, resp) =
                    ui.allocate_exact_size(Vec2::new(final_w, target_h), Sense::click());
                ui.painter().image(
                    texture.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                // Large content images get a subtle hairline border; compact badges stay clean.
                if target_h > 44.0 {
                    ui.painter().rect_stroke(
                        img_rect,
                        CornerRadius::same(6),
                        Stroke::new(1.0, theme.border),
                        StrokeKind::Inside,
                    );
                }
                if let Some(target_link) = link_url {
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        dispatch_link(rctx, target_link);
                    }
                }
                if let Some(t) = title {
                    resp.on_hover_text(t);
                } else if !alt.is_empty() {
                    resp.on_hover_text(alt);
                }
                return;
            }
            Some(CachedImage::Svg { cache, .. }) => {
                let (final_w, target_h) = fit_image_dims(
                    cache.graphic.width,
                    cache.graphic.height,
                    explicit_width,
                    max_w,
                );
                let (img_rect, resp) =
                    ui.allocate_exact_size(Vec2::new(final_w, target_h), Sense::click());
                crate::markdown::svg::paint_svg(
                    ui,
                    cache,
                    img_rect,
                    &rctx.style.ui_family,
                    &rctx.style.ui_bold_family,
                );
                if let Some(target_link) = link_url {
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        dispatch_link(rctx, target_link);
                    }
                }
                if let Some(t) = title {
                    resp.on_hover_text(t);
                } else if !alt.is_empty() {
                    resp.on_hover_text(alt);
                }
                return;
            }
            None => {}
        }
    }

    // Compact loading or fallback pill (width-capped so multi-badge rows never overflow).
    let pill_max_w = if inline_row {
        150.0f32.min(max_w)
    } else {
        260.0f32.min(max_w)
    };
    let short_label = if is_loading {
        if alt.is_empty() {
            "加载图片…".to_string()
        } else {
            format!("加载中: {alt}")
        }
    } else if alt.is_empty() {
        url.to_string()
    } else {
        alt.to_string()
    };
    let (galley, _) = crate::ui::widgets::ellipsize_mode(
        ui,
        &short_label,
        rctx.style.tiny(),
        if is_loading {
            theme.text_muted
        } else {
            theme.text_dim
        },
        (pill_max_w - 34.0).max(24.0),
        crate::ui::widgets::EllipsizeMode::End,
    );
    let pill_w = (galley.rect.width() + 34.0).min(pill_max_w).max(52.0);
    let pill_h = if inline_row { 24.0 } else { 28.0 };
    let (pill_rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), Sense::click());
    ui.painter()
        .rect_filled(pill_rect, CornerRadius::same(5), theme.panel_bg);
    ui.painter().rect_stroke(
        pill_rect,
        CornerRadius::same(5),
        Stroke::new(1.0, theme.border),
        StrokeKind::Inside,
    );
    let icon_r = Rect::from_center_size(
        Pos2::new(pill_rect.left() + 13.0, pill_rect.center().y),
        Vec2::splat(13.0),
    );
    icons::paint(
        ui.painter(),
        icon_r,
        if is_loading { Icon::Refresh } else { Icon::Globe },
        if fail_reason.is_some() {
            theme.warning
        } else {
            theme.accent
        },
        1.2,
    );
    ui.painter().galley(
        Pos2::new(
            pill_rect.left() + 24.0,
            pill_rect.center().y - galley.rect.height() * 0.5,
        ),
        galley,
        theme.text_dim,
    );
    let tip = match &fail_reason {
        Some(err) => format!("{short_label}\n{url}\n({err})"),
        None => format!("{short_label}\n{url}"),
    };
    let resp = resp.on_hover_text(tip);
    if let Some(target_link) = link_url.or(is_remote.then_some(url)) {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            dispatch_link(rctx, target_link);
        }
    }
}

fn load_local_texture(
    ctx: &egui::Context,
    path: &Path,
    modified: Option<SystemTime>,
) -> CachedImage {
    let Ok(bytes) = std::fs::read(path) else {
        return CachedImage::Failed("无法读取图片文件".to_string());
    };
    if bytes.len() > 16 * 1024 * 1024 {
        return CachedImage::Failed("图片体积超过 16 MB".to_string());
    }
    if crate::markdown::svg::looks_like_svg(&bytes) {
        return match crate::markdown::svg::parse_svg(&bytes) {
            Ok(graphic) => CachedImage::Svg {
                cache: crate::markdown::svg::SvgRenderCache::new(
                    ctx,
                    &path.to_string_lossy(),
                    std::sync::Arc::new(graphic),
                ),
                modified,
            },
            Err(err) => CachedImage::Failed(err),
        };
    }
    match image::load_from_memory(&bytes) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color_image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let texture = ctx.load_texture(
                path.to_string_lossy(),
                color_image,
                TextureOptions::LINEAR,
            );
            CachedImage::Loaded {
                texture,
                size,
                modified,
            }
        }
        Err(err) => CachedImage::Failed(err.to_string()),
    }
}

fn resolve_local_path(
    url: &str,
    doc_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Option<PathBuf> {
    let clean = url.split('#').next().unwrap_or(url).trim();
    if clean.is_empty() {
        return None;
    }
    let rel = PathBuf::from(clean);
    if rel.is_absolute() && rel.exists() {
        return Some(rel);
    }
    if let Some(dir) = doc_dir {
        let p = dir.join(&rel);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(root) = workspace_root {
        let p = root.join(&rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn dispatch_link(rctx: &mut RenderContext<'_>, url: &str) {
    let trimmed = url.trim();
    if let Some(anchor) = trimmed.strip_prefix('#') {
        rctx.state.pending_scroll_to = Some(anchor.to_lowercase());
        return;
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("mailto:")
    {
        rctx.events.push(PreviewEvent::OpenUrl(trimmed.to_string()));
        return;
    }
    let (file_part, anchor) = match trimmed.split_once('#') {
        Some((f, a)) => (f, Some(a.to_lowercase())),
        None => (trimmed, None),
    };
    if let Some(path) = resolve_local_path(file_part, rctx.doc_dir, rctx.workspace_root) {
        rctx.events.push(PreviewEvent::OpenFile { path, anchor });
    }
}
