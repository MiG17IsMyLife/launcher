//! 原始输入（Raw Input）：
//! - 枚举本机 HID 游戏控制器（方向盘 / 手柄 / 摇杆，usage page 0x01，usage 4/5）
//! - 在后台线程里注册 RIDEV_INPUTSINK 原始输入窗口并捕获第一个新按下的按键，
//!   供「输入设置」的按键捕获弹窗使用

#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            HIDP_BUTTON_CAPS, HIDP_CAPS, HIDP_VALUE_CAPS, HidD_GetProductString,
            HidP_GetButtonCaps, HidP_GetCaps, HidP_GetUsageValue, HidP_GetUsages,
            HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
        },
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::Input::{
            GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RAWINPUT,
            RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK,
            RIDI_DEVICEINFO, RIDI_DEVICENAME, RIDI_PREPARSEDDATA, RIM_TYPEHID,
            RegisterRawInputDevices,
        },
        UI::Input::XboxController::{
            XInputGetCapabilities, XInputGetState, XINPUT_CAPABILITIES, XINPUT_FLAG_GAMEPAD,
            XINPUT_STATE, XUSER_MAX_COUNT,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PEEK_MESSAGE_REMOVE_TYPE,
            PeekMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
            WM_INPUT, WNDCLASSW,
        },
    },
    core::w,
};

use crate::t;
use crate::tf;

const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_JOYSTICK: u16 = 0x04;
const HID_USAGE_GAMEPAD: u16 = 0x05;
const HID_USAGE_PAGE_BUTTON: u16 = 0x09;
/// 十字键（Hat Switch）usage：属于 Generic Desktop 页，取值 0~7（8=无方向）
const HID_USAGE_HAT: u16 = 0x39;

/// 十字键方向编号（落库时由调用方加上 1000 以区别于按钮编号）
pub const DPAD_UP: u32 = 1;
pub const DPAD_RIGHT: u32 = 2;
pub const DPAD_DOWN: u32 = 3;
pub const DPAD_LEFT: u32 = 4;

/// HID hat 值 → 方向编号：0=上 2=右 4=下 6=左；斜向与中立位返回 None（不捕获）
fn hat_to_dpad(v: u32) -> Option<u32> {
    match v {
        0 => Some(DPAD_UP),
        2 => Some(DPAD_RIGHT),
        4 => Some(DPAD_DOWN),
        6 => Some(DPAD_LEFT),
        _ => None,
    }
}

/// 枚举到的游戏输入设备
#[derive(Clone, Debug)]
pub struct GameDevice {
    /// 接口设备路径（唯一，写入 config.ini）
    pub path: String,
    /// 显示名称（从路径截取的 VID/PID 实例段）
    pub label: String,
}

/// 捕获到的按键
#[derive(Clone, Debug)]
pub struct CapturedButton {
    /// 按键编号（HID Button usage ID，从 1 起）；十字键时为 DPAD_* 方向编号
    pub button: u32,
    /// 是否为十字键（Hat Switch）捕获；true 时 button 是方向编号而非按钮号
    pub is_dpad: bool,
}

/// 捕获到的模拟轴（油门 / 刹车等踏板）：只记录轴的友好名，不校验模拟量范围。
#[derive(Clone, Debug)]
pub struct CapturedAxis {
    /// 轴的友好名（X / Y / Rx / Ry / Slider …），仅用于界面显示
    pub name: String,
}

static CAPTURE_SLOT: OnceLock<Mutex<Option<CapturedButton>>> = OnceLock::new();
static PREV_STATE: OnceLock<Mutex<HashMap<isize, Vec<u16>>>> = OnceLock::new();
/// 各设备上一次的十字键（hat）值，用于识别“新推动的方向”
static HAT_STATE: OnceLock<Mutex<HashMap<isize, u32>>> = OnceLock::new();
static AXIS_SLOT: OnceLock<Mutex<Option<CapturedAxis>>> = OnceLock::new();
/// 各设备上一次每个模拟轴的当前值，用于识别“新移动的轴”
static AXIS_STATE: OnceLock<Mutex<HashMap<isize, Vec<i32>>>> = OnceLock::new();
static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

fn capture_slot() -> &'static Mutex<Option<CapturedButton>> {
    CAPTURE_SLOT.get_or_init(|| Mutex::new(None))
}

fn prev_state() -> &'static Mutex<HashMap<isize, Vec<u16>>> {
    PREV_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hat_state() -> &'static Mutex<HashMap<isize, u32>> {
    HAT_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn axis_slot() -> &'static Mutex<Option<CapturedAxis>> {
    AXIS_SLOT.get_or_init(|| Mutex::new(None))
}

fn axis_state() -> &'static Mutex<HashMap<isize, Vec<i32>>> {
    AXIS_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 模拟轴的友好名：Generic Desktop 轴 usage → 短名
fn axis_usage_name(usage: u16) -> String {
    let s = match usage {
        0x30 => "X",
        0x31 => "Y",
        0x32 => "Z",
        0x33 => "Rx",
        0x34 => "Ry",
        0x35 => "Rz",
        0x36 => "Slider",
        0x37 => "Dial",
        0x38 => "Wheel",
        _ => return format!("Axis{:03X}", usage),
    };
    s.to_string()
}

/// 模拟轴移动判定阈值：取逻辑量程的 1/32（至少 16），避免静止抖动误触发
fn axis_threshold(cap: &HIDP_VALUE_CAPS) -> i32 {
    let range = (cap.LogicalMax as i64 - cap.LogicalMin as i64).unsigned_abs() as i32;
    if range <= 0 {
        256
    } else {
        (range / 32).max(16)
    }
}

/// 从接口路径截取显示名称：`\\?\HID#VID_044F&PID_B108&MI_00#...` → `VID_044F&PID_B108&MI_00`
fn label_from_path(path: &str) -> String {
    let lower = path.to_lowercase();
    if let Some((_, rest)) = lower.split_once("hid#") {
        rest.split('#').next().unwrap_or(rest).to_uppercase()
    } else {
        t!("未知设备", "Unknown device").to_string()
    }
}

/// 读取 HID 设备的产品名（如 "Wireless Controller"）；失败返回 None。
///
/// 以 0 访问权限打开设备接口——不申请读写，因此不会独占设备、也不影响游戏使用；
/// 仅为调用 `HidD_GetProductString` 取名称。
unsafe fn hid_product_name(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = CreateFileW(
        windows::core::PCWSTR(wide.as_ptr()),
        0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    )
    .ok()?;
    let mut buf = [0u16; 128];
    let ok = HidD_GetProductString(handle, buf.as_mut_ptr() as *mut _, buf.len() as u32);
    let _ = CloseHandle(handle);
    if !ok {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let name = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// 按输入类型枚举设备：
/// - `xinput`：枚举已连接的 XInput 手柄（索引 0~3）
/// - `dinput`（及其它非空值）：枚举 HID 游戏控制器（原始输入）
/// - `None`（尚未选择类型）：不扫描，返回空列表，由界面提示先选类型
pub fn list_input_devices(kind: Option<&str>) -> Vec<GameDevice> {
    match kind {
        Some(k) if k.eq_ignore_ascii_case("xinput") => list_xinput_devices(),
        Some(_) => list_game_devices(),
        None => Vec::new(),
    }
}

/// 枚举已连接的 XInput 手柄。XInput 没有设备路径概念，只有槽位索引，
/// 因此 `GameDevice.path` 直接存索引数字（"0"~"3"），写入 config.ini 即可区分布局。
fn list_xinput_devices() -> Vec<GameDevice> {
    let mut devices = Vec::new();
    for index in 0..XUSER_MAX_COUNT {
        let mut caps: XINPUT_CAPABILITIES = unsafe { std::mem::zeroed() };
        // 返回 0（ERROR_SUCCESS）表示该槽位有已连接的手柄
        if unsafe { XInputGetCapabilities(index, XINPUT_FLAG_GAMEPAD, &mut caps) } != 0 {
            continue;
        }
        devices.push(GameDevice {
            path: index.to_string(),
            label: tf!("手柄 {}（XInput）", "Gamepad {} (XInput)", index + 1),
        });
    }
    devices
}

/// 捕获到的 XInput 手柄按键（记录 mice 规范按钮名，如 "A" / "DPAD_UP"）
#[derive(Clone, Debug)]
pub struct CapturedXiButton {
    /// mice 按钮名（A / B / X / Y / LB / RB / LT / RT / DPAD_UP / DPAD_DOWN /
    /// DPAD_LEFT / DPAD_RIGHT / START / BACK / LTHUMB / RTHUMB）
    pub name: String,
}

/// XInput 按键位 → mice 按钮名（与 mice idas_xi_button_names 的规范名一致）。
/// 注：GUIDE 仅 XInputGetStateEx 可报，标准 XInputGetState 无法捕获，故不在此列。
fn xi_button_name(mask: u16) -> Option<&'static str> {
    const DPAD_UP: u16 = 0x0001;
    const DPAD_DOWN: u16 = 0x0002;
    const DPAD_LEFT: u16 = 0x0004;
    const DPAD_RIGHT: u16 = 0x0008;
    const START: u16 = 0x0010;
    const BACK: u16 = 0x0020;
    const LEFT_THUMB: u16 = 0x0040;
    const RIGHT_THUMB: u16 = 0x0080;
    const LEFT_SHOULDER: u16 = 0x0100;
    const RIGHT_SHOULDER: u16 = 0x0200;
    const A: u16 = 0x1000;
    const B: u16 = 0x2000;
    const X: u16 = 0x4000;
    const Y: u16 = 0x8000;
    Some(match mask {
        DPAD_UP => "DPAD_UP",
        DPAD_DOWN => "DPAD_DOWN",
        DPAD_LEFT => "DPAD_LEFT",
        DPAD_RIGHT => "DPAD_RIGHT",
        START => "START",
        BACK => "BACK",
        LEFT_THUMB => "LTHUMB",
        RIGHT_THUMB => "RTHUMB",
        LEFT_SHOULDER => "LB",
        RIGHT_SHOULDER => "RB",
        A => "A",
        B => "B",
        X => "X",
        Y => "Y",
        _ => return None,
    })
}

/// 阻塞捕获 XInput 手柄（pad_index 槽位）上第一个新按下的按键，返回其 mice 按钮名。
/// 同时识别 LT/RT 触发器（由 0 变非 0）为 "LT"/"RT"。最长 timeout_ms 毫秒，超时返回 None。
/// 槽位无已连接手柄时（XInputGetState 返回非 0）持续等待直至超时。
pub fn capture_xi_button(pad_index: u32, timeout_ms: u32) -> Option<CapturedXiButton> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    let mut prev_buttons: u16 = 0;
    let mut prev_lt = false;
    let mut prev_rt = false;
    loop {
        let mut state: XINPUT_STATE = unsafe { std::mem::zeroed() };
        // 返回 0（ERROR_SUCCESS）表示该槽位有已连接的手柄且读取成功
        if unsafe { XInputGetState(pad_index, &mut state) } == 0 {
            // wButtons 为 XINPUT_GAMEPAD_BUTTON_FLAGS 新类型，取其内部 u16 以做位运算
            let w = state.Gamepad.wButtons.0;
            let newly = w & !prev_buttons;
            if newly != 0 {
                // 取首个新置位的 bit（按位序，优先级固定）
                for bit in 0..16u32 {
                    let m = 1u16 << bit;
                    if newly & m != 0 {
                        if let Some(name) = xi_button_name(m) {
                            return Some(CapturedXiButton { name: name.to_string() });
                        }
                    }
                }
            }
            prev_buttons = w;
            let lt = state.Gamepad.bLeftTrigger > 0;
            let rt = state.Gamepad.bRightTrigger > 0;
            if lt && !prev_lt {
                return Some(CapturedXiButton {
                    name: "LT".to_string(),
                });
            }
            if rt && !prev_rt {
                return Some(CapturedXiButton {
                    name: "RT".to_string(),
                });
            }
            prev_lt = lt;
            prev_rt = rt;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 枚举本机 HID 游戏控制器（方向盘 / 手柄 / 摇杆），用于 dinput。
pub fn list_game_devices() -> Vec<GameDevice> {
    unsafe { list_game_devices_impl() }
}

unsafe fn list_game_devices_impl() -> Vec<GameDevice> {
    let mut count = 0u32;
    let flags = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    // 第一次调用取数量
    if GetRawInputDeviceList(None, &mut count, flags) != 0 || count == 0 {
        return Vec::new();
    }
    let mut list = vec![
        RAWINPUTDEVICELIST {
            hDevice: HANDLE::default(),
            dwType: RIM_TYPEHID,
        };
        count as usize
    ];
    if GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, flags) == u32::MAX {
        return Vec::new();
    }

    let mut devices = Vec::new();
    for entry in list.iter().take(count as usize) {
        if entry.dwType != RIM_TYPEHID {
            continue;
        }
        let Some(path) = device_info_string(entry.hDevice, RIDI_DEVICENAME) else {
            continue;
        };
        // 设备信息（按 usage 过滤：仅游戏控制器）
        let mut info_bytes = vec![0u8; 32];
        let mut size = info_bytes.len() as u32;
        let n = GetRawInputDeviceInfoW(
            Some(entry.hDevice),
            RIDI_DEVICEINFO,
            Some(info_bytes.as_mut_ptr() as *mut _),
            &mut size,
        );
        if n == u32::MAX {
            continue;
        }
        // RID_DEVICE_INFO 布局：cbSize(4) + dwType(4) + union{ hid: vendor(4)+product(4)+version(4)+usagePage(2)+usage(2) }
        // 即 usagePage 在偏移 20、usage 在偏移 22。
        // 注意：早期版本误读 16..20（读到的是 versionNumber），导致 usage 恒为 0、一台设备都扫不到。
        let usage = &info_bytes[20..24];
        let usage_page = u16::from_le_bytes([usage[0], usage[1]]);
        let usage = u16::from_le_bytes([usage[2], usage[3]]);
        if usage_page != HID_USAGE_PAGE_GENERIC
            || (usage != HID_USAGE_JOYSTICK && usage != HID_USAGE_GAMEPAD)
        {
            continue;
        }
        devices.push(GameDevice {
            // 优先显示 HID 产品名；取不到再退回接口路径里的 VID/PID 段
            label: hid_product_name(&path).unwrap_or_else(|| label_from_path(&path)),
            path,
        });
    }
    // 同一物理设备的多个 HID 接口会读到同一个产品名，重名时补接口段便于区分
    let mut counts: HashMap<String, usize> = HashMap::new();
    for d in &devices {
        *counts.entry(d.label.clone()).or_default() += 1;
    }
    for d in devices.iter_mut() {
        if counts.get(&d.label).copied().unwrap_or(0) > 1 {
            d.label = format!("{}（{}）", d.label, label_from_path(&d.path));
        }
    }
    devices
}

/// 读取设备字符串信息（设备路径等）。
unsafe fn device_info_string(
    hdevice: HANDLE,
    command: windows::Win32::UI::Input::RAW_INPUT_DEVICE_INFO_COMMAND,
) -> Option<String> {
    let mut size = 0u32;
    if GetRawInputDeviceInfoW(Some(hdevice), command, None, &mut size) == u32::MAX || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; size as usize];
    let n = GetRawInputDeviceInfoW(
        Some(hdevice),
        command,
        Some(buf.as_mut_ptr() as *mut _),
        &mut size,
    );
    if n == u32::MAX {
        return None;
    }
    buf.truncate(n as usize);
    Some(String::from_utf16_lossy(&buf))
}

/// 捕获类型：决定等待“按键”还是“模拟轴”
enum CaptureKind {
    Button,
    Axis,
}

/// 阻塞捕获第一个新按下的按键（内部泵消息，最长 timeout_ms 毫秒）。
pub fn capture_button(timeout_ms: u32) -> Option<CapturedButton> {
    if unsafe { capture_impl(Duration::from_millis(timeout_ms as u64), CaptureKind::Button) } {
        capture_slot().lock().unwrap().take()
    } else {
        None
    }
}

/// 阻塞捕获第一个移动的模拟轴（复用同一套窗口 / 消息泵，只记录轴编号）。
pub fn capture_axis(timeout_ms: u32) -> Option<CapturedAxis> {
    if unsafe { capture_impl(Duration::from_millis(timeout_ms as u64), CaptureKind::Axis) } {
        axis_slot().lock().unwrap().take()
    } else {
        None
    }
}

unsafe fn capture_impl(timeout: Duration, kind: CaptureKind) -> bool {
    *capture_slot().lock().unwrap() = None;
    *axis_slot().lock().unwrap() = None;
    prev_state().lock().unwrap().clear();
    hat_state().lock().unwrap().clear();
    axis_state().lock().unwrap().clear();

    let hinstance = match GetModuleHandleW(None).ok() {
        Some(h) => h,
        None => return false,
    };
    // 注册窗口类（全局一次；重复注册失败可忽略）
    if CLASS_REGISTERED.set(()).is_ok() {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(capture_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("DnetRawInputCapture"),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }

    let hwnd = match CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("DnetRawInputCapture"),
        w!("Dnet 按键捕获"),
        WINDOW_STYLE(0),
        0,
        0,
        0,
        0,
        None,
        None,
        Some(hinstance.into()),
        None,
    )
    .ok()
    {
        Some(h) => h,
        None => return false,
    };

    // 注册摇杆 + 手柄两类 usage，INPUTSINK 允许窗口非前台时也接收输入
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_JOYSTICK,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GAMEPAD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];
    // 注意：cbSize 必须是「单个 RAWINPUTDEVICE 结构体」的大小，传数组总长会得到
    // E_INVALIDARG（0x80070057），注册失败会导致捕获立刻超时。
    if RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err() {
        return false;
    }

    let deadline = Instant::now() + timeout;
    let mut msg = MSG::default();
    loop {
        // 泵完当前队列里的消息
        while PeekMessageW(
            &mut msg,
            Some(hwnd),
            0,
            0,
            PEEK_MESSAGE_REMOVE_TYPE(1), // PM_REMOVE
        )
        .as_bool()
        {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // 捕获到目标类型则取出返回
        let filled = match kind {
            CaptureKind::Button => capture_slot().lock().unwrap().is_some(),
            CaptureKind::Axis => axis_slot().lock().unwrap().is_some(),
        };
        if filled {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

unsafe extern "system" fn capture_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        handle_raw_input(lparam);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 解析 WM_INPUT：找出本报告中新按下的按键，写入捕获槽位。
unsafe fn handle_raw_input(lparam: LPARAM) {
    let hraw = windows::Win32::UI::Input::HRAWINPUT(lparam.0 as *mut _);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    if GetRawInputData(hraw, RID_INPUT, None, &mut size, header_size) == u32::MAX || size == 0 {
        return;
    }
    let mut buf = vec![0u8; size as usize];
    if GetRawInputData(
        hraw,
        RID_INPUT,
        Some(buf.as_mut_ptr() as *mut _),
        &mut size,
        header_size,
    ) == u32::MAX
    {
        return;
    }
    let raw = &*(buf.as_ptr() as *const RAWINPUT);
    if raw.data.hid.dwCount == 0 {
        return;
    }
    let hdevice = raw.header.hDevice;
    // bRawData 位于 RAWINPUTHEADER + RAWHID 头（dwSizeHid(4) + dwCount(4)）之后。
    // 这里必须是相对 buf 起始的“偏移量”；早期版本算成了绝对地址，
    // 导致 `report_end > buf.len()` 恒成立、每次输入都被判为越界而丢弃。
    let report_len = raw.data.hid.dwSizeHid as usize;
    let report_off = std::mem::offset_of!(RAWINPUT, data) + 8;

    // 预解析数据（HID 报告描述符的解析结果）
    let mut psize = 0u32;
    if GetRawInputDeviceInfoW(Some(hdevice), RIDI_PREPARSEDDATA, None, &mut psize) == u32::MAX
        || psize == 0
    {
        return;
    }
    let mut pbuf = vec![0u8; psize as usize];
    if GetRawInputDeviceInfoW(
        Some(hdevice),
        RIDI_PREPARSEDDATA,
        Some(pbuf.as_mut_ptr() as *mut _),
        &mut psize,
    ) == u32::MAX
    {
        return;
    }
    let preparsed = PHIDP_PREPARSED_DATA(pbuf.as_mut_ptr() as isize);

    // 按键容量（没有按钮的设备——如纯踏板盒——也要继续走轴解析）
    let mut caps: HIDP_CAPS = std::mem::zeroed();
    if HidP_GetCaps(preparsed, &mut caps).is_err() {
        return;
    }
    let mut bcaps =
        vec![std::mem::zeroed::<HIDP_BUTTON_CAPS>(); caps.NumberInputButtonCaps as usize];
    let mut bcount = caps.NumberInputButtonCaps;
    if bcount > 0 && HidP_GetButtonCaps(HidP_Input, bcaps.as_mut_ptr(), &mut bcount, preparsed).is_err() {
        return;
    }

    // 当前按下的按键列表（Button usage ID）
    let report_end = report_off + report_len;
    if report_end > buf.len() {
        return;
    }
    let report = &mut buf[report_off..report_end];
    // 当前按下的按键（Button usage ID）：按每个 button cap 的 link collection 逐一取。
    // 只查顶层 collection（0）会漏掉子集合里的按键（多见于方向盘 / 飞行杆）。
    let mut usages: Vec<u16> = Vec::new();
    for cap in bcaps.iter() {
        if cap.UsagePage != HID_USAGE_PAGE_BUTTON {
            continue;
        }
        let mut list = vec![0u16; 128];
        let mut ulen = list.len() as u32;
        if HidP_GetUsages(
            HidP_Input,
            HID_USAGE_PAGE_BUTTON,
            Some(cap.LinkCollection),
            list.as_mut_ptr(),
            &mut ulen,
            preparsed,
            report,
        )
        .is_err()
        {
            continue;
        }
        list.truncate(ulen as usize);
        for u in list {
            if !usages.contains(&u) {
                usages.push(u);
            }
        }
    }

    // 与上一次状态对比，找出新按下的键
    let key = hdevice.0 as isize;
    let newly = {
        let mut prev = prev_state().lock().unwrap();
        let old = prev.entry(key).or_default();
        let newly: Vec<u16> = usages
            .iter()
            .filter(|u| !old.contains(u))
            .copied()
            .collect();
        *old = usages;
        newly
    };
    if !newly.is_empty() {
        let mut slot = capture_slot().lock().unwrap();
        if slot.is_none() {
            *slot = Some(CapturedButton {
                button: newly[0] as u32,
                is_dpad: false,
            });
        }
    }

    // 值容量（十字键 + 模拟轴）：枚举一次，下面分别处理 hat 与模拟轴
    let mut vcaps = vec![std::mem::zeroed::<HIDP_VALUE_CAPS>(); caps.NumberInputValueCaps as usize];
    let mut vcount = caps.NumberInputValueCaps;
    if HidP_GetValueCaps(HidP_Input, vcaps.as_mut_ptr(), &mut vcount, preparsed).is_err() {
        return;
    }
    let vcaps = &vcaps[..vcount as usize];

    // 十字键（Hat Switch，usage 0x39，值 0~7）。
    // 很多手柄的十字键不是按钮而是 hat，早期只读 button 所以按十字键没反应。
    let mut hat: Option<u32> = None;
    for cap in vcaps.iter() {
        if cap.UsagePage != HID_USAGE_PAGE_GENERIC {
            continue;
        }
        let (min, max) = if cap.IsRange {
            (cap.Anonymous.Range.UsageMin, cap.Anonymous.Range.UsageMax)
        } else {
            let u = cap.Anonymous.NotRange.Usage;
            (u, u)
        };
        if !(min..=max).contains(&HID_USAGE_HAT) {
            continue;
        }
        let mut value: u32 = 0;
        if HidP_GetUsageValue(
            HidP_Input,
            HID_USAGE_PAGE_GENERIC,
            Some(cap.LinkCollection),
            HID_USAGE_HAT,
            &mut value,
            preparsed,
            report,
        )
        .is_ok()
        {
            hat = Some(value);
            break;
        }
    }
    if let Some(v) = hat {
        // 与上一次的 hat 值对比，只有“新推到的方向”才算一次捕获（斜向与中立位不捕获）
        let changed = hat_state().lock().unwrap().insert(key, v) != Some(v);
        if let Some(dir) = hat_to_dpad(v) {
            if changed {
                let mut slot = capture_slot().lock().unwrap();
                if slot.is_none() {
                    *slot = Some(CapturedButton {
                        button: dir,
                        is_dpad: true,
                    });
                }
            }
        }
    }

    // ---- 模拟轴（油门 / 刹车等踏板）：记录第一个移动的轴编号 ----
    // 轴编号 = 本设备 Generic Desktop 值容量中、排除十字键后的出现次序（0 起）。
    let mut axis_prev = axis_state().lock().unwrap();
    let axis_vec = axis_prev.entry(key).or_default();
    let mut axis_index: u32 = 0;
    for cap in vcaps.iter() {
        if cap.UsagePage != HID_USAGE_PAGE_GENERIC {
            continue;
        }
        let (min, max) = if cap.IsRange {
            (cap.Anonymous.Range.UsageMin, cap.Anonymous.Range.UsageMax)
        } else {
            let u = cap.Anonymous.NotRange.Usage;
            (u, u)
        };
        if (min..=max).contains(&HID_USAGE_HAT) {
            // 十字键占一个“槽”，但跳过（不算模拟轴）
            continue;
        }
        let usage = min; // 取范围内首个 usage
        let idx = axis_index as usize;
        axis_index += 1;
        let mut value: u32 = 0;
        if HidP_GetUsageValue(
            HidP_Input,
            HID_USAGE_PAGE_GENERIC,
            Some(cap.LinkCollection),
            usage,
            &mut value,
            preparsed,
            report,
        )
        .is_err()
        {
            continue;
        }
        let v = value as i32;
        let threshold = axis_threshold(cap);
        // 首份样本只记录基线，避免静止位被误判为移动
        let moved = match axis_vec.get(idx).copied() {
            Some(p) => (v - p).abs() >= threshold,
            None => false,
        };
        if moved {
            let mut slot = axis_slot().lock().unwrap();
            if slot.is_none() {
                *slot = Some(CapturedAxis {
                    name: axis_usage_name(usage),
                });
            }
        }
        // 更新该轴基线
        if idx >= axis_vec.len() {
            axis_vec.resize(idx + 1, v);
        }
        axis_vec[idx] = v;
    }
}



