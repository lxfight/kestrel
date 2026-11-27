//! Lightweight streaming HTTP client for Kestrel plugins.
//!
//! - On Windows, uses native `wininet.dll` (`HttpOpenRequestW` / `HttpSendRequestW` /
//!   `InternetReadFile`), automatically inheriting system proxy settings and TLS
//!   root certificates with zero extra crate dependencies.
//! - On macOS/Linux, delegates to system `curl -sS -N --no-buffer` for identical
//!   streaming and proxy behavior.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

/// Maximum concurrent `kestrel.http` requests across all plugins.
pub(crate) const MAX_CONCURRENT_HTTP: usize = 8;
/// Maximum response body buffered into memory (16 MiB).
pub(crate) const MAX_HTTP_BODY: usize = 16 * 1024 * 1024;
/// Default connect / receive timeout (60s, suitable for LLM APIs).
pub(crate) const DEFAULT_TIMEOUT_MS: u32 = 60_000;

pub(crate) struct HttpRequestSpec {
    pub runtime: u64,
    pub task: u64,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout_ms: u32,
    pub stream: bool,
}

pub(crate) enum HttpEvent {
    Chunk {
        runtime: u64,
        task: u64,
        status: u16,
        chunk: String,
    },
    Done {
        runtime: u64,
        task: u64,
        status: u16,
        headers: HashMap<String, String>,
        body: String,
        error: Option<String>,
    },
}

struct HttpSlot(Arc<AtomicUsize>);

impl Drop for HttpSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn spawn_request(
    spec: HttpRequestSpec,
    tx: Sender<HttpEvent>,
    running: Arc<AtomicUsize>,
    plugin_cancel: Arc<AtomicBool>,
    req_cancel: Arc<AtomicBool>,
    repaint_ctx: Option<egui::Context>,
) -> Result<(), String> {
    if !spec.url.starts_with("http://") && !spec.url.starts_with("https://") {
        return Err(format!("仅支持 http:// 或 https:// URL: {}", spec.url));
    }
    if running.fetch_add(1, Ordering::Relaxed) >= MAX_CONCURRENT_HTTP {
        running.fetch_sub(1, Ordering::Relaxed);
        return Err(format!(
            "kestrel.http 并发上限 {MAX_CONCURRENT_HTTP}，本次请求被拒绝"
        ));
    }
    let slot = HttpSlot(running);
    std::thread::Builder::new()
        .name("kestrel-plugin-http".into())
        .spawn(move || {
            let _slot = slot;
            let runtime = spec.runtime;
            let task = spec.task;
            let is_cancelled = || {
                plugin_cancel.load(Ordering::Relaxed) || req_cancel.load(Ordering::Relaxed)
            };
            let outcome = platform::execute(&spec, &is_cancelled, |status, chunk| {
                if spec.stream {
                    let _ = tx.send(HttpEvent::Chunk {
                        runtime,
                        task,
                        status,
                        chunk: String::from_utf8_lossy(chunk).into_owned(),
                    });
                    if let Some(ctx) = &repaint_ctx {
                        ctx.request_repaint();
                    }
                }
            });
            let done = match outcome {
                Ok(resp) => HttpEvent::Done {
                    runtime,
                    task,
                    status: resp.status,
                    headers: resp.headers,
                    body: resp.body,
                    error: if is_cancelled() {
                        Some("已取消".to_string())
                    } else {
                        None
                    },
                },
                Err(err) => HttpEvent::Done {
                    runtime,
                    task,
                    status: 0,
                    headers: HashMap::new(),
                    body: String::new(),
                    error: Some(err),
                },
            };
            let _ = tx.send(done);
            if let Some(ctx) = &repaint_ctx {
                ctx.request_repaint();
            }
        })
        .map(|_| ())
        .map_err(|err| format!("启动 HTTP 线程失败: {err}"))
}

struct ExecutedResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedUrl {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub target: String,
}

pub(crate) fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("无效 URL 协议: {url}"));
    };
    let (authority, target) = match rest.find(['/', '?', '#']) {
        Some(idx) => {
            let (a, tail) = rest.split_at(idx);
            let target = if tail.starts_with('/') {
                tail.split('#').next().unwrap_or("/").to_string()
            } else if tail.starts_with('?') {
                format!("/{}", tail.split('#').next().unwrap_or(""))
            } else {
                "/".to_string()
            };
            (a, target)
        }
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err(format!("URL 缺少主机名: {url}"));
    }
    let default_port = if secure { 443 } else { 80 };
    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let end = after_bracket
            .find(']')
            .ok_or_else(|| format!("无效 IPv6 地址: {url}"))?;
        let host = &after_bracket[..end];
        let rem = &after_bracket[end + 1..];
        let port = if let Some(p) = rem.strip_prefix(':') {
            p.parse::<u16>()
                .map_err(|_| format!("无效端口: {p}"))?
        } else if rem.is_empty() {
            default_port
        } else {
            return Err(format!("无效主机端口: {authority}"));
        };
        (host.to_string(), port)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        let port = p.parse::<u16>().map_err(|_| format!("无效端口: {p}"))?;
        (h.to_string(), port)
    } else {
        (authority.to_string(), default_port)
    };
    if host.is_empty() {
        return Err(format!("URL 主机名为空: {url}"));
    }
    Ok(ParsedUrl {
        secure,
        host,
        port,
        target,
    })
}

fn parse_raw_headers(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in raw.lines().skip(1) {
        let line = line.trim_end_matches('\r');
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if !key.is_empty() {
                map.entry(key)
                    .and_modify(|existing: &mut String| {
                        existing.push_str(", ");
                        existing.push_str(&val);
                    })
                    .or_insert(val);
            }
        }
    }
    map
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
        fn InternetConnectW(
            h_internet: *mut c_void,
            lpsz_server_name: *const u16,
            n_server_port: u16,
            lpsz_user_name: *const u16,
            lpsz_password: *const u16,
            dw_service: u32,
            dw_flags: u32,
            dw_context: usize,
        ) -> *mut c_void;
        fn HttpOpenRequestW(
            h_connect: *mut c_void,
            lpsz_verb: *const u16,
            lpsz_object_name: *const u16,
            lpsz_version: *const u16,
            lpsz_referer: *const u16,
            lplpsz_accept_types: *const *const u16,
            dw_flags: u32,
            dw_context: usize,
        ) -> *mut c_void;
        fn HttpSendRequestW(
            h_request: *mut c_void,
            lpsz_headers: *const u16,
            dw_headers_length: u32,
            lp_optional: *const c_void,
            dw_optional_length: u32,
        ) -> i32;
        fn HttpQueryInfoW(
            h_request: *mut c_void,
            dw_info_level: u32,
            lp_buffer: *mut c_void,
            lpdw_buffer_length: *mut u32,
            lpdw_index: *mut u32,
        ) -> i32;
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
        fn InternetCloseHandle(h_internet: *mut c_void) -> i32;
    }

    const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
    const INTERNET_OPEN_TYPE_DIRECT: u32 = 1;
    const INTERNET_SERVICE_HTTP: u32 = 3;
    const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
    const INTERNET_OPTION_SEND_TIMEOUT: u32 = 5;
    const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
    const INTERNET_FLAG_RELOAD: u32 = 0x8000_0000;
    const INTERNET_FLAG_KEEP_CONNECTION: u32 = 0x0040_0000;
    const INTERNET_FLAG_NO_CACHE_WRITE: u32 = 0x0400_0000;
    const INTERNET_FLAG_PRAGMA_NOCACHE: u32 = 0x0000_0100;
    const INTERNET_FLAG_SECURE: u32 = 0x0080_0000;
    const HTTP_QUERY_STATUS_CODE: u32 = 19;
    const HTTP_QUERY_RAW_HEADERS_CRLF: u32 = 22;
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

    pub(super) fn execute(
        spec: &HttpRequestSpec,
        is_cancelled: &dyn Fn() -> bool,
        mut on_chunk: impl FnMut(u16, &[u8]),
    ) -> Result<ExecutedResponse, String> {
        if is_cancelled() {
            return Err("已取消".to_string());
        }
        let parsed = parse_url(&spec.url)?;
        let access_type = if parsed.host.eq_ignore_ascii_case("localhost")
            || parsed.host == "127.0.0.1"
            || parsed.host == "::1"
        {
            INTERNET_OPEN_TYPE_DIRECT
        } else {
            INTERNET_OPEN_TYPE_PRECONFIG
        };

        let user_agent = spec
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| format!("Kestrel-Plugin/{}", crate::app::APP_VERSION));
        let agent_w = to_wide(&user_agent);
        let host_w = to_wide(&parsed.host);
        let method_w = to_wide(&spec.method.to_ascii_uppercase());
        let target_w = to_wide(&parsed.target);

        let mut header_str = String::new();
        for (k, v) in &spec.headers {
            if k.eq_ignore_ascii_case("user-agent") {
                continue;
            }
            let clean_k = k.replace(['\r', '\n'], "");
            let clean_v = v.replace(['\r', '\n'], "");
            header_str.push_str(&clean_k);
            header_str.push_str(": ");
            header_str.push_str(&clean_v);
            header_str.push_str("\r\n");
        }
        let headers_w = to_wide(&header_str);

        let mut flags = INTERNET_FLAG_RELOAD
            | INTERNET_FLAG_KEEP_CONNECTION
            | INTERNET_FLAG_NO_CACHE_WRITE
            | INTERNET_FLAG_PRAGMA_NOCACHE;
        if parsed.secure {
            flags |= INTERNET_FLAG_SECURE;
        }

        let timeout = spec.timeout_ms.max(1_000);

        // SAFETY: All wide buffers and optional body slice remain valid across the
        // synchronous WinINet calls on this single thread; WinInetGuard closes all
        // handles in reverse order on return.
        unsafe {
            let session = InternetOpenW(
                agent_w.as_ptr(),
                access_type,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err("无法初始化 Windows HTTP 会话".to_string());
            }
            let _session_guard = WinInetGuard(session);

            for opt in [
                INTERNET_OPTION_CONNECT_TIMEOUT,
                INTERNET_OPTION_SEND_TIMEOUT,
                INTERNET_OPTION_RECEIVE_TIMEOUT,
            ] {
                InternetSetOptionW(
                    session,
                    opt,
                    (&raw const timeout).cast::<c_void>(),
                    std::mem::size_of::<u32>() as u32,
                );
            }

            let conn = InternetConnectW(
                session,
                host_w.as_ptr(),
                parsed.port,
                std::ptr::null(),
                std::ptr::null(),
                INTERNET_SERVICE_HTTP,
                0,
                0,
            );
            if conn.is_null() {
                let err = std::io::Error::last_os_error();
                return Err(format!("连接 {}:{} 失败: {err}", parsed.host, parsed.port));
            }
            let _conn_guard = WinInetGuard(conn);

            let req = HttpOpenRequestW(
                conn,
                method_w.as_ptr(),
                target_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                flags,
                0,
            );
            if req.is_null() {
                let err = std::io::Error::last_os_error();
                return Err(format!("创建 HTTP 请求失败: {err}"));
            }
            let _req_guard = WinInetGuard(req);

            let (hdr_ptr, hdr_len) = if header_str.is_empty() {
                (std::ptr::null(), 0)
            } else {
                (headers_w.as_ptr(), headers_w.len().saturating_sub(1) as u32)
            };
            let body_bytes = spec.body.as_deref().map(str::as_bytes).unwrap_or(&[]);
            let (body_ptr, body_len) = if body_bytes.is_empty() {
                (std::ptr::null(), 0)
            } else {
                (body_bytes.as_ptr().cast::<c_void>(), body_bytes.len() as u32)
            };

            if HttpSendRequestW(req, hdr_ptr, hdr_len, body_ptr, body_len) == 0 {
                let err = std::io::Error::last_os_error();
                return Err(format!("发送 HTTP 请求失败: {err}"));
            }

            let mut status_code: u32 = 0;
            let mut status_len = std::mem::size_of::<u32>() as u32;
            let mut idx: u32 = 0;
            if HttpQueryInfoW(
                req,
                HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
                (&raw mut status_code).cast::<c_void>(),
                &raw mut status_len,
                &raw mut idx,
            ) == 0
            {
                return Err("读取 HTTP 状态码失败".to_string());
            }
            let status = status_code as u16;

            let mut raw_headers_buf = vec![0u16; 4096];
            let mut raw_headers_bytes = (raw_headers_buf.len() * 2) as u32;
            let mut hdr_idx: u32 = 0;
            let headers = if HttpQueryInfoW(
                req,
                HTTP_QUERY_RAW_HEADERS_CRLF,
                raw_headers_buf.as_mut_ptr().cast::<c_void>(),
                &raw mut raw_headers_bytes,
                &raw mut hdr_idx,
            ) != 0
            {
                let u16_len = (raw_headers_bytes as usize) / 2;
                let text = String::from_utf16_lossy(
                    &raw_headers_buf[..u16_len.min(raw_headers_buf.len())],
                );
                parse_raw_headers(text.trim_matches('\0'))
            } else {
                HashMap::new()
            };

            let mut body = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                if is_cancelled() {
                    return Err("已取消".to_string());
                }
                let mut read: u32 = 0;
                let ok = InternetReadFile(req, buf.as_mut_ptr(), buf.len() as u32, &raw mut read);
                if ok == 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(format!("读取 HTTP 响应失败: {err}"));
                }
                if read == 0 {
                    break;
                }
                let slice = &buf[..read as usize];
                on_chunk(status, slice);
                if body.len() + slice.len() <= MAX_HTTP_BODY {
                    body.extend_from_slice(slice);
                } else if !spec.stream {
                    return Err(format!("HTTP 响应超过上限 {}MB", MAX_HTTP_BODY / (1024 * 1024)));
                }
            }

            Ok(ExecutedResponse {
                status,
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            })
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    pub(super) fn execute(
        spec: &HttpRequestSpec,
        is_cancelled: &dyn Fn() -> bool,
        mut on_chunk: impl FnMut(u16, &[u8]),
    ) -> Result<ExecutedResponse, String> {
        if is_cancelled() {
            return Err("已取消".to_string());
        }
        let _ = parse_url(&spec.url)?;
        let mut cmd = Command::new("curl");
        cmd.args([
            "-sS",
            "-N",
            "--no-buffer",
            "-i",
            "-X",
            &spec.method.to_ascii_uppercase(),
            "--max-time",
            &(spec.timeout_ms.div_ceil(1000).max(1)).to_string(),
        ]);
        for (k, v) in &spec.headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        if spec.body.is_some() {
            cmd.args(["--data-binary", "@-"]);
        }
        cmd.arg(&spec.url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|err| format!("启动 curl 失败: {err}"))?;
        if let Some(body) = &spec.body
            && let Some(mut stdin) = child.stdin.take()
        {
            let _ = stdin.write_all(body.as_bytes());
        }
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法捕获 curl 输出".to_string())?;
        let mut raw_head = Vec::new();
        let mut headers_done = false;
        let mut status: u16 = 0;
        let mut headers = HashMap::new();
        let mut body = Vec::new();
        let mut buf = [0u8; 4096];

        loop {
            if is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("已取消".to_string());
            }
            let n = stdout
                .read(&mut buf)
                .map_err(|err| format!("读取 HTTP 响应失败: {err}"))?;
            if n == 0 {
                break;
            }
            let slice = &buf[..n];
            if !headers_done {
                raw_head.extend_from_slice(slice);
                if let Some(pos) = raw_head.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head_text = String::from_utf8_lossy(&raw_head[..pos]).into_owned();
                    status = head_text
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0);
                    headers = parse_raw_headers(&head_text);
                    let rest = raw_head[pos + 4..].to_vec();
                    headers_done = true;
                    if !rest.is_empty() {
                        on_chunk(status, &rest);
                        if body.len() + rest.len() <= MAX_HTTP_BODY {
                            body.extend_from_slice(&rest);
                        }
                    }
                }
            } else {
                on_chunk(status, slice);
                if body.len() + slice.len() <= MAX_HTTP_BODY {
                    body.extend_from_slice(slice);
                }
            }
        }
        let _ = child.wait();
        Ok(ExecutedResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_https_and_ipv6_urls() {
        let u = parse_url("https://api.openai.com/v1/chat/completions").unwrap();
        assert!(u.secure);
        assert_eq!(u.host, "api.openai.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.target, "/v1/chat/completions");

        let local = parse_url("http://127.0.0.1:11434/api/chat?stream=true").unwrap();
        assert!(!local.secure);
        assert_eq!(local.host, "127.0.0.1");
        assert_eq!(local.port, 11434);
        assert_eq!(local.target, "/api/chat?stream=true");

        let ipv6 = parse_url("http://[::1]:8080/v1").unwrap();
        assert_eq!(ipv6.host, "::1");
        assert_eq!(ipv6.port, 8080);
        assert_eq!(ipv6.target, "/v1");
    }
}
