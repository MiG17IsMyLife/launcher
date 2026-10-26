//! 游戏页：游戏列表行（含下拉配置卡片）与添加游戏 / 识别入库 / 伴随文件处理。
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use windows_reactor::*;

use crate::db;
use crate::detect::{self, IdentifyResult};
use crate::icf;
use crate::t;
use crate::tf;
use crate::storage::{GameRecord, Loader};

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{CompanionKind, Dialog, IdentifyPayload, IcfPayload, Message, RowAction};
use crate::reactor_app::state::PendingAdd;
use crate::reactor_app::widgets::{make_id, now_secs, record_db_match, row_chip, row_placeholder, same_path, separator, settings_row, settings_toggle};

pub(in crate::reactor_app) const ROW_COVER_W: f64 = 104.0;
pub(in crate::reactor_app) const ROW_COVER_H: f64 = 68.0;

impl Launcher {

    /// 游戏页：Win11 设置风格的长条卡片列表
    pub(in crate::reactor_app) fn games_page(&self, context: &mut ViewContext<Self>) -> View {
        let rows: Vec<KeyedView> = self
            .library
            .games
            .iter()
            .enumerate()
            .map(|(i, g)| self.game_row(context, i, g))
            .collect();

        let subtitle = tf!(
            "{} 款游戏 · 点击行头展开配置：启动 / 卡片选择 / 游戏设置 / ServerBox / 读卡器", "{} games · click a row header to expand: Launch / Card select / Game settings / ServerBox / Card reader",
            self.library.games.len()
        );

        // 与输入设置页完全一致：ScrollViewer 直接作页面根，标题与游戏列表同处一个滚动容器，
        // 滚动条贴窗口最右且贯穿整个内容区高度（不再用「固定标题 + 下方滚动」的两段结构，
        // 那样滚动条只在下半段、比输入页短一截）。悬停高亮全由我们自绘，不用原生 ListView。
        let games_view: View = if rows.is_empty() {
            // 空状态：居中提示，无滚动条
            StackPanel::new()
                .spacing(8.0)
                .margin(Thickness::new(0.0, 48.0, 0.0, 0.0))
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text(t!("还没有添加游戏", "No games added yet"))
                        .font_size(20.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new()
                        .text(t!(
                            "点击右上角「添加游戏」，选择游戏文件（.exe / .elf）\n添加后自动按 CRC 校验码识别",
                            "Click \"Add Game\" at top right and pick a game file (.exe / .elf)\nIt will be identified automatically by CRC checksum after adding",
                        ))
                        .opacity(0.6)
                        .text_wrapping(TextWrapping::Wrap)
                        .horizontal_alignment(HorizontalAlignment::Center),
                ))
                .into()
        } else {
            ScrollViewer::new()
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Visible)
                .content(
                    StackPanel::new()
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
                        .spacing(4.0)
                        .children((
                            // 标题（含副标题），与输入页同款置于滚动容器顶部
                            StackPanel::new()
                                .spacing(4.0)
                                .children((
                                    TextBlock::new()
                                        .text(t!("游戏", "Games"))
                                        .font_size(26.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new().text(subtitle).font_size(12.0).opacity(0.6),
                                )),
                            // 游戏列表行
                            StackPanel::new()
                                .horizontal_alignment(HorizontalAlignment::Stretch)
                                .spacing(6.0)
                                .keyed_children(rows),
                        )),
                )
                .into()
        };

        games_view
    }

    /// Win11 设置风格长条行：缩略图 + 标题/副标题 + 平台徽章 + 行尾「∨」展开指示符。
    /// 点击行头任意位置即可在该行下方就地展开/收起「投币设置」风格配置卡片
    /// （启动 / 卡片选择 / 游戏设置 / ServerBox / 读卡器）；进入卡片选择页请点卡片里的「卡片选择」。
    fn game_row(
        &self,
        context: &mut ViewContext<Self>,
        index: usize,
        game: &GameRecord,
    ) -> KeyedView {
        // 封面缩略图：优先旧档用户自定义封面（data/images/ 路径），
        // 其次识别库内置封面（exe 内嵌字节，纯内嵌方案，不再落盘）
        let cover: View = {
            let mut rendered: Option<View> = None;
            if let Some(path) = &game.image {
                if Path::new(path).is_file() {
                    if let Ok(img) = Image::new().source_file(path) {
                        rendered = Some(
                            img.width(ROW_COVER_W)
                                .height(ROW_COVER_H)
                                .stretch(Stretch::Uniform)
                                .into(),
                        );
                    }
                }
            }
            if rendered.is_none() {
                if let Some(bytes) = game.cover.as_deref().and_then(Self::builtin_cover_bytes) {
                    rendered = Some(
                        Image::new()
                            .source_data(EncodedImage::new(Arc::<[u8]>::from(bytes)))
                            .width(ROW_COVER_W)
                            .height(ROW_COVER_H)
                            .stretch(Stretch::Uniform)
                            .into(),
                    );
                }
            }
            rendered.unwrap_or_else(|| row_placeholder(&game.name).into())
        };

        // 副标题：版本 · game_id（识别库命中时）+ ServerBox 已配置。
        // 不再展示架构 / 游玩次数 / 读卡器已配置。
        let mut parts: Vec<String> = Vec::new();
        parts.push(game.version.as_deref().unwrap_or(t!("未识别版本", "Unidentified")).to_string());
        if let Some(id) = record_db_match(game).and_then(|m| m.game_id) {
            parts.push(id);
        }
        if game.serverbox.is_some() {
            parts.push(t!("ServerBox 已配置", "ServerBox configured").to_string());
        }
        let sub = parts.join(" · ");

        let base_fill = || -> Brush { ThemeBrush::CardBackground.into() };
        // 悬停高亮：与卡片底色形成对比的浅灰（框架 ThemeBrush 无专用 hover 笔刷，
        // 用 SolidBackground 作对比色，足以产生可见的「变色」反馈且不破坏卡片样式）。
        let hover_fill = || -> Brush { ThemeBrush::SolidBackground.into() };

        // 行尾「∨」展开指示符：与「投币设置」头部箭头同款 FontIcon——
        // 无按钮边框、无按钮底色；随展开状态在 ∨（收起）/∧（展开）间翻转
        let dropping = self.drop_index == Some(index);
        let drop_chevron = FontIcon::new().glyph(if dropping {
            "\u{E70D}" // ChevronUp
        } else {
            "\u{E76C}" // ChevronDown
        });

        // 平台徽章：优先识别库命中的 platform（games_db.json 字段，如 "ALLS"），
        // 旧档 / 未识别游戏无此字段时回退 loader 推导（Lindbergh / RingEdge）
        let platform_label = game
            .platform
            .clone()
            .unwrap_or_else(|| game.loader.platform().to_string());
        let right: View = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(2)
            .children((row_chip(&platform_label), drop_chevron));

        // 行头：缩略图 + 标题/副标题 + 平台徽章/∨指示符。行头整体作为展开开关（同「投币设置」）
        let header: View = Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(12.0)
            .children((
                Border::new()
                    .width(ROW_COVER_W)
                    .height(ROW_COVER_H)
                    .corner_radius(4.0)
                    .grid_column(0)
                    .content(cover),
                StackPanel::new()
                    .spacing(3.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .children((
                        TextBlock::new()
                            .text(game.name.clone())
                            .font_size(15.0)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_trimming(TextTrimming::CharacterEllipsis),
                        TextBlock::new().text(sub).font_size(12.0).opacity(0.6),
                    )),
                right,
            ))
            .into();

        // 行头内容（header）本身不再带背景、不再绑定事件——具体由下方两种状态各自
        // 建“行卡片/行头”时决定，避免同一区域被外层卡片与行头两层背景叠加涂灰。
        // 展开内容：仿「投币设置」下拉卡片，逐行展示各操作与配置项
        let row: View = if dropping {
            let eased = {
                let t = self.drop_anim;
                t * (2.0 - t)
            };
            // 已配置路径仅展示文件名
            let file_name = |p: &str| -> String {
                Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string())
            };
            // ServerBox 组仅当命中构建带 sb_crc 时显示；读卡器组对所有游戏显示
            let sb_supported = record_db_match(game).and_then(|m| m.sb_crc).is_some();

            // 运行状态：该游戏的游戏进程 / ServerBox 是否正在运行（按钮切「关闭」）
            let game_running_here = self.game_running
                && self.game_runtime.as_ref().map(|r| r.index) == Some(index);
            let sb_running_here = self
                .serverbox_runtime
                .as_ref()
                .map(|r| r.index)
                == Some(index);

            // 卡片选择行描述：显示当前选用的卡片文件名
            // （IDAS 卡 = cardfile.png；aime 卡 = DEVICE\code.txt），未选卡显示默认提示
            // 优先按本游戏记录（每个游戏独立保存）的 GameRecord.card，
            // 未配置时回退旧全局 config.ini [card] cardfile；避免所有游戏共用同一张卡片。
            // 拼接扩展名前先判重：扫到的 IDAS 卡为无扩展名主干，空白卡为带 .png 的
            // InidCrdXXXXX.png，aime 卡为 20 位数字——已带相应扩展名则不重复追加。
            let cards_desc = match game.card.clone().or_else(|| self.store.load_card()) {
                Some(c) => {
                    let file = if game.loader == Loader::Segatools {
                        if c.ends_with(".txt") {
                            c
                        } else {
                            format!("{c}.txt")
                        }
                    } else if c.ends_with(".png") {
                        c
                    } else {
                        format!("{c}.png")
                    };
                    tf!("当前卡片：{}", "Current card: {}", file)
                }
                None => t!(
                    "进入卡片选择界面（自动扫描游戏目录内的卡片）",
                    "Open the card selection screen (auto-scans cards in the game folder)"
                )
                .to_string(),
            };

            let mut rows: Vec<KeyedView> = vec![
                KeyedView::new(
                    "drop-launch",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Play).into(),
                        t!("启动游戏", "Launch game"),
                        t!("启动这款游戏", "Launch this game"),
                        // 启动按钮左侧的「测试模式」勾选框：kitten / mice 游戏显示
                        // （状态存到记录里，启动时追加 `-t`）；segatools 游戏无该参数，不显示。
                        // 游戏运行中：按钮变「关闭」，点击终止游戏与读卡器 / amdaemon / ServerBox
                        if game_running_here {
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .on_click(context.message(Message::LaunchStop(index)))
                                .content(t!("关闭", "Close"))
                                .into()
                        } else if game.loader == Loader::Segatools {
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::Launch,
                                }))
                                .content(t!("启动", "Launch"))
                                .into()
                        } else {
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(16.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .children((
                                    CheckBox::new()
                                        .is_checked(game.test_mode == Some(true))
                                        .on_is_checked_changed(context.callback(
                                            move |on: bool| Message::RowTestModeToggled(index, on),
                                        ))
                                        .vertical_alignment(VerticalAlignment::Center)
                                        // content() 直接返回 View，不能再接 .into()
                                        .content(t!("测试模式", "Test mode")),
                                    Button::new()
                                        .style(ButtonStyle::Accent)
                                        .on_click(context.message(Message::RowMenu {
                                            index,
                                            action: RowAction::Launch,
                                        }))
                                        .content(t!("启动", "Launch")),
                                ))
                                .into()
                        },
                    ),
                ),

                KeyedView::new("drop-sep-cards", separator()),
                KeyedView::new(
                    "drop-cards",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::List).into(),
                        t!("卡片选择", "Card select"),
                        &cards_desc,
                        Button::new()
                            .on_click(context.message(Message::RowMenu {
                                index,
                                action: RowAction::Cards,
                            }))
                            .content(t!("进入", "Enter")),
                    ),
                ),
            ];
                // IDAS6~8（mice）：图形补丁开关行（启动时部署 winmm.dll + scripts\）
                if game.loader == Loader::Mice {
                    rows.insert(1, KeyedView::new("drop-sep-gfx", separator()));
                    rows.insert(
                        2,
                        KeyedView::new(
                            "drop-gfx",
                            settings_row(
                            SymbolIcon::new().symbol(Symbol::Repair).into(),
                            t!("图形补丁", "Graphics patch"),
                            t!(
                                "启动时部署 ASI Loader 与 shaderfix 插件到游戏目录与 ServerBox 目录",
                                "Deploys the ASI Loader and shaderfix plugin to the game and ServerBox directories"
                            ),
                            settings_toggle(
                                game.gfx_patch.unwrap_or(true),
                                true,
                                context.callback(move |on: bool| Message::GfxPatchToggled(index, on)),
                            ),
                        ),
                    ),
                );
                }

            // ---- ServerBox ----
            if sb_supported {
                let sb_desc = game
                    .serverbox
                    .as_deref()
                    .map(|p| tf!("已配置：{}", "Configured: {}", file_name(p)))
                    .unwrap_or_else(|| {
                        t!("未配置：点击添加（需匹配该构建的 ServerBox CRC）", "Not configured: click Add (must match this build's ServerBox CRC)").to_string()
                    });
                let sb_control: View = if sb_running_here {
                    // ServerBox 运行中：启动按钮变「关闭」
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.message(Message::ServerBoxStop(index)))
                        .content(t!("关闭", "Close"))
                        .into()
                } else if game.serverbox.is_some() {
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::LaunchServerBox,
                                }))
                                .content(t!("启动", "Launch")),
                            Button::new()
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::AddServerBox,
                                }))
                                .content(t!("更换", "Replace")),
                            Button::new()
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::RemoveServerBox,
                                }))
                                .content(t!("移除", "Remove")),
                        ))
                        .into()
                } else {
                    Button::new()
                        .on_click(context.message(Message::RowMenu {
                            index,
                            action: RowAction::AddServerBox,
                        }))
                        .content(t!("添加", "Add"))
                };
                rows.push(KeyedView::new("drop-sep-sb", separator()));
                rows.push(KeyedView::new(
                    "drop-serverbox",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Manage).into(),
                        "ServerBox",
                        &sb_desc,
                        sb_control,
                    ),
                ));
            }

            // ---- 读卡器（picodaemon）：由启动器在启动游戏时自动解析游戏目录，不再手动配置 ----

            // ---- segatools 游戏（IDAC 等）：amdaemon.exe / 游戏主程序设置 ----
            if game.loader == Loader::Segatools {
                let companion_row = |key: &str,
                                     title: String,
                                     configured: Option<&String>,
                                     add: RowAction,
                                     remove: RowAction|
                 -> (KeyedView, KeyedView) {
                    let desc = configured
                        .as_deref()
                        .map(|p| tf!("已记录：{}", "Saved: {}", file_name(p)))
                        .unwrap_or_else(|| {
                            t!("未配置：点击添加并指定文件路径",
                               "Not configured: click Add and pick the file").to_string()
                        });
                    let control: View = if configured.is_some() {
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .on_click(context.message(Message::RowMenu { index, action: add }))
                                    .content(t!("更换", "Replace")),
                                Button::new()
                                    .on_click(context.message(Message::RowMenu { index, action: remove }))
                                    .content(t!("移除", "Remove")),
                            ))
                            .into()
                    } else {
                        Button::new()
                            .on_click(context.message(Message::RowMenu { index, action: add }))
                            .content(t!("添加", "Add"))
                    };
                    (
                        KeyedView::new(format!("drop-sep-{key}"), separator()),
                        KeyedView::new(
                            format!("drop-{key}"),
                            settings_row(
                                SymbolIcon::new().symbol(Symbol::Manage).into(),
                                &title,
                                &desc,
                                control,
                            ),
                        ),
                    )
                };
                let (sep1, row1) = companion_row(
                    "amdaemon",
                    "amdaemon.exe".to_string(),
                    game.amdaemon.as_ref(),
                    RowAction::AddAmdaemon,
                    RowAction::RemoveAmdaemon,
                );
                rows.push(sep1);
                rows.push(row1);
                let (sep2, row2) = companion_row(
                    "gameexe",
                    t!("游戏主程序", "Game executable").to_string(),
                    game.game_exe.as_ref(),
                    RowAction::AddGameExe,
                    RowAction::RemoveGameExe,
                );
                rows.push(sep2);
                rows.push(row2);
            }

            // 移除游戏：垃圾桶图标 + 「移除」按钮，点击弹出二次确认框
            rows.push(KeyedView::new("drop-sep-remove", separator()));
            rows.push(KeyedView::new(
                "drop-remove",
                settings_row(
                    FontIcon::new().glyph("\u{E74D}").into(),
                    t!("移除游戏", "Remove game"),
                    t!("从游戏库移除该游戏", "Remove this game from the library"),
                    Button::new()
                        .on_click(context.message(Message::GameRemoveClicked(index)))
                        .content(t!("移除", "Remove")),
                ),
            ));

            // 分隔行头与下拉内容；整块随动画进度淡入/淡出
            let divider: View = Border::new()
                .height(1.0)
                .background(ThemeBrush::CardStroke)
                .into();
            let rows_panel: View = StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .keyed_children(rows)
                .into();
            let drop_area = StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .opacity(eased)
                .children((divider, rows_panel));

            // 展开状态：行头是独立卡片（圆角 + 描边 + 悬停变色），下拉区直接在行头
            // 下方展开，不再被任何外层边框包裹（下拉框不在 border 里）。
            // 展开态行头：圆角 + 底色 + 悬停变色，但「不描边」——展开整块不含任何
            // 边框线（连行头描边也去掉），下拉区裸放在下方，彻底不在 border 里。
            let row_header: View = Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .corner_radius(6.0)
                .background(if self.hovered == Some(index) { hover_fill() } else { base_fill() })
                .margin(Thickness::new(0.0, 0.0, 0.0, 4.0))
                .padding(Thickness::new(10.0, 10.0, 16.0, 10.0))
                .on_pointer_entered(context.callback(move |_| Message::GameRowHovered(index)))
                .on_pointer_exited(context.callback(move |_| Message::GameRowUnhovered(index)))
                .on_pointer_pressed(context.callback(move |_| Message::GameRowDropToggled(index)))
                .content(header)
                .into();
            StackPanel::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .children((row_header, drop_area))
                .into()
        } else {
            // 收起状态：整张行卡片本身就是行头。底层 Border 提供卡片外观，上层叠一个
            // 透明 Subtle 按钮捕获点击并借其原生 PointerOver 视觉做悬停浅灰高亮——
            // 纯原生、不进 model、滚动时不重建整页（不卡顿），变色反馈保留。
            // 收起状态：整张行卡片本身就是行头。背景按悬停态在底色/浅灰间切换；
            // 点击展开由整块 pointer_pressed 处理，悬停高亮纯视觉、不重建整页。
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .corner_radius(6.0)
                .background(if self.hovered == Some(index) { hover_fill() } else { base_fill() })
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .margin(Thickness::new(0.0, 0.0, 0.0, 4.0))
                .padding(Thickness::new(10.0, 10.0, 16.0, 10.0))
                .on_pointer_entered(context.callback(move |_| Message::GameRowHovered(index)))
                .on_pointer_exited(context.callback(move |_| Message::GameRowUnhovered(index)))
                .on_pointer_pressed(context.callback(move |_| Message::GameRowDropToggled(index)))
                .content(header)
        };

        // 不再用 ListViewItem：原生 ListViewItem 自带的灰条会越过卡片边缘，且把卡片盖住
        KeyedView::new(format!("game-{}", game.id), row)
    }

    /// 「添加游戏」选中的文件：查重后按 ICF1 / EXE 分路在后台识别
    pub(in crate::reactor_app) fn handle_file_picked(&mut self, path: PathBuf, context: &ComponentContext<Self>) {
        if self.library.games.iter().any(|g| same_path(&g.path, &path)) {
            self.dialog = Dialog::Error {
                message: t!("该游戏已在库中", "This game is already in the library").to_string(),
            };
            return;
        }
        // ICF1 文件（IDZ / IDAC 等 ALLS 游戏的无后缀元数据文件）：走 ICF 解析 +
        // 识别库 game_id+版本匹配；其余（.exe / .elf）走原有 CRC 识别
        let is_icf = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("ICF1"));
        context.spawn_background(move |_| {
            if is_icf {
                let result = icf::parse_icf(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|info| {
                        match db::lookup_by_game_id(&info.id, &info.version) {
                            Some(m) => Ok((info, m)),
                            None => Err(tf!(
                                "识别库中未找到 id={}、version={} 的条目",
                                "No library entry found for ICF id={}, version={}",
                                info.id, info.version
                            )),
                        }
                    });
                Message::IcfIdentified(Box::new(IcfPayload { path, result }))
            } else {
                let result = detect::identify(&path).map_err(|e| e.to_string());
                // 识别成功后自动探测同目录 picodaemon.exe（命中 card_reader 库才关联）
                let reader = if result.is_ok() {
                    path.parent().and_then(detect::probe_card_reader)
                } else {
                    None
                };
                Message::Identified(Box::new(IdentifyPayload {
                    path,
                    result,
                    reader,
                }))
            }
        });
    }

    /// 识别结果回灌：命中识别库直接入库，未命中弹窗命名
    pub(in crate::reactor_app) fn handle_identified(&mut self, payload: IdentifyPayload) {
        let IdentifyPayload {
            path,
            result,
            reader,
        } = payload;
        match result {
            Err(e) => self.dialog = Dialog::Error { message: tf!("识别失败：{}", "Identification failed: {}", e) },
            Ok(ir) => {
                if let Some(d) = &ir.db {
                    // 命中识别库：直接入库
                    let reader_path = reader.as_ref().map(|(p, _)| p.display().to_string());
                    // 识别库配置了 image 就用它当默认封面：只记文件名，
                    // 运行时从 exe 内嵌字节渲染（纯内嵌方案，不再落盘 data/assets/）
                    self.library.games.push(GameRecord {
                        id: make_id(),
                        path: path.display().to_string(),
                        name: d.name.clone(),
                        crc: Some(format!("0x{:08x}", ir.crc)),
                        version: Some(d.version.clone()),
                        game_id: d.game_id.clone(),
                        arch: Some(ir.kind.arch().to_string()),
                        loader: d.loader,
                        platform: d.platform.clone(),
                        image: None,
                        cover: d.image.clone(),
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
                } else {
                    // 未命中：弹窗命名
                    let default = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.pending = Some(PendingAdd { path, ir, reader });
                    self.dialog = Dialog::Naming { name: default };
                }
            }
        }
    }

    /// ICF1 识别结果回灌：命中识别库以 ICF 文件为路径入库
    pub(in crate::reactor_app) fn handle_icf_identified(&mut self, payload: IcfPayload) {
        let IcfPayload { path, result } = payload;
        match result {
            Err(e) => self.dialog = Dialog::Error { message: tf!("识别失败：{}", "Identification failed: {}", e) },
            Ok((_info, m)) => {
                // 命中识别库：以 ICF 文件为入库路径，loader / 封面 / 版本取自识别库
                self.library.games.push(GameRecord {
                    id: make_id(),
                    path: path.display().to_string(),
                    name: m.name.clone(),
                    crc: None,
                    version: Some(m.version.clone()),
                    game_id: Some(_info.id.clone()),
                    arch: None,
                    loader: m.loader,
                    platform: m.platform.clone(),
                    image: None,
                    cover: m.image.clone(),
                    serverbox: None,
                    reader: None,
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
    }

    /// 展开/收起指定游戏行的下拉配置卡片，并启动过渡动画
    pub(in crate::reactor_app) fn toggle_row_drop(&mut self, index: usize, context: &ComponentContext<Self>) {
        // 同一行再次点击则收起，否则（含从其它行切换）展开该行
        if self.drop_index == Some(index) && self.drop_open {
            self.drop_open = false;
        } else {
            self.drop_index = Some(index);
            self.drop_open = true;
            self.drop_anim = 0.0;
        }
        // 启动展开/收起过渡动画：每 16ms 推进一步，约 200ms 完成
        self.drop_anim_gen += 1;
        let (epoch, step) = (self.drop_anim_gen, 0.016 / 0.2);
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(16));
            Message::GameRowDropTick(epoch, step)
        });
    }

    /// 行下拉卡片展开/收起动画推进（过期代数作废）
    pub(in crate::reactor_app) fn tick_row_drop(&mut self, epoch: u32, step: f64, context: &ComponentContext<Self>) {
        // 过期的动画循环（期间又点击了切换）直接作废
        if epoch != self.drop_anim_gen {
            return;
        }
        if self.drop_open {
            self.drop_anim = (self.drop_anim + step).min(1.0);
        } else {
            self.drop_anim = (self.drop_anim - step).max(0.0);
        }
        let done = if self.drop_open {
            self.drop_anim >= 1.0
        } else {
            if self.drop_anim <= 0.0 {
                // 收起动画结束：卸载面板，行恢复紧凑高度
                self.drop_index = None;
            }
            self.drop_anim <= 0.0
        };
        if !done {
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(16));
                Message::GameRowDropTick(epoch, step)
            });
        }
    }

    /// 游戏行下拉卡片的动作分发
    pub(in crate::reactor_app) fn handle_row_menu(&mut self, index: usize, action: RowAction, context: &ComponentContext<Self>) {
        if self.library.games.get(index).is_none() {
            return;
        }
        match action {
            RowAction::Launch => self.do_launch(context, index),
            RowAction::LaunchServerBox => self.do_launch_serverbox(context, index),
            RowAction::Cards => {
                self.dialog = Dialog::None;
                self.enter_cards(index, context);
            }
            RowAction::AddServerBox => {
                self.pick_companion(context, CompanionKind::ServerBox, index);
            }
            RowAction::RemoveServerBox => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.serverbox = None;
                }
                self.store.save_library(&self.library);
            }
            RowAction::AddAmdaemon => {
                self.pick_companion(context, CompanionKind::Amdaemon, index);
            }
            RowAction::RemoveAmdaemon => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.amdaemon = None;
                }
                self.store.save_library(&self.library);
            }
            RowAction::AddGameExe => {
                self.pick_companion(context, CompanionKind::GameExe, index);
            }
            RowAction::RemoveGameExe => {
                if let Some(g) = self.library.games.get_mut(index) {
                    g.game_exe = None;
                }
                self.store.save_library(&self.library);
            }
        }
    }

    /// 伴随文件选择结果：amdaemon / 游戏主程序直接记录路径，其余后台识别 CRC
    pub(in crate::reactor_app) fn handle_companion_picked(
        &mut self,
        kind: CompanionKind,
        index: usize,
        path: PathBuf,
        context: &ComponentContext<Self>,
    ) {
        // segatools 的 amdaemon / 游戏主程序：仅记录路径，不做 CRC 校验
        if matches!(kind, CompanionKind::Amdaemon | CompanionKind::GameExe) {
            if let Some(g) = self.library.games.get_mut(index) {
                let slot = match kind {
                    CompanionKind::Amdaemon => &mut g.amdaemon,
                    CompanionKind::GameExe => &mut g.game_exe,
                    _ => return,
                };
                *slot = Some(path.display().to_string());
                self.store.save_library(&self.library);
            }
            return;
        }
        // 文件对话框选中的伴随文件：后台识别其 CRC
        context.spawn_background(move |_| {
            let result = detect::identify(&path).map_err(|e| e.to_string());
            Message::CompanionIdentified {
                kind,
                index,
                path,
                result,
            }
        });
    }

    /// 伴随文件识别结果：ServerBox 校验期望 CRC，读卡器匹配已知 picodaemon
    pub(in crate::reactor_app) fn handle_companion_identified(
        &mut self,
        kind: CompanionKind,
        index: usize,
        path: PathBuf,
        result: Result<IdentifyResult, String>,
    ) {
        let Some(game) = self.library.games.get(index) else {
            return;
        };
        match kind {
            CompanionKind::ServerBox => {
                // IDAS4~8：ServerBox 必须是独立于游戏目录的程序；
                // 若选中的 ServerBox 与游戏处于同一目录，直接报错（与游戏混在一起会互相干扰）。
                if let (Some(gd), Some(sd)) = (Path::new(&game.path).parent(), path.parent()) {
                    if same_path(&gd.to_string_lossy(), sd) {
                        self.dialog = Dialog::Error {
                            message: t!("ServerBox 不能与游戏位于同一目录",
                               "ServerBox cannot be in the same directory as the game")
                                .to_string(),
                        };
                        return;
                    }
                }
                // 期望 CRC = 该记录命中构建的 sb_crc
                match record_db_match(game).and_then(|m| m.sb_crc) {
                    None => {
                        self.dialog = Dialog::Error {
                            message: t!("该游戏当前命中版本未提供 ServerBox 信息", "The currently matched version of this game provides no ServerBox info").to_string()
                        }
                    }
                    Some(expected) => match result {
                        Err(e) => self.dialog = Dialog::Error {
                            message: tf!("ServerBox 识别失败：{}", "ServerBox identification failed: {}", e),
                        },
                        Ok(ir) if ir.crc == expected => {
                            self.library.games[index].serverbox =
                                Some(path.display().to_string());
                            self.store.save_library(&self.library);
                        }
                        Ok(ir) => {
                            self.dialog = Dialog::Error {
                                message: tf!("所选文件不是该游戏的 ServerBox（CRC 0x{:08x}，期望 0x{expected:08x}）",
                                   "The selected file is not this game's ServerBox (CRC 0x{:08x}, expected 0x{expected:08x})",
                                ir.crc
                            )
                            }
                        }
                    },
                }
            }
            // amdaemon / 游戏主程序在 CompanionPicked 已直接记录，不经过 CRC 识别
            CompanionKind::Amdaemon | CompanionKind::GameExe => {}
        }
    }

}
