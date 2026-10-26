//! 店内对战（房间虚拟局域网）页：大厅视图 + 房间内视图 + 状态机与后台任务。
//!
//! - 大厅（Idle）：右上角「创建…/加入…/刷新」，主体为公开房间列表表格；
//! - 房间内（Hosting/Joined）：房间状态、成员列表（含机位 A1/A2/B1/B2）、离开/关闭房间按钮；
//! - 单房间约束由状态机保证。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use windows_reactor::*;

use crate::awl::{AwlManager, RoomStatus};
use crate::roomdir::RoomDirClient;
use crate::t;
use crate::tf;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{record_matches_product, selectable_text, separator, settings_row};

/// 成员侧：房主失联多久后自动退出。
const HOST_DOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// 房主侧：成员掉线多久后按「离开房间」处理（宽限内容忍瞬时网络抖动，
/// 超时后代成员释放座位，座位按最低空闲位顺位复用）。仅对「曾建连」的成员生效。
const MEMBER_KICK_AFTER: Duration = Duration::from_secs(10);
/// 房主侧：从未建连过的新成员的加入宽限。dir.join 在 awl 建链之前就把成员登记进
/// 目录（座位 IP 须先于拉起 awl 拿到），成员侧冷启动（拉起 awl + DHT bootstrap +
/// 邀请确认 + 拨号）经常超过 10s 掉线宽限——若同用短宽限，首次进房会被房主在
/// 建链完成前误踢（成员自身建链上限 LINK_TIMEOUT=30s，超过即放弃）。
const MEMBER_JOIN_GRACE: Duration = Duration::from_secs(90);
/// 未设昵称弹窗「跳过」后的会话内默认昵称（不落盘）。
pub(in crate::reactor_app) const ROOM_DEFAULT_NICKNAME: &str = "player1234";
/// 房间内本地观测周期。tick 内全部是本机操作（awl 127.0.0.1 API / 收件箱 /
/// 状态上报），目录请求已事件化——短周期只为快速消费长连接推送与刷新 UI，
/// 不产生任何外网流量。
const TICK_INTERVAL_IN_ROOM: Duration = Duration::from_secs(1);
/// 离开房间页后的观测周期（降频；宽限/失联计时为 Instant 时间基准，
/// 只影响检测延迟、不影响语义）。
const TICK_INTERVAL_OFF_PAGE: Duration = Duration::from_secs(15);

impl Launcher {
    pub(in crate::reactor_app) fn dir_client(&self) -> RoomDirClient {
        RoomDirClient::new(
            self.room
                .directory
                .clone()
                .unwrap_or_else(|| crate::roomdir::DEFAULT_DIRECTORY.to_string()),
        )
    }

    /// 房间模式网络覆盖（mice 部署用）：房间内 Some，否则 None 走原网段逻辑。
    pub(in crate::reactor_app) fn room_net_override(
        &self,
    ) -> Option<crate::mice::RoomNetOverride> {
        match &self.room_status {
            RoomStatus::Hosting { .. } => Some(crate::mice::RoomNetOverride {
                cidr: crate::awl::ROOM_CIDR.to_string(),
                my_game_ip: None,
                server_ip: crate::awl::HOST_SERVERBOX_IP.to_string(),
            }),
            RoomStatus::Joined { .. } => Some(crate::mice::RoomNetOverride {
                cidr: crate::awl::ROOM_CIDR.to_string(),
                my_game_ip: self.room_my_ip.clone(),
                server_ip: crate::awl::HOST_SERVERBOX_IP.to_string(),
            }),
            _ => None,
        }
    }

    /// 生效的主机（本机游戏）IP：房间内按房间覆盖（房主 .101 / 成员座位 IP）；
    /// 否则取网络设置页的主机IP，旧配置无该键（新增）时回退选中网卡第一个 IPv4；
    /// None = 未配置，渲染时跳过 game_ip。
    pub(in crate::reactor_app) fn effective_game_ip(&self) -> Option<String> {
        match self.room_net_override() {
            Some(override_) => Some(
                override_
                    .my_game_ip
                    .unwrap_or_else(|| crate::awl::HOST_GAME_IP.to_string()),
            ),
            None => self
                .net
                .host_ip
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    self.selected_adapter()
                        .and_then(|a| a.ips.first())
                        .map(|ip| ip.ip.clone())
                }),
        }
    }

    /// 生效的 ServerBox IP（bind_ip 用）：房间内强制为房主 ServerBox 地址 .102
    /// （awl0 上的该地址由房间流程管理，不读保存值——网络设置页可能仍是物理网 IP）；
    /// 否则保存值优先，回退选中网卡第二个 IPv4，再回退第一个 IP+1 ——
    /// 兼容旧配置（serverbox_ip 为空）与未重选网卡的机器。
    pub(in crate::reactor_app) fn effective_serverbox_ip(&self) -> Option<String> {
        if self.room_net_override().is_some() {
            return Some(crate::awl::HOST_SERVERBOX_IP.to_string());
        }
        let saved = self
            .net
            .serverbox_ip
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        saved.or_else(|| {
            let a = self.selected_adapter()?;
            a.ips
                .get(1)
                .map(|ip| ip.ip.clone())
                .or_else(|| a.ips.first().and_then(|ip| crate::netinfo::next_ip(&ip.ip)))
        })
    }

    /// 生效网段：房间内为房间网段（店内对战 172.10.1.0/24），否则取选中网卡
    /// 第一个 IPv4 所在网段（回退网络设置页保存的网段）；None = 未配置。
    pub(in crate::reactor_app) fn effective_subnet(&self) -> Option<String> {
        match self.room_net_override() {
            Some(override_) => Some(override_.cidr),
            None => self
                .selected_adapter()
                .and_then(|a| a.ips.first())
                .map(crate::netinfo::network_cidr)
                .or_else(|| {
                    self.net
                        .ip
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                }),
        }
    }

    /// 房间游戏在游戏库中的下标：记录须命中同名产品（房主开房与成员加入时
    /// room_game 均为识别库产品名；成员侧服务端不下发 game_id，故统一按产品名匹配）。
    /// 命中路径含 CRC 与 ICF（segatools 的 game_id + 版本）两种——后者覆盖识别库里
    /// 只有 version 没有 crc 的 segatools 条目。None = 该游戏尚未添加到游戏库。
    pub(in crate::reactor_app) fn room_game_index(&self) -> Option<usize> {
        let game = self.room_game.trim();
        if game.is_empty() {
            return None;
        }
        self.library
            .games
            .iter()
            .position(|r| record_matches_product(r, game))
    }

    /// 房间游戏是否配置了 ServerBox 程序（None / 空串视为未配置）。
    ///
    /// 只用于门控房主的「启动 ServerBox」按钮——未配置时点击只会报
    /// 「该游戏未配置 ServerBox」。与 ServerBox 地址行无关：地址行按
    /// 游戏形态（loader）门控，见 room_uses_serverbox。
    pub(in crate::reactor_app) fn room_has_serverbox(&self) -> bool {
        let Some(index) = self.room_game_index() else {
            return false;
        };
        self.library
            .games
            .get(index)
            .and_then(|r| r.serverbox.as_deref())
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    }

    /// 房间游戏形态是否存在 ServerBox 副地址 .102（与 ServerBox 程序是否
    /// 配置无关）。
    ///
    /// IDAS（kitten / mice）游戏房主恒持有 .102（awl host_flow 写入 ipNet2）；
    /// IDZ / IDAC 等 segatools 游戏单地址 .101；游戏未入库时保持隐藏。
    pub(in crate::reactor_app) fn room_uses_serverbox(&self) -> bool {
        let Some(index) = self.room_game_index() else {
            return false;
        };
        matches!(
            self.library.games.get(index).map(|r| &r.loader),
            Some(crate::storage::Loader::Kitten | crate::storage::Loader::Mice)
        )
    }

    /// 房间内直接启动：定位房间游戏后走既有启动链路（房间模式下网络强制
    /// awl0 + 房间网段，由 effective_* / room_net_override 覆盖）。游戏未添加
    /// 或 ServerBox 未配置 → toast + 跳转游戏页（房间与 awl 进程保持运行）。
    pub(in crate::reactor_app) fn room_launch(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
        serverbox: bool,
    ) {
        let Some(index) = self.room_game_index() else {
            let game = self.room_game.clone();
            self.dialog = Dialog::Error {
                message: tf!(
                    "尚未添加游戏「{0}」，已跳转游戏页，添加后即可启动",
                    "Game \"{0}\" is not in the library; open the Games page and add it to launch",
                    game
                ),
            };
            self.nav_tag = "games".to_string();
            return;
        };
        if serverbox {
            if self
                .library
                .games
                .get(index)
                .and_then(|r| r.serverbox.clone())
                .is_none()
            {
                self.dialog = Dialog::Error {
                    message: t!(
                        "该游戏未配置 ServerBox，已跳转游戏页",
                        "ServerBox is not configured for this game; opening the Games page"
                    )
                    .to_string(),
                };
                self.nav_tag = "games".to_string();
                return;
            }
            self.do_launch_serverbox(context, index);
        } else {
            self.do_launch(context, index);
        }
    }

    /// 后台刷新公开房间列表（进入房间页自动刷新 / 手动「刷新」按钮）。
    /// `with_overlay` = 手动刷新时显示 loading 覆盖层；进入页面时的自动刷新不打扰。
    pub(in crate::reactor_app) fn room_refresh_list(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
        with_overlay: bool,
    ) {
        let overlay = with_overlay;
        if overlay {
            self.room_overlay = Some(t!("正在刷新房间列表…", "Refreshing room list…").to_string());
        }
        let dir = self.dir_client();
        let filter = self.room_filter_game_id.clone();
        context.spawn_background(move |_| Message::RoomListRefreshed(dir.list_rooms(filter.as_deref()), overlay));
    }

    /// 生效昵称：设置页保存值优先；未设昵称弹窗「跳过」后的会话默认值次之；均无 = 空。
    pub(in crate::reactor_app) fn room_nickname(&self) -> String {
        if let Some(n) = self.room.nickname.as_deref() {
            let n = n.trim();
            if !n.is_empty() {
                return n.to_string();
            }
        }
        self.room_nick_override.clone().unwrap_or_default()
    }

    /// 房主：开房后台任务。`game_idx` = 创建对话框选择的游戏（db::games() 下标）。
    pub(in crate::reactor_app) fn room_start_host(
        &mut self,
        name: String,
        game_idx: Option<usize>,
        password: String,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        if self.room_status != RoomStatus::Idle {
            return;
        }
        // 未设昵称：先弹窗（前往设置 / 跳过用默认昵称）。暂挂创建参数并延迟一拍开窗——
        // 此刻仍处在创建弹窗 on_closed 的消息周期内，WinUI 同一时刻只允许一个
        // ContentDialog，立即开第二个会被静默拒绝（弹窗不出现 → 创建看似没反应）
        if self.room_nickname().is_empty() {
            self.room_nick_pending =
                Some(crate::reactor_app::message::PendingRoomAction::Create {
                    name,
                    game_idx,
                    password,
                });
            context.spawn_background(|_| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                Message::RoomNickPromptReady
            });
            return;
        }
        let name = name.trim().to_string(); // 留空时服务端用房间 ID 作为房间名
        let Some(game_idx) = game_idx else {
            self.dialog = Dialog::Error {
                message: t!("请选择游戏", "Select a game").to_string(),
            };
            return;
        };
        let games = crate::db::games();
        let Some(entry) = games.get(game_idx) else {
            self.dialog = Dialog::Error {
                message: t!("所选游戏不存在", "Selected game does not exist").to_string(),
            };
            return;
        };
        // 过滤键：games_db 的 game_id；无 game_id 的产品回退用名称（两侧同库,键一致）
        let game_id = entry.game_id.clone().unwrap_or_else(|| entry.name.clone());
        let game = entry.name.clone();
        let keychip = self.system.keychip.clone().unwrap_or_default();
        if keychip.trim().is_empty() {
            self.dialog = Dialog::Error {
                message: t!(
                    "未配置 Keychip 序列号，请先在「系统设置」页填写",
                    "Keychip serial is not configured; fill it in on the System page first"
                ).to_string(),
            };
            return;
        }
        self.room_keychip = keychip.clone();
        self.room.last_room_id = None;
        self.room.last_room_password = None;
        let _ = self.store.save_room(&self.room);
        let overlay_text = if name.is_empty() {
            t!("正在创建房间…", "Creating room…").to_string()
        } else {
            tf!("正在创建房间「{0}」…", "Creating room \"{0}\"…", name)
        };
        self.room_overlay = Some(overlay_text);
        self.room_status = RoomStatus::Creating { name: name.clone() };
        self.room_game = game.clone();
        self.room_password = password.clone();
        self.room_members_invited.clear();
        let cfg = self.room.clone();
        let dir = self.dir_client();
        let handle = self.room_handle.clone();
        context.spawn_background(move |_| {
            match AwlManager::host_flow(&cfg, &name, &game, &game_id, &password, &keychip, &dir) {
                Ok((h, room_id)) => {
                    *handle.lock().unwrap() = Some(h);
                    Message::RoomCreateResult(Ok(room_id))
                }
                Err(e) => Message::RoomCreateResult(Err(e)),
            }
        });
    }

    pub(in crate::reactor_app) fn room_on_created(
        &mut self,
        res: Result<String, String>,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        match res {
            Ok(room_id) => {
                let mut name = match &self.room_status {
                    RoomStatus::Creating { name } => name.clone(),
                    _ => String::new(),
                };
                if name.is_empty() {
                    name = room_id.clone(); // 未填房间名 → 用房间 ID 作为房间名
                }
                self.room_name = name.clone();
                self.room_status = RoomStatus::Hosting { room_id, name };
                self.room_my_ip = Some(crate::awl::HOST_GAME_IP.to_string());
                // 落盘回归记忆：房主房间重启后自动回归时据此恢复密码（显示用），
                // 否则会被误判为「公开房间」。房主回归走 keychip，不依赖 last_room_id。
                self.room.last_room_password = Some(self.room_password.clone());
                let _ = self.store.save_room(&self.room);
                        self.room_members_invited.clear();
                // 成员表长连接同步通道（房主权威）：先以空表广播，成员连入即收到
                self.room_dir_members = None;
                self.room_last_reconcile = None;
                self.room_last_playing_report = Some(self.game_running);
                let host_sync = crate::room_sync::HostSync::start();
                let host_peer = AwlManager::my_peer_id().unwrap_or_default();
                let nick = self.room_nickname();
                host_sync.publish(&host_peer, &[], self.game_running, Some(&nick));
                self.room_host_sync = Some(host_sync);
                self.room_schedule_tick(self.room_tick_interval(), context);
                self.spawn_room_watchdog();
            }
            Err(e) => {
                self.room_status = RoomStatus::Idle;
                self.dialog = Dialog::Error { message: e };
            }
        }
    }

    /// 房主进入 Hosting 时拉起房间看门狗子进程：监控本启动器进程（正常关闭 / 被强杀 /
    /// 崩溃）退出后，由看门狗调用目录服务 close 注销房间。看门狗为独立进程、不在 awl 作业
    /// 内，故强杀场景也能覆盖。开新房间 / 回归前先 kill 旧看门狗，避免重复进程残留。
    pub(in crate::reactor_app) fn spawn_room_watchdog(&mut self) {
        if !matches!(self.room_status, RoomStatus::Hosting { .. }) {
            return;
        }
        let room_id = match &self.room_status {
            RoomStatus::Hosting { room_id, .. } => room_id.clone(),
            _ => return,
        };
        let keychip = self.room_keychip.clone();
        // peer_id 缺失（极端：awl 配置丢失）时放弃拉起——关房可由成员侧失联兜底，
        // 且 close 仅需 keychip 即可放行，看门狗本身也需要父进程存活环境，这里更稳妥。
        let Ok(peer_id) = AwlManager::my_peer_id() else {
            return;
        };
        let directory = self
            .room
            .directory
            .clone()
            .unwrap_or_else(|| crate::roomdir::DEFAULT_DIRECTORY.to_string());
        // 先清旧看门狗（回归 / 重开场景）
        if let Some(mut w) = self.room_watchdog.take() {
            let _ = w.kill();
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let pid = std::process::id();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--room-watchdog")
            .arg(pid.to_string())
            .arg(&room_id)
            .arg(&keychip)
            .arg(&peer_id)
            .arg(&directory);
        // 隐藏窗口 + 与任何作业隔离（避免被误纳入作业随父进程一并回收）
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000 | 0x0100_0000); // CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB
        if let Ok(child) = cmd.spawn() {
            self.room_watchdog = Some(child);
        }
    }

    /// 成员：加入后台任务。
    pub(in crate::reactor_app) fn room_start_join(
        &mut self,
        room_id: String,
        password: String,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        if self.room_status != RoomStatus::Idle {
            return;
        }
        // 未设昵称：先弹窗（前往设置 / 跳过用默认昵称）。暂挂加入参数并延迟一拍开窗
        // （同 room_start_host：避免在加入弹窗 on_closed 周期内开第二个 ContentDialog）
        if self.room_nickname().is_empty() {
            self.room_nick_pending =
                Some(crate::reactor_app::message::PendingRoomAction::Join {
                    room_id,
                    password,
                });
            context.spawn_background(|_| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                Message::RoomNickPromptReady
            });
            return;
        }
        let room_id = room_id.trim().to_uppercase();
        if room_id.is_empty() {
            self.dialog = Dialog::Error {
                message: t!("请填写房间 ID", "Enter a room ID").to_string(),
            };
            return;
        }
        self.room.last_room_password = Some(password.clone());
        // 暂存加入密码：room_on_joined 落盘回归记忆与视图回退都依赖它（否则私密房密码被空串覆盖）
        self.room_join_password = password.clone();
        self.room_overlay = Some(t!("正在加入房间…", "Joining room…").to_string());
        self.room_status = RoomStatus::Joining { room_id: room_id.clone() };
        let cfg = self.room.clone();
        let dir = self.dir_client();
        let handle = self.room_handle.clone();
        context.spawn_background(move |_| match AwlManager::join_flow(&cfg, &room_id, &password, &dir) {
            Ok((h, join)) => {
                *handle.lock().unwrap() = Some(h);
                Message::RoomJoinResult(Ok(join))
            }
            Err(e) => Message::RoomJoinResult(Err(e)),
        });
    }

    pub(in crate::reactor_app) fn room_on_joined(
        &mut self,
        res: Result<crate::roomdir::JoinInfo, String>,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        match res {
            Ok(join) => {
                let room_id = match &self.room_status {
                    RoomStatus::Joining { room_id } => room_id.clone(),
                    _ => String::new(),
                };
                // 保存回归记忆（必须是房间 ID 而非房间名）：成员未退出直接关闭启动器后，
                // 下次进店内对战自动用 ID 重入（join 对同 peer_id 幂等，返回原座位）
                self.room.last_room_id = Some(room_id.clone());
                self.room.last_room_password = Some(self.room_join_password.clone());
                let _ = self.store.save_room(&self.room);
                self.room_status = RoomStatus::Joined { room_id: room_id.clone(), is_host: false };
                self.room_my_ip = Some(join.your_ip);
                self.room_game = join.game;
                // 旧服务端可能不带 name：回退用房间 ID（房主未填名时本就是 ID）
                self.room_name = if join.name.is_empty() { room_id } else { join.name };
                self.room_host_down = None;
                // 成员侧同步通道：吃房主长连接快照（无目录兜底轮询）
                self.room_sync_version = 0;
                self.room_sync_table = None;
                self.room_last_playing_report = None;
                self.room_last_status_sent = None;
                self.room_member_sync = Some(crate::room_sync::MemberSync::start(
                    AwlManager::my_peer_id().unwrap_or_default(),
                    self.room_nickname(),
                ));
                self.room_schedule_tick(self.room_tick_interval(), context);
            }
            Err(e) => {
                self.room_status = RoomStatus::Idle;
                // 加入失败：清掉上次回归记忆（密码可能已失效，重试需手动输入）
                self.room.last_room_id = None;
                self.room.last_room_password = None;
                let _ = self.store.save_room(&self.room);
                self.dialog = Dialog::Error { message: e };
            }
        }
    }

    /// 离开（成员）/ 关闭（房主）：收起句柄，后台完成目录注销 + 清 peers + 杀进程。
    pub(in crate::reactor_app) fn room_stop(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        // 房主主动关房：终止看门狗（房间已由本函数后台注销，无需它再动作）
        if let Some(mut w) = self.room_watchdog.take() {
            let _ = w.kill();
        }
        let (is_host, room_id) = match &self.room_status {
            RoomStatus::Hosting { room_id, .. } => (true, room_id.clone()),
            RoomStatus::Joined { room_id, is_host } => (*is_host, room_id.clone()),
            _ => return,
        };
        self.room_status = RoomStatus::Idle;
        self.room_members_invited.clear();
        self.room_member_down.clear();
        self.room_member_ever_connected.clear();
        self.room_seat_ips.clear();
        self.room_last_heartbeat = None;
        self.room_host_down = None;
        self.room_my_ip = None;
        self.room_saw_self_in_table = false;
        self.room_nick_pending = None;
        self.room_game.clear();
        self.room_name.clear();
        // 房主关房：先经同步通道广播 close 帧（成员收到立即退出），再释放通道
        if is_host {
            if let Some(sync) = self.room_host_sync.as_ref() {
                sync.notify_close();
                // 留出广播窗口：连接线程 500ms 轮询内发完 close 帧
                std::thread::sleep(std::time::Duration::from_millis(600));
            }
        }
        // 同步通道与事件驱动状态一并释放（Drop 停线程）
        self.room_host_sync = None;
        self.room_member_sync = None;
        self.room_sync_version = 0;
        self.room_sync_table = None;
        self.room_dir_members = None;
        self.room_last_reconcile = None;
        self.room_last_playing_report = None;
        self.room_last_status_sent = None;
        self.room.last_room_id = None;
        self.room.last_room_password = None;
        let _ = self.store.save_room(&self.room);
        // 覆盖层过渡：房间视图 → 转圈覆盖层（房主「正在关闭房间…」/ 成员「正在退出房间…」），
        // 后台目录注销 + 清 peers + 杀 awl 完成后由 RoomFinished 收起、回到大厅
        self.room_overlay = Some(if is_host {
            t!("正在关闭房间…", "Closing room…").to_string()
        } else {
            t!("正在退出房间…", "Leaving room…").to_string()
        });
        let handle = self.room_handle.lock().unwrap().take();
        let dir = self.dir_client();
        // keychip 是房间归属键（服务端稳定校验）：awl 身份重铸后仍能关闭自己的房间
        let keychip = self.room_keychip.clone();
        context.spawn_background(move |_| {
            // 关房 / 退出结果不再静默：失败经 RoomFinished 回灌 toast
            // （否则目录上房间残留，还会被「自动回归」重新拉回房间）
            let my = AwlManager::my_peer_id().ok();
            // 404 = 房间在服务端已不存在（房主早已关闭）：不算操作失败，提示「房间已关闭」
            let friendly = |e: String| {
                if e == "HTTP 404" {
                    t!("房间已关闭", "The room has been closed").to_string()
                } else {
                    e
                }
            };
            let res = if is_host {
                dir.close(&room_id, my.as_deref(), &keychip).map_err(|e| {
                    friendly(tf!("关闭房间失败：{}", "Failed to close the room: {}", e))
                })
            } else {
                match my {
                    Some(peer) => dir.leave(&room_id, &peer).map_err(|e| {
                        friendly(tf!("退出房间失败：{}", "Failed to leave the room: {}", e))
                    }),
                    None => Err(t!("未找到本机 awl 身份，无法退出房间",
                                   "Local awl identity not found; cannot leave the room").to_string()),
                }
            };
            AwlManager::remove_all_peers();
            drop(handle);
            Message::RoomFinished(res)
        });
    }

    /// 进入店内对战页时的自动回归：
    /// 优先房主（keychip 名下有活跃房间 → 原房直接拉起 awl）；
    /// 其次成员（上次加入的房间仍在 → 原座位重入,join 对同 peer_id 幂等）。
    /// 都没有 → 停留大厅。keychip 未配置但有待回归房间时,成员回归不需要 keychip。
    pub(in crate::reactor_app) fn room_try_resume(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        if self.room_status != RoomStatus::Idle {
            return;
        }
        let keychip = self.system.keychip.clone().unwrap_or_default();
        let has_keychip = !keychip.trim().is_empty();
        let last_room_id = self.room.last_room_id.clone().unwrap_or_default();
        let last_password = self.room.last_room_password.clone().unwrap_or_default();
        let has_last = !last_room_id.trim().is_empty();
        if !has_keychip && !has_last {
            return;
        }
        if has_keychip {
            self.room_keychip = keychip.clone();
        }
        // 成员回归：先置 Joining（join 完成后 room_on_joined 从这里取回房间 ID）。
        // 同步暂存密码：room_on_joined 落盘时依赖 room_join_password，否则会被启动时的空串覆盖，
        // 导致下次重入时 last_room_password 为空、服务端返回「密码错误」。
        if has_last {
            self.room_join_password = last_password.clone();
            self.room_status = RoomStatus::Joining { room_id: last_room_id.clone() };
        }
        self.room_overlay = Some(t!("正在回归房间…", "Returning to your room…").to_string());
        let cfg = self.room.clone();
        let dir = self.dir_client();
        let handle = self.room_handle.clone();
        context.spawn_background(move |_| {
            // 优先:房主回归(keychip 名下有活跃房间 → 原房直接拉起)
            if has_keychip {
                match dir.my_room(&keychip) {
                    Ok(Some(room)) => {
                        // 按房名从识别库反查 game_id：IDZ/IDAC 房间回归时房主同样单地址
                        let game_id = crate::db::games()
                            .into_iter()
                            .find(|g| g.name == room.game)
                            .and_then(|g| g.game_id);
                        match AwlManager::host_resume_flow(&cfg, game_id.as_deref()) {
                            Ok(h) => {
                                *handle.lock().unwrap() = Some(h);
                                return Message::RoomResumeResult(Ok(Some(room)));
                            }
                            Err(e) => return Message::RoomResumeResult(Err(e)),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => return Message::RoomResumeResult(Err(e)),
                }
            }
            // 成员:上次加入的房间仍在 → 原座位重入(join 对同 peer_id 幂等)
            if has_last {
                match AwlManager::join_flow(&cfg, &last_room_id, &last_password, &dir) {
                    Ok((h, join)) => {
                        *handle.lock().unwrap() = Some(h);
                        return Message::RoomJoinResult(Ok(join));
                    }
                    Err(e) => return Message::RoomJoinResult(Err(e)),
                }
            }
            Message::RoomResumeResult(Ok(None))
        });
    }

    /// 房间本地观测拍：房主=事件驱动目录对账（未知 peer/60s 节流）+ 长连接广播
    /// （成员表+游玩状态）；成员=消费房主快照（全互联 invite/被踢判定）+
    /// 房主失联检测 + 游玩状态上报。目录请求全部事件化，无周期轮询。
    pub(in crate::reactor_app) fn room_on_tick(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        // 刷新房间内成员视图快照（UI 成员列表直接读该字段）
        self.room_peers = AwlManager::known_peers();
        match self.room_status.clone() {
            RoomStatus::Hosting { room_id, .. } => {
                let dir = self.dir_client();
                // 事件驱动对账：无缓存 / 本地 awl 出现未知已连 peer / 60s 节流到点
                // 才拉一次目录成员表；其余拍只做本地观测与广播，目录零流量。
                let unknown_connected = match &self.room_dir_members {
                    Some(cache) => {
                        let known: HashSet<&str> =
                            cache.members.iter().map(|m| m.peer_id.as_str()).collect();
                        self.room_peers.iter().any(|p| {
                            p.connected && p.alias != "host" && !known.contains(p.peer_id.as_str())
                        })
                    }
                    None => true,
                };
                let reconcile_due = self
                    .room_last_reconcile
                    .map_or(true, |t| t.elapsed() >= Duration::from_secs(60));
                if unknown_connected || reconcile_due {
                    self.room_last_reconcile = Some(Instant::now());
                    match dir.members(&room_id) {
                        Ok(members) => self.room_dir_members = Some(members),
                        Err(_) => {} // 目录不可达：维持缓存，节流后重试
                    }
                }
                if let Some(cache) = self.room_dir_members.clone() {
                    let mut ids: HashSet<String> =
                        cache.members.iter().map(|m| m.peer_id.clone()).collect();
                    ids.insert(cache.host_peer_id.clone());
                    // 成员掉线按「离开房间」处理（宽限计时为本地 Instant，语义不变）：
                    // 曾建连的走短宽限（掉线=断连），从未建连的走长宽限（建链进行中）。
                    // 超限代发一次 leave + 从权威缓存剔除，快照随之广播。
                    let mut down_now: HashMap<String, Instant> = HashMap::new();
                    let mut kicked = false;
                    for m in &cache.members {
                        let connected = self
                            .room_peers
                            .iter()
                            .find(|p| p.peer_id == m.peer_id)
                            .map(|p| p.connected)
                            .unwrap_or(false);
                        if connected {
                            self.room_member_ever_connected.insert(m.peer_id.clone());
                            continue;
                        }
                        let grace = if self.room_member_ever_connected.contains(&m.peer_id) {
                            MEMBER_KICK_AFTER
                        } else {
                            MEMBER_JOIN_GRACE
                        };
                        let since = match self.room_member_down.get(&m.peer_id) {
                            Some(t) => *t,
                            None => {
                                down_now.insert(m.peer_id.clone(), Instant::now());
                                continue;
                            }
                        };
                        if since.elapsed() >= grace {
                            let _ = dir.leave(&room_id, &m.peer_id);
                            ids.remove(&m.peer_id);
                            kicked = true;
                        } else {
                            down_now.insert(m.peer_id.clone(), since);
                        }
                    }
                    self.room_member_down = down_now;
                    let mut cache = cache;
                    if kicked {
                        cache.members.retain(|m| ids.contains(&m.peer_id));
                        self.room_dir_members = Some(cache.clone());
                    }
                    // 已不在目录的成员清掉「曾建连」标记：重入按新一次加入重新计时
                    self.room_member_ever_connected.retain(|p| ids.contains(p));
                    // 新成员 invite（幂等；座位 IP 来自目录权威分配）
                    for m in &cache.members {
                        if !ids.contains(&m.peer_id) {
                            continue; // 已按掉线移出：不再邀请、不再展示
                        }
                        if !self.room_members_invited.iter().any(|p| p == &m.peer_id) {
                            if AwlManager::invite_member(&m.peer_id, &m.ip).is_ok() {
                                self.room_members_invited.push(m.peer_id.clone());
                            }
                        }
                    }
                    self.room_peers.retain(|p| ids.contains(&p.peer_id));
                    self.room_members_invited.retain(|p| ids.contains(p));
                    // 机位快照整体重建（以目录成员表为权威）：座位 IP → UI「机位」列
                    self.room_seat_ips = cache
                        .members
                        .iter()
                        .map(|m| (m.peer_id.clone(), m.ip.clone()))
                        .chain(std::iter::once((
                            cache.host_peer_id.clone(),
                            crate::awl::HOST_GAME_IP.to_string(),
                        )))
                        .collect();
                    // 长连接广播：HostSync 内部 diff（成员集/座位/任一游玩状态变化才推）
                    if let Some(sync) = self.room_host_sync.as_ref() {
                        let pairs: Vec<(String, String)> = cache
                            .members
                            .iter()
                            .map(|m| (m.peer_id.clone(), m.ip.clone()))
                            .collect();
                        let nick = self.room_nickname();
                        sync.publish(&cache.host_peer_id, &pairs, self.game_running, Some(&nick));
                    }
                    // 房主保活：每 5 分钟向目录刷新一次 last_heartbeat，
                    // 避免服务端 1h 无保活自动关房（仅目录可达时发，失败忽略）。
                    if self
                        .room_last_heartbeat
                        .map_or(true, |t| t.elapsed() >= Duration::from_secs(300))
                    {
                        if dir.heartbeat(&room_id, &self.room_keychip).is_ok() {
                            self.room_last_heartbeat = Some(Instant::now());
                        }
                    }
                }
                self.room_schedule_tick(self.room_tick_interval(), context);
            }
            RoomStatus::Joined { room_id: _, is_host: false } => {
                // 房主已关闭房间（收到 close 帧）→ 立即退出（优先于掉线超时兜底）
                if let Some(sync) = self.room_member_sync.as_ref() {
                    if sync.is_closed() {
                        self.room_stop(context);
                        self.dialog = Dialog::Error {
                            message: t!(
                                "房主已关闭房间",
                                "The host has closed the room"
                            )
                            .to_string(),
                        };
                        return;
                    }
                }
                // 消费房主快照（版本去重）：被踢判定 / 全互联 invite / 成员与机位快照。
                // 无目录兜底轮询：房主断开 = 房间断开（awl 数据面随房主启动器一起消失）。
                if let Some(sync) = self.room_member_sync.as_ref() {
                    if let Some(table) = sync.take_snapshot() {
                        if table.version != self.room_sync_version {
                            self.room_sync_version = table.version;
                            self.room_sync_table = Some(table.clone());
                            let my = AwlManager::my_peer_id().ok();
                            let self_in_table = my
                                .as_deref()
                                .map(|my| {
                                    my == table.host_peer_id
                                        || table.members.iter().any(|m| m.peer_id == my)
                                })
                                .unwrap_or(false);
                            if self_in_table {
                                self.room_saw_self_in_table = true;
                            }
                            // 被踢判定：本机曾在快照成员表中出现过、当前缺失 =
                            // 房主已移出。首次加入时房主对账（事件驱动 + 60s 节流）
                            // 可能尚未把新成员纳入广播表，首拍缺自己不算被踢，
                            // 否则新成员会被误判「你已被移出房间」直接踢出。
                            if !self_in_table
                                && self.room_saw_self_in_table
                                && self.nav_tag == "room"
                            {
                                // 掉线后座位已被房主释放（快照里没有自己）→ 退出回大厅，
                                // 重入时按新顺位分配座位
                                self.room_stop(context);
                                self.dialog = Dialog::Error {
                                    message: t!(
                                        "你已被移出房间",
                                        "You have been removed from the room"
                                    )
                                    .to_string(),
                                };
                                return;
                            }
                            // invite / 机位快照：本机在表中（或 peer_id 读取失败，保守处理）时执行
                            let still_member = my.is_none() || self_in_table;
                            if still_member {
                                // 成员全互联对账：把表里其他成员逐个 invite 进本机 awl
                                // （对方 autoAccept 自动接受；invite_member 含座位 IP 校正）
                                if let Some(my) = &my {
                                    for m in &table.members {
                                        if m.peer_id == *my
                                            || self
                                                .room_members_invited
                                                .iter()
                                                .any(|p| p == &m.peer_id)
                                        {
                                            continue;
                                        }
                                        if AwlManager::invite_member(&m.peer_id, &m.ip).is_ok() {
                                            self.room_members_invited.push(m.peer_id.clone());
                                        }
                                    }
                                }
                                // 以房主快照为权威剔除已离开的成员；保留房主 peer_id；
                                // invited 集合同步剔除（重入者换新座位，须允许重新 invite 校正 IP）
                                let mut ids: HashSet<String> =
                                    table.members.iter().map(|m| m.peer_id.clone()).collect();
                                ids.insert(table.host_peer_id.clone());
                                self.room_peers.retain(|p| ids.contains(&p.peer_id));
                                self.room_members_invited.retain(|p| ids.contains(p));
                                // 机位快照整体重建（含房主 .101=A1 与全部成员座位 IP）
                                self.room_seat_ips = table
                                    .members
                                    .iter()
                                    .map(|m| (m.peer_id.clone(), m.ip.clone()))
                                    .chain(std::iter::once((
                                        table.host_peer_id.clone(),
                                        crate::awl::HOST_GAME_IP.to_string(),
                                    )))
                                    .collect();
                            }
                        }
                    }
                }
                // 房主失联检测（本地 awl；仅房间页生效）——断开即「房间已断开」，
                // 不做目录兜底轮询：房主启动器退出会连带杀掉 awl，数据面必然已不可用
                if self.nav_tag != "room" {
                    self.room_schedule_tick(self.room_tick_interval(), context);
                    return;
                }
                let host_connected = self
                    .room_peers
                    .iter()
                    .find(|p| p.alias == "host")
                    .map(|p| p.connected)
                    .unwrap_or(false);
                if host_connected {
                    self.room_host_down = None;
                } else {
                    let since = *self.room_host_down.get_or_insert(Instant::now());
                    if since.elapsed() >= HOST_DOWN_TIMEOUT {
                        self.room_stop(context);
                        self.dialog = Dialog::Error {
                            message: t!("房间已断开", "Room disconnected").to_string(),
                        };
                        return;
                    }
                }
                // 游玩状态上报：变化即发 + 30s 保活（兼作连接活性心跳）
                let playing = self.game_running;
                let keepalive_due = self
                    .room_last_status_sent
                    .map_or(true, |t| t.elapsed() >= Duration::from_secs(30));
                if self.room_last_playing_report != Some(playing) || keepalive_due {
                    if let Some(sync) = self.room_member_sync.as_ref() {
                        sync.send_status(playing);
                    }
                    self.room_last_playing_report = Some(playing);
                    self.room_last_status_sent = Some(Instant::now());
                }
                self.room_schedule_tick(self.room_tick_interval(), context);
            }
            _ => {
                // 非 Hosting/Joined 状态：不再续拍
            }
        }
    }

    /// 当前一拍应使用的本地观测间隔（awl API / 收件箱 / 状态上报）：
    /// 在房间页 5s / 离页 15s；目录请求已事件化，不再轮询、不再退避。
    pub(in crate::reactor_app) fn room_tick_interval(&self) -> Duration {
        if self.nav_tag == "room" {
            TICK_INTERVAL_IN_ROOM
        } else {
            TICK_INTERVAL_OFF_PAGE
        }
    }

    pub(in crate::reactor_app) fn room_schedule_tick(&self, interval: Duration, context: &windows_reactor::ComponentContext<Self>) {
        context.spawn_background(move |_| {
            std::thread::sleep(interval);
            Message::RoomTick
        });
    }
}

// ---------- 页面 UI ----------

/// 机位名（成员表格「机位」列）：房主 = A1；成员按目录分配的座位 IP 末字节
/// 映射 A2/B1/B2（成员座位依次 .103/.104/.105，.102 为房主 ServerBox 不占成员位）。
/// 目录按「最低空闲位」分配并回收座位：某机位离开后座位即时回收，新加入者
/// 优先补最低空闲位——A2 离开后新玩家先补 A2，其次 B1、B2。
/// 座位未知（首拍成员表尚未返回 / 别名缺失）时显示 "?"。
fn seat_label(seat_ip: Option<&str>, alias: &str) -> String {
    let octet = seat_ip
        .and_then(|ip| ip.rsplit('.').next().and_then(|s| s.parse::<u8>().ok()))
        // 兜底：房主邀请时把成员别名写成 "join-{座位末字节}"（见 awl::invite_member）
        .or_else(|| alias.strip_prefix("join-").and_then(|s| s.parse::<u8>().ok()));
    match octet.and_then(seat_name_of_octet) {
        Some(name) => name.to_string(),
        None => "?".to_string(),
    }
}

/// 座位 IP 末字节 → 机位名（房间编址：房主 .101=A1；成员 .103/.104/.105 = A2/B1/B2；
/// .102 原为房主 ServerBox 不占成员位）。未知末字节返回 None。
/// 房间模式下启动 segatools 游戏时据此自动设置 dipsw 机位（见 launch.rs）。
pub(in crate::reactor_app) fn seat_name_of_octet(octet: u8) -> Option<&'static str> {
    match octet {
        101 => Some("A1"),
        103 => Some("A2"),
        104 => Some("B1"),
        105 => Some("B2"),
        _ => None,
    }
}

/// 成员表格行：机位 / 成员 / 地址 / 状态 / 游玩 / 延迟 / 链路（返回该行的全部单元格）。
fn member_cells(
    row: i32,
    seat: &str,
    alias: &str,
    ip: &str,
    connected: bool,
    playing_text: String,
    ping_text: String,
    link_text: String,
) -> Vec<KeyedView> {
    let dot_color = if connected {
        Color::argb(255, 0x6C, 0xCB, 0x5F) // 绿 = 在线
    } else {
        Color::argb(255, 0xFF, 0x99, 0xA4) // 红 = 离线
    };
    // 行/列定位必须设在 Grid 的直接子元素（这里的 Border）上，
    // 内层 v 上的 grid_row/grid_column 不会生效，否则所有格子会叠在 (0,0)。
    let cell = |col: i32, v: View| -> KeyedView {
        KeyedView::new(
            format!("m{row}c{col}"),
            Border::new()
                .margin(Thickness::new(0.0, 8.0, 0.0, 8.0))
                .grid_row(row)
                .grid_column(col)
                .content(v),
        )
    };
    vec![
        cell(0, View::from(
            TextBlock::new()
                .text(seat.to_string())
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center)
                .text_trimming(TextTrimming::CharacterEllipsis),
        )),
        cell(1, View::from(
            TextBlock::new()
                .text(alias.to_string())
                .vertical_alignment(VerticalAlignment::Center)
                .text_trimming(TextTrimming::CharacterEllipsis),
        )),
        cell(2, View::from(
            TextBlock::new()
                .text(ip.to_string())
                .font_size(12.0)
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(3, View::from(
            Border::new()
                .width(11.0)
                .height(11.0)
                .corner_radius(5.5)
                .background(Brush::Solid(dot_color))
                .horizontal_alignment(HorizontalAlignment::Left)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(4, View::from(
            TextBlock::new()
                .text(playing_text)
                .font_size(12.0)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(5, View::from(
            TextBlock::new()
                .text(ping_text)
                .font_size(12.0)
                .opacity(0.8)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(6, View::from(
            TextBlock::new()
                .text(link_text)
                .font_size(12.0)
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center),
        )),
    ]
}

/// 游玩状态文案：Some(状态) → 空闲/游玩中；None（快照未知）→ "—"。
fn playing_label(playing: Option<bool>) -> String {
    match playing {
        Some(true) => t!("游玩中", "Playing").to_string(),
        Some(false) => t!("空闲", "Idle").to_string(),
        None => "—".to_string(),
    }
}

impl Launcher {
    /// 店内对战页：大厅（Idle/创建中/加入中）→ 公开房间列表；房间内 → 房间视图。
    pub(in crate::reactor_app) fn room_page(&self, context: &mut ViewContext<Self>) -> View {
        match &self.room_status {
            RoomStatus::Hosting { .. } | RoomStatus::Joined { .. } => self.room_active_view(context),
            _ => self.room_lobby_view(context),
        }
    }

    /// 页头：左侧标题 + 副标题，右侧按钮组（「网络设置」页同款布局）。
    fn room_header(&self, _context: &mut ViewContext<Self>, buttons: View) -> View {
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .children((
                StackPanel::new().spacing(4.0).grid_column(0).children((
                    TextBlock::new()
                        .text(t!("店内对战", "In-Store Battles"))
                        .font_size(26.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new()
                        .text(t!(
                            "创建或加入房间，与好友联机店内对战",
                            "Create or join a room and play LAN games with friends"
                        ))
                        .font_size(12.0)
                        .opacity(0.6),
                )),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .children((buttons,)),
            ))
            .into()
    }

    /// 大厅视图（未在房间）：右上角创建/加入/刷新 + 公开房间列表表格。
    fn room_lobby_view(&self, context: &mut ViewContext<Self>) -> View {
        let idle = self.room_status == RoomStatus::Idle;
        let busy = !idle;

        let header_buttons: View = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(idle)
                    .on_click(context.message(Message::RoomOpenCreateDialog))
                    .content(t!("创建…", "Create…")),
                Button::new()
                    .is_enabled(idle)
                    .on_click(context.message(Message::RoomOpenJoinDialog))
                    .content(t!("加入…", "Join…")),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .is_enabled(idle)
                    .on_click(context.message(Message::RoomRefreshList))
                    .content(t!("刷新", "Refresh")),
            ))
            .into();

        // 连接中提示条（创建中/加入中时显示）
        let busy_hint: View = if busy {
            TextBlock::new()
                .text(match &self.room_status {
                    RoomStatus::Creating { name } => {
                        tf!("正在创建房间「{name}」…", "Creating room \"{name}\"…")
                    }
                    RoomStatus::Joining { room_id } => {
                        tf!("正在加入房间 {room_id}…", "Joining room {room_id}…")
                    }
                    _ => String::new(),
                })
                .font_size(12.0)
                .opacity(0.7)
                .into()
        } else {
            StackPanel::new().into()
        };

        // 公开房间列表表格：房间名称 / 游戏 / 人数 / 操作
        // 表头行与数据行放在同一个 Grid 里共享列宽（Auto 列联合测量），保证严格对齐
        let n = self.room_list.len();
        let mut row_defs = vec![GridLength::Auto]; // 0: 表头
        row_defs.push(GridLength::Pixel(1.0)); // 1: 分隔线
        for _ in 0..n {
            row_defs.push(GridLength::Auto); // 2..: 数据行
        }

        let mut cells: Vec<KeyedView> = Vec::new();
        let headers = [
            t!("房间名称", "Room").to_string(),
            t!("游戏", "Game").to_string(),
            t!("人数", "Players").to_string(),
        ];
        for (c, txt) in headers.iter().enumerate() {
            cells.push(KeyedView::new(format!("h{c}"), View::from(
                TextBlock::new()
                    .text(txt.clone())
                    .font_size(12.0)
                    .opacity(0.6)
                    .grid_row(0)
                    .grid_column(c as i32)
                    .vertical_alignment(VerticalAlignment::Center)
                    )));
        }
        // 表头下的全宽分隔线
        cells.push(KeyedView::new("sep", View::from(
            Border::new()
                .height(1.0)
                .margin(Thickness::new(0.0, 4.0, 0.0, 4.0))
                .background(ThemeBrush::CardStroke)
                .grid_row(1)
                .grid_column(0)
                .grid_column_span(4)
                )));

        for (i, r) in self.room_list.iter().enumerate() {
            let row = (i + 2) as i32;
            let room_id = r.room_id.clone();
            let (players_color, join_label) = if r.max > r.players {
                (Color::argb(255, 0x6C, 0xCB, 0x5F), t!("加入", "Join").to_string())
            } else {
                (Color::argb(255, 0xFF, 0x99, 0xA4), t!("已满", "Full").to_string())
            };
            cells.push(KeyedView::new(format!("r{row}c0"), View::from(
                TextBlock::new()
                    .text(r.name.clone())
                    .grid_row(row)
                    .grid_column(0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    )));
            cells.push(KeyedView::new(format!("r{row}c1"), View::from(
                TextBlock::new()
                    .text(r.game.clone())
                    .font_size(12.0)
                    .opacity(0.8)
                    .grid_row(row)
                    .grid_column(1)
                    .vertical_alignment(VerticalAlignment::Center)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    )));
            cells.push(KeyedView::new(format!("r{row}c2"), View::from(
                TextBlock::new()
                    .text(format!("{}/{}", r.players, r.max))
                    .font_size(12.0)
                    .foreground(Brush::Solid(players_color))
                    .grid_row(row)
                    .grid_column(2)
                    .vertical_alignment(VerticalAlignment::Center)
                    )));
            cells.push(KeyedView::new(format!("r{row}c3"), View::from(
                Button::new()
                    .is_enabled(idle && r.max > r.players)
                    .on_click(context.message(Message::RoomJoinDirect(room_id)))
                    .grid_row(row)
                    .grid_column(3)
                    .content(join_label)
                    )));
        }

        let list_body: View = if n == 0 {
            StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Center)
                .spacing(8.0)
                .margin(Thickness::new(0.0, 24.0, 0.0, 24.0))
                .children((
                    FontIcon::new()
                        .glyph("\u{E70A}")
                        .opacity(0.4),
                    TextBlock::new()
                        .text(t!(
                            "暂无公开房间，点右上角「创建…」开一个",
                            "No public rooms yet; click \"Create...\" to open one"
                        ))
                        .opacity(0.6),
                ))
                .into()
        } else {
            Grid::new()
                .columns([
                    GridLength::Star(1.2),
                    GridLength::Star(1.4),
                    GridLength::Pixel(56.0),
                    GridLength::Pixel(96.0),
                ])
                .rows(row_defs)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .keyed_children(cells)
                .into()
        };

        let list_card = Border::new()
            .margin(Thickness::new(0.0, 16.0, 0.0, 0.0))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(Thickness::new(16.0, 8.0, 16.0, 12.0))
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .children((
                        settings_row(
                            View::from(SymbolIcon::new().symbol(Symbol::List)),
                            t!("公开房间", "Public rooms"),
                            t!("按游戏筛选房间", "Filter rooms by game"),
                            View::from(
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(8.0)
                                    .children((
                                        ComboBox::new()
                                            .width(280.0)
                                            .items_source({
                                                let mut items = vec![t!("全部游戏", "All games").to_string()];
                                                items.extend(crate::db::game_names());
                                                items
                                            })
                                            .selected_index(
                                                self.room_filter_idx.map(|i| i + 1),
                                            )
                                            .placeholder_text(t!("全部游戏", "All games"))
                                            .on_selection_changed(
                                                context.callback(Message::RoomFilterChanged),
                                            )
                                            .vertical_alignment(VerticalAlignment::Center),
                                        Button::new()
                                            .style(ButtonStyle::Subtle)
                                            .is_enabled(!busy)
                                            .on_click(context.message(Message::RoomRefreshList))
                                            .content(t!("刷新", "Refresh")),
                                    )),
                            ),
                        ),
                        list_body,
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
                        self.room_header(context, header_buttons),
                        busy_hint,
                        list_card,
                    )),
            )
            .into()
    }

    /// 房间内视图（房主/成员已进入房间）：状态 + 信息 + 成员 + 离开/关闭。
    fn room_active_view(&self, context: &mut ViewContext<Self>) -> View {
        let (is_host, room_id) = match &self.room_status {
            RoomStatus::Hosting { room_id, .. } => (true, room_id.clone()),
            RoomStatus::Joined { room_id, .. } => (false, room_id.clone()),
            _ => return StackPanel::new().into(),
        };
        // 房间密码：房主取内存中的 room_password；成员取回归记忆 / 加入时暂存的密码
        let room_pw = if is_host {
            self.room_password.clone()
        } else {
            self.room
                .last_room_password
                .clone()
                .unwrap_or_else(|| self.room_join_password.clone())
        };
        let pw_display: String = if room_pw.trim().is_empty() {
            t!("无（公开房间）", "None (public room)").to_string()
        } else {
            room_pw
        };
        // ServerBox 地址行按游戏形态门控：IDAS（kitten / mice）房主恒有 .102
        // 副地址；IDAC / IDZ（segatools）单地址 .101。启动按钮另按 ServerBox
        // 程序配置门控（has_serverbox）。
        let has_serverbox_addr = self.room_uses_serverbox();
        let has_serverbox = self.room_has_serverbox();
        // 标题统一用房间名（房主=开房时的名字,成员=join 返回的名字,空名时均为房间 ID）

        // 状态灯
        let (status_text, sr, sg, sb) = match &self.room_status {
            RoomStatus::Hosting { .. } => {
                (t!("房间开放中", "Room open").to_string(), 0x6C, 0xCB, 0x5F)
            }
            RoomStatus::Joined { .. } => {
                (t!("已加入", "Joined").to_string(), 0x6C, 0xCB, 0x5F)
            }
            _ => (t!("处理中…", "Working…").to_string(), 0x9E, 0x9E, 0x9E),
        };

        // 成员表格：本机排第一，其后为远端成员（单 Grid 共享列宽,严格对齐）。
        // 机位列：房主 A1，成员按座位 IP 末字节映射 A2/B1/B2（见 seat_label）
        // 列宽：各列平均分配（等宽 Star）
        let mcols = [
            GridLength::Star(1.0), // 机位
            GridLength::Star(1.0), // 成员
            GridLength::Star(1.0), // 地址
            GridLength::Star(1.0), // 状态（在线圆点）
            GridLength::Star(1.0), // 游玩
            GridLength::Star(1.0), // 延迟
            GridLength::Star(1.0), // 链路
        ];
        let mut mcells: Vec<KeyedView> = Vec::new();
        let mheaders = [
            t!("机位", "Seat").to_string(),
            t!("成员", "Member").to_string(),
            t!("地址", "Address").to_string(),
            t!("状态", "Status").to_string(),
            t!("游玩", "Play").to_string(),
            t!("延迟", "Ping").to_string(),
            t!("链路", "Link").to_string(),
        ];
        for (c, txt) in mheaders.iter().enumerate() {
            mcells.push(KeyedView::new(
                format!("mh{c}"),
                View::from(
                    TextBlock::new()
                        .text(txt.clone())
                        .font_size(12.0)
                        .opacity(0.6)
                        .margin(Thickness::new(0.0, 4.0, 0.0, 4.0))
                        .grid_row(0)
                        .grid_column(c as i32)
                        .vertical_alignment(VerticalAlignment::Center),
                ),
            ));
        }
        let self_ip = self.room_my_ip.clone().unwrap_or_default();
        // 本机机位：房主恒 A1；成员按目录分配给自己的座位 IP 映射
        let self_seat = if is_host {
            "A1".to_string()
        } else {
            seat_label(self.room_my_ip.as_deref(), "")
        };
        // 表头下的全宽分隔线
        mcells.push(KeyedView::new(
            "msep",
            View::from(
                Border::new()
                    .height(1.0)
                    .margin(Thickness::new(0.0, 4.0, 0.0, 4.0))
                    .background(ThemeBrush::CardStroke)
                    .grid_row(1)
                    .grid_column(0)
                    .grid_column_span(7),
            ),
        ));
        // 「游玩」列数据源：房主 = HostSync 聚合的成员上报状态；
        // 成员 = 最近收到的房主快照（含房主自身状态）。本机行按本地 game_running。
        let playing_map: HashMap<String, bool> = if is_host {
            self.room_host_sync
                .as_ref()
                .map(|s| s.member_playing())
                .unwrap_or_default()
        } else {
            match &self.room_sync_table {
                Some(t) => t
                    .members
                    .iter()
                    .map(|m| (m.peer_id.clone(), m.playing))
                    .chain(std::iter::once((t.host_peer_id.clone(), t.host_playing)))
                    .collect(),
                None => HashMap::new(),
            }
        };
        // 本机行「成员」列：昵称 +（本机/房主）标记（昵称缺失回退默认昵称）
        let self_nick = {
            let n = self.room_nickname();
            if n.is_empty() {
                ROOM_DEFAULT_NICKNAME.to_string()
            } else {
                n
            }
        };
        let self_name = if is_host {
            tf!("{0}（房主·本机）", "{0} (host, this PC)", self_nick)
        } else {
            tf!("{0}（本机）", "{0} (this PC)", self_nick)
        };
        // 全部行（本机 + 远端）统一按机位 A1→A2→B1→B2 排序后渲染：
        // 任一端（含成员端）成员表顺序一致，本机行不再固定排第一
        struct MemberRow {
            octet: u8,
            seat: String,
            name: String,
            ip: String,
            connected: bool,
            playing: String,
            ping: String,
            link: String,
        }
        let mut rows = vec![MemberRow {
            octet: self_ip
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(u8::MAX),
            seat: self_seat,
            name: self_name,
            ip: self_ip,
            connected: true,
            playing: playing_label(Some(self.game_running)),
            ping: "-".to_string(),
            link: t!("本机", "local").to_string(),
        }];
        // 远端成员昵称映射：房主侧取 HostSync 聚合的上报昵称；
        // 成员侧取最近快照（host_nick + members[].nick）。缺失回退原别名。
        let nick_map: HashMap<String, String> = if is_host {
            self.room_host_sync
                .as_ref()
                .map(|s| s.member_nick())
                .unwrap_or_default()
        } else {
            match &self.room_sync_table {
                Some(t) => t
                    .members
                    .iter()
                    .filter_map(|m| m.nick.clone().map(|n| (m.peer_id.clone(), n)))
                    .chain(
                        t.host_nick
                            .clone()
                            .map(|n| (t.host_peer_id.clone(), n)),
                    )
                    .collect(),
                None => HashMap::new(),
            }
        };
        // 远端行：机位 = 目录成员表座位 IP（缺失兜底解析别名 "join-{末字节}"）；
        // 排序键 = 座位 IP 末字节（缺失时回退 awl 地址，未知排最后）
        for p in self.room_peers.iter() {
            let alias = if p.alias.is_empty() { "?" } else { p.alias.as_str() };
            // 「成员」列：昵称优先（缺失回退别名）；房主行追加（房主）标记
            let is_host_row = self
                .room_seat_ips
                .get(&p.peer_id)
                .map(|ip| ip.ends_with(".101"))
                .unwrap_or(false)
                || alias == "host";
            let name = match nick_map.get(&p.peer_id) {
                Some(n) if !n.is_empty() => n.clone(),
                _ => {
                    if alias == "host" {
                        ROOM_DEFAULT_NICKNAME.to_string()
                    } else {
                        alias.to_string()
                    }
                }
            };
            let name = if is_host_row {
                tf!("{0}（房主）", "{0} (host)", name)
            } else {
                name
            };
            let link = if p.through_relay {
                t!("中继", "relay").to_string()
            } else {
                t!("直连", "direct").to_string()
            };
            // 机位：优先取目录成员表的座位 IP（RoomTick 重建的机位快照），
            // 缺失时兜底解析别名 "join-{末字节}"
            let seat = seat_label(
                self.room_seat_ips.get(&p.peer_id).map(String::as_str),
                &p.alias,
            );
            let octet = self
                .room_seat_ips
                .get(&p.peer_id)
                .map(String::as_str)
                .or(Some(p.ip.as_str()))
                .and_then(|ip| ip.rsplit('.').next().and_then(|s| s.parse::<u8>().ok()));
            rows.push(MemberRow {
                octet: octet.unwrap_or(u8::MAX),
                seat,
                name,
                ip: p.ip.clone(),
                connected: p.connected,
                playing: playing_label(playing_map.get(&p.peer_id).copied()),
                ping: format!("{:.0}ms", p.ping_ms),
                link,
            });
        }
        // 统一按机位 A1→A2→B1→B2 排序（座位 IP 末字节升序；未知排最后）后渲染
        rows.sort_by_key(|r| r.octet);
        for (i, r) in rows.iter().enumerate() {
            mcells.extend(member_cells(
                (i + 2) as i32,
                &r.seat,
                &r.name,
                &r.ip,
                r.connected,
                r.playing.clone(),
                r.ping.clone(),
                r.link.clone(),
            ));
        }
        let members_count = rows.len();
        let mut mrow_defs = vec![GridLength::Auto]; // 表头
        mrow_defs.push(GridLength::Pixel(1.0)); // 分隔线
        for _ in 0..rows.len() {
            mrow_defs.push(GridLength::Auto); // 数据行
        }
        let members_table = Grid::new()
            .columns(mcols)
            .rows(mrow_defs)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .keyed_children(mcells);

        let members_card = Border::new()
            .margin(Thickness::new(0.0, 16.0, 0.0, 0.0))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(Thickness::new(16.0, 12.0, 16.0, 12.0))
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .spacing(8.0)
                    .children((
                        settings_row(
                            View::from(SymbolIcon::new().symbol(Symbol::Contact)),
                            t!("成员", "Members"),
                            tf!(
                                "当前 {} / 4 人",
                                "Currently {} / 4",
                                members_count
                            ).as_str(),
                            View::from(
                                TextBlock::new()
                                    .text(format!("● {status_text}"))
                                    .foreground(Brush::Solid(Color::argb(255, sr, sg, sb)))
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        ),
                        separator(),
                        members_table,
                    )),
            );

        // 房间信息卡片：房间 ID / 游戏 / 本机地址
        let info_card = Border::new()
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
                            View::from(SymbolIcon::new().symbol(Symbol::World)),
                            t!("房间 ID", "Room ID"),
                            t!("把 ID 告知好友，凭 ID + 密码加入", "Share this ID; friends join with ID + password"),
                            View::from(
                                selectable_text(room_id.clone())
                                    .font_size(18.0)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        ),
                        separator(),
                        settings_row(
                            View::from(SymbolIcon::new().symbol(Symbol::Permissions)),
                            t!("房间密码", "Room password"),
                            t!("把密码告知好友，凭 ID + 密码加入", "Share this password; friends join with ID + password"),
                            View::from(
                                selectable_text(pw_display)
                                    .font_size(16.0)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        ),
                        separator(),
                        settings_row(
                            View::from(SymbolIcon::new().symbol(Symbol::List)),
                            t!("游戏", "Game"),
                            t!("本房间的对战游戏", "The game played in this room"),
                            View::from(
                                selectable_text(self.room_game.clone())
                                    .opacity(0.85)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .text_trimming(TextTrimming::CharacterEllipsis),
                            ),
                        ),
                        separator(),
                        settings_row(
                            View::from(SymbolIcon::new().symbol(Symbol::Play)),
                            t!("游戏地址（本机）", "Game address (this PC)"),
                            t!("本机在房间虚拟局域网内的游戏 IP", "This PC's game IP inside the room LAN"),
                            View::from(
                                selectable_text(self.room_my_ip.clone().unwrap_or_default())
                                    .font_size(16.0)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        ),
                        // ServerBox 地址：.102 副地址按游戏形态存在
                        // （IDAS=kitten / mice 恒有；IDAC / IDZ 无）整行隐藏
                        if has_serverbox_addr {
                            separator()
                        } else {
                            StackPanel::new().into()
                        },
                        if has_serverbox_addr {
                            settings_row(
                                View::from(SymbolIcon::new().symbol(Symbol::MapDrive)),
                                t!("ServerBox 地址", "ServerBox address"),
                                t!("ServerBox 绑定地址，游戏通过它联机（仅房主机持有）", "ServerBox binds this; the game connects via it (host PC only)"),
                                View::from(
                                    selectable_text(crate::awl::HOST_SERVERBOX_IP.to_string())
                                        .font_size(16.0)
                                        .font_weight(FontWeight::SEMI_BOLD)
                                        .vertical_alignment(VerticalAlignment::Center),
                                ),
                            )
                        } else {
                            StackPanel::new().into()
                        },
                    )),
            );

        // 操作按钮组：启动游戏（房主/成员）+ 启动 ServerBox（仅房主）+ 关闭/离开房间；
        // 游戏 / ServerBox 运行中时对应按钮切换为「关闭」（与游戏页行内按钮同款行为）
        let room_index = self.room_game_index();
        // 未入库（room_index 为 None）时不能算「本机运行中」——两边都没有运行时会
        // 出现 None == None，房主侧的 ServerBox 按钮会错显示成「关闭」
        let game_running_here = self.game_running
            && room_index.is_some()
            && self.game_runtime.as_ref().map(|r| r.index) == room_index;
        let sb_running_here = room_index.is_some()
            && self.serverbox_runtime.as_ref().map(|r| r.index) == room_index;
        let mut actions: Vec<KeyedView> = Vec::new();
        actions.push(KeyedView::new(
            "launch-game",
            if game_running_here {
                View::from(
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.message(Message::LaunchStop(
                            room_index.unwrap_or_default(),
                        )))
                        .content(t!("关闭", "Close")),
                )
            } else {
                View::from(
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.message(Message::RoomLaunchGame))
                        .content(t!("启动游戏", "Launch game")),
                )
            },
        ));
        // ServerBox 仅房主机持有（.102 只在房主的 awl0 上），成员不显示该按钮；
        // 且须已配置 ServerBox 程序（未配置点击只会报「该游戏未配置 ServerBox」）
        if is_host && has_serverbox {
            actions.push(KeyedView::new(
                "launch-serverbox",
                if sb_running_here {
                    View::from(
                        Button::new()
                            .on_click(context.message(Message::ServerBoxStop(
                                room_index.unwrap_or_default(),
                            )))
                            .content(t!("关闭", "Close")),
                    )
                } else {
                    View::from(
                        Button::new()
                            .on_click(context.message(Message::RoomLaunchServerBox))
                            .content(t!("启动 ServerBox", "Launch ServerBox")),
                    )
                },
            ));
        }
        actions.push(KeyedView::new(
            "room-stop",
            if is_host {
                View::from(
                    Button::new()
                        .on_click(context.message(Message::RoomCloseClicked))
                        .content(t!("关闭房间", "Close room")),
                )
            } else {
                View::from(
                    Button::new()
                        .on_click(context.message(Message::RoomLeaveClicked))
                        .content(t!("离开房间", "Leave room")),
                )
            },
        ));
        let action: View = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .vertical_alignment(VerticalAlignment::Center)
            .keyed_children(actions)
            .into();


        let title = self.room_name.clone();

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
                                        .text(title)
                                        .font_size(26.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new()
                                        .text(if is_host {
                                            t!(
                                                "把房间 ID 分享给好友",
                                                "Share the room ID"
                                            )
                                            .to_string()
                                        } else {
                                            t!("已加入房间", "Joined")
                                                .to_string()
                                        })
                                        .font_size(12.0)
                                        .opacity(0.6),
                                )),
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(8.0)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .grid_column(1)
                                    .children((action,)),
                            )),
                        info_card,
                        members_card,
                    )),
            )
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::seat_name_of_octet;

    #[test]
    fn seat_name_of_octet_mapping() {
        // 房间编址：房主 .101=A1；成员 .103/.104/.105 = A2/B1/B2；.102 为 ServerBox 不占成员位
        assert_eq!(seat_name_of_octet(101), Some("A1"));
        assert_eq!(seat_name_of_octet(103), Some("A2"));
        assert_eq!(seat_name_of_octet(104), Some("B1"));
        assert_eq!(seat_name_of_octet(105), Some("B2"));
        assert_eq!(seat_name_of_octet(102), None);
        assert_eq!(seat_name_of_octet(0), None);
        assert_eq!(seat_name_of_octet(255), None);
    }
}
