//! Shell discovery, profile resolution, spawning, path quoting, and safe URL launching.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use portable_pty::{Child, CommandBuilder, SlavePty};

/// User-defined custom shell executable and launch arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomShellConfig {
    pub program: String,
    pub args: Vec<String>,
}

impl CustomShellConfig {
    pub fn from_settings(program: &str, args_line: &str) -> Option<Self> {
        let program = program.trim();
        if program.is_empty() {
            return None;
        }
        let args = split_shell_args(args_line);
        Some(Self {
            program: program.to_string(),
            args,
        })
    }
}

/// Parse a simple whitespace/quote-delimited argument string from settings.
pub fn split_shell_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    Default,
    PowerShell7,
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
    Zsh,
    Bash,
    Fish,
    Sh,
    Custom,
}

#[allow(dead_code)]
impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => {
                if cfg!(windows) {
                    "默认 (自动)"
                } else {
                    "默认 Shell"
                }
            }
            Self::PowerShell7 => "PowerShell 7",
            Self::PowerShell => "Windows PowerShell",
            Self::Cmd => "Command Prompt",
            Self::GitBash => "Git Bash",
            Self::Wsl => "WSL",
            Self::Zsh => "Zsh",
            Self::Bash => "Bash",
            Self::Fish => "Fish",
            Self::Sh => "sh",
            Self::Custom => "自定义 Shell",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::PowerShell7 => "pwsh",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
            Self::GitBash => "gitbash",
            Self::Wsl => "wsl",
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
            Self::Sh => "sh",
            Self::Custom => "custom",
        }
    }

    pub fn from_str(name: &str) -> Self {
        match name {
            "pwsh" => Self::PowerShell7,
            "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            "gitbash" => Self::GitBash,
            "wsl" => Self::Wsl,
            "zsh" => Self::Zsh,
            "bash" => Self::Bash,
            "fish" => Self::Fish,
            "sh" => Self::Sh,
            "custom" => Self::Custom,
            _ => Self::Default,
        }
    }
}

/// One shell installed on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellProfile {
    pub kind: ShellKind,
    pub label: &'static str,
}

const GIT_BASH_PATHS: &[&str] = &[
    "C:/Program Files/Git/bin/bash.exe",
    "C:/Program Files (x86)/Git/bin/bash.exe",
    "C:/Program Files/Git/usr/bin/bash.exe",
];

fn profile(kind: ShellKind) -> ShellProfile {
    ShellProfile {
        kind,
        label: kind.label(),
    }
}

/// Detect which shells are installed. Pure: the exists callback decides
/// availability, so tests never depend on the host.
pub fn probe_profiles(exists: impl Fn(&str) -> bool) -> Vec<ShellProfile> {
    #[cfg(windows)]
    {
        probe_windows_profiles(exists)
    }
    #[cfg(not(windows))]
    {
        probe_unix_profiles(exists)
    }
}

#[cfg(any(windows, test))]
pub fn probe_windows_profiles(exists: impl Fn(&str) -> bool) -> Vec<ShellProfile> {
    let mut profiles = Vec::new();
    if exists("pwsh.exe") {
        profiles.push(profile(ShellKind::PowerShell7));
    }
    if exists("powershell.exe") {
        profiles.push(profile(ShellKind::PowerShell));
    }
    if exists("cmd.exe") {
        profiles.push(profile(ShellKind::Cmd));
    }
    let git_bash = GIT_BASH_PATHS.iter().any(|path| exists(path)) || exists("bash.exe");
    if git_bash {
        profiles.push(profile(ShellKind::GitBash));
    }
    if exists("wsl.exe") {
        profiles.push(profile(ShellKind::Wsl));
    }
    if profiles.is_empty() {
        profiles.push(profile(ShellKind::Cmd));
    }
    profiles
}

#[cfg(any(not(windows), test))]
pub fn probe_unix_profiles(exists: impl Fn(&str) -> bool) -> Vec<ShellProfile> {
    let mut profiles = Vec::new();
    if exists("/bin/zsh") || exists("/usr/bin/zsh") || exists("zsh") {
        profiles.push(profile(ShellKind::Zsh));
    }
    if exists("/bin/bash") || exists("/usr/bin/bash") || exists("bash") {
        profiles.push(profile(ShellKind::Bash));
    }
    if exists("/usr/bin/fish") || exists("/usr/local/bin/fish") || exists("fish") {
        profiles.push(profile(ShellKind::Fish));
    }
    if exists("/bin/sh") || exists("sh") || profiles.is_empty() {
        profiles.push(profile(ShellKind::Sh));
    }
    profiles
}

/// Cached probe over the real file system; no process is spawned.
pub fn available_profiles() -> &'static [ShellProfile] {
    static PROFILES: LazyLock<Vec<ShellProfile>> = LazyLock::new(|| probe_profiles(shell_exists));
    PROFILES.as_slice()
}

fn shell_exists(name: &str) -> bool {
    if cfg!(windows)
        && name.eq_ignore_ascii_case("cmd.exe")
        && std::env::var_os("COMSPEC").is_some()
    {
        return true;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file();
    }
    command_in_path(name).is_some()
}

fn command_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) && Path::new(name).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path_var).find_map(|dir| {
        exts.iter().find_map(|ext| {
            let mut file = name.to_string();
            file.push_str(ext);
            let candidate = dir.join(file);
            candidate.is_file().then_some(candidate)
        })
    })
}

fn git_bash_path() -> Option<PathBuf> {
    GIT_BASH_PATHS
        .iter()
        .map(|path| Path::new(*path))
        .find(|path| path.is_file())
        .map(Path::to_path_buf)
        .or_else(|| command_in_path("bash.exe"))
}

pub(crate) fn spawn_shell_kind(
    slave: &dyn SlavePty,
    cwd: &Path,
    kind: ShellKind,
    custom: Option<&CustomShellConfig>,
) -> Result<(Box<dyn Child + Send + Sync>, String, ShellKind), String> {
    let try_spawn_args = |program: &str, args: &[&str]| {
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "kestrel");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        slave.spawn_command(cmd)
    };

    if kind == ShellKind::Custom {
        if let Some(cfg) = custom {
            let arg_refs: Vec<&str> = cfg.args.iter().map(String::as_str).collect();
            let title = Path::new(&cfg.program)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "自定义 Shell".to_string());
            return try_spawn_args(&cfg.program, &arg_refs)
                .map(|child| (child, title, ShellKind::Custom))
                .map_err(|err| format!("无法启动自定义 Shell `{}`: {err}", cfg.program));
        }
        return Err("未在设置中配置自定义 Shell 路径".to_string());
    }

    let result = match kind {
        ShellKind::PowerShell7 => try_spawn_args("pwsh.exe", &["-NoLogo"])
            .ok()
            .map(|child| (child, "PowerShell 7".to_string(), ShellKind::PowerShell7)),
        ShellKind::PowerShell => try_spawn_args("powershell.exe", &["-NoLogo"])
            .ok()
            .map(|child| (child, "Windows PowerShell".to_string(), ShellKind::PowerShell)),
        ShellKind::Cmd => try_spawn_args("cmd.exe", &[])
            .ok()
            .map(|child| (child, "Command Prompt".to_string(), ShellKind::Cmd)),
        ShellKind::GitBash => git_bash_path()
            .and_then(|path| try_spawn_args(path.to_string_lossy().as_ref(), &["--login"]).ok())
            .or_else(|| try_spawn_args("bash.exe", &[]).ok())
            .map(|child| (child, "Git Bash".to_string(), ShellKind::GitBash)),
        ShellKind::Wsl => try_spawn_args("wsl.exe", &[])
            .ok()
            .map(|child| (child, "WSL".to_string(), ShellKind::Wsl)),
        ShellKind::Zsh => try_spawn_args("zsh", &["-l"])
            .ok()
            .map(|child| (child, "Zsh".to_string(), ShellKind::Zsh)),
        ShellKind::Bash => try_spawn_args("bash", &["-l"])
            .ok()
            .map(|child| (child, "Bash".to_string(), ShellKind::Bash)),
        ShellKind::Fish => try_spawn_args("fish", &[])
            .ok()
            .map(|child| (child, "Fish".to_string(), ShellKind::Fish)),
        ShellKind::Sh => try_spawn_args("/bin/sh", &[])
            .ok()
            .map(|child| (child, "sh".to_string(), ShellKind::Sh)),
        ShellKind::Default | ShellKind::Custom => None,
    };

    if let Some(spawned) = result {
        return Ok(spawned);
    }
    spawn_shell_default(slave, cwd)
}

fn spawn_shell_default(
    slave: &dyn SlavePty,
    cwd: &Path,
) -> Result<(Box<dyn Child + Send + Sync>, String, ShellKind), String> {
    let try_spawn = |program: &str, arg: Option<&str>| {
        let mut cmd = CommandBuilder::new(program);
        if let Some(arg) = arg {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "kestrel");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        slave.spawn_command(cmd)
    };

    if cfg!(windows) {
        let first_powershell = available_profiles()
            .iter()
            .find(|profile| matches!(profile.kind, ShellKind::PowerShell7 | ShellKind::PowerShell))
            .map(|profile| profile.kind);
        if first_powershell == Some(ShellKind::PowerShell7)
            && let Ok(child) = try_spawn("pwsh.exe", Some("-NoLogo"))
        {
            return Ok((child, "PowerShell 7".to_string(), ShellKind::PowerShell7));
        }
        if first_powershell == Some(ShellKind::PowerShell)
            && let Ok(child) = try_spawn("powershell.exe", Some("-NoLogo"))
        {
            return Ok((child, "Windows PowerShell".to_string(), ShellKind::PowerShell));
        }
        try_spawn("cmd.exe", None)
            .map(|child| (child, "Command Prompt".to_string(), ShellKind::Cmd))
            .map_err(|err| format!("无法启动默认终端: {err}"))
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let resolved_kind = if shell.ends_with("zsh") {
            ShellKind::Zsh
        } else if shell.ends_with("fish") {
            ShellKind::Fish
        } else if shell.ends_with("bash") {
            ShellKind::Bash
        } else {
            ShellKind::Sh
        };
        try_spawn(&shell, None)
            .map(|child| (child, resolved_kind.label().to_string(), resolved_kind))
            .or_else(|_| {
                try_spawn("/bin/sh", None)
                    .map(|child| (child, "sh".to_string(), ShellKind::Sh))
            })
            .map_err(|err| format!("无法启动默认 Shell: {err}"))
    }
}

/// Quote a path using generic Windows/CMD rules.
#[allow(dead_code)]
pub fn quote_path(path: &Path) -> String {
    quote_path_for_shell(path, ShellKind::Cmd)
}

/// Quote and convert a path appropriately for the target shell.
pub fn quote_path_for_shell(path: &Path, shell: ShellKind) -> String {
    let raw = path.to_string_lossy();
    match shell {
        ShellKind::Wsl => quote_posix(&windows_path_to_wsl(&raw)),
        ShellKind::GitBash => quote_posix(&raw.replace('\\', "/")),
        ShellKind::Zsh | ShellKind::Bash | ShellKind::Fish | ShellKind::Sh => quote_posix(&raw),
        ShellKind::PowerShell | ShellKind::PowerShell7 => {
            let needs_quotes = raw.chars().any(|ch| {
                ch.is_whitespace()
                    || matches!(
                        ch,
                        '"' | '\''
                            | '&'
                            | '|'
                            | '<'
                            | '>'
                            | '('
                            | ')'
                            | ';'
                            | ','
                            | '='
                            | '$'
                            | '`'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '@'
                            | '#'
                            | '~'
                            | '*'
                            | '?'
                    )
            });
            if needs_quotes {
                format!("'{}'", raw.replace('\'', "''"))
            } else {
                raw.into_owned()
            }
        }
        ShellKind::Cmd | ShellKind::Default | ShellKind::Custom => {
            if !cfg!(windows) {
                return quote_posix(&raw);
            }
            let needs_quotes = raw.chars().any(|ch| {
                ch.is_whitespace()
                    || matches!(
                        ch,
                        '"' | '\''
                            | '&'
                            | '|'
                            | '<'
                            | '>'
                            | '^'
                            | '('
                            | ')'
                            | '%'
                            | ';'
                            | ','
                            | '='
                            | '!'
                            | '*'
                            | '?'
                    )
            });
            if needs_quotes {
                format!("\"{raw}\"")
            } else {
                raw.into_owned()
            }
        }
    }
}

fn windows_path_to_wsl(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = raw[2..].replace('\\', "/");
        if rest.starts_with('/') {
            format!("/mnt/{drive}{rest}")
        } else {
            format!("/mnt/{drive}/{rest}")
        }
    } else {
        raw.replace('\\', "/")
    }
}

fn quote_posix(text: &str) -> String {
    let needs_quotes = text.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\''
                    | '\\'
                    | '&'
                    | '|'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | ';'
                    | '$'
                    | '`'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '#'
                    | '~'
                    | '!'
                    | '='
            )
    });
    if needs_quotes {
        format!("'{}'", text.replace('\'', "'\\''"))
    } else {
        text.to_string()
    }
}

/// Open a URL safely with the platform default browser without invoking a shell.
pub fn open_url(url: &str) {
    crate::app::open_external_url(url);
}
