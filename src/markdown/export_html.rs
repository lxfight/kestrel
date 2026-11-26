//! Export parsed Markdown documents to standalone, responsive HTML5 web pages
//! matching Kestrel's Dark / Light themes so users can preview full web layouts
//! in a real browser.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::markdown::parser::{format_latex_unicode, parse};
use crate::markdown::{Align, Block, CalloutKind, Inline, MarkdownDoc};

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn align_attr(align: Option<Align>) -> &'static str {
    match align {
        Some(Align::Center) => " style=\"text-align:center\"",
        Some(Align::Right) => " style=\"text-align:right\"",
        Some(Align::Left) => " style=\"text-align:left\"",
        None => "",
    }
}

fn color_css(c: egui::Color32) -> String {
    if c.a() == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
    } else {
        format!(
            "rgba({},{},{},{:.2})",
            c.r(),
            c.g(),
            c.b(),
            c.a() as f32 / 255.0
        )
    }
}

fn callout_accent(kind: CalloutKind) -> (&'static str, &'static str) {
    match kind {
        CalloutKind::Note | CalloutKind::Info => ("#3b82f6", "rgba(59,130,246,0.10)"),
        CalloutKind::Tip | CalloutKind::Example => ("#10b981", "rgba(16,185,129,0.10)"),
        CalloutKind::Important => ("#a855f7", "rgba(168,85,247,0.10)"),
        CalloutKind::Warning | CalloutKind::Caution => ("#f59e0b", "rgba(245,158,11,0.10)"),
        CalloutKind::Danger | CalloutKind::Bug => ("#ef4444", "rgba(239,68,68,0.10)"),
        CalloutKind::Quote => ("#6b7280", "rgba(107,114,128,0.10)"),
    }
}

fn render_inlines_html(inlines: &[Inline], out: &mut String) {
    for item in inlines {
        match item {
            Inline::Text(t) => out.push_str(&escape_html(t)),
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push_str("<br>\n"),
            Inline::Code(c) => {
                let _ = write!(out, "<code>{}</code>", escape_html(c));
            }
            Inline::Math(tex) => {
                let _ = write!(
                    out,
                    "<span class=\"k-math\">{}</span>",
                    escape_html(&format_latex_unicode(tex))
                );
            }
            Inline::Kbd(k) => {
                let _ = write!(out, "<kbd>{}</kbd>", escape_html(k));
            }
            Inline::Strong(ch) => {
                out.push_str("<strong>");
                render_inlines_html(ch, out);
                out.push_str("</strong>");
            }
            Inline::Emphasis(ch) => {
                out.push_str("<em>");
                render_inlines_html(ch, out);
                out.push_str("</em>");
            }
            Inline::Strikethrough(ch) => {
                out.push_str("<del>");
                render_inlines_html(ch, out);
                out.push_str("</del>");
            }
            Inline::Highlight(ch) => {
                out.push_str("<mark>");
                render_inlines_html(ch, out);
                out.push_str("</mark>");
            }
            Inline::Underline(ch) => {
                out.push_str("<u>");
                render_inlines_html(ch, out);
                out.push_str("</u>");
            }
            Inline::Superscript(ch) => {
                out.push_str("<sup>");
                render_inlines_html(ch, out);
                out.push_str("</sup>");
            }
            Inline::Subscript(ch) => {
                out.push_str("<sub>");
                render_inlines_html(ch, out);
                out.push_str("</sub>");
            }
            Inline::Link { text, url, title } => {
                let _ = write!(out, "<a href=\"{}\"", escape_html(url));
                if let Some(t) = title {
                    let _ = write!(out, " title=\"{}\"", escape_html(t));
                }
                if url.starts_with("http://") || url.starts_with("https://") {
                    out.push_str(" target=\"_blank\" rel=\"noopener noreferrer\"");
                }
                out.push('>');
                render_inlines_html(text, out);
                out.push_str("</a>");
            }
            Inline::Image {
                alt,
                url,
                title,
                width,
            } => {
                let _ = write!(
                    out,
                    "<img src=\"{}\" alt=\"{}\"",
                    escape_html(url),
                    escape_html(alt)
                );
                if let Some(t) = title {
                    let _ = write!(out, " title=\"{}\"", escape_html(t));
                }
                if let Some(w) = width {
                    let _ = write!(out, " width=\"{:.0}\"", w);
                }
                out.push('>');
            }
            Inline::FootnoteRef { id, number } => {
                let _ = write!(
                    out,
                    "<sup><a href=\"#fn-{}\">[{}]</a></sup>",
                    escape_html(id),
                    number
                );
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
                let mut css = String::new();
                if let Some(c) = color {
                    let _ = write!(css, "color:{};", color_css(*c));
                }
                if let Some(b) = bg {
                    let _ = write!(
                        css,
                        "background:{};padding:1px 4px;border-radius:4px;",
                        color_css(*b)
                    );
                }
                if let Some(s) = scale {
                    let _ = write!(css, "font-size:{:.0}%;", s * 100.0);
                }
                if *bold {
                    css.push_str("font-weight:600;");
                }
                if *italic {
                    css.push_str("font-style:italic;");
                }
                if *underline && *strike {
                    css.push_str("text-decoration:underline line-through;");
                } else if *underline {
                    css.push_str("text-decoration:underline;");
                } else if *strike {
                    css.push_str("text-decoration:line-through;");
                }
                let tag = if title.is_some() { "abbr" } else { "span" };
                let _ = write!(out, "<{tag}");
                if !css.is_empty() {
                    let _ = write!(out, " style=\"{css}\"");
                }
                if let Some(t) = title {
                    let _ = write!(out, " title=\"{}\"", escape_html(t));
                }
                out.push('>');
                render_inlines_html(children, out);
                let _ = write!(out, "</{tag}>");
            }
        }
    }
}

fn render_blocks_html(blocks: &[Block], out: &mut String) {
    for block in blocks {
        match block {
            Block::Heading {
                level,
                slug,
                inlines,
                align,
                ..
            } => {
                let lvl = (*level).clamp(1, 6);
                let _ = write!(
                    out,
                    "<h{lvl} id=\"{}\"{}>",
                    escape_html(slug),
                    align_attr(*align)
                );
                render_inlines_html(inlines, out);
                let _ = writeln!(out, "</h{lvl}>");
            }
            Block::Paragraph { inlines, align } => {
                let _ = write!(out, "<p{}>", align_attr(*align));
                render_inlines_html(inlines, out);
                out.push_str("</p>\n");
            }
            Block::Container {
                align,
                blocks: inner,
                caption,
            } => {
                if caption.is_some() {
                    let _ = writeln!(out, "<figure{}>", align_attr(*align));
                    render_blocks_html(inner, out);
                    if let Some(cap) = caption {
                        out.push_str("<figcaption>");
                        render_inlines_html(cap, out);
                        out.push_str("</figcaption>\n");
                    }
                    out.push_str("</figure>\n");
                } else {
                    let _ = writeln!(out, "<div{}>", align_attr(*align));
                    render_blocks_html(inner, out);
                    out.push_str("</div>\n");
                }
            }
            Block::CodeBlock { lang, code, .. } => {
                let cls = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{}\"", escape_html(lang))
                };
                let _ = writeln!(
                    out,
                    "<pre><code{cls}>{}</code></pre>",
                    escape_html(code)
                );
            }
            Block::MathBlock { tex, .. } => {
                let _ = writeln!(
                    out,
                    "<div class=\"k-math-block\">{}</div>",
                    escape_html(&format_latex_unicode(tex))
                );
            }
            Block::Blockquote { callout, blocks } => {
                if let Some((kind, custom_title)) = callout {
                    let (border, bg) = callout_accent(*kind);
                    let title = custom_title
                        .as_deref()
                        .unwrap_or_else(|| kind.default_title());
                    let _ = writeln!(
                        out,
                        "<div class=\"k-callout\" style=\"border-left:4px solid {border};background:{bg}\">"
                    );
                    let _ = writeln!(
                        out,
                        "<div class=\"k-callout-title\" style=\"color:{border}\">{}</div>",
                        escape_html(title)
                    );
                    render_blocks_html(blocks, out);
                    out.push_str("</div>\n");
                } else {
                    out.push_str("<blockquote>\n");
                    render_blocks_html(blocks, out);
                    out.push_str("</blockquote>\n");
                }
            }
            Block::List {
                ordered,
                start,
                items,
                ..
            } => {
                if *ordered {
                    if *start != 1 {
                        let _ = writeln!(out, "<ol start=\"{start}\">");
                    } else {
                        out.push_str("<ol>\n");
                    }
                } else {
                    out.push_str("<ul>\n");
                }
                for item in items {
                    out.push_str("<li>");
                    if let Some(checked) = item.checked {
                        let attr = if checked { " checked" } else { "" };
                        let _ = write!(out, "<input type=\"checkbox\" disabled{attr}> ");
                    }
                    render_blocks_html(&item.blocks, out);
                    out.push_str("</li>\n");
                }
                out.push_str(if *ordered { "</ol>\n" } else { "</ul>\n" });
            }
            Block::Table {
                alignments,
                headers,
                rows,
                ..
            } => {
                out.push_str("<div class=\"k-table-wrap\"><table>\n");
                if !headers.is_empty() {
                    out.push_str("<thead><tr>");
                    for (idx, cell) in headers.iter().enumerate() {
                        let align = alignments.get(idx).copied();
                        let _ = write!(out, "<th{}>", align_attr(align));
                        render_inlines_html(cell, out);
                        out.push_str("</th>");
                    }
                    out.push_str("</tr></thead>\n");
                }
                out.push_str("<tbody>\n");
                for row in rows {
                    out.push_str("<tr>");
                    for (idx, cell) in row.iter().enumerate() {
                        let align = alignments.get(idx).copied();
                        let _ = write!(out, "<td{}>", align_attr(align));
                        render_inlines_html(cell, out);
                        out.push_str("</td>");
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</tbody></table></div>\n");
            }
            Block::DefinitionList { items } => {
                out.push_str("<dl>\n");
                for item in items {
                    out.push_str("<dt>");
                    render_inlines_html(&item.term, out);
                    out.push_str("</dt>\n");
                    for def in &item.definitions {
                        out.push_str("<dd>");
                        render_blocks_html(def, out);
                        out.push_str("</dd>\n");
                    }
                }
                out.push_str("</dl>\n");
            }
            Block::Details {
                summary,
                blocks,
                open_by_default,
                ..
            } => {
                let open = if *open_by_default { " open" } else { "" };
                let _ = writeln!(out, "<details{open}><summary>");
                render_inlines_html(summary, out);
                out.push_str("</summary>\n");
                render_blocks_html(blocks, out);
                out.push_str("</details>\n");
            }
            Block::ThematicBreak => out.push_str("<hr>\n"),
            Block::HtmlBlock { html } => {
                out.push_str(html);
                out.push('\n');
            }
        }
    }
}

fn dir_to_file_url(dir: &Path) -> String {
    let raw = dir.to_string_lossy().replace('\\', "/");
    let stripped = raw.strip_prefix("//?/").unwrap_or(&raw);
    let mut url = if stripped.starts_with('/') {
        format!("file://{stripped}")
    } else {
        format!("file:///{stripped}")
    };
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// Convert a parsed [`MarkdownDoc`] into a standalone HTML5 page.
pub fn render_doc_to_html(
    title: &str,
    doc: &MarkdownDoc,
    dark: bool,
    doc_path: Option<&Path>,
) -> String {
    let (bg, panel, code_bg, border, text, text_dim, accent) = if dark {
        (
            "#111318",
            "#161921",
            "#1c202a",
            "#2a3040",
            "#e6edf3",
            "#9ca3af",
            "#60a5fa",
        )
    } else {
        (
            "#ffffff",
            "#f6f8fa",
            "#f3f5f8",
            "#d8dee4",
            "#1f2328",
            "#57606a",
            "#2563eb",
        )
    };
    let base_tag = doc_path
        .and_then(|p| p.parent())
        .map(|dir| format!("<base href=\"{}\">\n", escape_html(&dir_to_file_url(dir))))
        .unwrap_or_default();

    let mut body = String::with_capacity(4096);
    if let Some(fm) = &doc.front_matter {
        body.push_str("<div class=\"k-frontmatter\">");
        for (k, v) in &fm.entries {
            let _ = write!(
                body,
                "<div><strong>{}:</strong> {}</div>",
                escape_html(k),
                escape_html(v)
            );
        }
        body.push_str("</div>\n");
    }
    render_blocks_html(&doc.blocks, &mut body);
    if !doc.footnotes.is_empty() {
        body.push_str("<hr><section class=\"k-footnotes\"><ol>\n");
        for f in &doc.footnotes {
            let _ = write!(body, "<li id=\"fn-{}\">", escape_html(&f.id));
            render_blocks_html(&f.blocks, &mut body);
            body.push_str("</li>\n");
        }
        body.push_str("</ol></section>\n");
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{base_tag}<title>{title}</title>
<style>
  :root {{ color-scheme: {scheme}; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; padding: 36px 24px 72px;
    background: {bg}; color: {text};
    font: 15px/1.72 -apple-system, BlinkMacSystemFont, "Segoe UI", "Microsoft YaHei UI", sans-serif;
  }}
  main {{ max-width: 880px; margin: 0 auto; }}
  h1, h2, h3, h4, h5, h6 {{ line-height: 1.3; margin: 1.4em 0 0.6em; font-weight: 600; }}
  h1 {{ font-size: 2em; padding-bottom: 0.3em; border-bottom: 1px solid {border}; }}
  h2 {{ font-size: 1.5em; padding-bottom: 0.25em; border-bottom: 1px solid {border}; }}
  p {{ margin: 0 0 1em; }}
  a {{ color: {accent}; text-decoration: none; }}
  a:hover {{ text-decoration: underline; }}
  img {{ max-width: 100%; height: auto; border-radius: 6px; }}
  figure {{ margin: 1.2em 0; }}
  figcaption {{ margin-top: 6px; font-size: 0.9em; color: {text_dim}; font-style: italic; }}
  code {{
    font-family: "Cascadia Mono", Consolas, monospace; font-size: 0.9em;
    background: {code_bg}; padding: 0.15em 0.4em; border-radius: 5px;
  }}
  pre {{
    background: {code_bg}; border: 1px solid {border}; border-radius: 8px;
    padding: 14px 16px; overflow-x: auto; line-height: 1.55;
  }}
  pre code {{ background: none; padding: 0; font-size: 0.9em; }}
  kbd {{
    font-family: "Cascadia Mono", Consolas, monospace; font-size: 0.85em;
    padding: 2px 6px; border-radius: 4px; border: 1px solid {border};
    background: {panel}; box-shadow: inset 0 -1px 0 {border};
  }}
  blockquote {{
    margin: 1em 0; padding: 8px 16px; border-left: 4px solid {border};
    background: {panel}; border-radius: 0 6px 6px 0; color: {text_dim};
  }}
  .k-callout {{ margin: 1em 0; padding: 10px 14px; border-radius: 6px; }}
  .k-callout-title {{ font-weight: 600; margin-bottom: 4px; }}
  .k-table-wrap {{ overflow-x: auto; margin: 1em 0; }}
  table {{ width: 100%; border-collapse: collapse; }}
  th, td {{ border: 1px solid {border}; padding: 8px 12px; }}
  th {{ background: {panel}; font-weight: 600; }}
  tr:nth-child(even) td {{ background: color-mix(in srgb, {panel} 45%, transparent); }}
  details {{
    background: {panel}; border: 1px solid {border}; border-radius: 8px;
    padding: 10px 14px; margin: 1em 0;
  }}
  summary {{ cursor: pointer; font-weight: 600; }}
  hr {{ border: 0; border-top: 1px solid {border}; margin: 1.5em 0; }}
  .k-frontmatter {{
    background: {panel}; border: 1px solid {border}; border-radius: 8px;
    padding: 10px 14px; margin-bottom: 1.4em; font-size: 0.9em; color: {text_dim};
  }}
  .k-math, .k-math-block {{ font-family: "Cambria Math", Georgia, serif; }}
  .k-math-block {{
    background: {panel}; border: 1px solid {border}; border-radius: 8px;
    padding: 12px 16px; margin: 1em 0; text-align: center; font-size: 1.08em;
  }}
  .k-footnotes {{ font-size: 0.9em; color: {text_dim}; }}
</style>
</head>
<body>
<main>
{body}</main>
</body>
</html>
"#,
        title = escape_html(title),
        scheme = if dark { "dark" } else { "light" },
    )
}

/// Parse `markdown`, write a standalone HTML5 preview into `%TEMP%/kestrel-preview/`,
/// and return its path so the app can launch it in the system browser.
pub fn write_temp_html_preview(
    title: &str,
    markdown: &str,
    dark: bool,
    doc_path: Option<&Path>,
) -> io::Result<PathBuf> {
    let doc = parse(markdown);
    let html = render_doc_to_html(title, &doc, dark, doc_path);
    let dir = std::env::temp_dir().join("kestrel-preview");
    std::fs::create_dir_all(&dir)?;
    let safe_stem: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    let stem = if safe_stem.is_empty() {
        "preview"
    } else {
        safe_stem.as_str()
    };
    let out_path = dir.join(format!("{stem}.html"));
    std::fs::write(&out_path, html)?;
    Ok(out_path)
}
