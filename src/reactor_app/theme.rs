//! 系统主题检测：决定自绘悬停高亮用黑还是白（跟随 AppsUseLightTheme）。
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

/// 系统是否使用浅色应用主题（跟随窗口 WindowTheme::System 的 AppsUseLightTheme）。
/// 决定自绘悬停高亮用哪种系统色：#19000000（浅色）/ #19FFFFFF（深色），
/// 这正是原生列表悬停刷子 SystemControlHighlightListLowBrush 的主题取值。
pub(super) fn system_uses_light_theme() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // 读取失败（键不存在等）按浅色兜底，与 Windows 默认一致
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status == ERROR_SUCCESS && value != 0
}
