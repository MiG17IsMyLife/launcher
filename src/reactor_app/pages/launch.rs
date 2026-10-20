//! 游戏与 ServerBox 的后台启动流程、进程退出轮询与结果处理。
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows_reactor::*;

use crate::kitten;
use crate::launch;
use crate::mice;
use crate::segatools;
use crate::t;
use crate::tf;
use crate::storage::Loader;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::state::GameRuntime;
use crate::reactor_app::widgets::{now_secs, record_db_match};

/// 拉起读卡器（picodaemon）后等它初始化完成
pub(in crate::reactor_app) const READER_BOOT_WAIT: Duration = Duration::from_secs(1);
/// 运行中游戏进程退出轮询间隔（轮询 child.try_wait 判断是否结束）
pub(in crate::reactor_app) const GAME_POLL_INTERVAL: Duration = Duration::from_secs(1);

impl Launcher {

    pub(in crate::reactor_app) fn do_launch(&mut self, context: &ComponentContext<Self>, index: usize) {
        // 取游戏记录（只读即可，结果通过消息回写 play_count）
        let Some(rec) = self.library.games.get(index).cloned() else {
            return;
        };

        // ServerBox 不再随游戏自动启动，请在该游戏下拉卡片的 ServerBox 配置行手动「启动」。
        // 弹出模态覆盖层（应用内「新窗口」）：部署阶段显示进度环，
        // 进程拉起后（GameLaunched）切换为封面 + 游戏名 + 「关闭游戏」
        self.launch_overlay = Some(index);

        // 当前选中卡片：优先取本游戏记录（每个游戏独立保存）的 GameRecord.card，
        // 未配置时回退旧全局 config.ini [card] cardfile；读卡器（picodaemon）
        // 改为启动器自动解析游戏目录，不再读取手动配置 rec.reader。
        let card = self
            .library
            .games
            .get(index)
            .and_then(|r| r.card.clone())
            .or_else(|| self.store.load_card());

            let settings = self.settings.clone();
            let detected = self.detected.clone();
            // 输入配置（dinput / H 排挡前置检查用）：独立于 snapshots 直接克隆全局配置
            let input_cfg = self.input.clone();

        // 生效主机IP与ServerBox IP：房间内按房间覆盖（房主 .101 / 成员座位 IP / .102），
        // 否则取网络设置页保存值，旧配置缺键时回退选中网卡实际地址（第一/第二/+1 IP）；
        // 连同生效网段一并替换进快照，kitten/mice 渲染与读卡器 idas.ini 共用。
        let mut net = self.net.clone();
        net.host_ip = self.effective_game_ip();
        net.serverbox_ip = self.effective_serverbox_ip();
        net.ip = self.effective_subnet();
        // 房间模式（店内对战）：awl0 虚拟局域网是真实网络，必须关闭「模拟网络」，
        // 否则 game_ip / bind_ip 不写、且游戏会自行模拟网络导致连不上房主 ServerBox
        if self.room_net_override().is_some() {
            net.simulate = Some(false);
        }
        let game_ip = net.host_ip.clone();
        // 生效网段：房间内为房间网段，否则取选中网卡实际网段（回退网络设置页保存值）
        let subnet = self.effective_subnet();
        // 前置检查：网络网段是否已配置（kitten/mice 用生效网段；segatools 维持
        // 原检查口径：房间模式由房间网段提供，否则要求已选网段）
        let net_configured = match rec.loader {
            Loader::Segatools => self
                .room_net_override()
                .is_some()
                || self
                    .net
                    .ip
                    .as_deref()
                    .is_some_and(|v| !v.trim().is_empty()),
            _ => subnet.is_some(),
        };

        // 部署快照：点击启动时一次性克隆全局设置（kitten 另取一次识别库 game_id），
        // 避免后台运行期间 UI 设置被改动导致生成不一致。
        let dep = match rec.loader {
            Loader::Kitten => {
                // 网段取生效网段（房间网段 / 选中网卡实际网段，回退保存值）
                net.ip = subnet.clone();
                Some(launch::DeployConfig::Kitten(kitten::DeploySnapshot {
                    net,
                    system: self.system.clone(),
                    coin: self.coin.clone(),
                    graphics: self.graphics.clone(),
                    input: self.input.clone(),
                    game_id: record_db_match(&rec).and_then(|m| m.game_id),
                }))
            }
            Loader::Mice => {
                net.ip = subnet.clone();
                Some(launch::DeployConfig::Mice(mice::DeploySnapshot {
                    net,
                    system: self.system.clone(),
                    coin: self.coin.clone(),
                    graphics: self.graphics.clone(),
                    input: self.input.clone(),
                    card: card.clone(),
                    room: self.room_net_override(),
                }))
            }
            // segatools（IDAC）：快照带游戏版本（spike 匹配）与 game_id（dipsw 仅 SDGT）；
            // 网段维持所选网段不变，房间模式由 room 覆盖（[keychip] subnet → 房间网段）
            Loader::Segatools => Some(launch::DeployConfig::Segatools(segatools::DeploySnapshot {
                net,
                system: self.system.clone(),
                coin: self.coin.clone(),
                input: self.input.clone(),
                graphics: self.graphics.clone(),
                version: rec.version.clone(),
                game_id: rec.game_id.clone(),
                aime_card: card.clone(),
                room: self.room_net_override(),
            })),
        };

        // ServerBox 静态 IP：启动时若选中网卡（非 awl 虚拟网卡）没有第二个 IPv4 地址，
        // 则为它添加该 IP（仅关闭模拟网络且已填 serverbox_ip 的 kitten/mice 游戏），
        // 游戏退出后删除本次添加的地址。取生效值（保存值优先，回退网卡第二/+1 IP，
        // 兼容旧配置缺 serverbox_ip 的机器）；网卡名提前取出，避免后台闭包内无法访问 self。
        let serverbox_ip = self.effective_serverbox_ip();
        // 房间模式：网络强制走 awl0（地址由房间流程管理，netsh 也会清 wintun 地址），
        // ServerBox 静态 IP 步骤整体跳过，绝不触碰物理网卡
        let serverbox_enabled =
            self.net.simulate == Some(false) && self.room_net_override().is_none();
        let serverbox_adapter = self.selected_adapter().map(|a| a.friendly.clone());

        // 启动不再自动连接/断开 VPN（需要连远程服务器时请先在 VPN 设置页手动连接）。
        // 全部逻辑在后台线程执行，避免部署 / 游戏运行期间冻结 UI。
        context.spawn_background(move |_| {
            // 启动游戏前：先检查 keychip 序列号是否已设置（mice / kitten / segatools 均需要）
            if dep.as_ref().and_then(|d| d.keychip()).is_none() {
                return Message::LaunchDone {
                    index,
                    result: Err(t!("未设置 Keychip 序列号，请先在设置页填写后重试",
                                   "Keychip serial is not set; please fill it in the settings page and retry").to_string()),
                    goto: Some(("system".to_string(), None)),
                };
            }

            // 前置检查 1：网络网段（所有游戏通用）
            if !net_configured {
                return Message::LaunchDone {
                    index,
                    result: Err(t!("未配置网络网段，请先在「网络设置」页选择网卡与网段",
                                   "Network subnet is not configured; select an adapter and subnet on the Network page first").to_string()),
                    goto: Some(("network".to_string(), None)),
                };
            }

            // 前置检查 1.5（ServerBox 静态 IP）移至下方「2.5」：部署成功后、拉起进程前执行，
            // 失败路径无需回滚已部署文件。房间模式（awl0）的网卡地址由房间流程管理。

            // 前置检查 2：按游戏形态检查必需配置，任一缺失即中止（不拉起任何进程）
            let card_missing =
                card.as_deref().map(str::trim).filter(|c| !c.is_empty()).is_none();
            let no_file = |p: &Option<String>| p.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_none();
            match rec.loader {
                // IDAS4~8（kitten / mice）：车卡 + ServerBox + 读卡器
                Loader::Kitten | Loader::Mice => {
                    // 前置检查：idas4~8 在 DirectInput 模式下仅支持序列排挡，
                    // H 排挡不被游戏支持，提前报错阻断启动（避免拉起后手柄无法换挡）
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
                // IDZ / IDAC（segatools）：车卡 + amdaemon + 游戏主程序
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

            // 部署顺序：**先把全部配置 / 工具文件写到位**（读卡器 + 游戏 / amdaemon），
            // 再统一拉起读卡器 → [segatools: amdaemon] → 游戏，杜绝
            // 「加载器已启动、配置文件还没拷贝」的竞态。
            // 读卡器（picodaemon，segatools 游戏除外——ALLS / Nu 用内置 Aime 读卡器模拟）；
            // 游戏退出后回收读卡器 / amdaemon 等伴随进程。
            // （ServerBox 不再随游戏自动启动，请在其配置行手动「启动」。）
            let mut companions: Vec<std::process::Child> = Vec::new();

            // 游戏目录：记录路径的父目录（kitten 的 elf / mice 的游戏 exe 所在目录）
            let game_dir = Path::new(&rec.path)
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));

            // 1) 读卡器文件部署（mice.exe / mice.dll + idas.ini cardfile）
            //    读卡器自动解析：游戏目录已有已知 picodaemon 则复用，否则拷依赖包；
            //    kitten（IDAS4~5）额外拷贝 cg.dll / cgGL.dll 到游戏目录。
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
                // IDAS4~5（kitten）：游戏进程不处理插卡键（kitten [xinput] 无 card 键），
                // 输入后端与插卡键都由读卡器侧 mice 读取：[io3] mode 按输入设置页的
                // 输入类型（同游戏侧 idas.ini，仅认 xinput/dinput），[xinput] card 写捕获的插卡键
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
                match launch::deploy_reader(&r, &settings, &detected, card.as_deref(), game_ip.as_deref(), xi_card, io3_mode) {
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

            // 2) 游戏（与 amdaemon）文件部署
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

            // 2.5) 前置检查 1.5：ServerBox 静态 IP —— 仅当关闭模拟网络、已填 serverbox_ip、
            //      选中网卡不是 awl 虚拟网卡（店内对战地址由房间流程管理，netsh 也会
            //      清空 wintun 地址）且该网卡当前没有第二个 IPv4 地址时，才把该 IP
            //      追加到网卡上；游戏退出后由退出轮询删除（见 poll_game_exit）。
            let mut added_static_ip: Option<(String, String)> = None;
            if serverbox_enabled && rec.loader != Loader::Segatools {
                if let (Some(ip), Some(adapter)) = (
                    serverbox_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                    serverbox_adapter.as_deref(),
                ) {
                    if adapter != crate::awl::AWL_IFACE_NAME {
                        // 重新枚举网卡取最新地址与掩码；找不到网卡则跳过（不阻断启动）
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
            // 回滚：添加静态 IP 之后的启动失败路径，尽力删除刚加的地址（幂等）
            let rollback_static_ip = |added: &Option<(String, String)>| {
                if let Some((adapter, ip)) = added {
                    let _ = launch::remove_serverbox_static_ip(adapter, ip);
                }
            };

            // 3) 拉起读卡器（picodaemon 载入 mice 内部运行，进程名是 mice.exe）
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

            // 4) segatools：拉起 amdaemon（伴随进程，游戏退出后 kill）
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

            // 5) 拉起游戏主进程（mice `-p -b <游戏exe>` / kitten `-b <文件名>`），保留 CMD 窗口
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

            // 游戏已成功拉起：把进程句柄交给组件状态，回报 GameLaunched 进入「运行中」。
            // 部分加载器进程会在游戏窗口关闭后继续驻留，若在本后台任务里 child.wait() 会一直
            // 阻塞、导致状态栏停在「正在启动游戏…」。改为由 GamePollTick 轮询 try_wait 判断
            // 退出，退出后回收读卡器等伴随进程并回报「已结束」。
            return Message::GameLaunched {
                runtime: GameRuntime {
                    index,
                    child: Arc::new(Mutex::new(game)),
                    companions: Arc::new(Mutex::new(companions)),
                    kill_amdaemon: rec.loader == Loader::Segatools,
                    // ServerBox 文件名：手动启动的 ServerBox 关闭游戏时按映像名回收
                    serverbox_image: rec.serverbox.as_deref().and_then(|p| {
                        Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned())
                    }),
                    // 启动前为本机网卡添加的 ServerBox 静态 IP：游戏退出后回删
                    // （awl 虚拟网卡分支恒为 None，地址由房间流程管理）
                    added_static_ip,
                },
            };
        });
    }

    /// 排一拍游戏进程退出轮询（后台 sleep 后回灌 UI 线程）
    pub(in crate::reactor_app) fn spawn_game_poll(context: &ComponentContext<Self>) {
        context.spawn_background(move |_| {
            thread::sleep(GAME_POLL_INTERVAL);
            Message::GamePollTick
        });
    }

    /// 单独启动该游戏的 ServerBox（不随游戏自动拉起）。
    /// 与游戏启动共用部署流程（同套配置快照），差异仅在 ServerBox 的 ip 偏移；
    /// 启动后不回收进程（常驻，直到用户手动关闭）。
    pub(in crate::reactor_app) fn do_launch_serverbox(&mut self, context: &ComponentContext<Self>, index: usize) {
        let Some(rec) = self.library.games.get(index).cloned() else {
            return;
        };
        if rec.serverbox.is_none() {
            return;
        }
        // segatools（ALLS：IDZ / IDAC 等）启动链路暂未接入
        if matches!(rec.loader, Loader::Segatools) {
            self.dialog = Dialog::Error {
                message: t!("segatools 加载器暂不支持 ServerBox",
                    "The segatools loader does not support ServerBox yet").to_string(),
            };
            return;
        }

        let settings = self.settings.clone();
        let detected = self.detected.clone();
        // 生效主机IP/ServerBox IP/网段：与游戏启动同源（房间内按房间覆盖，
        // 否则取保存值，旧配置缺键时回退选中网卡实际地址）
        let mut net = self.net.clone();
        net.host_ip = self.effective_game_ip();
        net.serverbox_ip = self.effective_serverbox_ip();
        net.ip = self.effective_subnet();
        // 房间模式（店内对战）：awl0 虚拟局域网是真实网络，必须关闭「模拟网络」，
        // 否则 game_ip / bind_ip 不写、且 ServerBox 会自行模拟网络导致连不上
        if self.room_net_override().is_some() {
            net.simulate = Some(false);
        }
        let net_configured = net.ip.is_some();
        let dep = match rec.loader {
            Loader::Kitten => Some(launch::DeployConfig::Kitten(kitten::DeploySnapshot {
                net,
                system: self.system.clone(),
                coin: self.coin.clone(),
                graphics: self.graphics.clone(),
                input: self.input.clone(),
                game_id: record_db_match(&rec).and_then(|m| m.game_id),
            })),
            Loader::Mice => Some(launch::DeployConfig::Mice(mice::DeploySnapshot {
                net,
                system: self.system.clone(),
                coin: self.coin.clone(),
                graphics: self.graphics.clone(),
                input: self.input.clone(),
                card: None,
                room: self.room_net_override(),
            })),
            // segatools 启动链路暂未接入（do_launch_serverbox 入口已提前拦截）
            Loader::Segatools => None,
        };

        context.spawn_background(move |_| {
            // 前置检查：网络网段是否已配置（与游戏启动同检查；房间模式由房间网段提供）
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

    /// 后台启动任务回报：成功回写游玩次数，失败弹窗展示原因
    pub(in crate::reactor_app) fn handle_launch_done(
        &mut self,
        index: usize,
        result: Result<(), String>,
        goto: Option<(String, Option<usize>)>,
    ) {
        self.game_running = false;
        // 两种结束路径（自然退出 / 手动关闭 / 启动失败）都收起模态覆盖层
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
                // 启动失败：弹窗展示原因（避免只在状态栏一闪而过）；
                // 若带跳转目标，弹窗提供「前往设置」按钮
                self.dialog = Dialog::LaunchFailed { message: e, goto };
            }
        }
    }

    /// 运行中游戏进程退出轮询：退出则回收伴随进程并回写，否则排下一拍
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
            // 游戏进程已退出：回收读卡器等伴随进程，回写游玩次数
            if let Ok(mut v) = rt.companions.lock() {
                for mut ch in v.drain(..) {
                    let _ = ch.kill();
                }
            }
            // 按映像名回收伴随进程：segatools 的 amdaemon（inject 派生，句柄杀不到）、
            // 手动启动的 ServerBox（启动器无句柄）；未在运行的静默忽略
            rt.taskkill_images();
            self.game_running = false;
            // 游戏自然退出：收起模态覆盖层，恢复主界面
            self.launch_overlay = None;
            if let Some(g) = self.library.games.get_mut(rt.index) {
                g.play_count += 1;
                g.last_played = Some(now_secs());
            }
            self.store.save_library(&self.library);
            // 启动前添加到本机网卡的 ServerBox 静态 IP：游戏结束后回删（后台执行，
            // 失败仅状态栏提示）。awl 虚拟网卡（店内对战）地址由房间流程管理，
            // added_static_ip 恒为 None，不在该机制内。
            if let Some((adapter, ip)) = rt.added_static_ip.clone() {
                context.spawn_background(move |_| {
                    Message::StaticIpRemoved(launch::remove_serverbox_static_ip(&adapter, &ip))
                });
            }
        } else {
            // 仍在运行：把句柄放回状态并排下一拍（每 GAME_POLL_INTERVAL 检查一次）
            self.game_runtime = Some(rt);
            Self::spawn_game_poll(context);
        }
    }

}
