//! 系统设置页：日志级别 / Region / Keychip / 读卡器初始化 / 机位 + 投币设置卡片。
use std::time::Duration;

use windows_reactor::*;

use crate::i18n;
use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle};

/// 系统设置页：日志级别候选（值, 中文标签, 英文标签）；首项 info 为默认。
/// kitten（默认 3=Info）/ mice（默认 4=Info）都无 off 项；加载器未设置时
/// 按各自默认 info 落配置，故界面不再提供「未设置」，默认选中 info。
pub(in crate::reactor_app) const SYS_LOGLEVELS: &[(Option<&str>, &str, &str)] = &[
    (Some("info"), "info（默认）", "info (default)"),
    (Some("error"), "error", "error"),
    (Some("warn"), "warn", "warn"),
    (Some("debug"), "debug", "debug"),
    (Some("trace"), "trace", "trace"),
];

/// 系统设置页：地区候选（JPN=日本 EXP=出口 CHN=中国）
pub(in crate::reactor_app) const SYS_REGIONS: &[&str] = &["JPN", "EXP", "CHN"];

/// 系统设置页：机位候选（存储值, 中文标签, 英文标签），仅对 SDGT（IDAC）生效。
/// 单机时 dipsw3=1；A1~B2 的 dipsw4/5 为机位二进制位（见 segatools.rs）。
pub(in crate::reactor_app) const SYS_SEATS: &[(&str, &str, &str)] = &[
    ("SINGLE", "单机", "Single"),
    ("A1", "A1", "A1"),
    ("A2", "A2", "A2"),
    ("B1", "B1", "B1"),
    ("B2", "B2", "B2"),
];

impl Launcher {

    /// 持久化系统设置到 config.ini（成功静默，失败弹窗提示）
    pub(in crate::reactor_app) fn save_system(&mut self) {
        if let Err(e) = self.store.save_system(&self.system) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    /// 持久化投币设置到 config.ini（成功静默，失败弹窗提示）
    pub(in crate::reactor_app) fn save_coin(&mut self) {
        if let Err(e) = self.store.save_coin(&self.coin) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    /// 展开/收起投币设置卡片，并启动展开/收起过渡动画
    pub(in crate::reactor_app) fn toggle_coin_expander(&mut self, context: &ComponentContext<Self>) {
        self.coin_expanded = !self.coin_expanded;
        // 启动展开/收起过渡动画：每 16ms 推进一步，约 200ms 完成
        self.coin_anim_gen += 1;
        let (epoch, step) = (self.coin_anim_gen, 0.016 / 0.2);
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(16));
            Message::CoinAnimTick(epoch, step)
        });
    }

    /// 投币设置展开/收起动画推进（过期代数作废）
    pub(in crate::reactor_app) fn tick_coin_anim(&mut self, epoch: u32, step: f64, context: &ComponentContext<Self>) {
        if epoch != self.coin_anim_gen {
            return;
        }
        if self.coin_expanded {
            self.coin_anim = (self.coin_anim + step).min(1.0);
        } else {
            self.coin_anim = (self.coin_anim - step).max(0.0);
        }
        let done = if self.coin_expanded {
            self.coin_anim >= 1.0
        } else {
            self.coin_anim <= 0.0
        };
        if !done {
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(16));
                Message::CoinAnimTick(epoch, step)
            });
        }
    }

    /// 系统设置页：日志级别 / Region / Keychip / 跳过读卡器初始化。
    /// 全部保存到启动器自己的 config.ini（[system] 节），由加载器读取。
    pub(in crate::reactor_app) fn system_page(&self, context: &mut ViewContext<Self>) -> View {
        // 日志级别下拉框：未设置 / 历史遗留值（如 off）都回落到默认 info（首项）
        let loglevel_items: Vec<String> = SYS_LOGLEVELS
            .iter()
            .map(|(_, zh, en)| i18n::pick(zh, en).to_string())
            .collect();
        let loglevel_idx = SYS_LOGLEVELS
            .iter()
            .position(|(v, _, _)| *v == self.system.loglevel.as_deref())
            .or(Some(0));

        // 地区下拉框：仅 JPN/EXP/CHN 三个值，未选择时不写入
        let region_items: Vec<String> = SYS_REGIONS.iter().map(|v| v.to_string()).collect();
        let region_idx = SYS_REGIONS
            .iter()
            .position(|v| Some(*v) == self.system.region.as_deref());

        // 机位下拉框：None 视为单机（首项）；「主机」勾选框在机位左侧
        let seat_items: Vec<String> = SYS_SEATS
            .iter()
            .map(|(_, zh, en)| i18n::pick(zh, en).to_string())
            .collect();
        let seat_idx = SYS_SEATS
            .iter()
            .position(|(v, _, _)| Some(*v) == self.system.seat.as_deref())
            .or(Some(0));

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
                    .children((
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::List).into(),
                        t!("日志级别", "Log level"),
                        t!("mice 加载器日志详细程度", "mice loader log verbosity"),
                        ComboBox::new()
                            .width(180.0)
                            .items_source(loglevel_items)
                            .selected_index(loglevel_idx)
                            .on_selection_changed(context.callback(Message::LogLevelSelected))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::World).into(),
                        "Region",
                        t!("机台地区：JPN=日本、EXP=出口、CHN=中国", "Machine region: JPN=Japan, EXP=Export, CHN=China"),
                        ComboBox::new()
                            .width(180.0)
                            .items_source(region_items)
                            .selected_index(region_idx)
                            .placeholder_text(t!("选择地区", "Select region"))
                            .on_selection_changed(context.callback(Message::RegionSelected))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Admin).into(),
                        "Keychip",
                        t!("钥匙芯片序列号", "Keychip serial number"),
                        TextBox::new()
                            .width(225.0)
                            .text(self.system.keychip.clone().unwrap_or_default())
                            .placeholder_text("A69E-01A99999999")
                            .on_text_changed(context.callback(Message::KeychipChanged))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Contact).into(),
                        t!("跳过读卡器初始化", "Skip reader initialization"),
                        t!("开启后游戏将跳过读卡器初始化", "Skip the card reader initialization at startup"),
                        settings_toggle(
                            self.system.skip_reader_init == Some(true),
                            true,
                            context.callback(Message::SkipReaderInitToggled),
                        ),
                    ),
                    separator(),
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Directions).into(),
                        t!("机位设置", "Seat position"),
                        t!("单机 / A1 / A2 / B1 / B2（仅对 SDGT / IDAC 生效）；选「单机」自动勾选主机",
                           "Single / A1 / A2 / B1 / B2 (SDGT / IDAC only); picking Single auto-checks Master"),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(12.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                CheckBox::new()
                                    .is_checked(
                                        self.system.master.unwrap_or(
                                            self.system.seat.as_deref().unwrap_or("SINGLE")
                                                == "SINGLE",
                                        ),
                                    )
                                    .on_is_checked_changed(
                                        context.callback(Message::SeatMasterToggled),
                                    )
                                    .content(t!("主机", "Master")),
                                ComboBox::new()
                                    .width(140.0)
                                    .items_source(seat_items)
                                    .selected_index(seat_idx)
                                    .on_selection_changed(context.callback(Message::SeatSelected))
                                    .vertical_alignment(VerticalAlignment::Center),
                            ))
                            .into(),
                    ),
                )),
            );

        // 投币设置：卡片 + 点击标题行展开/收起全部配置项
        let coin = &self.coin;
        let coin_number_row =
            |idx: usize, icon: Symbol, title: &str, desc: &str, val: Option<u32>| -> View {
                settings_row(
                    SymbolIcon::new().symbol(icon).into(),
                    title,
                    desc,
                    NumberBox::new()
                        .width(160.0)
                        .minimum(0.0)
                        .maximum(99.0)
                        .value(val.map(|v| v as f64))
                        .on_value_changed(
                            context.callback(move |v| Message::CoinValueChanged(idx, v)),
                        )
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                )
            };

        // 标题行：点击展开/收起（右侧箭头随状态翻转）
        let coin_header = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(16.0)
            .children((
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(14.0)
                    .grid_column(0)
                    .children((
                        SymbolIcon::new().symbol(Symbol::Manage),
                        StackPanel::new().spacing(2.0).children((
                            TextBlock::new()
                                .text(t!("投币设置", "Coins"))
                                .font_size(14.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            TextBlock::new()
                                .text(t!("免费游玩与各场景扣币数", "Free play and coin counts per scenario"))
                                .font_size(12.0)
                                .opacity(0.6),
                        )),
                    )),
                FontIcon::new()
                    .glyph(if self.coin_expanded {
                        "\u{E70D}" // ChevronUp
                    } else {
                        "\u{E76C}" // ChevronDown
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1),
            ));

        // 展开时追加全部投币配置项
        // 标题 Border 用透明背景（而非 CardBackground）：深色模式下实色主题刷会叠加在外层卡片上变亮，
        // 导致头部与内容区呈两种背景色；transparent 仍参与命中测试（热区不变），点击不穿透。
        let mut coin_rows: Vec<KeyedView> = vec![KeyedView::new(
            "coin-header",
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .background(Color::transparent())
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .on_pointer_pressed(context.callback(move |_| Message::CoinExpanderToggled))
                .content(coin_header),
        )];
        // 展开过渡期间内容行保持挂载（淡入/淡出都由整体透明度驱动），进度为 0 才卸载
        if self.coin_anim > 0.0 || self.coin_expanded {
            // 缓出曲线：前快后慢，观感更接近原生过渡
            let eased = {
                let t = self.coin_anim;
                t * (2.0 - t)
            };
            let items = StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .opacity(eased)
                .keyed_children(vec![
                KeyedView::new(
                    "coin-freeplay",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Like).into(),
                        t!("免费游玩", "Free play"),
                        t!("开启后游戏不扣币", "No coins consumed when on"),
                        settings_toggle(
                            coin.freeplay == Some(true),
                            true,
                            context.callback(Message::FreeplayToggled),
                        ),
                    ),
                ),
                KeyedView::new("coin-sep0", separator()),
                KeyedView::new(
                    "coin-start",
                    coin_number_row(
                        0,
                        Symbol::Play,
                        t!("开始游戏币数", "Start credits"),
                        t!("开始一局所需的币数", "Credits to start a game"),
                        coin.start_coin,
                    ),
                ),
                KeyedView::new("coin-sep1", separator()),
                KeyedView::new(
                    "coin-continue",
                    coin_number_row(
                        1,
                        Symbol::Redo,
                        t!("续关币数", "Continue credits"),
                        t!("续关一次所需的币数", "Credits per continue"),
                        coin.continue_coin,
                    ),
                ),
                KeyedView::new("coin-sep2", separator()),
                KeyedView::new(
                    "coin-card",
                    coin_number_row(
                        2,
                        Symbol::Shop,
                        t!("购买车卡币数", "Card purchase credits"),
                        t!("购买一张车卡所需的币数", "Credits to buy a card"),
                        coin.card_coin,
                    ),
                ),
                KeyedView::new("coin-sep3", separator()),
                KeyedView::new(
                    "coin-car",
                    coin_number_row(
                        3,
                        Symbol::Directions,
                        t!("购买车辆币数", "Car purchase credits"),
                        t!("购买一辆车所需的币数", "Credits to buy a car"),
                        coin.car_coin,
                    ),
                ),
            ]);
            coin_rows.push(KeyedView::new("coin-content", items));
        }

        let coin_card = Border::new()
            .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .keyed_children(coin_rows),
            );

        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
                    .spacing(4.0)
                    .children((
                        StackPanel::new()
                            .spacing(4.0)
                            .children((
                                TextBlock::new()
                                    .text(t!("系统设置", "System"))
                                    .font_size(26.0)
                                    .font_weight(FontWeight::SEMI_BOLD),
                                TextBlock::new()
                                    .text(t!("", ""))
                                    .font_size(12.0)
                                    .opacity(0.6),
                            )),
                        card,
                        coin_card,
                    )),
            )
            .into()
    }

}
