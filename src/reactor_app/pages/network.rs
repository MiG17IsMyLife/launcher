use std::sync::atomic::Ordering;
use std::time::Duration;

use windows_reactor::*;

use crate::netinfo::{self, AdapterInfo};
use crate::t;
use crate::tf;
use crate::vpn::{VpnManager, VpnStatus};

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle};

pub(in crate::reactor_app) const VPN_AUTO_RETRIES: u32 = 3;

pub(in crate::reactor_app) const VPN_STABLE_RESET: Duration = Duration::from_secs(300);

const VPN_WATCH_INTERVAL: Duration = Duration::from_secs(2);

impl Launcher {

    pub(in crate::reactor_app) fn selected_adapter(&self) -> Option<&AdapterInfo> {
        let guid = self.net.adapter.as_deref()?;
        self.adapters.iter().find(|a| a.guid == guid)
    }

    pub(in crate::reactor_app) fn awl_adapter_selected(&self) -> bool {
        self.selected_adapter()
            .is_some_and(|a| a.friendly == crate::awl::AWL_IFACE_NAME)
    }

    pub(in crate::reactor_app) fn save_net(&mut self) {
        if let Err(e) = self.store.save_network(&self.net) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    pub(in crate::reactor_app) fn network_page(&self, context: &mut ViewContext<Self>) -> View {
        let selected = self.selected_adapter();

        let adapter_items: Vec<String> = self.adapters.iter().map(|a| a.label()).collect();
        let adapter_idx =
            selected.and_then(|sel| self.adapters.iter().position(|a| a.guid == sel.guid));
        let adapter_placeholder = if !self.adapters_scanned {
            t!("正在扫描网卡…", "Scanning adapters...").to_string()
        } else if self.adapters.is_empty() {
            t!("没有已连接的网卡", "No connected adapter").to_string()
        } else if adapter_idx.is_some() {
            String::new()
        } else if self.net.adapter.is_some() {
            t!("之前保存的网卡当前不可用", "Previously saved adapter unavailable").to_string()
        } else {
            t!("选择网卡", "Select adapter").to_string()
        };

        let (ip_items, ip_idx, ip_enabled, ip_placeholder) = match selected {
            None => (Vec::new(), None, false, t!("先选择网卡", "Select an adapter first").to_string()),
            Some(a) if a.ips.is_empty() => {
                (Vec::new(), None, false, t!("该网卡没有 IPv4 网段", "No IPv4 subnet on this adapter").to_string())
            }
            Some(a) => {
                let items = netinfo::subnet_labels(&a.ips);
                let idx = self
                    .net
                    .ip
                    .as_ref()
                    .and_then(|saved| items.iter().position(|c| c == saved));
                let placeholder = if self.net.ip.is_some() && idx.is_none() {
                    t!("之前保存的网段当前不可用", "Previously saved subnet unavailable").to_string()
                } else {
                    t!("选择网段", "Select subnet").to_string()
                };
                (items, idx, true, placeholder)
            }
        };

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
                        FontIcon::new().glyph("\u{E939}").into(),
                        t!("网卡", "Network adapter"),
                        ComboBox::new()
                            .width(450.0)
                            .items_source(adapter_items)
                            .selected_index(adapter_idx)
                            .placeholder_text(adapter_placeholder)
                            .on_selection_changed(context.callback(Message::AdapterSelected))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        FontIcon::new().glyph("\u{E774}").into(),
                        t!("IP 网段", "IP subnet"),
                        ComboBox::new()
                            .width(450.0)
                            .items_source(ip_items)
                            .selected_index(ip_idx)
                            .placeholder_text(ip_placeholder)
                            .is_enabled(ip_enabled)
                            .on_selection_changed(context.callback(Message::IpSelected))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        FontIcon::new().glyph("\u{E9E9}").into(),
                        t!("模拟网络", "Simulated network"),
                        settings_toggle(
                            self.net.simulate != Some(false),
                            true,
                            context.callback(Message::NetworkSimulateToggled),
                        ),
                    ),
                    separator(),
                    settings_row(
                        FontIcon::new().glyph("\u{E80F}").into(),
                        t!("主机IP", "Host IP"),
                        TextBox::new()
                            .width(180.0)
                            .text(self.net.host_ip.clone().unwrap_or_default())
                            .placeholder_text("192.168.100.50")
                            .is_enabled(!self.awl_adapter_selected())
                            .on_text_changed(context.callback(Message::HostIpChanged))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                    separator(),
                    settings_row(
                        FontIcon::new().glyph("\u{E8D8}").into(),
                        t!("ServerBox IP", "ServerBox IP"),
                        TextBox::new()
                            .width(180.0)
                            .text(self.net.serverbox_ip.clone().unwrap_or_default())
                            .placeholder_text("192.168.100.101")
                            .is_enabled(self.net.simulate == Some(false) && !self.awl_adapter_selected())
                            .on_text_changed(context.callback(Message::ServerboxIpChanged))
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                )),
            );

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
                                    .grid_column(0)
                                    .children([TextBlock::new()
                                        .text(t!("网络设置", "Network"))
                                        .font_size(26.0)
                                        .font_weight(FontWeight::SEMI_BOLD)]),
                                Button::new()
                                    .style(ButtonStyle::Subtle)
                                    .on_click(context.message(Message::RefreshAdapters))
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .grid_column(1)
                                    .content(
                                        StackPanel::new()
                                            .orientation(Orientation::Horizontal)
                                            .spacing(8.0)
                                            .children((
                                                FontIcon::new().glyph("\u{E72C}"),
                                                TextBlock::new().text(t!("刷新网卡", "Refresh")),
                                            )),
                                    ),
                            )),
                        card,
                    )),
            )
            .into()
    }

    pub(in crate::reactor_app) fn vpn_page(&self, context: &mut ViewContext<Self>) -> View {
        use std::path::Path;

        let (status_text, sr, sg, sb) = match self.vpn_status {
            VpnStatus::NotConfigured => (t!("未配置", "Not configured"), 0x9E, 0x9E, 0x9E),
            VpnStatus::Disconnected => (t!("未连接", "Disconnected"), 0xFF, 0x99, 0xA4),
            VpnStatus::Connecting => (t!("连接中…", "Connecting…"), 0x9E, 0x9E, 0x9E),
            VpnStatus::Connected => (t!("已连接", "Connected"), 0x6C, 0xCB, 0x5F),
            VpnStatus::Failed => (t!("连接失败", "Connection failed"), 0xFF, 0x99, 0xA4),
        };
        let cfg_name = self
            .vpn
            .config_path
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| t!("未导入", "Not imported").to_string());
        let connected = self.vpn_status == VpnStatus::Connected;
        let can_connect = self.vpn.config_path.is_some() && !connected;

        let connect_btn = Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(can_connect)
            .on_click(context.message(Message::VpnConnectClicked))
            .content(t!("连接", "Connect"));
        let disconnect_btn = Button::new()
            .is_enabled(connected)
            .on_click(context.message(Message::VpnDisconnectClicked))
            .content(t!("断开", "Disconnect"));

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
                        View::from(SymbolIcon::new().symbol(Symbol::MapDrive)),
                        t!("配置文件", "Config file"),
                        View::from(
                            Button::new()
                                .on_click(context.message(Message::VpnImportClicked))
                                .content(t!("导入配置…", "Import config…")),
                        ),
                    ),
                    separator(),
                    settings_row(
                        View::from(SymbolIcon::new().symbol(Symbol::ProtectedDocument)),
                        t!("当前配置", "Current config"),
                        View::from(
                            TextBlock::new()
                                .text(cfg_name)
                                .opacity(0.8)
                                .vertical_alignment(VerticalAlignment::Center),
                        ),
                    ),
                    separator(),
                    settings_row(
                        View::from(SymbolIcon::new().symbol(Symbol::Permissions)),
                        t!("私钥密码", "Private key password"),
                        View::from(
                            PasswordBox::new()
                                .width(168.0)
                                .placeholder_text(t!("私钥密码", "Private key password"))
                                .password(self.vpn.passphrase.clone().unwrap_or_default())
                                .on_password_changed(context.callback(Message::VpnPassphraseChanged))
                                .vertical_alignment(VerticalAlignment::Center),
                        ),
                    ),
                    separator(),
                    settings_row(
                        View::from(SymbolIcon::new().symbol(Symbol::World)),
                        t!("连接状态", "Connection status"),
                        View::from(
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .children((
                                    TextBlock::new()
                                        .text(format!("● {status_text}"))
                                        .foreground(Brush::Solid(Color::argb(255, sr, sg, sb)))
                                        .vertical_alignment(VerticalAlignment::Center),
                                    connect_btn,
                                    disconnect_btn,
                                )),
                        ),
                    ),
                )),
            );

        StackPanel::new()
            .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
            .spacing(8.0)
            .children((
                TextBlock::new()
                    .text(t!("VPN 设置", "VPN"))
                    .font_size(26.0)
                    .font_weight(FontWeight::SEMI_BOLD),
                card,
            ))
            .into()
    }

    pub(in crate::reactor_app) fn start_vpn_connect(&mut self, context: &ComponentContext<Self>) {
        if self.vpn.config_path.is_none() {
            self.vpn_status = VpnStatus::NotConfigured;
            self.dialog = Dialog::Error {
                message: t!("请先导入 VPN 配置文件", "Import a VPN config file first").to_string(),
            };
            return;
        }

        if self.vpn_status == VpnStatus::Connected || self.vpn_status == VpnStatus::Connecting {
            return;
        }

        self.vpn_auto_retries = 0;
        self.spawn_vpn_connect_task(context);
    }

    pub(in crate::reactor_app) fn spawn_vpn_connect_task(&mut self, context: &ComponentContext<Self>) {
        self.vpn_status = VpnStatus::Connecting;
        let cfg = self.vpn.clone();
        let handle = self.vpn_handle.clone();

        let gens = self.vpn_watch_gen.clone();
        let epoch = self.vpn_watch_gen.fetch_add(1, Ordering::Relaxed) + 1;
        context.spawn_background(move |_| {
            match VpnManager::connect_blocking(
                &cfg,
                cfg.passphrase.as_deref(),
                Duration::from_secs(25),
            ) {
                Ok(h) => {
                    if gens.load(Ordering::Relaxed) != epoch {
                        drop(h);
                        return Message::VpnDied(None);
                    }
                    *handle.lock().unwrap() = Some(h);
                    Message::VpnConnectResult(None)
                }
                Err(e) => {
                    if gens.load(Ordering::Relaxed) != epoch {
                        return Message::VpnDied(None);
                    }
                    Message::VpnConnectResult(Some(e))
                }
            }
        });
    }

    pub(in crate::reactor_app) fn spawn_vpn_watch(&self, context: &ComponentContext<Self>) {
        let epoch = self.vpn_watch_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = self.vpn_handle.clone();
        let gens = self.vpn_watch_gen.clone();
        context.spawn_background(move |_| {
            loop {
                std::thread::sleep(VPN_WATCH_INTERVAL);

                if gens.load(Ordering::Relaxed) != epoch {
                    return Message::VpnDied(None);
                }
                let reason = {
                    let mut guard = match slot.lock() {
                        Ok(g) => g,
                        Err(_) => return Message::VpnDied(None),
                    };
                    match guard.as_mut() {

                        None => return Message::VpnDied(None),
                        Some(h) => {
                            if h.is_alive() {
                                continue;
                            }
                            Some(h.exit_detail())
                        }
                    }
                };

                return Message::VpnDied(reason);
            }
        });
    }

}
