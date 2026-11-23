//! Syntax highlighting: tree-sitter grammars for code, a small scanner for markdown.
//!
//! The editor owns one [`HighlightState`] per open document. It is fed every text
//! edit (as a tree-sitter [`InputEdit`] equivalent), then debounced calls to
//! [`HighlightState::parse`] keep the tree fresh incrementally. [`HighlightState::spans`]
//! returns non-overlapping byte ranges styled with a [`TokenKind`].

use std::path::Path;

pub mod markdown;
pub mod tree;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Json,
    Toml,
    C,
    Cpp,
    Go,
    Java,
    CSharp,
    Html,
    Css,
    Bash,
    Yaml,
    Markdown,
    #[default]
    Plain,
}

impl Language {
    /// Detect from a file path's extension or well-known file name.
    pub fn from_path(path: &Path) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match (name.as_str(), ext.as_str()) {
            ("cargo.toml" | "cargo.lock" | "pyproject.toml" | "rustfmt.toml", _) => Self::Toml,
            (_, "rs") => Self::Rust,
            (_, "py" | "pyi" | "pyw") => Self::Python,
            (_, "js" | "mjs" | "cjs" | "jsx") => Self::JavaScript,
            (_, "ts" | "mts" | "cts") => Self::TypeScript,
            (_, "tsx") => Self::Tsx,
            (_, "json" | "jsonc" | "json5" | "webmanifest" | "ipynb") => Self::Json,
            (_, "toml") => Self::Toml,
            (_, "c" | "h") => Self::C,
            (_, "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "c++" | "ino") => Self::Cpp,
            (_, "go") => Self::Go,
            (_, "java") => Self::Java,
            (_, "cs") => Self::CSharp,
            (_, "html" | "htm" | "xhtml" | "vue" | "svelte") => Self::Html,
            (_, "css" | "scss" | "less" | "sass") => Self::Css,
            (_, "sh" | "bash" | "zsh" | "fish" | "bashrc" | "zshrc" | "bash_profile") => Self::Bash,
            (_, "yml" | "yaml") => Self::Yaml,
            (_, "md" | "markdown" | "mdx" | "mdown" | "mkd") => Self::Markdown,
            _ => Self::Plain,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TypeScript React",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::CSharp => "C#",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Bash => "Shell Script",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
            Self::Plain => "Plain Text",
        }
    }
}

/// Semantic category of a highlighted span.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    Variable,
    Constant,
    Property,
    Operator,
    Punctuation,
    Macro,
    Attribute,
    Heading,
    Link,
    Tag,
}

/// A styled byte range in the document text.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// Sort spans by start and drop overlaps, keeping the earliest starting span
/// (at equal starts the longest one). Engines return spans in this shape.
pub(crate) fn normalize(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_unstable_by(|a, b| a.start.cmp(&b.start).then_with(|| b.end.cmp(&a.end)));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.end <= span.start {
            continue;
        }
        if out.last().is_none_or(|last| span.start >= last.end) {
            out.push(span);
        }
    }
    out
}

/// One edit, expressed the way tree-sitter's `InputEdit` needs it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyntaxEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_row: usize,
    pub start_column: usize,
    pub old_end_row: usize,
    pub old_end_column: usize,
    pub new_end_row: usize,
    pub new_end_column: usize,
}

/// Per-document highlighting state.
pub struct HighlightState {
    engine: Engine,
    spans: Vec<Span>,
    /// Version `spans` was computed for; `u64::MAX` until the first pass.
    spans_version: u64,
    parsed_version: u64,
    dirty: bool,
}

enum Engine {
    Tree(self::tree::TreeEngine),
    Markdown(self::markdown::MarkdownEngine),
    None,
}

impl HighlightState {
    pub fn new(language: Language) -> Self {
        let engine = match language {
            Language::Markdown => Engine::Markdown(self::markdown::MarkdownEngine::new()),
            Language::Plain => Engine::None,
            _ => Engine::Tree(self::tree::TreeEngine::new(language)),
        };
        Self {
            engine,
            spans: Vec::new(),
            spans_version: u64::MAX,
            parsed_version: u64::MAX,
            dirty: false,
        }
    }

    pub fn apply_edit(&mut self, edit: SyntaxEdit) {
        match &mut self.engine {
            Engine::Tree(engine) => engine.apply_edit(edit),
            Engine::Markdown(engine) => engine.apply_edit(edit),
            Engine::None => return,
        }
        self.dirty = true;
    }

    /// Re-parse incrementally. Safe to call repeatedly; returns false if nothing changed.
    pub fn parse(&mut self, text: &str, version: u64) -> bool {
        if !self.dirty && version == self.parsed_version {
            return false;
        }
        match &mut self.engine {
            Engine::Tree(engine) => engine.parse(text),
            Engine::Markdown(engine) => engine.parse(text),
            Engine::None => {}
        }
        self.parsed_version = version;
        self.dirty = false;
        true
    }

    /// Styled spans for the whole document. Non-overlapping, sorted by `start`.
    pub fn spans(&mut self, text: &str, version: u64) -> &[Span] {
        if self.dirty || version != self.spans_version {
            self.parse(text, version);
            self.spans = match &mut self.engine {
                Engine::Tree(engine) => engine.highlight(text),
                Engine::Markdown(engine) => engine.highlight(text),
                Engine::None => Vec::new(),
            };
            self.spans_version = version;
        }
        &self.spans
    }

    /// Extract outline symbols, foldable line ranges, and syntax diagnostics.
    pub fn analyze(
        &self,
        text: &str,
        language: Language,
    ) -> (
        Vec<crate::lsp::OutlineSymbol>,
        Vec<(usize, usize)>,
        Vec<crate::lsp::Diagnostic>,
    ) {
        match &self.engine {
            Engine::Tree(engine) => match engine.tree() {
                Some(tree) => crate::lsp::analyze_tree(tree, text, language),
                None => (Vec::new(), Vec::new(), Vec::new()),
            },
            Engine::Markdown(_) => {
                let (symbols, folds) = crate::lsp::analyze_markdown(text);
                (symbols, folds, Vec::new())
            }
            Engine::None => (Vec::new(), Vec::new(), Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One representative snippet per tree-sitter language, chosen to exercise
    /// at least one capture of the grammar's published highlights query.
    const SAMPLES: &[(Language, &str)] = &[
        (Language::Rust, "fn main() { let s = \"hi\"; }\n"),
        (Language::Python, "def f(x):\n    return x\n"),
        (Language::JavaScript, "function f(x) { return \"s\"; }\n"),
        (Language::TypeScript, "interface Foo { x: number }\n"),
        (
            Language::Tsx,
            "interface P { x: number }\nconst f = (p: P) => <div>{p.x}</div>;\n",
        ),
        (Language::Json, "{\"a\": 1}\n"),
        (Language::Toml, "title = \"kestrel\"\n"),
        (Language::C, "int f(int x) { return x; }\n"),
        (Language::Cpp, "class A { public: int f(); };\n"),
        (
            Language::Go,
            "package main\nfunc f(x int) int { return x }\n",
        ),
        (Language::Java, "class A { int f(int x) { return x; } }\n"),
        (Language::CSharp, "class A { int F(int x) { return x; } }\n"),
        (Language::Html, "<div class=\"a\">hi</div>\n"),
        (Language::Css, "a { color: red; }\n"),
        (Language::Bash, "echo \"hi\"\n"),
        (Language::Yaml, "key: value\n"),
    ];

    fn spans_for(language: Language, text: &str) -> Vec<Span> {
        let mut state = HighlightState::new(language);
        state.spans(text, 1).to_vec()
    }

    fn slice<'a>(text: &'a str, span: &Span) -> &'a str {
        &text[span.start..span.end]
    }

    fn assert_disjoint(language: Language, text: &str, spans: &[Span]) {
        for window in spans.windows(2) {
            let (a, b) = (window[0], window[1]);
            assert!(a.start < a.end, "{language:?}: empty span {a:?}");
            assert!(a.end <= b.start, "{language:?}: overlap {a:?} / {b:?}");
        }
        if let Some(last) = spans.last() {
            assert!(
                last.end <= text.len(),
                "{language:?}: span past EOF {last:?}"
            );
        }
    }

    #[test]
    fn language_detection() {
        assert_eq!(
            Language::from_path(Path::new("src/main.rs")),
            Language::Rust
        );
        assert_eq!(Language::from_path(Path::new("tool.py")), Language::Python);
        assert_eq!(Language::from_path(Path::new("App.tsx")), Language::Tsx);
        assert_eq!(
            Language::from_path(Path::new("README.md")),
            Language::Markdown
        );
        assert_eq!(Language::from_path(Path::new("Cargo.toml")), Language::Toml);
        assert_eq!(Language::from_path(Path::new("notes.xyz")), Language::Plain);
        assert_eq!(Language::from_path(Path::new("Makefile")), Language::Plain);
    }

    #[test]
    fn rust_keyword_string_comment() {
        let text = "fn main() { let s = \"hi\"; // note\n}";
        let spans = spans_for(Language::Rust, text);
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::Keyword && slice(text, s) == "fn")
        );
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::String && slice(text, s).contains("hi"))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::Comment && slice(text, s).contains("note"))
        );
    }

    #[test]
    fn every_grammar_produces_spans() {
        let mut dead = Vec::new();
        for (language, text) in SAMPLES {
            if spans_for(*language, text).is_empty() {
                dead.push(language.name());
            }
        }
        assert!(dead.is_empty(), "no highlighting for: {dead:?}");

        let mut state = HighlightState::new(Language::Plain);
        assert!(state.spans("fn main() {}", 1).is_empty());
    }

    #[test]
    fn spans_are_sorted_and_disjoint() {
        for (language, text) in SAMPLES {
            let spans = spans_for(*language, text);
            assert_disjoint(*language, text, &spans);
        }
        let markdown = "# Title\n\n```rust\nfn x() {}\n```\n\n[K](https://e.com)\n";
        let spans = spans_for(Language::Markdown, markdown);
        assert_disjoint(Language::Markdown, markdown, &spans);
    }

    #[test]
    fn incremental_edit_keeps_spans_sane() {
        let before = "fn main() { let s = \"hi\"; }";
        let mut state = HighlightState::new(Language::Rust);
        assert!(!state.spans(before, 1).is_empty());

        let start = before.find("hi").unwrap();
        let after = before.replace("hi", "hello");
        let after = after.as_str();
        state.apply_edit(SyntaxEdit {
            start_byte: start,
            old_end_byte: start + 2,
            new_end_byte: start + 5,
            start_row: 0,
            start_column: start,
            old_end_row: 0,
            old_end_column: start + 2,
            new_end_row: 0,
            new_end_column: start + 5,
        });
        let spans = state.spans(after, 2).to_vec();
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::String && slice(after, s).contains("hello")),
            "spans after edit: {spans:?}"
        );
        assert_disjoint(Language::Rust, after, &spans);
    }

    #[test]
    fn multiline_insert_keeps_spans_sane() {
        let before = "fn main() {\n}\n";
        let mut state = HighlightState::new(Language::Rust);
        state.spans(before, 1);

        let insert = "    let s = \"hi\";\n";
        let after = "fn main() {\n    let s = \"hi\";\n}\n";
        let start = before.find('}').unwrap();
        state.apply_edit(SyntaxEdit {
            start_byte: start,
            old_end_byte: start,
            new_end_byte: start + insert.len(),
            start_row: 1,
            start_column: 0,
            old_end_row: 1,
            old_end_column: 0,
            new_end_row: 2,
            new_end_column: 0,
        });
        let spans = state.spans(after, 2).to_vec();
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::String && slice(after, s).contains("hi")),
            "spans after edit: {spans:?}"
        );
        assert_disjoint(Language::Rust, after, &spans);
    }

    #[test]
    fn typescript_and_cpp_layer_sibling_queries() {
        let ts = "const s: string = \"hi\"; // note\n";
        let spans = spans_for(Language::TypeScript, ts);
        for kind in [TokenKind::Keyword, TokenKind::String, TokenKind::Comment] {
            assert!(
                spans.iter().any(|span| span.kind == kind),
                "missing {kind:?} in {spans:?}"
            );
        }

        let cpp = "int f() { /* note */ return \"hi\"; }\n";
        let spans = spans_for(Language::Cpp, cpp);
        for kind in [TokenKind::Keyword, TokenKind::String, TokenKind::Comment] {
            assert!(
                spans.iter().any(|span| span.kind == kind),
                "missing {kind:?} in {spans:?}"
            );
        }
    }
}
