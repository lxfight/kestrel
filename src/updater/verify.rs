//! Pure-Rust FIPS 180-4 SHA-256 verification and streaming ZIP binary extraction.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flate2::Crc;
use flate2::read::DeflateDecoder;

/// Maximum uncompressed binary size accepted when extracting a portable update ZIP (256 MiB).
pub const MAX_EXTRACTED_BINARY_BYTES: u64 = 256 * 1024 * 1024;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Streaming SHA-256 hasher (FIPS 180-4) with zero external crate dependencies.
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_bytes: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0u8; 64],
            block_len: 0,
            total_bytes: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_bytes = self.total_bytes.wrapping_add(data.len() as u64);

        if self.block_len > 0 {
            let take = (64 - self.block_len).min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == 64 {
                let block = self.block;
                self.compress(&block);
                self.block_len = 0;
            }
        }

        while data.len() >= 64 {
            let mut chunk = [0u8; 64];
            chunk.copy_from_slice(&data[..64]);
            self.compress(&chunk);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.block[..data.len()].copy_from_slice(data);
            self.block_len = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let total_bits = self.total_bytes.wrapping_mul(8);
        self.block[self.block_len] = 0x80;
        self.block_len += 1;

        if self.block_len > 56 {
            self.block[self.block_len..64].fill(0);
            let block = self.block;
            self.compress(&block);
            self.block.fill(0);
        } else {
            self.block[self.block_len..56].fill(0);
        }

        self.block[56..64].copy_from_slice(&total_bits.to_be_bytes());
        let block = self.block;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (idx, word) in self.state.iter().enumerate() {
            out[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    pub fn finalize_hex(self) -> String {
        let digest = self.finalize();
        let mut hex = String::with_capacity(64);
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg(test)]
pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize_hex()
}

pub fn sha256_file_hex(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("无法打开待校验文件: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("读取待校验文件失败: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

pub fn sha256_hex_matches(actual: &str, expected: &str) -> bool {
    let a = actual.trim();
    let e = expected.trim();
    if a.len() != 64 || e.len() != 64 {
        return false;
    }
    let mut diff = 0u8;
    for (ab, eb) in a.bytes().zip(e.bytes()) {
        diff |= ab.to_ascii_lowercase() ^ eb.to_ascii_lowercase();
    }
    diff == 0
}

#[derive(Debug)]
struct CdEntry {
    name: String,
    method: u16,
    crc32: u32,
    comp_size: u64,
    uncomp_size: u64,
    local_offset: u64,
}

/// Extracts a single target executable from a portable update ZIP archive using
/// streaming decompression, preventing ZipSlip traversal and decompression bombs.
pub fn extract_zip_binary(
    zip_path: &Path,
    expected_file_name: &str,
    dest_path: &Path,
    max_bytes: u64,
) -> Result<(u64, String), String> {
    let mut file = File::open(zip_path).map_err(|e| format!("无法打开更新压缩包: {e}"))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("无法读取压缩包元数据: {e}"))?
        .len();
    if file_len < 22 {
        return Err("更新压缩包已损坏（文件过小）".to_string());
    }

    let tail_len = file_len.min(22 + 65_535) as usize;
    let tail_offset = file_len - tail_len as u64;
    file.seek(SeekFrom::Start(tail_offset))
        .map_err(|e| format!("定位 ZIP 目录尾失败: {e}"))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)
        .map_err(|e| format!("读取 ZIP 目录尾失败: {e}"))?;

    let eocd_rel = find_eocd(&tail).ok_or_else(|| "无效的 ZIP 格式（未找到目录尾）".to_string())?;
    let cd_size = read_u32_le(&tail, eocd_rel + 12)? as u64;
    let cd_offset = read_u32_le(&tail, eocd_rel + 16)? as u64;
    if cd_size > 16 * 1024 * 1024 || cd_offset.saturating_add(cd_size) > file_len {
        return Err("ZIP 中央目录异常或越界".to_string());
    }

    file.seek(SeekFrom::Start(cd_offset))
        .map_err(|e| format!("定位 ZIP 中央目录失败: {e}"))?;
    let mut cd = vec![0u8; cd_size as usize];
    file.read_exact(&mut cd)
        .map_err(|e| format!("读取 ZIP 中央目录失败: {e}"))?;

    let entries = parse_central_directory(&cd)?;
    let target = select_binary_entry(&entries, expected_file_name)?;
    if target.uncomp_size > max_bytes {
        return Err("更新包内可执行文件超出安全大小限制".to_string());
    }

    file.seek(SeekFrom::Start(target.local_offset))
        .map_err(|e| format!("定位 ZIP 文件头失败: {e}"))?;
    let mut local_hdr = [0u8; 30];
    file.read_exact(&mut local_hdr)
        .map_err(|e| format!("读取 ZIP 本地文件头失败: {e}"))?;
    if &local_hdr[0..4] != b"PK\x03\x04" {
        return Err("无效的 ZIP 本地文件头".to_string());
    }
    let name_len = read_u16_le(&local_hdr, 26)? as u64;
    let extra_len = read_u16_le(&local_hdr, 28)? as u64;
    let data_start = target.local_offset + 30 + name_len + extra_len;
    if data_start.saturating_add(target.comp_size) > file_len {
        return Err("ZIP 条目数据越界".to_string());
    }
    file.seek(SeekFrom::Start(data_start))
        .map_err(|e| format!("定位 ZIP 数据区失败: {e}"))?;

    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建暂存目录失败: {e}"))?;
    }
    let out_file = File::create(dest_path).map_err(|e| format!("创建解压目标文件失败: {e}"))?;
    let mut writer = BufWriter::new(out_file);
    let mut crc = Crc::new();
    let mut sha = Sha256::new();

    let extract_res = match target.method {
        0 => stream_copy_checked(
            file.take(target.comp_size),
            &mut writer,
            &mut crc,
            &mut sha,
            max_bytes,
        ),
        8 => {
            let limited = file.take(target.comp_size);
            let decoder = DeflateDecoder::new(limited);
            stream_copy_checked(decoder, &mut writer, &mut crc, &mut sha, max_bytes)
        }
        other => Err(format!("不支持的 ZIP 压缩方式 ({other})")),
    };

    let written = match extract_res {
        Ok(n) => n,
        Err(err) => {
            drop(writer);
            let _ = fs::remove_file(dest_path);
            return Err(err);
        }
    };

    if let Err(err) = writer.flush() {
        drop(writer);
        let _ = fs::remove_file(dest_path);
        return Err(format!("写入解压文件失败: {err}"));
    }
    drop(writer);

    // Some minimal ZIP generators leave CRC=0 when untested; verify whenever present.
    if target.crc32 != 0 && crc.sum() != target.crc32 {
        let _ = fs::remove_file(dest_path);
        return Err("更新包解压 CRC 校验不匹配".to_string());
    }
    if target.uncomp_size > 0 && written != target.uncomp_size {
        let _ = fs::remove_file(dest_path);
        return Err("更新包解压长度与清单不符".to_string());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dest_path, fs::Permissions::from_mode(0o755));
    }

    Ok((written, sha.finalize_hex()))
}

fn stream_copy_checked<R: Read>(
    mut reader: R,
    writer: &mut BufWriter<File>,
    crc: &mut Crc,
    sha: &mut Sha256,
    max_bytes: u64,
) -> Result<u64, String> {
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("解压更新包失败: {e}"))?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > max_bytes {
            return Err("更新包解压体积超过安全上限".to_string());
        }
        let chunk = &buf[..n];
        crc.update(chunk);
        sha.update(chunk);
        writer
            .write_all(chunk)
            .map_err(|e| format!("写入解压文件失败: {e}"))?;
    }
    Ok(total)
}

fn parse_central_directory(cd: &[u8]) -> Result<Vec<CdEntry>, String> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos + 46 <= cd.len() {
        if &cd[pos..pos + 4] != b"PK\x01\x02" {
            break;
        }
        let method = read_u16_le(cd, pos + 10)?;
        let crc32 = read_u32_le(cd, pos + 16)?;
        let comp_size = read_u32_le(cd, pos + 20)? as u64;
        let uncomp_size = read_u32_le(cd, pos + 24)? as u64;
        let name_len = read_u16_le(cd, pos + 28)? as usize;
        let extra_len = read_u16_le(cd, pos + 30)? as usize;
        let comment_len = read_u16_le(cd, pos + 32)? as usize;
        let local_offset = read_u32_le(cd, pos + 42)? as u64;

        let name_start = pos + 46;
        let name_end = name_start + name_len;
        if name_end > cd.len() {
            return Err("ZIP 文件名越界".to_string());
        }
        let name = String::from_utf8_lossy(&cd[name_start..name_end]).replace('\\', "/");
        if !is_safe_zip_entry_path(&name) {
            return Err(format!("更新压缩包包含不安全路径: {name}"));
        }
        entries.push(CdEntry {
            name,
            method,
            crc32,
            comp_size,
            uncomp_size,
            local_offset,
        });
        pos = name_end + extra_len + comment_len;
    }
    Ok(entries)
}

fn is_safe_zip_entry_path(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.contains('\0') {
        return false;
    }
    if name.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    !name.split('/').any(|seg| seg == "..")
}

fn select_binary_entry<'a>(
    entries: &'a [CdEntry],
    expected_file_name: &str,
) -> Result<&'a CdEntry, String> {
    let expected_lower = expected_file_name.trim().to_ascii_lowercase();
    if expected_lower.is_empty() {
        return Err("更新清单未指定目标可执行文件名".to_string());
    }
    // Prefer exact top-level filename match first, then a single nested folder match.
    if let Some(entry) = entries
        .iter()
        .find(|e| !e.name.ends_with('/') && e.name.to_ascii_lowercase() == expected_lower)
    {
        return Ok(entry);
    }
    entries
        .iter()
        .find(|e| {
            !e.name.ends_with('/')
                && e.name
                    .rsplit('/')
                    .next()
                    .is_some_and(|base| base.to_ascii_lowercase() == expected_lower)
        })
        .ok_or_else(|| format!("更新压缩包中未找到目标文件: {expected_file_name}"))
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let mut i = data.len() - 22;
    loop {
        if &data[i..i + 4] == b"PK\x05\x06" {
            return Some(i);
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    None
}

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

#[cfg(test)]
pub(crate) fn build_zip_with_crc(files: &[(&str, &[u8])]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;

    let mut out = Vec::new();
    let mut cd = Vec::new();

    for (name, content) in files {
        let mut crc = Crc::new();
        crc.update(content);
        let crc_val = crc.sum();

        let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(content).unwrap();
        let compressed = enc.finish().unwrap();
        let local_offset = out.len() as u32;
        let name_bytes = name.as_bytes();

        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc_val.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&compressed);

        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&8u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&crc_val.to_le_bytes());
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
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_bytes_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_bytes_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_bytes_hex(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert!(sha256_hex_matches(
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
    }

    #[test]
    fn extracts_binary_and_rejects_path_traversal_and_oversize() {
        let dir = std::env::temp_dir().join(format!("kestrel-verify-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let payload = b"MZ-fake-kestrel-binary-v0.2.0";
        let zip_bytes = build_zip_with_crc(&[("Kestrel.exe", payload)]);
        let zip_path = dir.join("ok.zip");
        fs::write(&zip_path, &zip_bytes).unwrap();

        let out_path = dir.join("extracted.exe");
        let (size, sha) = extract_zip_binary(&zip_path, "Kestrel.exe", &out_path, 1024).unwrap();
        assert_eq!(size as usize, payload.len());
        assert_eq!(sha, sha256_bytes_hex(payload));
        assert_eq!(fs::read(&out_path).unwrap(), payload);

        // Oversize rejection.
        let err = extract_zip_binary(&zip_path, "Kestrel.exe", &out_path, 8).unwrap_err();
        assert!(err.contains("上限") || err.contains("限制"));

        // Path traversal rejection.
        let evil_zip = build_zip_with_crc(&[("../evil.exe", b"bad")]);
        let evil_path = dir.join("evil.zip");
        fs::write(&evil_path, &evil_zip).unwrap();
        let err = extract_zip_binary(&evil_path, "evil.exe", &out_path, 1024).unwrap_err();
        assert!(err.contains("不安全路径"));

        let _ = fs::remove_dir_all(&dir);
    }
}
