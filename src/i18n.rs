use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {

    Zh,

    En,
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}

pub fn pick<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if current() == Lang::En {
        en
    } else {
        zh
    }
}

pub fn set_lang(lang: Lang) {
    CURRENT.store(if lang == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn to_ini_value(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en-US",
        Lang::Zh => "zh-CN",
    }
}

pub fn from_ini_value(value: Option<&str>) -> Lang {
    match value {
        Some("en-US") => Lang::En,
        _ => Lang::Zh,
    }
}

pub fn detect_system() -> Lang {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;

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
