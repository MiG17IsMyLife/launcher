#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::os::windows::ffi::OsStrExt;

mod awl;
mod cards;
mod db;
mod i18n;
mod detect;
mod filedialog;
mod icf;
mod ini;
mod initiald;
mod kitten;
mod launch;
mod mice;
mod netinfo;
mod rawinput;
mod reactor_app;
mod roomdir;
mod segatools;
mod server;
mod storage;
mod update;
mod vpn;
mod win_util;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, SW_SHOWNORMAL};
use windows::core::PCWSTR;

/// 当前进程是否已提权（持有管理员令牌）。
fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// 以管理员身份重新拉起自身（runas，弹一次 UAC）。成功发起提权即退出当前进程。
fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe_wide: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb_wide = crate::win_util::to_wide_null("runas");
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: windows::core::PCWSTR::from_raw(verb_wide.as_ptr()),
            lpFile: windows::core::PCWSTR::from_raw(exe_wide.as_ptr()),
            lpParameters: windows::core::PCWSTR::null(),
            nShow: SW_SHOWNORMAL.0 as i32,
            ..Default::default()
        };
        if ShellExecuteExW(&mut info).is_ok() {
            // 提权实例已启动，退出当前未提权实例
            std::process::exit(0);
        }
    }
    false
}

/// 框架依赖模式：激活本机已安装的 Windows App SDK 运行时（自包含模式无需此步）。
/// 未安装运行时则弹系统错误框说明——windows_subsystem 程序 panic 不可见，必须显式提示。
fn bootstrap_runtime() {
    if let Err(e) = windows_reactor::bootstrap() {
        // 带上具体失败码：0x80070490=找不到匹配版本的运行时包（多为版本过旧），
        // 0x8007007e=文件缺失；笼统的「未安装」提示对排障没有帮助
        let msg: String = format!(
            "启动失败：Windows App SDK 运行时初始化失败。\n\n\
             错误信息：{e}\n\n\
             最常见原因：本机安装的运行时版本过旧（launcher 需要 2.x 版本族）。\n\
             请安装最新版 Windows App SDK 运行时（安装时选择「为所有用户安装」）后重试。"
        );
        let msg_wide = crate::win_util::to_wide_null(&msg);
        let title_wide = crate::win_util::to_wide_null("Sasayaki launcher");
        unsafe {
            MessageBoxW(
                None,
                PCWSTR::from_raw(msg_wide.as_ptr()),
                PCWSTR::from_raw(title_wide.as_ptr()),
                MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}

/// 房间看门狗（独立子进程）入口。
///
/// 由房主启动器以 `--room-watchdog <parent_pid> <room_id> <keychip> <peer_id> <directory_url>`
/// 拉起：以 `OpenProcess(SYNCHRONIZE)` 打开父进程并永久等待；父进程无论正常退出、被强杀
/// 还是崩溃都会终止并信号该句柄，`WaitForSingleObject` 返回后立即调用目录服务 `close`
/// 注销房间（幂等，已关闭返回 404 无害；带简要重试抗瞬时网络抖动）。
///
/// 看门狗是独立进程且不在 awl 作业对象内，因此启动器被强杀时仍可存活完成任务。
fn run_room_watchdog(parent_pid: u32, room_id: &str, keychip: &str, peer_id: &str, directory: &str) {
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) {
            // 父进程终止即返回（正常退出 / TerminateProcess / 崩溃均适用）
            let _ = WaitForSingleObject(h, 0xFFFF_FFFF); // INFINITE
            let _ = CloseHandle(h);
        }
    }
    // 父进程已退出 → 注销房间。close 仅需 keychip 即可放行（peer_id 缺失也能关房）。
    let client = crate::roomdir::RoomDirClient::new(directory);
    let peer = if peer_id.is_empty() { None } else { Some(peer_id) };
    for _ in 0..3 {
        match client.close(room_id, peer, keychip) {
            Ok(()) => break,
            Err(_) => std::thread::sleep(std::time::Duration::from_secs(1)),
        }
    }
}

fn main() {
    // 房间看门狗模式：被房主启动器拉起，仅监控父进程并在其退出（正常关闭 / 被强杀 /
    // 崩溃）后注销房间。必须放在提权重拉起逻辑之前，否则看门狗会被当作未提权实例
    // 自我重拉起，造成无限循环。看门狗本就不加载 GUI、不调用 bootstrap_runtime。
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--room-watchdog") {
        if let Some(p) = args.get(pos + 1..pos + 6) {
            run_room_watchdog(
                p[0].parse().unwrap_or(0),
                &p[1],
                &p[2],
                &p[3],
                &p[4],
            );
            return;
        }
    }
    // 方案1（运行时自提升）：若未提权，则以 runas 重新拉起自身，仅弹一次 UAC；
    // 提权后的进程及其子进程（openvpn / 游戏）均继承管理员令牌，连接/启动不再弹窗。
    // 用运行时提升而非 exe 清单，可避免 requireAdministrator 清单让 `cargo test` 无法运行。
    if !is_elevated() {
        let _ = relaunch_elevated();
    }
    // 启动前初始化界面语言：config.ini [ui] lang 已保存则沿用，否则按系统区域自动选择
    i18n::set_lang(storage::Store::new().load_lang());
    // 框架依赖模式：先激活本机 Windows App SDK 运行时，再启动 WinUI 界面
    bootstrap_runtime();
    windows_reactor::App::run_component::<reactor_app::Launcher>(()).unwrap();
}
