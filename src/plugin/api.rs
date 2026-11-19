//! The Lua host API exposed to plugins as the global `kestrel` table, plus the
//! document userdata handed to callbacks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};

use super::actions::{DocSnapshot, HostAction};
use super::{ExecDone, Event, PendingCommand, PluginShared};
use crate::commands::{Command, Shortcut};

/// A single `doc:text()` call refuses documents above this size; use ranges instead.
const MAX_TEXT_BYTES: usize = 1_500_000;

pub(crate) fn install(lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<()> {
    let kestrel = lua.create_table()?;
    kestrel.set("command", command_fn(lua, shared)?)?;
    kestrel.set("on", on_fn(lua, shared)?)?;
    kestrel.set("active_doc", active_doc_fn(lua, shared)?)?;
    kestrel.set("notify", notify_fn(lua, shared)?)?;
    kestrel.set("log", log_fn(lua, shared)?)?;
    kestrel.set("status_item", status_fn(lua, shared)?)?;
    kestrel.set("root", root_fn(lua, shared)?)?;
    kestrel.set("open_file", open_file_fn(lua, shared)?)?;
    kestrel.set("read_file", read_file_fn(lua, shared)?)?;
    kestrel.set("write_file", write_file_fn(lua, shared)?)?;
    kestrel.set("run_command", run_command_fn(lua, shared)?)?;
    kestrel.set("exec", exec_fn(lua, shared)?)?;
    kestrel.set("config", config_fn(lua, shared)?)?;
    kestrel.set("set_config", set_config_fn(lua, shared)?)?;
    lua.globals().set("kestrel", kestrel)?;

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
                Some(key) => Some(Shortcut::parse(&key).ok_or_else(|| {
                    mlua::Error::runtime(format!("无法解析快捷键: {key}"))
                })?),
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

fn on_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |lua, (name, handler): (String, Function)| {
        let event = Event::parse(&name)
            .ok_or_else(|| mlua::Error::runtime(format!("未知事件: {name}")))?;
        if event == Event::Changed && !shared.borrow().changed_allowed {
            return Err(mlua::Error::runtime(
                "document_changed 需要在 plugin.json 的 events 里声明",
            ));
        }
        let key = lua.create_registry_value(handler)?;
        shared.borrow_mut().events.push((event, key));
        Ok(())
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

fn notify_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, (text, level): (String, Option<String>)| {
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

fn log_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, text: String| {
        shared.borrow_mut().logs.push(text);
        Ok(())
    })
}

fn status_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(
        move |_, (key, text, command): (String, String, Option<String>)| {
            shared.borrow_mut().status.push((key, text, command));
            Ok(())
        },
    )
}

fn root_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, ()| {
        let root = shared
            .borrow()
            .ctx
            .as_ref()
            .and_then(|ctx| ctx.root.as_ref())
            .map(|path| path.to_string_lossy().to_string());
        Ok(root)
    })
}

fn open_file_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, path: String| {
        let path = resolve_path(&shared, &path);
        shared.borrow_mut().actions.push(HostAction::OpenFile(path));
        Ok(())
    })
}

fn read_file_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, path: String| {
        let path = resolve_path(&shared, &path);
        std::fs::read_to_string(&path)
            .map_err(|err| mlua::Error::runtime(format!("读取 {} 失败: {err}", path.display())))
    })
}

fn write_file_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, (path, text): (String, String)| {
        let path = resolve_path(&shared, &path);
        std::fs::write(&path, text)
            .map_err(|err| mlua::Error::runtime(format!("写入 {} 失败: {err}", path.display())))
    })
}

fn run_command_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, id: String| {
        let command = Command::from_id(&id)
            .ok_or_else(|| mlua::Error::runtime(format!("未知命令: {id}")))?;
        shared
            .borrow_mut()
            .actions
            .push(HostAction::RunBuiltin(command));
        Ok(())
    })
}

fn exec_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |lua, spec: Table| {
        let program: String = spec.get("program")?;
        let args: Vec<String> = match spec.get::<Option<Table>>("args")? {
            Some(table) => table
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let stdin: Option<String> = spec.get("stdin")?;
        let on_done: Function = spec.get("on_done")?;
        let callback = lua.create_registry_value(on_done)?;
        let (runtime, task, tx) = {
            let mut shared = shared.borrow_mut();
            let task = shared.next_task;
            shared.next_task += 1;
            shared.exec_callbacks.push((task, callback));
            (shared.runtime, task, shared.exec_tx.clone())
        };
        std::thread::spawn(move || {
            let done = run_process(runtime, task, &program, &args, stdin.as_deref());
            let _ = tx.send(done);
        });
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

fn set_config_fn(_lua: &Lua, shared: &Rc<RefCell<PluginShared>>) -> mlua::Result<Function> {
    let shared = shared.clone();
    _lua.create_function(move |_, (key, value): (String, Value)| {
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

fn resolve_path(shared: &Rc<RefCell<PluginShared>>, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }
    let root = shared
        .borrow()
        .ctx
        .as_ref()
        .and_then(|ctx| ctx.root.clone());
    match root {
        Some(root) => root.join(path),
        None => path,
    }
}

fn run_process(
    runtime: u64,
    task: u64,
    program: &str,
    args: &[String],
    stdin: Option<&str>,
) -> ExecDone {
    use std::io::Write;
    use std::process::{Command as Process, Stdio};
    let mut command = Process::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ExecDone {
                runtime,
                task,
                code: -1,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(err.to_string()),
            };
        }
    };
    if let Some(mut pipe) = child.stdin.take() {
        if let Some(input) = stdin {
            let _ = pipe.write_all(input.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(output) => ExecDone {
            runtime,
            task,
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            error: None,
        },
        Err(err) => ExecDone {
            runtime,
            task,
            code: -1,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(err.to_string()),
        },
    }
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
            Ok(text
                .trim_end_matches(['\n', '\r'])
                .to_string())
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
        methods.add_method("replace", |_, this, (start, end, text): (usize, usize, String)| {
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
        });
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

fn lua_to_json(value: &Value) -> mlua::Result<serde_json::Value> {
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
                        return Err(mlua::Error::runtime("配置表的键必须是字符串"));
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

fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
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
