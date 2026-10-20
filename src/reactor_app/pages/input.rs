//! 输入设置页：输入模式 / 设备选择 / 按键捕获 / 力反馈，及对应处理逻辑。
use std::time::{Duration, Instant};

use windows_reactor::*;

use crate::i18n;
use crate::rawinput;
use crate::storage::DPAD_CODE_BASE;
use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{binding_label, separator, settings_row, settings_section, settings_toggle};

/// 输入设置页：输入类型候选
pub(in crate::reactor_app) const INPUT_KINDS: &[&str] = &["xinput", "dinput"];

/// 输入设置页：按键捕获槽位名称（顺序对应 InputConfig 的各按键字段）；(中文, 英文)
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
/// 排挡类型候选（顺序对应 SHIFTER_TYPE_LABELS）；落库取值见注释
pub(in crate::reactor_app) const SHIFTER_TYPES: &[&str] = &["sequential", "h"];
/// 排挡类型下拉的显示名（中文, 英文）
pub(in crate::reactor_app) const SHIFTER_TYPE_LABELS: &[(&str, &str)] = &[("序列排挡", "Sequential"), ("H 排挡", "H-Pattern")];
/// 排在「排挡类型」之前的按键槽位：油门 / 刹车
pub(in crate::reactor_app) const INPUT_BUTTONS_HEAD: &[usize] = &[0, 1];
/// 排在排挡按键之后的按键槽位：方向四键 / start / view / 投币 / 插卡
pub(in crate::reactor_app) const INPUT_BUTTONS_TAIL: &[usize] = &[4, 5, 6, 7, 8, 9, 10, 11];
/// 序列排挡时的按键槽位：升档 / 降档
pub(in crate::reactor_app) const INPUT_BUTTONS_SEQ: &[usize] = &[2, 3];
/// H 排挡时的按键槽位：1~6 挡 + 倒挡
pub(in crate::reactor_app) const INPUT_BUTTONS_H: &[usize] = &[12, 13, 14, 15, 16, 17, 18];
/// 输入信号限制滑块（原「输入比率」）：范围 0~255，默认 128
pub(in crate::reactor_app) const INPUT_RATIO_MAX: f64 = 255.0;
pub(in crate::reactor_app) const INPUT_RATIO_DEFAULT: u32 = 128;
/// 左右摇杆死区滑块：范围 0~100（百分比），默认 20
pub(in crate::reactor_app) const INPUT_DEADZONE_DEFAULT: u32 = 20;
/// 力反馈参数滑块：范围 0~100
pub(in crate::reactor_app) const FFB_MAX: f64 = 100.0;
/// 力反馈各参数默认值（与 idas.ini/config.ini [ffb] 一致）
pub(in crate::reactor_app) const FFB_CONSTANT_FORCE_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_DAMPER_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_RUMBLE_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_RUMBLE_DURATION_DEFAULT: u32 = 100;
pub(in crate::reactor_app) const FFB_BASE_DAMPER_DEFAULT: u32 = 20;
pub(in crate::reactor_app) const FFB_DEAD_BAND_DEFAULT: u32 = 20;
/// 按键捕获倒计时总时长（秒）：进度条从满格走到 0，归零即自动关闭
pub(in crate::reactor_app) const INPUT_CAPTURE_SECS: f64 = 10.0;
/// 倒计时刷新间隔（毫秒）
pub(in crate::reactor_app) const INPUT_CAPTURE_TICK_MS: u64 = 100;
/// XInput 按钮名映射槽位的下标基址：与 dinput 的 INPUT_BUTTONS（最大 17）区间错开，
/// 在 start_capture / 分发时据此区分 XInput 映射槽与 dinput 按键槽。
pub(in crate::reactor_app) const XINPUT_MAP_BASE: usize = 100;
/// XInput 按钮名映射项：顺序向 dinput 的 INPUT_BUTTONS 看齐（升/降档 → 方向四键 → start/view → 投币 → 插卡），
/// 末尾为油门 / 刹车源（kitten 与 mice 的 [xinput] 均支持 accel/brake 键）。
/// 元组为：中文标题、英文标题、配置键名（与 mice.rs / kitten.rs 中硬编码一致，仅供参考）。
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

/// 按捕获槽位取标签（中文、英文）：XInput 映射槽（>= XINPUT_MAP_BASE）查 XINPUT_BUTTON_SLOTS，
/// 其余查 dinput 的 INPUT_BUTTONS。供超时提示文案复用。
pub(in crate::reactor_app) fn capture_label(slot: usize) -> Option<(&'static str, &'static str)> {
    if let Some(local) = slot.checked_sub(XINPUT_MAP_BASE) {
        XINPUT_BUTTON_SLOTS.get(local).map(|&(zh, en, _)| (zh, en))
    } else {
        INPUT_BUTTONS.get(slot).copied()
    }
}

impl Launcher {

    /// 持久化输入设置到 config.ini（成功静默，失败弹窗提示）
    pub(in crate::reactor_app) fn save_input(&mut self) {
        if let Err(e) = self.store.save_input(&self.input) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    /// 结束按键捕获：关闭弹窗、清空倒计时，代数 +1 使在途的捕获结果作废（避免重复提示）
    pub(in crate::reactor_app) fn end_capture(&mut self) {
        self.input_capture_gen += 1;
        self.dialog = Dialog::None;
        self.input_capture_start = None;
        self.input_capture_remain = 0.0;
    }

    /// 排下一拍倒计时（后台 sleep 后回灌 UI 线程）
    fn spawn_capture_tick(context: &ComponentContext<Self>, epoch: u32) {
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(INPUT_CAPTURE_TICK_MS));
            Message::InputCaptureTick(epoch)
        });
    }

    /// 按当前输入类型在后台重新扫描设备（xinput 枚举手柄槽位，dinput 枚举 HID 控制器）。
    /// 未选择输入类型时不扫描，界面会提示先选类型。
    pub(in crate::reactor_app) fn refresh_input_devices(&mut self, context: &ComponentContext<Self>) {
        self.input_devices_scanned = false;
        let kind = self.input.kind.clone();
        context.spawn_background(move |_| {
            Message::InputDevicesScanned(Box::new(rawinput::list_input_devices(kind.as_deref())))
        });
    }

    /// 输入设置页：输入类型（xinput/dinput）；dinput 时选择设备并捕获按键绑定。
    /// 全部保存到启动器自己的 config.ini（[input] 节），由加载器读取。
    pub(in crate::reactor_app) fn input_page(&self, context: &mut ViewContext<Self>) -> View {
        let cfg = &self.input;
        let dinput = cfg.kind.as_deref() == Some("dinput");
        let xinput = cfg.kind.as_deref() == Some("xinput");

        let kind_items: Vec<String> = INPUT_KINDS.iter().map(|v| v.to_string()).collect();
        let kind_idx = INPUT_KINDS
            .iter()
            .position(|v| Some(*v) == cfg.kind.as_deref());

        // 设备下拉框：快照标签 + 已保存值对应的下标（category：0=方向盘 1=踏板 2=排挡 3=XInput 手柄）。
        // dinput 保存的是设备名（label）；XInput 没有设备名，保存的是槽位索引（path 存 "0"~"3"），
        // 因此 xinput 行须按 path 匹配回显，否则选中值永远对不上（设备看起来无法被选中）。
        let device_row = |icon: Symbol,
                          title: &str,
                          saved: &Option<String>,
                          by_path: bool,
                          category: usize|
         -> View {
            let items: Vec<String> = self.input_devices.iter().map(|d| d.label.clone()).collect();
            let selected = saved.as_ref().and_then(|p| {
                self.input_devices
                    .iter()
                    .position(|d| if by_path { &d.path == p } else { &d.label == p })
            });
            let placeholder = if !self.input_devices_scanned {
                t!("正在扫描设备…", "Scanning devices...").to_string()
            } else if self.input.kind.is_none() {
                t!("请先选择输入类型", "Select an input type first").to_string()
            } else if self.input_devices.is_empty() {
                t!("未发现设备，点击右上角「扫描设备」", "No device found; click \"Scan\" at top right").to_string()
            } else if selected.is_some() {
                String::new()
            } else if saved.is_some() {
                t!("之前保存的设备当前不可用", "Previously saved device unavailable").to_string()
            } else {
                t!("选择设备", "Select device").to_string()
            };
            settings_row(
                SymbolIcon::new().symbol(icon).into(),
                title,
                "",
                ComboBox::new()
                    .width(450.0)
                    .items_source(items)
                    .selected_index(selected)
                    .placeholder_text(placeholder)
                    .on_selection_changed(
                        context.callback(move |i| Message::InputDeviceSelected(category, i)),
                    )
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            )
        };

        // 按键配置行：Windows 设置风格（标题 + 下方副文本显示当前绑定，右侧「捕获」按钮）
        let button_row = |slot: usize| -> View {
            let (zh, en) = INPUT_BUTTONS[slot];
            // 当前绑定的可读描述：油门 / 刹车为轴，其余为按键（含十字键）
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
            let desc = current.unwrap_or_else(|| t!("未绑定", "Unbound").to_string());
            // 左侧：图标 +（标题 + 下方副文本：当前绑定）
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
                            TextBlock::new()
                                .text(desc)
                                .font_size(12.0)
                                .opacity(0.6),
                        )),
                ))
                .into();
            // 右侧：油门(0)/刹车(1) 在捕获按钮前加「反转轴值」勾选框
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
            Grid::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((left, right))
                .into()
        };

        // XInput 按钮名映射行：与 dinput 的 button_row 同构，但当前绑定显示 mice 按钮名，
        // 捕获时走 XInput 捕获（Message::InputCaptureStart(XINPUT_MAP_BASE + local)）。
        // slot_local 为 0..9（对应 XINPUT_BUTTON_SLOTS）。
        let xi_button_row = |slot_local: usize| -> View {
            let (zh, en, _key) = XINPUT_BUTTON_SLOTS[slot_local];
            // 当前绑定的可读描述：已捕获的 mice 按钮名，否则「未绑定」
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
            let desc = current.unwrap_or_else(|| t!("未绑定", "Unbound").to_string());
            // 左侧：图标 +（标题 + 下方副文本：当前绑定）
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
                            TextBlock::new()
                                .text(desc)
                                .font_size(12.0)
                                .opacity(0.6),
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
            Grid::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(Thickness::new(16.0, 12.0, 16.0, 12.0))
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((left, right))
                .into()
        };

        // ---- 基本设置：固定的 test / service / 投币键 + 输入模式 + 输入比率 ----
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
                    "",
                    fixed_key("F1"),
                ),
            ),
            KeyedView::new("in-sep-test", separator()),
            KeyedView::new(
                "in-service",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Manage).into(),
                    t!("service 键", "Service key"),
                    "",
                    fixed_key("F2"),
                ),
            ),
            KeyedView::new("in-sep-service", separator()),
            KeyedView::new(
                "in-coin",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Keyboard).into(),
                    t!("投币键", "Coin key"),
                    "",
                    fixed_key("F3"),
                ),
            ),
            KeyedView::new("in-sep-coin", separator()),
            KeyedView::new(
                "in-kind",
                settings_row(
                    SymbolIcon::new().symbol(Symbol::Manage).into(),
                    t!("输入模式", "Input mode"),
                    "",
                    ComboBox::new()
                        // 原宽度 450 偏长，缩短 60%
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
                    "",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children({
                            // 显式标注类型：元组里连续 .into() 会推导不出目标类型
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
                                // E7A7 = Undo（逆时针箭头）；E72C 是顺时针的 Refresh，不适合“恢复默认”
                                .content(FontIcon::new().glyph("\u{E7A7}"))
                                .tooltip(tf!("恢复默认 {}", "Reset to default {}", INPUT_RATIO_DEFAULT))
                                .into();
                            (slider, text, reset)
                        })
                        .into(),
                ),
            ),
        ];

        // ---- 力反馈：独立配置块（基本设置下方、设备与按键上方）；开关式，默认开启，可下拉展开参数 ----
        let ffb_enable = cfg.ffb_enable == Some(true) || cfg.ffb_enable.is_none();
        let ffb_title = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(14.0)
            .children((
                SymbolIcon::new().symbol(Symbol::Rotate),
                StackPanel::new().spacing(2.0).children((
                    TextBlock::new()
                        .text(t!("力反馈", "Force feedback"))
                        .font_size(14.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new()
                        .text(t!("方向盘力反馈强度与手感", "Wheel force feedback strength and feel"))
                        .font_size(12.0)
                        .opacity(0.6),
                )),
            ));
        // 标题区可点击以展开/收起；力反馈开关直接放在此行右侧，不再单独占一行
        // （背景用透明而非 CardBackground：深色模式下透明主题刷会与外层卡片叠加变亮，
        //   导致卡片头部与内容区呈两种背景色；transparent 仍参与命中测试，热区不变）
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
        // 折叠箭头也作为展开/收起触发区（点击开关以外的区域都可展开）
        let ffb_chevron = Border::new()
            .background(Color::transparent())
            .on_pointer_pressed(context.callback(move |_| Message::FfbExpanderToggled))
            .grid_column(2)
            .content(
                FontIcon::new()
                    .glyph(if self.ffb_expanded {
                        "\u{E70D}" // ChevronUp
                    } else {
                        "\u{E76C}" // ChevronDown
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
        // 展开过渡期间内容行保持挂载（淡入/淡出都由整体透明度驱动），进度为 0 才卸载
        if self.ffb_anim > 0.0 || self.ffb_expanded {
            let eased = {
                let t = self.ffb_anim;
                t * (2.0 - t)
            };
            // 滑块行闭包：置于展开块内，避免对 context 的长借用跨过下方的设备/按键行
            let ffb_slider_row = |idx: usize, icon: Symbol, title: &str, desc: &str, value: f64| -> View {
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
                    desc,
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((slider, text))
                        .into(),
                )
            };
            let ffb_detail: Vec<KeyedView> = vec![
                KeyedView::new("ffb-cf", ffb_slider_row(0, Symbol::Directions, t!("恒力强度", "Constant force"), t!("恒力强度", "Constant force strength"), cfg.ffb_constant_force.unwrap_or(FFB_CONSTANT_FORCE_DEFAULT) as f64)),
                KeyedView::new("ffb-sep1", separator()),
                KeyedView::new("ffb-damper", ffb_slider_row(1, Symbol::Directions, t!("阻尼强度", "Damper"), t!("方向盘阻尼效果", "Steering wheel damper"), cfg.ffb_damper.unwrap_or(FFB_DAMPER_DEFAULT) as f64)),
                KeyedView::new("ffb-sep2", separator()),
                KeyedView::new("ffb-rumble", ffb_slider_row(2, Symbol::Directions, t!("震动强度", "Rumble"), t!("震动强度", "Rumble strength"), cfg.ffb_rumble.unwrap_or(FFB_RUMBLE_DEFAULT) as f64)),
                KeyedView::new("ffb-sep3", separator()),
                KeyedView::new("ffb-dur", ffb_slider_row(3, Symbol::Directions, t!("震动时长", "Rumble duration"), t!("震动时长系数（ms→µs）", "Rumble duration factor (ms→µs)"), cfg.ffb_rumble_duration.unwrap_or(FFB_RUMBLE_DURATION_DEFAULT) as f64)),
                KeyedView::new("ffb-sep4", separator()),
                KeyedView::new("ffb-basedamper", ffb_slider_row(4, Symbol::Directions, t!("基础阻尼", "Base damper"), t!("方向盘最小重量/刚度", "Minimum weight/stiffness"), cfg.ffb_base_damper.unwrap_or(FFB_BASE_DAMPER_DEFAULT) as f64)),
                KeyedView::new("ffb-sep5", separator()),
                KeyedView::new("ffb-deadband", ffb_slider_row(5, Symbol::Directions, t!("中心死区", "Dead band"), t!("方向盘中心无作用力区间", "No-force zone at center"), cfg.ffb_dead_band.unwrap_or(FFB_DEAD_BAND_DEFAULT) as f64)),
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

        // ---- 选定输入模式后，才显示具体的设备与按键设置 ----
        let mut sections: Vec<KeyedView> = vec![
            KeyedView::new("sec-basic", settings_section(t!("基本设置", "Basic"), basic_rows)),
            KeyedView::new("sec-ffb", ffb_card),
        ];
        if xinput || dinput {
            let mut detail: Vec<KeyedView> = Vec::new();
            if xinput {
                // XInput 没有设备路径，只有槽位索引：单独存 pad_index，不占用 wheel_device。
                // pad_index 存的是真实槽位（0~3），回显按 path 匹配（by_path = true）。
                let pad_saved = cfg.pad_index.map(|i| i.to_string());
                detail.push(KeyedView::new(
                    "in-pad",
                    device_row(
                        Symbol::Directions,
                        t!("手柄设备", "Gamepad"),
                        &pad_saved,
                        true,
                        3,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-pad", separator()));
                // ---- XInput 专属：双摇杆 / 线性方向 / 左右摇杆死区 ----
                detail.push(KeyedView::new(
                    "in-dual-stick",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Directions).into(),
                        t!("双摇杆模式", "Dual stick mode"),
                        "",
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
                        "",
                        settings_toggle(
                            // xinput 模式下未设置（None）时默认开启；用户显式关闭（Some(false)）仍生效
                            cfg.linear_steering != Some(false),
                            true,
                            context.callback(Message::InputLinearSteeringToggled),
                        ),
                    ),
                ));
                detail.push(KeyedView::new("in-sep-linear", separator()));
                // 死区滑块：0~100%，默认 20%，渲染为 kitten 的 left/rightStickDeadzone = 32767 × 百分比
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
                        "",
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
                // ---- XInput 专属：按钮名映射（参考 dinput 的捕获方式，捕获手柄按键并记录 mice 按钮名）----
                detail.push(KeyedView::new(
                    "in-xi-head",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Keyboard).into(),
                        t!("按键映射", "Button mapping"),
                        t!("捕获 XInput 手柄按键，记录为 mice 按钮名", "Capture XInput gamepad buttons; recorded as mice button names"),
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
                        xi_button_row(local),
                    ));
                }
            }
            if dinput {
                detail.push(KeyedView::new(
                    "in-wheel",
                    device_row(
                        Symbol::Directions,
                        t!("方向盘设备", "Steering wheel"),
                        &cfg.wheel_device,
                        false,
                        0,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-w", separator()));
                detail.push(KeyedView::new(
                    "in-pedal",
                    device_row(
                        Symbol::Up,
                        t!("油门踏板设备", "Throttle pedal"),
                        &cfg.pedal_device,
                        false,
                        1,
                    ),
                ));
                detail.push(KeyedView::new("in-sep-p", separator()));
                detail.push(KeyedView::new(
                    "in-shifter",
                    device_row(
                        Symbol::Switch,
                        t!("排挡设备", "Shifter"),
                        &cfg.shifter_device,
                        false,
                        2,
                    ),
                ));
                // 油门 / 刹车：排在「排挡类型」之前
                for slot in INPUT_BUTTONS_HEAD {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), button_row(*slot)));
                }
                // 排挡类型：选完才显示对应的挡位按键
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
                        "",
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
                // 挡位按键紧跟在「排挡类型」之后：序列的升/降档，或 H 排挡的 1~6 挡与倒挡
                let shifter_slots: &[usize] = match cfg.shifter_type.as_deref() {
                    Some("sequential") => INPUT_BUTTONS_SEQ,
                    Some("h") => INPUT_BUTTONS_H,
                    _ => &[],
                };
                for slot in shifter_slots {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), button_row(*slot)));
                }
                // 其余按键行（方向四键 / start / view / 插卡）
                for slot in INPUT_BUTTONS_TAIL {
                    detail.push(KeyedView::new(format!("in-sep-b{slot}"), separator()));
                    detail.push(KeyedView::new(format!("in-btn-{slot}"), button_row(*slot)));
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
                        Grid::new()
                            .columns([GridLength::Star(1.0), GridLength::Auto])
                            .children((
                                StackPanel::new()
                                    .spacing(4.0)
                                    .grid_column(0)
                                    .children((
                                        TextBlock::new()
                                            .text(t!("输入设置", "Input"))
                                            .font_size(26.0)
                                            .font_weight(FontWeight::SEMI_BOLD),
                                        TextBlock::new()
                                            .text(t!("", ""))
                                            .font_size(12.0)
                                            .opacity(0.6),
                                    )),
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
                                                FontIcon::new().glyph("\u{E72C}"), // Refresh
                                                TextBlock::new().text(t!("扫描设备", "Scan")),
                                            )),
                                    ),
                            )),
                        StackPanel::new()
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .keyed_children(sections),
                    )),
            )
            .into()
    }

    /// 展开/收起力反馈卡片，并启动过渡动画
    pub(in crate::reactor_app) fn toggle_ffb_expander(&mut self, context: &ComponentContext<Self>) {
        self.ffb_expanded = !self.ffb_expanded;
        // 启动展开/收起过渡动画：每 16ms 推进一步，约 200ms 完成
        self.ffb_anim_gen += 1;
        let (epoch, step) = (self.ffb_anim_gen, 0.016 / 0.2);
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(16));
            Message::FfbAnimTick(epoch, step)
        });
    }

    /// 力反馈展开/收起动画推进（过期代数作废）
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
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(16));
                Message::FfbAnimTick(epoch, step)
            });
        }
    }

    /// 设备下拉选择回灌：dinput 记设备名，xinput 记真实槽位索引
    pub(in crate::reactor_app) fn apply_device_selected(&mut self, slot: usize, index: usize) {
        if let Some(d) = self.input_devices.get(index) {
            let name = d.label.clone();
            match slot {
                // idas.ini 的 deviceName / pedalsName / shifterName 要的是设备名（产品名），不是 HID 路径
                0 => self.input.wheel_device = Some(name.clone()),
                1 => self.input.pedal_device = Some(name.clone()),
                2 => self.input.shifter_device = Some(name.clone()),
                // XInput 没有设备路径：GameDevice.path 存的就是真实槽位索引（"0"~"3"）。
                // 必须存它而非列表位置——部分手柄未连接时列表位置 ≠ 槽位索引
                _ => self.input.pad_index = d.path.parse::<u32>().ok(),
            }
            self.save_input();
        }
    }

    /// 开始捕获指定按键槽位：作废旧捕获、弹窗并启动倒计时与后台捕获线程
    pub(in crate::reactor_app) fn start_capture(&mut self, slot: usize, context: &ComponentContext<Self>) {
        // XInput 按钮名映射槽位（slot >= XINPUT_MAP_BASE）：label 取自 XINPUT_BUTTON_SLOTS，
        // 后台轮询 XInputGetState 捕获手柄按键并映射为 mice 按钮名。
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
        // 作废旧捕获并弹窗，后台线程阻塞等待设备按键（最长 10 秒）
        self.input_capture_gen += 1;
        let epoch = self.input_capture_gen;
        self.dialog = Dialog::InputCapture {
            slot,
            label: i18n::pick(zh, en),
        };
        // 启动 10 秒倒计时：进度条满格 → 0
        self.input_capture_start = Some(Instant::now());
        self.input_capture_remain = 1.0;
        Self::spawn_capture_tick(context, epoch);
        context.spawn_background(move |_| {
            if slot == 0 || slot == 1 {
                // 油门 / 刹车：捕获模拟轴（记录轴名），写入 idas.ini 的 accelAxis / brakeAxis
                match rawinput::capture_axis((INPUT_CAPTURE_SECS * 1000.0) as u32) {
                    Some(c) => Message::InputCapturedAxis(epoch, slot, c.name),
                    None => Message::InputCaptureTimeout(epoch, slot),
                }
            } else {
                match rawinput::capture_button((INPUT_CAPTURE_SECS * 1000.0) as u32) {
                    Some(c) => {
                        // 十字键落库为 1001~1004，按钮仍用原始编号
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

    /// 捕获倒计时推进：驱动进度条，归零等同超时自动关闭
    pub(in crate::reactor_app) fn tick_capture(&mut self, epoch: u32, context: &ComponentContext<Self>) {
        // 代数过期或已不在捕获弹窗：停止倒计时
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
            // 倒计时归零：等同超时，自动关闭弹窗
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

    /// 按键捕获成功：写入对应槽位并关闭弹窗
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
        // 收到按键：立即关闭弹窗（配置行会显示「已绑定：按钮 N」）
        self.end_capture();
    }

    /// XInput 按钮名映射捕获成功：写入对应槽位的 mice 按钮名并关闭弹窗
    pub(in crate::reactor_app) fn apply_captured_xi_button(&mut self, epoch: u32, slot: usize, name: String) {
        if epoch != self.input_capture_gen {
            return;
        }
        // 仅当确为 XInput 映射槽位才处理
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
        // 收到按键：立即关闭弹窗（配置行会显示捕获到的按钮名）
        self.end_capture();
    }

    /// 模拟轴捕获成功：写入油门/刹车轴并关闭弹窗
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
        // 收到轴：立即关闭弹窗（配置行会显示轴名）
        self.end_capture();
    }

}
