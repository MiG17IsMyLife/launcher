//! 原生文件选择对话框（Win32 GetOpenFileNameW，系统组件）

use std::path::PathBuf;

use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::core::PCWSTR;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 打开原生文件选择框；`pairs` 为 (过滤器名, 通配符) 序列。取消返回 None。
pub fn pick_open_file(pairs: &[(&str, &str)], title: &str) -> Option<PathBuf> {
    unsafe {
        let mut filter = Vec::new();
        for (name, pat) in pairs {
            filter.extend(to_wide(name));
            filter.extend(to_wide(pat));
        }
        filter.push(0); // 双 0 结尾

        let mut file_buf = [0u16; 1024];
        let title_w = to_wide(title);
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: windows::Win32::Foundation::HWND::default(),
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: windows::core::PWSTR(file_buf.as_mut_ptr()),
            nMaxFile: file_buf.len() as u32,
            lpstrTitle: PCWSTR(title_w.as_ptr()),
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
            ..Default::default()
        };
        if GetOpenFileNameW(&mut ofn).as_bool() {
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(0);
            Some(PathBuf::from(String::from_utf16_lossy(&file_buf[..len])))
        } else {
            None
        }
    }
}
