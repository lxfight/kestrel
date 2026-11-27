//! Dual-mode (Installed + Portable) release manifest and GitHub Releases parsing.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::version::Version;

pub const DEFAULT_REPO: &str = "lxfight/kestrel";
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/lxfight/kestrel/releases/latest/download/manifest.json";
pub const DEFAULT_RELEASES_API_URL: &str =
    "https://api.github.com/repos/lxfight/kestrel/releases?per_page=10";
pub const DEFAULT_RELEASES_PAGE: &str = "https://github.com/lxfight/kestrel/releases";

/// Marker file created by the Windows User-scope Inno Setup installer beside `Kestrel.exe`.
pub const INSTALLED_MARKER_FILE: &str = ".kestrel-installed";

/// How this copy of Kestrel was deployed on the user's machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed via system/user installer (Inno Setup); updates prefer silent installer upgrade.
    Installed,
    /// Standalone portable binary/zip; updates prefer in-place self-replacement.
    Portable,
}

impl InstallKind {
    pub fn detect() -> Self {
        if let Ok(override_kind) = std::env::var("KESTREL_INSTALL_KIND") {
            match override_kind.trim().to_ascii_lowercase().as_str() {
                "installed" | "installer" | "setup" => return Self::Installed,
                "portable" | "zip" => return Self::Portable,
                _ => {}
            }
        }
        let Ok(exe) = std::env::current_exe() else {
            return Self::Portable;
        };
        Self::detect_for_exe(&exe)
    }

    pub fn detect_for_exe(exe_path: &Path) -> Self {
        let Some(dir) = exe_path.parent() else {
            return Self::Portable;
        };
        if dir.join(INSTALLED_MARKER_FILE).is_file() {
            return Self::Installed;
        }
        #[cfg(windows)]
        {
            if dir.join("unins000.exe").is_file() {
                return Self::Installed;
            }
        }
        Self::Portable
    }

    pub fn label(self) -> &'static str {
        match self {
            InstallKind::Installed => "安装版",
            InstallKind::Portable => "便携版",
        }
    }
}

/// Artifact delivery format chosen for the active platform and install kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    /// Windows Inno Setup installer executable (`.exe`), run silently with `/VERYSILENT`.
    Installer,
    /// Portable ZIP archive containing the standalone executable.
    PortableZip,
}

impl PackageKind {
    pub fn label(self) -> &'static str {
        match self {
            PackageKind::Installer => "安装程序升级",
            PackageKind::PortableZip => "便携包原地替换",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "prerelease" | "beta" | "preview" | "nightly" => Self::Prerelease,
            _ => Self::Stable,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    Prompt,
    AutoDownload,
    Manual,
}

impl UpdateMode {
    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto_download" | "auto" | "silent" => Self::AutoDownload,
            "manual" | "off" | "disabled" => Self::Manual,
            _ => Self::Prompt,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub package_kind: PackageKind,
    pub url: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub raw_version: String,
    pub published_at: String,
    pub prerelease: bool,
    pub release_url: String,
    pub notes: String,
    pub asset: ReleaseAsset,
    pub install_kind: InstallKind,
}

/// Platform key used in `manifest.json`, e.g. `windows-x86_64`.
pub fn current_platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        "unknown"
    }
}

pub fn default_binary_file_name() -> &'static str {
    if cfg!(windows) {
        "Kestrel.exe"
    } else {
        "kestrel"
    }
}

/// Validates that an update URL uses HTTPS (or localhost HTTP when explicitly allowed).
pub fn validate_update_url(url: &str, allow_local_http: bool) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err("更新地址无效".to_string());
    }
    if trimmed.starts_with("https://") {
        return Ok(());
    }
    if allow_local_http
        && (trimmed.starts_with("http://127.0.0.1")
            || trimmed.starts_with("http://localhost")
            || trimmed.starts_with("http://[::1]"))
    {
        return Ok(());
    }
    Err("出于安全限制，更新源必须使用 HTTPS 协议".to_string())
}

/// Applies an optional user-configured mirror prefix to GitHub download URLs.
pub fn rewrite_asset_url(url: &str, mirror_setting: &str) -> String {
    let mirror = mirror_setting.trim();
    if mirror.is_empty() {
        return url.to_string();
    }
    // If the user configured a full manifest URL (ends with .json), do not prefix asset URLs
    // unless it is a proxy prefix like `https://mirror.example.com/`.
    if mirror.ends_with(".json") {
        return url.to_string();
    }
    if url.starts_with("https://github.com/") || url.starts_with("https://objects.githubusercontent.com/")
    {
        let base = mirror.trim_end_matches('/');
        return format!("{base}/{url}");
    }
    url.to_string()
}

/// Resolves the primary manifest URL based on environment override, user mirror, and channel.
pub fn resolve_manifest_url(
    mirror_setting: &str,
    env_override: Option<&str>,
) -> (String, bool) {
    if let Some(env_url) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        return (env_url.to_string(), true);
    }
    let mirror = mirror_setting.trim();
    if mirror.is_empty() {
        return (DEFAULT_MANIFEST_URL.to_string(), false);
    }
    if mirror.ends_with(".json") {
        let local = mirror.starts_with("http://127.0.0.1") || mirror.starts_with("http://localhost");
        return (mirror.to_string(), local);
    }
    let base = mirror.trim_end_matches('/');
    (format!("{base}/{DEFAULT_MANIFEST_URL}"), false)
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    #[allow(dead_code)]
    schema: u32,
    version: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    release_url: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    assets: HashMap<String, RawPlatformAssets>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPlatformAssets {
    Dual {
        #[serde(default)]
        installer: Option<RawAssetItem>,
        #[serde(default)]
        portable: Option<RawAssetItem>,
    },
    Single(RawAssetItem),
}

#[derive(Deserialize)]
struct RawAssetItem {
    url: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

/// Parses a `manifest.json` document and selects the matching asset for `(platform, install_kind)`.
pub fn parse_manifest(
    json_text: &str,
    platform: &str,
    install_kind: InstallKind,
    mirror_setting: &str,
    allow_local_http: bool,
) -> Result<ReleaseInfo, String> {
    let raw: RawManifest =
        serde_json::from_str(json_text).map_err(|e| format!("解析更新清单失败: {e}"))?;
    let version = Version::parse(&raw.version)
        .ok_or_else(|| format!("更新清单包含无效版本号: {}", raw.version))?;
    let platform_entry = raw
        .assets
        .get(platform)
        .ok_or_else(|| format!("该版本暂未提供当前平台 ({platform}) 的安装包"))?;

    let (package_kind, item) = match platform_entry {
        RawPlatformAssets::Dual {
            installer,
            portable,
        } => match install_kind {
            // Installed mode prefers the silent installer, gracefully falling back to portable zip.
            InstallKind::Installed => {
                if let Some(inst) = installer {
                    (PackageKind::Installer, inst)
                } else if let Some(port) = portable {
                    (PackageKind::PortableZip, port)
                } else {
                    return Err(format!("更新清单缺少 {platform} 的可用产物"));
                }
            }
            // Portable mode strictly prefers portable zip so we never pollute system install state.
            InstallKind::Portable => {
                if let Some(port) = portable {
                    (PackageKind::PortableZip, port)
                } else {
                    return Err(format!("该版本未提供 {platform} 便携版更新包，请前往发布页手动下载"));
                }
            }
        },
        RawPlatformAssets::Single(item) => {
            let kind = if item.url.to_ascii_lowercase().ends_with(".exe") {
                PackageKind::Installer
            } else {
                PackageKind::PortableZip
            };
            (kind, item)
        }
    };

    let url = rewrite_asset_url(&item.url, mirror_setting);
    validate_update_url(&url, allow_local_http)?;

    let file_name = if item.file_name.trim().is_empty() {
        match package_kind {
            PackageKind::Installer => url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("Kestrel-Setup.exe")
                .to_string(),
            PackageKind::PortableZip => default_binary_file_name().to_string(),
        }
    } else {
        item.file_name.trim().to_string()
    };

    let release_url = if raw.release_url.trim().is_empty() {
        format!("https://github.com/{DEFAULT_REPO}/releases/tag/v{version}")
    } else {
        raw.release_url.trim().to_string()
    };

    Ok(ReleaseInfo {
        prerelease: raw.prerelease || version.is_prerelease(),
        raw_version: raw.version,
        version,
        published_at: raw.published_at,
        release_url,
        notes: raw.notes,
        asset: ReleaseAsset {
            package_kind,
            url,
            file_name,
            size: item.size,
            sha256: item.sha256.clone().filter(|s| !s.trim().is_empty()),
            signature: item.signature.clone().filter(|s| !s.trim().is_empty()),
        },
        install_kind,
    })
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Outcome of scanning a GitHub Releases API response (used for prerelease channel or fallback).
#[derive(Debug)]
pub enum GitHubReleaseMatch {
    /// Release contains a `manifest.json` asset; fetch it to get full SHA-256 and dual-mode metadata.
    ManifestUrl(String),
    /// Release has no `manifest.json`; constructed directly from GitHub assets (plus optional `SHA256SUMS.txt` URL).
    Direct {
        info: ReleaseInfo,
        sha256sums_url: Option<String>,
    },
}

/// Selects the highest matching release from a GitHub `/releases` JSON array.
pub fn parse_github_releases_list(
    json_text: &str,
    channel: UpdateChannel,
    platform: &str,
    install_kind: InstallKind,
    mirror_setting: &str,
    allow_local_http: bool,
) -> Result<Option<GitHubReleaseMatch>, String> {
    let releases: Vec<GhRelease> =
        serde_json::from_str(json_text).map_err(|e| format!("解析 GitHub Releases 响应失败: {e}"))?;

    let mut best: Option<(Version, GhRelease)> = None;
    for rel in releases {
        if rel.draft {
            continue;
        }
        let Some(ver) = Version::parse(&rel.tag_name) else {
            continue;
        };
        let is_pre = rel.prerelease || ver.is_prerelease();
        if channel == UpdateChannel::Stable && is_pre {
            continue;
        }
        if best.as_ref().is_none_or(|(best_v, _)| ver > *best_v) {
            best = Some((ver, rel));
        }
    }

    let Some((version, rel)) = best else {
        return Ok(None);
    };

    if let Some(manifest_asset) = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("manifest.json"))
    {
        let url = rewrite_asset_url(&manifest_asset.browser_download_url, mirror_setting);
        validate_update_url(&url, allow_local_http)?;
        return Ok(Some(GitHubReleaseMatch::ManifestUrl(url)));
    }

    let sha256sums_url = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .map(|a| rewrite_asset_url(&a.browser_download_url, mirror_setting));

    let chosen = pick_github_asset(&rel.assets, platform, install_kind)
        .ok_or_else(|| format!("该版本未包含适配 {platform} 的发布包"))?;

    let (package_kind, gh_asset) = chosen;
    let url = rewrite_asset_url(&gh_asset.browser_download_url, mirror_setting);
    validate_update_url(&url, allow_local_http)?;

    let file_name = match package_kind {
        PackageKind::Installer => gh_asset.name.clone(),
        PackageKind::PortableZip => default_binary_file_name().to_string(),
    };

    Ok(Some(GitHubReleaseMatch::Direct {
        info: ReleaseInfo {
            prerelease: rel.prerelease || version.is_prerelease(),
            raw_version: rel.tag_name,
            version,
            published_at: rel.published_at,
            release_url: if rel.html_url.is_empty() {
                DEFAULT_RELEASES_PAGE.to_string()
            } else {
                rel.html_url
            },
            notes: rel.body,
            asset: ReleaseAsset {
                package_kind,
                url,
                file_name,
                size: (gh_asset.size > 0).then_some(gh_asset.size),
                sha256: None,
                signature: None,
            },
            install_kind,
        },
        sha256sums_url,
    }))
}

fn pick_github_asset<'a>(
    assets: &'a [GhAsset],
    platform: &str,
    install_kind: InstallKind,
) -> Option<(PackageKind, &'a GhAsset)> {
    let is_windows = platform.starts_with("windows-");
    let arch_tokens: &[&str] = if platform.ends_with("x86_64") {
        &["x86_64", "x64", "amd64"]
    } else if platform.ends_with("aarch64") {
        &["aarch64", "arm64"]
    } else {
        &[]
    };
    let os_token = platform.split('-').next().unwrap_or("");

    let matches_arch = |lower: &str| arch_tokens.iter().any(|tok| lower.contains(tok));

    let find_installer = || {
        if !is_windows {
            return None;
        }
        assets.iter().find(|a| {
            let l = a.name.to_ascii_lowercase();
            l.ends_with(".exe") && (l.contains("setup") || l.contains("installer")) && matches_arch(&l)
        })
    };

    let find_portable = || {
        assets.iter().find(|a| {
            let l = a.name.to_ascii_lowercase();
            l.ends_with(".zip") && l.contains(os_token) && matches_arch(&l)
        })
    };

    match install_kind {
        InstallKind::Installed => find_installer()
            .map(|a| (PackageKind::Installer, a))
            .or_else(|| find_portable().map(|a| (PackageKind::PortableZip, a))),
        InstallKind::Portable => find_portable().map(|a| (PackageKind::PortableZip, a)),
    }
}

/// Looks up the SHA-256 hex digest for `asset_file_name` in a standard `SHA256SUMS.txt` file.
pub fn lookup_sha256sum(sums_text: &str, asset_file_name: &str) -> Option<String> {
    let target = asset_file_name.trim().to_ascii_lowercase();
    for line in sums_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
            && name.to_ascii_lowercase() == target
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MANIFEST: &str = r#"{
        "schema": 1,
        "version": "0.2.0",
        "published_at": "2026-12-01T10:00:00Z",
        "prerelease": false,
        "release_url": "https://github.com/lxfight/kestrel/releases/tag/v0.2.0",
        "notes": "- 新增自动更新",
        "assets": {
            "windows-x86_64": {
                "portable": {
                    "url": "https://github.com/lxfight/kestrel/releases/download/v0.2.0/kestrel-v0.2.0-windows-x86_64.zip",
                    "file_name": "Kestrel.exe",
                    "size": 1000,
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "installer": {
                    "url": "https://github.com/lxfight/kestrel/releases/download/v0.2.0/Kestrel-Setup-v0.2.0-x64.exe",
                    "file_name": "Kestrel-Setup-v0.2.0-x64.exe",
                    "size": 1200,
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            }
        }
    }"#;

    #[test]
    fn adaptive_selection_picks_installer_for_installed_and_zip_for_portable() {
        let installed = parse_manifest(
            SAMPLE_MANIFEST,
            "windows-x86_64",
            InstallKind::Installed,
            "",
            false,
        )
        .unwrap();
        assert_eq!(installed.asset.package_kind, PackageKind::Installer);
        assert_eq!(installed.asset.file_name, "Kestrel-Setup-v0.2.0-x64.exe");

        let portable = parse_manifest(
            SAMPLE_MANIFEST,
            "windows-x86_64",
            InstallKind::Portable,
            "https://mirror.example.com",
            false,
        )
        .unwrap();
        assert_eq!(portable.asset.package_kind, PackageKind::PortableZip);
        assert!(portable
            .asset
            .url
            .starts_with("https://mirror.example.com/https://github.com/"));
    }

    #[test]
    fn rejects_insecure_http_urls_unless_local_override() {
        assert!(validate_update_url("https://github.com/a/b", false).is_ok());
        assert!(validate_update_url("http://example.com/a.zip", false).is_err());
        assert!(validate_update_url("http://127.0.0.1:8765/manifest.json", false).is_err());
        assert!(validate_update_url("http://127.0.0.1:8765/manifest.json", true).is_ok());
    }

    #[test]
    fn detects_installed_vs_portable_from_marker_file() {
        let dir = std::env::temp_dir().join(format!("kestrel-kind-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Kestrel.exe");
        std::fs::write(&exe, b"bin").unwrap();

        assert_eq!(InstallKind::detect_for_exe(&exe), InstallKind::Portable);

        std::fs::write(dir.join(INSTALLED_MARKER_FILE), b"installer=inno\n").unwrap();
        assert_eq!(InstallKind::detect_for_exe(&exe), InstallKind::Installed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_sha256sums_entries() {
        let sums = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  kestrel-v0.2.0-windows-x86_64.zip\n\
                    e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *Kestrel-Setup-v0.2.0-x64.exe\n";
        assert_eq!(
            lookup_sha256sum(sums, "Kestrel-Setup-v0.2.0-x64.exe").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }
}
