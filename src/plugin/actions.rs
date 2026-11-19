//! Data visible to plugin callbacks, queued host actions, and their application.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use ropey::Rope;

use crate::app::{KestrelApp, ToastKind};
use crate::commands::Command;
use crate::editor::Editor;

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
        }
    }
}

/// Snapshot handed to one callback: the subject document plus the active document.
#[derive(Clone, Default)]
pub struct PluginContext {
    pub active: Option<u64>,
    pub subject: Option<u64>,
    pub docs: HashMap<u64, Rc<DocSnapshot>>,
    pub root: Option<PathBuf>,
}

impl PluginContext {
    pub fn from_app(app: &KestrelApp, subject: Option<u64>) -> Self {
        let mut docs = HashMap::new();
        let active = app.tabs.get(app.active).map(|tab| tab.id);
        for id in [active, subject].into_iter().flatten() {
            if docs.contains_key(&id) {
                continue;
            }
            if let Some(tab) = app.tabs.iter().find(|tab| tab.id == id) {
                docs.insert(id, Rc::new(DocSnapshot::from_editor(tab)));
            }
        }
        Self {
            active,
            subject,
            docs,
            root: app.workspace.root.clone(),
        }
    }

    pub fn with_closed(mut self, id: u64, path: Option<PathBuf>, language: String) -> Self {
        self.subject = Some(id);
        self.docs.insert(id, Rc::new(DocSnapshot::closed(id, path, language)));
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
    pub(crate) fn plugin_context(&self, subject: Option<u64>) -> PluginContext {
        PluginContext::from_app(self, subject)
    }

    /// Apply queued plugin actions. Built-in commands are deferred to the next frame.
    pub(crate) fn apply_plugin_actions(&mut self) {
        let actions = self.plugins.take_actions();
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
            if result.is_ok() {
                tab.preview = false;
                tab.diff_version = 0;
            }
            result
        });
        match result {
            Some(Ok(())) => {
                if let Some(root) = self.workspace.root.clone() {
                    self.git.refresh(&root);
                }
            }
            Some(Err(err)) => self.toast(format!("保存失败: {err}"), ToastKind::Error),
            None => {}
        }
    }

    /// Per-frame plugin work: async results, document events, deferred commands.
    pub(crate) fn plugin_tick(&mut self, egui_ctx: &egui::Context) {
        if self.plugins.is_empty() {
            return;
        }
        let commands = std::mem::take(&mut self.pending_plugin_commands);
        for command in commands {
            self.dispatch(command, egui_ctx);
        }
        if self.plugins.has_pending_exec() {
            let ctx = self.plugin_context(None);
            self.plugins.poll_exec(&ctx);
            self.apply_plugin_actions();
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
        self.plugins.emit_to(runtime, crate::plugin::Event::Startup, &ctx);
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
