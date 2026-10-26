//! mice（RingEdge）游戏启动前的自动部署与配置生成：
//!
//! 仅对「加载器 = mice」的游戏生效，时机为拉起游戏之前。职责有两块：
//! 1. `render_config` / `render_idas`：以启动器全局设置为唯一来源，把
//!    `<目录>\mice\config.ini` 与 `<目录>\mice\idas.ini` 渲染为纯 `键 = 值` 文本。
//!    规则（与产品约定一致）：
//!    - 不含注释、不含模板，未在启动器界面设置的键一律不写（交给 mice 自身默认值兜底）；
//!    - 某节没有任何写出键时整节省略；
//!    - 固定派生键：`[mice] apply_patches=false`、`[network] naominet_jp / aime_naominet_jp`
//!      （私服域名）、`[network] ib_naominet_jp=127.0.0.1`；
//!    - 网段（CIDR）→ `keychip_subnet`，并推导 `ip_address`（ServerBox=基址+101、
//!      游戏=基址+102）、`subnet_mask`、`gateway`/`primary_dns`（网段末地址）；
//!    - 主机（本机游戏）IP → idas.ini `[ServerBox] game_ip`（仅关闭模拟网络时写；
//!      房间模式下快照值已按房间覆盖替换：房主 .101 / 成员座位 IP）；
//!    - 「跳过读卡器初始化」→ idas.ini `[ServerBox] skipInitialization`，**同向**且
//!      **始终写出**（上条「未设置一律不写」的例外）：勾选 → `true`，
//!      未勾选 / 从未设置 → `false`；
//!    - 日志级别未设置时写 info（4，mice 默认）。
//! 2. `deploy`：把 mice 工具目录（mice.exe 所在目录，即 launcher\tools\mice\）顶层文件
//!    复制到目标目录（排除运行时产物与启动器生成的配置），并原子写 `<目录>\mice\` 下的
//!    两份配置。游戏结束后这些文件不清理，下次启动时同名覆盖保持最新。

use std::fs;
use std::path::Path;

use crate::tf;
use crate::storage::{
    CoinConfig, GraphicsConfig, InputConfig, INPUT_RATIO_DEFAULT, NetworkConfig, SystemConfig,
};

/// 部署 + 配置生成所需的全局设置快照（点击「启动」时一次性克隆，
/// owned 便于跨线程，避免后台运行期间 UI 修改造成不一致）。
#[derive(Clone, Debug)]
pub struct DeploySnapshot {
    pub net: NetworkConfig,
    pub system: SystemConfig,
    pub coin: CoinConfig,
    pub graphics: GraphicsConfig,
    pub input: InputConfig,
    /// 选中的车卡（config.ini `[card] cardfile`）；有值时写入 idas.ini 的
    /// `[CardReader] cardfile`（游戏侧与 ServerBox 侧共用）
    pub card: Option<String>,
    /// 房间模式覆盖（已加入房间时 Some；否则 None 走大网/本地网段原逻辑）
    pub room: Option<RoomNetOverride>,
}

/// 房间模式的网络覆盖：虚拟局域网联机时网段/本机 IP/私服地址全部指向房间。
#[derive(Clone, Debug)]
pub struct RoomNetOverride {
    /// 房间子网（网络地址形式，如 "172.10.1.0/24"）
    pub cidr: String,
    /// 本机 Game IP（成员 = 座位 IP；房主 None → 走偏移 +101）
    pub my_game_ip: Option<String>,
    /// 房主 ServerBox IP（.102）：naominet 指向它，ServerBox 的 bind_ip 也用它
    pub server_ip: String,
}

/// 部署目标：决定 `[network] ip_address` 的网段内偏移。
/// 店内对战编址：游戏=.101、ServerBox=.102（房主）；成员=目录分配的座位 IP。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiceTarget {
    /// 游戏目录：网段基址 + 101
    Game,
    /// ServerBox 目录：网段基址 + 102
    ServerBox,
}

impl MiceTarget {
    fn ip_offset(self) -> u32 {
        match self {
            MiceTarget::Game => 101,
            MiceTarget::ServerBox => 102,
        }
    }
}

// ---------------------------------------------------------------------------
// INI 渲染（与 kitten.rs 同款 Sections：保序节 + `key = value` 行，可单测）
// ---------------------------------------------------------------------------

/// 待输出的若干节（保序）；节内每行形如 `key = value`。
pub(crate) struct Sections(Vec<(String, Vec<String>)>);

impl Sections {
    pub(crate) fn new() -> Self {
        Sections(Vec::new())
    }

    /// 把 `key = value` 追加到指定节；节不存在则新建并排在末尾。
    pub(crate) fn push(&mut self, section: &str, key: &str, value: &str) {
        if let Some((_, lines)) = self.0.iter_mut().find(|(s, _)| s == section) {
            lines.push(format!("{key} = {value}"));
        } else {
            self.0
                .push((section.to_string(), vec![format!("{key} = {value}")]));
        }
    }
}

impl std::fmt::Display for Sections {
    /// 节与节之间空一行；结尾以换行收束。文件不含任何注释。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, (title, lines)) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            writeln!(f, "[{title}]")?;
            for line in lines {
                writeln!(f, "{line}")?;
            }
        }
        Ok(())
    }
}

pub(crate) fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// 日志级别（launcher 值）→ mice `[mice] log_level` 数值。
/// mice：1=Game 2=Error 3=Warning 4=Info 5=Misc 6=Trace；info（4）为默认。
fn log_level_num(v: &str) -> Option<u32> {
    match v.trim().to_ascii_lowercase().as_str() {
        "error" => Some(2),
        "warn" => Some(3),
        "info" => Some(4),
        "debug" => Some(5),
        "trace" => Some(6),
        _ => None,
    }
}

/// 地区（launcher 值）→ mice `[sysconf] region` 数值。
/// mice 二进制说明：Board region. 1 = Jpn, 2 = USA, 4 = Exp, 8 = Chn。
fn region_num(v: &str) -> Option<u32> {
    match v.trim().to_ascii_uppercase().as_str() {
        "JPN" => Some(1),
        "EXP" => Some(4),
        "CHN" => Some(8),
        _ => None,
    }
}

/// 日志级别快照值 → 数值；未设置 / 非法值一律按默认 info（4）。
fn log_level_value(v: Option<&str>) -> u32 {
    v.and_then(log_level_num).unwrap_or(4)
}

/// 解析 "1920x1080" 这类分辨率字符串；无效返回 None。
fn split_resolution(v: &str) -> Option<(u32, u32)> {
    let s = v.trim();
    let x = s.find(['x', 'X'])?;
    let w = s[..x].trim().parse().ok()?;
    let h = s[x + 1..].trim().parse().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// 死区百分比（0~100）→ mice 的整数值（`32767 × 百分比`，四舍五入）。
pub(crate) fn deadzone_value(pct: u32) -> u32 {
    (((32767u64 * pct as u64) + 50) / 100) as u32
}

// ---------------------------------------------------------------------------
// 网段推导（纯函数，可单测）
// ---------------------------------------------------------------------------

pub(crate) fn format_ipv4(v: u32) -> String {
    let b = v.to_be_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

pub(crate) fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (b, p) in bytes.iter_mut().zip(&parts) {
        *b = p.trim().parse().ok()?;
    }
    Some(u32::from_be_bytes(bytes))
}

/// 前缀长度 → 32 位子网掩码（主机序）。
pub(crate) fn mask_of(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix.min(32) as u32)
    }
}

/// 解析 CIDR `"192.168.100.0/24"` →（网段基址，前缀）；无 `/` 时按 /32 单机地址处理。
pub(crate) fn parse_cidr(s: &str) -> Option<(u32, u8)> {
    let s = s.trim();
    let (ip_part, prefix_part) = match s.split_once('/') {
        Some((ip, p)) => (ip, p.trim().parse::<u8>().ok()?),
        None => (s, 32),
    };
    if prefix_part > 32 {
        return None;
    }
    let ip = parse_ipv4(ip_part)?;
    Some((ip & mask_of(prefix_part), prefix_part))
}

/// 网段最后一个可用地址（broadcast − 1）。
/// /30 以上的极小网段（/31、/32）没有可用的「末地址」，返回 None。
pub(crate) fn last_host(base: u32, prefix: u8) -> Option<u32> {
    if prefix >= 31 {
        return None;
    }
    Some((base | !mask_of(prefix)).wrapping_sub(1))
}

// ---------------------------------------------------------------------------
// config.ini / idas.ini 渲染（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 按启动器全局设置快照渲染 mice config.ini 文本。
/// 只写映射表内的键；网段未设置时 `[network]` 推导键（ip/mask/gateway/dns）不写。
pub fn render_config(dep: &DeploySnapshot, target: MiceTarget) -> String {
    let mut s = Sections::new();

    // ---- [mice] ----
    s.push("mice", "apply_patches", "false");
    s.push("mice", "log_level", &log_level_value(dep.system.loglevel.as_deref()).to_string());

    // ---- [sysconf] ----
    if let Some(k) = dep.system.keychip.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        s.push("sysconf", "keychip_serial", k);
    }
    if let Some(r) = dep.system.region.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        if let Some(n) = region_num(r) {
            s.push("sysconf", "region", &n.to_string());
        }
    }
    if let Some(fp) = dep.coin.freeplay {
        s.push("sysconf", "freeplay", bool_str(fp));
    }

    // ---- [network]：网段派生 + 固定私服域名 ----
    // 房间模式覆盖：网段/本机 IP/私服地址全部指向房间（虚拟局域网联机）
    let room = dep.room.as_ref();
    let cidr = room.map(|r| r.cidr.as_str()).or(dep.net.ip.as_deref());
    if let Some(cidr) = cidr {
        if let Some((base, prefix)) = parse_cidr(cidr) {
            s.push("sysconf", "keychip_subnet", cidr.trim());
            // 成员 Game 用目录分配的座位 IP；房主（或 ServerBox 目录）走偏移
            let ip_override = match (room, target) {
                (Some(r), MiceTarget::Game) => r.my_game_ip.clone(),
                _ => None,
            };
            match ip_override {
                Some(ip) => s.push("network", "ip_address", &ip),
                None => s.push("network", "ip_address", &format_ipv4(base + target.ip_offset())),
            }
            s.push("network", "subnet_mask", &format_ipv4(mask_of(prefix)));
            if let Some(last) = last_host(base, prefix) {
                s.push("network", "gateway", &format_ipv4(last));
                s.push("network", "primary_dns", &format_ipv4(last));
            }
        }
    }
    s.push("network", "naominet_jp", dep.net.server_host());
    s.push("network", "aime_naominet_jp", dep.net.server_host());
    s.push("network", "ib_naominet_jp", "127.0.0.1");

    // ---- [window]：仅当不使用默认图形配置时考虑；无任何自定义键则整节省略 ----
    if dep.graphics.default != Some(true) {
        if let Some(b) = dep.graphics.fullscreen {
            s.push("window", "windowed", bool_str(!b));
        }
        if let Some(b) = dep.graphics.borderless {
            s.push("window", "borderless", bool_str(b));
        }
        if let Some((w, h)) = dep.graphics.resolution.as_deref().and_then(split_resolution) {
            // mice 默认 dipsw 解析会覆盖自定义分辨率，这里必须显式关掉
            s.push("window", "dipsw", "false");
            s.push("window", "w", &w.to_string());
            s.push("window", "h", &h.to_string());
        }
    }

    // ---- [ffb]：力反馈（与 kitten config.ini 同语义；未设置按默认值） ----
    s.push("ffb", "enable", bool_str(dep.input.ffb_enable.unwrap_or(true)));
    s.push("ffb", "constantForceStrength", &dep.input.ffb_constant_force.unwrap_or(100).to_string());
    s.push("ffb", "damperStrength", &dep.input.ffb_damper.unwrap_or(100).to_string());
    s.push("ffb", "rumbleStrength", &dep.input.ffb_rumble.unwrap_or(100).to_string());
    s.push("ffb", "rumbleDuration", &dep.input.ffb_rumble_duration.unwrap_or(100).to_string());
    s.push("ffb", "baseDamperStrength", &dep.input.ffb_base_damper.unwrap_or(20).to_string());
    s.push("ffb", "deadBand", &dep.input.ffb_dead_band.unwrap_or(20).to_string());

    // ---- [ServerBox]：ServerBox 静态绑定 IP（仅关闭模拟网络的 ServerBox 目标时） ----
    // ServerBox 进程从 config.ini 读取绑定 IP；游戏（Game）目标的 bind_ip 已写在 idas.ini。
    // 房间模式覆盖：绑定房主 ServerBox IP（.102）。
    let bind_ip = if let Some(r) = dep.room.as_ref() {
        Some(r.server_ip.clone())
    } else {
        trimmed(&dep.net.serverbox_ip).map(str::to_string)
    };
    if target == MiceTarget::ServerBox && dep.net.simulate == Some(false) {
        if let Some(ip) = bind_ip {
            s.push("ServerBox", "bind_ip", &ip);
        }
    }

    s.to_string()
}

/// 取非空 trimmed 字符串；空白视为未设置。
fn trimmed(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// 写入 XInput 按钮名映射：仅当名称已设置且非空时才写出对应键，否则交 mice 默认兜底。
fn push_xi_button(s: &mut Sections, key: &str, name: &Option<String>) {
    if let Some(v) = name.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        s.push("xinput", key, v);
    }
}

/// 渲染倍率（launcher "2x"/"4x"/"8x"）→ mice `[Graphics] render_scale` 数值
/// （mice：1=x1 2=x2 3=x4 4=x8）；无效返回 None。
fn render_scale_num(v: &str) -> Option<u32> {
    let digits: String = v.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    match digits.as_str() {
        "2" => Some(2),
        "4" => Some(3),
        "8" => Some(4),
        _ => None,
    }
}

/// 按启动器全局设置快照渲染 idas.ini 文本（头文字D 游戏侧配置）。
pub fn render_idas(dep: &DeploySnapshot) -> String {
    let mut s = Sections::new();

    // ---- [ServerBox]：模拟网络开关（未设置视为开，同 kitten）；
    //      offlineMode 固定写 false（真机定案：ServerBox 需显式在线，不随模拟网络开关省略/取反） ----
    let sim = dep.net.simulate.unwrap_or(true);
    s.push("ServerBox", "offlineMode", "false");
    s.push("ServerBox", "simulateNetwork", bool_str(sim));
    // 「跳过读卡器初始化」→ skipInitialization 同向：勾选 → true，未勾选 / 未设置 → false。
    // 该键始终写出（不交 mice 默认兜底），语义与开关一致，勿改回取反。
    s.push(
        "ServerBox",
        "skipInitialization",
        bool_str(dep.system.skip_reader_init == Some(true)),
    );

    // ---- [ServerBox] 静态 IP：仅关闭模拟网络时才写 ----
    //      模拟网络开启（None / Some(true)）时 IP 由 mice 自行模拟派生，
    //      写死 bind_ip / game_ip 会与之冲突，故一律不写。
    //      bind_ip / game_ip 与 simulateNetwork 同节（mice 按节名精确匹配读取，小写节会被忽略）
    if dep.net.simulate == Some(false) {
        // ServerBox 静态绑定 IP
        if let Some(ip) = dep.net.serverbox_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            s.push("ServerBox", "bind_ip", ip);
        }
        // 主机（本机游戏）IP：ServerBox / 读卡器侧据此找游戏
        // （房间模式已按覆盖替换快照值：房主 .101 / 成员座位 IP）
        if let Some(ip) = dep.net.host_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            s.push("ServerBox", "game_ip", ip);
        }
    }

    // ---- [Coins]：未填数值的键跳过不写 ----
    if let Some(n) = dep.coin.start_coin {
        s.push("Coins", "start", &n.to_string());
    }
    if let Some(n) = dep.coin.continue_coin {
        s.push("Coins", "continues", &n.to_string());
    }
    if let Some(n) = dep.coin.card_coin {
        s.push("Coins", "card", &n.to_string());
    }
    if let Some(n) = dep.coin.car_coin {
        s.push("Coins", "car", &n.to_string());
    }

    // ---- [Graphics]：渲染倍数 ----
    if dep.graphics.render_scale == Some(true) {
        if let Some(n) = dep.graphics.render_scale_level.as_deref().and_then(render_scale_num) {
            s.push("Graphics", "render_scale", &n.to_string());
        }
    }

    // ---- [io3]：输入模式（mice 为字符串 xinput/dinput）与输入信号限制（原「输入比率」）----
    let kind_lower = dep
        .input
        .kind
        .as_deref()
        .map(|v| v.trim().to_ascii_lowercase());
    match kind_lower.as_deref() {
        Some("xinput") => s.push("io3", "mode", "xinput"),
        Some("dinput") => s.push("io3", "mode", "dinput"),
        _ => {}
    }
    // restrict_ = 输入信号限制（原「输入比率」；未设置时写默认 128）
    s.push(
        "io3",
        "restrict_",
        &dep.input.ratio.unwrap_or(INPUT_RATIO_DEFAULT).to_string(),
    );

    // ---- [xinput]：仅 XInput 模式；手柄槽位 / 双摇杆 / 线性方向 / 左右摇杆死区 / 按钮名映射 ----
    if kind_lower.as_deref() == Some("xinput") {
        // 手柄槽位（deviceIndex）：复用 pad_index，始终写出（默认 0），避免未切换过
        // pad_index（默认 None）时游戏缺键行为不确定；mice 仅认 0~3 的整数。
        s.push("xinput", "deviceIndex", &dep.input.pad_index.unwrap_or(0).to_string());
        // 双摇杆模式开启 → 单摇杆转向关闭（singleStickSteering = false）；关闭/未设置 → true。
        // 始终写入，避免未切换过 dual_stick（默认 None）时游戏缺少该键而行为不确定。
        // mice 只认 true/false（布尔写法），不认 1/0。
        let dual = dep.input.dual_stick.unwrap_or(false);
        s.push("xinput", "singleStickSteering", bool_str(!dual));
        // 线性方向：xinput 模式下未设置时默认开启（与输入设置 UI 默认一致）
        let linear = dep.input.linear_steering.unwrap_or(true);
        s.push("xinput", "linearSteering", bool_str(linear));
        // 死区默认 20%（与启动器输入设置 UI 默认值一致）
        let ldz = dep.input.left_deadzone.unwrap_or(20);
        let rdz = dep.input.right_deadzone.unwrap_or(20);
        s.push("xinput", "leftStickDeadzone", &deadzone_value(ldz).to_string());
        s.push("xinput", "rightStickDeadzone", &deadzone_value(rdz).to_string());
        // 按钮名映射：仅在用户捕获过（已设置且非空）时写出，否则交 mice 默认（A/B/DPAD_* 等）兜底
        push_xi_button(&mut s, "start", &dep.input.xi_start);
        push_xi_button(&mut s, "viewChg", &dep.input.xi_view);
        push_xi_button(&mut s, "up", &dep.input.xi_up);
        push_xi_button(&mut s, "down", &dep.input.xi_down);
        push_xi_button(&mut s, "left", &dep.input.xi_left);
        push_xi_button(&mut s, "right", &dep.input.xi_right);
        push_xi_button(&mut s, "shiftDn", &dep.input.xi_shift_dn);
        push_xi_button(&mut s, "shiftUp", &dep.input.xi_shift_up);
        push_xi_button(&mut s, "coin", &dep.input.xi_coin);
        push_xi_button(&mut s, "card", &dep.input.xi_card);
        // 油门 / 刹车源（trigger 或按钮名）：仅在用户捕获过（已设置且非空）时写出，
        // 否则交 mice 默认（扳机模拟量）兜底
        push_xi_button(&mut s, "accel", &dep.input.xi_accel);
        push_xi_button(&mut s, "brake", &dep.input.xi_brake);
    }

    // ---- [dinput]：仅 DirectInput 模式；设备与按键映射 ----
    if kind_lower.as_deref() == Some("dinput") {
        if let Some(v) = trimmed(&dep.input.wheel_device) {
            s.push("dinput", "deviceName", v);
        }
        if let Some(v) = trimmed(&dep.input.pedal_device) {
            s.push("dinput", "pedalsName", v);
        }
        if let Some(v) = trimmed(&dep.input.shifter_device) {
            s.push("dinput", "shifterName", v);
        }
        if let Some(n) = dep.input.start_button {
            s.push("dinput", "start", &n.to_string());
        }
        if let Some(n) = dep.input.view_button {
            s.push("dinput", "viewChg", &n.to_string());
        }
        if let Some(n) = dep.input.shift_up {
            s.push("dinput", "shiftUp", &n.to_string());
        }
        if let Some(n) = dep.input.shift_down {
            s.push("dinput", "shiftDn", &n.to_string());
        }
        // 投币 / 插卡按键（与 xinput 同名的 mice [dinput] 键；未设置不写）
        if let Some(n) = dep.input.coin_button {
            s.push("dinput", "coin", &n.to_string());
        }
        if let Some(n) = dep.input.card_button {
            s.push("dinput", "card", &n.to_string());
        }
        // 油门 / 刹车为模拟轴（dinput 下写入轴名，非按键）
        if let Some(ref n) = dep.input.throttle_axis {
            s.push("dinput", "accelAxis", n);
        }
        if let Some(ref n) = dep.input.brake_axis {
            s.push("dinput", "brakeAxis", n);
        }
        // 反转轴值：勾选为 true，未勾选（含未设置）为 false
        s.push(
            "dinput",
            "reverseAccelAxis",
            bool_str(dep.input.reverse_accel_axis.unwrap_or(false)),
        );
        s.push(
            "dinput",
            "reverseBrakeAxis",
            bool_str(dep.input.reverse_brake_axis.unwrap_or(false)),
        );
    }

    // ---- [CardReader]：选中的车卡（空白卡为 InidCrdXXXXX.png 同样照写） ----
    if let Some(c) = dep.card.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        s.push("CardReader", "cardfile", c);
    }

    s.to_string()
}

// ---------------------------------------------------------------------------
// 部署：复制工具文件 + 原子写入配置
// ---------------------------------------------------------------------------

/// 复制时排除的运行时产物 / 启动器自行生成的文件（文件名不区分大小写）
const EXCLUDE_FILES: &[&str] = &["config.ini", "idas.ini", "sram.bin", "eeprom.bin"];

/// 部署 mice 到目标目录：
/// 1) 把工具目录（mice.exe 所在目录）**顶层**常规文件复制到该目录（同名覆盖）；
///    跳过 config.ini / idas.ini（由启动器生成）、*.log / sram.bin / eeprom.bin（运行时产物），
///    目标与源大小 + 修改时间一致时跳过以省 IO。
/// 2) 确保 `<目录>\mice\` 存在（无则新建），并把 `render_config` / `render_idas`
///    结果原子写入（临时文件 + rename）。
pub fn deploy(
    dir: &Path,
    tool_dir: &Path,
    dep: &DeploySnapshot,
    target: MiceTarget,
) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(tf!("目标目录不存在：{}", "Target directory does not exist: {}", dir.display()));
    }
    if !tool_dir.is_dir() {
        return Err(tf!("mice 工具目录不存在：{}", "mice tool directory does not exist: {}", tool_dir.display()));
    }

    copy_tool_files(tool_dir, dir)?;

    let cfg_dir = dir.join("mice");
    fs::create_dir_all(&cfg_dir).map_err(|e| {
        tf!(
            "创建 mice 配置目录失败（{}）：{e}",
            "Failed to create mice config directory ({}): {e}",
            cfg_dir.display()
        )
    })?;
    write_atomic(&cfg_dir.join("config.ini"), &render_config(dep, target))?;
    write_atomic(&cfg_dir.join("idas.ini"), &render_idas(dep))?;
    Ok(())
}

/// 把 tool_dir 顶层常规文件复制到 dest（同名覆盖）。子目录不递归。
fn copy_tool_files(tool_dir: &Path, dest: &Path) -> Result<(), String> {
    let entries = fs::read_dir(tool_dir).map_err(|e| {
        tf!(
            "读取 mice 工具目录失败（{}）：{e}",
            "Failed to read mice tool directory ({}): {e}",
            tool_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            tf!(
                "读取 mice 工具目录条目失败（{}）：{e}",
                "Failed to read mice tool directory entry ({}): {e}",
                tool_dir.display()
            )
        })?;
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let lower = name.to_lowercase();
        if EXCLUDE_FILES.contains(&lower.as_str()) || lower.ends_with(".log") {
            continue;
        }
        let src = entry.path();
        let dst = dest.join(&name);
        if same_file(&src, &dst) {
            continue;
        }
        fs::copy(&src, &dst).map_err(|e| {
            tf!(
                "复制 mice 工具文件失败（{} → {}）：{e}",
                "Failed to copy mice tool file ({} -> {}): {e}",
                src.display(),
                dst.display()
            )
        })?;
    }
    Ok(())
}

/// 目标已存在且与源文件「大小 + 修改时间」一致则视为无需更新。
fn same_file(src: &Path, dst: &Path) -> bool {
    match (fs::metadata(src), fs::metadata(dst)) {
        (Ok(s), Ok(d)) => {
            s.is_file() && d.is_file() && s.len() == d.len() && s.modified().ok() == d.modified().ok()
        }
        _ => false,
    }
}

/// 先写临时文件再 rename 到目标，避免中途崩溃留下半个配置文件；
/// 内容无变化则不动 mtime（幂等）。
pub(crate) fn write_atomic(target: &Path, text: &str) -> Result<(), String> {
    if fs::read_to_string(target).is_ok_and(|old| old == text) {
        return Ok(());
    }
    let tmp = target.with_extension("ini.tmp");
    fs::write(&tmp, text).map_err(|e| {
        tf!(
            "写入临时配置失败（{}）：{e}",
            "Failed to write temporary config ({}): {e}",
            tmp.display()
        )
    })?;
    fs::rename(&tmp, target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        tf!(
            "替换配置失败（{}）：{e}",
            "Failed to replace config ({}): {e}",
            target.display()
        )
    })
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn snapshot() -> DeploySnapshot {
        DeploySnapshot {
            net: NetworkConfig::default(),
            system: SystemConfig::default(),
            coin: CoinConfig::default(),
            graphics: GraphicsConfig::default(),
            input: InputConfig::default(),
            card: None,
            room: None,
        }
    }

    // ---- 网段推导 ----

    #[test]
    fn parse_cidr_and_derivations() {
        assert_eq!(parse_cidr("192.168.100.0/24"), Some((0xC0A8_6400, 24)));
        assert_eq!(parse_cidr("192.168.100.114/24"), Some((0xC0A8_6400, 24)));
        assert_eq!(parse_cidr("10.0.0.5"), Some((0x0A00_0005, 32)));
        assert_eq!(parse_cidr("bad"), None);
        assert_eq!(parse_cidr("1.2.3.4/33"), None);

        assert_eq!(mask_of(24), 0xFFFF_FF00);
        assert_eq!(format_ipv4(mask_of(24)), "255.255.255.0");
        assert_eq!(format_ipv4(mask_of(16)), "255.255.0.0");
        assert_eq!(format_ipv4(mask_of(8)), "255.0.0.0");

        // /24 末地址 = broadcast-1 = x.x.x.254
        let (base, prefix) = parse_cidr("192.168.100.0/24").unwrap();
        assert_eq!(last_host(base, prefix).map(format_ipv4).as_deref(), Some("192.168.100.254"));
        // /32、/31 无末地址
        assert_eq!(last_host(0xC0A8_6400, 32), None);
        assert_eq!(last_host(0xC0A8_6400, 31), None);
        // /30：broadcast-1
        let (b30, p30) = parse_cidr("192.168.1.4/30").unwrap();
        assert_eq!(last_host(b30, p30).map(format_ipv4).as_deref(), Some("192.168.1.6"));
    }

    // ---- config.ini 渲染 ----

    #[test]
    fn render_config_fixed_keys_and_defaults() {
        let text = render_config(&snapshot(), MiceTarget::Game);
        assert_eq!(
            text,
            "[mice]\n\
             apply_patches = false\n\
             log_level = 4\n\
             \n\
             [network]\n\
             naominet_jp = sdgt.arcadehub.xyz\n\
             aime_naominet_jp = sdgt.arcadehub.xyz\n\
             ib_naominet_jp = 127.0.0.1\n\
             \n\
             [ffb]\n\
             enable = true\n\
             constantForceStrength = 100\n\
             damperStrength = 100\n\
             rumbleStrength = 100\n\
             rumbleDuration = 100\n\
             baseDamperStrength = 20\n\
             deadBand = 20\n"
        );
    }

    #[test]
    fn render_config_ffb_default_and_custom() {
        let text = render_config(&snapshot(), MiceTarget::Game);
        assert!(text.contains("[ffb]"), "config.ini 应写 [ffb] 力反馈段");
        assert!(text.contains("enable = true"));
        assert!(text.contains("constantForceStrength = 100"));
        assert!(text.contains("damperStrength = 100"));
        assert!(text.contains("rumbleStrength = 100"));
        assert!(text.contains("rumbleDuration = 100"));
        assert!(text.contains("baseDamperStrength = 20"));
        assert!(text.contains("deadBand = 20"));

        // 自定义值
        let mut dep = snapshot();
        dep.input.ffb_enable = Some(false);
        dep.input.ffb_constant_force = Some(50);
        dep.input.ffb_damper = Some(40);
        dep.input.ffb_rumble = Some(30);
        dep.input.ffb_rumble_duration = Some(60);
        dep.input.ffb_base_damper = Some(10);
        dep.input.ffb_dead_band = Some(5);
        let text = render_config(&dep, MiceTarget::Game);
        assert!(text.contains("enable = false"));
        assert!(text.contains("constantForceStrength = 50"));
        assert!(text.contains("damperStrength = 40"));
        assert!(text.contains("rumbleStrength = 30"));
        assert!(text.contains("rumbleDuration = 60"));
        assert!(text.contains("baseDamperStrength = 10"));
        assert!(text.contains("deadBand = 5"));
    }

    #[test]
    fn render_config_subnet_game_vs_serverbox() {
        let mut dep = snapshot();
        dep.net.ip = Some("192.168.100.0/24".into());
        let game = render_config(&dep, MiceTarget::Game);
        let sb = render_config(&dep, MiceTarget::ServerBox);
        for (text, ip) in [(game, "192.168.100.101"), (sb, "192.168.100.102")] {
            assert!(text.contains("keychip_subnet = 192.168.100.0/24"));
            assert!(text.contains(&format!("ip_address = {ip}")));
            assert!(text.contains("subnet_mask = 255.255.255.0"));
            assert!(text.contains("gateway = 192.168.100.254"));
            assert!(text.contains("primary_dns = 192.168.100.254"));
        }
        // 节顺序：sysconf 在 network 之前
        let game = render_config(&dep, MiceTarget::Game);
        assert!(game.find("[sysconf]").unwrap() < game.find("[network]").unwrap());
    }

    #[test]
    fn render_config_unset_subnet_omits_network_derived() {
        let mut dep = snapshot();
        dep.net.ip = Some("not-a-cidr".into());
        let text = render_config(&dep, MiceTarget::Game);
        assert!(!text.contains("keychip_subnet"));
        assert!(!text.contains("ip_address"));
        assert!(!text.contains("gateway"));
        assert!(!text.contains("primary_dns"));
        // 固定私服键仍在
        assert!(text.contains("naominet_jp = sdgt.arcadehub.xyz"));
    }

    #[test]
    fn render_config_loglevel_mapping() {
        let mut dep = snapshot();
        for (raw, want) in [
            ("error", 2),
            ("warn", 3),
            ("info", 4),
            ("debug", 5),
            ("trace", 6),
            ("unknown", 4), // 非法值回落默认
        ] {
            dep.system.loglevel = Some(raw.into());
            assert!(
                render_config(&dep, MiceTarget::Game).contains(&format!("log_level = {want}")),
                "{raw} → {want}"
            );
        }
        dep.system.loglevel = None;
        assert!(render_config(&dep, MiceTarget::Game).contains("log_level = 4"));
    }

    #[test]
    fn render_config_region_mapping() {
        let mut dep = snapshot();
        for (raw, want) in [("JPN", 1), ("EXP", 4), ("CHN", 8)] {
            dep.system.region = Some(raw.into());
            assert!(
                render_config(&dep, MiceTarget::Game).contains(&format!("region = {want}")),
                "{raw} → {want}"
            );
        }
        // 未知地区 / 未设置不写
        dep.system.region = Some("USA".into());
        assert!(!render_config(&dep, MiceTarget::Game).contains("region ="));
        dep.system.region = Some(" jpn ".into()); // trim + 大小写不敏感
        assert!(render_config(&dep, MiceTarget::Game).contains("region = 1"));
        dep.system.region = None;
        assert!(!render_config(&dep, MiceTarget::Game).contains("region ="));
    }

    #[test]
    fn render_config_keychip_freeplay_and_window() {
        let mut dep = snapshot();
        dep.system.keychip = Some(" A69E-01A99999999 ".into());
        dep.coin.freeplay = Some(true);
        dep.graphics.default = Some(false);
        dep.graphics.fullscreen = Some(true);
        dep.graphics.borderless = Some(false);
        dep.graphics.resolution = Some("1920x1080".into());
        let text = render_config(&dep, MiceTarget::Game);
        assert!(text.contains("keychip_serial = A69E-01A99999999"));
        assert!(text.contains("freeplay = true"));
        assert!(text.contains("[window]\nwindowed = false\nborderless = false\ndipsw = false\nw = 1920\nh = 1080\n"));
    }

    #[test]
    fn render_config_window_omitted_when_default() {
        let mut dep = snapshot();
        dep.graphics.default = Some(true);
        dep.graphics.resolution = Some("1920x1080".into());
        let text = render_config(&dep, MiceTarget::Game);
        assert!(!text.contains("[window]"));
        assert!(!text.contains("dipsw"));
    }

    // ---- idas.ini 渲染 ----

    #[test]
    fn render_idas_serverbox_and_coins_skip_unset() {
        let mut dep = snapshot();
        dep.coin.start_coin = Some(2);
        dep.coin.car_coin = Some(1);
        let text = render_idas(&dep);
        // [io3] restrict_ 始终写出（未设置比率时默认 128）；
        // skipInitialization 也始终写出，未设置开关时为 false（同向，未勾选=不跳过）
        assert_eq!(
            text,
            "[ServerBox]\n\
             offlineMode = false\n\
             simulateNetwork = true\n\
             skipInitialization = false\n\
             \n\
             [Coins]\n\
             start = 2\n\
             car = 1\n\
             \n\
             [io3]\n\
             restrict_ = 128\n"
        );
    }

    /// 「跳过读卡器初始化」→ idas.ini `[ServerBox] skipInitialization` 同向且始终写出
    #[test]
    fn render_idas_skip_initialization_same_direction_always_written() {
        for (skip, want) in [(None, "false"), (Some(false), "false"), (Some(true), "true")] {
            let mut dep = snapshot();
            dep.system.skip_reader_init = skip;
            let text = render_idas(&dep);
            assert!(
                text.contains(&format!("skipInitialization = {want}")),
                "skip_reader_init={skip:?} 时应写 skipInitialization = {want}：\n{text}"
            );
        }
    }

    #[test]
    fn render_idas_offline_mode_always_false() {
        // offlineMode 固定写 false；simulateNetwork 随「模拟网络」开关（未设置视为开）
        for (sim, sim_net) in [(None, "true"), (Some(true), "true"), (Some(false), "false")] {
            let mut dep = snapshot();
            dep.net.simulate = sim;
            let text = render_idas(&dep);
            assert!(
                text.contains("offlineMode = false"),
                "simulate={sim:?} 时应写 offlineMode = false：\n{text}"
            );
            assert!(
                text.contains(&format!("simulateNetwork = {sim_net}")),
                "simulate={sim:?} 时应写 simulateNetwork = {sim_net}：\n{text}"
            );
        }
    }

    #[test]
    fn render_idas_render_scale_mapping() {
        let mut dep = snapshot();
        dep.graphics.render_scale = Some(true);
        let cases: &[(&str, Option<u32>)] = &[
            ("2x", Some(2)),
            ("4x", Some(3)),
            ("8x", Some(4)),
            ("16x", None),
            ("bad", None),
        ];
        for (raw, want) in cases {
            dep.graphics.render_scale_level = Some((*raw).into());
            let text = render_idas(&dep);
            match want {
                Some(n) => assert!(text.contains(&format!("render_scale = {n}"))),
                None => assert!(!text.contains("render_scale"), "{raw} 不应写出"),
            }
        }
        // 渲染倍数开关关闭时不写
        dep.graphics.render_scale = Some(false);
        dep.graphics.render_scale_level = Some("2x".into());
        assert!(!render_idas(&dep).contains("render_scale"));
    }

    #[test]
    fn render_idas_input_mappings() {
        let mut dep = snapshot();
        dep.input.kind = Some("xinput".into());
        dep.input.ratio = Some(128);
        dep.input.dual_stick = Some(true);
        dep.input.linear_steering = Some(false);
        dep.input.left_deadzone = Some(20);
        dep.input.right_deadzone = Some(20);
        let text = render_idas(&dep);
        assert!(text.contains("[io3]\nmode = xinput\nrestrict_ = 128\n"));
        assert!(text.contains("singleStickSteering = false"));
        assert!(text.contains("linearSteering = false"));
        // 20% → 32767 × 0.2 ≈ 6553
        assert!(text.contains("leftStickDeadzone = 6553"));
        assert!(text.contains("rightStickDeadzone = 6553"));
        assert!(!text.contains("[dinput]"), "xinput 模式不写 dinput 节");

        // dinput 模式：设备与按键
        let mut dep2 = snapshot();
        dep2.input.kind = Some("dinput".into());
        dep2.input.wheel_device = Some("Wireless Controller".into());
        dep2.input.start_button = Some(2);
        dep2.input.view_button = Some(3);
        dep2.input.shift_up = Some(6);
        dep2.input.shift_down = Some(5);
        dep2.input.coin_button = Some(11);
        dep2.input.card_button = Some(12);
        dep2.input.throttle_axis = Some("Y".into());
        dep2.input.brake_axis = Some("RZ".into());
        let text2 = render_idas(&dep2);
        assert!(text2.contains("mode = dinput"));
        assert!(text2.contains("deviceName = Wireless Controller"));
        assert!(text2.contains("start = 2"));
        assert!(text2.contains("viewChg = 3"));
        assert!(text2.contains("shiftUp = 6"));
        assert!(text2.contains("shiftDn = 5"));
        // 投币 / 插卡按键写入 [dinput]
        assert!(text2.contains("coin = 11"));
        assert!(text2.contains("card = 12"));
        // 油门 / 刹车为模拟轴，写入 accelAxis / brakeAxis（轴名）
        assert!(text2.contains("accelAxis = Y"));
        assert!(text2.contains("brakeAxis = RZ"));
        // 反转轴默认未勾选 → false
        assert!(text2.contains("reverseAccelAxis = false"));
        assert!(text2.contains("reverseBrakeAxis = false"));
        assert!(!text2.contains("[xinput]"), "dinput 模式不写 xinput 节");
    }

    #[test]
    fn render_idas_xinput_device_index_and_buttons() {
        // 未选手柄槽位（pad_index = None）→ deviceIndex 始终写出默认 0
        let mut dep = snapshot();
        dep.input.kind = Some("xinput".into());
        let text = render_idas(&dep);
        assert!(text.contains("[xinput]"));
        // deviceIndex 始终写，默认 0
        let block = text.split("[xinput]").nth(1).unwrap();
        let line = block.lines().find(|l| l.starts_with("deviceIndex")).unwrap();
        assert_eq!(line, "deviceIndex = 0");
        // 未捕获任何按钮名 → 不写 start/viewChg/... 等键（交 mice 默认兜底）
        assert!(!text.contains("start ="));
        assert!(!text.contains("viewChg ="));
        assert!(!text.contains("coin ="));
        assert!(!text.contains("accel ="));
        assert!(!text.contains("brake ="));

        // 选定槽位 + 捕获若干按钮名 → 写出对应键；空的仍省略
        let mut dep2 = snapshot();
        dep2.input.kind = Some("xinput".into());
        dep2.input.pad_index = Some(2);
        dep2.input.xi_start = Some("A".into());
        dep2.input.xi_coin = Some("Y".into());
        dep2.input.xi_card = Some("START".into());
        dep2.input.xi_accel = Some("RT".into());
        dep2.input.xi_brake = Some("LT".into());
        dep2.input.xi_up = Some(" ".into()); // 空白视为未设置
        let text2 = render_idas(&dep2);
        assert!(text2.contains("deviceIndex = 2"));
        assert!(text2.contains("start = A"));
        assert!(!text2.contains("viewChg =")); // 未设置不写
        assert!(text2.contains("coin = Y"));
        assert!(text2.contains("card = START"));
        assert!(text2.contains("accel = RT"), "油门源应写捕获的按钮名");
        assert!(text2.contains("brake = LT"), "刹车源应写捕获的按钮名");
        // 空白值不写出
        assert!(!text2.contains("up ="));
        // 未设置的其余键也不写
        assert!(!text2.contains("shiftDn ="));
        assert!(!text2.contains("down ="));
    }

    // ---- 部署 ----

    /// 每个测试独立的临时目录，结束后自动清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "mice_test_{}_{tag}_{seq}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn deploy_copies_tools_and_writes_configs() {
        let tmp = TempDir::new("deploy");
        let tool = tmp.0.join("tool");
        let game = tmp.0.join("game");
        fs::create_dir_all(&tool).unwrap();
        fs::create_dir_all(&game).unwrap();

        // 工具目录：mice.exe + 依赖 DLL + 应被排除的文件
        fs::write(tool.join("mice.exe"), "mice-bin").unwrap();
        fs::write(tool.join("mice.dll"), "dll-bin").unwrap();
        fs::write(tool.join("config.ini"), "[old]\nk = v\n").unwrap();
        fs::write(tool.join("idas.ini"), "[old]\nk = v\n").unwrap();
        fs::write(tool.join("mice.log"), "log").unwrap();
        fs::write(tool.join("sram.bin"), "sram").unwrap();
        fs::write(tool.join("eeprom.bin"), "eeprom").unwrap();

        let mut dep = snapshot();
        dep.net.ip = Some("192.168.100.0/24".into());
        dep.system.keychip = Some("A69E-01A99999999".into());

        mice_deploy(&game, &tool, &dep).unwrap();

        // 顶层复制：mice.exe、DLL 在，排除的文件不进来
        assert_eq!(fs::read(game.join("mice.exe")).unwrap(), b"mice-bin");
        assert_eq!(fs::read(game.join("mice.dll")).unwrap(), b"dll-bin");
        assert!(!game.join("config.ini").exists(), "不应复制工具自带 config.ini");
        assert!(!game.join("idas.ini").exists());
        assert!(!game.join("mice.log").exists());
        assert!(!game.join("sram.bin").exists());
        assert!(!game.join("eeprom.bin").exists());

        // <目录>\mice\ 下两份配置
        let cfg = fs::read_to_string(game.join("mice/config.ini")).unwrap();
        assert_eq!(cfg, render_config(&dep, MiceTarget::Game));
        assert!(cfg.contains("ip_address = 192.168.100.101"));
        assert_eq!(
            fs::read_to_string(game.join("mice/idas.ini")).unwrap(),
            render_idas(&dep)
        );

        // 幂等：再部署一次不报错（配置内容一致不重写；工具文件跳过）
        let mtime_before = fs::metadata(game.join("mice/config.ini"))
            .unwrap()
            .modified()
            .unwrap();
        mice_deploy(&game, &tool, &dep).unwrap();
        let mtime_after = fs::metadata(game.join("mice/config.ini"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime_before, mtime_after, "内容一致不应重写配置");
    }

    #[test]
    fn deploy_serverbox_target_uses_102() {
        let tmp = TempDir::new("sb");
        let tool = tmp.0.join("tool");
        let sb = tmp.0.join("sb");
        fs::create_dir_all(&tool).unwrap();
        fs::create_dir_all(&sb).unwrap();
        fs::write(tool.join("mice.exe"), "x").unwrap();
        let mut dep = snapshot();
        dep.net.ip = Some("192.168.100.0/24".into());
        deploy(&sb, &tool, &dep, MiceTarget::ServerBox).unwrap();
        let cfg = fs::read_to_string(sb.join("mice/config.ini")).unwrap();
        assert!(cfg.contains("ip_address = 192.168.100.102"));
    }

    #[test]
    fn deploy_missing_dirs_error() {
        let tmp = TempDir::new("missing");
        let tool = tmp.0.join("tool");
        let game = tmp.0.join("game");
        fs::create_dir_all(&tool).unwrap();
        fs::write(tool.join("mice.exe"), "x").unwrap();

        // 目标目录不存在 → 报错
        let err = mice_deploy(&game, &tool, &snapshot()).unwrap_err();
        assert!(err.contains("目标目录不存在"), "{err}");
        // 工具目录不存在 → 报错
        fs::create_dir_all(&game).unwrap();
        let err = mice_deploy(&game, &tmp.0.join("nope"), &snapshot()).unwrap_err();
        assert!(err.contains("工具目录不存在"), "{err}");
    }

    /// 与 launch.rs 调用形态一致的便捷封装（Game 目标）。
    fn mice_deploy(
        dir: &Path,
        tool_dir: &Path,
        dep: &DeploySnapshot,
    ) -> Result<(), String> {
        deploy(dir, tool_dir, dep, MiceTarget::Game)
    }

    #[test]
    fn render_idas_cardreader_cardfile() {
        let mut dep = snapshot();
        // 未选卡：不写 [CardReader]
        assert!(!render_idas(&dep).contains("[CardReader]"));

        // 选卡（含空白卡 InidCrdXXXXX.png）：写入 [CardReader] cardfile
        dep.card = Some("InidCrd12345.png".into());
        let text = render_idas(&dep);
        assert!(text.contains("[CardReader]
cardfile = InidCrd12345.png"));
    }

    #[test]
    fn render_config_naominet_uses_server_setting() {
        let mut dep = snapshot();
        dep.net.server = Some(" my.server.net ".into());
        let text = render_config(&dep, MiceTarget::Game);
        assert!(text.contains("naominet_jp = my.server.net"));
        assert!(text.contains("aime_naominet_jp = my.server.net"));
        assert!(!text.contains("sdgt.arcadehub.xyz"), "自定义服务器时不再写默认域名");
    }

    #[test]
    fn render_idas_serverbox_bind_ip() {
        // 未配置 serverbox_ip：不写 bind_ip
        let text = render_idas(&snapshot());
        assert!(!text.contains("bind_ip"));

        // 配置后并入 [ServerBox] 节（trim 前后空白）；仅关闭模拟网络时生效
        let mut dep = snapshot();
        dep.net.simulate = Some(false);
        dep.net.serverbox_ip = Some("  192.168.100.101  ".into());
        let text = render_idas(&dep);
        assert!(
            text.contains("bind_ip = 192.168.100.101"),
            "应写出 [ServerBox] bind_ip：\n{text}"
        );
        assert!(
            text.find("bind_ip = 192.168.100.101").unwrap() > text.find("[ServerBox]").unwrap(),
            "bind_ip 必须位于 [ServerBox] 节内：\n{text}"
        );
        assert!(!text.contains("[serverbox]"), "不应出现独立的 [serverbox] 节");
    }

    #[test]
    fn render_idas_host_game_ip() {
        // 未配置 host_ip：不写 game_ip
        let text = render_idas(&snapshot());
        assert!(!text.contains("game_ip"));

        // 模拟网络开启（None / Some(true)）：即便配置了 host_ip 也不写 game_ip
        let mut dep = snapshot();
        dep.net.host_ip = Some("  192.168.100.50  ".into());
        assert!(
            !render_idas(&dep).contains("game_ip"),
            "模拟网络开启时不应写 game_ip"
        );
        dep.net.simulate = Some(true);
        assert!(
            !render_idas(&dep).contains("game_ip"),
            "模拟网络开启时不应写 game_ip"
        );

        // 关闭模拟网络后并入 [ServerBox] 节（trim 前后空白）
        dep.net.simulate = Some(false);
        let text = render_idas(&dep);
        assert!(
            text.contains("game_ip = 192.168.100.50"),
            "应写出 [ServerBox] game_ip：\n{text}"
        );
        assert!(
            text.find("game_ip = 192.168.100.50").unwrap() > text.find("[ServerBox]").unwrap(),
            "game_ip 必须位于 [ServerBox] 节内：\n{text}"
        );
    }

    #[test]
    fn render_config_serverbox_bind_ip() {
        // 游戏（Game）目标：即便配置了 serverbox_ip，config.ini 也不写 [ServerBox]
        let mut dep = snapshot();
        dep.net.simulate = Some(false);
        dep.net.serverbox_ip = Some("192.168.100.101".into());
        assert!(
            !render_config(&dep, MiceTarget::Game).contains("[ServerBox]"),
            "Game 目标的 config.ini 不应含 [ServerBox]"
        );

        // ServerBox 目标 + 关闭模拟网络 + 已配置：写出 [ServerBox] bind_ip
        let mut dep = snapshot();
        dep.net.simulate = Some(false);
        dep.net.serverbox_ip = Some("  192.168.100.101  ".into());
        let text = render_config(&dep, MiceTarget::ServerBox);
        assert!(
            text.contains("[ServerBox]\nbind_ip = 192.168.100.101\n"),
            "ServerBox 的 config.ini 应写出 [ServerBox] bind_ip：\n{text}"
        );

        // 未配置 serverbox_ip：不写
        let text = render_config(&snapshot(), MiceTarget::ServerBox);
        assert!(!text.contains("[ServerBox]"));

        // 模拟网络未关闭（None / Some(true)）：不写
        for sim in [None, Some(true)] {
            let mut dep = snapshot();
            dep.net.simulate = sim;
            dep.net.serverbox_ip = Some("192.168.100.101".into());
            assert!(
                !render_config(&dep, MiceTarget::ServerBox).contains("[ServerBox]"),
                "simulate={sim:?} 时不应写 [ServerBox]"
            );
        }
    }
}
