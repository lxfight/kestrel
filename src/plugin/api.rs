//! The Lua host API exposed to plugins as the global `kestrel` table, local
//! sandboxed `require`, streaming HTTP / exec / JSON-RPC handles, workspace tools,
//! declarative UI views, and document userdata.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};

use super::actions::{DocSnapshot, HostAction};
use super::http::{DEFAULT_TIMEOUT_MS, HttpRequestSpec};
use super::rpc::{RpcControl, RpcFraming, RpcSpawnSpec};
use super::view::{ViewLocation, parse_nodes};
use super::{
    Event, ExecEvent, MAX_CONCURRENT_EXEC, PendingCommand, PendingExecCallbacks,
    PendingHttpCallbacks, PendingProposal, PendingRpcCallbacks, PendingSearchCallbacks,
    PendingTimer, PendingViewReg, PendingViewUpdate, PluginShared, SearchDone, SearchHitItem,
};
use crate::commands::{Command, Shortcut};
use crate::lsp::DiagSeverity;
use crate::workspace::search::SearchQuery;

/// A single `doc:text()` call refuses documents above this size; use ranges instead.
const MAX_TEXT_BYTES: usize = 1_500_000;
const MAX_READ_LINES: usize = 2_000;
const MAX_LIST_FILES: usize = 5_000;

pub(crate) fn install(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<()> {
    let kestrel = lua.create_table()?;

    // Core registration & host feedback
    kestrel.set("command", command_fn(lua, shared)?)?;
    kestrel.set("on", on_fn(lua, shared)?)?;
    kestrel.set("notify", notify_fn(lua, shared)?)?;
    kestrel.set("log", log_fn(lua, shared)?)?;
    kestrel.set("status_item", status_fn(lua, shared)?)?;
    kestrel.set("run_command", run_command_fn(lua, shared)?)?;
    kestrel.set("config", config_fn(lua, shared)?)?;
    kestrel.set("set_config", set_config_fn(lua, shared)?)?;
    kestrel.set("set_timeout", set_timeout_fn(lua, shared)?)?;

    // JSON helpers
    let json_tbl = lua.create_table()?;
    json_tbl.set("encode", json_encode_fn(lua)?)?;
    json_tbl.set("decode", json_decode_fn(lua)?)?;
    kestrel.set("json", json_tbl)?;

    // Isolated per-plugin secret storage
    kestrel.set("secret_get", secret_get_fn(lua, shared)?)?;
    kestrel.set("secret_set", secret_set_fn(lua, shared)?)?;
    kestrel.set("secret_delete", secret_delete_fn(lua, shared)?)?;

    // Documents & editor context
    kestrel.set("active_doc", active_doc_fn(lua, shared)?)?;
    kestrel.set("open_docs", open_docs_fn(lua, shared)?)?;
    kestrel.set("doc_by_path", doc_by_path_fn(lua, shared)?)?;

    // Workspace & filesystem tools
    kestrel.set("root", root_fn(lua, shared)?)?;
    kestrel.set("open_file", open_file_fn(lua, shared)?)?;
    kestrel.set("read_file", read_file_fn(lua, shared)?)?;
    kestrel.set("read_lines", read_lines_fn(lua, shared)?)?;
    kestrel.set("write_file", write_file_fn(lua, shared)?)?;
    kestrel.set("edit_file", edit_file_fn(lua, shared)?)?;
    kestrel.set("propose_edit", propose_edit_fn(lua, shared)?)?;
    kestrel.set("stat", stat_fn(lua, shared)?)?;
    kestrel.set("mkdir", mkdir_fn(lua, shared)?)?;
    kestrel.set("delete_file", delete_file_fn(lua, shared)?)?;
    kestrel.set("list_files", list_files_fn(lua, shared)?)?;
    kestrel.set("search", search_fn(lua, shared)?)?;

    // Process, streaming HTTP, PTY terminal, and bidirectional JSON-RPC
    kestrel.set("exec", exec_fn(lua, shared)?)?;
    kestrel.set("http", http_fn(lua, shared)?)?;
    kestrel.set("spawn_rpc", spawn_rpc_fn(lua, shared)?)?;
    kestrel.set("terminal_run", terminal_run_fn(lua, shared)?)?;
    kestrel.set("terminal_send", terminal_send_fn(lua, shared)?)?;
    kestrel.set("terminal_list", terminal_list_fn(lua, shared)?)?;
    kestrel.set("terminal_read", terminal_read_fn(lua, shared)?)?;

    // Retained declarative UI views
    kestrel.set("register_view", register_view_fn(lua, shared)?)?;
    kestrel.set("set_view", set_view_fn(lua, shared)?)?;
    kestrel.set("open_view", open_view_fn(lua, shared)?)?;
    kestrel.set("close_view", close_view_fn(lua, shared)?)?;

    lua.globals().set("kestrel", kestrel)?;

    // Sandboxed local `require` restricted strictly to the plugin's own directory.
    lua.globals().set("require", require_fn(lua, shared)?)?;

    // Route `print` into the Output panel instead of stdout.
    let logs = shared.clone();
    let print = lua.create_function(move |_, args: mlua::Variadic<Value>| {
        let text = args
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join("\t");
        logs.borrow_mut().logs.push(text);
        Ok(())
    })?;
    lua.globals().set("print", print)?;
    Ok(())
}

fn require_permission(shared: &Rc<RefCell<PluginShared>>, perm: &str) -> mlua::Result<()> {
    if !shared.borrow().allows(perm) {
        return Err(mlua::Error::runtime(format!(
            "缺少权限 `{perm}`，请在 plugin.json 的 permissions 中声明"
        )));
    }
    Ok(())
}

fn forbid_in_before_save(shared: &Rc<RefCell<PluginShared>>, api: &str) -> mlua::Result<()> {
    if shared.borrow().in_before_save {
        return Err(mlua::Error::runtime(format!(
            "before_save 钩子内禁止调用 {api}"
        )));
    }
    Ok(())
}

fn require_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, modname: String| {
        if modname.is_empty()
            || modname.contains("..")
            || modname.starts_with('/')
            || modname.starts_with('\\')
            || modname.contains(':')
        {
            return Err(mlua::Error::runtime(format!("非法模块名: {modname}")));
        }
        if let Some(cached) = {
            let s = shared.borrow();
            if s.loading_modules.contains(&modname) {
                return Err(mlua::Error::runtime(format!("检测到循环 require: {modname}")));
            }
            s.loaded_modules
                .get(&modname)
                .map(|key| lua.registry_value::<Value>(key))
        } {
            return cached;
        }

        let plugin_dir = shared.borrow().plugin_dir.clone();
        let rel = modname.replace('.', "/");
        let candidate_file = plugin_dir.join(format!("{rel}.lua"));
        let candidate_init = plugin_dir.join(&rel).join("init.lua");
        let target = if candidate_file.is_file() {
            candidate_file
        } else if candidate_init.is_file() {
            candidate_init
        } else {
            return Err(mlua::Error::runtime(format!(
                "找不到插件模块 `{modname}`"
            )));
        };

        let canon_dir = std::fs::canonicalize(&plugin_dir).unwrap_or(plugin_dir);
        let canon_target = std::fs::canonicalize(&target)
            .map_err(|err| mlua::Error::runtime(format!("解析模块路径失败: {err}")))?;
        if !canon_target.starts_with(&canon_dir) {
            return Err(mlua::Error::runtime(format!(
                "模块路径越界（限制在插件目录内）: {modname}"
            )));
        }

        let source = std::fs::read_to_string(&canon_target)
            .map_err(|err| mlua::Error::runtime(format!("读取模块 `{modname}` 失败: {err}")))?;

        let plugin_id = shared.borrow().plugin_id.clone();
        shared.borrow_mut().loading_modules.insert(modname.clone());
        let eval_res = lua
            .load(&source)
            .set_name(format!("@{plugin_id}/{rel}.lua"))
            .eval::<Value>();
        shared.borrow_mut().loading_modules.remove(&modname);

        let val = eval_res?;
        let stored = match val {
            Value::Nil => Value::Boolean(true),
            other => other,
        };
        let key = lua.create_registry_value(stored.clone())?;
        shared.borrow_mut().loaded_modules.insert(modname, key);
        Ok(stored)
    })
}

fn json_encode_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(|_, (val, pretty): (Value, Option<bool>)| {
        let json = lua_to_json(&val)?;
        let text = if pretty.unwrap_or(false) {
            serde_json::to_string_pretty(&json)
        } else {
            serde_json::to_string(&json)
        };
        text.map_err(|err| mlua::Error::runtime(format!("JSON 编码失败: {err}")))
    })
}

fn json_decode_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(|lua, text: String| {
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|err| mlua::Error::runtime(format!("JSON 解码失败: {err}")))?;
        json_to_lua(lua, &json)
    })
}

fn secret_get_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, key: String| {
        let plugin_id = shared.borrow().plugin_id.clone();
        super::secret::get_secret(&plugin_id, &key).map_err(mlua::Error::runtime)
    })
}

fn secret_set_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (key, value): (String, String)| {
        let plugin_id = shared.borrow().plugin_id.clone();
        super::secret::set_secret(&plugin_id, &key, &value).map_err(mlua::Error::runtime)
    })
}

fn secret_delete_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, key: String| {
        let plugin_id = shared.borrow().plugin_id.clone();
        super::secret::delete_secret(&plugin_id, &key).map_err(mlua::Error::runtime)
    })
}

fn command_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(
        move |lua, (id, title, handler, key): (String, String, Function, Option<String>)| {
            if id.is_empty()
                || !id
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            {
                return Err(mlua::Error::runtime(format!("非法命令 id: {id}")));
            }
            let shortcut = match key {
                Some(key) => Some(
                    Shortcut::parse(&key)
                        .ok_or_else(|| mlua::Error::runtime(format!("无法解析快捷键: {key}")))?,
                ),
                None => None,
            };
            let handler = lua.create_registry_value(handler)?;
            shared.borrow_mut().commands.push(PendingCommand {
                id,
                title,
                shortcut,
                handler,
            });
            Ok(())
        },
    )
}

fn on_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, (name, handler): (String, Function)| {
        let event = Event::parse(&name)
            .ok_or_else(|| mlua::Error::runtime(format!("未知事件: {name}")))?;
        if event == Event::Changed && !shared.borrow().changed_allowed {
            return Err(mlua::Error::runtime(
                "changed 需要在 plugin.json 的 events 里声明",
            ));
        }
        let key = lua.create_registry_value(handler)?;
        shared.borrow_mut().events.push((event, key));
        Ok(())
    })
}

struct CancelHandle {
    id: u64,
    cancel: Arc<AtomicBool>,
}

impl UserData for CancelHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("id", |_, this, ()| Ok(this.id));
        methods.add_method("cancel", |_, this, ()| {
            this.cancel.store(true, Ordering::Relaxed);
            Ok(())
        });
        methods.add_method("is_cancelled", |_, this, ()| {
            Ok(this.cancel.load(Ordering::Relaxed))
        });
    }
}

fn set_timeout_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, (delay_ms, callback): (u64, Function)| {
        let key = lua.create_registry_value(callback)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let id = {
            let mut s = shared.borrow_mut();
            let id = s.next_task;
            s.next_task += 1;
            s.timers.push(PendingTimer {
                deadline: Instant::now() + Duration::from_millis(delay_ms.min(600_000)),
                callback: key,
                cancelled: cancelled.clone(),
            });
            id
        };
        Ok(CancelHandle {
            id,
            cancel: cancelled,
        })
    })
}

fn active_doc_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, ()| {
        let snapshot = {
            let shared = shared.borrow();
            let Some(ctx) = shared.ctx.as_ref() else {
                return Ok(None);
            };
            let Some(id) = ctx.active else {
                return Ok(None);
            };
            ctx.docs.get(&id).cloned()
        };
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let handle = DocHandle::new(snapshot, shared.clone());
        Ok(Some(lua.create_userdata(handle)?))
    })
}

fn open_docs_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, ()| {
        let snapshots: Vec<Rc<DocSnapshot>> = {
            let s = shared.borrow();
            let Some(ctx) = s.ctx.as_ref() else {
                return lua.create_table();
            };
            ctx.open_order
                .iter()
                .filter_map(|id| ctx.docs.get(id).cloned())
                .collect()
        };
        let table = lua.create_table_with_capacity(snapshots.len(), 0)?;
        for (i, snap) in snapshots.into_iter().enumerate() {
            table.set(i + 1, lua.create_userdata(DocHandle::new(snap, shared.clone()))?)?;
        }
        Ok(table)
    })
}

fn doc_by_path_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, raw_path: String| {
        let target = resolve_path(&shared, &raw_path)?;
        let snapshot = {
            let s = shared.borrow();
            let Some(ctx) = s.ctx.as_ref() else {
                return Ok(None);
            };
            ctx.docs.values().find(|doc| {
                doc.path.as_ref().is_some_and(|p| {
                    p == &target
                        || std::fs::canonicalize(p)
                            .map(|c| c == target)
                            .unwrap_or(false)
                })
            }).cloned()
        };
        match snapshot {
            Some(snap) => Ok(Some(lua.create_userdata(DocHandle::new(snap, shared.clone()))?)),
            None => Ok(None),
        }
    })
}

fn notify_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (text, level): (String, Option<String>)| {
        let level = match level.as_deref() {
            Some("warning") | Some("warn") => crate::app::ToastKind::Warning,
            Some("error") => crate::app::ToastKind::Error,
            Some("success") => crate::app::ToastKind::Success,
            _ => crate::app::ToastKind::Info,
        };
        shared
            .borrow_mut()
            .actions
            .push(HostAction::Notify { text, level });
        Ok(())
    })
}

fn log_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, text: String| {
        shared.borrow_mut().logs.push(text);
        Ok(())
    })
}

fn status_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(
        move |_, (key, text, command): (String, String, Option<String>)| {
            shared.borrow_mut().status.push((key, text, command));
            Ok(())
        },
    )
}

fn root_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, ()| {
        let root = shared
            .borrow()
            .ctx
            .as_ref()
            .and_then(|ctx| ctx.root.as_ref())
            .map(|path| path.to_string_lossy().to_string());
        Ok(root)
    })
}

fn open_file_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, path: String| {
        let path = resolve_path(&shared, &path)?;
        shared.borrow_mut().actions.push(HostAction::OpenFile(path));
        Ok(())
    })
}

fn read_file_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, path: String| {
        let path = resolve_path(&shared, &path)?;
        let meta = std::fs::metadata(&path)
            .map_err(|err| mlua::Error::runtime(format!("读取 {} 失败: {err}", path.display())))?;
        if meta.len() > (MAX_TEXT_BYTES as u64) * 2 {
            return Err(mlua::Error::runtime(
                "文件过大，请使用 kestrel.read_lines 分段读取",
            ));
        }
        std::fs::read_to_string(&path)
            .map_err(|err| mlua::Error::runtime(format!("读取 {} 失败: {err}", path.display())))
    })
}

fn read_lines_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(
        move |lua, (path, start_line, end_line): (String, Option<usize>, Option<usize>)| {
            let path = resolve_path(&shared, &path)?;
            let raw = std::fs::read_to_string(&path)
                .map_err(|err| mlua::Error::runtime(format!("读取 {} 失败: {err}", path.display())))?;
            let all_lines: Vec<&str> = raw.lines().collect();
            let total = all_lines.len();
            let start = start_line.unwrap_or(1).max(1);
            let requested_end = end_line.unwrap_or(start + 200 - 1).max(start);
            let capped_end = requested_end.min(start + MAX_READ_LINES - 1).min(total);
            let slice = if start > total || total == 0 {
                String::new()
            } else {
                all_lines[start - 1..capped_end].join("\n")
            };
            let table = lua.create_table()?;
            table.set("text", slice)?;
            table.set("start_line", start)?;
            table.set("end_line", capped_end)?;
            table.set("total_lines", total)?;
            table.set("truncated", capped_end < requested_end && capped_end < total)?;
            Ok(table)
        },
    )
}

fn write_file_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (path, text): (String, String)| {
        require_permission(&shared, "fs:write")?;
        let path = resolve_path(&shared, &path)?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text)
            .map_err(|err| mlua::Error::runtime(format!("写入 {} 失败: {err}", path.display())))
    })
}

fn edit_file_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (path, text, save): (String, String, Option<bool>)| {
        require_permission(&shared, "fs:write")?;
        let path = resolve_path(&shared, &path)?;
        shared.borrow_mut().actions.push(HostAction::EditOrWriteFile {
            path,
            text,
            save: save.unwrap_or(true),
        });
        Ok(())
    })
}

fn propose_edit_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        require_permission(&shared, "fs:write")?;
        let raw_path: String = spec.get("path")?;
        let text: String = spec.get("text")?;
        let title: String = spec.get::<Option<String>>("title")?.unwrap_or_default();
        let on_accept = match spec.get::<Option<Function>>("on_accept")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_reject = match spec.get::<Option<Function>>("on_reject")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let path = resolve_path(&shared, &raw_path)?;
        let proposal_id = {
            let mut s = shared.borrow_mut();
            let id = s.next_task;
            s.next_task += 1;
            s.proposals.push(PendingProposal {
                id,
                path: path.clone(),
                on_accept,
                on_reject,
            });
            s.actions.push(HostAction::ProposeEdit {
                proposal_id: id,
                path,
                text,
                title,
            });
            id
        };
        Ok(proposal_id)
    })
}

fn stat_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, path: String| {
        let resolved = resolve_path(&shared, &path)?;
        let table = lua.create_table()?;
        match std::fs::metadata(&resolved) {
            Ok(meta) => {
                table.set("exists", true)?;
                table.set("is_file", meta.is_file())?;
                table.set("is_dir", meta.is_dir())?;
                table.set("size", meta.len())?;
                table.set("readonly", meta.permissions().readonly())?;
            }
            Err(_) => {
                table.set("exists", false)?;
                table.set("is_file", false)?;
                table.set("is_dir", false)?;
                table.set("size", 0u64)?;
                table.set("readonly", false)?;
            }
        }
        Ok(table)
    })
}

fn mkdir_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, path: String| {
        require_permission(&shared, "fs:write")?;
        let resolved = resolve_path(&shared, &path)?;
        std::fs::create_dir_all(&resolved)
            .map_err(|err| mlua::Error::runtime(format!("创建目录失败: {err}")))?;
        shared.borrow_mut().actions.push(HostAction::RefreshWorkspace);
        Ok(())
    })
}

fn delete_file_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, path: String| {
        require_permission(&shared, "fs:write")?;
        let resolved = resolve_path(&shared, &path)?;
        if resolved.is_dir() {
            return Err(mlua::Error::runtime("delete_file 不能删除目录"));
        }
        std::fs::remove_file(&resolved)
            .map_err(|err| mlua::Error::runtime(format!("删除文件失败: {err}")))?;
        shared.borrow_mut().actions.push(HostAction::RefreshWorkspace);
        Ok(())
    })
}

fn list_files_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, opts: Option<Table>| {
        let root = shared
            .borrow()
            .ctx
            .as_ref()
            .and_then(|ctx| ctx.root.clone())
            .ok_or_else(|| mlua::Error::runtime("当前未打开工作区"))?;
        let prefix: Option<String> = opts.as_ref().and_then(|t| t.get("prefix").ok()).flatten();
        let ext: Option<String> = opts.as_ref().and_then(|t| t.get("ext").ok()).flatten();
        let limit: usize = opts
            .as_ref()
            .and_then(|t| t.get::<Option<usize>>("limit").ok())
            .flatten()
            .unwrap_or(500)
            .min(MAX_LIST_FILES);

        let base = if let Some(pref) = &prefix {
            resolve_path(&shared, pref)?
        } else {
            root.clone()
        };
        let canon_root = std::fs::canonicalize(&root).unwrap_or(root);
        let mut results = Vec::new();
        let walker = ignore::WalkBuilder::new(&base)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .ignore(true)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build();
        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let p = entry.path();
            if let Some(ext_filter) = &ext {
                let target_ext = ext_filter.trim_start_matches('.');
                let matches = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(target_ext));
                if !matches {
                    continue;
                }
            }
            let canon_p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let rel = canon_p
                .strip_prefix(&canon_root)
                .unwrap_or(&canon_p)
                .to_string_lossy()
                .replace('\\', "/");
            results.push(rel);
            if results.len() >= limit {
                break;
            }
        }
        results.sort();
        let table = lua.create_table_with_capacity(results.len(), 0)?;
        for (idx, rel) in results.into_iter().enumerate() {
            table.set(idx + 1, rel)?;
        }
        Ok(table)
    })
}

fn search_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        let root = shared
            .borrow()
            .ctx
            .as_ref()
            .and_then(|ctx| ctx.root.clone())
            .ok_or_else(|| mlua::Error::runtime("当前未打开工作区"))?;
        let pattern: String = spec.get("pattern")?;
        if pattern.is_empty() {
            return Err(mlua::Error::runtime("search pattern 不能为空"));
        }
        let on_done: Function = spec.get("on_done")?;
        let query = SearchQuery {
            pattern,
            regex: spec.get::<Option<bool>>("regex")?.unwrap_or(false),
            case_sensitive: spec.get::<Option<bool>>("case_sensitive")?.unwrap_or(false),
            whole_word: spec.get::<Option<bool>>("whole_word")?.unwrap_or(false),
            filename_only: false,
            include: spec.get::<Option<String>>("include")?.unwrap_or_default(),
            exclude: spec.get::<Option<String>>("exclude")?.unwrap_or_default(),
            max_results: spec
                .get::<Option<usize>>("max_results")?
                .unwrap_or(100)
                .clamp(1, 500),
        };
        let compiled = query.build().map_err(mlua::Error::runtime)?;
        let callback = lua.create_registry_value(on_done)?;
        let req_cancel = Arc::new(AtomicBool::new(false));

        let (runtime, task, tx, plugin_cancel, repaint_ctx) = {
            let mut s = shared.borrow_mut();
            let task = s.next_task;
            s.next_task += 1;
            s.search_callbacks
                .push((task, PendingSearchCallbacks { on_done: callback }));
            (
                s.runtime,
                task,
                s.search_tx.clone(),
                s.cancel_all.clone(),
                s.repaint_ctx.clone(),
            )
        };

        let cancel_handle = CancelHandle {
            id: task,
            cancel: req_cancel.clone(),
        };

        std::thread::Builder::new()
            .name("kestrel-plugin-search".into())
            .spawn(move || {
                let canon_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
                let mut hits = Vec::new();
                let mut truncated = false;
                let walker = ignore::WalkBuilder::new(&root)
                    .hidden(true)
                    .git_ignore(true)
                    .git_exclude(true)
                    .ignore(true)
                    .filter_entry(|entry| entry.file_name() != ".git")
                    .build();
                'walk: for entry in walker.flatten() {
                    if plugin_cancel.load(Ordering::Relaxed) || req_cancel.load(Ordering::Relaxed) {
                        let _ = tx.send(SearchDone {
                            runtime,
                            task,
                            results: Vec::new(),
                            truncated: false,
                            error: Some("已取消".to_string()),
                        });
                        if let Some(ctx) = &repaint_ctx {
                            ctx.request_repaint();
                        }
                        return;
                    }
                    if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                        continue;
                    }
                    let path = entry.path();
                    let Ok(meta) = std::fs::metadata(path) else {
                        continue;
                    };
                    if meta.len() > 2 * 1024 * 1024 {
                        continue;
                    }
                    let canon_p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                    let rel = canon_p
                        .strip_prefix(&canon_root)
                        .unwrap_or(&canon_p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    if !query.include.is_empty() && !simple_glob_match(&query.include, &rel) {
                        continue;
                    }
                    if !query.exclude.is_empty() && simple_glob_match(&query.exclude, &rel) {
                        continue;
                    }
                    let Ok(content) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    for (line_idx, line) in content.lines().enumerate() {
                        if let Some(m) = compiled.find(line) {
                            let col = line[..m.start()].chars().count() as u32 + 1;
                            let preview: String = line.trim().chars().take(180).collect();
                            hits.push(SearchHitItem {
                                path: rel.clone(),
                                line: line_idx as u32 + 1,
                                column: col,
                                preview,
                            });
                            if hits.len() >= query.max_results {
                                truncated = true;
                                break 'walk;
                            }
                        }
                    }
                }
                let _ = tx.send(SearchDone {
                    runtime,
                    task,
                    results: hits,
                    truncated,
                    error: None,
                });
                if let Some(ctx) = &repaint_ctx {
                    ctx.request_repaint();
                }
            })
            .map_err(|err| mlua::Error::runtime(format!("启动搜索线程失败: {err}")))?;

        Ok(cancel_handle)
    })
}

fn simple_glob_match(pattern: &str, text: &str) -> bool {
    for part in pattern.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(suffix) = part.strip_prefix("*.") {
            if text.ends_with(&format!(".{suffix}")) {
                return true;
            }
        } else if text.contains(part.trim_matches('*')) {
            return true;
        }
    }
    false
}

fn run_command_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, id: String| {
        let command = Command::from_id(&id)
            .ok_or_else(|| mlua::Error::runtime(format!("未知命令: {id}")))?;
        shared
            .borrow_mut()
            .actions
            .push(HostAction::RunBuiltin(command));
        Ok(())
    })
}

/// Holds one of the [`MAX_CONCURRENT_EXEC`] slots; the exec thread releases it on exit.
struct ExecSlot(Arc<AtomicUsize>);

impl Drop for ExecSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn exec_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        require_permission(&shared, "exec")?;
        forbid_in_before_save(&shared, "kestrel.exec")?;

        let program: String = spec.get("program")?;
        let args: Vec<String> = match spec.get::<Option<Table>>("args")? {
            Some(table) => table
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let stdin: Option<String> = spec.get("stdin")?;
        let cwd: Option<PathBuf> = match spec.get::<Option<String>>("cwd")? {
            Some(raw) => Some(resolve_path(&shared, &raw)?),
            None => shared.borrow().ctx.as_ref().and_then(|ctx| ctx.root.clone()),
        };
        let mut env = Vec::new();
        if let Some(env_tbl) = spec.get::<Option<Table>>("env")? {
            for pair in env_tbl.pairs::<String, String>() {
                env.push(pair?);
            }
        }
        let timeout_ms: Option<u64> = spec.get("timeout_ms")?;
        let on_stdout = match spec.get::<Option<Function>>("on_stdout")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_stderr = match spec.get::<Option<Function>>("on_stderr")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_done = match spec.get::<Option<Function>>("on_done")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let stream_out = on_stdout.is_some();
        let stream_err = on_stderr.is_some();
        let req_cancel = Arc::new(AtomicBool::new(false));

        let (runtime, task, tx, running, plugin_cancel, repaint_ctx) = {
            let mut s = shared.borrow_mut();
            if s.exec_running.fetch_add(1, Ordering::Relaxed) >= MAX_CONCURRENT_EXEC {
                s.exec_running.fetch_sub(1, Ordering::Relaxed);
                return Err(mlua::Error::runtime(format!(
                    "kestrel.exec 并发上限 {MAX_CONCURRENT_EXEC}，本次调用被拒绝"
                )));
            }
            let task = s.next_task;
            s.next_task += 1;
            s.exec_callbacks.push((
                task,
                PendingExecCallbacks {
                    on_stdout,
                    on_stderr,
                    on_done,
                    cancel: req_cancel.clone(),
                },
            ));
            (
                s.runtime,
                task,
                s.exec_tx.clone(),
                s.exec_running.clone(),
                s.cancel_all.clone(),
                s.repaint_ctx.clone(),
            )
        };
        let slot = ExecSlot(running);
        let handle = CancelHandle {
            id: task,
            cancel: req_cancel.clone(),
        };
        if let Err(err) = std::thread::Builder::new()
            .name("kestrel-plugin-exec".into())
            .spawn(move || {
                let _slot = slot;
                run_process(
                    runtime,
                    task,
                    &program,
                    &args,
                    stdin.as_deref(),
                    cwd.as_deref(),
                    &env,
                    timeout_ms,
                    stream_out,
                    stream_err,
                    &plugin_cancel,
                    &req_cancel,
                    &tx,
                    repaint_ctx.as_ref(),
                );
            })
        {
            shared
                .borrow_mut()
                .exec_callbacks
                .retain(|(id, _)| *id != task);
            return Err(mlua::Error::runtime(format!("启动外部命令线程失败: {err}")));
        }
        Ok(handle)
    })
}

fn http_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        require_permission(&shared, "network")?;
        forbid_in_before_save(&shared, "kestrel.http")?;

        let url: String = spec.get("url")?;
        let method: String = spec
            .get::<Option<String>>("method")?
            .unwrap_or_else(|| "GET".to_string());
        let mut headers = Vec::new();
        if let Some(tbl) = spec.get::<Option<Table>>("headers")? {
            for pair in tbl.pairs::<String, String>() {
                headers.push(pair?);
            }
        }
        let body: Option<String> = match spec.get::<Value>("body")? {
            Value::Nil => None,
            Value::String(s) => Some(s.to_string_lossy()),
            tbl @ Value::Table(_) => {
                let json = lua_to_json(&tbl)?;
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("Content-Type".to_string(), "application/json".to_string()));
                }
                Some(
                    serde_json::to_string(&json)
                        .map_err(|err| mlua::Error::runtime(format!("JSON 编码失败: {err}")))?,
                )
            }
            other => {
                return Err(mlua::Error::runtime(format!(
                    "http.body 必须是字符串或表，收到 {}",
                    other.type_name()
                )));
            }
        };
        let timeout_ms: u32 = spec
            .get::<Option<u32>>("timeout_ms")?
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let on_chunk = match spec.get::<Option<Function>>("on_chunk")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_done: Function = spec.get("on_done")?;
        let done_key = lua.create_registry_value(on_done)?;
        let stream = on_chunk.is_some();
        let req_cancel = Arc::new(AtomicBool::new(false));

        let (runtime, task, tx, running, plugin_cancel, repaint_ctx) = {
            let mut s = shared.borrow_mut();
            let task = s.next_task;
            s.next_task += 1;
            s.http_callbacks.push((
                task,
                PendingHttpCallbacks {
                    on_chunk,
                    on_done: done_key,
                    cancel: req_cancel.clone(),
                },
            ));
            (
                s.runtime,
                task,
                s.http_tx.clone(),
                s.http_running.clone(),
                s.cancel_all.clone(),
                s.repaint_ctx.clone(),
            )
        };

        let req_spec = HttpRequestSpec {
            runtime,
            task,
            method,
            url,
            headers,
            body,
            timeout_ms,
            stream,
        };
        if let Err(err) = super::http::spawn_request(
            req_spec,
            tx,
            running,
            plugin_cancel,
            req_cancel.clone(),
            repaint_ctx,
        ) {
            shared
                .borrow_mut()
                .http_callbacks
                .retain(|(id, _)| *id != task);
            return Err(mlua::Error::runtime(err));
        }
        Ok(CancelHandle {
            id: task,
            cancel: req_cancel,
        })
    })
}

struct RpcHandle(RpcControl);

impl UserData for RpcHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("id", |_, this, ()| Ok(this.0.id));
        methods.add_method("is_alive", |_, this, ()| Ok(this.0.is_alive()));
        methods.add_method("send", |_, this, val: Value| {
            let json = lua_to_json(&val)?;
            this.0.send_json(&json).map_err(mlua::Error::runtime)
        });
        methods.add_method("send_raw", |_, this, text: String| {
            this.0.send_raw(&text).map_err(mlua::Error::runtime)
        });
        methods.add_method("kill", |_, this, ()| {
            this.0.kill();
            Ok(())
        });
    }
}

fn spawn_rpc_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        require_permission(&shared, "rpc")?;
        forbid_in_before_save(&shared, "kestrel.spawn_rpc")?;

        let program: String = spec.get("program")?;
        let args: Vec<String> = match spec.get::<Option<Table>>("args")? {
            Some(table) => table
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let cwd: Option<PathBuf> = match spec.get::<Option<String>>("cwd")? {
            Some(raw) => Some(resolve_path(&shared, &raw)?),
            None => shared.borrow().ctx.as_ref().and_then(|ctx| ctx.root.clone()),
        };
        let mut env = Vec::new();
        if let Some(env_tbl) = spec.get::<Option<Table>>("env")? {
            for pair in env_tbl.pairs::<String, String>() {
                env.push(pair?);
            }
        }
        let framing = RpcFraming::parse(spec.get::<Option<String>>("framing")?.as_deref())
            .map_err(mlua::Error::runtime)?;
        let on_message = match spec.get::<Option<Function>>("on_message")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_stderr = match spec.get::<Option<Function>>("on_stderr")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        let on_exit = match spec.get::<Option<Function>>("on_exit")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };

        let (runtime, id, tx, running, plugin_cancel, repaint_ctx) = {
            let mut s = shared.borrow_mut();
            let id = s.next_task;
            s.next_task += 1;
            (
                s.runtime,
                id,
                s.rpc_tx.clone(),
                s.rpc_running.clone(),
                s.cancel_all.clone(),
                s.repaint_ctx.clone(),
            )
        };

        let control = super::rpc::spawn_rpc(
            RpcSpawnSpec {
                runtime,
                id,
                program,
                args,
                cwd,
                env,
                framing,
            },
            tx,
            running,
            plugin_cancel,
            repaint_ctx,
        )
        .map_err(mlua::Error::runtime)?;

        shared.borrow_mut().rpc_callbacks.push((
            id,
            PendingRpcCallbacks {
                on_message,
                on_stderr,
                on_exit,
                control: control.clone(),
            },
        ));
        Ok(RpcHandle(control))
    })
}

fn terminal_run_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, spec: Table| {
        require_permission(&shared, "terminal")?;
        let command: String = spec.get("command")?;
        let name: Option<String> = spec.get("name")?;
        let cwd: Option<PathBuf> = match spec.get::<Option<String>>("cwd")? {
            Some(raw) => Some(resolve_path(&shared, &raw)?),
            None => None,
        };
        let show: bool = spec.get::<Option<bool>>("show")?.unwrap_or(true);
        shared.borrow_mut().actions.push(HostAction::TerminalRun {
            name,
            command,
            cwd,
            show,
        });
        Ok(())
    })
}

fn terminal_send_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (name, text): (Option<String>, String)| {
        require_permission(&shared, "terminal")?;
        shared
            .borrow_mut()
            .actions
            .push(HostAction::TerminalSend { name, text });
        Ok(())
    })
}

fn terminal_list_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, ()| {
        require_permission(&shared, "terminal")?;
        let s = shared.borrow();
        let table = lua.create_table()?;
        if let Some(ctx) = &s.ctx {
            for (idx, term) in ctx.terminals.iter().enumerate() {
                let item = lua.create_table()?;
                item.set("id", term.id)?;
                item.set("title", term.title.as_str())?;
                item.set("cwd", term.cwd.to_string_lossy().to_string())?;
                item.set("alive", term.alive)?;
                table.set(idx + 1, item)?;
            }
        }
        Ok(table)
    })
}

fn terminal_read_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (name, max_lines): (Option<String>, Option<usize>)| {
        require_permission(&shared, "terminal")?;
        let s = shared.borrow();
        let Some(ctx) = &s.ctx else {
            return Ok(None);
        };
        let target = match name.as_deref() {
            Some(name) => ctx.terminals.iter().find(|t| t.title == name),
            None => ctx.terminals.last(),
        };
        let Some(term) = target else {
            return Ok(None);
        };
        let text = if let Some(limit) = max_lines {
            let lines: Vec<&str> = term.tail.lines().collect();
            let start = lines.len().saturating_sub(limit);
            lines[start..].join("\n")
        } else {
            term.tail.clone()
        };
        Ok(Some(text))
    })
}

fn register_view_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, spec: Table| {
        require_permission(&shared, "ui")?;
        let id: String = spec.get("id")?;
        if id.is_empty()
            || !id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            return Err(mlua::Error::runtime(format!("非法视图 id: {id}")));
        }
        let title: String = spec
            .get::<Option<String>>("title")?
            .unwrap_or_else(|| id.clone());
        let location = ViewLocation::parse(spec.get::<Option<String>>("location")?.as_deref());
        let on_event = match spec.get::<Option<Function>>("on_event")? {
            Some(f) => Some(lua.create_registry_value(f)?),
            None => None,
        };
        shared.borrow_mut().view_regs.push(PendingViewReg {
            id,
            title,
            location,
            on_event,
        });
        Ok(())
    })
}

fn set_view_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (id, nodes_table): (String, Table)| {
        require_permission(&shared, "ui")?;
        let nodes = parse_nodes(&nodes_table)?;
        shared
            .borrow_mut()
            .view_updates
            .push(PendingViewUpdate { id, nodes });
        Ok(())
    })
}

fn open_view_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, id: String| {
        require_permission(&shared, "ui")?;
        shared.borrow_mut().actions.push(HostAction::OpenView(id));
        Ok(())
    })
}

fn close_view_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, id: String| {
        require_permission(&shared, "ui")?;
        shared.borrow_mut().actions.push(HostAction::CloseView(id));
        Ok(())
    })
}

fn config_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |lua, (key, default): (String, Option<Value>)| {
        let stored = shared.borrow().config.get(&key).cloned();
        match stored {
            Some(value) => json_to_lua(lua, &value),
            None => Ok(default.unwrap_or(Value::Nil)),
        }
    })
}

fn set_config_fn(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    lua.create_function(move |_, (key, value): (String, Value)| {
        let json = lua_to_json(&value)?;
        let mut shared = shared.borrow_mut();
        shared.config.insert(key.clone(), json.clone());
        let plugin = shared.plugin_id.clone();
        shared.actions.push(HostAction::SetPluginConfig {
            plugin,
            key,
            value: json,
        });
        Ok(())
    })
}

fn resolve_path(shared: &Rc<RefCell<PluginShared>>, raw: &str) -> mlua::Result<PathBuf> {
    let input = PathBuf::from(raw);
    let root = shared
        .borrow()
        .ctx
        .as_ref()
        .and_then(|ctx| ctx.root.clone());
    let Some(root) = root else {
        if input.is_absolute() {
            return Ok(input);
        }
        return Err(mlua::Error::runtime("当前未打开工作区，无法解析相对路径"));
    };
    let candidate = if input.is_absolute() {
        input
    } else {
        root.join(input)
    };
    let canon_root = std::fs::canonicalize(&root).unwrap_or(root);
    let canon_target = if candidate.exists() {
        std::fs::canonicalize(&candidate).unwrap_or_else(|_| normalize_lexical(&candidate))
    } else if let Some(parent) = candidate.parent()
        && let Some(name) = candidate.file_name()
        && let Ok(canon_parent) = std::fs::canonicalize(parent)
    {
        canon_parent.join(name)
    } else {
        normalize_lexical(&candidate)
    };
    if !canon_target.starts_with(&canon_root) {
        return Err(mlua::Error::runtime(format!(
            "插件文件访问越界（限制在当前工作区内）: {raw}"
        )));
    }
    Ok(canon_target)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

type PipeOutput = (Arc<Mutex<Vec<u8>>>, JoinHandle<()>);

/// Drain one child pipe on its own thread, optionally forwarding live chunks.
#[allow(clippy::too_many_arguments)]
fn drain_pipe(
    mut pipe: impl Read + Send + 'static,
    runtime: u64,
    task: u64,
    is_stderr: bool,
    stream: bool,
    tx: std::sync::mpsc::Sender<ExecEvent>,
    repaint_ctx: Option<egui::Context>,
) -> PipeOutput {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = buffer.clone();
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(read) = pipe.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let slice = &chunk[..read];
            if let Ok(mut buffer) = sink.lock()
                && buffer.len() + slice.len() <= 8 * 1024 * 1024
            {
                buffer.extend_from_slice(slice);
            }
            if stream {
                let text = String::from_utf8_lossy(slice).into_owned();
                let ev = if is_stderr {
                    ExecEvent::Stderr {
                        runtime,
                        task,
                        chunk: text,
                    }
                } else {
                    ExecEvent::Stdout {
                        runtime,
                        task,
                        chunk: text,
                    }
                };
                let _ = tx.send(ev);
                if let Some(ctx) = &repaint_ctx {
                    ctx.request_repaint();
                }
            }
        }
    });
    (buffer, handle)
}

fn collect_pipe(pipe: Option<PipeOutput>) -> String {
    let Some((buffer, handle)) = pipe else {
        return String::new();
    };
    let _ = handle.join();
    buffer
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn run_process(
    runtime: u64,
    task: u64,
    program: &str,
    args: &[String],
    stdin: Option<&str>,
    cwd: Option<&Path>,
    env: &[(String, String)],
    timeout_ms: Option<u64>,
    stream_out: bool,
    stream_err: bool,
    plugin_cancel: &AtomicBool,
    req_cancel: &AtomicBool,
    tx: &std::sync::mpsc::Sender<ExecEvent>,
    repaint_ctx: Option<&egui::Context>,
) {
    use std::io::Write;
    use std::process::{Command as Process, Stdio};

    let finish = |code: i32, stdout: String, stderr: String, error: Option<String>| {
        let _ = tx.send(ExecEvent::Done {
            runtime,
            task,
            code,
            stdout,
            stderr,
            error,
        });
        if let Some(ctx) = repaint_ctx {
            ctx.request_repaint();
        }
    };

    if plugin_cancel.load(Ordering::Relaxed) || req_cancel.load(Ordering::Relaxed) {
        finish(-1, String::new(), String::new(), Some("已取消".to_string()));
        return;
    }

    let mut command = Process::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    for (k, v) in env {
        command.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            finish(-1, String::new(), String::new(), Some(err.to_string()));
            return;
        }
    };
    let stdout = child.stdout.take().map(|p| {
        drain_pipe(
            p,
            runtime,
            task,
            false,
            stream_out,
            tx.clone(),
            repaint_ctx.cloned(),
        )
    });
    let stderr = child.stderr.take().map(|p| {
        drain_pipe(
            p,
            runtime,
            task,
            true,
            stream_err,
            tx.clone(),
            repaint_ctx.cloned(),
        )
    });
    if let Some(mut pipe) = child.stdin.take()
        && let Some(input) = stdin
    {
        let _ = pipe.write_all(input.as_bytes());
    }
    let started = Instant::now();
    let mut cancelled = false;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                finish(
                    -1,
                    collect_pipe(stdout),
                    collect_pipe(stderr),
                    Some(err.to_string()),
                );
                return;
            }
        }
        if !cancelled
            && (plugin_cancel.load(Ordering::Relaxed) || req_cancel.load(Ordering::Relaxed))
        {
            cancelled = true;
            let _ = child.kill();
        } else if !timed_out
            && let Some(limit) = timeout_ms
            && started.elapsed() >= Duration::from_millis(limit)
        {
            timed_out = true;
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let error = if cancelled {
        Some("已取消".to_string())
    } else if timed_out {
        Some("执行超时".to_string())
    } else {
        None
    };
    finish(
        status.and_then(|s| s.code()).unwrap_or(-1),
        collect_pipe(stdout),
        collect_pipe(stderr),
        error,
    );
}

/// Document handle passed to Lua. Reads come from the invocation snapshot;
/// writes are queued as host actions.
pub(crate) struct DocHandle {
    snapshot: Rc<DocSnapshot>,
    shared: Rc<RefCell<PluginShared>>,
}

impl DocHandle {
    pub(crate) fn new(snapshot: Rc<DocSnapshot>, shared: Rc<RefCell<PluginShared>>) -> Self {
        Self { snapshot, shared }
    }

    fn push(&self, action: HostAction) {
        self.shared.borrow_mut().actions.push(action);
    }

    fn rope(&self) -> mlua::Result<&ropey::Rope> {
        self.snapshot
            .rope
            .as_ref()
            .ok_or_else(|| mlua::Error::runtime("文档已关闭"))
    }
}

impl UserData for DocHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("id", |_, this, ()| Ok(this.snapshot.id));
        methods.add_method("path", |_, this, ()| {
            Ok(this
                .snapshot
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()))
        });
        methods.add_method("language", |_, this, ()| Ok(this.snapshot.language.clone()));
        methods.add_method("text", |_, this, ()| {
            let rope = this.rope()?;
            if rope.len_bytes() > MAX_TEXT_BYTES {
                return Err(mlua::Error::runtime(format!(
                    "文档大于 {}MB，请用 doc:text_range 分段读取",
                    MAX_TEXT_BYTES / 1_000_000
                )));
            }
            Ok(rope.to_string())
        });
        methods.add_method("text_range", |_, this, (start, end): (usize, usize)| {
            let rope = this.rope()?;
            if start > end || end > this.snapshot.char_len {
                return Err(mlua::Error::runtime("text_range 区间越界"));
            }
            Ok(rope.slice(start..end).to_string())
        });
        methods.add_method("line", |_, this, line: usize| {
            let rope = this.rope()?;
            if line >= this.snapshot.line_count {
                return Err(mlua::Error::runtime("行号越界"));
            }
            let text = rope.line(line).to_string();
            Ok(text.trim_end_matches(['\n', '\r']).to_string())
        });
        methods.add_method("line_count", |_, this, ()| Ok(this.snapshot.line_count));
        methods.add_method("caret", |_, this, ()| Ok(this.snapshot.caret));
        methods.add_method("selection", |_, this, ()| {
            Ok((
                this.snapshot.caret.min(this.snapshot.anchor),
                this.snapshot.caret.max(this.snapshot.anchor),
            ))
        });
        methods.add_method("is_dirty", |_, this, ()| Ok(this.snapshot.dirty));
        methods.add_method("diagnostics", |lua, this, ()| {
            let table = lua.create_table_with_capacity(this.snapshot.diagnostics.len(), 0)?;
            for (idx, d) in this.snapshot.diagnostics.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("line", d.line)?;
                row.set("col", d.col)?;
                row.set("end_col", d.end_col)?;
                row.set(
                    "severity",
                    match d.severity {
                        DiagSeverity::Error => "error",
                        DiagSeverity::Warning => "warning",
                        DiagSeverity::Info => "info",
                    },
                )?;
                row.set("message", d.message.as_str())?;
                row.set("source", d.source.as_str())?;
                table.set(idx + 1, row)?;
            }
            Ok(table)
        });
        methods.add_method("outline", |lua, this, ()| {
            let table = lua.create_table_with_capacity(this.snapshot.symbols.len(), 0)?;
            for (idx, s) in this.snapshot.symbols.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("name", s.name.as_str())?;
                row.set("detail", s.detail.as_str())?;
                row.set("kind", s.kind.label())?;
                row.set("start_line", s.start_line)?;
                row.set("end_line", s.end_line)?;
                row.set("depth", s.depth)?;
                table.set(idx + 1, row)?;
            }
            Ok(table)
        });
        methods.add_method("set_text", |_, this, text: String| {
            this.push(HostAction::Edit {
                doc: this.snapshot.id,
                start: 0,
                end: this.snapshot.char_len,
                text,
                caret_after: Some(this.snapshot.caret),
            });
            Ok(())
        });
        methods.add_method("insert", |_, this, text: String| {
            let start = this.snapshot.caret.min(this.snapshot.anchor);
            let end = this.snapshot.caret.max(this.snapshot.anchor);
            let caret = start + text.chars().count();
            this.push(HostAction::Edit {
                doc: this.snapshot.id,
                start,
                end,
                text,
                caret_after: Some(caret),
            });
            Ok(())
        });
        methods.add_method(
            "replace",
            |_, this, (start, end, text): (usize, usize, String)| {
                if start > end || end > this.snapshot.char_len {
                    return Err(mlua::Error::runtime("replace 区间越界"));
                }
                let caret = start + text.chars().count();
                this.push(HostAction::Edit {
                    doc: this.snapshot.id,
                    start,
                    end,
                    text,
                    caret_after: Some(caret),
                });
                Ok(())
            },
        );
        methods.add_method("set_selection", |_, this, (start, end): (usize, usize)| {
            if start > end || end > this.snapshot.char_len {
                return Err(mlua::Error::runtime("set_selection 区间越界"));
            }
            this.push(HostAction::SetSelection {
                doc: this.snapshot.id,
                start,
                end,
            });
            Ok(())
        });
        methods.add_method("save", |_, this, ()| {
            this.push(HostAction::SaveDoc(this.snapshot.id));
            Ok(())
        });
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(flag) => flag.to_string(),
        Value::Integer(number) => number.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.to_string_lossy(),
        other => format!("<{}>", other.type_name()),
    }
}

pub(crate) fn lua_to_json(value: &Value) -> mlua::Result<serde_json::Value> {
    use serde_json::Value as Json;
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(flag) => Json::Bool(*flag),
        Value::Integer(number) => Json::from(*number),
        Value::Number(number) => Json::from(*number),
        Value::String(text) => Json::String(text.to_string_lossy()),
        Value::Table(table) => {
            let pairs: Vec<(Value, Value)> = table
                .pairs::<Value, Value>()
                .collect::<mlua::Result<Vec<_>>>()?;
            let is_array = !pairs.is_empty()
                && pairs.iter().enumerate().all(|(index, (key, _))| {
                    matches!(key, Value::Integer(number) if *number as usize == index + 1)
                });
            if is_array {
                Json::Array(
                    pairs
                        .iter()
                        .map(|(_, value)| lua_to_json(value))
                        .collect::<mlua::Result<Vec<_>>>()?,
                )
            } else {
                let mut object = serde_json::Map::new();
                for (key, value) in &pairs {
                    let Value::String(key) = key else {
                        return Err(mlua::Error::runtime("表的键必须是字符串"));
                    };
                    object.insert(key.to_string_lossy(), lua_to_json(value)?);
                }
                Json::Object(object)
            }
        }
        other => {
            return Err(mlua::Error::runtime(format!(
                "不支持的值类型: {}",
                other.type_name()
            )));
        }
    })
}

pub(crate) fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as Json;
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(flag) => Value::Boolean(*flag),
        Json::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(integer), _) => Value::Integer(integer),
            (None, Some(float)) => Value::Number(float),
            _ => Value::Nil,
        },
        Json::String(text) => Value::String(lua.create_string(text)?),
        Json::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (index, item) in items.iter().enumerate() {
                table.set(index + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        Json::Object(map) => {
            let table = lua.create_table_with_capacity(0, map.len())?;
            for (key, item) in map {
                table.set(key.as_str(), json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
    })
}

pub(crate) fn headers_to_lua(lua: &Lua, headers: &HashMap<String, String>) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, headers.len())?;
    for (k, v) in headers {
        table.set(k.as_str(), v.as_str())?;
    }
    Ok(table)
}
