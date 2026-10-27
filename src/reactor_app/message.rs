//! UI 事件词表与弹窗/动作类型（reactor_app 子树内共享）。
use std::collections::HashMap;
use std::path::PathBuf;

use windows_reactor::{ContentDialogResult, WindowSize};

use crate::cards::CardScan;
use crate::db;
use crate::detect::IdentifyResult;
use crate::icf;
use crate::netinfo::AdapterInfo;
use crate::rawinput;
use crate::server::ServerState;
use crate::update::{TargetStatus, UpdateTarget};

use super::state::{GameRuntime, ServerBoxRuntime};

/// 游戏行下拉配置卡片里的动作（由行内按钮直接触发）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Launch,
    LaunchServerBox,
    Cards,
    AddServerBox,
    RemoveServerBox,
    AddAmdaemon,
    RemoveAmdaemon,
    AddGameExe,
    RemoveGameExe,
}

/// 伴随文件类型：ServerBox、读卡器（picodaemon）、segatools 游戏的
/// amdaemon.exe 与游戏主程序
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionKind {
    ServerBox,
    Amdaemon,
    GameExe,
}

#[derive(Clone)]
pub enum Message {
    NavChanged(Option<String>),
    PaneOpenChanged(bool),
    AddClicked,
    FilePicked(Option<PathBuf>),
    Identified(Box<IdentifyPayload>),
    /// 「添加游戏」选中 ICF1 文件后的识别结果（ICF 解析 + 识别库 game_id+版本匹配）
    IcfIdentified(Box<IcfPayload>),
    ServerPolled(ServerState),
    CardsScanned(Box<CardScan>),
    CardClicked(usize),
    WindowResized(WindowSize),
    CardSlideTick(f64),
    BackToGames,
    /// 展开/收起指定游戏行的下拉配置卡片（仿「投币设置」展开卡片）
    GameRowDropToggled(usize),
    /// 游戏行悬停进入：整卡/行头变浅灰高亮（纯视觉，不进动画、不重建整页）
    GameRowHovered(usize),
    /// 游戏行悬停离开：恢复底色
    GameRowUnhovered(usize),
    /// 行下拉卡片展开/收起动画推进：u32 为动画代数（过期循环作废），f64 为每帧步进
    GameRowDropTick(u32, f64),
    /// 游戏行下拉配置卡片动作
    RowMenu {
        index: usize,
        action: RowAction,
    },
    /// 游戏行「测试模式」勾选框状态变化：勾选后启动游戏会在命令行末尾追加 `-t`
    RowTestModeToggled(usize, bool),
    /// 输入设置：油门 / 刹车「反转轴值」勾选框变化（slot 0=油门，1=刹车）
    InputReverseToggled(usize, bool),
    /// 输入设置：力反馈卡片展开/收起
    FfbExpanderToggled,
    /// 力反馈展开/收起动画推进：u32=代数（过期作废），f64=每帧步进
    FfbAnimTick(u32, f64),
    /// 输入设置：力反馈总开关变化
    FfbEnableToggled(bool),
    /// 输入设置：力反馈参数滑块变化（idx：0=constantForce 1=damper 2=rumble 3=rumbleDuration 4=baseDamper 5=deadBand）
    FfbParamChanged(usize, f64),
    /// 伴随文件选择结果（ServerBox / 读卡器）：由行下拉卡片「添加/更换」触发
    CompanionPicked {
        kind: CompanionKind,
        index: usize,
        path: Option<PathBuf>,
    },
    /// 伴随文件识别结果
    CompanionIdentified {
        kind: CompanionKind,
        index: usize,
        path: PathBuf,
        result: Result<IdentifyResult, String>,
    },
    NamingTextChanged(String),
    /// 点击游戏行下拉卡片的「移除游戏」：弹出二次确认框
    GameRemoveClicked(usize),
    DialogClosed(ContentDialogResult),
    DpiScaleChanged(f64),
    /// 卡片合成失败汇总（由卡片页的 use_effect 发出，写入状态栏 toast）
    CardComposeFailed(String),
    /// 卡片页加载推进：分拍预热卡片合成缓存，避免大量卡片一次解码卡住 UI
    CardsLoadTick,
    /// 新建 aime 车卡（segatools：在 amdaemon 目录 DEVICE\ 生成 20 位数字 .txt）
    AimeCardCreate,
    /// 新建 aime 车卡结果（Ok = 新卡的 access code）
    AimeCardCreated(Result<String, String>),
    /// 选用空白车卡（IDAS：cardfile 写 InidCrdXXXXX.png，XXXXX 为 5 位随机数字）
    BlankCardSelect,
    /// 图形补丁勾选切换（IDAS6~8 / mice 游戏）：控制启动时是否部署 winmm.dll + scripts
    GfxPatchToggled(usize, bool),
    // ---- 网络设置页 ----
    RefreshAdapters,
    AdaptersScanned(Box<Vec<AdapterInfo>>),
    AdapterSelected(Option<usize>),
    IpSelected(Option<usize>),
    /// 模拟网络开关切换
    NetworkSimulateToggled(bool),
    /// 服务器地址（域名 / IP）编辑；空 = 默认 sdgt.arcadehub.xyz
    ServerHostChanged(String),
    /// 房间昵称编辑（设置页）；空 = 未设置（创建/加入房间时弹窗提示）
    RoomNickChanged(String),
    /// ServerBox 静态绑定 IP 编辑（仅「模拟网络」关闭时可配置）；空 = 未配置
    ServerboxIpChanged(String),
    /// 主机（本机游戏）IP 编辑（选中 awl 网卡时锁定）；空 = 未配置
    HostIpChanged(String),
    /// 启动游戏前为选中网卡添加的 ServerBox 静态 IP 已在游戏退出后删除；
    /// Err = netsh 删除失败（仅提示，不阻断）
    StaticIpRemoved(Result<(), String>),
    // ---- 系统设置页 ----
    LogLevelSelected(Option<usize>),
    /// 界面语言选择（下拉框：中文 / English）；切换后立即重绘整窗
    LangSelected(Option<usize>),
    RegionSelected(Option<usize>),
    KeychipChanged(String),
    SkipReaderInitToggled(bool),
    /// 机位选择（系统设置页，仅对 SDGT 生效）；选择「单机」时自动勾选「主机」
    SeatSelected(Option<usize>),
    /// 机位设置行的「主机」勾选框切换（对应 segatools.ini dipsw1）
    SeatMasterToggled(bool),
    // ---- 投币设置 ----
    /// 展开/收起投币设置卡片
    CoinExpanderToggled,
    /// 投币设置展开/收起动画推进：u32 为动画代数（过期循环作废），f64 为每帧步进
    CoinAnimTick(u32, f64),
    FreeplayToggled(bool),
    /// 币数输入变化：usize 为字段序号（0=开始 1=续关 2=购车卡 3=购买车辆）
    CoinValueChanged(usize, Option<f64>),
    // ---- 图形设置页 ----
    GraphicsDefaultToggled(bool),
    GraphicsBorderlessToggled(bool),
    GraphicsResolutionSelected(Option<usize>),
    GraphicsRenderScaleToggled(bool),
    GraphicsRenderScaleLevelSelected(Option<usize>),
    GraphicsMsaaToggled(bool),
    GraphicsMsaaLevelSelected(Option<usize>),
    GraphicsAfToggled(bool),
    GraphicsAfLevelSelected(Option<usize>),
    // ---- 输入设置页 ----
    InputKindSelected(Option<usize>),
    InputDevicesScanned(Box<Vec<rawinput::GameDevice>>),
    InputRefreshDevices,
    /// 设备选择：usize 为类别（0=方向盘 1=油门踏板 2=排挡 3=XInput 手柄）
    InputDeviceSelected(usize, Option<usize>),
    /// 开始捕获指定槽位（0~7，对应 INPUT_BUTTONS）
    InputCaptureStart(usize),
    /// 捕获成功：u32=代数（过期作废） usize=槽位 u32=按键号
    InputCaptured(u32, usize, u32),
    /// 模拟轴捕获成功：u32=代数（过期作废） usize=槽位 String=轴名（X/Y/Rz…）
    InputCapturedAxis(u32, usize, String),
    /// XInput 按钮名映射捕获成功：u32=代数（过期作废） usize=槽位（XINPUT_MAP_BASE+local） String=mice 按钮名
    InputCapturedXi(u32, usize, String),
    /// 捕获超时（后台捕获线程 10 秒无按键）
    InputCaptureTimeout(u32, usize),
    /// 倒计时推进：u32=代数（过期作废），驱动进度条并在归零时自动关闭
    InputCaptureTick(u32),
    /// 输入信号限制滑块变化（0~255；原「输入比率」）
    InputRatioChanged(f64),
    /// 输入信号限制恢复默认（128）
    InputRatioReset,
    /// 排挡类型选择（下标对应 SHIFTER_TYPES）
    InputShifterTypeSelected(Option<usize>),
    /// 双摇杆模式（XInput）：开启 → kitten `singleStickSteering = false`
    InputDualStickToggled(bool),
    /// 线性方向（XInput）：开启 → kitten `linearSteering = true`
    InputLinearSteeringToggled(bool),
    /// 左摇杆死区（百分比 0~100，默认 20）：XInput
    InputLeftDeadzoneChanged(f64),
    /// 右摇杆死区（百分比 0~100，默认 20）：XInput
    InputRightDeadzoneChanged(f64),
    // ---- VPN 设置页 ----
    /// 点击「导入配置」后后台文件选择的结果
    VpnConfigPicked(Option<PathBuf>),
    /// 点击「导入配置」按钮：后台打开文件对话框
    VpnImportClicked,
    /// 私钥密码输入框变化（PasswordBox 回调传回明文）
    VpnPassphraseChanged(String),
    /// 手动「连接」按钮
    VpnConnectClicked,
    /// 手动「断开」按钮
    VpnDisconnectClicked,
    /// 连接后台任务结果：None = 成功（已连接），Some(err) = 失败（带错误信息）
    VpnConnectResult(Option<String>),
    /// VPN 监督线程回报：Some(reason) = openvpn 进程已意外退出（reason 为日志提炼原因）；
    /// None = 监督线程因代数过期/句柄被取走而静默退出（update 直接忽略）
    VpnDied(Option<String>),
    // ---- 房间（虚拟局域网） ----
    /// 大厅游戏筛选变化（None/Some(0) = 全部；Some(i>0) = db::games()[i-1]）
    RoomFilterChanged(Option<usize>),
    /// 创建对话框：游戏选择变化（下标对应 db::game_names()）
    RoomCreateGameChanged(Option<usize>),
    /// 开房后台任务结果：Ok(room_id) / Err(错误信息)
    RoomCreateResult(Result<String, String>),
    /// 加入后台任务结果：Ok(JoinInfo 含座位 IP/游戏) / Err(错误信息)
    RoomJoinResult(Result<crate::roomdir::JoinInfo, String>),
    /// 成员点「离开房间」
    RoomLeaveClicked,
    /// 房主点「关闭房间」
    RoomCloseClicked,
    /// 房间内直接启动本房间的游戏（库中未添加则跳转游戏页）
    RoomLaunchGame,
    /// 房间内直接启动 ServerBox（未配置则跳转游戏页）
    RoomLaunchServerBox,
    /// 退出房间的后台清理完成（手动离开/关闭、房主端异常均走此消息收尾）
    RoomFinished(Result<(), String>),
    /// 房间状态轮询心跳（2s 一拍；状态机在此推进成员 invite / 掉线检测 / 目录心跳）
    RoomTick,
    /// 房间页：打开「创建房间」对话框
    RoomOpenCreateDialog,
    /// 创建对话框：房间名输入
    RoomCreateNameChanged(String),
    /// 创建对话框：密码输入（非空 = 私密房）
    RoomCreatePasswordChanged(String),
    /// 房间页：打开「加入房间」对话框
    RoomOpenJoinDialog,
    /// 延迟一拍后打开昵称提示弹窗：昵称为空分支此刻仍处在创建/加入弹窗
    /// on_closed 的消息周期内，WinUI 同一时刻只允许一个 ContentDialog，
    /// 立即开第二个会被静默拒绝（弹窗不出现 → 创建/加入看似没反应）。
    /// 动作参数先暂挂在 `room_nick_pending`，本消息到达后再真正开窗。
    RoomNickPromptReady,
    /// 房间页：手动刷新公开房间列表
    RoomRefreshList,
    /// 房间页：公开房间行内直接加入（公开房无需密码）
    RoomJoinDirect(String),
    /// 加入对话框：房间 ID 输入
    RoomJoinIdChanged(String),
    /// 加入对话框：密码输入
    RoomJoinPasswordChanged(String),
    /// 公开房间列表刷新结果；bool = 是否由「刷新」覆盖层发起（是则收到时关闭覆盖层）
    RoomListRefreshed(Result<Vec<crate::roomdir::RoomSummary>, String>, bool),
    /// 回归已创建房间的结果：Ok(None)=无历史房间；Ok(Some(房间))=已回归；Err=失败
    RoomResumeResult(Result<Option<crate::roomdir::MyRoom>, String>),
    // ---- 自动更新 ----
    /// 手动「立即检查」（强制走网络，绕过 24h 缓存）
    CheckUpdatesNow,
    /// 定时自动检查（允许使用缓存；同时负责每约 1 小时重新排期）
    AutoCheckTick,
    /// 更新源 Release 拉取/解析完成（可能为网络/解析错误）
    UpdatesChecked(Result<HashMap<UpdateTarget, TargetStatus>, String>),
    /// 用户点「更新」某个加载器
    UpdateLoader(UpdateTarget),
    /// 用户点「取消更新」（中止进行中的下载）
    CancelUpdate(UpdateTarget),
    /// 用户点「全部更新」（批量更新所有有更新的组件）
    UpdateAllClicked,
    /// 进度轮询心跳（触发重绘；实际进度由后台线程写入共享状态）
    UpdatePoll,
    /// 下载 + 安装结束
    UpdateFinished(UpdateTarget, Result<(), String>),
    /// 自动检查开关
    AutoUpdateToggled(bool),
    /// 启动游戏的后台任务结果（含游戏下标，便于回写 play_count）。
    /// 启动游戏不再自动连接/断开 VPN，VPN 由 VPN 设置页手动控制。
    LaunchDone {
        index: usize,
        result: Result<(), String>,
        /// 启动前检查失败时的跳转目标：(nav_tag, 可选游戏下标)。
        /// None = 普通错误（仅展示，无跳转按钮）。
        goto: Option<(String, Option<usize>)>,
    },
    /// 游戏进程已成功拉起（进入「运行中」）；真正的「退出」由轮询任务（GamePollTick）回报 LaunchDone。
    /// 拆分目的是避免加载器进程驻留时阻塞后台任务、导致状态栏一直停在「正在启动游戏…」。
    /// 携带进程句柄（Arc<Mutex> 以满足 Message 的 Clone 约束），供轮询任务判断退出并回收伴随进程。
    GameLaunched { runtime: GameRuntime },
    /// 运行中游戏进程退出轮询心跳：检查 child 是否已退出，未退出则排下一拍
    GamePollTick,
    /// ServerBox 手动启动的后台任务结果（toast 提示，不回收进程）。
    ServerBoxLaunched {
        result: Result<(), String>,
        /// 启动成功时的进程句柄（供「关闭」按钮回收）；失败为 None
        runtime: Option<ServerBoxRuntime>,
        /// 启动前检查失败时的跳转目标：(nav_tag, 可选游戏下标)。
        goto: Option<(String, Option<usize>)>,
    },
    /// 关闭该游戏拉起的全部进程（游戏 + 读卡器 / amdaemon + ServerBox）
    LaunchStop(usize),
    /// 关闭单独启动的 ServerBox
    ServerBoxStop(usize),
}

#[derive(Clone)]
pub struct IdentifyPayload {
    pub(super) path: PathBuf,
    pub(super) result: Result<IdentifyResult, String>,
    /// 添加游戏时在同目录自动探测到的读卡器（命中识别库 card_reader）：(路径, 版本)
    pub(super) reader: Option<(PathBuf, String)>,
}

/// ICF 文件识别结果：`icf::parse_icf` 解析出 id/version 后，
/// 再按识别库 `lookup_by_game_id`（game_id + xx.xx.xx 版本）匹配的结果
#[derive(Clone)]
pub struct IcfPayload {
    pub(super) path: PathBuf,
    pub(super) result: Result<(icf::IcfInfo, db::DbMatch), String>,
}

/// 未设昵称弹窗暂挂的房间动作（「跳过」后用默认昵称重放；UI 输入仍在组件状态，
/// 但创建对话框参数随弹窗关闭丢失，故在此暂存）。
#[derive(Clone, Debug)]
pub(super) enum PendingRoomAction {
    Create { name: String, game_idx: Option<usize>, password: String },
    Join { room_id: String, password: String },
}

pub(super) enum Dialog {
    None,
    Naming {
        name: String,
    },
    /// 移除游戏二次确认框
    ConfirmRemove {
        index: usize,
        name: String,
    },
    /// 输入设置：按键捕获中（slot 对应 INPUT_BUTTONS，label 为按键名）
    InputCapture {
        slot: usize,
        label: &'static str,
    },
    /// 启动失败（部署 / 拉起加载器任一环节失败），整段错误文案弹窗展示
    LaunchFailed {
        message: String,
        /// 跳转目标：(nav_tag, 可选游戏下标)。Some = 显示「前往设置 / 取消」；
        /// None = 普通错误，仅显示「知道了」。
        goto: Option<(String, Option<usize>)>,
    },
    /// 有组件可更新：醒目提醒（主按钮跳转设置页）
    UpdateAvailable { detail: String },
    /// 创建房间：房间名 + 游戏 + 密码（密码非空 = 私密房）
    RoomCreate {
        name: String,
        /// 选中的游戏（db::game_names() 下标）
        game: Option<usize>,
        password: String,
    },
    /// 加入房间：房间 ID + 密码（私密房必填）
    RoomJoin {
        room_id: String,
        password: String,
    },
    /// 未设昵称提示（创建/加入房间前）：主按钮「前往设置」/
    /// 关闭按钮「跳过（本次用默认昵称 player1234 并重放暂挂动作）」
    RoomNickPrompt {
        pending: PendingRoomAction,
    },
    /// 通用报错弹窗：整段错误信息展示，仅「知道了」按钮关闭
    Error {
        message: String,
    },
}
