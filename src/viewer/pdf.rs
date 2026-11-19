//! Native PDF viewer powered by Windows `Windows.Data.Pdf` (WinRT) on Windows
//! with asynchronous background page rasterization and bounded GPU texture cache.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{
    Align2, Color32, ColorImage, CornerRadius, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind,
    TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};

use crate::ui::UiStyle;
use crate::viewer::ViewerEvent;
use crate::viewer::image_view::format_bytes;

enum PdfRequest {
    Render {
        page: u32,
        width_px: u32,
        request_id: u64,
    },
    Unlock(String),
}

enum PdfResponse {
    Loaded {
        page_count: u32,
    },
    NeedsPassword,
    Error(String),
    Rendered {
        page: u32,
        width_px: u32,
        rgba: Vec<u8>,
        size: [usize; 2],
        request_id: u64,
    },
}

const MAX_CACHED_PAGES: usize = 12;

pub struct PdfViewer {
    path: PathBuf,
    file_bytes: u64,
    page_count: u32,
    current_page: u32,
    page_input: String,
    zoom: f32,
    fit_width: bool,
    needs_password: bool,
    password_input: String,
    loading: bool,
    error: Option<String>,
    tx: Option<Sender<PdfRequest>>,
    rx: Option<Receiver<PdfResponse>>,
    cache: HashMap<(u32, u32), TextureHandle>,
    cache_order: VecDeque<(u32, u32)>,
    pending_request: Option<(u32, u32, u64)>,
    next_request_id: u64,
}

impl PdfViewer {
    pub fn load(path: &Path) -> Self {
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(err) => {
                return Self::with_error(path, file_bytes, format!("无法读取 PDF 文件: {err}"));
            }
        };

        let (req_tx, req_rx) = channel::<PdfRequest>();
        let (res_tx, res_rx) = channel::<PdfResponse>();

        spawn_pdf_worker(bytes, req_rx, res_tx);

        Self {
            path: path.to_path_buf(),
            file_bytes,
            page_count: 0,
            current_page: 0,
            page_input: "1".to_string(),
            zoom: 1.0,
            fit_width: true,
            needs_password: false,
            password_input: String::new(),
            loading: true,
            error: None,
            tx: Some(req_tx),
            rx: Some(res_rx),
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            pending_request: None,
            next_request_id: 1,
        }
    }

    fn with_error(path: &Path, file_bytes: u64, error: String) -> Self {
        Self {
            path: path.to_path_buf(),
            file_bytes,
            page_count: 0,
            current_page: 0,
            page_input: "1".to_string(),
            zoom: 1.0,
            fit_width: true,
            needs_password: false,
            password_input: String::new(),
            loading: false,
            error: Some(error),
            tx: None,
            rx: None,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            pending_request: None,
            next_request_id: 1,
        }
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else {
            return;
        };
        let mut received = false;
        while let Ok(msg) = rx.try_recv() {
            received = true;
            match msg {
                PdfResponse::Loaded { page_count } => {
                    self.page_count = page_count;
                    self.loading = false;
                    self.needs_password = false;
                    self.error = None;
                }
                PdfResponse::NeedsPassword => {
                    self.loading = false;
                    self.needs_password = true;
                }
                PdfResponse::Error(err) => {
                    self.loading = false;
                    self.error = Some(err);
                }
                PdfResponse::Rendered {
                    page,
                    width_px,
                    rgba,
                    size,
                    request_id,
                } => {
                    let color = ColorImage::from_rgba_unmultiplied(size, &rgba);
                    let tex = ctx.load_texture(
                        format!("pdf:{}:{page}:{width_px}", self.path.display()),
                        color,
                        TextureOptions::LINEAR,
                    );
                    let key = (page, width_px);
                    self.cache.insert(key, tex);
                    self.cache_order.retain(|k| k != &key);
                    self.cache_order.push_back(key);
                    while self.cache_order.len() > MAX_CACHED_PAGES {
                        if let Some(old) = self.cache_order.pop_front() {
                            self.cache.remove(&old);
                        }
                    }
                    if self
                        .pending_request
                        .is_some_and(|(_, _, id)| id == request_id)
                    {
                        self.pending_request = None;
                    }
                }
            }
        }
        if received {
            ctx.request_repaint();
        }
    }

    fn request_render(&mut self, page: u32, width_px: u32) {
        if self.page_count == 0 || page >= self.page_count {
            return;
        }
        let width_px = width_px.clamp(240, 2600);
        if self.cache.contains_key(&(page, width_px)) {
            return;
        }
        if self
            .pending_request
            .is_some_and(|(p, w, _)| p == page && w == width_px)
        {
            return;
        }
        if let Some(tx) = &self.tx {
            let id = self.next_request_id;
            self.next_request_id += 1;
            let _ = tx.send(PdfRequest::Render {
                page,
                width_px,
                request_id: id,
            });
            self.pending_request = Some((page, width_px, id));
        }
    }

    fn go_to_page(&mut self, page: u32) {
        if self.page_count == 0 {
            return;
        }
        self.current_page = page.min(self.page_count - 1);
        self.page_input = (self.current_page + 1).to_string();
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        let mut events = Vec::new();
        let theme = style.theme;
        self.poll_worker(ui.ctx());

        let full_rect = ui.max_rect();
        ui.painter().rect_filled(full_rect, 0.0, theme.bg);

        // Toolbar
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

        let info = if self.page_count > 0 {
            format!(
                "PDF · 共 {} 页 · {}",
                self.page_count,
                format_bytes(self.file_bytes)
            )
        } else {
            format!("PDF · {}", format_bytes(self.file_bytes))
        };
        ui.painter().text(
            Pos2::new(bar_rect.left() + 14.0, bar_rect.center().y),
            Align2::LEFT_CENTER,
            info,
            style.tiny(),
            theme.text_dim,
        );

        let mut rx = bar_rect.right() - 10.0;
        let cy = bar_rect.center().y;
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "pdf_btn",
            &mut rx,
            cy,
            "复制路径",
            false,
            60.0,
            "复制 PDF 路径",
        ) {
            events.push(ViewerEvent::CopyText(
                self.path.to_string_lossy().to_string(),
            ));
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "pdf_btn",
            &mut rx,
            cy,
            "100%",
            !self.fit_width && (self.zoom - 1.0).abs() < 0.01,
            44.0,
            "原始比例",
        ) {
            self.fit_width = false;
            self.zoom = 1.0;
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "pdf_btn",
            &mut rx,
            cy,
            "适应宽度",
            self.fit_width,
            62.0,
            "自适应视图宽度",
        ) {
            self.fit_width = true;
            self.zoom = 1.0;
        }
        if crate::viewer::toolbar_btn(ui, style, "pdf_btn", &mut rx, cy, "+", false, 24.0, "放大")
        {
            self.fit_width = false;
            self.zoom = (self.zoom * 1.2).min(3.0);
        }
        let pct = if self.fit_width {
            "适应".to_string()
        } else {
            format!("{}%", (self.zoom * 100.0).round() as i32)
        };
        rx -= 44.0;
        ui.painter().text(
            Pos2::new(rx + 22.0, cy),
            Align2::CENTER_CENTER,
            &pct,
            style.tiny(),
            theme.text,
        );
        if crate::viewer::toolbar_btn(ui, style, "pdf_btn", &mut rx, cy, "-", false, 24.0, "缩小")
        {
            self.fit_width = false;
            self.zoom = (self.zoom / 1.2).max(0.4);
        }

        if self.page_count > 0 {
            if crate::viewer::toolbar_btn(
                ui,
                style,
                "pdf_btn",
                &mut rx,
                cy,
                "下一页",
                false,
                50.0,
                "下一页 (PageDown)",
            ) {
                self.go_to_page(self.current_page + 1);
            }
            rx -= 34.0;
            ui.painter().text(
                Pos2::new(rx + 17.0, cy),
                Align2::CENTER_CENTER,
                format!("/ {}", self.page_count),
                style.tiny(),
                theme.text_dim,
            );
            rx -= 42.0;
            let page_r = Rect::from_center_size(Pos2::new(rx + 19.0, cy), Vec2::new(38.0, 22.0));
            let p_resp = ui.put(
                page_r,
                TextEdit::singleline(&mut self.page_input)
                    .font(style.tiny())
                    .margin(Vec2::new(4.0, 3.0)),
            );
            if p_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Ok(p) = self.page_input.trim().parse::<u32>()
                    && p >= 1
                {
                    self.go_to_page(p - 1);
                } else {
                    self.page_input = (self.current_page + 1).to_string();
                }
            }
            if crate::viewer::toolbar_btn(
                ui,
                style,
                "pdf_btn",
                &mut rx,
                cy,
                "上一页",
                false,
                50.0,
                "上一页 (PageUp)",
            ) {
                self.go_to_page(self.current_page.saturating_sub(1));
            }
        }

        // Password prompt if encrypted
        if self.needs_password {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("此 PDF 已加密，请输入密码解锁")
                            .font(style.ui())
                            .color(theme.text),
                    );
                    ui.add_space(8.0);
                    let resp = ui.add_sized(
                        Vec2::new(240.0, 28.0),
                        TextEdit::singleline(&mut self.password_input).password(true),
                    );
                    ui.add_space(8.0);
                    if (ui.button("解锁").clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                        && let Some(tx) = &self.tx
                    {
                        self.loading = true;
                        let _ = tx.send(PdfRequest::Unlock(self.password_input.clone()));
                    }
                });
            });
            return events;
        }

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

        if self.loading || self.page_count == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("正在加载 PDF 文档…")
                        .font(style.ui())
                        .color(theme.text_dim),
                );
            });
            ui.ctx().request_repaint();
            return events;
        }

        // Page keyboard navigation & Ctrl+Scroll zoom
        if ui.input(|i| i.modifiers.ctrl && i.smooth_scroll_delta.y.abs() > 0.1) {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            self.fit_width = false;
            self.zoom = (self.zoom * if dy > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.4, 3.0);
        }

        let avail_w = ui.available_width().max(240.0);
        let ppp = ui.ctx().pixels_per_point();
        let target_logical_w = if self.fit_width {
            (avail_w - 48.0).clamp(240.0, 1100.0)
        } else {
            (760.0 * self.zoom).clamp(240.0, 2200.0)
        };
        // Snap render width to 40px increments so minor window resizing doesn't re-render constantly
        let target_px = (((target_logical_w * ppp) / 40.0).round() as u32 * 40).clamp(320, 2400);
        self.request_render(self.current_page, target_px);

        // Find best available cached texture for current page (exact or closest width)
        let exact = self.cache.get(&(self.current_page, target_px)).cloned();
        let fallback = if exact.is_none() {
            self.cache
                .iter()
                .find(|((p, _), _)| *p == self.current_page)
                .map(|(_, tex)| tex.clone())
        } else {
            None
        };
        let tex = exact.or(fallback);

        ScrollArea::both()
            .id_salt(("pdf_scroll", &self.path, self.current_page))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(16.0);
                if let Some(texture) = tex {
                    let sz = texture.size();
                    let aspect = sz[1].max(1) as f32 / sz[0].max(1) as f32;
                    let draw_w = target_logical_w;
                    let draw_h = draw_w * aspect;
                    let pad_x = ((avail_w - draw_w) * 0.5).max(20.0);
                    ui.horizontal(|ui| {
                        ui.add_space(pad_x);
                        let (img_rect, _) =
                            ui.allocate_exact_size(Vec2::new(draw_w, draw_h), Sense::hover());
                        ui.painter().rect_filled(
                            img_rect.expand(1.0),
                            CornerRadius::same(4),
                            Color32::WHITE,
                        );
                        ui.painter().image(
                            texture.id(),
                            img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        ui.painter().rect_stroke(
                            img_rect,
                            CornerRadius::same(2),
                            Stroke::new(1.0, theme.border_strong),
                            StrokeKind::Inside,
                        );
                    });
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(60.0);
                        ui.label(
                            egui::RichText::new(format!("正在渲染第 {} 页…", self.current_page + 1))
                                .font(style.ui())
                                .color(theme.text_dim),
                        );
                    });
                    ui.ctx().request_repaint();
                }
                ui.add_space(24.0);
            });

        events
    }
}

#[cfg(windows)]
fn spawn_pdf_worker(bytes: Vec<u8>, req_rx: Receiver<PdfRequest>, res_tx: Sender<PdfResponse>) {
    std::thread::spawn(move || {
        use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
        use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        use windows::core::HSTRING;

        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        let load_doc = |pdf_bytes: &[u8], password: Option<&str>| -> windows::core::Result<PdfDocument> {
            let stream = InMemoryRandomAccessStream::new()?;
            let writer = DataWriter::CreateDataWriter(&stream)?;
            writer.WriteBytes(pdf_bytes)?;
            writer.StoreAsync()?.join()?;
            writer.FlushAsync()?.join()?;
            let _ = writer.DetachStream();
            stream.Seek(0)?;
            if let Some(pw) = password {
                PdfDocument::LoadFromStreamWithPasswordAsync(&stream, &HSTRING::from(pw))?.join()
            } else {
                PdfDocument::LoadFromStreamAsync(&stream)?.join()
            }
        };

        let mut doc = match load_doc(&bytes, None) {
            Ok(d) => d,
            Err(err) => {
                // HRESULT 0x8007052B (ERROR_WRONG_PASSWORD) or password protected
                if err.code().0 as u32 == 0x8007_052B || err.message().to_lowercase().contains("password") {
                    let _ = res_tx.send(PdfResponse::NeedsPassword);
                    loop {
                        match req_rx.recv() {
                            Ok(PdfRequest::Unlock(pw)) => match load_doc(&bytes, Some(&pw)) {
                                Ok(d) => break d,
                                Err(_) => {
                                    let _ = res_tx.send(PdfResponse::NeedsPassword);
                                }
                            },
                            Ok(PdfRequest::Render { .. }) => {}
                            Err(_) => return,
                        }
                    }
                } else {
                    let _ = res_tx.send(PdfResponse::Error(format!("打开 PDF 失败: {err}")));
                    return;
                }
            }
        };

        let page_count = doc.PageCount().unwrap_or(0);
        let _ = res_tx.send(PdfResponse::Loaded { page_count });

        while let Ok(req) = req_rx.recv() {
            match req {
                PdfRequest::Unlock(pw) => {
                    if let Ok(d) = load_doc(&bytes, Some(&pw)) {
                        doc = d;
                        let page_count = doc.PageCount().unwrap_or(0);
                        let _ = res_tx.send(PdfResponse::Loaded { page_count });
                    }
                }
                PdfRequest::Render {
                    page,
                    width_px,
                    request_id,
                } => {
                    if page >= page_count {
                        continue;
                    }
                    let render_res = (|| -> Result<(Vec<u8>, [usize; 2]), String> {
                        let pdf_page = doc.GetPage(page).map_err(|e| e.to_string())?;
                        let out_stream =
                            InMemoryRandomAccessStream::new().map_err(|e| e.to_string())?;
                        let opts = PdfPageRenderOptions::new().map_err(|e| e.to_string())?;
                        opts.SetDestinationWidth(width_px)
                            .map_err(|e| e.to_string())?;
                        pdf_page
                            .RenderWithOptionsToStreamAsync(&out_stream, &opts)
                            .map_err(|e| e.to_string())?
                            .join()
                            .map_err(|e| e.to_string())?;
                        let out_size = out_stream.Size().map_err(|e| e.to_string())? as usize;
                        out_stream.Seek(0).map_err(|e| e.to_string())?;
                        let reader =
                            DataReader::CreateDataReader(&out_stream).map_err(|e| e.to_string())?;
                        reader
                            .LoadAsync(out_size as u32)
                            .map_err(|e| e.to_string())?
                            .join()
                            .map_err(|e| e.to_string())?;
                        let mut buf = vec![0u8; out_size];
                        reader.ReadBytes(&mut buf).map_err(|e| e.to_string())?;
                        let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?;
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        Ok((rgba.into_raw(), size))
                    })();

                    match render_res {
                        Ok((rgba, size)) => {
                            let _ = res_tx.send(PdfResponse::Rendered {
                                page,
                                width_px,
                                rgba,
                                size,
                                request_id,
                            });
                        }
                        Err(err) => {
                            let _ = res_tx.send(PdfResponse::Error(format!("渲染页面失败: {err}")));
                        }
                    }
                }
            }
        }
    });
}

#[cfg(not(windows))]
fn spawn_pdf_worker(_bytes: Vec<u8>, _req_rx: Receiver<PdfRequest>, res_tx: Sender<PdfResponse>) {
    let _ = res_tx.send(PdfResponse::Error(
        "当前平台暂不支持原生 PDF 渲染，请在系统默认查看器中打开".to_string(),
    ));
}
