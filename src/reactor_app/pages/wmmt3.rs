//! The WMMT3 parts of the interface: the Input page's tab for Wangan Midnight Maximum Tune 3
//! and the WMMT3 settings page opened from a WMMT3 game's row. Both are drawn from the table
//! in `games::wmmt3::settings`, one row per setting, its control chosen by the setting's kind.

use windows_reactor::*;

use rust_i18n::t;

use crate::games::wmmt3::settings::{self, AXES, KEYS, Kind, Label, Section, Setting, XINPUT_BUTTONS};
use crate::rawinput;
use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{binding_label, separator, settings_row, settings_section, settings_toggle};

use super::input::INPUT_CAPTURE_SECS;

/// The game whose settings the Input page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::reactor_app) enum InputTab {
    #[default]
    InitialD,
    Wmmt3,
}

impl InputTab {
    const ALL: [InputTab; 2] = [InputTab::InitialD, InputTab::Wmmt3];

    fn title(self) -> String {
        match self {
            InputTab::InitialD => t!("wmmt3.tab_initial_d"),
            InputTab::Wmmt3 => t!("wmmt3.tab_wmmt3"),
        }
        .to_string()
    }
}

impl Launcher {
    /// Whether the library has a WMMT3 game; only then the Input page has tabs.
    pub(in crate::reactor_app) fn has_wmmt3_game(&self) -> bool {
        self.library.games.iter().any(crate::games::wmmt3::is_wmmt3_game)
    }

    /// The game whose input settings the Input page shows now.
    pub(in crate::reactor_app) fn shown_input_tab(&self) -> InputTab {
        if self.has_wmmt3_game() { self.input_tab } else { InputTab::InitialD }
    }

    /// The tabs of the Input page, when there is more than one game to set up.
    pub(in crate::reactor_app) fn input_tabs(&self, context: &mut ViewContext<Self>) -> Option<View> {
        if !self.has_wmmt3_game() {
            return None;
        }
        let items = InputTab::ALL.map(|tab| {
            KeyedView::new(
                format!("{tab:?}"),
                SelectorBarItem::new().text(tab.title()).is_selected(self.input_tab == tab),
            )
        });
        Some(
            SelectorBar::new()
                .on_selected_text_changed(context.callback(Message::InputTabSelected))
                .collection_slot(SelectorBarSlot::Items, items)
                .into(),
        )
    }

    /// The Input page's sections on the WMMT3 tab.
    pub(in crate::reactor_app) fn wmmt3_input_sections(&self, context: &mut ViewContext<Self>) -> Vec<KeyedView> {
        self.wmmt3_sections(context, settings::INPUT_SECTIONS)
    }

    /// The WMMT3 settings page, opened from a WMMT3 game's row.
    pub(in crate::reactor_app) fn wmmt3_settings_page(&self, context: &mut ViewContext<Self>) -> View {
        let header: View = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .children((
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Message::BackToGames))
                    .vertical_alignment(VerticalAlignment::Center)
                    .content(SymbolIcon::new().symbol(Symbol::Back)),
                TextBlock::new()
                    .text(t!("wmmt3.settings_title"))
                    .font_size(26.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center),
            ))
            .into();
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
                    .spacing(4.0)
                    .children((
                        header,
                        StackPanel::new()
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .keyed_children(self.wmmt3_sections(context, settings::PAGE_SECTIONS)),
                    )),
            )
            .into()
    }

    /// The sections that apply to the input mode in use, each with a row per setting.
    fn wmmt3_sections(&self, context: &mut ViewContext<Self>, sections: &'static [Section]) -> Vec<KeyedView> {
        let mode = self.wmmt3.input_mode().to_string();
        sections
            .iter()
            .filter(|section| section.input_mode.is_none_or(|m| m == mode))
            .map(|section| {
                let mut rows = Vec::new();
                for (i, setting) in section.settings.iter().enumerate() {
                    if i > 0 {
                        rows.push(KeyedView::new(format!("sep-{}", setting.key), separator()));
                    }
                    rows.push(KeyedView::new(setting.key, self.wmmt3_setting_row(context, setting)));
                }
                KeyedView::new(section.title, settings_section(&t!(section.title), rows))
            })
            .collect()
    }

    /// The row of one setting: its name, and the control its kind calls for.
    fn wmmt3_setting_row(&self, context: &mut ViewContext<Self>, setting: &'static Setting) -> View {
        let key = setting.key;
        let value = self.wmmt3.value(setting).to_string();
        let changed = move |v: Option<String>| Message::Wmmt3SettingChanged(key, v);

        let control: View = match &setting.kind {
            Kind::Toggle => settings_toggle(
                value == "true",
                true,
                context.callback(move |on: bool| changed(Some(on.to_string()))),
            ),
            Kind::Number { min, max } => number_box(context, &value, *min, *max, key),
            Kind::Choice(items) => {
                let labels = items.iter().map(|(_, label)| label_text(label)).collect();
                let values: Vec<&'static str> = items.iter().map(|(v, _)| *v).collect();
                choice_box(context, labels, values, &value, key)
            }
            Kind::Axis => {
                let labels = AXES.iter().map(|a| a.to_string()).collect();
                choice_box(context, labels, AXES.to_vec(), &value, key)
            }
            Kind::Key => {
                let labels = KEYS.iter().map(|(v, name)| if *v == "0x00" { unbound() } else { name.to_string() }).collect();
                let values: Vec<&'static str> = KEYS.iter().map(|(v, _)| *v).collect();
                choice_box(context, labels, values, &value, key)
            }
            Kind::XinputButton => {
                let mut values: Vec<&'static str> = XINPUT_BUTTONS.to_vec();
                values.push("none");
                let labels = values.iter().map(|v| if *v == "none" { unbound() } else { v.to_string() }).collect();
                let choice = choice_box(context, labels, values, &value, key);
                with_capture(context, choice, key)
            }
            Kind::DinputButton => {
                let shown: View = TextBlock::new()
                    .text(match value.parse::<u32>() {
                        Ok(0) | Err(_) => unbound(),
                        Ok(n) => binding_label(n),
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .opacity(0.7)
                    .into();
                let clear: View = Button::new()
                    .on_click(context.message(Message::Wmmt3SettingChanged(key, Some("0".to_string()))))
                    .content(t!("wmmt3.clear").to_string())
                    .into();
                let binding: View = StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(12.0)
                    .children((shown, clear))
                    .into();
                with_capture(context, binding, key)
            }
            Kind::Device => {
                let mut values: Vec<String> = vec![String::new()];
                values.extend(self.wmmt3_devices.iter().map(|d| d.name.clone()));
                if !values.contains(&value) {
                    values.push(value.clone());
                }
                let labels: Vec<String> = values
                    .iter()
                    .map(|v| if v.is_empty() { t!("wmmt3.device_default").to_string() } else { v.clone() })
                    .collect();
                let selected = values.iter().position(|v| *v == value);
                ComboBox::new()
                    .width(260.0)
                    .items_source(labels)
                    .selected_index(selected)
                    .on_selection_changed(context.callback(move |i: Option<usize>| {
                        changed(i.and_then(|i| values.get(i).cloned()))
                    }))
                    .vertical_alignment(VerticalAlignment::Center)
                    .into()
            }
            Kind::Text => TextBox::new()
                .text(value)
                .on_text_changed(context.callback(move |text: String| changed(Some(text))))
                .min_width(220.0)
                .into(),
        };
        settings_row(SymbolIcon::new().symbol(Symbol::Setting).into(), &t!(setting.label), control)
    }

    /// Handles the messages of the WMMT3 tab and page; hands the others back.
    pub(in crate::reactor_app) fn handle_wmmt3(&mut self, message: Message, context: &ComponentContext<Self>) -> Option<Message> {
        match message {
            Message::InputTabSelected(Some(title)) => {
                if let Some(tab) = InputTab::ALL.into_iter().find(|tab| tab.title() == title) {
                    self.input_tab = tab;
                    if tab == InputTab::Wmmt3 {
                        self.scan_wmmt3_devices(context);
                    }
                }
            }
            Message::InputTabSelected(None) => {}
            Message::Wmmt3SettingChanged(key, Some(value)) => self.set_wmmt3(key, value),
            Message::Wmmt3SettingChanged(_, None) => {}
            Message::Wmmt3CaptureStart(key) => self.start_wmmt3_capture(key, context),
            Message::Wmmt3Captured(epoch, key, value) => {
                if epoch == self.input_capture_gen {
                    self.end_capture();
                    self.set_wmmt3(key, value);
                }
            }
            Message::Wmmt3CapturedDpad(epoch) => {
                if epoch == self.input_capture_gen {
                    self.end_capture();
                    self.dialog = Dialog::Error { message: t!("wmmt3.dpad_not_assignable").to_string() };
                }
            }
            Message::Wmmt3DevicesScanned(devices) => self.wmmt3_devices = *devices,
            other => return Some(other),
        }
        None
    }

    /// Looks for DirectInput devices to offer on the WMMT3 tab.
    pub(in crate::reactor_app) fn scan_wmmt3_devices(&self, context: &ComponentContext<Self>) {
        context.spawn_background(|_| Message::Wmmt3DevicesScanned(Box::new(rawinput::list_game_devices())));
    }

    /// Saves a WMMT3 setting, unless it already has that value.
    fn set_wmmt3(&mut self, key: &str, value: String) {
        let Some(setting) = settings::find(key) else {
            return;
        };
        if self.wmmt3.value(setting) == value {
            return;
        }
        if let Err(e) = self.wmmt3.set(&self.store.config_path(), setting, value) {
            self.dialog = Dialog::Error {
                message: t!("common.failed_to_save_config_ini", error = e).to_string(),
            };
        }
    }

    /// Waits for a button on the device of the input mode in use and binds it to `key`.
    fn start_wmmt3_capture(&mut self, key: &'static str, context: &ComponentContext<Self>) {
        let Some(setting) = settings::find(key) else {
            return;
        };
        let xinput = matches!(setting.kind, Kind::XinputButton);
        let pad = settings::find("xinput_device")
            .and_then(|s| self.wmmt3.value(s).parse::<u32>().ok())
            .unwrap_or(0);

        self.input_capture_gen += 1;
        let epoch = self.input_capture_gen;
        self.dialog = Dialog::InputCapture { label_key: setting.label };
        self.input_capture_start = Some(std::time::Instant::now());
        self.input_capture_remain = 1.0;
        Self::spawn_capture_tick(context, epoch);

        let timeout_ms = (INPUT_CAPTURE_SECS * 1000.0) as u32;
        context.spawn_background(move |_| {
            if xinput {
                return match rawinput::capture_xi_button(pad, timeout_ms) {
                    Some(c) => Message::Wmmt3Captured(epoch, key, c.name),
                    None => Message::InputCaptureTimeout(epoch),
                };
            }
            match rawinput::capture_button(timeout_ms) {
                // KittenTools moves the lever with the wheel's POV hat by itself; a hat
                // direction is not a button it can bind.
                Some(c) if c.is_dpad => Message::Wmmt3CapturedDpad(epoch),
                Some(c) => Message::Wmmt3Captured(epoch, key, c.button.to_string()),
                None => Message::InputCaptureTimeout(epoch),
            }
        });
    }
}

/// The text of a choice.
fn label_text(label: &Label) -> String {
    match label {
        Label::Plain(text) => text.to_string(),
        Label::Text(key) => t!(*key).to_string(),
    }
}

/// The text for "no binding".
fn unbound() -> String {
    t!("page_input.unbound").to_string()
}

/// A drop-down of `labels`, standing for `values`, with `value` selected.
fn choice_box(
    context: &mut ViewContext<Launcher>,
    labels: Vec<String>,
    values: Vec<&'static str>,
    value: &str,
    key: &'static str,
) -> View {
    let selected = values.iter().position(|v| v.eq_ignore_ascii_case(value));
    ComboBox::new()
        .width(180.0)
        .items_source(labels)
        .selected_index(selected)
        .on_selection_changed(context.callback(move |i: Option<usize>| {
            Message::Wmmt3SettingChanged(key, i.and_then(|i| values.get(i)).map(|v| v.to_string()))
        }))
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

/// A whole number from `min` to `max`.
fn number_box(context: &mut ViewContext<Launcher>, value: &str, min: i64, max: i64, key: &'static str) -> View {
    NumberBox::new()
        .minimum(min as f64)
        .maximum(max as f64)
        .value(value.parse::<f64>().ok())
        .on_value_changed(context.callback(move |v: Option<f64>| {
            Message::Wmmt3SettingChanged(key, v.map(|v| (v.round() as i64).clamp(min, max).to_string()))
        }))
        .into()
}

/// `control` followed by a button that captures the binding from the device.
fn with_capture(context: &mut ViewContext<Launcher>, control: View, key: &'static str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(12.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            control,
            Button::new()
                .on_click(context.message(Message::Wmmt3CaptureStart(key)))
                .content(t!("page_input.capture").to_string()),
        ))
        .into()
}
