use std::time::{Duration, Instant};

use windows_reactor::*;

use crate::i18n;
use crate::rawinput;
use crate::storage::DPAD_CODE_BASE;
use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{binding_label, separator, settings_row, settings_section, settings_toggle, spawn_anim_tick};

pub(in crate::reactor_app) const INPUT_KINDS: &[&str] = &["xinput", "dinput"];

pub(in crate::reactor_app) fn device_selection_index(
    devices: &[rawinput::GameDevice],
    saved: Option<&str>,
    saved_path: Option<&str>,
    by_path: bool,
) -> Option<usize> {
    let by_name = |key: &str| devices.iter().position(|d| d.name == key);
    let by_dev_path = |key: &str| devices.iter().position(|d| d.path == key);

    let name = saved.map(|s| crate::storage::normalize_device_name(s.trim()));
    if by_path {

        return name.and_then(by_dev_path);
    }
    saved_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .and_then(by_dev_path)
        .or_else(|| name.and_then(by_name))
}

pub(in crate::reactor_app) const INPUT_BUTTONS: &[(&str, &str)] = &[
    ("油门", "Throttle"),
    ("刹车", "Brake"),
    ("升档", "Shift Up"),
    ("降档", "Shift Down"),
    ("方向上", "Up"),
    ("方向下", "Down"),
    ("方向左", "Left"),
    ("方向右", "Right"),
    ("start 键", "Start"),
    ("view 键", "View"),
    ("投币键", "Coin"),
    ("插卡键", "Insert Card"),
    ("1 挡", "Gear 1"),
    ("2 挡", "Gear 2"),
    ("3 挡", "Gear 3"),
    ("4 挡", "Gear 4"),
    ("5 挡", "Gear 5"),
    ("6 挡", "Gear 6"),
    ("倒挡", "Reverse"),
];

pub(in crate::reactor_app) const SHIFTER_TYPES: &[&str] = &["sequential", "h"];

pub(in crate::reactor_app) const SHIFTER_TYPE_LABELS: &[(&str, &str)] = &[("序列排挡", "Sequential"), ("H 排挡", "H-Pattern")];

pub(in crate::reactor_app) const INPUT_BUTTONS_HEAD: &[usize] = &[0, 1];

pub(in crate::reactor_app) const INPUT_BUTTONS_TAIL: &[usize] = &[4, 5, 6, 7, 8, 9, 10, 11];

pub(in crate::reactor_app) const INPUT_BUTTONS_SEQ: &[usize] = &[2, 3];

pub(in crate::reactor_app) const INPUT_BUTTONS_H: &[usize] = &[12, 13, 14, 15, 16, 17, 18];

pub(in crate::reactor_app) const INPUT_RATIO_MAX: f64 = 255.0;
pub(in crate::reactor_app) const INPUT_RATIO_DEFAULT: u32 = 128;

pub(in crate::reactor_app) const INPUT_DEADZONE_DEFAULT: u32 = 20;

pub(in crate::reactor_app) const FFB_MAX: f64 = 100.0;

pub(in crate::reactor_app) const FFB_CONSTANT_FORCE_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_DAMPER_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_RUMBLE_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_RUMBLE_DURATION_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_BASE_DAMPER_DEFAULT: u32 = 20;
pub(in crate::reactor_app) const FFB_DEAD_BAND_DEFAULT: u32 = 20;

pub(in crate::reactor_app) const INPUT_CAPTURE_SECS: f64 = 10.0;

pub(in crate::reactor_app) const INPUT_CAPTURE_TICK_MS: u64 = 100;

pub(in crate::reactor_app) const XINPUT_MAP_BASE: usize = 100;

pub(in crate::reactor_app) const XINPUT_BUTTON_SLOTS: &[(&str, &str, &str)] = &[
    ("升档键", "Shift Up", "shiftUp"),
    ("降档键", "Shift Down", "shiftDn"),
    ("方向上", "Up", "up"),
    ("方向下", "Down", "down"),
    ("方向左", "Left", "left"),
    ("方向右", "Right", "right"),
    ("start 键", "Start", "start"),
    ("view 键", "View", "viewChg"),
    ("投币键", "Coin", "coin"),
    ("插卡键", "Card", "card"),
    ("油门键", "Accel", "accel"),
    ("刹车键", "Brake", "brake"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::reactor_app) enum DefaultBinding {
    Name(&'static str),
}

pub(in crate::reactor_app) const DINPUT_DEFAULTS: &[Option<DefaultBinding>] = &[
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
];

pub(in crate::reactor_app) const XINPUT_DEFAULTS: &[Option<DefaultBinding>] = &[
    Some(DefaultBinding::Name("RB")),
    Some(DefaultBinding::Name("LB")),
    Some(DefaultBinding::Name("DPAD_UP")),
    Some(DefaultBinding::Name("DPAD_DOWN")),
    Some(DefaultBinding::Name("DPAD_LEFT")),
    Some(DefaultBinding::Name("DPAD_RIGHT")),
    Some(DefaultBinding::Name("A")),
    Some(DefaultBinding::Name("B")),
    Some(DefaultBinding::Name("X")),
    Some(DefaultBinding::Name("Y")),
    Some(DefaultBinding::Name("RT")),
    Some(DefaultBinding::Name("LT")),
];

pub(in crate::reactor_app) fn default_binding_label(d: DefaultBinding) -> String {
    let DefaultBinding::Name(n) = d;
    n.to_string()
}

pub(in crate::reactor_app) fn capture_label(slot: usize) -> Option<(&'static str, &'static str)> {
    if let Some(local) = slot.checked_sub(XINPUT_MAP_BASE) {
        XINPUT_BUTTON_SLOTS.get(local).map(|&(zh, en, _)| (zh, en))
    } else {
        INPUT_BUTTONS.get(slot).copied()
    }
}

impl Launcher {

    fn dinput_slot_button(&self, slot: usize) -> Option<u32> {
        match slot {
            2 => self.input.shift_up,
            3 => self.input.shift_down,
            4 => self.input.up_button,
            5 => self.input.down_button,
            6 => self.input.left_button,
            7 => self.input.right_button,
            8 => self.input.start_button,
            9 => self.input.view_button,
            10 => self.input.coin_button,
            11 => self.input.card_button,
            12 => self.input.gear1_button,
            13 => self.input.gear2_button,
            14 => self.input.gear3_button,
            15 => self.input.gear4_button,
            16 => self.input.gear5_button,
            17 => self.input.gear6_button,
            18 => self.input.reverse_button,
            _ => None,
        }
    }

    fn dinput_slot_axis(&self, slot: usize) -> Option<&str> {
        match slot {
            0 => self.input.throttle_axis.as_deref(),
            1 => self.input.brake_axis.as_deref(),
            _ => None,
        }
    }

    fn xi_slot_binding(&self, local: usize) -> Option<&str> {
        match local {
            0 => self.input.xi_shift_up.as_deref(),
            1 => self.input.xi_shift_dn.as_deref(),
            2 => self.input.xi_up.as_deref(),
            3 => self.input.xi_down.as_deref(),
            4 => self.input.xi_left.as_deref(),
            5 => self.input.xi_right.as_deref(),
            6 => self.input.xi_start.as_deref(),
            7 => self.input.xi_view.as_deref(),
            8 => self.input.xi_coin.as_deref(),
            9 => self.input.xi_card.as_deref(),
            10 => self.input.xi_accel.as_deref(),
            11 => self.input.xi_brake.as_deref(),
            _ => None,
        }
    }

    pub(in crate::reactor_app) fn dinput_conflicts(&self) -> Vec<bool> {
        let mut flags = vec![false; INPUT_BUTTONS.len()];

        if let (Some(a), Some(b)) = (self.dinput_slot_axis(0), self.dinput_slot_axis(1)) {
            if a.eq_ignore_ascii_case(b) {
                flags[0] = true;
                flags[1] = true;
            }
        }

        let bound: Vec<(usize, u32)> = (2..INPUT_BUTTONS.len())
            .filter_map(|s| self.dinput_slot_button(s).map(|c| (s, c)))
            .collect();
        for i in 0..bound.len() {
            for j in (i + 1)..bound.len() {
                if bound[i].1 == bound[j].1 {
                    flags[bound[i].0] = true;
                    flags[bound[j].0] = true;
                }
            }
        }
        flags
    }

    pub(in crate::reactor_app) fn xinput_conflicts(&self) -> Vec<bool> {
        let mut flags = vec![false; XINPUT_BUTTON_SLOTS.len()];
        let bound: Vec<(usize, &str)> = (0..XINPUT_BUTTON_SLOTS.len())
            .filter_map(|s| self.xi_slot_binding(s).map(|n| (s, n)))
            .collect();
        for i in 0..bound.len() {
            for j in (i + 1)..bound.len() {
                if bound[i].1.eq_ignore_ascii_case(bound[j].1) {
                    flags[bound[i].0] = true;
                    flags[bound[j].0] = true;
                }
            }
        }
        flags
    }

    pub(in crate::reactor_app) fn save_input(&mut self) {
        if let Err(e) = self.store.save_input(&self.input) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    pub(in crate::reactor_app) fn end_capture(&mut self) {
        self.input_capture_gen += 1;
        self.dialog = Dialog::None;
        self.input_capture_start = None;
        self.input_capture_remain = 0.0;
    }

    fn spawn_capture_tick(context: &ComponentContext<Self>, epoch: u32) {
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(INPUT_CAPTURE_TICK_MS));
            Message::InputCaptureTick(epoch)
        });
    }

    pub(in crate::reactor_app) fn refresh_input_devices(&mut self, context: &ComponentContext<Self>) {
        self.input_devices_scanned = false;
        let kind = self.input.kind.clone();
        context.spawn_background(move |_| {
            Message::InputDevicesScanned(Box::new(rawinput::list_input_devices(kind.as_deref())))
        });
    }

    pub(in crate::reactor_app) fn input_page(&self, context: &mut ViewContext<Self>) -> View {
        let cfg = &self.input;
        let dinput = cfg.kind.as_deref() == Some("dinput");
        let xinput = cfg.kind.as_deref() == Some("xinput");

        let dinput_conf = self.dinput_conflicts();
        let xi_conf = self.xinput_conflicts();

        let basic_rows = self.input_basic_rows(context);

        let ffb_card = self.input_ffb_card(context);

        let mut sections: Vec<KeyedView> = vec![
            KeyedView::new("sec-basic", settings_section(t!("基本设置", "Basic"), basic_rows)),
            KeyedView::new("sec-ffb", ffb_card),
        ];
        if xinput || dinput {
            let mut detail: Vec<KeyedView> = Vec::new();
            if xinput {

                let pad_saved = cfg.pad_index.map(|i| i.to_string());

                let pad_path: Option<String> = None;
                detail.push(KeyedView::new(
                    "in-pad",
                    self.input_device_row(context,
                        Symbol::Directions,
                        t!("手柄设备", "Gamepad"),
                        &pad_saved,
                        &pad_path,
                        true,
                        3,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-pad", separator()));

                detail.push(KeyedView::new(
                    "in-dual-stick",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Directions).into(),
                        t!("双摇杆模式", "Dual stick mode"),
                        settings_toggle(
                            cfg.dual_stick == Some(true),
                            true,
                            context.callback(Message::InputDualStickToggled),
                        ),
                    ),
                ));
                detail.push(KeyedView::new("in-sep-dual", separator()));
                detail.push(KeyedView::new(
                    "in-linear",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Directions).into(),
                        t!("线性方向", "Linear steering"),
                        settings_toggle(

                            cfg.linear_steering != Some(false),
                            true,
                            context.callback(Message::InputLinearSteeringToggled),
                        ),
                    ),
                ));
                detail.push(KeyedView::new("in-sep-linear", separator()));

                let deadzone_row = |title: &str, _key: &str, value: f64, to_msg: fn(f64) -> Message| -> View {
                    let slider: View = Slider::new()
                        .width(260.0)
                        .minimum(0.0)
                        .maximum(100.0)
                        .step_frequency(1.0)
                        .value(value)
                        .on_value_changed(context.callback(move |v: f64| to_msg(v)))
                        .vertical_alignment(VerticalAlignment::Center)
                        .into();
                    let text: View = TextBlock::new()
                        .text(format!("{}%", value as u32))
                        .vertical_alignment(VerticalAlignment::Center)
                        .into();
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Directions).into(),
                        title,
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(12.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((slider, text))
                            .into(),
                    )
                };
                let ldz = cfg.left_deadzone.unwrap_or(INPUT_DEADZONE_DEFAULT) as f64;
                detail.push(KeyedView::new(
                    "in-left-dz",
                    deadzone_row(t!("左摇杆死区", "Left stick deadzone"), "leftStickDeadzone", ldz, Message::InputLeftDeadzoneChanged),
                ));
                detail.push(KeyedView::new("in-sep-ldz", separator()));
                let rdz = cfg.right_deadzone.unwrap_or(INPUT_DEADZONE_DEFAULT) as f64;
                detail.push(KeyedView::new(
                    "in-right-dz",
                    deadzone_row(t!("右摇杆死区", "Right stick deadzone"), "rightStickDeadzone", rdz, Message::InputRightDeadzoneChanged),
                ));
                detail.push(KeyedView::new("in-sep-rdz", separator()));

                detail.push(KeyedView::new(
                    "in-xi-head",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Keyboard).into(),
                        t!("按键映射", "Button mapping"),
                        View::from(StackPanel::new()),
                    ),
                ));
                for local in 0..XINPUT_BUTTON_SLOTS.len() {
                    detail.push(KeyedView::new(
                        format!("in-sep-xi{local}"),
                        separator(),
                    ));
                    detail.push(KeyedView::new(
                        format!("in-xi-{local}"),
                        self.input_xi_button_row(context, local, &xi_conf),
                    ));
                }
            }
            if dinput {
                detail.push(KeyedView::new(
                    "in-wheel",
                    self.input_device_row(context,
                        Symbol::Directions,
                        t!("方向盘设备", "Steering wheel"),
                        &cfg.wheel_device,
                        &cfg.wheel_device_path,
                        false,
                        0,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-w", separator()));
                detail.push(KeyedView::new(
                    "in-pedal",
                    self.input_device_row(context,
                        Symbol::Up,
                        t!("油门踏板设备", "Throttle pedal"),
                        &cfg.pedal_device,
                        &cfg.pedal_device_path,
                        false,
                        1,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-p", separator()));
                detail.push(KeyedView::new(
                    "in-shifter",
                    self.input_device_row(context,
                        Symbol::Switch,
                        t!("排挡设备", "Shifter"),
                        &cfg.shifter_device,
                        &cfg.shifter_device_path,
                        false,
                        2,
                    ),
                ));

                for slot in INPUT_BUTTONS_HEAD {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), self.input_button_row(context, *slot, &dinput_conf)));
                }

                detail.push(KeyedView::new("in-sep-s", separator()));
                let shifter_items: Vec<String> =
                    SHIFTER_TYPE_LABELS.iter().map(|&(zh, en)| i18n::pick(zh, en).to_string()).collect();
                let shifter_idx = SHIFTER_TYPES
                    .iter()
                    .position(|v| Some(*v) == cfg.shifter_type.as_deref());
                detail.push(KeyedView::new(
                    "in-shifter-type",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::List).into(),
                        t!("排挡类型", "Shifter type"),
                        ComboBox::new()
                            .width(180.0)
                            .items_source(shifter_items)
                            .selected_index(shifter_idx)
                            .placeholder_text(t!("选择排挡类型", "Select shifter type"))
                            .on_selection_changed(
                                context.callback(Message::InputShifterTypeSelected),
                            )
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                ));

                let shifter_slots: &[usize] = match cfg.shifter_type.as_deref() {
                    Some("sequential") => INPUT_BUTTONS_SEQ,
                    Some("h") => INPUT_BUTTONS_H,
                    _ => &[],
                };
                for slot in shifter_slots {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), self.input_button_row(context, *slot, &dinput_conf)));
                }

                for slot in INPUT_BUTTONS_TAIL {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), self.input_button_row(context, *slot, &dinput_conf)));
                }
            }
            sections.push(KeyedView::new(
                "sec-detail",
                settings_section(t!("设备与按键", "Devices & buttons"), detail),
            ));
        }

        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
                    .spacing(4.0)
                    .children((
                        self.input_header(context),
                        StackPanel::new()
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .keyed_children(sections),
                    )),
            )
            .into()
    }

    fn input_header(&self, context: &mut ViewContext<Self>) -> View {

        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                StackPanel::new()
                    .grid_column(0)
                    .children([TextBlock::new()
                        .text(t!("输入设置", "Input"))
                        .font_size(26.0)
                        .font_weight(FontWeight::SEMI_BOLD)]),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Message::InputRefreshDevices))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .content(
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                FontIcon::new().glyph("\u{E72C}"),
                                TextBlock::new().text(t!("扫描设备", "Scan")),
                            )),
                    ),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Message::InputDeviceDiagnosticsOpen))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(2)
                    .content(
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                FontIcon::new().glyph("\u{E9E9}"),
                                TextBlock::new()
                                    .text(t!("设备诊断", "Diagnostics")),
                            )),
                    )
                    .tooltip(t!(
                        "列出各 HID 接口解析到的设备名与写入值，可与「控制面板 → 游戏控制器」里的名字对照",
                        "List parsed device names and the value written to ini; compare with Control Panel → Game Controllers"
                    )),
            ))
    }

    fn input_ffb_card(&self, context: &mut ViewContext<Self>) -> View {
        let cfg = &self.input;

        let ffb_enable = cfg.ffb_enable == Some(true) || cfg.ffb_enable.is_none();
        let ffb_title = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(14.0)
            .children((
                SymbolIcon::new().symbol(Symbol::Rotate),
                StackPanel::new().children([TextBlock::new()
                    .text(t!("力反馈", "Force feedback"))
                    .font_size(14.0)
                    .font_weight(FontWeight::SEMI_BOLD)]),
            ));

        let ffb_title_btn = Border::new()
            .background(Color::transparent())
            .on_pointer_pressed(context.callback(move |_| Message::FfbExpanderToggled))
            .grid_column(0)
            .content(ffb_title);
        let ffb_toggle = StackPanel::new()
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1)
            .children((settings_toggle(
                ffb_enable,
                true,
                context.callback(Message::FfbEnableToggled),
            ),));

        let ffb_chevron = Border::new()
            .background(Color::transparent())
            .on_pointer_pressed(context.callback(move |_| Message::FfbExpanderToggled))
            .grid_column(2)
            .content(
                FontIcon::new()
                    .glyph(if self.ffb_expanded {
                        "\u{E70D}"
                    } else {
                        "\u{E76C}"
                    })
                    .vertical_alignment(VerticalAlignment::Center),
            );
        let ffb_header = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
            .column_spacing(16.0)
            .children((ffb_title_btn, ffb_toggle, ffb_chevron));
        let mut ffb_rows: Vec<KeyedView> = vec![KeyedView::new(
            "ffb-header",
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .background(Color::transparent())
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .content(ffb_header),
        )];

        if self.ffb_anim > 0.0 || self.ffb_expanded {
            let eased = {
                let t = self.ffb_anim;
                t * (2.0 - t)
            };

            let ffb_slider_row = |idx: usize, icon: Symbol, title: &str, value: f64| -> View {
                let slider: View = Slider::new()
                    .width(260.0)
                    .minimum(0.0)
                    .maximum(FFB_MAX)
                    .step_frequency(1.0)
                    .value(value)
                    .on_value_changed(context.callback(move |v: f64| Message::FfbParamChanged(idx, v)))
                    .vertical_alignment(VerticalAlignment::Center)
                    .into();
                let text: View = TextBlock::new()
                    .text(format!("{}", value as u32))
                    .vertical_alignment(VerticalAlignment::Center)
                    .into();
                settings_row(
                    SymbolIcon::new().symbol(icon).into(),
                    title,
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((slider, text))
                        .into(),
                )
            };
            let ffb_detail: Vec<KeyedView> = vec![
                KeyedView::new("ffb-cf", ffb_slider_row(0, Symbol::Directions, t!("恒力强度", "Constant force"), cfg.ffb_constant_force.unwrap_or(FFB_CONSTANT_FORCE_DEFAULT) as f64)),
                KeyedView::new("ffb-sep1", separator()),
                KeyedView::new("ffb-damper", ffb_slider_row(1, Symbol::Directions, t!("阻尼强度", "Damper"), cfg.ffb_damper.unwrap_or(FFB_DAMPER_DEFAULT) as f64)),
                KeyedView::new("ffb-sep2", separator()),
                KeyedView::new("ffb-rumble", ffb_slider_row(2, Symbol::Directions, t!("震动强度", "Rumble"), cfg.ffb_rumble.unwrap_or(FFB_RUMBLE_DEFAULT) as f64)),
                KeyedView::new("ffb-sep3", separator()),
                KeyedView::new("ffb-dur", ffb_slider_row(3, Symbol::Directions, t!("震动时长", "Rumble duration"), cfg.ffb_rumble_duration.unwrap_or(FFB_RUMBLE_DURATION_DEFAULT) as f64)),
                KeyedView::new("ffb-sep4", separator()),
                KeyedView::new("ffb-basedamper", ffb_slider_row(4, Symbol::Directions, t!("基础阻尼", "Base damper"), cfg.ffb_base_damper.unwrap_or(FFB_BASE_DAMPER_DEFAULT) as f64)),
                KeyedView::new("ffb-sep5", separator()),
                KeyedView::new("ffb-deadband", ffb_slider_row(5, Symbol::Directions, t!("中心死区", "Dead band"), cfg.ffb_dead_band.unwrap_or(FFB_DEAD_BAND_DEFAULT) as f64)),
            ];
            let items = StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .opacity(eased)
                .keyed_children(ffb_detail);
            ffb_rows.push(KeyedView::new("ffb-content", items));
        }
        let ffb_card: View = Border::new()
            .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .keyed_children(ffb_rows),
            )
            .into();

        ffb_card
    }

    fn input_basic_rows(&self, context: &mut ViewContext<Self>) -> Vec<KeyedView> {
        let cfg = &self.input;
        let kind_items: Vec<String> = INPUT_KINDS.iter().map(|v| v.to_string()).collect();
        let kind_idx = INPUT_KINDS
            .iter()
            .position(|v| Some(*v) == cfg.kind.as_deref());

        let ratio_value = cfg.ratio.unwrap_or(INPUT_RATIO_DEFAULT) as f64;
        let fixed_key = |key: &str| -> View {
            TextBlock::new()
                .text(key)
                .vertical_alignment(VerticalAlignment::Center)
                .into()
        };
        let basic_rows: Vec<KeyedView> = vec![
            KeyedView::new(
                "in-test",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Setting).into(),
                    t!("test 键", "Test key"),
                    fixed_key("F1"),
                ),
            ),
            KeyedView::new("in-sep-test", separator()),
            KeyedView::new(
                "in-service",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Manage).into(),
                    t!("service 键", "Service key"),
                    fixed_key("F2"),
                ),
            ),
            KeyedView::new("in-sep-service", separator()),
            KeyedView::new(
                "in-coin",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Keyboard).into(),
                    t!("投币键", "Coin key"),
                    fixed_key("F3"),
                ),
            ),
            KeyedView::new("in-sep-coin", separator()),
            KeyedView::new(
                "in-kind",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Manage).into(),
                    t!("输入模式", "Input mode"),
                    ComboBox::new()

                        .width(180.0)
                        .items_source(kind_items)
                        .selected_index(kind_idx)
                        .placeholder_text(t!("选择输入模式", "Select input mode"))
                        .on_selection_changed(context.callback(Message::InputKindSelected))
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                ),
            ),
            KeyedView::new("in-sep-kind", separator()),
            KeyedView::new(
                "in-ratio",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Rotate).into(),
                    t!("输入信号限制", "Input signal limit"),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children({

                            let slider: View = Slider::new()
                                .width(300.0)
                                .minimum(0.0)
                                .maximum(INPUT_RATIO_MAX)
                                .step_frequency(1.0)
                                .value(ratio_value)
                                .on_value_changed(context.callback(Message::InputRatioChanged))
                                .vertical_alignment(VerticalAlignment::Center)
                                .into();
                            let text: View = TextBlock::new()
                                .text(format!("{}", ratio_value as u32))
                                .vertical_alignment(VerticalAlignment::Center)
                                .into();
                            let reset: View = Button::new()
                                .style(ButtonStyle::Subtle)
                                .on_click(context.message(Message::InputRatioReset))
                                .vertical_alignment(VerticalAlignment::Center)

                                .content(FontIcon::new().glyph("\u{E7A7}"))
                                .tooltip(tf!("恢复默认 {}", "Reset to default {}", INPUT_RATIO_DEFAULT))
                                .into();
                            (slider, text, reset)
                        })
                        .into(),
                ),
            ),
        ];

        basic_rows
    }

    fn input_xi_button_row(
        &self,
        context: &mut ViewContext<Self>,
        slot_local: usize,
        xi_conf: &[bool],
    ) -> View {
            let (zh, en, _key) = XINPUT_BUTTON_SLOTS[slot_local];

            let current: Option<String> = match slot_local {
                0 => self.input.xi_shift_up.clone(),
                1 => self.input.xi_shift_dn.clone(),
                2 => self.input.xi_up.clone(),
                3 => self.input.xi_down.clone(),
                4 => self.input.xi_left.clone(),
                5 => self.input.xi_right.clone(),
                6 => self.input.xi_start.clone(),
                7 => self.input.xi_view.clone(),
                8 => self.input.xi_coin.clone(),
                9 => self.input.xi_card.clone(),
                10 => self.input.xi_accel.clone(),
                _ => self.input.xi_brake.clone(),
            };

            let conflict = xi_conf[slot_local];

            let desc = match current {
                Some(c) => c,
                None => match XINPUT_DEFAULTS.get(slot_local).copied().flatten() {
                    Some(d) => tf!("默认 {}", "Default: {}", default_binding_label(d)),
                    None => t!("未绑定", "Unbound").to_string(),
                },
            };

            let left: View = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(14.0)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .children((
                    View::from(SymbolIcon::new().symbol(Symbol::Accept)),
                    StackPanel::new()
                        .spacing(2.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new().text(i18n::pick(zh, en)).font_size(14.0),
                            {
                                let mut tb = TextBlock::new().text(desc).font_size(12.0);
                                if conflict {
                                    tb = tb.foreground(Brush::Solid(Color::argb(255, 0xE8, 0x11, 0x23)));
                                } else {
                                    tb = tb.opacity(0.6);
                                }
                                tb
                            },
                        )),
                ))
                .into();
            let capture_btn: View = Button::new()
                .on_click(
                    context.message(Message::InputCaptureStart(XINPUT_MAP_BASE + slot_local)),
                )
                .content(t!("捕获", "Capture"))
                .into();
            let right: View = Border::new()
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .content(capture_btn)
                .into();
            let row: View = Grid::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((left, right))
                .into();
            if conflict {

                Border::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .corner_radius(4.0)
                    .border_brush(Brush::Solid(Color::argb(255, 0xE8, 0x11, 0x23)))
                    .border_thickness(1.5)
                    .background(Color::argb(30, 0xE8, 0x11, 0x23))
                    .content(row)
                    .into()
            } else {
                row
            }
    }

    fn input_button_row(
        &self,
        context: &mut ViewContext<Self>,
        slot: usize,
        dinput_conf: &[bool],
    ) -> View {
            let (zh, en) = INPUT_BUTTONS[slot];

            let current: Option<String> = match slot {
                0 => self.input.throttle_axis.as_ref().map(|a| tf!("轴 {}", "Axis {}", a)),
                1 => self.input.brake_axis.as_ref().map(|a| tf!("轴 {}", "Axis {}", a)),
                2 => self.input.shift_up.map(binding_label),
                3 => self.input.shift_down.map(binding_label),
                4 => self.input.up_button.map(binding_label),
                5 => self.input.down_button.map(binding_label),
                6 => self.input.left_button.map(binding_label),
                7 => self.input.right_button.map(binding_label),
                8 => self.input.start_button.map(binding_label),
                9 => self.input.view_button.map(binding_label),
                10 => self.input.coin_button.map(binding_label),
                11 => self.input.card_button.map(binding_label),
                12 => self.input.gear1_button.map(binding_label),
                13 => self.input.gear2_button.map(binding_label),
                14 => self.input.gear3_button.map(binding_label),
                15 => self.input.gear4_button.map(binding_label),
                16 => self.input.gear5_button.map(binding_label),
                17 => self.input.gear6_button.map(binding_label),
                _ => self.input.reverse_button.map(binding_label),
            };

            let conflict = dinput_conf[slot];

            let desc = match current {
                Some(c) => c,
                None => match DINPUT_DEFAULTS.get(slot).copied().flatten() {
                    Some(d) => tf!("默认 {}", "Default: {}", default_binding_label(d)),
                    None => t!("未绑定", "Unbound").to_string(),
                },
            };

            let left: View = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(14.0)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .children((
                    View::from(SymbolIcon::new().symbol(Symbol::Accept)),
                    StackPanel::new()
                        .spacing(2.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new().text(i18n::pick(zh, en)).font_size(14.0),
                            {
                                let mut tb = TextBlock::new().text(desc).font_size(12.0);
                                if conflict {
                                    tb = tb.foreground(Brush::Solid(Color::argb(255, 0xE8, 0x11, 0x23)));
                                } else {
                                    tb = tb.opacity(0.6);
                                }
                                tb
                            },
                        )),
                ))
                .into();

            let capture_btn: View = Button::new()
                .on_click(context.message(Message::InputCaptureStart(slot)))
                .content(t!("捕获", "Capture"))
                .into();
            let right_content: View = if slot == 0 || slot == 1 {
                let rev = match slot {
                    0 => self.input.reverse_accel_axis.unwrap_or(false),
                    1 => self.input.reverse_brake_axis.unwrap_or(false),
                    _ => false,
                };
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(12.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        CheckBox::new()
                            .is_checked(rev)
                            .on_is_checked_changed(
                                context.callback(move |on: bool| Message::InputReverseToggled(slot, on)),
                            )
                            .vertical_alignment(VerticalAlignment::Center)
                            .content(t!("反转轴值", "Reverse axis")),
                        capture_btn,
                    ))
                    .into()
            } else {
                capture_btn
            };
            let right: View = Border::new()
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .content(right_content)
                .into();
            let row: View = Grid::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((left, right))
                .into();
            if conflict {

                Border::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .corner_radius(4.0)
                    .border_brush(Brush::Solid(Color::argb(255, 0xE8, 0x11, 0x23)))
                    .border_thickness(1.5)
                    .background(Color::argb(30, 0xE8, 0x11, 0x23))
                    .content(row)
                    .into()
            } else {
                row
            }
    }

    fn input_device_row(
        &self,
        context: &mut ViewContext<Self>,
        icon: Symbol,
        title: &str,
        saved: &Option<String>,
        saved_path: &Option<String>,
        by_path: bool,
        category: usize,
    ) -> View {
            let items: Vec<String> = self.input_devices.iter().map(|d| d.label.clone()).collect();

            let selected = device_selection_index(
                &self.input_devices,
                saved.as_deref(),
                saved_path.as_deref(),
                by_path,
            );
            let placeholder = if !self.input_devices_scanned {
                t!("正在扫描设备…", "Scanning devices...").to_string()
            } else if self.input.kind.is_none() {
                t!("请先选择输入类型", "Select an input type first").to_string()
            } else if self.input_devices.is_empty() {
                t!("未发现设备，点击右上角「扫描设备」", "No device found; click \"Scan\" at top right").to_string()
            } else if selected.is_some() {
                String::new()
            } else if let Some(v) = saved.as_deref().map(str::trim).filter(|v| !v.is_empty()) {

                tf!(
                    "已保存：{}（列表中无同名设备，按此值写入）",
                    "Saved: {} (no device with this name in the list; this value is written as-is)",
                    v
                )
            } else {
                t!("选择设备", "Select device").to_string()
            };
            let combo: View = ComboBox::new()
                .width(430.0)
                .items_source(items)
                .selected_index(selected)
                .placeholder_text(placeholder)
                .on_selection_changed(
                    context.callback(move |i| Message::InputDeviceSelected(category, i)),
                )
                .vertical_alignment(VerticalAlignment::Center)
                .into();

            let right: View = combo;
            settings_row(SymbolIcon::new().symbol(icon).into(), title, right)
    }

    pub(in crate::reactor_app) fn toggle_ffb_expander(&mut self, context: &ComponentContext<Self>) {
        self.ffb_expanded = !self.ffb_expanded;

        self.ffb_anim_gen += 1;
        let (epoch, step) = (self.ffb_anim_gen, 0.016 / 0.2);
        spawn_anim_tick(context, move || Message::FfbAnimTick(epoch, step));
    }

    pub(in crate::reactor_app) fn tick_ffb_anim(&mut self, epoch: u32, step: f64, context: &ComponentContext<Self>) {
        if epoch != self.ffb_anim_gen {
            return;
        }
        if self.ffb_expanded {
            self.ffb_anim = (self.ffb_anim + step).min(1.0);
        } else {
            self.ffb_anim = (self.ffb_anim - step).max(0.0);
        }
        let done = if self.ffb_expanded {
            self.ffb_anim >= 1.0
        } else {
            self.ffb_anim <= 0.0
        };
        if !done {
            spawn_anim_tick(context, move || Message::FfbAnimTick(epoch, step));
        }
    }

    pub(in crate::reactor_app) fn apply_device_selected(&mut self, slot: usize, index: usize) {
        if let Some(d) = self.input_devices.get(index) {

            let name = d.name.clone();

            let path = d.path.clone();
            match slot {

                0 => {
                    self.input.wheel_device = Some(name);
                    self.input.wheel_device_path = Some(path);
                }
                1 => {
                    self.input.pedal_device = Some(name);
                    self.input.pedal_device_path = Some(path);
                }
                2 => {
                    self.input.shifter_device = Some(name);
                    self.input.shifter_device_path = Some(path);
                }

                _ => self.input.pad_index = d.path.parse::<u32>().ok(),
            }
            self.save_input();
        }
    }

    pub(in crate::reactor_app) fn start_capture(&mut self, slot: usize, context: &ComponentContext<Self>) {

        if let Some(local) = slot.checked_sub(XINPUT_MAP_BASE) {
            if let Some(&(zh, en, _)) = XINPUT_BUTTON_SLOTS.get(local) {
                self.input_capture_gen += 1;
                let epoch = self.input_capture_gen;
                self.dialog = Dialog::InputCapture {
                    slot,
                    label: i18n::pick(zh, en),
                };
                self.input_capture_start = Some(Instant::now());
                self.input_capture_remain = 1.0;
                Self::spawn_capture_tick(context, epoch);
                let pad = self.input.pad_index.unwrap_or(0);
                context.spawn_background(move |_| {
                    match rawinput::capture_xi_button(pad, (INPUT_CAPTURE_SECS * 1000.0) as u32) {
                        Some(c) => Message::InputCapturedXi(epoch, slot, c.name),
                        None => Message::InputCaptureTimeout(epoch, slot),
                    }
                });
                return;
            }
        }
        let Some(&(zh, en)) = INPUT_BUTTONS.get(slot) else {
            return;
        };

        self.input_capture_gen += 1;
        let epoch = self.input_capture_gen;
        self.dialog = Dialog::InputCapture {
            slot,
            label: i18n::pick(zh, en),
        };

        self.input_capture_start = Some(Instant::now());
        self.input_capture_remain = 1.0;
        Self::spawn_capture_tick(context, epoch);
        context.spawn_background(move |_| {
            if slot == 0 || slot == 1 {

                match rawinput::capture_axis((INPUT_CAPTURE_SECS * 1000.0) as u32) {
                    Some(c) => Message::InputCapturedAxis(epoch, slot, c.name),
                    None => Message::InputCaptureTimeout(epoch, slot),
                }
            } else {
                match rawinput::capture_button((INPUT_CAPTURE_SECS * 1000.0) as u32) {
                    Some(c) => {

                        let code = if c.is_dpad {
                            DPAD_CODE_BASE + c.button
                        } else {
                            c.button
                        };
                        Message::InputCaptured(epoch, slot, code)
                    }
                    None => Message::InputCaptureTimeout(epoch, slot),
                }
            }
        });
    }

    pub(in crate::reactor_app) fn tick_capture(&mut self, epoch: u32, context: &ComponentContext<Self>) {

        if epoch != self.input_capture_gen
            || !matches!(self.dialog, Dialog::InputCapture { .. })
        {
            return;
        }
        let Some(start) = self.input_capture_start else {
            return;
        };
        let remain =
            (1.0 - start.elapsed().as_secs_f64() / INPUT_CAPTURE_SECS).clamp(0.0, 1.0);
        self.input_capture_remain = remain;
        if remain <= 0.0 {

            let slot = match self.dialog {
                Dialog::InputCapture { slot, .. } => Some(slot),
                _ => None,
            };
            if let Some((zh, en)) = slot.and_then(capture_label) {
                self.dialog = Dialog::Error {
                    message: tf!("捕获「{}」超时，未检测到按键", "Capture of \"{}\" timed out; no key detected",
                        i18n::pick(zh, en)
                    ),
                };
            }
            self.end_capture();
            return;
        }
        Self::spawn_capture_tick(context, epoch);
    }

    pub(in crate::reactor_app) fn apply_captured_button(&mut self, epoch: u32, slot: usize, button: u32) {
        if epoch != self.input_capture_gen {
            return;
        }
        if INPUT_BUTTONS.get(slot).is_some() {
            match slot {
                2 => self.input.shift_up = Some(button),
                3 => self.input.shift_down = Some(button),
                4 => self.input.up_button = Some(button),
                5 => self.input.down_button = Some(button),
                6 => self.input.left_button = Some(button),
                7 => self.input.right_button = Some(button),
                8 => self.input.start_button = Some(button),
                9 => self.input.view_button = Some(button),
                10 => self.input.coin_button = Some(button),
                11 => self.input.card_button = Some(button),
                12 => self.input.gear1_button = Some(button),
                13 => self.input.gear2_button = Some(button),
                14 => self.input.gear3_button = Some(button),
                15 => self.input.gear4_button = Some(button),
                16 => self.input.gear5_button = Some(button),
                17 => self.input.gear6_button = Some(button),
                _ => self.input.reverse_button = Some(button),
            }
            self.save_input();
        }

        self.end_capture();
    }

    pub(in crate::reactor_app) fn apply_captured_xi_button(&mut self, epoch: u32, slot: usize, name: String) {
        if epoch != self.input_capture_gen {
            return;
        }

        let Some(local) = slot.checked_sub(XINPUT_MAP_BASE) else {
            self.end_capture();
            return;
        };
        if XINPUT_BUTTON_SLOTS.get(local).is_some() {
            match local {
                0 => self.input.xi_shift_up = Some(name.clone()),
                1 => self.input.xi_shift_dn = Some(name.clone()),
                2 => self.input.xi_up = Some(name.clone()),
                3 => self.input.xi_down = Some(name.clone()),
                4 => self.input.xi_left = Some(name.clone()),
                5 => self.input.xi_right = Some(name.clone()),
                6 => self.input.xi_start = Some(name.clone()),
                7 => self.input.xi_view = Some(name.clone()),
                8 => self.input.xi_coin = Some(name.clone()),
                9 => self.input.xi_card = Some(name.clone()),
                10 => self.input.xi_accel = Some(name.clone()),
                _ => self.input.xi_brake = Some(name.clone()),
            }
            self.save_input();
        }

        self.end_capture();
    }

    pub(in crate::reactor_app) fn apply_captured_axis(&mut self, epoch: u32, slot: usize, axis_name: String) {
        if epoch != self.input_capture_gen {
            return;
        }
        if INPUT_BUTTONS.get(slot).is_some() {
            match slot {
                0 => self.input.throttle_axis = Some(axis_name.clone()),
                1 => self.input.brake_axis = Some(axis_name.clone()),
                _ => {}
            }
            self.save_input();
        }

        self.end_capture();
    }

}

#[cfg(test)]
mod tests {
    use super::device_selection_index;
    use crate::rawinput::GameDevice;

    fn t598() -> Vec<GameDevice> {
        ["MI_00", "MI_01", "MI_02"]
            .iter()
            .map(|mi| GameDevice {
                path: format!(r"\\?\HID#VID_044F&PID_B696&{}", mi),
                label: format!("Thrustmaster Advance Racer（VID_044F&PID_B696&{}）", mi),
                name: "Thrustmaster Advance Racer".to_string(),
            })
            .collect()
    }

    #[test]
    fn same_name_interfaces_echo_by_path() {
        let devices = t598();

        assert_eq!(
            device_selection_index(&devices, Some("Thrustmaster Advance Racer"), None, false),
            Some(0)
        );

        assert_eq!(
            device_selection_index(
                &devices,
                Some("Thrustmaster Advance Racer"),
                Some(r"\\?\HID#VID_044F&PID_B696&MI_01"),
                false
            ),
            Some(1)
        );
        assert_eq!(
            device_selection_index(
                &devices,
                Some("Thrustmaster Advance Racer"),
                Some(r"\\?\HID#VID_044F&PID_B696&MI_02"),
                false
            ),
            Some(2)
        );
    }

    #[test]
    fn typed_name_without_match_is_not_selected() {
        let devices = t598();

        assert_eq!(
            device_selection_index(&devices, Some("My Custom Wheel"), None, false),
            None
        );

        assert_eq!(
            device_selection_index(&devices, Some("Thrustmaster Advance Racer"), None, false),
            Some(0)
        );
    }

    #[test]
    fn legacy_suffixed_saved_name_still_matches() {
        let devices = t598();

        assert_eq!(
            device_selection_index(
                &devices,
                Some("Thrustmaster Advance Racer（VID_044F&PID_B696&MI_01）"),
                None,
                false
            ),
            Some(0)
        );
    }

    #[test]
    fn xinput_slot_matches_path() {
        let devices = vec![
            GameDevice {
                path: "0".into(),
                label: "手柄 1（XInput）".into(),
                name: "手柄 1（XInput）".into(),
            },
            GameDevice {
                path: "3".into(),
                label: "手柄 4（XInput）".into(),
                name: "手柄 4（XInput）".into(),
            },
        ];
        assert_eq!(device_selection_index(&devices, Some("3"), None, true), Some(1));
        assert_eq!(device_selection_index(&devices, Some("2"), None, true), None);
    }
}
