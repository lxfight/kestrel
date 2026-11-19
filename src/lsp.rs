//! Code intelligence: Outline symbols, folding ranges, syntax/LSP diagnostics,
//! autocompletion, hover documentation, go-to-definition, and formatting.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tree_sitter::{Node, Tree};

use crate::fuzzy;
use crate::highlight::Language;
use crate::icons::Icon;

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Interface,
    Enum,
    Module,
    Constant,
    Variable,
    Field,
    Property,
    Keyword,
    Snippet,
    Heading,
}

impl SymbolKind {
    pub fn icon(self) -> Icon {
        match self {
            Self::Function => Icon::SymbolFunction,
            Self::Method => Icon::SymbolMethod,
            Self::Struct => Icon::SymbolStruct,
            Self::Class => Icon::SymbolClass,
            Self::Interface => Icon::SymbolInterface,
            Self::Enum => Icon::SymbolEnum,
            Self::Module => Icon::SymbolModule,
            Self::Constant => Icon::SymbolConstant,
            Self::Variable => Icon::SymbolVariable,
            Self::Field => Icon::SymbolField,
            Self::Property => Icon::SymbolProperty,
            Self::Keyword => Icon::SymbolKeyword,
            Self::Snippet => Icon::SymbolSnippet,
            Self::Heading => Icon::SymbolClass,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Interface => "trait/interface",
            Self::Enum => "enum",
            Self::Module => "mod",
            Self::Constant => "const",
            Self::Variable => "var",
            Self::Field => "field",
            Self::Property => "prop",
            Self::Keyword => "keyword",
            Self::Snippet => "snippet",
            Self::Heading => "section",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutlineSymbol {
    pub name: String,
    pub detail: String,
    pub kind: SymbolKind,
    /// 0-based start line.
    pub start_line: usize,
    /// 0-based end line.
    pub end_line: usize,
    pub byte_offset: usize,
    pub depth: usize,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagSeverity {
    Error,
    Warning,
    Info,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// 0-based line number.
    pub line: usize,
    /// 0-based character column.
    pub col: usize,
    pub end_col: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub severity: DiagSeverity,
    pub message: String,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub insert_text: String,
    pub detail: String,
    pub kind: SymbolKind,
    pub score: i32,
}

/// Extract document outline symbols, foldable line ranges, and syntax diagnostics from a tree-sitter tree.
pub fn analyze_tree(
    tree: &Tree,
    text: &str,
    language: Language,
) -> (Vec<OutlineSymbol>, Vec<(usize, usize)>, Vec<Diagnostic>) {
    let mut symbols = Vec::new();
    let mut folds = Vec::new();
    let mut diagnostics = Vec::new();
    let root = tree.root_node();

    walk_analysis(
        root,
        text,
        language,
        0,
        &mut symbols,
        &mut folds,
        &mut diagnostics,
    );

    folds.sort_by_key(|(s, e)| (*s, *e));
    folds.dedup_by(|a, b| a.0 == b.0);
    (symbols, folds, diagnostics)
}

/// Extract outline symbols and folding ranges for Markdown documents.
pub fn analyze_markdown(text: &str) -> (Vec<OutlineSymbol>, Vec<(usize, usize)>) {
    let mut symbols = Vec::new();
    let mut folds = Vec::new();
    let mut byte_pos = 0usize;
    let lines: Vec<&str> = text.split('\n').collect();
    let mut heading_lines: Vec<(usize, usize)> = Vec::new();

    for (line_idx, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
            if hashes > 0 && hashes <= 6 {
                let title = trimmed[hashes..].trim();
                if !title.is_empty() {
                    let depth = hashes.saturating_sub(1);
                    symbols.push(OutlineSymbol {
                        name: title.to_string(),
                        detail: format!("H{hashes}"),
                        kind: SymbolKind::Heading,
                        start_line: line_idx,
                        end_line: line_idx,
                        byte_offset: byte_pos,
                        depth,
                    });
                    heading_lines.push((line_idx, hashes));
                }
            }
        }
        byte_pos += raw_line.len() + 1;
    }

    for (i, (start_line, level)) in heading_lines.iter().enumerate() {
        let end_line = heading_lines[i + 1..]
            .iter()
            .find(|(_, l)| *l <= *level)
            .map(|(line, _)| line.saturating_sub(1))
            .unwrap_or_else(|| lines.len().saturating_sub(1));
        if end_line > *start_line {
            folds.push((*start_line, end_line));
            if let Some(sym) = symbols.get_mut(i) {
                sym.end_line = end_line;
            }
        }
    }

    (symbols, folds)
}

fn walk_analysis(
    node: Node,
    text: &str,
    language: Language,
    depth: usize,
    symbols: &mut Vec<OutlineSymbol>,
    folds: &mut Vec<(usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let kind_str = node.kind();
    let start_pos = node.start_position();
    let end_pos = node.end_position();

    // 1. Check for syntax errors / missing tokens.
    if (node.is_error() || node.is_missing()) && diagnostics.len() < 120 {
        let snippet = text
            .get(node.start_byte()..node.end_byte().min(node.start_byte() + 32))
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        let msg = if node.is_missing() {
            format!("语法缺失: 缺少 `{kind_str}`")
        } else if snippet.is_empty() {
            "语法错误 (Syntax Error)".to_string()
        } else {
            format!("语法错误: 意外的 `{snippet}`")
        };
        diagnostics.push(Diagnostic {
            line: start_pos.row,
            col: start_pos.column,
            end_col: if end_pos.row == start_pos.row {
                end_pos.column.max(start_pos.column + 1)
            } else {
                start_pos.column + snippet.len().max(1)
            },
            start_byte: node.start_byte(),
            end_byte: node.end_byte().max(node.start_byte() + 1),
            severity: DiagSeverity::Error,
            message: msg,
            source: language.name().to_string(),
        });
    }

    // 2. Foldable multi-line constructs.
    if end_pos.row > start_pos.row + 1 && is_foldable_node(kind_str) && folds.len() < 2000 {
        folds.push((start_pos.row, end_pos.row));
    }

    // 3. Outline symbol extraction.
    let mut next_depth = depth;
    if let Some((sym_kind, detail)) = classify_symbol_node(kind_str)
        && let Some(name) = extract_symbol_name(node, text)
        && !name.is_empty()
        && symbols.len() < 800
    {
        symbols.push(OutlineSymbol {
            name,
            detail: detail.to_string(),
            kind: sym_kind,
            start_line: start_pos.row,
            end_line: end_pos.row,
            byte_offset: node.start_byte(),
            depth,
        });
        next_depth = (depth + 1).min(4);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_analysis(
            child,
            text,
            language,
            next_depth,
            symbols,
            folds,
            diagnostics,
        );
    }
}

fn is_foldable_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_definition"
            | "function_declaration"
            | "method_definition"
            | "method_declaration"
            | "struct_item"
            | "enum_item"
            | "impl_item"
            | "trait_item"
            | "mod_item"
            | "class_definition"
            | "class_declaration"
            | "interface_declaration"
            | "block"
            | "declaration_list"
            | "field_declaration_list"
            | "enum_variant_list"
            | "object"
            | "array"
            | "table"
            | "element"
            | "rule_set"
            | "block_mapping"
            | "block_sequence"
    )
}

fn classify_symbol_node(kind: &str) -> Option<(SymbolKind, &'static str)> {
    Some(match kind {
        "function_item" | "function_definition" | "function_declaration" => {
            (SymbolKind::Function, "fn")
        }
        "method_definition" | "method_declaration" => (SymbolKind::Method, "method"),
        "struct_item" | "struct_specifier" => (SymbolKind::Struct, "struct"),
        "enum_item" | "enum_specifier" | "enum_declaration" => (SymbolKind::Enum, "enum"),
        "trait_item" | "interface_declaration" | "type_alias_declaration" => {
            (SymbolKind::Interface, "interface")
        }
        "impl_item" => (SymbolKind::Class, "impl"),
        "class_definition" | "class_declaration" => (SymbolKind::Class, "class"),
        "mod_item" | "namespace_definition" | "package_clause" => (SymbolKind::Module, "module"),
        "const_item" | "static_item" => (SymbolKind::Constant, "const"),
        "table" => (SymbolKind::Module, "section"),
        "pair" => (SymbolKind::Property, "key"),
        _ => return None,
    })
}

fn extract_symbol_name(node: Node, text: &str) -> Option<String> {
    for field in ["name", "type", "key"] {
        if let Some(child) = node.child_by_field_name(field) {
            let s = text.get(child.start_byte()..child.end_byte())?.trim();
            let clean = s.trim_matches(|c| c == '"' || c == '\'');
            if !clean.is_empty() && !clean.contains('\n') {
                return Some(clean.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "type_identifier" | "field_identifier" | "property_identifier" | "bare_key"
        ) {
            let s = text.get(child.start_byte()..child.end_byte())?.trim();
            if !s.is_empty() && !s.contains('\n') {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Completion rows that fit in the popup; navigation never exceeds what is painted.
pub const COMPLETION_MAX_ITEMS: usize = 8;

/// Bytes of document text the completion matcher inspects.
const COMPLETION_SCAN_BYTES: usize = 160_000;

/// Build completion candidates for `prefix` at the current cursor position.
pub fn complete(
    prefix: &str,
    language: Language,
    symbols: &[OutlineSymbol],
    doc_text: &str,
) -> Vec<CompletionItem> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    // 1. Document Outline symbols (highest priority).
    for sym in symbols {
        if sym.name == prefix || !seen.insert(sym.name.clone()) {
            continue;
        }
        if let Some(score) = fuzzy::quick_score(prefix, &sym.name) {
            items.push(CompletionItem {
                label: sym.name.clone(),
                insert_text: sym.name.clone(),
                detail: format!("{} (行 {})", sym.detail, sym.start_line + 1),
                kind: sym.kind,
                score: score + 50,
            });
        }
    }

    // 2. Language keywords & snippets.
    for &(kw, detail, snippet) in language_keywords(language) {
        if kw == prefix || !seen.insert(kw.to_string()) {
            continue;
        }
        if let Some(score) = fuzzy::quick_score(prefix, kw) {
            items.push(CompletionItem {
                label: kw.to_string(),
                insert_text: snippet.unwrap_or(kw).to_string(),
                detail: detail.to_string(),
                kind: if snippet.is_some() {
                    SymbolKind::Snippet
                } else {
                    SymbolKind::Keyword
                },
                score: score + 35,
            });
        }
    }

    // 3. Buffer identifier words (bounded scan for speed).
    let scan_slice = if doc_text.len() > COMPLETION_SCAN_BYTES {
        &doc_text[..doc_text.floor_char_boundary(COMPLETION_SCAN_BYTES)]
    } else {
        doc_text
    };
    for word in scan_slice
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 3 && w.len() <= 48 && *w != prefix)
    {
        if word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        if !seen.insert(word.to_string()) {
            continue;
        }
        if let Some(score) = fuzzy::quick_score(prefix, word) {
            items.push(CompletionItem {
                label: word.to_string(),
                insert_text: word.to_string(),
                detail: "标识符".to_string(),
                kind: SymbolKind::Variable,
                score,
            });
            if items.len() >= 80 {
                break;
            }
        }
    }

    items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    items.truncate(COMPLETION_MAX_ITEMS);
    items
}

/// Candidates shown right after `.` or `::`: current file symbols followed by
/// the language keywords, capped to the popup row count.
pub fn member_candidates(language: Language, symbols: &[OutlineSymbol]) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::with_capacity(COMPLETION_MAX_ITEMS);
    for sym in symbols {
        if !seen.insert(sym.name.as_str()) {
            continue;
        }
        items.push(CompletionItem {
            label: sym.name.clone(),
            insert_text: sym.name.clone(),
            detail: format!("{} (行 {})", sym.detail, sym.start_line + 1),
            kind: sym.kind,
            score: 0,
        });
        if items.len() >= COMPLETION_MAX_ITEMS {
            return items;
        }
    }
    for &(kw, detail, snippet) in language_keywords(language) {
        if !seen.insert(kw) {
            continue;
        }
        items.push(CompletionItem {
            label: kw.to_string(),
            insert_text: snippet.unwrap_or(kw).to_string(),
            detail: detail.to_string(),
            kind: if snippet.is_some() {
                SymbolKind::Snippet
            } else {
                SymbolKind::Keyword
            },
            score: 0,
        });
        if items.len() >= COMPLETION_MAX_ITEMS {
            break;
        }
    }
    items
}

/// Find the definition of `symbol_name` in the current document or across workspace files.
/// Returns `(Option<PathBuf>, byte_offset, line_number)`.
pub fn find_definition(
    symbol_name: &str,
    current_path: Option<&Path>,
    current_symbols: &[OutlineSymbol],
    workspace_files: &[PathBuf],
) -> Option<(Option<PathBuf>, usize, usize)> {
    if symbol_name.trim().is_empty() {
        return None;
    }
    // 1. Check current document outline symbols first.
    if let Some(sym) = current_symbols.iter().find(|s| s.name == symbol_name) {
        return Some((
            current_path.map(|p| p.to_path_buf()),
            sym.byte_offset,
            sym.start_line + 1,
        ));
    }

    // 2. Search across workspace files for common definition patterns.
    let escaped = regex::escape(symbol_name);
    let pattern = format!(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|type|const|static|mod|def|class|interface|function|var|let)\s+{escaped}\b"
    );
    let re = regex::Regex::new(&pattern).ok()?;

    for path in workspace_files {
        let Some(content) = crate::workspace::index::read_searchable(path) else {
            continue;
        };
        if let Some(m) = re.find(&content) {
            let byte_offset = m.start();
            let line = content[..byte_offset].bytes().filter(|b| *b == b'\n').count() + 1;
            return Some((Some(path.clone()), byte_offset, line));
        }
    }
    None
}

/// Generate hover documentation for a token/symbol at the given line/byte.
pub fn hover_documentation(
    word: &str,
    line: usize,
    symbols: &[OutlineSymbol],
    diagnostics: &[Diagnostic],
) -> Option<String> {
    for diag in diagnostics {
        if diag.line == line {
            return Some(format!("[{}] {}", diag.source, diag.message));
        }
    }
    if word.is_empty() {
        return None;
    }
    if let Some(sym) = symbols.iter().find(|s| s.name == word) {
        return Some(format!(
            "{} `{}` — 定义于第 {} 行",
            sym.kind.label(),
            sym.name,
            sym.start_line + 1
        ));
    }
    None
}

/// Formatted document text plus whether an external formatter produced it.
pub struct FormatResult {
    pub text: String,
    pub external: bool,
}

/// Format document text using an external language formatter or the built-in
/// whitespace cleaner.
pub fn format_document(
    language: Language,
    text: &str,
    tab_size: usize,
    insert_spaces: bool,
) -> FormatResult {
    if let Some((cmd, args)) = external_formatter(language)
        && let Some(text) = run_stdio_formatter(cmd, args, text)
    {
        return FormatResult {
            text,
            external: true,
        };
    }
    // Built-in clean formatter: normalize trailing whitespace, tabs/spaces, and final newline.
    let indent_unit = if insert_spaces {
        " ".repeat(tab_size.max(1))
    } else {
        "\t".to_string()
    };
    let mut out = String::with_capacity(text.len() + 16);
    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let trimmed_end = line.trim_end_matches([' ', '\t', '\r']);
        if insert_spaces && trimmed_end.starts_with('\t') {
            let tabs = trimmed_end.bytes().take_while(|b| *b == b'\t').count();
            out.push_str(&indent_unit.repeat(tabs));
            out.push_str(&trimmed_end[tabs..]);
        } else {
            out.push_str(trimmed_end);
        }
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    FormatResult {
        text: out,
        external: false,
    }
}

fn external_formatter(language: Language) -> Option<(&'static str, &'static [&'static str])> {
    match language {
        Language::Rust => Some(("rustfmt", &["--edition", "2024"])),
        Language::Python => Some(("ruff", &["format", "-"])),
        Language::Go => Some(("gofmt", &[])),
        _ => None,
    }
}

fn run_stdio_formatter(program: &str, args: &[&str], input: &str) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).ok()?;
    }
    let output = child.wait_with_output().ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        String::from_utf8(output.stdout).ok().map(|s| s.replace("\r\n", "\n"))
    } else {
        None
    }
}

fn language_keywords(
    language: Language,
) -> &'static [(&'static str, &'static str, Option<&'static str>)] {
    match language {
        Language::Rust => &[
            ("fn", "Rust 函数定义", Some("fn name() {\n    \n}")),
            ("pub fn", "公开函数定义", Some("pub fn name() {\n    \n}")),
            ("struct", "结构体定义", Some("struct Name {\n    \n}")),
            ("enum", "枚举定义", Some("enum Name {\n    \n}")),
            ("impl", "实现块", Some("impl Name {\n    \n}")),
            ("trait", "特征定义", Some("trait Name {\n    \n}")),
            ("match", "模式匹配", Some("match value {\n    _ => {}\n}")),
            ("let", "变量绑定", None),
            ("mut", "可变修饰符", None),
            ("return", "返回值", None),
            ("use", "导入路径", None),
            ("mod", "模块声明", None),
            ("const", "常量声明", None),
            ("async", "异步修饰符", None),
            ("await", "等待异步Future", None),
            ("Option", "标准库可选类型", None),
            ("Result", "标准库结果类型", None),
            ("String", "UTF-8 字符串", None),
            ("Vec", "动态数组", None),
            ("println!", "打印到标准输出", Some("println!(\"{}\");")),
            ("cfg(test)", "测试模块配置", Some("#[cfg(test)]\nmod tests {\n    use super::*;\n}")),
        ],
        Language::Python => &[
            ("def", "Python 函数", Some("def name():\n    pass")),
            ("class", "Python 类", Some("class Name:\n    def __init__(self):\n        pass")),
            ("import", "导入模块", None),
            ("from", "从模块导入", None),
            ("return", "返回值", None),
            ("async def", "异步函数", Some("async def name():\n    pass")),
            ("await", "等待协程", None),
            ("if __name__", "主入口块", Some("if __name__ == \"__main__\":\n    ")),
            ("print", "内置打印函数", None),
            ("isinstance", "类型检查", None),
        ],
        Language::TypeScript | Language::Tsx | Language::JavaScript => &[
            ("function", "函数声明", Some("function name() {\n    \n}")),
            ("interface", "TypeScript 接口", Some("interface Name {\n    \n}")),
            ("type", "类型别名", None),
            ("const", "常量声明", None),
            ("let", "块级变量", None),
            ("export", "导出声明", None),
            ("import", "导入模块", None),
            ("async", "异步函数", None),
            ("await", "等待 Promise", None),
            ("return", "返回值", None),
            ("console.log", "控制台输出", Some("console.log();")),
        ],
        Language::Go => &[
            ("func", "Go 函数", Some("func name() {\n\t\n}")),
            ("type", "Go 类型定义", None),
            ("struct", "Go 结构体", None),
            ("interface", "Go 接口", None),
            ("package", "包声明", None),
            ("import", "导入包", None),
            ("return", "返回", None),
            ("if err != nil", "错误检查模板", Some("if err != nil {\n\treturn err\n}")),
        ],
        Language::C | Language::Cpp | Language::CSharp | Language::Java => &[
            ("class", "类定义", None),
            ("struct", "结构体定义", None),
            ("public", "公开访问修饰", None),
            ("private", "私有访问修饰", None),
            ("static", "静态修饰", None),
            ("void", "无返回值类型", None),
            ("return", "返回语句", None),
            ("include", "头文件引入", None),
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_outline_and_folds() {
        let md = "# Title\nIntro\n## Section 1\nLine\n## Section 2\nEnd\n";
        let (symbols, folds) = analyze_markdown(md);
        assert_eq!(symbols.len(), 3);
        assert_eq!(symbols[0].name, "Title");
        assert_eq!(symbols[1].name, "Section 1");
        assert!(!folds.is_empty());
    }

    #[test]
    fn completion_ranks_symbols_and_keywords() {
        let symbols = vec![OutlineSymbol {
            name: "render_view".to_string(),
            detail: "fn".to_string(),
            kind: SymbolKind::Function,
            start_line: 10,
            end_line: 20,
            byte_offset: 100,
            depth: 0,
        }];
        let items = complete("rend", Language::Rust, &symbols, "let x = 1;");
        assert!(!items.is_empty());
        assert_eq!(items[0].label, "render_view");
    }

    fn function_symbol(name: &str) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_string(),
            detail: "fn".to_string(),
            kind: SymbolKind::Function,
            start_line: 0,
            end_line: 0,
            byte_offset: 0,
            depth: 0,
        }
    }

    #[test]
    fn completion_results_fit_the_popup() {
        let symbols: Vec<OutlineSymbol> = (0..20)
            .map(|index| function_symbol(&format!("symbol_{index}")))
            .collect();
        let members = member_candidates(Language::Rust, &symbols);
        assert_eq!(members.len(), COMPLETION_MAX_ITEMS);
        let items = complete("symbol", Language::Rust, &symbols, "symbol_alpha symbol_beta");
        assert!(items.len() <= COMPLETION_MAX_ITEMS);
    }

    #[test]
    fn member_candidates_fall_back_to_keywords() {
        let members = member_candidates(Language::Rust, &[]);
        assert!(!members.is_empty());
        assert!(members.len() <= COMPLETION_MAX_ITEMS);
        assert!(
            members
                .iter()
                .any(|item| matches!(item.kind, SymbolKind::Keyword | SymbolKind::Snippet))
        );
    }

    #[test]
    fn completion_scan_handles_multibyte_boundary() {
        // 180 KB of 3-byte characters: the 160 KB scan boundary lands mid-character.
        let text = "中".repeat(60_000);
        let items = complete("a", Language::Rust, &[], &text);
        assert!(items.len() <= COMPLETION_MAX_ITEMS);
    }
}
