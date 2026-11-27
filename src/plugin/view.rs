//! Retained-mode declarative UI views for plugins and the right-hand Auxiliary Sidebar.
//!
//! Plugins submit a declarative [`UiNode`] tree only when their state changes
//! (never per frame). The Rust host caches the tree and paints it natively with
//! `egui` at full frame rate, reusing Kestrel's Markdown renderer and syntax
//! highlighter, then dispatches user interactions back to Lua asynchronously.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use egui::{
    Align2, Color32, ComboBox, CornerRadius, Frame, Key, Margin, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, Vec2,
};
use mlua::{Table, Value};

use crate::app::{KestrelApp, ToastKind};
use crate::icons::{self, Icon};
use crate::markdown::{MarkdownPreviewState, PreviewEvent};
use crate::theme::Theme;
use crate::ui::UiStyle;

const MAX_UI_DEPTH: usize = 24;
const MAX_UI_NODES: usize = 2_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ViewLocation {
    #[default]
    Right,
    Left,
}

impl ViewLocation {
    pub fn parse(name: Option<&str>) -> Self {
        match name {
            Some("left") => Self::Left,
            _ => Self::Right,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tone {
    #[default]
    Default,
    Accent,
    Info,
    Success,
    Warning,
    Error,
}

impl Tone {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("accent") => Self::Accent,
            Some("info") => Self::Info,
            Some("success") => Self::Success,
            Some("warning") | Some("warn") => Self::Warning,
            Some("error") | Some("danger") => Self::Error,
            _ => Self::Default,
        }
    }

    fn fg(self, theme: &Theme) -> Color32 {
        match self {
            Self::Default => theme.text,
            Self::Accent | Self::Info => theme.accent,
            Self::Success => theme.git_added,
            Self::Warning => theme.warning,
            Self::Error => theme.error,
        }
    }

    fn tint(self, theme: &Theme) -> Color32 {
        match self {
            Self::Default => theme.surface_subtle,
            Self::Accent => theme.accent_subtle,
            Self::Info => theme.accent.gamma_multiply(0.12),
            Self::Success => theme.git_added.gamma_multiply(0.14),
            Self::Warning => theme.warning.gamma_multiply(0.15),
            Self::Error => theme.error.gamma_multiply(0.15),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonVariant {
    #[default]
    Default,
    Primary,
    Danger,
    Ghost,
}

impl ButtonVariant {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("primary") => Self::Primary,
            Some("danger") => Self::Danger,
            Some("ghost") => Self::Ghost,
            _ => Self::Default,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub enum UiNode {
    VStack {
        spacing: f32,
        padding: f32,
        children: Vec<UiNode>,
    },
    HStack {
        spacing: f32,
        wrap: bool,
        children: Vec<UiNode>,
    },
    Scroll {
        id: String,
        stick_to_bottom: bool,
        max_height: Option<f32>,
        children: Vec<UiNode>,
    },
    Card {
        title: Option<String>,
        subtitle: Option<String>,
        tone: Tone,
        children: Vec<UiNode>,
    },
    Collapsible {
        id: String,
        title: String,
        badge: Option<String>,
        default_open: bool,
        children: Vec<UiNode>,
    },
    Text {
        text: String,
        size: Option<f32>,
        bold: bool,
        mono: bool,
        dim: bool,
        tone: Tone,
    },
    Markdown {
        id: String,
        text: String,
        hash: u64,
    },
    Code {
        code: String,
        language: Option<String>,
        copyable: bool,
    },
    DiffCard {
        path: String,
        added: usize,
        removed: usize,
        summary: Option<String>,
        action_open: Option<String>,
        action_accept: Option<String>,
        action_reject: Option<String>,
    },
    Button {
        id: String,
        label: String,
        variant: ButtonVariant,
        disabled: bool,
    },
    TextInput {
        id: String,
        value: Option<String>,
        placeholder: String,
        multiline: bool,
        rows: usize,
        submit_on_enter: bool,
        password: bool,
        clear_on_submit: bool,
    },
    Select {
        id: String,
        value: String,
        options: Vec<SelectOption>,
    },
    Checkbox {
        id: String,
        label: String,
        checked: bool,
    },
    Badge {
        text: String,
        tone: Tone,
    },
    Spinner {
        text: Option<String>,
    },
    Separator,
    Spacer {
        size: f32,
    },
}

/// User interaction emitted while painting a plugin view, dispatched to Lua after paint.
#[derive(Clone, Debug)]
pub struct ViewEvent {
    pub view_id: String,
    pub kind: String,
    pub id: String,
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub values: HashMap<String, String>,
}

/// Local Rust-owned interactive widget state (text inputs, markdown caches,
/// collapsible open flags) so typing & scrolling run at native 60–120 FPS with
/// zero per-frame Lua invocations.
#[derive(Default)]
pub struct ViewRenderState {
    /// Live text buffers keyed by input `id`.
    pub inputs: HashMap<String, String>,
    /// Last `value` pushed from Lua for each input `id`, so Lua can reset/clear
    /// an input without clobbering user typing on unrelated tree updates.
    pub synced_inputs: HashMap<String, String>,
    pub collapsed: HashMap<String, bool>,
    pub markdown: HashMap<String, MarkdownPreviewState>,
}

pub struct PluginView {
    pub plugin_index: usize,
    pub runtime: u64,
    pub id: String,
    pub title: String,
    pub location: ViewLocation,
    pub nodes: Vec<UiNode>,
    pub state: ViewRenderState,
}

impl PluginView {
    pub fn new(
        plugin_index: usize,
        runtime: u64,
        id: String,
        title: String,
        location: ViewLocation,
    ) -> Self {
        Self {
            plugin_index,
            runtime,
            id,
            title,
            location,
            nodes: Vec::new(),
            state: ViewRenderState::default(),
        }
    }
}

/// Parse a Lua list of UI node tables into validated [`UiNode`]s.
pub fn parse_nodes(table: &Table) -> mlua::Result<Vec<UiNode>> {
    let mut count = 0usize;
    parse_node_list(table, 0, &mut count)
}

fn parse_node_list(table: &Table, depth: usize, count: &mut usize) -> mlua::Result<Vec<UiNode>> {
    if depth > MAX_UI_DEPTH {
        return Err(mlua::Error::runtime("UI 节点嵌套超过最大深度"));
    }
    let mut nodes = Vec::new();
    for item in table.sequence_values::<Table>() {
        let item = item?;
        nodes.push(parse_node(&item, depth, count)?);
    }
    Ok(nodes)
}

fn parse_children(table: &Table, depth: usize, count: &mut usize) -> mlua::Result<Vec<UiNode>> {
    match table.get::<Option<Table>>("children")? {
        Some(children) => parse_node_list(&children, depth + 1, count),
        None => Ok(Vec::new()),
    }
}

fn parse_node(table: &Table, depth: usize, count: &mut usize) -> mlua::Result<UiNode> {
    *count += 1;
    if *count > MAX_UI_NODES {
        return Err(mlua::Error::runtime(format!(
            "UI 节点数量超过上限 {MAX_UI_NODES}"
        )));
    }
    let kind: String = table.get("type")?;
    match kind.as_str() {
        "vstack" => Ok(UiNode::VStack {
            spacing: table.get::<Option<f32>>("spacing")?.unwrap_or(6.0),
            padding: table.get::<Option<f32>>("padding")?.unwrap_or(0.0),
            children: parse_children(table, depth, count)?,
        }),
        "hstack" => Ok(UiNode::HStack {
            spacing: table.get::<Option<f32>>("spacing")?.unwrap_or(6.0),
            wrap: table.get::<Option<bool>>("wrap")?.unwrap_or(false),
            children: parse_children(table, depth, count)?,
        }),
        "scroll" => Ok(UiNode::Scroll {
            id: table
                .get::<Option<String>>("id")?
                .unwrap_or_else(|| format!("scroll_{count}")),
            stick_to_bottom: table.get::<Option<bool>>("stick_to_bottom")?.unwrap_or(false),
            max_height: table.get::<Option<f32>>("max_height")?,
            children: parse_children(table, depth, count)?,
        }),
        "card" => Ok(UiNode::Card {
            title: table.get::<Option<String>>("title")?,
            subtitle: table.get::<Option<String>>("subtitle")?,
            tone: Tone::parse(table.get::<Option<String>>("tone")?.as_deref()),
            children: parse_children(table, depth, count)?,
        }),
        "collapsible" => Ok(UiNode::Collapsible {
            id: table
                .get::<Option<String>>("id")?
                .unwrap_or_else(|| format!("collapse_{count}")),
            title: table.get::<Option<String>>("title")?.unwrap_or_default(),
            badge: table.get::<Option<String>>("badge")?,
            default_open: table.get::<Option<bool>>("default_open")?.unwrap_or(false),
            children: parse_children(table, depth, count)?,
        }),
        "text" => Ok(UiNode::Text {
            text: table.get::<Option<String>>("text")?.unwrap_or_default(),
            size: table.get::<Option<f32>>("size")?,
            bold: table.get::<Option<bool>>("bold")?.unwrap_or(false),
            mono: table.get::<Option<bool>>("mono")?.unwrap_or(false),
            dim: table.get::<Option<bool>>("dim")?.unwrap_or(false),
            tone: Tone::parse(table.get::<Option<String>>("tone")?.as_deref()),
        }),
        "markdown" => {
            let text = table.get::<Option<String>>("text")?.unwrap_or_default();
            let id = table
                .get::<Option<String>>("id")?
                .unwrap_or_else(|| format!("md_{count}"));
            let hash = hash_str(&text);
            Ok(UiNode::Markdown { id, text, hash })
        }
        "code" => Ok(UiNode::Code {
            code: table.get::<Option<String>>("code")?.unwrap_or_default(),
            language: table.get::<Option<String>>("language")?,
            copyable: table.get::<Option<bool>>("copyable")?.unwrap_or(true),
        }),
        "diff_card" => Ok(UiNode::DiffCard {
            path: table.get::<Option<String>>("path")?.unwrap_or_default(),
            added: table.get::<Option<usize>>("added")?.unwrap_or(0),
            removed: table.get::<Option<usize>>("removed")?.unwrap_or(0),
            summary: table.get::<Option<String>>("summary")?,
            action_open: table.get::<Option<String>>("action_open")?,
            action_accept: table.get::<Option<String>>("action_accept")?,
            action_reject: table.get::<Option<String>>("action_reject")?,
        }),
        "button" => Ok(UiNode::Button {
            id: table.get("id")?,
            label: table.get("label")?,
            variant: ButtonVariant::parse(table.get::<Option<String>>("variant")?.as_deref()),
            disabled: table.get::<Option<bool>>("disabled")?.unwrap_or(false),
        }),
        "text_input" => {
            let multiline = table.get::<Option<bool>>("multiline")?.unwrap_or(false);
            Ok(UiNode::TextInput {
                id: table.get("id")?,
                value: table.get::<Option<String>>("value")?,
                placeholder: table
                    .get::<Option<String>>("placeholder")?
                    .unwrap_or_default(),
                multiline,
                rows: table.get::<Option<usize>>("rows")?.unwrap_or(if multiline { 3 } else { 1 }),
                submit_on_enter: table
                    .get::<Option<bool>>("submit_on_enter")?
                    .unwrap_or(!multiline),
                password: table.get::<Option<bool>>("password")?.unwrap_or(false),
                clear_on_submit: table.get::<Option<bool>>("clear_on_submit")?.unwrap_or(false),
            })
        }
        "select" => {
            let id: String = table.get("id")?;
            let value: String = table.get::<Option<String>>("value")?.unwrap_or_default();
            let mut options = Vec::new();
            if let Some(opts) = table.get::<Option<Table>>("options")? {
                for opt in opts.sequence_values::<Value>() {
                    match opt? {
                        Value::String(s) => {
                            let v = s.to_string_lossy();
                            options.push(SelectOption {
                                value: v.clone(),
                                label: v,
                            });
                        }
                        Value::Table(t) => {
                            let v: String = t.get("value")?;
                            let l: String = t
                                .get::<Option<String>>("label")?
                                .unwrap_or_else(|| v.clone());
                            options.push(SelectOption { value: v, label: l });
                        }
                        _ => {}
                    }
                }
            }
            Ok(UiNode::Select { id, value, options })
        }
        "checkbox" => Ok(UiNode::Checkbox {
            id: table.get("id")?,
            label: table.get::<Option<String>>("label")?.unwrap_or_default(),
            checked: table.get::<Option<bool>>("checked")?.unwrap_or(false),
        }),
        "badge" => Ok(UiNode::Badge {
            text: table.get::<Option<String>>("text")?.unwrap_or_default(),
            tone: Tone::parse(table.get::<Option<String>>("tone")?.as_deref()),
        }),
        "spinner" => Ok(UiNode::Spinner {
            text: table.get::<Option<String>>("text")?,
        }),
        "separator" => Ok(UiNode::Separator),
        "spacer" => Ok(UiNode::Spacer {
            size: table.get::<Option<f32>>("size")?.unwrap_or(8.0),
        }),
        other => Err(mlua::Error::runtime(format!("未知 UI 组件类型: {other}"))),
    }
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish().max(1)
}

struct PaintCtx<'a> {
    view_id: &'a str,
    style: &'a UiStyle,
    workspace_root: Option<&'a Path>,
    state: &'a mut ViewRenderState,
    events: Vec<ViewEvent>,
    md_events: Vec<PreviewEvent>,
}

fn paint_nodes(ui: &mut Ui, ctx: &mut PaintCtx<'_>, nodes: &[UiNode]) {
    for (idx, node) in nodes.iter().enumerate() {
        ui.push_id(idx, |ui| {
            paint_node(ui, ctx, node);
        });
    }
}

fn paint_node(ui: &mut Ui, ctx: &mut PaintCtx<'_>, node: &UiNode) {
    let theme = ctx.style.theme;
    match node {
        UiNode::VStack {
            spacing,
            padding,
            children,
        } => {
            Frame::new()
                .inner_margin(Margin::same(*padding as i8))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = *spacing;
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        paint_nodes(ui, ctx, children);
                    });
                });
        }
        UiNode::HStack {
            spacing,
            wrap,
            children,
        } => {
            ui.spacing_mut().item_spacing.x = *spacing;
            if *wrap {
                ui.horizontal_wrapped(|ui| {
                    paint_nodes(ui, ctx, children);
                });
            } else {
                ui.horizontal(|ui| {
                    paint_nodes(ui, ctx, children);
                });
            }
        }
        UiNode::Scroll {
            id,
            stick_to_bottom,
            max_height,
            children,
        } => {
            let mut scroll = ScrollArea::vertical()
                .id_salt(("plugin_view_scroll", ctx.view_id, id.as_str()))
                .auto_shrink([false, false])
                .stick_to_bottom(*stick_to_bottom);
            if let Some(h) = max_height {
                scroll = scroll.max_height(*h);
            }
            scroll.show(ui, |ui| {
                ui.set_width(ui.available_width());
                paint_nodes(ui, ctx, children);
            });
        }
        UiNode::Card {
            title,
            subtitle,
            tone,
            children,
        } => {
            let border = match tone {
                Tone::Default => theme.border,
                other => other.fg(&theme).gamma_multiply(0.45),
            };
            Frame::new()
                .fill(tone.tint(&theme))
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(crate::theme::R_MD))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if title.is_some() || subtitle.is_some() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if let Some(title) = title {
                                ui.label(
                                    RichText::new(title)
                                        .font(ctx.style.bold(11.5))
                                        .color(tone.fg(&theme)),
                                );
                            }
                            if let Some(sub) = subtitle {
                                ui.label(
                                    RichText::new(sub)
                                        .font(ctx.style.tiny())
                                        .color(theme.text_muted),
                                );
                            }
                        });
                        if !children.is_empty() {
                            ui.add_space(4.0);
                        }
                    }
                    paint_nodes(ui, ctx, children);
                });
        }
        UiNode::Collapsible {
            id,
            title,
            badge,
            default_open,
            children,
        } => {
            let open = *ctx.state.collapsed.entry(id.clone()).or_insert(*default_open);
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(crate::theme::R_SM),
                    theme.hover,
                );
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(rect.left() + 10.0, rect.center().y),
                    Vec2::splat(12.0),
                ),
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                theme.text_dim,
                1.2,
            );
            ui.painter().text(
                Pos2::new(rect.left() + 22.0, rect.center().y),
                Align2::LEFT_CENTER,
                title,
                ctx.style.small(),
                theme.text_dim,
            );
            if let Some(badge) = badge {
                ui.painter().text(
                    Pos2::new(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    badge,
                    ctx.style.tiny(),
                    theme.text_faint,
                );
            }
            let next_open = if resp.clicked() { !open } else { open };
            ctx.state.collapsed.insert(id.clone(), next_open);
            if next_open && !children.is_empty() {
                Frame::new()
                    .inner_margin(Margin {
                        left: 14,
                        right: 2,
                        top: 4,
                        bottom: 4,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        paint_nodes(ui, ctx, children);
                    });
            }
        }
        UiNode::Text {
            text,
            size,
            bold,
            mono,
            dim,
            tone,
        } => {
            let sz = size.unwrap_or(12.5);
            let font = if *mono {
                egui::FontId::new(sz, ctx.style.mono_family.clone())
            } else if *bold {
                ctx.style.bold(sz)
            } else {
                ctx.style.font(sz)
            };
            let color = if *dim {
                theme.text_dim
            } else {
                tone.fg(&theme)
            };
            ui.add(egui::Label::new(RichText::new(text).font(font).color(color)).wrap());
        }
        UiNode::Markdown { id, text, hash } => {
            let md_state = ctx.state.markdown.entry(id.clone()).or_default();
            let evs = crate::markdown::render::embedded_ui(
                ui,
                ctx.style,
                md_state,
                *hash,
                text,
                ctx.workspace_root,
                12.5,
            );
            ctx.md_events.extend(evs);
        }
        UiNode::Code {
            code,
            language,
            copyable,
        } => {
            Frame::new()
                .fill(theme.input_bg)
                .stroke(Stroke::new(1.0, theme.border))
                .corner_radius(CornerRadius::same(crate::theme::R_SM))
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if language.is_some() || *copyable {
                        ui.horizontal(|ui| {
                            if let Some(lang) = language {
                                ui.label(
                                    RichText::new(lang)
                                        .font(ctx.style.tiny())
                                        .color(theme.text_faint),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if *copyable
                                        && ui
                                            .small_button(
                                                RichText::new("复制")
                                                    .font(ctx.style.tiny())
                                                    .color(theme.text_dim),
                                            )
                                            .clicked()
                                    {
                                        ctx.md_events.push(PreviewEvent::CopyText(code.clone()));
                                    }
                                },
                            );
                        });
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(code)
                                .font(egui::FontId::new(11.5, ctx.style.mono_family.clone()))
                                .color(theme.text),
                        )
                        .wrap(),
                    );
                });
        }
        UiNode::DiffCard {
            path,
            added,
            removed,
            summary,
            action_open,
            action_accept,
            action_reject,
        } => {
            Frame::new()
                .fill(theme.surface_subtle)
                .stroke(Stroke::new(1.0, theme.border))
                .corner_radius(CornerRadius::same(crate::theme::R_MD))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new(path)
                                .font(egui::FontId::new(11.5, ctx.style.mono_family.clone()))
                                .color(theme.text),
                        );
                        if *added > 0 {
                            ui.label(
                                RichText::new(format!("+{added}"))
                                    .font(ctx.style.tiny())
                                    .color(theme.git_added),
                            );
                        }
                        if *removed > 0 {
                            ui.label(
                                RichText::new(format!("-{removed}"))
                                    .font(ctx.style.tiny())
                                    .color(theme.error),
                            );
                        }
                    });
                    if let Some(summary) = summary {
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(summary)
                                    .font(ctx.style.tiny())
                                    .color(theme.text_dim),
                            )
                            .wrap(),
                        );
                    }
                    if action_open.is_some() || action_accept.is_some() || action_reject.is_some() {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if let Some(id) = action_open
                                && paint_button(ui, ctx.style, "查看 Diff", ButtonVariant::Default, false)
                            {
                                ctx.events.push(ViewEvent {
                                    view_id: ctx.view_id.to_string(),
                                    kind: "click".to_string(),
                                    id: id.clone(),
                                    value: Some(path.clone()),
                                    checked: None,
                                    values: ctx.state.inputs.clone(),
                                });
                            }
                            if let Some(id) = action_accept
                                && paint_button(ui, ctx.style, "接受修改", ButtonVariant::Primary, false)
                            {
                                ctx.events.push(ViewEvent {
                                    view_id: ctx.view_id.to_string(),
                                    kind: "click".to_string(),
                                    id: id.clone(),
                                    value: Some(path.clone()),
                                    checked: None,
                                    values: ctx.state.inputs.clone(),
                                });
                            }
                            if let Some(id) = action_reject
                                && paint_button(ui, ctx.style, "拒绝", ButtonVariant::Danger, false)
                            {
                                ctx.events.push(ViewEvent {
                                    view_id: ctx.view_id.to_string(),
                                    kind: "click".to_string(),
                                    id: id.clone(),
                                    value: Some(path.clone()),
                                    checked: None,
                                    values: ctx.state.inputs.clone(),
                                });
                            }
                        });
                    }
                });
        }
        UiNode::Button {
            id,
            label,
            variant,
            disabled,
        } => {
            if paint_button(ui, ctx.style, label, *variant, *disabled) {
                ctx.events.push(ViewEvent {
                    view_id: ctx.view_id.to_string(),
                    kind: "click".to_string(),
                    id: id.clone(),
                    value: None,
                    checked: None,
                    values: ctx.state.inputs.clone(),
                });
            }
        }
        UiNode::TextInput {
            id,
            value,
            placeholder,
            multiline,
            rows,
            submit_on_enter,
            password,
            clear_on_submit,
        } => {
            if let Some(lua_val) = value {
                let changed_from_lua = ctx
                    .state
                    .synced_inputs
                    .get(id)
                    .is_none_or(|prev| prev != lua_val);
                if changed_from_lua {
                    ctx.state.synced_inputs.insert(id.clone(), lua_val.clone());
                    ctx.state.inputs.insert(id.clone(), lua_val.clone());
                }
            }
            let buf = ctx.state.inputs.entry(id.clone()).or_default();
            let mut submitted = false;
            if *multiline && *submit_on_enter {
                let enter_pressed = ui.input(|i| {
                    i.key_pressed(Key::Enter) && !i.modifiers.shift && !i.modifiers.ctrl
                });
                let widget_id = ui.make_persistent_id(("plugin_input", ctx.view_id, id.as_str()));
                if enter_pressed && ui.memory(|m| m.has_focus(widget_id)) {
                    ui.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, Key::Enter);
                    });
                    submitted = true;
                }
                let edit = TextEdit::multiline(buf)
                    .id(widget_id)
                    .hint_text(placeholder)
                    .desired_rows((*rows).clamp(1, 12))
                    .desired_width(ui.available_width())
                    .font(ctx.style.small());
                ui.add(edit);
            } else if *multiline {
                let edit = TextEdit::multiline(buf)
                    .hint_text(placeholder)
                    .desired_rows((*rows).clamp(1, 12))
                    .desired_width(ui.available_width())
                    .font(ctx.style.small());
                ui.add(edit);
            } else {
                let edit = TextEdit::singleline(buf)
                    .hint_text(placeholder)
                    .password(*password)
                    .desired_width(ui.available_width())
                    .font(ctx.style.small());
                let resp = ui.add(edit);
                if *submit_on_enter
                    && resp.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                {
                    submitted = true;
                }
            }
            if submitted {
                let submitted_value = buf.clone();
                if *clear_on_submit {
                    buf.clear();
                }
                let values = ctx.state.inputs.clone();
                ctx.events.push(ViewEvent {
                    view_id: ctx.view_id.to_string(),
                    kind: "submit".to_string(),
                    id: id.clone(),
                    value: Some(submitted_value),
                    checked: None,
                    values,
                });
            }
        }
        UiNode::Select { id, value, options } => {
            let selected_label = options
                .iter()
                .find(|opt| &opt.value == value)
                .map(|opt| opt.label.as_str())
                .unwrap_or(value.as_str());
            let mut next_val = value.clone();
            ComboBox::from_id_salt(("plugin_select", ctx.view_id, id.as_str()))
                .selected_text(RichText::new(selected_label).font(ctx.style.small()))
                .show_ui(ui, |ui| {
                    for opt in options {
                        ui.selectable_value(&mut next_val, opt.value.clone(), &opt.label);
                    }
                });
            if next_val != *value {
                ctx.state.inputs.insert(id.clone(), next_val.clone());
                ctx.events.push(ViewEvent {
                    view_id: ctx.view_id.to_string(),
                    kind: "select".to_string(),
                    id: id.clone(),
                    value: Some(next_val),
                    checked: None,
                    values: ctx.state.inputs.clone(),
                });
            }
        }
        UiNode::Checkbox { id, label, checked } => {
            let mut val = *checked;
            if ui
                .checkbox(&mut val, RichText::new(label).font(ctx.style.small()))
                .changed()
            {
                ctx.events.push(ViewEvent {
                    view_id: ctx.view_id.to_string(),
                    kind: "toggle".to_string(),
                    id: id.clone(),
                    value: None,
                    checked: Some(val),
                    values: ctx.state.inputs.clone(),
                });
            }
        }
        UiNode::Badge { text, tone } => {
            Frame::new()
                .fill(tone.tint(&theme))
                .stroke(Stroke::new(1.0, tone.fg(&theme).gamma_multiply(0.35)))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(text)
                            .font(ctx.style.tiny())
                            .color(tone.fg(&theme)),
                    );
                });
        }
        UiNode::Spinner { text } => {
            ui.horizontal(|ui| {
                ui.spinner();
                if let Some(text) = text {
                    ui.label(
                        RichText::new(text)
                            .font(ctx.style.small())
                            .color(theme.text_dim),
                    );
                }
            });
        }
        UiNode::Separator => {
            ui.add_space(4.0);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
            ui.painter().line_segment(
                [rect.left_center(), rect.right_center()],
                Stroke::new(1.0, theme.border),
            );
            ui.add_space(4.0);
        }
        UiNode::Spacer { size } => {
            ui.add_space(*size);
        }
    }
}

fn paint_button(
    ui: &mut Ui,
    style: &UiStyle,
    label: &str,
    variant: ButtonVariant,
    disabled: bool,
) -> bool {
    let theme = style.theme;
    let (bg, fg, stroke_color) = match variant {
        ButtonVariant::Primary => (theme.accent_fill, theme.accent_text, theme.accent_fill),
        ButtonVariant::Danger => (
            theme.error.gamma_multiply(0.18),
            theme.error,
            theme.error.gamma_multiply(0.5),
        ),
        ButtonVariant::Ghost => (Color32::TRANSPARENT, theme.text_dim, Color32::TRANSPARENT),
        ButtonVariant::Default => (theme.input_bg, theme.text, theme.border),
    };
    let btn = egui::Button::new(
        RichText::new(label)
            .font(style.small())
            .color(if disabled { theme.text_faint } else { fg }),
    )
    .fill(if disabled { theme.surface_subtle } else { bg })
    .stroke(Stroke::new(1.0, stroke_color))
    .corner_radius(CornerRadius::same(crate::theme::R_SM));
    ui.add_enabled(!disabled, btn).clicked()
}

impl KestrelApp {
    /// Render the right-hand Auxiliary Sidebar hosting plugin views (e.g. AI Agent chat).
    pub(crate) fn aux_sidebar_ui(&mut self, ui: &mut Ui, style: &UiStyle) {
        let theme = style.theme;
        ui.spacing_mut().item_spacing.y = 0.0;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme.sidebar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left() + 0.5, rect.top()),
                Pos2::new(rect.left() + 0.5, rect.bottom()),
            ],
            Stroke::new(1.0, theme.border),
        );

        let right_views: Vec<(String, String)> = self
            .plugins
            .views()
            .iter()
            .filter(|v| v.location == ViewLocation::Right)
            .map(|v| (v.id.clone(), v.title.clone()))
            .collect();

        // Ensure active_view points to an existing right-side view if available.
        let active_id = match &self.aux_sidebar.active_view {
            Some(id) if right_views.iter().any(|(vid, _)| vid == id) => Some(id.clone()),
            _ => {
                let first = right_views.first().map(|(vid, _)| vid.clone());
                self.aux_sidebar.active_view = first.clone();
                first
            }
        };

        // Header bar with view selector tabs and close button.
        let (header, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
        ui.painter().line_segment(
            [
                Pos2::new(header.left(), header.bottom() - 0.5),
                Pos2::new(header.right(), header.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.border),
        );

        let close_rect = Rect::from_center_size(
            Pos2::new(header.right() - 18.0, header.center().y),
            Vec2::splat(22.0),
        );
        let close_resp = ui.interact(
            close_rect,
            ui.scope_id().with("aux_sidebar_close"),
            Sense::click(),
        );
        if close_resp.hovered() {
            ui.painter()
                .rect_filled(close_rect, CornerRadius::same(5), theme.hover);
        }
        icons::paint(
            ui.painter(),
            close_rect.shrink(4.0),
            Icon::Close,
            if close_resp.hovered() {
                theme.text
            } else {
                theme.text_dim
            },
            1.2,
        );
        if close_resp.on_hover_text("关闭辅助侧边栏 (Ctrl+Alt+B)").clicked() {
            self.aux_sidebar.visible = false;
        }

        if right_views.is_empty() {
            ui.painter().text(
                Pos2::new(header.left() + 14.0, header.center().y),
                Align2::LEFT_CENTER,
                "辅助面板",
                style.font(11.5),
                theme.text_dim,
            );
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("暂无已注册的右侧插件视图。\n安装并启用支持自定义面板的插件（如 AI Agent）后将在此显示。")
                            .font(style.small())
                            .color(theme.text_faint),
                    )
                    .wrap(),
                );
            });
            return;
        }

        let mut tab_x = header.left() + 10.0;
        let max_tab_x = close_rect.left() - 6.0;
        for (vid, title) in &right_views {
            let is_active = active_id.as_deref() == Some(vid.as_str());
            let w = (title.chars().count() as f32 * 9.5 + 18.0).clamp(54.0, 160.0);
            if tab_x + w > max_tab_x {
                break;
            }
            let r = Rect::from_min_size(Pos2::new(tab_x, header.top() + 5.0), Vec2::new(w, 26.0));
            let resp = ui.interact(
                r,
                ui.scope_id().with(("aux_view_tab", vid.as_str())),
                Sense::click(),
            );
            if is_active {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(5), theme.active);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(5), theme.hover);
            }
            ui.painter()
                .with_clip_rect(r.shrink2(Vec2::new(6.0, 0.0)))
                .text(
                    Pos2::new(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    title,
                    if is_active {
                        style.bold(11.5)
                    } else {
                        style.font(11.5)
                    },
                    if is_active { theme.text } else { theme.text_dim },
                );
            if resp.clicked() {
                self.aux_sidebar.active_view = Some(vid.clone());
            }
            tab_x += w + 4.0;
        }

        let Some(active_id) = active_id else {
            return;
        };

        let root_opt = self.workspace.root.clone();
        let (events, md_events) = {
            let Some(view) = self.plugins.view_mut(&active_id) else {
                return;
            };
            let nodes = view.nodes.clone();
            let mut pctx = PaintCtx {
                view_id: &active_id,
                style,
                workspace_root: root_opt.as_deref(),
                state: &mut view.state,
                events: Vec::new(),
                md_events: Vec::new(),
            };
            Frame::new()
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(ui.available_height());
                    paint_nodes(ui, &mut pctx, &nodes);
                });
            (pctx.events, pctx.md_events)
        };

        // Handle embedded markdown interactions (copy code, open links/files).
        for ev in md_events {
            match ev {
                PreviewEvent::CopyText(text) => {
                    ui.ctx().copy_text(text);
                    self.toast("已复制到剪贴板", ToastKind::Info);
                }
                PreviewEvent::OpenUrl(url) => {
                    let _ = crate::terminal::open_url(&url);
                }
                PreviewEvent::OpenFile { path, .. } => {
                    self.open_file(path);
                }
                _ => {}
            }
        }

        // Dispatch UI events back to the plugin cleanly outside the paint borrow.
        if !events.is_empty() {
            let ctx = self.plugin_context(self.tabs.get(self.active).map(|tab| tab.id));
            for event in events {
                self.plugins.dispatch_view_event(&event, &ctx);
            }
            self.apply_plugin_actions();
            for message in self.plugins.take_errors() {
                self.toast(message, ToastKind::Error);
            }
        }
    }
}
