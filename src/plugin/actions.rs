//! Data visible to plugin callbacks, queued host actions, and their application.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use ropey::Rope;

use crate::app::{KestrelApp, ToastKind};
use crate::commands::Command;
use crate::editor::Editor;
use crate::lsp::{Diagnostic, OutlineSymbol};

/// Document contents visible to one plugin callback. The `rope` field is an
/// O(1) clone; text is converted only when the script asks for it.
pub struct DocSnapshot {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub language: String,
    pub dirty: bool,
    pub caret: usize,
    pub anchor: usize,
    pub char_len: usize,
    pub line_count: usize,
    pub rope: Option<Rope>,
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: Vec<OutlineSymbol>,
}

impl DocSnapshot {
    pub fn from_editor(editor: &Editor) -> Self {
        Self {
            id: editor.id,
            path: editor.doc.path.clone(),
            language: editor.language.name().to_string(),
            dirty: editor.is_dirty(),
            caret: editor.caret,
            anchor: editor.anchor,
            char_len: editor.doc.len_chars(),
            line_count: editor.doc.line_count(),
            rope: Some(editor.doc.rope.clone()),
            diagnostics: editor.diagnostics.clone(),
            symbols: editor.symbols.clone(),
        }
    }

    /// Metadata for a document that was already closed; text access is disabled.
    pub fn closed(id: u64, path: Option<PathBuf>, language: String) -> Self {
        Self {
            id,
            path,
            language,
            dirty: false,
            caret: 0,
            anchor: 0,
            char_len: 0,
            line_count: 0,
            rope: None,
            diagnostics: Vec::new(),
            symbols: Vec::new(),
        }
    }
}

/// Snapshot of an integrated PTY terminal exposed to plugins with `terminal` permission.
#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub id: u64,
    pub title: String,
    pub cwd: PathBuf,
    pub alive: bool,
    pub tail: String,
}

/// Snapshot handed to one callback: all open text documents, active/subject IDs,
/// workspace root, and optional terminal snapshots.
#[derive(Clone, Default)]
pub struct PluginContext {
    pub active: Option<u64>,
    pub subject: Option<u64>,
    pub docs: HashMap<u64, Rc<DocSnapshot>>,
    pub open_order: Vec<u64>,
    pub root: Option<PathBuf>,
    pub terminals: Vec<TerminalSnapshot>,
}

impl PluginContext {
    pub fn from_app(app: &mut KestrelApp, subject: Option<u64>) -> Self {
        let mut docs = HashMap::new();
        let mut open_order = Vec::new();
        let active = app.tabs.get(app.active).map(|tab| tab.id);
        for tab in &app.tabs {
            if tab.diff.is_some() || tab.multimodal.is_some() {
                continue;
            }
            docs.insert(tab.id, Rc::new(DocSnapshot::from_editor(tab)));
            open_order.push(tab.id);
        }
        if let Some(sub) = subject
            && !docs.contains_key(&sub)
            && let Some(tab) = app.tabs.iter().find(|tab| tab.id == sub)
        {
            docs.insert(sub, Rc::new(DocSnapshot::from_editor(tab)));
        }
        let terminals = if app.plugins.any_has_permission("terminal") {
            app.bottom
                .terminals
                .iter_mut()
                .map(|term| TerminalSnapshot {
                    id: term.id,
                    title: term.title(),
                    cwd: term.cwd.clone(),
                    alive: term.is_alive(),
                    tail: term.tail_text(300),
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            active,
            subject,
            docs,
            open_order,
            root: app.workspace.root.clone(),
            terminals,
        }
    }

    pub fn with_closed(mut self, id: u64, path: Option<PathBuf>, language: String) -> Self {
        self.subject = Some(id);
        self.docs
            .insert(id, Rc::new(DocSnapshot::closed(id, path, language)));
        self
    }
}

/// A mutation requested by a plugin. Applied by the app after the Lua callback returns.
pub enum HostAction {
    Edit {
        doc: u64,
        start: usize,
        end: usize,
        text: String,
        caret_after: Option<usize>,
    },
    SetSelection {
        doc: u64,
        start: usize,
        end: usize,
    },
    SaveDoc(u64),
    OpenFile(PathBuf),
    EditOrWriteFile {
        path: PathBuf,
        text: String,
        save: bool,
    },
    ProposeEdit {
        proposal_id: u64,
        path: PathBuf,
        text: String,
        title: String,
    },
    OpenView(String),
    CloseView(String),
    TerminalRun {
        name: Option<String>,
        command: String,
        cwd: Option<PathBuf>,
        show: bool,
    },
    TerminalSend {
        name: Option<String>,
        text: String,
    },
    RefreshWorkspace,
    Notify {
        text: String,
        level: ToastKind,
    },
    RunBuiltin(Command),
    SetPluginConfig {
        plugin: String,
        key: String,
        value: serde_json::Value,
    },
}

impl KestrelApp {
    pub(crate) fn plugin_context(&mut self, subject: Option<u64>) -> PluginContext {
        PluginContext::from_app(self, subject)
    }

    /// Apply queued plugin actions. Built-in commands are deferred to the next frame.
    pub(crate) fn apply_plugin_actions(&mut self) {
        let actions = self.plugins.take_actions();
        let mut needs_refresh = false;
        for (runtime, action) in actions {
            if !self.plugins.is_runtime_loaded(runtime) {
                continue;
            }
            match action {
                HostAction::Edit {
                    doc,
                    start,
                    end,
                    text,
                    caret_after,
                } => {
                    let Some(index) = self.tabs.iter().position(|tab| tab.id == doc) else {
                        continue;
                    };
                    if self.tabs[index].diff.is_some() || self.tabs[index].multimodal.is_some() {
                        continue;
                    }
                    if !self.tabs[index].apply_plugin_edit(start, end, &text, caret_after) {
                        self.toast("文档为只读，插件编辑被忽略", ToastKind::Warning);
                    }
                }
                HostAction::SetSelection { doc, start, end } => {
                    let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == doc) else {
                        continue;
                    };
                    let len = tab.doc.len_chars();
                    let start = start.min(len);
                    let end = end.clamp(start, len);
                    tab.caret = end;
                    tab.anchor = start;
                    tab.ensure_visible = true;
                    tab.focus_requested = true;
                }
                HostAction::SaveDoc(doc) => {
                    if let Some(index) = self.tabs.iter().position(|tab| tab.id == doc) {
                        self.save_tab_raw(index);
                    }
                }
                HostAction::OpenFile(path) => self.open_file(path),
                HostAction::EditOrWriteFile { path, text, save } => {
                    self.apply_file_content(path, text, save);
                    needs_refresh = true;
                }
                HostAction::ProposeEdit {
                    proposal_id,
                    path,
                    text,
                    title,
                } => {
                    self.open_proposal_diff(runtime, proposal_id, path, text, title);
                }
                HostAction::OpenView(view_id) => {
                    self.aux_sidebar.active_view = Some(view_id);
                    self.aux_sidebar.visible = true;
                }
                HostAction::CloseView(view_id) => {
                    if self.aux_sidebar.active_view.as_deref() == Some(view_id.as_str()) {
                        self.aux_sidebar.visible = false;
                    }
                }
                HostAction::TerminalRun {
                    name,
                    command,
                    cwd,
                    show,
                } => {
                    self.plugin_terminal_run(name, command, cwd, show);
                }
                HostAction::TerminalSend { name, text } => {
                    self.plugin_terminal_send(name, text);
                }
                HostAction::RefreshWorkspace => {
                    needs_refresh = true;
                }
                HostAction::Notify { text, level } => self.toast(text, level),
                HostAction::RunBuiltin(command) => self.pending_plugin_commands.push(command),
                HostAction::SetPluginConfig { plugin, key, value } => {
                    self.settings
                        .plugin_config
                        .entry(plugin)
                        .or_default()
                        .insert(key, value);
                    self.settings.save();
                }
            }
        }
        if needs_refresh && let Some(root) = self.workspace.root.clone() {
            self.workspace.tree.reload();
            self.workspace.index.scan(&root);
            self.git.refresh(&root);
        }
    }

    /// Replace an open editor buffer (single undo step) or write directly to disk
    /// when no open tab matches `path`.
    pub(crate) fn apply_file_content(&mut self, path: PathBuf, text: String, save: bool) {
        let target_canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let open_idx = self.tabs.iter().position(|tab| {
            tab.diff.is_none()
                && tab.multimodal.is_none()
                && tab.doc.path.as_ref().is_some_and(|p| {
                    p == &path
                        || std::fs::canonicalize(p)
                            .map(|c| c == target_canon)
                            .unwrap_or(false)
                })
        });
        if let Some(index) = open_idx {
            let len = self.tabs[index].doc.len_chars();
            let caret = self.tabs[index].caret;
            if self.tabs[index].apply_plugin_edit(0, len, &text, Some(caret)) {
                if save {
                    self.save_tab_raw(index);
                }
            } else {
                self.toast("文档为只读，无法应用修改", ToastKind::Warning);
            }
        } else {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(err) = std::fs::write(&path, text) {
                self.toast(format!("写入文件失败: {err}"), ToastKind::Error);
            }
        }
    }

    /// Emit one plugin event and apply whatever it queued.
    pub(crate) fn emit_plugin_event(&mut self, event: crate::plugin::Event, ctx: &PluginContext) {
        if !self.plugins.has_handlers(event) {
            return;
        }
        self.plugins.emit(event, ctx);
        self.apply_plugin_actions();
    }

    pub(crate) fn emit_plugin_event_for(&mut self, event: crate::plugin::Event, doc_id: u64) {
        if !self.plugins.has_handlers(event) {
            return;
        }
        let ctx = self.plugin_context(Some(doc_id));
        self.plugins.emit(event, &ctx);
        self.apply_plugin_actions();
    }

    /// Save a tab without running plugin hooks; used by `doc:save()`.
    pub(crate) fn save_tab_raw(&mut self, index: usize) {
        let result = self.tabs.get_mut(index).map(|tab| {
            let result = tab.doc.save();
            if matches!(result, crate::doc::SaveOutcome::Saved) {
                tab.preview = false;
                tab.diff_version = 0;
            }
            result
        });
        match result {
            Some(crate::doc::SaveOutcome::Saved) => {
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
            }
            Some(crate::doc::SaveOutcome::DiskChanged) => {
                self.toast("保存失败: 文件已在磁盘上被修改", ToastKind::Warning)
            }
            Some(crate::doc::SaveOutcome::ReadOnly) => {
                self.toast("保存失败: 文件只读", ToastKind::Warning)
            }
            Some(crate::doc::SaveOutcome::Failed(err)) => {
                self.toast(format!("保存失败: {err}"), ToastKind::Error)
            }
            None => {}
        }
    }

    /// Per-frame plugin work: async results (exec/http/rpc/search/timers),
    /// document events, deferred commands.
    pub(crate) fn plugin_tick(&mut self, egui_ctx: &egui::Context) {
        if self.plugins.is_empty() {
            return;
        }
        let commands = std::mem::take(&mut self.pending_plugin_commands);
        for command in commands {
            self.dispatch(command, egui_ctx);
        }
        if self.plugins.has_pending_async() {
            let ctx = self.plugin_context(None);
            let handled = self.plugins.poll_async(&ctx);
            if handled {
                self.apply_plugin_actions();
            }
        }
        if self.plugins.wants_document_events() {
            let tracks: Vec<crate::plugin::DocTrack> = self
                .tabs
                .iter()
                .map(|tab| crate::plugin::DocTrack {
                    id: tab.id,
                    path: tab.doc.path.clone(),
                    language: tab.language.name().to_string(),
                    version: tab.doc.version,
                    idle_ms: tab.last_edit.elapsed().as_millis() as u64,
                })
                .collect();
            let active = self.tabs.get(self.active).map(|tab| tab.id);
            for event in self.plugins.plan_doc_events(&tracks, active) {
                match event {
                    crate::plugin::DocEvent::Opened(id) => {
                        let ctx = self.plugin_context(Some(id));
                        self.emit_plugin_event(crate::plugin::Event::Opened, &ctx);
                    }
                    crate::plugin::DocEvent::Changed(id) => {
                        let ctx = self.plugin_context(Some(id));
                        self.emit_plugin_event(crate::plugin::Event::Changed, &ctx);
                    }
                    crate::plugin::DocEvent::Closed(meta) => {
                        let ctx = self
                            .plugin_context(None)
                            .with_closed(meta.id, meta.path, meta.language);
                        self.emit_plugin_event(crate::plugin::Event::Closed, &ctx);
                    }
                }
            }
        }
        for message in self.plugins.take_errors() {
            self.toast(message, ToastKind::Error);
        }
    }

    /// Run a plugin command from the palette, a keybinding, or a status item.
    pub(crate) fn run_plugin_command(&mut self, index: usize) {
        let ctx = self.plugin_context(self.tabs.get(self.active).map(|tab| tab.id));
        self.plugins.run_command(index, &ctx);
        self.apply_plugin_actions();
        for message in self.plugins.take_errors() {
            self.toast(message, ToastKind::Error);
        }
    }

    pub(crate) fn load_plugins(&mut self) {
        if self.plugins.is_empty() {
            return;
        }
        let configs = self.settings.plugin_config.clone();
        for runtime in self.plugins.load_enabled(&configs) {
            self.startup_plugin(runtime);
        }
    }

    /// Run the `startup` hook for one freshly loaded plugin.
    pub(crate) fn startup_plugin(&mut self, runtime: u64) {
        if !self.plugins.has_handlers(crate::plugin::Event::Startup) {
            return;
        }
        let ctx = self.plugin_context(None);
        self.plugins
            .emit_to(runtime, crate::plugin::Event::Startup, &ctx);
        self.apply_plugin_actions();
    }

    pub(crate) fn store_plugin_disabled(&mut self, id: &str, disabled: bool) {
        self.settings.disabled_plugins.retain(|entry| entry != id);
        if disabled {
            self.settings.disabled_plugins.push(id.to_string());
        }
        self.settings.save();
    }
}
