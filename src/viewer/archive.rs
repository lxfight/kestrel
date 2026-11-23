//! ZIP / JAR / WHL / APK / NUPKG archive viewer powered by `zip_util::ZipArchive`.

use std::path::{Path, PathBuf};

use egui::{Align2, FontId, Pos2, Rect, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2};

use crate::ui::UiStyle;
use crate::viewer::ViewerEvent;
use crate::viewer::image_view::format_bytes;
use crate::viewer::zip_util::ZipArchive;

#[derive(Clone, Debug)]
struct ArchiveEntryRow {
    name: String,
    uncomp_size: usize,
    comp_size: usize,
    method: u16,
    is_dir: bool,
}

pub struct ArchiveViewer {
    path: PathBuf,
    file_size: u64,
    archive: Option<ZipArchive>,
    entries: Vec<ArchiveEntryRow>,
    total_uncomp: u64,
    total_comp: u64,
    search: String,
    selected: Option<String>,
    preview_text: Option<String>,
    error: Option<String>,
}

/// 解压归档内部条目到临时目录，返回可直接交给打开流程的临时文件路径。
/// 条目名先做安全清理：去掉盘符与前导 `/`、统一分隔符、拒绝 `..` 路径段。
pub(crate) fn extract_entry(
    archive: &ZipArchive,
    entry: &str,
    archive_path: &Path,
) -> Result<PathBuf, String> {
    let normalized = entry.replace('\\', "/");
    if normalized.ends_with('/') {
        return Err(format!("目录条目无法打开: {entry}"));
    }
    let mut body = normalized.as_str();
    if body.len() > 2 && body.as_bytes()[1] == b':' && body.as_bytes()[0].is_ascii_alphabetic() {
        body = &body[2..];
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in body.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(format!("非法条目路径: {entry}")),
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        return Err(format!("非法条目路径: {entry}"));
    }
    let mut temp = std::env::temp_dir()
        .join("kestrel-archive")
        .join(safe_archive_name(archive_path));
    for segment in &segments {
        temp.push(segment);
    }
    let bytes = archive.read_entry(entry)?;
    if let Some(parent) = temp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建临时目录失败: {e}"))?;
    }
    std::fs::write(&temp, bytes).map_err(|e| format!("写出临时文件失败: {e}"))?;
    Ok(temp)
}

fn safe_archive_name(archive_path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    archive_path.hash(&mut h);
    let digest = h.finish();
    let stem: String = archive_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}-{digest:08x}")
}

impl ArchiveViewer {
    pub fn load(path: &Path) -> Self {
        const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if file_size > MAX_ARCHIVE_BYTES {
            return Self {
                path: path.to_path_buf(),
                file_size,
                archive: None,
                entries: Vec::new(),
                total_uncomp: 0,
                total_comp: 0,
                search: String::new(),
                selected: None,
                preview_text: None,
                error: Some("压缩包体积超过 64 MB 预览限制".to_string()),
            };
        }
        let Ok(bytes) = std::fs::read(path) else {
            return Self {
                path: path.to_path_buf(),
                file_size,
                archive: None,
                entries: Vec::new(),
                total_uncomp: 0,
                total_comp: 0,
                search: String::new(),
                selected: None,
                preview_text: None,
                error: Some("无法读取压缩包文件".to_string()),
            };
        };
        match ZipArchive::from_bytes(bytes) {
            Ok(arc) => {
                let mut total_uncomp = 0u64;
                let mut total_comp = 0u64;
                let entries: Vec<ArchiveEntryRow> = arc
                    .entry_metas()
                    .into_iter()
                    .map(|(name, uncomp_size, comp_size, method)| {
                        total_uncomp += uncomp_size as u64;
                        total_comp += comp_size as u64;
                        let is_dir = name.ends_with('/');
                        ArchiveEntryRow {
                            name,
                            uncomp_size,
                            comp_size,
                            method,
                            is_dir,
                        }
                    })
                    .collect();
                Self {
                    path: path.to_path_buf(),
                    file_size,
                    archive: Some(arc),
                    entries,
                    total_uncomp,
                    total_comp,
                    search: String::new(),
                    selected: None,
                    preview_text: None,
                    error: None,
                }
            }
            Err(err) => Self {
                path: path.to_path_buf(),
                file_size,
                archive: None,
                entries: Vec::new(),
                total_uncomp: 0,
                total_comp: 0,
                search: String::new(),
                selected: None,
                preview_text: None,
                error: Some(err),
            },
        }
    }

    fn select_entry(&mut self, name: &str) {
        self.selected = Some(name.to_string());
        self.preview_text = None;
        let Some(arc) = &self.archive else {
            return;
        };
        if name.ends_with('/') {
            return;
        }
        if let Ok(bytes) = arc.read_entry(name) {
            if bytes.len() > 256 * 1024 {
                self.preview_text = Some(format!(
                    "条目较大 ({})，仅展示元数据。",
                    format_bytes(bytes.len() as u64)
                ));
            } else if crate::doc::is_binary_bytes(&bytes) {
                self.preview_text = Some(format!(
                    "该条目为二进制内容 ({})，无法以文本预览。",
                    format_bytes(bytes.len() as u64)
                ));
            } else {
                self.preview_text = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, style: &UiStyle) -> Vec<ViewerEvent> {
        let mut events = Vec::new();
        let theme = style.theme;

        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::hover());
        ui.painter().rect_filled(bar_rect, 0.0, theme.panel_bg);
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.left(), bar_rect.bottom() - 0.5),
                Pos2::new(bar_rect.right(), bar_rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "压缩归档".to_string());
        let ratio = if self.total_uncomp > 0 {
            ((1.0 - self.total_comp as f64 / self.total_uncomp as f64) * 100.0).max(0.0)
        } else {
            0.0
        };
        let info = format!(
            "{}  ·  {} 项  ·  原始 {}  ·  压缩后 {} (-{:.0}%)",
            name,
            self.entries.len(),
            format_bytes(self.total_uncomp),
            format_bytes(self.file_size),
            ratio,
        );
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
            "arc_btn",
            &mut rx,
            cy,
            "复制路径",
            false,
            60.0,
            "复制归档路径",
        ) {
            events.push(ViewerEvent::CopyText(
                self.path.to_string_lossy().to_string(),
            ));
        }

        rx -= 182.0;
        let search_r = Rect::from_center_size(Pos2::new(rx + 88.0, cy), Vec2::new(172.0, 22.0));
        ui.put(
            search_r,
            TextEdit::singleline(&mut self.search)
                .hint_text("筛选归档条目…")
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

        let needle = self.search.trim().to_ascii_lowercase();
        let filtered: Vec<&ArchiveEntryRow> = self
            .entries
            .iter()
            .filter(|e| needle.is_empty() || e.name.to_ascii_lowercase().contains(&needle))
            .collect();

        let has_preview = self.preview_text.is_some();
        let avail_h = ui.available_height();
        let list_h = if has_preview {
            (avail_h * 0.56).max(120.0)
        } else {
            avail_h.max(80.0)
        };

        let mut clicked_entry: Option<String> = None;
        let mut open_entry: Option<String> = None;
        let row_h = 24.0;
        let total = filtered.len();
        ScrollArea::vertical()
            .id_salt(("arc_list", &self.path))
            .max_height(list_h)
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total, |ui, range| {
                ui.set_min_width(ui.available_width());
                for idx in range {
                    let row = filtered[idx];
                    let is_sel = self.selected.as_deref() == Some(row.name.as_str());
                    let (r, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
                    if is_sel {
                        ui.painter().rect_filled(r, 0.0, theme.active);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, theme.hover);
                    }
                    ui.painter().text(
                        Pos2::new(r.left() + 14.0, r.center().y),
                        Align2::LEFT_CENTER,
                        if row.is_dir {
                            format!("📁 {}", row.name)
                        } else {
                            row.name.clone()
                        },
                        FontId::new(12.0, style.mono_family.clone()),
                        if is_sel {
                            theme.text
                        } else if row.is_dir {
                            theme.text_dim
                        } else {
                            theme.text
                        },
                    );
                    if !row.is_dir {
                        let method = match row.method {
                            0 => "存储",
                            8 => "Deflate",
                            _ => "压缩",
                        };
                        let meta = format!(
                            "{}  →  {}  ({})",
                            format_bytes(row.uncomp_size as u64),
                            format_bytes(row.comp_size as u64),
                            method
                        );
                        ui.painter().text(
                            Pos2::new(r.right() - 14.0, r.center().y),
                            Align2::RIGHT_CENTER,
                            meta,
                            style.tiny(),
                            theme.text_faint,
                        );
                    }
                    if resp.clicked() {
                        clicked_entry = Some(row.name.clone());
                    }
                    if !row.is_dir {
                        if resp.double_clicked() {
                            open_entry = Some(row.name.clone());
                        }
                        resp.context_menu(|ui| {
                            ui.set_width(180.0);
                            if ui.button("打开内部文件").clicked() {
                                open_entry = Some(row.name.clone());
                                ui.close();
                            }
                        });
                    }
                }
            });

        if let Some(entry_name) = clicked_entry {
            self.select_entry(&entry_name);
        }
        if let Some(entry_name) = open_entry {
            let extracted = match &self.archive {
                Some(archive) => extract_entry(archive, &entry_name, &self.path),
                None => Err("归档未加载".to_string()),
            };
            match extracted {
                Ok(temp) => events.push(ViewerEvent::OpenPath(temp)),
                Err(err) => self.preview_text = Some(format!("无法打开内部文件: {err}")),
            }
        }

        if let Some(preview) = &self.preview_text {
            let sep_rect = ui
                .allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover())
                .0;
            ui.painter().rect_filled(sep_rect, 0.0, theme.border);
            let title = self.selected.as_deref().unwrap_or("");
            let (hdr_r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
            ui.painter().rect_filled(hdr_r, 0.0, theme.panel_bg);
            ui.painter().text(
                Pos2::new(hdr_r.left() + 12.0, hdr_r.center().y),
                Align2::LEFT_CENTER,
                format!("预览: {title}"),
                style.tiny(),
                theme.text_dim,
            );
            let mut copy_btn = hdr_r.right() - 8.0;
            if crate::viewer::toolbar_btn(
                ui,
                style,
                "arc_prev",
                &mut copy_btn,
                hdr_r.center().y,
                "复制内容",
                false,
                60.0,
                "复制当前预览文本",
            ) {
                events.push(ViewerEvent::CopyText(preview.clone()));
            }
            ScrollArea::both()
                .id_salt(("arc_preview", &self.path))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(preview)
                                .font(FontId::new(12.0, style.mono_family.clone()))
                                .color(theme.text),
                        );
                    });
                    ui.add_space(8.0);
                });
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::zip_util::build_test_zip;

    fn write_temp_zip(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kestrel-archive-test-{name}.zip"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn extract_entry_writes_nested_entry_to_temp_dir() {
        let bytes = build_test_zip(&[
            ("docs/notes/readme.txt", b"hello archive"),
            ("top.txt", b"top"),
        ]);
        let archive = ZipArchive::from_bytes(bytes.clone()).unwrap();
        let zip_path = write_temp_zip("nested", &bytes);

        let extracted = extract_entry(&archive, "docs/notes/readme.txt", &zip_path).unwrap();
        assert_eq!(std::fs::read(&extracted).unwrap(), b"hello archive");
        assert!(extracted.starts_with(std::env::temp_dir().join("kestrel-archive")));
        assert!(extracted.ends_with("docs/notes/readme.txt"));

        let _ = std::fs::remove_file(&extracted);
        let _ = std::fs::remove_file(&zip_path);
    }

    #[test]
    fn extract_entry_rejects_escaping_entry_path() {
        let bytes = build_test_zip(&[("..\\evil.txt", b"boom")]);
        let archive = ZipArchive::from_bytes(bytes.clone()).unwrap();
        let zip_path = write_temp_zip("traversal", &bytes);

        let err = extract_entry(&archive, "..\\evil.txt", &zip_path).unwrap_err();
        assert!(err.contains("非法"), "unexpected error: {err}");
        assert!(!std::env::temp_dir().join("kestrel-archive").join("evil.txt").exists());

        let _ = std::fs::remove_file(&zip_path);
    }
}
