//! 房间内成员表长连接同步（星型：房主权威 + 成员状态经房主中转）。
//!
//! 目录服务只在加入/离开等事件时被调用；房间内的成员表与游玩状态经本通道同步：
//!
//! - 房主（权威）在 `HOST_GAME_IP:ROOM_SYNC_PORT`（awl 虚拟网卡，仅房间子网可达）
//!   监听 TCP；成员连接后立即收到全量成员表快照，此后每次变化即时推送；
//! - 成员经同一连接上报自身游玩状态（空闲/游玩中，按启动器 `game_running` 判定）
//!   与昵称，30s 保活兼作连接活性心跳；房主聚合后随成员表广播；
//! - 房主关闭房间（或启动器退出）先广播 close 帧再停线程；成员收到后立即退出房间
//!   （未收到时靠通道断开 / awl 掉线兜底）。
//!
//! 协议（行分隔 JSON，`\n` 结尾）：
//! - 房主→成员：`{"type":"table","version":u64,"host_peer_id":"…","host_nick":"…",`
//!   `"host_playing":bool,"members":[{"peer_id":"…","ip":"…","nick":"…","playing":bool}]}`
//!   / `{"type":"close"}`（房主关闭房间通知）
//! - 成员→房主：`{"type":"hello","peer_id":"…","nick":"…","playing":bool}`（连接首帧）/
//!   `{"type":"status","peer_id":"…","nick":"…","playing":bool}`（变化 + 30s 保活）
//! - `nick` / `host_nick` 为可选字段（serde default）：旧版对端缺失时昵称回退别名显示。
//!
//! 线程模型（与 `server::spawn_probe` 同模式——reactor 框架的 spawn_background 是
//! FnOnce→单条消息，无法长驻回灌，走共享态 + RoomTick 本地消费）：
//! - HostSync：accept 线程（非阻塞轮询 stop + 绑定失败重试）+ 每连接一线程
//!   （读超时轮询：读成员帧更新状态、版本号推进即推快照、closing 置位即广播 close）；
//! - MemberSync：连接线程（断线重试，收到 close 或 stop 后退出）+ 每会话读线程
//!   （收 table 入收件箱 / close 置位）+ 写线程（发 hello/status/保活）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::awl::{HOST_GAME_IP, ROOM_SYNC_PORT};

/// 共享态轮询/推帧间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 成员连接失败重试间隔。
const CONNECT_RETRY: Duration = Duration::from_secs(1);
/// 成员状态保活周期（兼作连接活性心跳）。
const STATUS_KEEPALIVE: Duration = Duration::from_secs(30);
/// 成员状态帧过期：超过该时长无任何帧视为掉线（游玩状态回落空闲）。
const STATUS_STALE: Duration = Duration::from_secs(75);
/// 连接读超时（轮询 stop/版本号用的非阻塞替代）。
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// 成员条目（昵称 + 游玩状态）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncMember {
    pub peer_id: String,
    pub ip: String,
    /// 成员昵称（缺省 None = 旧版对端未上报，UI 回退别名显示）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(default)]
    pub playing: bool,
}

/// 房主广播的全量成员表快照。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncTable {
    pub version: u64,
    pub host_peer_id: String,
    /// 房主昵称（缺省 None = 旧版房主未上报）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_nick: Option<String>,
    #[serde(default)]
    pub host_playing: bool,
    pub members: Vec<SyncMember>,
}

/// 成员→房主上行帧。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UpFrame {
    Hello {
        peer_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nick: Option<String>,
        #[serde(default)]
        playing: bool,
    },
    Status {
        peer_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nick: Option<String>,
        #[serde(default)]
        playing: bool,
    },
}

/// 房主→成员下行帧类型标记（table 之外识别 close 用）。
#[derive(Deserialize)]
struct DownTag {
    #[serde(rename = "type")]
    kind: String,
}

/// close 帧字面量（房主关闭房间通知）。
const CLOSE_FRAME: &str = "{\"type\":\"close\"}\n";

// ---------- 房主侧 ----------

struct HostState {
    stop: AtomicBool,
    /// 关房广播标志：置位后各连接线程发 close 帧并退出（Drop 兜底杀 awl）
    closing: AtomicBool,
    /// 当前快照版本（有变化才 +1；连接线程据此推帧）
    version: AtomicU64,
    /// UI 设置的权威成员表：(房主 peer_id, [(成员 peer_id, 座位 ip)], 房主游玩状态)
    base: Mutex<Option<(String, Vec<(String, String)>, bool)>>,
    /// 房主昵称（随 table 帧广播）
    host_nick: Mutex<Option<String>>,
    /// 成员上报的游玩状态：peer_id → (playing, 最后帧时刻)
    statuses: Mutex<HashMap<String, (bool, Instant)>>,
    /// 成员上报的昵称：peer_id → nick
    nicks: Mutex<HashMap<String, String>>,
    /// 最近一次构建的快照（diff 用，避免无谓广播）
    last: Mutex<Option<SyncTable>>,
}

/// 由权威成员表 + 成员上报状态重建快照；内容有变化才升版本。
fn host_rebuild(state: &HostState) {
    let Some((host_peer_id, members, host_playing)) = state.base.lock().unwrap().clone() else {
        return;
    };
    let host_nick = state.host_nick.lock().unwrap().clone();
    let now = Instant::now();
    let list: Vec<SyncMember> = {
        let statuses = state.statuses.lock().unwrap();
        let nicks = state.nicks.lock().unwrap();
        members
            .iter()
            .map(|(peer_id, ip)| SyncMember {
                playing: statuses
                    .get(peer_id)
                    .map(|(p, seen)| *p && now.duration_since(*seen) < STATUS_STALE)
                    .unwrap_or(false),
                nick: nicks.get(peer_id).cloned(),
                peer_id: peer_id.clone(),
                ip: ip.clone(),
            })
            .collect()
    };
    let mut last = state.last.lock().unwrap();
    let same = last
        .as_ref()
        .map(|t| {
            t.host_peer_id == host_peer_id
                && t.host_playing == host_playing
                && t.host_nick == host_nick
                && t.members.len() == list.len()
                && t.members
                    .iter()
                    .zip(list.iter())
                    .all(|(a, b)| {
                        a.peer_id == b.peer_id
                            && a.ip == b.ip
                            && a.nick == b.nick
                            && a.playing == b.playing
                    })
        })
        .unwrap_or(false);
    if same {
        return;
    }
    let version = state.version.load(Ordering::SeqCst) + 1;
    state.version.store(version, Ordering::SeqCst);
    *last = Some(SyncTable {
        version,
        host_peer_id,
        host_nick,
        host_playing,
        members: list,
    });
}

/// 上行帧处理：更新成员状态/昵称并重建快照。
fn host_handle_up(state: &HostState, frame: UpFrame) {
    let (peer_id, playing, nick) = match frame {
        UpFrame::Hello { peer_id, nick, playing } | UpFrame::Status { peer_id, nick, playing } => {
            (peer_id, playing, nick)
        }
    };
    let now = Instant::now();
    state
        .statuses
        .lock()
        .unwrap()
        .insert(peer_id.clone(), (playing, now));
    if let Some(n) = nick {
        if !n.is_empty() {
            state.nicks.lock().unwrap().insert(peer_id, n);
        }
    }
    host_rebuild(state);
}

/// 单连接线程：读成员帧 + 版本推进即推快照 + 关房广播（读超时轮询，单线程收发）。
fn host_conn_loop(state: Arc<HostState>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let Ok(reader_handle) = stream.try_clone() else { return };
    let mut reader = BufReader::new(reader_handle);
    let mut writer = stream;
    let mut sent_version = 0u64;
    let mut line = String::new();
    loop {
        if state.stop.load(Ordering::SeqCst) {
            return;
        }
        // 关房广播：发 close 帧后退出（成员收到即主动退出房间）
        if state.closing.load(Ordering::SeqCst) {
            let _ = writer.write_all(CLOSE_FRAME.as_bytes()).and_then(|_| writer.flush());
            return;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return, // 对端关闭
            Ok(_) => {
                match serde_json::from_str::<UpFrame>(line.trim()) {
                    Ok(frame) => host_handle_up(&state, frame),
                    Err(_) => {} // 坏帧忽略
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        // 版本推进（含连接后首推）→ 发全量快照
        let v = state.version.load(Ordering::SeqCst);
        if v != sent_version {
            let snap = state.last.lock().unwrap().clone();
            if let Some(t) = snap {
                let mut buf = serde_json::to_string(&t).unwrap_or_default();
                buf.push('\n');
                if writer.write_all(buf.as_bytes()).and_then(|_| writer.flush()).is_err() {
                    return;
                }
                sent_version = v;
            }
        }
    }
}

/// accept 线程：绑定失败（房间网段未就绪）逐秒重试；stop 后退出。
fn host_accept_loop(state: Arc<HostState>) {
    let mut listener: Option<TcpListener> = None;
    while !state.stop.load(Ordering::SeqCst) {
        if listener.is_none() {
            match TcpListener::bind((HOST_GAME_IP, ROOM_SYNC_PORT)) {
                Ok(l) => {
                    let _ = l.set_nonblocking(true);
                    listener = Some(l);
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        match listener.as_ref().unwrap().accept() {
            Ok((stream, _)) => {
                let st = state.clone();
                let _ = std::thread::Builder::new()
                    .name("room-sync-host-conn".into())
                    .spawn(move || host_conn_loop(st, stream));
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(POLL_INTERVAL),
            Err(_) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

/// 房主侧同步通道句柄（Drop 即停线程）。
pub struct HostSync {
    state: Arc<HostState>,
}

impl HostSync {
    pub fn start() -> HostSync {
        let state = Arc::new(HostState {
            stop: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            version: AtomicU64::new(0),
            base: Mutex::new(None),
            host_nick: Mutex::new(None),
            statuses: Mutex::new(HashMap::new()),
            nicks: Mutex::new(HashMap::new()),
            last: Mutex::new(None),
        });
        let s = state.clone();
        let _ = std::thread::Builder::new()
            .name("room-sync-host".into())
            .spawn(move || host_accept_loop(s));
        HostSync { state }
    }

    /// UI 设置权威成员表、房主昵称与房主游玩状态；内容有变化才升版本广播。
    pub fn publish(
        &self,
        host_peer_id: &str,
        members: &[(String, String)],
        host_playing: bool,
        host_nick: Option<&str>,
    ) {
        *self.state.base.lock().unwrap() =
            Some((host_peer_id.to_string(), members.to_vec(), host_playing));
        *self.state.host_nick.lock().unwrap() =
            host_nick.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        host_rebuild(&self.state);
    }

    /// 成员游玩状态（peer_id → playing；超时未上报的成员回落空闲）。
    pub fn member_playing(&self) -> HashMap<String, bool> {
        let now = Instant::now();
        self.state
            .statuses
            .lock()
            .unwrap()
            .iter()
            .map(|(k, (p, seen))| (k.clone(), *p && now.duration_since(*seen) < STATUS_STALE))
            .collect()
    }

    /// 成员昵称（peer_id → nick；仅含已上报昵称的成员）。
    pub fn member_nick(&self) -> HashMap<String, String> {
        self.state.nicks.lock().unwrap().clone()
    }

    /// 房主关闭房间：广播 close 帧后停全部连接/线程（Drop 兜底杀 awl）。
    pub fn notify_close(&self) {
        self.state.closing.store(true, Ordering::SeqCst);
    }
}

impl Drop for HostSync {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
    }
}

// ---------- 成员侧 ----------

struct MemberState {
    stop: AtomicBool,
    connected: AtomicBool,
    /// 收到房主 close 帧（房主已关闭房间）→ 停重连，UI 据此立即退出
    closed: AtomicBool,
    /// 本机 awl 身份
    peer_id: String,
    /// 本机昵称（hello/status 帧携带；UI 可随时更新）
    nick: Mutex<String>,
    /// 最近收到的快照（take 消费，版本去重由 UI 负责）
    inbox: Mutex<Option<SyncTable>>,
    /// 待发送的游玩状态（UI 线程投递，写线程取走；仅保留最新值）
    outbox: Mutex<Option<bool>>,
}

/// 组装上行帧 JSON（hello/status 通用；昵称空时不带字段）。
fn up_frame_json(kind: &str, peer_id: &str, nick: &str, playing: bool) -> String {
    let nick_field = if nick.is_empty() {
        String::new()
    } else {
        let mut n = String::from(",\"nick\":");
        n.push_str(&serde_json::to_string(nick).unwrap_or_else(|_| "\"\"".into()));
        n
    };
    format!(
        "{{\"type\":\"{kind}\",\"peer_id\":{},\"playing\":{playing}{nick_field}}}",
        serde_json::to_string(peer_id).unwrap_or_else(|_| "\"\"".into())
    )
}

/// 单会话：阻塞读 table/close 帧；EOF/错误即返回（外层重连）。
fn member_session(state: &Arc<MemberState>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let Ok(writer_handle) = stream.try_clone() else { return };
    let Ok(hello_writer) = stream.try_clone() else { return };
    let mut writer = hello_writer;
    // 首帧 hello（带昵称与最新待报状态；无则视为空闲，UI 下一拍会补发真实状态）
    let hello_nick = state.nick.lock().unwrap().clone();
    let hello_playing = state.outbox.lock().unwrap().take().unwrap_or(false);
    let hello = up_frame_json("hello", &state.peer_id, &hello_nick, hello_playing);
    if writer
        .write_all(hello.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .is_err()
    {
        return;
    }
    state.connected.store(true, Ordering::SeqCst);
    // 写线程：status 上报（outbox）+ 30s 保活
    let wstate = state.clone();
    let writer_thread = std::thread::Builder::new()
        .name("room-sync-member-w".into())
        .spawn(move || {
            let mut writer = writer_handle;
            let mut last_sent = (hello_playing, Instant::now());
            while !wstate.stop.load(Ordering::SeqCst) && !wstate.closed.load(Ordering::SeqCst) {
                let pending = wstate.outbox.lock().unwrap().take();
                let due = last_sent.1.elapsed() >= STATUS_KEEPALIVE;
                if pending.is_some() || due {
                    let playing = pending.unwrap_or(last_sent.0);
                    let nick = wstate.nick.lock().unwrap().clone();
                    let frame = up_frame_json("status", &wstate.peer_id, &nick, playing);
                    if writer
                        .write_all(frame.as_bytes())
                        .and_then(|_| writer.write_all(b"\n"))
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        return; // 写失败 → 会话结束触发重连
                    }
                    last_sent = (playing, Instant::now());
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        });
    // 读线程（本线程）：收 table 帧入收件箱 / close 帧置位
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if state.stop.load(Ordering::SeqCst) || state.closed.load(Ordering::SeqCst) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                // close 帧优先识别（房主已关闭房间 → 立即退出，停重连）
                if serde_json::from_str::<DownTag>(trimmed)
                    .map(|t| t.kind == "close")
                    .unwrap_or(false)
                {
                    state.closed.store(true, Ordering::SeqCst);
                    break;
                }
                if let Ok(table) = serde_json::from_str::<SyncTable>(trimmed) {
                    *state.inbox.lock().unwrap() = Some(table);
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    state.connected.store(false, Ordering::SeqCst);
    if let Ok(handle) = writer_thread {
        let _ = handle.join();
    }
}

/// 连接线程：断线重试直至 stop / 收到 close。
fn member_connect_loop(state: Arc<MemberState>) {
    while !state.stop.load(Ordering::SeqCst) && !state.closed.load(Ordering::SeqCst) {
        match TcpStream::connect((HOST_GAME_IP, ROOM_SYNC_PORT)) {
            Ok(stream) => member_session(&state, stream),
            Err(_) => {
                state.connected.store(false, Ordering::SeqCst);
            }
        }
        // 会话结束/连接失败：退避重试（stop / closed 立即退出）
        let deadline = Instant::now() + CONNECT_RETRY;
        while Instant::now() < deadline
            && !state.stop.load(Ordering::SeqCst)
            && !state.closed.load(Ordering::SeqCst)
        {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// 成员侧同步通道句柄（Drop 即停线程）。
pub struct MemberSync {
    state: Arc<MemberState>,
}

impl MemberSync {
    pub fn start(my_peer_id: String, my_nick: String) -> MemberSync {
        let state = Arc::new(MemberState {
            stop: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            peer_id: my_peer_id,
            nick: Mutex::new(my_nick),
            inbox: Mutex::new(None),
            outbox: Mutex::new(None),
        });
        let s = state.clone();
        let _ = std::thread::Builder::new()
            .name("room-sync-member".into())
            .spawn(move || member_connect_loop(s));
        MemberSync { state }
    }

    /// 取走最新快照（无新帧返回 None；版本去重由 UI 负责）。
    pub fn take_snapshot(&self) -> Option<SyncTable> {
        self.state.inbox.lock().unwrap().take()
    }

    /// 房主是否已关闭房间（收到 close 帧；置位后不再复位的单向标志）。
    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::SeqCst)
    }

    /// 上报本机游玩状态（连续变化合并为最新值，写线程串行发送）。
    pub fn send_status(&self, playing: bool) {
        *self.state.outbox.lock().unwrap() = Some(playing);
    }
}

impl Drop for MemberSync {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_frame_json_with_and_without_nick() {
        let with = up_frame_json("hello", "peer1", "玩家", true);
        assert_eq!(with, "{\"type\":\"hello\",\"peer_id\":\"peer1\",\"playing\":true,\"nick\":\"玩家\"}");
        let without = up_frame_json("status", "peer1", "", false);
        assert_eq!(without, "{\"type\":\"status\",\"peer_id\":\"peer1\",\"playing\":false}");
        // 两种形态都能被房主侧反序列化
        let f: UpFrame = serde_json::from_str(&with).unwrap();
        match f {
            UpFrame::Hello { peer_id, nick, playing } => {
                assert_eq!(peer_id, "peer1");
                assert_eq!(nick.as_deref(), Some("玩家"));
                assert!(playing);
            }
            _ => panic!("应解析为 hello"),
        }
    }

    #[test]
    fn up_frame_without_nick_is_backward_compatible() {
        // 旧版成员帧（无 nick 字段）可解析
        let f: UpFrame =
            serde_json::from_str("{\"type\":\"status\",\"peer_id\":\"p\",\"playing\":true}").unwrap();
        match f {
            UpFrame::Status { peer_id, nick, playing } => {
                assert_eq!(peer_id, "p");
                assert_eq!(nick, None);
                assert!(playing);
            }
            _ => panic!("应解析为 status"),
        }
    }

    #[test]
    fn host_rebuild_aggregates_nick_and_host_nick() {
        let state = Arc::new(HostState {
            stop: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            version: AtomicU64::new(0),
            base: Mutex::new(Some(("host".into(), vec![("m1".into(), "172.10.1.103".into())], false))),
            host_nick: Mutex::new(Some("房主昵称".into())),
            statuses: Mutex::new(HashMap::new()),
            nicks: Mutex::new(HashMap::from([("m1".into(), "成员昵称".into())])),
            last: Mutex::new(None),
        });
        host_rebuild(&state);
        let t = state.last.lock().unwrap().clone().unwrap();
        assert_eq!(t.host_nick.as_deref(), Some("房主昵称"));
        assert_eq!(t.members[0].nick.as_deref(), Some("成员昵称"));
    }

    #[test]
    fn table_and_close_frames_roundtrip() {
        // table 帧带昵称序列化 → 成员侧可解析
        let t = SyncTable {
            version: 3,
            host_peer_id: "host".into(),
            host_nick: Some("H".into()),
            host_playing: false,
            members: vec![SyncMember {
                peer_id: "m".into(),
                ip: "172.10.1.103".into(),
                nick: Some("M".into()),
                playing: true,
            }],
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: SyncTable = serde_json::from_str(&json).unwrap();
        assert_eq!(back.host_nick.as_deref(), Some("H"));
        assert_eq!(back.members[0].nick.as_deref(), Some("M"));
        // 旧版 table（无昵称字段）可解析
        let legacy: SyncTable = serde_json::from_str(
            "{\"version\":1,\"host_peer_id\":\"h\",\"members\":[{\"peer_id\":\"m\",\"ip\":\"i\"}]}",
        )
        .unwrap();
        assert_eq!(legacy.host_nick, None);
        assert_eq!(legacy.members[0].nick, None);
        // close 帧识别
        let tag: DownTag = serde_json::from_str("{\"type\":\"close\"}").unwrap();
        assert_eq!(tag.kind, "close");
    }
}
