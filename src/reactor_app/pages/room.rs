//! 店内对战（房间虚拟局域网）页：大厅视图 + 房间内视图 + 状态机与后台任务。
//!
//! - 大厅（Idle）：右上角「创建…/加入…/刷新」，主体为公开房间列表表格；
//! - 房间内（Hosting/Joined）：房间状态、成员列表、离开/关闭房间按钮；
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
use crate::reactor_app::widgets::{record_db_match, selectable_text, separator, settings_row};

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
/// 轮询周期。
const TICK_INTERVAL: Duration = Duration::from_secs(2);

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

    /// 房间游戏在游戏库中的下标：按「识别库命中产品名 == 房间游戏名」匹配
    /// （房主开房与成员加入时 room_game 均为识别库产品名；成员侧服务端不下发
    /// game_id，故统一按名称匹配）。None = 该游戏尚未添加到游戏库。
    pub(in crate::reactor_app) fn room_game_index(&self) -> Option<usize> {
        let game = self.room_game.trim();
        if game.is_empty() {
            return None;
        }
        self.library
            .games
            .iter()
            .position(|r| record_db_match(r).is_some_and(|m| m.name == game))
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
                self.room_schedule_tick(context);
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
                self.room_schedule_tick(context);
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
        self.room_last_heartbeat = None;
        self.room_host_down = None;
        self.room_my_ip = None;
        self.room_game.clear();
        self.room_name.clear();
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
            let res = if is_host {
                dir.close(&room_id, my.as_deref(), &keychip).map_err(|e| {
                    tf!("关闭房间失败：{}", "Failed to close the room: {}", e)
                })
            } else {
                match my {
                    Some(peer) => dir.leave(&room_id, &peer).map_err(|e| {
                        tf!("退出房间失败：{}", "Failed to leave the room: {}", e)
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
                    Ok(Some(room)) => match AwlManager::host_resume_flow(&cfg) {
                        Ok(h) => {
                            *handle.lock().unwrap() = Some(h);
                            return Message::RoomResumeResult(Ok(Some(room)));
                        }
                        Err(e) => return Message::RoomResumeResult(Err(e)),
                    },
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

    /// 房间轮询心跳：房主=目录成员表 invite；成员=成员表对账（全互联 invite）+
    /// 房主失联检测；刷新成员快照。
    pub(in crate::reactor_app) fn room_on_tick(
        &mut self,
        context: &windows_reactor::ComponentContext<Self>,
    ) {
        // 刷新房间内成员视图快照（UI 成员列表直接读该字段）
        self.room_peers = AwlManager::known_peers();
        match self.room_status.clone() {
            RoomStatus::Hosting { room_id, .. } => {
                let dir = self.dir_client();
                // 以目录成员为权威：新成员 → invite；已离开成员 → 从展示/邀请集合剔除
                if let Ok(members) = dir.members(&room_id) {
                    let mut ids: HashSet<String> =
                        members.members.iter().map(|m| m.peer_id.clone()).collect();
                    ids.insert(members.host_peer_id.clone());
                    // 成员掉线按「离开房间」处理：awl 断连超过宽限期 → 代成员释放座位
                    // （等同 leave，幂等），座位即时回收、重入按最低空闲位重新分配。
                    // 曾建连的成员走短宽限（掉线=断连）；从未建连的新成员走长宽限
                    //（建链还在进行中，不能按掉线计时，否则首次进房会被误踢）
                    let mut down_now: HashMap<String, Instant> = HashMap::new();
                    for m in &members.members {
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
                            // 成员已从房间移除（列表实时反映，无需额外提示）
                        } else {
                            down_now.insert(m.peer_id.clone(), since);
                        }
                    }
                    self.room_member_down = down_now;
                    // 已不在目录的成员清掉「曾建连」标记：重入按新一次加入重新计时
                    self.room_member_ever_connected.retain(|p| ids.contains(p));
                    for m in &members.members {
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
                    // 房主保活：每 5 分钟向目录刷新一次 last_heartbeat，
                    // 避免服务端 1h 无保活自动关房（仅目录可达时发，失败忽略）。
                    if self.room_last_heartbeat.map_or(true, |t| t.elapsed() >= Duration::from_secs(300)) {
                        if dir.heartbeat(&room_id, &self.room_keychip).is_ok() {
                            self.room_last_heartbeat = Some(Instant::now());
                        }
                    }
                }
                self.room_schedule_tick(context);
            }
            RoomStatus::Joined { room_id, is_host: false } => {
                let dir = self.dir_client();
                // 目录上房间已消失 = 房主已正常关闭 → 立即退出回大厅（不等失联超时）。
                // 不在房间页时不做退房判定：后台 tick 仍每拍续跑，房间与 awl 进程始终
                // 保活，回到房间页后判定立即恢复。
                match dir.members(&room_id) {
                    Err(crate::roomdir::DirError::NotFound) => {
                        if self.nav_tag != "room" {
                            self.room_schedule_tick(context);
                            return;
                        }
                        self.room_stop(context);
                        return;
                    }
                    Ok(members) => {
                        if let Ok(my) = AwlManager::my_peer_id() {
                            // 掉线后座位已被房主释放（成员表里没有自己）→ 按「离开房间」
                            // 处理：退出回大厅，重入时按新顺位分配座位（仅房间页判定）
                            let still_member = my == members.host_peer_id
                                || members.members.iter().any(|m| m.peer_id == my);
                            if !still_member && self.nav_tag == "room" {
                                self.room_stop(context);
                                return;
                            }
                            // 成员全互联对账：把目录里其他成员逐个 invite 进本机 awl
                            // （对方 autoAccept 自动接受；invite_member 含座位 IP 校正），
                            // 本机 awl 不再只认识房主，成员列表才能显示全部成员
                            if still_member {
                                for m in &members.members {
                                    if m.peer_id == my
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
                        }
                        // 以目录成员为权威剔除已离开的成员；保留房主 peer_id（别名 "host"）；
                        // invited 集合同步剔除（重入者换新座位，须允许重新 invite 校正 IP）
                        let mut ids: HashSet<String> =
                            members.members.iter().map(|m| m.peer_id.clone()).collect();
                        ids.insert(members.host_peer_id.clone());
                        self.room_peers.retain(|p| ids.contains(&p.peer_id));
                        self.room_members_invited.retain(|p| ids.contains(p));
                    }
                    Err(_) => { /* 网络等错误：保留现状，下次 tick 再试 */ }
                }
                // 兜底：房主失联检测（别名固定为 "host"，见 link_member_to_host）——
                // 仅房间页生效，理由同上（瞬时抖动不踢人）
                if self.nav_tag != "room" {
                    self.room_schedule_tick(context);
                    return;
                }
                let host_connected = AwlManager::known_peers()
                    .into_iter()
                    .find(|p| p.alias == "host")
                    .map(|p| p.connected)
                    .unwrap_or(false);
                if host_connected {
                    self.room_host_down = None;
                } else {
                    let since = *self.room_host_down.get_or_insert(Instant::now());
                    if since.elapsed() >= HOST_DOWN_TIMEOUT {
                        self.room_stop(context);
                        return;
                    }
                }
                self.room_schedule_tick(context);
            }
            _ => {
                // 非 Hosting/Joined 状态：不再续拍
            }
        }
    }

    pub(in crate::reactor_app) fn room_schedule_tick(&self, context: &windows_reactor::ComponentContext<Self>) {
        context.spawn_background(move |_| {
            std::thread::sleep(TICK_INTERVAL);
            Message::RoomTick
        });
    }
}

// ---------- 页面 UI ----------

/// 成员表格行：成员 / 地址 / 状态 / 延迟 / 链路（返回该行的全部单元格）。
fn member_cells(
    row: i32,
    alias: &str,
    ip: &str,
    connected: bool,
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
                .text(alias.to_string())
                .vertical_alignment(VerticalAlignment::Center)
                .text_trimming(TextTrimming::CharacterEllipsis),
        )),
        cell(1, View::from(
            TextBlock::new()
                .text(ip.to_string())
                .font_size(12.0)
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(2, View::from(
            Border::new()
                .width(11.0)
                .height(11.0)
                .corner_radius(5.5)
                .background(Brush::Solid(dot_color))
                .horizontal_alignment(HorizontalAlignment::Left)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(3, View::from(
            TextBlock::new()
                .text(ping_text)
                .font_size(12.0)
                .opacity(0.8)
                .vertical_alignment(VerticalAlignment::Center),
        )),
        cell(4, View::from(
            TextBlock::new()
                .text(link_text)
                .font_size(12.0)
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center),
        )),
    ]
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

        // 成员表格：本机排第一，其后为远端成员（单 Grid 共享列宽,严格对齐）
        let mcols = [
            GridLength::Star(1.0),
            GridLength::Star(1.0),
            GridLength::Star(1.0),
            GridLength::Star(1.0),
            GridLength::Star(1.0),
        ];
        let mut mcells: Vec<KeyedView> = Vec::new();
        let mheaders = [
            t!("成员", "Member").to_string(),
            t!("地址", "Address").to_string(),
            t!("状态", "Status").to_string(),
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
        let self_role = if is_host {
            t!("房主（本机）", "Host (this PC)")
        } else {
            t!("成员（本机）", "Member (this PC)")
        };
        let self_ip = self.room_my_ip.clone().unwrap_or_default();
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
                    .grid_column_span(5),
            ),
        ));
        mcells.extend(member_cells(
            2,
            &self_role,
            &self_ip,
            true,
            "-".to_string(),
            t!("本机", "local").to_string(),
        ));
        for (i, p) in self.room_peers.iter().enumerate() {
            let alias = if p.alias.is_empty() { "?" } else { p.alias.as_str() };
            let link = if p.through_relay {
                t!("中继", "relay").to_string()
            } else {
                t!("直连", "direct").to_string()
            };
            mcells.extend(member_cells(
                (i + 3) as i32,
                alias,
                &p.ip,
                p.connected,
                format!("{:.0}ms", p.ping_ms),
                link,
            ));
        }
        let members_count = self.room_peers.len() + 1;
        let mut mrow_defs = vec![GridLength::Auto]; // 表头
        mrow_defs.push(GridLength::Pixel(1.0)); // 分隔线
        mrow_defs.push(GridLength::Auto); // 本机行
        for _ in 0..self.room_peers.len() {
            mrow_defs.push(GridLength::Auto);
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
                        separator(),
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
                        ),
                    )),
            );

        // 操作按钮组：启动游戏（房主/成员）+ 启动 ServerBox（仅房主）+ 关闭/离开房间；
        // 游戏 / ServerBox 运行中时对应按钮切换为「关闭」（与游戏页行内按钮同款行为）
        let room_index = self.room_game_index();
        let game_running_here =
            self.game_running && self.game_runtime.as_ref().map(|r| r.index) == room_index;
        let sb_running_here = self.serverbox_runtime.as_ref().map(|r| r.index) == room_index;
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
        // ServerBox 仅房主机持有（.102 只在房主的 awl0 上），成员不显示该按钮
        if is_host {
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
