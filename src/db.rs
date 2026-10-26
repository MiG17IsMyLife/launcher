//! 游戏识别库：解析嵌入的 games_db.json（人工维护的嵌套结构）：
//!
//! ```json
//! {
//!   "version": 1,
//!   "games": [                      // 游戏产品数组
//!     { "name", "game_id", "platform", "loader", "image", "support_game": [
//!         { "crc": "0x..", "version", "sb_crc": "0x.."|null|"" }
//!     ]}
//!   ],
//!   "card_reader": {                // 可选：读卡器（picodaemon）产品
//!     "name": "picodaemon", "platform", "loader", "support_game": [ {crc, version} ]
//!   }
//! }
//! ```
//!
//! 提供按 CRC + 加载器查表命中游戏构建、按 game_id + 版本（xx.xx.xx，ICF 识别路径）
//! 查产品与构建，以及读卡器 CRC 匹配。

use std::sync::OnceLock;

use serde::Deserialize;

use crate::storage::Loader;

/// lookup 命中结果：产品信息 + 命中的构建
#[derive(Clone, Debug)]
pub struct DbMatch {
    /// 产品名，如 "InitialD Arcade Stage 8 JPN"
    pub name: String,
    /// 产品 game_id（如 "SBZZ"）；空串视为 None
    pub game_id: Option<String>,
    /// 平台（"Lindbergh" | "RingEdge"）；当前展示以 loader 推导，预留字段
    #[allow(dead_code)]
    pub platform: Option<String>,
    /// 默认封面文件名（如 "logo_D4.png"），位于 data/assets/ 下；未配置为 None
    pub image: Option<String>,
    pub loader: Loader,
    /// 命中构建版本，如 "2.02"
    pub version: String,
    /// 构建对应的 ServerBox CRC；缺失 / null / 空串均视为 None
    pub sb_crc: Option<u32>,
}

/// 解析后的识别库（games 产品 + 可选读卡器组）
struct Db {
    games: Vec<Product>,
    card_reader: Option<Product>,
}

/// 游戏产品：产品信息 + 可识别构建列表
struct Product {
    name: String,
    game_id: Option<String>,
    platform: Option<String>,
    /// 默认封面文件名（data/assets/ 下）
    image: Option<String>,
    loader: Loader,
    builds: Vec<Build>,
}

/// 可识别构建
struct Build {
    /// 游戏 exe 的 CRC；None = 未登记 CRC（ICF 识别的游戏），仅可按 version 匹配
    crc: Option<u32>,
    version: String,
    sb_crc: Option<u32>,
}

fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| {
        let raw: RawDb =
            serde_json::from_str(include_str!("../games_db.json")).expect("games_db.json 解析失败");
        raw.into_db()
    })
}

// ---------- serde 中间层（保留原文以做容错） ----------

#[derive(Deserialize)]
struct RawDb {
    #[allow(dead_code)]
    #[serde(default)]
    version: u32,
    #[serde(default)]
    games: Vec<RawProduct>,
    #[serde(default)]
    card_reader: Option<RawProduct>,
}

#[derive(Deserialize)]
struct RawProduct {
    #[serde(default)]
    name: String,
    #[serde(default)]
    game_id: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    /// 默认封面文件名；缺失 / 空串视为未配置
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default)]
    support_game: Vec<RawBuild>,
}

#[derive(Deserialize)]
struct RawBuild {
    /// 缺失 / 空串 / 非法十六进制均容错为 None（ICF 识别的游戏没有 CRC）
    #[serde(default)]
    crc: String,
    #[serde(default)]
    version: String,
    /// 缺失 / null 为 None；空串（""）也容错为 None
    #[serde(default)]
    sb_crc: Option<String>,
}

impl RawDb {
    fn into_db(self) -> Db {
        // parse 是纯函数闭包：把产品原始条目（含容错）规整为强类型产品
        let parse = |p: &RawProduct, fallback_name: &str| -> Product {
            let name = if p.name.is_empty() {
                fallback_name.to_string()
            } else {
                p.name.clone()
            };
            let loader_str = p.loader.as_deref().unwrap_or("");
            let loader = match parse_loader(loader_str) {
                Some(l) => l,
                None => panic!("games_db.json：产品「{name}」loader「{loader_str}」无效"),
            };
            let builds = p
                .support_game
                .iter()
                .map(|b| Build {
                    crc: parse_crc(&b.crc),
                    version: b.version.clone(),
                    sb_crc: b
                        .sb_crc
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .and_then(parse_crc),
                })
                .collect();
            Product {
                name,
                game_id: p.game_id.clone().filter(|v| !v.is_empty()),
                platform: p.platform.clone().filter(|v| !v.is_empty()),
                image: p.image.clone().filter(|v| !v.is_empty()),
                loader,
                builds,
            }
        };

        let games = self.games.iter().map(|p| parse(p, "未命名游戏")).collect();
        let card_reader = self.card_reader.as_ref().map(|p| parse(p, "card_reader"));
        Db { games, card_reader }
    }
}

fn parse_loader(s: &str) -> Option<Loader> {
    match s.trim().to_ascii_lowercase().as_str() {
        "kitten" => Some(Loader::Kitten),
        "mice" => Some(Loader::Mice),
        "segatools" => Some(Loader::Segatools),
        _ => None,
    }
}

/// 解析 "0x1fd6136a" 形式的十六进制 CRC；同时兼容无前缀 / 大写
fn parse_crc(s: &str) -> Option<u32> {
    let t = s.trim();
    let hex = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u32::from_str_radix(hex, 16).ok()
}

// ---------- 对外 API ----------

/// 按 CRC 和加载器查游戏库；未命中返回 None（无 CRC 的构建不参与此匹配）。
pub fn lookup(crc: u32, loader: Loader) -> Option<DbMatch> {
    db().games.iter().find_map(|g| {
        if g.loader != loader {
            return None;
        }
        g.builds.iter().find(|b| b.crc == Some(crc)).map(|b| DbMatch {
            name: g.name.clone(),
            game_id: g.game_id.clone(),
            platform: g.platform.clone(),
            image: g.image.clone(),
            loader,
            version: b.version.clone(),
            sb_crc: b.sb_crc,
        })
    })
}

/// 按 game_id + 版本查产品与构建（ICF 识别路径）：ICF 解析出的 id / version 与库内
/// `game_id`、`support_game[].version`（xx.xx.xx 格式）匹配。版本按数值比较，
/// "3.21.00" 与 "3.21.0" / "3.21" 等价；同 id 多产品时取首个。未命中返回 None。
pub fn lookup_by_game_id(game_id: &str, version: &str) -> Option<DbMatch> {
    let want = parse_version(version)?;
    db().games.iter().find_map(|g| {
        if g.game_id.as_deref() != Some(game_id) {
            return None;
        }
        g.builds
            .iter()
            .find(|b| parse_version(&b.version) == Some(want))
            .map(|b| DbMatch {
                name: g.name.clone(),
                game_id: g.game_id.clone(),
                platform: g.platform.clone(),
                image: g.image.clone(),
                loader: g.loader,
                version: b.version.clone(),
                sb_crc: b.sb_crc,
            })
    })
}

/// 解析 "xx.xx.xx" 版本串为 (major, minor, build)；允许缺省末段（"3.21" == (3,21,0)）。
/// 非 "段.段[.段]" 纯数字格式（如 "REV A"）返回 None，不参与版本匹配。
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut v = [0u32; 3];
    for (i, p) in parts.iter().enumerate() {
        v[i] = p.trim().parse().ok()?;
    }
    Some((v[0], v[1], v[2]))
}

/// 识别库全部游戏产品名（创建房间时选择"玩的什么游戏"用）。
pub fn game_names() -> Vec<String> {
    db().games.iter().map(|g| g.name.clone()).collect()
}

/// 识别库游戏条目（房间筛选/创建用：展示名 + 过滤键）。
#[derive(Clone, Debug)]
pub struct GameEntry {
    pub name: String,
    pub game_id: Option<String>,
}

/// 识别库全部游戏条目。
pub fn games() -> Vec<GameEntry> {
    db().games
        .iter()
        .map(|g| GameEntry { name: g.name.clone(), game_id: g.game_id.clone() })
        .collect()
}

/// 读卡器 CRC 匹配：命中顶层 card_reader 的某构建则返回其版本（如 "2014"）。
pub fn match_card_reader(crc: u32) -> Option<String> {
    db().card_reader
        .as_ref()
        .and_then(|p| p.builds.iter().find(|b| b.crc == Some(crc)))
        .map(|b| b.version.clone())
}

/// 游戏可识别构建总数（跨全部产品）。仅用于单测整库一致性。
#[allow(dead_code)]
pub fn build_count() -> usize {
    db().games.iter().map(|g| g.builds.len()).sum()
}

/// 识别库全部已配置的封面文件名（含重复）。仅用于单测校对 COVER_ASSETS。
#[cfg(test)]
pub fn cover_images() -> Vec<String> {
    db().games
        .iter()
        .filter_map(|g| g.image.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_mice_d8_returns_product_and_build() {
        // InitialD Arcade Stage 8 JPN 构建 2.01
        let m = lookup(0x1FD6_136A, Loader::Mice).expect("D8 JPN 2.01 应命中");
        assert_eq!(m.name, "InitialD Arcade Stage 8 JPN");
        assert_eq!(m.version, "2.01");
        assert_eq!(m.loader, Loader::Mice);
        assert_eq!(m.platform.as_deref(), Some("RingEdge"));
        assert_eq!(m.game_id.as_deref(), Some("SBZZ"));
        assert_eq!(m.sb_crc, Some(0x8667_432A));
        assert_eq!(m.image.as_deref(), Some("logo_D8.png"));
    }

    #[test]
    fn lookup_kitten_d4_returns_product_and_build() {
        // InitialD Arcade Stage 4 JPN REV A
        let m = lookup(0x361D_1CBE, Loader::Kitten).expect("D4 REV A 应命中");
        assert_eq!(m.name, "InitialD Arcade Stage 4 JPN");
        assert_eq!(m.version, "REV A");
        assert_eq!(m.loader, Loader::Kitten);
        assert_eq!(m.platform.as_deref(), Some("Lindbergh"));
        assert_eq!(m.sb_crc, Some(0x6049_6EEA));
        assert_eq!(m.image.as_deref(), Some("logo_D4.png"));
    }

    #[test]
    fn lookup_empty_sb_crc_yields_none() {
        // D5 EXP 2.0 条目 sb_crc 为空字符串 ""：应容错为 None
        let m = lookup(0xD491_0E75, Loader::Kitten).expect("D5 EXP2.0 应命中");
        assert_eq!(m.name, "InitialD Arcade Stage 5 EXP 2.0");
        assert_eq!(m.version, "REV A");
        assert_eq!(m.sb_crc, None);
    }

    #[test]
    fn lookup_miss_and_cross_loader() {
        assert!(lookup(0xDEAD_BEEF, Loader::Mice).is_none());
        // 跨加载器不误命中：kitten 的 CRC 不应命中 mice 库
        assert!(lookup(0x361D_1CBE, Loader::Mice).is_none());
        assert!(lookup(0x1FD6_136A, Loader::Kitten).is_none());
    }

    #[test]
    fn card_reader_matches() {
        assert_eq!(match_card_reader(0x037F_F93C).as_deref(), Some("2014"));
        assert_eq!(match_card_reader(0xD293_9F28).as_deref(), Some("2007"));
        // 游戏构建 CRC 不应命中读卡器
        assert_eq!(match_card_reader(0x1FD6_136A), None);
    }

    #[test]
    fn all_db_entries_parse_and_loader_match() {
        // 整库一致性：构建数不少于既有版本（JSON 现含 18 个构建）
        assert!(
            build_count() >= 16,
            "games_db 构建数异常：{}",
            build_count()
        );
    }

    #[test]
    fn segatools_loader_is_accepted() {
        assert_eq!(parse_loader("segatools"), Some(Loader::Segatools));
        // 容错：首尾空白 / 大小写不敏感
        assert_eq!(parse_loader(" SegaTools "), Some(Loader::Segatools));
    }

    #[test]
    fn parse_version_formats() {
        assert_eq!(parse_version("3.21.00"), Some((3, 21, 0)));
        assert_eq!(parse_version(" 12.03.07 "), Some((12, 3, 7)));
        // 缺省末段等价：与 ICF 解析出的 "3.21.00" 数值相等
        assert_eq!(parse_version("3.21.0"), Some((3, 21, 0)));
        assert_eq!(parse_version("3.21"), Some((3, 21, 0)));
        // 非版本格式（如旧游戏的 "REV A"）不参与 ICF 版本匹配
        assert_eq!(parse_version("REV A"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn lookup_by_game_id_hits_d8_build() {
        // D8 JPN 2.01：真库正例（版本数值匹配，末段补零等价）
        let m = lookup_by_game_id("SBZZ", "2.01").expect("SBZZ 2.01 应命中");
        assert_eq!(m.name, "InitialD Arcade Stage 8 JPN");
        assert_eq!(m.version, "2.01");
        assert_eq!(m.sb_crc, Some(0x8667_432A));
        assert_eq!(m.loader, Loader::Mice);

        let m2 = lookup_by_game_id("SBZZ", "2.01.0").expect("xx.xx 与 xx.xx.xx 应等价命中");
        assert_eq!(m2.version, "2.01");

        // 1.0 命中且能区分不同构建
        assert_eq!(lookup_by_game_id("SBZZ", "1.0").unwrap().version, "1.0");
    }

    #[test]
    fn lookup_by_game_id_misses() {
        assert!(lookup_by_game_id("SBZZ", "9.99.99").is_none(), "无此版本");
        assert!(lookup_by_game_id("ZZZZ", "1.0").is_none(), "无此产品");
        // 库内版本非 xx.xx.xx 格式（REV A）时不可按版本命中
        assert!(lookup_by_game_id("SBML", "REV A").is_none());
    }

    #[test]
    fn builds_without_crc_are_tolerated() {
        // ICF 识别的游戏手写条目：support_game 可以只有 version，无 crc 不 panic
        let raw: RawDb = serde_json::from_str(
            r#"{"games":[{"name":"InitialD The Arcade","game_id":"SDGT",
                 "platform":"ALLS","loader":"segatools",
                 "support_game":[{"version":"3.21.00"},{"crc":"0x00000000","version":"3.00.00"}]}]}"#,
        )
        .expect("无 crc 的条目应可解析");
        let parsed = raw.into_db();
        let g = &parsed.games[0];
        assert_eq!(g.loader, Loader::Segatools);
        assert_eq!(g.builds[0].crc, None, "缺失 crc 容错为 None");
        assert_eq!(g.builds[0].version, "3.21.00");
        assert_eq!(g.builds[1].crc, Some(0), "显式 0x00000000 仍有效");
    }
}
