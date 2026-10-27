//! WinUI 3 主界面（windows-reactor，Elm 架构单组件）：
//! Mica 窗口 + 顶栏 + 游戏列表行（点击行头展开 ServerBox/读卡器等配置卡片，行尾「∨」指示方向）+ 卡片页 + 底部状态栏 + 弹窗。
//!
//! 模块划分：`message` = 事件词表；`state` = 跨模块状态结构；`theme` = 系统主题；
//! `widgets` = 通用小部件；`pages/*` = 各页面渲染与对应消息处理。
//! 本文件只保留 `Launcher` 组件本体与 `create` / `update` / `view` 骨架。

mod message;
mod pages;
mod state;
mod theme;
mod widgets;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

use windows_reactor::*;

use crate::cards::{self, CardScan};
use crate::awl::{AwlHandle, PeerView, RoomStatus};
use crate::filedialog;
use crate::i18n::{self, Lang};
use crate::launch;
use crate::netinfo::{self, AdapterInfo};
use crate::rawinput;
use crate::roomdir::RoomSummary;
use crate::server::{self, ServerState};
use crate::storage::{self, Library, Settings, VpnConfig};
use crate::t;
use crate::tf;
use crate::update::{HttpAbort, UpdatePhase, UpdateState, UpdateStatus, UpdateTarget};
use crate::vpn::{VpnHandle, VpnStatus};

use self::message::{Dialog, Message};
use self::pages::graphics::{GFX_AF_LEVELS, GFX_MSAA_LEVELS, GFX_RENDER_SCALE_LEVELS, GFX_RESOLUTIONS};
use self::pages::input::{capture_label, FFB_MAX, INPUT_KINDS, INPUT_RATIO_DEFAULT, INPUT_RATIO_MAX, SHIFTER_TYPES};
use self::pages::network::{VPN_AUTO_RETRIES, VPN_STABLE_RESET};
use self::pages::system::{SYS_LOGLEVELS, SYS_REGIONS, SYS_SEATS};
use self::state::{CardBaseCache, GameRuntime, PendingAdd, ServerBoxRuntime};
use self::theme::system_uses_light_theme;

/// 应用图标（由 src/assets/logo_sasayaki.webp 转出的多尺寸 ico，见 build.rs 的资源嵌入）。
/// 启动时写入临时目录，供 AppWindow::SetIcon 设置标题栏 / 任务栏图标。
const APP_ICON: &[u8] = include_bytes!("assets/launcher.ico");

// ---------- 主组件 ----------
pub struct Launcher {
    store: storage::Store,
    library: Library,
    settings: Settings,
    detected: launch::LoaderPaths,
    server_shared: Arc<Mutex<server::ServerStatus>>,
    server_state: ServerState,
    nav_tag: String,
    pane_open: bool,
    dialog: Dialog,
    pending: Option<PendingAdd>,
    /// 网络设置页：本机网卡快照（后台扫描结果）
    adapters: Vec<AdapterInfo>,
    /// 首次网卡扫描是否已完成（区分「扫描中」与「无网卡」）
    adapters_scanned: bool,
    /// 网络设置（启动器 config.ini 的 [network] 节）
    net: storage::NetworkConfig,
    /// 系统设置（启动器 config.ini 的 [system] 节，由加载器自行读取）
    system: storage::SystemConfig,
    /// 界面语言（驱动「语言」下拉框选中态；同时写入全局原子驱动 `t!` 取值）
    lang: Lang,
    /// 投币设置（启动器 config.ini 的 [coin] 节，由加载器自行读取）
    coin: storage::CoinConfig,
    /// 图形设置（启动器 config.ini 的 [graphics] 节，由加载器自行读取）
    graphics: storage::GraphicsConfig,
    /// 输入设置（启动器 config.ini 的 [input] 节，由加载器自行读取）
    input: storage::InputConfig,
    /// VPN 配置（启动器 config.ini 的 [vpn] 节）
    vpn: VpnConfig,
    /// VPN 连接状态（状态栏 / 设置页展示）
    vpn_status: VpnStatus,
    /// 当前 VPN 连接句柄（持有 openvpn 子进程；Drop 时自动断开）。
    /// 用 Arc<Mutex> 以便后台线程在连接成功后写入、在游戏退出/手动断开时取出。
    vpn_handle: Arc<Mutex<Option<VpnHandle>>>,
    /// VPN 自动重连：本轮已尝试次数（连接成功/手动连接/手动断开时清零）
    vpn_auto_retries: u32,
    /// VPN 最近一次连接成功的时刻（自动重连预算按「稳定运行满 5 分钟重新计满」判定）
    vpn_connected_at: Option<Instant>,
    /// VPN 监督线程代数：每次连接成功/手动断开/换配置 +1，过期监督线程自行退出
    vpn_watch_gen: Arc<AtomicUsize>,
    /// 房间功能配置（启动器 config.ini 的 [room] 节）
    room: storage::RoomConfig,
    /// 房间会话状态机（Idle/Creating/Hosting/Joining/Joined）
    room_status: RoomStatus,
    /// awl 子进程句柄（持有补丁版 awl；Drop 时自动杀进程）
    room_handle: Arc<Mutex<Option<AwlHandle>>>,
    /// 房主房间看门狗子进程（Hosting 时拉起；监控本启动器进程退出后注销房间）。
    /// 独立于 awl 作业对象，启动器被强杀时仍可存活完成任务；主动关房时一并 kill。
    room_watchdog: Option<Child>,
    /// 房主：当前房间密码（仅内存，不落盘；目录侧丢失时的提示依赖它不重连）
    room_password: String,
    /// 房主：已邀请的成员 peer_id（invite 幂等去重）
    room_members_invited: Vec<String>,
    /// 成员：房主失联起始时刻（超时自动退出）
    room_host_down: Option<Instant>,
    /// 房间页：公开房间列表（进入页面 / 刷新时拉取）
    room_list: Vec<RoomSummary>,
    /// 房间页：房间内成员视图快照（RoomTick 时从 awl API 拉取，供 UI 渲染）
    room_peers: Vec<PeerView>,
    /// 机位快照：peer_id → 座位 IP（房主=.101；成员=目录分配的座位 IP）。
    /// RoomTick 时以目录成员表为权威整体重建，供成员表格「机位」列
    /// 显示机位名（房主 A1 / 成员 A2、B1、B2，按座位 IP 末字节映射）
    room_seat_ips: HashMap<String, String>,
    /// 本机在房间内的 Game IP（房主=.102，成员=座位 IP；用于 mice 部署覆盖）
    room_my_ip: Option<String>,
    /// 成员端：本机 peer 是否曾在房主快照的成员表中出现过。
    /// 被踢判定依据（曾在 → 缺失 = 被移出）：首次加入时房主对账（事件驱动 +
    /// 60s 节流）可能尚未把新成员纳入广播表，首拍缺自己不算被踢，否则误踢
    room_saw_self_in_table: bool,
    /// 昵称未设提示：暂挂的创建/加入动作（延迟一拍再开提示弹窗——
    /// 立即开会被 WinUI 单 ContentDialog 约束静默拒绝，见 RoomNickPromptReady）
    room_nick_pending: Option<crate::reactor_app::message::PendingRoomAction>,
    /// 当前房间的游戏（games_db 产品名；状态灯与 mice 部署展示用）
    room_game: String,
    /// 当前房间名（房间内视图标题；成员由目录 join 返回）
    room_name: String,
    /// 房主：本房间归属 keychip（心跳/回归校验用）
    room_keychip: String,
    /// 房主：成员掉线宽限计时（peer_id → 首次观测到断连的时刻；
    /// 超过宽限期按「离开房间」处理，代为释放座位）
    room_member_down: HashMap<String, Instant>,
    /// 房主：本房间内曾建连过的成员（peer_id）——掉线走短宽限（MEMBER_KICK_AFTER）；
    /// 从未建连的加入中成员走长宽限（MEMBER_JOIN_GRACE，见 pages/room.rs）
    room_member_ever_connected: HashSet<String>,
    /// 房主：上次成功上报心跳的时刻（5 分钟节流用；None=尚未发过）
    room_last_heartbeat: Option<Instant>,
    /// 房主侧成员表长连接同步（Hosting 期间持有；room_stop 时 Drop 停线程）
    room_host_sync: Option<crate::room_sync::HostSync>,
    /// 成员侧同步通道（Joined 期间持有）
    room_member_sync: Option<crate::room_sync::MemberSync>,
    /// 成员侧已消费的房主快照版本（去重）
    room_sync_version: u64,
    /// 成员侧最近一次收到的快照（成员表格「游玩」列渲染用）
    room_sync_table: Option<crate::room_sync::SyncTable>,
    /// 未设昵称弹窗「跳过」后的会话内默认昵称（不落盘；优先级低于设置页保存值）
    room_nick_override: Option<String>,
    /// 房主侧目录成员表缓存（事件驱动对账的权威快照；None=尚未拉到）
    room_dir_members: Option<crate::roomdir::MembersInfo>,
    /// 房主侧上次目录对账时刻（60s 节流）
    room_last_reconcile: Option<Instant>,
    /// 本机游玩状态（None=尚未发布/上报过）
    room_last_playing_report: Option<bool>,
    /// 成员侧上次状态上报时刻（30s 保活节流用）
    room_last_status_sent: Option<Instant>,
    /// 房间 loading 覆盖层文案（Some=显示，锁定背景；创建/加入/回归结束后清除）
    room_overlay: Option<String>,
    /// 大厅游戏筛选：ComboBox 选中项（None/Some(0)=全部）
    room_filter_idx: Option<usize>,
    /// 大厅游戏筛选键（games_db game_id,缺省回退产品名）
    room_filter_game_id: Option<String>,
    /// 加入中暂存的房间密码（加入成功后写入回归记忆）
    room_join_password: String,
    /// 输入设备快照（后台扫描结果）
    input_devices: Vec<rawinput::GameDevice>,
    /// 首次设备扫描是否已完成
    input_devices_scanned: bool,
    /// 按键捕获代数：关闭弹窗或重新发起捕获时 +1，过期结果作废
    input_capture_gen: u32,
    /// 按键捕获开始时刻（用于计算倒计时剩余时间）
    input_capture_start: Option<Instant>,
    /// 按键捕获剩余进度 1.0 → 0.0：每拍更新以驱动进度条重渲染
    input_capture_remain: f64,
    /// 投币设置卡片是否展开
    coin_expanded: bool,
    /// 投币设置展开动画进度（0=收起 1=展开），驱动内容淡入/淡出
    coin_anim: f64,
    /// 动画代数：每次切换展开状态 +1，旧动画循环过期作废
    coin_anim_gen: u32,
    /// 力反馈卡片是否展开
    ffb_expanded: bool,
    /// 力反馈展开动画进度（0=收起 1=展开），驱动内容淡入/淡出
    ffb_anim: f64,
    /// 力反馈动画代数：每次切换 +1，旧动画循环过期作废
    ffb_anim_gen: u32,
    /// 当前展开下拉卡片的游戏行下标（None=全部收起）
    drop_index: Option<usize>,
    /// 行下拉卡片是否处于展开目标状态（动画推进期间也保持不变）
    drop_open: bool,
    /// 行下拉卡片展开动画进度（0=收起 1=展开），驱动内容淡入/淡出
    drop_anim: f64,
    /// 动画代数：每次切换 +1，旧动画循环过期作废
    drop_anim_gen: u32,
    /// 系统是否使用浅色主题：决定自绘悬停高亮用黑(#19000000)还是白(#19FFFFFF)，
    /// 与窗口 WindowTheme::System 保持一致
    light_theme: bool,
    /// 卡片选择页对应的游戏下标
    cards_game: Option<usize>,
    /// 卡片页是否为 aime 模式（segatools 的 SDDF / SDER / SDGT：DEVICE 目录 20 位数字 .txt 卡）
    cards_aime: bool,
    /// 卡片页是否为 ID4 模式（头文字D4 / D4 kai，SBML，kitten 加载器）：车卡合成使用 id4_base.png 底图
    cards_id4: bool,
    /// 卡片页是否为 ID8 模式（头文字D8，SBZZ / SDAL）：车卡合成使用 id8_base.png 底图
    cards_id8: bool,
    /// 卡片页是否为 ID6 模式（头文字D6 系列，SBUU / SBVW / SBWV）：车卡合成使用 id6_base.png 底图
    cards_id6: bool,
    /// 卡片页加载中：扫描 + 分拍预热合成缓存期间显示 loading 过渡
    cards_loading: bool,
    /// 已预热合成的卡片数（配合 cards_loading 渐进显示）
    cards_warm: usize,
    cards: CardScan,
    /// 窗口客户区尺寸（有效像素），用于卡片等比缩放
    win_size: WindowSize,
    /// 卡片页进入过渡动画进度（0.0~1.0），驱动左右滑入 + 淡入
    card_slide: f64,
    /// 进入卡片页的序号（每次进入递增），用于触发进入动画重新执行
    card_enter_seq: u32,
    /// 当前窗口栅格化缩放比例（= DPI/96），用于把像素换算为 DIP，让图片按原像素显示
    dpi_scale: f64,
    /// 用于观察 RasterizationScale 的隐藏 Image 引用
    dpi_image: ElementRef<Image>,
    /// 1×1 透明 PNG 字节（作为观察者 Image 的占位 source）
    dpi_placeholder: Arc<Vec<u8>>,
    /// 卡片合成结果缓存：card.path -> 合成结果（成功或失败原因）。
    /// 避免窗口缩放/重渲染时重复合成，也避免损坏的卡图被反复解码
    compose_cache: RefCell<HashMap<PathBuf, Result<(Arc<[u8]>, f64, f64), Arc<str>>>>,
    /// 解码后的底图缓存（含文件指纹，用户替换底图文件后自动重新加载）
    base_cache: RefCell<Option<CardBaseCache>>,
    /// 更新状态共享（后台线程写、UI 读）
    update_shared: Arc<Mutex<UpdateStatus>>,
    /// 服务器地址（网络设置页可改），供后台探测线程每轮读取
    probe_host: Arc<Mutex<String>>,
    /// 进度轮询是否正在运行（避免重复启动）
    poll_running: bool,
    /// 进行中下载的取消标记（点「取消更新」置位，下载循环检测后中止）
    update_cancel: HashMap<UpdateTarget, Arc<AtomicBool>>,
    /// 进行中下载的句柄登记（取消时关闭句柄，强制解阻塞 WinHTTP 调用）
    update_abort: HashMap<UpdateTarget, Arc<HttpAbort>>,
    /// 是否有游戏正在运行（运行时不允许更新加载器）
    game_running: bool,
    /// 运行中游戏的进程句柄（退出轮询用；None 表示当前无游戏运行）
    game_runtime: Option<GameRuntime>,
    /// 运行中的 ServerBox 进程句柄（行内「启动」后跟踪；None = 未启动）
    serverbox_runtime: Option<ServerBoxRuntime>,
    /// 模态覆盖层显示中对应的游戏下标（None=不显示）：点击启动即弹出，
    /// 游戏拉起后显示封面+名称+「关闭游戏」，失败 / 退出 / 手动关闭时清除
    launch_overlay: Option<usize>,
    /// 最近一次「有更新」提醒对应的版本集（同批版本只提醒一次）
    update_prompt_key: Option<String>,
    /// 窗口图标文件路径（AppWindow::SetIcon 要求 'static；启动时一次性准备并泄漏）
    window_icon: Option<&'static str>,
    /// 当前悬停的游戏行下标（纯视觉高亮，不进动画、不重建整页）
    hovered: Option<usize>,
}

impl Component for Launcher {
    type Message = Message;
    type Input = ();

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let store = storage::Store::new();
        let library = store.load_library();
        let settings = store.load_settings();
        let net = store.load_network();
        let system = store.load_system();
        let coin = store.load_coin();
        let graphics = store.load_graphics();
        let input = store.load_input();
        let vpn = store.load_vpn();
        let room = store.load_room();
        let lang = store.load_lang();
        i18n::set_lang(lang);
        let detected = launch::detect_loaders();
        let server_shared = Arc::new(Mutex::new(server::ServerStatus::default()));
        // 服务器地址设置（网络设置页可改）：探测线程每轮读取，改动即时生效
        let probe_host = Arc::new(Mutex::new(net.server_host().to_string()));
        server::spawn_probe(server_shared.clone(), probe_host.clone());

        // 服务器状态轮询（任务完成后在 update 里再排下一个）
        let shared = server_shared.clone();
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_secs(2));
            Message::ServerPolled(shared.lock().unwrap().state)
        });

        // 网卡枚举放后台（极少数机器上 GetAdaptersAddresses 可能较慢，不阻塞首帧）
        context.spawn_background(|_| Message::AdaptersScanned(Box::new(netinfo::list_adapters())));

        // 输入设备枚举放后台（按已保存的输入类型决定扫描方式；未选类型则不扫描）
        let input_kind = input.kind.clone();
        context.spawn_background(move |_| {
            Message::InputDevicesScanned(Box::new(rawinput::list_input_devices(
                input_kind.as_deref(),
            )))
        });

        let mut launcher = Launcher {
            store,
            library,
            settings,
            detected,
            server_shared,
            server_state: ServerState::Unknown,
            nav_tag: "games".to_string(),
            pane_open: true,
            dialog: Dialog::None,
            pending: None,
            adapters: Vec::new(),
            adapters_scanned: false,
            net,
            system,
            lang,
            coin,
            graphics,
            input,
            vpn,
            vpn_status: VpnStatus::NotConfigured,
            vpn_handle: Arc::new(Mutex::new(None)),
            vpn_auto_retries: 0,
            vpn_connected_at: None,
            vpn_watch_gen: Arc::new(AtomicUsize::new(0)),
            room,
            room_status: RoomStatus::Idle,
            room_handle: Arc::new(Mutex::new(None)),
            room_watchdog: None,
            room_password: String::new(),
            room_members_invited: Vec::new(),
            room_host_down: None,
            room_list: Vec::new(),
            room_peers: Vec::new(),
            room_seat_ips: HashMap::new(),
            room_my_ip: None,
            room_saw_self_in_table: false,
            room_nick_pending: None,
            room_game: String::new(),
            room_name: String::new(),
            room_keychip: String::new(),
            room_member_down: HashMap::new(),
            room_member_ever_connected: HashSet::new(),
            room_last_heartbeat: None,
            room_host_sync: None,
            room_member_sync: None,
            room_sync_version: 0,
            room_sync_table: None,
            room_nick_override: None,
            room_dir_members: None,
            room_last_reconcile: None,
            room_last_playing_report: None,
            room_last_status_sent: None,
            room_overlay: None,
            room_filter_idx: None,
            room_filter_game_id: None,
            room_join_password: String::new(),
            input_devices: Vec::new(),
            input_devices_scanned: false,
            input_capture_gen: 0,
            input_capture_start: None,
            input_capture_remain: 0.0,
            coin_expanded: false,
            coin_anim: 0.0,
            coin_anim_gen: 0,
            ffb_expanded: true,
            ffb_anim: 1.0,
            ffb_anim_gen: 0,
            drop_index: None,
            drop_open: false,
            drop_anim: 0.0,
            drop_anim_gen: 0,
            hovered: None,
            light_theme: system_uses_light_theme(),
            cards_game: None,
            cards_aime: false,
            cards_id4: false,
            cards_id8: false,
            cards_id6: false,
            cards_loading: false,
            cards_warm: 0,
            cards: CardScan::default(),
            card_slide: 0.0,
            card_enter_seq: 0,
            win_size: WindowSize::default(),
            dpi_scale: 1.0,
            dpi_image: ElementRef::new(),
            dpi_placeholder: {
                let mut buf = Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]))
                    .write_to(&mut buf, image::ImageFormat::Png)
                    .expect("encode 1x1 placeholder png");
                Arc::new(buf.into_inner())
            },
            compose_cache: RefCell::new(HashMap::new()),
            base_cache: RefCell::new(None),
            update_shared: Arc::new(Mutex::new(UpdateStatus::default())),
            probe_host,
            poll_running: false,
            update_cancel: HashMap::new(),
            update_abort: HashMap::new(),
            game_running: false,
            game_runtime: None,
            serverbox_runtime: None,
            launch_overlay: None,
            update_prompt_key: None,
            window_icon: Self::prepare_window_icon(),
        };
        // 启动更新检查（启动即查一次 + 之后每约 1 小时自动复查）
        // 启动 launcher 后立即强制检查一次更新（绕过缓存）
        launcher.run_check(true, context);
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_secs(3600));
            Message::AutoCheckTick
        });
        // 启动即自动连接 VPN：仅当 VPN 配置齐全（已导入 .ovpn）时自动发起，
        // 失败由 VpnConnectResult 走 «连接失败» 提示，不影响启动流程。
        if launcher.vpn.config_path.is_some() {
            launcher.start_vpn_connect(context);
        }
        launcher
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            // None = 选中被清空（如进入无菜单项的卡片页），保持当前页
            Message::NavChanged(Some(tag)) => {
                self.nav_tag = tag;
                // 进入店内对战页：未配置 keychip 则弹窗并转系统设置；否则刷新列表并尝试回归已有房间
                if self.nav_tag == "room" {
                    let keychip_missing = self
                        .system
                        .keychip
                        .as_deref()
                        .map(|v| v.trim().is_empty())
                        .unwrap_or(true);
                    let has_rejoin_memory = self
                        .room
                        .last_room_id
                        .as_deref()
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false);
                    if keychip_missing && !has_rejoin_memory && matches!(self.room_status, RoomStatus::Idle) {
                        self.dialog = Dialog::LaunchFailed {
                            message: t!(
                                "未配置 Keychip 序列号，无法使用店内对战。
请先在「系统设置」页填写 Keychip 序列号。",
                                "Keychip serial is not configured; In-Store Battles is unavailable.
Fill in the Keychip serial on the System page first."
                            ).to_string(),
                            goto: None,
                        };
                        self.nav_tag = "system".to_string();
                    } else {
                        self.room_refresh_list(context, false);
                        self.room_try_resume(context);
                    }
                }
                // 进入网络设置页：自动刷新一次网卡列表（保证网卡实际地址 / 网段最新）
                if self.nav_tag == "network" {
                    self.adapters_scanned = false;
                    context.spawn_background(|_| {
                        Message::AdaptersScanned(Box::new(netinfo::list_adapters()))
                    });
                }
            }
            Message::NavChanged(None) => {}
            Message::PaneOpenChanged(open) => self.pane_open = open,
            Message::AddClicked => {
                context.spawn_background(|_| {
                    let picked = filedialog::pick_open_file(
                        &[
                            // 首项为默认过滤器：可执行文件 + ICF1 全部支持格式
                            (
                                t!("所有支持的格式", "All supported"),
                                "*.exe;*.elf;ICF1",
                            ),
                            (t!("游戏文件", "Executable"), "*.exe;*.elf"),
                            (t!("ICF1 文件", "ICF1 file"), "ICF1"),
                            (t!("所有文件", "All files"), "*.*"),
                        ],
                        t!("选择游戏文件", "Select game file"),
                    );
                    Message::FilePicked(picked)
                });
            }
            Message::FilePicked(None) => {}
            Message::FilePicked(Some(path)) => self.handle_file_picked(path, context),
            Message::Identified(boxed) => self.handle_identified(*boxed),
            Message::IcfIdentified(boxed) => self.handle_icf_identified(*boxed),
            Message::ServerPolled(state) => {
                self.server_state = state;
                let shared = self.server_shared.clone();
                context.spawn_background(move |_| {
                    std::thread::sleep(Duration::from_secs(15));
                    Message::ServerPolled(shared.lock().unwrap().state)
                });
            }
            Message::GameRowDropToggled(index) => self.toggle_row_drop(index, context),
            Message::GameRowDropTick(epoch, step) => self.tick_row_drop(epoch, step, context),
            Message::GameRowHovered(index) => self.hovered = Some(index),
            Message::GameRowUnhovered(index) => {
                if self.hovered == Some(index) {
                    self.hovered = None;
                }
            }

            Message::CardsScanned(scan) => self.apply_cards_scanned(scan, context),
            Message::CardClicked(index) => {
                let Some(gi) = self.cards_game else {
                    return;
                };
                let Some(name) = self.cards.cards.get(index).map(|c| c.name.clone()) else {
                    return;
                };
                // 写入本游戏记录（每个游戏独立保存），不再写全局 config.ini
                if let Some(rec) = self.library.games.get_mut(gi) {
                    rec.card = Some(name.clone());
                    self.store.save_library(&self.library);
                    self.cards.active = Some(name.clone());
                } else {
                    self.dialog = Dialog::Error {
                        message: t!(
                            "保存卡片选择失败：游戏不存在",
                            "Failed to save card selection: game no longer exists"
                        )
                        .to_string(),
                    };
                }
            }
            Message::CardsLoadTick => self.warm_card_cache(context),
            Message::AimeCardCreate => {
                if !self.cards_aime {
                    return;
                }
                let Some(dir) = self.aime_device_dir() else {
                    self.dialog = Dialog::Error {
                        message: t!("未配置 amdaemon.exe，无法确定卡片目录",
                            "amdaemon.exe is not configured; card directory unknown").to_string(),
                    };
                    return;
                };
                context.spawn_background(move |_| {
                    Message::AimeCardCreated(cards::create_aime_card_in(&dir).map_err(|e| e.to_string()))
                });
            }
            Message::AimeCardCreated(Ok(code)) => self.reload_aime_cards(code),
            Message::AimeCardCreated(Err(e)) => {
                self.dialog = Dialog::Error {
                    message: tf!("新建车卡失败：{}", "Failed to create card: {}", e),
                };
            }
            Message::BlankCardSelect => {
                // 空白车卡：cardfile 写 InidCrdXXXXX.png（游戏内置空白卡模板），
                // 启动时随 idas.ini 的 [CardReader] cardfile 一起下发给 mice
                let name = cards::blank_card_name();
                // 写入本游戏记录（每个游戏独立保存），不再写全局 config.ini
                if let Some(gi) = self.cards_game {
                    if let Some(rec) = self.library.games.get_mut(gi) {
                        rec.card = Some(name.clone());
                        self.store.save_library(&self.library);
                        self.cards.active = Some(name.clone());
                    } else {
                        self.dialog = Dialog::Error {
                            message: t!(
                                "保存卡片选择失败：游戏不存在",
                                "Failed to save card selection: game no longer exists"
                            )
                            .to_string(),
                        };
                    }
                }
            }
            Message::BackToGames => {
                self.nav_tag = "games".to_string();
                self.cards_game = None;
                self.cards_aime = false;
                self.cards_id4 = false;
                self.cards_id6 = false;
                self.cards_loading = false;
                self.cards_warm = 0;
            }
            Message::CardSlideTick(t) => {
                self.card_slide = t;
                // 递归调度下一帧（每 16ms 推进一次，约 300ms 完成滑入动画）
                if t < 1.0 {
                    let next = (t + 0.016 / 0.30).min(1.0);
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(16));
                        Message::CardSlideTick(next)
                    });
                }
            }
            Message::WindowResized(size) => {
                self.win_size = size;
            }
            Message::RowTestModeToggled(index, on) => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.test_mode = Some(on);
                    self.store.save_library(&self.library);
                }
            }
            Message::GfxPatchToggled(index, on) => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.gfx_patch = Some(on);
                    self.store.save_library(&self.library);
                }
            }
            Message::InputReverseToggled(slot, on) => {
                match slot {
                    0 => self.input.reverse_accel_axis = Some(on),
                    1 => self.input.reverse_brake_axis = Some(on),
                    _ => {}
                }
                self.save_input();
            }
            // ---- 力反馈卡片 ----
            Message::FfbExpanderToggled => self.toggle_ffb_expander(context),
            Message::FfbAnimTick(epoch, step) => self.tick_ffb_anim(epoch, step, context),
            Message::FfbEnableToggled(on) => {
                self.input.ffb_enable = Some(on);
                self.save_input();
            }
            Message::FfbParamChanged(idx, v) => {
                let value = v.round().clamp(0.0, FFB_MAX) as u32;
                match idx {
                    0 => self.input.ffb_constant_force = Some(value),
                    1 => self.input.ffb_damper = Some(value),
                    2 => self.input.ffb_rumble = Some(value),
                    3 => self.input.ffb_rumble_duration = Some(value),
                    4 => self.input.ffb_base_damper = Some(value),
                    5 => self.input.ffb_dead_band = Some(value),
                    _ => {}
                }
                self.save_input();
            }
            Message::RowMenu { index, action } => self.handle_row_menu(index, action, context),
            Message::CompanionPicked {
                kind,
                index,
                path: Some(path),
            } => self.handle_companion_picked(kind, index, path, context),
            Message::CompanionPicked { path: None, .. } => {}
            Message::CompanionIdentified {
                kind,
                index,
                path,
                result,
            } => self.handle_companion_identified(kind, index, path, result),
            Message::NamingTextChanged(name) => {
                if let Dialog::Naming { name: slot } = &mut self.dialog {
                    *slot = name;
                }
            }
            Message::GameRemoveClicked(index) => {
                if let Some(g) = self.library.games.get(index) {
                    self.dialog = Dialog::ConfirmRemove {
                        index,
                        name: g.name.clone(),
                    };
                }
            }
            Message::DialogClosed(ContentDialogResult::Primary) => self.apply_dialog(context),
            Message::DialogClosed(_) => {
                // 按键捕获弹窗取消：作废未完成的捕获并关掉倒计时
                if matches!(self.dialog, Dialog::InputCapture { .. }) {
                    self.end_capture();
                }
                // 未设昵称弹窗「跳过」：本次会话用默认昵称并重放暂挂的创建/加入动作
                // （主键「前往设置」走 apply_dialog，不进本分支）
                if let Dialog::RoomNickPrompt { pending } =
                    std::mem::replace(&mut self.dialog, Dialog::None)
                {
                    self.room_nick_override =
                        Some(crate::reactor_app::pages::room::ROOM_DEFAULT_NICKNAME.to_string());
                    match pending {
                        crate::reactor_app::message::PendingRoomAction::Create {
                            name,
                            game_idx,
                            password,
                        } => self.room_start_host(name, game_idx, password, context),
                        crate::reactor_app::message::PendingRoomAction::Join {
                            room_id,
                            password,
                        } => self.room_start_join(room_id, password, context),
                    }
                } else {
                    self.dialog = Dialog::None;
                }
                self.pending = None;
            }
            Message::DpiScaleChanged(s) => self.dpi_scale = s,
            Message::CardComposeFailed(summary) => {
                self.dialog = Dialog::Error { message: summary };
            }
            // ---- 网络设置页 ----
            Message::RefreshAdapters => {
                self.adapters_scanned = false;
                context.spawn_background(|_| {
                    Message::AdaptersScanned(Box::new(netinfo::list_adapters()))
                });
            }
            Message::AdaptersScanned(list) => {
                self.adapters = *list;
                self.adapters_scanned = true;
            }
            // 下拉框清除选择（None）一律忽略，避免误清已保存的配置
            Message::AdapterSelected(Some(index)) => {
                if let Some(a) = self.adapters.get(index) {
                    // 换网卡后原 IP 选择不再属于新网卡，重置为该网卡的第一个网段
                    // （仍可在下拉框改选其他网段）
                    self.net.adapter = Some(a.guid.clone());
                    self.net.ip = netinfo::subnet_labels(&a.ips).first().cloned();
                    // 选中 awl 虚拟网卡（店内对战）：主机IP = awl0 第一个地址
                    // （房主 .101 / 成员座位 IP），ServerBox IP 固定 .102，两字段锁定
                    if a.friendly == crate::awl::AWL_IFACE_NAME {
                        self.net.host_ip = self
                            .room_my_ip
                            .clone()
                            .or_else(|| a.ips.first().map(|ip| ip.ip.clone()));
                        self.net.serverbox_ip = Some(crate::awl::HOST_SERVERBOX_IP.to_string());
                    } else {
                        // 普通网卡：第一个 IP → 主机IP；第二个 IP → ServerBox IP，
                        // 没有第二个时默认取第一个 +1（此处只填字段，不配置网卡）
                        self.net.host_ip = a.ips.first().map(|ip| ip.ip.clone());
                        self.net.serverbox_ip = match a.ips.get(1) {
                            Some(second) => Some(second.ip.clone()),
                            None => a.ips.first().and_then(|ip| netinfo::next_ip(&ip.ip)),
                        };
                    }
                    self.save_net();
                }
            }
            Message::AdapterSelected(None) => {}
            Message::IpSelected(Some(index)) => {
                if let Some(a) = self.selected_adapter() {
                    let labels = netinfo::subnet_labels(&a.ips);
                    if let Some(cidr) = labels.get(index) {
                        self.net.ip = Some(cidr.clone());
                        self.save_net();
                    }
                }
            }
            Message::IpSelected(None) => {}
            Message::NetworkSimulateToggled(on) => {
                self.net.simulate = Some(on);
                self.save_net();
            }
            Message::ServerHostChanged(text) => {
                let t = text.trim().to_string();
                self.net.server = if t.is_empty() { None } else { Some(t) };
                self.save_net();
                // 探测线程每轮读取该值，改动即时生效
                if let Ok(mut h) = self.probe_host.lock() {
                    *h = self.net.server_host().to_string();
                }
            }
            Message::RoomNickChanged(text) => {
                // 昵称截断到 24 字符（按字符数，避免切出半个多字节字符）
                let t: String = text.trim().chars().take(24).collect();
                self.room.nickname = if t.is_empty() { None } else { Some(t) };
                let _ = self.store.save_room(&self.room);
            }
            Message::ServerboxIpChanged(text) => {
                let t = text.trim().to_string();
                // 仅「模拟网络」关闭时可配置；开启时忽略输入，字段保持未设置；
                // awl 网卡（店内对战）时两字段锁定，忽略输入
                if self.net.simulate == Some(false) && !self.awl_adapter_selected() {
                    self.net.serverbox_ip = if t.is_empty() { None } else { Some(t) };
                    self.save_net();
                }
            }
            Message::HostIpChanged(text) => {
                // awl 网卡（店内对战）时主机IP 锁定为房间地址，忽略输入
                if !self.awl_adapter_selected() {
                    let t = text.trim().to_string();
                    self.net.host_ip = if t.is_empty() { None } else { Some(t) };
                    self.save_net();
                }
            }
            Message::StaticIpRemoved(result) => {
                // 游戏退出后回删启动时添加的静态 IP；失败仅提示（下次启动会因
                // 「已有第二个地址」直接复用，不影响功能）
                if let Err(e) = result {
                    self.dialog = Dialog::Error {
                        message: tf!(
                            "删除网卡静态 IP 失败：{}",
                            "Failed to remove static IP from adapter: {}",
                            e
                        ),
                    };
                }
            }
            // ---- 系统设置页 ----
            // 候选首项「未设置」会把该键清空
            Message::LogLevelSelected(Some(index)) => {
                self.system.loglevel = SYS_LOGLEVELS
                    .get(index)
                    .and_then(|(v, _, _)| v.map(String::from));
                self.save_system();
            }
            Message::LogLevelSelected(None) => {}
            // 语言切换：更新模型字段 + 全局原子（驱动 t!）+ 持久化，框架自动重绘整窗
            Message::LangSelected(Some(index)) => {
                let new_lang = if index == 1 { Lang::En } else { Lang::Zh };
                if new_lang != self.lang {
                    self.lang = new_lang;
                    i18n::set_lang(new_lang);
                    if let Err(e) = self.store.save_lang(new_lang) {
                        self.dialog = Dialog::Error {
                            message: format!("{}: {e}", t!("保存语言失败", "Failed to save language")),
                        };
                    }
                }
            }
            Message::LangSelected(None) => {}
            Message::RegionSelected(Some(index)) => {
                self.system.region = SYS_REGIONS.get(index).map(|v| v.to_string());
                self.save_system();
            }
            Message::RegionSelected(None) => {}
            Message::KeychipChanged(text) => {
                let t = text.trim().to_string();
                self.system.keychip = (!t.is_empty()).then_some(t);
                self.save_system();
            }
            Message::SeatSelected(Some(index)) => {
                if let Some((v, _, _)) = SYS_SEATS.get(index) {
                    self.system.seat = Some(v.to_string());
                    // 选「单机」时自动勾选主机；其他机位保持用户手动的勾选状态
                    if *v == "SINGLE" {
                        self.system.master = Some(true);
                    }
                    self.save_system();
                }
            }
            Message::SeatSelected(None) => {}
            Message::SeatMasterToggled(on) => {
                self.system.master = Some(on);
                self.save_system();
            }
            Message::SkipReaderInitToggled(on) => {
                self.system.skip_reader_init = Some(on);
                self.save_system();
            }
            // ---- 投币设置 ----
            Message::CoinExpanderToggled => self.toggle_coin_expander(context),
            Message::CoinAnimTick(epoch, step) => self.tick_coin_anim(epoch, step, context),
            Message::FreeplayToggled(on) => {
                self.coin.freeplay = Some(on);
                self.save_coin();
            }
            Message::CoinValueChanged(field, value) => {
                // 清空输入 = 未设置；小数截断为整数
                let v = value.map(|f| f as u32);
                match field {
                    0 => self.coin.start_coin = v,
                    1 => self.coin.continue_coin = v,
                    2 => self.coin.card_coin = v,
                    _ => self.coin.car_coin = v,
                }
                self.save_coin();
            }
            // ---- 图形设置页 ----
            Message::GraphicsDefaultToggled(on) => {
                self.graphics.default = Some(on);
                self.save_graphics();
            }
            Message::GraphicsBorderlessToggled(on) => {
                self.graphics.borderless = Some(on);
                self.save_graphics();
            }
            Message::GraphicsResolutionSelected(Some(index)) => {
                self.graphics.resolution = GFX_RESOLUTIONS.get(index).map(|v| v.to_string());
                self.save_graphics();
            }
            Message::GraphicsResolutionSelected(None) => {}
            Message::GraphicsRenderScaleToggled(on) => {
                self.graphics.render_scale = Some(on);
                self.save_graphics();
            }
            Message::GraphicsRenderScaleLevelSelected(Some(index)) => {
                self.graphics.render_scale_level =
                    GFX_RENDER_SCALE_LEVELS.get(index).map(|v| v.to_string());
                self.save_graphics();
            }
            Message::GraphicsRenderScaleLevelSelected(None) => {}
            Message::GraphicsMsaaToggled(on) => {
                self.graphics.msaa = Some(on);
                self.save_graphics();
            }
            Message::GraphicsMsaaLevelSelected(Some(index)) => {
                self.graphics.msaa_level = GFX_MSAA_LEVELS.get(index).map(|v| v.to_string());
                self.save_graphics();
            }
            Message::GraphicsMsaaLevelSelected(None) => {}
            Message::GraphicsAfToggled(on) => {
                self.graphics.af = Some(on);
                self.save_graphics();
            }
            Message::GraphicsAfLevelSelected(Some(index)) => {
                self.graphics.af_level = GFX_AF_LEVELS.get(index).map(|v| v.to_string());
                self.save_graphics();
            }
            Message::GraphicsAfLevelSelected(None) => {}
            // ---- 输入设置页 ----
            Message::InputKindSelected(Some(index)) => {
                self.input.kind = INPUT_KINDS.get(index).map(|v| v.to_string());
                // 注意：切换模式不清空任何已保存的设备——
                // 两种模式的设备各存各的字段（xinput 用 pad_index，dinput 用 wheel/pedal/shifter），
                // 来回切换不会丢配置。
                self.save_input();
                // 选完类型立刻按该类型重新扫描一次设备
                self.refresh_input_devices(context);
            }
            Message::InputKindSelected(None) => {}
            Message::InputRefreshDevices => self.refresh_input_devices(context),
            Message::InputRatioChanged(v) => {
                let value = v.round().clamp(0.0, INPUT_RATIO_MAX) as u32;
                self.input.ratio = Some(value);
                self.save_input();
            }
            Message::InputShifterTypeSelected(Some(index)) => {
                self.input.shifter_type = SHIFTER_TYPES.get(index).map(|v| v.to_string());
                self.save_input();
            }
            Message::InputShifterTypeSelected(None) => {}
            Message::InputDualStickToggled(b) => {
                self.input.dual_stick = Some(b);
                self.save_input();
            }
            Message::InputLinearSteeringToggled(b) => {
                self.input.linear_steering = Some(b);
                self.save_input();
            }
            Message::InputLeftDeadzoneChanged(v) => {
                let value = v.round().clamp(0.0, 100.0) as u32;
                self.input.left_deadzone = Some(value);
                self.save_input();
            }
            Message::InputRightDeadzoneChanged(v) => {
                let value = v.round().clamp(0.0, 100.0) as u32;
                self.input.right_deadzone = Some(value);
                self.save_input();
            }
            Message::InputDevicesScanned(list) => {
                self.input_devices = *list;
                self.input_devices_scanned = true;
            }
            Message::InputDeviceSelected(slot, Some(index)) => self.apply_device_selected(slot, index),
            Message::InputDeviceSelected(_, None) => {}
            Message::InputRatioReset => {
                self.input.ratio = Some(INPUT_RATIO_DEFAULT);
                self.save_input();
            }
            Message::InputCaptureStart(slot) => self.start_capture(slot, context),
            Message::InputCaptureTick(epoch) => self.tick_capture(epoch, context),
            Message::InputCaptured(epoch, slot, button) => self.apply_captured_button(epoch, slot, button),
            Message::InputCapturedAxis(epoch, slot, axis_name) => self.apply_captured_axis(epoch, slot, axis_name),
            Message::InputCapturedXi(epoch, slot, name) => self.apply_captured_xi_button(epoch, slot, name),
            Message::InputCaptureTimeout(epoch, slot) => {
                if epoch != self.input_capture_gen {
                    return;
                }
                if let Some((zh, en)) = capture_label(slot) {
                    self.dialog = Dialog::Error {
                        message: tf!("捕获「{}」超时，未检测到按键", "Capture of \"{}\" timed out; no key detected",
                            i18n::pick(zh, en)
                        ),
                    };
                }
                self.end_capture();
            }
            // ---- VPN 设置 ----
            Message::VpnImportClicked => {
                context.spawn_background(|_| {
                    let picked = filedialog::pick_open_file(
                        &[(t!("OpenVPN 配置", "OpenVPN config"), "*.ovpn"), (t!("所有文件", "All files"), "*.*")],
                        t!("选择 VPN 配置文件", "Select VPN config file"),
                    );
                    Message::VpnConfigPicked(picked)
                });
            }
            Message::VpnConfigPicked(Some(path)) => {
                // 换配置前先断开旧连接：旧 openvpn 可能仍在跑，避免句柄与新状态漂移
                self.vpn_watch_gen.fetch_add(1, Ordering::Relaxed);
                self.vpn_auto_retries = 0;
                self.vpn_connected_at = None;
                if let Some(h) = self.vpn_handle.lock().unwrap().take() {
                    drop(h);
                }
                // 仅导入路径，不校验内容；明文密码留待用户在设置页填写
                self.vpn.config_path = Some(path.display().to_string());
                if let Err(e) = self.store.save_vpn(&self.vpn) {
                    self.dialog = Dialog::Error {
                        message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
                    };
                }
                self.vpn_status = VpnStatus::Disconnected;
            }
            Message::VpnConfigPicked(None) => {}
            Message::VpnPassphraseChanged(text) => {
                let t = text; // PasswordBox 回调已传回完整明文
                self.vpn.passphrase = if t.is_empty() {
                    None
                } else {
                    Some(t)
                };
                if let Err(e) = self.store.save_vpn(&self.vpn) {
                    self.dialog = Dialog::Error {
                        message: tf!("保存 config.ini 失败：{}", "Failed to save config.ini: {}", e),
                    };
                }
            }
            Message::VpnConnectClicked => self.start_vpn_connect(context),
            Message::VpnDisconnectClicked => {
                // 作废在途监督线程、清空重连计数，再取走句柄
                //（Drop 里杀进程、关作业对象、删密码/日志临时文件）
                self.vpn_watch_gen.fetch_add(1, Ordering::Relaxed);
                self.vpn_auto_retries = 0;
                self.vpn_connected_at = None;
                if let Some(h) = self.vpn_handle.lock().unwrap().take() {
                    drop(h);
                }
                self.vpn_status = VpnStatus::Disconnected;
            }
            Message::VpnConnectResult(None) => {
                // 计数不在此清零：自动重连预算由 VpnDied 侧按「稳定满 5 分钟重新计满」
                // 释放，否则「连上又立刻死」的抖动隧道会借每次成功清零无限重试
                self.vpn_connected_at = Some(Instant::now());
                self.vpn_status = VpnStatus::Connected;
                // 连接成功后启动监督：openvpn 意外退出时感知并自动重连
                self.spawn_vpn_watch(context);
            }
            Message::VpnConnectResult(Some(err)) => {
                if self.vpn_auto_retries > 0 {
                    // 自动重连失败：未达上限继续重试，达上限后停手待手动重连
                    if self.vpn_auto_retries < VPN_AUTO_RETRIES {
                        self.dialog = Dialog::Error {
                            message: tf!("自动重连失败（{}/{}）：{}\n将继续重试…",
                                "Auto-reconnect failed ({}/{}): {}\nRetrying...",
                                self.vpn_auto_retries, VPN_AUTO_RETRIES, err),
                        };
                        self.spawn_vpn_connect_task(context);
                    } else {
                        self.vpn_auto_retries = 0;
                        self.vpn_status = VpnStatus::Failed;
                        self.dialog = Dialog::Error {
                            message: tf!("VPN 自动重连失败（已重试 {} 次）：{}\n请检查网络后手动重连",
                                "VPN auto-reconnect failed after {} attempts: {}\nPlease reconnect manually",
                                VPN_AUTO_RETRIES, err),
                        };
                    }
                } else {
                    self.vpn_status = VpnStatus::Failed;
                    self.dialog = Dialog::Error { message: err };
                }
            }
            Message::VpnDied(None) => {}
            Message::VpnDied(Some(reason)) => {
                // 取走死句柄清理（kill 对已退出进程是 no-op，Drop 负责删密码/日志文件）；
                // 槽为空说明用户已先手动断开，忽略本次回报
                if self.vpn_handle.lock().unwrap().take().is_none() {
                    return;
                }
                // 连接已稳定运行过一段时间：重新给满自动重连预算，
                // 防「连上又立刻死」的抖动隧道无限循环（次数上限兜底）
                if self.vpn_connected_at.is_some_and(|t| t.elapsed() >= VPN_STABLE_RESET) {
                    self.vpn_auto_retries = 0;
                }
                if self.vpn_auto_retries < VPN_AUTO_RETRIES {
                    self.vpn_auto_retries += 1;
                    self.vpn_status = VpnStatus::Connecting;
                    self.dialog = Dialog::Error {
                        message: tf!("VPN 已断开（{reason}），正在自动重连（{}/{}）…",
                            "VPN disconnected ({reason}), reconnecting ({}/{})...",
                            self.vpn_auto_retries, VPN_AUTO_RETRIES),
                    };
                    self.spawn_vpn_connect_task(context);
                } else {
                    self.vpn_auto_retries = 0;
                    self.vpn_status = VpnStatus::Failed;
                    self.dialog = Dialog::Error {
                        message: tf!("VPN 已断开（{reason}），自动重连失败，请手动重连",
                            "VPN disconnected ({reason}); auto-reconnect failed, please reconnect manually"),
                    };
                }
            }
            // ---- 房间（虚拟局域网） ----
            Message::RoomFilterChanged(sel) => {
                // ComboBox 首项是「全部游戏」：换算为 games 下标（None = 全部），
                // room_filter_idx 存换算后的下标，渲染时 +1 回下拉坐标系，两侧一致
                let games_idx = match sel {
                    None | Some(0) => None,
                    Some(i) => Some(i - 1),
                };
                self.room_filter_idx = games_idx;
                self.room_filter_game_id = games_idx.and_then(|i| {
                    crate::db::games().get(i).map(|g| {
                        g.game_id.clone().unwrap_or_else(|| g.name.clone())
                    })
                });
                // 筛选变化立即按新条件静默刷新列表
                self.room_refresh_list(context, false);
            }
            Message::RoomCreateGameChanged(idx) => {
                if let Dialog::RoomCreate { game, .. } = &mut self.dialog {
                    *game = idx;
                }
            }
            Message::RoomCreateResult(res) => {
                self.room_overlay = None;
                self.room_on_created(res, context);
            }
            Message::RoomJoinResult(res) => {
                self.room_overlay = None;
                self.room_on_joined(res, context);
            }
            Message::RoomNickPromptReady => {
                // 延迟一拍后真正打开昵称提示弹窗（见 room_start_host/room_start_join 注释）
                if let Some(pending) = self.room_nick_pending.take() {
                    self.dialog = Dialog::RoomNickPrompt { pending };
                }
            }
            Message::RoomLeaveClicked | Message::RoomCloseClicked => self.room_stop(context),
            Message::RoomLaunchGame => self.room_launch(context, false),
            Message::RoomLaunchServerBox => self.room_launch(context, true),
            Message::RoomFinished(res) => {
                // 收起「正在关闭/退出房间…」覆盖层（失败也收起，避免困住界面）
                self.room_overlay = None;
                if let Err(e) = res {
                    self.dialog = Dialog::Error { message: e };
                }
                // 回到大厅后刷新公开房间列表
                self.room_refresh_list(context, false);
            }
            Message::RoomTick => self.room_on_tick(context),
            Message::RoomRefreshList => self.room_refresh_list(context, true),
            Message::RoomResumeResult(res) => {
                self.room_overlay = None;
                match res {
                Ok(None) => {
                    // 该 keychip 无历史房间：停留大厅
                }
                Ok(Some(room)) => {
                    self.room_status = RoomStatus::Hosting { room_id: room.room_id.clone(), name: room.name.clone() };
                    self.room_name = room.name;
                    self.room_game = room.game;
                    self.room_my_ip = Some(crate::awl::HOST_GAME_IP.to_string());
                    // 恢复房主密码（来自落盘的回归记忆），保证重启回归后仍正确显示为私有房间
                    self.room_password = self.room.last_room_password.clone().unwrap_or_default();
                    self.room_members_invited.clear();
                    // 房主回归：重启成员表长连接同步通道
                    self.room_dir_members = None;
                    self.room_last_reconcile = None;
                    self.room_last_playing_report = Some(self.game_running);
                    let host_sync = crate::room_sync::HostSync::start();
                    let host_peer = crate::awl::AwlManager::my_peer_id().unwrap_or_default();
                    let nick = self.room_nickname();
                    host_sync.publish(&host_peer, &[], self.game_running, Some(&nick));
                    self.room_host_sync = Some(host_sync);
                    self.room_schedule_tick(self.room_tick_interval(), context);
                    self.spawn_room_watchdog();
                }
                    Err(e) => self.dialog = Dialog::Error { message: e },
                }
            },
            Message::RoomOpenCreateDialog => {
                if self.room_status == RoomStatus::Idle {
                    self.dialog = Dialog::RoomCreate { name: String::new(), game: None, password: String::new() };
                }
            }
            Message::RoomCreateNameChanged(s) => {
                if let Dialog::RoomCreate { name, .. } = &mut self.dialog {
                    *name = s;
                }
            }
            Message::RoomCreatePasswordChanged(s) => {
                if let Dialog::RoomCreate { password, .. } = &mut self.dialog {
                    *password = s;
                }
            }
            Message::RoomOpenJoinDialog => {
                if self.room_status == RoomStatus::Idle {
                    self.dialog = Dialog::RoomJoin { room_id: String::new(), password: String::new() };
                }
            }
            Message::RoomJoinDirect(room_id) => {
                if self.room_status == RoomStatus::Idle {
                    // 公开房间行内直接加入：无密码
                    self.room_start_join(room_id, String::new(), context)
                }
            }
            Message::RoomJoinIdChanged(s) => {
                if let Dialog::RoomJoin { room_id, .. } = &mut self.dialog {
                    *room_id = s;
                }
            }
            Message::RoomJoinPasswordChanged(s) => {
                if let Dialog::RoomJoin { password, .. } = &mut self.dialog {
                    *password = s;
                }
            }
            Message::RoomListRefreshed(res, from_overlay) => {
                // 只有「刷新」覆盖层发起的请求才关覆盖层；
                // 进页时的静默刷新不能动它（否则会把回归/加入的 loading 提前关掉）
                if from_overlay {
                    self.room_overlay = None;
                }
                match res {
                    Ok(list) => self.room_list = list,
                    Err(e) => self.dialog = Dialog::Error { message: e },
                }
            }
            Message::LaunchDone { index, result, goto } => {
                self.handle_launch_done(index, result, goto)
            }
            Message::GameLaunched { runtime, .. } => {
                // 游戏进程已成功拉起：进入「运行中」，状态栏不再显示「正在启动游戏…」；
                // 启动退出轮询，进程真正退出时由 GamePollTick 回收并回报「已结束」。
                self.game_running = true;
                self.game_runtime = Some(runtime);
                Self::spawn_game_poll(context);
                // 游戏拉起后立即强制检查一次更新（游玩期间结果即可就绪）
                self.run_check(true, context);
            }
            Message::GamePollTick => self.poll_game_exit(context),
            Message::ServerBoxLaunched {
                result,
                runtime,
                goto,
            } => {
                self.serverbox_runtime = runtime;
                match result {
                    Ok(()) => {}
                    Err(e) => {
                        self.dialog = Dialog::LaunchFailed {
                            message: e,
                            goto,
                        };
                    }
                }
            }
            Message::LaunchStop(index) => {
                // 「关闭」：终止该游戏拉起的游戏 / 读卡器 / amdaemon / ServerBox 全部进程
                let game_running_here =
                    self.game_running && self.game_runtime.as_ref().map(|r| r.index) == Some(index);
                let sb = self.serverbox_runtime.take();
                let sb_running_here = sb.as_ref().map(|r| r.index) == Some(index);
                if !game_running_here && !sb_running_here {
                    return;
                }
                let rt = self.game_runtime.take();
                self.game_running = false;
                // 立即收起模态覆盖层（进程终止在后台线程执行，不阻塞 UI）
                if game_running_here && self.launch_overlay == Some(index) {
                    self.launch_overlay = None;
                }
                // 启动前添加到本机网卡的 ServerBox 静态 IP：手动「关闭」同样必须回删
                // （自然退出由 poll_game_exit 负责；本分支 take 走了 game_runtime，
                //  轮询拿不到 added_static_ip，不在此处回删就会永久残留）。
                // 单独起后台任务执行 netsh，结果经 StaticIpRemoved 回灌（失败弹窗）。
                let added_static_ip = rt.as_ref().and_then(|r| r.added_static_ip.clone());
                if let Some((adapter, ip)) = added_static_ip {
                    context.spawn_background(move |_| {
                        Message::StaticIpRemoved(launch::remove_serverbox_static_ip(&adapter, &ip))
                    });
                }
                context.spawn_background(move |_| {
                    if let Some(r) = &rt {
                        // 按映像名回收伴随进程：segatools 的 amdaemon（inject 派生，句柄杀不到）、
                        // 手动启动的 ServerBox（启动器无句柄）；未在运行的静默忽略
                        r.taskkill_images();
                        if let Ok(mut c) = r.child.lock() {
                            let _ = c.kill();
                        }
                        if let Ok(mut v) = r.companions.lock() {
                            for mut ch in v.drain(..) {
                                let _ = ch.kill();
                            }
                        }
                    }
                    if sb_running_here {
                        if let Some(s) = &sb {
                            if let Ok(mut c) = s.child.lock() {
                                let _ = c.kill();
                            }
                        }
                    }
                    Message::LaunchDone { index, result: Ok(()), goto: None }
                });
            }
            Message::ServerBoxStop(index) => {
                if self.serverbox_runtime.as_ref().map(|r| r.index) != Some(index) {
                    return;
                }
                let sb = self.serverbox_runtime.take();
                if let Some(s) = &sb {
                    if let Ok(mut c) = s.child.lock() {
                        let _ = c.kill();
                    }
                }
            }
            // ---------------- 自动更新 ----------------
            Message::CheckUpdatesNow => self.run_check(true, context),
            Message::AutoCheckTick => {
                // 排下一小时定时器
                let _ = context.spawn_background(move |_| {
                    std::thread::sleep(Duration::from_secs(3600));
                    Message::AutoCheckTick
                });
                if self.settings.auto_check_updates {
                    self.run_check(false, context);
                }
            }
            Message::UpdatesChecked(res) => self.apply_updates_checked(res),
            Message::UpdateLoader(target) => self.start_loader_update(target, context),
            Message::CancelUpdate(target) => self.cancel_loader_update(target),
            Message::UpdateAllClicked => self.update_all_loaders(context),
            Message::UpdateFinished(target, result) => self.finish_update(target, result),
            Message::UpdatePoll => self.poll_update_progress(context),
            Message::AutoUpdateToggled(v) => {
                self.settings.auto_check_updates = v;
                self.store.save_settings(&self.settings);
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title(t!("Sasayaki Launcher", "Sasayaki Launcher"));

        // 观察当前窗口的栅格化缩放比例，用于把物理像素换算为 DIP
        let dpi_image = self.dpi_image.clone();
        let sender = context.sender();
        context.use_effect("dpi-scale-observer", (), move || {
            let dpi_image = dpi_image.clone();
            let sender = sender.clone();
            let observation = dpi_image.observe_rasterization_scale(move |scale| {
                let _ = sender.send(Message::DpiScaleChanged(scale));
            });
            Some(Box::new(move || drop(observation)))
        });

        // 窗口视觉：Mica 背景 + 系统主题 + 固定客户区；图标可用时设置标题栏 / 任务栏图标
        let mut visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .theme(WindowTheme::System)
            .client_size(1180.0, 720.0);
        if let Some(icon) = self.window_icon {
            visuals = visuals.icon(icon);
        }
        context.window_visuals(visuals);

        // 监听窗口尺寸变化，用于卡片随窗口缩放（50%~100%）
        context.on_window_size(context.callback(Message::WindowResized));

        let tag = self.nav_tag.clone();
        let nav_item = |t: &'static str, label: &'static str, icon: Symbol| {
            let selected = tag == t;
            KeyedView::new(
                t,
                NavigationViewItem::new()
                    .tag(t)
                    .is_selected(selected)
                    .slots([
                        SlotView::new(NavigationViewItemSlot::Content, label),
                        SlotView::new(NavigationViewItemSlot::Icon, SymbolIcon::new().symbol(icon)),
                    ]),
            )
        };
        let menu_items = vec![
            nav_item("games", t!("游戏", "Games"), Symbol::Play),
            nav_item("room", t!("店内对战", "In-Store Battles"), Symbol::Shop),
            nav_item("graphics", t!("图形设置", "Graphics"), Symbol::Video),
            nav_item("network", t!("网络设置", "Network"), Symbol::Globe),
            nav_item("vpn", t!("VPN 设置", "VPN"), Symbol::Permissions),
            nav_item("system", t!("系统设置", "System"), Symbol::Setting),
            nav_item("input", t!("输入设置", "Input"), Symbol::Keyboard),
            nav_item("settings", t!("设置", "Settings"), Symbol::Repair),
        ];

        let content: View = match self.nav_tag.as_str() {
            "cards" => self.cards_page(context),
            "graphics" => self.graphics_page(context),
            "network" => self.network_page(context),
            "room" => self.room_page(context),
            "vpn" => self.vpn_page(context),
            "system" => self.system_page(context),
            "input" => self.input_page(context),
            "settings" => self.settings_page(context),
            _ => self.games_page(context),
        };

        let pane_open = self.pane_open;
        let navigation = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::Left)
            .open_pane_length(170.0)
            .is_pane_open(pane_open)
            .on_is_pane_open_changed(context.callback(Message::PaneOpenChanged))
            .is_pane_toggle_button_visible(true)
            .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
            .is_settings_visible(false)
            .always_show_header(false)
            .pane_title(t!("Sasayaki 启动器", "Sasayaki Launcher"))
            .on_selected_tag_changed(
                context.callback(|opt: Option<String>| Message::NavChanged(opt)),
            )
            .grid_row(1)
            .slots([
                SlotView::collection(NavigationViewSlot::MenuItems, menu_items),
                SlotView::new(
                    NavigationViewSlot::Content,
                    // 窗口主体（内容区）右侧边界：仅右侧 1px 描边，与左侧导航分隔线呼应
                    Border::new()
                        .border_brush(ThemeBrush::CardStroke)
                        .border_thickness(Thickness::new(0.0, 0.0, 1.0, 0.0))
                        .content(content),
                ),
            ]);

        let dialog_view = self.dialog_view(context);

        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .children((
                // 顶栏（标题 + 添加游戏）
                self.top_bar(context),
                // 导航 + 内容
                navigation,
                // 状态栏
                self.status_bar(),
                // 弹窗
                dialog_view,
                // 隐藏的 DPI 观察者（1×1 透明 PNG，opacity 0），堆叠在 row 0
                Image::new()
                    .source_data(EncodedImage::new((*self.dpi_placeholder).clone()))
                    .element_ref(&self.dpi_image)
                    .width(1.0)
                    .height(1.0)
                    .opacity(0.0)
                    .grid_row(0),
                // 游戏启动 / 运行中的模态覆盖层（None 时为空视图）：最顶层、跨全部行
                self.launch_overlay_view(context),
                // 房间创建/加入/回归的 loading 覆盖层（同款锁定背景方案）
                self.room_overlay_view(),
            ))
    }
}

impl Launcher {
    /// 准备窗口图标：把内嵌 ico 写到临时目录（内容不变则跳过），返回长期有效的路径。
    /// 路径串按 'static 泄漏（仅一次、几十字节）；写失败则返回 None（exe 内嵌资源不受影响）。
    fn prepare_window_icon() -> Option<&'static str> {
        let path = std::env::temp_dir().join("sasayaki_launcher.ico");
        let stale = match std::fs::read(&path) {
            Ok(bytes) => bytes != APP_ICON,
            Err(_) => true,
        };
        if stale && std::fs::write(&path, APP_ICON).is_err() {
            return None;
        }
        let path_str = path.to_str()?;
        Some(Box::leak(path_str.to_string().into_boxed_str()))
    }

    fn top_bar(&self, context: &mut ViewContext<Self>) -> View {
        Border::new()
            .grid_row(0)
            .margin(Thickness::new(16.0, 12.0, 16.0, 8.0))
            .content(
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        TextBlock::new()
                            .text(t!("Sasayaki 启动器", "Sasayaki Launcher"))
                            .font_size(16.0)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(0),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(1)
                            .children((
                                // 访问 DNET 网站：说明文字 + 原生 HyperlinkButton（手型 + 下划线，
                                // navigate_uri 直接拉起默认浏览器）。置于「添加游戏」左侧。
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(6.0)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .children((
                                        TextBlock::new()
                                            .text(t!("访问DNET网站", "Visit DNET website"))
                                            .font_size(14.0)
                                            .vertical_alignment(VerticalAlignment::Center)
                                            .opacity(0.8),
                                        HyperlinkButton::new()
                                            .navigate_uri("https://dnet.arcadehub.xyz")
                                            .expect("homepage uri 合法")
                                            .content(
                                                TextBlock::new()
                                                    .text("https://dnet.arcadehub.xyz")
                                                    .font_size(14.0)
                                                    .foreground(ThemeBrush::Accent),
                                            ),
                                    )),
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .on_click(context.message(Message::AddClicked))
                                    .content(t!("＋ 添加游戏", "Add Game")),
                            )),
                    )),
            )
    }

    fn status_bar(&self) -> View {
        let srv_color = match self.server_state {
            ServerState::Online => Color::argb(255, 0x6C, 0xCB, 0x5F),
            ServerState::Offline => Color::argb(255, 0xFF, 0x99, 0xA4),
            ServerState::Unknown => Color::argb(255, 0x9E, 0x9E, 0x9E),
        };
        let vpn_color = match self.vpn_status {
            VpnStatus::NotConfigured => Color::argb(255, 0x9E, 0x9E, 0x9E),
            VpnStatus::Disconnected => Color::argb(255, 0xFF, 0x99, 0xA4),
            VpnStatus::Connecting => Color::argb(255, 0x9E, 0x9E, 0x9E),
            VpnStatus::Connected => Color::argb(255, 0x6C, 0xCB, 0x5F),
            VpnStatus::Failed => Color::argb(255, 0xFF, 0x99, 0xA4),
        };

        // 更新状态指示灯：错误/检查中/有更新/已最新
        let (upd_color, upd_label) = {
            let s = self.update_shared.lock().unwrap();
            let mut checking = false;
            let mut outdated = false;
            let mut error = false;
            let mut up_to_date = false;
            for ts in s.per_target.values() {
                match ts.state {
                    UpdateState::Checking => checking = true,
                    UpdateState::Outdated => outdated = true,
                    UpdateState::Error => error = true,
                    UpdateState::UpToDate => up_to_date = true,
                    UpdateState::NotConfigured => {}
                }
                if matches!(
                    ts.phase,
                    UpdatePhase::Checking
                        | UpdatePhase::Downloading
                        | UpdatePhase::Extracting
                        | UpdatePhase::Installing
                ) {
                    checking = true;
                }
            }
            if error {
                (
                    Color::argb(255, 0xFF, 0x99, 0xA4),
                    t!("更新错误", "Update error"),
                )
            } else if checking {
                (
                    Color::argb(255, 0xFF, 0xB0, 0x4D),
                    t!("检查更新…", "Checking…"),
                )
            } else if outdated {
                (
                    Color::argb(255, 0xFF, 0xB0, 0x4D),
                    t!("有更新", "Update available"),
                )
            } else if up_to_date {
                // 检查已结束且全部组件都是最新：亮绿灯
                (
                    Color::argb(255, 0x6C, 0xCB, 0x5F),
                    t!("已最新", "Up to date"),
                )
            } else {
                // 尚未检查 / 未配置更新源：灰灯
                (
                    Color::argb(255, 0x9E, 0x9E, 0x9E),
                    t!("未检查", "Not checked"),
                )
            }
        };

        // 圆形指示灯：固定尺寸的 Border 配合等于半径的圆角即为圆点
        let light = |color: Color| -> View {
            Border::new()
                .width(11.0)
                .height(11.0)
                .corner_radius(5.5)
                .background(Brush::Solid(color))
                .into()
        };
        // 标签 + 指示灯：保留「服务器」「VPN」字样，状态仅由灯的颜色表达
        let item = |label: &str, color: Color| -> View {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text(label.to_string())
                        .font_size(12.0)
                        .opacity(0.7),
                    light(color),
                ))
                .into()
        };

        Border::new()
            .grid_row(2)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0))
            .margin(Thickness::new(16.0, 0.0, 16.0, 4.0))
            .padding(Thickness::new(0.0, 6.0, 0.0, 6.0))
            .content(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(20.0)
                    .children((
                        item(t!("服务器", "Server"), srv_color),
                        item("VPN", vpn_color),
                        {
                            let (room_color, room_label) = match &self.room_status {
                                RoomStatus::Idle => (Color::argb(255, 0x9E, 0x9E, 0x9E), t!("房间", "Room")),
                                RoomStatus::Creating { .. } | RoomStatus::Joining { .. } => {
                                    (Color::argb(255, 0x9E, 0x9E, 0x9E), t!("房间连接中…", "Room connecting…"))
                                }
                                RoomStatus::Hosting { .. } | RoomStatus::Joined { .. } => {
                                    (Color::argb(255, 0x6C, 0xCB, 0x5F), t!("房间", "Room"))
                                }
                            };
                            item(room_label, room_color)
                        },
                        item(upd_label, upd_color),
                    )),
            )
    }
}
