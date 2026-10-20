//! 游戏启动 / 运行中的模态覆盖层（应用内「新窗口」）：
//! - 点击「启动游戏」即弹出：全屏暗色半透明遮罩锁定背景（带 background 的 Grid 拦截
//!   命中测试，下层完全不可操作），居中长方形面板 —— 宽 = 窗口宽、高 = 窗口高 1/3
//!   （随 WindowResized 实时更新），半透明磨砂底色视觉近似 Acrylic。
//! - 面板内容：游戏封面（自定义封面 → 识别库内嵌封面 → 名称占位）+ 游戏名；
//!   部署阶段显示进度环与「正在启动…」，进程拉起后（GameLaunched）显示
//!   「游戏运行中」与「关闭游戏」按钮（复用 LaunchStop 回收全部进程）。
//! - 启动失败 / 游戏退出 / 手动关闭：`launch_overlay` 置 None，覆盖层消失。
use std::path::Path;
use std::sync::Arc;

use windows_reactor::*;

use crate::t;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::Message;

/// 覆盖层面板内封面尺寸（DIP；行内缩略图 104×68 的放大版，保持视觉层级）
const PANEL_COVER_W: f64 = 168.0;
const PANEL_COVER_H: f64 = 112.0;
/// 窗口尺寸未知（首帧 WindowResized 尚未回灌）时的回退客户区，与 window_visuals 一致
const FALLBACK_WIN_W: f64 = 1180.0;
const FALLBACK_WIN_H: f64 = 720.0;

/// 面板内封面占位块（无任何封面来源时）：圆角底 + 游戏名前几个字
fn panel_cover_placeholder(name: &str) -> View {
    let ch: String = name.chars().take(4).collect();
    Border::new()
        .width(PANEL_COVER_W)
        .height(PANEL_COVER_H)
        .corner_radius(8.0)
        .background(ThemeBrush::CardStroke)
        .content(
            TextBlock::new()
                .text(ch)
                .font_size(24.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .opacity(0.5)
                .text_wrapping(TextWrapping::Wrap)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center),
        )
        .into()
}

impl Launcher {

    /// 模态覆盖层视图：`launch_overlay` 为 None 时返回空 Grid（无背景，不拦截任何点击）。
    pub(in crate::reactor_app) fn launch_overlay_view(
        &self,
        context: &mut ViewContext<Self>,
    ) -> View {
        let Some(index) = self.launch_overlay else {
            return Grid::new().into();
        };
        let Some(game) = self.library.games.get(index) else {
            return Grid::new().into();
        };

        // 阶段：后台部署中（game_running=false）→ 进度环；进程已拉起 → 运行中 + 关闭按钮
        let running = self.game_running
            && self.game_runtime.as_ref().map(|r| r.index) == Some(index);

        // 面板尺寸：宽 = 窗口客户区宽，高 = 窗口客户区高 1/3
        let win_w = if self.win_size.width > 0.0 { self.win_size.width } else { FALLBACK_WIN_W };
        let win_h = if self.win_size.height > 0.0 { self.win_size.height } else { FALLBACK_WIN_H };
        let panel_w = win_w;
        let panel_h = (win_h / 3.0).max(200.0);

        // 封面：优先旧档用户自定义封面（data/images/），其次识别库内嵌封面字节，
        // 最后名称占位块（与游戏行渲染同源）
        let cover: View = {
            let mut rendered: Option<View> = None;
            if let Some(path) = &game.image {
                if Path::new(path).is_file() {
                    if let Ok(img) = Image::new().source_file(path) {
                        rendered = Some(
                            img.width(PANEL_COVER_W)
                                .stretch(Stretch::Uniform)
                                .into(),
                        );
                    }
                }
            }
            if rendered.is_none() {
                // 悬浮窗优先用高清 big 版封面（若存在），否则回退常规内嵌封面
                let big_key = game
                    .cover
                    .as_deref()
                    .map(|c| format!("{}_big.png", c.trim_end_matches(".png")));
                let bytes = big_key
                    .and_then(|k| Self::builtin_cover_bytes(&k))
                    .or_else(|| game.cover.as_deref().and_then(Self::builtin_cover_bytes));
                if let Some(bytes) = bytes {
                    rendered = Some(
                        Image::new()
                            .source_data(EncodedImage::new(Arc::<[u8]>::from(bytes)))
                            .width(PANEL_COVER_W)
                            .stretch(Stretch::Uniform)
                            .into(),
                    );
                }
            }
            rendered.unwrap_or_else(|| panel_cover_placeholder(&game.name))
        };

        // 状态行：部署中 → 进度环 + 「正在启动…」；运行中 → 状态文字
        let status_row: View = if running {
            TextBlock::new()
                .text(t!("游戏运行中…", "Game running…"))
                .font_size(14.0)
                .opacity(0.65)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into()
        } else {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .children((
                    ProgressRing::new()
                        .is_indeterminate(true)
                        .is_active(true)
                        .width(18.0)
                        .height(18.0),
                    TextBlock::new()
                        .text(t!("正在启动游戏…", "Starting game…"))
                        .font_size(14.0)
                        .opacity(0.65)
                        .vertical_alignment(VerticalAlignment::Center),
                ))
                .into()
        };

        // 封面 + 游戏名（平台徽章做副标题）
        let platform_label = game
            .platform
            .clone()
            .unwrap_or_else(|| game.loader.platform().to_string());
        let title_row = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(20.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .children((
                Border::new()
                    .corner_radius(8.0)
                    .content(cover),
                StackPanel::new()
                    .spacing(6.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        TextBlock::new()
                            .text(game.name.clone())
                            .font_size(22.0)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_wrapping(TextWrapping::Wrap)
                            .max_lines(2),
                        TextBlock::new()
                            .text(platform_label)
                            .font_size(13.0)
                            .opacity(0.55),
                    )),
            ));

        // 「关闭游戏」按钮：终止游戏 + 读卡器 / amdaemon / ServerBox（复用 LaunchStop）
        // （对齐等布局属性须在 .content() 消费为 View 之前设置）
        let close_button = Button::new()
            .style(ButtonStyle::Accent)
            .min_width(160.0)
            .min_height(36.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .on_click(context.message(Message::LaunchStop(index)))
            .content(t!("关闭游戏", "Close game"));

        // 面板：半透明底色叠在暗色遮罩上视觉近似磨砂（主题感知），细描边 + 圆角
        let panel_fill = if self.light_theme {
            Color::argb(0xE8, 0xF3, 0xF3, 0xF3)
        } else {
            Color::argb(0xE8, 0x2B, 0x2B, 0x2B)
        };

        // 第三行：运行中 → 关闭按钮；部署中 → 空占位（保持面板结构稳定）
        let action_row: View = if running {
            close_button
        } else {
            Grid::new().into()
        };

        // 面板内容：封面+游戏名 → 状态行 → 关闭按钮，整体在面板内垂直居中
        let panel_body = StackPanel::new()
            .spacing(22.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((title_row, status_row, action_row));

        let panel = Border::new()
            .width(panel_w)
            .height(panel_h)
            .background(panel_fill)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::uniform(1.0))
            .content(panel_body);

        // 全屏遮罩：带 background 的 Grid 拦截命中测试 → 背景完全不可操作
        Grid::new()
            .background(Color::argb(0x99, 0x00, 0x00, 0x00))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .vertical_alignment(VerticalAlignment::Stretch)
            .grid_row_span(3)
            .children((panel,))
            .into()
    }
}

impl Launcher {
    /// 房间 loading 覆盖层：创建 / 加入 / 回归房间期间的转圈过渡（锁定背景）。
    /// `room_overlay` 为 None 时返回空 Grid（不拦截任何点击）。
    pub(in crate::reactor_app) fn room_overlay_view(&self) -> View {
        let Some(text) = &self.room_overlay else {
            return Grid::new().into();
        };

        let win_w = if self.win_size.width > 0.0 { self.win_size.width } else { FALLBACK_WIN_W };
        let win_h = if self.win_size.height > 0.0 { self.win_size.height } else { FALLBACK_WIN_H };
        let panel_w = win_w;
        let panel_h = (win_h / 3.0).max(200.0);

        let status_row = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(10.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .children((
                ProgressRing::new()
                    .is_indeterminate(true)
                    .is_active(true)
                    .width(18.0)
                    .height(18.0),
                TextBlock::new()
                    .text(text.clone())
                    .font_size(14.0)
                    .opacity(0.65)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        let panel_fill = if self.light_theme {
            Color::argb(0xE8, 0xF3, 0xF3, 0xF3)
        } else {
            Color::argb(0xE8, 0x2B, 0x2B, 0x2B)
        };

        let panel = Border::new()
            .width(panel_w)
            .height(panel_h)
            .background(panel_fill)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::uniform(1.0))
            .content(
                StackPanel::new()
                    .spacing(22.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((status_row,)),
            );

        Grid::new()
            .background(Color::argb(0x99, 0x00, 0x00, 0x00))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .vertical_alignment(VerticalAlignment::Stretch)
            .grid_row_span(3)
            .children((panel,))
            .into()
    }
}
