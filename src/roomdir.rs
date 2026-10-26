//! 房间目录服务 HTTP 客户端（WinHTTP，支持 https，5s 超时）。
//!
//! 只做发现层交互：开房 / 心跳 / 列表 / 加入 / 成员表 / 离开 / 关房。
//! 数据面（awl 的 P2P 连接）与这里完全无关，目录不可达只影响「新加入」。

use serde::Deserialize;
use std::sync::OnceLock;
use windows::core::PCWSTR;

use crate::t;
use crate::tf;

/// 进程级复用的 WinHTTP 会话（OnceLock 懒创建，存裸句柄）：
/// 同一 session 内 WinHTTP 自动池化底层 TCP/TLS 连接，轮询期间免去每拍一次的
/// 握手开销。进程生命周期持有、不主动关闭（WinHTTP 进程退出时统一回收）。
/// 创建失败不缓存，下次请求重试。
static DIR_SESSION: OnceLock<usize> = OnceLock::new();

/// 取（或首次创建）共享 WinHTTP 会话；接收超时（5s）在会话级一次性设置。
fn shared_session() -> Result<*mut core::ffi::c_void, String> {
    use windows::Win32::Networking::WinHttp::{
        WinHttpOpen, WinHttpSetOption, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_OPTION_RECEIVE_TIMEOUT,
    };
    if let Some(&s) = DIR_SESSION.get() {
        return Ok(s as *mut core::ffi::c_void);
    }
    let session = unsafe {
        WinHttpOpen(
            PCWSTR::null(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    if session.is_null() {
        return Err(t!("无法初始化网络", "Failed to initialize network").to_string());
    }
    unsafe {
        let timeout_bytes = 5000u32.to_ne_bytes();
        let _ = WinHttpSetOption(Some(session), WINHTTP_OPTION_RECEIVE_TIMEOUT, Some(&timeout_bytes));
    }
    let _ = DIR_SESSION.set(session as usize);
    Ok(session)
}

/// 启动器 config.ini [room] 未配置时的默认目录地址。
pub const DEFAULT_DIRECTORY: &str = "https://dnet.arcadehub.xyz/roomapi";

#[derive(Clone, Debug)]
pub struct RoomDirClient {
    base: String,
}

/// 目录请求的分类错误（UI 按类别提示）。
#[derive(Clone, Debug)]
pub enum DirError {
    /// 房间不存在 / 已过期（房主据此重新开房自愈）
    NotFound,
    /// 密码错误
    Forbidden,
    /// 房间已满（座位 3/3）
    Full,
    /// 网络/服务不可达
    Transport(String),
    /// 其他服务端错误
    Other(String),
}

impl DirError {
    fn from_status(status: u16, body: &str) -> DirError {
        match status {
            404 => DirError::NotFound,
            403 => DirError::Forbidden,
            409 => DirError::Full,
            _ => DirError::Other(format!("HTTP {status}: {body}")),
        }
    }
    pub fn message(&self) -> String {
        match self {
            DirError::NotFound => t!("房间不存在或已过期", "Room not found or expired").to_string(),
            DirError::Forbidden => t!("房间密码错误", "Wrong room password").to_string(),
            DirError::Full => t!("房间已满（4/4）", "Room is full (4/4)").to_string(),
            DirError::Transport(_e) => tf!("无法连接房间服务器", "Cannot reach the room server").to_string(),
            DirError::Other(m) => m.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RoomSummary {
    pub room_id: String,
    pub name: String,
    pub game: String,
    #[allow(dead_code)]
    pub game_id: String,
    pub players: u32,
    pub max: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct JoinInfo {
    pub host_peer_id: String,
    pub host_ip: String,
    pub host_ip2: String,
    pub your_ip: String,
    pub game: String,
    #[serde(default)]
    pub name: String,
}

/// 房主 keychip 名下的活跃房间（进入店内对战页时查询；存在则直接回归）。
#[derive(Clone, Debug, Deserialize)]
pub struct MyRoom {
    pub room_id: String,
    pub name: String,
    pub game: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemberInfo {
    pub peer_id: String,
    pub ip: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MembersInfo {
    #[allow(dead_code)]
    pub host_peer_id: String,
    pub members: Vec<MemberInfo>,
}

// WinHttpAddRequestHeaders 原始绑定：按「字节数」解析长度（-1 = 自动）。
#[link(name = "winhttp")]
unsafe extern "system" {
    #[link_name = "WinHttpAddRequestHeaders"]
    fn raw_add_request_headers(
        request: *mut core::ffi::c_void,
        headers: *const u16,
        len: u32,
        flags: u32,
    ) -> i32;
}

impl RoomDirClient {
    pub fn new(base: impl Into<String>) -> Self {
        RoomDirClient { base: base.into() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    /// 发一次请求，返回 (HTTP 状态码, 响应体文本)。
    fn request(&self, verb: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
        use windows::Win32::Networking::WinHttp::{
            WinHttpCloseHandle, WinHttpConnect, WinHttpOpenRequest,
            WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
            WinHttpSendRequest, WinHttpSetOption, SECURITY_FLAG_IGNORE_CERT_CN_INVALID,
            WINHTTP_ADDREQ_FLAG_ADD,
            WINHTTP_FLAG_SECURE,
            WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS, WINHTTP_OPTION_SECURITY_FLAGS,
            WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
        };
        let url = self.url(path);
        let url_t = url.trim();
        let (secure, rest) = if let Some(r) = url_t.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url_t.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(t!("目录地址仅支持 http/https", "Directory URL must be http/https").to_string());
        };
        let (host_part, path_part) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match host_part.rfind(':').and_then(|i| host_part[i + 1..].parse::<u16>().ok()) {
            Some(p) => (&host_part[..host_part.rfind(':').unwrap()], p),
            None => (host_part, if secure { 443 } else { 80 }),
        };
        let host = host.to_string();

        unsafe {
            // 复用进程级 WinHTTP 会话（TCP/TLS 连接池），失败路径只关 connect/request，
            // 会话本体跨请求保留。
            let session = shared_session()?;

            let host_w = crate::win_util::to_wide_null(&host);
            let connect = WinHttpConnect(session, PCWSTR::from_raw(host_w.as_ptr()), port, 0);
            if connect.is_null() {
                return Err(tf!("无法连接 {host}", "Cannot connect to {host}").to_string());
            }
            let path_w = crate::win_util::to_wide_null(&path_part);
            let verb_w = crate::win_util::to_wide_null(verb);
            let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
            let request = WinHttpOpenRequest(
                connect,
                PCWSTR::from_raw(verb_w.as_ptr()),
                PCWSTR::from_raw(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if request.is_null() {
                let _ = WinHttpCloseHandle(connect);
                return Err(t!("无法创建请求", "Failed to create request").to_string());
            }
            let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
            let _ = WinHttpSetOption(
                Some(request),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&policy.to_ne_bytes()),
            );
            let sec: u32 = SECURITY_FLAG_IGNORE_CERT_CN_INVALID;
            let _ = WinHttpSetOption(Some(request), WINHTTP_OPTION_SECURITY_FLAGS, Some(&sec.to_ne_bytes()));

            let result = (|| -> Result<(), String> {
                // Content-Type 头（POST 有 JSON 体；DELETE/GET 只有 Host 由 WinHTTP 自动补）
                let hdr = crate::win_util::to_wide_null("Content-Type: application/json\r\n");
                if verb == "POST" {
                    let ok = raw_add_request_headers(request, hdr.as_ptr(), (-1i32) as u32, WINHTTP_ADDREQ_FLAG_ADD);
                    if ok == 0 {
                        return Err(t!("添加请求头失败", "Failed to add request headers").to_string());
                    }
                }
                let body_bytes = body.unwrap_or("").as_bytes();
                let optional: Option<*const core::ffi::c_void> = if body_bytes.is_empty() {
                    None
                } else {
                    Some(body_bytes.as_ptr() as *const core::ffi::c_void)
                };
                let body_len = body_bytes.len() as u32;
                WinHttpSendRequest(request, None, optional, body_len, body_len, 0)
                    .map_err(|e| tf!("发送请求失败：{e}", "Failed to send request: {e}").to_string())?;
                WinHttpReceiveResponse(request, core::ptr::null_mut())
                    .map_err(|e| tf!("接收响应失败：{e}", "Failed to receive response: {e}").to_string())?;
                Ok(())
            })();
            if let Err(e) = result {
                let _ = WinHttpCloseHandle(request);
                let _ = WinHttpCloseHandle(connect);
                return Err(e);
            }

            let mut status: u32 = 0;
            let mut len: u32 = 4;
            let _ = WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut status as *mut u32) as *mut core::ffi::c_void),
                &mut len,
                core::ptr::null_mut(),
            );
            let mut body_bytes: Vec<u8> = Vec::new();
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(request, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let start = body_bytes.len();
                body_bytes.resize(start + avail as usize, 0);
                let mut read: u32 = 0;
                if WinHttpReadData(request, body_bytes.as_mut_ptr().add(start) as *mut core::ffi::c_void, avail, &mut read).is_err() || read == 0 {
                    break;
                }
                body_bytes.truncate(start + read as usize);
            }
            let _ = WinHttpCloseHandle(request);
            let _ = WinHttpCloseHandle(connect);
            Ok((status as u16, String::from_utf8_lossy(&body_bytes).to_string()))
        }
    }

    fn parse_json<T: for<'de> Deserialize<'de>>(status: u16, body: &str) -> Result<T, DirError> {
        if !(200..300).contains(&status) {
            return Err(DirError::from_status(status, body));
        }
        serde_json::from_str(body).map_err(|e| DirError::Other(format!("bad json: {e}")))
    }

    /// 开房，返回 6 位 room_id。`game` = games_db 产品名（展示）；`game_id` = 过滤键；
    /// `keychip` 作为房间归属键（同 keychip 重复开房幂等返回原房间）。
    pub fn create_room(
        &self,
        name: &str,
        game: &str,
        game_id: &str,
        password: &str,
        host_peer_id: &str,
        keychip: &str,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "name": name,
            "game": game,
            "game_id": game_id,
            "password": password,
            "host_peer_id": host_peer_id,
            "keychip": keychip,
        });
        let (status, text) = self.request("POST", "/rooms", Some(&body.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: {text}"));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bad json: {e}"))?;
        v.get("room_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("missing room_id: {text}"))
    }

    /// 公开房间列表（`game_id` = Some 时按游戏过滤，None/空 = 全部）。
    pub fn list_rooms(&self, game_id: Option<&str>) -> Result<Vec<RoomSummary>, String> {
        let path = match game_id.map(str::trim).filter(|v| !v.is_empty()) {
            Some(id) => format!("/rooms?game_id={id}"),
            None => "/rooms".to_string(),
        };
        let (status, text) = self.request("GET", &path, None)?;
        #[derive(Deserialize)]
        struct Resp {
            rooms: Vec<RoomSummary>,
        }
        let r: Resp = Self::parse_json(status, &text).map_err(|e| e.message())?;
        Ok(r.rooms)
    }

    /// 加入房间（验密 + 分配座位），返回接入所需的全部信息。
    pub fn join(&self, room_id: &str, password: &str, peer_id: &str) -> Result<JoinInfo, DirError> {
        let body = serde_json::json!({ "peer_id": peer_id, "password": password });
        let (status, text) = self
            .request("POST", &format!("/rooms/{room_id}/join"), Some(&body.to_string()))
            .map_err(DirError::Transport)?;
        Self::parse_json(status, &text)
    }

    /// 查询 keychip 名下的活跃房间：Some(房间) / None(无)。
    pub fn my_room(&self, keychip: &str) -> Result<Option<MyRoom>, String> {
        let (status, text) = self
            .request("GET", &format!("/rooms/mine?keychip={}", keychip), None)
            .map_err(|e| e)?;
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: {text}"));
        }
        let r: MyRoom = serde_json::from_str(&text).map_err(|e| format!("bad json: {e}"))?;
        Ok(Some(r))
    }

    /// 房主拉取当前成员座位表。
    pub fn members(&self, room_id: &str) -> Result<MembersInfo, DirError> {
        let (status, text) = self
            .request("GET", &format!("/rooms/{room_id}/members"), None)
            .map_err(DirError::Transport)?;
        Self::parse_json(status, &text)
    }

    /// 离开房间（释放座位）。
    pub fn leave(&self, room_id: &str, peer_id: &str) -> Result<(), String> {
        let body = serde_json::json!({ "peer_id": peer_id });
        let (status, _text) = self.request("POST", &format!("/rooms/{room_id}/leave"), Some(&body.to_string()))?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("HTTP {status}"))
        }
    }

    /// 房主关闭房间：host_peer_id（awl 身份）与 keychip（房间归属键，稳定）一并上送，
    /// 服务端任一匹配即放行 —— awl 身份重铸（config 丢失 / dist 重拷）后仍能关掉自己的房间。
    pub fn close(
        &self,
        room_id: &str,
        host_peer_id: Option<&str>,
        keychip: &str,
    ) -> Result<(), String> {
        let mut q = format!("/rooms/{room_id}?keychip={}", keychip.trim());
        if let Some(p) = host_peer_id.map(str::trim).filter(|s| !s.is_empty()) {
            q.push_str(&format!("&host_peer_id={p}"));
        }
        let (status, _text) = self.request("DELETE", &q, None)?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("HTTP {status}"))
        }
    }

    /// 房主心跳保活：携带稳定归属键 keychip 刷新房间 last_heartbeat。
    /// 200 -> Ok(())；其余状态码映射为 DirError（调用方通常忽略）。
    pub fn heartbeat(&self, room_id: &str, keychip: &str) -> Result<(), DirError> {
        let body = serde_json::json!({ "keychip": keychip });
        let (status, text) = self
            .request("POST", &format!("/rooms/{room_id}/heartbeat"), Some(&body.to_string()))
            .map_err(DirError::Transport)?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(DirError::from_status(status, &text))
        }
    }
}
