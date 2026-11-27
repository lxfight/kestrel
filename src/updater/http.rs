//! Lightweight streaming HTTP client for the updater.
//!
//! - On Windows, uses native `wininet.dll` (`INTERNET_OPEN_TYPE_PRECONFIG`),
//!   automatically inheriting system proxy settings (Clash/v2ray/Edge), system
//!   TLS root certificates, and HTTP redirects with zero extra crate bloat.
//! - On macOS/Linux, delegates to system `curl` with streaming I/O so download
//!   progress and cancellation behave identically.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum metadata response size (`manifest.json` / GitHub Releases API): 2 MiB.
pub const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
/// Maximum installer / portable package download size: 256 MiB.
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

fn user_agent() -> String {
    format!("Kestrel/{} ({})", crate::app::APP_VERSION, std::env::consts::OS)
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "wininet")]
    unsafe extern "system" {
        fn InternetOpenW(
            lpsz_agent: *const u16,
            dw_access_type: u32,
            lpsz_proxy: *const u16,
            lpsz_proxy_bypass: *const u16,
            dw_flags: u32,
        ) -> *mut c_void;
        fn InternetOpenUrlW(
            h_internet: *mut c_void,
            lpsz_url: *const u16,
            lpsz_headers: *const u16,
            dw_headers_length: u32,
            dw_flags: u32,
            dw_context: usize,
        ) -> *mut c_void;
        fn InternetReadFile(
            h_file: *mut c_void,
            lp_buffer: *mut u8,
            dw_number_of_bytes_to_read: u32,
            lpdw_number_of_bytes_read: *mut u32,
        ) -> i32;
        fn InternetSetOptionW(
            h_internet: *mut c_void,
            dw_option: u32,
            lp_buffer: *const c_void,
            dw_buffer_length: u32,
        ) -> i32;
        fn HttpQueryInfoW(
            h_request: *mut c_void,
            dw_info_level: u32,
            lp_buffer: *mut c_void,
            lpdw_buffer_length: *mut u32,
            lpdw_index: *mut u32,
        ) -> i32;
        fn InternetCloseHandle(h_internet: *mut c_void) -> i32;
    }

    const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
    const INTERNET_OPEN_TYPE_DIRECT: u32 = 1;
    const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
    const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
    const INTERNET_FLAG_RELOAD: u32 = 0x8000_0000;
    const INTERNET_FLAG_NO_CACHE_WRITE: u32 = 0x0400_0000;
    const INTERNET_FLAG_PRAGMA_NOCACHE: u32 = 0x0000_0100;
    const INTERNET_FLAG_SECURE: u32 = 0x0080_0000;
    const HTTP_QUERY_CONTENT_LENGTH: u32 = 5;
    const HTTP_QUERY_STATUS_CODE: u32 = 19;
    const HTTP_QUERY_CUSTOM: u32 = 65535;
    const HTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    struct WinInetGuard(*mut c_void);
    impl Drop for WinInetGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    InternetCloseHandle(self.0);
                }
            }
        }
    }

    fn open_request(
        url: &str,
        extra_headers: &str,
        connect_timeout_ms: u32,
        receive_timeout_ms: u32,
    ) -> Result<(WinInetGuard, WinInetGuard, u16), String> {
        let agent = to_wide(&user_agent());
        let url_wide = to_wide(url);
        let headers_wide = to_wide(extra_headers);
        let mut flags =
            INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE | INTERNET_FLAG_PRAGMA_NOCACHE;
        if url.starts_with("https://") {
            flags |= INTERNET_FLAG_SECURE;
        }

        let access_type = if url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost")
            || url.starts_with("http://[::1]")
        {
            INTERNET_OPEN_TYPE_DIRECT
        } else {
            INTERNET_OPEN_TYPE_PRECONFIG
        };

        // SAFETY: All UTF-16 buffers are NUL-terminated and live across WinINet calls;
        // WinInetGuard ensures handles are always closed on return.
        unsafe {
            let session = InternetOpenW(
                agent.as_ptr(),
                access_type,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err("无法初始化 Windows 网络会话".to_string());
            }
            let session_guard = WinInetGuard(session);

            InternetSetOptionW(
                session,
                INTERNET_OPTION_CONNECT_TIMEOUT,
                (&raw const connect_timeout_ms).cast::<c_void>(),
                std::mem::size_of::<u32>() as u32,
            );
            InternetSetOptionW(
                session,
                INTERNET_OPTION_RECEIVE_TIMEOUT,
                (&raw const receive_timeout_ms).cast::<c_void>(),
                std::mem::size_of::<u32>() as u32,
            );

            let handle = InternetOpenUrlW(
                session,
                url_wide.as_ptr(),
                headers_wide.as_ptr(),
                headers_wide.len().saturating_sub(1) as u32,
                flags,
                0,
            );
            if handle.is_null() {
                let err = std::io::Error::last_os_error();
                return Err(format!("无法连接到更新服务器 ({err})"));
            }
            let handle_guard = WinInetGuard(handle);

            let mut status_code: u32 = 0;
            let mut status_len = std::mem::size_of::<u32>() as u32;
            let mut idx: u32 = 0;
            if HttpQueryInfoW(
                handle,
                HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
                (&raw mut status_code).cast::<c_void>(),
                &raw mut status_len,
                &raw mut idx,
            ) == 0
            {
                return Err("无法读取服务器响应状态码".to_string());
            }
            Ok((session_guard, handle_guard, status_code as u16))
        }
    }

    fn query_header_string(handle: *mut c_void, name: &str) -> Option<String> {
        let mut buf = to_wide(name);
        buf.resize(512, 0);
        let mut byte_len = (buf.len() * std::mem::size_of::<u16>()) as u32;
        let mut idx: u32 = 0;
        let ok = unsafe {
            HttpQueryInfoW(
                handle,
                HTTP_QUERY_CUSTOM,
                buf.as_mut_ptr().cast::<c_void>(),
                &raw mut byte_len,
                &raw mut idx,
            )
        };
        if ok == 0 {
            return None;
        }
        let u16_len = (byte_len as usize) / 2;
        let text = String::from_utf16_lossy(&buf[..u16_len.min(buf.len())]);
        let trimmed = text.trim_matches('\0').trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    fn query_content_length(handle: *mut c_void) -> Option<u64> {
        let mut buf = [0u16; 64];
        let mut byte_len = (buf.len() * std::mem::size_of::<u16>()) as u32;
        let mut idx: u32 = 0;
        let ok = unsafe {
            HttpQueryInfoW(
                handle,
                HTTP_QUERY_CONTENT_LENGTH,
                buf.as_mut_ptr().cast::<c_void>(),
                &raw mut byte_len,
                &raw mut idx,
            )
        };
        if ok == 0 {
            return None;
        }
        let u16_len = (byte_len as usize) / 2;
        let text = String::from_utf16_lossy(&buf[..u16_len.min(buf.len())]);
        text.trim_matches('\0').trim().parse::<u64>().ok()
    }

    pub fn get_bytes(
        url: &str,
        etag: Option<&str>,
        accept: &str,
        max_bytes: usize,
        timeout_ms: u32,
    ) -> Result<HttpResponse, String> {
        let mut headers = format!("Accept: {accept}\r\n");
        if let Some(tag) = etag.map(str::trim).filter(|s| !s.is_empty()) {
            headers.push_str(&format!("If-None-Match: {tag}\r\n"));
        }

        let (_session, handle, status) = open_request(url, &headers, timeout_ms, timeout_ms)?;
        let resp_etag = query_header_string(handle.0, "ETag");

        if status == 304 {
            return Ok(HttpResponse {
                status,
                etag: resp_etag,
                body: Vec::new(),
            });
        }
        if status >= 400 {
            return Err(format!("更新服务器返回 HTTP {status}"));
        }

        let mut data = Vec::with_capacity(8 * 1024);
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let mut read: u32 = 0;
            let ok = unsafe {
                InternetReadFile(
                    handle.0,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    &raw mut read,
                )
            };
            if ok == 0 {
                return Err("读取更新响应数据失败".to_string());
            }
            if read == 0 {
                break;
            }
            if data.len().saturating_add(read as usize) > max_bytes {
                return Err("更新清单响应体超过安全大小上限".to_string());
            }
            data.extend_from_slice(&chunk[..read as usize]);
        }

        Ok(HttpResponse {
            status,
            etag: resp_etag,
            body: data,
        })
    }

    pub fn download_to_file<F>(
        url: &str,
        dest: &Path,
        max_bytes: u64,
        cancel: &AtomicBool,
        mut on_progress: F,
    ) -> Result<u64, String>
    where
        F: FnMut(u64, Option<u64>),
    {
        let (_session, handle, status) =
            open_request(url, "Accept: application/octet-stream,*/*\r\n", 15_000, 30_000)?;
        if status >= 400 {
            return Err(format!("下载安装包失败（HTTP {status}）"));
        }
        let total = query_content_length(handle.0);
        if total.is_some_and(|n| n > max_bytes) {
            return Err("更新包体积超出安全上限".to_string());
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建下载目录失败: {e}"))?;
        }
        let file = File::create(dest).map_err(|e| format!("无法创建下载暂存文件: {e}"))?;
        let mut writer = BufWriter::new(file);
        let mut buf = [0u8; 64 * 1024];
        let mut downloaded = 0u64;
        on_progress(0, total);

        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err("已取消下载".to_string());
            }
            let mut read: u32 = 0;
            let ok = unsafe {
                InternetReadFile(
                    handle.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &raw mut read,
                )
            };
            if ok == 0 {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err("下载连接中断".to_string());
            }
            if read == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(u64::from(read));
            if downloaded > max_bytes {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err("下载数据超过安全大小上限".to_string());
            }
            if let Err(err) = writer.write_all(&buf[..read as usize]) {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err(format!("写入更新暂存文件失败: {err}"));
            }
            on_progress(downloaded, total);
        }

        if let Err(err) = writer.flush() {
            drop(writer);
            let _ = fs::remove_file(dest);
            return Err(format!("刷新下载文件失败: {err}"));
        }
        Ok(downloaded)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};

    pub fn get_bytes(
        url: &str,
        etag: Option<&str>,
        accept: &str,
        max_bytes: usize,
        timeout_ms: u32,
    ) -> Result<HttpResponse, String> {
        let timeout_secs = (timeout_ms / 1000).max(5).to_string();
        let header_path = std::env::temp_dir().join(format!(
            "kestrel-upd-hdr-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut cmd = Command::new("curl");
        cmd.args([
            "-sSL",
            "--max-time",
            &timeout_secs,
            "--max-filesize",
            &max_bytes.to_string(),
            "-A",
            &user_agent(),
            "-H",
            &format!("Accept: {accept}"),
            "-D",
        ])
        .arg(&header_path);
        if let Some(tag) = etag.map(str::trim).filter(|s| !s.is_empty()) {
            cmd.args(["-H", &format!("If-None-Match: {tag}")]);
        }
        cmd.arg(url);

        let output = cmd
            .output()
            .map_err(|e| format!("无法调用 curl 获取更新信息: {e}"))?;
        let raw_headers = fs::read_to_string(&header_path).unwrap_or_default();
        let _ = fs::remove_file(&header_path);

        let status = parse_last_http_status(&raw_headers).unwrap_or(0);
        let resp_etag = parse_last_header(&raw_headers, "etag");

        if status == 304 {
            return Ok(HttpResponse {
                status: 304,
                etag: resp_etag,
                body: Vec::new(),
            });
        }
        if !output.status.success() || status >= 400 {
            return Err(if status >= 400 {
                format!("更新服务器返回 HTTP {status}")
            } else {
                "请求更新服务器失败".to_string()
            });
        }
        if output.stdout.len() > max_bytes {
            return Err("更新清单响应体超过安全大小上限".to_string());
        }
        Ok(HttpResponse {
            status: if status == 0 { 200 } else { status },
            etag: resp_etag,
            body: output.stdout,
        })
    }

    pub fn download_to_file<F>(
        url: &str,
        dest: &Path,
        max_bytes: u64,
        cancel: &AtomicBool,
        mut on_progress: F,
    ) -> Result<u64, String>
    where
        F: FnMut(u64, Option<u64>),
    {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建下载目录失败: {e}"))?;
        }
        let mut child = Command::new("curl")
            .args([
                "-fsSL",
                "--connect-timeout",
                "15",
                "-A",
                &user_agent(),
                url,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("无法启动 curl 下载更新: {e}"))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法读取 curl 输出流".to_string())?;
        let file = File::create(dest).map_err(|e| format!("无法创建下载暂存文件: {e}"))?;
        let mut writer = BufWriter::new(file);
        let mut buf = [0u8; 64 * 1024];
        let mut downloaded = 0u64;
        on_progress(0, None);

        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err("已取消下载".to_string());
            }
            let n = stdout
                .read(&mut buf)
                .map_err(|e| format!("读取下载流失败: {e}"))?;
            if n == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(n as u64);
            if downloaded > max_bytes {
                let _ = child.kill();
                let _ = child.wait();
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err("下载数据超过安全大小上限".to_string());
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| format!("写入更新暂存文件失败: {e}"))?;
            on_progress(downloaded, None);
        }

        let status = child
            .wait()
            .map_err(|e| format!("等待 curl 结束失败: {e}"))?;
        if !status.success() {
            drop(writer);
            let _ = fs::remove_file(dest);
            return Err("下载安装包失败".to_string());
        }
        writer
            .flush()
            .map_err(|e| format!("刷新下载文件失败: {e}"))?;
        Ok(downloaded)
    }

    fn parse_last_http_status(headers: &str) -> Option<u16> {
        let mut last = None;
        for line in headers.lines() {
            if line.starts_with("HTTP/") {
                if let Some(code) = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()) {
                    last = Some(code);
                }
            }
        }
        last
    }

    fn parse_last_header(headers: &str, name_lower: &str) -> Option<String> {
        let mut last = None;
        for line in headers.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case(name_lower) {
                    let val = v.trim();
                    if !val.is_empty() {
                        last = Some(val.to_string());
                    }
                }
            }
        }
        last
    }
}

pub fn get_bytes(
    url: &str,
    etag: Option<&str>,
    accept: &str,
    max_bytes: usize,
    timeout_ms: u32,
) -> Result<HttpResponse, String> {
    platform::get_bytes(url, etag, accept, max_bytes, timeout_ms)
}

pub fn download_to_file<F>(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
    on_progress: F,
) -> Result<u64, String>
where
    F: FnMut(u64, Option<u64>),
{
    platform::download_to_file(url, dest, max_bytes, cancel, on_progress)
}
