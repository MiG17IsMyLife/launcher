use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db;
use crate::detect;
use crate::kitten;
use crate::mice;
use crate::segatools;
use crate::storage::{GameRecord, GraphicsConfig, Loader, Settings, launcher_dir};
use crate::t;
use crate::tf;

#[derive(Clone, Debug, Default)]
pub struct LoaderPaths {
    pub kitten: Option<PathBuf>,
    pub mice: Option<PathBuf>,
    pub mice64: Option<PathBuf>,

    pub inject: Option<PathBuf>,
}

const LOADER_NAMES: &[&str] = &["kitten.exe", "mice.exe", "mice64.exe", "inject.exe"];

#[derive(Clone)]
pub enum DeployConfig {
    Kitten(kitten::DeploySnapshot),
    Mice(mice::DeploySnapshot),
    Segatools(segatools::DeploySnapshot),
}

impl DeployConfig {

    pub fn without_graphics(mut self) -> Self {
        match &mut self {
            DeployConfig::Kitten(s) => s.graphics = GraphicsConfig::default(),
            DeployConfig::Mice(s) => s.graphics = GraphicsConfig::default(),
            DeployConfig::Segatools(s) => s.graphics = GraphicsConfig::default(),
        }
        self
    }

    fn mismatch_error() -> String {
        t!("游戏启动缺少配置快照或快照与加载器不匹配（内部错误）",
           "Game launch is missing the config snapshot or the snapshot does not match the loader (internal error)").to_string()
    }

    pub fn keychip(&self) -> Option<&str> {
        let k = match self {
            DeployConfig::Kitten(s) => &s.system.keychip,
            DeployConfig::Mice(s) => &s.system.keychip,
            DeployConfig::Segatools(s) => &s.system.keychip,
        };
        k.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn server_host(&self) -> &str {
        let net = match self {
            DeployConfig::Kitten(s) => &s.net,
            DeployConfig::Mice(s) => &s.net,
            DeployConfig::Segatools(s) => &s.net,
        };
        net.server_host()
    }
}

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

fn game_dir_of(record: &GameRecord) -> PathBuf {
    Path::new(&record.path)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

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

fn serverbox_dir_of(record: &GameRecord) -> Option<PathBuf> {
    let sb = record
        .serverbox
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let p = Path::new(sb);
    if !p.is_file() {
        return None;
    }
    let dir = p.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    Some(if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    })
}

fn serverbox_record(record: &GameRecord) -> Result<GameRecord, String> {
    let sb_path = record
        .serverbox
        .as_deref()
        .ok_or_else(|| t!("该游戏未配置 ServerBox", "This game has no ServerBox configured").to_string())?;
    if !Path::new(sb_path).is_file() {
        return Err(tf!("ServerBox 文件不存在：{sb_path}", "ServerBox file does not exist: {sb_path}"));
    }
    let mut sb_rec = record.clone();
    sb_rec.path = sb_path.to_string();
    sb_rec.test_mode = None;
    Ok(sb_rec)
}

pub fn deploy_serverbox(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<Option<PathBuf>, String> {

    let dep_owned = dep.cloned().map(DeployConfig::without_graphics);
    let dep = dep_owned.as_ref();
    let sb_rec = serverbox_record(record)?;

    let sb_dir = serverbox_dir_of(&sb_rec).unwrap_or_else(|| PathBuf::from("."));
    if sb_rec.loader != Loader::Segatools {
        let enabled = sb_rec.gfx_patch.unwrap_or(true);
        if enabled {
            let tools = crate::update::tools_dir();
            if find_asi_loader(&tools).is_none() {
                return Err(tf!(
                    "未安装 Ultimate-ASI-Loader 组件（{}），无法部署图形补丁；请先在设置页更新该组件",
                    "Ultimate-ASI-Loader is not installed ({}); install it from the Settings page first",
                    tools.join("asi-loader").join("dinput8.dll").display()
                ));
            }
        }
        deploy_gfx_patch(&sb_dir, enabled)?;
    }

    match sb_rec.loader {
        Loader::Mice => {
            let Some(DeployConfig::Mice(snapshot)) = dep else {
                return Err(DeployConfig::mismatch_error());
            };
            let loader = resolve_loader(&sb_rec, settings, detected)?;
            let tool_dir = loader.parent().ok_or_else(|| {
                tf!("无法确定 mice 工具目录：{}", "Cannot determine the mice tool directory: {}", loader.display())
            })?;
            mice::deploy(&sb_dir, tool_dir, snapshot, mice::MiceTarget::ServerBox)?;
            Ok(None)
        }
        Loader::Kitten => deploy_only(&sb_rec, settings, detected, dep).map(Some),
        Loader::Segatools => Err(t!("segatools 加载器暂不支持 ServerBox",
            "The segatools loader does not support ServerBox yet").to_string()),
    }
}

pub fn launch_serverbox(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<std::process::Child, String> {

    let dep_owned = dep.cloned().map(DeployConfig::without_graphics);
    let dep = dep_owned.as_ref();

    let sb_exe = deploy_serverbox(record, settings, detected, dep)?;
    let sb_rec = serverbox_record(record)?;
    let exe = match sb_rec.loader {
        Loader::Mice => resolve_loader(&sb_rec, settings, detected)?,
        Loader::Kitten => {
            sb_exe.ok_or_else(|| DeployConfig::mismatch_error())?
        }
        Loader::Segatools => {
            return Err(t!("segatools 加载器暂不支持 ServerBox",
                "The segatools loader does not support ServerBox yet").to_string())
        }
    };
    spawn_game(&sb_rec, &exe).map_err(|e| format!("ServerBox {e}"))
}

pub fn deploy_reader(
    reader_path: &str,
    settings: &Settings,
    detected: &LoaderPaths,
    card: Option<&str>,
    game_ip: Option<&str>,
    card_button: Option<&str>,
    io3_mode: Option<&str>,
    auto_insert: bool,
) -> Result<PathBuf, String> {
    let reader = Path::new(reader_path);
    if !reader.is_file() {
        return Err(tf!("读卡器文件不存在：{reader_path}", "Card reader file does not exist: {reader_path}"));
    }
    let reader_dir = reader
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(launcher_dir);

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

    for name in ["mice.exe", "mice.dll"] {
        let src = tool_dir.join(name);
        if !src.is_file() {
            continue;
        }
        copy_file_if_changed(&src, &reader_dir.join(name))?;
    }

    let mut ini_text = String::new();
    ini_text.push_str(&format!(
        "[CardReader]\nauto_insert = {}\n",
        if auto_insert { "true" } else { "false" }
    ));
    if let Some(card) = card {
        ini_text.push_str(&format!("cardfile = {card}\n"));
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
        crate::mice::write_atomic(&ini, &ini_text)?;
    }
    Ok(mice)
}

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

pub fn ensure_picodaemon(game_dir: &Path) -> Result<PathBuf, String> {
    let dep = launcher_dir().join("tools").join("dependency");
    ensure_picodaemon_from(game_dir, &dep)
}

pub fn ensure_cg_libs(game_dir: &Path) -> Result<(), String> {
    let dep = launcher_dir().join("tools").join("dependency");
    ensure_cg_libs_from(game_dir, &dep)
}

fn ensure_picodaemon_from(game_dir: &Path, dep: &Path) -> Result<PathBuf, String> {
    let candidate = game_dir.join("picodaemon.exe");
    let src = dep.join("picodaemon.exe");

    if candidate.is_file() && detect::probe_card_reader(game_dir).is_some() {
        return Ok(candidate);
    }

    if candidate.is_file() && src.is_file() && files_equal(&candidate, &src) {
        return Ok(candidate);
    }

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

fn files_equal(a: &Path, b: &Path) -> bool {

    if let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) {
        if ma.len() != mb.len() {
            return false;
        }
    }
    let (Ok(ra), Ok(rb)) = (fs::read(a), fs::read(b)) else {
        return false;
    };
    ra == rb
}

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

pub fn deploy_gfx_patch(dir: &Path, enabled: bool) -> Result<(), String> {
    deploy_gfx_patch_from(dir, enabled, &crate::update::tools_dir())
}

fn deploy_gfx_patch_from(dir: &Path, enabled: bool, tools: &Path) -> Result<(), String> {
    let winmm = dir.join("winmm.dll");
    if !enabled {
        if winmm.is_file() {
            let _ = fs::remove_file(&winmm);
        }
        return Ok(());
    }

    if let Some(dll) = find_asi_loader(tools) {
        copy_file_if_changed(&dll, &winmm)?;
    }

    let sf_dir = tools.join("shaderfix");
    if sf_dir.is_dir() {
        copy_files_flat(&sf_dir, &dir.join("scripts"))?;
    }
    Ok(())
}

fn find_asi_loader(tools: &Path) -> Option<PathBuf> {
    let root = tools.join("asi-loader");
    let direct = root.join("dinput8.dll");
    if direct.is_file() {
        return Some(direct);
    }

    find_file_recursive(&root, "dinput8.dll")
}

fn find_file_recursive(dir: &Path, name: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file_recursive(&p, name) {
                return Some(found);
            }
        } else if entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(p);
        }
    }
    None
}

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

pub fn prepare_game(
    record: &GameRecord,
    settings: &Settings,
    detected: &LoaderPaths,
    dep: Option<&DeployConfig>,
) -> Result<PathBuf, String> {
    let exe = deploy_only(record, settings, detected, dep)?;

    let game_dir = Path::new(&record.path)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let host = dep
        .map(|d| d.server_host())
        .unwrap_or(crate::server::SERVER_HOST);
    let _ = initiald_patch(record, &game_dir, host);

    if record.loader == Loader::Mice {
        let enabled = record.gfx_patch.unwrap_or(true);
        deploy_gfx_patch(&game_dir, enabled)?;

        if let Some(sb_dir) = serverbox_dir_of(record) {
            let _ = deploy_gfx_patch(&sb_dir, enabled);
        }
    }

    Ok(exe)
}

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

    cmd.spawn()
        .map_err(|e| tf!("启动 amdaemon 失败（{}）：{e}", "Failed to start amdaemon ({}): {e}", inject.display()))
}

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
    cmd.creation_flags(crate::win_util::CREATE_NO_WINDOW);
    cmd.spawn()
        .map_err(|e| tf!("启动游戏失败（{}）：{e}", "Failed to start game ({}): {e}", game_exe.display()))
}

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

    cmd.current_dir(&game_dir);
    match record.loader {

        Loader::Kitten | Loader::Mice => {
            let file = Path::new(&record.path)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&record.path));
            cmd.arg("-b").arg(&file);
            cmd.creation_flags(crate::win_util::CREATE_NO_WINDOW);
        }
        Loader::Segatools => {

            return Err(t!("segatools 游戏暂不支持 ServerBox / 该启动路径",
                "The segatools loader does not support ServerBox / this launch path yet").to_string());
        }
    }

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
        let dep = tmp("cg_missing_dep");
        let err = ensure_cg_libs_from(&game, &dep).unwrap_err();
        assert!(err.contains("cg.dll"), "the error should say cg.dll is missing: {err}");
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
            "the error should say picodaemon.exe is missing: {err}"
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

        ensure_picodaemon_from(&game, &dep).unwrap();

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

        let exe = deploy_reader(
            &reader,
            &settings,
            &detected,
            Some("01010409725450755799"),
            Some("192.168.100.101"),
            Some("START"),
            Some("xinput"),
            false,
        )
        .unwrap();
        assert!(exe.is_file());
        let ini = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(
            ini.contains("[CardReader]\nauto_insert = false\ncardfile = 01010409725450755799\n")
        );
        assert!(ini.contains("[ServerBox]\ngame_ip = 192.168.100.101\n"));
        assert!(ini.contains("[io3]\nmode = xinput\n"));
        assert!(ini.contains("[xinput]\ncard = START\n"));

        deploy_reader(&reader, &settings, &detected, Some("0101"), None, None, None, true).unwrap();
        let ini2 = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(ini2.contains("[CardReader]\nauto_insert = true\ncardfile = 0101\n"));
        assert!(!ini2.contains("[io3]"));
        assert!(!ini2.contains("[xinput]"));

        deploy_reader(&reader, &settings, &detected, Some("0101"), None, Some("  "), Some("dinput"), false).unwrap();
        let ini3 = fs::read_to_string(reader_dir.join("mice/idas.ini")).unwrap();
        assert!(ini3.contains("[CardReader]\nauto_insert = false\ncardfile = 0101\n"));
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

    fn record_with_serverbox(serverbox: Option<String>) -> GameRecord {
        GameRecord {
            id: "gfx".to_string(),
            path: String::new(),
            name: "gfx".to_string(),
            crc: None,
            version: None,
            arch: None,
            loader: Loader::Mice,
            platform: None,
            image: None,
            cover: None,
            serverbox,
            reader: None,
            amdaemon: None,
            game_exe: None,
            game_id: None,
            gfx_patch: None,
            test_mode: None,
            card: None,
            region: None,
            play_count: 0,
            last_played: None,
            added_at: 0,
        }
    }

    fn tools_with_gfx(name: &str) -> PathBuf {
        let tools = tmp(name);
        fs::create_dir_all(tools.join("asi-loader")).unwrap();
        write(&tools.join("asi-loader/dinput8.dll"), b"loader");
        fs::create_dir_all(tools.join("shaderfix/plugins")).unwrap();
        write(&tools.join("shaderfix/shaderfix.asi"), b"asi");
        write(&tools.join("shaderfix/plugins/extra.asi"), b"extra");
        tools
    }

    #[test]
    fn gfx_patch_deploys_loader_and_shaderfix() {
        let dir = tmp("gfx_dir");
        let tools = tools_with_gfx("gfx_tools");
        deploy_gfx_patch_from(&dir, true, &tools).unwrap();
        assert_eq!(fs::read(dir.join("winmm.dll")).unwrap(), b"loader".to_vec());
        assert_eq!(
            fs::read(dir.join("scripts/shaderfix.asi")).unwrap(),
            b"asi".to_vec()
        );

        assert_eq!(
            fs::read(dir.join("scripts/extra.asi")).unwrap(),
            b"extra".to_vec()
        );
        cleanup(&dir);
        cleanup(&tools);
    }

    #[test]
    fn gfx_patch_disabled_removes_winmm() {
        let dir = tmp("gfx_off_dir");
        let tools = tools_with_gfx("gfx_off_tools");
        write(&dir.join("winmm.dll"), b"old");
        deploy_gfx_patch_from(&dir, false, &tools).unwrap();
        assert!(!dir.join("winmm.dll").is_file(), "with the switch off winmm.dll should be removed");
        cleanup(&dir);
        cleanup(&tools);
    }

    #[test]
    fn gfx_patch_missing_components_is_noop() {
        let dir = tmp("gfx_empty_dir");
        let tools = tmp("gfx_empty_tools");
        deploy_gfx_patch_from(&dir, true, &tools).unwrap();
        assert!(!dir.join("winmm.dll").exists());
        assert!(!dir.join("scripts").exists());
        cleanup(&dir);
        cleanup(&tools);
    }

    #[test]
    fn gfx_patch_covers_game_and_serverbox_dirs() {

        let game = tmp("gfx_game");
        let sb = tmp("gfx_sb");
        let tools = tools_with_gfx("gfx_both_tools");
        write(&sb.join("serverbox.exe"), b"sb");
        let rec = record_with_serverbox(Some(
            sb.join("serverbox.exe").to_string_lossy().into_owned(),
        ));
        let sb_dir = serverbox_dir_of(&rec).expect("a configured ServerBox should resolve to a folder");
        for dir in [&game, &sb_dir] {
            deploy_gfx_patch_from(dir, true, &tools).unwrap();
            assert!(dir.join("winmm.dll").is_file(), "{} should have winmm.dll", dir.display());
            assert!(dir.join("scripts/shaderfix.asi").is_file());
        }
        cleanup(&game);
        cleanup(&sb);
        cleanup(&tools);
    }

    #[test]
    fn gfx_patch_finds_loader_in_nested_dir() {

        let dir = tmp("gfx_nested_dir");
        let tools = tmp("gfx_nested_tools");
        fs::create_dir_all(tools.join("asi-loader/Ultimate-ASI-Loader-win32")).unwrap();
        write(
            &tools.join("asi-loader/Ultimate-ASI-Loader-win32/dinput8.dll"),
            b"loader",
        );
        deploy_gfx_patch_from(&dir, true, &tools).unwrap();
        assert_eq!(fs::read(dir.join("winmm.dll")).unwrap(), b"loader".to_vec());
        cleanup(&dir);
        cleanup(&tools);
    }

    #[test]
    fn serverbox_dir_of_variants() {

        assert!(serverbox_dir_of(&record_with_serverbox(None)).is_none());
        assert!(serverbox_dir_of(&record_with_serverbox(Some(String::new()))).is_none());
        assert!(serverbox_dir_of(&record_with_serverbox(Some("  ".to_string()))).is_none());
        let sb = tmp("sb_variants");

        let missing = sb.join("nope.exe").to_string_lossy().into_owned();
        assert!(serverbox_dir_of(&record_with_serverbox(Some(missing))).is_none());

        write(&sb.join("serverbox.exe"), b"sb");
        let ok = sb.join("serverbox.exe").to_string_lossy().into_owned();
        assert_eq!(
            serverbox_dir_of(&record_with_serverbox(Some(ok))),
            Some(sb.clone())
        );
        cleanup(&sb);
    }

    fn graphics_all_set() -> GraphicsConfig {
        GraphicsConfig {
            default: Some(false),
            borderless: Some(false),
            resolution: Some("1920x1080".into()),
            render_scale: Some(true),
            render_scale_level: Some("2x".into()),
            msaa: Some(true),
            msaa_level: Some("4x".into()),
            af: Some(true),
            af_level: Some("16x".into()),
        }
    }

    #[test]
    fn without_graphics_ignores_kitten_display() {
        use crate::kitten;
        use crate::storage::{CoinConfig, InputConfig, NetworkConfig, SystemConfig};
        let dep = DeployConfig::Kitten(kitten::DeploySnapshot {
            net: NetworkConfig::default(),
            system: SystemConfig::default(),
            coin: CoinConfig::default(),
            graphics: graphics_all_set(),
            input: InputConfig::default(),
            game_id: None,
        });

        let DeployConfig::Kitten(orig) = &dep else { unreachable!() };
        assert!(kitten::render(orig).contains("[display]"));

        let text = match dep.clone().without_graphics() {
            DeployConfig::Kitten(s) => kitten::render(&s),
            _ => unreachable!(),
        };
        assert!(!text.contains("[display]"), "the ServerBox side should have no [display] section:\n{text}");
        assert!(!text.contains("width ="), "the resolution should not be written:\n{text}");
        assert!(!text.contains("anisotropic"), "anisotropic filtering should not be written:\n{text}");

        assert!(text.contains("[network]"), "the network section should stay:\n{text}");
    }

    #[test]
    fn without_graphics_ignores_mice_window_and_render_scale() {
        use crate::mice;
        use crate::storage::{CoinConfig, InputConfig, NetworkConfig, SystemConfig};
        let dep = DeployConfig::Mice(mice::DeploySnapshot {
            net: NetworkConfig::default(),
            system: SystemConfig::default(),
            coin: CoinConfig::default(),
            graphics: graphics_all_set(),
            input: InputConfig::default(),
            card: None,
            room: None,
        });

        let DeployConfig::Mice(orig) = &dep else { unreachable!() };
        assert!(mice::render_config(orig, mice::MiceTarget::Game).contains("[window]"));
        assert!(mice::render_idas(orig).contains("render_scale"));

        let dep2 = dep.clone().without_graphics();
        let DeployConfig::Mice(s) = &dep2 else { unreachable!() };
        let cfg = mice::render_config(s, mice::MiceTarget::ServerBox);
        assert!(!cfg.contains("[window]"), "the ServerBox config.ini should have no [window] section:\n{cfg}");
        assert!(!cfg.contains("windowed"), "the windowed key should not be written:\n{cfg}");
        let idas = mice::render_idas(s);
        assert!(!idas.contains("render_scale"), "the ServerBox idas.ini should not write render_scale:\n{idas}");
        assert!(!idas.contains("[Graphics]"), "the ServerBox idas.ini should have no [Graphics] section:\n{idas}");

        assert!(cfg.contains("[network]"), "the network section of config.ini should stay:\n{cfg}");
        assert!(idas.contains("simulateNetwork"), "the simulate network key of idas.ini should stay:\n{idas}");
    }

    #[test]
    fn without_graphics_clears_segatools_graphics() {
        use crate::storage::{CoinConfig, InputConfig, NetworkConfig, SystemConfig};
        let dep = DeployConfig::Segatools(segatools::DeploySnapshot {
            net: NetworkConfig::default(),
            system: SystemConfig::default(),
            coin: CoinConfig::default(),
            input: InputConfig::default(),
            graphics: graphics_all_set(),
            version: None,
            game_id: None,
            aime_card: None,
            room: None,
        });
        let dep = dep.without_graphics();
        let DeployConfig::Segatools(s) = &dep else { unreachable!() };
        assert_eq!(s.graphics, GraphicsConfig::default());
    }
}
