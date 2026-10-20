//! 本地化核心：语言枚举、全局当前语言、系统区域检测，以及零依赖翻译宏 `t!`。
//!
//! 设计：所有界面文案以 `t!("中文", "English")` 成对写在调用处（中英同行、便于审阅、
//! 零第三方依赖），渲染时按全局当前语言选取。全局用无锁 `AtomicU8`，`view` 高频读取零开销。

use std::sync::atomic::{AtomicU8, Ordering};

/// 界面语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// 中文（简体）
    Zh,
    /// 英文
    En,
}

/// 0 = Zh, 1 = En。无锁原子，`t!` 在每次 `view` 重建时高频读取。
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 读取全局当前语言。
pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}

/// 按当前语言返回中英双标签之一：用于来自常量数组（无法用 `t!` 字面量宏）的下拉候选标签。
pub fn pick<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if current() == Lang::En {
        en
    } else {
        zh
    }
}

/// 设置全局当前语言（写入频率极低，仅切换时发生）。
pub fn set_lang(lang: Lang) {
    CURRENT.store(if lang == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

/// 把语言序列化为 `config.ini [ui] lang` 的存储值。
pub fn to_ini_value(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en-US",
        Lang::Zh => "zh-CN",
    }
}

/// 从 `config.ini [ui] lang` 的存储值解析语言；非 `en-US` 一律视为中文（含缺省）。
pub fn from_ini_value(value: Option<&str>) -> Lang {
    match value {
        Some("en-US") => Lang::En,
        _ => Lang::Zh,
    }
}

/// 按系统用户区域自动选择默认语言：区域名以 `en` 开头判英文，否则中文；检测失败回落中文。
pub fn detect_system() -> Lang {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH = 85（含结尾 NUL）
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len > 0 {
        let n = (len as usize).saturating_sub(1).min(buf.len());
        let name = String::from_utf16_lossy(&buf[..n]);
        if name.to_ascii_lowercase().starts_with("en") {
            return Lang::En;
        }
    }
    Lang::Zh
}

/// 翻译宏：`t!("中文", "English")` 按当前语言返回对应字面量（`&'static str`）。
///
/// 动态组合文案写作 `tf!("{} 款游戏", "{} games", n)`。
#[macro_export]
macro_rules! t {
    ($zh:literal, $en:literal $(,)?) => {
        if $crate::i18n::current() == $crate::i18n::Lang::En {
            $en
        } else {
            $zh
        }
    };
}

/// 带参数的翻译宏：`tf!("中文{}", "English{}", args...)` 等价于按语言选模板后 `format!(模板, args...)`。
/// 必须在 `format!` 分支内展开字面量，故不能写成 `format!(t!(...), ...)`（`format!` 首参要求字面量）。
#[macro_export]
macro_rules! tf {
    ($zh:literal, $en:literal) => {
        if $crate::i18n::current() == $crate::i18n::Lang::En {
            format!($en)
        } else {
            format!($zh)
        }
    };
    ($zh:literal, $en:literal, $($arg:tt)*) => {
        if $crate::i18n::current() == $crate::i18n::Lang::En {
            format!($en, $($arg)*)
        } else {
            format!($zh, $($arg)*)
        }
    };
}
