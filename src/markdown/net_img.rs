//! Async remote image (`http://`, `https://`) and `data:image/...` loader for
//! Markdown & HTML previews.
//!
//! On Windows, downloads run through `wininet.dll` (`INTERNET_OPEN_TYPE_PRECONFIG`),
//! which automatically inherits Windows System Proxy settings (Clash/v2ray/Edge),
//! system TLS root certificates, and HTTP redirects without spawning subprocesses.
//! Results are cached on disk under `%TEMP%/kestrel-img-cache/` for instant reopens.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use egui::ColorImage;

use crate::markdown::svg::{SvgGraphic, looks_like_svg, parse_svg};

const MAX_REMOTE_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const FETCH_WORKERS: usize = 4;

pub enum DecodedImage {
    Raster {
        image: ColorImage,
        size: [usize; 2],
    },
    Svg(Arc<SvgGraphic>),
}

pub struct RemoteImageResult {
    pub url: String,
    pub result: Result<DecodedImage, String>,
}

struct FetchJob {
    url: String,
    ctx: egui::Context,
    reply: Sender<RemoteImageResult>,
}

struct FetchPool {
    queue: Arc<(Mutex<VecDeque<FetchJob>>, Condvar)>,
}

impl FetchPool {
    fn global() -> &'static Self {
        static POOL: LazyLock<FetchPool> = LazyLock::new(FetchPool::new);
        &POOL
    }

    fn new() -> Self {
        let queue = Arc::new((Mutex::new(VecDeque::<FetchJob>::new()), Condvar::new()));
        for idx in 0..FETCH_WORKERS {
            let shared = Arc::clone(&queue);
            let _ = std::thread::Builder::new()
                .name(format!("kestrel-img-fetch-{idx}"))
                .spawn(move || fetch_worker(shared));
        }
        Self { queue }
    }

    fn submit(&self, job: FetchJob) {
        let (lock, cvar) = &*self.queue;
        if let Ok(mut guard) = lock.lock() {
            guard.push_back(job);
            cvar.notify_one();
        }
    }
}

fn fetch_worker(shared: Arc<(Mutex<VecDeque<FetchJob>>, Condvar)>) {
    let (lock, cvar) = &*shared;
    loop {
        let job = {
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            while guard.is_empty() {
                match cvar.wait(guard) {
                    Ok(next) => guard = next,
                    Err(_) => return,
                }
            }
            guard.pop_front()
        };
        let Some(job) = job else {
            continue;
        };
        let result = fetch_and_decode(&job.url);
        let ctx = job.ctx;
        let _ = job.reply.send(RemoteImageResult {
            url: job.url,
            result,
        });
        ctx.request_repaint();
    }
}

pub struct RemoteImageQueue {
    tx: Sender<RemoteImageResult>,
    rx: Receiver<RemoteImageResult>,
    in_flight: HashSet<String>,
}

impl Default for RemoteImageQueue {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            in_flight: HashSet::new(),
        }
    }
}

impl RemoteImageQueue {
    pub fn request(&mut self, url: &str, ctx: &egui::Context) {
        if !self.in_flight.insert(url.to_string()) {
            return;
        }
        FetchPool::global().submit(FetchJob {
            url: url.to_string(),
            ctx: ctx.clone(),
            reply: self.tx.clone(),
        });
    }

    pub fn is_loading(&self, url: &str) -> bool {
        self.in_flight.contains(url)
    }

    pub fn drain(&mut self) -> Vec<RemoteImageResult> {
        let mut out = Vec::new();
        while let Ok(item) = self.rx.try_recv() {
            self.in_flight.remove(&item.url);
            out.push(item);
        }
        out
    }
}

pub fn decode_image_bytes(bytes: &[u8]) -> Result<DecodedImage, String> {
    if bytes.is_empty() {
        return Err("图片数据为空".to_string());
    }
    if looks_like_svg(bytes) {
        return parse_svg(bytes).map(|g| DecodedImage::Svg(Arc::new(g)));
    }
    let dyn_img = image::load_from_memory(bytes).map_err(|e| format!("图片解码失败: {e}"))?;
    let rgba = dyn_img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Ok(DecodedImage::Raster {
        image,
        size: [w as usize, h as usize],
    })
}

pub fn decode_data_uri_bytes(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if meta.to_ascii_lowercase().contains(";base64") {
        decode_base64(payload)
    } else {
        Some(percent_decode(payload))
    }
}

fn fetch_and_decode(url: &str) -> Result<DecodedImage, String> {
    if url.starts_with("data:") {
        let bytes = decode_data_uri_bytes(url).ok_or_else(|| "无效的 data: 图片 URI".to_string())?;
        return decode_image_bytes(&bytes);
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("不支持的图片协议".to_string());
    }
    if let Some(cached) = read_disk_cache(url)
        && let Ok(decoded) = decode_image_bytes(&cached)
    {
        return Ok(decoded);
    }
    let bytes = download_url(url)?;
    let decoded = decode_image_bytes(&bytes)?;
    write_disk_cache(url, &bytes);
    Ok(decoded)
}

fn cache_path_for(url: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    std::env::temp_dir()
        .join("kestrel-img-cache")
        .join(format!("{hash:016x}.bin"))
}

fn read_disk_cache(url: &str) -> Option<Vec<u8>> {
    let path = cache_path_for(url);
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    if SystemTime::now().duration_since(modified).ok()? > CACHE_TTL {
        return None;
    }
    if meta.len() as usize > MAX_REMOTE_IMAGE_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

fn write_disk_cache(url: &str, bytes: &[u8]) {
    let path = cache_path_for(url);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, bytes);
}

#[cfg(windows)]
fn download_url(url: &str) -> Result<Vec<u8>, String> {
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
    const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
    const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
    const INTERNET_FLAG_RELOAD: u32 = 0x8000_0000;
    const INTERNET_FLAG_NO_CACHE_WRITE: u32 = 0x0400_0000;
    const INTERNET_FLAG_PRAGMA_NOCACHE: u32 = 0x0000_0100;
    const HTTP_QUERY_STATUS_CODE: u32 = 19;
    const HTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

    let agent: Vec<u16> = OsStr::new("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Kestrel/0.1")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let url_wide: Vec<u16> = OsStr::new(url).encode_wide().chain(Some(0)).collect();
    let headers: Vec<u16> = OsStr::new("Accept: image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.9,*/*;q=0.5\r\n")
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SAFETY: All pointers are NUL-terminated UTF-16 buffers alive across WinINet calls,
    // and handles are closed before returning.
    unsafe {
        let session = InternetOpenW(
            agent.as_ptr(),
            INTERNET_OPEN_TYPE_PRECONFIG,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return Err("无法初始化系统网络会话".to_string());
        }
        let timeout_ms: u32 = 12_000;
        InternetSetOptionW(
            session,
            INTERNET_OPTION_CONNECT_TIMEOUT,
            (&raw const timeout_ms).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        );
        InternetSetOptionW(
            session,
            INTERNET_OPTION_RECEIVE_TIMEOUT,
            (&raw const timeout_ms).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        );

        let handle = InternetOpenUrlW(
            session,
            url_wide.as_ptr(),
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE | INTERNET_FLAG_PRAGMA_NOCACHE,
            0,
        );
        if handle.is_null() {
            InternetCloseHandle(session);
            return Err("网络请求失败".to_string());
        }

        let mut status_code: u32 = 0;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        let mut idx: u32 = 0;
        if HttpQueryInfoW(
            handle,
            HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
            (&raw mut status_code).cast::<c_void>(),
            &raw mut status_len,
            &raw mut idx,
        ) != 0
            && status_code >= 400
        {
            InternetCloseHandle(handle);
            InternetCloseHandle(session);
            return Err(format!("HTTP {status_code}"));
        }

        let mut data = Vec::with_capacity(16 * 1024);
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let mut read: u32 = 0;
            let ok = InternetReadFile(
                handle,
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                &raw mut read,
            );
            if ok == 0 {
                InternetCloseHandle(handle);
                InternetCloseHandle(session);
                return Err("读取网络响应失败".to_string());
            }
            if read == 0 {
                break;
            }
            if data.len() + read as usize > MAX_REMOTE_IMAGE_BYTES {
                InternetCloseHandle(handle);
                InternetCloseHandle(session);
                return Err("远程图片超过 16 MB 限制".to_string());
            }
            data.extend_from_slice(&chunk[..read as usize]);
        }
        InternetCloseHandle(handle);
        InternetCloseHandle(session);
        Ok(data)
    }
}

#[cfg(not(windows))]
fn download_url(url: &str) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "12",
            "--max-filesize",
            &MAX_REMOTE_IMAGE_BYTES.to_string(),
            url,
        ])
        .output()
        .map_err(|e| format!("无法调用 curl: {e}"))?;
    if !out.status.success() {
        return Err("远程图片下载失败".to_string());
    }
    Ok(out.stdout)
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_and_svg_data_uris() {
        let svg_uri = "data:image/svg+xml;utf8,%3Csvg%20width%3D%2240%22%20height%3D%2220%22%3E%3Crect%20width%3D%2240%22%20height%3D%2220%22%20fill%3D%22red%22/%3E%3C/svg%3E";
        let bytes = decode_data_uri_bytes(svg_uri).unwrap();
        assert!(matches!(decode_image_bytes(&bytes), Ok(DecodedImage::Svg(_))));

        let b64 = decode_base64("SGVsbG8=").unwrap();
        assert_eq!(b64, b"Hello");
    }
}
