//! Plugin host: discovery, per-plugin Lua runtimes, commands, events, hot reload.

pub mod actions;
mod api;
mod manifest;
mod panel;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use mlua::{Function, HookTriggers, Lua, LuaOptions, RegistryKey, StdLib, VmState};

use crate::commands::{Command, Shortcut};
use crate::settings::Settings;

use actions::{HostAction, PluginContext};

pub const INSTRUCTION_BUDGET: u64 = 5_000_000;

fn libraries() -> StdLib {
    StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    Startup,
    Opened,
    BeforeSave,
    Saved,
    Closed,
    Changed,
    Shutdown,
}

impl Event {
    pub fn name(self) -> &'static str {
        match self {
            Event::Startup => "startup",
            Event::Opened => "opened",
            Event::BeforeSave => "before_save",
            Event::Saved => "saved",
            Event::Closed => "closed",
            Event::Changed => "changed",
            Event::Shutdown => "shutdown",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "startup" => Event::Startup,
            "opened" => Event::Opened,
            "before_save" => Event::BeforeSave,
            "saved" => Event::Saved,
            "closed" => Event::Closed,
            "changed" => Event::Changed,
            "shutdown" => Event::Shutdown,
            _ => return None,
        })
    }
}

/// Document metadata handed to the host once per frame for event detection.
pub struct DocTrack {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub language: String,
    pub version: u64,
    pub idle_ms: u64,
}

pub struct ClosedMeta {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub language: String,
}

pub enum DocEvent {
    Opened(u64),
    Closed(ClosedMeta),
    Changed(u64),
}

struct TrackedDoc {
    path: Option<PathBuf>,
    language: String,
    version: u64,
}

pub struct StatusItem {
    pub plugin: u64,
    pub key: String,
    pub text: String,
    pub command: Option<String>,
}

pub struct PluginCommand {
    pub plugin: usize,
    pub id: String,
    pub title: String,
    pub category: String,
    pub shortcut: Option<Shortcut>,
    handler: RegistryKey,
}

impl PluginCommand {
    pub fn shortcut_text(&self) -> Option<String> {
        self.shortcut.map(|shortcut| shortcut.display())
    }
}

pub(crate) struct PendingCommand {
    pub id: String,
    pub title: String,
    pub shortcut: Option<Shortcut>,
    pub handler: RegistryKey,
}

pub(crate) struct ExecDone {
    pub runtime: u64,
    pub task: u64,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

pub(crate) struct PluginShared {
    pub plugin_id: String,
    pub runtime: u64,
    pub changed_allowed: bool,
    pub ctx: Option<PluginContext>,
    pub actions: Vec<HostAction>,
    pub commands: Vec<PendingCommand>,
    pub events: Vec<(Event, RegistryKey)>,
    pub status: Vec<(String, String, Option<String>)>,
    pub logs: Vec<String>,
    pub config: HashMap<String, serde_json::Value>,
    pub exec_callbacks: Vec<(u64, RegistryKey)>,
    pub next_task: u64,
    pub exec_tx: Sender<ExecDone>,
}

impl PluginShared {
    fn new(
        plugin_id: String,
        runtime: u64,
        changed_allowed: bool,
        config: HashMap<String, serde_json::Value>,
        exec_tx: Sender<ExecDone>,
    ) -> Self {
        Self {
            plugin_id,
            runtime,
            changed_allowed,
            ctx: None,
            actions: Vec::new(),
            commands: Vec::new(),
            events: Vec::new(),
            status: Vec::new(),
            logs: Vec::new(),
            config,
            exec_callbacks: Vec::new(),
            next_task: 0,
            exec_tx,
        }
    }
}

pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub loaded: bool,
    pub error: Option<String>,
    pub memory: usize,
}

pub struct Plugin {
    pub runtime: u64,
    pub dir: PathBuf,
    pub manifest: manifest::Manifest,
    pub enabled: bool,
    pub loaded: bool,
    pub error: Option<String>,
    lua: Option<Lua>,
    shared: Option<Rc<RefCell<PluginShared>>>,
    handlers: HashMap<Event, Vec<RegistryKey>>,
    budget: Rc<Cell<u64>>,
    pending_exec: HashMap<u64, RegistryKey>,
}

impl Plugin {
    fn enter(&self, ctx: Option<&PluginContext>) {
        self.budget.set(0);
        if let Some(shared) = &self.shared {
            shared.borrow_mut().ctx = ctx.cloned();
        }
    }

    fn leave(&self) {
        if let Some(shared) = &self.shared {
            shared.borrow_mut().ctx = None;
        }
    }

    fn dispatch(&self, event: Event, ctx: &PluginContext) -> mlua::Result<()> {
        let (Some(lua), Some(shared)) = (self.lua.as_ref(), self.shared.as_ref()) else {
            return Ok(());
        };
        self.enter(Some(ctx));
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("name", event.name())?;
            if let Some(subject) = ctx.subject
                && let Some(snapshot) = ctx.docs.get(&subject) {
                    let handle = api::DocHandle::new(snapshot.clone(), shared.clone());
                    table.set("doc", lua.create_userdata(handle)?)?;
                }
            let keys: Vec<&RegistryKey> = self
                .handlers
                .get(&event)
                .map(|keys| keys.iter().collect())
                .unwrap_or_default();
            for key in keys {
                let function: Function = lua.registry_value(key)?;
                function.call::<()>(table.clone())?;
            }
            Ok(())
        })();
        self.leave();
        result
    }

    fn call_handler(&self, key: &RegistryKey, ctx: Option<&PluginContext>) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(ctx);
        let result = (|| -> mlua::Result<()> {
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(())
        })();
        self.leave();
        result
    }

    fn call_exec(
        &self,
        key: &RegistryKey,
        done: &ExecDone,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx));
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("code", done.code)?;
            table.set("stdout", done.stdout.as_str())?;
            table.set("stderr", done.stderr.as_str())?;
            table.set("error", done.error.as_deref())?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }
}

pub struct PluginHost {
    root: PathBuf,
    plugins: Vec<Plugin>,
    commands: Vec<PluginCommand>,
    status_items: Vec<StatusItem>,
    logs: Vec<String>,
    actions: Vec<(u64, HostAction)>,
    open_docs: HashMap<u64, TrackedDoc>,
    exec_tx: Sender<ExecDone>,
    exec_rx: Receiver<ExecDone>,
    next_runtime: u64,
    load_errors: Vec<String>,
    errors: Vec<String>,
    trace: bool,
}

impl PluginHost {
    pub fn new(settings: &Settings) -> Self {
        let root = std::env::var_os("KESTREL_PLUGINS")
            .map(PathBuf::from)
            .unwrap_or_else(|| Settings::config_dir().join("plugins"));
        Self::with_root(root, settings)
    }

    pub fn with_root(root: PathBuf, settings: &Settings) -> Self {
        let _ = std::fs::create_dir_all(&root);
        let (exec_tx, exec_rx) = channel();
        let mut host = Self {
            root,
            plugins: Vec::new(),
            commands: Vec::new(),
            status_items: Vec::new(),
            logs: Vec::new(),
            actions: Vec::new(),
            open_docs: HashMap::new(),
            exec_tx,
            exec_rx,
            next_runtime: 1,
            load_errors: Vec::new(),
            errors: Vec::new(),
            trace: std::env::var_os("KESTREL_TRACE").is_some(),
        };
        host.scan(settings);
        host
    }

    fn scan(&mut self, settings: &Settings) {
        let (found, errors) = manifest::discover(&self.root);
        self.load_errors = errors;
        for discovered in found {
            let mut manifest = discovered.manifest.unwrap_or_default();
            if manifest.api_version == 0 {
                manifest.api_version = manifest::API_VERSION;
            }
            let error = discovered.error;
            let enabled = error.is_none()
                && !settings
                    .disabled_plugins
                    .iter()
                    .any(|id| id == &manifest.id);
            self.plugins.push(Plugin {
                runtime: 0,
                dir: discovered.dir,
                manifest,
                enabled,
                loaded: false,
                error,
                lua: None,
                shared: None,
                handlers: HashMap::new(),
                budget: Rc::new(Cell::new(0)),
                pending_exec: HashMap::new(),
            });
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    pub fn load_enabled(&mut self, configs: &HashMap<String, HashMap<String, serde_json::Value>>) -> Vec<u64> {
        let indices: Vec<usize> = self
            .plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.enabled && !plugin.loaded)
            .map(|(index, _)| index)
            .collect();
        let mut loaded = Vec::new();
        for index in indices {
            let id = self.plugins[index].manifest.id.clone();
            let config = configs.get(&id).cloned().unwrap_or_default();
            if let Ok(runtime) = self.load_runtime(index, config) {
                loaded.push(runtime);
            }
        }
        loaded
    }

    fn load_runtime(&mut self, index: usize, config: HashMap<String, serde_json::Value>) -> Result<u64, String> {
        let started = std::time::Instant::now();
        self.unload(index);
        self.plugins[index].enabled = true;
        let manifest = self.plugins[index].manifest.clone();
        if manifest.api_version > manifest::API_VERSION {
            let err = format!(
                "apiVersion {} 高于宿主支持的 {}",
                manifest.api_version,
                manifest::API_VERSION
            );
            self.plugins[index].error = Some(err.clone());
            self.errors.push(format!("[{}] {err}", manifest.id));
            return Err(err);
        }
        let dir = self.plugins[index].dir.clone();
        let runtime = self.next_runtime;
        self.next_runtime += 1;
        let shared = Rc::new(RefCell::new(PluginShared::new(
            manifest.id.clone(),
            runtime,
            manifest.events.iter().any(|event| event == "changed"),
            config,
            self.exec_tx.clone(),
        )));
        let budget = Rc::new(Cell::new(0u64));
        let result = (|| -> Result<Option<Lua>, String> {
            let Some(main) = manifest.main.as_deref() else {
                return Ok(None);
            };
            let source = std::fs::read_to_string(dir.join(main))
                .map_err(|err| format!("读取入口 {main} 失败: {err}"))?;
            let lua = Lua::new_with(libraries(), LuaOptions::default())
                .map_err(|err| err.to_string())?;
            api::install(&lua, &shared).map_err(|err| err.to_string())?;
            install_budget(&lua, &budget).map_err(|err| err.to_string())?;
            lua.load(&source)
                .set_name(format!("@{}/{}", manifest.id, main))
                .exec()
                .map_err(|err| err.to_string())?;
            Ok(Some(lua))
        })();
        match result {
            Ok(lua) => {
                self.plugins[index].runtime = runtime;
                self.plugins[index].shared = Some(shared);
                self.plugins[index].budget = budget;
                self.plugins[index].lua = lua;
                self.plugins[index].loaded = true;
                self.plugins[index].error = None;
                self.drain_plugin(index);
                if self.trace {
                    let memory = self.plugins[index]
                        .lua
                        .as_ref()
                        .map(|lua| lua.used_memory())
                        .unwrap_or(0);
                    self.trace_log(format!(
                        "加载插件 {} {:.2}ms, Lua 内存 {}KB",
                        manifest.id,
                        started.elapsed().as_secs_f64() * 1000.0,
                        memory / 1024
                    ));
                }
                Ok(runtime)
            }
            Err(err) => {
                self.plugins[index].error = Some(err.clone());
                self.plugins[index].lua = None;
                self.plugins[index].shared = None;
                self.plugins[index].loaded = false;
                self.errors
                    .push(format!("[{}] 加载失败: {err}", manifest.id));
                Err(err)
            }
        }
    }

    pub fn enable(
        &mut self,
        index: usize,
        config: HashMap<String, serde_json::Value>,
    ) -> Result<u64, String> {
        if self.plugins[index].loaded {
            return Ok(self.plugins[index].runtime);
        }
        self.load_runtime(index, config)
    }

    pub fn disable(&mut self, index: usize) {
        self.unload(index);
        if let Some(plugin) = self.plugins.get_mut(index) {
            plugin.enabled = false;
        }
    }

    pub fn reload(
        &mut self,
        index: usize,
        config: HashMap<String, serde_json::Value>,
    ) -> Result<u64, String> {
        self.load_runtime(index, config)
    }

    pub fn rescan(&mut self, settings: &Settings) {
        for index in 0..self.plugins.len() {
            self.unload(index);
        }
        self.plugins.clear();
        self.commands.clear();
        self.status_items.clear();
        self.actions.clear();
        self.open_docs.clear();
        self.scan(settings);
    }

    fn unload(&mut self, index: usize) {
        let runtime = self.plugins[index].runtime;
        self.commands.retain(|command| command.plugin != index);
        self.status_items.retain(|item| item.plugin != runtime);
        self.actions.retain(|(plugin, _)| *plugin != runtime);
        if let Some(shared) = self.plugins[index].shared.take() {
            let mut shared = shared.borrow_mut();
            shared.ctx = None;
            shared.actions.clear();
            shared.commands.clear();
            shared.events.clear();
            shared.status.clear();
            shared.exec_callbacks.clear();
        }
        self.plugins[index].handlers.clear();
        self.plugins[index].pending_exec.clear();
        self.plugins[index].lua = None;
        self.plugins[index].loaded = false;
    }

    fn drain_plugin(&mut self, index: usize) {
        let Some(shared) = self.plugins[index].shared.clone() else {
            return;
        };
        let runtime = self.plugins[index].runtime;
        let (commands, events, status, logs, actions, execs) = {
            let mut shared = shared.borrow_mut();
            (
                std::mem::take(&mut shared.commands),
                std::mem::take(&mut shared.events),
                std::mem::take(&mut shared.status),
                std::mem::take(&mut shared.logs),
                std::mem::take(&mut shared.actions),
                std::mem::take(&mut shared.exec_callbacks),
            )
        };
        for pending in commands {
            let plugin_name = self.plugins[index].manifest.name.clone();
            let duplicate = self.commands.iter().any(|command| command.id == pending.id)
                || Command::from_id(&pending.id).is_some();
            if duplicate {
                self.record_message(index, format!("命令 id 重复，已忽略: {}", pending.id));
                continue;
            }
            self.commands.push(PluginCommand {
                plugin: index,
                id: pending.id,
                title: pending.title,
                category: plugin_name,
                shortcut: pending.shortcut,
                handler: pending.handler,
            });
        }
        for (event, key) in events {
            self.plugins[index].handlers.entry(event).or_default().push(key);
        }
        for (key, text, command) in status {
            if let Some(existing) = self
                .status_items
                .iter_mut()
                .find(|item| item.plugin == runtime && item.key == key)
            {
                existing.text = text;
                existing.command = command;
            } else {
                self.status_items.push(StatusItem {
                    plugin: runtime,
                    key,
                    text,
                    command,
                });
            }
        }
        let plugin_id = self.plugins[index].manifest.id.clone();
        for line in logs {
            self.logs.push(format!("[{plugin_id}] {line}"));
        }
        trim_logs(&mut self.logs);
        for action in actions {
            self.actions.push((runtime, action));
        }
        for (task, key) in execs {
            self.plugins[index].pending_exec.insert(task, key);
        }
    }

    fn record_error(&mut self, index: usize, err: mlua::Error) {
        self.record_message(index, err.to_string());
    }

    fn record_message(&mut self, index: usize, message: String) {
        let id = self
            .plugins
            .get(index)
            .map(|plugin| plugin.manifest.id.clone())
            .unwrap_or_default();
        let line = format!("[{id}] {message}");
        if let Some(plugin) = self.plugins.get_mut(index) {
            plugin.error = Some(message);
        }
        self.logs.push(line.clone());
        trim_logs(&mut self.logs);
        self.errors.push(line);
    }

    pub fn has_handlers(&self, event: Event) -> bool {
        self.plugins.iter().any(|plugin| {
            plugin.loaded
                && plugin.lua.is_some()
                && plugin
                    .handlers
                    .get(&event)
                    .is_some_and(|keys| !keys.is_empty())
        })
    }

    pub fn wants_document_events(&self) -> bool {
        self.has_handlers(Event::Opened)
            || self.has_handlers(Event::Changed)
            || self.has_handlers(Event::Closed)
    }

    pub fn emit(&mut self, event: Event, ctx: &PluginContext) {
        let mut errors = Vec::new();
        for index in 0..self.plugins.len() {
            let plugin = &self.plugins[index];
            if plugin.lua.is_none()
                || plugin
                    .handlers
                    .get(&event)
                    .is_none_or(|keys| keys.is_empty())
            {
                continue;
            }
            if let Err(err) = plugin.dispatch(event, ctx) {
                errors.push((index, err));
            }
            self.drain_plugin(index);
        }
        for (index, err) in errors {
            self.record_error(index, err);
        }
    }

    pub fn emit_to(&mut self, runtime: u64, event: Event, ctx: &PluginContext) {
        let Some(index) = self
            .plugins
            .iter()
            .position(|plugin| plugin.loaded && plugin.runtime == runtime)
        else {
            return;
        };
        if let Err(err) = self.plugins[index].dispatch(event, ctx) {
            self.record_error(index, err);
        }
        self.drain_plugin(index);
    }

    pub fn plan_doc_events(&mut self, tracks: &[DocTrack], active: Option<u64>) -> Vec<DocEvent> {
        let mut events = Vec::new();
        let current: std::collections::HashSet<u64> = tracks.iter().map(|track| track.id).collect();
        let closed: Vec<u64> = self
            .open_docs
            .keys()
            .filter(|id| !current.contains(id))
            .copied()
            .collect();
        for id in closed {
            if let Some(tracked) = self.open_docs.remove(&id) {
                events.push(DocEvent::Closed(ClosedMeta {
                    id,
                    path: tracked.path,
                    language: tracked.language,
                }));
            }
        }
        for track in tracks {
            match self.open_docs.get_mut(&track.id) {
                None => {
                    self.open_docs.insert(
                        track.id,
                        TrackedDoc {
                            path: track.path.clone(),
                            language: track.language.clone(),
                            version: track.version,
                        },
                    );
                    events.push(DocEvent::Opened(track.id));
                }
                Some(tracked) => {
                    let changed = tracked.version != track.version;
                    tracked.path = track.path.clone();
                    tracked.language = track.language.clone();
                    if changed {
                        tracked.version = track.version;
                        if active == Some(track.id) && track.idle_ms >= 500 {
                            events.push(DocEvent::Changed(track.id));
                        }
                    }
                }
            }
        }
        events
    }

    pub fn poll_exec(&mut self, ctx: &PluginContext) {
        let mut done = Vec::new();
        while let Ok(item) = self.exec_rx.try_recv() {
            done.push(item);
        }
        for item in done {
            let Some(index) = self
                .plugins
                .iter()
                .position(|plugin| plugin.loaded && plugin.runtime == item.runtime)
            else {
                continue;
            };
            let Some(key) = self.plugins[index].pending_exec.remove(&item.task) else {
                continue;
            };
            if let Err(err) = self.plugins[index].call_exec(&key, &item, ctx) {
                self.record_error(index, err);
            }
            self.drain_plugin(index);
        }
    }

    pub fn has_pending_exec(&self) -> bool {
        self.plugins
            .iter()
            .any(|plugin| !plugin.pending_exec.is_empty())
    }

    pub fn run_command(&mut self, index: usize, ctx: &PluginContext) -> bool {
        let started = std::time::Instant::now();
        let Some(command) = self.commands.get(index) else {
            return false;
        };
        let plugin_index = command.plugin;
        let command_id = command.id.clone();
        let Some(plugin) = self.plugins.get(plugin_index) else {
            return false;
        };
        if !plugin.loaded || plugin.lua.is_none() {
            return false;
        }
        if let Err(err) = plugin.call_handler(&command.handler, Some(ctx)) {
            self.record_error(plugin_index, err);
        }
        self.drain_plugin(plugin_index);
        if self.trace {
            self.trace_log(format!(
                "命令 {command_id} {:.2}ms",
                started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        true
    }

    pub fn trace_log(&mut self, line: String) {
        self.logs.push(format!("[trace] {line}"));
        trim_logs(&mut self.logs);
    }

    pub fn commands(&self) -> &[PluginCommand] {
        &self.commands
    }

    pub fn command_index_of_id(&self, id: &str) -> Option<usize> {
        self.commands.iter().position(|command| command.id == id)
    }

    pub fn command_category_of(&self, id: &str) -> Option<&str> {
        self.commands
            .iter()
            .find(|command| command.id == id)
            .map(|command| command.category.as_str())
    }

    pub fn shortcut_candidates(&self) -> Vec<(usize, Shortcut)> {
        self.commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| command.shortcut.map(|shortcut| (index, shortcut)))
            .collect()
    }

    pub fn status_items(&self) -> &[StatusItem] {
        &self.status_items
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    pub fn take_actions(&mut self) -> Vec<(u64, HostAction)> {
        std::mem::take(&mut self.actions)
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    pub fn is_runtime_loaded(&self, runtime: u64) -> bool {
        self.plugins
            .iter()
            .any(|plugin| plugin.loaded && plugin.runtime == runtime)
    }

    pub fn plugin_infos(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|plugin| PluginInfo {
                id: plugin.manifest.id.clone(),
                name: plugin.manifest.name.clone(),
                version: plugin.manifest.version.clone(),
                description: plugin.manifest.description.clone(),
                enabled: plugin.enabled,
                loaded: plugin.loaded,
                error: plugin.error.clone(),
                memory: plugin
                    .lua
                    .as_ref()
                    .map(|lua| lua.used_memory())
                    .unwrap_or(0),
            })
            .collect()
    }
}

fn trim_logs(logs: &mut Vec<String>) {
    const MAX_LOGS: usize = 500;
    if logs.len() > MAX_LOGS {
        let excess = logs.len() - MAX_LOGS;
        logs.drain(0..excess);
    }
}

fn install_budget(lua: &Lua, budget: &Rc<Cell<u64>>) -> mlua::Result<()> {
    let budget = budget.clone();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(10_000),
        move |_lua, _debug| {
            let used = budget.get().saturating_add(10_000);
            budget.set(used);
            if used > INSTRUCTION_BUDGET {
                Err(mlua::Error::runtime("插件指令预算超限，已中断"))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;
    use crate::editor::Editor;
    use crate::plugin::actions::DocSnapshot;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kestrel-plugin-test-{}-{}",
                std::process::id(),
                TEST_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_plugin(root: &Path, id: &str, main: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"0.1.0","apiVersion":1,"main":"main.lua"}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("main.lua"), main).unwrap();
    }

    fn load(root: &Path) -> PluginHost {
        let mut host = PluginHost::with_root(root.to_path_buf(), &Settings::default());
        host.load_enabled(&HashMap::new());
        host
    }

    #[test]
    fn registers_commands_and_applies_host_actions() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "demo",
            r#"
kestrel.command("demo.ping", "Ping", function()
  kestrel.status_item("ping", "PONG", "demo.ping")
  kestrel.notify("pong")
end)
"#,
        );
        let mut host = load(root.path());
        assert_eq!(host.commands().len(), 1);
        assert_eq!(host.commands()[0].id, "demo.ping");
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        assert_eq!(host.status_items().len(), 1);
        assert_eq!(host.status_items()[0].text, "PONG");
        let actions = host.take_actions();
        assert!(actions.iter().any(|(_, action)| {
            matches!(action, HostAction::Notify { text, .. } if text == "pong")
        }));
        assert!(host.take_errors().is_empty());
    }

    #[test]
    fn disable_and_enable_are_hot() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "demo",
            r#"
kestrel.command("demo.ping", "Ping", function()
  kestrel.status_item("ping", "PONG", "demo.ping")
end)
"#,
        );
        let mut host = load(root.path());
        assert_eq!(host.commands().len(), 1);
        host.disable(0);
        assert!(host.commands().is_empty());
        assert!(host.status_items().is_empty());
        assert!(!host.plugin_infos()[0].enabled);
        let runtime = host.enable(0, HashMap::new()).unwrap();
        assert!(host.is_runtime_loaded(runtime));
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn unknown_event_fails_load_without_panicking() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "bad",
            r#"kestrel.on("hover", function() end)"#,
        );
        let host = load(root.path());
        assert!(host.commands().is_empty());
        let info = &host.plugin_infos()[0];
        assert!(info.error.as_deref().unwrap_or_default().contains("未知事件"));
    }

    #[test]
    fn sandbox_hides_io_os_and_package() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "sandbox",
            r#"
assert(type(io) == "nil", "io leaked")
assert(type(os) == "nil", "os leaked")
assert(type(package) == "nil", "package leaked")
kestrel.command("sandbox.ok", "ok", function() end)
"#,
        );
        let host = load(root.path());
        assert_eq!(host.commands().len(), 1);
        assert!(host.plugin_infos()[0].error.is_none());
    }

    #[test]
    fn instruction_budget_stops_runaway_plugin() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "spin",
            r#"kestrel.command("spin.run", "spin", function() while true do end end)"#,
        );
        let mut host = load(root.path());
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        let errors = host.take_errors();
        assert!(errors.iter().any(|line| line.contains("预算")), "{errors:?}");
    }

    #[test]
    fn document_handle_reads_snapshot_and_queues_edits() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "doc",
            r#"
kestrel.command("doc.run", "doc", function()
  local doc = kestrel.active_doc()
  kestrel.log(doc:text())
  doc:replace(0, 5, "HELLO")
end)
"#,
        );
        let mut host = load(root.path());
        let mut editor = Editor::new(Document::from_text("hello world"), 7, 0);
        editor.caret = 0;
        editor.anchor = 5;
        let mut ctx = PluginContext::default();
        ctx.active = Some(7);
        ctx.subject = Some(7);
        ctx.docs
            .insert(7, Rc::new(DocSnapshot::from_editor(&editor)));
        assert!(host.run_command(0, &ctx));
        assert!(host.logs().iter().any(|line| line.contains("hello world")));
        let actions = host.take_actions();
        assert!(actions.iter().any(|(_, action)| matches!(
            action,
            HostAction::Edit { doc, start, end, text, .. }
                if *doc == 7 && *start == 0 && *end == 5 && text == "HELLO"
        )));
    }

    #[test]
    fn disabled_plugins_are_not_loaded() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "off",
            r#"kestrel.command("off.run", "off", function() end)"#,
        );
        let mut settings = Settings::default();
        settings.disabled_plugins.push("off".to_string());
        let mut host = PluginHost::with_root(root.path().to_path_buf(), &settings);
        assert!(host.load_enabled(&HashMap::new()).is_empty());
        assert!(host.commands().is_empty());
        let info = &host.plugin_infos()[0];
        assert!(!info.enabled);
    }

    #[test]
    fn events_reach_plugins_and_can_edit_documents() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "events",
            r#"
kestrel.on("saved", function(ev)
  kestrel.log("saved " .. (ev.doc:path() or "none"))
end)
kestrel.on("before_save", function(ev)
  ev.doc:set_text("formatted")
end)
"#,
        );
        let mut host = load(root.path());
        assert!(host.has_handlers(Event::Saved));
        assert!(host.has_handlers(Event::BeforeSave));
        let mut editor = Editor::new(Document::from_text("raw"), 3, 0);
        editor.caret = 1;
        editor.anchor = 1;
        let mut ctx = PluginContext::default();
        ctx.subject = Some(3);
        ctx.docs
            .insert(3, Rc::new(DocSnapshot::from_editor(&editor)));
        host.emit(Event::Saved, &ctx);
        assert!(host.logs().iter().any(|line| line.contains("saved none")));
        host.emit(Event::BeforeSave, &ctx);
        assert!(host.take_actions().iter().any(|(_, action)| matches!(
            action,
            HostAction::Edit { doc, start, end, text, .. }
                if *doc == 3 && *start == 0 && *end == 3 && text == "formatted"
        )));
    }

    #[test]
    fn twenty_plugins_load_fast_and_small() {
        let root = TempRoot::new();
        for index in 0..20 {
            write_plugin(
                root.path(),
                &format!("p{index}"),
                r#"
kestrel.command("p.ping", "Ping", function()
  kestrel.status_item("ping", "PONG")
end)
"#,
            );
        }
        let started = std::time::Instant::now();
        let mut host = PluginHost::with_root(root.path().to_path_buf(), &Settings::default());
        let runtimes = host.load_enabled(&HashMap::new());
        let elapsed = started.elapsed();
        assert_eq!(runtimes.len(), 20);
        let memory: usize = host.plugin_infos().iter().map(|info| info.memory).sum();
        println!("20 plugins: {elapsed:?}, {} KB", memory / 1024);
        assert!(elapsed < std::time::Duration::from_millis(200), "{elapsed:?}");
        assert!(memory < 3 * 1024 * 1024);
    }

    #[test]
    fn manifest_api_version_is_validated() {
        let root = TempRoot::new();
        let dir = root.path().join("future");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"id":"future","name":"future","version":"1.0.0","apiVersion":2}"#,
        )
        .unwrap();
        let host = load(root.path());
        assert!(host.commands().is_empty());
        let info = &host.plugin_infos()[0];
        assert!(!info.enabled);
        assert!(info.error.as_deref().unwrap_or_default().contains("apiVersion"));
    }
}
