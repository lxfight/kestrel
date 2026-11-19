//! Complete CommonMark + GFM + Extended Markdown parser and rich egui renderer.

pub mod parser;
pub mod render;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::highlight::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MarkdownViewMode {
    #[default]
    Source,
    Split,
    Preview,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrontMatterKind {
    Yaml,
    Toml,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontMatter {
    pub kind: FrontMatterKind,
    pub entries: Vec<(String, String)>,
    pub raw: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
    Info,
    Danger,
    Bug,
    Example,
    Quote,
}

impl CalloutKind {
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_uppercase().as_str() {
            "NOTE" => Some(Self::Note),
            "TIP" | "HINT" => Some(Self::Tip),
            "IMPORTANT" => Some(Self::Important),
            "WARNING" | "ATTENTION" => Some(Self::Warning),
            "CAUTION" => Some(Self::Caution),
            "INFO" => Some(Self::Info),
            "DANGER" | "ERROR" => Some(Self::Danger),
            "BUG" => Some(Self::Bug),
            "EXAMPLE" => Some(Self::Example),
            "QUOTE" | "CITE" => Some(Self::Quote),
            _ => None,
        }
    }

    pub fn default_title(self) -> &'static str {
        match self {
            Self::Note => "笔记 (Note)",
            Self::Tip => "提示 (Tip)",
            Self::Important => "重要 (Important)",
            Self::Warning => "警告 (Warning)",
            Self::Caution => "注意 (Caution)",
            Self::Info => "信息 (Info)",
            Self::Danger => "危险 (Danger)",
            Self::Bug => "缺陷 (Bug)",
            Self::Example => "示例 (Example)",
            Self::Quote => "引用 (Quote)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug)]
pub struct FootnoteDef {
    pub id: String,
    pub number: usize,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub struct DefItem {
    pub term: Vec<Inline>,
    pub definitions: Vec<Vec<Block>>,
}

#[derive(Clone, Debug)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub checkbox_byte: Option<usize>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
#[allow(clippy::enum_variant_names)] // CodeBlock / MathBlock / HtmlBlock 与 Markdown AST 术语一致
pub enum Block {
    Heading {
        level: u8,
        slug: String,
        inlines: Vec<Inline>,
        source_line: usize,
    },
    Paragraph {
        inlines: Vec<Inline>,
        align: Option<Align>,
    },
    CodeBlock {
        lang: String,
        meta: String,
        code: String,
        spans: Vec<Span>,
        source_line: usize,
    },
    MathBlock {
        tex: String,
        source_line: usize,
    },
    Blockquote {
        callout: Option<(CalloutKind, Option<String>)>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: usize,
        tight: bool,
        items: Vec<ListItem>,
    },
    Table {
        alignments: Vec<Align>,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        source_line: usize,
    },
    DefinitionList {
        items: Vec<DefItem>,
    },
    Details {
        summary: Vec<Inline>,
        blocks: Vec<Block>,
        open_by_default: bool,
        source_line: usize,
    },
    ThematicBreak,
    HtmlBlock {
        html: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text(String),
    SoftBreak,
    HardBreak,
    Code(String),
    Math(String),
    Kbd(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Highlight(Vec<Inline>),
    Underline(Vec<Inline>),
    Superscript(Vec<Inline>),
    Subscript(Vec<Inline>),
    Link {
        text: Vec<Inline>,
        url: String,
        title: Option<String>,
    },
    Image {
        alt: String,
        url: String,
        title: Option<String>,
        width: Option<f32>,
    },
    FootnoteRef {
        id: String,
        number: usize,
    },
}

#[derive(Clone, Debug, Default)]
pub struct MarkdownDoc {
    pub front_matter: Option<FrontMatter>,
    pub blocks: Vec<Block>,
    pub footnotes: Vec<FootnoteDef>,
}

pub enum CachedImage {
    Loaded {
        texture: egui::TextureHandle,
        size: [usize; 2],
        modified: Option<SystemTime>,
    },
    Failed(String),
}

#[derive(Default)]
pub struct MarkdownPreviewState {
    pub parsed_version: u64,
    pub doc: MarkdownDoc,
    pub scroll_y: f32,
    pub pending_scroll_to: Option<String>,
    pub anchor_y: HashMap<String, f32>,
    /// (source line, rendered y) anchors measured during the last paint.
    pub line_anchors: Vec<(usize, f32)>,
    /// Scroll offset to apply on the next paint (source -> preview sync).
    pub pending_scroll_y: Option<f32>,
    pub content_height: f32,
    pub viewport_height: f32,
    /// Set when the preview drove the source scroll, to avoid a feedback loop.
    pub hold_source_sync: bool,
    /// First paint of split mode should align the preview with the source scroll.
    pub split_synced: bool,
    pub textures: HashMap<String, CachedImage>,
}

impl MarkdownPreviewState {
    pub fn ensure_parsed(&mut self, version: u64, text: &str) {
        if self.parsed_version != version {
            self.doc = parser::parse(text);
            self.parsed_version = version;
        }
    }
}

#[derive(Clone, Debug)]
pub enum PreviewEvent {
    ToggleTask {
        byte_offset: usize,
        checked: bool,
    },
    OpenUrl(String),
    OpenFile {
        path: PathBuf,
        anchor: Option<String>,
    },
    CopyText(String),
    JumpSourceLine(usize),
}
