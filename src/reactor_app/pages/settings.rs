//! 「设置」页：语言 + 自动更新（检查 / 下载 / 安装进度与各加载器折叠配置行）。
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use windows_reactor::*;

use crate::i18n::Lang;
use crate::t;
use crate::tf;
use crate::update::{self, TargetStatus, UpdateIndex, UpdatePhase, UpdateState, UpdateTarget};

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{Dialog, Message};
use crate::reactor_app::widgets::{separator, settings_row, settings_toggle};

// 更新状态指示灯 / 进度条配色
const COLOR_GREY: Color = Color::argb(255, 0x9E, 0x9E, 0x9E);
const COLOR_OK: Color = Color::argb(255, 0x6C, 0xCB, 0x5F);
const COLOR_WARN: Color = Color::argb(255, 0xFF, 0xB0, 0x4D);
const COLOR_ERR: Color = Color::argb(255, 0xFF, 0x99, 0xA4);
const COLOR_ACCENT: Color = Color::argb(255, 0x4C, 0x8B, 0xF5);

impl Launcher {

    /// 触发一次更新检查：
    /// - `force`：并行拉取全部组件最新 Release（各组件走各自默认仓库，语言决定优先级）；
    /// - 否则缓存有效时先按缓存秒显，但凡有「缓存里查不到」的组件（上次拉取失败/从未成功），
    ///   只对这些缺失组件后台补拉一次（其余吃缓存，不每轮全量打网络），结果就绪即刷新。
    pub(in crate::reactor_app) fn run_check(&mut self, force: bool, context: &ComponentContext<Self>) {
        // 需要联网查询的组件：force → 全量；缓存命中但有缺失 → 仅缺失子集；
        // 缓存未命中/过期 → 全量。
        let to_query: Vec<UpdateTarget> = if force {
            update::ALL_TARGETS.to_vec()
        } else if let Some(idx) = update::load_cache() {
            let results = update::check_updates(&idx, &self.settings);
            update::apply_check(&self.update_shared, results);
            let missing: Vec<UpdateTarget> = update::ALL_TARGETS
                .iter()
                .copied()
                .filter(|t| !idx.loaders.iter().any(|l| l.name == t.dir_name()))
                .collect();
            if missing.is_empty() {
                return;
            }
            // 缺失组件先标「检查中」，避免被误显为绿色「已是最新」
            {
                let mut s = self.update_shared.lock().unwrap();
                for t in &missing {
                    if let Some(ts) = s.per_target.get_mut(t) {
                        ts.state = UpdateState::Checking;
                        ts.phase = UpdatePhase::Checking;
                    }
                }
            }
            missing
        } else {
            update::ALL_TARGETS.to_vec()
        };

        // 进入联网查询路径：先把目标组件标为「检查中」
        {
            let mut s = self.update_shared.lock().unwrap();
            for t in &to_query {
                if let Some(ts) = s.per_target.get_mut(t) {
                    ts.state = UpdateState::Checking;
                    ts.phase = UpdatePhase::Checking;
                }
            }
        }
        let settings = self.settings.clone();
        let shared = self.update_shared.clone();
        self.ensure_poll(context);
        context.spawn_background(move |_| {
            // 每个组件结果就绪即写入共享状态（心跳轮询实时上屏），不等其他组件
            let mut index = UpdateIndex::default();
            update::fetch_updates_for(&to_query, &mut |target, rel| {
                let ts = update::check_one(target, rel.as_ref(), &settings);
                update::apply_check(&shared, HashMap::from([(target, ts)]));
                if let Some(rel) = rel {
                    index.loaders.push(rel);
                }
            });
            update::save_cache(&index);
            // 全部就绪（或超期）：收尾聚合（弹「有更新」提醒等）
            let results = {
                let s = shared.lock().unwrap();
                s.per_target.clone()
            };
            Message::UpdatesChecked(Ok(results))
        });
    }

    /// 启动一个进度轮询心跳（仅在仍有活动阶段时持续，空闲时自动停止）。
    fn ensure_poll(&mut self, context: &ComponentContext<Self>) {
        if self.poll_running {
            return;
        }
        self.poll_running = true;
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(200));
            Message::UpdatePoll
        });
    }

    /// 「全部更新」：批量更新所有处于「有更新」状态的组件。
    pub(in crate::reactor_app) fn update_all_loaders(&mut self, context: &ComponentContext<Self>) {
        if self.game_running {
            self.dialog = Dialog::Error {
                message: t!(
                    "游戏运行中，无法更新；请先退出游戏",
                    "Cannot update while a game is running"
                )
                .to_string(),
            };
            return;
        }
        let targets: Vec<UpdateTarget> = {
            let s = self.update_shared.lock().unwrap();
            update::ALL_TARGETS
                .iter()
                .filter(|t| {
                    s.per_target
                        .get(t)
                        .map(|ts| ts.state == UpdateState::Outdated)
                        .unwrap_or(false)
                })
                .copied()
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        for target in targets {
            self.start_loader_update(target, context);
        }
    }

    /// 单个加载器的配置行：齿轮图标 + 名称/详情 + 状态 + 进度条（名称右侧）+ 按钮。


    /// 「组件更新」卡片：表格常驻列出全部加载器（组件 / 版本 / 状态 / 操作），
    /// 列宽对齐风格参考「店内对战」公开房间列表。
    fn loaders_card(&self, context: &mut ViewContext<Self>) -> View {
        let n = update::ALL_TARGETS.len();
        // 没有组件时，只显示「无」
        if n == 0 {
            return Border::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .corner_radius(4.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .padding(Thickness::new(20.0, 14.0, 20.0, 18.0))
                .content(
                    TextBlock::new()
                        .text(t!("无", "None"))
                        .font_size(13.0)
                        .opacity(0.5)
                        .horizontal_alignment(HorizontalAlignment::Center),
                )
                .into();
        }
        // 行定义：表头 / 表头分隔线 / (数据行 / 行分隔线)*
        let mut row_defs = vec![GridLength::Auto]; // 0: 表头
        row_defs.push(GridLength::Auto); // 1: 表头分隔线（高度由分隔线自身决定）
        for k in 0..n {
            row_defs.push(GridLength::Auto); // 数据行
            if k < n - 1 {
                row_defs.push(GridLength::Auto); // 行分隔线（高度由分隔线自身决定）
            }
        }

        let mut cells: Vec<KeyedView> = Vec::new();
        let headers = [
            t!("组件", "Component").to_string(),
            t!("版本", "Version").to_string(),
            t!("状态", "Status").to_string(),
            String::new(), // 操作列不显示表头
        ];
        for (c, txt) in headers.iter().enumerate() {
            cells.push(KeyedView::new(format!("h{c}"), View::from(
                TextBlock::new()
                    .text(txt.clone())
                    .font_size(13.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .opacity(0.5)
                    .grid_row(0)
                    .grid_column(c as i32)
                    .vertical_alignment(VerticalAlignment::Center),
            )));
        }
        // 表头与表格之间的全宽分隔线（加粗、上下留白）
        cells.push(KeyedView::new("sep-h", View::from(
            Border::new()
                .height(1.5)
                .margin(Thickness::new(0.0, 10.0, 0.0, 10.0))
                .background(ThemeBrush::CardStroke)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_row(1)
                .grid_column(0)
                .grid_column_span(4),
        )));

        for (i, target) in update::ALL_TARGETS.iter().enumerate() {
            let row = (2 + 2 * i) as i32;
            let ts = {
                let s = self.update_shared.lock().unwrap();
                s.per_target.get(target).cloned().unwrap_or_default()
            };
            let active = matches!(
                ts.phase,
                UpdatePhase::Downloading | UpdatePhase::Extracting | UpdatePhase::Installing
            );
            let pct = if ts.download_total > 0 {
                ts.download_done as f64 / ts.download_total as f64
            } else {
                0.0
            };
            let (label, color) = match ts.state {
                UpdateState::NotConfigured => (t!("未配置更新源", "Update source not configured"), COLOR_GREY),
                UpdateState::Checking => (t!("检查中…", "Checking…"), COLOR_WARN),
                UpdateState::UpToDate => (t!("已是最新", "Up to date"), COLOR_OK),
                UpdateState::Outdated => (t!("有更新", "Update available"), COLOR_WARN),
                UpdateState::Error => (t!("错误", "Error"), COLOR_ERR),
            };
            let phase_text = match ts.phase {
                UpdatePhase::Downloading => t!("下载中", "Downloading"),
                UpdatePhase::Extracting => t!("解压中", "Extracting"),
                UpdatePhase::Installing => t!("安装中", "Installing"),
                _ => "",
            };

            // 版本列：进行中显示进度条，否则显示「本地基线 → 远端最新」
            let version_cell: View = if active {
                View::from(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .grid_row(row)
                        .grid_column(1)
                        .children((
                            View::from(
                                Grid::new()
                                    .width(200.0)
                                    .height(6.0)
                                    .children((
                                        View::from(
                                            Border::new()
                                                .corner_radius(3.0)
                                                .background(Brush::Solid(COLOR_GREY)),
                                        ),
                                        View::from(
                                            Border::new()
                                                .corner_radius(3.0)
                                                .background(Brush::Solid(COLOR_ACCENT))
                                                .width(pct * 200.0)
                                                .horizontal_alignment(HorizontalAlignment::Left),
                                        ),
                                    )),
                            ),
                            View::from(
                                TextBlock::new()
                                    .text(format!("{} {:.0}%", phase_text, pct * 100.0))
                                    .font_size(12.0),
                            ),
                        )),
                )
            } else {
                let local = update::last_version_of(&self.settings, *target);
                let remote = ts.remote_version.clone();
                // 有更新时统一以「老版本 → 新版本」呈现；本机未记录版本时老版本记为 —
                let ver = if ts.state == UpdateState::Outdated {
                    match (&local, &remote) {
                        (Some(l), Some(r)) => format!("{} → {}", l, r),
                        (None, Some(r)) => format!("— → {}", r),
                        (Some(l), None) => l.clone(),
                        _ => "—".to_string(),
                    }
                } else {
                    match (&local, &remote) {
                        (Some(l), Some(r)) if l != r => format!("{} → {}", l, r),
                        (_, Some(r)) => r.clone(),
                        (Some(l), _) => l.clone(),
                        _ => "—".to_string(),
                    }
                };
                View::from(
                    TextBlock::new()
                        .text(ver)
                        .font_size(12.0)
                        .opacity(0.85)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .grid_row(row)
                        .grid_column(1)
                        .vertical_alignment(VerticalAlignment::Center),
                )
            };

            // 状态列：错误时附原因
            let status_text = if ts.state == UpdateState::Error {
                ts.detail.clone().unwrap_or_else(|| label.to_string())
            } else {
                label.to_string()
            };
            let status_cell: View = View::from(
                TextBlock::new()
                    .text(status_text)
                    .font_size(12.0)
                    .foreground(Brush::Solid(color))
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .grid_row(row)
                    .grid_column(2)
                    .vertical_alignment(VerticalAlignment::Center),
            );

            // 操作列：进行中 → 取消；有更新 → 更新；其余留空
            let action_cell: View = if active {
                View::from(
                    Button::new()
                        .on_click(context.callback(move |_| Message::CancelUpdate(*target)))
                        .grid_row(row)
                        .grid_column(3)
                        .vertical_alignment(VerticalAlignment::Center)
                        .content(t!("取消", "Cancel")),
                )
            } else if ts.state == UpdateState::Outdated {
                View::from(
                    Button::new()
                        .on_click(context.callback(move |_| Message::UpdateLoader(*target)))
                        .grid_row(row)
                        .grid_column(3)
                        .vertical_alignment(VerticalAlignment::Center)
                        .content(t!("更新", "Update")),
                )
            } else {
                View::from(
                    StackPanel::new()
                        .grid_row(row)
                        .grid_column(3)
                        .vertical_alignment(VerticalAlignment::Center),
                )
            };

            cells.push(KeyedView::new(format!("r{row}c0"), View::from(
                TextBlock::new()
                    .text(target.display_name())
                    .font_size(14.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .grid_row(row)
                    .grid_column(0)
                    .vertical_alignment(VerticalAlignment::Center),
            )));
            cells.push(KeyedView::new(format!("r{row}c1"), version_cell));
            cells.push(KeyedView::new(format!("r{row}c2"), status_cell));
            cells.push(KeyedView::new(format!("r{row}c3"), action_cell));
            if i + 1 < n {
                cells.push(KeyedView::new(format!("sep{i}"), View::from(
                    Border::new()
                        .height(1.0)
                        .margin(Thickness::new(0.0, 7.0, 0.0, 7.0))
                        .background(ThemeBrush::CardStroke)
                        .vertical_alignment(VerticalAlignment::Center)
                        .grid_row(row + 1)
                        .grid_column(0)
                        .grid_column_span(4),
                )));
            }
        }

        Border::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(Thickness::new(20.0, 14.0, 20.0, 18.0))
            .content(
                Grid::new()
                    .columns([
                        GridLength::Star(1.6),
                        GridLength::Star(1.2),
                        GridLength::Star(1.0),
                        GridLength::Pixel(96.0),
                    ])
                    .column_spacing(28.0)
                    .rows(row_defs)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .keyed_children(cells),
            )
            .into()
    }

    /// 「设置」页：语言（自系统页迁移）+ 更新（自动检查/立即检查/各加载器状态与进度）。
    pub(in crate::reactor_app) fn settings_page(&self, context: &mut ViewContext<Self>) -> View {
        let lang_items: Vec<String> = vec![
            t!("中文", "Chinese").to_string(),
            "English".to_string(),
        ];
        let lang_idx = if self.lang == Lang::En { 1 } else { 0 };

        let title: View = StackPanel::new()
            .spacing(4.0)
            .children((
                View::from(
                    TextBlock::new()
                        .text(t!("设置", "Settings"))
                        .font_size(26.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                ),
                View::from(
                    TextBlock::new()
                        .text(t!("语言与自动更新", "Language and automatic updates"))
                        .font_size(12.0)
                        .opacity(0.6),
                ),
            ))
            .into();

        let lang_card = Border::new()
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(settings_row(
                SymbolIcon::new().symbol(Symbol::Globe).into(),
                t!("语言", "Language"),
                t!("界面显示语言；切换后立即生效", "Interface display language; applies immediately"),
                ComboBox::new()
                    .width(180.0)
                    .items_source(lang_items)
                    .selected_index(Some(lang_idx))
                    .on_selection_changed(context.callback(Message::LangSelected))
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            ));

        // 服务器地址（域名 / IP）；留空 = 默认 sdgt.arcadehub.xyz。
        // 复用网络设置写入路径，改动即时同步给后台服务器探测线程。
        let server_card = Border::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(settings_row(
                SymbolIcon::new().symbol(Symbol::Globe).into(),
                t!("服务器地址", "Server address"),
                t!("服务器地址（域名或 IP），留空为默认 sdgt.arcadehub.xyz", "Private server address (domain or IP); blank = default sdgt.arcadehub.xyz"),
                View::from(
                    TextBox::new()
                        .width(360.0)
                        .placeholder_text(t!("默认 sdgt.arcadehub.xyz", "default sdgt.arcadehub.xyz"))
                        .text(self.net.server.clone().unwrap_or_default())
                        .on_text_changed(context.callback(Message::ServerHostChanged))
                        .vertical_alignment(VerticalAlignment::Center),
                ),
            ));

        // 房间昵称：店内对战成员表「成员」列显示；留空 = 未设置（创建/加入房间时弹窗提示）。
        // 与服务器地址卡片同模式：输入即时写入 config.ini [room] nickname。
        let nick_card = Border::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(settings_row(
                SymbolIcon::new().symbol(Symbol::Contact).into(),
                t!("房间昵称", "Room nickname"),
                t!("店内对战房间内显示的昵称（≤24 字符），留空则进房时提示",
                   "Nickname shown in the room member table (up to 24 chars); blank = prompt on room create/join"),
                View::from(
                    TextBox::new()
                        .width(360.0)
                        .placeholder_text(t!("例如：player1234", "e.g. player1234"))
                        .text(self.room.nickname.clone().unwrap_or_default())
                        .on_text_changed(context.callback(Message::RoomNickChanged))
                        .vertical_alignment(VerticalAlignment::Center),
                ),
            ));

        let update_card = Border::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .corner_radius(4.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .children((
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Manage).into(),
                        t!("自动检查更新", "Automatically check for updates"),
                        t!("每隔约 1 小时后台检查一次", "Check in background about every hour"),
                        settings_toggle(
                            self.settings.auto_check_updates,
                            true,
                            context.callback(Message::AutoUpdateToggled),
                        ),
                    ),
                    separator(),
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Repair).into(),
                        t!("检查更新", "Check for updates"),
                        t!("立即向更新源查询最新版本", "Query update sources for the latest version now"),
                        Button::new()
                            .on_click(context.message(Message::CheckUpdatesNow))
                            .content(t!("立即检查", "Check now"))
                            .into(),
                    ),
                    separator(),
                )),
            );

        // 组件更新：一级标题 + 「全部更新」按钮（并排，按钮居右）
        // 与 settings_row 相同的 16px 水平边距：让「全部更新」按钮与「检查更新」按钮左缘对齐
        let components_title: View = Grid::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .margin(Thickness::new(16.0, 0.0, 16.0, 0.0))
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(16.0)
            .children((
                TextBlock::new()
                    .text(t!("组件更新", "Component updates"))
                    .font_size(20.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                Button::new()
                    .on_click(context.message(Message::UpdateAllClicked))
                    .style(ButtonStyle::Accent)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .content(t!("全部更新", "Update all")),
            ))
            .into();

        // 内容较长（5 个组件 + 更新源 / 语言 / 自动检查），超出窗口高度时可滚动
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                // 与输入设置页一致：ScrollViewer 无外层右边距，滚动条贴窗口最右；
                // 28px 右缩进放在内部内容上，卡片不贴住滚动条
                StackPanel::new()
                    .margin(Thickness::new(28.0, 16.0, 28.0, 24.0))
                    .spacing(14.0)
                    .children((
                        title,
                        lang_card,
                        server_card,
                        nick_card,
                        update_card,
                        components_title,
                        self.loaders_card(context),
                    )),
            )
            .into()
    }

    /// 更新检查结果回灌：成功套用（有更新者自动展开折叠行），失败把各目标标记为错误
    pub(in crate::reactor_app) fn apply_updates_checked(
        &mut self,
        res: Result<HashMap<UpdateTarget, TargetStatus>, String>,
    ) {
        match res {
        Ok(results) => {
            update::apply_check(&self.update_shared, results);
            // 有可用更新：收集明细并弹醒目提醒（同一批版本只提醒一次；
            // 前往设置页后重置标记，下次检查有新版本会再次提醒）
            let outdated: Vec<String> = {
                let s = self.update_shared.lock().unwrap();
                update::ALL_TARGETS
                    .iter()
                    .filter_map(|t| {
                        s.per_target.get(t).and_then(|ts| {
                            (ts.state == UpdateState::Outdated).then(|| {
                                format!(
                                    "{}　{}",
                                    t.display_name(),
                                    ts.remote_version.as_deref().unwrap_or("?")
                                )
                            })
                        })
                    })
                    .collect()
            };
            if !outdated.is_empty() {
                let key = outdated.join("|");
                if self.update_prompt_key.as_deref() != Some(key.as_str()) {
                    self.update_prompt_key = Some(key);
                    self.dialog = Dialog::UpdateAvailable {
                        detail: outdated.join("\n"),
                    };
                }
            }
        }
        Err(e) => {
            if let Ok(mut s) = self.update_shared.lock() {
                for ts in s.per_target.values_mut() {
                    ts.state = UpdateState::Error;
                    ts.phase = UpdatePhase::Error;
                    ts.detail = Some(e.clone());
                }
            }
            self.dialog = Dialog::Error {
                message: tf!("检查更新失败：{e}", "Update check failed: {e}"),
            };
        }
        }
    }

    /// 「取消更新」：置位取消标记 + 关闭底层网络句柄强制解阻塞 + UI 立即退出进行中状态。
    pub(in crate::reactor_app) fn cancel_loader_update(&mut self, target: UpdateTarget) {
        if let Some(flag) = self.update_cancel.get(&target) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // 关闭请求句柄：阻塞中的连接/发送/接收/读块调用立即失败，后台线程随即退出
        if let Some(abort) = self.update_abort.get(&target) {
            abort.close_all();
        }
        // UI 立即反馈：进度条消失、按钮恢复
        if let Ok(mut s) = self.update_shared.lock() {
            if let Some(ts) = s.per_target.get_mut(&target) {
                ts.phase = UpdatePhase::Idle;
                ts.download_done = 0;
                ts.download_total = 0;
            }
        }
    }
    /// 触发单个加载器的下载安装：后台执行，进度由共享状态承载、UpdatePoll 心跳刷新
    pub(in crate::reactor_app) fn start_loader_update(&mut self, target: UpdateTarget, context: &ComponentContext<Self>) {
        let (url, mirrors, sha) = {
            let s = self.update_shared.lock().unwrap();
            let ts = s.per_target.get(&target).cloned().unwrap_or_default();
            (ts.url.clone(), ts.mirrors.clone(), ts.sha256.clone())
        };
        if url.is_none() {
            self.dialog = Dialog::Error {
                message: t!("没有可用的下载地址", "No download URL available").to_string(),
            };
            return;
        }
        // mirrors 保留字段但当前恒为空（单源查询，不做镜像轮询）
        if self.game_running {
            self.dialog = Dialog::Error {
                message: t!(
                    "游戏运行中，无法更新；请先退出游戏",
                    "Cannot update while a game is running"
                )
                .to_string(),
            };
            return;
        }
        let shared = self.update_shared.clone();
        let base = update::tools_dir();
        let progress = {
            let shared = shared.clone();
            move |phase: UpdatePhase, done: u64, total: u64| {
                if let Ok(mut s) = shared.lock() {
                    if let Some(ts) = s.per_target.get_mut(&target) {
                        ts.phase = phase;
                        ts.download_done = done;
                        ts.download_total = total;
                    }
                }
            }
        };
        {
            let mut s = shared.lock().unwrap();
            if let Some(ts) = s.per_target.get_mut(&target) {
                ts.phase = UpdatePhase::Downloading;
                ts.download_done = 0;
                ts.download_total = 0;
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.update_cancel.insert(target, cancel.clone());
        let abort = Arc::new(update::HttpAbort::new());
        self.update_abort.insert(target, abort.clone());
        self.ensure_poll(context);
        context.spawn_background(move |_| {
            let r = update::download_and_install(
                target,
                url.as_deref().unwrap(),
                &mirrors,
                sha.as_deref(),
                &base,
                &cancel,
                &abort,
                &progress,
            );
            Message::UpdateFinished(target, r)
        });
    }

    /// 下载安装结束：取消→恢复原状态；成功回写版本并标记已最新；失败标记错误
    pub(in crate::reactor_app) fn finish_update(&mut self, target: UpdateTarget, result: Result<(), String>) {
        let was_cancelled = self
            .update_cancel
            .remove(&target)
            .map(|f| f.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false);
        if was_cancelled && result.is_err() {
            if let Ok(mut s) = self.update_shared.lock() {
                if let Some(ts) = s.per_target.get_mut(&target) {
                    // 保留 state（有更新/已最新），仅退出进行中阶段
                    ts.phase = UpdatePhase::Idle;
                    ts.download_done = 0;
                    ts.download_total = 0;
                }
            }
            return;
        }
        // 竞态：点了取消但下载实际已完成 → 按成功处理
        match result {
        Ok(()) => {
            let ver = {
                let s = self.update_shared.lock().unwrap();
                s.per_target
                    .get(&target)
                    .and_then(|t| t.remote_version.clone())
            };
            if let Some(v) = ver {
                update::set_last_version(&mut self.settings, target, &v);
                self.store.save_settings(&self.settings);
                if let Ok(mut s) = self.update_shared.lock() {
                    if let Some(ts) = s.per_target.get_mut(&target) {
                        ts.state = UpdateState::UpToDate;
                        ts.phase = UpdatePhase::Done;
                        ts.download_done = ts.download_total.max(1);
                        ts.detail = Some(v.clone());
                    }
                }
            }
            // 安装成功后重扫加载器：self.detected 是启动器启动时的一次性目录扫描缓存，
            // 不刷新的话本会话内新装/更新的 mice/kitten 会一直报「未找到」，重启启动器才恢复
            self.detected = crate::launch::detect_loaders();
        }
        Err(e) => {
            if let Ok(mut s) = self.update_shared.lock() {
                if let Some(ts) = s.per_target.get_mut(&target) {
                    ts.state = UpdateState::Error;
                    ts.phase = UpdatePhase::Error;
                }
            }
            self.dialog = Dialog::Error {
                message: tf!("更新失败：{e}", "Update failed: {e}"),
            };
        }
        }
    }

    /// 进度心跳：仍有活动阶段则继续排下一拍，否则停止轮询
    pub(in crate::reactor_app) fn poll_update_progress(&mut self, context: &ComponentContext<Self>) {
        let active = {
            let s = self.update_shared.lock().unwrap();
            s.per_target.values().any(|t| {
                matches!(
                    t.phase,
                    UpdatePhase::Checking
                        | UpdatePhase::Downloading
                        | UpdatePhase::Extracting
                        | UpdatePhase::Installing
                )
            })
        };
        if active {
            self.ensure_poll(context);
        } else {
            self.poll_running = false;
        }
    }

}
