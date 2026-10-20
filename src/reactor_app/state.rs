//! 跨模块共享的运行时状态小结构（进程句柄 / 待入库游戏 / 底图缓存）。
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::detect::IdentifyResult;

/// 运行中游戏的进程句柄与伴随进程（读卡器），用于退出轮询与回收。
/// 用 Arc<Mutex> 包装以便跨消息传递（Message 需 Clone）并在轮询任务间共享。
#[derive(Clone)]
pub struct GameRuntime {
    pub(super) index: usize,
    pub(super) child: Arc<Mutex<std::process::Child>>,
    pub(super) companions: Arc<Mutex<Vec<std::process::Child>>>,
    /// segatools 游戏：游戏退出后按映像名终止 amdaemon.exe
    /// （对应 launch.bat 末尾的 taskkill /f /im amdaemon.exe）
    pub(super) kill_amdaemon: bool,
    /// 该游戏配置的 ServerBox 文件名（IDAS4~8 / IDZ 等）：
    /// 手动启动的 ServerBox 启动器不持有句柄，游戏关闭 / 退出时按映像名回收
    pub(super) serverbox_image: Option<String>,
    /// 启动游戏前为本机网卡添加的 ServerBox 静态 IP（网卡名, IP）：
    /// 仅普通网卡且无第二个地址时才添加；游戏退出后据此删除。
    /// awl 虚拟网卡（店内对战）地址由房间流程管理，恒为 None，不参与增删。
    pub(super) added_static_ip: Option<(String, String)>,
}

impl GameRuntime {
    /// 游戏退出后需按映像名强杀的伴随进程：
    /// - segatools：amdaemon（inject 派生，句柄杀不到）
    /// - IDAS4~8 / IDZ：手动启动的 ServerBox（取游戏记录中的 ServerBox 文件名）
    /// taskkill 未命中（如该进程未在运行）仅返回错误码，静默忽略。
    pub(super) fn taskkill_images(&self) {
        let mut images: Vec<&str> = Vec::new();
        if self.kill_amdaemon {
            images.push("amdaemon.exe");
        }
        if let Some(sb) = &self.serverbox_image {
            images.push(sb.as_str());
        }
        for image in images {
            let _ = std::process::Command::new("taskkill")
                .args(["/f", "/im", image])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .spawn();
        }
    }
}

/// 运行中的 ServerBox 进程句柄（行内「启动」后跟踪，供「关闭」按钮回收）。
#[derive(Clone)]
pub struct ServerBoxRuntime {
    pub(super) index: usize,
    pub(super) child: Arc<Mutex<std::process::Child>>,
}

/// 未命中识别库、等待「命名」弹窗确认入库的待添加游戏
pub(super) struct PendingAdd {
    pub(super) path: PathBuf,
    pub(super) ir: IdentifyResult,
    /// 同目录自动探测到的读卡器（命中识别库 card_reader）：(路径, 版本)
    pub(super) reader: Option<(PathBuf, String)>,
}

/// 卡片底图缓存：内嵌字节解码结果（纯内嵌方案，用户无法再替换底图）
pub(super) struct CardBaseCache {
    /// 解码后的底图，或加载失败原因（失败也缓存，避免每帧重复 IO / 解码）
    pub(super) result: Result<image::RgbaImage, String>,
}
