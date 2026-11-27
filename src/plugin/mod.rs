//! Plugin host: discovery, per-plugin Lua runtimes, commands, events, streaming
//! HTTP/exec/RPC, declarative UI views, edit proposals, timers, and hot reload.

pub mod actions;
mod api;
mod http;
mod manifest;
mod panel;
mod rpc;
mod secret;
pub mod view;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, LuaOptions, RegistryKey, StdLib, VmState};

use crate::commands::{Command, Shortcut};
use crate::settings::Settings;

use actions::{HostAction, PluginContext};
use http::HttpEvent;
use rpc::{RpcControl, RpcEvent};
use view::{PluginView, UiNode, ViewEvent, ViewLocation};

pub const INSTRUCTION_BUDGET: u64 = 5_000_000;
/// Concurrent `kestrel.exec` processes across all plugins; extra calls are rejected.
pub(crate) const MAX_CONCURRENT_EXEC: usize = 4;

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

pub(crate) enum ExecEvent {
    Stdout {
        runtime: u64,
        task: u64,
        chunk: String,
    },
    Stderr {
        runtime: u64,
        task: u64,
        chunk: String,
    },
    Done {
        runtime: u64,
        task: u64,
        code: i32,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
}

pub(crate) struct PendingExecCallbacks {
    pub on_stdout: Option<RegistryKey>,
    pub on_stderr: Option<RegistryKey>,
    pub on_done: Option<RegistryKey>,
    pub cancel: Arc<AtomicBool>,
}

pub(crate) struct PendingHttpCallbacks {
    pub on_chunk: Option<RegistryKey>,
    pub on_done: RegistryKey,
    pub cancel: Arc<AtomicBool>,
}

pub(crate) struct PendingRpcCallbacks {
    pub on_message: Option<RegistryKey>,
    pub on_stderr: Option<RegistryKey>,
    pub on_exit: Option<RegistryKey>,
    pub control: RpcControl,
}

pub(crate) struct SearchHitItem {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

pub(crate) struct SearchDone {
    pub runtime: u64,
    pub task: u64,
    pub results: Vec<SearchHitItem>,
    pub truncated: bool,
    pub error: Option<String>,
}

pub(crate) struct PendingSearchCallbacks {
    pub on_done: RegistryKey,
}

pub(crate) struct PendingTimer {
    pub deadline: Instant,
    pub callback: RegistryKey,
    pub cancelled: Arc<AtomicBool>,
}

pub(crate) struct PendingProposal {
    pub id: u64,
    pub path: PathBuf,
    pub on_accept: Option<RegistryKey>,
    pub on_reject: Option<RegistryKey>,
}

pub(crate) struct PendingViewReg {
    pub id: String,
    pub title: String,
    pub location: ViewLocation,
    pub on_event: Option<RegistryKey>,
}

pub(crate) struct PendingViewUpdate {
    pub id: String,
    pub nodes: Vec<UiNode>,
}

pub(crate) struct PluginShared {
    pub plugin_id: String,
    pub plugin_dir: PathBuf,
    pub runtime: u64,
    pub changed_allowed: bool,
    pub permissions: Option<Vec<String>>,
    pub in_before_save: bool,
    pub ctx: Option<PluginContext>,
    pub repaint_ctx: Option<egui::Context>,
    pub actions: Vec<HostAction>,
    pub commands: Vec<PendingCommand>,
    pub events: Vec<(Event, RegistryKey)>,
    pub status: Vec<(String, String, Option<String>)>,
    pub logs: Vec<String>,
    pub config: HashMap<String, serde_json::Value>,
    pub next_task: u64,
    pub exec_tx: Sender<ExecEvent>,
    pub exec_running: Arc<AtomicUsize>,
    pub exec_callbacks: Vec<(u64, PendingExecCallbacks)>,
    pub http_tx: Sender<HttpEvent>,
    pub http_running: Arc<AtomicUsize>,
    pub http_callbacks: Vec<(u64, PendingHttpCallbacks)>,
    pub rpc_tx: Sender<RpcEvent>,
    pub rpc_running: Arc<AtomicUsize>,
    pub rpc_callbacks: Vec<(u64, PendingRpcCallbacks)>,
    pub search_tx: Sender<SearchDone>,
    pub search_callbacks: Vec<(u64, PendingSearchCallbacks)>,
    pub timers: Vec<PendingTimer>,
    pub proposals: Vec<PendingProposal>,
    pub view_regs: Vec<PendingViewReg>,
    pub view_updates: Vec<PendingViewUpdate>,
    /// Set on disable/reload so all running exec/http/rpc/search tasks terminate cleanly.
    pub cancel_all: Arc<AtomicBool>,
    pub loaded_modules: HashMap<String, RegistryKey>,
    pub loading_modules: HashSet<String>,
}

impl PluginShared {
    #[allow(clippy::too_many_arguments)]
    fn new(
        plugin_id: String,
        plugin_dir: PathBuf,
        runtime: u64,
        changed_allowed: bool,
        permissions: Option<Vec<String>>,
        config: HashMap<String, serde_json::Value>,
        repaint_ctx: Option<egui::Context>,
        exec_tx: Sender<ExecEvent>,
        exec_running: Arc<AtomicUsize>,
        http_tx: Sender<HttpEvent>,
        http_running: Arc<AtomicUsize>,
        rpc_tx: Sender<RpcEvent>,
        rpc_running: Arc<AtomicUsize>,
        search_tx: Sender<SearchDone>,
    ) -> Self {
        Self {
            plugin_id,
            plugin_dir,
            runtime,
            changed_allowed,
            permissions,
            in_before_save: false,
            ctx: None,
            repaint_ctx,
            actions: Vec::new(),
            commands: Vec::new(),
            events: Vec::new(),
            status: Vec::new(),
            logs: Vec::new(),
            config,
            next_task: 1,
            exec_tx,
            exec_running,
            exec_callbacks: Vec::new(),
            http_tx,
            http_running,
            http_callbacks: Vec::new(),
            rpc_tx,
            rpc_running,
            rpc_callbacks: Vec::new(),
            search_tx,
            search_callbacks: Vec::new(),
            timers: Vec::new(),
            proposals: Vec::new(),
            view_regs: Vec::new(),
            view_updates: Vec::new(),
            cancel_all: Arc::new(AtomicBool::new(false)),
            loaded_modules: HashMap::new(),
            loading_modules: HashSet::new(),
        }
    }

    pub(crate) fn allows(&self, perm: &str) -> bool {
        match perm {
            "network" | "terminal" | "ui" | "rpc" => self
                .permissions
                .as_deref()
                .is_some_and(|list| list.iter().any(|p| p == perm)),
            "exec" | "fs:write" => match self.permissions.as_deref() {
                None => true,
                Some(list) => list.iter().any(|p| p == perm),
            },
            _ => false,
        }
    }
}

pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<String>,
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
    pending_exec: HashMap<u64, PendingExecCallbacks>,
    pending_http: HashMap<u64, PendingHttpCallbacks>,
    pending_rpc: HashMap<u64, PendingRpcCallbacks>,
    pending_search: HashMap<u64, PendingSearchCallbacks>,
    pending_timers: Vec<PendingTimer>,
    pending_proposals: HashMap<u64, PendingProposal>,
    view_handlers: HashMap<String, RegistryKey>,
}

impl Plugin {
    fn enter(&self, ctx: Option<&PluginContext>, in_before_save: bool) {
        self.budget.set(0);
        if let Some(shared) = &self.shared {
            let mut s = shared.borrow_mut();
            s.ctx = ctx.cloned();
            s.in_before_save = in_before_save;
        }
    }

    fn leave(&self) {
        if let Some(shared) = &self.shared {
            let mut s = shared.borrow_mut();
            s.ctx = None;
            s.in_before_save = false;
        }
    }

    fn dispatch(&self, event: Event, ctx: &PluginContext) -> mlua::Result<()> {
        let (Some(lua), Some(shared)) = (self.lua.as_ref(), self.shared.as_ref()) else {
            return Ok(());
        };
        self.enter(Some(ctx), event == Event::BeforeSave);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("name", event.name())?;
            if let Some(subject) = ctx.subject
                && let Some(snapshot) = ctx.docs.get(&subject)
            {
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
        self.enter(ctx, false);
        let result = (|| -> mlua::Result<()> {
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(())
        })();
        self.leave();
        result
    }

    fn call_exec_chunk(
        &self,
        key: &RegistryKey,
        chunk: &str,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(chunk)
        })();
        self.leave();
        result
    }

    fn call_exec_done(
        &self,
        key: &RegistryKey,
        code: i32,
        stdout: &str,
        stderr: &str,
        error: Option<&str>,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("code", code)?;
            table.set("stdout", stdout)?;
            table.set("stderr", stderr)?;
            table.set("error", error)?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }

    fn call_http_chunk(
        &self,
        key: &RegistryKey,
        status: u16,
        chunk: &str,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let function: Function = lua.registry_value(key)?;
            function.call::<()>((chunk, status))
        })();
        self.leave();
        result
    }

    fn call_http_done(
        &self,
        key: &RegistryKey,
        status: u16,
        headers: &HashMap<String, String>,
        body: &str,
        error: Option<&str>,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("status", status)?;
            table.set("headers", api::headers_to_lua(lua, headers)?)?;
            table.set("body", body)?;
            table.set("error", error)?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }

    fn call_rpc_message(
        &self,
        key: &RegistryKey,
        payload: &serde_json::Value,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let val = api::json_to_lua(lua, payload)?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(val)
        })();
        self.leave();
        result
    }

    fn call_rpc_exit(
        &self,
        key: &RegistryKey,
        code: i32,
        error: Option<&str>,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("code", code)?;
            table.set("error", error)?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }

    fn call_search_done(
        &self,
        key: &RegistryKey,
        done: &SearchDone,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            let hits = lua.create_table_with_capacity(done.results.len(), 0)?;
            for (i, hit) in done.results.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("path", hit.path.as_str())?;
                row.set("line", hit.line)?;
                row.set("column", hit.column)?;
                row.set("preview", hit.preview.as_str())?;
                hits.set(i + 1, row)?;
            }
            table.set("results", hits)?;
            table.set("truncated", done.truncated)?;
            table.set("error", done.error.as_deref())?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }

    fn call_proposal_result(
        &self,
        key: &RegistryKey,
        path: &Path,
        accepted: bool,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("accepted", accepted)?;
            table.set("path", path.to_string_lossy().to_string())?;
            let function: Function = lua.registry_value(key)?;
            function.call::<()>(table)
        })();
        self.leave();
        result
    }

    fn call_view_event(
        &self,
        key: &RegistryKey,
        event: &ViewEvent,
        ctx: &PluginContext,
    ) -> mlua::Result<()> {
        let Some(lua) = self.lua.as_ref() else {
            return Ok(());
        };
        self.enter(Some(ctx), false);
        let result = (|| -> mlua::Result<()> {
            let table = lua.create_table()?;
            table.set("view", event.view_id.as_str())?;
            table.set("type", event.kind.as_str())?;
            table.set("id", event.id.as_str())?;
            if let Some(val) = &event.value {
                table.set("value", val.as_str())?;
            }
            if let Some(checked) = event.checked {
                table.set("checked", checked)?;
            }
            let values = lua.create_table_with_capacity(0, event.values.len())?;
            for (k, v) in &event.values {
                values.set(k.as_str(), v.as_str())?;
            }
            table.set("values", values)?;
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
    views: Vec<PluginView>,
    logs: Vec<String>,
    actions: Vec<(u64, HostAction)>,
    open_docs: HashMap<u64, TrackedDoc>,
    repaint_ctx: Option<egui::Context>,
    exec_tx: Sender<ExecEvent>,
    exec_rx: Receiver<ExecEvent>,
    exec_running: Arc<AtomicUsize>,
    http_tx: Sender<HttpEvent>,
    http_rx: Receiver<HttpEvent>,
    http_running: Arc<AtomicUsize>,
    rpc_tx: Sender<RpcEvent>,
    rpc_rx: Receiver<RpcEvent>,
    rpc_running: Arc<AtomicUsize>,
    search_tx: Sender<SearchDone>,
    search_rx: Receiver<SearchDone>,
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
        let (http_tx, http_rx) = channel();
        let (rpc_tx, rpc_rx) = channel();
        let (search_tx, search_rx) = channel();
        let mut host = Self {
            root,
            plugins: Vec::new(),
            commands: Vec::new(),
            status_items: Vec::new(),
            views: Vec::new(),
            logs: Vec::new(),
            actions: Vec::new(),
            open_docs: HashMap::new(),
            repaint_ctx: None,
            exec_tx,
            exec_rx,
            exec_running: Arc::new(AtomicUsize::new(0)),
            http_tx,
            http_rx,
            http_running: Arc::new(AtomicUsize::new(0)),
            rpc_tx,
            rpc_rx,
            rpc_running: Arc::new(AtomicUsize::new(0)),
            search_tx,
            search_rx,
            next_runtime: 1,
            load_errors: Vec::new(),
            errors: Vec::new(),
            trace: std::env::var_os("KESTREL_TRACE").is_some(),
        };
        host.scan(settings);
        host
    }

    pub fn set_repaint_ctx(&mut self, ctx: egui::Context) {
        self.repaint_ctx = Some(ctx.clone());
        for plugin in &self.plugins {
            if let Some(shared) = &plugin.shared {
                shared.borrow_mut().repaint_ctx = Some(ctx.clone());
            }
        }
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
                pending_http: HashMap::new(),
                pending_rpc: HashMap::new(),
                pending_search: HashMap::new(),
                pending_timers: Vec::new(),
                pending_proposals: HashMap::new(),
                view_handlers: HashMap::new(),
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

    pub fn any_has_permission(&self, perm: &str) -> bool {
        self.plugins
            .iter()
            .any(|p| p.loaded && p.manifest.allows(perm))
    }

    pub fn views(&self) -> &[PluginView] {
        &self.views
    }

    pub fn view_mut(&mut self, id: &str) -> Option<&mut PluginView> {
        self.views.iter_mut().find(|v| v.id == id)
    }

    pub fn load_enabled(
        &mut self,
        configs: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Vec<u64> {
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

    fn load_runtime(
        &mut self,
        index: usize,
        config: HashMap<String, serde_json::Value>,
    ) -> Result<u64, String> {
        let started = Instant::now();
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
            dir.clone(),
            runtime,
            manifest.events.iter().any(|event| event == "changed"),
            manifest.permissions.clone(),
            config,
            self.repaint_ctx.clone(),
            self.exec_tx.clone(),
            self.exec_running.clone(),
            self.http_tx.clone(),
            self.http_running.clone(),
            self.rpc_tx.clone(),
            self.rpc_running.clone(),
            self.search_tx.clone(),
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
        self.views.clear();
        self.actions.clear();
        self.open_docs.clear();
        self.scan(settings);
    }

    fn unload(&mut self, index: usize) {
        let runtime = self.plugins[index].runtime;
        self.commands.retain(|command| command.plugin != index);
        self.status_items.retain(|item| item.plugin != runtime);
        self.views.retain(|view| view.runtime != runtime);
        self.actions.retain(|(plugin, _)| *plugin != runtime);
        if let Some(shared) = self.plugins[index].shared.take() {
            let mut shared = shared.borrow_mut();
            shared.cancel_all.store(true, Ordering::Relaxed);
            shared.ctx = None;
            shared.actions.clear();
            shared.commands.clear();
            shared.events.clear();
            shared.status.clear();
            shared.exec_callbacks.clear();
            shared.http_callbacks.clear();
            for (_, rpc) in shared.rpc_callbacks.drain(..) {
                rpc.control.kill();
            }
            shared.search_callbacks.clear();
            shared.timers.clear();
            shared.proposals.clear();
            shared.view_regs.clear();
            shared.view_updates.clear();
            shared.loaded_modules.clear();
            shared.loading_modules.clear();
        }
        for (_, rpc) in self.plugins[index].pending_rpc.drain() {
            rpc.control.kill();
        }
        self.plugins[index].handlers.clear();
        self.plugins[index].pending_exec.clear();
        self.plugins[index].pending_http.clear();
        self.plugins[index].pending_search.clear();
        self.plugins[index].pending_timers.clear();
        self.plugins[index].pending_proposals.clear();
        self.plugins[index].view_handlers.clear();
        self.plugins[index].lua = None;
        self.plugins[index].loaded = false;
    }

    fn drain_plugin(&mut self, index: usize) {
        let Some(shared) = self.plugins[index].shared.clone() else {
            return;
        };
        let runtime = self.plugins[index].runtime;
        let (
            commands,
            events,
            status,
            logs,
            actions,
            execs,
            https,
            rpcs,
            searches,
            timers,
            proposals,
            view_regs,
            view_updates,
        ) = {
            let mut s = shared.borrow_mut();
            (
                std::mem::take(&mut s.commands),
                std::mem::take(&mut s.events),
                std::mem::take(&mut s.status),
                std::mem::take(&mut s.logs),
                std::mem::take(&mut s.actions),
                std::mem::take(&mut s.exec_callbacks),
                std::mem::take(&mut s.http_callbacks),
                std::mem::take(&mut s.rpc_callbacks),
                std::mem::take(&mut s.search_callbacks),
                std::mem::take(&mut s.timers),
                std::mem::take(&mut s.proposals),
                std::mem::take(&mut s.view_regs),
                std::mem::take(&mut s.view_updates),
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
            self.plugins[index]
                .handlers
                .entry(event)
                .or_default()
                .push(key);
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
        for (task, cb) in execs {
            self.plugins[index].pending_exec.insert(task, cb);
        }
        for (task, cb) in https {
            self.plugins[index].pending_http.insert(task, cb);
        }
        for (id, cb) in rpcs {
            self.plugins[index].pending_rpc.insert(id, cb);
        }
        for (task, cb) in searches {
            self.plugins[index].pending_search.insert(task, cb);
        }
        self.plugins[index].pending_timers.extend(timers);
        for prop in proposals {
            self.plugins[index].pending_proposals.insert(prop.id, prop);
        }
        for reg in view_regs {
            if let Some(cb) = reg.on_event {
                self.plugins[index]
                    .view_handlers
                    .insert(reg.id.clone(), cb);
            }
            if let Some(existing) = self.views.iter_mut().find(|v| v.id == reg.id) {
                if existing.runtime == runtime {
                    existing.title = reg.title;
                    existing.location = reg.location;
                } else {
                    self.record_message(index, format!("视图 id 重复，已忽略: {}", reg.id));
                }
            } else {
                self.views.push(PluginView::new(
                    index,
                    runtime,
                    reg.id,
                    reg.title,
                    reg.location,
                ));
            }
        }
        for update in view_updates {
            if let Some(view) = self
                .views
                .iter_mut()
                .find(|v| v.id == update.id && v.runtime == runtime)
            {
                view.nodes = update.nodes;
            }
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

    pub fn dispatch_view_event(&mut self, event: &ViewEvent, ctx: &PluginContext) {
        let Some((plugin_index, runtime)) = self
            .views
            .iter()
            .find(|v| v.id == event.view_id)
            .map(|v| (v.plugin_index, v.runtime))
        else {
            return;
        };
        let Some(plugin) = self.plugins.get(plugin_index) else {
            return;
        };
        if !plugin.loaded || plugin.runtime != runtime {
            return;
        }
        let result = if let Some(key) = plugin.view_handlers.get(&event.view_id) {
            plugin.call_view_event(key, event, ctx)
        } else {
            Ok(())
        };
        if let Err(err) = result {
            self.record_error(plugin_index, err);
        }
        self.drain_plugin(plugin_index);
    }

    pub fn resolve_proposal(
        &mut self,
        runtime: u64,
        proposal_id: u64,
        accepted: bool,
        ctx: &PluginContext,
    ) {
        let Some(index) = self
            .plugins
            .iter()
            .position(|p| p.loaded && p.runtime == runtime)
        else {
            return;
        };
        let Some(prop) = self.plugins[index].pending_proposals.remove(&proposal_id) else {
            return;
        };
        let cb = if accepted {
            prop.on_accept
        } else {
            prop.on_reject
        };
        if let Some(key) = cb
            && let Err(err) = self.plugins[index].call_proposal_result(&key, &prop.path, accepted, ctx)
        {
            self.record_error(index, err);
        }
        self.drain_plugin(index);
    }

    pub fn plan_doc_events(&mut self, tracks: &[DocTrack], active: Option<u64>) -> Vec<DocEvent> {
        let mut events = Vec::new();
        let current: HashSet<u64> = tracks.iter().map(|track| track.id).collect();
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

    pub fn has_pending_async(&self) -> bool {
        self.plugins.iter().any(|p| {
            !p.pending_exec.is_empty()
                || !p.pending_http.is_empty()
                || !p.pending_rpc.is_empty()
                || !p.pending_search.is_empty()
                || !p.pending_timers.is_empty()
        })
    }

    pub fn next_timer_delay(&self) -> Option<Duration> {
        let now = Instant::now();
        self.plugins
            .iter()
            .flat_map(|p| p.pending_timers.iter())
            .filter(|t| !t.cancelled.load(Ordering::Relaxed))
            .map(|t| t.deadline.saturating_duration_since(now))
            .min()
    }

    /// Poll all async channels (exec, streaming HTTP, JSON-RPC, search, timers).
    /// Returns true when at least one Lua callback executed.
    pub fn poll_async(&mut self, ctx: &PluginContext) -> bool {
        let mut fired = false;
        fired |= self.poll_exec(ctx);
        fired |= self.poll_http(ctx);
        fired |= self.poll_rpc(ctx);
        fired |= self.poll_search(ctx);
        fired |= self.poll_timers(ctx);
        fired
    }

    fn runtime_index(&self, runtime: u64) -> Option<usize> {
        self.plugins
            .iter()
            .position(|plugin| plugin.loaded && plugin.runtime == runtime)
    }

    fn poll_exec(&mut self, ctx: &PluginContext) -> bool {
        let mut events = Vec::new();
        while let Ok(item) = self.exec_rx.try_recv() {
            events.push(item);
        }
        if events.is_empty() {
            return false;
        }
        for ev in events {
            match ev {
                ExecEvent::Stdout {
                    runtime,
                    task,
                    chunk,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    if let Some(cb) = self.plugins[index].pending_exec.get(&task)
                        && !cb.cancel.load(Ordering::Relaxed)
                        && let Some(key) = &cb.on_stdout
                        && let Err(err) = self.plugins[index].call_exec_chunk(key, &chunk, ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
                ExecEvent::Stderr {
                    runtime,
                    task,
                    chunk,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    if let Some(cb) = self.plugins[index].pending_exec.get(&task)
                        && !cb.cancel.load(Ordering::Relaxed)
                        && let Some(key) = &cb.on_stderr
                        && let Err(err) = self.plugins[index].call_exec_chunk(key, &chunk, ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
                ExecEvent::Done {
                    runtime,
                    task,
                    code,
                    stdout,
                    stderr,
                    error,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    let Some(cb) = self.plugins[index].pending_exec.remove(&task) else {
                        continue;
                    };
                    if let Some(key) = cb.on_done
                        && let Err(err) = self.plugins[index].call_exec_done(
                            &key,
                            code,
                            &stdout,
                            &stderr,
                            error.as_deref(),
                            ctx,
                        )
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
            }
        }
        true
    }

    fn poll_http(&mut self, ctx: &PluginContext) -> bool {
        let mut events = Vec::new();
        while let Ok(item) = self.http_rx.try_recv() {
            events.push(item);
        }
        if events.is_empty() {
            return false;
        }
        for ev in events {
            match ev {
                HttpEvent::Chunk {
                    runtime,
                    task,
                    status,
                    chunk,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    if let Some(cb) = self.plugins[index].pending_http.get(&task)
                        && !cb.cancel.load(Ordering::Relaxed)
                        && let Some(key) = &cb.on_chunk
                        && let Err(err) =
                            self.plugins[index].call_http_chunk(key, status, &chunk, ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
                HttpEvent::Done {
                    runtime,
                    task,
                    status,
                    headers,
                    body,
                    error,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    let Some(cb) = self.plugins[index].pending_http.remove(&task) else {
                        continue;
                    };
                    if let Err(err) = self.plugins[index].call_http_done(
                        &cb.on_done,
                        status,
                        &headers,
                        &body,
                        error.as_deref(),
                        ctx,
                    ) {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
            }
        }
        true
    }

    fn poll_rpc(&mut self, ctx: &PluginContext) -> bool {
        let mut events = Vec::new();
        while let Ok(item) = self.rpc_rx.try_recv() {
            events.push(item);
        }
        if events.is_empty() {
            return false;
        }
        for ev in events {
            match ev {
                RpcEvent::Message {
                    runtime,
                    id,
                    payload,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    if let Some(cb) = self.plugins[index].pending_rpc.get(&id)
                        && let Some(key) = &cb.on_message
                        && let Err(err) = self.plugins[index].call_rpc_message(key, &payload, ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
                RpcEvent::Stderr { runtime, id, chunk } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    if let Some(cb) = self.plugins[index].pending_rpc.get(&id)
                        && let Some(key) = &cb.on_stderr
                        && let Err(err) = self.plugins[index].call_exec_chunk(key, &chunk, ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
                RpcEvent::Exit {
                    runtime,
                    id,
                    code,
                    error,
                } => {
                    let Some(index) = self.runtime_index(runtime) else {
                        continue;
                    };
                    let Some(cb) = self.plugins[index].pending_rpc.remove(&id) else {
                        continue;
                    };
                    if let Some(key) = cb.on_exit
                        && let Err(err) =
                            self.plugins[index].call_rpc_exit(&key, code, error.as_deref(), ctx)
                    {
                        self.record_error(index, err);
                    }
                    self.drain_plugin(index);
                }
            }
        }
        true
    }

    fn poll_search(&mut self, ctx: &PluginContext) -> bool {
        let mut done = Vec::new();
        while let Ok(item) = self.search_rx.try_recv() {
            done.push(item);
        }
        if done.is_empty() {
            return false;
        }
        for item in done {
            let Some(index) = self.runtime_index(item.runtime) else {
                continue;
            };
            let Some(cb) = self.plugins[index].pending_search.remove(&item.task) else {
                continue;
            };
            if let Err(err) = self.plugins[index].call_search_done(&cb.on_done, &item, ctx) {
                self.record_error(index, err);
            }
            self.drain_plugin(index);
        }
        true
    }

    fn poll_timers(&mut self, ctx: &PluginContext) -> bool {
        let now = Instant::now();
        let mut fired = false;
        for index in 0..self.plugins.len() {
            if !self.plugins[index].loaded || self.plugins[index].pending_timers.is_empty() {
                continue;
            }
            let mut due = Vec::new();
            let mut keep = Vec::new();
            for timer in std::mem::take(&mut self.plugins[index].pending_timers) {
                if timer.cancelled.load(Ordering::Relaxed) {
                    continue;
                }
                if timer.deadline <= now {
                    due.push(timer);
                } else {
                    keep.push(timer);
                }
            }
            self.plugins[index].pending_timers = keep;
            for timer in due {
                fired = true;
                if let Err(err) = self.plugins[index].call_handler(&timer.callback, Some(ctx)) {
                    self.record_error(index, err);
                }
                self.drain_plugin(index);
            }
        }
        fired
    }

    pub fn run_command(&mut self, index: usize, ctx: &PluginContext) -> bool {
        let started = Instant::now();
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
                permissions: plugin.manifest.permissions_list().to_vec(),
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

    fn write_plugin_with_perms(root: &Path, id: &str, perms: &[&str], main: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let perms_json = serde_json::to_string(perms).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"0.1.0","apiVersion":1,"main":"main.lua","permissions":{perms_json}}}"#
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
        ctx.open_order.push(7);
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
        let started = Instant::now();
        let mut host = PluginHost::with_root(root.path().to_path_buf(), &Settings::default());
        let runtimes = host.load_enabled(&HashMap::new());
        let elapsed = started.elapsed();
        assert_eq!(runtimes.len(), 20);
        let memory: usize = host.plugin_infos().iter().map(|info| info.memory).sum();
        println!("20 plugins: {elapsed:?}, {} KB", memory / 1024);
        assert!(elapsed < Duration::from_millis(300), "{elapsed:?}");
        assert!(memory < 4 * 1024 * 1024);
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

    #[test]
    fn local_require_loads_modules_and_blocks_traversal() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "modtest",
            r#"
local helper = require("sub.helper")
kestrel.command("modtest.run", "run", function()
  kestrel.log("val=" .. helper.greet("kestrel"))
  local ok = pcall(function() require("../escape") end)
  assert(not ok, "traversal must fail")
end)
"#,
        );
        let sub_dir = root.path().join("modtest").join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(
            sub_dir.join("helper.lua"),
            r#"return { greet = function(name) return "hi " .. name end }"#,
        )
        .unwrap();

        let mut host = load(root.path());
        assert!(host.plugin_infos()[0].error.is_none());
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());
        assert!(host.logs().iter().any(|l| l.contains("val=hi kestrel")));
    }

    #[test]
    fn json_encode_decode_and_permissions_work() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "jsonperm",
            r#"
kestrel.command("jsonperm.run", "run", function()
  local s = kestrel.json.encode({ model = "gpt", tags = { "a", "b" } })
  local obj = kestrel.json.decode(s)
  assert(obj.model == "gpt" and obj.tags[2] == "b")
  -- network permission not declared -> must fail
  local ok, err = pcall(function()
    kestrel.http({ url = "https://example.com", on_done = function() end })
  end)
  assert(not ok and tostring(err):find("network"), "expected permission error")
  kestrel.log("json_ok")
end)
"#,
        );
        let mut host = load(root.path());
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());
        assert!(host.logs().iter().any(|l| l.contains("json_ok")));
    }

    #[test]
    fn declarative_ui_view_registers_updates_and_handles_events() {
        let root = TempRoot::new();
        write_plugin_with_perms(
            root.path(),
            "uiview",
            &["ui"],
            r###"
kestrel.register_view({
  id = "uiview.panel",
  title = "Agent",
  location = "right",
  on_event = function(ev)
    kestrel.log("ev=" .. ev.type .. ":" .. ev.id .. ":" .. (ev.values.prompt or ""))
  end,
})
kestrel.set_view("uiview.panel", {
  { type = "markdown", id = "m1", text = "## Hello Agent" },
  { type = "text_input", id = "prompt", placeholder = "Ask..." },
  { type = "button", id = "send", label = "Send", variant = "primary" },
})
"###,
        );
        let mut host = load(root.path());
        assert!(host.plugin_infos()[0].error.is_none());
        assert_eq!(host.views().len(), 1);
        assert_eq!(host.views()[0].id, "uiview.panel");
        assert_eq!(host.views()[0].nodes.len(), 3);

        let mut values = HashMap::new();
        values.insert("prompt".to_string(), "fix bug".to_string());
        let ev = ViewEvent {
            view_id: "uiview.panel".to_string(),
            kind: "click".to_string(),
            id: "send".to_string(),
            value: None,
            checked: None,
            values,
        };
        host.dispatch_view_event(&ev, &PluginContext::default());
        assert!(host.take_errors().is_empty());
        assert!(
            host.logs()
                .iter()
                .any(|l| l.contains("ev=click:send:fix bug"))
        );

        host.disable(0);
        assert!(host.views().is_empty());
    }

    #[test]
    fn workspace_tools_and_propose_edit_lifecycle() {
        let root = TempRoot::new();
        let ws = TempRoot::new();
        std::fs::write(ws.path().join("hello.txt"), "line1\nline2\nline3\n").unwrap();

        write_plugin_with_perms(
            root.path(),
            "agent",
            &["fs:write"],
            r#"
kestrel.command("agent.inspect", "inspect", function()
  local files = kestrel.list_files()
  assert(#files == 1 and files[1] == "hello.txt")
  local chunk = kestrel.read_lines("hello.txt", 2, 3)
  assert(chunk.text == "line2\nline3" and chunk.total_lines == 3)
  local st = kestrel.stat("hello.txt")
  assert(st.exists and st.is_file)
  kestrel.propose_edit({
    path = "hello.txt",
    text = "line1\nUPDATED\nline3\n",
    title = "Update line 2",
    on_accept = function(res) kestrel.log("accepted:" .. tostring(res.accepted)) end,
    on_reject = function(res) kestrel.log("rejected:" .. tostring(res.accepted)) end,
  })
end)
"#,
        );
        let mut host = load(root.path());
        let mut ctx = PluginContext::default();
        ctx.root = Some(ws.path().to_path_buf());
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());
        let actions = host.take_actions();
        let (runtime, proposal_id) = actions
            .iter()
            .find_map(|(rt, a)| match a {
                HostAction::ProposeEdit { proposal_id, .. } => Some((*rt, *proposal_id)),
                _ => None,
            })
            .expect("expected ProposeEdit action");
        host.resolve_proposal(runtime, proposal_id, true, &ctx);
        assert!(host.logs().iter().any(|l| l.contains("accepted:true")));
    }

    #[test]
    fn timer_fires_after_deadline_and_can_be_cancelled() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "tmr",
            r#"
kestrel.command("tmr.run", "run", function()
  kestrel.set_timeout(1, function() kestrel.log("fired") end)
  local c = kestrel.set_timeout(1, function() kestrel.log("cancelled") end)
  c:cancel()
end)
"#,
        );
        let mut host = load(root.path());
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        std::thread::sleep(Duration::from_millis(10));
        assert!(host.poll_async(&ctx));
        assert!(host.logs().iter().any(|l| l.contains("fired")));
        assert!(!host.logs().iter().any(|l| l.contains("cancelled")));
    }

    #[test]
    fn before_save_forbids_exec() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "nosaveexec",
            r#"
kestrel.on("before_save", function()
  kestrel.exec({ program = "cmd", args = { "/C", "echo hi" }, on_done = function() end })
end)
"#,
        );
        let mut host = load(root.path());
        host.emit(Event::BeforeSave, &PluginContext::default());
        let errors = host.take_errors();
        assert!(
            errors.iter().any(|e| e.contains("before_save")),
            "{errors:?}"
        );
    }

    #[test]
    fn async_search_and_streaming_http_work_end_to_end() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req_buf = [0u8; 2048];
                let _ = stream.read(&mut req_buf);
                let body = "data: {\"delta\":\"hello \"}\n\ndata: {\"delta\":\"kestrel\"}\n\n";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        let root = TempRoot::new();
        let ws = TempRoot::new();
        std::fs::write(ws.path().join("alpha.rs"), "fn needle_fn() {}\n").unwrap();

        let script = format!(
            r#"
kestrel.command("net.test", "net", function()
  kestrel.search({{
    pattern = "needle_fn",
    on_done = function(res)
      kestrel.log("search_hits=" .. tostring(#res.results) .. ":" .. (res.results[1] and res.results[1].path or ""))
    end,
  }})
  local chunks = ""
  kestrel.http({{
    method = "POST",
    url = "http://127.0.0.1:{}/v1/chat",
    headers = {{ ["X-Test"] = "1" }},
    body = {{ prompt = "hi" }},
    on_chunk = function(chunk, status)
      chunks = chunks .. chunk
    end,
    on_done = function(res)
      kestrel.log("http_status=" .. tostring(res.status) .. " has_chunk=" .. tostring(chunks:find("kestrel") ~= nil))
    end,
  }})
end)
"#,
            addr.port()
        );
        write_plugin_with_perms(root.path(), "nettest", &["network"], &script);
        let mut host = load(root.path());
        let mut ctx = PluginContext::default();
        ctx.root = Some(ws.path().to_path_buf());
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());

        let deadline = Instant::now() + Duration::from_secs(5);
        while host.has_pending_async() && Instant::now() < deadline {
            host.poll_async(&ctx);
            std::thread::sleep(Duration::from_millis(15));
        }
        let _ = server.join();
        assert!(host.take_errors().is_empty());
        assert!(
            host.logs()
                .iter()
                .any(|l| l.contains("search_hits=1:alpha.rs")),
            "{:?}",
            host.logs()
        );
        assert!(
            host.logs()
                .iter()
                .any(|l| l.contains("http_status=200 has_chunk=true")),
            "{:?}",
            host.logs()
        );
    }

    #[test]
    fn open_docs_exposes_diagnostics_and_outline() {
        let root = TempRoot::new();
        write_plugin(
            root.path(),
            "inspect",
            r#"
kestrel.command("inspect.run", "run", function()
  local docs = kestrel.open_docs()
  assert(#docs == 1)
  local diags = docs[1]:diagnostics()
  local syms = docs[1]:outline()
  kestrel.log("d=" .. tostring(#diags) .. ":" .. diags[1].severity .. " s=" .. tostring(#syms) .. ":" .. syms[1].name)
end)
"#,
        );
        let mut host = load(root.path());
        let mut editor = Editor::new(Document::from_text("fn demo() {}"), 11, 0);
        editor.diagnostics.push(crate::lsp::Diagnostic {
            line: 0,
            col: 0,
            end_col: 2,
            start_byte: 0,
            end_byte: 2,
            severity: crate::lsp::DiagSeverity::Warning,
            message: "unused".to_string(),
            source: "syntax".to_string(),
        });
        editor.symbols.push(crate::lsp::OutlineSymbol {
            name: "demo".to_string(),
            detail: "fn demo()".to_string(),
            kind: crate::lsp::SymbolKind::Function,
            start_line: 0,
            end_line: 0,
            byte_offset: 0,
            depth: 0,
        });
        let mut ctx = PluginContext::default();
        ctx.active = Some(11);
        ctx.open_order.push(11);
        ctx.docs
            .insert(11, Rc::new(DocSnapshot::from_editor(&editor)));
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());
        assert!(
            host.logs()
                .iter()
                .any(|l| l.contains("d=1:warning s=1:demo"))
        );
    }

    #[test]
    fn cline_style_agent_plugin_loads_and_renders_view() {
        let root = TempRoot::new();
        write_plugin_with_perms(
            root.path(),
            "agent-mini",
            &["network", "exec", "terminal", "ui", "fs:write"],
            r###"
local tools = require("tools")
kestrel.register_view({
  id = "agent.chat",
  title = "AI Agent",
  location = "right",
  on_event = function(ev)
    if ev.id == "ping" then
      kestrel.log("agent_ping:" .. tools.tag())
    end
  end,
})
kestrel.on("startup", function()
  kestrel.set_view("agent.chat", {
    { type = "badge", text = "ready", tone = "accent" },
    { type = "markdown", id = "welcome", text = '### AI Agent Ready' },
    { type = "button", id = "ping", label = "Ping", variant = "primary" },
  })
end)
kestrel.command("agent.open", "Open Agent", function()
  kestrel.open_view("agent.chat")
end)
"###,
        );
        std::fs::write(
            root.path().join("agent-mini").join("tools.lua"),
            r#"return { tag = function() return "ok" end }"#,
        )
        .unwrap();

        let mut host = PluginHost::with_root(root.path().to_path_buf(), &Settings::default());
        let loaded = host.load_enabled(&HashMap::new());
        assert_eq!(loaded.len(), 1);
        assert!(host.plugin_infos()[0].error.is_none());
        let ctx = PluginContext::default();
        host.emit_to(loaded[0], Event::Startup, &ctx);
        assert!(host.take_errors().is_empty());
        let view = host
            .views()
            .iter()
            .find(|v| v.id == "agent.chat")
            .expect("agent.chat view must be registered");
        assert_eq!(view.nodes.len(), 3);
        assert!(host.command_index_of_id("agent.open").is_some());

        // When local docs/plugins/cline-demo is present in the workspace, also smoke-test it.
        let demo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("plugins");
        if demo_root.join("cline-demo").join("plugin.json").is_file() {
            let mut demo_host = PluginHost::with_root(demo_root, &Settings::default());
            let demo_loaded = demo_host.load_enabled(&HashMap::new());
            for info in demo_host.plugin_infos() {
                assert!(
                    info.error.is_none(),
                    "plugin {} failed: {:?}",
                    info.id,
                    info.error
                );
            }
            for rt in demo_loaded {
                demo_host.emit_to(rt, Event::Startup, &ctx);
            }
            assert!(demo_host.take_errors().is_empty());
            assert!(demo_host.views().iter().any(|v| v.id == "cline.agent"));
        }
    }

    #[test]
    fn spawn_rpc_receives_ndjson_messages_and_exit() {
        let root = TempRoot::new();
        #[cfg(windows)]
        let script = r#"
kestrel.command("rpc.test", "rpc", function()
  local h = kestrel.spawn_rpc({
    program = "powershell",
    args = { "-NoProfile", "-Command", "$line = [Console]::In.ReadLine(); Write-Output $line" },
    framing = "ndjson",
    on_message = function(msg)
      kestrel.log("rpc_msg=" .. tostring(msg.result))
    end,
    on_exit = function(ev)
      kestrel.log("rpc_exit=" .. tostring(ev.code))
    end,
  })
  h:send({ jsonrpc = "2.0", result = "pong" })
end)
"#;
        #[cfg(not(windows))]
        let script = r#"
kestrel.command("rpc.test", "rpc", function()
  local h = kestrel.spawn_rpc({
    program = "sh",
    args = { "-c", "IFS= read -r line; printf '%s\\n' \"$line\"" },
    framing = "ndjson",
    on_message = function(msg)
      kestrel.log("rpc_msg=" .. tostring(msg.result))
    end,
    on_exit = function(ev)
      kestrel.log("rpc_exit=" .. tostring(ev.code))
    end,
  })
  h:send({ jsonrpc = "2.0", result = "pong" })
end)
"#;
        write_plugin_with_perms(root.path(), "rpctest", &["rpc"], script);
        let mut host = load(root.path());
        let ctx = PluginContext::default();
        assert!(host.run_command(0, &ctx));
        assert!(host.take_errors().is_empty());

        let deadline = Instant::now() + Duration::from_secs(5);
        while host.has_pending_async() && Instant::now() < deadline {
            host.poll_async(&ctx);
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(host.take_errors().is_empty());
        assert!(
            host.logs().iter().any(|l| l.contains("rpc_msg=pong")),
            "{:?}",
            host.logs()
        );
        assert!(
            host.logs().iter().any(|l| l.contains("rpc_exit=0")),
            "{:?}",
            host.logs()
        );
    }
}
