use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct CardEntry {
    pub path: PathBuf,

    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct CardScan {
    pub cards: Vec<CardEntry>,

    pub active: Option<String>,
}

impl CardScan {
    pub fn active_index(&self) -> Option<usize> {
        let active = self.active.as_deref()?;
        self.cards
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(active))
    }
}

fn has_inid_chunk(path: &Path) -> bool {
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut sig = [0u8; 8];
    if f.read_exact(&mut sig).is_err() || sig != [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    {
        return false;
    }
    loop {
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            return false;
        }
        let len = u32::from_be_bytes(hdr[..4].try_into().unwrap()) as u64;
        let typ = &hdr[4..8];
        if typ == b"INID" {
            return true;
        }
        if typ == b"IEND" || len > 100_000_000 {
            return false;
        }
        if f.seek(SeekFrom::Current(len as i64 + 4)).is_err() {
            return false;
        }
    }
}

pub fn scan_cards(game_file: &Path) -> CardScan {
    let root = game_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut scan = CardScan::default();
    let mut queue = vec![(root, 0usize)];
    let mut visited = 0usize;

    while let Some((dir, depth)) = queue.pop() {
        visited += 1;
        if visited > 512 {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let p = e.path();
            if ft.is_dir() {
                if depth < 3 {
                    queue.push((p, depth + 1));
                }
            } else {
                let fname = e.file_name().to_string_lossy().to_lowercase();
                if fname.ends_with(".png") && has_inid_chunk(&p) {
                    let name = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    scan.cards.push(CardEntry { path: p, name });
                }
            }
        }
    }

    scan.cards.sort_by(|a, b| a.name.cmp(&b.name));
    scan.cards.dedup_by(|a, b| a.name == b.name);
    scan
}

pub fn format_access_code(code: &str) -> String {
    let digits: Vec<char> = code.chars().collect();
    digits
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

fn gen_access_code() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut st =
        nanos ^ SEQ.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        st
    };
    let mut code = String::with_capacity(20);
    code.push('5');
    for _ in 1..20 {
        code.push(char::from(b'0' + (next() % 10) as u8));
    }
    code
}

pub fn blank_card_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut st =
        nanos ^ SEQ.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let n = {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        st % 100_000
    };
    format!("InidCrd{n:05}.png")
}

pub fn create_aime_card_in(device_dir: &Path) -> std::io::Result<String> {
    fs::create_dir_all(device_dir)?;
    for _ in 0..64 {
        let code = gen_access_code();
        let path = device_dir.join(format!("{code}.txt"));
        if path.exists() {
            continue;
        }
        fs::write(&path, format!("{code}\n"))?;
        return Ok(code);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "无法生成不重复的 access code",
    ))
}

pub fn scan_aime_cards_in(device_dir: &Path) -> CardScan {
    let mut paths: Vec<PathBuf> = fs::read_dir(device_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt")))
        .collect();
    paths.sort();

    let mut cards: Vec<CardEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };

        let code = raw.trim_start_matches('\u{feff}').trim();
        if code.len() != 20 || !code.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let canonical = device_dir.join(format!("{code}.txt"));
        let final_path = if stem == code {
            path
        } else {

            let _ = fs::rename(&path, &canonical);
            canonical
        };
        if seen.insert(code.to_string()) {
            cards.push(CardEntry {
                name: code.to_string(),
                path: final_path,
            });
        }
    }

    cards.sort_by_cached_key(|c| {
        std::cmp::Reverse(fs::metadata(&c.path).and_then(|m| m.modified()).ok())
    });
    CardScan {
        cards,
        active: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(data);
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    fn make_png(with_inid: bool) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        v.extend(chunk(b"IHDR", &[0; 13]));
        if with_inid {
            v.extend(chunk(b"INID", &[0xAB; 2048]));
        }
        v.extend(chunk(b"IDAT", &[0x01, 0x02]));
        v.extend(chunk(b"IEND", &[]));
        v
    }

    #[test]
    fn inid_chunk_detection() {
        let dir = std::env::temp_dir().join("dnet_cards_test");
        fs::create_dir_all(&dir).unwrap();
        let with = dir.join("with.png");
        let without = dir.join("without.png");
        fs::write(&with, make_png(true)).unwrap();
        fs::write(&without, make_png(false)).unwrap();
        fs::write(dir.join("not_png.png"), b"hello world").unwrap();

        assert!(has_inid_chunk(&with));
        assert!(!has_inid_chunk(&without));
        assert!(!has_inid_chunk(&dir.join("not_png.png")));
        assert!(!has_inid_chunk(&dir.join("missing.png")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn blank_card_name_format() {
        let a = blank_card_name();
        let b = blank_card_name();
        for name in [&a, &b] {
            assert!(name.starts_with("InidCrd"), "{name}");
            assert!(name.ends_with(".png"), "{name}");
            let digits = &name[7..name.len() - 4];
            assert_eq!(digits.len(), 5, "{name}");
            assert!(digits.chars().all(|c| c.is_ascii_digit()), "{name}");
        }
    }

    #[test]
    fn access_code_formatting() {
        assert_eq!(
            format_access_code("12345678901234567890"),
            "1234 5678 9012 3456 7890"
        );
        assert_eq!(format_access_code("123"), "123");
        assert_eq!(format_access_code(""), "");
    }

    #[test]
    fn aime_card_create_and_scan_newest_first() {
        let dir = std::env::temp_dir().join(format!("dnet_aime_{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let a = create_aime_card_in(&dir).expect("新建卡 a 应成功");
        std::thread::sleep(std::time::Duration::from_millis(30));
        let b = create_aime_card_in(&dir).expect("新建卡 b 应成功");
        assert_ne!(a, b, "重号应自动重试");
        for code in [&a, &b] {
            assert_eq!(code.len(), 20);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
            assert!(code.starts_with('5'), "aime 惯例首位为 5");
        }

        let scan = scan_aime_cards_in(&dir);
        assert_eq!(scan.cards.len(), 2);
        assert_eq!(scan.cards[0].name, b, "新建卡显示在最前面");
        assert_eq!(scan.cards[1].name, a);

        assert_eq!(
            fs::read_to_string(dir.join(format!("{b}.txt"))).unwrap(),
            format!("{b}\n")
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_accepts_any_named_txt_and_normalizes() {
        let dir = std::env::temp_dir().join(format!("dnet_aime_scan_{}", std::process::id()));
        fs::create_dir_all(&dir).ok();

        let code = "53314980123456789012";
        fs::write(dir.join("random_name.txt"), format!("{code}\n")).unwrap();

        fs::write(dir.join("bom.txt"), "\u{feff} 51112222333344445555 \r\n").unwrap();

        fs::write(dir.join("note.txt"), "hello world").unwrap();
        fs::write(dir.join("short.txt"), "12345").unwrap();

        let scan = scan_aime_cards_in(&dir);
        assert_eq!(scan.cards.len(), 2, "非 20 位数字的 txt 应被跳过");
        assert_eq!(scan.cards[1].name, code);
        assert!(dir.join(format!("{code}.txt")).is_file(), "应改名为 <code>.txt");
        assert!(!dir.join("random_name.txt").exists(), "旧文件名应已不存在");
        assert!(dir.join("note.txt").is_file(), "不合规文件原样保留");
        assert!(!scan.cards.iter().any(|c| c.name == "note"));
        assert_eq!(scan.cards[0].name, "51112222333344445555");

        fs::write(dir.join("dup.txt"), code).unwrap();
        let scan = scan_aime_cards_in(&dir);
        assert_eq!(scan.cards.len(), 2);
        fs::remove_dir_all(&dir).ok();
    }
}
