//! 图形设置页：默认设置 / 无边框 / 全屏 / 分辨率 / 渲染倍数 / MSAA / 各向异性过滤。
use windows_reactor::*;

use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle};

/// 图形设置页：分辨率候选
pub(in crate::reactor_app) const GFX_RESOLUTIONS: &[&str] = &["1360x768", "1920x1080", "2560x1440", "3840x2160"];

/// 图形设置页：MSAA 采样倍数候选
pub(in crate::reactor_app) const GFX_MSAA_LEVELS: &[&str] = &["2x", "4x", "8x", "16x"];

/// 图形设置页：渲染倍率候选
pub(in crate::reactor_app) const GFX_RENDER_SCALE_LEVELS: &[&str] = &["2x", "4x", "8x"];

/// 图形设置页：各向异性过滤倍数候选
pub(in crate::reactor_app) const GFX_AF_LEVELS: &[&str] = &["2x", "4x", "8x", "16x"];

impl Launcher {

    /// 持久化图形设置到 config.ini（成功静默，失败弹窗提示）
    pub(in crate::reactor_app) fn save_graphics(&mut self) {
        if let Err(e) = self.store.save_graphics(&self.graphics) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    /// 图形设置页：默认设置 / 无边框 / 全屏 / 分辨率 / 渲染倍数 / MSAA / 各向异性过滤。
    /// 全部保存到启动器自己的 config.ini（[graphics] 节），由加载器读取。
    pub(in crate::reactor_app) fn graphics_page(&self, context: &mut ViewContext<Self>) -> View {
        let gfx = &self.graphics;
        // 「默认设置」开启时其余选项禁用（使用游戏默认图形配置）；未设置时默认为开启
        let locked = gfx.default != Some(false);

        // 分辨率下拉框
        let resolution_items: Vec<String> = GFX_RESOLUTIONS.iter().map(|v| v.to_string()).collect();
        let resolution_idx = GFX_RESOLUTIONS
            .iter()
            .position(|v| Some(*v) == gfx.resolution.as_deref());

        // 渲染倍率 / MSAA / 各向异性过滤倍数下拉框（对应开关打开时才显示对应行）
        let render_scale_items: Vec<String> = GFX_RENDER_SCALE_LEVELS
            .iter()
            .map(|v| v.to_string())
            .collect();
        let render_scale_idx = GFX_RENDER_SCALE_LEVELS
            .iter()
            .position(|v| Some(*v) == gfx.render_scale_level.as_deref());
        let msaa_level_items: Vec<String> = GFX_MSAA_LEVELS.iter().map(|v| v.to_string()).collect();
        let msaa_level_idx = GFX_MSAA_LEVELS
            .iter()
            .position(|v| Some(*v) == gfx.msaa_level.as_deref());
        let af_level_items: Vec<String> = GFX_AF_LEVELS.iter().map(|v| v.to_string()).collect();
        let af_level_idx = GFX_AF_LEVELS
            .iter()
            .position(|v| Some(*v) == gfx.af_level.as_deref());

        let toggle_row = |icon: Symbol,
                          title: &str,
                          desc: &str,
                          on: bool,
                          enabled: bool,
                          msg: fn(bool) -> Message|
         -> View {
            settings_row(
                SymbolIcon::new().symbol(icon).into(),
                title,
                desc,
                settings_toggle(on, enabled, context.callback(msg)),
            )
        };
        let combo_row = |icon: Symbol,
                         title: &str,
                         desc: &str,
                         items: Vec<String>,
                         idx: Option<usize>,
                         placeholder: &str,
                         enabled: bool,
                         msg: fn(Option<usize>) -> Message|
         -> View {
            settings_row(
                SymbolIcon::new().symbol(icon).into(),
                title,
                desc,
                ComboBox::new()
                    .width(180.0)
                    .items_source(items)
                    .selected_index(idx)
                    .placeholder_text(placeholder)
                    .is_enabled(enabled)
                    .on_selection_changed(context.callback(msg))
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            )
        };

        let mut rows: Vec<KeyedView> = vec![
            KeyedView::new(
                "gfx-default",
                toggle_row(
                    Symbol::Repair,
                    t!("默认设置", "Default settings"),
                    t!("开启后使用游戏默认图形配置，忽略以下选项", "Use the game's default graphics config; ignore the options below"),
                    gfx.default != Some(false),
                    true,
                    Message::GraphicsDefaultToggled,
                ),
            ),
            KeyedView::new("gfx-sep0", separator()),
            KeyedView::new(
                "gfx-borderless",
                toggle_row(
                    Symbol::BackToWindow,
                    t!("无边框模式", "Borderless"),
                    t!("以无边框窗口运行", "Run in a borderless window"),
                    gfx.borderless == Some(true),
                    !locked,
                    Message::GraphicsBorderlessToggled,
                ),
            ),
            KeyedView::new("gfx-sep1", separator()),
            KeyedView::new(
                "gfx-fullscreen",
                toggle_row(
                    Symbol::FullScreen,
                    t!("全屏模式", "Fullscreen"),
                    t!("以全屏模式运行", "Run in fullscreen mode"),
                    gfx.fullscreen == Some(true),
                    !locked,
                    Message::GraphicsFullscreenToggled,
                ),
            ),
            KeyedView::new("gfx-sep2", separator()),
            KeyedView::new(
                "gfx-resolution",
                combo_row(
                    Symbol::View,
                    t!("分辨率", "Resolution"),
                    t!("显示分辨率", "Display resolution"),
                    resolution_items,
                    resolution_idx,
                    t!("选择分辨率", "Select resolution"),
                    !locked,
                    Message::GraphicsResolutionSelected,
                ),
            ),
            KeyedView::new("gfx-sep3", separator()),
            KeyedView::new(
                "gfx-render-scale",
                toggle_row(
                    Symbol::Zoom,
                    t!("渲染倍数", "Render scale"),
                    t!("渲染分辨率缩放", "Render resolution scaling"),
                    gfx.render_scale == Some(true),
                    !locked,
                    Message::GraphicsRenderScaleToggled,
                ),
            ),
        ];
        // 渲染倍数开启时显示倍率下拉行
        if gfx.render_scale == Some(true) {
            rows.push(KeyedView::new("gfx-sep-rs", separator()));
            rows.push(KeyedView::new(
                "gfx-render-scale-level",
                combo_row(
                    Symbol::List,
                    t!("渲染倍率", "Render scale level"),
                    t!("渲染分辨率倍率", "Render resolution multiplier"),
                    render_scale_items,
                    render_scale_idx,
                    t!("选择倍率", "Select level"),
                    !locked,
                    Message::GraphicsRenderScaleLevelSelected,
                ),
            ));
        }
        rows.push(KeyedView::new("gfx-sep4", separator()));
        rows.push(KeyedView::new(
            "gfx-msaa",
            toggle_row(
                Symbol::Filter,
                t!("MSAA 抗锯齿", "MSAA antialiasing"),
                t!("多重采样抗锯齿", "Multisample antialiasing"),
                gfx.msaa == Some(true),
                !locked,
                Message::GraphicsMsaaToggled,
            ),
        ));
        // MSAA 开启时显示倍数下拉行
        if gfx.msaa == Some(true) {
            rows.push(KeyedView::new("gfx-sep5", separator()));
            rows.push(KeyedView::new(
                "gfx-msaa-level",
                combo_row(
                    Symbol::List,
                    t!("MSAA 倍数", "MSAA level"),
                    t!("采样倍数", "Sample count"),
                    msaa_level_items,
                    msaa_level_idx,
                    t!("选择倍数", "Select level"),
                    !locked,
                    Message::GraphicsMsaaLevelSelected,
                ),
            ));
        }
        rows.push(KeyedView::new("gfx-sep6", separator()));
        rows.push(KeyedView::new(
            "gfx-af",
            toggle_row(
                Symbol::Rotate,
                t!("各向异性过滤", "Anisotropic filtering"),
                t!("提升斜视角度下的纹理清晰度", "Sharper textures at oblique viewing angles"),
                gfx.af == Some(true),
                !locked,
                Message::GraphicsAfToggled,
            ),
        ));
        // 各向异性过滤开启时显示倍数下拉行
        if gfx.af == Some(true) {
            rows.push(KeyedView::new("gfx-sep7", separator()));
            rows.push(KeyedView::new(
                "gfx-af-level",
                combo_row(
                    Symbol::List,
                    t!("过滤倍数", "Filter level"),
                    t!("各向异性采样倍数", "Anisotropic sample count"),
                    af_level_items,
                    af_level_idx,
                    t!("选择倍数", "Select level"),
                    !locked,
                    Message::GraphicsAfLevelSelected,
                ),
            ));
        }

        let card = Border::new()
            .margin(Thickness::new(0.0, 16.0, 0.0, 0.0))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .keyed_children(rows),
            );

        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
                    .spacing(4.0)
                    .children((
                        StackPanel::new().spacing(4.0).children((
                                                       TextBlock::new()
                                .text(t!("图形设置", "Graphics"))
                                .font_size(26.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            TextBlock::new()
                                .text(t!("", ""))
                                .font_size(12.0)
                                .opacity(0.6),
                        )),
                        card,
                    )),
            )
            .into()
    }

}
