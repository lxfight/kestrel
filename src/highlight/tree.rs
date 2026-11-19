//! tree-sitter based highlighting engine.
//!
//! One parser per document; the tree is kept across edits and every [`parse`]
//! is incremental. The grammar's published highlights query maps nodes to
//! [`TokenKind`]s; when a grammar or its query cannot be loaded the engine
//! silently produces no spans instead of failing the editor.

use std::borrow::Cow;

use tree_sitter::{
    InputEdit, Language as TreeLanguage, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

use crate::highlight::{Language, Span, SyntaxEdit, TokenKind};

/// Documents larger than this get no tree-sitter highlighting at all; the query
/// pass would take far too long and the editor caps rendering anyway.
const MAX_HIGHLIGHT_BYTES: usize = 2 * 1024 * 1024;

pub struct TreeEngine {
    parser: Parser,
    query: Option<Query>,
    /// `TokenKind` per query capture index; `None` for captures we do not paint.
    capture_kinds: Vec<Option<TokenKind>>,
    tree: Option<Tree>,
    /// The tree was edited but not yet re-parsed, so its byte offsets are stale.
    stale: bool,
}

impl TreeEngine {
    pub fn new(language: Language) -> Self {
        let mut parser = Parser::new();
        let mut query = None;
        let mut capture_kinds = Vec::new();
        if let Some((ts_language, source)) = grammar(language)
            && parser.set_language(&ts_language).is_ok()
                && let Ok(compiled) = Query::new(&ts_language, &source) {
                    capture_kinds = compiled
                        .capture_names()
                        .iter()
                        .map(|name| capture_kind(name))
                        .collect();
                    query = Some(compiled);
                }
        Self {
            parser,
            query,
            capture_kinds,
            tree: None,
            stale: false,
        }
    }

    pub fn apply_edit(&mut self, edit: SyntaxEdit) {
        let Some(tree) = self.tree.as_mut() else {
            return;
        };
        tree.edit(&InputEdit {
            start_byte: edit.start_byte,
            old_end_byte: edit.old_end_byte,
            new_end_byte: edit.new_end_byte,
            start_position: Point::new(edit.start_row, edit.start_column),
            old_end_position: Point::new(edit.old_end_row, edit.old_end_column),
            new_end_position: Point::new(edit.new_end_row, edit.new_end_column),
        });
        self.stale = true;
    }

    pub fn parse(&mut self, text: &str) {
        if self.query.is_none() || text.len() > MAX_HIGHLIGHT_BYTES {
            self.tree = None;
            self.stale = false;
            return;
        }
        self.tree = self.parser.parse(text, self.tree.as_ref());
        self.stale = false;
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    pub fn highlight(&mut self, text: &str) -> Vec<Span> {
        if self.stale || text.len() > MAX_HIGHLIGHT_BYTES {
            return Vec::new();
        }
        let (Some(query), Some(tree)) = (self.query.as_ref(), self.tree.as_ref()) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(query, tree.root_node(), text.as_bytes());
        let mut spans = Vec::new();
        while let Some((matched, capture_index)) = captures.next() {
            let capture = matched.captures()[*capture_index];
            let Some(kind) = self.capture_kinds[capture.index as usize] else {
                continue;
            };
            let node = capture.node;
            if node.start_byte() < node.end_byte() {
                spans.push(Span {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    kind,
                });
            }
        }
        resolve(spans)
    }
}

/// The grammar and highlight query for a language. TypeScript reuses the
/// JavaScript query for its JS syntax, and the published C++ query is only a
/// fragment, so both are layered on top of the sibling query.
fn grammar(language: Language) -> Option<(TreeLanguage, Cow<'static, str>)> {
    let (function, query) = match language {
        Language::Rust => (
            tree_sitter_rust::LANGUAGE,
            Cow::Borrowed(tree_sitter_rust::HIGHLIGHTS_QUERY),
        ),
        Language::Python => (
            tree_sitter_python::LANGUAGE,
            Cow::Borrowed(tree_sitter_python::HIGHLIGHTS_QUERY),
        ),
        Language::JavaScript => (
            tree_sitter_javascript::LANGUAGE,
            Cow::Borrowed(tree_sitter_javascript::HIGHLIGHT_QUERY),
        ),
        Language::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            Cow::Owned(format!(
                "{} {}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            )),
        ),
        Language::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX,
            Cow::Owned(format!(
                "{} {}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            )),
        ),
        Language::Json => (
            tree_sitter_json::LANGUAGE,
            Cow::Borrowed(tree_sitter_json::HIGHLIGHTS_QUERY),
        ),
        Language::Toml => (
            tree_sitter_toml_ng::LANGUAGE,
            Cow::Borrowed(tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
        ),
        Language::C => (
            tree_sitter_c::LANGUAGE,
            Cow::Borrowed(tree_sitter_c::HIGHLIGHT_QUERY),
        ),
        Language::Cpp => (
            tree_sitter_cpp::LANGUAGE,
            Cow::Owned(format!(
                "{} {}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            )),
        ),
        Language::Go => (
            tree_sitter_go::LANGUAGE,
            Cow::Borrowed(tree_sitter_go::HIGHLIGHTS_QUERY),
        ),
        Language::Java => (
            tree_sitter_java::LANGUAGE,
            Cow::Borrowed(tree_sitter_java::HIGHLIGHTS_QUERY),
        ),
        Language::CSharp => (
            tree_sitter_c_sharp::LANGUAGE,
            Cow::Borrowed(tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
        ),
        Language::Html => (
            tree_sitter_html::LANGUAGE,
            Cow::Borrowed(tree_sitter_html::HIGHLIGHTS_QUERY),
        ),
        Language::Css => (
            tree_sitter_css::LANGUAGE,
            Cow::Borrowed(tree_sitter_css::HIGHLIGHTS_QUERY),
        ),
        Language::Bash => (
            tree_sitter_bash::LANGUAGE,
            Cow::Borrowed(tree_sitter_bash::HIGHLIGHT_QUERY),
        ),
        Language::Yaml => (
            tree_sitter_yaml::LANGUAGE,
            Cow::Borrowed(tree_sitter_yaml::HIGHLIGHTS_QUERY),
        ),
        Language::Markdown | Language::Plain => return None,
    };
    Some((TreeLanguage::new(function), query))
}

/// Map a tree-sitter capture name to the editor's token vocabulary.
/// Names are matched by their dotted root, with a few exact overrides.
fn capture_kind(name: &str) -> Option<TokenKind> {
    match name {
        "function.macro" => return Some(TokenKind::Macro),
        "constant.numeric" => return Some(TokenKind::Number),
        _ => {}
    }
    if name.starts_with("constant.numeric.") {
        return Some(TokenKind::Number);
    }
    let root = name.split('.').next().unwrap_or(name);
    Some(match root {
        "keyword" => TokenKind::Keyword,
        "string" | "escape" | "label" => TokenKind::String,
        "comment" => TokenKind::Comment,
        "number" | "float" | "integer" => TokenKind::Number,
        "function" | "method" | "constructor" => TokenKind::Function,
        "type" | "module" | "namespace" => TokenKind::Type,
        "variable" | "parameter" => TokenKind::Variable,
        "property" | "field" => TokenKind::Property,
        "constant" | "boolean" => TokenKind::Constant,
        "operator" => TokenKind::Operator,
        "punctuation" | "delimiter" | "bracket" => TokenKind::Punctuation,
        "attribute" | "decorator" => TokenKind::Attribute,
        "tag" => TokenKind::Tag,
        "macro" => TokenKind::Macro,
        "charset" | "import" | "keyframes" | "media" | "supports" => TokenKind::Keyword,
        _ => return None,
    })
}

/// How eagerly a kind wins a same-start conflict; more specific kinds rank higher.
fn specificity(kind: TokenKind) -> u8 {
    match kind {
        TokenKind::Attribute => 9,
        TokenKind::Macro => 8,
        TokenKind::Tag => 7,
        TokenKind::Function => 6,
        TokenKind::Type => 5,
        TokenKind::Keyword => 4,
        TokenKind::String | TokenKind::Number | TokenKind::Constant => 3,
        TokenKind::Comment => 2,
        TokenKind::Property => 1,
        TokenKind::Variable | TokenKind::Operator | TokenKind::Punctuation => 0,
        TokenKind::Heading | TokenKind::Link => 0,
    }
}

/// Sort spans and drop overlaps: the earliest start wins, then the more specific
/// kind, then the longer span.
fn resolve(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_unstable_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| specificity(b.kind).cmp(&specificity(a.kind)))
            .then_with(|| b.end.cmp(&a.end))
    });
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if out.last().is_none_or(|last| span.start >= last.end) {
            out.push(span);
        }
    }
    out
}
