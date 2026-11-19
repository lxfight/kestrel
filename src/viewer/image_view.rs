//! Image viewer supporting PNG, JPEG, GIF, WEBP, BMP, ICO, and TIFF with zoom, pan, rotate, and flip.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, ColorImage, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, Vec2,
};

use crate::ui::UiStyle;
use crate::viewer::ViewerEvent;

pub struct ImageViewer {
    path: PathBuf,
    raw_img: Option<image::DynamicImage>,
    texture: Option<TextureHandle>,
    size: [usize; 2],
    file_bytes: u64,
    format_name: String,
    zoom: f32,
    fit_mode: bool,
    pan: Vec2,
    error: Option<String>,
}

impl ImageViewer {
    pub fn load(path: &Path) -> Self {
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let format_name = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("IMAGE")
            .to_ascii_uppercase();

        let (raw_img, size, error) = match std::fs::read(path) {
            Ok(bytes) => {
                if bytes.len() > 64 * 1024 * 1024 {
                    (None, [0, 0], Some("图片体积超过 64 MB 限制".to_string()))
                } else {
                    match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let sz = [img.width() as usize, img.height() as usize];
                            (Some(img), sz, None)
                        }
                        Err(err) => (None, [0, 0], Some(format!("解码图片失败: {err}"))),
                    }
                }
            }
            Err(err) => (None, [0, 0], Some(format!("读取文件失败: {err}"))),
        };

        Self {
            path: path.to_path_buf(),
            raw_img,
            texture: None,
            size,
            file_bytes,
            format_name,
            zoom: 1.0,
            fit_mode: true,
            pan: Vec2::ZERO,
            error,
        }
    }

    fn ensure_texture(&mut self, ctx: &egui::Context) {
        if self.texture.is_some() {
            return;
        }
        if let Some(img) = &self.raw_img {
            let rgba = img.to_rgba8();
            let sz = [rgba.width() as usize, rgba.height() as usize];
            self.size = sz;
            let color_img = ColorImage::from_rgba_unmultiplied(sz, rgba.as_raw());
            let tex = ctx.load_texture(
                format!("img_view:{}", self.path.display()),
                color_img,
                TextureOptions::LINEAR,
            );
            self.texture = Some(tex);
        }
    }

    fn rotate(&mut self, cw: bool) {
        if let Some(img) = self.raw_img.take() {
            self.raw_img = Some(if cw { img.rotate90() } else { img.rotate270() });
            self.texture = None;
        }
    }

    fn flip_horizontal(&mut self) {
        if let Some(img) = self.raw_img.take() {
            self.raw_img = Some(img.fliph());
            self.texture = None;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        let mut events = Vec::new();
        let theme = style.theme;
        self.ensure_texture(ui.ctx());

        let full_rect = ui.max_rect();
        ui.painter().rect_filled(full_rect, 0.0, theme.bg);

        // Top Toolbar
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

        let info = if self.size[0] > 0 {
            format!(
                "{} · {} × {} px · {}",
                self.format_name,
                self.size[0],
                self.size[1],
                format_bytes(self.file_bytes)
            )
        } else {
            self.format_name.clone()
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
            "img_btn",
            &mut rx,
            cy,
            "复制路径",
            false,
            60.0,
            "复制图片文件路径",
        ) {
            events.push(ViewerEvent::CopyText(
                self.path.to_string_lossy().to_string(),
            ));
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "img_btn",
            &mut rx,
            cy,
            "水平翻转",
            false,
            60.0,
            "水平翻转预览",
        ) {
            self.flip_horizontal();
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "img_btn",
            &mut rx,
            cy,
            "右旋 90°",
            false,
            60.0,
            "顺时针旋转 90°",
        ) {
            self.rotate(true);
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "img_btn",
            &mut rx,
            cy,
            "左旋 90°",
            false,
            60.0,
            "逆时针旋转 90°",
        ) {
            self.rotate(false);
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "img_btn",
            &mut rx,
            cy,
            "100%",
            !self.fit_mode && (self.zoom - 1.0).abs() < 0.01,
            44.0,
            "实际大小 (1:1)",
        ) {
            self.fit_mode = false;
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
        }
        if crate::viewer::toolbar_btn(
            ui,
            style,
            "img_btn",
            &mut rx,
            cy,
            "适应窗口",
            self.fit_mode,
            62.0,
            "自适应窗口大小",
        ) {
            self.fit_mode = true;
            self.pan = Vec2::ZERO;
        }
        if crate::viewer::toolbar_btn(ui, style, "img_btn", &mut rx, cy, "+", false, 24.0, "放大")
        {
            self.fit_mode = false;
            self.zoom = (self.zoom * 1.25).min(8.0);
        }
        let pct = format!("{}%", (self.zoom * 100.0).round() as i32);
        rx -= 44.0;
        ui.painter().text(
            Pos2::new(rx + 22.0, cy),
            Align2::CENTER_CENTER,
            &pct,
            style.tiny(),
            theme.text,
        );
        if crate::viewer::toolbar_btn(ui, style, "img_btn", &mut rx, cy, "-", false, 24.0, "缩小")
        {
            self.fit_mode = false;
            self.zoom = (self.zoom / 1.25).max(0.1);
        }

        // Canvas
        let avail = ui.available_size();
        let (canvas_rect, canvas_resp) = ui.allocate_exact_size(
            Vec2::new(avail.x.max(1.0), avail.y.max(1.0)),
            Sense::click_and_drag(),
        );

        if let Some(err) = &self.error {
            ui.painter().text(
                canvas_rect.center(),
                Align2::CENTER_CENTER,
                err,
                style.ui(),
                theme.error,
            );
            return events;
        }

        let Some(texture) = &self.texture else {
            return events;
        };

        let nat_w = self.size[0].max(1) as f32;
        let nat_h = self.size[1].max(1) as f32;
        if self.fit_mode {
            let max_w = (canvas_rect.width() - 40.0).max(40.0);
            let max_h = (canvas_rect.height() - 40.0).max(40.0);
            self.zoom = (max_w / nat_w).min(max_h / nat_h).clamp(0.05, 1.0);
        }

        if canvas_resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let factor = if scroll > 0.0 { 1.12 } else { 1.0 / 1.12 };
                self.fit_mode = false;
                self.zoom = (self.zoom * factor).clamp(0.05, 8.0);
            }
        }
        if canvas_resp.dragged() {
            self.pan += canvas_resp.drag_delta();
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if canvas_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if canvas_resp.double_clicked() {
            self.fit_mode = !self.fit_mode;
            if !self.fit_mode {
                self.zoom = 1.0;
            }
            self.pan = Vec2::ZERO;
        }

        let draw_size = Vec2::new(nat_w * self.zoom, nat_h * self.zoom);
        let img_rect = Rect::from_center_size(canvas_rect.center() + self.pan, draw_size);
        let painter = ui.painter_at(canvas_rect);

        // Checkerboard background for transparent images
        paint_checkerboard(&painter, img_rect.intersect(canvas_rect), theme.kind.is_dark());
        painter.image(
            texture.id(),
            img_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.rect_stroke(
            img_rect,
            0.0,
            Stroke::new(1.0, theme.border_strong),
            StrokeKind::Inside,
        );

        events
    }
}

fn paint_checkerboard(painter: &egui::Painter, rect: Rect, dark: bool) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let (c1, c2) = if dark {
        (
            Color32::from_rgb(0x22, 0x25, 0x2C),
            Color32::from_rgb(0x2C, 0x30, 0x38),
        )
    } else {
        (
            Color32::from_rgb(0xF0, 0xF0, 0xF4),
            Color32::from_rgb(0xDE, 0xE0, 0xE6),
        )
    };
    painter.rect_filled(rect, 0.0, c1);
    let tile = 12.0;
    let cols = (rect.width() / tile).ceil() as usize;
    let rows = (rect.height() / tile).ceil() as usize;
    // Cap tile count to keep painting fast at huge zoom levels
    if cols.saturating_mul(rows) > 4000 {
        return;
    }
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 1 {
                let x = rect.left() + c as f32 * tile;
                let y = rect.top() + r as f32 * tile;
                let cell = Rect::from_min_max(
                    Pos2::new(x, y),
                    Pos2::new((x + tile).min(rect.right()), (y + tile).min(rect.bottom())),
                );
                painter.rect_filled(cell, 0.0, c2);
            }
        }
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
