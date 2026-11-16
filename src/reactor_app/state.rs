use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::detect::IdentifyResult;

#[derive(Clone)]
pub struct GameRuntime {
    pub(super) index: usize,
    pub(super) child: Arc<Mutex<std::process::Child>>,
    pub(super) companions: Arc<Mutex<Vec<std::process::Child>>>,

    pub(super) kill_amdaemon: bool,

    pub(super) serverbox_images: Vec<String>,

    pub(super) added_static_ip: Option<(String, String)>,
}

impl GameRuntime {

    pub(super) fn taskkill_images(&self) {
        let mut images: Vec<&str> = Vec::new();
        if self.kill_amdaemon {
            images.push("amdaemon.exe");
        }
        for sb in &self.serverbox_images {
            images.push(sb.as_str());
        }
        for image in images {
            let _ = std::process::Command::new("taskkill")
                .args(["/f", "/im", image])
                .creation_flags(crate::win_util::CREATE_NO_WINDOW)
                .spawn();
        }
    }
}

#[derive(Clone)]
pub struct ServerBoxRuntime {
    pub(super) index: usize,
    pub(super) child: Arc<Mutex<std::process::Child>>,
}

pub(super) struct PendingAdd {
    pub(super) path: PathBuf,
    pub(super) ir: IdentifyResult,
}

pub(super) struct CardBaseCache {

    pub(super) result: Result<image::RgbaImage, String>,
}
