//! 跨模块复用的 Windows 平台辅助函数（宽字符串构造、DPAPI 加解密等）。
use std::iter;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// 将 `&str` 转为以 NUL 结尾的 UTF-16 宽字符串（`Vec<u16>`），
/// 供 `windows::core::PCWSTR::from_raw(v.as_ptr())` 使用。
///
/// 取代各模块里重复的 `s.encode_utf16().chain(once(0)).collect()` 样板；
/// `update.rs` 与 `roomdir.rs` 中原有的 `wide` 辅助现已统一收敛到此函数。
pub fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(iter::once(0)).collect()
}

/// DPAPI 密文在 INI 中的前缀，用于区分「已加密」与「旧明文」（向后兼容）。
const SECRET_PREFIX: &str = "dpapi:";

/// 用 Windows DPAPI 加密敏感字符串（UTF-8 明文 -> `"dpapi:"` + base64 密文）。
///
/// 失败（如非交互会话、无用户配置文件）时回退为原明文返回，避免崩溃；
/// 调用方据此在保存时仍写入可恢复内容（下次成功时再加密）。
pub fn protect_secret(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    let bytes: Vec<u8> = plain.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let data = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let res = unsafe {
        CryptProtectData(
            &data as *const CRYPT_INTEGER_BLOB,
            PCWSTR(std::ptr::null()),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if let Ok(()) = res {
        let cipher = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
        let encoded = b64_encode(cipher);
        let _ = unsafe { LocalFree(Some(HLOCAL(out.pbData as *mut std::ffi::c_void))) };
        format!("{SECRET_PREFIX}{encoded}")
    } else {
        plain.to_string()
    }
}

/// 解密 DPAPI 密文（`"dpapi:"` + base64）。非本格式（旧明文）或解密失败直接返回原串，
/// 保证旧版明文配置向后兼容、DPAPI 不可用时（如服务会话）仍可启动。
pub fn unprotect_secret(stored: &str) -> String {
    if let Some(b64) = stored.strip_prefix(SECRET_PREFIX) {
        if let Some(cipher) = b64_decode(b64) {
            let data = CRYPT_INTEGER_BLOB {
                cbData: cipher.len() as u32,
                pbData: cipher.as_ptr() as *mut u8,
            };
            let mut out = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            let res = unsafe {
                CryptUnprotectData(
                    &data as *const CRYPT_INTEGER_BLOB,
                    None,
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            };
            if let Ok(()) = res {
                let raw = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
                let units: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let plain = String::from_utf16_lossy(&units);
                let _ = unsafe { LocalFree(Some(HLOCAL(out.pbData as *mut std::ffi::c_void))) };
                return plain;
            }
        }
    }
    stored.to_string()
}

// ---- 内联 base64（避免引入额外依赖；仅用于 DPAPI 二进制密文的 INI 存储） ----
fn b64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).map(|b| *b as u32).unwrap_or(0);
        let b2 = chunk.get(2).map(|b| *b as u32).unwrap_or(0);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(input: &str) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut n: u32 = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        n += 1;
        if n == 4 {
            buf.push((acc >> 16) as u8);
            buf.push((acc >> 8) as u8);
            buf.push(acc as u8);
            acc = 0;
            n = 0;
        }
    }
    if n == 2 {
        buf.push((acc >> 4) as u8);
    } else if n == 3 {
        buf.push((acc >> 10) as u8);
        buf.push((acc >> 2) as u8);
    } else if n != 0 {
        return None;
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_null_terminated() {
        let w = to_wide_null("ab");
        assert_eq!(w, vec!['a' as u16, 'b' as u16, 0]);
    }

    #[test]
    fn secret_roundtrip() {
        let plain = "hunter2!@#";
        let enc = protect_secret(plain);
        assert!(enc.starts_with(SECRET_PREFIX));
        assert_eq!(unprotect_secret(&enc), plain);
        // 旧明文原样返回（向后兼容）
        assert_eq!(unprotect_secret("plaintext"), "plaintext");
        // 空串不加密
        assert_eq!(protect_secret(""), "");
    }
}
