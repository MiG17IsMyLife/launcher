//! ICF 文件解析：SEGA Nu / ALLS 平台的游戏元数据文件（识别 IDAC 用）。
//!
//! 格式复刻自 rakisaionji/icf-reader（MIT，https://github.com/rakisaionji/icf-reader）：
//!
//! - **加密层**：整文件 AES-128-CBC（Rijndael，无填充），固定密钥/IV；按 0x1000 字节
//!   分块独立解密（每块重置 IV），每块明文的前 16 字节再与该块在文件中的偏移量
//!   `location`（小端 u64 ×2）异或；
//! - **头部 0x40 字节**（小端）：`0x00` u32 主 CRC32（= 其后全部字节的 CRC32）、
//!   `0x04` u32 总长、`0x08` u64 填充=0、`0x10` u64 条目数 count（须满足
//!   `0x40*(count+1)==总长`）、`0x18` 4 字符 appid（即游戏 id，如 "SDGT"）、
//!   `0x1C` 3 字节 platid、`0x1F` platgen、`0x20` u32 子 CRC32（启用条目各 0x40
//!   字节 CRC32 的异或累积）、`0x24` 起 7×u32 填充=0；
//! - **条目区**：count 个 0x40 字节条目：`data[0]==2 && data[1]==1` 为启用、
//!   `+0x04` u32 类型（0x0000 SYSTEM / 0x0001 APP / 0x0101 PATCH / 0x0002 OPTION）、
//!   `+0x20` 版本（build u8、minor u8、major u16 LE）。
//!
//! 版本取第一个启用的 APP 条目，格式化为 `major.minor.build`（后两段两位补零）。

use std::path::Path;

use aes::Aes128;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, generic_array::GenericArray};
use cbc::Decryptor;

/// 解析结果：游戏 id（4 字符，如 "SDGT"）与版本（如 "3.21.00"）
#[allow(dead_code)] // IDAC 接入预留接口：识别流程接入后即有调用方
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IcfInfo {
    pub id: String,
    pub version: String,
}

/// 固定主密钥 / IV（SEGA Nu / ALLS 分发格式，与 icf-reader 一致）
const MASTER_KEY: [u8; 16] = [
    0x09, 0xCA, 0x5E, 0xFD, 0x30, 0xC9, 0xAA, 0xEF, 0x38, 0x04, 0xD0, 0xA7, 0xE3, 0xFA, 0x71, 0x20,
];
const MASTER_IV: [u8; 16] = [
    0xB1, 0x55, 0xC2, 0x2C, 0x2E, 0x7F, 0x04, 0x91, 0xFA, 0x7F, 0x0F, 0xDC, 0x21, 0x7A, 0xFF, 0x90,
];

/// AES-CBC 分块大小、头部 / 条目大小
const CHUNK: usize = 0x1000;
const HEAD: usize = 0x40;
const ENTRY: usize = 0x40;

/// APP 条目类型：其版本即游戏版本
const TYPE_APP: u32 = 0x0001;

/// 解析 ICF 文件，返回游戏 id 与版本。
#[allow(dead_code)] // IDAC 接入预留接口：识别流程接入后即有调用方
pub fn parse_icf(path: &Path) -> Result<IcfInfo, String> {
    let cipher = std::fs::read(path)
        .map_err(|e| format!("读取 ICF 文件失败（{}）：{e}", path.display()))?;
    let data =
        decrypt_icf(&cipher).map_err(|e| format!("ICF 解密失败（{}）：{e}", path.display()))?;
    parse_icf_bytes(&data).map_err(|e| format!("ICF 解析失败（{}）：{e}", path.display()))
}

/// 解密 ICF 字节：按 0x1000 块 AES-128-CBC（每块重置 IV、无填充）解密，
/// 每块明文前 16 字节与块偏移（小端 u64 ×2）异或。
fn decrypt_icf(cipher: &[u8]) -> Result<Vec<u8>, String> {
    if cipher.is_empty() {
        return Err("文件为空".to_string());
    }
    if cipher.len() % 16 != 0 {
        return Err(format!(
            "文件长度 {} 不是 AES 块大小（16 字节）的整数倍",
            cipher.len()
        ));
    }
    let mut plain = cipher.to_vec();
    for (idx, chunk) in plain.chunks_mut(CHUNK).enumerate() {
        // 与 C# 实现一致：每个 0x1000 块都新建解密器，CBC 链从 IV 重新开始
        let mut dec = Decryptor::<Aes128>::new_from_slices(&MASTER_KEY, &MASTER_IV)
            .expect("密钥/IV 为固定 16 字节");
        for block in chunk.chunks_exact_mut(16) {
            dec.decrypt_block_mut(GenericArray::from_mut_slice(block));
        }
        let location = (idx * CHUNK) as u64;
        for (i, b) in chunk.iter_mut().take(16).enumerate() {
            *b ^= (location >> ((i % 8) * 8)) as u8;
        }
    }
    Ok(plain)
}

/// 解析解密后的 ICF 明文：头部校验 → 子 CRC32 校验 → 取 APP 条目版本。
fn parse_icf_bytes(data: &[u8]) -> Result<IcfInfo, String> {
    if data.len() < HEAD {
        return Err(format!("数据长度 {} 小于头部大小 0x40", data.len()));
    }
    // 主 CRC32：除前 4 字节外的全部数据
    let crc = crc32fast::hash(&data[4..]);
    if crc != rd_u32(data, 0) {
        return Err(format!(
            "主 CRC32 校验失败：头部记录 0x{:08x}，实际 0x{:08x}",
            rd_u32(data, 0),
            crc
        ));
    }
    let size = rd_u32(data, 4) as usize;
    if size != data.len() {
        return Err(format!("长度字段不符：头部记录 {size}，实际 {}", data.len()));
    }
    if rd_u64(data, 8) != 0 {
        return Err("头部 8 字节填充非零".to_string());
    }
    let count = rd_u64(data, 0x10);
    let expect_len = (count as usize)
        .checked_add(1)
        .and_then(|c| c.checked_mul(ENTRY))
        .ok_or_else(|| format!("条目数 {count} 异常"))?;
    if expect_len != data.len() {
        return Err(format!(
            "条目数不符：count={count} 应得总长 {expect_len:#x}，实际 {:#x}",
            data.len()
        ));
    }
    // appid：头部 4 字节 ASCII
    let id_bytes = &data[0x18..0x1C];
    let id = std::str::from_utf8(id_bytes)
        .map_err(|_| format!("appid 非 ASCII：{:02x?}", id_bytes))?
        .to_string();
    if id.is_empty() {
        return Err("appid 为空".to_string());
    }
    // 头部尾部 7×u32 填充须为 0
    for i in 0..7 {
        if rd_u32(data, 0x24 + i * 4) != 0 {
            return Err(format!("头部尾部第 {i} 个 u32 填充非零"));
        }
    }
    // 子 CRC32：启用条目逐条 CRC32 异或累积
    let mut sub = 0u32;
    for i in 0..count as usize {
        let e = &data[HEAD + i * ENTRY..HEAD + (i + 1) * ENTRY];
        if e[0] == 2 && e[1] == 1 {
            sub ^= crc32fast::hash(e);
        }
    }
    if sub != rd_u32(data, 0x20) {
        return Err(format!(
            "子 CRC32 校验失败：头部记录 0x{:08x}，实际 0x{:08x}",
            rd_u32(data, 0x20),
            sub
        ));
    }
    // 第一个启用的 APP 条目：build u8、minor u8、major u16 LE
    for i in 0..count as usize {
        let e = &data[HEAD + i * ENTRY..HEAD + (i + 1) * ENTRY];
        if !(e[0] == 2 && e[1] == 1) || rd_u32(e, 4) != TYPE_APP {
            continue;
        }
        let build = e[0x20];
        let minor = e[0x21];
        let major = rd_u16(e, 0x22);
        return Ok(IcfInfo {
            id,
            version: format!("{}.{:02}.{:02}", major, minor, build),
        });
    }
    Err("未找到启用的 APP 条目（无游戏版本信息）".to_string())
}

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}

fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().expect("读取范围固定 4 字节"))
}

fn rd_u64(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off + 8].try_into().expect("读取范围固定 8 字节"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::BlockEncryptMut;
    use cbc::Encryptor;

    type EntrySpec = (bool, u32, (u16, u8, u8)); // (启用, 类型, (major, minor, build))

    /// 合成 ICF 明文：头部 + 条目，填好总长 / 主 CRC32 / 子 CRC32 / appid="SDGT"
    fn build_plain_icf(entries: &[EntrySpec]) -> Vec<u8> {
        let count = entries.len();
        let mut data = vec![0u8; HEAD * (count + 1)];
        data[0x18..0x1C].copy_from_slice(b"SDGT");
        data[0x1C..0x1F].copy_from_slice(b"X64");
        data[0x1F] = 2;
        for (i, (enabled, ty, (major, minor, build))) in entries.iter().enumerate() {
            let off = HEAD + i * ENTRY;
            if *enabled {
                data[off] = 2;
                data[off + 1] = 1;
            }
            data[off + 4..off + 8].copy_from_slice(&ty.to_le_bytes());
            data[off + 0x20] = *build;
            data[off + 0x21] = *minor;
            data[off + 0x22..off + 0x24].copy_from_slice(&major.to_le_bytes());
        }
        let mut sub = 0u32;
        for i in 0..count {
            let e = &data[HEAD + i * ENTRY..HEAD + (i + 1) * ENTRY];
            if e[0] == 2 && e[1] == 1 {
                sub ^= crc32fast::hash(e);
            }
        }
        data[0x10..0x18].copy_from_slice(&(count as u64).to_le_bytes());
        data[0x20..0x24].copy_from_slice(&sub.to_le_bytes());
        let total = data.len() as u32;
        data[4..8].copy_from_slice(&total.to_le_bytes());
        let crc = crc32fast::hash(&data[4..]);
        data[0..4].copy_from_slice(&crc.to_le_bytes());
        data
    }

    /// encrypt_icf_bytes：decrypt_icf 的逆过程（异或 → AES-CBC 加密），构造测试夹具
    fn encrypt_icf_bytes(plain: &[u8]) -> Vec<u8> {
        let mut out = plain.to_vec();
        for (idx, chunk) in out.chunks_mut(CHUNK).enumerate() {
            let location = (idx * CHUNK) as u64;
            for (i, b) in chunk.iter_mut().take(16).enumerate() {
                *b ^= (location >> ((i % 8) * 8)) as u8;
            }
            let mut enc = Encryptor::<Aes128>::new_from_slices(&MASTER_KEY, &MASTER_IV)
                .expect("密钥/IV 为固定 16 字节");
            for block in chunk.chunks_exact_mut(16) {
                enc.encrypt_block_mut(GenericArray::from_mut_slice(block));
            }
        }
        out
    }

    fn temp_icf_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dnet_icf_test_{}_{}.bin", std::process::id(), name))
    }

    fn write_temp_icf(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = temp_icf_path(name);
        std::fs::write(&p, bytes).expect("写临时 ICF 失败");
        p
    }

    const SYSTEM: u32 = 0x0000;
    const APP: u32 = 0x0001;

    #[test]
    fn round_trip_sdgt_3_21_00() {
        // SYSTEM 条目在前，APP 条目在后：应跳过 SYSTEM 命中 APP
        let plain = build_plain_icf(&[
            (true, SYSTEM, (2, 0, 0)),
            (true, APP, (3, 21, 0)),
        ]);
        let p = write_temp_icf("roundtrip", &encrypt_icf_bytes(&plain));
        let info = parse_icf(&p).expect("合成 ICF 应解析成功");
        assert_eq!(info.id, "SDGT");
        assert_eq!(info.version, "3.21.00");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn disabled_app_entry_is_skipped() {
        let plain = build_plain_icf(&[
            (false, APP, (3, 21, 0)),
            (true, APP, (2, 0, 1)),
        ]);
        let info = parse_icf_bytes(&plain).expect("应命中第二个启用 APP 条目");
        assert_eq!(info.version, "2.00.01");
    }

    #[test]
    fn version_pads_minor_and_build() {
        let plain = build_plain_icf(&[(true, APP, (12, 3, 7))]);
        let info = parse_icf_bytes(&plain).expect("应解析成功");
        assert_eq!(info.version, "12.03.07");
        let plain = build_plain_icf(&[(true, APP, (3, 21, 5))]);
        assert_eq!(parse_icf_bytes(&plain).unwrap().version, "3.21.05");
    }

    #[test]
    fn multi_chunk_file_round_trips() {
        // 64 条目 → 总长 0x1040，跨两个 0x1000 解密块（验证块偏移异或）
        let mut entries = vec![(true, APP, (3, 21, 0))];
        entries.resize(64, (false, 0xFFFF, (0, 0, 0)));
        let plain = build_plain_icf(&entries);
        assert_eq!(plain.len(), 0x1040);
        let p = write_temp_icf("multichunk", &encrypt_icf_bytes(&plain));
        let info = parse_icf(&p).expect("跨块 ICF 应解析成功");
        assert_eq!(info.version, "3.21.00");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn tampered_cipher_fails_main_crc() {
        let plain = build_plain_icf(&[(true, APP, (3, 21, 0))]);
        let mut cipher = encrypt_icf_bytes(&plain);
        cipher[10] ^= 0xFF;
        let p = write_temp_icf("tampered", &cipher);
        let err = parse_icf(&p).expect_err("篡改密文应失败");
        assert!(err.contains("主 CRC32"), "实际错误：{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn truncated_data_fails_count_check() {
        // 截掉一个条目并同步修正头部的总长 / 主 CRC32，让失败确定性地落在条目数校验
        let mut plain = build_plain_icf(&[(true, APP, (3, 21, 0))]);
        let cut = plain.len() - ENTRY;
        plain.truncate(cut);
        let total = plain.len() as u32;
        plain[4..8].copy_from_slice(&total.to_le_bytes());
        let crc = crc32fast::hash(&plain[4..]);
        plain[0..4].copy_from_slice(&crc.to_le_bytes());
        let err = parse_icf_bytes(&plain).expect_err("截断应失败");
        assert!(err.contains("条目数"), "实际错误：{err}");
    }

    #[test]
    fn too_short_data_fails() {
        let err = parse_icf_bytes(&[0u8; 0x20]).expect_err("过短应失败");
        assert!(err.contains("小于头部大小"), "实际错误：{err}");
    }

    #[test]
    fn non_block_sized_file_fails() {
        let p = write_temp_icf("garbage", &[0xABu8; 100]);
        let err = parse_icf(&p).expect_err("非 16 倍数长度应失败");
        assert!(err.contains("AES 块大小"), "实际错误：{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn no_enabled_app_entry_fails() {
        let plain = build_plain_icf(&[(true, SYSTEM, (2, 0, 0))]);
        let err = parse_icf_bytes(&plain).expect_err("无 APP 条目应失败");
        assert!(err.contains("APP 条目"), "实际错误：{err}");
    }

    #[test]
    fn decrypt_encrypt_round_trip_is_identity() {
        let plain = build_plain_icf(&[(true, APP, (3, 21, 0))]);
        let cipher = encrypt_icf_bytes(&plain);
        assert_eq!(decrypt_icf(&cipher).unwrap(), plain);
    }
}
