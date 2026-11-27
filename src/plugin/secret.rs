//! Per-plugin secret storage isolated from `settings.json`.
//!
//! Secrets are persisted to `%APPDATA%\Kestrel\secrets.json` (namespaced by
//! plugin ID). On Windows, values are encrypted using native Windows DPAPI
//! (`CryptProtectData` / `CryptUnprotectData`), bound to the current OS user
//! account, with zero external crate dependencies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::settings::Settings;

type SecretMap = HashMap<String, HashMap<String, String>>;

pub(crate) fn secrets_path() -> PathBuf {
    Settings::config_dir().join("secrets.json")
}

pub(crate) fn get_secret_at(path: &Path, plugin_id: &str, key: &str) -> Result<Option<String>, String> {
    let map = load_map(path);
    let Some(encoded) = map.get(plugin_id).and_then(|ns| ns.get(key)) else {
        return Ok(None);
    };
    decode_secret(encoded).map(Some)
}

pub(crate) fn set_secret_at(
    path: &Path,
    plugin_id: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let mut map = load_map(path);
    let encoded = encode_secret(value)?;
    map.entry(plugin_id.to_string())
        .or_default()
        .insert(key.to_string(), encoded);
    save_map(path, &map)
}

pub(crate) fn delete_secret_at(path: &Path, plugin_id: &str, key: &str) -> Result<(), String> {
    let mut map = load_map(path);
    if let Some(ns) = map.get_mut(plugin_id) {
        ns.remove(key);
        if ns.is_empty() {
            map.remove(plugin_id);
        }
    }
    save_map(path, &map)
}

pub(crate) fn get_secret(plugin_id: &str, key: &str) -> Result<Option<String>, String> {
    get_secret_at(&secrets_path(), plugin_id, key)
}

pub(crate) fn set_secret(plugin_id: &str, key: &str, value: &str) -> Result<(), String> {
    set_secret_at(&secrets_path(), plugin_id, key, value)
}

pub(crate) fn delete_secret(plugin_id: &str, key: &str) -> Result<(), String> {
    delete_secret_at(&secrets_path(), plugin_id, key)
}

fn load_map(path: &Path) -> SecretMap {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<SecretMap>(&text).ok())
        .unwrap_or_default()
}

fn save_map(path: &Path, map: &SecretMap) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("创建凭据目录失败: {err}"))?;
    }
    let json = serde_json::to_string_pretty(map)
        .map_err(|err| format!("序列化凭据失败: {err}"))?;
    std::fs::write(path, json).map_err(|err| format!("写入凭据文件失败: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(windows)]
fn encode_secret(plain: &str) -> Result<String, String> {
    let encrypted = dpapi::protect(plain.as_bytes())?;
    Ok(format!("dpapi:{}", b64_encode(&encrypted)))
}

#[cfg(windows)]
fn decode_secret(stored: &str) -> Result<String, String> {
    if let Some(payload) = stored.strip_prefix("dpapi:") {
        let bytes = b64_decode(payload)?;
        let decrypted = dpapi::unprotect(&bytes)?;
        String::from_utf8(decrypted).map_err(|err| format!("凭据 UTF-8 解码失败: {err}"))
    } else if let Some(payload) = stored.strip_prefix("b64:") {
        let bytes = b64_decode(payload)?;
        String::from_utf8(bytes).map_err(|err| format!("凭据 UTF-8 解码失败: {err}"))
    } else {
        Ok(stored.to_string())
    }
}

#[cfg(not(windows))]
fn encode_secret(plain: &str) -> Result<String, String> {
    Ok(format!("b64:{}", b64_encode(plain.as_bytes())))
}

#[cfg(not(windows))]
fn decode_secret(stored: &str) -> Result<String, String> {
    if let Some(payload) = stored.strip_prefix("b64:") {
        let bytes = b64_decode(payload)?;
        String::from_utf8(bytes).map_err(|err| format!("凭据 UTF-8 解码失败: {err}"))
    } else {
        Ok(stored.to_string())
    }
}

#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            p_data_in: *const DataBlob,
            sz_data_descr: *const u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            p_data_in: *const DataBlob,
            ppsz_data_descr: *mut *mut u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    pub(super) fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let mut input_copy = plain.to_vec();
        let in_blob = DataBlob {
            cb_data: input_copy.len() as u32,
            pb_data: if input_copy.is_empty() {
                std::ptr::null_mut()
            } else {
                input_copy.as_mut_ptr()
            },
        };
        let mut out_blob = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &raw const in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut out_blob,
            )
        };
        if ok == 0 {
            return Err(format!(
                "DPAPI 加密失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = if out_blob.pb_data.is_null() || out_blob.cb_data == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize).to_vec()
            }
        };
        if !out_blob.pb_data.is_null() {
            unsafe {
                LocalFree(out_blob.pb_data.cast::<c_void>());
            }
        }
        Ok(result)
    }

    pub(super) fn unprotect(cipher: &[u8]) -> Result<Vec<u8>, String> {
        let mut input_copy = cipher.to_vec();
        let in_blob = DataBlob {
            cb_data: input_copy.len() as u32,
            pb_data: if input_copy.is_empty() {
                std::ptr::null_mut()
            } else {
                input_copy.as_mut_ptr()
            },
        };
        let mut out_blob = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &raw const in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut out_blob,
            )
        };
        if ok == 0 {
            return Err(format!(
                "DPAPI 解密失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = if out_blob.pb_data.is_null() || out_blob.cb_data == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize).to_vec()
            }
        };
        if !out_blob.pb_data.is_null() {
            unsafe {
                LocalFree(out_blob.pb_data.cast::<c_void>());
            }
        }
        Ok(result)
    }
}

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[((n >> 18) & 63) as usize] as char);
        out.push(B64_TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn b64_val(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some((b - b'A') as u32),
        b'a'..=b'z' => Some((b - b'a' + 26) as u32),
        b'0'..=b'9' => Some((b - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_decode(input: &str) -> Result<Vec<u8>, String> {
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if bytes.len() % 4 != 0 {
        return Err("Base64 长度无效".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let v0 = b64_val(chunk[0]).ok_or_else(|| "Base64 字符无效".to_string())?;
        let v1 = b64_val(chunk[1]).ok_or_else(|| "Base64 字符无效".to_string())?;
        let p2 = chunk[2] == b'=';
        let p3 = chunk[3] == b'=';
        let v2 = if p2 {
            0
        } else {
            b64_val(chunk[2]).ok_or_else(|| "Base64 字符无效".to_string())?
        };
        let v3 = if p3 {
            0
        } else {
            b64_val(chunk[3]).ok_or_else(|| "Base64 字符无效".to_string())?
        };
        let n = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push(((n >> 16) & 0xff) as u8);
        if !p2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if !p3 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_roundtrip_is_encrypted_and_isolated() {
        let dir = std::env::temp_dir().join(format!(
            "kestrel-secret-test-{}",
            std::process::id()
        ));
        let path = dir.join("secrets.json");
        let _ = std::fs::remove_dir_all(&dir);

        set_secret_at(&path, "plugin_a", "api_key", "sk-test-12345").unwrap();
        assert_eq!(
            get_secret_at(&path, "plugin_a", "api_key").unwrap().as_deref(),
            Some("sk-test-12345")
        );
        assert_eq!(get_secret_at(&path, "plugin_b", "api_key").unwrap(), None);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-test-12345"), "secret must not be plaintext");

        delete_secret_at(&path, "plugin_a", "api_key").unwrap();
        assert_eq!(get_secret_at(&path, "plugin_a", "api_key").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
