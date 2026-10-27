//! kitten 游戏启动前的自动部署与配置生成：
//!
//! 仅对「加载器 = kitten」的 Lindbergh 游戏（头文字D 4/5）生效，时机为 VPN 连接成功之后、
//! 拉起游戏之前。职责有两块：
//! 1. `render`：以启动器全局设置为唯一来源，把 `config.ini` 渲染为纯 `键 = 值` 文本。
//!    规则（与产品约定一致）：
//!    - 不含注释，不拷贝 kitten 模板里的任何注释 / 示例键；
//!    - 启动器未设置（或 UI 无对应项）的键一律不写，交给 kitten 自身默认值兜底；
//!    - 某节没有任何写出键时整节省略；
//!    - 固定派生键例外：`[network] redirect`（必须替换 kitten 默认回环地址）、
//!      `[network] simulateNetwork`（「模拟网络」默认开启，未设置时写 true）、
//!      `[display] dipsw=false`（自定义分辨率必须关掉 dipsw，否则被覆盖）、
//!      `[sysconf] dipsw=0x3A`（仅区域 CHN，0x02 中文位）。
//! 2. `deploy`：把工具目录顶层文件复制到游戏目录（排除运行时产物与启动器生成的 config.ini），
//!    并把 `render` 结果原子写入 `<游戏目录>\kitten\config.ini`，返回可启动的
//!    `<游戏目录>\kitten.exe`。游戏结束后这些文件不清理，下次启动时同名覆盖保持最新。

use std::fs;
use std::path::{Path, PathBuf};

use crate::mice;
use crate::tf;
use crate::storage::{
    CoinConfig, GameRecord, GraphicsConfig, InputConfig, INPUT_RATIO_DEFAULT, NetworkConfig,
    SystemConfig,
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
    /// 命中识别库的产品 game_id（SBML/SBQZ/SBTS）；未命中为 None，不写
    pub game_id: Option<String>,
}

// ---------------------------------------------------------------------------
// config.ini 渲染（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 待输出的若干节（保序）；节内每行形如 `key = value`。
struct Sections(Vec<(String, Vec<String>)>);

impl Sections {
    fn new() -> Self {
        Sections(Vec::new())
    }

    /// 把 `key = value` 追加到指定节；节不存在则新建并排在末尾。
    fn push(&mut self, section: &str, key: &str, value: &str) {
        if let Some((_, lines)) = self.0.iter_mut().find(|(s, _)| s == section) {
            lines.push(format!("{key} = {value}"));
        } else {
            self.0.push((
                section.to_string(),
                vec![format!("{key} = {value}")],
            ));
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

fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// 日志级别（launcher 值）→ kitten log_level 数值。
/// `off` 是历史遗留值：kitten 无 off，按未设置处理（不写，kitten 默认 3 = Info）。
fn log_level_num(v: &str) -> Option<u32> {
    match v.trim().to_ascii_lowercase().as_str() {
        "error" => Some(5),
        "warn" => Some(4),
        "info" => Some(3),
        "debug" => Some(1),
        "trace" => Some(0),
        _ => None,
    }
}

/// 地区（launcher 值）→ kitten region 数值。JPN=0 / EXP=2 / CHN=2。
fn region_num(v: &str) -> Option<u32> {
    match v.trim().to_ascii_uppercase().as_str() {
        "JPN" => Some(0),
        "EXP" | "CHN" => Some(2),
        _ => None,
    }
}

/// 解析 "1920x1080" 这类分辨率字符串；无效返回 None。
fn split_resolution(v: &str) -> Option<(u32, u32)> {
    let s = v.trim();
    let x = s.find(['x', 'X'])?;
    let w = s[..x].trim().parse().ok()?;
    let h = s[x + 1..].trim().parse().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// 解析 "2x" / "4x" / "16x"（也接受纯数字）倍率 → 数值；无效返回 None。
fn nx_num(v: &str) -> Option<u32> {
    let digits: String = v.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// 死区百分比（0~100）→ kitten 的整数值（`32767 × 百分比`，四舍五入）。
fn deadzone_value(pct: u32) -> u32 {
    (((32767u64 * pct as u64) + 50) / 100) as u32
}

/// 写入 XInput 按钮名映射：仅当名称已设置且非空时才写出对应键，否则交 kitten 默认兜底。
fn push_xi_button(s: &mut Sections, key: &str, name: &Option<String>) {
    if let Some(v) = name.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        s.push("xinput", key, v);
    }
}

/// 按启动器全局设置快照渲染 kitten config.ini 文本。
pub fn render(dep: &DeploySnapshot) -> String {
    let mut s = Sections::new();

    // ---- [display]：仅当不使用默认图形配置时考虑；无任何自定义键则整节省略 ----
    if dep.graphics.default != Some(true) {
        if let Some((w, h)) = dep.graphics.resolution.as_deref().and_then(split_resolution) {
            // kitten 默认 dipsw=true 会覆盖自定义分辨率，这里必须显式关掉
            s.push("display", "dipsw", "false");
            s.push("display", "width", &w.to_string());
            s.push("display", "height", &h.to_string());
        }
        if let Some(b) = dep.graphics.borderless {
            s.push("display", "borderless", bool_str(b));
        }
        if dep.graphics.render_scale == Some(true) {
            if let Some(n) = dep.graphics.render_scale_level.as_deref().and_then(nx_num) {
                s.push("display", "render_scale", &n.to_string());
            }
        }
        if dep.graphics.msaa == Some(true) {
            if let Some(n) = dep.graphics.msaa_level.as_deref().and_then(nx_num) {
                s.push("display", "msaa_samples", &n.to_string());
            }
        }
        if dep.graphics.af == Some(true) {
            if let Some(n) = dep.graphics.af_level.as_deref().and_then(nx_num) {
                s.push("display", "anisotropic", &n.to_string());
            }
        }
    }

    // ---- [network] ----
    // redirect 恒写：必须替换 kitten 默认的 127.0.0.1（回环地址会被 kitten 拒绝）
    s.push("network", "redirect", dep.net.server_host());
    if let Some(ip) = &dep.net.ip {
        s.push("network", "subnet", ip);
    }
    // 启动器「模拟网络」开关默认开启（UI 上 None 也显示为开），因此恒写：
    // None / Some(true) -> true；仅用户显式关掉（Some(false)）才写 false
    let sim = dep.net.simulate.unwrap_or(true);
    s.push("network", "simulateNetwork", bool_str(sim));

    // ---- [network] 静态 IP：仅关闭模拟网络时才写 ----
    // 模拟网络开启（None / Some(true)）时 IP 由 kitten 自行模拟派生，
    // 写死 bind_ip / game_ip 会与之冲突，故一律不写。
    if dep.net.simulate == Some(false) {
        // ServerBox 静态绑定 IP：kitten 的 serverbox.exe 读取 config.ini 的 [network] 节
        if let Some(ip) = dep.net.serverbox_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            s.push("network", "bind_ip", ip);
        }
        // 主机（本机游戏）IP（房间模式已按覆盖替换快照值）
        if let Some(ip) = dep.net.host_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            s.push("network", "game_ip", ip);
        }
    }

    // ---- [sysconf] ----
    if let Some(lv) = &dep.system.loglevel {
        if let Some(n) = log_level_num(lv) {
            s.push("sysconf", "log_level", &n.to_string());
        }
    }
    if let Some(r) = &dep.system.region {
        if let Some(n) = region_num(r) {
            s.push("sysconf", "region", &n.to_string());
            // CHN：0x02 = 中文位（仅 D5 EXP2.0 有意义），与默认分辨率位 0x38 合成 0x3A
            if r.trim().eq_ignore_ascii_case("CHN") {
                s.push("sysconf", "dipsw", "0x3A");
            }
        }
    }
    if let Some(id) = &dep.game_id {
        if !id.is_empty() {
            s.push("sysconf", "game_id", id);
        }
    }
    if let Some(k) = &dep.system.keychip {
        if !k.trim().is_empty() {
            s.push("sysconf", "keychip_serial", k.trim());
        }
    }
    if let Some(v) = dep.system.skip_reader_init {
        s.push("sysconf", "skipInitCard", bool_str(v));
    }

    // ---- [Coins] ----
    if let Some(fp) = dep.coin.freeplay {
        s.push("Coins", "freeplay", bool_str(fp));
    }
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

    // ---- [io3] ----
    match dep.input.kind.as_deref().map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("xinput") => s.push("io3", "mode", "1"),
        Some("dinput") => s.push("io3", "mode", "2"),
        _ => {}
    }
    // restrict_ = 输入信号限制（原「输入比率」；未设置时写默认 128）
    s.push(
        "io3",
        "restrict_",
        &dep.input.ratio.unwrap_or(INPUT_RATIO_DEFAULT).to_string(),
    );

    // ---- [xinput]：仅 XInput 模式；双摇杆 / 线性方向 / 左右摇杆死区 ----
    if dep
        .input
        .kind
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("xinput"))
        .unwrap_or(false)
    {
        // 双摇杆模式开启 → 单摇杆转向关闭（singleStickSteering = false）；关闭/未设置 → true。
        // 始终写入，避免未切换过 dual_stick（默认 None）时游戏缺少该键而行为不确定。
        // kitten 只认 true/false（布尔写法），不认 1/0。
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
        // 按钮名映射：仅在用户捕获过（已设置且非空）时写出，否则交 kitten 默认兜底。
        // kitten [xinput] 没有 card 键（mice 的 idas.ini 才有），插卡映射无处可写、跳过。
        push_xi_button(&mut s, "start", &dep.input.xi_start);
        push_xi_button(&mut s, "viewChg", &dep.input.xi_view);
        push_xi_button(&mut s, "up", &dep.input.xi_up);
        push_xi_button(&mut s, "down", &dep.input.xi_down);
        push_xi_button(&mut s, "left", &dep.input.xi_left);
        push_xi_button(&mut s, "right", &dep.input.xi_right);
        push_xi_button(&mut s, "shiftDn", &dep.input.xi_shift_dn);
        push_xi_button(&mut s, "shiftUp", &dep.input.xi_shift_up);
        push_xi_button(&mut s, "coin", &dep.input.xi_coin);
        // 油门 / 刹车源：接受 trigger（默认，未捕获不写）或按钮名
        push_xi_button(&mut s, "accel", &dep.input.xi_accel);
        push_xi_button(&mut s, "brake", &dep.input.xi_brake);
    }

    // ---- [dinput]：仅 DirectInput 模式；设备与按键映射（与 mice/idas.ini 同语义） ----
    if dep
        .input
        .kind
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("dinput"))
        .unwrap_or(false)
    {
        if let Some(v) = dep.input.wheel_device.as_deref() {
            s.push("dinput", "deviceName", v.trim());
        }
        if let Some(v) = dep.input.pedal_device.as_deref() {
            s.push("dinput", "pedalsName", v.trim());
        }
        if let Some(v) = dep.input.shifter_device.as_deref() {
            s.push("dinput", "shifterName", v.trim());
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
        // 油门 / 刹车为模拟轴
        if let Some(ref n) = dep.input.throttle_axis {
            s.push("dinput", "accelAxis", n);
        }
        if let Some(ref n) = dep.input.brake_axis {
            s.push("dinput", "brakeAxis", n);
        }
        // 反转轴值：勾选 true，未勾选（含未设置）为 false
        s.push("dinput", "reverseAccelAxis", bool_str(dep.input.reverse_accel_axis.unwrap_or(false)));
        s.push("dinput", "reverseBrakeAxis", bool_str(dep.input.reverse_brake_axis.unwrap_or(false)));
    }

    // ---- [ffb]：力反馈（与 mice/idas.ini 同语义；未设置按默认值） ----
    s.push("ffb", "enable", bool_str(dep.input.ffb_enable.unwrap_or(true)));
    s.push("ffb", "constantForceStrength", &dep.input.ffb_constant_force.unwrap_or(100).to_string());
    s.push("ffb", "damperStrength", &dep.input.ffb_damper.unwrap_or(100).to_string());
    s.push("ffb", "rumbleStrength", &dep.input.ffb_rumble.unwrap_or(100).to_string());
    s.push("ffb", "rumbleDuration", &dep.input.ffb_rumble_duration.unwrap_or(100).to_string());
    s.push("ffb", "baseDamperStrength", &dep.input.ffb_base_damper.unwrap_or(20).to_string());
    s.push("ffb", "deadBand", &dep.input.ffb_dead_band.unwrap_or(20).to_string());

    s.to_string()
}

// ---------------------------------------------------------------------------
// 部署：复制工具文件 + 原子写入 config.ini
// ---------------------------------------------------------------------------

/// 复制时排除的运行时产物 / 启动器自行生成的文件（文件名不区分大小写）
const EXCLUDE_FILES: &[&str] = &["config.ini", "sram.bin", "eeprom.bin"];

/// 部署 kitten：
/// 1) 把工具目录（kitten.exe 所在目录）**顶层**常规文件复制到游戏目录（同名覆盖）；
///    跳过 config.ini（由启动器生成）、*.log / sram.bin / eeprom.bin（运行时产物），
///    目标与源大小 + 修改时间一致时跳过以省 IO。
/// 2) 确保 `<游戏目录>\kitten\` 存在，并把 `render` 的 config.ini 整体写入（临时文件 + rename）。
///
/// 返回可启动的 `<游戏目录>\kitten.exe` 路径。
pub fn deploy(
    record: &GameRecord,
    tool_dir: &Path,
    dep: &DeploySnapshot,
) -> Result<PathBuf, String> {
    let game = Path::new(&record.path);
    if !game.is_file() {
        return Err(tf!("游戏文件不存在：{}", "Game file does not exist: {}", game.display()));
    }
    let Some(game_dir) = game.parent() else {
        return Err(tf!("无法确定游戏目录：{}", "Cannot determine game directory: {}", game.display()));
    };
    if !game_dir.is_dir() {
        return Err(tf!("游戏目录不存在：{}", "Game directory does not exist: {}", game_dir.display()));
    }

    copy_tool_files(tool_dir, game_dir)?;

    let cfg_dir = game_dir.join("kitten");
    fs::create_dir_all(&cfg_dir)
        .map_err(|e| tf!("创建 kitten 配置目录失败（{}）：{e}", "Failed to create kitten config directory ({}): {e}", cfg_dir.display()))?;
    write_config(&cfg_dir, dep)?;

    // D4/D5 同样生成 mice 形态的 config.ini 到 <游戏目录>\mice\，
    // 供 mice 系工具复用同一套启动器设置（网络 / 系统 / 投币 / 图形 / 输入）
    let mice_dep = mice::DeploySnapshot {
        net: dep.net.clone(),
        system: dep.system.clone(),
        coin: dep.coin.clone(),
        graphics: dep.graphics.clone(),
        input: dep.input.clone(),
        card: None,
        room: None,
    };
    let mice_dir = game_dir.join("mice");
    fs::create_dir_all(&mice_dir)
        .map_err(|e| tf!("创建 mice 配置目录失败（{}）：{e}", "Failed to create mice config directory ({}): {e}", mice_dir.display()))?;
    mice::write_atomic(&mice_dir.join("config.ini"), &mice::render_config(&mice_dep, mice::MiceTarget::Game))?;

    Ok(game_dir.join("kitten.exe"))
}

/// 把 tool_dir 顶层常规文件复制到 dest（同名覆盖）。子目录不递归。
fn copy_tool_files(tool_dir: &Path, dest: &Path) -> Result<(), String> {
    let entries = fs::read_dir(tool_dir)
        .map_err(|e| tf!("读取 kitten 工具目录失败（{}）：{e}", "Failed to read kitten tool directory ({}): {e}", tool_dir.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| tf!("读取 kitten 工具目录条目失败（{}）：{e}", "Failed to read kitten tool directory entry ({}): {e}", tool_dir.display()))?;
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
            tf!("复制 kitten 工具文件失败（{} → {}）：{e}", "Failed to copy kitten tool file ({} -> {}): {e}",
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

/// 先写临时文件再 rename 到目标，避免中途崩溃留下半个 config.ini。
fn write_config(cfg_dir: &Path, dep: &DeploySnapshot) -> Result<(), String> {
    let target = cfg_dir.join("config.ini");
    let tmp = cfg_dir.join("config.ini.tmp");
    let text = render(dep);
    fs::write(&tmp, text)
        .map_err(|e| tf!("写入临时 config 失败（{}）：{e}", "Failed to write temporary config ({}): {e}", tmp.display()))?;
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        tf!("替换 config.ini 失败（{}）：{e}", "Failed to replace config.ini ({}): {e}", target.display())
    })
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Loader;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

    /// 每个测试独立的临时目录，结束后自动清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "kitten_test_{}_{tag}_{seq}",
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

    fn snapshot() -> DeploySnapshot {
        DeploySnapshot {
            net: NetworkConfig::default(),
            system: SystemConfig::default(),
            coin: CoinConfig::default(),
            graphics: GraphicsConfig::default(),
            input: InputConfig::default(),
            game_id: None,
        }
    }

    fn record_at(game_dir: &Path) -> GameRecord {
        GameRecord {
            id: "test".into(),
            path: game_dir.join("id5.elf").to_string_lossy().into_owned(),
            name: "测试游戏".into(),
            crc: Some("0x12345678".into()),
            version: Some("REV A".into()),
            arch: Some("x86".into()),
            loader: Loader::Kitten,
            platform: None,
            image: None,
            cover: None,
            serverbox: None,
            reader: None,
            amdaemon: None,
            game_exe: None,
            gfx_patch: None,
            game_id: None,
            test_mode: None,
            card: None,
            play_count: 0,
            last_played: None,
            added_at: 0,
        }
    }

    #[test]
    fn render_minimal_redirect_and_simulate() {
        let text = render(&snapshot());
        // 未设置任何项：固定派生键 redirect / simulateNetwork（默认开启），
        // 以及始终写出的 [io3] restrict_（默认 128），不含注释
        assert_eq!(
            text,
            "[network]\nredirect = sdgt.arcadehub.xyz\nsimulateNetwork = true\n\n[io3]\nrestrict_ = 128\n\n[ffb]\nenable = true\nconstantForceStrength = 100\ndamperStrength = 100\nrumbleStrength = 100\nrumbleDuration = 100\nbaseDamperStrength = 20\ndeadBand = 20\n"
        );
    }

    #[test]
    fn render_simulate_defaults_true() {
        // 未设置（None）：UI 开关默认开，写 true
        let mut dep = snapshot();
        dep.net.simulate = None;
        assert!(render(&dep).contains("simulateNetwork = true"));
        // 显式开启
        dep.net.simulate = Some(true);
        assert!(render(&dep).contains("simulateNetwork = true"));
        // 显式关闭
        dep.net.simulate = Some(false);
        assert!(render(&dep).contains("simulateNetwork = false"));
    }

    #[test]
    fn render_off_loglevel_treated_as_unset() {
        let mut dep = snapshot();
        dep.system.loglevel = Some("off".into());
        let text = render(&dep);
        assert!(!text.contains("log_level"), "历史遗留 off 不应写 log_level");
        assert!(text.contains("[network]"));
    }

    #[test]
    fn render_full_mapping_and_chn_dipsw() {
        let mut dep = snapshot();
        dep.net.ip = Some("192.168.100.0/24".into());
        dep.net.simulate = Some(false);
        dep.system.loglevel = Some("debug".into());
        dep.system.region = Some("CHN".into());
        dep.system.keychip = Some("A69E-01A99999999".into());
        dep.system.skip_reader_init = Some(true);
        dep.game_id = Some("SBTS".into());
        dep.graphics.default = Some(false);
        dep.graphics.borderless = Some(false);
        dep.graphics.resolution = Some("1920x1080".into());
        dep.graphics.render_scale = Some(true);
        dep.graphics.render_scale_level = Some("2x".into());
        dep.graphics.msaa = Some(true);
        dep.graphics.msaa_level = Some("4x".into());
        dep.graphics.af = Some(true);
        dep.graphics.af_level = Some("16x".into());
        dep.coin.freeplay = Some(false);
        dep.coin.start_coin = Some(2);
        dep.coin.continue_coin = Some(2);
        dep.coin.card_coin = Some(3);
        dep.coin.car_coin = Some(1);
        dep.input.kind = Some("dinput".into());
        dep.input.ratio = Some(128);

        let text = render(&dep);

        // 分区断言：存在且值正确
        assert!(text.contains("[display]\ndipsw = false\nwidth = 1920\nheight = 1080\n"));
        assert!(text.contains("borderless = false"));
        assert!(text.contains("render_scale = 2"));
        assert!(text.contains("msaa_samples = 4"));
        assert!(text.contains("anisotropic = 16"));
        assert!(text.contains("[network]\nredirect = sdgt.arcadehub.xyz\n"));
        assert!(text.contains("subnet = 192.168.100.0/24"));
        assert!(text.contains("simulateNetwork = false"));
        assert!(text.contains("log_level = 1"));
        assert!(text.contains("region = 2"));
        assert!(text.contains("dipsw = 0x3A"), "CHN 应派生写 dipsw=0x3A");
        assert!(text.contains("game_id = SBTS"));
        assert!(text.contains("keychip_serial = A69E-01A99999999"));
        assert!(text.contains("skipInitCard = true"));
        assert!(text.contains("[Coins]"));
        assert!(text.contains("freeplay = false"));
        assert!(text.contains("start = 2"));
        assert!(text.contains("continues = 2"));
        assert!(text.contains("card = 3"));
        assert!(text.contains("car = 1"));
        assert!(text.contains("mode = 2"));
        assert!(text.contains("restrict_ = 128"));
        // DirectInput 反转轴默认未勾选 → false
        assert!(text.contains("[dinput]"), "dinput 模式应写 [dinput] 段");
        assert!(text.contains("reverseAccelAxis = false"));
        assert!(text.contains("reverseBrakeAxis = false"));
        assert!(!text.contains(';'), "生成文件不得含注释");
    }

    #[test]
    fn render_dinput_reverse_axes_true() {
        let mut dep = snapshot();
        dep.input.kind = Some("dinput".into());
        dep.input.wheel_device = Some("Thrustmaster Wheel".into());
        dep.input.throttle_axis = Some("Y".into());
        dep.input.brake_axis = Some("RZ".into());
        dep.input.reverse_accel_axis = Some(true);
        dep.input.reverse_brake_axis = Some(true);

        let text = render(&dep);

        assert!(text.contains("[dinput]"));
        assert!(text.contains("deviceName = Thrustmaster Wheel"));
        assert!(text.contains("accelAxis = Y"));
        assert!(text.contains("brakeAxis = RZ"));
        assert!(text.contains("reverseAccelAxis = true"));
        assert!(text.contains("reverseBrakeAxis = true"));
    }

    #[test]
    fn render_ffb_section() {
        let mut dep = snapshot();
        dep.input.kind = Some("dinput".into());
        // 未设置力反馈 → 全部默认值
        let text = render(&dep);
        assert!(text.contains("[ffb]"), "应写 [ffb] 力反馈段");
        assert!(text.contains("enable = true"));
        assert!(text.contains("constantForceStrength = 100"));
        assert!(text.contains("damperStrength = 100"));
        assert!(text.contains("rumbleStrength = 100"));
        assert!(text.contains("rumbleDuration = 100"));
        assert!(text.contains("baseDamperStrength = 20"));
        assert!(text.contains("deadBand = 20"));

        // 自定义值
        dep.input.ffb_enable = Some(false);
        dep.input.ffb_constant_force = Some(50);
        dep.input.ffb_damper = Some(40);
        dep.input.ffb_rumble = Some(30);
        dep.input.ffb_rumble_duration = Some(60);
        dep.input.ffb_base_damper = Some(10);
        dep.input.ffb_dead_band = Some(5);
        let text = render(&dep);
        assert!(text.contains("enable = false"));
        assert!(text.contains("constantForceStrength = 50"));
        assert!(text.contains("damperStrength = 40"));
        assert!(text.contains("rumbleStrength = 30"));
        assert!(text.contains("rumbleDuration = 60"));
        assert!(text.contains("baseDamperStrength = 10"));
        assert!(text.contains("deadBand = 5"));
    }

    #[test]
    fn render_no_chn_no_dipsw() {
        let mut dep = snapshot();
        dep.system.region = Some("JPN".into());
        dep.system.loglevel = Some("info".into());
        let text = render(&dep);
        assert!(text.contains("region = 0"));
        assert!(text.contains("log_level = 3"));
        assert!(!text.contains("dipsw"), "非 CHN 不写 dipsw");
    }

    #[test]
    fn render_display_default_true_omits_section() {
        let mut dep = snapshot();
        dep.graphics.default = Some(true);
        dep.graphics.resolution = Some("1920x1080".into());
        dep.graphics.render_scale = Some(true);
        dep.graphics.render_scale_level = Some("2x".into());
        let text = render(&dep);
        assert!(!text.contains("[display]"));
        assert!(!text.contains("width"));
    }

    #[test]
    fn render_xinput_mode_and_unset_ratio() {
        let mut dep = snapshot();
        dep.input.kind = Some("xinput".into());
        dep.input.ratio = None;
        let text = render(&dep);
        assert!(text.contains("mode = 1"));
        // 未设置比率也始终写 restrict_，取默认 128
        assert!(text.contains("restrict_ = 128"), "未设置比率应写默认 128");
    }

    #[test]
    fn render_xinput_button_mappings() {
        // 未捕获任何按钮名 → 不写 start/viewChg/... 等键（交 kitten 默认兜底）
        let mut dep = snapshot();
        dep.input.kind = Some("xinput".into());
        let text = render(&dep);
        assert!(!text.contains("start ="));
        assert!(!text.contains("viewChg ="));
        assert!(!text.contains("coin ="));
        assert!(!text.contains("accel ="));
        assert!(!text.contains("brake ="));

        // 捕获若干按钮名 → 写出对应键；空白视为未设置；kitten 无 card 键不写
        let mut dep2 = snapshot();
        dep2.input.kind = Some("xinput".into());
        dep2.input.xi_start = Some("A".into());
        dep2.input.xi_coin = Some("Y".into());
        dep2.input.xi_up = Some(" ".into()); // 空白视为未设置
        dep2.input.xi_card = Some("START".into());
        dep2.input.xi_accel = Some("RT".into());
        dep2.input.xi_brake = Some("LT".into());
        let text2 = render(&dep2);
        assert!(text2.contains("start = A"));
        assert!(text2.contains("coin = Y"));
        assert!(text2.contains("accel = RT"), "油门源应写捕获的按钮名");
        assert!(text2.contains("brake = LT"), "刹车源应写捕获的按钮名");
        assert!(!text2.contains("viewChg ="), "未设置不写");
        assert!(!text2.contains("up ="), "空白值不写出");
        assert!(!text2.contains("card ="), "kitten [xinput] 无 card 键");
        assert!(!text2.contains("shiftDn ="));
        assert!(!text2.contains("down ="));
    }

    #[test]
    fn render_region_exp_equal_two() {
        let mut dep = snapshot();
        dep.system.region = Some("EXP".into());
        let text = render(&dep);
        assert!(text.contains("region = 2"));
    }

    #[test]
    fn deploy_copies_tools_and_writes_config() {
        let tmp = TempDir::new("deploy");
        let tool = tmp.0.join("tool");
        let game = tmp.0.join("game");
        fs::create_dir_all(&tool).unwrap();
        fs::create_dir_all(&game).unwrap();

        // 工具目录：kitten.exe + 依赖 DLL + 应被排除的 config.ini / *.log / sram.bin
        fs::write(tool.join("kitten.exe"), "kitten-bin").unwrap();
        fs::write(tool.join("mice.dll"), "dll-bin").unwrap();
        fs::write(tool.join("config.ini"), "[sample]\nkey = value\n").unwrap();
        fs::write(tool.join("kitten.log"), "log").unwrap();
        fs::write(tool.join("sram.bin"), "sram").unwrap();
        // 游戏目录的 ELF（作为 record.path 指向的文件）
        fs::write(game.join("id5.elf"), "elf").unwrap();

        let mut dep = snapshot();
        dep.system.region = Some("CHN".into());
        dep.game_id = Some("SBTS".into());

        let exe = deploy(&record_at(&game), &tool, &dep).unwrap();
        assert_eq!(exe, game.join("kitten.exe"));
        assert!(exe.is_file());

        // 顶层复制：kitten.exe、DLL 在，排除的文件不进来
        assert_eq!(fs::read(game.join("kitten.exe")).unwrap(), b"kitten-bin");
        assert_eq!(fs::read(game.join("mice.dll")).unwrap(), b"dll-bin");
        assert!(!game.join("config.ini").exists(), "不应复制工具自带 config.ini");
        assert!(!game.join("kitten.log").exists(), "不应复制 *.log");
        assert!(!game.join("sram.bin").exists(), "不应复制 sram.bin");

        // config.ini 写入 <游戏目录>\kitten\
        let cfg = fs::read_to_string(game.join("kitten/config.ini")).unwrap();
        assert_eq!(cfg, render(&dep));
        assert!(cfg.contains("region = 2"));
        assert!(cfg.contains("dipsw = 0x3A"));

        // 同时生成 mice 形态 config.ini 到 <游戏目录>\mice\
        let mice_cfg = fs::read_to_string(game.join("mice/config.ini")).unwrap();
        assert!(mice_cfg.contains("naominet_jp = sdgt.arcadehub.xyz"));
        assert!(mice_cfg.contains("apply_patches = false"));

        // 幂等：再次部署不报错（未变化的文件跳过）
        deploy(&record_at(&game), &tool, &dep).unwrap();
        assert_eq!(
            fs::read(game.join("kitten.exe")).unwrap(),
            b"kitten-bin"
        );
    }

    #[test]
    fn deploy_missing_game_file_errors() {
        let tmp = TempDir::new("missing");
        let tool = tmp.0.join("tool");
        let game = tmp.0.join("game");
        fs::create_dir_all(&tool).unwrap();
        fs::create_dir_all(&game).unwrap();
        fs::write(tool.join("kitten.exe"), "x").unwrap();

        // record.path 指向不存在的文件 → 应报错
        let rec = record_at(&game);
        let err = deploy(&rec, &tool, &snapshot()).unwrap_err();
        assert!(err.contains("游戏文件不存在"), "{err}");
    }

    #[test]
    fn render_redirect_uses_server_setting() {
        let mut snapshot = snapshot();
        snapshot.net.server = Some(" my.server.net ".into());
        let text = render(&snapshot);
        assert!(text.contains("redirect = my.server.net"));
        assert!(!text.contains("sdgt.arcadehub.xyz"), "自定义服务器时不再写默认域名");
    }

    #[test]
    fn render_serverbox_bind_ip() {
        // 未配置 serverbox_ip：不写 bind_ip，也不出现 [serverbox] 节
        let text = render(&snapshot());
        assert!(!text.contains("bind_ip"));
        assert!(!text.contains("[serverbox]"));

        // 模拟网络未关闭（None / Some(true)）：不写 bind_ip
        let mut dep = snapshot();
        dep.net.serverbox_ip = Some("192.168.100.101".into());
        assert!(!render(&dep).contains("bind_ip = 192.168.100.101"));
        dep.net.simulate = Some(true);
        assert!(!render(&dep).contains("bind_ip = 192.168.100.101"));

        // 配置后写入 [network] 节的 bind_ip（trim 前后空白）；仅关闭模拟网络时生效
        let mut dep = snapshot();
        dep.net.simulate = Some(false);
        dep.net.serverbox_ip = Some("  192.168.100.101  ".into());
        let text = render(&dep);
        assert!(
            text.contains("bind_ip = 192.168.100.101"),
            "应写出 [network] bind_ip：\n{text}"
        );
        assert!(
            text.contains("[network]")
                && text.find("bind_ip = 192.168.100.101").unwrap() > text.find("[network]").unwrap(),
            "bind_ip 必须位于 [network] 节内：\n{text}"
        );
        assert!(!text.contains("[serverbox]"), "不应出现独立的 [serverbox] 节");
    }

    #[test]
    fn render_host_game_ip() {
        // 未配置 host_ip：不写 game_ip
        let text = render(&snapshot());
        assert!(!text.contains("game_ip"));

        // 模拟网络开启（None / Some(true)）：即便配置了 host_ip 也不写 game_ip
        let mut dep = snapshot();
        dep.net.host_ip = Some("  192.168.100.50  ".into());
        assert!(
            !render(&dep).contains("game_ip"),
            "模拟网络开启时不应写 game_ip"
        );
        dep.net.simulate = Some(true);
        assert!(
            !render(&dep).contains("game_ip"),
            "模拟网络开启时不应写 game_ip"
        );

        // 关闭模拟网络后写入 [network] 节的 game_ip（trim 前后空白）
        dep.net.simulate = Some(false);
        let text = render(&dep);
        assert!(
            text.contains("game_ip = 192.168.100.50"),
            "应写出 [network] game_ip：\n{text}"
        );
        assert!(
            text.find("game_ip = 192.168.100.50").unwrap() > text.find("[network]").unwrap(),
            "game_ip 必须位于 [network] 节内：\n{text}"
        );
    }
}
