//! 数据模型与持久化：游戏库、设置、数据目录（绿色免安装，均在 exe 同目录 data/ 下）

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::i18n::{self, Lang};
use crate::ini;

// ---------------------------------------------------------------------------
// INI 写入辅助：把 `Option` 值格式化为字符串（缺失写空值）。
// 各 `save_*_to` 原本各定义了一份相同的 `text`/`num`/`boolv`/`flag` 闭包，
// 此处收敛为模块级单一实现，消除重复。
// ---------------------------------------------------------------------------
/// `Option<&str>` → `&str`（`None` 视为空）。
fn text(v: Option<&str>) -> &str {
    v.unwrap_or("")
}
/// `Option<u32>` → 十进制字符串（`None` 视为空）。
fn num(v: Option<u32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}
/// `Option<bool>` → `"true"`/`"false"`（`None` 视为空）。
fn boolv(v: Option<bool>) -> &'static str {
    v.map(|b| if b { "true" } else { "false" })
        .unwrap_or_default()
}
/// `Option<bool>` → `"1"`/`"0"`（`None` 视为空），用于 `[graphics]` 等开关键。
fn flag(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Kitten,
    Mice,
    /// ALLS 平台游戏（IDZ / IDAC 等），经 segatools 系加载器运行；启动链路暂未接入
    Segatools,
}

impl Loader {
    /// 平台显示名：游戏列表徽章用（Lindbergh=头文字D 4/5 基板，RingEdge=6/7/8 基板，
    /// ALLS=IDZ / IDAC 基板）
    pub fn platform(&self) -> &'static str {
        match self {
            Loader::Kitten => "Lindbergh",
            Loader::Mice => "RingEdge",
            Loader::Segatools => "ALLS",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameRecord {
    pub id: String,
    pub path: String,
    pub name: String,
    /// 识别库命中的 CRC（"0x1fd6136a"）；手动添加的未知游戏为 None
    pub crc: Option<String>,
    pub version: Option<String>,
    /// 架构（x86/x64），用于 mice 32/64 加载器选择
    pub arch: Option<String>,
    pub loader: Loader,
    /// 识别库命中的平台名（games_db.json 的 platform 字段，如 "ALLS"）；
    /// 旧档 / 未识别游戏缺省时显示回退为 loader 推导
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 用户提供的封面图片（data/images/ 下的缓存副本路径，旧档兼容）
    pub image: Option<String>,
    /// 识别库默认封面文件名（如 "logo_D4.png"）；从 exe 内嵌字节渲染，None=用首字母占位块
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    /// 已校验的 ServerBox 文件路径；None=未配置（旧档缺省兼容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serverbox: Option<String>,
    /// 读卡器（picodaemon.exe）路径：**已弃用**——现由启动器在启动游戏时自动解析
    /// 游戏目录，不再读取此字段。保留以兼容旧档序列化（反序列化后忽略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<String>,
    /// segatools 游戏（IDAC 等）的 amdaemon.exe 路径；None=未配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amdaemon: Option<String>,
    /// segatools 游戏的游戏主程序（GameProject-Win64-Shipping.exe）路径；None=未配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_exe: Option<String>,
    /// 识别库命中的游戏 id（ICF 识别写入解析值，如 "SDGT"；CRC 命中写入库内 game_id）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    /// 图形补丁勾选（IDAS6~8 / mice 游戏：启动时部署 winmm.dll + scripts）；
    /// None 视为勾选（默认开启）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gfx_patch: Option<bool>,
    /// 测试模式勾选状态；None=未勾选（旧档缺省兼容）。
    /// 当前仅记录状态，尚未接入启动流程（语义待定）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_mode: Option<bool>,
    /// 选中的卡片（车卡 cardfile 名 / aime 20 位 access code）；每个游戏独立保存，
    /// None = 未选择（旧档兼容），启动时回退全局 config.ini [card] cardfile
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    pub play_count: u32,
    pub last_played: Option<u64>,
    pub added_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Library {
    pub version: u32,
    pub games: Vec<GameRecord>,
}

impl Default for Library {
    fn default() -> Self {
        Library {
            version: 1,
            games: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// 手动指定的加载器路径；为空时使用自动检测结果
    pub kitten_path: Option<String>,
    pub mice_path: Option<String>,
    pub mice64_path: Option<String>,
    /// segatools 工具目录内的 inject.exe 路径（segatools 游戏）；为空时使用自动检测
    #[serde(default)]
    pub segatools_path: Option<String>,
    /// 自动检查更新（默认开启）
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    /// 各加载器已安装版本（安装成功后写回，作为更新判定的本地基线）
    #[serde(default)]
    pub last_kitten_version: Option<String>,
    #[serde(default)]
    pub last_mice_version: Option<String>,
    #[serde(default)]
    pub last_asiloader_version: Option<String>,
    #[serde(default)]
    pub last_shaderfix_version: Option<String>,
    #[serde(default)]
    pub last_segatools_version: Option<String>,
    /// awl 已安装版本（复用更新判定基线）
    #[serde(default)]
    pub last_awl_version: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            kitten_path: None,
            mice_path: None,
            mice64_path: None,
            segatools_path: None,
            auto_check_updates: true,
            last_kitten_version: None,
            last_mice_version: None,
            last_asiloader_version: None,
            last_shaderfix_version: None,
            last_segatools_version: None,
            last_awl_version: None,
        }
    }
}

/// 网络设置（保存在启动器自己的 config.ini `[network]` 节，与 settings.json 互不相干）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkConfig {
    /// 选中的网卡（GetAdaptersAddresses 的 AdapterName GUID）；None = 未选择
    pub adapter: Option<String>,
    /// 选中的 IPv4 网段（CIDR，如 "192.168.100.0/24"）；None = 未选择
    pub ip: Option<String>,
    /// 模拟网络：开启时由启动器在本机模拟游戏网络环境；None 视为开启
    pub simulate: Option<bool>,
    /// 服务器地址（域名或 IP）；None / 空 = 默认 sdgt.arcadehub.xyz
    pub server: Option<String>,
    /// 主机（本机游戏）IP：选中网卡后自动填第一个 IP，可手改；awl 网卡时锁定。
    /// 启动游戏时写入 kitten `[network] game_ip` / mice idas.ini `[ServerBox] game_ip`；
    /// 房间模式下由房间覆盖（房主 .101 / 成员座位 IP）。
    pub host_ip: Option<String>,
    /// ServerBox 静态绑定 IP：仅当「模拟网络」关闭（Some(false)）时可配置。
    /// 启动游戏时若非空且选中网卡无第二个 IPv4 地址，则为该网卡添加此静态 IP
    /// （awl 网卡除外，其地址由房间流程管理），并写入 kitten `[network]` /
    /// mice 配置的 `[ServerBox] bind_ip`。
    pub serverbox_ip: Option<String>,
}

impl NetworkConfig {
    /// 服务器地址：未设置 / 空白时回退默认 [`crate::server::SERVER_HOST`]。
    pub fn server_host(&self) -> &str {
        self.server
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(crate::server::SERVER_HOST)
    }
}

/// 系统设置（保存在启动器自己的 config.ini `[system]` 节）。
/// None = 未设置：仅写入启动器 config.ini 的空值，由加载器自行读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemConfig {
    /// 日志级别（error/warn/info/debug/trace；kitten 无 off 故不提供，
    /// 历史遗留的 "off" 视为未设置 → kitten 默认 Info）
    pub loglevel: Option<String>,
    /// 地区代码（JPN=日本 EXP=出口 CHN=中国）
    pub region: Option<String>,
    /// 钥匙芯片序列号（如 A69E-01A99999999）
    pub keychip: Option<String>,
    /// 跳过读卡器初始化
    pub skip_reader_init: Option<bool>,
    /// 机位（segatools / IDAC 专用）：SINGLE=单机、A1/A2/B1/B2=4 机位；None 视为单机
    pub seat: Option<String>,
    /// 主机（segatools / IDAC 专用，对应 [system] dipsw1）；None 视为单机时的自动值
    pub master: Option<bool>,
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// 读取选用的卡片（config.ini `[card]` 节 `cardfile`；缺失或为空为 None）。
/// 仅作为旧档回退：新选择已按游戏存入 `GameRecord.card`（`library.json`）。
fn load_card_from(path: &Path) -> Option<String> {
    ini::read_value(path, "card", "cardfile").filter(|s| !s.is_empty())
}

/// 投币设置（保存在启动器自己的 config.ini `[coin]` 节）。
/// None = 未设置：仅写入启动器 config.ini 的空值，由加载器自行读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoinConfig {
    /// 免费游玩
    pub freeplay: Option<bool>,
    /// 开始游戏币数
    pub start_coin: Option<u32>,
    /// 续关币数
    pub continue_coin: Option<u32>,
    /// 购买车卡币数
    pub card_coin: Option<u32>,
    /// 购买车辆币数
    pub car_coin: Option<u32>,
}

/// 从 INI 文件读取投币设置（`[coin]` 节）；键缺失或为空均视为未设置。
fn load_coin_from(path: &Path) -> CoinConfig {
    let read = |key: &str| ini::read_value(path, "coin", key).filter(|s| !s.is_empty());
    CoinConfig {
        freeplay: read("freeplay").map(|v| truthy(&v)),
        start_coin: read("start_coin").and_then(|v| v.trim().parse().ok()),
        continue_coin: read("continue_coin").and_then(|v| v.trim().parse().ok()),
        card_coin: read("card_coin").and_then(|v| v.trim().parse().ok()),
        car_coin: read("car_coin").and_then(|v| v.trim().parse().ok()),
    }
}

/// 把投币设置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_coin_to(path: &Path, cfg: &CoinConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }

    ini::set_value(
        path,
        "coin",
        "freeplay",
        match cfg.freeplay {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        },
    )?;
    ini::set_value(path, "coin", "start_coin", &num(cfg.start_coin))?;
    ini::set_value(path, "coin", "continue_coin", &num(cfg.continue_coin))?;
    ini::set_value(path, "coin", "card_coin", &num(cfg.card_coin))?;
    ini::set_value(path, "coin", "car_coin", &num(cfg.car_coin))
}

/// 图形设置（保存在启动器自己的 config.ini `[graphics]` 节）。
/// None = 未设置：仅写入启动器 config.ini 的空值，由加载器自行读取。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphicsConfig {
    /// 使用默认设置（开启时忽略其余图形选项）
    pub default: Option<bool>,
    /// 无边框模式
    pub borderless: Option<bool>,
    /// 全屏模式
    pub fullscreen: Option<bool>,
    /// 分辨率（如 "1920x1080"）
    pub resolution: Option<String>,
    /// 渲染倍数
    pub render_scale: Option<bool>,
    /// 渲染倍率（如 "2x"）
    pub render_scale_level: Option<String>,
    /// MSAA 抗锯齿
    pub msaa: Option<bool>,
    /// MSAA 倍数（如 "4x"）
    pub msaa_level: Option<String>,
    /// 各向异性过滤
    pub af: Option<bool>,
    /// 过滤倍数（如 "16x"）
    pub af_level: Option<String>,
}

/// 从 INI 文件读取图形设置（`[graphics]` 节）；键缺失或为空均视为未设置。
fn load_graphics_from(path: &Path) -> GraphicsConfig {
    let read = |key: &str| ini::read_value(path, "graphics", key).filter(|s| !s.is_empty());
    GraphicsConfig {
        default: read("default").map(|v| truthy(&v)),
        borderless: read("borderless").map(|v| truthy(&v)),
        fullscreen: read("fullscreen").map(|v| truthy(&v)),
        resolution: read("resolution"),
        render_scale: read("render_scale").map(|v| truthy(&v)),
        render_scale_level: read("render_scale_level"),
        msaa: read("msaa").map(|v| truthy(&v)),
        msaa_level: read("msaa_level"),
        af: read("af").map(|v| truthy(&v)),
        af_level: read("af_level"),
    }
}

/// 把图形设置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_graphics_to(path: &Path, cfg: &GraphicsConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }

    ini::set_value(path, "graphics", "default", flag(cfg.default))?;
    ini::set_value(path, "graphics", "borderless", flag(cfg.borderless))?;
    ini::set_value(path, "graphics", "fullscreen", flag(cfg.fullscreen))?;
    ini::set_value(
        path,
        "graphics",
        "resolution",
        text(cfg.resolution.as_deref()),
    )?;
    ini::set_value(path, "graphics", "render_scale", flag(cfg.render_scale))?;
    ini::set_value(
        path,
        "graphics",
        "render_scale_level",
        text(cfg.render_scale_level.as_deref()),
    )?;
    ini::set_value(path, "graphics", "msaa", flag(cfg.msaa))?;
    ini::set_value(
        path,
        "graphics",
        "msaa_level",
        text(cfg.msaa_level.as_deref()),
    )?;
    ini::set_value(path, "graphics", "af", flag(cfg.af))?;
    ini::set_value(path, "graphics", "af_level", text(cfg.af_level.as_deref()))
}

/// 十字键（Hat Switch）落库基准：方向编号 1~4 加上它，得到 1001~1004，
/// 与按钮编号（一般 1~64）区分，加载器按数值范围判断是按钮还是十字键
pub(crate) const DPAD_CODE_BASE: u32 = 1000;

/// 输入信号限制（`InputConfig.ratio`，原「输入比率」）的默认值：UI 滑块与各加载器配置键
/// （mice/kitten 的 `[io3] restrict_`、segatools 的 `[io3]/[io4] restrict`）共用；
/// ratio 未设置（None）时配置生成按此值写出。
pub const INPUT_RATIO_DEFAULT: u32 = 128;

/// 输入设置（保存在启动器自己的 config.ini `[input]` 节）。
/// None = 未设置：仅写入启动器 config.ini 的空值，由加载器自行读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputConfig {
    /// 输入类型（"xinput" | "dinput"）
    pub kind: Option<String>,
    /// 方向盘设备路径
    pub wheel_device: Option<String>,
    /// 油门踏板设备路径
    pub pedal_device: Option<String>,
    /// 排挡设备路径
    pub shifter_device: Option<String>,
    /// 油门轴名（dinput：写入 idas.ini [dinput] accelAxis，如 "Y" / "RZ"）
    pub throttle_axis: Option<String>,
    /// 刹车轴名（dinput：写入 idas.ini [dinput] brakeAxis，如 "Y" / "RZ"）
    pub brake_axis: Option<String>,
    /// 反转油门轴值（dinput：写入 idas.ini [dinput] reverseAccelAxis；勾选 true，否则 false）
    pub reverse_accel_axis: Option<bool>,
    /// 反转刹车轴值（dinput：写入 idas.ini [dinput] reverseBrakeAxis；勾选 true，否则 false）
    pub reverse_brake_axis: Option<bool>,
    /// 升档按键
    pub shift_up: Option<u32>,
    /// 降档按键
    pub shift_down: Option<u32>,
    /// start 键
    pub start_button: Option<u32>,
    /// view 键
    pub view_button: Option<u32>,
    /// 投币键
    pub coin_button: Option<u32>,
    /// 插卡键
    pub card_button: Option<u32>,
    /// 方向键：上
    pub up_button: Option<u32>,
    /// 方向键：下
    pub down_button: Option<u32>,
    /// 方向键：左
    pub left_button: Option<u32>,
    /// 方向键：右
    pub right_button: Option<u32>,
    /// 输入信号限制（0~255 滑块，默认 128；None 表示未设置，按默认 128 显示；原「输入比率」）
    pub ratio: Option<u32>,
    /// 排挡类型（"sequential"=序列排挡 | "h"=H 排挡）；None=未选择
    pub shifter_type: Option<String>,
    /// H 排挡：1 挡按键
    pub gear1_button: Option<u32>,
    /// H 排挡：2 挡按键
    pub gear2_button: Option<u32>,
    /// H 排挡：3 挡按键
    pub gear3_button: Option<u32>,
    /// H 排挡：4 挡按键
    pub gear4_button: Option<u32>,
    /// H 排挡：5 挡按键
    pub gear5_button: Option<u32>,
    /// H 排挡：6 挡按键
    pub gear6_button: Option<u32>,
    /// H 排挡：倒挡按键
    pub reverse_button: Option<u32>,
    /// XInput 手柄槽位索引（0~3），仅 xinput 模式使用。
    /// 独立字段：避免与 dinput 的 wheel_device（HID 设备路径）互相覆盖
    pub pad_index: Option<u32>,
    /// 双摇杆模式（XInput）：Some(true) → kitten `singleStickSteering = false`；
    /// Some(false)/None → `true`（单摇杆，kitten 默认）
    pub dual_stick: Option<bool>,
    /// 线性方向（XInput）：Some(true) → kitten `linearSteering = true`，Some(false) → `false`
    pub linear_steering: Option<bool>,
    /// 左摇杆死区（百分比 0~100，默认 20）：XInput，渲染为 kitten `leftStickDeadzone = 32767 × 百分比`
    pub left_deadzone: Option<u32>,
    /// 右摇杆死区（百分比 0~100，默认 20）：XInput，渲染为 kitten `rightStickDeadzone = 32767 × 百分比`
    pub right_deadzone: Option<u32>,
    /// XInput 按键名映射（写入 idas.ini [xinput] 的按钮名，如 "A" / "DPAD_UP"）；
    /// 仅当用户捕获过才写出，否则交 mice 默认兜底。键名加 `xi_` 前缀避免与 dinput 键名冲突。
    pub xi_start: Option<String>,
    pub xi_view: Option<String>,
    pub xi_up: Option<String>,
    pub xi_down: Option<String>,
    pub xi_left: Option<String>,
    pub xi_right: Option<String>,
    pub xi_shift_dn: Option<String>,
    pub xi_shift_up: Option<String>,
    pub xi_coin: Option<String>,
    pub xi_card: Option<String>,
    /// XInput 油门源 / 刹车源（写入 idas.ini / config.ini [xinput] 的 `accel` / `brake` 键，
    /// 取值 trigger 或按钮名如 "A"）；仅当用户捕获过才写出，否则交工具默认（扳机）兜底。
    pub xi_accel: Option<String>,
    pub xi_brake: Option<String>,
    /// 力反馈总开关（写入 idas.ini/config.ini [ffb] enable；None 视为 true，默认开启）
    pub ffb_enable: Option<bool>,
    /// 力反馈：恒力强度（居中弹簧效果），0~100，默认 100
    pub ffb_constant_force: Option<u32>,
    /// 力反馈：阻尼强度（方向盘阻尼效果），0~100，默认 100
    pub ffb_damper: Option<u32>,
    /// 力反馈：路面隆隆强度，0~100，默认 100
    pub ffb_rumble: Option<u32>,
    /// 力反馈：隆隆时长系数（ms→µs），0~100，默认 100
    pub ffb_rumble_duration: Option<u32>,
    /// 力反馈：方向盘基础阻尼/重量，0~100，默认 20
    pub ffb_base_damper: Option<u32>,
    /// 力反馈：方向盘中心死区，0~100，默认 20
    pub ffb_dead_band: Option<u32>,
}

/// VPN 配置（保存在启动器自己的 config.ini `[vpn]` 节）。
/// None = 未设置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VpnConfig {
    /// .ovpn 配置文件路径
    pub config_path: Option<String>,
    /// 私钥密码（加密私钥需要；明文存本地，后续可接 Windows DPAPI 加密）
    pub passphrase: Option<String>,
    /// openvpn.exe 路径（缺省为 launcher/tools/openvpn/openvpn.exe）
    pub openvpn_path: Option<String>,
}

/// 从 INI 文件读取输入设置（`[input]` 节）；键缺失或为空均视为未设置。
fn load_input_from(path: &Path) -> InputConfig {
    let read = |key: &str| ini::read_value(path, "input", key).filter(|s| !s.is_empty());
    let num = |key: &str| read(key).and_then(|v| v.trim().parse().ok());
    // 轴名：标准化为 X/Y/Z/Rx/Ry/Rz/Slider/Dial/Wheel；旧的数字轴号或未知值原样保留
    let read_axis = |key: &str| -> Option<String> {
        let raw = read(key)?;
        let up = raw.trim().to_ascii_uppercase();
        let known = ["X", "Y", "Z", "RX", "RY", "RZ", "SLIDER", "DIAL", "WHEEL"];
        if known.contains(&up.as_str()) {
            Some(up)
        } else {
            Some(raw.trim().to_string())
        }
    };
    InputConfig {
        kind: read("type"),
        wheel_device: read("wheel_device"),
        pedal_device: read("pedal_device"),
        shifter_device: read("shifter_device"),
        // 兼容旧键名 throttle_button / brake_button（早期误存为按键）
        throttle_axis: read_axis("throttle_axis").or_else(|| read_axis("throttle_button")),
        brake_axis: read_axis("brake_axis").or_else(|| read_axis("brake_button")),
        reverse_accel_axis: read("reverseAccelAxis").map(|v| v.trim() == "true" || v.trim() == "1"),
        reverse_brake_axis: read("reverseBrakeAxis").map(|v| v.trim() == "true" || v.trim() == "1"),
        shift_up: num("shift_up"),
        shift_down: num("shift_down"),
        start_button: num("start_button"),
        view_button: num("view_button"),
        coin_button: num("coin_button"),
        card_button: num("card_button"),
        up_button: num("up_button"),
        down_button: num("down_button"),
        left_button: num("left_button"),
        right_button: num("right_button"),
        ratio: num("ratio"),
        pad_index: num("pad_index"),
        shifter_type: read("shifter_type"),
        gear1_button: num("gear1_button"),
        gear2_button: num("gear2_button"),
        gear3_button: num("gear3_button"),
        gear4_button: num("gear4_button"),
        gear5_button: num("gear5_button"),
        gear6_button: num("gear6_button"),
        reverse_button: num("reverse_button"),
        dual_stick: read("dual_stick").map(|v| v.trim() == "true" || v.trim() == "1"),
        linear_steering: read("linear_steering").map(|v| v.trim() == "true" || v.trim() == "1"),
        left_deadzone: num("left_deadzone"),
        right_deadzone: num("right_deadzone"),
        xi_start: read("xi_start"),
        xi_view: read("xi_view"),
        xi_up: read("xi_up"),
        xi_down: read("xi_down"),
        xi_left: read("xi_left"),
        xi_right: read("xi_right"),
        xi_shift_dn: read("xi_shift_dn"),
        xi_shift_up: read("xi_shift_up"),
        xi_coin: read("xi_coin"),
        xi_card: read("xi_card"),
        xi_accel: read("xi_accel"),
        xi_brake: read("xi_brake"),
        ffb_enable: read("ffb_enable").map(|v| v.trim() == "true" || v.trim() == "1"),
        ffb_constant_force: num("ffb_constant_force"),
        ffb_damper: num("ffb_damper"),
        ffb_rumble: num("ffb_rumble"),
        ffb_rumble_duration: num("ffb_rumble_duration"),
        ffb_base_damper: num("ffb_base_damper"),
        ffb_dead_band: num("ffb_dead_band"),
    }
}

/// 把输入设置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_input_to(path: &Path, cfg: &InputConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }


    ini::set_value(path, "input", "type", text(cfg.kind.as_deref()))?;
    ini::set_value(
        path,
        "input",
        "wheel_device",
        text(cfg.wheel_device.as_deref()),
    )?;
    ini::set_value(
        path,
        "input",
        "pedal_device",
        text(cfg.pedal_device.as_deref()),
    )?;
    ini::set_value(
        path,
        "input",
        "shifter_device",
        text(cfg.shifter_device.as_deref()),
    )?;
    ini::set_value(path, "input", "throttle_axis", text(cfg.throttle_axis.as_deref()))?;
    ini::set_value(path, "input", "brake_axis", text(cfg.brake_axis.as_deref()))?;
    ini::set_value(
        path,
        "input",
        "reverseAccelAxis",
        boolv(cfg.reverse_accel_axis),
    )?;
    ini::set_value(
        path,
        "input",
        "reverseBrakeAxis",
        boolv(cfg.reverse_brake_axis),
    )?;
    ini::set_value(path, "input", "shift_up", &num(cfg.shift_up))?;
    ini::set_value(path, "input", "shift_down", &num(cfg.shift_down))?;
    ini::set_value(path, "input", "start_button", &num(cfg.start_button))?;
    ini::set_value(path, "input", "view_button", &num(cfg.view_button))?;
    ini::set_value(path, "input", "coin_button", &num(cfg.coin_button))?;
    ini::set_value(path, "input", "card_button", &num(cfg.card_button))?;
    ini::set_value(path, "input", "up_button", &num(cfg.up_button))?;
    ini::set_value(path, "input", "down_button", &num(cfg.down_button))?;
    ini::set_value(path, "input", "left_button", &num(cfg.left_button))?;
    ini::set_value(path, "input", "right_button", &num(cfg.right_button))?;
    ini::set_value(path, "input", "ratio", &num(cfg.ratio))?;
    ini::set_value(path, "input", "pad_index", &num(cfg.pad_index))?;
    ini::set_value(
        path,
        "input",
        "shifter_type",
        text(cfg.shifter_type.as_deref()),
    )?;
    ini::set_value(path, "input", "gear1_button", &num(cfg.gear1_button))?;
    ini::set_value(path, "input", "gear2_button", &num(cfg.gear2_button))?;
    ini::set_value(path, "input", "gear3_button", &num(cfg.gear3_button))?;
    ini::set_value(path, "input", "gear4_button", &num(cfg.gear4_button))?;
    ini::set_value(path, "input", "gear5_button", &num(cfg.gear5_button))?;
    ini::set_value(path, "input", "gear6_button", &num(cfg.gear6_button))?;
    ini::set_value(
        path,
        "input",
        "dual_stick",
        boolv(cfg.dual_stick),
    )?;
    ini::set_value(
        path,
        "input",
        "linear_steering",
        boolv(cfg.linear_steering),
    )?;
    ini::set_value(path, "input", "left_deadzone", &num(cfg.left_deadzone))?;
    ini::set_value(path, "input", "right_deadzone", &num(cfg.right_deadzone))?;
    ini::set_value(path, "input", "ffb_enable", boolv(cfg.ffb_enable))?;
    ini::set_value(path, "input", "ffb_constant_force", &num(cfg.ffb_constant_force))?;
    ini::set_value(path, "input", "ffb_damper", &num(cfg.ffb_damper))?;
    ini::set_value(path, "input", "ffb_rumble", &num(cfg.ffb_rumble))?;
    ini::set_value(path, "input", "ffb_rumble_duration", &num(cfg.ffb_rumble_duration))?;
    ini::set_value(path, "input", "ffb_base_damper", &num(cfg.ffb_base_damper))?;
    ini::set_value(path, "input", "ffb_dead_band", &num(cfg.ffb_dead_band))?;
    ini::set_value(path, "input", "xi_start", text(cfg.xi_start.as_deref()))?;
    ini::set_value(path, "input", "xi_view", text(cfg.xi_view.as_deref()))?;
    ini::set_value(path, "input", "xi_up", text(cfg.xi_up.as_deref()))?;
    ini::set_value(path, "input", "xi_down", text(cfg.xi_down.as_deref()))?;
    ini::set_value(path, "input", "xi_left", text(cfg.xi_left.as_deref()))?;
    ini::set_value(path, "input", "xi_right", text(cfg.xi_right.as_deref()))?;
    ini::set_value(path, "input", "xi_shift_dn", text(cfg.xi_shift_dn.as_deref()))?;
    ini::set_value(path, "input", "xi_shift_up", text(cfg.xi_shift_up.as_deref()))?;
    ini::set_value(path, "input", "xi_coin", text(cfg.xi_coin.as_deref()))?;
    ini::set_value(path, "input", "xi_card", text(cfg.xi_card.as_deref()))?;
    ini::set_value(path, "input", "xi_accel", text(cfg.xi_accel.as_deref()))?;
    ini::set_value(path, "input", "xi_brake", text(cfg.xi_brake.as_deref()))?;
    ini::set_value(path, "input", "reverse_button", &num(cfg.reverse_button))
}

/// 从 INI 文件读取 VPN 配置（`[vpn]` 节）；键缺失或为空均视为未设置。
fn load_vpn_from(path: &Path) -> VpnConfig {
    let read = |key: &str| ini::read_value(path, "vpn", key).filter(|s| !s.is_empty());
    VpnConfig {
        config_path: read("config"),
        passphrase: read("passphrase").map(|s| crate::win_util::unprotect_secret(&s)),
        openvpn_path: read("openvpn"),
    }
}

/// 房间功能配置（启动器 config.ini 的 `[room]` 节）。
/// None = 未设置（走默认值）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomConfig {
    /// 房间目录服务地址（缺省 https://sdgt.arcadehub.xyz/roomapi）
    pub directory: Option<String>,
    /// awl-tray.exe 路径（缺省 launcher/tools/awl/awl-tray.exe）
    pub awl_path: Option<String>,
    /// 上次加入的房间 ID（成员未退出直接关启动器后，下次进入店内对战自动回归）
    pub last_room_id: Option<String>,
    /// 上次房间的密码（私密房回归需要；与 VPN 密码同策略，经 DPAPI 加密存储，旧明文向后兼容）
    pub last_room_password: Option<String>,
    /// 房间昵称（成员表「成员」列显示；未设置时创建/加入房间会弹窗提示，
    /// 跳过后本次会话用默认昵称 player1234）。上限 24 字符，保存时已 trim。
    pub nickname: Option<String>,
}

/// 从 INI 文件读取房间配置（`[room]` 节）；键缺失或为空均视为未设置。
fn load_room_from(path: &Path) -> RoomConfig {
    let read = |key: &str| ini::read_value(path, "room", key).filter(|s| !s.is_empty());
    RoomConfig {
        directory: read("directory"),
        awl_path: read("awl"),
        last_room_id: read("last_room"),
        last_room_password: read("last_password").map(|s| crate::win_util::unprotect_secret(&s)),
        nickname: read("nickname"),
    }
}

/// 把房间配置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_room_to(path: &Path, cfg: &RoomConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }
    ini::set_value(path, "room", "directory", text(cfg.directory.as_deref()))?;
    ini::set_value(path, "room", "awl", text(cfg.awl_path.as_deref()))?;
    ini::set_value(path, "room", "last_room", text(cfg.last_room_id.as_deref()))?;
    ini::set_value(path, "room", "nickname", text(cfg.nickname.as_deref()))?;
    ini::set_value(
        path,
        "room",
        "last_password",
        text(
            cfg.last_room_password
                .as_deref()
                .map(crate::win_util::protect_secret)
                .as_deref(),
        ),
    )
}

/// 把 VPN 配置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_vpn_to(path: &Path, cfg: &VpnConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }
    ini::set_value(path, "vpn", "config", text(cfg.config_path.as_deref()))?;
    ini::set_value(
        path,
        "vpn",
        "passphrase",
        text(
            cfg.passphrase
                .as_deref()
                .map(crate::win_util::protect_secret)
                .as_deref(),
        ),
    )?;
    ini::set_value(path, "vpn", "openvpn", text(cfg.openvpn_path.as_deref()))
}

/// 从 INI 文件读取网络设置（`[network]` 节）；键缺失或为空均视为未设置。
fn load_network_from(path: &Path) -> NetworkConfig {
    let read = |key: &str| ini::read_value(path, "network", key).filter(|s| !s.is_empty());
    NetworkConfig {
        adapter: read("adapter"),
        ip: read("ip"),
        simulate: read("simulate").map(|v| truthy(&v)),
        server: read("server"),
        host_ip: read("host_ip"),
        serverbox_ip: read("serverbox_ip"),
    }
}

/// 把网络设置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_network_to(path: &Path, cfg: &NetworkConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }
    ini::set_value(
        path,
        "network",
        "adapter",
        cfg.adapter.as_deref().unwrap_or(""),
    )?;
    ini::set_value(path, "network", "ip", cfg.ip.as_deref().unwrap_or(""))?;
    ini::set_value(
        path,
        "network",
        "simulate",
        match cfg.simulate {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        },
    )?;
    ini::set_value(
        path,
        "network",
        "server",
        cfg.server.as_deref().unwrap_or(""),
    )?;
    ini::set_value(path, "network", "host_ip", cfg.host_ip.as_deref().unwrap_or(""))?;
    ini::set_value(
        path,
        "network",
        "serverbox_ip",
        cfg.serverbox_ip.as_deref().unwrap_or(""),
    )
}

/// 从 INI 文件读取系统设置（`[system]` 节）；键缺失或为空均视为未设置。
fn load_system_from(path: &Path) -> SystemConfig {
    let read = |key: &str| ini::read_value(path, "system", key).filter(|s| !s.is_empty());
    SystemConfig {
        loglevel: read("loglevel"),
        region: read("region"),
        keychip: read("keychip"),
        skip_reader_init: read("skip_reader_init").map(|v| truthy(&v)),
        seat: read("seat"),
        master: read("master").map(|v| truthy(&v)),
    }
}

/// 把系统设置写入 INI 文件（保留其余内容与注释）；未设置的项写空值。
fn save_system_to(path: &Path, cfg: &SystemConfig) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, "; Sasayaki 启动器配置\n")?;
    }
    ini::set_value(
        path,
        "system",
        "loglevel",
        cfg.loglevel.as_deref().unwrap_or(""),
    )?;
    ini::set_value(
        path,
        "system",
        "region",
        cfg.region.as_deref().unwrap_or(""),
    )?;
    ini::set_value(
        path,
        "system",
        "keychip",
        cfg.keychip.as_deref().unwrap_or(""),
    )?;
    ini::set_value(
        path,
        "system",
        "skip_reader_init",
        match cfg.skip_reader_init {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        },
    )?;
    ini::set_value(path, "system", "seat", cfg.seat.as_deref().unwrap_or(""))?;
    ini::set_value(
        path,
        "system",
        "master",
        match cfg.master {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        },
    )
}

pub fn launcher_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    launcher_dir().join("data")
}

pub fn images_dir() -> PathBuf {
    data_dir().join("images")
}

fn ensure_dir(p: &Path) -> io::Result<()> {
    fs::create_dir_all(p)
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new() -> Store {
        Store { dir: data_dir() }
    }



    fn library_path(&self) -> PathBuf {
        self.dir.join("library.json")
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    /// 启动器自己的配置文件（exe 同目录，绿色便携，与 data/ 平级）
    fn config_path(&self) -> PathBuf {
        launcher_dir().join("config.ini")
    }

    pub fn load_library(&self) -> Library {
        load_json(&self.library_path()).unwrap_or_default()
    }

    pub fn save_library(&self, lib: &Library) {
        if let Err(e) = save_json(&self.library_path(), lib) {
            eprintln!("保存 library.json 失败: {e}");
        }
    }

    pub fn load_settings(&self) -> Settings {
        load_json(&self.settings_path()).unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) {
        if let Err(e) = save_json(&self.settings_path(), s) {
            eprintln!("保存 settings.json 失败: {e}");
        }
    }

    /// 从 config.ini 读取网络设置；文件/键缺失均视为未选择（模拟网络未设置视为开启）。
    pub fn load_network(&self) -> NetworkConfig {
        load_network_from(&self.config_path())
    }

    /// 把网络设置写入 config.ini（保留文件其余内容与注释）；未选择时写空值。
    pub fn save_network(&self, cfg: &NetworkConfig) -> io::Result<()> {
        save_network_to(&self.config_path(), cfg)
    }

    /// 从 config.ini 读取 VPN 设置；文件/键缺失均视为未配置。
    pub fn load_vpn(&self) -> VpnConfig {
        load_vpn_from(&self.config_path())
    }

    /// 把 VPN 设置写入 config.ini（保留文件其余内容与注释）。
    /// 私钥密码明文存储（MVP）；未设置的项写空值。
    pub fn save_vpn(&self, cfg: &VpnConfig) -> io::Result<()> {
        save_vpn_to(&self.config_path(), cfg)
    }

    /// 从 config.ini 读取房间功能设置；文件/键缺失均视为未配置（走默认值）。
    pub fn load_room(&self) -> RoomConfig {
        load_room_from(&self.config_path())
    }

    /// 把房间功能设置写入 config.ini（保留文件其余内容与注释）。
    pub fn save_room(&self, cfg: &RoomConfig) -> io::Result<()> {
        save_room_to(&self.config_path(), cfg)
    }

    /// 从 config.ini 读取系统设置；文件/键缺失均视为未设置。
    pub fn load_system(&self) -> SystemConfig {
        load_system_from(&self.config_path())
    }

    /// 把系统设置写入 config.ini（保留文件其余内容与注释）。
    pub fn save_system(&self, cfg: &SystemConfig) -> io::Result<()> {
        save_system_to(&self.config_path(), cfg)
    }

    /// 读取界面语言（`config.ini [ui] lang`）；未设置或非法则按系统区域自动检测。
    pub fn load_lang(&self) -> Lang {
        let raw = ini::read_value(&self.config_path(), "ui", "lang");
        let lang = i18n::from_ini_value(raw.as_deref());
        if lang == Lang::Zh && raw.is_none() {
            // 未保存过：回落到系统区域自动检测
            i18n::detect_system()
        } else {
            lang
        }
    }

    /// 把界面语言写入 config.ini（保留文件其余内容与注释）。
    pub fn save_lang(&self, lang: Lang) -> io::Result<()> {
        let path = self.config_path();
        if !path.exists() {
            fs::write(&path, "; Sasayaki 启动器配置\n")?;
        }
        ini::set_value(&path, "ui", "lang", i18n::to_ini_value(lang))
    }

    /// 从 config.ini 读取选用的卡片；文件/键缺失视为未选择。
    /// 仅作为旧档回退：新选择已按游戏存入 `GameRecord.card`。
    pub fn load_card(&self) -> Option<String> {
        load_card_from(&self.config_path())
    }

    /// 从 config.ini 读取投币设置；文件/键缺失均视为未设置。
    pub fn load_coin(&self) -> CoinConfig {
        load_coin_from(&self.config_path())
    }

    /// 把投币设置写入 config.ini（保留文件其余内容与注释）。
    pub fn save_coin(&self, cfg: &CoinConfig) -> io::Result<()> {
        save_coin_to(&self.config_path(), cfg)
    }

    /// 从 config.ini 读取图形设置；文件/键缺失均视为未设置。
    pub fn load_graphics(&self) -> GraphicsConfig {
        load_graphics_from(&self.config_path())
    }

    /// 把图形设置写入 config.ini（保留文件其余内容与注释）。
    pub fn save_graphics(&self, cfg: &GraphicsConfig) -> io::Result<()> {
        save_graphics_to(&self.config_path(), cfg)
    }

    /// 从 config.ini 读取输入设置；文件/键缺失均视为未设置。
    pub fn load_input(&self) -> InputConfig {
        load_input_from(&self.config_path())
    }

    /// 把输入设置写入 config.ini（保留文件其余内容与注释）。
    pub fn save_input(&self, cfg: &InputConfig) -> io::Result<()> {
        save_input_to(&self.config_path(), cfg)
    }

    pub fn remove_image(&self, id: &str) {
        if let Ok(entries) = fs::read_dir(images_dir()) {
            for e in entries.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{id}."))
                {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("dnet_storage_test");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn system_config_roundtrip() {
        let path = temp_config("system_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置
        assert_eq!(load_system_from(&path), SystemConfig::default());

        // 写入并读回
        let cfg = SystemConfig {
            loglevel: Some("debug".into()),
            region: Some("JPN".into()),
            keychip: Some("A69E-01A99999999".into()),
            skip_reader_init: Some(true),
            seat: Some("A1".into()),
            master: Some(false),
        };
        save_system_to(&path, &cfg).unwrap();
        assert_eq!(load_system_from(&path), cfg);

        // 部分覆盖：只写 region，其余键被写空回到未设置
        save_system_to(
            &path,
            &SystemConfig {
                region: Some("EXP".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let loaded = load_system_from(&path);
        assert_eq!(loaded.region, Some("EXP".into()));
        assert_eq!(loaded.loglevel, None);
        assert_eq!(loaded.keychip, None);
        assert_eq!(loaded.skip_reader_init, None);

        // 布尔字面量
        fs::write(&path, "[system]\nskip_reader_init = true\n").unwrap();
        let loaded = load_system_from(&path);
        assert_eq!(loaded.skip_reader_init, Some(true));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn coin_config_roundtrip() {
        let path = temp_config("coin_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置
        assert_eq!(load_coin_from(&path), CoinConfig::default());

        // 写入并读回
        let cfg = CoinConfig {
            freeplay: Some(true),
            start_coin: Some(1),
            continue_coin: Some(2),
            card_coin: Some(3),
            car_coin: Some(5),
        };
        save_coin_to(&path, &cfg).unwrap();
        assert_eq!(load_coin_from(&path), cfg);

        // 覆盖为默认：全部键写空回到未设置
        save_coin_to(&path, &CoinConfig::default()).unwrap();
        assert_eq!(load_coin_from(&path), CoinConfig::default());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn graphics_config_roundtrip() {
        let path = temp_config("graphics_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置
        assert_eq!(load_graphics_from(&path), GraphicsConfig::default());

        // 写入并读回
        let cfg = GraphicsConfig {
            default: Some(false),
            borderless: Some(true),
            fullscreen: Some(false),
            resolution: Some("1920x1080".into()),
            render_scale: Some(true),
            render_scale_level: Some("2x".into()),
            msaa: Some(true),
            msaa_level: Some("4x".into()),
            af: Some(false),
            af_level: Some("16x".into()),
        };
        save_graphics_to(&path, &cfg).unwrap();
        assert_eq!(load_graphics_from(&path), cfg);

        // 覆盖为默认：全部键写空回到未设置
        save_graphics_to(&path, &GraphicsConfig::default()).unwrap();
        assert_eq!(load_graphics_from(&path), GraphicsConfig::default());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn input_config_roundtrip() {
        let path = temp_config("input_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置
        assert_eq!(load_input_from(&path), InputConfig::default());

        // 写入并读回
        let cfg = InputConfig {
            kind: Some("dinput".into()),
            wheel_device: Some(r"\\?\HID#VID_044F".into()),
            pedal_device: Some(r"\\?\HID#VID_044F&MI_01".into()),
            shifter_device: Some(r"\\?\HID#VID_044F&MI_02".into()),
            throttle_axis: Some("Y".into()),
            brake_axis: Some("RZ".into()),
            shift_up: Some(5),
            shift_down: Some(6),
            start_button: Some(9),
            view_button: Some(10),
            coin_button: Some(11),
            card_button: Some(12),
            up_button: Some(13),
            down_button: Some(14),
            left_button: Some(15),
            right_button: Some(16),
            ratio: Some(128),
            pad_index: Some(2),
            shifter_type: Some("h".into()),
            gear1_button: Some(17),
            gear2_button: Some(18),
            gear3_button: Some(19),
            gear4_button: Some(20),
            gear5_button: Some(21),
            gear6_button: Some(22),
            reverse_button: Some(23),
            reverse_accel_axis: Some(true),
            reverse_brake_axis: Some(false),
            dual_stick: Some(true),
            linear_steering: Some(false),
            left_deadzone: Some(15),
            right_deadzone: Some(25),
            ffb_enable: Some(true),
            ffb_constant_force: Some(80),
            ffb_damper: Some(70),
            ffb_rumble: Some(60),
            ffb_rumble_duration: Some(90),
            ffb_base_damper: Some(10),
            ffb_dead_band: Some(15),
            xi_start: Some("A".into()),
            xi_view: Some("B".into()),
            xi_up: Some("DPAD_UP".into()),
            xi_down: Some("DPAD_DOWN".into()),
            xi_left: Some("DPAD_LEFT".into()),
            xi_right: Some("DPAD_RIGHT".into()),
            xi_shift_dn: Some("LB".into()),
            xi_shift_up: Some("RB".into()),
            xi_coin: Some("Y".into()),
            xi_card: Some("START".into()),
            xi_accel: Some("RT".into()),
            xi_brake: Some("LT".into()),
        };
        save_input_to(&path, &cfg).unwrap();
        assert_eq!(load_input_from(&path), cfg);

        // 覆盖为默认：全部键写空回到未设置
        save_input_to(&path, &InputConfig::default()).unwrap();
        assert_eq!(load_input_from(&path), InputConfig::default());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn room_config_nickname_roundtrip() {
        let path = temp_config("room_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置（含 nickname）
        assert_eq!(load_room_from(&path), RoomConfig::default());

        // 写入并读回
        let cfg = RoomConfig {
            directory: Some("https://dir.example/roomapi".into()),
            awl_path: Some("C:/tools/awl-tray.exe".into()),
            last_room_id: Some("ABCD1234".into()),
            last_room_password: Some("pw".into()),
            nickname: Some("玩家一号".into()),
        };
        save_room_to(&path, &cfg).unwrap();
        assert_eq!(load_room_from(&path), cfg);

        // 覆盖为默认：全部键写空回到未设置
        save_room_to(&path, &RoomConfig::default()).unwrap();
        assert_eq!(load_room_from(&path), RoomConfig::default());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn network_config_roundtrip() {
        let path = temp_config("network_roundtrip.ini");
        fs::remove_file(&path).ok();

        // 键缺失 → 全部未设置（模拟网络未设置视为开启）
        assert_eq!(load_network_from(&path), NetworkConfig::default());

        // 写入并读回
        let cfg = NetworkConfig {
            adapter: Some("{GUID}".into()),
            ip: Some("192.168.100.0/24".into()),
            simulate: Some(false),
            server: Some("my.server.net".into()),
            host_ip: Some("192.168.100.50".into()),
            serverbox_ip: Some("192.168.100.101".into()),
        };
        save_network_to(&path, &cfg).unwrap();
        assert_eq!(load_network_from(&path), cfg);

        // 覆盖为默认：全部键写空回到未设置
        save_network_to(&path, &NetworkConfig::default()).unwrap();
        assert_eq!(load_network_from(&path), NetworkConfig::default());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn game_record_legacy_and_roundtrip() {
        // 旧版 library.json 记录没有 serverbox / reader 字段，应可解析且为 None
        let legacy = r#"{"id":"a","path":"C:/g/game.exe","name":"D8 JPN","crc":"0x1fd6136a","version":"2.01","arch":"x64","loader":"mice","image":null,"play_count":0,"last_played":null,"added_at":1700000000}"#;
        let rec: GameRecord = serde_json::from_str(legacy).expect("旧档无新字段应可解析");
        assert_eq!(rec.serverbox, None);
        assert_eq!(rec.reader, None);

        // None 序列化时不落字段，保持旧档干净
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("serverbox"));
        assert!(!json.contains("reader"));

        // 配置后往返一致
        let mut rec2 = rec.clone();
        rec2.serverbox = Some("C:/sb/server.exe".into());
        rec2.reader = Some("C:/g/picodaemon.exe".into());
        let json2 = serde_json::to_string(&rec2).unwrap();
        assert!(json2.contains("serverbox"));
        let back: GameRecord = serde_json::from_str(&json2).expect("配置后的记录可读回");
        assert_eq!(back.serverbox.as_deref(), Some("C:/sb/server.exe"));
        assert_eq!(back.reader.as_deref(), Some("C:/g/picodaemon.exe"));
    }

    #[test]
    fn game_record_card_is_per_game_and_legacy_safe() {
        // 旧档无 card 字段应解析为 None（兼容升级前数据）
        let legacy = r#"{"id":"a","path":"C:/g/game.exe","name":"D8 JPN","crc":"0x1fd6136a","version":"2.01","arch":"x64","loader":"mice","play_count":0,"last_played":null,"added_at":1700000000}"#;
        let rec: GameRecord = serde_json::from_str(legacy).expect("旧档无 card 应可解析");
        assert_eq!(rec.card, None);

        // None 序列化时不落字段，保持旧档干净
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("\"card\""), "card=None 不应序列化");

        // 配置后往返一致
        let mut rec2 = rec.clone();
        rec2.card = Some("InidCrd01234.png".into());
        let json2 = serde_json::to_string(&rec2).unwrap();
        assert!(json2.contains("\"card\":\"InidCrd01234.png\""));
        let back: GameRecord = serde_json::from_str(&json2).expect("配置 card 的记录可读回");
        assert_eq!(back.card.as_deref(), Some("InidCrd01234.png"));
    }
}
