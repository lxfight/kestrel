//! Plugin manifests (`plugin.json`) and plugin directory scanning.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const API_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(rename = "apiVersion")]
    pub api_version: u32,
    pub main: Option<String>,
    /// Events the plugin may subscribe to beyond the default set.
    pub events: Vec<String>,
    /// Explicit capability grants (`network`, `exec`, `terminal`, `ui`, `rpc`, `fs:write`).
    /// When omitted (`None`), legacy v1 capabilities (`exec`, `fs:write`) remain allowed
    /// while new high-privilege capabilities (`network`, `terminal`, `ui`, `rpc`) require
    /// explicit declaration.
    pub permissions: Option<Vec<String>>,
}

impl Manifest {
    pub fn permissions_list(&self) -> &[String] {
        self.permissions.as_deref().unwrap_or(&[])
    }

    pub fn allows(&self, perm: &str) -> bool {
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

pub struct Discovered {
    pub dir: PathBuf,
    pub manifest: Option<Manifest>,
    pub error: Option<String>,
}

/// Read every `<root>/<dir>/plugin.json`. Directories without a manifest are skipped.
pub fn discover(root: &Path) -> (Vec<Discovered>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (found, errors);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("plugin.json");
        if !manifest_path.is_file() {
            continue;
        }
        let fallback = || Manifest {
            id: dir_name(&dir),
            name: dir_name(&dir),
            version: "0.0.0".to_string(),
            api_version: API_VERSION,
            ..Manifest::default()
        };
        match std::fs::read_to_string(&manifest_path) {
            Ok(text) => match serde_json::from_str::<Manifest>(&text) {
                Ok(mut manifest) => match validate(&mut manifest, &dir) {
                    Ok(()) => found.push(Discovered {
                        dir,
                        manifest: Some(manifest),
                        error: None,
                    }),
                    Err(err) => {
                        errors.push(format!("{}: {err}", manifest_path.display()));
                        found.push(Discovered {
                            dir,
                            manifest: Some(manifest),
                            error: Some(err),
                        });
                    }
                },
                Err(err) => {
                    errors.push(format!("{}: 清单解析失败: {err}", manifest_path.display()));
                    let mut manifest = fallback();
                    manifest.id = dir_name(&dir);
                    found.push(Discovered {
                        dir,
                        manifest: Some(manifest),
                        error: Some(format!("清单解析失败: {err}")),
                    });
                }
            },
            Err(err) => errors.push(format!("{}: 读取失败: {err}", manifest_path.display())),
        }
    }
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    (found, errors)
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin")
        .to_ascii_lowercase()
}

fn validate(manifest: &mut Manifest, dir: &Path) -> Result<(), String> {
    if manifest.id.is_empty() {
        manifest.id = dir_name(dir);
    }
    if !manifest
        .id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("id 只能包含小写字母、数字和 . _ -".to_string());
    }
    if manifest.name.is_empty() {
        manifest.name = manifest.id.clone();
    }
    if manifest.version.is_empty() {
        manifest.version = "0.0.0".to_string();
    }
    if manifest.api_version == 0 {
        manifest.api_version = API_VERSION;
    }
    if manifest.api_version > API_VERSION {
        return Err(format!(
            "apiVersion {} 高于宿主支持的 {API_VERSION}",
            manifest.api_version
        ));
    }
    if let Some(main) = &manifest.main
        && !dir.join(main).is_file()
    {
        return Err(format!("入口文件不存在: {main}"));
    }
    for event in &manifest.events {
        if !event_name_known(event) {
            return Err(format!("未知事件: {event}"));
        }
    }
    if let Some(perms) = &manifest.permissions {
        for perm in perms {
            if !permission_name_known(perm) {
                return Err(format!("未知权限声明: {perm}"));
            }
        }
    }
    Ok(())
}

pub fn event_name_known(name: &str) -> bool {
    matches!(
        name,
        "startup" | "opened" | "before_save" | "saved" | "closed" | "changed" | "shutdown"
    )
}

pub fn permission_name_known(name: &str) -> bool {
    matches!(
        name,
        "network" | "exec" | "terminal" | "ui" | "rpc" | "fs:write"
    )
}
