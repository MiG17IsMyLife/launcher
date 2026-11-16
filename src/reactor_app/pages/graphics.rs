use windows_reactor::*;

use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle};

pub(in crate::reactor_app) const GFX_RESOLUTIONS: &[&str] = &["1360x768", "1920x1080", "2560x1440", "3840x2160"];

pub(in crate::reactor_app) const GFX_MSAA_LEVELS: &[&str] = &["2x", "4x", "8x", "16x"];

pub(in crate::reactor_app) const GFX_RENDER_SCALE_LEVELS: &[&str] = &["2x", "4x", "8x"];

pub(in crate::reactor_app) const GFX_AF_LEVELS: &[&str] = &["2x", "4x", "8x", "16x"];

impl Launcher {

    pub(in crate::reactor_app) fn save_graphics(&mut self) {
        if let Err(e) = self.store.save_graphics(&self.graphics) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    pub(in crate::reactor_app) fn graphics_page(&self, context: &mut ViewContext<Self>) -> View {
        let gfx = &self.graphics;

        let locked = gfx.default != Some(false);

        let resolution_items: Vec<String> = GFX_RESOLUTIONS.iter().map(|v| v.to_string()).collect();
        let resolution_idx = GFX_RESOLUTIONS
            .iter()
            .position(|v| Some(*v) == gfx.resolution.as_deref());

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
                          on: bool,
                          enabled: bool,
                          msg: fn(bool) -> Message|
         -> View {
            settings_row(
                SymbolIcon::new().symbol(icon).into(),
                title,
                settings_toggle(on, enabled, context.callback(msg)),
            )
        };
        let combo_row = |icon: Symbol,
                         title: &str,
                         items: Vec<String>,
                         idx: Option<usize>,
                         placeholder: &str,
                         enabled: bool,
                         msg: fn(Option<usize>) -> Message|
         -> View {
            settings_row(
                SymbolIcon::new().symbol(icon).into(),
                title,
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
                    gfx.borderless == Some(true),
                    !locked,
                    Message::GraphicsBorderlessToggled,
                ),
            ),
            KeyedView::new("gfx-sep1", separator()),
            KeyedView::new(
                "gfx-resolution",
                combo_row(
                    Symbol::View,
                    t!("分辨率", "Resolution"),
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
                    gfx.render_scale == Some(true),
                    !locked,
                    Message::GraphicsRenderScaleToggled,
                ),
            ),
        ];

        if gfx.render_scale == Some(true) {
            rows.push(KeyedView::new("gfx-sep-rs", separator()));
            rows.push(KeyedView::new(
                "gfx-render-scale-level",
                combo_row(
                    Symbol::List,
                    t!("渲染倍率", "Render scale level"),
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
                gfx.msaa == Some(true),
                !locked,
                Message::GraphicsMsaaToggled,
            ),
        ));

        if gfx.msaa == Some(true) {
            rows.push(KeyedView::new("gfx-sep5", separator()));
            rows.push(KeyedView::new(
                "gfx-msaa-level",
                combo_row(
                    Symbol::List,
                    t!("MSAA 倍数", "MSAA level"),
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
                gfx.af == Some(true),
                !locked,
                Message::GraphicsAfToggled,
            ),
        ));

        if gfx.af == Some(true) {
            rows.push(KeyedView::new("gfx-sep7", separator()));
            rows.push(KeyedView::new(
                "gfx-af-level",
                combo_row(
                    Symbol::List,
                    t!("过滤倍数", "Filter level"),
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
                        StackPanel::new().children([TextBlock::new()
                            .text(t!("图形设置", "Graphics"))
                            .font_size(26.0)
                            .font_weight(FontWeight::SEMI_BOLD)]),
                        card,
                    )),
            )
            .into()
    }

}
