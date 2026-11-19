//! Multimodal file viewers: Images, Word (`.docx`), Excel (`.xlsx` / `.xlsm`), CSV/TSV, and PDF.

pub mod archive;
pub mod docx;
pub mod image_view;
pub mod pdf;
pub mod xlsx;
pub mod zip_util;

use std::path::Path;

use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::ui::UiStyle;

use archive::ArchiveViewer;
use docx::DocxViewer;
use image_view::ImageViewer;
use pdf::PdfViewer;
use xlsx::SpreadsheetViewer;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MultimodalKind {
    Image,
    Pdf,
    Docx,
    Xlsx,
    Csv,
    Tsv,
    Archive,
}

impl MultimodalKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" => {
                Some(Self::Image)
            }
            "pdf" => Some(Self::Pdf),
            "docx" => Some(Self::Docx),
            "xlsx" | "xlsm" => Some(Self::Xlsx),
            "csv" => Some(Self::Csv),
            "tsv" => Some(Self::Tsv),
            "zip" | "jar" | "whl" | "apk" | "nupkg" | "vsix" | "xpi" => Some(Self::Archive),
            _ => None,
        }
    }

    /// Binary formats that should not be loaded into the text `Rope` buffer.
    pub fn is_binary(self) -> bool {
        !matches!(self, Self::Csv | Self::Tsv)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "图片",
            Self::Pdf => "PDF 文档",
            Self::Docx => "Word 文档",
            Self::Xlsx => "Excel 表格",
            Self::Csv => "CSV 表格",
            Self::Tsv => "TSV 表格",
            Self::Archive => "ZIP 压缩归档",
        }
    }
}

#[derive(Clone, Debug)]
pub enum ViewerEvent {
    CopyText(String),
    OpenUrl(String),
}

pub enum MultimodalViewer {
    Image(ImageViewer),
    Pdf(PdfViewer),
    Docx(DocxViewer),
    Spreadsheet(SpreadsheetViewer),
    Archive(ArchiveViewer),
}

impl MultimodalViewer {
    pub fn load_binary(path: &Path, kind: MultimodalKind) -> Self {
        match kind {
            MultimodalKind::Image => Self::Image(ImageViewer::load(path)),
            MultimodalKind::Pdf => Self::Pdf(PdfViewer::load(path)),
            MultimodalKind::Docx => Self::Docx(DocxViewer::load(path)),
            MultimodalKind::Xlsx => Self::Spreadsheet(SpreadsheetViewer::load_xlsx(path)),
            MultimodalKind::Csv => Self::Spreadsheet(SpreadsheetViewer::load_csv(path, "", ',')),
            MultimodalKind::Tsv => Self::Spreadsheet(SpreadsheetViewer::load_csv(path, "", '\t')),
            MultimodalKind::Archive => Self::Archive(ArchiveViewer::load(path)),
        }
    }

    pub fn load_tabular_text(path: &Path, text: &str, kind: MultimodalKind) -> Self {
        let delim = if kind == MultimodalKind::Tsv { '\t' } else { ',' };
        Self::Spreadsheet(SpreadsheetViewer::load_csv(path, text, delim))
    }

    pub fn kind(&self) -> MultimodalKind {
        match self {
            Self::Image(_) => MultimodalKind::Image,
            Self::Pdf(_) => MultimodalKind::Pdf,
            Self::Docx(_) => MultimodalKind::Docx,
            Self::Spreadsheet(_) => MultimodalKind::Xlsx,
            Self::Archive(_) => MultimodalKind::Archive,
        }
    }

    pub fn is_showing_source(&self) -> bool {
        matches!(self, Self::Spreadsheet(s) if s.show_source)
    }

    pub fn set_show_source(&mut self, show: bool) {
        if let Self::Spreadsheet(s) = self {
            s.show_source = show;
        }
    }

    pub fn sync_tabular_text(&mut self, text: &str, kind: MultimodalKind) {
        if let Self::Spreadsheet(s) = self {
            let delim = if kind == MultimodalKind::Tsv { '\t' } else { ',' };
            s.refresh_csv(text, delim);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        match self {
            Self::Image(v) => v.ui(ui, style),
            Self::Pdf(v) => v.ui(ui, style),
            Self::Docx(v) => v.ui(ui, style),
            Self::Spreadsheet(v) => v.ui(ui, style),
            Self::Archive(v) => v.ui(ui, style),
        }
    }
}

/// Shared right-aligned pill button helper for multimodal viewer toolbars.
pub(crate) fn toolbar_btn(
    ui: &mut Ui,
    style: &UiStyle,
    id_ns: &str,
    rx: &mut f32,
    center_y: f32,
    label: &str,
    active: bool,
    w: f32,
    tip: &str,
) -> bool {
    let theme = style.theme;
    *rx -= w + 4.0;
    let r = Rect::from_center_size(Pos2::new(*rx + w * 0.5, center_y), Vec2::new(w, 22.0));
    let resp = ui.interact(r, ui.scope_id().with((id_ns, label, tip)), Sense::click());
    let bg = if active {
        theme.accent
    } else if resp.hovered() {
        theme.hover
    } else {
        theme.input_bg
    };
    ui.painter().rect_filled(r, CornerRadius::same(4), bg);
    if !active {
        ui.painter().rect_stroke(
            r,
            CornerRadius::same(4),
            Stroke::new(1.0, theme.border),
            StrokeKind::Inside,
        );
    }
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        style.tiny(),
        if active {
            theme.accent_text
        } else {
            theme.text_dim
        },
    );
    resp.on_hover_text(tip).clicked()
}
