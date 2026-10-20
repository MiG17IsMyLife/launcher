//! 加载器自动更新核心：仓库 Release 解析 + 版本比对 + 整目录下载/校验/替换（带回滚）。
//!
//! 设计要点：
//! - 更新源 = 按界面语言为每个组件选定的唯一仓库（中文→GitCode，英文→GitHub），并行查询；
//! - 取**最新** Release：优先单对象端点 `/releases/latest`；数组端点按「新→旧」重排
//!   （GitCode 的 `/releases` 是**升序**的，直接取首个会拿到最旧的一条）；
//! - 从 Release 资产名识别加载器（kitten.zip / mice.zip，兼容内嵌版本命名），版本取 tag 或文件名；
//!   最新一条没有对应资产时，按「新→旧」回退到更早的一条；
//! - 判定主键为版本号（点分段数值比较），sha256（GitHub asset digest）仅作下载后完整性校验；
//! - 更新源返回非 200 / 查询失败时统一按「暂无更新」显示，不轮询其他镜像源；
//! - 替换范围 = 整目录覆盖 `tools/<loader>`（含依赖），替换前做备份、失败回滚。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_SECURITY_FLAGS, SECURITY_FLAG_IGNORE_CERT_CN_INVALID,
    WINHTTP_OPTION_CONNECT_TIMEOUT, WINHTTP_OPTION_RECEIVE_TIMEOUT,
    WINHTTP_OPTION_IPV6_FAST_FALLBACK,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS,
    WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::i18n::{self, Lang};
use crate::storage::{self, Settings, launcher_dir};
use crate::t;
use crate::tf;

/// 更新目标加载器。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UpdateTarget {
    Kitten,
    Mice,
    Segatools,
    /// Ultimate-ASI-Loader（GitHub ThirteenAG，win32 NoPDB 变体）
    AsiLoader,
    /// shaderfix-idas（Tendokyu Gitea puniru，IDAS 着色器修复，需 ASI Loader）
    ShaderFix,
    /// awl（AnywhereLAN，虚拟局域网组件，从 git Release 拉取 awl-tray.exe）
    Awl,
}

impl UpdateTarget {
    /// 目录名 / loader 名（同时用于匹配 Release 资产名前缀）。
    pub fn dir_name(&self) -> &'static str {
        match self {
            UpdateTarget::Kitten => "kitten",
            UpdateTarget::Mice => "mice",
            UpdateTarget::Segatools => "segatools",
            UpdateTarget::AsiLoader => "asi-loader",
            UpdateTarget::ShaderFix => "shaderfix",
            UpdateTarget::Awl => "awl",
        }
    }

    /// 组件行显示名。
    pub fn display_name(&self) -> &'static str {
        match self {
            UpdateTarget::Kitten => "kittentools",
            UpdateTarget::Mice => "micetools",
            UpdateTarget::Segatools => "segatools",
            UpdateTarget::AsiLoader => "Ultimate-ASI-Loader",
            UpdateTarget::ShaderFix => "shaderfix-idas",
            UpdateTarget::Awl => "awl",
        }
    }

    /// 组件的默认仓库列表；按 [`select_repo`] 依据界面语言选定唯一更新源。
    pub fn default_repos(&self) -> &'static [&'static str] {
        match self {
            UpdateTarget::Kitten => &[
                "https://github.com/project-sasayaki/kittentools",
                "https://gitcode.com/project-sasayaki/kittentools",
            ],
            UpdateTarget::Mice => &[
                "https://github.com/project-sasayaki/mice-inid-release",
                "https://gitcode.com/project-sasayaki/mice-inid-release"
            ],
            UpdateTarget::Segatools => &[
                "https://github.com/project-sasayaki/segatools-inid-release",
                "https://gitcode.com/project-sasayaki/segatools-inid-release"
            ],
            UpdateTarget::AsiLoader => &[
                "https://github.com/ThirteenAG/Ultimate-ASI-Loader",
                "https://gitcode.com/project-sasayaki/Ultimate-ASI-Loader"],
            UpdateTarget::ShaderFix => &["https://gitea.tendokyu.moe/puniru/shaderfix-idas"],
            // 语言选定：中文 → GitCode，英文 → GitHub（select_repo 依据 i18n::Lang）
            UpdateTarget::Awl => &[
                "https://github.com/project-sasayaki/awl",
                "https://gitcode.com/project-sasayaki/awl",
            ],
        }
    }

    /// Release 资产名（小写）是否匹配该组件：
    /// - kitten / mice / segatools：前缀匹配的 zip；
    /// - asi-loader：win32 NoPDB 变体（排除 x64 与带 PDB 包）；
    /// - shaderfix：固定 shaderfix.zip。
    pub fn asset_matches(&self, name_lc: &str) -> bool {
        match self {
            UpdateTarget::Kitten | UpdateTarget::Mice | UpdateTarget::Segatools => {
                name_lc.starts_with(self.dir_name()) && name_lc.ends_with(".zip")
            }
            UpdateTarget::AsiLoader => {
                name_lc.starts_with("ultimate-asi-loader")
                    && name_lc.contains("nopdb")
                    && !name_lc.contains("x64")
            }
            UpdateTarget::ShaderFix => name_lc == "shaderfix.zip",
            // awl：Windows 构建，资产形如 awl.zip / awl-windows*.zip / awl_v0.19.0*.zip
            UpdateTarget::Awl => name_lc.starts_with("awl") && name_lc.ends_with(".zip"),
        }
    }
}

pub const ALL_TARGETS: [UpdateTarget; 6] = [
    UpdateTarget::Kitten,
    UpdateTarget::Mice,
    UpdateTarget::Segatools,
    UpdateTarget::AsiLoader,
    UpdateTarget::ShaderFix,
    UpdateTarget::Awl,
];

/// 单加载器更新状态（仅用于「未配置/有更新/已最新」等粗粒度指示）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpdateState {
    NotConfigured,
    Checking,
    UpToDate,
    Outdated,
    Error,
}

/// 更新进行中的阶段（驱动进度条文案）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Downloading,
    Extracting,
    Installing,
    Done,
    Error,
}

/// 单个加载器的实时状态（共享给后台线程写、UI 读）。
#[derive(Clone, Debug)]
pub struct TargetStatus {
    pub state: UpdateState,
    pub phase: UpdatePhase,
    pub download_done: u64,
    pub download_total: u64,
    /// Release 解析出的远端版本（供安装后回写 baseline）
    pub remote_version: Option<String>,
    /// 首选下载地址
    pub url: Option<String>,
    /// 可选 sha256（下载后校验）
    pub sha256: Option<String>,
    /// 备用下载地址
    pub mirrors: Vec<String>,
    /// 展示文案，如 "1.4.2 → 1.5.0"
    pub detail: Option<String>,
}

impl Default for TargetStatus {
    fn default() -> Self {
        TargetStatus {
            state: UpdateState::NotConfigured,
            phase: UpdatePhase::Idle,
            download_done: 0,
            download_total: 0,
            remote_version: None,
            url: None,
            sha256: None,
            mirrors: Vec::new(),
            detail: None,
        }
    }
}

/// 全部加载器的更新状态（进程内共享）。
#[derive(Clone, Debug)]
pub struct UpdateStatus {
    pub per_target: HashMap<UpdateTarget, TargetStatus>,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        let mut per_target = HashMap::new();
        for &t in ALL_TARGETS.iter() {
            per_target.insert(t, TargetStatus::default());
        }
        UpdateStatus { per_target }
    }
}

/// 从仓库 Release 解析出的单个加载器更新条目。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoaderRelease {
    pub name: String,
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// 全部加载器的远端更新索引（多仓库 Release 解析合并后的结果）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UpdateIndex {
    pub loaders: Vec<LoaderRelease>,
}

/// Release 资产（GitHub / GitCode / Gitea 响应同构）。
#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    /// GitHub 提供的完整性摘要，形如 "sha256:abcd..."；其他平台无此字段。
    #[serde(default)]
    pub digest: Option<String>,
}

/// Release 响应（只取需要的字段）。
///
/// `release_status` / `created_at` 仅 GitCode / Gitee 系平台提供，用于从**数组端点**
/// 返回的 Release 列表中识别哪一条是最新的（GitCode 的 `/releases` 按发布时间**升序**
/// 返回，直接取首个会拿到最旧的一条）。
#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
    /// GitCode 标记最新 Release 为 "latest"（其余为 "none"）。
    #[serde(default)]
    pub release_status: Option<String>,
    /// ISO8601 创建时间，如 "2026-10-11T10:56:09+08:00"。
    #[serde(default)]
    pub created_at: Option<String>,
}

// ----------------------------------------------------------------------------
// 版本比较（点分段数值比较，避免 "1.10" < "1.9" 这类字典序错误）
// ----------------------------------------------------------------------------

/// 解析版本号为纯数字段（去掉前导 v，忽略非数字后缀）。
fn parse_version(s: &str) -> Vec<u64> {
    s.trim_start_matches('v')
        .split(['.', '-', '+'])
        .filter_map(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .collect()
}

/// 比较两个版本：Less / Equal / Greater。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = parse_version(a);
    let pb = parse_version(b);
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

// ----------------------------------------------------------------------------
// 缓存（24h 内不重拉；手动检查可破缓存）
// ----------------------------------------------------------------------------

/// 缓存结构版本：解析规则变更（如修掉「拿到最旧 Release」这类问题）时递增，
/// 使旧版本写入的 `update_cache.json` 自动失效，无需用户手工删除。
const CACHE_SCHEMA: u32 = 2;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    schema: u32,
    saved_at: u64,
    index: UpdateIndex,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读取未过期的缓存索引（24h 内，且结构版本匹配）。
pub fn load_cache() -> Option<UpdateIndex> {
    let path = storage::data_dir().join("update_cache.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let c: CacheFile = serde_json::from_str(&text).ok()?;
    if c.schema != CACHE_SCHEMA {
        return None;
    }
    if now_secs().saturating_sub(c.saved_at) < 24 * 3600 {
        Some(c.index)
    } else {
        None
    }
}

/// 写入缓存索引：与「仍有效（<24h）」的旧缓存按组件名合并，
/// 只覆盖本次查到的组件，保留其余组件的上次结果（避免子集补查把其它组件冲掉）。
pub fn save_cache(index: &UpdateIndex) {
    let path = storage::data_dir().join("update_cache.json");
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("创建更新缓存目录失败：{e}");
        }
    }
    let mut merged = load_cache().unwrap_or_default();
    for l in &index.loaders {
        if let Some(slot) = merged.loaders.iter_mut().find(|x| x.name == l.name) {
            *slot = l.clone();
        } else {
            merged.loaders.push(l.clone());
        }
    }
    let c = CacheFile {
        schema: CACHE_SCHEMA,
        saved_at: now_secs(),
        index: merged,
    };
    if let Ok(s) = serde_json::to_string_pretty(&c) {
        if let Err(e) = std::fs::write(&path, s) {
            eprintln!("写入更新缓存失败：{e}");
        }
    }
}

// ----------------------------------------------------------------------------
// 共享状态辅助
// ----------------------------------------------------------------------------

/// 读取某加载器已安装版本（作为比对基线）。
pub fn last_version_of(settings: &Settings, t: UpdateTarget) -> Option<String> {
    match t {
        UpdateTarget::Kitten => settings.last_kitten_version.clone(),
        UpdateTarget::Mice => settings.last_mice_version.clone(),
        UpdateTarget::Segatools => settings.last_segatools_version.clone(),
        UpdateTarget::AsiLoader => settings.last_asiloader_version.clone(),
        UpdateTarget::ShaderFix => settings.last_shaderfix_version.clone(),
        UpdateTarget::Awl => settings.last_awl_version.clone(),
    }
}

    /// 写回某加载器已安装版本（安装成功后调用）。
pub fn set_last_version(settings: &mut Settings, t: UpdateTarget, v: &str) {
    match t {
        UpdateTarget::Kitten => settings.last_kitten_version = Some(v.to_string()),
        UpdateTarget::Mice => settings.last_mice_version = Some(v.to_string()),
        UpdateTarget::Segatools => settings.last_segatools_version = Some(v.to_string()),
        UpdateTarget::AsiLoader => settings.last_asiloader_version = Some(v.to_string()),
        UpdateTarget::ShaderFix => settings.last_shaderfix_version = Some(v.to_string()),
        UpdateTarget::Awl => settings.last_awl_version = Some(v.to_string()),
    }
}

/// 把 check 结果写入共享状态。
pub fn apply_check(shared: &Arc<Mutex<UpdateStatus>>, results: HashMap<UpdateTarget, TargetStatus>) {
    if let Ok(mut s) = shared.lock() {
        for (t, ts) in results {
            if let Some(slot) = s.per_target.get_mut(&t) {
                *slot = ts;
            }
        }
    }
}

/// 把所有加载器标记为「未配置」（源为空时）。
/// 将单个组件标记为「未配置更新源」（该组件无默认仓库且未配置源时）。
#[allow(dead_code)]
pub fn mark_component_not_configured(shared: &Arc<Mutex<UpdateStatus>>, target: UpdateTarget) {
    if let Ok(mut s) = shared.lock() {
        if let Some(ts) = s.per_target.get_mut(&target) {
            ts.state = UpdateState::NotConfigured;
            ts.phase = UpdatePhase::Idle;
            ts.url = None;
            ts.remote_version = None;
            ts.sha256 = None;
            ts.mirrors.clear();
            ts.detail = None;
        }
    }
}

/// 单组件比对：远端条目缺失（源返回非 200 / 查询失败）统一显示「暂无更新」。
pub fn check_one(
    target: UpdateTarget,
    rel: Option<&LoaderRelease>,
    settings: &Settings,
) -> TargetStatus {
    let Some(r) = rel else {
        return TargetStatus {
            state: UpdateState::UpToDate,
            detail: Some(t!("暂无更新", "No updates available").to_string()),
            ..Default::default()
        };
    };
    let local = last_version_of(settings, target);
    let cmp = local
        .as_deref()
        .map(|v| compare_versions(&r.version, v))
        .unwrap_or(std::cmp::Ordering::Greater);
    let state = if cmp == std::cmp::Ordering::Greater {
        UpdateState::Outdated
    } else {
        UpdateState::UpToDate
    };
    let detail = Some(match local {
        Some(v) => format!("{} → {}", v, r.version),
        None => r.version.clone(),
    });
    TargetStatus {
        state,
        phase: UpdatePhase::Idle,
        download_done: 0,
        download_total: 0,
        remote_version: Some(r.version.clone()),
        url: Some(r.url.clone()),
        sha256: r.sha256.clone(),
        mirrors: Vec::new(),
        detail,
    }
}

/// 比对远端索引与各加载器本地基线，产出完整 TargetStatus。
/// 索引缺失的组件 = 其更新源返回非 200 / 查询失败，统一显示「暂无更新」。
pub fn check_updates(index: &UpdateIndex, settings: &Settings) -> HashMap<UpdateTarget, TargetStatus> {
    let mut map = HashMap::new();
    for &target in ALL_TARGETS.iter() {
        let remote = index
            .loaders
            .iter()
            .find(|l| l.name == target.dir_name());
        map.insert(target, check_one(target, remote, settings));
    }
    map
}

// ----------------------------------------------------------------------------
// 网络（原生 WinHTTP，零额外下载依赖）
// ----------------------------------------------------------------------------

/// 所有请求统一附加的头。api.github.com 缺失 User-Agent 直接 403；
/// 用通用浏览器 UA 对各托管平台与 CDN 兼容性最好。必须以 \r\n 结尾。
const HTTP_HEADERS: &str = "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36\r\n";

// windows-rs 的安全绑定把头字符串长度按「字符数」传下去，而 WinHTTP 的
// WinHttpAddRequestHeaders 实际按「字节数」解析（字符数会把头截断，直接报 87 参数错误）。
// 实测长度传 -1（由系统按 null 结尾自行计算）可用，因此声明原始符号绕过绑定。
#[link(name = "winhttp")]
unsafe extern "system" {
    #[link_name = "WinHttpAddRequestHeaders"]
    fn raw_add_request_headers(
        request: *mut core::ffi::c_void,
        headers: *const u16,
        headers_len: u32,
        modifiers: u32,
    ) -> i32;
}

/// 可中止句柄组：登记一次请求的 session/connect/request 句柄，
/// 取消方关闭句柄使另一线程中阻塞的 WinHTTP 调用立即失败（标准中止手法）。
/// 句柄以 usize 存放以保证 Send。
#[derive(Default)]
pub struct HttpAbort {
    handles: std::sync::Mutex<Vec<usize>>,
}

impl HttpAbort {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, handle: *mut core::ffi::c_void) {
        self.handles.lock().unwrap().push(handle as usize);
    }

    /// 关闭当前登记的全部句柄；谁先取走谁负责关闭，另一方拿到空表（不会重复关闭）。
    pub fn close_all(&self) {
        for h in std::mem::take(&mut *self.handles.lock().unwrap()) {
            unsafe {
                let _ = WinHttpCloseHandle(h as *mut core::ffi::c_void);
            }
        }
    }
}

/// 解析 http/https URL → (host, port, path, secure)。
fn parse_url(url: &str) -> Result<(String, u16, String, bool), String> {
    let url = url.trim();
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(t!("仅支持 http/https 链接", "Only http/https URLs are supported").to_string());
    };
    let (hostpart, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostpart.rfind(':') {
        Some(i) => (
            hostpart[..i].to_string(),
            hostpart[i + 1..]
                .parse::<u16>()
                .map_err(|_| t!("URL 端口非法", "Invalid URL port").to_string())?,
        ),
        None => (hostpart.to_string(), if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(t!("URL 主机为空", "URL host is empty").to_string());
    }
    Ok((host, port, path.to_string(), secure))
}

fn query_number(request: *mut core::ffi::c_void, flag: u32) -> Option<u32> {
    let mut buf: u32 = 0;
    let mut len: u32 = 4;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            flag,
            PCWSTR::null(),
            Some((&mut buf as *mut u32) as *mut core::ffi::c_void),
            &mut len,
            core::ptr::null_mut(),
        )
    }
    .is_ok();
    if ok {
        Some(buf)
    } else {
        None
    }
}

/// 打开会话/连接/请求，发送并接收响应，校验 2xx 后把请求句柄交给 `f`。
/// `f` 负责读取响应体（GET 已自动跟随重定向）。
/// `connect_timeout` 单独控制建连（DNS/TLS）超时：检查路径收紧以快速淘汰不可达源；
/// 下载路径等满 `timeout`。
/// `abort` 提供时登记全部句柄，取消方关闭句柄可使阻塞中的调用立即失败。
fn with_http<F>(
    url: &str,
    timeout: Duration,
    connect_timeout: Duration,
    abort: Option<&HttpAbort>,
    f: F,
) -> Result<(), String>
where
    F: FnOnce(*mut core::ffi::c_void, Option<u64>) -> Result<(), String>,
{
    let (host, port, path, secure) = parse_url(url)?;
    // 用域名直连（SNI = 真实域名，证书校验才正确）；
    // IPv6 黑洞由 WINHTTP_OPTION_IPV6_FAST_FALLBACK 处理：连不上 IPv6 时秒回退 IPv4。
    let connect_host = host.clone();
    unsafe {
        // 强制直连：不使用任何系统/显式代理
        let session = WinHttpOpen(
            PCWSTR::null(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        // IPv6 快速回退（Happy Eyeballs）：某些更新源（如 gitea.tendokyu.moe）的 IPv6
        // 在特定网络是黑洞，会卡满建连；开启后连不上 IPv6 立即回退 IPv4，且 SNI 仍用真实域名。
        let ipv6_fb: u32 = 1;
        let ipv6_fb_bytes = ipv6_fb.to_ne_bytes();
        let _ = WinHttpSetOption(
            Some(session),
            WINHTTP_OPTION_IPV6_FAST_FALLBACK,
            Some(&ipv6_fb_bytes),
        );
        if session.is_null() {
            return Err(t!("无法初始化网络", "Failed to initialize network").to_string());
        }
        if let Some(a) = abort {
            a.register(session);
        }
        let ct = timeout.as_millis().min(u32::MAX as u128) as u32;
        let ct_bytes = ct.to_ne_bytes();
        let connect_ms = connect_timeout.as_millis().min(u32::MAX as u128) as u32;
        let connect_bytes = connect_ms.to_ne_bytes();
        let _ = WinHttpSetOption(
            Some(session),
            WINHTTP_OPTION_CONNECT_TIMEOUT,
            Some(&connect_bytes),
        );
        let _ = WinHttpSetOption(
            Some(session),
            WINHTTP_OPTION_RECEIVE_TIMEOUT,
            Some(&ct_bytes),
        );
        let host_w = crate::win_util::to_wide_null(&connect_host);
        let connect = WinHttpConnect(session, PCWSTR::from_raw(host_w.as_ptr()), port, 0);
        if connect.is_null() {
            if let Some(a) = abort {
                a.close_all();
            } else {
                let _ = WinHttpCloseHandle(session);
            }
            return Err(tf!("无法连接 {host}", "Failed to connect to {host}"));
        }
        if let Some(a) = abort {
            a.register(connect);
        }
        let path_w = crate::win_util::to_wide_null(&path);
        let verb_w = crate::win_util::to_wide_null("GET");
        let flags = if secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
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
            if let Some(a) = abort {
                a.close_all();
            } else {
                let _ = WinHttpCloseHandle(connect);
                let _ = WinHttpCloseHandle(session);
            }
            return Err(t!("无法创建请求", "Failed to create request").to_string());
        }
        if let Some(a) = abort {
            a.register(request);
        }
        // 自动跟随重定向（含 https→https）
        let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
        let policy_bytes = policy.to_ne_bytes();
        let _ = WinHttpSetOption(
            Some(request),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&policy_bytes),
        );
        // 直连 IPv4 时证书 CN 是域名而非 IP，跳过 CN 名校验（证书链/CA 仍校验）
        let sec: u32 = SECURITY_FLAG_IGNORE_CERT_CN_INVALID;
        let sec_bytes = sec.to_ne_bytes();
        let _ = WinHttpSetOption(Some(request), WINHTTP_OPTION_SECURITY_FLAGS, Some(&sec_bytes));
        let result = (|| -> Result<(), String> {
            let headers = crate::win_util::to_wide_null(HTTP_HEADERS);
            let ok = raw_add_request_headers(request, headers.as_ptr(), (-1i32) as u32, WINHTTP_ADDREQ_FLAG_ADD);
            // connect 用的是 IPv4，需把 Host 改写为真实域名（否则 github/gitcode 等按 Host 路由会 404）
            let host_hdr = crate::win_util::to_wide_null(&format!("Host: {host}"));
            let _ = raw_add_request_headers(
                request,
                host_hdr.as_ptr(),
                (-1i32) as u32,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                return Err(tf!("添加请求头失败：{e}", "Failed to add request headers: {e}").to_string());
            }
            WinHttpSendRequest(request, None, None, 0, 0, 0).map_err(|e| {
                tf!("发送请求失败：{e}", "Failed to send request: {e}").to_string()
            })?;
            WinHttpReceiveResponse(request, core::ptr::null_mut()).map_err(|e| {
                tf!("接收响应失败：{e}", "Failed to receive response: {e}").to_string()
            })?;
            let status = query_number(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER)
                .ok_or_else(|| {
                    t!("无法读取状态码", "Failed to read status code").to_string()
                })?;
            if status < 200 || status >= 300 {
                return Err(tf!(
                    "HTTP 状态 {status}（{url}）",
                    "HTTP status {status} ({url})"
                ));
            }
            let total = query_number(
                request,
                WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            )
            .map(|v| v as u64);
            f(request, total)
        })();
        // 清理：有 abort 时统一走注册表（取消方可能已提前关闭并取走句柄）
        if let Some(a) = abort {
            a.close_all();
        } else {
            let _ = WinHttpCloseHandle(request);
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
        }
        result
    }
}

/// 读取响应体到 `out`。
fn read_body(request: *mut core::ffi::c_void, out: &mut Vec<u8>) -> Result<(), String> {
    loop {
        let mut avail: u32 = 0;
        if unsafe { WinHttpQueryDataAvailable(request, &mut avail) }.is_err() {
            break;
        }
        if avail == 0 {
            break;
        }
        let mut buf = vec![0u8; avail as usize];
        let mut read: u32 = 0;
        if unsafe {
            WinHttpReadData(
                request,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                avail,
                &mut read,
            )
        }
        .is_err()
        {
            break;
        }
        if read == 0 {
            break;
        }
        buf.truncate(read as usize);
        out.extend_from_slice(&buf);
    }
    Ok(())
}

/// 读取响应体到文件并上报下载进度；`cancel` 置位即中止。
fn read_body_to_file(
    request: *mut core::ffi::c_void,
    file: &mut std::fs::File,
    total: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut written: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(t!("已取消更新", "Update cancelled").to_string());
        }
        let mut avail: u32 = 0;
        if unsafe { WinHttpQueryDataAvailable(request, &mut avail) }.is_err() {
            break;
        }
        if avail == 0 {
            break;
        }
        let mut buf = vec![0u8; avail as usize];
        let mut read: u32 = 0;
        if unsafe {
            WinHttpReadData(
                request,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                avail,
                &mut read,
            )
        }
        .is_err()
        {
            break;
        }
        if read == 0 {
            break;
        }
        buf.truncate(read as usize);
        file.write_all(&buf)
            .map_err(|e| tf!("写入失败：{e}", "Write failed: {e}"))?;
        written += read as u64;
        on_progress(written, total.unwrap_or(0));
    }
    // 读循环因句柄被关闭/读错误退出时，归因为取消
    if cancel.load(Ordering::Relaxed) {
        return Err(t!("已取消更新", "Update cancelled").to_string());
    }
    Ok(())
}

/// 把仓库地址转换为 Release 查询地址。
/// 支持 github.com / gitcode.com / gitea.tendokyu.moe（含 www），容忍尾部 `/`、`.git`、
/// `/releases`、`/releases/latest`；`/releases/tag/<tag>` 锁定指定版本（改调 tags 端点）。
pub fn repo_api_url(src: &str) -> Result<String, String> {
    let err = || {
        t!(
            "仅支持 GitHub/GitCode 仓库地址",
            "Only GitHub/GitCode repo URLs are supported"
        )
        .to_string()
    };
    let url = src.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(err)?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_matches('/')),
        None => (rest, ""),
    };
    let host = host.trim_start_matches("www.");
    // (API 基址, releases 子路径)：三个平台都支持 /releases/latest 单对象端点
    // （GitCode 实测同样可用，返回 release_status="latest" 的那一条）；
    // parse_releases 仍兼容数组形态，供 Gitea 等只提供数组端点的平台使用。
    let (api_base, suffix) = match host {
        "github.com" => ("https://api.github.com/repos", "/releases/latest"),
        "gitcode.com" => ("https://api.gitcode.com/api/v5/repos", "/releases/latest"),
        // Gitea 实例（API 与 Gitee v5 同构，且支持 /releases/latest）
        "gitea.tendokyu.moe" => ("https://gitea.tendokyu.moe/api/v1/repos", "/releases/latest"),
        _ => return Err(err()),
    };
    if path.is_empty() {
        return Err(err());
    }
    // /owner/repo/releases/tag/<tag> → 锁定 tag
    if let Some(idx) = path.find("/releases/tag/") {
        let tag = path[idx + "/releases/tag/".len()..].trim_matches('/');
        let owner_repo = &path[..idx];
        if tag.is_empty() || !is_owner_repo(owner_repo) {
            return Err(err());
        }
        return Ok(format!("{api_base}/{owner_repo}/releases/tags/{tag}"));
    }
    // 剥可选后缀（各只剥一次）
    let mut p = path;
    if let Some(s) = p.strip_suffix("/releases/latest") {
        p = s;
    } else if let Some(s) = p.strip_suffix("/releases") {
        p = s;
    }
    if let Some(s) = p.strip_suffix(".git") {
        p = s;
    }
    let p = p.trim_matches('/');
    if !is_owner_repo(p) {
        return Err(err());
    }
    Ok(format!("{api_base}/{p}{suffix}"))
}

/// 校验路径形如 /{owner}/{repo} 且两段均非空。
fn is_owner_repo(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segs.len() == 2
}

/// 从资产文件名提取内嵌版本：kitten1.4.2.zip / kitten-1.4.2.zip / kitten_v1.4.2.zip → "1.4.2"；
/// 固定名（kitten.zip）→ None，由调用方回退到 tag。
fn version_from_asset_name(asset_name: &str, loader: &str) -> Option<String> {
    let base = asset_name.strip_suffix(".zip")?;
    let rest = base.strip_prefix(loader)?;
    let rest = rest.trim_start_matches(['-', '_', ' ', 'v']);
    let mut first: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    while first.ends_with('.') {
        first.pop();
    }
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

/// 把 Release 响应转换为加载器更新条目；没有任何可识别的 kitten/mice 资产时报错
/// （而不是静默返回空，避免界面误显示「已是最新」）。
/// 解析 Release 中匹配指定组件的资产（取第一个命中）。
fn release_to_loader_for(
    rel: &ReleaseInfo,
    target: UpdateTarget,
) -> Result<LoaderRelease, String> {
    let tag_version = rel.tag_name.trim_start_matches('v').to_string();
    for asset in &rel.assets {
        let name_lc = asset.name.to_lowercase();
        if target.asset_matches(&name_lc) {
            let loader = target.dir_name();
            return Ok(LoaderRelease {
                name: loader.to_string(),
                version: version_from_asset_name(&name_lc, loader)
                    .unwrap_or_else(|| tag_version.clone()),
                url: asset.browser_download_url.clone(),
                sha256: asset
                    .digest
                    .as_ref()
                    .and_then(|d| d.strip_prefix("sha256:").map(|h| h.to_string())),
            });
        }
    }
    Err(t!(
        "Release 中没有可用资产",
        "No usable asset found in the release"
    )
    .to_string())
}

/// 解析 Release 响应文本，返回按「新 → 旧」排序的候选列表。
///
/// 兼容两种响应形态：
/// - 单对象（GitHub / GitCode / Gitea 的 `/releases/latest`）→ 单元素列表；
/// - 数组（平台的 `/releases` 列表端点）→ 排序后返回。
///
/// 数组形态**不能**直接取首个：GitCode 的 `/releases` 按发布时间**升序**返回，
/// 首个是最旧的一条（曾导致 kittentools 永远停在 1.0.0.0）。排序判据优先级：
/// `release_status == "latest"` → `created_at` 最大 → 保持原顺序末位优先（升序时末位即最新）。
fn parse_releases(text: &str) -> Result<Vec<ReleaseInfo>, String> {
    if let Ok(rel) = serde_json::from_str::<ReleaseInfo>(text) {
        return Ok(vec![rel]);
    }
    let mut rels: Vec<ReleaseInfo> = serde_json::from_str(text).map_err(|e| {
        tf!("Release 解析失败：{e}", "Failed to parse release: {e}")
    })?;
    if rels.is_empty() {
        return Err(t!("仓库没有可用的 Release", "The repo has no usable release").to_string());
    }
    sort_releases_newest_first(&mut rels);
    Ok(rels)
}

/// 就地把 Release 列表排成「新 → 旧」（见 [`parse_releases`]）。
fn sort_releases_newest_first(rels: &mut Vec<ReleaseInfo>) {
    let is_latest = |r: &ReleaseInfo| r.release_status.as_deref() == Some("latest");
    // 判据 1：平台显式标记的最新条目（GitCode 的 release_status = "latest"）
    if rels.iter().any(|r| is_latest(r)) {
        rels.sort_by(|a, b| is_latest(b).cmp(&is_latest(a)));
        return;
    }
    // 判据 2：创建时间倒序（ISO8601，同一平台时区偏移一致时字典序即时间序）
    if rels.iter().any(|r| r.created_at.is_some()) {
        rels.sort_by(|a, b| {
            b.created_at
                .as_deref()
                .unwrap_or("")
                .cmp(a.created_at.as_deref().unwrap_or(""))
        });
        return;
    }
    // 判据 3：无任何时间信息时，升序返回的末位即最新
    rels.reverse();
}

/// 拉取仓库 Release，返回按「新 → 旧」排序的候选列表（`parse_releases`）。
fn fetch_latest_release(
    api_url: &str,
    abort: Option<&HttpAbort>,
) -> Result<Vec<ReleaseInfo>, String> {
    let text = http_get_text(api_url, abort)?;
    parse_releases(&text)
}

/// 从候选 Release（新 → 旧）中解析出指定组件的更新条目：
/// 取**第一个含匹配资产**的 Release，最新一条没有对应 zip 时回退到更早的一条。
fn loader_from_candidates(
    rels: &[ReleaseInfo],
    target: UpdateTarget,
) -> Result<LoaderRelease, String> {
    let mut last_err = t!(
        "仓库没有可用的 Release",
        "The repo has no usable release"
    )
    .to_string();
    for rel in rels {
        match release_to_loader_for(rel, target) {
            Ok(l) => return Ok(l),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// 按界面语言为组件选定**唯一**更新源：
/// 中文优先 GitCode（国内直连快），英文优先 GitHub；都无命中时用列表第一个。
/// 选定的源返回非 200 / 网络失败时统一按「暂无更新」处理，**不再轮询其他镜像源**。
fn select_repo(target: UpdateTarget, lang: Lang) -> &'static str {
    let repos = target.default_repos();
    let pick = |host: &str| repos.iter().find(|r| r.contains(host)).copied();
    match lang {
        Lang::Zh => pick("gitcode.com").or_else(|| repos.first().copied()),
        Lang::En => pick("github.com").or_else(|| repos.first().copied()),
    }
    .unwrap_or("https://github.com")
}

/// 更新检查的全局硬期限：超期未返回的组件按「暂无更新」收尾，不再等待。
const CHECK_DEADLINE: Duration = Duration::from_secs(15);

/// 并行拉取 `targets` 各组件（语言选定的）更新源的最新 Release，结果**就绪即回调**：
/// 成功 → `on_result(target, Some(rel))`；源返回非 200 / 查询失败 → `on_result(target, None)`
/// （`check_one` 统一按「暂无更新」处理），不轮询其他镜像源。
/// 整体受 [`CHECK_DEADLINE`] 硬期限约束，超期未返回的组件以 None 回调收尾。
/// 全量检查传入 [`ALL_TARGETS`]；缓存命中后补查缺失组件时只传缺失子集。
pub fn fetch_updates_for(
    targets: &[UpdateTarget],
    on_result: &mut dyn FnMut(UpdateTarget, Option<LoaderRelease>),
) {
    use std::sync::mpsc;
    use std::time::Instant;

    let lang = i18n::current();
    // 查询作业：每个组件一个（语言选定的）仓库。
    let jobs: Vec<(UpdateTarget, String)> = targets
        .iter()
        .map(|&t| (t, select_repo(t, lang).to_string()))
        .collect();

    let (tx, rx) = mpsc::channel::<(UpdateTarget, Option<LoaderRelease>)>();
    // 每个作业独立的 abort：期限到达时统一关闭句柄，掐断在途请求。
    // 不共享单个 abort：with_http 收尾会 close_all 取走全部句柄，会误杀其他在途请求。
    let aborts: Vec<std::sync::Arc<HttpAbort>> = (0..jobs.len())
        .map(|_| std::sync::Arc::new(HttpAbort::new()))
        .collect();
    for ((target, repo), abort) in jobs.iter().zip(&aborts) {
        let tx = tx.clone();
        let (target, repo) = (*target, (*repo).clone());
        let abort = abort.clone();
        std::thread::spawn(move || {
            let r = repo_api_url(&repo)
                .and_then(|api| fetch_latest_release(&api, Some(&abort)))
                .and_then(|rels| loader_from_candidates(&rels, target))
                .ok();
            let _ = tx.send((target, r));
        });
    }
    drop(tx);

    let deadline = Instant::now() + CHECK_DEADLINE;
    for _ in 0..jobs.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok((target, rel)) => on_result(target, rel),
            Err(_) => break,
        }
    }
    // 期限到达 / 全部就绪：掐断剩余在途请求
    for a in &aborts {
        a.close_all();
    }
}

fn http_get_text(
    url: &str,
    abort: Option<&HttpAbort>,
) -> Result<String, String> {
    let mut body = Vec::new();
    // 读 15s / 建连 10s：API 响应体很小，建连超时收紧可快速淘汰不可达源；
    // 但给慢源（gitea.tendokyu.moe 等）留足建连时间，避免误杀
    with_http(
        url,
        Duration::from_secs(15),
        Duration::from_secs(10),
        abort,
        |request, _total| read_body(request, &mut body),
    )?;
    String::from_utf8(body)
        .map_err(|_| t!("响应不是合法 UTF-8", "Response is not valid UTF-8").to_string())
}

/// 下载 URL（失败自动切换 mirrors）到本地文件，过程上报进度；`cancel` 置位
/// 或 `abort` 关闭句柄即中止。
fn http_download_to_file(
    primary_url: &str,
    mirrors: &[String],
    out: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    abort: &HttpAbort,
    on_progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let tmp = out.with_extension("part");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| tf!("创建临时文件失败：{e}", "Failed to create temp file: {e}"))?;
    let urls: Vec<String> = std::iter::once(primary_url.to_string())
        .chain(mirrors.iter().cloned())
        .collect();
    let mut last_err = String::new();
    let mut ok = false;
    for url in &urls {
        if cancel.load(Ordering::Relaxed) {
            last_err = t!("已取消更新", "Update cancelled").to_string();
            break;
        }
        // 下载路径：读 / 建连都等满 30s（慢源必须给足时间）
        match with_http(
            url,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Some(abort),
            |request, total| read_body_to_file(request, &mut file, total, cancel, on_progress),
        ) {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last_err = e,
        }
    }
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Err(if cancel.load(Ordering::Relaxed) {
            t!("已取消更新", "Update cancelled").to_string()
        } else {
            tf!("下载失败：{last_err}", "Download failed: {last_err}")
        });
    }
    std::fs::rename(&tmp, out)
        .map_err(|e| tf!("重命名失败：{e}", "Rename failed: {e}"))
}

fn sha256_of_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

// ----------------------------------------------------------------------------
// 解压 + 整目录替换 + 回滚
// ----------------------------------------------------------------------------

/// 若 zip 内仅含单一顶层目录，返回该目录（剥一层），否则返回解压根。
fn strip_top_dir(dir: &Path) -> PathBuf {
    if let Ok(mut entries) = std::fs::read_dir(dir) {
        let first = entries.next().and_then(|e| e.ok());
        if let Some(f) = first {
            if entries.next().is_none() && f.path().is_dir() {
                return f.path();
            }
        }
    }
    dir.to_path_buf()
}

/// 递归移动目录（同卷 rename，跨卷回退到拷贝）。
fn move_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_dir(src, dst)?;
    std::fs::remove_dir_all(src)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 解压 zip 到 out_dir，并上报解压进度；`cancel` 置位即中止。
fn extract_zip(
    zip_path: &Path,
    out_dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| tf!("打开压缩包失败：{e}", "Failed to open archive: {e}"))?;
    let total = archive.len();
    let mut done = 0u64;
    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            return Err(t!("已取消更新", "Update cancelled").to_string());
        }
        let mut entry = archive
            .by_index(i)
            .map_err(|e| tf!("读取条目失败：{e}", "Failed to read entry: {e}"))?;
        let name = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let out_path = out_dir.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut outf = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut outf).map_err(|e| e.to_string())?;
        }
        done += 1;
        progress(done, total as u64);
    }
    Ok(())
}

/// 把已下载的 zip 安装到 `base_dir/<target.dir_name()/>`：备份旧目录、整体替换、失败回滚。
/// 解压与替换前检查 `cancel`；替换一旦开始（重命名级，耗时极短）不再中断。
/// 注意：本函数开头会清空重建 `base_dir/.__update_{loader}` 临时目录，
/// 因此 `zip_path` 必须位于该目录之外（download_and_install 使用同级 `.__update_{loader}.zip`）。
pub fn install_zip(
    zip_path: &Path,
    base_dir: &Path,
    target: UpdateTarget,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(UpdatePhase, u64, u64),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    progress(UpdatePhase::Extracting, 0, 0);
    let tmp_extract = base_dir.join(format!(".__update_{}", target.dir_name()));
    let _ = std::fs::remove_dir_all(&tmp_extract);
    std::fs::create_dir_all(&tmp_extract)
        .map_err(|e| tf!("创建临时目录失败：{e}", "Failed to create temp dir: {e}"))?;
    extract_zip(zip_path, &tmp_extract, cancel, &|done, total| {
        progress(UpdatePhase::Extracting, done, total)
    })
    .map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp_extract);
        tf!("解压失败：{e}", "Extraction failed: {e}")
    })?;
    let _ = std::fs::remove_file(zip_path);

    // 替换前的最后取消点
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(&tmp_extract);
        return Err(t!("已取消更新", "Update cancelled").to_string());
    }

    let content_root = strip_top_dir(&tmp_extract);
    let target_dir = base_dir.join(target.dir_name());
    let backup = base_dir.join(format!(".{}.old", target.dir_name()));
    let _ = std::fs::remove_dir_all(&backup);
    if target_dir.exists() {
        std::fs::rename(&target_dir, &backup).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_extract);
            tf!("备份失败：{e}", "Backup failed: {e}")
        })?;
    }

    progress(UpdatePhase::Installing, 0, 0);
    move_dir(&content_root, &target_dir).map_err(|e| {
        // 回滚
        let _ = std::fs::remove_dir_all(&target_dir);
        if backup.exists() {
            let _ = std::fs::rename(&backup, &target_dir);
        }
        let _ = std::fs::remove_dir_all(&tmp_extract);
        tf!("安装失败：{e}", "Installation failed: {e}")
    })?;

    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&tmp_extract);
    progress(UpdatePhase::Done, 1, 1);
    Ok(())
}

/// 完整流程：下载（含可选 sha256 校验）→ 整目录安装。
/// `cancel` 置位或 `abort.close_all()`（关闭阻塞中的网络句柄）即中止。
pub fn download_and_install(
    target: UpdateTarget,
    primary_url: &str,
    mirrors: &[String],
    sha256: Option<&str>,
    base_dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    abort: &HttpAbort,
    progress: &dyn Fn(UpdatePhase, u64, u64),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    progress(UpdatePhase::Downloading, 0, 0);
    // 解压临时目录会在 install_zip 开头被整体清空重建，
    // 因此 zip 必须放在该目录之外（同级文件），否则解压前就被删掉
    let tmp_extract = base_dir.join(format!(".__update_{}", target.dir_name()));
    let zip_path = base_dir.join(format!(".__update_{}.zip", target.dir_name()));
    let _ = std::fs::remove_dir_all(&tmp_extract);
    std::fs::create_dir_all(&tmp_extract)
        .map_err(|e| tf!("创建临时目录失败：{e}", "Failed to create temp dir: {e}"))?;

    let r = http_download_to_file(
        primary_url,
        mirrors,
        &zip_path,
        cancel,
        abort,
        &|done, total| progress(UpdatePhase::Downloading, done, total),
    );
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&tmp_extract);
        let _ = std::fs::remove_file(&zip_path);
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(&tmp_extract);
        let _ = std::fs::remove_file(&zip_path);
        return Err(t!("已取消更新", "Update cancelled").to_string());
    }

    if let Some(expected) = sha256 {
        let actual = sha256_of_file(&zip_path)
            .map_err(|e| tf!("校验计算失败：{e}", "Checksum computation failed: {e}"))?;
        if actual.to_lowercase() != expected.to_lowercase() {
            let _ = std::fs::remove_dir_all(&tmp_extract);
            let _ = std::fs::remove_file(&zip_path);
            return Err(t!("sha256 校验失败", "SHA256 verification failed").to_string());
        }
    }

    // install_zip 内部解压后自行删除 zip 并清理临时目录；失败时此处兜底删 zip
    let r = install_zip(&zip_path, base_dir, target, cancel, progress);
    if r.is_err() {
        let _ = std::fs::remove_file(&zip_path);
    }
    r
}

/// 工具：返回加载器目录基址 `launcher/tools`。
pub fn tools_dir() -> PathBuf {
    launcher_dir().join("tools")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert_eq!(compare_versions("1.9", "1.10"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("1.10", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("1.4", "1.4.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("v1.4.2", "1.4"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("2.0.1", "2.0.0"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn check_updates_matches_loaders() {
        let index = UpdateIndex {
            loaders: vec![LoaderRelease {
                name: "kitten".into(),
                version: "1.5.0".into(),
                url: "https://x/k.zip".into(),
                sha256: None,
            }],
        };
        // 未安装过任何版本 → 有更新（提示可升级）
        let mut settings = Settings::default();
        let r = check_updates(&index, &settings);
        assert_eq!(
            r[&UpdateTarget::Kitten].state,
            UpdateState::Outdated
        );
        // mice 不在索引中（其更新源返回非 200 / 失败）→ 统一显示「暂无更新」
        assert_eq!(r[&UpdateTarget::Mice].state, UpdateState::UpToDate);
        assert_eq!(r[&UpdateTarget::Mice].detail.as_deref(), Some("暂无更新"));

        // 已安装 kitten 1.5.0 → 已最新
        settings.last_kitten_version = Some("1.5.0".into());
        let r2 = check_updates(&index, &settings);
        assert_eq!(r2[&UpdateTarget::Kitten].state, UpdateState::UpToDate);

        // 已安装 kitten 1.4.0（远端更新）→ 有更新
        settings.last_kitten_version = Some("1.4.0".into());
        let r3 = check_updates(&index, &settings);
        assert_eq!(r3[&UpdateTarget::Kitten].state, UpdateState::Outdated);
    }

    #[test]
    fn select_repo_by_language() {
        // 中文 → GitCode；英文 → GitHub；只有单源的组件不受语言影响
        let zh = i18n::Lang::Zh;
        let en = i18n::Lang::En;
        assert_eq!(
            select_repo(UpdateTarget::Kitten, zh),
            "https://gitcode.com/project-sasayaki/kittentools"
        );
        assert_eq!(
            select_repo(UpdateTarget::Kitten, en),
            "https://github.com/project-sasayaki/kittentools"
        );
        // shaderfix 仅有 tendokyu 单源，语言不影响
        assert_eq!(
            select_repo(UpdateTarget::ShaderFix, zh),
            "https://gitea.tendokyu.moe/puniru/shaderfix-idas"
        );
        assert_eq!(
            select_repo(UpdateTarget::ShaderFix, en),
            "https://gitea.tendokyu.moe/puniru/shaderfix-idas"
        );
    }

    #[test]
    fn repo_api_url_variants() {
        let err_of = |s: &str| repo_api_url(s).unwrap_err();
        // 常规形态
        assert_eq!(
            repo_api_url("https://github.com/a/b").unwrap(),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        assert_eq!(
            repo_api_url("https://github.com/a/b/").unwrap(),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        assert_eq!(
            repo_api_url("https://www.github.com/a/b.git").unwrap(),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        assert_eq!(
            repo_api_url("https://github.com/a/b/releases").unwrap(),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        assert_eq!(
            repo_api_url("https://github.com/a/b/releases/latest").unwrap(),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        assert_eq!(
            repo_api_url("https://gitcode.com/a/b").unwrap(),
            "https://api.gitcode.com/api/v5/repos/a/b/releases/latest"
        );
        // 锁定 tag
        assert_eq!(
            repo_api_url("https://github.com/a/b/releases/tag/v1.4.2").unwrap(),
            "https://api.github.com/repos/a/b/releases/tags/v1.4.2"
        );
        // 非法输入
        assert!(err_of("https://gitlab.com/a/b").contains("GitHub"));
        assert!(err_of("github.com/a/b").contains("GitHub"));
        assert!(err_of("https://github.com/a").contains("GitHub"));
        assert!(err_of("https://github.com/").contains("GitHub"));
        assert!(err_of("https://github.com/a/b/releases/tag/").contains("GitHub"));
    }

    /// GitCode `/releases/latest` 的单对象响应。
    const GITCODE_LATEST_JSON: &str = r#"{
        "tag_name": "1.0.1.0",
        "created_at": "2026-10-11T10:56:09+08:00",
        "release_status": "latest",
        "assets": [
            {"browser_download_url": "https://x/1.0.1.0.zip", "name": "1.0.1.0.zip", "type": "source"},
            {"browser_download_url": "https://gitcode.com/o/r/releases/download/1.0.1.0/kitten.zip", "name": "kitten.zip", "type": "attach"}
        ]
    }"#;

    /// GitCode `/releases` 的数组响应（**升序**：最旧在前）。
    const GITCODE_ARRAY_JSON: &str = r#"[
        {
            "tag_name": "1.0.0.0",
            "created_at": "2026-09-18T12:33:59+08:00",
            "release_status": "none",
            "assets": [
                {"browser_download_url": "https://gitcode.com/o/r/releases/download/1.0.0.0/kitten.zip", "name": "kitten.zip", "type": "attach"}
            ]
        },
        {
            "tag_name": "1.0.1.0",
            "created_at": "2026-10-11T10:56:09+08:00",
            "release_status": "latest",
            "assets": [
                {"browser_download_url": "https://gitcode.com/o/r/releases/download/1.0.1.0/kitten.zip", "name": "kitten.zip", "type": "attach"}
            ]
        }
    ]"#;

    #[test]
    fn parse_releases_single_object() {
        let rels = parse_releases(GITCODE_LATEST_JSON).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].tag_name, "1.0.1.0");
    }

    #[test]
    fn parse_releases_picks_newest_from_ascending_array() {
        // GitCode 的 /releases 按发布时间升序，取首个会拿到最旧的 1.0.0.0
        let rels = parse_releases(GITCODE_ARRAY_JSON).unwrap();
        assert_eq!(rels[0].tag_name, "1.0.1.0");
        assert_eq!(rels[1].tag_name, "1.0.0.0");
    }

    #[test]
    fn parse_releases_falls_back_to_created_at_and_tail() {
        // 无 release_status 标记时按 created_at 取最新
        let by_time = r#"[
            {"tag_name": "v1", "created_at": "2026-01-01T00:00:00+08:00", "assets": []},
            {"tag_name": "v3", "created_at": "2026-03-01T00:00:00+08:00", "assets": []},
            {"tag_name": "v2", "created_at": "2026-02-01T00:00:00+08:00", "assets": []}
        ]"#;
        let rels = parse_releases(by_time).unwrap();
        assert_eq!(rels[0].tag_name, "v3");

        // 完全没有时间信息时，升序数组的末位即最新
        let no_time = r#"[
            {"tag_name": "v1", "assets": []},
            {"tag_name": "v2", "assets": []}
        ]"#;
        let rels = parse_releases(no_time).unwrap();
        assert_eq!(rels[0].tag_name, "v2");

        // 空数组 / 非法结构 → Err
        assert!(parse_releases("[]").is_err());
        assert!(parse_releases("not json").is_err());
    }

    #[test]
    fn loader_from_candidates_prefers_newest_with_matching_asset() {
        let rels = parse_releases(GITCODE_ARRAY_JSON).unwrap();
        let l = loader_from_candidates(&rels, UpdateTarget::Kitten).unwrap();
        assert_eq!(l.version, "1.0.1.0");
        assert_eq!(
            l.url,
            "https://gitcode.com/o/r/releases/download/1.0.1.0/kitten.zip"
        );
    }

    #[test]
    fn loader_from_candidates_skips_release_without_asset() {
        // 最新一条没有 kitten.zip（只有源码归档），应回退到更早那条含资产的
        let json = r#"[
            {
                "tag_name": "1.0.0.0",
                "assets": [{"browser_download_url": "https://x/kitten.zip", "name": "kitten.zip"}]
            },
            {
                "tag_name": "1.0.1.0",
                "release_status": "latest",
                "assets": [{"browser_download_url": "https://x/1.0.1.0.zip", "name": "1.0.1.0.zip"}]
            }
        ]"#;
        let rels = parse_releases(json).unwrap();
        assert_eq!(rels[0].tag_name, "1.0.1.0");
        let l = loader_from_candidates(&rels, UpdateTarget::Kitten).unwrap();
        assert_eq!(l.version, "1.0.0.0");

        // 全部都没有匹配资产 → Err
        assert!(loader_from_candidates(&rels, UpdateTarget::Mice).is_err());
    }

    #[test]
    fn cache_schema_invalidates_files_written_by_older_builds() {
        // 旧版本写入的缓存没有 schema 字段 → 反序列化为 0，判为失效
        let old = format!(
            r#"{{"saved_at": {}, "index": {{"loaders": [{{"name": "kitten", "version": "1.0.0.0", "url": "https://x/k.zip", "sha256": null}}]}}}}"#,
            now_secs()
        );
        let c: CacheFile = serde_json::from_str(&old).unwrap();
        assert_ne!(c.schema, CACHE_SCHEMA);

        // 当前结构写入后 schema 保留
        let cur = CacheFile {
            schema: CACHE_SCHEMA,
            saved_at: now_secs(),
            index: UpdateIndex::default(),
        };
        let text = serde_json::to_string(&cur).unwrap();
        let back: CacheFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.schema, CACHE_SCHEMA);
    }

    #[test]
    fn install_zip_swaps_and_rolls_back() {
        let tmp = std::env::temp_dir().join("dnet_update_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let base = tmp.join("tools");
        std::fs::create_dir_all(&base).unwrap();
        let target_dir = base.join("kitten");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("old.txt"), b"old").unwrap();

        // 构建一个 kitten.zip（含单一顶层 kitten/ 目录）
        let zip_path = tmp.join("kitten.zip");
        {
            use std::io::Write;
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zw.start_file("kitten/new.txt", opts).unwrap();
            zw.write_all(b"new").unwrap();
            zw.finish().unwrap();
        }

        let progress = |_: UpdatePhase, _: u64, _: u64| {};
        let cancel = std::sync::atomic::AtomicBool::new(false);
        install_zip(&zip_path, &base, UpdateTarget::Kitten, &cancel, &progress).unwrap();

        assert!(target_dir.join("new.txt").exists());
        assert!(!target_dir.join("old.txt").exists());
        // 备份目录应已清理
        assert!(!base.join(".kitten.old").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
