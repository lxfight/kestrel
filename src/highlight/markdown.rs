//! Hand rolled markdown highlighter.
//!
//! Markdown is not parsed with tree-sitter here: a single left-to-right pass over
//! lines covers the block constructs, and each non-block line is scanned once for
//! inline constructs. The engine is stateless, so edits and parses are no-ops.

use crate::highlight::{Span, SyntaxEdit, TokenKind, normalize};

pub struct MarkdownEngine;

impl MarkdownEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn apply_edit(&mut self, _edit: SyntaxEdit) {}

    pub fn parse(&mut self, _text: &str) {}

    pub fn highlight(&mut self, text: &str) -> Vec<Span> {
        scan(text)
    }
}

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    len: usize,
}

fn scan(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut fence: Option<Fence> = None;
    // Text span of the previous line when it can still become a setext heading.
    let mut prev_paragraph: Option<(usize, usize)> = None;
    let mut in_html_comment = false;
    let mut line_start = 0;
    for raw in text.split('\n') {
        let raw_len = raw.len();
        let line_end = line_start + raw_len - usize::from(raw.ends_with('\r'));
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let content = line[indent..].trim_end_matches([' ', '\t']);
        let content_end = indent + content.len();

        if in_html_comment {
            scan_inline(line, line_start, &mut spans, &mut in_html_comment);
            prev_paragraph = None;
        } else if let Some(open) = fence {
            push(&mut spans, line_start, line_end, TokenKind::String);
            let closes = content.len() >= open.len && content.bytes().all(|b| b == open.marker);
            if closes {
                fence = None;
            }
            prev_paragraph = None;
        } else if content.is_empty() {
            prev_paragraph = None;
        } else if indent <= 3 && is_fence(content) {
            let marker = content.as_bytes()[0];
            let len = content.bytes().take_while(|b| *b == marker).count();
            fence = Some(Fence { marker, len });
            push(&mut spans, line_start, line_end, TokenKind::String);
            prev_paragraph = None;
        } else if indent <= 3 && atx_heading(content) {
            push(
                &mut spans,
                line_start + indent,
                line_start + content_end,
                TokenKind::Heading,
            );
            prev_paragraph = None;
        } else if let Some((prev_start, prev_end)) =
            prev_paragraph.filter(|_| is_setext_underline(content))
        {
            // The paragraph line was already scanned for inlines; only the
            // heading covering both lines may paint it.
            spans.retain(|span| span.start < prev_start);
            push(&mut spans, prev_start, prev_end, TokenKind::Heading);
            push(
                &mut spans,
                line_start + indent,
                line_start + content_end,
                TokenKind::Heading,
            );
            prev_paragraph = None;
        } else if is_hr(content) {
            push(
                &mut spans,
                line_start + indent,
                line_start + content_end,
                TokenKind::Punctuation,
            );
            prev_paragraph = None;
        } else if is_blockquote(content) {
            push(
                &mut spans,
                line_start + indent,
                line_start + indent + 1,
                TokenKind::Comment,
            );
            scan_inline(
                &line[indent + 1..],
                line_start + indent + 1,
                &mut spans,
                &mut in_html_comment,
            );
            prev_paragraph = None;
        } else if let Some(len) = list_marker_len(content) {
            push(
                &mut spans,
                line_start + indent,
                line_start + indent + len,
                TokenKind::Keyword,
            );
            scan_inline(
                &content[len..],
                line_start + indent + len,
                &mut spans,
                &mut in_html_comment,
            );
            prev_paragraph = None;
        } else {
            scan_inline(
                content,
                line_start + indent,
                &mut spans,
                &mut in_html_comment,
            );
            let is_html_block = content.starts_with("<!--");
            prev_paragraph =
                (!is_html_block).then_some((line_start + indent, line_start + content_end));
        }
        line_start += raw_len + 1;
    }
    normalize(spans)
}

fn scan_inline(line: &str, base: usize, spans: &mut Vec<Span>, in_comment: &mut bool) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if *in_comment {
            if let Some(rel) = line[i..].find("-->") {
                let end = i + rel + 3;
                push(spans, base + i, base + end, TokenKind::Comment);
                *in_comment = false;
                i = end;
            } else {
                push(spans, base + i, base + line.len(), TokenKind::Comment);
                return;
            }
            continue;
        }
        match bytes[i] {
            b'`' => {
                let run = count_run(bytes, i, b'`');
                match find_exact_run(bytes, i + run, b'`', run) {
                    Some(close) => {
                        let end = close + run;
                        push(spans, base + i, base + end, TokenKind::String);
                        i = end;
                    }
                    None => i += run,
                }
            }
            b'*' | b'_' => {
                let ch = bytes[i];
                if ch == b'_' && i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
                    i += 1;
                    continue;
                }
                let run = count_run(bytes, i, ch).min(2);
                match find_exact_run(bytes, i + run, ch, run) {
                    Some(close) if close > i + run => {
                        let end = close + run;
                        push(spans, base + i, base + end, TokenKind::Attribute);
                        i = end;
                    }
                    _ => i += run,
                }
            }
            b'[' | b'!' => {
                let bracket = if bytes[i] == b'!' {
                    if bytes.get(i + 1) != Some(&b'[') {
                        i += 1;
                        continue;
                    }
                    i + 1
                } else {
                    i
                };
                match link_at(line, bracket, base, spans) {
                    Some(end) => i = end,
                    None => i += 1,
                }
            }
            b'<' => {
                if line[i..].starts_with("<!--") {
                    match line[i + 4..].find("-->") {
                        Some(rel) => {
                            let end = i + 4 + rel + 3;
                            push(spans, base + i, base + end, TokenKind::Comment);
                            i = end;
                        }
                        None => {
                            push(spans, base + i, base + line.len(), TokenKind::Comment);
                            *in_comment = true;
                            return;
                        }
                    }
                } else if let Some(rel) = line[i + 1..].find('>') {
                    let inner = &line[i + 1..i + 1 + rel];
                    let end = i + 1 + rel + 1;
                    if is_autolink_target(inner) {
                        push(spans, base + i + 1, base + i + 1 + rel, TokenKind::Link);
                    } else if is_tag_start(inner) {
                        push(spans, base + i, base + end, TokenKind::Tag);
                    }
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Parse `[text](url)` / `[text][ref]` starting at the opening bracket.
/// Returns the byte index just past the construct on success.
fn link_at(line: &str, bracket: usize, base: usize, spans: &mut Vec<Span>) -> Option<usize> {
    let bytes = line.as_bytes();
    let close = bracket + 1 + line[bracket + 1..].find(']')?;
    let text = (bracket + 1, close);
    match bytes.get(close + 1) {
        Some(b'(') => {
            let url_start = close + 2;
            let url_close = url_start + line[url_start..].find(')')?;
            let raw = &line[url_start..url_close];
            let lead = raw.len() - raw.trim_start().len();
            let trail = raw.len() - raw.trim_end().len();
            push(spans, base + text.0, base + text.1, TokenKind::Function);
            push(
                spans,
                base + url_start + lead,
                base + url_close - trail,
                TokenKind::Link,
            );
            Some(url_close + 1)
        }
        Some(b'[') => {
            let ref_close = close + 2 + line[close + 2..].find(']')?;
            push(spans, base + text.0, base + text.1, TokenKind::Function);
            push(spans, base + close + 2, base + ref_close, TokenKind::Link);
            Some(ref_close + 1)
        }
        _ => None,
    }
}

fn push(spans: &mut Vec<Span>, start: usize, end: usize, kind: TokenKind) {
    if end > start {
        spans.push(Span { start, end, kind });
    }
}

fn count_run(bytes: &[u8], from: usize, ch: u8) -> usize {
    bytes[from..].iter().take_while(|b| **b == ch).count()
}

/// Find a run of exactly `len` `ch` bytes at or after `from`.
fn find_exact_run(bytes: &[u8], mut from: usize, ch: u8, len: usize) -> Option<usize> {
    while from < bytes.len() {
        if bytes[from] != ch {
            from += 1;
            continue;
        }
        let start = from;
        while from < bytes.len() && bytes[from] == ch {
            from += 1;
        }
        if from - start == len {
            return Some(start);
        }
    }
    None
}

fn is_fence(content: &str) -> bool {
    let b = content.as_bytes();
    matches!(b.first(), Some(b'`' | b'~')) && b.iter().take_while(|c| **c == b[0]).count() >= 3
}

fn atx_heading(content: &str) -> bool {
    let b = content.as_bytes();
    let hashes = b.iter().take_while(|c| **c == b'#').count();
    hashes > 0 && hashes <= 6 && (hashes == b.len() || matches!(b[hashes], b' ' | b'\t'))
}

fn is_setext_underline(content: &str) -> bool {
    let b = content.as_bytes();
    let marker = b[0];
    matches!(marker, b'=' | b'-') && b.iter().all(|c| *c == marker)
}

fn is_hr(content: &str) -> bool {
    let b = content.as_bytes();
    let marker = b[0];
    let mut count = 0;
    for byte in b {
        match byte {
            b' ' | b'\t' => {}
            c if *c == marker && matches!(c, b'-' | b'*' | b'_') => count += 1,
            _ => return false,
        }
    }
    count >= 3
}

fn is_blockquote(content: &str) -> bool {
    content.starts_with('>')
}

fn list_marker_len(content: &str) -> Option<usize> {
    let b = content.as_bytes();
    match b.first()? {
        b'-' | b'*' | b'+' if b.len() == 1 || matches!(b[1], b' ' | b'\t') => Some(1),
        b'0'..=b'9' => {
            let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits <= 9
                && matches!(b.get(digits), Some(&b'.') | Some(&b')'))
                && matches!(b.get(digits + 1), None | Some(&b' ') | Some(&b'\t'))
            {
                Some(digits + 1)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_autolink_target(inner: &str) -> bool {
    let lower = inner.get(..10).unwrap_or(inner).to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("mailto:")
}

fn is_tag_start(inner: &str) -> bool {
    match inner.as_bytes().first() {
        Some(b'/' | b'!' | b'?') => true,
        Some(c) => c.is_ascii_alphabetic(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Vec<Span> {
        MarkdownEngine::new().highlight(text)
    }

    fn slice<'a>(text: &'a str, span: &Span) -> &'a str {
        &text[span.start..span.end]
    }

    fn find<'a>(text: &'a str, spans: &[Span], kind: TokenKind) -> Vec<&'a str> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .map(|span| slice(text, span))
            .collect()
    }

    #[test]
    fn headings() {
        let text = "# Title\n\n## Sub\n\nBody\n===\n";
        let spans = scan(text);
        let headings = find(text, &spans, TokenKind::Heading);
        assert!(headings.contains(&"# Title"), "{headings:?}");
        assert!(headings.contains(&"## Sub"), "{headings:?}");
        assert!(headings.contains(&"Body"), "{headings:?}");
        assert!(headings.contains(&"==="), "{headings:?}");
    }

    #[test]
    fn fenced_code_is_string() {
        let text = "```rust\nfn x() {}\n```\n";
        let spans = scan(text);
        let strings = find(text, &spans, TokenKind::String);
        assert!(strings.contains(&"```rust"), "{strings:?}");
        assert!(
            strings.iter().any(|s| s.contains("fn x() {}")),
            "{strings:?}"
        );
        assert!(strings.contains(&"```"), "{strings:?}");
    }

    #[test]
    fn links_style_text_and_url() {
        let text = "See [Kestrel](https://example.com) and <https://kestrel.dev>.";
        let spans = scan(text);
        assert!(find(text, &spans, TokenKind::Function).contains(&"Kestrel"));
        let links = find(text, &spans, TokenKind::Link);
        assert!(links.contains(&"https://example.com"), "{links:?}");
        assert!(links.contains(&"https://kestrel.dev"), "{links:?}");
    }

    #[test]
    fn inline_and_block_constructs() {
        let text = "\
> quoted
- item
1. numbered
inline `code` and **bold** and *italic*
<div class=\"a\">html</div>
<!-- a comment -->
---
";
        let spans = scan(text);
        assert!(
            find(text, &spans, TokenKind::Comment)
                .iter()
                .any(|s| *s == ">")
        );
        assert!(find(text, &spans, TokenKind::Keyword).contains(&"-"));
        assert!(find(text, &spans, TokenKind::Keyword).contains(&"1."));
        let strings = find(text, &spans, TokenKind::String);
        assert!(strings.contains(&"`code`"), "{strings:?}");
        let emphasis = find(text, &spans, TokenKind::Attribute);
        assert!(emphasis.contains(&"**bold**"), "{emphasis:?}");
        assert!(emphasis.contains(&"*italic*"), "{emphasis:?}");
        assert!(
            find(text, &spans, TokenKind::Tag)
                .iter()
                .any(|s| s.starts_with("<div"))
        );
        assert!(
            find(text, &spans, TokenKind::Comment)
                .iter()
                .any(|s| s.starts_with("<!--"))
        );
        assert!(find(text, &spans, TokenKind::Punctuation).contains(&"---"));
    }

    #[test]
    fn multiline_html_comment() {
        let text = "a <!-- start\nstill comment\nend --> b\n";
        let spans = scan(text);
        let comments: Vec<&str> = find(text, &spans, TokenKind::Comment);
        assert_eq!(comments.len(), 3, "{comments:?}");
        assert!(comments[0].ends_with("start"));
        assert_eq!(comments[1], "still comment");
        assert!(comments[2].starts_with("end -->"));
    }

    #[test]
    fn spans_are_sorted_and_disjoint() {
        let text = "\
# Title

```rust
fn x() {}
```

> quote **bold** [link](https://e.com)
- item `code`

   
---
";
        let spans = scan(text);
        for window in spans.windows(2) {
            let (a, b) = (window[0], window[1]);
            assert!(a.start < a.end, "empty span {a:?}");
            assert!(a.end <= b.start, "overlap {a:?} / {b:?}");
        }
    }

    #[test]
    fn whitespace_only_lines_and_crlf_do_not_panic() {
        let text = "# a\r\n   \r\n\t\r\n```\r\ncode\r\n```\r\n";
        let spans = scan(text);
        assert!(!spans.is_empty());
        for span in &spans {
            assert!(
                text.get(span.start..span.end).is_some(),
                "{span:?} not on char boundary"
            );
        }
    }
}
