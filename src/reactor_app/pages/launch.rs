use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows_reactor::*;

use crate::kitten;
use crate::launch;
use crate::mice;
use crate::segatools;
use crate::storage::GameRecord;
use crate::t;
use crate::tf;
use crate::storage::Loader;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::state::GameRuntime;
use crate::reactor_app::widgets::{now_secs, record_db_match};

pub(in crate::reactor_app) const READER_BOOT_WAIT: Duration = Duration::from_secs(1);

pub(in crate::reactor_app) const GAME_POLL_INTERVAL: Duration = Duration::from_secs(1);

impl Launcher {

    fn build_launch_snapshot(
        &mut self,
        rec: &GameRecord,
        card: Option<String>,
        for_serverbox: bool,
    ) -> (Option<launch::DeployConfig>, bool, Option<String>) {
        self.detected = launch::detect_loaders();
        let mut net = self.net.clone();
        net.host_ip = self.effective_game_ip();
        net.serverbox_ip = self.effective_serverbox_ip();
        net.ip = self.effective_subnet();

        if self.room_net_override().is_some() {
            net.simulate = Some(false);
        }
        let game_ip = net.host_ip.clone();

        let subnet = self.effective_subnet();

        let net_configured = if for_serverbox {
            net.ip.is_some()
        } else {
            match rec.loader {
                Loader::Segatools => {
                    self.room_net_override().is_some()
                        || self.net.ip.as_deref().is_some_and(|v| !v.trim().is_empty())
                }
                _ => subnet.is_some(),
            }
        };

        let mut system = self.system.clone();
        system.region = rec.region.clone().or_else(|| self.system.region.clone());
        let dep = match rec.loader {
            Loader::Kitten => {

                net.ip = subnet.clone();
                Some(launch::DeployConfig::Kitten(kitten::DeploySnapshot {
                    net,
                    system: system.clone(),
                    coin: self.coin.clone(),
                    graphics: self.graphics.clone(),
                    input: self.input.clone(),
                    game_id: record_db_match(rec).and_then(|m| m.game_id),
                }))
            }
            Loader::Mice => {
                net.ip = subnet.clone();
                Some(launch::DeployConfig::Mice(mice::DeploySnapshot {
                    net,
                    system: system.clone(),
                    coin: self.coin.clone(),
                    graphics: self.graphics.clone(),
                    input: self.input.clone(),
                    card,
                    room: self.room_net_override(),
                }))
            }
            Loader::Segatools if for_serverbox => None,

            Loader::Segatools => {
                let mut system = system.clone();
                if self.room_net_override().is_some() {
                    let is_host = matches!(
                        self.room_status,
                        crate::awl::RoomStatus::Hosting { .. }
                    );
                    system.master = Some(is_host);
                    let seat_ip = if is_host {
                        Some(crate::awl::HOST_GAME_IP.to_string())
                    } else {
                        self.room_my_ip.clone()
                    };
                    if let Some(name) = seat_ip
                        .as_deref()
                        .and_then(|ip| ip.rsplit('.').next())
                        .and_then(|s| s.parse::<u8>().ok())
                        .and_then(super::room::seat_name_of_octet)
                    {
                        system.seat = Some(name.to_string());
                    }
                }
                Some(launch::DeployConfig::Segatools(segatools::DeploySnapshot {
                    net,
                    system,
                    coin: self.coin.clone(),
                    input: self.input.clone(),
                    graphics: self.graphics.clone(),
                    version: rec.version.clone(),
                    game_id: rec.game_id.clone(),
                    aime_card: card,
                    room: self.room_net_override(),
                }))
            }
        };
        (dep, net_configured, game_ip)
    }

    pub(in crate::reactor_app) fn do_launch(&mut self, context: &ComponentContext<Self>, index: usize) {

        let Some(rec) = self.library.games.get(index).cloned() else {
            return;
        };

        self.launch_overlay = Some(index);

        let card = self
            .library
            .games
            .get(index)
            .and_then(|r| r.card.clone())
            .or_else(|| self.store.load_card());

        let (dep, net_configured, game_ip) = self.build_launch_snapshot(&rec, card.clone(), false);
        let settings = self.settings.clone();
        let detected = self.detected.clone();

        let input_cfg = self.input.clone();
        let auto_insert = self.system.auto_insert_card == Some(true);
        let region_missing = rec
            .region
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_none();

        let serverbox_ip = self.effective_serverbox_ip();

        let serverbox_enabled =
            self.net.simulate == Some(false) && self.room_net_override().is_none();
        let serverbox_adapter = self.selected_adapter().map(|a| a.friendly.clone());

        context.spawn_background(move |_| {

            if region_missing {
                return Message::LaunchDone {
                    index,
                    result: Err(t!("未设置地区，请在该游戏行的下拉配置中选择地区",
                                   "Region is not set; select it in this game's dropdown").to_string()),
                    goto: Some(("games".to_string(), Some(index))),
                };
            }

            if dep.as_ref().and_then(|d| d.keychip()).is_none() {
                return Message::LaunchDone {
                    index,
                    result: Err(t!("未设置 Keychip 序列号，请先在设置页填写后重试",
                                   "Keychip serial is not set; please fill it in the settings page and retry").to_string()),
                    goto: Some(("system".to_string(), None)),
                };
            }

            if !net_configured {
                return Message::LaunchDone {
                    index,
                    result: Err(t!("未配置网络网段，请先在「网络设置」页选择网卡与网段",
                                   "Network subnet is not configured; select an adapter and subnet on the Network page first").to_string()),
                    goto: Some(("network".to_string(), None)),
                };
            }

            let card_missing =
                card.as_deref().map(str::trim).filter(|c| !c.is_empty()).is_none();
            let no_file = |p: &Option<String>| p.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_none();
            match rec.loader {

                Loader::Kitten | Loader::Mice => {

                    if input_cfg.kind.as_deref() == Some("dinput")
                        && input_cfg.shifter_type.as_deref() == Some("h")
                    {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!(
                                "该游戏不支持 DirectInput 的 H 排挡，请改为序列排挡或切换为 XInput 后重试",
                                "This game does not support DirectInput H-pattern shifter; switch to sequential or use XInput and retry"
                            )
                            .to_string()),
                            goto: Some(("input".to_string(), None)),
                        };
                    }
                    if card_missing {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!("未设置车卡，请先进入「卡片选择」页选用一张卡片",
                                           "No card selected; open Card select and pick a card first").to_string()),
                            goto: Some(("cards".to_string(), Some(index))),
                        };
                    }
                    if no_file(&rec.serverbox) {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!("未配置 ServerBox，请在该游戏行的 ServerBox 行点击「添加」",
                                           "ServerBox is not configured; click Add on this game's ServerBox row").to_string()),
                            goto: Some(("games".to_string(), Some(index))),
                        };
                    }
                }

                Loader::Segatools => {
                    if card_missing {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!("未设置车卡，请先进入「卡片选择」页选用一张卡片",
                                           "No card selected; open Card select and pick a card first").to_string()),
                            goto: Some(("cards".to_string(), Some(index))),
                        };
                    }
                    if no_file(&rec.amdaemon) {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!("未设置 amdaemon.exe，请在该游戏行的下拉配置中选择",
                                           "amdaemon.exe is not configured; select it in this game's dropdown").to_string()),
                            goto: Some(("games".to_string(), Some(index))),
                        };
                    }
                    if no_file(&rec.game_exe) {
                        return Message::LaunchDone {
                            index,
                            result: Err(t!("未设置游戏主程序（GameProject-Win64-Shipping.exe），请在该游戏行的下拉配置中选择",
                                           "Game executable (GameProject-Win64-Shipping.exe) is not configured; select it in this game's dropdown").to_string()),
                            goto: Some(("games".to_string(), Some(index))),
                        };
                    }
                }
            }

            let mut companions: Vec<std::process::Child> = Vec::new();

            let game_dir = Path::new(&rec.path)
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));

            let (reader_str, reader_mice_exe) = if rec.loader != Loader::Segatools {
                let reader_path = match launch::ensure_picodaemon(&game_dir) {
                    Ok(p) => p,
                    Err(e) => {
                        return Message::LaunchDone {
                            index,
                            result: Err(tf!("读卡器准备失败：{}", "Card reader prep failed: {}", e)),
                            goto: None,
                        };
                    }
                };
                if rec.loader == Loader::Kitten {
                    if let Err(e) = launch::ensure_cg_libs(&game_dir) {
                        return Message::LaunchDone {
                            index,
                            result: Err(tf!("图形库准备失败：{}", "Graphics lib prep failed: {}", e)),
                            goto: None,
                        };
                    }
                }
                let r = reader_path.to_string_lossy().into_owned();

                let (io3_mode, xi_card) = if rec.loader == Loader::Kitten {
                    let mode = match input_cfg.kind.as_deref().map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                        Some("xinput") => Some("xinput"),
                        Some("dinput") => Some("dinput"),
                        _ => None,
                    };
                    let card_btn = input_cfg.xi_card.as_deref().map(str::trim).filter(|s| !s.is_empty());
                    (mode, card_btn)
                } else {
                    (None, None)
                };
                match launch::deploy_reader(&r, &settings, &detected, card.as_deref(), game_ip.as_deref(), xi_card, io3_mode, auto_insert) {
                    Ok(exe) => (Some(r), Some(exe)),
                    Err(e) => {
                        return Message::LaunchDone {
                            index,
                            result: Err(tf!("读卡器部署失败：{}", "Card reader deploy failed: {}", e)),
                            goto: None,
                        };
                    }
                }
            } else {
                (None, None)
            };

            let loader_exe = match launch::prepare_game(&rec, &settings, &detected, dep.as_ref()) {
                Ok(exe) => exe,
                Err(e) => {
                    return Message::LaunchDone {
                        index,
                        result: Err(tf!("游戏启动失败：{}", "Game launch failed: {}", e)),
                        goto: None,
                    };
                }
            };

            let mut added_static_ip: Option<(String, String)> = None;
            if serverbox_enabled && rec.loader != Loader::Segatools {
                if let (Some(ip), Some(adapter)) = (
                    serverbox_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                    serverbox_adapter.as_deref(),
                ) {
                    if adapter != crate::awl::AWL_IFACE_NAME {

                        let fresh = crate::netinfo::list_adapters()
                            .into_iter()
                            .find(|a| a.friendly == adapter);
                        if let Some(a) = fresh.filter(|a| a.ips.len() < 2) {
                            let mask = a
                                .ips
                                .first()
                                .map(|ip| mice::format_ipv4(mice::mask_of(ip.prefix)))
                                .unwrap_or_else(|| "255.255.255.0".to_string());
                            if let Err(e) = launch::add_serverbox_static_ip(adapter, ip, &mask) {
                                return Message::LaunchDone {
                                    index,
                                    result: Err(tf!("为网卡添加 ServerBox 静态 IP 失败：{}", "Failed to add ServerBox static IP to adapter: {}", e)),
                                    goto: None,
                                };
                            }
                            added_static_ip = Some((adapter.to_string(), ip.to_string()));
                        }
                    }
                }
            }

            let rollback_static_ip = |added: &Option<(String, String)>| {
                if let Some((adapter, ip)) = added {
                    let _ = launch::remove_serverbox_static_ip(adapter, ip);
                }
            };

            if let Some(mice_exe) = &reader_mice_exe {
                let p = reader_str.as_deref().unwrap_or_default();
                match launch::spawn_reader(p, mice_exe) {
                    Ok(child) => companions.push(child),
                    Err(e) => {
                        rollback_static_ip(&added_static_ip);
                        return Message::LaunchDone {
                            index,
                            result: Err(tf!("读卡器启动失败：{}", "Failed to start card reader ({}): {e}", e)),
                            goto: None,
                        };
                    }
                }
                thread::sleep(READER_BOOT_WAIT);
            }

            if rec.loader == Loader::Segatools {
                match launch::spawn_amdaemon(&rec, &loader_exe, dep.as_ref()) {
                    Ok(child) => companions.push(child),
                    Err(e) => {
                        for mut c in companions {
                            let _ = c.kill();
                        }
                        rollback_static_ip(&added_static_ip);
                        return Message::LaunchDone {
                            index,
                            result: Err(tf!("amdaemon 启动失败：{}", "Failed to start amdaemon: {}", e)),
                            goto: None,
                        };
                    }
                }
            }

            let game = match launch::spawn_prepared_game(&rec, &loader_exe, dep.as_ref()) {
                Ok(child) => child,
                Err(e) => {
                    for mut c in companions {
                        let _ = c.kill();
                    }
                    rollback_static_ip(&added_static_ip);
                    return Message::LaunchDone {
                        index,
                        result: Err(tf!("游戏启动失败：{}", "Game launch failed: {}", e)),
                        goto: None,
                    };
                }
            };

            let game_id = record_db_match(&rec)
                .and_then(|m| m.game_id)
                .or_else(|| {
                    rec.game_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                });
            let is_idz = crate::segatools::game_kind(game_id.as_deref())
                == crate::segatools::GameKind::Idz;
            let mut serverbox_images: Vec<String> = rec
                .serverbox
                .as_deref()
                .and_then(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()))
                .into_iter()
                .collect();
            if is_idz {
                let fixed = crate::segatools::idz_serverbox_image(game_id.as_deref());
                if !serverbox_images.iter().any(|n| n.eq_ignore_ascii_case(fixed)) {
                    serverbox_images.push(fixed.to_string());
                }
            }

            return Message::GameLaunched {
                runtime: GameRuntime {
                    index,
                    child: Arc::new(Mutex::new(game)),
                    companions: Arc::new(Mutex::new(companions)),
                    kill_amdaemon: rec.loader == Loader::Segatools,

                    serverbox_images,

                    added_static_ip,
                },
            };
        });
    }

    pub(in crate::reactor_app) fn spawn_game_poll(context: &ComponentContext<Self>) {
        context.spawn_background(move |_| {
            thread::sleep(GAME_POLL_INTERVAL);
            Message::GamePollTick
        });
    }

    pub(in crate::reactor_app) fn do_launch_serverbox(&mut self, context: &ComponentContext<Self>, index: usize) {
        let Some(rec) = self.library.games.get(index).cloned() else {
            return;
        };
        if rec.serverbox.is_none() {
            return;
        }

        if matches!(rec.loader, Loader::Segatools) {
            self.dialog = Dialog::Error {
                message: t!("segatools 加载器暂不支持 ServerBox",
                    "The segatools loader does not support ServerBox yet").to_string(),
            };
            return;
        }

        let (dep, net_configured, _game_ip) = self.build_launch_snapshot(&rec, None, true);
        let settings = self.settings.clone();
        let detected = self.detected.clone();

        context.spawn_background(move |_| {

            if !net_configured {
                return Message::ServerBoxLaunched {
                    result: Err(t!("未配置网络网段，请先在「网络设置」页选择网卡与网段",
                                   "Network subnet is not configured; select an adapter and subnet on the Network page first").to_string()),
                    runtime: None,
                    goto: Some(("network".to_string(), None)),
                };
            }
            if dep.as_ref().and_then(|d| d.keychip()).is_none() {
                return Message::ServerBoxLaunched {
                    result: Err(t!("未设置 Keychip 序列号，请先在设置页填写后重试",
                                   "Keychip serial is not set; please fill it in the settings page and retry").to_string()),
                    runtime: None,
                    goto: Some(("system".to_string(), None)),
                };
            }
            let launched = launch::launch_serverbox(&rec, &settings, &detected, dep.as_ref())
                .map(|child| {
                    crate::reactor_app::state::ServerBoxRuntime {
                        index,
                        child: Arc::new(Mutex::new(child)),
                    }
                })
                .map_err(|e| tf!("ServerBox 启动失败：{}", "ServerBox launch failed: {}", e));
            let runtime = launched.as_ref().ok().cloned();
            Message::ServerBoxLaunched {
                result: launched.map(|_| ()),
                runtime,
                goto: None,
            }
        });
    }

    pub(in crate::reactor_app) fn handle_launch_done(
        &mut self,
        index: usize,
        result: Result<(), String>,
        goto: Option<(String, Option<usize>)>,
    ) {
        self.game_running = false;

        if self.launch_overlay == Some(index) {
            self.launch_overlay = None;
        }
        match result {
            Ok(()) => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.play_count += 1;
                    g.last_played = Some(now_secs());
                }
                self.store.save_library(&self.library);
            }
            Err(e) => {

                self.dialog = Dialog::LaunchFailed { message: e, goto };
            }
        }
    }

    pub(in crate::reactor_app) fn poll_game_exit(&mut self, context: &ComponentContext<Self>) {
        let rt = match self.game_runtime.take() {
            Some(r) => r,
            None => return,
        };
        let exited = match rt.child.lock() {
            Ok(mut c) => c.try_wait().map(|s| s.is_some()).unwrap_or(true),
            Err(_) => true,
        };
        if exited {

            if let Ok(mut v) = rt.companions.lock() {
                for mut ch in v.drain(..) {
                    let _ = ch.kill();
                }
            }

            rt.taskkill_images();
            self.game_running = false;

            self.launch_overlay = None;
            if let Some(g) = self.library.games.get_mut(rt.index) {
                g.play_count += 1;
                g.last_played = Some(now_secs());
            }
            self.store.save_library(&self.library);

            if let Some((adapter, ip)) = rt.added_static_ip.clone() {
                context.spawn_background(move |_| {
                    Message::StaticIpRemoved(launch::remove_serverbox_static_ip(&adapter, &ip))
                });
            }
        } else {

            self.game_runtime = Some(rt);
            Self::spawn_game_poll(context);
        }
    }

}
