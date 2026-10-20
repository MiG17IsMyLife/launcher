//! initialD.ini 私服改写（仅 D4-D7，启动游戏前）。
//!
//! 这些游戏（头文字D 4/5/6/7）目录下的 `initialD.ini` 需要把服务器地址 / 端口 /
//! 公告 URL 改成私服的对应值。私服地址取自网络设置「服务器地址」（未设置时回退
//! `crate::server::SERVER_HOST`，即 sdgt.arcadehub.xyz）。该文件是 **Shift-JIS 编码**、
//! 行尾 CRLF、混有大量日文注释 —— 既不能用 `include_str!`/UTF-8 读写（会破坏编码），
//! 也不能用现有 `ini` 模块（UTF-8 且按 `[section]` 精确匹配，而本文件节名是带空格的
//! `[ Network ]`）。因此这里一律**字节级**处理：按 `\n` 切行、保留 `\r`、只替换 `=`
//! 之后的值部分，左侧原始字节（含对齐空格）与注释全部原样透传。
//!
//! 适用范围按识别库 `game_id` 白名单判定（SBML/SBQZ/SBTS/SBUU/SBVW/SBWV/SBYD/SBYY），
//! 即 D4-D7；D8（SBZZ/SDAL）与未命中识别库的游戏不改写。

use std::fs;
use std::path::{Path, PathBuf};

/// 需要改写的游戏目录文件名（实际为 `initialD.ini`，按大小写不敏感查找）。
const INI_NAME: &str = "initialD.ini";

/// 公告 URL 端口（私服 news 服务固定端口）。
const NEWS_PORT: u16 = 8086;

/// 服务器地址：空白（未设置）时回退默认 [`crate::server::SERVER_HOST`]。
fn host_or_default(host: &str) -> &str {
    let h = host.trim();
    if h.is_empty() {
        crate::server::SERVER_HOST
    } else {
        h
    }
}

/// 该 game_id 是否属于需要改写的 D4-D7（其它 / 未命中识别库一律不改）。
#[allow(dead_code)] // 对外查询接口，当前由 patch_game_ini 内部判定即可
pub fn supports(game_id: Option<&str>) -> bool {
    game_id.and_then(idas_version).is_some()
}

/// game_id → idas 版本号（4/5/6/7）；D8 与未知返回 None（不改写）。
fn idas_version(game_id: &str) -> Option<u8> {
    match game_id {
        "SBML" => Some(4),                       // D4 / D4 kai
        "SBQZ" | "SBTS" => Some(5),              // D5 / D5 EXP 2.0
        "SBUU" | "SBVW" | "SBWV" => Some(6),     // D6AA 各版
        "SBYD" | "SBYY" => Some(7),              // D7AAX 各版
        _ => None,                               // SBZZ / SDAL (D8) 等
    }
}

/// 改写某游戏目录下的 initialD.ini（大小写不敏感查找）。
///
/// `host` 为网络设置「服务器地址」（域名或 IP，可含空白；空白回退默认）。
///
/// 返回 `Ok(true)` = 已改写并写盘；`Ok(false)` = 未命中白名单 / 文件不存在 /
/// 内容无需变化。失败（读写异常）**不阻断启动流程**，返回 `Ok(false)` 静默跳过。
pub fn patch_game_ini(game_dir: &Path, game_id: Option<&str>, host: &str) -> Result<bool, String> {
    let Some(idas) = game_id.and_then(idas_version) else {
        return Ok(false); // 非 D4-D7，不动
    };
    let Some(path) = find_ini(game_dir) else {
        return Ok(false); // 没有该文件：静默跳过（不阻断启动）
    };
    let src = match fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Ok(false),
    };
    let out = patch_bytes(&src, idas, host);
    if out == src {
        return Ok(false); // 幂等：内容无变化，不动 mtime
    }
    // 临时文件 + rename 原子写（与 kitten::write_config 同款，避免中途崩溃留半文件）
    let tmp = path.with_extension("ini.tmp");
    fs::write(&tmp, &out)
        .map_err(|e| format!("写入 initialD.ini 临时文件失败（{}）：{e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("替换 initialD.ini 失败（{}）：{e}", path.display())
    })?;
    Ok(true)
}

/// 大小写不敏感查找游戏目录内的 initialD.ini。
fn find_ini(game_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(game_dir).ok()?;
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().eq_ignore_ascii_case(INI_NAME) {
            return Some(e.path());
        }
    }
    None
}

/// 纯函数：按规则改写整份字节（便于单测）。原注释 / Shift-JIS / CRLF / 对齐空格全部保留。
/// 逐行扫描，行分隔符（`\n`，含 CRLF 的 `\r`）按原样还原，绝不丢行。
pub fn patch_bytes(src: &[u8], idas: u8, host: &str) -> Vec<u8> {
    let host = host_or_default(host);
    let url = format!("http://{host}:{NEWS_PORT}/idas{idas}/news/news50.txt");
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // 定位下一个换行，按原样还原分隔符（精确保留行数与 CRLF）
        let nl = src[i..].iter().position(|b| *b == b'\n').map(|p| i + p);
        let line_end = nl.unwrap_or(src.len());
        let line = &src[i..line_end];
        out.extend_from_slice(&patch_line(line, host, &url));
        match nl {
            Some(n) => {
                out.push(b'\n');
                i = n + 1;
            }
            None => i = src.len(),
        }
    }
    out
}

/// 处理单行（不含 `\n`）：返回改写后的整行字节（含可能的尾部 `\r`）。
/// 注释 / 节头 / 非键值行原样返回 `line`。
fn patch_line(line: &[u8], host: &str, url: &str) -> Vec<u8> {
    let cr = line.last() == Some(&b'\r');
    let content: &[u8] = if cr { &line[..line.len() - 1] } else { line };

    // 空行 / 注释（; 或 #）/ 节头（[）直接透传
    let trimmed = trim_ascii(content);
    if trimmed.is_empty()
        || trimmed.first() == Some(&b';')
        || trimmed.first() == Some(&b'#')
        || trimmed.first() == Some(&b'[')
    {
        return line.to_vec();
    }

    // 找到第一个 '=' 才视为键值行
    let Some(eq) = content.iter().position(|b| *b == b'=') else {
        return line.to_vec();
    };
    let key = trim_ascii(&content[..eq]);
    let key_str = match std::str::from_utf8(key) {
        Ok(s) => s,
        Err(_) => return line.to_vec(),
    };
    match new_value(key_str, host, url) {
        Some(v) => {
            // 保留 '=' 之前的原始字节（含对齐空格），只替换值部分
            let mut o = Vec::with_capacity(content.len());
            o.extend_from_slice(&content[..eq]);
            o.extend_from_slice(b"= ");
            o.extend_from_slice(v.as_bytes());
            if cr {
                o.push(b'\r');
            }
            o
        }
        None => line.to_vec(),
    }
}

/// 按规则表返回键对应的新值（ASCII）；不匹配返回 None（原样保留）。
/// `host` 为私服地址（网络设置「服务器地址」，已保证非空）。
/// 优先级：TAG_MATCH 的 UDP 必须先于通用 MATCH 判定（前者包含后者子串）。
fn new_value(key: &str, host: &str, url: &str) -> Option<String> {
    if key.starts_with("TAG_MATCH_SERVER_PORT_UDP_SEND") {
        Some("12353".to_string())
    } else if key.starts_with("TAG_MATCH_SERVER_PORT_UDP_RECV") {
        Some("12354".to_string())
    } else if key.contains("SERVER_ADDR") {
        // 所有 *_SERVER_ADDR*（MATCH / USERDB / EVENT / SCREENSHOT / CARDB 等）
        Some(host.to_string())
    } else if key.starts_with("MATCH_SERVER_PORT_TCP") {
        Some("10003".to_string())
    } else if key.starts_with("MATCH_SERVER_PORT_UDP_SEND") {
        Some("12353".to_string())
    } else if key.starts_with("MATCH_SERVER_PORT_UDP_RECV") {
        Some("12354".to_string())
    } else if key.starts_with("USERDB_SERVER_PORT_TCP") {
        Some("10000".to_string())
    } else if key.starts_with("EVENT_SERVER_PORT_TCP") {
        Some("10000".to_string())
    } else if key.starts_with("ADV_NEWS_SERVER_URL") {
        Some(url.to_string())
    } else {
        None
    }
}

/// 仅裁剪 ASCII 空白（空格 / 制表符 / \r 等），不触碰其它字节。
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用服务器地址（与默认私服同值，便于复用既有断言）
    const H: &str = "sdgt.arcadehub.xyz";

    /// 覆盖：注释行、重复键、对齐空格、各规则、TAG_ 前缀、未列出键、CARDB
    fn sample() -> Vec<u8> {
        ";; 注释\n\
         ;MATCH_SERVER_ADDR_0 = old\n\
         MATCH_SERVER_ADDR_0     = 1.2.3.4\n\
         MATCH_SERVER_ADDR_3     = linux.am3\n\
         USERDB_SERVER_ADDR_2    = id5001.am3.sega.co.jp\n\
         EVENT_SERVER_ADDR_1     = 203.179.83.139\n\
         SCREENSHOT_SERVER_ADDR_2 = a\n\
         SCREENSHOT_SERVER_ADDR_2 = b\n\
         MATCH_SERVER_PORT_TCP_0   = 7766\n\
         MATCH_SERVER_PORT_TCP_15  = 7766\n\
         MATCH_SERVER_PORT_UDP_SEND_8 = 12354\n\
         MATCH_SERVER_PORT_UDP_RECV_8 = 12353\n\
         USERDB_SERVER_PORT_TCP_5  = 22876\n\
         EVENT_SERVER_PORT_TCP_5   = 24029\n\
         ADV_NEWS_SERVER_URL_0 = http://id5-patch01/news/news50.txt\n\
         TAG_MATCH_SERVER_PORT_TCP_8 = 27666\n\
         TAG_MATCH_SERVER_PORT_UDP_SEND_8 = 23353\n\
         TAG_MATCH_SERVER_PORT_UDP_RECV_8 = 23354\n\
         STORE_LIVECAST_ENABLE = 1\n\
         CARDB_SERVER_ADDR = 157.109.70.97\n"
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn applies_all_rules() {
        let out = patch_bytes(&sample(), 5, H);
        let s = String::from_utf8(out).unwrap();
        // SERVER_ADDR 全部（含 MATCH/USERDB/EVENT/SCREENSHOT/CARDB）
        assert!(s.contains("MATCH_SERVER_ADDR_0     = sdgt.arcadehub.xyz"));
        assert!(s.contains("MATCH_SERVER_ADDR_3     = sdgt.arcadehub.xyz"));
        assert!(s.contains("USERDB_SERVER_ADDR_2    = sdgt.arcadehub.xyz"));
        assert!(s.contains("EVENT_SERVER_ADDR_1     = sdgt.arcadehub.xyz"));
        assert!(s.contains("SCREENSHOT_SERVER_ADDR_2 = sdgt.arcadehub.xyz")); // 重复键都改
        // 端口
        assert!(s.contains("MATCH_SERVER_PORT_TCP_0   = 10003"));
        assert!(s.contains("MATCH_SERVER_PORT_TCP_15  = 10003"));
        assert!(s.contains("MATCH_SERVER_PORT_UDP_SEND_8 = 12353"));
        assert!(s.contains("MATCH_SERVER_PORT_UDP_RECV_8 = 12354"));
        assert!(s.contains("USERDB_SERVER_PORT_TCP_5  = 10000"));
        assert!(s.contains("EVENT_SERVER_PORT_TCP_5   = 10000"));
        // URL（idas5）
        assert!(s.contains("ADV_NEWS_SERVER_URL_0 = http://sdgt.arcadehub.xyz:8086/idas5/news/news50.txt"));
    }

    #[test]
    fn tag_udp_changed_tcp_unchanged() {
        let out = patch_bytes(&sample(), 5, H);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("TAG_MATCH_SERVER_PORT_UDP_SEND_8 = 12353"));
        assert!(s.contains("TAG_MATCH_SERVER_PORT_UDP_RECV_8 = 12354"));
        // 未列出的 TAG_MATCH_SERVER_PORT_TCP 保持原样
        assert!(s.contains("TAG_MATCH_SERVER_PORT_TCP_8 = 27666"));
    }

    #[test]
    fn keeps_comments_alignment_and_other_keys() {
        let out = patch_bytes(&sample(), 5, H);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(";; 注释"));
        assert!(s.contains(";MATCH_SERVER_ADDR_0 = old")); // 注释行原样
        assert!(s.contains("STORE_LIVECAST_ENABLE = 1")); // 未列出键不动
        // CARDB_SERVER_ADDR 含 SERVER_ADDR → 也被改写
        assert!(s.contains("CARDB_SERVER_ADDR = sdgt.arcadehub.xyz"));
    }

    #[test]
    fn idas_version_mapping_and_supports() {
        assert_eq!(idas_version("SBML"), Some(4));
        assert_eq!(idas_version("SBQZ"), Some(5));
        assert_eq!(idas_version("SBTS"), Some(5));
        assert_eq!(idas_version("SBWV"), Some(6));
        assert_eq!(idas_version("SBYY"), Some(7));
        assert_eq!(idas_version("SBZZ"), None); // D8
        assert_eq!(idas_version("SDAL"), None);
        assert!(supports(Some("SBQZ")));
        assert!(!supports(Some("SBZZ")));
        assert!(!supports(None));
        assert!(!supports(Some("UNKNOWN")));
    }

    #[test]
    fn idempotent() {
        let once = patch_bytes(&sample(), 5, H);
        let twice = patch_bytes(&once, 5, H);
        assert_eq!(once, twice);
    }

    #[test]
    fn crlf_preserved() {
        let src = b"MATCH_SERVER_ADDR_0 = x\r\nMATCH_SERVER_PORT_TCP_0 = 1\r\n";
        let out = patch_bytes(src, 5, H);
        assert_eq!(
            out,
            b"MATCH_SERVER_ADDR_0 = sdgt.arcadehub.xyz\r\nMATCH_SERVER_PORT_TCP_0 = 10003\r\n"
        );
    }

    #[test]
    fn patch_game_ini_missing_file_or_unsupported() {
        let tmp =
            std::env::temp_dir().join(format!("initiald_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // 没有文件：Ok(false)
        assert_eq!(patch_game_ini(&tmp, Some("SBQZ"), H).unwrap(), false);
        // 有文件但 D8：不改写（Ok(false)），内容保持原样
        let p = tmp.join("initialD.ini");
        fs::write(&p, "MATCH_SERVER_ADDR_0 = x\n").unwrap();
        assert_eq!(patch_game_ini(&tmp, Some("SBZZ"), H).unwrap(), false);
        assert_eq!(fs::read_to_string(&p).unwrap(), "MATCH_SERVER_ADDR_0 = x\n");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn patch_game_ini_supported_writes() {
        let tmp =
            std::env::temp_dir().join(format!("initiald_test2_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("initialD.ini");
        fs::write(&p, "MATCH_SERVER_ADDR_0 = x\n").unwrap();
        assert_eq!(patch_game_ini(&tmp, Some("SBQZ"), H).unwrap(), true);
        assert!(fs::read_to_string(&p).unwrap().contains(H));
        let _ = fs::remove_dir_all(&tmp);
    }

    /// 自定义服务器地址（如局域网 192.168.100.114）：地址与公告 URL 都用它
    #[test]
    fn custom_server_host_used() {
        let out = patch_bytes(&sample(), 6, "192.168.100.114");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("MATCH_SERVER_ADDR_0     = 192.168.100.114"));
        assert!(s.contains("USERDB_SERVER_ADDR_2    = 192.168.100.114"));
        assert!(s.contains("CARDB_SERVER_ADDR = 192.168.100.114"));
        assert!(s.contains(
            "ADV_NEWS_SERVER_URL_0 = http://192.168.100.114:8086/idas6/news/news50.txt"
        ));
        assert!(!s.contains(H), "不应残留默认私服地址");
    }

    /// 空白服务器地址 → 回退默认 SERVER_HOST
    #[test]
    fn blank_host_falls_back_to_default() {
        let out = patch_bytes(&sample(), 5, "  ");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(&format!("MATCH_SERVER_ADDR_0     = {}", crate::server::SERVER_HOST)));
        assert!(s.contains(&format!("http://{}:8086/idas5", crate::server::SERVER_HOST)));
    }
}
