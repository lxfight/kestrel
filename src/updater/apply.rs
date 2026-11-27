//! Dual-mode update application:
//! - Portable builds: spawn the verified new executable with `--kestrel-apply-update`
//!   so it waits for the old process to exit, backs up the target executable,
//!   atomically swaps in the replacement (rolling back on error), and relaunches.
//! - Installed builds (Windows Inno Setup): launch the verified User-scope setup
//!   executable silently (`/VERYSILENT`) with wait-PID and optional relaunch flags.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::settings::Settings;

use super::manifest::PackageKind;
use super::verify::{sha256_file_hex, sha256_hex_matches};
use super::StagedUpdate;

pub const ARG_APPLY_UPDATE: &str = "--kestrel-apply-update";
pub const ARG_POST_UPDATE: &str = "--kestrel-post-update";
pub const ARG_UPDATE_FAILED: &str = "--kestrel-update-failed";
pub const ARG_CLEANUP_DIR: &str = "--cleanup-dir";

/// Result of launching an update handoff from the running Kestrel instance.
#[derive(Debug)]
pub enum ApplyLaunchError {
    /// Target directory is read-only and requires elevation (e.g. `C:\Program Files\Kestrel`).
    NeedsElevation(PathBuf),
    /// Any other failure before or during launcher spawn.
    Failed(String),
}

/// Information passed into a freshly relaunched Kestrel process right after an update.
#[derive(Clone, Debug, Default)]
pub struct PostUpdateNotice {
    pub from_version: Option<String>,
    pub error: Option<String>,
    pub cleanup_dir: Option<PathBuf>,
}

/// Directory storing downloaded and staged update artifacts under the user config dir.
pub fn updates_root_dir() -> PathBuf {
    Settings::config_dir().join("updates")
}

fn update_log_path() -> PathBuf {
    Settings::config_dir().join("update.log")
}

pub fn log_update(message: &str) {
    let dir = Settings::config_dir();
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(update_log_path())
    {
        let _ = writeln!(file, "[unix={ts}] {message}");
    }
}

/// Parses post-update startup arguments (`--kestrel-post-update`, `--kestrel-update-failed`,
/// `--cleanup-dir`) so the UI can show a completion/failure toast and clean up staged files.
pub fn parse_post_update_args() -> Option<PostUpdateNotice> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut notice = PostUpdateNotice::default();
    let mut found = false;
    let mut i = 0;
    while i < args.len() {
        let key = args[i].to_string_lossy();
        match key.as_ref() {
            ARG_POST_UPDATE => {
                found = true;
                if let Some(next) = args.get(i + 1) {
                    let val = next.to_string_lossy();
                    if !val.starts_with("--") {
                        notice.from_version = Some(val.into_owned());
                        i += 1;
                    }
                }
            }
            ARG_UPDATE_FAILED => {
                found = true;
                if let Some(next) = args.get(i + 1) {
                    notice.error = Some(next.to_string_lossy().into_owned());
                    i += 1;
                }
            }
            ARG_CLEANUP_DIR => {
                if let Some(next) = args.get(i + 1) {
                    notice.cleanup_dir = Some(PathBuf::from(next));
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found.then_some(notice)
}

#[derive(Debug)]
struct HelperArgs {
    pid: u32,
    target: PathBuf,
    expected_sha256: Option<String>,
    from_version: Option<String>,
    cleanup_dir: Option<PathBuf>,
    relaunch: bool,
}

/// Called at the very start of `main()` before GUI/GPU initialization.
/// If this process was launched as an updater helper (`--kestrel-apply-update`),
/// executes the replacement synchronously and returns `Some(exit_code)`.
pub fn maybe_run_helper_from_args() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if !args.iter().any(|a| a.to_string_lossy() == ARG_APPLY_UPDATE) {
        return None;
    }

    let Some(parsed) = parse_helper_args(&args) else {
        log_update("helper invoked with missing or invalid arguments");
        return Some(2);
    };

    let self_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log_update(&format!("helper failed to resolve current_exe: {err}"));
            return Some(1);
        }
    };

    log_update(&format!(
        "helper start: pid={}, self={}, target={}, relaunch={}",
        parsed.pid,
        self_exe.display(),
        parsed.target.display(),
        parsed.relaunch
    ));

    if parsed.pid > 0 {
        wait_for_process_exit(parsed.pid, Duration::from_secs(15));
    }

    match swap_binary_with_rollback(&self_exe, &parsed.target, parsed.expected_sha256.as_deref()) {
        Ok(()) => {
            log_update("helper swap succeeded");
            if parsed.relaunch {
                let mut cmd = Command::new(&parsed.target);
                cmd.arg(ARG_POST_UPDATE);
                if let Some(from) = parsed.from_version.as_deref().filter(|s| !s.is_empty()) {
                    cmd.arg(from);
                }
                if let Some(cleanup) = &parsed.cleanup_dir {
                    cmd.arg(ARG_CLEANUP_DIR).arg(cleanup);
                }
                if let Err(err) = cmd.spawn() {
                    log_update(&format!("helper failed to relaunch updated binary: {err}"));
                }
            }
            Some(0)
        }
        Err(err) => {
            log_update(&format!("helper swap failed: {err}"));
            if parsed.relaunch && parsed.target.is_file() {
                let _ = Command::new(&parsed.target)
                    .arg(ARG_UPDATE_FAILED)
                    .arg(&err)
                    .spawn();
            }
            Some(1)
        }
    }
}

fn parse_helper_args(args: &[OsString]) -> Option<HelperArgs> {
    let mut pid: u32 = 0;
    let mut target: Option<PathBuf> = None;
    let mut expected_sha256: Option<String> = None;
    let mut from_version: Option<String> = None;
    let mut cleanup_dir: Option<PathBuf> = None;
    let mut relaunch = false;

    let mut i = 0;
    while i < args.len() {
        let key = args[i].to_string_lossy();
        match key.as_ref() {
            "--pid" => {
                pid = args.get(i + 1)?.to_string_lossy().parse().ok()?;
                i += 1;
            }
            "--target" => {
                target = Some(PathBuf::from(args.get(i + 1)?));
                i += 1;
            }
            "--expected-sha256" => {
                expected_sha256 = Some(args.get(i + 1)?.to_string_lossy().into_owned());
                i += 1;
            }
            "--from-version" => {
                from_version = Some(args.get(i + 1)?.to_string_lossy().into_owned());
                i += 1;
            }
            ARG_CLEANUP_DIR => {
                cleanup_dir = Some(PathBuf::from(args.get(i + 1)?));
                i += 1;
            }
            "--relaunch" => {
                relaunch = true;
            }
            _ => {}
        }
        i += 1;
    }

    Some(HelperArgs {
        pid,
        target: target?,
        expected_sha256,
        from_version,
        cleanup_dir,
        relaunch,
    })
}

/// Tests whether `dir` allows creating and removing files under the current user token.
pub fn probe_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".kestrel-update-probe-{}", std::process::id()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Copies `source_exe` beside `target_exe`, verifies integrity, backs up `target_exe`,
/// and atomically swaps the new binary in place. Restores backup if replacement fails.
pub fn swap_binary_with_rollback(
    source_exe: &Path,
    target_exe: &Path,
    expected_sha256: Option<&str>,
) -> Result<(), String> {
    if !source_exe.is_file() {
        return Err(format!("更新源文件不存在: {}", source_exe.display()));
    }
    let target_dir = target_exe
        .parent()
        .ok_or_else(|| "无法解析目标程序所在目录".to_string())?;
    let target_name = target_exe
        .file_name()
        .ok_or_else(|| "无法解析目标程序文件名".to_string())?
        .to_string_lossy();

    let new_path = target_dir.join(format!("{target_name}.new"));
    let _ = fs::remove_file(&new_path);

    let src_len = fs::metadata(source_exe)
        .map_err(|e| format!("无法读取新版本文件信息: {e}"))?
        .len();
    fs::copy(source_exe, &new_path).map_err(|e| format!("复制新版本到目标目录失败: {e}"))?;

    let copied_len = fs::metadata(&new_path)
        .map_err(|e| format!("无法读取已复制文件信息: {e}"))?
        .len();
    if copied_len != src_len {
        let _ = fs::remove_file(&new_path);
        return Err("复制后的新版本文件长度不一致".to_string());
    }

    if let Some(expected) = expected_sha256.filter(|s| !s.trim().is_empty()) {
        let actual = sha256_file_hex(&new_path)?;
        if !sha256_hex_matches(&actual, expected) {
            let _ = fs::remove_file(&new_path);
            return Err("新版本文件写入后 SHA-256 复验失败".to_string());
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&new_path, fs::Permissions::from_mode(0o755));
    }

    // Pick a free backup path so another running instance holding an old `.bak` cannot block us.
    let mut backup_path = target_dir.join(format!("{target_name}.bak"));
    if backup_path.exists() && fs::remove_file(&backup_path).is_err() {
        backup_path = target_dir.join(format!("{target_name}.bak.{}", std::process::id()));
    }

    let had_target = target_exe.exists();
    if had_target {
        let mut moved = false;
        let mut last_err = None;
        for _ in 0..15 {
            match fs::rename(target_exe, &backup_path) {
                Ok(()) => {
                    moved = true;
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        if !moved {
            let _ = fs::remove_file(&new_path);
            return Err(format!(
                "备份旧版本可执行文件失败（可能仍被占用或无写入权限）: {}",
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "未知错误".to_string())
            ));
        }
    }

    if let Err(err) = fs::rename(&new_path, target_exe) {
        let _ = fs::remove_file(&new_path);
        if had_target {
            let _ = fs::rename(&backup_path, target_exe);
        }
        return Err(format!("替换目标可执行文件失败，已自动回滚: {err}"));
    }

    Ok(())
}

/// Removes leftover `.bak` / `.new` files next to the running binary and cleans up a finished
/// staging directory if it lives inside `updates_root_dir()`.
pub fn cleanup_stale_update_artifacts(cleanup_dir: Option<&Path>) {
    if let Ok(exe) = std::env::current_exe()
        && let (Some(dir), Some(name)) = (exe.parent(), exe.file_name())
    {
        let base = name.to_string_lossy();
        let bak_prefix = format!("{base}.bak");
        let new_name = format!("{base}.new");
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let s = fname.to_string_lossy();
                if s == new_name || s == bak_prefix || s.starts_with(&format!("{bak_prefix}.")) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    if let Some(dir) = cleanup_dir {
        let updates_root = updates_root_dir();
        if dir.starts_with(&updates_root) && dir != updates_root {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

/// Spawns the appropriate updater handoff for a verified `StagedUpdate`:
/// - `PackageKind::Installer` -> silent Inno Setup upgrade with wait-PID & optional relaunch.
/// - `PackageKind::PortableZip` -> self-exec helper swap with backup/rollback.
pub fn launch_staged_update(
    staged: &StagedUpdate,
    relaunch: bool,
    allow_elevation: bool,
) -> Result<(), ApplyLaunchError> {
    if !staged.staged_path.is_file() {
        return Err(ApplyLaunchError::Failed(
            "更新暂存文件已丢失，请重新下载".to_string(),
        ));
    }
    let actual_sha =
        sha256_file_hex(&staged.staged_path).map_err(ApplyLaunchError::Failed)?;
    if !sha256_hex_matches(&actual_sha, &staged.staged_sha256) {
        return Err(ApplyLaunchError::Failed(
            "更新暂存文件校验失效，请重新下载".to_string(),
        ));
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| ApplyLaunchError::Failed(format!("无法定位当前程序路径: {e}")))?;
    let target_dir = current_exe
        .parent()
        .ok_or_else(|| ApplyLaunchError::Failed("无法解析当前程序目录".to_string()))?;
    let current_pid = std::process::id();

    match staged.info.asset.package_kind {
        PackageKind::Installer => {
            launch_installer(staged, target_dir, current_pid, relaunch, allow_elevation)
        }
        PackageKind::PortableZip => {
            launch_portable_helper(staged, &current_exe, target_dir, current_pid, relaunch, allow_elevation)
        }
    }
}

fn launch_installer(
    staged: &StagedUpdate,
    install_dir: &Path,
    current_pid: u32,
    relaunch: bool,
    allow_elevation: bool,
) -> Result<(), ApplyLaunchError> {
    let args = vec![
        "/VERYSILENT".to_string(),
        "/SUPPRESSMSGBOXES".to_string(),
        "/NORESTART".to_string(),
        "/SP-".to_string(),
        "/CLOSEAPPLICATIONS".to_string(),
        format!("/DIR={}", install_dir.display()),
        format!("/KESTREL_WAIT_PID={current_pid}"),
        format!("/KESTREL_RELAUNCH={}", if relaunch { "1" } else { "0" }),
        format!("/KESTREL_FROM_VER={}", crate::app::APP_VERSION),
        format!("/KESTREL_CLEANUP_DIR={}", staged.staged_dir.display()),
    ];

    if !probe_dir_writable(install_dir) {
        if !allow_elevation {
            return Err(ApplyLaunchError::NeedsElevation(install_dir.to_path_buf()));
        }
        return spawn_elevated(&staged.staged_path, &args);
    }

    match Command::new(&staged.staged_path).args(&args).spawn() {
        Ok(_) => {
            log_update(&format!(
                "launched silent installer {} (relaunch={relaunch})",
                staged.staged_path.display()
            ));
            Ok(())
        }
        Err(err) => {
            #[cfg(windows)]
            if err.raw_os_error() == Some(740) {
                if !allow_elevation {
                    return Err(ApplyLaunchError::NeedsElevation(install_dir.to_path_buf()));
                }
                return spawn_elevated(&staged.staged_path, &args);
            }
            Err(ApplyLaunchError::Failed(format!("无法启动安装程序: {err}")))
        }
    }
}

fn launch_portable_helper(
    staged: &StagedUpdate,
    current_exe: &Path,
    target_dir: &Path,
    current_pid: u32,
    relaunch: bool,
    allow_elevation: bool,
) -> Result<(), ApplyLaunchError> {
    let mut args = vec![
        ARG_APPLY_UPDATE.to_string(),
        "--pid".to_string(),
        current_pid.to_string(),
        "--target".to_string(),
        current_exe.display().to_string(),
        "--expected-sha256".to_string(),
        staged.staged_sha256.clone(),
        "--from-version".to_string(),
        crate::app::APP_VERSION.to_string(),
        ARG_CLEANUP_DIR.to_string(),
        staged.staged_dir.display().to_string(),
    ];
    if relaunch {
        args.push("--relaunch".to_string());
    }

    if !probe_dir_writable(target_dir) {
        if !allow_elevation {
            return Err(ApplyLaunchError::NeedsElevation(target_dir.to_path_buf()));
        }
        return spawn_elevated(&staged.staged_path, &args);
    }

    Command::new(&staged.staged_path)
        .args(&args)
        .spawn()
        .map_err(|e| ApplyLaunchError::Failed(format!("无法启动更新辅助进程: {e}")))?;

    log_update(&format!(
        "launched portable swap helper {} (relaunch={relaunch})",
        staged.staged_path.display()
    ));
    Ok(())
}

/// Quotes an argument string according to Windows `CommandLineToArgvW` rules.
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.bytes().any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"')) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            backslashes += 1;
        } else if ch == '"' {
            for _ in 0..(backslashes * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            backslashes = 0;
            out.push(ch);
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(windows)]
fn spawn_elevated(exe: &Path, args: &[String]) -> Result<(), ApplyLaunchError> {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    fn to_wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    let joined_params = args
        .iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ");
    let verb = to_wide(OsStr::new("runas"));
    let file = to_wide(exe.as_os_str());
    let params = to_wide(OsStr::new(&joined_params));
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            0, // SW_HIDE
        )
    };
    if ret <= 32 {
        return Err(ApplyLaunchError::Failed(
            "已取消管理员授权或无法提权执行更新".to_string(),
        ));
    }
    log_update(&format!("launched elevated updater: {}", exe.display()));
    Ok(())
}

#[cfg(not(windows))]
fn spawn_elevated(_exe: &Path, _args: &[String]) -> Result<(), ApplyLaunchError> {
    Err(ApplyLaunchError::Failed(
        "当前安装目录不可写，请使用具有写入权限的账户或前往发布页手动覆盖安装".to_string(),
    ))
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32, timeout: Duration) {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut c_void;
        fn WaitForSingleObject(h_handle: *mut c_void, dw_milliseconds: u32) -> u32;
        fn CloseHandle(h_object: *mut c_void) -> i32;
    }

    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        WaitForSingleObject(handle, ms);
        CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for_process_exit(pid: u32, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success());
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::verify::sha256_bytes_hex;

    #[test]
    fn swap_replaces_binary_and_rolls_back_on_hash_mismatch() {
        let dir = std::env::temp_dir().join(format!("kestrel-swap-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let target = dir.join("Kestrel.exe");
        let staged = dir.join("staged-new.exe");
        fs::write(&target, b"old-v0.1.0").unwrap();
        fs::write(&staged, b"new-v0.2.0").unwrap();

        // Wrong expected hash must fail without touching target.
        let err = swap_binary_with_rollback(
            &staged,
            &target,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        assert!(err.contains("SHA-256"));
        assert_eq!(fs::read(&target).unwrap(), b"old-v0.1.0");

        // Matching hash swaps cleanly and creates `.bak`.
        let good_sha = sha256_bytes_hex(b"new-v0.2.0");
        swap_binary_with_rollback(&staged, &target, Some(&good_sha)).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new-v0.2.0");
        assert_eq!(fs::read(dir.join("Kestrel.exe.bak")).unwrap(), b"old-v0.1.0");
        assert!(probe_dir_writable(&dir));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quotes_windows_arguments_with_spaces_and_backslashes() {
        assert_eq!(quote_windows_arg("simple"), "simple");
        assert_eq!(
            quote_windows_arg(r#"C:\Program Files\Kestrel\"#),
            r#""C:\Program Files\Kestrel\\""#
        );
        assert_eq!(
            quote_windows_arg(r#"say "hi""#),
            r#""say \"hi\"""#
        );
    }
}
