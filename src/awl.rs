//! awl（Anywherelan v0.19.0-mp2 补丁版）子进程管理与房间接入流程。
//!
//! 设计要点（对齐 PoC F:\Dnet\awl-poc\poc-kit 验证过的流程）：
//! - awl 以子进程方式运行，`AWL_DATA_DIR` 指向启动器私有目录，配置由启动器生成；
//!   `p2pNode.identity` 跨房间保留（机器身份稳定），其余字段每次重写。
//! - 运行期通过本机 HTTP API（127.0.0.1:8639）控制：invite_peer / update_settings /
//!   get_known / peers/remove。
//! - 接入流程（房主发起，成员自动接受）：
//!   成员 join 目录拿座位 → 房主轮询 members 表逐个 invite（指定成员座位 IP）→
//!   成员侧 update_settings 把房主条目修正为 ip=.101（游戏）/ ip2=.102（ServerBox）。
//! - 房主双地址：主 IP .101（游戏，ipNet 写入 awl 配置）+ 副地址 .102（ServerBox，
//!   ipNet2 写入 awl 配置）。awl mp2 起启动时自行把 ipNet2 以增量方式加到 TUN 上
//!   （不再被整卡重配冲掉），入包 dst 保留规则也依赖它识别本机副地址；
//!   `ensure_serverbox_ip` 保留作幂等兜底。
//! - 首次运行无身份：用「禁用 TUN」的临时配置跑一次让 awl 生成身份后立即回收，
//!   再写正式配置二次拉起。

use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::json;

use crate::roomdir::{JoinInfo, RoomDirClient};
use crate::storage::RoomConfig;
use crate::t;
use crate::tf;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LOCAL_API_HOST: &str = "127.0.0.1";
const LOCAL_API_PORT: u16 = 8639;
const AWL_VERSION_EXPECT: &str = "v0.19.0-mp2";
const LOCAL_API_READY_TIMEOUT: Duration = Duration::from_secs(15);
const LINK_TIMEOUT: Duration = Duration::from_secs(30);

/// 房间固定编址（与 PoC/roomdir 约定一致，单房间机制下全服复用同一网段）。
pub const HOST_GAME_IP: &str = "172.10.1.101";
pub const HOST_SERVERBOX_IP: &str = "172.10.1.102";
/// 房间子网（网络地址形式，mice 网段派生 / 防火墙 / TUN 配置共用）。
pub const ROOM_CIDR: &str = "172.10.1.0/24";
/// awl 虚拟网卡名（TUN 配置与「网络设置」页识别共用）。
pub const AWL_IFACE_NAME: &str = "awl0";
const AWL_IFACE: &str = AWL_IFACE_NAME;

/// 房间会话状态（UI 状态机 / 状态栏展示）。
#[derive(Clone, PartialEq, Debug)]
pub enum RoomStatus {
    /// 未在任何房间
    Idle,
    /// 开房中（目录注册 + awl 拉起）
    Creating { name: String },
    /// 房主就绪，等待/已有成员
    Hosting { room_id: String, name: String },
    /// 加入中（目录验密 + awl 拉起 + 对接）
    Joining { room_id: String },
    /// 已加入（房主或成员）
    Joined { room_id: String, is_host: bool },
}

/// 从 get_known 提取的单个对端视图（UI 成员列表 / 掉线检测共用）。
#[derive(Clone, Debug)]
pub struct PeerView {
    pub peer_id: String,
    pub alias: String,
    pub ip: String,
    #[allow(dead_code)]
    pub ip2: String,
    pub connected: bool,
    pub confirmed: bool,
    pub ping_ms: f64,
    pub through_relay: bool,
}

/// 持有 awl 子进程；析构时 kill 进程并关闭作业对象。
/// `job` 设置了 KILL_ON_JOB_CLOSE：launcher 无论正常退出/崩溃/被强杀，系统都收掉 awl。
pub struct AwlHandle {
    child: Child,
    job: Option<usize>,
}

impl Drop for AwlHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        crate::vpn::close_job(self.job);
    }
}

pub struct AwlManager;

impl AwlManager {
    /// 解析 awl-tray.exe 路径：优先配置项，缺省 `launcher/tools/awl/awl-tray.exe`。
    pub fn exe_path(cfg: &RoomConfig) -> PathBuf {
        if let Some(p) = &cfg.awl_path {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
        crate::storage::launcher_dir().join("tools").join("awl").join("awl-tray.exe")
    }

    fn data_dir() -> PathBuf {
        crate::storage::data_dir().join("awl")
    }

    fn config_path() -> PathBuf {
        Self::data_dir().join("config_awl.json")
    }

    fn read_json_config() -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(Self::config_path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 从 config_awl.json 读取本机身份（peerId）。
    pub fn my_peer_id() -> Result<String, String> {
        let v = Self::read_json_config().ok_or_else(|| {
            t!("awl 身份未初始化", "awl identity not initialized").to_string()
        })?;
        v.pointer("/p2pNode/peerId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| t!("awl 身份缺失（peerId）", "awl identity missing (peerId)").to_string())
    }

    fn has_identity() -> bool {
        Self::read_json_config()
            .and_then(|v| {
                v.pointer("/p2pNode/identity")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
            })
            .unwrap_or(false)
    }

    /// 生成 awl 配置。`ip_net` 形如 "172.10.1.102/24"；`ip_net2` 为可选副地址
    /// （房主侧传 ServerBox .102，awl mp2 起启动时自行追加到 TUN，且入包 dst
    /// 保留规则依赖它识别本机副地址；成员侧无副地址传 None）。
    /// `disable_tun` 仅用于首次身份生成。已存在的身份（p2pNode.identity/peerId）原样保留。
    fn write_config(ip_net: &str, ip_net2: Option<&str>, disable_tun: bool) -> Result<(), String> {
        let dir = Self::data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| {
            tf!("创建 awl 数据目录失败：{e}", "Failed to create awl data dir: {e}")
        })?;
        let old = Self::read_json_config();
        let identity = old
            .as_ref()
            .and_then(|v| v.pointer("/p2pNode/identity").and_then(|v| v.as_str()))
            .unwrap_or("");
        let peer_id = old
            .as_ref()
            .and_then(|v| v.pointer("/p2pNode/peerId").and_then(|v| v.as_str()))
            .unwrap_or("");

        let mut p2p = json!({
            "name": "sasayaki",
            "autoAcceptAuthRequests": true,
            "bootstrapPeers": [],
            "listenAddresses": [],
            "reconnectionIntervalSec": 20,
        });
        if !identity.is_empty() {
            p2p["identity"] = json!(identity);
        }
        if !peer_id.is_empty() {
            p2p["peerId"] = json!(peer_id);
        }
        let mut vpn = json!({
            "interfaceName": AWL_IFACE,
            "ipNet": ip_net,
            "disableVPNInterface": disable_tun,
        });
        if let Some(ip_net2) = ip_net2 {
            vpn["ipNet2"] = json!(ip_net2);
        }
        let cfg = json!({
            "loggerLevel": "info",
            "httpListenAddress": format!("{LOCAL_API_HOST}:{LOCAL_API_PORT}"),
            "p2pNode": p2p,
            "vpn": vpn,
            // 注意：listenerEnabled:false 单独写会被 awl 的零值检查重置，必须连 listenAddress 一起写非空
            "socks5": {
                "listenerEnabled": false,
                "proxyingEnabled": false,
                "listenAddress": "127.0.0.66:8080",
            },
            "dns": { "disableDNS": true },
        });
        std::fs::write(Self::config_path(), serde_json::to_string_pretty(&cfg).unwrap())
            .map_err(|e| tf!("写入 awl 配置失败：{e}", "Failed to write awl config: {e}"))
    }

    /// 本机 awl HTTP API 调用（HTTP/1.0，读满即关，无分块编码问题）。
    fn local_request(verb: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect((LOCAL_API_HOST, LOCAL_API_PORT))
            .map_err(|e| tf!("连接 awl API 失败：{e}", "Cannot reach awl API: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let body = body.unwrap_or("");
        let req = format!(
            "{verb} {path} HTTP/1.0\r\nHost: {LOCAL_API_HOST}:{LOCAL_API_PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        use std::io::Write;
        stream
            .write_all(req.as_bytes())
            .map_err(|e| tf!("发送 awl API 请求失败：{e}", "Failed to send awl API request: {e}"))?;
        let mut resp = Vec::new();
        stream
            .read_to_end(&mut resp)
            .map_err(|e| tf!("读取 awl API 响应失败：{e}", "Failed to read awl API response: {e}"))?;
        let text = String::from_utf8_lossy(&resp);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| t!("awl API 响应异常", "Unexpected awl API response").to_string())?;
        Ok((status, body.to_string()))
    }

    /// 版本自检：`awl-tray.exe -v` 必须包含期望版本（两台机器与补丁版一致性靠它保证）。
    pub fn version_check(exe: &Path) -> Result<(), String> {
        let out = Command::new(exe)
            .arg("-v")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| tf!("无法执行 awl-tray.exe：{e}", "Cannot execute awl-tray.exe: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.contains(AWL_VERSION_EXPECT) {
            Ok(())
        } else {
            Err(tf!(
                "awl-tray.exe 版本不符：期望 {AWL_VERSION_EXPECT}，实际：{}",
                "awl-tray.exe version mismatch: expected {AWL_VERSION_EXPECT}, got: {}",
                text.trim()
            ))
        }
    }

    /// 防火墙幂等放行（launcher 已提权；失败不致命——首连失败时 awl 日志可见）。
    fn ensure_firewall(exe: &Path) {
        let _ = Command::new("netsh")
            .args(["advfirewall", "firewall", "delete", "rule", "name=Sasayaki Awl"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = Command::new("netsh")
            .args([
                "advfirewall", "firewall", "add", "rule", "name=Sasayaki Awl",
                "dir=in", "action=allow", "enable=yes",
                &format!("program={}", exe.display()),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = Command::new("netsh")
            .args(["advfirewall", "firewall", "delete", "rule", "name=Sasayaki Awl ICMP"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = Command::new("netsh")
            .args([
                "advfirewall", "firewall", "add", "rule", "name=Sasayaki Awl ICMP",
                "protocol=icmpv4:8,any", "dir=in", "action=allow",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }

    /// PowerShell 查询某 IP 是否已在指定网卡上。
    fn ps_ip_present(ip: &str, iface: &str) -> bool {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "if(Get-NetIPAddress -IPAddress {ip} -InterfaceAlias {iface} -ErrorAction SilentlyContinue){{'1'}}else{{'0'}}"
                ),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        matches!(&out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "1")
    }

    /// 通过 CreateUnicastIpAddressEntry 追加地址：纯增量，不触碰网卡上已有地址。
    /// （netsh add 在 wintun 网卡上会整卡覆盖 .101 —— 实测踩坑；PoC 时代已验证此 API 安全。）
    fn add_unicast_ip(iface: &str, ip: &str, prefix_len: u8) -> bool {
        use windows::Win32::NetworkManagement::IpHelper::{
            ConvertInterfaceAliasToLuid, CreateUnicastIpAddressEntry, InitializeUnicastIpAddressEntry,
            MIB_UNICASTIPADDRESS_ROW,
        };
        use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
        use windows::core::PCWSTR;

        let octets: [u8; 4] = match ip.split('.').map(|o| o.parse::<u8>()).collect::<Result<Vec<_>, _>>() {
            Ok(v) if v.len() == 4 => [v[0], v[1], v[2], v[3]],
            _ => return false,
        };
        unsafe {
            let alias_w = crate::win_util::to_wide_null(iface);
            let mut luid = NET_LUID_LH::default();
            if ConvertInterfaceAliasToLuid(PCWSTR::from_raw(alias_w.as_ptr()), &mut luid).is_err() {
                return false;
            }
            let mut row = MIB_UNICASTIPADDRESS_ROW::default();
            InitializeUnicastIpAddressEntry(&mut row);
            row.InterfaceLuid = luid;
            // MIB_UNICASTIPADDRESS_ROW.Address 是 SOCKADDR_INET 联合体：
            // 内存布局 offset 0-1 = 地址族（AF_INET=2，小端），offset 4-7 = IPv4 地址
            let mut sinet = windows::Win32::Networking::WinSock::SOCKADDR_INET::default();
            let base = &mut sinet as *mut _ as *mut u8;
            *base.add(0) = 2;
            *base.add(1) = 0;
            core::ptr::copy_nonoverlapping(octets.as_ptr(), base.add(4), 4);
            row.Address = sinet;
            row.OnLinkPrefixLength = prefix_len;
            CreateUnicastIpAddressEntry(&row).0 == 0
        }
    }

    /// 幂等兜底：确保 ServerBox IP（.102）在 awl0 网卡上。
    /// awl mp2 起按配置里的 ipNet2 在启动时自行追加 .102（增量语义），此时本函数
    /// 检查到地址已存在即返回，零开销；仅对未声明 ipNet2 的旧版 awl 才真正补地址。
    /// 时序：先等 awl 把主地址 .101 写上，再检查/追加 .102。
    pub fn ensure_serverbox_ip() -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !Self::ps_ip_present(HOST_GAME_IP, AWL_IFACE_NAME) {
            if Instant::now() >= deadline {
                return false; // awl 主地址一直没出现,此时追加会被 set 语义冲掉
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        if Self::ps_ip_present(HOST_SERVERBOX_IP, AWL_IFACE_NAME) {
            return true;
        }
        for _ in 0..3 {
            if Self::add_unicast_ip(AWL_IFACE_NAME, HOST_SERVERBOX_IP, 24)
                && Self::ps_ip_present(HOST_SERVERBOX_IP, AWL_IFACE_NAME)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }

    /// 首次运行时生成 awl 身份：写「禁用 TUN」的临时配置跑一次，等身份落盘后立即回收。
    pub fn ensure_identity(exe: &Path) -> Result<(), String> {
        if Self::has_identity() {
            return Ok(());
        }
        // 身份生成用的临时配置（禁用 TUN），随后各流程会重写正式配置；
        // 此时区分不了房主/成员，ipNet2 一律不写。
        Self::write_config(&format!("{HOST_GAME_IP}/24"), None, true)?;
        let handle = Self::spawn_awl(exe)?;
        let deadline = Instant::now() + LOCAL_API_READY_TIMEOUT;
        loop {
            if Self::has_identity() {
                drop(handle);
                return Ok(());
            }
            if Instant::now() >= deadline {
                drop(handle);
                return Err(t!("awl 未能在时限内生成身份", "awl failed to mint identity in time").to_string());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// 拉起 awl 子进程并等待本机 API 就绪。
    fn spawn_awl(exe: &Path) -> Result<AwlHandle, String> {
        let mut cmd = Command::new(exe);
        cmd.env("AWL_DATA_DIR", Self::data_dir());
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd
            .spawn()
            .map_err(|e| tf!("启动 awl 失败（{}）：{e}", "Failed to start awl ({}): {e}", exe.display()))?;
        let job = crate::vpn::attach_job(&child);

        let deadline = Instant::now() + LOCAL_API_READY_TIMEOUT;
        loop {
            if let Ok(Some(_)) = child.try_wait() {
                crate::vpn::close_job(job);
                let _ = child.wait();
                return Err(t!(
                    "awl 启动后立即退出（常见原因：杀软拦截或缺少管理员权限）",
                    "awl exited immediately (common causes: antivirus or missing admin rights)"
                )
                .to_string());
            }
            match Self::local_request("GET", "/api/v0/peers/get_known", None) {
                Ok((200, _)) => return Ok(AwlHandle { child, job }),
                _ => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        crate::vpn::close_job(job);
                        return Err(t!(
                            "awl API 未在时限内就绪",
                            "awl API did not become ready in time"
                        )
                        .to_string());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// 从 get_known 解析对端视图（失败返回空表）。
    pub fn known_peers() -> Vec<PeerView> {
        let Ok((200, body)) = Self::local_request("GET", "/api/v0/peers/get_known", None) else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            return Vec::new();
        };
        let Some(arr) = v.as_array() else { return Vec::new() };
        arr.iter()
            .map(|p| PeerView {
                peer_id: p.get("PeerID").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                alias: p.get("Alias").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                ip: p.get("IpAddr").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                ip2: p.get("IpAddr2").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                connected: p.get("Connected").and_then(|v| v.as_bool()).unwrap_or(false),
                confirmed: p.get("Confirmed").and_then(|v| v.as_bool()).unwrap_or(false),
                ping_ms: p.get("Ping").and_then(|v| v.as_f64()).unwrap_or(0.0) / 1e6,
                through_relay: p
                    .get("Connections")
                    .and_then(|v| v.as_array())
                    .map(|cs| cs.iter().any(|c| c.get("ThroughRelay").and_then(|v| v.as_bool()).unwrap_or(false)))
                    .unwrap_or(false),
            })
            .collect()
    }

    /// 成员侧：对接房主（invite → 等 confirmed → update_settings 修正房主 IP）。
    /// 成员侧登记房主的两个地址：游戏 .101（ipAddr）+ ServerBox .102（ipAddr2），
    /// 成员对两个地址都要可达。
    pub fn link_member_to_host(
        host_peer_id: &str,
        host_game_ip: &str,
        host_serverbox_ip: &str,
    ) -> Result<(), String> {
        let invite = json!({
            "peerId": host_peer_id,
            "ipAddr": host_game_ip,
            "ipAddr2": host_serverbox_ip,
            "alias": "host",
        });
        let (status, body) =
            Self::local_request("POST", "/api/v0/peers/invite_peer", Some(&invite.to_string()))?;
        // "peer has already been added" 视为成功（重入场景）
        if status != 200 && !body.contains("already been added") {
            return Err(tf!("invite 房主失败：{body}", "Failed to invite host: {body}"));
        }
        // 等双向 confirmed（成员自动接受 + 房主自动接受）
        let deadline = Instant::now() + LINK_TIMEOUT;
        loop {
            let linked = Self::known_peers()
                .iter()
                .any(|p| p.peer_id == host_peer_id && p.confirmed && p.connected);
            if linked {
                break;
            }
            if Instant::now() >= deadline {
                return Err(t!("等待房主确认超时", "Timed out waiting for host confirmation").to_string());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        // 修正房主在本机的条目：游戏 .101 + ServerBox .102
        let update = json!({
            "peerId": host_peer_id,
            "ipAddr": host_game_ip,
            "ipAddr2": host_serverbox_ip,
            "alias": "host",
            "domainName": "host",
        });
        let (status, body) = Self::local_request(
            "POST",
            "/api/v0/peers/update_settings",
            Some(&update.to_string()),
        )?;
        if status != 200 {
            return Err(tf!("修正房主 IP 失败：{body}", "Failed to update host IP: {body}"));
        }
        Ok(())
    }

    /// 房主侧：邀请一个成员（指定其座位 IP）。重复邀请（already added）视为成功。
    pub fn invite_member(member_peer_id: &str, seat_ip: &str) -> Result<(), String> {
        let alias = format!("join-{}", seat_ip.rsplit('.').next().unwrap_or("m"));
        let invite = json!({
            "peerId": member_peer_id,
            "ipAddr": seat_ip,
            "alias": alias,
        });
        let (status, body) =
            Self::local_request("POST", "/api/v0/peers/invite_peer", Some(&invite.to_string()))?;
        // "already been added" = 成员先反向邀请、本机已自动接受：条目存在但 IP 是 awl
        // 自动分配的（子网内下一个空闲，可能撞 ServerBox 的 .102），座位 IP 并未生效——
        // 因此无论 invite 是否新建条目，都必须再用 update_settings 强制校正为座位 IP。
        if status != 200 && !body.contains("already been added") {
            return Err(tf!("invite 成员失败：{body}", "Failed to invite member: {body}"));
        }
        let update = json!({
            "peerId": member_peer_id,
            "ipAddr": seat_ip,
            "alias": alias,
            "domainName": alias,
        });
        let (status, body) = Self::local_request(
            "POST",
            "/api/v0/peers/update_settings",
            Some(&update.to_string()),
        )?;
        if status != 200 {
            return Err(tf!(
                "校正成员座位 IP 失败：{body}",
                "Failed to correct member seat IP: {body}"
            ));
        }
        Ok(())
    }

    /// 清空本机 knownPeers（离开/关房时调用；逐个 remove，忽略单个失败）。
    pub fn remove_all_peers() {
        for p in Self::known_peers() {
            let body = json!({ "peerId": p.peer_id });
            let _ = Self::local_request("POST", "/api/v0/peers/remove", Some(&body.to_string()));
        }
    }

    /// 房主全流程：身份 → 配置 → 防火墙 → 拉起 → 目录开房 → 补 .101。
    /// 返回 (句柄, room_id)。
    pub fn host_flow(
        room_cfg: &RoomConfig,
        name: &str,
        game: &str,
        game_id: &str,
        password: &str,
        keychip: &str,
        dir: &RoomDirClient,
    ) -> Result<(AwlHandle, String), String> {
        let exe = Self::exe_path(room_cfg);
        if !exe.is_file() {
            return Err(tf!(
                "找不到 awl-tray.exe：{}\n（请将 awl-tray.exe 放入 tools/awl/，或在 config.ini 的 [room] 设置 awl= 绝对路径）",
                "awl-tray.exe not found: {}\n(Put awl-tray.exe under tools/awl/, or set awl= absolute path in config.ini [room])",
                exe.display()
            ));
        }
        Self::version_check(&exe)?;
        Self::ensure_identity(&exe)?;
        Self::write_config(
            &format!("{HOST_GAME_IP}/24"),
            Some(&format!("{HOST_SERVERBOX_IP}/24")),
            false,
        )?;
        Self::ensure_firewall(&exe);
        let handle = Self::spawn_awl(&exe)?;
        let my_peer_id = match Self::my_peer_id() {
            Ok(id) => id,
            Err(e) => {
                drop(handle);
                return Err(e);
            }
        };
        let room_id = match dir.create_room(name, game, game_id, password, &my_peer_id, keychip) {
            Ok(id) => id,
            Err(e) => {
                drop(handle);
                return Err(e);
            }
        };
        if !Self::ensure_serverbox_ip() {
            // 非致命：.102 没补上时 ServerBox 不可达，但成员仍可连 .101；提示由 UI 层追加
            eprintln!("warn: failed to add serverbox ip {HOST_SERVERBOX_IP}");
        }
        Ok((handle, room_id))
    }

    /// 房主回归流程：本 keychip 在目录上已有活跃房间时，跳过开房直接拉起 awl。
    /// 与首次开房一致：拉起后幂等确保 ServerBox IP .102 在网卡上（兜底）。
    pub fn host_resume_flow(room_cfg: &RoomConfig) -> Result<AwlHandle, String> {
        let exe = Self::exe_path(room_cfg);
        if !exe.is_file() {
            return Err(tf!(
                "找不到 awl-tray.exe：{}
（请将 awl-tray.exe 放入 tools/awl/，或在 config.ini 的 [room] 设置 awl= 绝对路径）",
                "awl-tray.exe not found: {}
(Put awl-tray.exe under tools/awl/, or set awl= absolute path in config.ini [room])",
                exe.display()
            ));
        }
        Self::version_check(&exe)?;
        Self::ensure_identity(&exe)?;
        Self::write_config(
            &format!("{HOST_GAME_IP}/24"),
            Some(&format!("{HOST_SERVERBOX_IP}/24")),
            false,
        )?;
        Self::ensure_firewall(&exe);
        let handle = Self::spawn_awl(&exe)?;
        let _ = Self::ensure_serverbox_ip();
        Ok(handle)
    }

    /// 成员全流程：身份 → 目录验密拿座位 → 配置 → 防火墙 → 拉起 → 对接房主。
    pub fn join_flow(
        room_cfg: &RoomConfig,
        room_id: &str,
        password: &str,
        dir: &RoomDirClient,
    ) -> Result<(AwlHandle, JoinInfo), String> {
        let exe = Self::exe_path(room_cfg);
        if !exe.is_file() {
            return Err(tf!(
                "找不到 awl-tray.exe：{}\n（请将 awl-tray.exe 放入 tools/awl/，或在 config.ini 的 [room] 设置 awl= 绝对路径）",
                "awl-tray.exe not found: {}\n(Put awl-tray.exe under tools/awl/, or set awl= absolute path in config.ini [room])",
                exe.display()
            ));
        }
        Self::version_check(&exe)?;
        Self::ensure_identity(&exe)?;
        let my_peer_id = Self::my_peer_id()?;
        let join = dir.join(room_id, password, &my_peer_id).map_err(|e| e.message())?;
        // 成员无副地址，不写 ipNet2
        Self::write_config(&format!("{}/24", join.your_ip), None, false)?;
        Self::ensure_firewall(&exe);
        let handle = Self::spawn_awl(&exe)?;
        if let Err(e) = Self::link_member_to_host(&join.host_peer_id, &join.host_ip, &join.host_ip2) {
            drop(handle);
            return Err(e);
        }
        Ok((handle, join))
    }
}
