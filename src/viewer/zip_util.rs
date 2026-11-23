//! Minimal, zero-bloat ZIP reader powered by `flate2` for `.docx` and `.xlsx` files.

use std::collections::HashMap;
use std::io::Read;

use flate2::read::DeflateDecoder;

/// Maximum uncompressed size allowed when reading a single ZIP entry (64 MiB).
pub const MAX_ZIP_ENTRY_BYTES: usize = 64 * 1024 * 1024;

fn read_u16_le(data: &[u8], off: usize) -> Result<u16, String> {
    let bytes: [u8; 2] = data
        .get(off..off + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| "ZIP 文件头截断".to_string())?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(data: &[u8], off: usize) -> Result<u32, String> {
    let bytes: [u8; 4] = data
        .get(off..off + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| "ZIP 文件头截断".to_string())?;
    Ok(u32::from_le_bytes(bytes))
}

#[derive(Clone, Debug)]
struct ZipEntry {
    method: u16,
    comp_size: usize,
    uncomp_size: usize,
    local_offset: usize,
}

pub struct ZipArchive {
    data: Vec<u8>,
    entries: HashMap<String, ZipEntry>,
}

impl ZipArchive {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, String> {
        let eocd = find_eocd(&data).ok_or_else(|| "无效的 ZIP 文件格式（缺少目录尾）".to_string())?;
        if eocd + 22 > data.len() {
            return Err("ZIP 目录尾截断".to_string());
        }
        let cd_size = read_u32_le(&data, eocd + 12)? as usize;
        let cd_offset = read_u32_le(&data, eocd + 16)? as usize;
        if cd_offset.saturating_add(cd_size) > data.len() {
            return Err("ZIP 中央目录越界".to_string());
        }

        let mut entries = HashMap::new();
        let mut pos = cd_offset;
        let cd_end = cd_offset + cd_size;

        while pos + 46 <= cd_end {
            if &data[pos..pos + 4] != b"PK\x01\x02" {
                break;
            }
            let method = read_u16_le(&data, pos + 10)?;
            let comp_size = read_u32_le(&data, pos + 20)? as usize;
            let uncomp_size = read_u32_le(&data, pos + 24)? as usize;
            let name_len = read_u16_le(&data, pos + 28)? as usize;
            let extra_len = read_u16_le(&data, pos + 30)? as usize;
            let comment_len = read_u16_le(&data, pos + 32)? as usize;
            let local_offset = read_u32_le(&data, pos + 42)? as usize;

            let name_start = pos + 46;
            let name_end = name_start + name_len;
            if name_end > cd_end {
                break;
            }
            let name = String::from_utf8_lossy(&data[name_start..name_end])
                .replace('\\', "/");
            entries.insert(
                name,
                ZipEntry {
                    method,
                    comp_size,
                    uncomp_size,
                    local_offset,
                },
            );
            pos = name_end + extra_len + comment_len;
        }

        Ok(Self { data, entries })
    }

    pub fn entry_names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn entry_metas(&self) -> Vec<(String, usize, usize, u16)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .map(|(name, e)| (name.clone(), e.uncomp_size, e.comp_size, e.method))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn read_entry(&self, name: &str) -> Result<Vec<u8>, String> {
        let normalized = name.trim_start_matches('/').replace('\\', "/");
        let entry = self
            .entries
            .get(&normalized)
            .or_else(|| {
                let lower = normalized.to_ascii_lowercase();
                self.entries
                    .iter()
                    .find(|(k, _)| k.to_ascii_lowercase() == lower)
                    .map(|(_, v)| v)
            })
            .ok_or_else(|| format!("缺少 ZIP 条目: {normalized}"))?;

        let off = entry.local_offset;
        if off + 30 > self.data.len() || &self.data[off..off + 4] != b"PK\x03\x04" {
            return Err(format!("无效的本地文件头: {normalized}"));
        }
        let name_len = read_u16_le(&self.data, off + 26)? as usize;
        let extra_len = read_u16_le(&self.data, off + 28)? as usize;
        let data_start = off + 30 + name_len + extra_len;
        let data_end = data_start.saturating_add(entry.comp_size);
        if data_end > self.data.len() {
            return Err(format!("ZIP 条目数据越界: {normalized}"));
        }
        let slice = &self.data[data_start..data_end];
        match entry.method {
            0 => {
                if slice.len() > MAX_ZIP_ENTRY_BYTES {
                    return Err(format!("ZIP 条目超过 64 MB 大小上限: {normalized}"));
                }
                Ok(slice.to_vec())
            }
            8 => {
                let mut out = Vec::with_capacity(entry.uncomp_size.min(MAX_ZIP_ENTRY_BYTES));
                let mut decoder = DeflateDecoder::new(slice).take((MAX_ZIP_ENTRY_BYTES + 1) as u64);
                decoder
                    .read_to_end(&mut out)
                    .map_err(|e| format!("解压 {normalized} 失败: {e}"))?;
                if out.len() > MAX_ZIP_ENTRY_BYTES {
                    return Err(format!("ZIP 条目解压超过 64 MB 安全上限: {normalized}"));
                }
                Ok(out)
            }
            other => Err(format!("不支持的 ZIP 压缩方式 ({other}): {normalized}")),
        }
    }

    pub fn read_entry_string(&self, name: &str) -> Result<String, String> {
        let bytes = self.read_entry(name)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let min_pos = data.len().saturating_sub(22 + 65_535);
    let mut i = data.len() - 22;
    loop {
        if &data[i..i + 4] == b"PK\x05\x06" {
            return Some(i);
        }
        if i <= min_pos {
            break;
        }
        i -= 1;
    }
    None
}

#[cfg(test)]
pub(crate) fn build_test_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    let mut out = Vec::new();
    let mut cd = Vec::new();

    for (name, content) in files {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(content).unwrap();
        let compressed = enc.finish().unwrap();
        let local_offset = out.len() as u32;
        let name_bytes = name.as_bytes();

        // Local file header
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&compressed);

        // Central directory header
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&8u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(content.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&local_offset.to_le_bytes());
        cd.extend_from_slice(name_bytes);
    }

    let cd_offset = out.len() as u32;
    let cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);

    // EOCD
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_deflated_entries_from_zip() {
        let zip_bytes = build_test_zip(&[
            ("hello.txt", b"Hello Kestrel!"),
            ("nested/data.xml", b"<root><item>42</item></root>"),
        ]);
        let archive = ZipArchive::from_bytes(zip_bytes).unwrap();
        assert_eq!(archive.read_entry_string("hello.txt").unwrap(), "Hello Kestrel!");
        assert_eq!(
            archive.read_entry_string("NESTED\\data.xml").unwrap(),
            "<root><item>42</item></root>"
        );
    }
}
