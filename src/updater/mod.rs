//! Asynchronous dual-mode auto-updater service for Kestrel.
//!
//! Coordinates background version checks, streaming downloads with progress and
//! cancellation, SHA-256 verification, and adaptive update handoff (silent
//! User-scope installer upgrade for installed copies, or atomic self-replacement
//! for portable copies).

pub mod apply;
pub mod http;
pub mod manifest;
pub mod verify;
pub mod version;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

use self::apply::{ApplyLaunchError, PostUpdateNotice, cleanup_stale_update_artifacts, log_update, updates_root_dir};
use self::manifest::{
    DEFAULT_RELEASES_API_URL, DEFAULT_RELEASES_PAGE, GitHubReleaseMatch, InstallKind, PackageKind,
    ReleaseInfo, UpdateChannel, UpdateMode, current_platform_key, lookup_sha256sum,
    parse_github_releases_list, parse_manifest, resolve_manifest_url, validate_update_url,
};
use self::verify::{MAX_EXTRACTED_BINARY_BYTES, extract_zip_binary, sha256_file_hex, sha256_hex_matches};
use self::version::Version;

/// Delay after app startup before performing the automatic background check.
const AUTO_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(5);
/// Minimum interval between automatic background update checks (24 hours).
const AUTO_CHECK_COOLDOWN_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug)]
pub struct StagedUpdate {
    pub info: ReleaseInfo,
    pub staged_path: PathBuf,
    pub staged_dir: PathBuf,
    pub staged_sha256: String,
}

#[derive(Clone, Debug)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading {
        info: ReleaseInfo,
        downloaded: u64,
        total: Option<u64>,
        speed_bps: u64,
    },
    Verifying {
        info: ReleaseInfo,
    },
    ReadyToInstall(StagedUpdate),
    Applying {
        version: String,
    },
    Error {
        message: String,
        release_url: Option<String>,
    },
}

/// High-level events produced by `UpdateService::poll` for the UI layer.
#[derive(Clone, Debug)]
pub enum UpdateNotice {
    Available {
        version: String,
        manual: bool,
        auto_download: bool,
    },
    UpToDate {
        version: String,
        manual: bool,
    },
    ReadyToInstall {
        version: String,
    },
    Error {
        message: String,
        manual: bool,
    },
}

enum WorkerEvent {
    CheckDone {
        manual: bool,
        checked_at: u64,
        etag: Option<String>,
        cached_manifest: Option<String>,
        manifest_url: String,
        outcome: CheckOutcome,
    },
    DownloadProgress {
        downloaded: u64,
        total: Option<u64>,
        speed_bps: u64,
    },
    Verifying,
    DownloadReady(StagedUpdate),
    DownloadCanceled(ReleaseInfo),
    DownloadFailed {
        message: String,
        release_url: String,
    },
}

enum CheckOutcome {
    UpToDate,
    Available(ReleaseInfo),
    Error(String),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedUpdateState {
    last_check_unix: u64,
    etag: Option<String>,
    cached_manifest: Option<String>,
    cached_manifest_url: Option<String>,
}

impl PersistedUpdateState {
    fn path() -> PathBuf {
        Settings::config_dir().join("update_state.json")
    }

    fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let dir = Settings::config_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), json);
        }
    }
}

pub struct UpdateService {
    pub state: UpdateState,
    pub install_kind: InstallKind,
    pub last_check_unix: u64,
    pub pending_apply_on_exit: Option<StagedUpdate>,
    pub post_update_notice: Option<PostUpdateNotice>,
    pub elevation_prompt: Option<(PathBuf, bool)>,
    pub dev_restricted: bool,
    started_at: Instant,
    auto_check_dispatched: bool,
    tx: Sender<WorkerEvent>,
    rx: Receiver<WorkerEvent>,
    cancel_download: Arc<AtomicBool>,
    persisted: PersistedUpdateState,
}

impl UpdateService {
    pub fn new(post_update: Option<PostUpdateNotice>) -> Self {
        let cleanup_dir = post_update.as_ref().and_then(|n| n.cleanup_dir.as_deref());
        cleanup_stale_update_artifacts(cleanup_dir);

        let persisted = PersistedUpdateState::load();
        let (tx, rx) = mpsc::channel();
        Self {
            state: UpdateState::Idle,
            install_kind: InstallKind::detect(),
            last_check_unix: persisted.last_check_unix,
            pending_apply_on_exit: None,
            post_update_notice: post_update,
            elevation_prompt: None,
            dev_restricted: is_dev_restricted(),
            started_at: Instant::now(),
            auto_check_dispatched: false,
            tx,
            rx,
            cancel_download: Arc::new(AtomicBool::new(false)),
            persisted,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Checking
                | UpdateState::Downloading { .. }
                | UpdateState::Verifying { .. }
                | UpdateState::Applying { .. }
        )
    }

    /// Triggers a check for updates in a background thread.
    pub fn check_now(&mut self, settings: &Settings, manual: bool, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        if matches!(self.state, UpdateState::ReadyToInstall(_)) && !manual {
            return;
        }
        self.elevation_prompt = None;
        self.state = UpdateState::Checking;

        let channel = UpdateChannel::from_str(&settings.update_channel);
        let mirror = settings.update_mirror_url.clone();
        let skipped = settings.update_skipped_version.clone();
        let install_kind = self.install_kind;
        let prev_etag = self.persisted.etag.clone();
        let prev_cached = self.persisted.cached_manifest.clone();
        let prev_url = self.persisted.cached_manifest_url.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();

        let _ = std::thread::Builder::new()
            .name("kestrel-update-check".to_string())
            .spawn(move || {
                let checked_at = now_unix();
                let env_override = std::env::var("KESTREL_UPDATE_URL").ok();
                let (manifest_url, allow_local_http) =
                    resolve_manifest_url(&mirror, env_override.as_deref());

                let etag_for_url = if prev_url.as_deref() == Some(manifest_url.as_str()) {
                    prev_etag.as_deref()
                } else {
                    None
                };

                let res = perform_check(
                    &manifest_url,
                    allow_local_http,
                    channel,
                    &mirror,
                    install_kind,
                    etag_for_url,
                    prev_cached.as_deref(),
                    skipped.as_deref(),
                    manual,
                );

                let (etag, cached_manifest, outcome) = match res {
                    Ok((tag, cached, outcome)) => (tag, cached, outcome),
                    Err(err) => (prev_etag, prev_cached, CheckOutcome::Error(err)),
                };

                let _ = tx.send(WorkerEvent::CheckDone {
                    manual,
                    checked_at,
                    etag,
                    cached_manifest,
                    manifest_url,
                    outcome,
                });
                ctx.request_repaint();
            });
    }

    /// Downloads and verifies the update package in a background thread.
    pub fn start_download(&mut self, info: ReleaseInfo, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        self.elevation_prompt = None;
        self.cancel_download = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel_download);
        let total = info.asset.size;
        self.state = UpdateState::Downloading {
            info: info.clone(),
            downloaded: 0,
            total,
            speed_bps: 0,
        };

        let tx = self.tx.clone();
        let ctx = ctx.clone();

        let _ = std::thread::Builder::new()
            .name("kestrel-update-download".to_string())
            .spawn(move || {
                let release_url = info.release_url.clone();
                let staged_dir = updates_root_dir().join(format!("v{}", info.version));
                let _ = fs::create_dir_all(&staged_dir);
                let part_path = staged_dir.join("download.part");

                let started = Instant::now();
                let mut last_emit = Instant::now() - Duration::from_secs(1);
                let tx_prog = tx.clone();
                let ctx_prog = ctx.clone();

                let dl_res = http::download_to_file(
                    &info.asset.url,
                    &part_path,
                    http::MAX_PACKAGE_BYTES,
                    &cancel,
                    |downloaded, resp_total| {
                        let now = Instant::now();
                        if downloaded == 0 || now.duration_since(last_emit) >= Duration::from_millis(120) {
                            last_emit = now;
                            let elapsed = now.duration_since(started).as_secs_f64().max(0.05);
                            let speed_bps = (downloaded as f64 / elapsed) as u64;
                            let _ = tx_prog.send(WorkerEvent::DownloadProgress {
                                downloaded,
                                total: resp_total.or(total),
                                speed_bps,
                            });
                            ctx_prog.request_repaint();
                        }
                    },
                );

                match dl_res {
                    Err(err) if cancel.load(Ordering::Relaxed) || err == "已取消下载" => {
                        let _ = fs::remove_file(&part_path);
                        let _ = tx.send(WorkerEvent::DownloadCanceled(info));
                        ctx.request_repaint();
                        return;
                    }
                    Err(err) => {
                        let _ = fs::remove_file(&part_path);
                        let _ = tx.send(WorkerEvent::DownloadFailed {
                            message: err,
                            release_url,
                        });
                        ctx.request_repaint();
                        return;
                    }
                    Ok(_) => {}
                }

                let _ = tx.send(WorkerEvent::Verifying);
                ctx.request_repaint();

                match finalize_download(&info, &part_path, &staged_dir) {
                    Ok(staged) => {
                        log_update(&format!(
                            "download verified and staged: v{} ({:?}) -> {}",
                            staged.info.version,
                            staged.info.asset.package_kind,
                            staged.staged_path.display()
                        ));
                        let _ = tx.send(WorkerEvent::DownloadReady(staged));
                    }
                    Err(err) => {
                        let _ = fs::remove_file(&part_path);
                        let _ = tx.send(WorkerEvent::DownloadFailed {
                            message: err,
                            release_url,
                        });
                    }
                }
                ctx.request_repaint();
            });
    }

    pub fn cancel_download(&mut self) {
        self.cancel_download.store(true, Ordering::Relaxed);
    }

    /// Schedules a verified update to be applied silently when Kestrel exits.
    pub fn schedule_apply_on_exit(&mut self) -> Option<String> {
        let UpdateState::ReadyToInstall(staged) = &self.state else {
            return None;
        };
        let ver = staged.info.version.to_string();
        self.pending_apply_on_exit = Some(staged.clone());
        Some(ver)
    }

    /// Launches the verified update immediately (either silent installer or portable swap helper).
    pub fn begin_apply_now(&mut self, allow_elevation: bool) -> Result<String, ApplyLaunchError> {
        let staged = match &self.state {
            UpdateState::ReadyToInstall(s) => s.clone(),
            _ => {
                return Err(ApplyLaunchError::Failed(
                    "当前没有可安装的已下载更新".to_string(),
                ));
            }
        };
        if self.dev_restricted {
            return Err(ApplyLaunchError::Failed(
                "开发调试构建默认禁止覆盖可执行文件（可设置 KESTREL_UPDATE_ALLOW_DEV=1 启用联调）"
                    .to_string(),
            ));
        }
        apply::launch_staged_update(&staged, true, allow_elevation)?;
        let version = staged.info.version.to_string();
        self.pending_apply_on_exit = None;
        self.elevation_prompt = None;
        self.state = UpdateState::Applying {
            version: version.clone(),
        };
        Ok(version)
    }

    /// Called from `KestrelApp::on_exit` to silently apply an update queued for exit.
    pub fn apply_pending_on_exit(&mut self) {
        let Some(staged) = self.pending_apply_on_exit.take() else {
            return;
        };
        if self.dev_restricted {
            return;
        }
        if let Err(err) = apply::launch_staged_update(&staged, false, false) {
            log_update(&format!("apply on exit skipped: {err:?}"));
        }
    }

    /// Polls background worker events and triggers scheduled automatic checks.
    pub fn poll(&mut self, settings: &Settings, ctx: &egui::Context) -> Vec<UpdateNotice> {
        self.maybe_auto_check(settings, ctx);

        let mut notices = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                WorkerEvent::CheckDone {
                    manual,
                    checked_at,
                    etag,
                    cached_manifest,
                    manifest_url,
                    outcome,
                } => {
                    self.last_check_unix = checked_at;
                    self.persisted.last_check_unix = checked_at;
                    self.persisted.etag = etag;
                    self.persisted.cached_manifest = cached_manifest;
                    self.persisted.cached_manifest_url = Some(manifest_url);
                    self.persisted.save();

                    match outcome {
                        CheckOutcome::UpToDate => {
                            self.state = UpdateState::UpToDate;
                            notices.push(UpdateNotice::UpToDate {
                                version: crate::app::APP_VERSION.to_string(),
                                manual,
                            });
                        }
                        CheckOutcome::Available(info) => {
                            let ver = info.version.to_string();
                            let auto_download = !manual
                                && UpdateMode::from_str(&settings.update_mode)
                                    == UpdateMode::AutoDownload;
                            self.state = UpdateState::Available(info.clone());
                            notices.push(UpdateNotice::Available {
                                version: ver,
                                manual,
                                auto_download,
                            });
                            if auto_download {
                                self.start_download(info, ctx);
                            }
                        }
                        CheckOutcome::Error(message) => {
                            log_update(&format!("update check failed (manual={manual}): {message}"));
                            if manual {
                                self.state = UpdateState::Error {
                                    message: message.clone(),
                                    release_url: Some(DEFAULT_RELEASES_PAGE.to_string()),
                                };
                            } else {
                                // Stay unobtrusive on background network/offline errors.
                                self.state = UpdateState::Idle;
                            }
                            notices.push(UpdateNotice::Error { message, manual });
                        }
                    }
                }
                WorkerEvent::DownloadProgress {
                    downloaded,
                    total,
                    speed_bps,
                } => {
                    if let UpdateState::Downloading {
                        downloaded: d,
                        total: t,
                        speed_bps: s,
                        ..
                    } = &mut self.state
                    {
                        *d = downloaded;
                        *t = total;
                        *s = speed_bps;
                    }
                }
                WorkerEvent::Verifying => {
                    if let UpdateState::Downloading { info, .. } = &self.state {
                        self.state = UpdateState::Verifying { info: info.clone() };
                    }
                }
                WorkerEvent::DownloadReady(staged) => {
                    let version = staged.info.version.to_string();
                    self.state = UpdateState::ReadyToInstall(staged);
                    notices.push(UpdateNotice::ReadyToInstall { version });
                }
                WorkerEvent::DownloadCanceled(info) => {
                    self.state = UpdateState::Available(info);
                }
                WorkerEvent::DownloadFailed {
                    message,
                    release_url,
                } => {
                    log_update(&format!("update download failed: {message}"));
                    self.state = UpdateState::Error {
                        message: message.clone(),
                        release_url: Some(release_url),
                    };
                    notices.push(UpdateNotice::Error {
                        message,
                        manual: true,
                    });
                }
            }
        }
        notices
    }

    pub fn repaint_after(&self, settings: &Settings) -> Option<Duration> {
        if self.is_busy() {
            return Some(Duration::from_millis(100));
        }
        if !self.auto_check_dispatched && self.can_auto_check(settings) {
            let rem = AUTO_CHECK_STARTUP_DELAY.saturating_sub(self.started_at.elapsed());
            return Some(rem.max(Duration::from_millis(50)));
        }
        None
    }

    /// Compact status-bar indicator shown only when an update action needs attention.
    pub fn status_badge(&self) -> Option<(String, &'static str, bool)> {
        match &self.state {
            UpdateState::Available(info) => Some((
                format!("↑ 新版本 v{}", info.version),
                "点击查看新版本详情并更新",
                true,
            )),
            UpdateState::Downloading {
                downloaded, total, ..
            } => {
                let text = match total {
                    Some(t) if *t > 0 => {
                        let pct = ((*downloaded as f64 / *t as f64) * 100.0).min(100.0);
                        format!("↓ 下载更新 {pct:.0}%")
                    }
                    _ => format!("↓ 下载更新 {}", format_bytes(*downloaded)),
                };
                Some((text, "正在下载更新，点击查看进度", false))
            }
            UpdateState::Verifying { .. } => Some((
                "校验更新…".to_string(),
                "正在校验安装包完整性",
                false,
            )),
            UpdateState::ReadyToInstall(staged) => {
                let label = if self.pending_apply_on_exit.is_some() {
                    format!("↻ 退出时更新 v{}", staged.info.version)
                } else {
                    format!("↻ 重启以更新 v{}", staged.info.version)
                };
                Some((label, "更新包已就绪，点击重启并安装", true))
            }
            _ => None,
        }
    }

    /// One-line summary string for the About / Settings dialogs.
    pub fn status_summary(&self) -> String {
        match &self.state {
            UpdateState::Idle => format!(
                "当前版本 v{}（{}）",
                crate::app::APP_VERSION,
                self.install_kind.label()
            ),
            UpdateState::Checking => "正在检查更新…".to_string(),
            UpdateState::UpToDate => {
                format!("已是最新版本 (v{})", crate::app::APP_VERSION)
            }
            UpdateState::Available(info) => format!("发现新版本 v{}", info.version),
            UpdateState::Downloading {
                info,
                downloaded,
                total,
                ..
            } => match total {
                Some(t) if *t > 0 => {
                    let pct = ((*downloaded as f64 / *t as f64) * 100.0).min(100.0);
                    format!("正在下载 v{} ({pct:.0}%)", info.version)
                }
                _ => format!("正在下载 v{} ({})", info.version, format_bytes(*downloaded)),
            },
            UpdateState::Verifying { info } => format!("正在校验 v{} 完整性…", info.version),
            UpdateState::ReadyToInstall(staged) => {
                format!("v{} 已下载就绪，可重启安装", staged.info.version)
            }
            UpdateState::Applying { version } => format!("正在启动 v{version} 更新程序…"),
            UpdateState::Error { message, .. } => format!("检查/更新失败: {message}"),
        }
    }

    fn can_auto_check(&self, settings: &Settings) -> bool {
        if crate::devshot::enabled() || std::env::var_os("KESTREL_NO_UPDATE").is_some() {
            return false;
        }
        if self.dev_restricted {
            return false;
        }
        if UpdateMode::from_str(&settings.update_mode) == UpdateMode::Manual {
            return false;
        }
        let has_override = std::env::var("KESTREL_UPDATE_URL")
            .ok()
            .is_some_and(|s| !s.trim().is_empty());
        has_override || now_unix().saturating_sub(self.last_check_unix) >= AUTO_CHECK_COOLDOWN_SECS
    }

    fn maybe_auto_check(&mut self, settings: &Settings, ctx: &egui::Context) {
        if self.auto_check_dispatched {
            return;
        }
        if !self.can_auto_check(settings) {
            self.auto_check_dispatched = true;
            return;
        }
        if self.started_at.elapsed() < AUTO_CHECK_STARTUP_DELAY {
            return;
        }
        self.auto_check_dispatched = true;
        self.check_now(settings, false, ctx);
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_check(
    manifest_url: &str,
    allow_local_http: bool,
    channel: UpdateChannel,
    mirror: &str,
    install_kind: InstallKind,
    etag: Option<&str>,
    cached_manifest: Option<&str>,
    skipped_version: Option<&str>,
    manual: bool,
) -> Result<(Option<String>, Option<String>, CheckOutcome), String> {
    let current = Version::parse(crate::app::APP_VERSION)
        .ok_or_else(|| format!("当前版本号格式无效: {}", crate::app::APP_VERSION))?;
    let platform = current_platform_key();

    // If prerelease channel is selected and the user did NOT point to a direct `.json` file,
    // query GitHub Releases list first so pre-releases are discovered.
    let using_direct_json = manifest_url.ends_with(".json")
        && manifest_url != manifest::DEFAULT_MANIFEST_URL;

    let (next_etag, next_cached, info_opt) = if channel == UpdateChannel::Prerelease && !using_direct_json {
        fetch_via_github_releases_api(
            channel,
            platform,
            install_kind,
            mirror,
            allow_local_http,
            etag,
        )?
    } else {
        validate_update_url(manifest_url, allow_local_http)?;
        match http::get_bytes(
            manifest_url,
            etag,
            "application/json,*/*;q=0.8",
            http::MAX_METADATA_BYTES,
            12_000,
        ) {
            Ok(resp) if resp.status == 304 => {
                if let Some(cached) = cached_manifest {
                    let info =
                        parse_manifest(cached, platform, install_kind, mirror, allow_local_http)?;
                    (
                        resp.etag.or_else(|| etag.map(str::to_string)),
                        Some(cached.to_string()),
                        Some(info),
                    )
                } else {
                    // Cache missing despite 304: re-fetch without ETag.
                    let fresh = http::get_bytes(
                        manifest_url,
                        None,
                        "application/json,*/*;q=0.8",
                        http::MAX_METADATA_BYTES,
                        12_000,
                    )?;
                    let text = String::from_utf8_lossy(&fresh.body).into_owned();
                    let info =
                        parse_manifest(&text, platform, install_kind, mirror, allow_local_http)?;
                    (fresh.etag, Some(text), Some(info))
                }
            }
            Ok(resp) => {
                let text = String::from_utf8_lossy(&resp.body).into_owned();
                let info = parse_manifest(&text, platform, install_kind, mirror, allow_local_http)?;
                (resp.etag, Some(text), Some(info))
            }
            Err(err) if manifest_url == manifest::DEFAULT_MANIFEST_URL && err.contains("HTTP 404") => {
                // Fallback to GitHub Releases API if a repo release does not yet include manifest.json.
                fetch_via_github_releases_api(
                    channel,
                    platform,
                    install_kind,
                    mirror,
                    allow_local_http,
                    None,
                )?
            }
            Err(err) => return Err(err),
        }
    };

    let Some(info) = info_opt else {
        return Ok((next_etag, next_cached, CheckOutcome::UpToDate));
    };

    if channel == UpdateChannel::Stable && info.prerelease {
        return Ok((next_etag, next_cached, CheckOutcome::UpToDate));
    }
    if info.version <= current {
        return Ok((next_etag, next_cached, CheckOutcome::UpToDate));
    }
    if !manual
        && skipped_version
            .map(str::trim)
            .is_some_and(|s| s == info.version.to_string())
    {
        return Ok((next_etag, next_cached, CheckOutcome::UpToDate));
    }

    Ok((next_etag, next_cached, CheckOutcome::Available(info)))
}

fn fetch_via_github_releases_api(
    channel: UpdateChannel,
    platform: &str,
    install_kind: InstallKind,
    mirror: &str,
    allow_local_http: bool,
    etag: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<ReleaseInfo>), String> {
    let resp = http::get_bytes(
        DEFAULT_RELEASES_API_URL,
        etag,
        "application/vnd.github+json",
        http::MAX_METADATA_BYTES,
        12_000,
    )?;
    if resp.status == 304 {
        return Ok((resp.etag.or_else(|| etag.map(str::to_string)), None, None));
    }
    let body = String::from_utf8_lossy(&resp.body);
    let matched = parse_github_releases_list(
        &body,
        channel,
        platform,
        install_kind,
        mirror,
        allow_local_http,
    )?;

    match matched {
        None => Ok((resp.etag, None, None)),
        Some(GitHubReleaseMatch::ManifestUrl(url)) => {
            let m_resp = http::get_bytes(
                &url,
                None,
                "application/json,*/*;q=0.8",
                http::MAX_METADATA_BYTES,
                12_000,
            )?;
            let text = String::from_utf8_lossy(&m_resp.body).into_owned();
            let info = parse_manifest(&text, platform, install_kind, mirror, allow_local_http)?;
            Ok((resp.etag, Some(text), Some(info)))
        }
        Some(GitHubReleaseMatch::Direct {
            mut info,
            sha256sums_url,
        }) => {
            if let Some(sums_url) = sha256sums_url
                && validate_update_url(&sums_url, allow_local_http).is_ok()
                && let Ok(sums_resp) = http::get_bytes(
                    &sums_url,
                    None,
                    "text/plain,*/*;q=0.8",
                    256 * 1024,
                    10_000,
                )
            {
                let sums_text = String::from_utf8_lossy(&sums_resp.body);
                let url_file = info.asset.url.rsplit('/').next().unwrap_or("");
                if let Some(hash) = lookup_sha256sum(&sums_text, url_file) {
                    info.asset.sha256 = Some(hash);
                }
            }
            Ok((resp.etag, None, Some(info)))
        }
    }
}

fn finalize_download(
    info: &ReleaseInfo,
    part_path: &PathBuf,
    staged_dir: &PathBuf,
) -> Result<StagedUpdate, String> {
    let download_sha = sha256_file_hex(part_path)?;
    if let Some(expected) = info.asset.sha256.as_deref() {
        if !sha256_hex_matches(&download_sha, expected) {
            return Err("安装包 SHA-256 校验不匹配，文件可能已损坏或被篡改".to_string());
        }
    } else {
        return Err("更新清单缺少 SHA-256 校验值，已拒绝安装以保障安全".to_string());
    }

    match info.asset.package_kind {
        PackageKind::Installer => {
            fs::create_dir_all(staged_dir).map_err(|e| format!("创建暂存目录失败: {e}"))?;
            let safe_name = sanitize_file_name(&info.asset.file_name, "Kestrel-Setup.exe");
            let dest = staged_dir.join(safe_name);
            let _ = fs::remove_file(&dest);
            fs::rename(part_path, &dest).map_err(|e| format!("暂存安装程序失败: {e}"))?;
            Ok(StagedUpdate {
                info: info.clone(),
                staged_path: dest,
                staged_dir: staged_dir.clone(),
                staged_sha256: download_sha,
            })
        }
        PackageKind::PortableZip => {
            let safe_name = sanitize_file_name(
                &info.asset.file_name,
                manifest::default_binary_file_name(),
            );
            let dest = staged_dir.join(safe_name);
            let _ = fs::remove_file(&dest);
            let (_bytes, bin_sha) = extract_zip_binary(
                part_path,
                &info.asset.file_name,
                &dest,
                MAX_EXTRACTED_BINARY_BYTES,
            )?;
            let _ = fs::remove_file(part_path);
            Ok(StagedUpdate {
                info: info.clone(),
                staged_path: dest,
                staged_dir: staged_dir.clone(),
                staged_sha256: bin_sha,
            })
        }
    }
}

fn sanitize_file_name(name: &str, fallback: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim();
    if base.is_empty()
        || base == "."
        || base == ".."
        || base.bytes().any(|b| b.is_ascii_control() || matches!(b, b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|'))
    {
        fallback.to_string()
    } else {
        base.to_string()
    }
}

fn is_dev_restricted() -> bool {
    if std::env::var("KESTREL_UPDATE_ALLOW_DEV")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return false;
    }
    if cfg!(debug_assertions) {
        return true;
    }
    if let Ok(exe) = std::env::current_exe() {
        let lower = exe.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        if lower.contains("/target/debug/") || lower.contains("/target/release/") {
            return true;
        }
    }
    false
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_unix_relative(ts: u64) -> String {
    if ts == 0 {
        return "从未检查".to_string();
    }
    let delta = now_unix().saturating_sub(ts);
    if delta < 60 {
        "刚刚".to_string()
    } else if delta < 3600 {
        format!("{} 分钟前", delta / 60)
    } else if delta < 86_400 {
        format!("{} 小时前", delta / 3600)
    } else {
        format!("{} 天前", delta / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_test_http_server(
        routes: Vec<(String, String, Vec<u8>)>,
        etag: Option<&'static str>,
        max_requests: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let handle = std::thread::spawn(move || {
            for _ in 0..max_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut req_buf = [0u8; 4096];
                let n = stream.read(&mut req_buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&req_buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let has_matching_etag = etag.is_some_and(|tag| {
                    req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("if-none-match:") && l.contains(tag)
                    })
                });

                if has_matching_etag && path == "/manifest.json" {
                    let resp = format!(
                        "HTTP/1.1 304 Not Modified\r\nETag: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        etag.unwrap()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    continue;
                }

                if let Some((_, content_type, body)) = routes.iter().find(|(p, _, _)| p == path) {
                    let etag_hdr = etag
                        .filter(|_| path == "/manifest.json")
                        .map(|t| format!("ETag: {t}\r\n"))
                        .unwrap_or_default();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{etag_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body);
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        });
        (base, handle)
    }

    #[test]
    fn end_to_end_local_http_check_etag_download_verify_and_dual_staging() {
        let bin_payload = b"MZ-kestrel-release-0.99.0-payload-data".repeat(128);
        let zip_bytes = verify::build_zip_with_crc(&[(
            manifest::default_binary_file_name(),
            &bin_payload,
        )]);
        let zip_sha = verify::sha256_bytes_hex(&zip_bytes);

        let installer_payload = b"MZ-kestrel-inno-setup-0.99.0".repeat(128);
        let inst_sha = verify::sha256_bytes_hex(&installer_payload);

        let platform = current_platform_key();

        // Start a real local TCP server serving manifest.json, portable zip, and installer exe.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        drop(listener);

        let manifest_json = serde_json::json!({
            "schema": 1,
            "version": "0.99.0",
            "published_at": "2026-12-01T00:00:00Z",
            "prerelease": false,
            "release_url": "https://github.com/lxfight/kestrel/releases/tag/v0.99.0",
            "notes": "- 测试更新说明",
            "assets": {
                platform: {
                    "portable": {
                        "url": format!("{base}/kestrel.zip"),
                        "file_name": manifest::default_binary_file_name(),
                        "size": zip_bytes.len(),
                        "sha256": zip_sha
                    },
                    "installer": {
                        "url": format!("{base}/Kestrel-Setup.exe"),
                        "file_name": "Kestrel-Setup.exe",
                        "size": installer_payload.len(),
                        "sha256": inst_sha
                    }
                }
            }
        })
        .to_string();

        // Bind actual server on a free port and rebuild manifest URLs to match it cleanly.
        let (actual_base, server) = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let actual_base = format!("http://{}", l.local_addr().unwrap());
            let m = manifest_json.replace(&base, &actual_base);
            drop(l);
            let _ = m;
            // Use helper directly by binding once inside a custom inline server.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let actual_base = format!("http://{}", listener.local_addr().unwrap());
            let m_bytes = manifest_json.replace(&base, &actual_base).into_bytes();
            let routes = vec![
                ("/manifest.json".to_string(), "application/json".to_string(), m_bytes),
                ("/kestrel.zip".to_string(), "application/zip".to_string(), zip_bytes.clone()),
                (
                    "/Kestrel-Setup.exe".to_string(),
                    "application/octet-stream".to_string(),
                    installer_payload.clone(),
                ),
            ];
            let handle = std::thread::spawn(move || {
                for _ in 0..5 {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let mut req_buf = [0u8; 4096];
                    let n = stream.read(&mut req_buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let has_etag = req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("if-none-match:")
                            && l.contains("\"v099\"")
                    });
                    if has_etag && path == "/manifest.json" {
                        let _ = stream.write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"v099\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        continue;
                    }
                    if let Some((_, ct, body)) = routes.iter().find(|(p, _, _)| p == path) {
                        let etag_hdr = if path == "/manifest.json" {
                            "ETag: \"v099\"\r\n"
                        } else {
                            ""
                        };
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\n{etag_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(body);
                    } else {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });
            (actual_base, handle)
        };

        let manifest_url = format!("{actual_base}/manifest.json");
        let work_dir = std::env::temp_dir().join(format!("kestrel-e2e-upd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work_dir);
        fs::create_dir_all(&work_dir).unwrap();

        // 1. Check in Portable mode -> selects ZIP asset and caches ETag.
        let (etag1, cached1, out1) = perform_check(
            &manifest_url,
            true,
            UpdateChannel::Stable,
            "",
            InstallKind::Portable,
            None,
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(etag1.as_deref(), Some("\"v099\""));
        let portable_info = match out1 {
            CheckOutcome::Available(info) => info,
            other => panic!("expected Available, got {:?}", std::mem::discriminant(&other)),
        };
        assert_eq!(portable_info.asset.package_kind, PackageKind::PortableZip);

        // 2. Second check with ETag hits 304 Not Modified and reuses cached manifest!
        let (etag2, _, out2) = perform_check(
            &manifest_url,
            true,
            UpdateChannel::Stable,
            "",
            InstallKind::Installed,
            etag1.as_deref(),
            cached1.as_deref(),
            None,
            true,
        )
        .unwrap();
        assert_eq!(etag2.as_deref(), Some("\"v099\""));
        let installed_info = match out2 {
            CheckOutcome::Available(info) => info,
            other => panic!("expected Available on 304 cached parse, got {:?}", std::mem::discriminant(&other)),
        };
        assert_eq!(installed_info.asset.package_kind, PackageKind::Installer);

        // 3. Download + verify + extract portable ZIP.
        let cancel = AtomicBool::new(false);
        let mut saw_progress = false;
        let part_zip = work_dir.join("portable.part");
        let dl_n = http::download_to_file(
            &portable_info.asset.url,
            &part_zip,
            http::MAX_PACKAGE_BYTES,
            &cancel,
            |done, total| {
                if done > 0 && total == Some(zip_bytes.len() as u64) {
                    saw_progress = true;
                }
            },
        )
        .unwrap();
        assert_eq!(dl_n, zip_bytes.len() as u64);
        assert!(saw_progress);
        let staged_port =
            finalize_download(&portable_info, &part_zip, &work_dir.join("staged-port")).unwrap();
        assert_eq!(fs::read(&staged_port.staged_path).unwrap(), bin_payload);

        // 4. Download + verify + stage installer EXE.
        let part_inst = work_dir.join("installer.part");
        http::download_to_file(
            &installed_info.asset.url,
            &part_inst,
            http::MAX_PACKAGE_BYTES,
            &cancel,
            |_, _| {},
        )
        .unwrap();
        let staged_inst =
            finalize_download(&installed_info, &part_inst, &work_dir.join("staged-inst")).unwrap();
        assert_eq!(fs::read(&staged_inst.staged_path).unwrap(), installer_payload);

        // 5. Tampered SHA-256 must be rejected.
        let mut bad_info = installed_info.clone();
        bad_info.asset.sha256 =
            Some("0000000000000000000000000000000000000000000000000000000000000000".to_string());
        let bad_part = work_dir.join("bad.part");
        http::download_to_file(
            &bad_info.asset.url,
            &bad_part,
            http::MAX_PACKAGE_BYTES,
            &cancel,
            |_, _| {},
        )
        .unwrap();
        let err = finalize_download(&bad_info, &bad_part, &work_dir.join("staged-bad")).unwrap_err();
        assert!(err.contains("SHA-256"));

        let _ = server.join();
        let _ = fs::remove_dir_all(&work_dir);
    }

    #[test]
    fn dummy_server_helper_compiles() {
        // Keep spawn_test_http_server exercised for future HTTP tests.
        let (url, handle) = spawn_test_http_server(
            vec![("/ping".to_string(), "text/plain".to_string(), b"pong".to_vec())],
            None,
            1,
        );
        let resp = http::get_bytes(&format!("{url}/ping"), None, "*/*", 1024, 5000).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"pong");
        let _ = handle.join();
    }
}
