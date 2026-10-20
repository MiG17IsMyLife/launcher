//! 网络设置页与 VPN 设置页（含手动连接的后台启动逻辑、连接后进程监督）。
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

/// openvpn 意外退出后的自动重连次数上限（超过后停在「连接失败」待手动重连）
pub(in crate::reactor_app) const VPN_AUTO_RETRIES: u32 = 3;
/// 本轮连接稳定运行超过该时长后，自动重连预算重新计满
/// （防「连上又立刻死」的抖动隧道借稳定判定无限循环重试）
pub(in crate::reactor_app) const VPN_STABLE_RESET: Duration = Duration::from_secs(300);
/// VPN 监督线程轮询 openvpn 进程的间隔
const VPN_WATCH_INTERVAL: Duration = Duration::from_secs(2);

impl Launcher {

    /// 当前选中的网卡（config.ini 里保存的 GUID 与本次快照匹配）
    pub(in crate::reactor_app) fn selected_adapter(&self) -> Option<&AdapterInfo> {
        let guid = self.net.adapter.as_deref()?;
        self.adapters.iter().find(|a| a.guid == guid)
    }

    /// 当前选中网卡是否为 awl 虚拟网卡（店内对战）：是则主机IP / ServerBox IP 锁定
    pub(in crate::reactor_app) fn awl_adapter_selected(&self) -> bool {
        self.selected_adapter()
            .is_some_and(|a| a.friendly == crate::awl::AWL_IFACE_NAME)
    }

    /// 持久化网络设置到 config.ini（成功静默，失败弹窗提示）
    pub(in crate::reactor_app) fn save_net(&mut self) {
        if let Err(e) = self.store.save_network(&self.net) {
            self.dialog = Dialog::Error {
                message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
            };
        }
    }

    /// 网络设置页（Win11 设置风格：大标题 + 卡片分组 + 行内左文案右控件 + InfoBar）
    pub(in crate::reactor_app) fn network_page(&self, context: &mut ViewContext<Self>) -> View {
        let selected = self.selected_adapter();

        // 网卡下拉框：快照标签 + 已保存 GUID 对应的下标（列表仅含已连接网卡）
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

        // 网段下拉框：仅显示选中网卡的 IPv4 网段（CIDR，同网段地址去重）
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
                        FontIcon::new().glyph("\u{E939}").into(), // Segoe Fluent Icons: Ethernet
                        t!("网卡", "Network adapter"),
                        t!("选择要使用的网络适配器", "Select the network adapter to use"),
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
                        FontIcon::new().glyph("\u{E774}").into(), // Segoe Fluent Icons: Globe
                        t!("IP 网段", "IP subnet"),
                        t!("选中网卡所在的 IPv4 网段", "The IPv4 subnet of the selected adapter"),
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
                        FontIcon::new().glyph("\u{E9E9}").into(), // Segoe Fluent Icons: Network Adapter
                        t!("模拟网络", "Simulated network"),
                        t!("开启后由启动器在本机模拟游戏网络环境", "When on, the launcher simulates the game network environment locally"),
                        settings_toggle(
                            self.net.simulate != Some(false),
                            true,
                            context.callback(Message::NetworkSimulateToggled),
                        ),
                    ),
                    separator(),
                    settings_row(
                        FontIcon::new().glyph("\u{E80F}").into(), // Segoe Fluent Icons: Home
                        t!("主机IP", "Host IP"),
                        t!(
                            "本机游戏 IP，启动时写入游戏与 ServerBox 配置",
                            "This machine's game IP, written to the game and ServerBox configs at launch"
                        ),
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
                        FontIcon::new().glyph("\u{E8D8}").into(), // Segoe Fluent Icons: Server
                        t!("ServerBox IP", "ServerBox IP"),
                        t!(
                            "关闭模拟网络后用于绑定本机 ServerBox 的静态 IP",
                            "Static IP bound to the local ServerBox when simulated network is off"
                        ),
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
                                StackPanel::new().spacing(4.0).grid_column(0).children((
                                    TextBlock::new()
                                        .text(t!("网络设置", "Network"))
                                        .font_size(26.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new()
                                        .text(t!("", ""))
                                        .font_size(12.0)
                                        .opacity(0.6),
                                )),
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
                                                FontIcon::new().glyph("\u{E72C}"), // Refresh
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
        // (文本, r, g, b)
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
                        t!("OpenVPN 客户端配置", "OpenVPN client config"),
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
                        t!("已导入的配置文件", "Imported config file"),
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
                        t!("OpenVPN 连接密码", "OpenVPN connection password"),
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
                        t!("OpenVPN 连接状态", "OpenVPN connection status"),
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
                    TextBlock::new()
                    .text(t!("", ""))
                    .font_size(12.0)
                    .opacity(0.6)
                    .text_wrapping(TextWrapping::Wrap),
                card,
            ))
            .into()
    }

    /// 手动「连接」：校验配置 + 防重入守卫后发起后台连接，结果经 VpnConnectResult 回灌
    pub(in crate::reactor_app) fn start_vpn_connect(&mut self, context: &ComponentContext<Self>) {
        if self.vpn.config_path.is_none() {
            self.vpn_status = VpnStatus::NotConfigured;
            self.dialog = Dialog::Error {
                message: t!("请先导入 VPN 配置文件", "Import a VPN config file first").to_string(),
            };
            return;
        }
        // 已连接/连接中时忽略重复点击：并发连接会拉起第二个 openvpn，
        // 后写入句柄的一方会把仍在工作的第一个隧道 Drop 掉
        if self.vpn_status == VpnStatus::Connected || self.vpn_status == VpnStatus::Connecting {
            return;
        }
        // 手动发起视为新一轮连接：清空上一轮的自动重连计数
        self.vpn_auto_retries = 0;
        self.spawn_vpn_connect_task(context);
    }

    /// 发起一次后台连接（手动连接与断线自动重连共用）：状态置 Connecting，
    /// 后台阻塞等待连接成功，句柄写入 vpn_handle 槽后经 VpnConnectResult 回灌。
    pub(in crate::reactor_app) fn spawn_vpn_connect_task(&mut self, context: &ComponentContext<Self>) {
        self.vpn_status = VpnStatus::Connecting;
        let cfg = self.vpn.clone();
        let handle = self.vpn_handle.clone();
        // 捕获独立代数：连接期间若发生断开/换配置/新连接任务（代数推进），本任务
        // 结果作废，不再把旧配置的句柄写回槽内（否则 UI 显示已连接而实际配置已变/
        // 用户已断开，或与并发的手动连接任务互相覆盖句柄）
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

    /// 连接成功后启动监督线程：每 2 秒检查 openvpn 进程是否仍在运行，
    /// 意外退出时从日志提炼原因回报 `VpnDied(Some(reason))`，由 UI 触发自动重连；
    /// 手动断开/换配置会推进代数（vpn_watch_gen）并取走句柄，监督线程感知后
    /// 回报 `VpnDied(None)` 静默退出，不会误触发重连。
    pub(in crate::reactor_app) fn spawn_vpn_watch(&self, context: &ComponentContext<Self>) {
        let epoch = self.vpn_watch_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = self.vpn_handle.clone();
        let gens = self.vpn_watch_gen.clone();
        context.spawn_background(move |_| {
            loop {
                std::thread::sleep(VPN_WATCH_INTERVAL);
                // 代数过期：期间发生过断开/换配置/新连接，本线程作废
                if gens.load(Ordering::Relaxed) != epoch {
                    return Message::VpnDied(None);
                }
                let reason = {
                    let mut guard = match slot.lock() {
                        Ok(g) => g,
                        Err(_) => return Message::VpnDied(None),
                    };
                    match guard.as_mut() {
                        // 句柄已被取走（手动断开/换配置/上一次 VpnDied 已消费）→ 静默退出
                        None => return Message::VpnDied(None),
                        Some(h) => {
                            if h.is_alive() {
                                continue;
                            }
                            Some(h.exit_detail())
                        }
                    }
                };
                // 死句柄留在槽内，由 VpnDied 处理端取走清理（kill no-op + 删临时文件）
                return Message::VpnDied(reason);
            }
        });
    }

}
