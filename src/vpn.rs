//! VPN 子进程管理：以 openvpn.exe 子进程方式拉起 OpenVPN，喂入私钥密码，
//! 通过 wintun 驱动创建虚拟网卡（wintun.dll 需与 openvpn.exe 同目录）。
//!
//! 设计要点：
//! - 连接成功后返回 `VpnHandle`，其内部持有子进程；`Drop` 时自动 kill 并删除临时密码文件。
//! - `connect_blocking` 同步阻塞直到连接成功/失败/超时，供「启动游戏后台任务」与「手动连接」复用。

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use crate::storage::VpnConfig;
use crate::t;
use crate::tf;

/// VPN 连接状态（状态栏 / 设置页展示）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VpnStatus {
    /// 未配置（无 .ovpn）
    NotConfigured,
    /// 已断开
    Disconnected,
    /// 连接中
    Connecting,
    /// 已连接
    Connected,
    /// 连接失败
    Failed,
}

/// 持有 openvpn 子进程；析构时 kill 进程、关闭作业对象并删除临时密码/日志文件。
///
/// `job` 为作业对象句柄的裸值（以 `usize` 存储以保证 `Send`，便于跨线程持有），
/// 创建时设置了 `KILL_ON_JOB_CLOSE`：只要本 launcher 进程退出（正常关闭、崩溃或被强杀），
/// 系统都会自动终止其中的 openvpn，避免连接残留。
pub struct VpnHandle {
    child: Child,
    passfile: PathBuf,
    /// openvpn `--log` 日志文件。连接期间 openvpn 一直握着该句柄，当场删除会因
    /// Windows 共享冲突失败并残留含成功标志的旧内容（下次连接会被误读为已连接），
    /// 因此留到 Drop 中、待进程退出句柄释放后再删。
    logfile: PathBuf,
    job: Option<usize>,
}

impl Drop for VpnHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        close_job(self.job);
        if !self.passfile.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.passfile);
        }
        if !self.logfile.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.logfile);
        }
    }
}

impl VpnHandle {
    /// 进程是否仍在运行（监督线程轮询用；不影响句柄生命周期）。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 进程已退出后，从 `--log` 日志提炼一句死亡原因（复用连接期的错误提取逻辑）。
    pub fn exit_detail(&mut self) -> String {
        build_detail(&read_log(&self.logfile))
    }
}

/// openvpn 管理入口（全部为静态方法，无需实例化）。
pub struct VpnManager;

impl VpnManager {
    /// 解析 openvpn.exe 路径：优先配置项，缺省 `launcher/tools/openvpn/openvpn.exe`。
    pub fn openvpn_path(cfg: &VpnConfig) -> PathBuf {
        if let Some(p) = &cfg.openvpn_path {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
        crate::storage::launcher_dir()
            .join("tools")
            .join("openvpn")
            .join("openvpn.exe")
    }

    /// 启动 openvpn 并阻塞等待连接成功，返回持有子进程的的连接句柄。
    ///
    /// - 私钥密码写入临时文件，通过 `--askpass` 与 `--auth-user-pass` 同时喂入，
    ///   兼顾「加密私钥口令」与「服务器账号密码」两种常见认证方式（避免 openvpn 在
    ///   stdin 上等待账号密码而挂起）。
    /// - windows 驱动自适应：同目录存在 `wintun.dll` 时优先 wintun，否则回退 `tap-windows6`。
    pub fn connect_blocking(
        cfg: &VpnConfig,
        passphrase: Option<&str>,
        timeout: Duration,
    ) -> Result<VpnHandle, String> {
        let exe = Self::openvpn_path(cfg);
        if !exe.is_file() {
            return Err(tf!("找不到 openvpn.exe：{}\n（请将 openvpn.exe 放入 tools/openvpn/，或在 config.ini 的 [vpn] 设置 openvpn= 绝对路径）",
                   "openvpn.exe not found: {}\n(Put openvpn.exe under tools/openvpn/, or set openvpn= absolute path in config.ini [vpn])",
                exe.display()
            ));
        }
        let config = match &cfg.config_path {
            Some(c) => PathBuf::from(c),
            None => return Err(t!("未配置 VPN（缺少 .ovpn 文件）", "VPN not configured (missing .ovpn file)").to_string()),
        };
        if !config.is_file() {
            return Err(tf!("VPN 配置文件不存在：{}", "VPN config file does not exist: {}", config.display()));
        }

        // 私钥密码写入临时文件。写入两行相同密码：
        // - --askpass 取首行作为私钥口令；
        // - --auth-user-pass 取「用户名=密码」= 同一密码。
        let passfile = passphrase
            .filter(|s| !s.is_empty())
            .map(|p| {
                let tmp =
                    std::env::temp_dir().join(format!("dnet-vpn-{}.tmp", std::process::id()));
                let mut f = fs::File::create(&tmp)
                    .map_err(|e| tf!("创建临时密码文件失败：{e}", "Failed to create temporary password file: {e}"))?;
                writeln!(f, "{p}").map_err(|e| tf!("写入临时密码失败：{e}", "Failed to write temporary password: {e}"))?;
                writeln!(f, "{p}").map_err(|e| tf!("写入临时密码失败：{e}", "Failed to write temporary password: {e}"))?;
                Ok::<_, String>(tmp)
            })
            .transpose()?;

        // 候选 windows 驱动：wintun.dll 在场优先 wintun，否则优先 tap-windows6；
        // 任一种因驱动缺失/不被支持失败时回退到另一种，最后回退到「不指定驱动」（兼容老版本 openvpn）。
        let has_wintun = exe
            .parent()
            .map(|d| d.join("wintun.dll"))
            .map(|p| p.is_file())
            .unwrap_or(false);
        let mut drivers: Vec<Option<&str>> = if has_wintun {
            vec![Some("wintun"), Some("tap-windows6")]
        } else {
            vec![Some("tap-windows6"), Some("wintun")]
        };
        drivers.push(None);

        let mut last_err = t!("VPN 连接失败", "VPN connection failed").to_string();
        for driver in &drivers {
            match Self::try_connect_once(&exe, &config, &passfile, *driver, timeout) {
                Ok(handle) => {
                    // 密码文件保留到 VpnHandle::drop 再删：TLS 重协商（reneg-sec，默认每
                    // 小时一次）与 ping-restart 软重启时 openvpn 会再次读取认证凭据
                    // （auth-nocache 场景下文件被删会导致再认证失败，隧道约每小时掉一次；
                    // 官方 GUI 经 management 接口按需提供凭据故无此问题）
                    return Ok(handle);
                }
                Err(e) => {
                    last_err = e;
                    let lo = last_err.to_lowercase();
                    // 仅当失败与驱动选项相关时才尝试下一个驱动，否则直接返回
                    let is_driver_issue = driver.is_some()
                        && (lo.contains("wintun")
                            || lo.contains("tap")
                            || lo.contains("driver")
                            || lo.contains("unrecognized option")
                            || lo.contains("options error"));
                    if !is_driver_issue {
                        break;
                    }
                }
            }
        }
        if let Some(pf) = &passfile {
            let _ = fs::remove_file(pf);
        }
        Err(last_err)
    }

    /// 单次连接尝试（指定 windows 驱动，`None` 表示不指定、由 openvpn 自选）。
    /// 成功返回句柄，失败返回包含 openvpn 关键错误的文案。
    fn try_connect_once(
        exe: &PathBuf,
        config: &PathBuf,
        passfile: &Option<PathBuf>,
        driver: Option<&str>,
        timeout: Duration,
    ) -> Result<VpnHandle, String> {
        // 用 openvpn 自身的 --log 写日志文件：openvpn 逐行 flush，可绕开「管道下 stderr
        // 全缓冲导致成功标志行滞留在缓冲区、进程退出才刷出」的问题（该问题会让连接判定超时并误杀已建立的隧道）。
        let logfile = std::env::temp_dir().join(format!(
            "dnet-vpn-{}-{}.log",
            std::process::id(),
            driver.unwrap_or("default")
        ));
        // 文件名按（launcher PID, 驱动）固定，上一次成功连接可能残留含成功标志的旧日志
        //（成功时 openvpn 仍握着句柄、当场删不掉），必须先删掉：否则 spawn 后首轮轮询会
        // 在新 openvpn 截断文件之前读到旧内容，把上一次的 "initialization sequence
        // completed" 误判成本次连接成功。
        let _ = fs::remove_file(&logfile);
        let mut cmd = Command::new(exe);
        cmd.arg("--config")
            .arg(config)
            .arg("--suppress-timestamps")
            .arg("--verb")
            .arg("3")
            .arg("--log")
            .arg(&logfile);
        if let Some(d) = driver {
            cmd.arg("--windows-driver").arg(d);
        }
        // 配置文件里没有任何保活指令时注入默认 keepalive：命令行选项覆盖配置文件，
        // 服务端 push 的 keepalive 仍优先。缺保活的 UDP 隧道挂机时会被 NAT 静默掐断
        // 且 openvpn 无从发现；注入后 ping-restart 触发进程内软重启即可自愈重建。
        if needs_keepalive(config) {
            cmd.arg("--ping").arg("10").arg("--ping-restart").arg("60");
        }
        if let Some(pf) = passfile {
            // 同一份密码文件同时喂给两种认证方式
            cmd.arg("--askpass").arg(pf);
            cmd.arg("--auth-user-pass").arg(pf);
        }
        // 日志已交给 --log 文件，控制台不再需要
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        // 隐藏 openvpn 的控制台窗口（避免连接时弹出黑框）
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd
            .spawn()
            .map_err(|e| tf!("启动 openvpn 失败（{}）：{e}", "Failed to start openvpn ({}): {e}", exe.display()))?;

        // 将 openvpn 纳入作业对象：launcher 进程退出时系统自动终止它（避免连接残留）
        let job = attach_job(&child);

        let deadline = Instant::now() + timeout;
        let marker = "initialization sequence completed";
        let driver_label = driver.unwrap_or(t!("默认", "Default"));
        loop {
            // 进程已自行退出（连接失败/配置错误）
            if let Ok(Some(_)) = child.try_wait() {
                std::thread::sleep(Duration::from_millis(200));
                let detail = build_detail(&read_log(&logfile));
                let _ = child.wait();
                close_job(job);
                let _ = fs::remove_file(&logfile);
                return Err(append_wintun_hint(&tf!("VPN 连接失败（驱动 {driver_label}）：{detail}", "VPN connection failed (driver {driver_label}): {detail}"
                )));
            }

            // 成功标志：openvpn 日志文件出现该字样即表示隧道已建立
            {
                let lines = read_log(&logfile);
                if lines.iter().any(|l| l.to_lowercase().contains(marker)) {
                    // 二次确认：防「init 完成后随即崩溃」的窗口期误报成功
                    std::thread::sleep(Duration::from_millis(300));
                    if let Ok(Some(_)) = child.try_wait() {
                        let detail = build_detail(&read_log(&logfile));
                        let _ = child.wait();
                        close_job(job);
                        let _ = fs::remove_file(&logfile);
                        return Err(append_wintun_hint(&tf!("VPN 连接失败（驱动 {driver_label}）：{detail}", "VPN connection failed (driver {driver_label}): {detail}"
                        )));
                    }
                    // 日志不在此处删除（openvpn 仍握着句柄删不掉），交给 VpnHandle::Drop
                    return Ok(VpnHandle {
                        child,
                        passfile: passfile.clone().unwrap_or_else(|| PathBuf::new()),
                        logfile,
                        job,
                    });
                }
            }

            if Instant::now() >= deadline {
                let detail = build_detail(&read_log(&logfile));
                let _ = child.kill();
                let _ = child.wait();
                close_job(job);
                let _ = fs::remove_file(&logfile);
                return Err(append_wintun_hint(&tf!("连接超时（{} 秒，驱动 {driver_label}）：{detail}", "Connection timed out ({} seconds, driver {driver_label}): {detail}",
                    timeout.as_secs()
                )));
            }

            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// .ovpn 是否缺少保活指令：跳过 `#`/`;` 注释行与 `<xxx>…</xxx>` 内联块后，
/// 出现 ping / ping-restart / ping-exit / keepalive / inactive 任一即视为已有。
/// 读不到文件按「缺保活」处理——多注入一份默认值无害（服务端 push 仍优先）。
fn needs_keepalive(config: &PathBuf) -> bool {
    let text = match fs::read_to_string(config) {
        Ok(s) => s,
        Err(_) => return true,
    };
    let mut inline = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('<') {
            // 进入 <tag> 内联块后跳过内容行，</tag> 结束恢复扫描
            inline = !l.starts_with("</");
            continue;
        }
        if inline || l.starts_with('#') || l.starts_with(';') {
            continue;
        }
        let first = l.split_whitespace().next().unwrap_or("");
        if matches!(first, "ping" | "ping-restart" | "ping-exit" | "keepalive" | "inactive") {
            return false;
        }
    }
    true
}

/// 读取 openvpn --log 日志文件内容（文件被另一进程占用或尚未创建时返回空）。
fn read_log(path: &PathBuf) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(s) => s.lines().map(|l| l.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 将子进程纳入作业对象，并设置 `KILL_ON_JOB_CLOSE`：只要持有作业句柄的 launcher
/// 进程退出（正常关闭、崩溃、被强杀），系统都会自动终止其中的 openvpn，避免连接残留。
/// 返回作业句柄的裸值（`usize`，便于 `Send`），失败返回 `None` 降级处理。
#[cfg(windows)]
pub(crate) fn attach_job(child: &Child) -> Option<usize> {
    use std::os::windows::io::AsRawHandle;
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }
        let ph = HANDLE(child.as_raw_handle() as *mut core::ffi::c_void);
        if AssignProcessToJobObject(job, ph).is_err() {
            let _ = CloseHandle(job);
            return None;
        }
        // 转为裸值以便 `Send`（HANDLE 原始指针类型本身不满足 Send）
        Some(job.0 as usize)
    }
}

/// 关闭作业对象句柄（触发 `KILL_ON_JOB_CLOSE`，终止其中的 openvpn）。
#[cfg(windows)]
pub(crate) fn close_job(job: Option<usize>) {
    if let Some(j) = job {
        unsafe {
            let _ = CloseHandle(HANDLE(j as *mut core::ffi::c_void));
        }
    }
}

/// 从 openvpn 日志中提取一句最相关的错误（无则取最后一行非空日志）。
fn build_detail(log: &[String]) -> String {
    extract_error_line(log)
        .or_else(|| log.iter().rev().find(|l| !l.trim().is_empty()).cloned())
        .unwrap_or_else(|| t!("openvpn 未输出明确错误信息", "openvpn did not output a clear error message").to_string())
        .trim()
        .to_string()
}

/// 取最后一条命中关键错误词的日志行。
fn extract_error_line(log: &[String]) -> Option<String> {
    let keywords = [
        "auth_failed",
        "auth failed",
        "tls error",
        "tls handshake",
        "expired",
        "fatal",
        "options error",
        "error",
        "wintun",
        "failed to",
        "cannot",
        "timed out",
        "could not",
        "no such",
        "unable to",
    ];
    for l in log.iter().rev() {
        let lo = l.to_lowercase();
        if keywords.iter().any(|k| lo.contains(k)) {
            return Some(l.trim().to_string());
        }
    }
    None
}

/// 若错误信息涉及 wintun，追加「请将 wintun.dll 放在 openvpn.exe 同目录」提示。
fn append_wintun_hint(msg: &str) -> String {
    if msg.to_lowercase().contains("wintun") {
        format!("{msg}\n{}", t!("（请确认 wintun.dll 已放在 openvpn.exe 同目录）", "(Make sure wintun.dll is placed next to openvpn.exe)"))
    } else {
        msg.to_string()
    }
}
