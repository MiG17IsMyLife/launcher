use windows_reactor::*;

use crate::i18n;
use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle, spawn_anim_tick};

pub(in crate::reactor_app) const SYS_LOGLEVELS: &[(Option<&str>, &str, &str)] = &[
    (Some("info"), "info（默认）", "info (default)"),
    (Some("error"), "error", "error"),
    (Some("warn"), "warn", "warn"),
    (Some("debug"), "debug", "debug"),
    (Some("trace"), "trace", "trace"),
];

pub(in crate::reactor_app) const SYS_SEATS: &[(&str, &str, &str)] = &[
    ("SINGLE", "单机", "Single"),
    ("A1", "A1", "A1"),
    ("A2", "A2", "A2"),
    ("B1", "B1", "B1"),
    ("B2", "B2", "B2"),
];

impl Launcher {

    pub(in crate::reactor_app) fn save_system(&mut self) {
        if let Err(e) = self.store.save_system(&self.system) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    pub(in crate::reactor_app) fn save_coin(&mut self) {
        if let Err(e) = self.store.save_coin(&self.coin) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    pub(in crate::reactor_app) fn toggle_coin_expander(&mut self, context: &ComponentContext<Self>) {
        self.coin_expanded = !self.coin_expanded;

        self.coin_anim_gen += 1;
        let (epoch, step) = (self.coin_anim_gen, 0.016 / 0.2);
        spawn_anim_tick(context, move || Message::CoinAnimTick(epoch, step));
    }

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
            spawn_anim_tick(context, move || Message::CoinAnimTick(epoch, step));
        }
    }

    pub(in crate::reactor_app) fn system_page(&self, context: &mut ViewContext<Self>) -> View {

        let loglevel_items: Vec<String> = SYS_LOGLEVELS
            .iter()
            .map(|(_, zh, en)| i18n::pick(zh, en).to_string())
            .collect();
        let loglevel_idx = SYS_LOGLEVELS
            .iter()
            .position(|(v, _, _)| *v == self.system.loglevel.as_deref())
            .or(Some(0));

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
                        SymbolIcon::new().symbol(Symbol::Admin).into(),
                        "Keychip",
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

        let coin = &self.coin;
        let coin_number_row =
            |idx: usize, icon: Symbol, title: &str, val: Option<u32>| -> View {
                settings_row(
                    SymbolIcon::new().symbol(icon).into(),
                    title,
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
                        StackPanel::new().children([TextBlock::new()
                            .text(t!("投币设置", "Coins"))
                            .font_size(14.0)
                            .font_weight(FontWeight::SEMI_BOLD)]),
                    )),
                FontIcon::new()
                    .glyph(if self.coin_expanded {
                        "\u{E70D}"
                    } else {
                        "\u{E76C}"
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1),
            ));

        let mut coin_rows: Vec<KeyedView> = vec![KeyedView::new(
            "coin-header",
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .background(Color::transparent())
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .on_pointer_pressed(context.callback(move |_| Message::CoinExpanderToggled))
                .content(coin_header),
        )];

        if self.coin_anim > 0.0 || self.coin_expanded {

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
                            .children([TextBlock::new()
                                .text(t!("系统设置", "System"))
                                .font_size(26.0)
                                .font_weight(FontWeight::SEMI_BOLD)]),
                        card,
                        coin_card,
                    )),
            )
            .into()
    }

}
