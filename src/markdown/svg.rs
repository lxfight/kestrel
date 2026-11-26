//! Native lightweight SVG parser & egui renderer (backed by `roxmltree`) for
//! Markdown web badges (`shields.io`, `trendshift.io`, `hellogithub`), icons,
//! and local `.svg` diagrams without requiring external browser engines.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui,
};

use crate::markdown::parser::parse_css_color;

#[derive(Clone, Copy, Debug)]
struct Transform2D {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Transform2D {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn mul(self, rhs: Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    fn avg_scale(self) -> f32 {
        let sx = (self.a * self.a + self.b * self.b).sqrt();
        let sy = (self.c * self.c + self.d * self.d).sqrt();
        (sx + sy) * 0.5
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TextAnchor {
    Start,
    Middle,
    End,
}

#[derive(Clone, Debug)]
enum Paint {
    None,
    Solid(Color32),
}

#[derive(Clone, Debug)]
struct SvgStyle {
    fill: Paint,
    fill_opacity: f32,
    stroke: Paint,
    stroke_width: f32,
    stroke_opacity: f32,
    opacity: f32,
    font_size: f32,
    bold: bool,
    text_anchor: TextAnchor,
}

impl Default for SvgStyle {
    fn default() -> Self {
        Self {
            fill: Paint::Solid(Color32::BLACK),
            fill_opacity: 1.0,
            stroke: Paint::None,
            stroke_width: 1.0,
            stroke_opacity: 1.0,
            opacity: 1.0,
            font_size: 12.0,
            bold: false,
            text_anchor: TextAnchor::Start,
        }
    }
}

#[derive(Clone, Debug)]
enum SvgCmd {
    Rect {
        min: [f32; 2],
        max: [f32; 2],
        rx: f32,
        fill: Option<Color32>,
        stroke: Option<(Color32, f32)>,
    },
    Circle {
        center: [f32; 2],
        r: f32,
        fill: Option<Color32>,
        stroke: Option<(Color32, f32)>,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
        stroke: Color32,
        stroke_w: f32,
    },
    PathStroke {
        subpaths: Vec<(Vec<[f32; 2]>, bool)>,
        stroke: Color32,
        stroke_w: f32,
    },
    Text {
        pos: [f32; 2],
        text: String,
        size: f32,
        bold: bool,
        color: Color32,
        anchor: TextAnchor,
        text_length: Option<f32>,
    },
    EmbeddedImage {
        min: [f32; 2],
        max: [f32; 2],
        pixels: Arc<ColorImage>,
    },
}

struct PatternDraw {
    min: [f32; 2],
    max: [f32; 2],
    circle_clip: bool,
    uri: String,
}

/// Parsed vector graphic + optional pre-rasterized complex-path & pattern-image layers.
#[derive(Clone)]
pub struct SvgGraphic {
    pub width: f32,
    pub height: f32,
    pub corner_rx: f32,
    cmds: Vec<SvgCmd>,
    /// Rasterized RGBA layer for concave/filled `<path>` and `<polygon>` shapes.
    path_layer: Option<Arc<ColorImage>>,
    /// Composited RGBA layer for `<pattern><image/></pattern>` fills (e.g. contributor avatar grids).
    pattern_layer: Option<Arc<ColorImage>>,
}

pub struct SvgRenderCache {
    pub graphic: Arc<SvgGraphic>,
    pub path_tex: Option<TextureHandle>,
    pub pattern_tex: Option<TextureHandle>,
    pub embedded_tex: Vec<Option<TextureHandle>>,
}

impl SvgRenderCache {
    pub fn new(ctx: &egui::Context, key: &str, graphic: Arc<SvgGraphic>) -> Self {
        let path_tex = graphic.path_layer.as_ref().map(|img| {
            ctx.load_texture(
                format!("md_svg_path:{key}"),
                (**img).clone(),
                TextureOptions::LINEAR,
            )
        });
        let pattern_tex = graphic.pattern_layer.as_ref().map(|img| {
            ctx.load_texture(
                format!("md_svg_pat:{key}"),
                (**img).clone(),
                TextureOptions::LINEAR,
            )
        });
        let embedded_tex = graphic
            .cmds
            .iter()
            .enumerate()
            .map(|(idx, cmd)| {
                if let SvgCmd::EmbeddedImage { pixels, .. } = cmd {
                    Some(ctx.load_texture(
                        format!("md_svg_emb:{key}:{idx}"),
                        (**pixels).clone(),
                        TextureOptions::LINEAR,
                    ))
                } else {
                    None
                }
            })
            .collect();
        Self {
            graphic,
            path_tex,
            pattern_tex,
            embedded_tex,
        }
    }
}

pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let Ok(s) = std::str::from_utf8(head) else {
        return false;
    };
    let lower = s.trim_start_matches('\u{feff}').trim_start().to_ascii_lowercase();
    lower.starts_with("<svg") || (lower.starts_with("<?xml") && lower.contains("<svg"))
}

pub fn parse_svg(bytes: &[u8]) -> Result<SvgGraphic, String> {
    let raw = std::str::from_utf8(bytes).map_err(|_| "SVG 非有效 UTF-8 文本".to_string())?;
    let trimmed = raw.trim_start_matches('\u{feff}');
    let doc = roxmltree::Document::parse(trimmed).map_err(|e| format!("SVG XML 解析失败: {e}"))?;
    let root = doc.root_element();
    if !root.tag_name().name().eq_ignore_ascii_case("svg") {
        return Err("根节点不是 <svg>".to_string());
    }

    let view_box = root.attribute("viewBox").and_then(parse_view_box);
    let attr_w = root.attribute("width").and_then(parse_svg_len);
    let attr_h = root.attribute("height").and_then(parse_svg_len);

    let (vb_x, vb_y, vb_w, vb_h) = match (view_box, attr_w, attr_h) {
        (Some(vb), _, _) => vb,
        (None, Some(w), Some(h)) => (0.0, 0.0, w, h),
        (None, Some(w), None) => (0.0, 0.0, w, w),
        (None, None, Some(h)) => (0.0, 0.0, h, h),
        (None, None, None) => (0.0, 0.0, 120.0, 24.0),
    };
    let width = attr_w.unwrap_or(vb_w).clamp(8.0, 2400.0);
    let height = attr_h.unwrap_or(vb_h).clamp(8.0, 2400.0);

    let sx = if vb_w.abs() > 1e-4 { width / vb_w } else { 1.0 };
    let sy = if vb_h.abs() > 1e-4 { height / vb_h } else { 1.0 };
    let root_xf = Transform2D {
        a: sx,
        b: 0.0,
        c: 0.0,
        d: sy,
        e: -vb_x * sx,
        f: -vb_y * sy,
    };

    // Collect `<linearGradient>` / `<radialGradient>` colors, `<pattern>` image URIs,
    // `<clipPath>` radius, and element `id` map for `<use>`.
    let mut gradients: HashMap<String, Color32> = HashMap::new();
    let mut patterns: HashMap<String, String> = HashMap::new();
    let mut id_map: HashMap<&str, roxmltree::Node<'_, '_>> = HashMap::new();
    let mut corner_rx = 0.0f32;
    for node in doc.descendants() {
        if let Some(id) = node.attribute("id") {
            id_map.insert(id, node);
        }
        let name = node.tag_name().name();
        if (name == "linearGradient" || name == "radialGradient")
            && let Some(id) = node.attribute("id")
            && let Some(c) = resolve_gradient_color(node)
        {
            gradients.insert(id.to_string(), c);
        }
        if name == "pattern"
            && let Some(id) = node.attribute("id")
        {
            for child in node.descendants() {
                if child.tag_name().name() == "image"
                    && let Some(href) = child
                        .attribute("href")
                        .or_else(|| child.attribute(("http://www.w3.org/1999/xlink", "href")))
                {
                    patterns.insert(id.to_string(), href.to_string());
                    break;
                }
            }
        }
        if name == "clipPath" {
            for child in node.children() {
                if child.tag_name().name() == "rect"
                    && let Some(rx) = child
                        .attribute("rx")
                        .or_else(|| child.attribute("ry"))
                        .and_then(parse_svg_len)
                {
                    corner_rx = corner_rx.max(rx * sx);
                }
            }
        }
    }

    let mut cmds = Vec::new();
    let mut filled_paths: Vec<(Vec<Vec<[f32; 2]>>, Color32)> = Vec::new();
    let mut pattern_draws: Vec<PatternDraw> = Vec::new();
    let base_style = SvgStyle::default();
    walk_svg_node(
        root,
        root_xf,
        &base_style,
        &gradients,
        &patterns,
        &id_map,
        &mut cmds,
        &mut filled_paths,
        &mut pattern_draws,
        0,
    );

    let path_layer = if !filled_paths.is_empty() {
        rasterize_filled_paths(width, height, &filled_paths).map(Arc::new)
    } else {
        None
    };
    let pattern_layer = if !pattern_draws.is_empty() {
        rasterize_pattern_layer(width, height, &pattern_draws).map(Arc::new)
    } else {
        None
    };

    Ok(SvgGraphic {
        width,
        height,
        corner_rx: corner_rx.min(height * 0.5),
        cmds,
        path_layer,
        pattern_layer,
    })
}

fn fill_pattern_ref<'a>(
    node: roxmltree::Node<'_, '_>,
    patterns: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let raw = attr_or_style(node, "fill")?;
    let id = raw
        .trim()
        .strip_prefix("url(#")
        .and_then(|r| r.strip_suffix(')'))?;
    patterns.get(id).map(String::as_str)
}

#[allow(clippy::too_many_arguments)]
fn walk_svg_node<'a>(
    node: roxmltree::Node<'a, 'a>,
    xf: Transform2D,
    parent_style: &SvgStyle,
    gradients: &HashMap<String, Color32>,
    patterns: &HashMap<String, String>,
    id_map: &HashMap<&str, roxmltree::Node<'a, 'a>>,
    cmds: &mut Vec<SvgCmd>,
    filled_paths: &mut Vec<(Vec<Vec<[f32; 2]>>, Color32)>,
    pattern_draws: &mut Vec<PatternDraw>,
    depth: usize,
) {
    if depth > 24 || !node.is_element() {
        return;
    }
    let tag = node.tag_name().name();
    if matches!(
        tag,
        "defs"
            | "title"
            | "desc"
            | "metadata"
            | "style"
            | "script"
            | "filter"
            | "clipPath"
            | "mask"
            | "pattern"
            | "linearGradient"
            | "radialGradient"
    ) {
        return;
    }
    // Skip shields.io blurred drop-shadow text duplicates (`aria-hidden="true"` or `filter=...`).
    if node.attribute("aria-hidden") == Some("true") && (tag == "g" || tag == "text") {
        return;
    }
    if node.attribute("display") == Some("none") || node.attribute("visibility") == Some("hidden") {
        return;
    }

    let mut cur_xf = if let Some(t) = node.attribute("transform") {
        xf.mul(parse_transform(t))
    } else {
        xf
    };
    let style = inherit_style(node, parent_style, gradients);
    if style.opacity <= 0.01 {
        return;
    }

    match tag {
        "svg" => {
            if depth > 0 {
                let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
                let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
                cur_xf = cur_xf.mul(Transform2D {
                    e: x,
                    f: y,
                    ..Transform2D::IDENTITY
                });
                if let Some((vb_x, vb_y, vb_w, vb_h)) =
                    node.attribute("viewBox").and_then(parse_view_box)
                {
                    let w = node.attribute("width").and_then(parse_svg_len).unwrap_or(vb_w);
                    let h = node.attribute("height").and_then(parse_svg_len).unwrap_or(vb_h);
                    let sx = if vb_w.abs() > 1e-4 { w / vb_w } else { 1.0 };
                    let sy = if vb_h.abs() > 1e-4 { h / vb_h } else { 1.0 };
                    cur_xf = cur_xf.mul(Transform2D {
                        a: sx,
                        b: 0.0,
                        c: 0.0,
                        d: sy,
                        e: -vb_x * sx,
                        f: -vb_y * sy,
                    });
                }
            }
            for child in node.children() {
                walk_svg_node(
                    child,
                    cur_xf,
                    &style,
                    gradients,
                    patterns,
                    id_map,
                    cmds,
                    filled_paths,
                    pattern_draws,
                    depth + 1,
                );
            }
        }
        "g" | "a" => {
            for child in node.children() {
                walk_svg_node(
                    child,
                    cur_xf,
                    &style,
                    gradients,
                    patterns,
                    id_map,
                    cmds,
                    filled_paths,
                    pattern_draws,
                    depth + 1,
                );
            }
        }
        "use" => {
            let href = node
                .attribute("href")
                .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")));
            if let Some(id) = href.and_then(|h| h.strip_prefix('#'))
                && let Some(&target) = id_map.get(id)
            {
                let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
                let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
                let use_xf = cur_xf.mul(Transform2D {
                    e: x,
                    f: y,
                    ..Transform2D::IDENTITY
                });
                walk_svg_node(
                    target,
                    use_xf,
                    &style,
                    gradients,
                    patterns,
                    id_map,
                    cmds,
                    filled_paths,
                    pattern_draws,
                    depth + 1,
                );
            }
        }
        "foreignObject" => {
            let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
            let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
            let w = node.attribute("width").and_then(parse_svg_len).unwrap_or(100.0);
            let h = node.attribute("height").and_then(parse_svg_len).unwrap_or(20.0);
            let mut text = String::new();
            collect_all_text(node, &mut text);
            let text = text.trim().to_string();
            if !text.is_empty() {
                let st = node.attribute("style").unwrap_or("");
                let color = style_attr(st, "color")
                    .and_then(|c| parse_css_color(&c))
                    .or_else(|| resolve_fill(&style))
                    .unwrap_or(Color32::BLACK);
                let scale = cur_xf.avg_scale().max(0.01);
                let font_size = style_attr(st, "font-size")
                    .and_then(|v| parse_svg_len(&v))
                    .unwrap_or(style.font_size)
                    * scale;
                let bold = style_attr(st, "font-weight").is_some_and(|v| {
                    let vl = v.to_ascii_lowercase();
                    vl == "bold" || vl == "bolder" || vl.parse::<u16>().is_ok_and(|n| n >= 600)
                }) || style.bold;
                let anchor = match style_attr(st, "text-align")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "center" => TextAnchor::Middle,
                    "right" | "end" => TextAnchor::End,
                    _ => TextAnchor::Start,
                };
                let ax = match anchor {
                    TextAnchor::Middle => x + w * 0.5,
                    TextAnchor::End => x + w,
                    TextAnchor::Start => x,
                };
                // Baseline roughly at `y + font_size * 0.95` inside the HTML box.
                let ay = y + (font_size / scale).min(h) * 0.95;
                let (px, py) = cur_xf.apply(ax, ay);
                cmds.push(SvgCmd::Text {
                    pos: [px, py],
                    text,
                    size: font_size.clamp(6.0, 96.0),
                    bold,
                    color,
                    anchor,
                    text_length: None,
                });
            }
        }
        "rect" => {
            let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
            let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
            let w = node.attribute("width").and_then(parse_svg_len).unwrap_or(0.0);
            let h = node.attribute("height").and_then(parse_svg_len).unwrap_or(0.0);
            if w > 0.0 && h > 0.0 {
                let (x0, y0) = cur_xf.apply(x, y);
                let (x1, y1) = cur_xf.apply(x + w, y + h);
                let min = [x0.min(x1), y0.min(y1)];
                let max = [x0.max(x1), y0.max(y1)];
                let rx = node
                    .attribute("rx")
                    .or_else(|| node.attribute("ry"))
                    .and_then(parse_svg_len)
                    .unwrap_or(0.0)
                    * cur_xf.avg_scale();
                let mut fill = resolve_fill(&style);
                if let Some(uri) = fill_pattern_ref(node, patterns) {
                    pattern_draws.push(PatternDraw {
                        min,
                        max,
                        circle_clip: false,
                        uri: uri.to_string(),
                    });
                    fill = None;
                }
                let stroke = resolve_stroke(&style, cur_xf.avg_scale());
                if fill.is_some() || stroke.is_some() {
                    cmds.push(SvgCmd::Rect {
                        min,
                        max,
                        rx,
                        fill,
                        stroke,
                    });
                }
            }
        }
        "circle" => {
            let cx = node.attribute("cx").and_then(parse_svg_len).unwrap_or(0.0);
            let cy = node.attribute("cy").and_then(parse_svg_len).unwrap_or(0.0);
            let r = node.attribute("r").and_then(parse_svg_len).unwrap_or(0.0);
            if r > 0.0 {
                let (px, py) = cur_xf.apply(cx, cy);
                let sr = r * cur_xf.avg_scale();
                let mut fill = resolve_fill(&style);
                if let Some(uri) = fill_pattern_ref(node, patterns) {
                    pattern_draws.push(PatternDraw {
                        min: [px - sr, py - sr],
                        max: [px + sr, py + sr],
                        circle_clip: true,
                        uri: uri.to_string(),
                    });
                    fill = None;
                }
                let stroke = resolve_stroke(&style, cur_xf.avg_scale());
                if fill.is_some() || stroke.is_some() {
                    cmds.push(SvgCmd::Circle {
                        center: [px, py],
                        r: sr,
                        fill,
                        stroke,
                    });
                }
            }
        }
        "ellipse" => {
            let cx = node.attribute("cx").and_then(parse_svg_len).unwrap_or(0.0);
            let cy = node.attribute("cy").and_then(parse_svg_len).unwrap_or(0.0);
            let rx = node.attribute("rx").and_then(parse_svg_len).unwrap_or(0.0);
            let ry = node.attribute("ry").and_then(parse_svg_len).unwrap_or(0.0);
            if rx > 0.0 && ry > 0.0 {
                let mut pts = Vec::with_capacity(28);
                for k in 0..28 {
                    let ang = (k as f32 / 28.0) * std::f32::consts::TAU;
                    let (px, py) = cur_xf.apply(cx + rx * ang.cos(), cy + ry * ang.sin());
                    pts.push([px, py]);
                }
                if let Some(fill) = resolve_fill(&style) {
                    filled_paths.push((vec![pts.clone()], fill));
                }
                if let Some((stroke, stroke_w)) = resolve_stroke(&style, cur_xf.avg_scale()) {
                    cmds.push(SvgCmd::PathStroke {
                        subpaths: vec![(pts, true)],
                        stroke,
                        stroke_w,
                    });
                }
            }
        }
        "line" => {
            let x1 = node.attribute("x1").and_then(parse_svg_len).unwrap_or(0.0);
            let y1 = node.attribute("y1").and_then(parse_svg_len).unwrap_or(0.0);
            let x2 = node.attribute("x2").and_then(parse_svg_len).unwrap_or(0.0);
            let y2 = node.attribute("y2").and_then(parse_svg_len).unwrap_or(0.0);
            if let Some((stroke, stroke_w)) = resolve_stroke(&style, cur_xf.avg_scale()) {
                let (ax, ay) = cur_xf.apply(x1, y1);
                let (bx, by) = cur_xf.apply(x2, y2);
                cmds.push(SvgCmd::Line {
                    a: [ax, ay],
                    b: [bx, by],
                    stroke,
                    stroke_w,
                });
            }
        }
        "polygon" | "polyline" => {
            if let Some(pts_attr) = node.attribute("points") {
                let nums = parse_number_list(pts_attr);
                let mut pts = Vec::with_capacity(nums.len() / 2);
                for pair in nums.as_chunks::<2>().0 {
                    let (px, py) = cur_xf.apply(pair[0], pair[1]);
                    pts.push([px, py]);
                }
                if pts.len() >= 2 {
                    let closed = tag == "polygon";
                    if closed && let Some(fill) = resolve_fill(&style) {
                        filled_paths.push((vec![pts.clone()], fill));
                    }
                    if let Some((stroke, stroke_w)) = resolve_stroke(&style, cur_xf.avg_scale()) {
                        cmds.push(SvgCmd::PathStroke {
                            subpaths: vec![(pts, closed)],
                            stroke,
                            stroke_w,
                        });
                    }
                }
            }
        }
        "path" => {
            if let Some(d) = node.attribute("d") {
                let subpaths = parse_svg_path(d, cur_xf);
                if !subpaths.is_empty() {
                    if let Some(fill) = resolve_fill(&style) {
                        let polys: Vec<Vec<[f32; 2]>> = subpaths
                            .iter()
                            .filter(|(pts, _)| pts.len() >= 3)
                            .map(|(pts, _)| pts.clone())
                            .collect();
                        if !polys.is_empty() {
                            filled_paths.push((polys, fill));
                        }
                    }
                    if let Some((stroke, stroke_w)) = resolve_stroke(&style, cur_xf.avg_scale()) {
                        cmds.push(SvgCmd::PathStroke {
                            subpaths,
                            stroke,
                            stroke_w,
                        });
                    }
                }
            }
        }
        "text" => {
            if node.attribute("filter").is_some() {
                return;
            }
            let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
            let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
            let mut text = String::new();
            collect_svg_text(node, &mut text);
            let text = text.trim().to_string();
            if !text.is_empty()
                && let Some(color) = resolve_fill(&style)
            {
                let (px, py) = cur_xf.apply(x, y);
                let scale = cur_xf.avg_scale().max(0.01);
                let size = (style.font_size * scale).clamp(6.0, 96.0);
                let text_length = node
                    .attribute("textLength")
                    .and_then(parse_svg_len)
                    .map(|l| l * scale);
                cmds.push(SvgCmd::Text {
                    pos: [px, py],
                    text,
                    size,
                    bold: style.bold,
                    color,
                    anchor: style.text_anchor,
                    text_length,
                });
            }
        }
        "image" => {
            let href = node
                .attribute("href")
                .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")));
            if let Some(href) = href
                && let Some(bytes) = crate::markdown::net_img::decode_data_uri_bytes(href)
            {
                let x = node.attribute("x").and_then(parse_svg_len).unwrap_or(0.0);
                let y = node.attribute("y").and_then(parse_svg_len).unwrap_or(0.0);
                if looks_like_svg(&bytes) {
                    if let Ok(raw) = std::str::from_utf8(&bytes)
                        && let Ok(sub_doc) = roxmltree::Document::parse(raw.trim_start_matches('\u{feff}'))
                    {
                        let sub_root = sub_doc.root_element();
                        let sub_vb = sub_root.attribute("viewBox").and_then(parse_view_box);
                        let sub_w = sub_root.attribute("width").and_then(parse_svg_len);
                        let sub_h = sub_root.attribute("height").and_then(parse_svg_len);
                        let (vb_x, vb_y, vb_w, vb_h) = match (sub_vb, sub_w, sub_h) {
                            (Some(vb), _, _) => vb,
                            (None, Some(w), Some(h)) => (0.0, 0.0, w, h),
                            _ => (0.0, 0.0, 100.0, 100.0),
                        };
                        let w = node.attribute("width").and_then(parse_svg_len).or(sub_w).unwrap_or(vb_w);
                        let h = node.attribute("height").and_then(parse_svg_len).or(sub_h).unwrap_or(vb_h);
                        let sx = if vb_w.abs() > 1e-4 { w / vb_w } else { 1.0 };
                        let sy = if vb_h.abs() > 1e-4 { h / vb_h } else { 1.0 };
                        let sub_xf = cur_xf.mul(Transform2D {
                            a: sx,
                            b: 0.0,
                            c: 0.0,
                            d: sy,
                            e: x - vb_x * sx,
                            f: y - vb_y * sy,
                        });
                        let mut sub_grads = gradients.clone();
                        let mut sub_ids: HashMap<&str, roxmltree::Node<'_, '_>> = HashMap::new();
                        for sn in sub_doc.descendants() {
                            if let Some(id) = sn.attribute("id") {
                                sub_ids.insert(id, sn);
                            }
                            let n = sn.tag_name().name();
                            if (n == "linearGradient" || n == "radialGradient")
                                && let Some(id) = sn.attribute("id")
                                && let Some(c) = resolve_gradient_color(sn)
                            {
                                sub_grads.insert(id.to_string(), c);
                            }
                        }
                        let sub_base = SvgStyle::default();
                        for child in sub_root.children() {
                            walk_svg_node(
                                child,
                                sub_xf,
                                &sub_base,
                                &sub_grads,
                                patterns,
                                &sub_ids,
                                cmds,
                                filled_paths,
                                pattern_draws,
                                depth + 1,
                            );
                        }
                    }
                } else if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let (iw, ih) = rgba.dimensions();
                    let pixels = Arc::new(ColorImage::from_rgba_unmultiplied(
                        [iw as usize, ih as usize],
                        rgba.as_raw(),
                    ));
                    let w = node
                        .attribute("width")
                        .and_then(parse_svg_len)
                        .unwrap_or(iw as f32);
                    let h = node
                        .attribute("height")
                        .and_then(parse_svg_len)
                        .unwrap_or(ih as f32);
                    let (x0, y0) = cur_xf.apply(x, y);
                    let (x1, y1) = cur_xf.apply(x + w, y + h);
                    cmds.push(SvgCmd::EmbeddedImage {
                        min: [x0.min(x1), y0.min(y1)],
                        max: [x0.max(x1), y0.max(y1)],
                        pixels,
                    });
                }
            }
        }
        _ => {
            for child in node.children() {
                walk_svg_node(
                    child,
                    cur_xf,
                    &style,
                    gradients,
                    patterns,
                    id_map,
                    cmds,
                    filled_paths,
                    pattern_draws,
                    depth + 1,
                );
            }
        }
    }
}

pub fn paint_svg(
    ui: &mut Ui,
    cache: &SvgRenderCache,
    rect: Rect,
    ui_family: &egui::FontFamily,
    ui_bold_family: &egui::FontFamily,
) {
    let g = &cache.graphic;
    let sx = rect.width() / g.width.max(1.0);
    let sy = rect.height() / g.height.max(1.0);
    let scale = sx.min(sy);
    let map_pt = |p: [f32; 2]| Pos2::new(rect.left() + p[0] * sx, rect.top() + p[1] * sy);

    let painter = ui.painter().with_clip_rect(rect);
    let outer_rx = (g.corner_rx * scale).round().clamp(0.0, 24.0) as u8;

    // 0. Composited `<pattern><image/></pattern>` layer (under circle/rect strokes so borders sit cleanly on top).
    if let Some(tex) = &cache.pattern_tex {
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // 1. Rects & Circles & Lines (background shapes)
    for (idx, cmd) in g.cmds.iter().enumerate() {
        match cmd {
            SvgCmd::Rect {
                min,
                max,
                rx,
                fill,
                stroke,
            } => {
                let r = Rect::from_min_max(map_pt(*min), map_pt(*max));
                let radius = CornerRadius::same(((*rx * scale).round().clamp(0.0, 24.0) as u8).max(
                    if (r.width() - rect.width()).abs() < 2.0
                        && (r.height() - rect.height()).abs() < 2.0
                    {
                        outer_rx
                    } else {
                        0
                    },
                ));
                if let Some(c) = fill {
                    painter.rect_filled(r, radius, *c);
                }
                if let Some((sc, sw)) = stroke {
                    painter.rect_stroke(r, radius, Stroke::new((sw * scale).max(0.75), *sc), StrokeKind::Inside);
                }
            }
            SvgCmd::Circle {
                center,
                r,
                fill,
                stroke,
            } => {
                let c = map_pt(*center);
                let rr = (*r * scale).max(0.5);
                if let Some(fc) = fill {
                    painter.circle_filled(c, rr, *fc);
                }
                if let Some((sc, sw)) = stroke {
                    painter.circle_stroke(c, rr, Stroke::new((sw * scale).max(0.75), *sc));
                }
            }
            SvgCmd::Line {
                a,
                b,
                stroke,
                stroke_w,
            } => {
                painter.line_segment(
                    [map_pt(*a), map_pt(*b)],
                    Stroke::new((*stroke_w * scale).max(0.75), *stroke),
                );
            }
            SvgCmd::EmbeddedImage { min, max, .. } => {
                if let Some(Some(tex)) = cache.embedded_tex.get(idx) {
                    let r = Rect::from_min_max(map_pt(*min), map_pt(*max));
                    painter.image(
                        tex.id(),
                        r,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            _ => {}
        }
    }

    // 2. Rasterized complex filled `<path>` / `<polygon>` layer
    if let Some(tex) = &cache.path_tex {
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // 3. Stroked paths & crisp vector `<text>` on top
    for cmd in &g.cmds {
        match cmd {
            SvgCmd::PathStroke {
                subpaths,
                stroke,
                stroke_w,
            } => {
                let st = Stroke::new((*stroke_w * scale).max(0.75), *stroke);
                for (pts, closed) in subpaths {
                    if pts.len() < 2 {
                        continue;
                    }
                    let mapped: Vec<Pos2> = pts.iter().map(|p| map_pt(*p)).collect();
                    if *closed {
                        painter.add(Shape::closed_line(mapped, st));
                    } else {
                        painter.add(Shape::line(mapped, st));
                    }
                }
            }
            SvgCmd::Text {
                pos,
                text,
                size,
                bold,
                color,
                anchor,
                text_length,
            } => {
                let fam = if *bold {
                    ui_bold_family.clone()
                } else {
                    ui_family.clone()
                };
                let mut font_px = (*size * scale).clamp(8.0, 72.0);
                // Fit to SVG `textLength` if the rendered font runs slightly wider (e.g. shields.io).
                if let Some(max_len) = text_length {
                    let target_w = (*max_len * sx).max(8.0);
                    let meas = ui.ctx().fonts_mut(|f| {
                        f.layout_no_wrap(text.clone(), FontId::new(font_px, fam.clone()), *color)
                            .rect
                            .width()
                    });
                    if meas > target_w * 1.04 {
                        font_px = (font_px * (target_w / meas)).clamp(7.5, font_px);
                    }
                }
                let p = map_pt(*pos);
                // SVG `y` is text baseline; adjust upward slightly to align visually with `CENTER`.
                let draw_p = Pos2::new(p.x, p.y - font_px * 0.34);
                let align = match anchor {
                    TextAnchor::Start => Align2::LEFT_CENTER,
                    TextAnchor::Middle => Align2::CENTER_CENTER,
                    TextAnchor::End => Align2::RIGHT_CENTER,
                };
                painter.text(draw_p, align, text, FontId::new(font_px, fam), *color);
            }
            _ => {}
        }
    }
}

fn collect_svg_text(node: roxmltree::Node<'_, '_>, out: &mut String) {
    for child in node.children() {
        if child.is_text() {
            if let Some(t) = child.text() {
                out.push_str(t);
            }
        } else if child.is_element() && child.tag_name().name() == "tspan" {
            collect_svg_text(child, out);
        }
    }
}

fn collect_all_text(node: roxmltree::Node<'_, '_>, out: &mut String) {
    for child in node.children() {
        if child.is_text() {
            if let Some(t) = child.text() {
                out.push_str(t);
            }
        } else if child.is_element() {
            collect_all_text(child, out);
        }
    }
}

fn resolve_gradient_color(node: roxmltree::Node<'_, '_>) -> Option<Color32> {
    let mut stops = Vec::new();
    for child in node.children() {
        if child.tag_name().name() != "stop" {
            continue;
        }
        let style = child.attribute("style").unwrap_or("");
        let color_str = child
            .attribute("stop-color")
            .map(str::to_string)
            .or_else(|| style_attr(style, "stop-color"))
            .unwrap_or_else(|| "#000000".to_string());
        let opacity = child
            .attribute("stop-opacity")
            .map(str::to_string)
            .or_else(|| style_attr(style, "stop-opacity"))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        if let Some(c) = parse_css_color(&color_str) {
            stops.push((c, opacity));
        }
    }
    if stops.is_empty() {
        return None;
    }
    let (r, g, b, a) = stops.iter().fold((0.0f32, 0.0, 0.0, 0.0), |acc, (c, op)| {
        (
            acc.0 + c.r() as f32,
            acc.1 + c.g() as f32,
            acc.2 + c.b() as f32,
            acc.3 + (c.a() as f32 * *op),
        )
    });
    let n = stops.len() as f32;
    Some(Color32::from_rgba_unmultiplied(
        (r / n) as u8,
        (g / n) as u8,
        (b / n) as u8,
        (a / n) as u8,
    ))
}

fn style_attr(style: &str, key: &str) -> Option<String> {
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            let val = v.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn attr_or_style<'a>(node: roxmltree::Node<'a, 'a>, key: &str) -> Option<String> {
    if let Some(st) = node.attribute("style")
        && let Some(v) = style_attr(st, key)
    {
        return Some(v);
    }
    node.attribute(key).map(str::to_string)
}

fn parse_paint(raw: &str, gradients: &HashMap<String, Color32>) -> Option<Paint> {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("transparent") {
        return Some(Paint::None);
    }
    if let Some(inner) = s
        .strip_prefix("url(#")
        .and_then(|r| r.strip_suffix(')'))
    {
        return gradients.get(inner).copied().map(Paint::Solid);
    }
    parse_css_color(s).map(Paint::Solid)
}

fn inherit_style(
    node: roxmltree::Node<'_, '_>,
    parent: &SvgStyle,
    gradients: &HashMap<String, Color32>,
) -> SvgStyle {
    let mut s = parent.clone();
    if let Some(v) = attr_or_style(node, "fill")
        && let Some(p) = parse_paint(&v, gradients)
    {
        s.fill = p;
    }
    if let Some(v) = attr_or_style(node, "fill-opacity")
        && let Ok(op) = v.parse::<f32>()
    {
        s.fill_opacity = op.clamp(0.0, 1.0);
    }
    if let Some(v) = attr_or_style(node, "stroke")
        && let Some(p) = parse_paint(&v, gradients)
    {
        s.stroke = p;
    }
    if let Some(v) = attr_or_style(node, "stroke-width")
        && let Some(w) = parse_svg_len(&v)
    {
        s.stroke_width = w.max(0.2);
    }
    if let Some(v) = attr_or_style(node, "stroke-opacity")
        && let Ok(op) = v.parse::<f32>()
    {
        s.stroke_opacity = op.clamp(0.0, 1.0);
    }
    if let Some(v) = attr_or_style(node, "opacity")
        && let Ok(op) = v.parse::<f32>()
    {
        s.opacity *= op.clamp(0.0, 1.0);
    }
    if let Some(v) = attr_or_style(node, "font-size")
        && let Some(fs) = parse_svg_len(&v)
    {
        s.font_size = fs;
    }
    if let Some(v) = attr_or_style(node, "font-weight") {
        let vl = v.to_ascii_lowercase();
        s.bold = vl == "bold" || vl == "bolder" || vl.parse::<u16>().is_ok_and(|n| n >= 600);
    }
    if let Some(v) = attr_or_style(node, "text-anchor") {
        s.text_anchor = match v.trim().to_ascii_lowercase().as_str() {
            "middle" => TextAnchor::Middle,
            "end" => TextAnchor::End,
            _ => TextAnchor::Start,
        };
    }
    s
}

fn apply_alpha(c: Color32, alpha: f32) -> Option<Color32> {
    let a = ((c.a() as f32) * alpha.clamp(0.0, 1.0)).round() as u8;
    (a > 0).then(|| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a))
}

fn resolve_fill(s: &SvgStyle) -> Option<Color32> {
    match s.fill {
        Paint::None => None,
        Paint::Solid(c) => apply_alpha(c, s.fill_opacity * s.opacity),
    }
}

fn resolve_stroke(s: &SvgStyle, scale: f32) -> Option<(Color32, f32)> {
    match s.stroke {
        Paint::None => None,
        Paint::Solid(c) => {
            let col = apply_alpha(c, s.stroke_opacity * s.opacity)?;
            Some((col, (s.stroke_width * scale).max(0.5)))
        }
    }
}

fn parse_svg_len(raw: &str) -> Option<f32> {
    let s = raw.trim();
    if s.ends_with('%') {
        return None;
    }
    let num = s
        .trim_end_matches("px")
        .trim_end_matches("pt")
        .trim_end_matches("em")
        .trim();
    num.parse::<f32>().ok().filter(|v| v.is_finite())
}

fn parse_view_box(raw: &str) -> Option<(f32, f32, f32, f32)> {
    let nums = parse_number_list(raw);
    if nums.len() >= 4 && nums[2] > 0.0 && nums[3] > 0.0 {
        Some((nums[0], nums[1], nums[2], nums[3]))
    } else {
        None
    }
}

fn parse_transform(raw: &str) -> Transform2D {
    let mut out = Transform2D::IDENTITY;
    let mut rest = raw.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        let Some(close) = rest[open + 1..].find(')') else {
            break;
        };
        let args = parse_number_list(&rest[open + 1..open + 1 + close]);
        let next = match name.as_str() {
            "translate" if !args.is_empty() => Transform2D {
                e: args[0],
                f: args.get(1).copied().unwrap_or(0.0),
                ..Transform2D::IDENTITY
            },
            "scale" if !args.is_empty() => Transform2D {
                a: args[0],
                d: args.get(1).copied().unwrap_or(args[0]),
                ..Transform2D::IDENTITY
            },
            "rotate" if !args.is_empty() => {
                let rad = args[0].to_radians();
                let (s, c) = rad.sin_cos();
                let rot = Transform2D {
                    a: c,
                    b: s,
                    c: -s,
                    d: c,
                    e: 0.0,
                    f: 0.0,
                };
                if args.len() >= 3 {
                    let cx = args[1];
                    let cy = args[2];
                    Transform2D {
                        e: cx,
                        f: cy,
                        ..Transform2D::IDENTITY
                    }
                    .mul(rot)
                    .mul(Transform2D {
                        e: -cx,
                        f: -cy,
                        ..Transform2D::IDENTITY
                    })
                } else {
                    rot
                }
            }
            "matrix" if args.len() >= 6 => Transform2D {
                a: args[0],
                b: args[1],
                c: args[2],
                d: args[3],
                e: args[4],
                f: args[5],
            },
            _ => Transform2D::IDENTITY,
        };
        out = out.mul(next);
        rest = rest[open + 1 + close + 1..].trim_start_matches([',', ' ', '\t', '\n', '\r']);
    }
    out
}

fn parse_number_list(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        if bytes[i] == b'+' || bytes[i] == b'-' {
            i += 1;
        }
        let mut saw_dot = false;
        let mut saw_exp = false;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_digit() {
                i += 1;
            } else if b == b'.' && !saw_dot && !saw_exp {
                saw_dot = true;
                i += 1;
            } else if (b == b'e' || b == b'E') && !saw_exp && i > start {
                saw_exp = true;
                i += 1;
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
            } else {
                break;
            }
        }
        if i == start {
            i += 1;
            continue;
        }
        if let Ok(v) = s[start..i].parse::<f32>()
            && v.is_finite()
        {
            out.push(v);
        }
    }
    out
}

/// Flatten an SVG `<path d="...">` into transformed polylines `(points, closed)`.
fn parse_svg_path(d: &str, xf: Transform2D) -> Vec<(Vec<[f32; 2]>, bool)> {
    let mut subpaths: Vec<(Vec<[f32; 2]>, bool)> = Vec::new();
    let mut cur_poly: Vec<[f32; 2]> = Vec::new();
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut last_ctrl: Option<(f32, f32)> = None;
    let mut cmd = b'M';

    let bytes = d.as_bytes();
    let mut i = 0;

    let skip_sep = |idx: &mut usize| {
        while *idx < bytes.len() && (bytes[*idx].is_ascii_whitespace() || bytes[*idx] == b',') {
            *idx += 1;
        }
    };
    let next_num = |idx: &mut usize| -> Option<f32> {
        skip_sep(idx);
        if *idx >= bytes.len() {
            return None;
        }
        let start = *idx;
        if bytes[*idx] == b'+' || bytes[*idx] == b'-' {
            *idx += 1;
        }
        let mut saw_dot = false;
        let mut saw_exp = false;
        let mut digits = false;
        while *idx < bytes.len() {
            let b = bytes[*idx];
            if b.is_ascii_digit() {
                digits = true;
                *idx += 1;
            } else if b == b'.' && !saw_dot && !saw_exp {
                saw_dot = true;
                *idx += 1;
            } else if (b == b'e' || b == b'E') && !saw_exp && digits {
                saw_exp = true;
                *idx += 1;
                if *idx < bytes.len() && (bytes[*idx] == b'+' || bytes[*idx] == b'-') {
                    *idx += 1;
                }
            } else {
                break;
            }
        }
        if !digits {
            *idx = start;
            return None;
        }
        d[start..*idx].parse::<f32>().ok().filter(|v| v.is_finite())
    };
    let next_flag = |idx: &mut usize| -> Option<bool> {
        skip_sep(idx);
        if *idx >= bytes.len() {
            return None;
        }
        match bytes[*idx] {
            b'0' => {
                *idx += 1;
                Some(false)
            }
            b'1' => {
                *idx += 1;
                Some(true)
            }
            _ => None,
        }
    };

    let push_pt = |poly: &mut Vec<[f32; 2]>, x: f32, y: f32| {
        let (tx, ty) = xf.apply(x, y);
        poly.push([tx, ty]);
    };

    while i < bytes.len() {
        skip_sep(&mut i);
        if i >= bytes.len() {
            break;
        }
        if bytes[i].is_ascii_alphabetic() {
            cmd = bytes[i];
            i += 1;
        }
        match cmd {
            b'M' | b'm' => {
                let Some(x) = next_num(&mut i) else { break };
                let Some(y) = next_num(&mut i) else { break };
                if !cur_poly.is_empty() {
                    subpaths.push((std::mem::take(&mut cur_poly), false));
                }
                if cmd == b'm' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                sx = cx;
                sy = cy;
                push_pt(&mut cur_poly, cx, cy);
                last_ctrl = None;
                // Subsequent coordinate pairs after M/m are implicit L/l.
                cmd = if cmd == b'm' { b'l' } else { b'L' };
            }
            b'L' | b'l' => {
                let Some(x) = next_num(&mut i) else { break };
                let Some(y) = next_num(&mut i) else { break };
                if cmd == b'l' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                push_pt(&mut cur_poly, cx, cy);
                last_ctrl = None;
            }
            b'H' | b'h' => {
                let Some(x) = next_num(&mut i) else { break };
                cx = if cmd == b'h' { cx + x } else { x };
                push_pt(&mut cur_poly, cx, cy);
                last_ctrl = None;
            }
            b'V' | b'v' => {
                let Some(y) = next_num(&mut i) else { break };
                cy = if cmd == b'v' { cy + y } else { y };
                push_pt(&mut cur_poly, cx, cy);
                last_ctrl = None;
            }
            b'C' | b'c' => {
                let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x), Some(y)) = (
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                ) else {
                    break;
                };
                let (p1x, p1y, p2x, p2y, ex, ey) = if cmd == b'c' {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x1, y1, x2, y2, x, y)
                };
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    let mt = 1.0 - t;
                    let px = mt * mt * mt * cx
                        + 3.0 * mt * mt * t * p1x
                        + 3.0 * mt * t * t * p2x
                        + t * t * t * ex;
                    let py = mt * mt * mt * cy
                        + 3.0 * mt * mt * t * p1y
                        + 3.0 * mt * t * t * p2y
                        + t * t * t * ey;
                    push_pt(&mut cur_poly, px, py);
                }
                last_ctrl = Some((p2x, p2y));
                cx = ex;
                cy = ey;
            }
            b'S' | b's' => {
                let (Some(x2), Some(y2), Some(x), Some(y)) = (
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                ) else {
                    break;
                };
                let (p1x, p1y) = match last_ctrl {
                    Some((lx, ly)) => (2.0 * cx - lx, 2.0 * cy - ly),
                    None => (cx, cy),
                };
                let (p2x, p2y, ex, ey) = if cmd == b's' {
                    (cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x2, y2, x, y)
                };
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    let mt = 1.0 - t;
                    let px = mt * mt * mt * cx
                        + 3.0 * mt * mt * t * p1x
                        + 3.0 * mt * t * t * p2x
                        + t * t * t * ex;
                    let py = mt * mt * mt * cy
                        + 3.0 * mt * mt * t * p1y
                        + 3.0 * mt * t * t * p2y
                        + t * t * t * ey;
                    push_pt(&mut cur_poly, px, py);
                }
                last_ctrl = Some((p2x, p2y));
                cx = ex;
                cy = ey;
            }
            b'Q' | b'q' => {
                let (Some(x1), Some(y1), Some(x), Some(y)) = (
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                ) else {
                    break;
                };
                let (p1x, p1y, ex, ey) = if cmd == b'q' {
                    (cx + x1, cy + y1, cx + x, cy + y)
                } else {
                    (x1, y1, x, y)
                };
                for step in 1..=8 {
                    let t = step as f32 / 8.0;
                    let mt = 1.0 - t;
                    let px = mt * mt * cx + 2.0 * mt * t * p1x + t * t * ex;
                    let py = mt * mt * cy + 2.0 * mt * t * p1y + t * t * ey;
                    push_pt(&mut cur_poly, px, py);
                }
                last_ctrl = Some((p1x, p1y));
                cx = ex;
                cy = ey;
            }
            b'T' | b't' => {
                let (Some(x), Some(y)) = (next_num(&mut i), next_num(&mut i)) else {
                    break;
                };
                let (p1x, p1y) = match last_ctrl {
                    Some((lx, ly)) => (2.0 * cx - lx, 2.0 * cy - ly),
                    None => (cx, cy),
                };
                let (ex, ey) = if cmd == b't' { (cx + x, cy + y) } else { (x, y) };
                for step in 1..=8 {
                    let t = step as f32 / 8.0;
                    let mt = 1.0 - t;
                    let px = mt * mt * cx + 2.0 * mt * t * p1x + t * t * ex;
                    let py = mt * mt * cy + 2.0 * mt * t * p1y + t * t * ey;
                    push_pt(&mut cur_poly, px, py);
                }
                last_ctrl = Some((p1x, p1y));
                cx = ex;
                cy = ey;
            }
            b'A' | b'a' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(x), Some(y)) = (
                    next_num(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                    next_flag(&mut i),
                    next_flag(&mut i),
                    next_num(&mut i),
                    next_num(&mut i),
                ) else {
                    break;
                };
                let (ex, ey) = if cmd == b'a' { (cx + x, cy + y) } else { (x, y) };
                flatten_svg_arc(cx, cy, rx, ry, rot, large, sweep, ex, ey, |px, py| {
                    push_pt(&mut cur_poly, px, py);
                });
                cx = ex;
                cy = ey;
                last_ctrl = None;
            }
            b'Z' | b'z' => {
                if !cur_poly.is_empty() {
                    subpaths.push((std::mem::take(&mut cur_poly), true));
                }
                cx = sx;
                cy = sy;
                last_ctrl = None;
            }
            _ => break,
        }
    }
    if !cur_poly.is_empty() {
        subpaths.push((cur_poly, false));
    }
    subpaths
}

#[allow(clippy::too_many_arguments)]
fn flatten_svg_arc(
    x1: f32,
    y1: f32,
    mut rx: f32,
    mut ry: f32,
    phi_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
    mut emit: impl FnMut(f32, f32),
) {
    rx = rx.abs();
    ry = ry.abs();
    if rx < 1e-4 || ry < 1e-4 || ((x1 - x2).abs() < 1e-4 && (y1 - y2).abs() < 1e-4) {
        emit(x2, y2);
        return;
    }
    let phi = phi_deg.to_radians();
    let (sin_p, cos_p) = phi.sin_cos();
    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_p * dx2 + sin_p * dy2;
    let y1p = -sin_p * dx2 + cos_p * dy2;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let num = (rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p).max(0.0);
    let den = (rx2 * y1p * y1p + ry2 * x1p * x1p).max(1e-6);
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coef = sign * (num / den).sqrt();
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);
    let cx = cos_p * cxp - sin_p * cyp + (x1 + x2) * 0.5;
    let cy = sin_p * cxp + cos_p * cyp + (y1 + y2) * 0.5;

    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt().max(1e-6);
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }
    let steps = ((dtheta.abs() / 0.25).ceil() as usize).clamp(4, 32);
    for s in 1..=steps {
        let t = theta1 + dtheta * (s as f32 / steps as f32);
        let (st, ct) = t.sin_cos();
        let px = cos_p * rx * ct - sin_p * ry * st + cx;
        let py = sin_p * rx * ct + cos_p * ry * st + cy;
        emit(px, py);
    }
}

/// Fast 2x supersampled even-odd scanline rasterizer for filled concave SVG paths.
fn rasterize_filled_paths(
    width: f32,
    height: f32,
    paths: &[(Vec<Vec<[f32; 2]>>, Color32)],
) -> Option<ColorImage> {
    let ss = 2usize;
    let max_dim = 960.0f32;
    let down_scale = (max_dim / width.max(height).max(1.0)).min(1.0);
    let out_w = ((width * down_scale).ceil() as usize).clamp(1, 960);
    let out_h = ((height * down_scale).ceil() as usize).clamp(1, 960);
    let sx = (out_w * ss) as f32 / width.max(1.0);
    let sy = (out_h * ss) as f32 / height.max(1.0);
    let rw = out_w * ss;
    let rh = out_h * ss;
    let mut buf = vec![0u8; rw * rh * 4];
    let mut xs: Vec<f32> = Vec::with_capacity(32);

    for (polys, color) in paths {
        let mut min_y = rh as f32;
        let mut max_y = 0.0f32;
        for poly in polys {
            for pt in poly {
                let y = pt[1] * sy;
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
        let y0 = (min_y.floor().max(0.0) as usize).min(rh);
        let y1 = (max_y.ceil().max(0.0) as usize).min(rh);
        let sr = color.r() as u32;
        let sg = color.g() as u32;
        let sb = color.b() as u32;
        let sa = color.a() as u32;
        if sa == 0 {
            continue;
        }
        for py in y0..y1 {
            let scan_y = py as f32 + 0.5;
            xs.clear();
            for poly in polys {
                let n = poly.len();
                if n < 3 {
                    continue;
                }
                for k in 0..n {
                    let p0 = poly[k];
                    let p1 = poly[(k + 1) % n];
                    let y_a = p0[1] * sy;
                    let y_b = p1[1] * sy;
                    if (y_a <= scan_y && y_b > scan_y) || (y_b <= scan_y && y_a > scan_y) {
                        let t = (scan_y - y_a) / (y_b - y_a);
                        xs.push((p0[0] + t * (p1[0] - p0[0])) * sx);
                    }
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_unstable_by(|a, b| a.total_cmp(b));
            for span in xs.as_chunks::<2>().0 {
                let x0 = (span[0].floor().max(0.0) as usize).min(rw);
                let x1 = (span[1].ceil().max(0.0) as usize).min(rw);
                for px in x0..x1 {
                    let off = (py * rw + px) * 4;
                    let inv = 255 - sa;
                    let da = buf[off + 3] as u32;
                    let out_a = sa + (da * inv) / 255;
                    if let Some(nr) = (sr * sa + buf[off] as u32 * da * inv / 255).checked_div(out_a)
                    {
                        buf[off] = nr as u8;
                        buf[off + 1] =
                            (sg * sa + buf[off + 1] as u32 * da * inv / 255).checked_div(out_a).unwrap_or(0) as u8;
                        buf[off + 2] =
                            (sb * sa + buf[off + 2] as u32 * da * inv / 255).checked_div(out_a).unwrap_or(0) as u8;
                        buf[off + 3] = out_a.min(255) as u8;
                    }
                }
            }
        }
    }

    // 2x2 box-filter downsample for crisp anti-aliased edges.
    let mut down = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        for x in 0..out_w {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut a = 0u32;
            for dy in 0..ss {
                for dx in 0..ss {
                    let off = ((y * ss + dy) * rw + (x * ss + dx)) * 4;
                    let pa = buf[off + 3] as u32;
                    r += buf[off] as u32 * pa;
                    g += buf[off + 1] as u32 * pa;
                    b += buf[off + 2] as u32 * pa;
                    a += pa;
                }
            }
            let out_off = (y * out_w + x) * 4;
            if let Some(nr) = r.checked_div(a) {
                down[out_off] = nr.min(255) as u8;
                down[out_off + 1] = g.checked_div(a).unwrap_or(0).min(255) as u8;
                down[out_off + 2] = b.checked_div(a).unwrap_or(0).min(255) as u8;
                down[out_off + 3] = (a / (ss * ss) as u32).min(255) as u8;
            }
        }
    }
    Some(ColorImage::from_rgba_unmultiplied([out_w, out_h], &down))
}

/// Composite all `<pattern><image/></pattern>` fills (such as `contrib.rocks` contributor
/// avatar grids) into one shared RGBA texture with smooth anti-aliased circular clipping.
fn rasterize_pattern_layer(
    width: f32,
    height: f32,
    draws: &[PatternDraw],
) -> Option<ColorImage> {
    let max_dim = 1280.0f32;
    let down_scale = (max_dim / width.max(height).max(1.0)).min(1.0);
    let out_w = ((width * down_scale).ceil() as usize).clamp(1, 1280);
    let out_h = ((height * down_scale).ceil() as usize).clamp(1, 1280);
    let sx = out_w as f32 / width.max(1.0);
    let sy = out_h as f32 / height.max(1.0);

    let mut out = vec![0u8; out_w * out_h * 4];
    let mut decoded_any = false;
    type DecodedPattern = (Vec<u8>, u32, u32);
    let mut cache: HashMap<&str, Option<DecodedPattern>> = HashMap::new();

    for draw in draws {
        let img = cache.entry(draw.uri.as_str()).or_insert_with(|| {
            let bytes = crate::markdown::net_img::decode_data_uri_bytes(&draw.uri)?;
            let dyn_img = image::load_from_memory(&bytes).ok()?;
            let rgba = dyn_img.to_rgba8();
            let (iw, ih) = rgba.dimensions();
            Some((rgba.into_raw(), iw.max(1), ih.max(1)))
        });
        let Some((src, iw, ih)) = img.as_ref() else {
            continue;
        };
        decoded_any = true;
        let iw = *iw as usize;
        let ih = *ih as usize;
        let dx0 = draw.min[0] * sx;
        let dy0 = draw.min[1] * sy;
        let dx1 = draw.max[0] * sx;
        let dy1 = draw.max[1] * sy;
        let dw = (dx1 - dx0).max(1.0);
        let dh = (dy1 - dy0).max(1.0);
        let cx = (dx0 + dx1) * 0.5;
        let cy = (dy0 + dy1) * 0.5;
        let radius = dw.min(dh) * 0.5;

        let x_start = (dx0.floor().max(0.0) as usize).min(out_w);
        let x_end = (dx1.ceil().max(0.0) as usize).min(out_w);
        let y_start = (dy0.floor().max(0.0) as usize).min(out_h);
        let y_end = (dy1.ceil().max(0.0) as usize).min(out_h);

        for py in y_start..y_end {
            let fy = py as f32 + 0.5;
            let v = ((fy - dy0) / dh).clamp(0.0, 0.9999);
            let sy_idx = (v * ih as f32) as usize;
            for px in x_start..x_end {
                let fx = px as f32 + 0.5;
                let mask_alpha = if draw.circle_clip {
                    let dist = ((fx - cx) * (fx - cx) + (fy - cy) * (fy - cy)).sqrt();
                    if dist >= radius + 0.6 {
                        continue;
                    }
                    ((radius + 0.6 - dist) / 1.2).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let u = ((fx - dx0) / dw).clamp(0.0, 0.9999);
                let sx_idx = (u * iw as f32) as usize;
                let soff = (sy_idx * iw + sx_idx) * 4;
                let sr = src[soff] as u32;
                let sg = src[soff + 1] as u32;
                let sb = src[soff + 2] as u32;
                let sa = ((src[soff + 3] as f32) * mask_alpha).round() as u32;
                if sa == 0 {
                    continue;
                }
                let doff = (py * out_w + px) * 4;
                if sa >= 255 {
                    out[doff] = sr as u8;
                    out[doff + 1] = sg as u8;
                    out[doff + 2] = sb as u8;
                    out[doff + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    let da = out[doff + 3] as u32;
                    let oa = sa + (da * inv) / 255;
                    if let Some(nr) = (sr * sa + out[doff] as u32 * da * inv / 255).checked_div(oa)
                    {
                        out[doff] = nr as u8;
                        out[doff + 1] = (sg * sa + out[doff + 1] as u32 * da * inv / 255)
                            .checked_div(oa)
                            .unwrap_or(0) as u8;
                        out[doff + 2] = (sb * sa + out[doff + 2] as u32 * da * inv / 255)
                            .checked_div(oa)
                            .unwrap_or(0) as u8;
                        out[doff + 3] = oa.min(255) as u8;
                    }
                }
            }
        }
    }
    decoded_any.then(|| ColorImage::from_rgba_unmultiplied([out_w, out_h], &out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shields_io_badge_svg() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="84" height="20"><clipPath id="r"><rect width="84" height="20" rx="3"/></clipPath><g clip-path="url(#r)"><rect width="49" height="20" fill="#555"/><rect x="49" width="35" height="20" fill="#2563eb"/></g><g fill="#fff" text-anchor="middle" font-size="110"><g transform="scale(.1)"><g aria-hidden="true" fill="#010101"><text x="255" y="150">Kestrel</text></g><text x="255" y="140" textLength="390">Kestrel</text></g><g transform="scale(.1)"><text x="655" y="140" textLength="250">Rust</text></g></g></svg>"##;
        assert!(looks_like_svg(svg));
        let g = parse_svg(svg).unwrap();
        assert_eq!(g.width, 84.0);
        assert_eq!(g.height, 20.0);
        assert_eq!(g.corner_rx, 3.0);
        let texts: Vec<&str> = g
            .cmds
            .iter()
            .filter_map(|c| match c {
                SvgCmd::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Kestrel", "Rust"]);
    }
}
