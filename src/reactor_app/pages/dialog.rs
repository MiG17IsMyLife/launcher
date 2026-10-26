//! 弹窗渲染与弹窗确认后的入库 / 移除等应用。
use windows_reactor::*;

use crate::detect::FileKind;
use crate::filedialog;
use crate::t;
use crate::tf;
use crate::storage::{GameRecord, Loader};

use crate::reactor_app::Launcher;
use super::input::INPUT_CAPTURE_SECS;
use crate::reactor_app::message::{CompanionKind, Dialog, Message};
use crate::reactor_app::state::PendingAdd;
use crate::reactor_app::widgets::{make_id, now_secs};

impl Launcher {

    pub(in crate::reactor_app) fn dialog_view(&self, context: &mut ViewContext<Self>) -> View {
        match &self.dialog {
            Dialog::None => StackPanel::new().into(),
            Dialog::Naming { name } => ContentDialog::new()
                .title(t!("添加游戏", "Add Game"))
                .primary_button_text(t!("添加", "Add"))
                .close_button_text(t!("取消", "Cancel"))
                .is_open(true)
                .on_closed(context.callback(Message::DialogClosed))
                .content(
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text(t!("未能自动识别该游戏（CRC 未命中），请输入名称：", "Could not auto-identify this game (CRC not matched). Enter a name:"))
                            .opacity(0.7)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBox::new()
                            .text(name.clone())
                            .on_text_changed(context.callback(Message::NamingTextChanged)),
                    )),
                )
                .into(),
            Dialog::ConfirmRemove { name, .. } => ContentDialog::new()
                .title(t!("移除游戏", "Remove Game"))
                .primary_button_text(t!("移除", "Remove"))
                .close_button_text(t!("取消", "Cancel"))
                .is_open(true)
                .on_closed(context.callback(Message::DialogClosed))
                .content(
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text(tf!("确认从游戏库移除「{}」？", "Remove \"{}\" from the game library?", name))
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(t!("该游戏的封面与配置将一并清除，此操作不可撤销。", "The game's cover and settings will be removed too. This cannot be undone."))
                            .opacity(0.6)
                            .font_size(12.0)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )
                .into(),
            Dialog::InputCapture { label, .. } => {
                let remain = self.input_capture_remain.clamp(0.0, 1.0);
                let secs = (remain * INPUT_CAPTURE_SECS).ceil() as u32;
                ContentDialog::new()
                    .title(t!("请按下按键", "Press the button"))
                    .close_button_text(t!("取消", "Cancel"))
                    .is_open(true)
                    .on_closed(context.callback(Message::DialogClosed))
                    .content(
                        StackPanel::new().spacing(10.0).children((
                            TextBlock::new()
                                .text(tf!(
                                    "请在设备上按下「{}」对应的按键，或推动十字键（方向键）", "Press the button for \"{}\" on the device, or move the D-pad (arrow keys)",
                                    label
                                ))
                                .text_wrapping(TextWrapping::Wrap),
                            // 倒计时进度条：起始满格，10 秒后归零
                            ProgressBar::new()
                                .width(320.0)
                                .minimum(0.0)
                                .maximum(100.0)
                                .value(remain * 100.0),
                            TextBlock::new()
                                .text(tf!("剩余 {} 秒，未按下将自动关闭", "{}s remaining; auto-closes if no key is pressed", secs))
                                .font_size(12.0)
                                .opacity(0.6),
                        )),
                    )
                    .into()
            }
            Dialog::UpdateAvailable { detail } => ContentDialog::new()
                .title(t!("组件有可用更新", "Component updates available"))
                .primary_button_text(t!("前往设置", "Go to settings"))
                .close_button_text(t!("稍后", "Later"))
                .is_open(true)
                .on_closed(context.callback(Message::DialogClosed))
                .content(
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text(t!(
                                "以下组件有新版本，可前往「设置」页更新：",
                                "The following components have new versions; go to Settings to update:"
                            ))
                            .opacity(0.8),
                        TextBlock::new()
                            .text(detail.clone())
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )
                .into(),
            Dialog::LaunchFailed { message, goto } => {
                // 带跳转目标：主按钮「前往设置」、关闭按钮「取消」；
                // 普通错误：仅一个主按钮「知道了」
                let (primary, close) = match goto {
                    Some(_) => (t!("前往设置", "Go to settings"), Some(t!("取消", "Cancel"))),
                    None => (t!("知道了", "OK"), None),
                };
                let dlg = ContentDialog::new()
                    .title(t!("启动失败", "Launch failed"))
                    .primary_button_text(primary)
                    .is_open(true)
                    .on_closed(context.callback(Message::DialogClosed));
                let dlg = match close {
                    Some(c) => dlg.close_button_text(c),
                    None => dlg,
                };
                dlg
                    .content(
                        StackPanel::new().spacing(8.0).children((
                            TextBlock::new()
                                .text(message.clone())
                                .opacity(0.8)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    )
                    .into()
            }
            Dialog::RoomNickPrompt { pending } => {
                let (action_hint, default_note) = match pending {
                    crate::reactor_app::message::PendingRoomAction::Create { .. } => (
                        t!("创建房间", "create a room"),
                        t!("跳过后本次将使用默认昵称 player1234", "Skip to use the default nickname player1234 this time"),
                    ),
                    crate::reactor_app::message::PendingRoomAction::Join { .. } => (
                        t!("加入房间", "join a room"),
                        t!("跳过后本次将使用默认昵称 player1234", "Skip to use the default nickname player1234 this time"),
                    ),
                };
                ContentDialog::new()
                    .title(t!("未设置昵称", "Nickname not set"))
                    .primary_button_text(t!("前往设置", "Go to settings"))
                    .close_button_text(t!("跳过", "Skip"))
                    .is_open(true)
                    .on_closed(context.callback(Message::DialogClosed))
                    .content(
                        StackPanel::new().spacing(8.0).children((
                            TextBlock::new()
                                .text(tf!(
                                    "尚未设置房间昵称，房间内成员将无法辨认你。要现在前往「设置」页配置后再{0}吗？",
                                    "No room nickname is set; other players cannot tell who you are. Go to Settings and set one now before you {0}?",
                                    action_hint
                                ))
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(default_note)
                                .opacity(0.6)
                                .font_size(12.0)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    )
                    .into()
            }
            Dialog::Error { message } => ContentDialog::new()
                .title(t!("错误", "Error"))
                .primary_button_text(t!("知道了", "OK"))
                .is_open(true)
                .on_closed(context.callback(Message::DialogClosed))
                .content(
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text(message.clone())
                            .opacity(0.8)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )
                .into(),
            Dialog::RoomCreate { name, game, password } => {
                let games = crate::db::game_names();
                ContentDialog::new()
                    .title(t!("创建房间", "Create Room"))
                    .primary_button_text(t!("创建", "Create"))
                    .close_button_text(t!("取消", "Cancel"))
                    .is_open(true)
                    .on_closed(context.callback(Message::DialogClosed))
                    .content(
                        StackPanel::new().spacing(10.0).children((
                            TextBlock::new()
                                .text(t!("玩的什么游戏", "Game"))
                                .opacity(0.7),
                            ComboBox::new()
                                .width(320.0)
                                .items_source(games)
                                .selected_index(*game)
                                .placeholder_text(t!("选择游戏", "Select a game"))
                                .on_selection_changed(context.callback(Message::RoomCreateGameChanged))
                                .vertical_alignment(VerticalAlignment::Center),
                            TextBlock::new()
                                .text(t!("房间名", "Room name (shown in the public room list)"))
                                .opacity(0.7),
                            TextBox::new()
                                .text(name.clone())
                                .on_text_changed(context.callback(Message::RoomCreateNameChanged)),
                            TextBlock::new()
                                .text(t!("密码（留空 = 公开房）", "Password (blank = public room)"))
                                .opacity(0.7),
                            PasswordBox::new()
                                .password(password.clone())
                                .on_password_changed(context.callback(Message::RoomCreatePasswordChanged)),
                        )),
                    )
                    .into()
            }
            Dialog::RoomJoin { room_id, password } => ContentDialog::new()
                .title(t!("加入房间", "Join Room"))
                .primary_button_text(t!("加入", "Join"))
                .close_button_text(t!("取消", "Cancel"))
                .is_open(true)
                .on_closed(context.callback(Message::DialogClosed))
                .content(
                    StackPanel::new().spacing(10.0).children((
                        TextBlock::new()
                            .text(t!("房间 ID", "Room ID"))
                            .opacity(0.7),
                        TextBox::new()
                            .text(room_id.clone())
                            .on_text_changed(context.callback(Message::RoomJoinIdChanged)),
                        TextBlock::new()
                            .text(t!("密码", "Password"))
                            .opacity(0.7),
                        PasswordBox::new()
                            .password(password.clone())
                            .on_password_changed(context.callback(Message::RoomJoinPasswordChanged)),
                    )),
                )
                .into(),
        }
    }

    pub(in crate::reactor_app) fn apply_dialog(&mut self, context: &ComponentContext<Self>) {
        match std::mem::replace(&mut self.dialog, Dialog::None) {
            Dialog::RoomCreate { name, game, password } => {
                self.room_start_host(name, game, password, context);
            }
            Dialog::RoomJoin { room_id, password } => {
                self.room_start_join(room_id, password, context);
            }
            Dialog::RoomNickPrompt { .. } => {
                // 主按钮「前往设置」：转设置页（跳过走 DialogClosed 非主键分支）
                self.dialog = Dialog::None;
                self.nav_tag = "settings".to_string();
            }
            Dialog::Naming { name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.pending = None;
                    return;
                }
                if let Some(add) = self.pending.take() {
                    let PendingAdd { path, ir, reader } = add;
                    let loader = match ir.kind {
                        FileKind::Elf32 => Loader::Kitten,
                        _ => Loader::Mice,
                    };
                    let reader_path = reader.as_ref().map(|(p, _)| p.display().to_string());
                    self.library.games.push(GameRecord {
                        id: make_id(),
                        path: path.display().to_string(),
                        name,
                        crc: Some(format!("0x{:08x}", ir.crc)),
                        version: None,
                        game_id: None,
                        arch: Some(ir.kind.arch().to_string()),
                        loader,
                        platform: None,
                        image: None,
                        cover: None,
                        serverbox: None,
                        reader: reader_path,
                        amdaemon: None,
                        game_exe: None,
            gfx_patch: None,
                        test_mode: None,
                        card: None,
                        play_count: 0,
                        last_played: None,
                        added_at: now_secs(),
                    });
                    self.store.save_library(&self.library);
                }
            }
            Dialog::ConfirmRemove { index, .. } => {
                if self.library.games.get(index).is_some() {
                    let g = self.library.games.remove(index);
                    self.store.remove_image(&g.id);
                    self.store.save_library(&self.library);
                    // 列表下标已变动：收起行下拉卡片，避免指向另一款游戏
                    self.drop_index = None;
                    self.drop_open = false;
                }
            }
            Dialog::UpdateAvailable { .. } => {
                // 「前往设置」：跳转设置页并重置提醒标记（之后有新版本会再次提醒）
                self.nav_tag = "settings".to_string();
                self.update_prompt_key = None;
            }
            Dialog::LaunchFailed { goto, .. } => {
                // 主按钮「前往设置」：根据启动前检查失败项跳转到对应配置页
                if let Some((tag, idx)) = goto {
                    self.nav_tag = tag.clone();
                    if let Some(i) = idx {
                        match tag.as_str() {
                            // 车卡：复用 enter_cards 进入卡片选择页（含扫描 / 缓存逻辑）
                            "cards" => self.enter_cards(i, context),
                            // ServerBox / amdaemon / 游戏主程序：展开该游戏行下拉卡片
                            "games" => {
                                self.drop_index = Some(i);
                                self.drop_open = true;
                                self.drop_anim = 1.0;
                            }
                            // 其余（system / network / input 等）：仅切换导航页
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 从行菜单「添加/更换 ServerBox / 读卡器」打开文件对话框（后台线程，避免阻塞 UI）。
    pub(in crate::reactor_app) fn pick_companion(&self, context: &ComponentContext<Self>, kind: CompanionKind, index: usize) {
        let exe_pairs = || {
            vec![
                (t!("可执行文件", "Executable"), "*.exe;*.elf"),
                (t!("所有文件", "All files"), "*.*"),
            ]
        };
        let (pairs, title) = match kind {
            CompanionKind::ServerBox => (exe_pairs(), t!("选择 ServerBox 文件", "Select ServerBox file")),
            CompanionKind::Amdaemon => (
                exe_pairs(),
                t!("选择 amdaemon.exe", "Select amdaemon.exe"),
            ),
            CompanionKind::GameExe => (
                exe_pairs(),
                t!("选择游戏主程序（GameProject-Win64-Shipping.exe）", "Select game executable (GameProject-Win64-Shipping.exe)"),
            ),
        };
        context.spawn_background(move |_| {
            let picked = filedialog::pick_open_file(&pairs, title);
            Message::CompanionPicked {
                kind,
                index,
                path: picked,
            }
        });
    }

}
