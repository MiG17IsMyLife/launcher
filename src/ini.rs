//! 通用 INI 单键读写：保留文件其余内容与注释，仅替换/插入/追加指定的 section/key。
//! 用于启动器自身的 config.ini（网络设置/系统设置/卡片选择）。

use std::fs;
use std::io;
use std::path::Path;

/// 读取 `[section] key` 的原始值（trim 后；不存在返回 None）。
pub fn read_value(ini: &Path, section: &str, key: &str) -> Option<String> {
    let text = fs::read_to_string(ini).ok()?;
    let header = format!("[{section}]");
    let mut in_sec = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_sec = t.eq_ignore_ascii_case(&header);
        } else if in_sec {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// 写入 `[section] key = value`：键已存在则原位替换，节存在则节尾插入，否则在文件末尾追加节。
/// 文件其余内容（含注释）原样保留；读取时 CRLF 归一为 LF。
pub fn set_value(ini: &Path, section: &str, key: &str, value: &str) -> io::Result<()> {
    let text = fs::read_to_string(ini).unwrap_or_default();
    let header = format!("[{section}]");
    let mut out = String::new();
    let mut in_sec = false;
    let mut replaced = false;

    let write_kv = |out: &mut String| {
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(value);
        out.push('\n');
    };

    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            if in_sec && !replaced {
                write_kv(&mut out);
                replaced = true;
            }
            in_sec = t.trim_end().eq_ignore_ascii_case(&header);
        } else if in_sec && !replaced {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    write_kv(&mut out);
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_sec && !replaced {
        write_kv(&mut out);
        replaced = true;
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&header);
        out.push('\n');
        write_kv(&mut out);
    }
    fs::write(ini, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ini(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("dnet_ini_test");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn read_requires_matching_section() {
        let ini = temp_ini("read_section.ini");
        fs::write(&ini, "[a]\nkey = 1\n[b]\nkey = 2\n[c]\nother = 3\n").unwrap();
        assert_eq!(read_value(&ini, "a", "key").as_deref(), Some("1"));
        assert_eq!(read_value(&ini, "b", "key").as_deref(), Some("2"));
        assert_eq!(read_value(&ini, "c", "key"), None);
        assert_eq!(read_value(&ini, "missing", "key"), None);
        // 不区分节名/键名大小写
        assert_eq!(read_value(&ini, "A", "KEY").as_deref(), Some("1"));
        fs::remove_file(&ini).ok();
    }

    #[test]
    fn set_value_across_sections_preserves_content() {
        let ini = temp_ini("set_multi.ini");
        fs::write(
            &ini,
            "; comment\n[net]\nip = 1.2.3.4\nextra = keep\n[other]\nx = 1\n",
        )
        .unwrap();
        set_value(&ini, "net", "ip", "5.6.7.8").unwrap();
        let out = fs::read_to_string(&ini).unwrap();
        assert!(out.starts_with("; comment\n[net]\n"));
        assert!(out.contains("ip = 5.6.7.8\n"));
        assert!(out.contains("extra = keep"));
        assert!(out.contains("[other]\nx = 1"));
        assert_eq!(read_value(&ini, "net", "ip").as_deref(), Some("5.6.7.8"));
        // other 节的键不受影响
        set_value(&ini, "other", "x", "2").unwrap();
        let out = fs::read_to_string(&ini).unwrap();
        assert!(out.contains("x = 2"));
        assert!(out.contains("ip = 5.6.7.8"));
        fs::remove_file(&ini).ok();
    }

    #[test]
    fn set_value_appends_missing_section() {
        let ini = temp_ini("set_append.ini");
        fs::write(&ini, "[a]\nx = 1").unwrap();
        set_value(&ini, "net", "adapter", "abc").unwrap();
        set_value(&ini, "net", "ip", "1.1.1.1").unwrap();
        let out = fs::read_to_string(&ini).unwrap();
        let a_pos = out.find("[a]").unwrap();
        let net_pos = out.find("[net]").unwrap();
        let adapter_pos = out.find("adapter").unwrap();
        let ip_pos = out.find("ip").unwrap();
        assert!(a_pos < net_pos && adapter_pos < ip_pos);
        assert!(out.contains("adapter = abc\n"));
        assert!(out.contains("ip = 1.1.1.1\n"));
        assert!(out.contains("x = 1"));
        fs::remove_file(&ini).ok();
    }
}
