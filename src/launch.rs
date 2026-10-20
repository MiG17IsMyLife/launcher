//! 加载器定位与游戏启动：
//! - 约定：用户将 kitten.exe / mice.exe / mice64.exe 放入启动器 exe 的子目录
//!   （如 launcher\tools\kitten\kitten.exe、launcher\tools\mice\mice.exe），启动时自动扫描
//! - 启动：kitten -> 先部署工具与 config.ini 到游戏目录，再 `kitten.exe -b <游戏文件名>`
//!          （cwd=游戏目录，与游戏目录内的 start_game.bat 一致；只传文件名，
//!           绝对路径里的空格会被拆开导致 "Failed to parse ELF file"；
//!           CREATE_NO_WINDOW 隐藏 kitten 的 CMD 窗口）
//!          mice   -> 先部署（复制 tools\mice 顶层文件到游戏目录 + 生成
//!                    <游戏目录>\mice\config.ini（ip=网段基址+102）与 idas.ini，
//!                    见 mice.rs），再 `mice.exe -b <游戏.exe>`（cwd=游戏目录）；
//!                    存在 mice64.exe 且游戏为 x64 时优先 mice64
//! - 测试模式：游戏行勾选「测试模式」时，命令行末尾追加 `-t`（`kitten -b id5.elf -t`）；
//!             仅作用于游戏本体，ServerBox 不带该参数
//! - ServerBox：模拟网络（默认开启）且该游戏配置了 ServerBox 时，`launch_serverbox`
//!              走与游戏同一套部署流程，差异仅在 ip 偏移（kitten 同游戏；mice=网段基址+101），
//!              以 `-b <ServerBox exe>` 拉起；CMD 窗口隐藏；游戏退出后关闭
//! - 拉起顺序（mice / RingEdge）：**ServerBox 先启动** → 延迟 10 秒 → 读卡器 + 游戏
//!              （游戏 `mice -b <游戏exe>`——不带读卡器专用的 `-p`、
//!               读卡器 `mice -p -b picodaemon.exe`、窗口均保留）；
//!              游戏退出后关闭读卡器与 ServerBox；kitten 维持 ServerBox → 读卡器 → 游戏
//! - 读卡器：配置了 picodaemon 时，`launch_reader` —— 把 mice 工具目录的 `mice.exe`、
//!           `mice.dll` 复制到 picodaemon 所在目录，写 `<该目录>\mice\idas.ini`
//!           （`[CardReader] cardfile = <当前选中卡片>`；IDAS4~5 另写 `[io3] mode`
//!           与捕获的插卡键 `[xinput] card`），再 `mice -p -b picodaemon.exe`
//!           （cwd=该目录，**窗口保留**）；游戏退出后关闭
//! - initialD.ini：D4-D7 启动前，先 `initiald::patch_game_ini` 按私服规则改写游戏目录的
//!           `initialD.ini`（字节级，保留 Shift-JIS / 注释 / CRLF），再启动游戏

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db;
use crate::detect;
use crate::kitten;
use crate::mice;
use crate::segatools;
use crate::storage::{GameRecord, Loader, Settings, launcher_dir};
use crate::t;
use crate::tf;

#[derive(Clone, Debug, Default)]
pub struct LoaderPaths {
    pub kitten: Option<PathBuf>,
    pub mice: Option<PathBuf>,
    pub mice64: Option<PathBuf>,
    /// segatools 工具目录内的 inject.exe（IDAC 等游戏）
    pub inject: Option<PathBuf>,
}

const LOADER_NAMES: &[&str] = &["kitten.exe", "mice.exe", "mice64.exe", "inject.exe"];

/// 统一部署快照：按加载器区分（点击「启动」时由 UI 克隆全局设置构建）。
pub enum DeployConfig {
    Kitten(kitten::DeploySnapshot),
    Mice(mice::DeploySnapshot),
    Segatools(segatools::DeploySnapshot),
}

impl DeployConfig {
    /// 快照与加载器类型是否匹配（不匹配属于内部错误）。
    fn mismatch_error() -> String {
        t!("游戏启动缺少配置快照或快照与加载器不匹配（内部错误）",
           "Game launch is missing the config snapshot or the snapshot does not match the loader (internal error)").to_string()
    }

    /// 取出 keychip 序列号（已 trim，空白视为未设置）。kitten / mice / segatools 快照字段一致。
    pub fn keychip(&self) -> Option<&str> {
        let k = match self {
            DeployConfig::Kitten(s) => &s.system.keychip,
            DeployConfig::Mice(s) => &s.system.keychip,
            DeployConfig::Segatools(s) => &s.system.keychip,
        };
        k.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// 网络设置「服务器地址」（未设置回退默认）。kitten / mice / segatools 快照字段一致。
    pub fn server_host(&self) -> &str {
        let net = match self {
            DeployConfig::Kitten(s) => &s.net,
            DeployConfig::Mice(s) => &s.net,
            DeployConfig::Segatools(s) => &s.net,
        };
        net.server_host()
    }
}

/// 扫描启动器目录（两层）查找加载器，按文件名（忽略大小写）匹配。
pub fn detect_loaders() -> LoaderPaths {
    let mut found = LoaderPaths::default();
    let root = launcher_dir();
    scan_dir(&root, 0, &mut found);
    found
}

fn scan_dir(dir: &Path, depth: u32, found: &mut LoaderPaths) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            scan_dir(&path, depth + 1, found);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let lower = name.to_lowercase();
        for cand in LOADER_NAMES {
            if lower != *cand {
                continue;
            }
            let slot = match *cand {
                "kitten.exe" => &mut found.kitten,
                "mice.exe" => &mut found.mice,
                "inject.exe" => &mut found.inject,
                _ => &mut found.mice64,
            };
            if slot.is_none() {
                *slot = Some(path.clone());
            }
        }
    }
}

/// 解析某游戏应使用的加载器路径。
pub fn resolve_loader(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
) -> Result<PathBuf, String> {
    let pick = |custom: &Option<String>, auto: &Option<PathBuf>| -> Option<PathBuf> {
        custom.as_ref().map(PathBuf::from).or_else(|| auto.clone())
    };
    let exists = |p: &Path| p.is_file();

    match record.loader {
        Loader::Kitten => {
            let p = pick(&settings.kitten_path, &detected.kitten)
                .ok_or_else(|| t!("未找到 kitten.exe，请将加载器放入启动器子目录（如 launcher\\tools\\kitten\\kitten.exe），或在设置中指定路径", "kitten.exe not found. Put the loader under the launcher subdirectory (e.g. launcher\\tools\\kitten\\kitten.exe), or specify its path in settings").to_string())?;
            if !exists(&p) {
                return Err(tf!("kitten 加载器不存在：{}", "kitten loader does not exist: {}", p.display()));
            }
            Ok(p)
        }
        Loader::Mice => {
            let is_x64 = record.arch.as_deref() == Some("x64");
            let preferred = if is_x64 {
                pick(&settings.mice64_path, &detected.mice64)
                    .or_else(|| pick(&settings.mice_path, &detected.mice))
            } else {
                pick(&settings.mice_path, &detected.mice)
                    .or_else(|| pick(&settings.mice64_path, &detected.mice64))
            };
            let p = preferred.ok_or_else(|| t!("未找到 mice.exe/mice64.exe，请将加载器放入启动器子目录（如 launcher\\mice\\mice.exe），或在设置中指定路径", "mice.exe/mice64.exe not found. Put the loader under the launcher subdirectory (e.g. launcher\\mice\\mice.exe), or specify its path in settings").to_string())?;
            if !exists(&p) {
                return Err(tf!("mice 加载器不存在：{}", "mice loader does not exist: {}", p.display()));
            }
            Ok(p)
        }
        Loader::Segatools => {
            let p = pick(&settings.segatools_path, &detected.inject)
                .ok_or_else(|| t!("未找到 inject.exe，请将 segatools 工具放入启动器子目录（如 launcher\\tools\\segatools\\inject.exe），或在设置中指定路径", "inject.exe not found. Put the segatools tool under the launcher subdirectory (e.g. launcher\\tools\\segatools\\inject.exe), or specify its path in settings").to_string())?;
            if !exists(&p) {
                return Err(tf!("inject.exe 不存在：{}", "inject.exe does not exist: {}", p.display()));
            }
            Ok(p)
        }
    }
}

/// 只做启动前的准备，**不拉起游戏**：
/// - kitten：resolve 源 kitten.exe → 以「源 exe 所在目录」为工具目录执行
///   `kitten::deploy`（复制工具顶层文件到游戏目录 + 生成 `<游戏目录>\kitten\config.ini`），
///   返回部署后游戏目录内的 kitten.exe 路径；
/// - mice：以「mice.exe 所在目录」为工具目录执行 `mice::deploy`
///   （复制工具顶层文件到游戏目录 + 生成 `<游戏目录>\mice\config.ini` 与 `idas.ini`），
///   返回加载器路径（游戏以完整路径拉起，无需部署后复制）。
///
/// `snapshot` 仅对应 loader 分支需要（None / 类型不匹配会报错）。
pub fn deploy_only(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<PathBuf, String> {
    let loader = resolve_loader(record, settings, detected)?;
    match record.loader {
        Loader::Kitten => {
            let Some(DeployConfig::Kitten(dep)) = dep else {
                return Err(DeployConfig::mismatch_error());
            };
            let tool_dir = loader.parent().ok_or_else(|| {
                tf!("无法确定 kitten 工具目录：{}", "Cannot determine the kitten tool directory: {}", loader.display())
            })?;
            kitten::deploy(record, tool_dir, dep)
        }
        Loader::Mice => {
            let Some(DeployConfig::Mice(dep)) = dep else {
                return Err(DeployConfig::mismatch_error());
            };
            let tool_dir = loader.parent().ok_or_else(|| {
                tf!("无法确定 mice 工具目录：{}", "Cannot determine the mice tool directory: {}", loader.display())
            })?;
            let game_dir = game_dir_of(record);
            mice::deploy(&game_dir, tool_dir, dep, mice::MiceTarget::Game)?;
            Ok(loader)
        }
        Loader::Segatools => {
            let Some(DeployConfig::Segatools(dep)) = dep else {
                return Err(DeployConfig::mismatch_error());
            };
            // 部署目标 = amdaemon.exe 所在目录（segatools.ini / config_hook.json / inject 需与之一同）
            let amdaemon = record
                .amdaemon
                .as_deref()
                .map(PathBuf::from)
                .ok_or_else(|| t!("该游戏未设置 amdaemon.exe，请在游戏行的下拉配置中选择", "This game has no amdaemon.exe configured; select it in the game row's dropdown").to_string())?;
            if !amdaemon.is_file() {
                return Err(tf!("amdaemon.exe 不存在：{}", "amdaemon.exe does not exist: {}", amdaemon.display()));
            }
            let game_dir = amdaemon
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| tf!("无法确定 amdaemon 目录：{}", "Cannot determine the amdaemon directory: {}", amdaemon.display()))?;
            segatools::deploy(&game_dir, &loader.parent().unwrap_or(Path::new(".")), dep)?;
            // 与 launch.bat 一致：执行 amdaemon 目录内部署好的 inject.exe（与游戏包
            // hook dll 同目录、同版本配套），而非 tools 工具目录里的 inject.exe
            let local_inject = game_dir.join("inject.exe");
            if !local_inject.is_file() {
                return Err(tf!(
                    "部署后未在游戏目录找到 inject.exe（{}），请检查 segatools 工具目录是否完整",
                    "inject.exe not found in the game directory after deployment ({}); check that the segatools tool directory is complete",
                    local_inject.display()
                ));
            }
            Ok(local_inject)
        }
    }
}

/// 取游戏记录的所在目录（record.path 的父目录；异常时退回当前目录）。
fn game_dir_of(record: &GameRecord) -> PathBuf {
    Path::new(&record.path)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 为选中网卡开启 DHCP 静态 IP 共存并添加静态 IP（启动游戏前，serverbox_ip 已配置时调用）。
///
/// 分两步执行 `netsh`：
/// 1. `interface ipv4 set interface interface="<网卡名>" dhcpstaticipcoexistence=enabled`
///    —— 允许在 DHCP 网卡上追加静态 IP（否则第二步会失败）；
/// 2. `interface ipv4 add address "<网卡名>" <ip> <mask>`
///    —— 实际添加静态 IP；IP 已存在（"already exists"）视为成功，保证幂等。
///
/// `friendly` 为网卡连接名（如「以太网」），`mask` 为点分十进制子网掩码
/// （随选中网段前缀推导，缺省 255.255.255.0）。
/// 启动器以管理员身份运行，后台线程继承提权令牌，无需再次提权。
pub fn add_serverbox_static_ip(friendly: &str, ip: &str, mask: &str) -> Result<(), String> {
    let set_if = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "set",
            "interface",
            &format!("interface=\"{friendly}\""),
            "dhcpstaticipcoexistence=enabled",
        ])
        .output()
        .map_err(|e| tf!("执行 netsh 失败：{}", "Failed to run netsh: {}", e))?;
    if !set_if.status.success() {
        return Err(tf!(
            "开启网卡 DHCP 静态共存失败：{}",
            "Failed to enable DHCP static IP coexistence: {}",
            String::from_utf8_lossy(&set_if.stderr)
        )
        .trim()
        .to_string());
    }

    let add = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "add",
            "address",
            &format!("\"{friendly}\""),
            ip,
            mask,
        ])
        .output()
        .map_err(|e| tf!("执行 netsh 失败：{}", "Failed to run netsh: {}", e))?;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    if add.status.success()
        || combined.contains("already exists")
        || combined.contains("已存在")
    {
        Ok(())
    } else {
        Err(tf!(
            "为网卡添加静态 IP 失败：{}",
            "Failed to add static IP to adapter: {}",
            combined.trim()
        )
        .trim()
        .to_string())
    }
}

/// 删除之前通过 [`add_serverbox_static_ip`] 添加的静态 IP（游戏退出后回收）。
/// 地址本就不在网卡上（已被手动删除等）视为成功，保证幂等；调用方对错误仅提示。
pub fn remove_serverbox_static_ip(friendly: &str, ip: &str) -> Result<(), String> {
    let del = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "delete",
            "address",
            &format!("\"{friendly}\""),
            ip,
        ])
        .output()
        .map_err(|e| tf!("执行 netsh 失败：{}", "Failed to run netsh: {}", e))?;
    if del.status.success() {
        return Ok(());
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&del.stdout),
        String::from_utf8_lossy(&del.stderr)
    );
    if combined.contains("not found")
        || combined.contains("找不到")
        || combined.contains("不存在")
    {
        return Ok(());
    }
    Err(tf!(
        "从网卡删除静态 IP 失败：{}",
        "Failed to remove static IP from adapter: {}",
        combined.trim()
    )
    .trim()
    .to_string())
}

/// 启动该游戏配置的 **ServerBox**（模拟网络时本机跑的服务端）：
/// **与启动游戏同一套部署流程**，差异仅在部署目标的 ip 偏移：
/// - kitten：先把工具（含依赖 dll）与 config.ini 部署到 ServerBox 目录，
///   再 `kitten.exe -b <文件名>`（cwd = ServerBox 目录）拉起；
/// - mice：先把工具与 mice\config.ini（ip = 网段基址+101）/ idas.ini 部署到
///   ServerBox 目录，再 `mice.exe -b <ServerBox exe>`（cwd = ServerBox 目录）拉起；
///   CMD 窗口隐藏（CREATE_NO_WINDOW）。
/// 仅部署 ServerBox（复制工具 + 生成 config.ini / idas.ini），**不拉起进程**。
/// 供调试用：单独验证文件拷贝 / 配置生成是否正确。
pub fn deploy_serverbox(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<(), String> {
    let sb_path = record
        .serverbox
        .as_deref()
        .ok_or_else(|| t!("该游戏未配置 ServerBox", "This game has no ServerBox configured").to_string())?;
    if !Path::new(sb_path).is_file() {
        return Err(tf!("ServerBox 文件不存在：{sb_path}", "ServerBox file does not exist: {sb_path}"));
    }
    // 以 ServerBox 路径构造一份临时记录，部署目录 / cwd 都由它推导，与游戏一致
    let mut sb_rec = record.clone();
    sb_rec.path = sb_path.to_string();
    // 测试模式只作用于游戏本体，ServerBox 不带 -t
    sb_rec.test_mode = None;
    match sb_rec.loader {
        Loader::Mice => {
            let Some(DeployConfig::Mice(snapshot)) = dep else {
                return Err(DeployConfig::mismatch_error());
            };
            let loader = resolve_loader(&sb_rec, settings, detected)?;
            let tool_dir = loader.parent().ok_or_else(|| {
                tf!("无法确定 mice 工具目录：{}", "Cannot determine the mice tool directory: {}", loader.display())
            })?;
            let sb_dir = Path::new(sb_path)
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            mice::deploy(&sb_dir, tool_dir, snapshot, mice::MiceTarget::ServerBox)?;
            // IDAS6~8：图形补丁同样部署到 ServerBox 目录
            deploy_gfx_patch(&sb_dir, sb_rec.gfx_patch.unwrap_or(true))
        }
        Loader::Kitten => deploy_only(&sb_rec, settings, detected, dep).map(|_| ()),
        Loader::Segatools => Err(t!("segatools 加载器暂不支持 ServerBox",
            "The segatools loader does not support ServerBox yet").to_string()),
    }
}

/// 启动该游戏配置的 **ServerBox**（模拟网络时本机跑的服务端）：
/// 先部署（复制工具 + 生成配置），再以 `-b <ServerBox exe>` 拉起。
pub fn launch_serverbox(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<std::process::Child, String> {
    deploy_serverbox(record, settings, detected, dep)?;
    let sb_path = record
        .serverbox
        .as_deref()
        .ok_or_else(|| t!("该游戏未配置 ServerBox", "This game has no ServerBox configured").to_string())?;
    let mut sb_rec = record.clone();
    sb_rec.path = sb_path.to_string();
    sb_rec.test_mode = None;
    match sb_rec.loader {
        Loader::Mice => {
            let loader = resolve_loader(&sb_rec, settings, detected)?;
            spawn_game(&sb_rec, &loader)
        }
        Loader::Kitten => {
            let exe = deploy_only(&sb_rec, settings, detected, dep)?;
            spawn_game(&sb_rec, &exe)
        }
        Loader::Segatools => Err(t!("segatools 加载器暂不支持 ServerBox",
            "The segatools loader does not support ServerBox yet").to_string()),
    }
    .map_err(|e| format!("ServerBox {e}"))
}

/// 读卡器部署 + 启动（picodaemon）：与游戏启动前同思路，先把 mice 工具放进读卡器目录。
///
/// 1) 把 mice 工具目录的 `mice.exe`、`mice.dll` 复制到 **picodaemon.exe 所在目录**；
/// 2) 在该目录的 `mice\` 子目录写入 `idas.ini`，内容为当前选中卡片，及（调用方传入时）
///    输入后端与插卡键映射：
///    ```ini
///    [CardReader]
///    cardfile = 01010409725450755799
///
///    [io3]
///    mode = xinput
///
///    [xinput]
///    card = START
///    ```
///    （未选择卡片时不写该文件，交给 mice 自身默认；`io3_mode` / `card_button` 仅
///    IDAS4~5（kitten）传入——kitten 游戏进程不处理插卡键，输入后端与插卡按钮由
///    读卡器侧 mice 读取：mode 按启动器输入类型（"xinput"/"dinput"），card 为捕获的按钮名）
/// 3) 以 `<读卡器目录>\mice.exe -p -b picodaemon.exe` 启动，cwd = 读卡器目录，
///    **不隐藏窗口**（需要看到读卡器输出）。
/// 仅部署读卡器（复制 mice.exe / mice.dll + 写 idas.ini），**不拉起进程**。
/// 供调试用：单独验证文件拷贝 / 配置生成是否正确。
pub fn deploy_reader(
    reader_path: &str,
    settings: &Settings,
    detected: &LoaderPaths,
    card: Option<&str>,
    game_ip: Option<&str>,
    card_button: Option<&str>,
    io3_mode: Option<&str>,
) -> Result<PathBuf, String> {
    let reader = Path::new(reader_path);
    if !reader.is_file() {
        return Err(tf!("读卡器文件不存在：{reader_path}", "Card reader file does not exist: {reader_path}"));
    }
    let reader_dir = reader
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(launcher_dir);

    // mice.exe：设置里的路径优先，其次自动扫描到的
    let mice = settings
        .mice_path
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| detected.mice.clone())
        .ok_or_else(|| {
            t!("未找到 mice.exe，请将 mice 工具放入启动器子目录（如 launcher\\tools\\mice\\mice.exe），或在设置中指定路径", "mice.exe not found. Put the mice tool under the launcher subdirectory (e.g. launcher\\tools\\mice\\mice.exe), or specify its path in settings").to_string()
        })?;
    if !mice.is_file() {
        return Err(tf!("mice 加载器不存在：{}", "mice loader does not exist: {}", mice.display()));
    }
    let tool_dir = mice
        .parent()
        .ok_or_else(|| tf!("无法确定 mice 工具目录：{}", "Cannot determine the mice tool directory: {}", mice.display()))?;

    // 1) mice.exe / mice.dll 复制到读卡器目录
    for name in ["mice.exe", "mice.dll"] {
        let src = tool_dir.join(name);
        if !src.is_file() {
            continue;
        }
        copy_file_if_changed(&src, &reader_dir.join(name))?;
    }

    // 2) <读卡器目录>\mice\idas.ini：[CardReader] cardfile + [ServerBox] game_ip
    //    （picodaemon 由 mice 载入，与游戏侧 idas.ini 同键名读主机 IP）+
    //    [io3] mode / [xinput] card（IDAS4~5：输入后端与插卡键由读卡器侧 mice 读取，
    //    mode 按启动器输入类型，card 写捕获的按钮名）
    let mut ini_text = String::new();
    if let Some(card) = card {
        ini_text.push_str(&format!("[CardReader]\ncardfile = {card}\n"));
    }
    if let Some(ip) = game_ip.map(str::trim).filter(|s| !s.is_empty()) {
        ini_text.push_str(&format!("[ServerBox]\ngame_ip = {ip}\n"));
    }
    if let Some(mode) = io3_mode.map(str::trim).filter(|s| !s.is_empty()) {
        ini_text.push_str(&format!("[io3]\nmode = {mode}\n"));
    }
    if let Some(btn) = card_button.map(str::trim).filter(|s| !s.is_empty()) {
        ini_text.push_str(&format!("[xinput]\ncard = {btn}\n"));
    }
    if !ini_text.is_empty() {
        let mice_dir = reader_dir.join("mice");
        fs::create_dir_all(&mice_dir).map_err(|e| {
            tf!("创建读卡器配置目录失败（{}）：{e}", "Failed to create card reader config directory ({}): {e}", mice_dir.display())
        })?;
        let ini = mice_dir.join("idas.ini");
        fs::write(&ini, ini_text)
            .map_err(|e| tf!("写入 idas.ini 失败（{}）：{e}", "Failed to write idas.ini ({}): {e}", ini.display()))?;
    }
    Ok(mice)
}

/// 拉起读卡器（**须先完成 [`deploy_reader]**）：以 `<读卡器目录>\mice.exe -p -b picodaemon.exe`
/// 启动，cwd = 读卡器目录，**不隐藏窗口**（需要看到读卡器输出）。
pub fn spawn_reader(reader_path: &str, mice_exe: &Path) -> Result<std::process::Child, String> {
    let reader = Path::new(reader_path);
    let reader_dir = reader
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(launcher_dir);
    let reader_file = reader
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(reader_path));

    let mut cmd = Command::new(mice_exe);
    cmd.current_dir(&reader_dir);
    cmd.arg("-p").arg("-b").arg(&reader_file);
    cmd.spawn()
        .map_err(|e| tf!("启动读卡器失败（{}）：{e}", "Failed to start card reader ({}): {e}", mice_exe.display()))
}

/// 自动解析游戏目录的读卡器（picodaemon.exe）：游戏目录已存在已知 picodaemon
/// （CRC 命中识别库 card_reader）或与依赖包完全一致时直接复用；否则从
/// `tools/dependency/picodaemon.exe` 拷贝（覆盖）到游戏目录后返回该路径。
/// 依赖缺失时返回明确错误，由调用方阻断启动。
pub fn ensure_picodaemon(game_dir: &Path) -> Result<PathBuf, String> {
    let dep = launcher_dir().join("tools").join("dependency");
    ensure_picodaemon_from(game_dir, &dep)
}

/// 仅 IDAS4~5（kitten）启动前：把依赖的 `cg.dll` / `cgGL.dll` 从 `tools/dependency/` 拷入
/// 游戏目录（覆盖）。依赖缺失时返回明确错误，由调用方阻断启动。
pub fn ensure_cg_libs(game_dir: &Path) -> Result<(), String> {
    let dep = launcher_dir().join("tools").join("dependency");
    ensure_cg_libs_from(game_dir, &dep)
}

/// `ensure_picodaemon` 的可测试内核：`dep` 为依赖目录（默认 `launcher_dir/tools/dependency`）。
fn ensure_picodaemon_from(game_dir: &Path, dep: &Path) -> Result<PathBuf, String> {
    let candidate = game_dir.join("picodaemon.exe");
    let src = dep.join("picodaemon.exe");
    // 已存在：已知 picodaemon（CRC 命中识别库）→ 直接复用，不拷贝
    if candidate.is_file() && detect::probe_card_reader(game_dir).is_some() {
        return Ok(candidate);
    }
    // 已存在但与依赖包完全一致（上一轮已拷入的已知好副本）→ 复用，避免每轮重拷
    if candidate.is_file() && src.is_file() && files_equal(&candidate, &src) {
        return Ok(candidate);
    }
    // 否则从依赖包拷贝（覆盖）
    if !src.is_file() {
        return Err(tf!(
            "未找到 picodaemon.exe：{}\n（请将 picodaemon.exe 放入 tools/dependency/，随启动器打包附上）",
            "picodaemon.exe not found: {}\n(Put a picodaemon.exe under tools/dependency/ and ship it with the launcher)",
            src.display()
        ));
    }
    copy_file_if_changed(&src, &candidate)?;
    Ok(candidate)
}

/// `ensure_cg_libs` 的可测试内核：`dep` 为依赖目录（默认 `launcher_dir/tools/dependency`）。
fn ensure_cg_libs_from(game_dir: &Path, dep: &Path) -> Result<(), String> {
    for name in ["cg.dll", "cgGL.dll"] {
        let src = dep.join(name);
        if !src.is_file() {
            return Err(tf!(
                "未找到 {}：{}\n（请将 {} 放入 tools/dependency/，随启动器打包附上）",
                "{} not found: {}\n(Put {} under tools/dependency/ and ship it with the launcher)",
                name, src.display(), name
            ));
        }
        copy_file_if_changed(&src, &game_dir.join(name))?;
    }
    Ok(())
}

/// 两文件是否完全一致（大小相同且逐字节相等）；任一读取失败视为不等。
fn files_equal(a: &Path, b: &Path) -> bool {
    let (Ok(ra), Ok(rb)) = (fs::read(a), fs::read(b)) else {
        return false;
    };
    ra == rb
}

/// 复制 src -> dest：大小与修改时间都相同则跳过，避免无谓 IO。
fn copy_file_if_changed(src: &Path, dest: &Path) -> Result<(), String> {
    if let (Ok(a), Ok(b)) = (fs::metadata(src), fs::metadata(dest)) {
        if a.len() == b.len() && a.modified().ok() == b.modified().ok() {
            return Ok(());
        }
    }
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| tf!("复制 {} 失败：{e}", "Failed to copy {}: {e}", src.display()))
}

/// 部署 IDAS6~8 的图形补丁到目标目录（幂等）：
/// - 勾选：`tools/asi-loader/dinput8.dll` → `<dir>\winmm.dll`（改名，DLL 劫持目标），
///   `tools/asi-loader/` 内其余文件 → `<dir>\scripts\`（加载器从 scripts 加载 .asi 插件）；
/// - 未勾选：移除此前部署的 winmm.dll（scripts 内无加载器时插件自然不生效）；
/// - 组件未安装（toolssi-loader\dinput8.dll 缺失）：静默跳过，不阻断启动。
pub fn deploy_gfx_patch(dir: &Path, enabled: bool) -> Result<(), String> {
    let winmm = dir.join("winmm.dll");
    if !enabled {
        if winmm.is_file() {
            let _ = fs::remove_file(&winmm);
        }
        return Ok(());
    }
    let tools = crate::update::tools_dir();

    // 1) ASI Loader：dinput8.dll → winmm.dll（组件未安装则跳过）
    let dll = tools.join("asi-loader").join("dinput8.dll");
    if dll.is_file() {
        copy_file_if_changed(&dll, &winmm)?;
    }

    // 2) shaderfix：tools/shaderfix/ 下所有文件 → scripts\（含子目录内文件，展平）
    let sf_dir = tools.join("shaderfix");
    if sf_dir.is_dir() {
        copy_files_flat(&sf_dir, &dir.join("scripts"))?;
    }
    Ok(())
}

/// 递归收集 `src` 下全部文件并**展平**复制到 `dest`（目录结构不保留，重名覆盖）。
fn copy_files_flat(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| {
        tf!(
            "创建目录失败（{}）：{e}",
            "Failed to create directory ({}): {e}",
            dest.display()
        )
    })?;
    for entry in fs::read_dir(src).map_err(|e| {
        tf!("读取目录失败（{}）：{e}", "Failed to read directory ({}): {e}", src.display())
    })? {
        let e = entry.map_err(|e| {
            tf!("读取目录条目失败（{}）：{e}", "Failed to read directory entry ({}): {e}", src.display())
        })?;
        let ft = e.file_type().map_err(|e| {
            tf!("读取文件类型失败：{e}", "Failed to read file type: {e}")
        })?;
        if ft.is_dir() {
            copy_files_flat(&e.path(), dest)?;
        } else if ft.is_file() {
            copy_file_if_changed(&e.path(), &dest.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// 启动前**只做文件部署**（kitten / mice / segatools 各自的配置与工具拷贝 +
/// D4-D7 的 initialD.ini 改写），不拉起任何进程。
/// 返回加载器 exe 路径（kitten.exe / mice 游戏传入 spawn 的 mice.exe / inject.exe）。
/// 这样 do_launch 可以把**所有**配置文件先写好，再统一拉起读卡器 / amdaemon / 游戏，
/// 避免加载器先启动、配置后落盘的竞态。
pub fn prepare_game(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<PathBuf, String> {
    let exe = deploy_only(record, settings, detected, dep)?;

    // D4-D7：启动前改写游戏目录的 initialD.ini（私服地址 / 端口 / 公告 URL）。
    // 私服地址取网络设置「服务器地址」（未设置回退默认），与 kitten / mice / segatools
    // 生成配置时用的是同一个值。
    // 失败（读写异常 / 非 D4-D7 / 文件缺失）一律不阻断启动，静默跳过。
    let game_dir = Path::new(&record.path)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let host = dep
        .map(|d| d.server_host())
        .unwrap_or(crate::server::SERVER_HOST);
    let _ = initiald_patch(record, &game_dir, host);

    // IDAS6~8（mice）：图形补丁（winmm.dll + scripts\），勾选才部署；
    // 未勾选时 deploy_gfx_patch 移除游戏目录内的 winmm.dll。
    // kitten 不部署该补丁（无 gfx_patch 概念），不要往 kitten 游戏目录拷 ASI Loader。
    if record.loader == Loader::Mice {
        deploy_gfx_patch(&game_dir, record.gfx_patch.unwrap_or(true))?;
    }

    Ok(exe)
}

/// 按加载器拉起游戏主进程（文件须已由 [`prepare_game`] 部署完毕）。
pub fn spawn_prepared_game(
    record: &GameRecord,
    loader_exe: &Path,
    dep: Option<&DeployConfig>,
) -> Result<std::process::Child, String> {
    match record.loader {
        Loader::Segatools => spawn_idac_game(record, loader_exe, dep),
        _ => spawn_game(record, loader_exe),
    }
}

/// segatools 游戏的 amdaemon.exe 路径（已校验存在），返回（路径, 所在目录）。
fn amdaemon_of(record: &GameRecord) -> Result<(PathBuf, PathBuf), String> {
    let amdaemon = record
        .amdaemon
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| t!("该游戏未设置 amdaemon.exe，请在游戏行的下拉配置中选择",
            "This game has no amdaemon.exe configured; select it in the game row's dropdown").to_string())?;
    if !amdaemon.is_file() {
        return Err(tf!("amdaemon.exe 不存在：{}", "amdaemon.exe does not exist: {}", amdaemon.display()));
    }
    let dir = amdaemon
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| tf!("无法确定 amdaemon 目录：{}", "Cannot determine the amdaemon directory: {}", amdaemon.display()))?;
    Ok((amdaemon, dir))
}

/// segatools 游戏的主程序路径（已校验存在）。
fn game_exe_of(record: &GameRecord) -> Result<PathBuf, String> {
    let exe = record
        .game_exe
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| t!("该游戏未设置游戏主程序（GameProject-Win64-Shipping.exe），请在游戏行的下拉配置中选择",
            "This game has no game executable (GameProject-Win64-Shipping.exe) configured; select it in the game row's dropdown").to_string())?;
    if !exe.is_file() {
        return Err(tf!("游戏主程序不存在：{}", "Game executable does not exist: {}", exe.display()));
    }
    Ok(exe)
}

/// 拉起 segatools 游戏的 **amdaemon**（伴随进程，文件须已由 [`prepare_game`] 部署）：
/// 以 `inject -d -k <hook dll> amdaemon.exe -c <配置清单>` 启动（复刻 launch.bat）。
/// cwd = amdaemon 目录，窗口保持可见；游戏退出后由调用方按映像名终止 amdaemon
/// （对应 launch.bat 末尾的 `taskkill /f /im amdaemon.exe`）。
pub fn spawn_amdaemon(
    record: &GameRecord,
    inject: &Path,
    dep: Option<&DeployConfig>,
) -> Result<std::process::Child, String> {
    let Some(DeployConfig::Segatools(snap)) = dep else {
        return Err(DeployConfig::mismatch_error());
    };
    let kind = segatools::game_kind(snap.game_id.as_deref());
    let dir = Path::new(&record.amdaemon.as_deref().unwrap_or_default())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let mut cmd = Command::new(inject);
    cmd.current_dir(&dir);
    cmd.args(segatools::amdaemon_args(kind, snap.game_id.as_deref()));
    // amdaemon 窗口保持可见（同 launch.bat 的 start /min），便于观察运行日志
    cmd.spawn()
        .map_err(|e| tf!("启动 amdaemon 失败（{}）：{e}", "Failed to start amdaemon ({}): {e}", inject.display()))
}

/// 拉起 segatools 游戏主程序：
/// IDAC（SDGT）：`inject -d -k idachook.dll <游戏exe> -culture=<ja|en> launch=Cabinet ...`；
/// IDZ（SDDF / SDER）：`inject -d -k idzhook.dll InitialD0_DX11_Nu.exe -m`。
/// culture 由系统设置 Region 推导（JPN→ja，其余→en，仅 IDAC 使用）。
fn spawn_idac_game(
    record: &GameRecord,
    inject: &Path,
    dep: Option<&DeployConfig>,
) -> Result<std::process::Child, String> {
    let Some(DeployConfig::Segatools(snap)) = dep else {
        return Err(DeployConfig::mismatch_error());
    };
    let kind = segatools::game_kind(snap.game_id.as_deref());
    let (_, dir) = amdaemon_of(record)?;
    let game_exe = game_exe_of(record)?;
    let culture = segatools::culture_of(snap.system.region.as_deref());
    let user_dir = segatools::find_userdata_dir(&dir);

    let mut cmd = Command::new(inject);
    cmd.current_dir(&dir);
    cmd.args(segatools::game_args(kind, &game_exe, culture, &user_dir));
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd.spawn()
        .map_err(|e| tf!("启动游戏失败（{}）：{e}", "Failed to start game ({}): {e}", game_exe.display()))
}

/// 取识别库 game_id 并改写 initialD.ini（仅 D4-D7 生效）。
///
/// game_id 优先取 CRC 命中识别库的结果，未命中时回退记录里已存的 `game_id`
/// （如 ICF 识别写入的值）。`host` 为网络设置「服务器地址」。
fn initiald_patch(record: &GameRecord, game_dir: &Path, host: &str) -> Result<bool, String> {
    let game_id = record
        .crc
        .as_deref()
        .and_then(|s| {
            let hex = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .unwrap_or(s);
            u32::from_str_radix(hex.trim(), 16).ok()
        })
        .and_then(|crc| db::lookup(crc, record.loader))
        .and_then(|m| m.game_id)
        .or_else(|| record.game_id.clone());
    crate::initiald::patch_game_ini(game_dir, game_id.as_deref(), host)
}

/// 以游戏/ServerBox 目录为 cwd 启动加载器，返回子进程。
///
/// 与游戏目录内 start_game.bat / start_serverbox.bat 的调用形态一致：
/// 游戏 / ServerBox 均为 `mice -b <裸文件名>`（**不带 `-p`**——那是读卡器
/// picodaemon 专用参数，附加到游戏会导致其初始化阶段与读卡器进程争用而冻结：
/// 只出钩子日志、无窗口）；kitten 同样为 `kitten -b <裸文件名>`。
/// kitten 与 mice 均为控制台程序，均以 CREATE_NO_WINDOW 隐藏 CMD 窗口；
/// 仅读卡器 picodaemon（spawn_reader）保留 CMD 窗口供观察输出。
pub fn spawn_game(record: &GameRecord, loader: &Path) -> Result<std::process::Child, String> {
    let game = PathBuf::from(&record.path);
    if !game.is_file() {
        return Err(tf!("游戏文件不存在：{}", "Game file does not exist: {}", game.display()));
    }
    let game_dir = game
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| launcher_dir());
    if !game_dir.is_dir() {
        return Err(tf!("游戏目录不存在：{}", "Game directory does not exist: {}", game_dir.display()));
    }

    let mut cmd = Command::new(loader);
    // cwd 必须是游戏目录：kitten / mice 都按相对路径找配置（<游戏目录>\kitten\config.ini /
    // <游戏目录>\mice\config.ini）与存档、日志，子进程默认会继承启动器的 cwd
    cmd.current_dir(&game_dir);
    match record.loader {
        Loader::Kitten => {
            // 与游戏目录内 start_game.bat 一致：`kitten -b id5.elf`。
            // 只传文件名（cwd 已是游戏目录）：绝对路径里的空格会被拆开，
            // 导致 kitten 报 "Failed to parse ELF file: D:\software\Initial"
            let file = Path::new(&record.path)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&record.path));
            cmd.arg("-b").arg(&file);
            // kitten 是控制台程序，禁止创建 CMD 窗口（同 vpn.rs 隐藏 openvpn 窗口）
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        Loader::Mice => {
            // 与游戏目录内 start_game.bat / start_serverbox.bat 一致：
            // `mice -b <游戏exe>`（裸文件名，cwd 已是游戏目录；不带读卡器专用的 -p，
            // 也规避 kitten 已验证过的「绝对路径含空格被拆开」解析隐患）
            let file = Path::new(&record.path)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&record.path));
            cmd.arg("-b").arg(&file);
            // mice 也是控制台程序，运行时会弹出多余 CMD 窗口；
            // 游戏本体与 ServerBox 一律隐藏，仅 picodaemon 保留窗口供观察
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        Loader::Segatools => {
            // launch_game 已把 segatools 路由到 spawn_idac_game；此处仅 ServerBox 路径可达前的防御
            return Err(t!("segatools 游戏暂不支持 ServerBox / 该启动路径",
                "The segatools loader does not support ServerBox / this launch path yet").to_string());
        }
    }
    // 测试模式（游戏行「测试模式」勾选框）：末尾追加 `-t`，即 `kitten -b id5.elf -t`
    if record.test_mode == Some(true) {
        cmd.arg("-t");
    }
    cmd.spawn()
        .map_err(|e| tf!("启动加载器失败（{}）：{e}", "Failed to start loader ({}): {e}", loader.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("dnet_launch_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    fn cleanup(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    #[test]
    fn ensure_cg_libs_missing_dependency_errors() {
        let game = tmp("cg_missing_game");
        let dep = tmp("cg_missing_dep"); // 不含 cg.dll
        let err = ensure_cg_libs_from(&game, &dep).unwrap_err();
        assert!(err.contains("cg.dll"), "应提示 cg.dll 缺失：{err}");
        cleanup(&game);
        cleanup(&dep);
    }

    #[test]
    fn ensure_cg_libs_copies_both() {
        let game = tmp("cg_game");
        let dep = tmp("cg_dep");
        write(&dep.join("cg.dll"), b"cg-content");
        write(&dep.join("cgGL.dll"), b"cggl-content");
        ensure_cg_libs_from(&game, &dep).unwrap();
        assert!(game.join("cg.dll").is_file());
        assert!(game.join("cgGL.dll").is_file());
        cleanup(&game);
        cleanup(&dep);
    }

    #[test]
    fn ensure_picodaemon_missing_dependency_errors() {
        let game = tmp("pd_missing_game");
        let dep = tmp("pd_missing_dep");
        let err = ensure_picodaemon_from(&game, &dep).unwrap_err();
        assert!(
            err.contains("picodaemon.exe"),
            "应提示 picodaemon.exe 缺失：{err}"
        );
        cleanup(&game);
        cleanup(&dep);
    }

    #[test]
    fn ensure_picodaemon_copies_when_absent() {
        let game = tmp("pd_game");
        let dep = tmp("pd_dep");
        write(&dep.join("picodaemon.exe"), b"pico-build");
        let out = ensure_picodaemon_from(&game, &dep).unwrap();
        assert_eq!(out, game.join("picodaemon.exe"));
        assert!(game.join("picodaemon.exe").is_file());
        cleanup(&game);
        cleanup(&dep);
    }

    #[test]
    fn ensure_picodaemon_reuses_identical_existing() {
        let game = tmp("pd_reuse_game");
        let dep = tmp("pd_reuse_dep");
        write(&dep.join("picodaemon.exe"), b"same-build");
        // 第一轮：从依赖拷贝入游戏目录
        ensure_picodaemon_from(&game, &dep).unwrap();
        // 第二轮：已存在且与依赖完全一致 → 复用，不报错、不重拷
        let out = ensure_picodaemon_from(&game, &dep).unwrap();
        assert_eq!(out, game.join("picodaemon.exe"));
        assert!(game.join("picodaemon.exe").is_file());
        cleanup(&game);
        cleanup(&dep);
    }

    #[test]
    fn deploy_reader_writes_cardfile_gameip_io3_and_card_button() {
        let root = tmp("deploy_reader");
        let tool = root.join("mice_tool");
        let reader_dir = root.join("reader");
        fs::create_dir_all(&tool).unwrap();
        fs::create_dir_all(&reader_dir).unwrap();
        write(&tool.join("mice.exe"), b"mice-bin");
        write(&tool.join("mice.dll"), b"dll-bin");
        write(&reader_dir.join("picodaemon.exe"), b"pico");
        let mut settings = Settings::default();
        settings.mice_path = Some(tool.join("mice.exe").to_string_lossy().into_owned());
        let detected = LoaderPaths::default();
        let reader = reader_dir.join("picodaemon.exe").to_string_lossy().into_owned();

        // 卡片 + 主机 IP + 输入类型 + 插卡键（IDAS4~5）→ 四节全写
        let exe = deploy_reader(
            &reader,
            &settings,
            &detected,
            Some("01010409725450755799"),
            Some("192.168.100.101"),
            Some("START"),
            Some("xinput"),
        )
        .unwrap();
        assert!(exe.is_file());
        let ini = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(ini.contains("[CardReader]\ncardfile = 01010409725450755799\n"));
        assert!(ini.contains("[ServerBox]\ngame_ip = 192.168.100.101\n"));
        assert!(ini.contains("[io3]\nmode = xinput\n"));
        assert!(ini.contains("[xinput]\ncard = START\n"));

        // 输入类型 / 插卡键均无 → 不写 [io3] / [xinput] 节
        deploy_reader(&reader, &settings, &detected, Some("0101"), None, None, None).unwrap();
        let ini2 = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(ini2.contains("[CardReader]\ncardfile = 0101\n"));
        assert!(!ini2.contains("[io3]"));
        assert!(!ini2.contains("[xinput]"));

        // 有输入类型但插卡键空白 → 写 [io3]，不写 [xinput]
        deploy_reader(&reader, &settings, &detected, Some("0101"), None, Some("  "), Some("dinput")).unwrap();
        let ini3 = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(ini3.contains("[io3]\nmode = dinput\n"));
        assert!(!ini3.contains("[xinput]"));

        cleanup(&root);
    }

    #[test]
    fn files_equal_detects_difference() {
        let a = tmp("fe_a");
        let b = tmp("fe_b");
        write(&a.join("f"), b"x");
        write(&b.join("f"), b"y");
        assert!(!files_equal(&a.join("f"), &b.join("f")));
        write(&b.join("f"), b"x");
        assert!(files_equal(&a.join("f"), &b.join("f")));
        cleanup(&a);
        cleanup(&b);
    }
}
