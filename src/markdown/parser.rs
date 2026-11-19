//! Full-featured CommonMark + GFM + Extended Markdown parser.

use std::collections::{HashMap, HashSet};

use crate::highlight::{HighlightState, Language, Span};
use crate::markdown::{
    Align, Block, CalloutKind, DefItem, FootnoteDef, FrontMatter, FrontMatterKind, Inline,
    ListItem, MarkdownDoc,
};

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    raw_start: usize,
    line_no: usize,
}

#[derive(Clone)]
struct LinkDef {
    url: String,
    title: Option<String>,
}

struct ParseCtx {
    link_defs: HashMap<String, LinkDef>,
    footnote_defs: HashMap<String, Vec<String>>,
    footnote_order: Vec<String>,
    footnote_numbers: HashMap<String, usize>,
    slug_counts: HashMap<String, usize>,
}

impl ParseCtx {
    fn new() -> Self {
        Self {
            link_defs: HashMap::new(),
            footnote_defs: HashMap::new(),
            footnote_order: Vec::new(),
            footnote_numbers: HashMap::new(),
            slug_counts: HashMap::new(),
        }
    }

    fn register_footnote(&mut self, id: &str) -> usize {
        if let Some(&n) = self.footnote_numbers.get(id) {
            return n;
        }
        let n = self.footnote_order.len() + 1;
        self.footnote_order.push(id.to_string());
        self.footnote_numbers.insert(id.to_string(), n);
        n
    }

    fn register_heading(&mut self, inlines: &[Inline]) -> String {
        let base = slugify(&plain_text_of_inlines(inlines));
        let count = self.slug_counts.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        slug
    }
}

pub fn parse(text: &str) -> MarkdownDoc {
    let mut lines = split_lines(text);
    let mut front_matter = None;
    let body_start = if let Some((fm, next_idx)) = try_parse_front_matter(&lines) {
        front_matter = Some(fm);
        next_idx
    } else {
        0
    };
    let body_lines = &lines[body_start..];
    let mut ctx = ParseCtx::new();
    let filtered = extract_definitions(body_lines, &mut ctx);
    lines.clear();

    let blocks = parse_blocks(&filtered, &mut ctx, 0);

    // Build ordered footnotes (including any defined footnotes not yet referenced, if desired,
    // or all referenced ones in citation order followed by unreferenced definitions).
    let mut footnotes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let order = ctx.footnote_order.clone();
    for id in order {
        if seen.insert(id.clone())
            && let Some(raw_lines) = ctx.footnote_defs.get(&id).cloned()
        {
            let number = ctx.footnote_numbers[&id];
            let sub_lines: Vec<Line<'_>> = raw_lines
                .iter()
                .enumerate()
                .map(|(i, s)| Line {
                    text: s.as_str(),
                    raw_start: 0,
                    line_no: i,
                })
                .collect();
            let blocks = parse_blocks(&sub_lines, &mut ctx, 1);
            footnotes.push(FootnoteDef { id, number, blocks });
        }
    }

    MarkdownDoc {
        front_matter,
        blocks,
        footnotes,
    }
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (line_no, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        out.push(Line {
            text: line,
            raw_start: offset,
            line_no,
        });
        offset += raw.len() + 1;
    }
    out
}

fn try_parse_front_matter(lines: &[Line<'_>]) -> Option<(FrontMatter, usize)> {
    let first = lines.first()?.text.trim();
    let (kind, delim) = if first == "---" {
        (FrontMatterKind::Yaml, "---")
    } else if first == "+++" {
        (FrontMatterKind::Toml, "+++")
    } else {
        return None;
    };
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        let t = line.text.trim();
        if t == delim || (kind == FrontMatterKind::Yaml && t == "...") {
            end_idx = Some(i);
            break;
        }
    }
    let end = end_idx?;
    if end <= 1 {
        return None;
    }
    let mut raw = String::new();
    let mut entries = Vec::new();
    for line in &lines[1..end] {
        if !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(line.text);
        let sep = if kind == FrontMatterKind::Yaml { ':' } else { '=' };
        if let Some((k, v)) = line.text.split_once(sep) {
            let key = k.trim();
            if !key.is_empty() && !key.starts_with('#') {
                let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                entries.push((key.to_string(), val));
            }
        }
    }
    Some((
        FrontMatter {
            kind,
            entries,
            raw,
        },
        end + 1,
    ))
}

/// First pass: extract link reference definitions (`[ref]: url "title"`) and footnote
/// definitions (`[^id]: text`), skipping fenced code blocks, and return remaining lines.
fn extract_definitions<'a>(lines: &[Line<'a>], ctx: &mut ParseCtx) -> Vec<Line<'a>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut fence: Option<(u8, usize)> = None;
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.text.trim_start();
        let indent = line.text.len() - trimmed.len();

        if let Some((marker, len)) = fence {
            if is_closing_fence(trimmed, marker, len) {
                fence = None;
            }
            out.push(line);
            i += 1;
            continue;
        }

        if indent <= 3
            && let Some((marker, len, _)) = parse_opening_fence(trimmed)
        {
            fence = Some((marker, len));
            out.push(line);
            i += 1;
            continue;
        }

        // Footnote definition: `[^id]: ...`
        if indent <= 3
            && let Some((id, first_rest)) = parse_footnote_def_start(trimmed)
        {
            let mut body = vec![first_rest.to_string()];
            i += 1;
            while i < lines.len() {
                let next = lines[i].text;
                if next.trim().is_empty() {
                    // Check if following non-empty line is indented >= 2 spaces or tab
                    let mut j = i + 1;
                    while j < lines.len() && lines[j].text.trim().is_empty() {
                        j += 1;
                    }
                    if j < lines.len()
                        && (lines[j].text.starts_with("  ") || lines[j].text.starts_with('\t'))
                        && !lines[j].text.trim_start().starts_with("[^")
                    {
                        body.push(String::new());
                        i += 1;
                        continue;
                    }
                    break;
                } else if next.starts_with("    ") || next.starts_with('\t') || next.starts_with("  ")
                {
                    let stripped = if let Some(s) = next.strip_prefix("    ") {
                        s
                    } else if let Some(s) = next.strip_prefix('\t') {
                        s
                    } else {
                        next.trim_start()
                    };
                    body.push(stripped.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            ctx.footnote_defs.entry(id).or_insert(body);
            continue;
        }

        // Link reference definition: `[label]: url "title"`
        if indent <= 3
            && let Some((label, def, consumed)) = parse_link_def(lines, i)
        {
            ctx.link_defs.entry(label).or_insert(def);
            i += consumed;
            continue;
        }

        out.push(line);
        i += 1;
    }

    out
}

fn parse_footnote_def_start(trimmed: &str) -> Option<(String, &str)> {
    let rest = trimmed.strip_prefix("[^")?;
    let close = rest.find("]:")?;
    let id = rest[..close].trim();
    if id.is_empty() || id.contains(char::is_whitespace) {
        return None;
    }
    let after = rest[close + 2..].trim_start();
    Some((id.to_string(), after))
}

fn parse_link_def(lines: &[Line<'_>], idx: usize) -> Option<(String, LinkDef, usize)> {
    let trimmed = lines[idx].text.trim_start();
    if !trimmed.starts_with('[') || trimmed.starts_with("[^") {
        return None;
    }
    let close = trimmed[1..].find("]:")? + 1;
    let label = normalize_label(&trimmed[1..close]);
    if label.is_empty() {
        return None;
    }
    let mut rest = trimmed[close + 2..].trim();
    let mut consumed = 1usize;
    if rest.is_empty() && idx + 1 < lines.len() {
        let next = lines[idx + 1].text.trim();
        if !next.is_empty() && !next.starts_with('[') {
            rest = next;
            consumed = 2;
        } else {
            return None;
        }
    }
    if rest.is_empty() {
        return None;
    }

    let (url, after_url) = if let Some(inner) = rest.strip_prefix('<') {
        let end = inner.find('>')?;
        (inner[..end].to_string(), inner[end + 1..].trim())
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (rest[..end].to_string(), rest[end..].trim())
    };
    if url.is_empty() {
        return None;
    }

    let mut title = None;
    if !after_url.is_empty() {
        {
            let t = extract_quoted_title(after_url)?;
            title = Some(t);
        }
    } else if idx + consumed < lines.len() {
        let next = lines[idx + consumed].text.trim();
        if let Some(t) = extract_quoted_title(next) {
            title = Some(t);
            consumed += 1;
        }
    }

    Some((label, LinkDef { url, title }, consumed))
}

fn extract_quoted_title(s: &str) -> Option<String> {
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        || (s.starts_with('(') && s.ends_with(')') && s.len() >= 2)
    {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

fn normalize_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn parse_blocks(lines: &[Line<'_>], ctx: &mut ParseCtx, depth: usize) -> Vec<Block> {
    if depth > 16 {
        return Vec::new();
    }
    let mut blocks = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        let leading = count_leading_spaces(line.text);
        let no_indent = line.text.trim_start_matches([' ', '\t']);

        // 1. HTML comment block `<!-- ... -->`
        if leading <= 3 && no_indent.starts_with("<!--") {
            let mut j = i;
            let mut found = false;
            while j < lines.len() {
                if lines[j].text.contains("-->") {
                    found = true;
                    break;
                }
                j += 1;
            }
            i = if found { j + 1 } else { lines.len() };
            continue;
        }

        // 2. Collapsible `<details>` block
        if leading <= 3 && no_indent.to_ascii_lowercase().starts_with("<details")
            && let Some((details_block, next_i)) = parse_details_block(lines, i, ctx, depth) {
                blocks.push(details_block);
                i = next_i;
                continue;
            }

        // 3. Fenced Code Block (``` or ~~~)
        if leading <= 3
            && let Some((marker, len, info)) = parse_opening_fence(no_indent)
        {
            let source_line = line.line_no;
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                let cur_trim = lines[i].text.trim_start();
                if is_closing_fence(cur_trim, marker, len) {
                    i += 1;
                    break;
                }
                code_lines.push(lines[i].text);
                i += 1;
            }
            let code = code_lines.join("\n");
            let (lang_token, meta) = split_fence_info(info);
            if matches!(lang_token.to_ascii_lowercase().as_str(), "math" | "latex" | "katex") {
                blocks.push(Block::MathBlock {
                    tex: code,
                    source_line,
                });
            } else {
                let spans = highlight_code_block(&lang_token, &code);
                blocks.push(Block::CodeBlock {
                    lang: lang_token,
                    meta,
                    code,
                    spans,
                    source_line,
                });
            }
            continue;
        }

        // 4. Display Math Block (`$$ ... $$`)
        if leading <= 3 && no_indent.starts_with("$$") {
            let source_line = line.line_no;
            let after = no_indent[2..].trim();
            if after.ends_with("$$") && after.len() >= 2 {
                let tex = after[..after.len() - 2].trim().to_string();
                blocks.push(Block::MathBlock { tex, source_line });
                i += 1;
                continue;
            } else {
                let mut math_lines = Vec::new();
                if !after.is_empty() {
                    math_lines.push(after);
                }
                let mut j = i + 1;
                let mut closed = false;
                while j < lines.len() {
                    let t = lines[j].text.trim();
                    if let Some(prefix) = t.strip_suffix("$$") {
                        if !prefix.trim().is_empty() {
                            math_lines.push(prefix.trim());
                        }
                        closed = true;
                        j += 1;
                        break;
                    }
                    math_lines.push(lines[j].text);
                    j += 1;
                }
                if closed {
                    blocks.push(Block::MathBlock {
                        tex: math_lines.join("\n"),
                        source_line,
                    });
                    i = j;
                    continue;
                }
            }
        }

        // 5. Indented Code Block (4+ spaces or tab)
        if leading >= 4 {
            let source_line = line.line_no;
            let mut code_lines = Vec::new();
            while i < lines.len() {
                let t = lines[i].text;
                if t.trim().is_empty() {
                    // Include blank lines only if followed by another indented line
                    let mut j = i + 1;
                    while j < lines.len() && lines[j].text.trim().is_empty() {
                        j += 1;
                    }
                    if j < lines.len() && count_leading_spaces(lines[j].text) >= 4 {
                        code_lines.push("");
                        i += 1;
                        continue;
                    }
                    break;
                }
                if count_leading_spaces(t) >= 4 {
                    code_lines.push(strip_four_spaces(t));
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::CodeBlock {
                lang: String::new(),
                meta: String::new(),
                code: code_lines.join("\n"),
                spans: Vec::new(),
                source_line,
            });
            continue;
        }

        // 6. ATX Heading (`#`..`######`)
        if let Some((level, heading_text)) = parse_atx_heading(no_indent) {
            let inlines = parse_inlines(heading_text, ctx);
            let slug = ctx.register_heading(&inlines);
            blocks.push(Block::Heading {
                level,
                slug,
                inlines,
                source_line: line.line_no,
            });
            i += 1;
            continue;
        }

        // 7. Thematic Break (`---`, `***`, `___`)
        if is_thematic_break(no_indent) {
            blocks.push(Block::ThematicBreak);
            i += 1;
            continue;
        }

        // 8. Blockquote & Callouts (`> ...`)
        if no_indent.starts_with('>') {
            let mut quote_lines: Vec<Line<'_>> = Vec::new();
            while i < lines.len() {
                let cur = lines[i];
                let cur_trim = cur.text.trim_start();
                if let Some(after) = cur_trim.strip_prefix('>') {
                    let stripped = after.strip_prefix(' ').unwrap_or(after);
                    let delta = cur.text.len() - after.len() + usize::from(after.starts_with(' '));
                    quote_lines.push(Line {
                        text: stripped,
                        raw_start: cur.raw_start + delta,
                        line_no: cur.line_no,
                    });
                    i += 1;
                } else if !cur_trim.is_empty()
                    && !is_block_interrupt(cur_trim)
                    && !quote_lines.last().is_some_and(|l| l.text.trim().is_empty())
                {
                    // Lazy continuation line
                    quote_lines.push(cur);
                    i += 1;
                } else {
                    break;
                }
            }
            let (callout, inner_lines) = detect_callout(&quote_lines);
            let inner_blocks = parse_blocks(&inner_lines, ctx, depth + 1);
            blocks.push(Block::Blockquote {
                callout,
                blocks: inner_blocks,
            });
            continue;
        }

        // 9. List (Ordered / Unordered / Task List)
        if let Some(first_marker) = parse_list_marker(line.text, line.raw_start) {
            let (list_block, next_i) = parse_list(lines, i, first_marker, ctx, depth);
            blocks.push(list_block);
            i = next_i;
            continue;
        }

        // 10. GFM Pipe Table
        if i + 1 < lines.len()
            && line.text.contains('|')
            && let Some(alignments) = parse_table_delimiter(lines[i + 1].text)
        {
            let source_line = line.line_no;
            let raw_headers = split_table_row(line.text);
            if !raw_headers.is_empty() {
                let col_count = alignments.len().max(raw_headers.len());
                let mut norm_align = alignments;
                norm_align.resize(col_count, Align::Left);

                let headers: Vec<Vec<Inline>> = (0..col_count)
                    .map(|c| {
                        raw_headers
                            .get(c)
                            .map(|cell| parse_inlines(cell, ctx))
                            .unwrap_or_default()
                    })
                    .collect();

                let mut rows = Vec::new();
                i += 2;
                while i < lines.len() {
                    let row_text = lines[i].text.trim();
                    if row_text.is_empty() || !row_text.contains('|') || is_block_interrupt(row_text)
                    {
                        break;
                    }
                    let raw_cells = split_table_row(lines[i].text);
                    let row: Vec<Vec<Inline>> = (0..col_count)
                        .map(|c| {
                            raw_cells
                                .get(c)
                                .map(|cell| parse_inlines(cell, ctx))
                                .unwrap_or_default()
                        })
                        .collect();
                    rows.push(row);
                    i += 1;
                }

                blocks.push(Block::Table {
                    alignments: norm_align,
                    headers,
                    rows,
                    source_line,
                });
                continue;
            }
        }

        // 11. Simple HTML Block (`<hr>`, `<p align=...>`, `<h1..h6>`, `<blockquote>`, `<pre>`, `<img>`, `<br>`)
        if leading <= 3 && no_indent.starts_with('<') && !no_indent.starts_with("<http")
            && let Some((html_block, next_i)) = try_parse_html_block(lines, i, ctx) {
                blocks.push(html_block);
                i = next_i;
                continue;
            }

        // 12. Definition List (`Term` followed by `: Definition`)
        if i + 1 < lines.len() && lines[i + 1].text.trim_start().starts_with(": ") {
            let (def_block, next_i) = parse_definition_list(lines, i, ctx, depth);
            blocks.push(def_block);
            i = next_i;
            continue;
        }

        // 13. Paragraph (and Setext Heading check!)
        let source_line = line.line_no;
        let mut para_lines: Vec<&str> = vec![line.text];
        i += 1;
        let mut setext_level: Option<u8> = None;

        while i < lines.len() {
            let cur = lines[i].text;
            let cur_trim = cur.trim();
            if cur_trim.is_empty() {
                break;
            }
            if para_lines.len() == 1
                && let Some(lvl) = parse_setext_underline(cur_trim)
            {
                setext_level = Some(lvl);
                i += 1;
                break;
            }
            if is_paragraph_interrupt(cur) {
                break;
            }
            para_lines.push(cur);
            i += 1;
        }

        if let Some(level) = setext_level {
            let text = para_lines[0].trim();
            let inlines = parse_inlines(text, ctx);
            let slug = ctx.register_heading(&inlines);
            blocks.push(Block::Heading {
                level,
                slug,
                inlines,
                source_line,
            });
        } else {
            let inlines = parse_paragraph_inlines(&para_lines, ctx);
            blocks.push(Block::Paragraph {
                inlines,
                align: None,
            });
        }
    }

    blocks
}

fn parse_paragraph_inlines(lines: &[&str], ctx: &mut ParseCtx) -> Vec<Inline> {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed_start = line.trim_start();
        let hard_spaces = line.ends_with("  ");
        let hard_slash = trimmed_start.ends_with('\\') && !trimmed_start.ends_with("\\\\");
        let clean = if hard_slash {
            &trimmed_start[..trimmed_start.len() - 1]
        } else {
            trimmed_start.trim_end()
        };
        out.extend(parse_inlines(clean, ctx));
        if idx + 1 < lines.len() {
            if hard_spaces || hard_slash {
                out.push(Inline::HardBreak);
            } else {
                out.push(Inline::SoftBreak);
            }
        }
    }
    out
}

fn parse_details_block(
    lines: &[Line<'_>],
    start: usize,
    ctx: &mut ParseCtx,
    depth: usize,
) -> Option<(Block, usize)> {
    let source_line = lines[start].line_no;
    let first_lower = lines[start].text.to_ascii_lowercase();
    let open_by_default = first_lower.contains(" open");

    let mut summary_text = String::new();
    let mut inner_lines: Vec<Line<'_>> = Vec::new();
    let mut j = start + 1;
    let mut closed = false;

    while j < lines.len() {
        let t = lines[j].text;
        let lower = t.trim().to_ascii_lowercase();
        if lower == "</details>" || lower.contains("</details>") {
            closed = true;
            j += 1;
            break;
        }
        if summary_text.is_empty()
            && let Some(s_start) = lower.find("<summary>")
            && let Some(s_end) = lower[s_start + 9..].find("</summary>")
        {
            let slice_start = s_start + 9;
            summary_text = t.trim()[slice_start..slice_start + s_end].trim().to_string();
            j += 1;
            continue;
        }
        inner_lines.push(lines[j]);
        j += 1;
    }

    if !closed {
        return None;
    }
    if summary_text.is_empty() {
        summary_text = "详情 (Details)".to_string();
    }
    let summary = parse_inlines(&summary_text, ctx);
    let blocks = parse_blocks(&inner_lines, ctx, depth + 1);
    Some((
        Block::Details {
            summary,
            blocks,
            open_by_default,
            source_line,
        },
        j,
    ))
}

fn try_parse_html_block(
    lines: &[Line<'_>],
    start: usize,
    ctx: &mut ParseCtx,
) -> Option<(Block, usize)> {
    let source_line = lines[start].line_no;
    let raw = lines[start].text.trim();
    let lower = raw.to_ascii_lowercase();

    if lower == "<hr>" || lower == "<hr/>" || lower == "<hr />" {
        return Some((Block::ThematicBreak, start + 1));
    }

    // Single-line `<p align="center">...</p>` or `<div align="center">...</div>`
    for tag in ["p", "div"] {
        let open_prefix = format!("<{tag}");
        let close_tag = format!("</{tag}>");
        if lower.starts_with(&open_prefix)
            && let Some(gt) = raw.find('>')
        {
            let attrs = &lower[..gt];
            let align = if attrs.contains("center") {
                Some(Align::Center)
            } else if attrs.contains("right") {
                Some(Align::Right)
            } else if attrs.contains("left") {
                Some(Align::Left)
            } else {
                None
            };
            if let Some(close_pos) = lower.rfind(&close_tag)
                && close_pos > gt
            {
                let inner = raw[gt + 1..close_pos].trim();
                let inlines = parse_inlines(inner, ctx);
                return Some((Block::Paragraph { inlines, align }, start + 1));
            }
            // Multi-line `<div align="center"> ... </div>`
            let mut j = start + 1;
            let mut inner_parts = Vec::new();
            let tail = raw[gt + 1..].trim();
            if !tail.is_empty() {
                inner_parts.push(tail);
            }
            while j < lines.len() {
                let t = lines[j].text.trim();
                if let Some(pos) = t.to_ascii_lowercase().find(&close_tag) {
                    let before = t[..pos].trim();
                    if !before.is_empty() {
                        inner_parts.push(before);
                    }
                    let joined = inner_parts.join(" ");
                    let inlines = parse_inlines(&joined, ctx);
                    return Some((Block::Paragraph { inlines, align }, j + 1));
                }
                if !t.is_empty() {
                    inner_parts.push(t);
                }
                j += 1;
            }
        }
    }

    // Single-line `<img ...>`
    if lower.starts_with("<img ")
        && let Some(img) = parse_html_img_tag(raw)
    {
        return Some((
            Block::Paragraph {
                inlines: vec![img],
                align: None,
            },
            start + 1,
        ));
    }

    // Single-line `<h1>..</h1>` .. `<h6>..</h6>`
    for lvl in 1u8..=6 {
        let open = format!("<h{lvl}");
        let close = format!("</h{lvl}>");
        if lower.starts_with(&open)
            && let Some(gt) = raw.find('>')
            && let Some(end) = lower.rfind(&close)
            && end > gt
        {
            let inner = raw[gt + 1..end].trim();
            let inlines = parse_inlines(inner, ctx);
            let slug = ctx.register_heading(&inlines);
            return Some((
                Block::Heading {
                    level: lvl,
                    slug,
                    inlines,
                    source_line,
                },
                start + 1,
            ));
        }
    }

    // Single-line `<br>` / `<br/>` -> blank break
    if lower == "<br>" || lower == "<br/>" || lower == "<br />" {
        return Some((
            Block::Paragraph {
                inlines: vec![Inline::HardBreak],
                align: None,
            },
            start + 1,
        ));
    }

    // Multi-line or single-line raw HTML block fallback
    if lower.starts_with("<table")
        || lower.starts_with("<pre")
        || lower.starts_with("<blockquote")
        || lower.starts_with("<style")
        || lower.starts_with("<script")
    {
        let mut collected = vec![lines[start].text];
        let mut j = start + 1;
        while j < lines.len() && !lines[j].text.trim().is_empty() {
            collected.push(lines[j].text);
            j += 1;
        }
        return Some((
            Block::HtmlBlock {
                html: collected.join("\n"),
            },
            j,
        ));
    }

    None
}

fn parse_definition_list(
    lines: &[Line<'_>],
    start: usize,
    ctx: &mut ParseCtx,
    depth: usize,
) -> (Block, usize) {
    let mut items = Vec::new();
    let mut i = start;

    while i + 1 < lines.len() && lines[i + 1].text.trim_start().starts_with(": ") {
        let term = parse_inlines(lines[i].text.trim(), ctx);
        i += 1;
        let mut definitions = Vec::new();
        while i < lines.len() && lines[i].text.trim_start().starts_with(": ") {
            let def_first = lines[i].text.trim_start()[2..].trim_start();
            let mut def_lines = vec![Line {
                text: def_first,
                raw_start: lines[i].raw_start,
                line_no: lines[i].line_no,
            }];
            i += 1;
            while i < lines.len() {
                let t = lines[i].text;
                if t.trim().is_empty()
                    || t.trim_start().starts_with(": ")
                    || is_block_interrupt(t.trim_start())
                {
                    break;
                }
                if t.starts_with("  ") || t.starts_with('\t') {
                    def_lines.push(Line {
                        text: t.trim_start(),
                        raw_start: lines[i].raw_start,
                        line_no: lines[i].line_no,
                    });
                    i += 1;
                } else {
                    break;
                }
            }
            definitions.push(parse_blocks(&def_lines, ctx, depth + 1));
        }
        items.push(DefItem { term, definitions });
        if i < lines.len()
            && lines[i].text.trim().is_empty()
            && i + 2 < lines.len()
            && lines[i + 2].text.trim_start().starts_with(": ")
        {
            i += 1;
        }
    }

    (Block::DefinitionList { items }, i)
}

fn detect_callout<'a>(lines: &[Line<'a>]) -> (Option<(CalloutKind, Option<String>)>, Vec<Line<'a>>) {
    let Some(first) = lines.first() else {
        return (None, Vec::new());
    };
    let trimmed = first.text.trim();
    if let Some(after_open) = trimmed.strip_prefix("[!")
        && let Some(close) = after_open.find(']')
    {
        let tag = &after_open[..close];
        if let Some(kind) = CalloutKind::from_tag(tag) {
            let rest_title = after_open[close + 1..].trim();
            let custom_title = (!rest_title.is_empty()).then(|| rest_title.to_string());
            return (Some((kind, custom_title)), lines[1..].to_vec());
        }
    }
    (None, lines.to_vec())
}

#[derive(Clone, Copy)]
struct ListMarker {
    ordered: bool,
    start: usize,
    delim: char,
    indent: usize,
    content_col: usize,
    content_byte: usize,
}

fn parse_list_marker(line: &str, _raw_start: usize) -> Option<ListMarker> {
    let indent = count_leading_spaces(line);
    if indent > 3 {
        return None;
    }
    let rest = line.trim_start_matches([' ', '\t']);
    let lead_bytes = line.len() - rest.len();
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if matches!(bytes[0], b'-' | b'*' | b'+') {
        if bytes.len() == 1 || bytes[1] == b' ' || bytes[1] == b'\t' {
            // Do not mistake thematic break `---` or `***` for list marker
            if is_thematic_break(rest) {
                return None;
            }
            let marker_len = if bytes.len() > 1 { 2 } else { 1 };
            return Some(ListMarker {
                ordered: false,
                start: 1,
                delim: bytes[0] as char,
                indent,
                content_col: indent + marker_len,
                content_byte: lead_bytes + marker_len,
            });
        }
        return None;
    }

    let digits_end = rest
        .bytes()
        .take_while(|b| b.is_ascii_digit())
        .count()
        .min(9);
    if digits_end == 0 || digits_end >= bytes.len() {
        return None;
    }
    let delim = bytes[digits_end] as char;
    if delim != '.' && delim != ')' {
        return None;
    }
    if digits_end + 1 < bytes.len() && !matches!(bytes[digits_end + 1], b' ' | b'\t') {
        return None;
    }
    let num: usize = rest[..digits_end].parse().ok()?;
    let after_len = if digits_end + 1 < bytes.len() { 2 } else { 1 };
    Some(ListMarker {
        ordered: true,
        start: num,
        delim,
        indent,
        content_col: indent + digits_end + after_len,
        content_byte: lead_bytes + digits_end + after_len,
    })
}

fn parse_list(
    lines: &[Line<'_>],
    start_idx: usize,
    first_marker: ListMarker,
    ctx: &mut ParseCtx,
    depth: usize,
) -> (Block, usize) {
    let mut items = Vec::new();
    let mut tight = true;
    let mut i = start_idx;

    while i < lines.len() {
        let cur = lines[i];
        if cur.text.trim().is_empty() {
            let mut j = i + 1;
            while j < lines.len() && lines[j].text.trim().is_empty() {
                j += 1;
            }
            if j < lines.len()
                && let Some(next_m) = parse_list_marker(lines[j].text, lines[j].raw_start)
                && next_m.ordered == first_marker.ordered
                && next_m.indent <= first_marker.indent + 1
                && (!first_marker.ordered || next_m.delim == first_marker.delim)
            {
                tight = false;
                i = j;
                continue;
            }
            break;
        }

        let Some(marker) = parse_list_marker(cur.text, cur.raw_start) else {
            break;
        };
        if marker.ordered != first_marker.ordered
            || (first_marker.ordered && marker.delim != first_marker.delim)
            || marker.indent > first_marker.indent + 1
        {
            break;
        }

        let after_marker = &cur.text[marker.content_byte..];
        let (checked, checkbox_byte, body_first) =
            parse_task_prefix(after_marker, cur.raw_start + marker.content_byte);

        let mut item_lines = vec![Line {
            text: body_first,
            raw_start: cur.raw_start + cur.text.len() - body_first.len(),
            line_no: cur.line_no,
        }];
        i += 1;

        while i < lines.len() {
            let next = lines[i];
            if next.text.trim().is_empty() {
                let mut j = i + 1;
                while j < lines.len() && lines[j].text.trim().is_empty() {
                    j += 1;
                }
                if j < lines.len() && count_leading_spaces(lines[j].text) >= marker.content_col {
                    item_lines.push(Line {
                        text: "",
                        raw_start: next.raw_start,
                        line_no: next.line_no,
                    });
                    i += 1;
                    continue;
                }
                break;
            }
            let next_indent = count_leading_spaces(next.text);
            if next_indent >= marker.content_col {
                let stripped = slice_from_col(next.text, marker.content_col);
                let delta = next.text.len() - stripped.len();
                item_lines.push(Line {
                    text: stripped,
                    raw_start: next.raw_start + delta,
                    line_no: next.line_no,
                });
                i += 1;
            } else if parse_list_marker(next.text, next.raw_start).is_none()
                && !is_block_interrupt(next.text.trim_start())
                && !item_lines.last().is_some_and(|l| l.text.trim().is_empty())
            {
                item_lines.push(next);
                i += 1;
            } else {
                break;
            }
        }

        let item_blocks = parse_blocks(&item_lines, ctx, depth + 1);
        items.push(ListItem {
            checked,
            checkbox_byte,
            blocks: item_blocks,
        });
    }

    (
        Block::List {
            ordered: first_marker.ordered,
            start: first_marker.start,
            tight,
            items,
        },
        i,
    )
}

fn parse_task_prefix(s: &str, base_byte: usize) -> (Option<bool>, Option<usize>, &str) {
    let bytes = s.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'['
        && bytes[2] == b']'
        && matches!(bytes[3], b' ' | b'\t')
        && matches!(bytes[1], b' ' | b'x' | b'X')
    {
        let checked = bytes[1] != b' ';
        return (Some(checked), Some(base_byte + 1), &s[4..]);
    }
    (None, None, s)
}

fn slice_from_col(line: &str, target_col: usize) -> &str {
    let mut col = 0usize;
    for (idx, ch) in line.char_indices() {
        if col >= target_col {
            return &line[idx..];
        }
        match ch {
            ' ' => col += 1,
            '\t' => col += 4 - (col % 4),
            _ => return &line[idx..],
        }
    }
    ""
}

fn count_leading_spaces(line: &str) -> usize {
    let mut cols = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => cols += 1,
            '\t' => cols += 4 - (cols % 4),
            _ => break,
        }
    }
    cols
}

fn strip_four_spaces(line: &str) -> &str {
    slice_from_col(line, 4)
}

fn parse_opening_fence(trimmed: &str) -> Option<(u8, usize, &str)> {
    let bytes = trimmed.as_bytes();
    let marker = *bytes.first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = bytes.iter().take_while(|b| **b == marker).count();
    if len < 3 {
        return None;
    }
    let info = trimmed[len..].trim();
    if marker == b'`' && info.contains('`') {
        return None;
    }
    Some((marker, len, info))
}

fn is_closing_fence(trimmed: &str, marker: u8, open_len: usize) -> bool {
    let bytes = trimmed.trim_end().as_bytes();
    bytes.len() >= open_len && bytes.iter().all(|b| *b == marker)
}

fn split_fence_info(info: &str) -> (String, String) {
    let mut parts = info.splitn(2, char::is_whitespace);
    let lang = parts.next().unwrap_or("").trim().to_string();
    let meta = parts.next().unwrap_or("").trim().to_string();
    (lang, meta)
}

fn highlight_code_block(lang: &str, code: &str) -> Vec<Span> {
    if code.is_empty() || code.len() > 200_000 {
        return Vec::new();
    }
    let language = match lang.to_ascii_lowercase().as_str() {
        "rs" | "rust" => Language::Rust,
        "py" | "python" => Language::Python,
        "js" | "javascript" | "mjs" | "cjs" => Language::JavaScript,
        "ts" | "typescript" => Language::TypeScript,
        "tsx" | "jsx" => Language::Tsx,
        "json" | "jsonc" | "json5" => Language::Json,
        "toml" => Language::Toml,
        "c" => Language::C,
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "h" => Language::Cpp,
        "go" | "golang" => Language::Go,
        "java" => Language::Java,
        "cs" | "csharp" | "c#" => Language::CSharp,
        "html" | "htm" | "xml" | "svg" => Language::Html,
        "css" | "scss" | "less" => Language::Css,
        "sh" | "bash" | "zsh" | "shell" | "console" => Language::Bash,
        "yml" | "yaml" => Language::Yaml,
        _ => return Vec::new(),
    };
    let mut state = HighlightState::new(language);
    state.spans(code, 1).to_vec()
}

fn parse_atx_heading(trimmed: &str) -> Option<(u8, &str)> {
    let level = trimmed.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let mut text = rest.trim();
    let no_closing = text.trim_end_matches('#');
    if no_closing.is_empty() {
        text = "";
    } else if no_closing.len() < text.len() && no_closing.ends_with([' ', '\t']) {
        text = no_closing.trim_end();
    }
    Some((level as u8, text))
}

fn parse_setext_underline(trimmed: &str) -> Option<u8> {
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b == b'=') {
        Some(1)
    } else if trimmed.bytes().all(|b| b == b'-') && trimmed.len() >= 2 {
        Some(2)
    } else {
        None
    }
}

fn is_thematic_break(trimmed: &str) -> bool {
    let mut marker = None;
    let mut count = 0usize;
    for b in trimmed.bytes() {
        if b == b' ' || b == b'\t' {
            continue;
        }
        if !matches!(b, b'-' | b'*' | b'_') {
            return false;
        }
        match marker {
            None => marker = Some(b),
            Some(m) if m != b => return false,
            _ => {}
        }
        count += 1;
    }
    count >= 3
}

fn is_block_interrupt(trimmed: &str) -> bool {
    trimmed.starts_with('#')
        || trimmed.starts_with('>')
        || trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || trimmed.starts_with("$$")
        || is_thematic_break(trimmed)
        || parse_list_marker(trimmed, 0).is_some()
}

fn is_paragraph_interrupt(line: &str) -> bool {
    let lead = count_leading_spaces(line);
    if lead > 3 {
        return false;
    }
    let trimmed = line.trim_start();
    parse_atx_heading(trimmed).is_some()
        || trimmed.starts_with('>')
        || parse_opening_fence(trimmed).is_some()
        || trimmed.starts_with("$$")
        || is_thematic_break(trimmed)
        || parse_list_marker(line, 0).is_some()
}

fn parse_table_delimiter(line: &str) -> Option<Vec<Align>> {
    let cells = split_table_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let c = cell.trim();
        if c.is_empty() {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let inner = c
            .strip_prefix(':')
            .unwrap_or(c)
            .strip_suffix(':')
            .unwrap_or(c.strip_prefix(':').unwrap_or(c));
        if inner.is_empty() || !inner.bytes().all(|b| b == b'-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(aligns)
}

fn split_table_row(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if let Some(rest) = s.strip_prefix('|') {
        s = rest;
    }
    if s.ends_with('|') && !s.ends_with("\\|") {
        s = &s[..s.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_code = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(ch);
            }
            '|' if !in_code => {
                cells.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

// ---------------------------------------------------------------------------
// Inline Parser
// ---------------------------------------------------------------------------

fn parse_inlines(input: &str, ctx: &mut ParseCtx) -> Vec<Inline> {
    parse_inlines_depth(input, ctx, 0)
}

fn parse_inlines_depth(input: &str, ctx: &mut ParseCtx, depth: usize) -> Vec<Inline> {
    if depth > 16 || input.is_empty() {
        return if input.is_empty() {
            Vec::new()
        } else {
            vec![Inline::Text(input.to_string())]
        };
    }
    let mut out = Vec::new();
    let mut text_buf = String::new();
    let bytes = input.as_bytes();
    let mut i = 0usize;

    let flush_text = |buf: &mut String, out: &mut Vec<Inline>| {
        if !buf.is_empty() {
            out.push(Inline::Text(std::mem::take(buf)));
        }
    };

    while i < bytes.len() {
        let ch = input[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();

        match ch {
            // Backslash escape
            '\\' if i + 1 < bytes.len() => {
                let next = input[i + 1..].chars().next().unwrap();
                if is_ascii_punct(next) {
                    text_buf.push(next);
                    i += 1 + next.len_utf8();
                    continue;
                }
                text_buf.push('\\');
                i += 1;
            }

            // Inline code span `` `...` ``
            '`' => {
                let run = count_byte_run(bytes, i, b'`');
                if let Some(close) = find_closing_backticks(bytes, i + run, run) {
                    flush_text(&mut text_buf, &mut out);
                    let raw = &input[i + run..close];
                    let normalized = normalize_code_span(raw);
                    out.push(Inline::Code(normalized));
                    i = close + run;
                    continue;
                }
                for _ in 0..run {
                    text_buf.push('`');
                }
                i += run;
            }

            // Inline math `$...$`
            '$' => {
                if bytes.get(i + 1) != Some(&b'$')
                    && let Some((tex, end)) = parse_inline_math(input, i)
                {
                    flush_text(&mut text_buf, &mut out);
                    out.push(Inline::Math(tex));
                    i = end;
                    continue;
                }
                text_buf.push('$');
                i += 1;
            }

            // Image `![alt](url)` or `![alt][ref]`
            '!' if bytes.get(i + 1) == Some(&b'[') => {
                if let Some((img, end)) = parse_image(input, i, ctx) {
                    flush_text(&mut text_buf, &mut out);
                    out.push(img);
                    i = end;
                    continue;
                }
                text_buf.push('!');
                i += 1;
            }

            // Footnote ref `[^id]` or Link `[text](url)` / `[text][ref]` / `[ref]`
            '[' => {
                if bytes.get(i + 1) == Some(&b'^')
                    && let Some((id, end)) = parse_footnote_ref(input, i)
                    && ctx.footnote_defs.contains_key(&id)
                {
                    flush_text(&mut text_buf, &mut out);
                    let number = ctx.register_footnote(&id);
                    out.push(Inline::FootnoteRef { id, number });
                    i = end;
                    continue;
                }
                if let Some((link, end)) = parse_link(input, i, ctx, depth) {
                    flush_text(&mut text_buf, &mut out);
                    out.push(link);
                    i = end;
                    continue;
                }
                text_buf.push('[');
                i += 1;
            }

            // Emphasis / Strong (`*` or `_`)
            '*' | '_' => {
                let marker = ch as u8;
                let prev_alnum = i > 0
                    && input[..i]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric());
                let run = count_byte_run(bytes, i, marker).min(3);
                if marker == b'_' && prev_alnum {
                    text_buf.push(ch);
                    i += 1;
                    continue;
                }
                if let Some((inner, end, used_run)) =
                    find_delim_span(input, i, marker, run)
                {
                    flush_text(&mut text_buf, &mut out);
                    let children = parse_inlines_depth(inner, ctx, depth + 1);
                    let node = match used_run {
                        3 => Inline::Strong(vec![Inline::Emphasis(children)]),
                        2 => Inline::Strong(children),
                        _ => Inline::Emphasis(children),
                    };
                    out.push(node);
                    i = end;
                    continue;
                }
                text_buf.push(ch);
                i += 1;
            }

            // Strikethrough `~~...~~` or Subscript `~...~`
            '~' => {
                if bytes.get(i + 1) == Some(&b'~') {
                    if let Some((inner, end)) = find_double_delim(input, i, b'~') {
                        flush_text(&mut text_buf, &mut out);
                        let children = parse_inlines_depth(inner, ctx, depth + 1);
                        out.push(Inline::Strikethrough(children));
                        i = end;
                        continue;
                    }
                } else if let Some((inner, end)) = find_single_inline_delim(input, i, b'~') {
                    flush_text(&mut text_buf, &mut out);
                    let children = parse_inlines_depth(inner, ctx, depth + 1);
                    out.push(Inline::Subscript(children));
                    i = end;
                    continue;
                }
                text_buf.push('~');
                i += 1;
            }

            // Highlight `==...==`
            '=' if bytes.get(i + 1) == Some(&b'=') => {
                if let Some((inner, end)) = find_double_delim(input, i, b'=') {
                    flush_text(&mut text_buf, &mut out);
                    let children = parse_inlines_depth(inner, ctx, depth + 1);
                    out.push(Inline::Highlight(children));
                    i = end;
                    continue;
                }
                text_buf.push('=');
                i += 1;
            }

            // Underline `++...++`
            '+' if bytes.get(i + 1) == Some(&b'+') => {
                if let Some((inner, end)) = find_double_delim(input, i, b'+') {
                    flush_text(&mut text_buf, &mut out);
                    let children = parse_inlines_depth(inner, ctx, depth + 1);
                    out.push(Inline::Underline(children));
                    i = end;
                    continue;
                }
                text_buf.push('+');
                i += 1;
            }

            // Superscript `^...^`
            '^' => {
                if let Some((inner, end)) = find_single_inline_delim(input, i, b'^') {
                    flush_text(&mut text_buf, &mut out);
                    let children = parse_inlines_depth(inner, ctx, depth + 1);
                    out.push(Inline::Superscript(children));
                    i = end;
                    continue;
                }
                text_buf.push('^');
                i += 1;
            }

            // Autolink `<https://...>` or inline HTML (`<br>`, `<kbd>`, `<b>`, `<a>`, `<img>`, etc.)
            '<' => {
                if input[i..].starts_with("<!--") {
                    if let Some(rel) = input[i + 4..].find("-->") {
                        i += 4 + rel + 3;
                        continue;
                    }
                } else if let Some((node, end)) = parse_angle_or_html(input, i, ctx, depth) {
                    flush_text(&mut text_buf, &mut out);
                    if let Some(inline) = node {
                        out.push(inline);
                    }
                    i = end;
                    continue;
                }
                text_buf.push('<');
                i += 1;
            }

            // HTML Entities `&amp;`, `&#123;`, `&#x1F600;`
            '&' => {
                if let Some((decoded, end)) = decode_entity(input, i) {
                    text_buf.push_str(&decoded);
                    i = end;
                    continue;
                }
                text_buf.push('&');
                i += 1;
            }

            // Emoji shortcode `:rocket:`
            ':' => {
                if let Some((emoji, end)) = parse_emoji_shortcode(input, i) {
                    text_buf.push_str(emoji);
                    i = end;
                    continue;
                }
                text_buf.push(':');
                i += 1;
            }

            // Bare URL autolink (`https://...`, `http://...`, `www....`)
            'h' | 'w' if is_url_boundary(input, i) => {
                if let Some((url, end)) = scan_bare_url(input, i) {
                    flush_text(&mut text_buf, &mut out);
                    let full_url = if url.starts_with("www.") {
                        format!("https://{url}")
                    } else {
                        url.clone()
                    };
                    out.push(Inline::Link {
                        text: vec![Inline::Text(url)],
                        url: full_url,
                        title: None,
                    });
                    i = end;
                    continue;
                }
                text_buf.push(ch);
                i += ch_len;
            }

            _ => {
                text_buf.push(ch);
                i += ch_len;
            }
        }
    }

    flush_text(&mut text_buf, &mut out);
    out
}

fn is_ascii_punct(ch: char) -> bool {
    ch.is_ascii_punctuation()
}

fn count_byte_run(bytes: &[u8], start: usize, target: u8) -> usize {
    bytes[start..].iter().take_while(|b| **b == target).count()
}

fn find_closing_backticks(bytes: &[u8], mut i: usize, run: usize) -> Option<usize> {
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let found = count_byte_run(bytes, i, b'`');
            if found == run {
                return Some(i);
            }
            i += found;
        } else {
            i += 1;
        }
    }
    None
}

fn normalize_code_span(raw: &str) -> String {
    let s = raw.replace('\n', " ");
    if s.len() >= 2 && s.starts_with(' ') && s.ends_with(' ') && s.bytes().any(|b| b != b' ') {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
}

fn parse_inline_math(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    let after = start + 1;
    if after >= bytes.len() || bytes[after].is_ascii_whitespace() {
        return None;
    }
    let mut i = after;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'$' {
            if i == after || bytes[i - 1].is_ascii_whitespace() {
                return None;
            }
            // Avoid currency like `$10 and $20`
            if bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit()) {
                return None;
            }
            return Some((input[after..i].to_string(), i + 1));
        }
        i += 1;
    }
    None
}

fn find_delim_span(
    input: &str,
    start: usize,
    marker: u8,
    max_run: usize,
) -> Option<(&str, usize, usize)> {
    let bytes = input.as_bytes();
    for run in (1..=max_run).rev() {
        let content_start = start + run;
        if content_start >= bytes.len() {
            continue;
        }
        if bytes[content_start].is_ascii_whitespace() {
            continue;
        }
        let mut i = content_start;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'`' {
                let tick_run = count_byte_run(bytes, i, b'`');
                if let Some(close) = find_closing_backticks(bytes, i + tick_run, tick_run) {
                    i = close + tick_run;
                    continue;
                }
                i += tick_run;
                continue;
            }
            if bytes[i] == marker {
                let found = count_byte_run(bytes, i, marker);
                if found >= run && i > content_start && !bytes[i - 1].is_ascii_whitespace() {
                    if marker == b'_'
                        && bytes.get(i + run).is_some_and(|b| b.is_ascii_alphanumeric())
                    {
                        i += found;
                        continue;
                    }
                    return Some((&input[content_start..i], i + run, run));
                }
                i += found;
                continue;
            }
            i += 1;
        }
    }
    None
}

fn find_double_delim(input: &str, start: usize, marker: u8) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    let content_start = start + 2;
    if content_start >= bytes.len() || bytes[content_start].is_ascii_whitespace() {
        return None;
    }
    let mut i = content_start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == marker && bytes[i + 1] == marker && i > content_start
            && !bytes[i - 1].is_ascii_whitespace() {
                return Some((&input[content_start..i], i + 2));
            }
        i += 1;
    }
    None
}

fn find_single_inline_delim(
    input: &str,
    start: usize,
    marker: u8,
) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    let content_start = start + 1;
    if content_start >= bytes.len() || bytes[content_start].is_ascii_whitespace() {
        return None;
    }
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == marker && bytes.get(i + 1) != Some(&marker) {
            if i > content_start && !bytes[i - 1].is_ascii_whitespace() {
                return Some((&input[content_start..i], i + 1));
            }
            return None;
        }
        i += 1;
    }
    None
}

fn find_matching_bracket(input: &str, open_bracket: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut depth = 0usize;
    let mut i = open_bracket;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => {
                let run = count_byte_run(bytes, i, b'`');
                if let Some(close) = find_closing_backticks(bytes, i + run, run) {
                    i = close + run;
                } else {
                    i += run;
                }
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn parse_inline_dest_and_title(input: &str, paren_start: usize) -> Option<(String, Option<String>, usize)> {
    let bytes = input.as_bytes();
    if bytes.get(paren_start) != Some(&b'(') {
        return None;
    }
    let mut i = paren_start + 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let url = if bytes.get(i) == Some(&b'<') {
        i += 1;
        let url_start = i;
        while i < bytes.len() && bytes[i] != b'>' {
            if bytes[i] == b'\\' {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        let u = input[url_start..i].to_string();
        i += 1;
        u
    } else {
        let url_start = i;
        let mut paren_depth = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'(' => {
                    paren_depth += 1;
                    i += 1;
                }
                b')' => {
                    if paren_depth == 0 {
                        break;
                    }
                    paren_depth -= 1;
                    i += 1;
                }
                b if b.is_ascii_whitespace() => break,
                _ => i += 1,
            }
        }
        input[url_start..i].to_string()
    };

    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut title = None;
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
        let q = bytes[i];
        i += 1;
        let t_start = i;
        while i < bytes.len() && bytes[i] != q {
            if bytes[i] == b'\\' {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        title = Some(input[t_start..i].to_string());
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    if bytes.get(i) != Some(&b')') {
        return None;
    }
    Some((url, title, i + 1))
}

fn parse_image(input: &str, bang: usize, ctx: &ParseCtx) -> Option<(Inline, usize)> {
    let close = find_matching_bracket(input, bang + 1)?;
    let alt = input[bang + 2..close].to_string();
    let bytes = input.as_bytes();
    match bytes.get(close + 1) {
        Some(b'(') => {
            let (url, title, end) = parse_inline_dest_and_title(input, close + 1)?;
            Some((
                Inline::Image {
                    alt,
                    url,
                    title,
                    width: None,
                },
                end,
            ))
        }
        Some(b'[') => {
            let ref_close = find_matching_bracket(input, close + 1)?;
            let raw_ref = &input[close + 2..ref_close];
            let label = if raw_ref.trim().is_empty() {
                normalize_label(&alt)
            } else {
                normalize_label(raw_ref)
            };
            let def = ctx.link_defs.get(&label)?;
            Some((
                Inline::Image {
                    alt,
                    url: def.url.clone(),
                    title: def.title.clone(),
                    width: None,
                },
                ref_close + 1,
            ))
        }
        _ => {
            let def = ctx.link_defs.get(&normalize_label(&alt))?;
            Some((
                Inline::Image {
                    alt,
                    url: def.url.clone(),
                    title: def.title.clone(),
                    width: None,
                },
                close + 1,
            ))
        }
    }
}

fn parse_link(
    input: &str,
    bracket: usize,
    ctx: &mut ParseCtx,
    depth: usize,
) -> Option<(Inline, usize)> {
    let close = find_matching_bracket(input, bracket)?;
    let raw_label = &input[bracket + 1..close];
    let bytes = input.as_bytes();
    match bytes.get(close + 1) {
        Some(b'(') => {
            let (url, title, end) = parse_inline_dest_and_title(input, close + 1)?;
            let text = parse_inlines_depth(raw_label, ctx, depth + 1);
            Some((Inline::Link { text, url, title }, end))
        }
        Some(b'[') => {
            let ref_close = find_matching_bracket(input, close + 1)?;
            let raw_ref = &input[close + 2..ref_close];
            let key = if raw_ref.trim().is_empty() {
                normalize_label(raw_label)
            } else {
                normalize_label(raw_ref)
            };
            let def = ctx.link_defs.get(&key)?.clone();
            let text = parse_inlines_depth(raw_label, ctx, depth + 1);
            Some((
                Inline::Link {
                    text,
                    url: def.url,
                    title: def.title,
                },
                ref_close + 1,
            ))
        }
        _ => {
            let def = ctx.link_defs.get(&normalize_label(raw_label))?.clone();
            let text = parse_inlines_depth(raw_label, ctx, depth + 1);
            Some((
                Inline::Link {
                    text,
                    url: def.url,
                    title: def.title,
                },
                close + 1,
            ))
        }
    }
}

fn parse_footnote_ref(input: &str, bracket: usize) -> Option<(String, usize)> {
    let rest = &input[bracket + 2..];
    let close = rest.find(']')?;
    let id = rest[..close].trim();
    if id.is_empty() || id.contains(char::is_whitespace) {
        return None;
    }
    Some((id.to_string(), bracket + 2 + close + 1))
}

fn parse_angle_or_html(
    input: &str,
    start: usize,
    ctx: &mut ParseCtx,
    depth: usize,
) -> Option<(Option<Inline>, usize)> {
    let rest = &input[start + 1..];
    let gt_rel = rest.find('>')?;
    let tag_inner = &rest[..gt_rel];
    let tag_end = start + 1 + gt_rel + 1;

    // Autolink `<https://...>` or `<email@host>`
    if !tag_inner.contains(char::is_whitespace) {
        let lower = tag_inner.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:")
        {
            let display = tag_inner.strip_prefix("mailto:").unwrap_or(tag_inner);
            return Some((
                Some(Inline::Link {
                    text: vec![Inline::Text(display.to_string())],
                    url: tag_inner.to_string(),
                    title: None,
                }),
                tag_end,
            ));
        }
        if tag_inner.contains('@') && tag_inner.contains('.') && !tag_inner.contains('<') {
            return Some((
                Some(Inline::Link {
                    text: vec![Inline::Text(tag_inner.to_string())],
                    url: format!("mailto:{tag_inner}"),
                    title: None,
                }),
                tag_end,
            ));
        }
    }

    let tag_trim = tag_inner.trim();
    let tag_lower = tag_trim.to_ascii_lowercase();
    if tag_lower == "br" || tag_lower == "br/" || tag_lower == "br /" {
        return Some((Some(Inline::HardBreak), tag_end));
    }
    if (tag_lower.starts_with("img ") || tag_lower.starts_with("img\t"))
        && let Some(img) = parse_html_img_tag(&input[start..tag_end]) {
            return Some((Some(img), tag_end));
        }

    let tag_name = tag_lower
        .split([' ', '\t', '/'])
        .next()
        .unwrap_or("");
    if matches!(
        tag_name,
        "b" | "strong"
            | "i"
            | "em"
            | "u"
            | "ins"
            | "s"
            | "del"
            | "strike"
            | "code"
            | "kbd"
            | "mark"
            | "sup"
            | "sub"
            | "a"
            | "span"
    ) {
        let close_needle = format!("</{tag_name}>");
        let after = &input[tag_end..];
        if let Some(close_rel) = after.to_ascii_lowercase().find(&close_needle) {
            let inner = &after[..close_rel];
            let end = tag_end + close_rel + close_needle.len();
            let node = match tag_name {
                "code" => Inline::Code(inner.to_string()),
                "kbd" => Inline::Kbd(inner.trim().to_string()),
                "b" | "strong" => Inline::Strong(parse_inlines_depth(inner, ctx, depth + 1)),
                "i" | "em" => Inline::Emphasis(parse_inlines_depth(inner, ctx, depth + 1)),
                "u" | "ins" => Inline::Underline(parse_inlines_depth(inner, ctx, depth + 1)),
                "s" | "del" | "strike" => {
                    Inline::Strikethrough(parse_inlines_depth(inner, ctx, depth + 1))
                }
                "mark" => Inline::Highlight(parse_inlines_depth(inner, ctx, depth + 1)),
                "sup" => Inline::Superscript(parse_inlines_depth(inner, ctx, depth + 1)),
                "sub" => Inline::Subscript(parse_inlines_depth(inner, ctx, depth + 1)),
                "a" => {
                    let href = extract_html_attr(tag_trim, "href").unwrap_or_default();
                    let title = extract_html_attr(tag_trim, "title");
                    Inline::Link {
                        text: parse_inlines_depth(inner, ctx, depth + 1),
                        url: href,
                        title,
                    }
                }
                _ => Inline::Text(plain_text_of_inlines(&parse_inlines_depth(
                    inner,
                    ctx,
                    depth + 1,
                ))),
            };
            return Some((Some(node), end));
        }
    }

    None
}

fn parse_html_img_tag(tag: &str) -> Option<Inline> {
    let url = extract_html_attr(tag, "src")?;
    let alt = extract_html_attr(tag, "alt").unwrap_or_default();
    let title = extract_html_attr(tag, "title");
    let width = extract_html_attr(tag, "width").and_then(|w| {
        w.trim_end_matches("px")
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|v| *v > 0.0)
    });
    Some(Inline::Image {
        alt,
        url,
        title,
        width,
    })
}

fn extract_html_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let pos = lower.find(&needle)?;
    let rest = tag[pos + needle.len()..].trim_start();
    if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else if let Some(inner) = rest.strip_prefix('\'') {
        let end = inner.find('\'')?;
        Some(inner[..end].to_string())
    } else {
        let end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
        Some(rest[..end].to_string())
    }
}

fn decode_entity(input: &str, amp: usize) -> Option<(String, usize)> {
    let rest = &input[amp + 1..];
    let semi = rest.find(';')?;
    if semi == 0 || semi > 16 {
        return None;
    }
    let name = &rest[..semi];
    let decoded = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())?
    } else if let Some(dec) = name.strip_prefix('#') {
        dec.parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())?
    } else {
        match name {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => "\u{00A0}",
            "mdash" => "—",
            "ndash" => "–",
            "hellip" => "…",
            "copy" => "©",
            "reg" => "®",
            "trade" => "™",
            "rarr" => "→",
            "larr" => "←",
            "uarr" => "↑",
            "darr" => "↓",
            "harr" => "↔",
            "times" => "×",
            "divide" => "÷",
            "plusmn" => "±",
            "ne" => "≠",
            "le" => "≤",
            "ge" => "≥",
            "middot" => "·",
            "bull" => "•",
            _ => return None,
        }
        .to_string()
    };
    Some((decoded, amp + 1 + semi + 1))
}

fn parse_emoji_shortcode(input: &str, colon: usize) -> Option<(&'static str, usize)> {
    let rest = &input[colon + 1..];
    let end = rest.find(':')?;
    if end == 0 || end > 28 {
        return None;
    }
    let name = &rest[..end];
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'+')
    {
        return None;
    }
    let emoji = match name {
        "rocket" => "🚀",
        "sparkles" => "✨",
        "fire" => "🔥",
        "star" => "⭐",
        "heart" => "❤️",
        "thumbsup" | "+1" => "👍",
        "thumbsdown" | "-1" => "👎",
        "tada" => "🎉",
        "check" | "heavy_check_mark" => "✔️",
        "white_check_mark" => "✅",
        "x" => "❌",
        "warning" => "⚠️",
        "bulb" => "💡",
        "memo" => "📝",
        "bug" => "🐛",
        "book" => "📖",
        "bookmark" => "🔖",
        "link" => "🔗",
        "lock" => "🔒",
        "key" => "🔑",
        "hammer" => "🔨",
        "wrench" => "🔧",
        "gear" => "⚙️",
        "package" => "📦",
        "zap" => "⚡",
        "eyes" => "👀",
        "100" => "💯",
        "pin" | "pushpin" => "📌",
        "info" | "information_source" => "ℹ️",
        "question" => "❓",
        "exclamation" => "❗",
        "hourglass" => "⏳",
        "construction" => "🚧",
        "hammer_and_wrench" => "🛠️",
        _ => return None,
    };
    Some((emoji, colon + 1 + end + 1))
}

fn is_url_boundary(input: &str, idx: usize) -> bool {
    idx == 0
        || input[..idx]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"'))
}

fn scan_bare_url(input: &str, start: usize) -> Option<(String, usize)> {
    let slice = &input[start..];
    if !(slice.starts_with("https://") || slice.starts_with("http://") || slice.starts_with("www."))
    {
        return None;
    }
    let mut end = slice
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
        .unwrap_or(slice.len());
    let mut url = &slice[..end];
    while url.ends_with(['.', ',', ';', ':', '!', '?']) {
        url = &url[..url.len() - 1];
        end -= 1;
    }
    if url.ends_with(')') && url.matches('(').count() < url.matches(')').count() {
        url = &url[..url.len() - 1];
        end -= 1;
    }
    if url.len() < 7 || !url.contains('.') {
        return None;
    }
    Some((url.to_string(), start + end))
}

pub fn plain_text_of_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for item in inlines {
        match item {
            Inline::Text(t) | Inline::Code(t) | Inline::Math(t) | Inline::Kbd(t) => {
                out.push_str(t)
            }
            Inline::SoftBreak | Inline::HardBreak => out.push(' '),
            Inline::Strong(c)
            | Inline::Emphasis(c)
            | Inline::Strikethrough(c)
            | Inline::Highlight(c)
            | Inline::Underline(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c)
            | Inline::Link { text: c, .. } => out.push_str(&plain_text_of_inlines(c)),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::FootnoteRef { number, .. } => out.push_str(&format!("[{number}]")),
        }
    }
    out
}

pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.trim().chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                slug.push(lc);
            }
            prev_dash = false;
        } else if (ch == '-' || ch == '_' || ch.is_whitespace()) && !prev_dash && !slug.is_empty()
        {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

/// Convert common LaTeX inline symbols into readable Unicode math text.
pub fn format_latex_unicode(tex: &str) -> String {
    let mut s = tex.trim().to_string();
    let replacements = [
        ("\\alpha", "α"),
        ("\\beta", "β"),
        ("\\gamma", "γ"),
        ("\\delta", "δ"),
        ("\\epsilon", "ε"),
        ("\\varepsilon", "ε"),
        ("\\zeta", "ζ"),
        ("\\eta", "η"),
        ("\\theta", "θ"),
        ("\\lambda", "λ"),
        ("\\mu", "μ"),
        ("\\pi", "π"),
        ("\\rho", "ρ"),
        ("\\sigma", "σ"),
        ("\\tau", "τ"),
        ("\\phi", "φ"),
        ("\\varphi", "φ"),
        ("\\omega", "ω"),
        ("\\Gamma", "Γ"),
        ("\\Delta", "Δ"),
        ("\\Theta", "Θ"),
        ("\\Lambda", "Λ"),
        ("\\Sigma", "Σ"),
        ("\\Phi", "Φ"),
        ("\\Omega", "Ω"),
        ("\\infty", "∞"),
        ("\\sum", "∑"),
        ("\\prod", "∏"),
        ("\\int", "∫"),
        ("\\partial", "∂"),
        ("\\nabla", "∇"),
        ("\\sqrt", "√"),
        ("\\pm", "±"),
        ("\\mp", "∓"),
        ("\\times", "×"),
        ("\\cdot", "·"),
        ("\\div", "÷"),
        ("\\neq", "≠"),
        ("\\ne", "≠"),
        ("\\leq", "≤"),
        ("\\le", "≤"),
        ("\\geq", "≥"),
        ("\\ge", "≥"),
        ("\\approx", "≈"),
        ("\\equiv", "≡"),
        ("\\sim", "∼"),
        ("\\in", "∈"),
        ("\\notin", "∉"),
        ("\\subset", "⊂"),
        ("\\subseteq", "⊆"),
        ("\\cup", "∪"),
        ("\\cap", "∩"),
        ("\\emptyset", "∅"),
        ("\\forall", "∀"),
        ("\\exists", "∃"),
        ("\\rightarrow", "→"),
        ("\\to", "→"),
        ("\\leftarrow", "←"),
        ("\\Rightarrow", "⇒"),
        ("\\Leftarrow", "⇐"),
        ("\\leftrightarrow", "↔"),
        ("\\Leftrightarrow", "⇔"),
        ("\\dots", "…"),
        ("\\cdots", "⋯"),
        ("\\ldots", "…"),
    ];
    for (pat, rep) in replacements {
        s = s.replace(pat, rep);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_headings_and_slugs() {
        let md = "---\ntitle: Kestrel Guide\nauthor: Rust\n---\n\n# Intro\n\n## Intro\n\nSubheading\n---\n";
        let doc = parse(md);
        let fm = doc.front_matter.expect("front matter");
        assert_eq!(fm.entries.len(), 2);
        let headings: Vec<(u8, &str)> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { level, slug, .. } => Some((*level, slug.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(headings, [(1, "intro"), (2, "intro-1"), (2, "subheading")]);
    }

    #[test]
    fn parses_tables_callouts_tasks_footnotes_and_nested_lists() {
        let md = "\
> [!WARNING] 注意事项
> 嵌套**加粗**与`行内代码`

- [ ] 待办任务
  - 子项 *斜体*
- [x] 已完成任务

| 列A | 居中 | 右对齐 |
| :--- | :---: | ---: |
| `code` | ==高亮== | ~~删除~~ |

参考链接 [Kestrel][k] 与脚注[^1]。

[k]: https://example.com \"示例\"
[^1]: 这是脚注说明。
";
        let doc = parse(md);
        assert!(matches!(
            &doc.blocks[0],
            Block::Blockquote {
                callout: Some((CalloutKind::Warning, Some(t))),
                ..
            } if t == "注意事项"
        ));
        if let Block::List { items, .. } = &doc.blocks[1] {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].checked, Some(false));
            assert!(items[0].checkbox_byte.is_some());
            assert_eq!(items[1].checked, Some(true));
        } else {
            panic!("expected list");
        }
        if let Block::Table {
            alignments,
            headers,
            rows,
            ..
        } = &doc.blocks[2]
        {
            assert_eq!(alignments, &[Align::Left, Align::Center, Align::Right]);
            assert_eq!(headers.len(), 3);
            assert_eq!(rows.len(), 1);
        } else {
            panic!("expected table");
        }
        assert_eq!(doc.footnotes.len(), 1);
        assert_eq!(doc.footnotes[0].number, 1);
    }

    #[test]
    fn parses_math_details_and_definition_lists() {
        let md = "\
$$
\\sum_{i=1}^n i = \\frac{n(n+1)}{2}
$$

Term
: Definition with $x^2 + y^2$ and <kbd>Ctrl</kbd>

<details open>
<summary>展开查看</summary>

1. 第一项
2. 第二项

</details>
";
        let doc = parse(md);
        assert!(matches!(&doc.blocks[0], Block::MathBlock { .. }));
        assert!(matches!(&doc.blocks[1], Block::DefinitionList { .. }));
        assert!(matches!(
            &doc.blocks[2],
            Block::Details {
                open_by_default: true,
                ..
            }
        ));
    }

    #[test]
    fn intraword_underscores_and_escapes_stay_literal() {
        let md = "snake_case_identifier and \\*escaped\\* and $100 or $200";
        let doc = parse(md);
        if let Block::Paragraph { inlines, .. } = &doc.blocks[0] {
            let text = plain_text_of_inlines(inlines);
            assert_eq!(text, "snake_case_identifier and *escaped* and $100 or $200");
        } else {
            panic!("expected paragraph");
        }
    }
}
