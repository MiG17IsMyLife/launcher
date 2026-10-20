//! 通用小部件与纯函数助手：设置行 / 分隔线 / 徽章 / 占位块 / 识别库再匹配 / id 与时间戳。
use std::time::{SystemTime, UNIX_EPOCH};

use windows_reactor::*;

use crate::db;
use crate::rawinput;
use crate::storage::{DPAD_CODE_BASE, GameRecord};
use crate::t;
use crate::tf;

use super::pages::games::{ROW_COVER_H, ROW_COVER_W};

/// 行内右侧的文本标注（平台 / ServerBox / 读卡器状态）。
/// 参考 Windows「设置」列表：纯文字、无方框、无底色。
pub(super) fn row_chip(label: &str) -> View {
    TextBlock::new()
        .text(label.to_string())
        .font_size(11.0)
        .opacity(0.65)
        .into()
}

/// 按键绑定的可读描述：按钮编号，或 1001~1004 的十字键方向
pub(super) fn binding_label(value: u32) -> String {
    use rawinput::{DPAD_DOWN, DPAD_LEFT, DPAD_RIGHT, DPAD_UP};
    match value.checked_sub(DPAD_CODE_BASE) {
        Some(DPAD_UP) => t!("十字键 上", "D-pad Up").to_string(),
        Some(DPAD_RIGHT) => t!("十字键 右", "D-pad Right").to_string(),
        Some(DPAD_DOWN) => t!("十字键 下", "D-pad Down").to_string(),
        Some(DPAD_LEFT) => t!("十字键 左", "D-pad Left").to_string(),
        _ => tf!("按钮 {}", "Button {}", value),
    }
}

/// 按记录中的 crc + loader 再次命中识别库，得到该记录当前对应的识别构建
/// （用于渲染菜单时推导 ServerBox 可用性 / 校验期望 CRC）。
pub(super) fn record_db_match(game: &GameRecord) -> Option<db::DbMatch> {
    let s = game.crc.as_deref()?;
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let crc = u32::from_str_radix(s, 16).ok()?;
    db::lookup(crc, game.loader)
}

pub(super) fn row_placeholder(name: &str) -> View {
    let ch: String = name.chars().take(3).collect();
    Border::new()
        .width(ROW_COVER_W)
        .height(ROW_COVER_H)
        .corner_radius(4.0)
        .background(ThemeBrush::CardStroke)
        .content(
            TextBlock::new()
                .text(ch)
                .font_size(18.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .opacity(0.5)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center),
        )
}

/// Win11 设置风格的分组：小标题 + 卡片（用于把设置页拆成多组）
pub(super) fn settings_section(title: &str, rows: Vec<KeyedView>) -> View {
    StackPanel::new()
        .spacing(8.0)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .margin(Thickness::new(0.0, 16.0, 0.0, 0.0))
        .children((
            TextBlock::new()
                .text(title)
                .font_size(16.0)
                .font_weight(FontWeight::SEMI_BOLD),
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .corner_radius(4.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    StackPanel::new()
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .keyed_children(rows),
                ),
        ))
        .into()
}

/// Win11 设置风格的设置行：左侧图标 + 标题，右侧控件。
/// 标题下方不再显示解释性文字（所有设置界面统一，输入页的方案推广到全部）。
/// `desc` 参数保留以维持调用签名兼容，但不再渲染。
pub(super) fn settings_row(icon: View, title: &str, desc: &str, control: View) -> View {
    // 描述为空时不渲染第二行（部分行只有标题），避免空 TextBlock 撑高
    let text: View = if desc.is_empty() {
        StackPanel::new()
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children([TextBlock::new().text(title).font_size(14.0)])
            .into()
    } else {
        StackPanel::new()
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                TextBlock::new().text(title).font_size(14.0),
                TextBlock::new()
                    .text(desc)
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap),
            ))
            .into()
    };
    Grid::new()
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(14.0)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .children((icon, text)),
            Border::new()
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .content(control),
        ))
        .into()
}

/// 可被选中的值文本：鼠标可选中 / 全选 / 复制。
/// 用于房间 ID、密码、IP 等字符串 / 数字类型的设置值。
pub(super) fn selectable_text(text: impl Into<String>) -> TextBlock {
    TextBlock::new().text(text).is_text_selection_enabled(true)
}

/// 设置行右侧的开关：文字在左、开关在右。
/// WinUI ToggleSwitch 默认模板是"滑块在左、On/Off 文字在右"且固有宽度很大，
/// 因此这里清空自带文字、用固定宽度只保留滑块（否则滑块会停在距行右缘很远的左侧），
/// 再用独立 TextBlock 在滑块左侧显示"开/关"状态。
pub(super) fn settings_toggle<C: IntoPayloadCallback<bool>>(on: bool, enabled: bool, callback: C) -> View {
    let state_text = if on { t!("开", "On") } else { t!("关", "Off") };
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            TextBlock::new()
                .text(state_text)
                .font_size(14.0)
                .vertical_alignment(VerticalAlignment::Center),
            ToggleSwitch::new()
                .is_on(on)
                .is_enabled(enabled)
                .on_toggled(callback)
                .width(56.0)
                .min_width(0.0)
                .vertical_alignment(VerticalAlignment::Center)
                .slots([
                    SlotView::new(ToggleSwitchSlot::OnContent, TextBlock::new()),
                    SlotView::new(ToggleSwitchSlot::OffContent, TextBlock::new()),
                ]),
        ))
        .into()
}

/// 卡片内两行之间的 1px 分隔线
pub(super) fn separator() -> View {
    Border::new()
        .height(1.0)
        .margin(Thickness::new(16.0, 0.0, 16.0, 0.0))
        .background(ThemeBrush::CardStroke)
        .into()
}

/// 设置占位页（图形/网络/输入）
pub(super) fn placeholder_page(title: &str, desc: &str) -> View {
    StackPanel::new()
        .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
        .spacing(16.0)
        .children((
            StackPanel::new().spacing(4.0).children((
                TextBlock::new()
                    .text(title)
                    .font_size(26.0)
                    .font_weight(FontWeight::SEMI_BOLD),
                TextBlock::new().text(desc).font_size(12.0).opacity(0.6),
            )),
            Border::new()
                .corner_radius(6.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .padding(Thickness::new(20.0, 28.0, 20.0, 28.0))
                .content(
                    TextBlock::new()
                        .text(t!("此页面正在建设中，设置项将逐步开放。", "This page is under construction; settings will be added gradually."))
                        .font_size(13.0)
                        .opacity(0.6)
                        .horizontal_alignment(HorizontalAlignment::Center),
                ),
        ))
        .into()
}

pub(super) fn same_path(a: &str, b: &std::path::Path) -> bool {
    let pa = std::path::Path::new(a);
    if let (Ok(ca), Ok(cb)) = (pa.canonicalize(), b.canonicalize()) {
        return ca == cb;
    }
    pa.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

pub(super) fn make_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
