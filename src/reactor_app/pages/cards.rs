//! 卡片选择页：ID5 / ID8 车卡与 aime 车卡的扫描、内存合成（底图 + 车卡叠加）与展示。
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use windows_reactor::*;

use crate::cards::{self, CardEntry, CardScan};
use crate::t;
use crate::tf;
use crate::storage::Loader;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::Message;
use crate::reactor_app::state::CardBaseCache;
use crate::reactor_app::widgets::placeholder_page;

/// 卡片底图（ID5 驾照卡设计）原始尺寸 698x441；ID8 底图为 698x440（见 ID8_BASE_W/H）
pub(in crate::reactor_app) const CARD_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const CARD_BASE_H: f64 = 441.0;

/// 卡片显示尺寸：固定显示宽度（高度按当前底图比例在 card_fallback_size 中推导）
pub(in crate::reactor_app) const CARD_W: f64 = 380.0;
/// 合成完成后整体缩小 20%：展示尺寸乘以 0.8（合成 PNG 本身保留原分辨率，仅在显示层缩放）
pub(in crate::reactor_app) const CARD_DISPLAY_SCALE: f64 = 0.8;
/// 卡片行内列间距 / 行间距（DIP）
pub(in crate::reactor_app) const CARD_GAP: f64 = 24.0;
pub(in crate::reactor_app) const CARD_ROW_SPACING: f64 = 16.0;
/// 卡片页内容区左右外边距（与页面 Grid 的 margin 左右值保持一致）
pub(in crate::reactor_app) const CARD_PAGE_MARGIN: f64 = 28.0;
/// 计算一行可放几张卡时预留的垂直滚动条宽度 + 安全余量
pub(in crate::reactor_app) const CARD_SCROLL_RESERVE: f64 = 20.0;
/// 选中卡片整卡叠加的系统强调色透明度（浅蓝，约 14%）
pub(in crate::reactor_app) const CARD_ACTIVE_TINT_OPACITY: f64 = 0.14;

/// 内置底图字节（编译期嵌入 exe，纯内嵌方案，不再落盘 data/assets/）
pub(in crate::reactor_app) const CARD_BASE_PNG: &[u8] = include_bytes!("../../assets/id5_base.png");

/// aime 卡底图（segatools / SDDF、SDGT 的车卡选择），编译期内嵌
pub(in crate::reactor_app) const AIME_BASE_PNG: &[u8] = include_bytes!("../../assets/aime.png");
pub(in crate::reactor_app) const AIME_BASE_W: f64 = 682.0;
pub(in crate::reactor_app) const AIME_BASE_H: f64 = 432.0;

/// ID8（头文字D Arcade Stage 8，SBZZ / SDAL）底图，原始尺寸 698x440，编译期内嵌
pub(in crate::reactor_app) const ID8_BASE_PNG: &[u8] = include_bytes!("../../assets/id8_base.png");
pub(in crate::reactor_app) const ID8_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID8_BASE_H: f64 = 440.0;

/// ID4（头文字D4 / D4 kai，SBML，kitten 加载器）底图，原始尺寸 698x441，编译期内嵌
pub(in crate::reactor_app) const ID4_BASE_PNG: &[u8] = include_bytes!("../../assets/id4_base.png");
pub(in crate::reactor_app) const ID4_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID4_BASE_H: f64 = 441.0;

/// ID6（头文字D6 系列，SBUU / SBVW / SBWV，mice 加载器）底图，原始尺寸 698x440，编译期内嵌
pub(in crate::reactor_app) const ID6_BASE_PNG: &[u8] = include_bytes!("../../assets/id6_base.png");
pub(in crate::reactor_app) const ID6_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID6_BASE_H: f64 = 440.0;

/// 识别库（games_db.json）里 image 字段指向的默认封面：全部编译期嵌入 exe，
/// 运行时直接从字节渲染，不再写出到 data/assets/（用户也无法再替换）。
pub(in crate::reactor_app) const COVER_ASSETS: &[(&str, &[u8])] = &[
    ("logo_D4.png", include_bytes!("../../assets/logo_D4.png")),
    ("logo_D4kai.png", include_bytes!("../../assets/logo_D4kai.png")),
    ("logo_D5.png", include_bytes!("../../assets/logo_D5.png")),
    ("logo_D6.png", include_bytes!("../../assets/logo_D6.png")),
    (
        "logo_D6chn.png",
        include_bytes!("../../assets/logo_D6chn.png"),
    ),
    ("logo_D7.png", include_bytes!("../../assets/logo_D7.png")),
    ("logo_D7chn.png", include_bytes!("../../assets/logo_D7chn.png")),
    ("logo_D8.png", include_bytes!("../../assets/logo_D8.png")),
    ("logo_dac.png", include_bytes!("../../assets/logo_dac.png")),
    ("logo_idz.png", include_bytes!("../../assets/logo_idz.png")),
    // 高清 big 版（仅启动时悬浮窗使用；游戏行仍用上面的小图）
    ("logo_D4_big.png", include_bytes!("../../assets/logo_D4_big.png")),
    ("logo_D4kai_big.png", include_bytes!("../../assets/logo_D4kai_big.png")),
    ("logo_D5_big.png", include_bytes!("../../assets/logo_D5_big.png")),
    ("logo_D6_big.png", include_bytes!("../../assets/logo_D6_big.png")),
    ("logo_D6chn_big.png", include_bytes!("../../assets/logo_D6chn_big.png")),
    ("logo_D7_big.png", include_bytes!("../../assets/logo_D7_big.png")),
    ("logo_D7chn_big.png", include_bytes!("../../assets/logo_D7chn_big.png")),
    ("logo_D8_big.png", include_bytes!("../../assets/logo_D8_big.png")),
    ("logo_dac_big.png", include_bytes!("../../assets/logo_dac_big.png")),
    ("logo_idz_big.png", include_bytes!("../../assets/logo_idz_big.png")),
];

/// 车卡相对居中位置的像素偏移（画板坐标，合成时换算为物理像素）：向右 / 向下
pub(in crate::reactor_app) const CARD_CAR_OFFSET_X: i64 = 0;
pub(in crate::reactor_app) const CARD_CAR_OFFSET_Y: i64 = 4;

impl Launcher {

    /// 卡片选择页：返回按钮 + 游戏名 + 启动按钮；卡片 = 底图 + 打印层两张图组合
    pub(in crate::reactor_app) fn cards_page(&self, context: &mut ViewContext<Self>) -> View {
        // 进入过渡动画：发送首个 tick，update 中递归调度后续帧，驱动左右滑入 + 淡入
        let seq = self.card_enter_seq;
        let sender = context.sender();
        context.use_effect("card-enter-anim", seq, move || {
            let _ = sender.send(Message::CardSlideTick(0.0));
            None
        });
        let Some(gi) = self.cards_game else {
            return placeholder_page(t!("卡片选择", "Card select"), t!("请先在「游戏」页单击一个游戏进入卡片选择。", "Go to the Games page and click a game to open card selection."));
        };
        let Some(game) = self.library.games.get(gi) else {
            return placeholder_page(t!("卡片选择", "Card select"), t!("游戏不存在。", "The game no longer exists."));
        };
        let _ = game;
        let aime_mode = self.cards_aime;
        // aime 卡显示尺寸：宽度与 ID5 卡一致，高度按 aime 底图（682×432）宽高比
        let aime_item_w = CARD_W * CARD_DISPLAY_SCALE;
        let aime_item_h = aime_item_w * AIME_BASE_H / AIME_BASE_W;

        // 卡片网格（行拆分 + 滚动容器）：ID5 与 aime 两种卡共用
        let wrap_rows = |rows: Vec<KeyedView>| -> View {
            ScrollViewer::new()
                .margin(Thickness::new(0.0, 12.0, 0.0, 0.0))
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Visible)
                // 窗口过窄时固定尺寸卡片放不下，允许横向滚动兜底，避免被裁切
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                // 虚拟化：仅实例化视口内的卡片行，滚出的行被回收，不再让全部
                // 车卡常驻 GPU 合成——这是滚轮滚动卡顿的根因（非虚拟化铺满所有图元）。
                // 早期担心“虚拟化导致 Image 反复解码闪烁”已不成立：框架对 EncodedImage
                // 按内容判等，且 compose_cache 缓存合成字节，re-realize 不会重新解码。
                .content(
                    ItemsRepeater::new()
                        .items(rows),
                )
                .into()
        };

        let body: View = if self.cards_loading {
            // 加载过渡：转圈 + 文案（扫描与合成缓存预热期间显示）
            StackPanel::new()
                .spacing(12.0)
                .margin(Thickness::new(0.0, 48.0, 0.0, 0.0))
                .horizontal_alignment(HorizontalAlignment::Center)
                .children((
                    ProgressRing::new()
                        .is_indeterminate(true)
                        .is_active(true)
                        .width(40.0)
                        .height(40.0)
                        .horizontal_alignment(HorizontalAlignment::Center),
                    TextBlock::new()
                        .text(t!("正在加载卡片…", "Loading cards…"))
                        .font_size(14.0)
                        .opacity(0.7)
                        .horizontal_alignment(HorizontalAlignment::Center),
                ))
                .into()
        } else if aime_mode {
            // aime 模式：「新建车卡」图框排最前，其后为卡片列表（新卡在前）
            let cols = self.card_columns(aime_item_w);
            let mut items: Vec<KeyedView> = vec![KeyedView::new(
                "aime-create",
                self.aime_create_tile(context, aime_item_w, aime_item_h),
            )];
            for (i, c) in self.cards.cards.iter().enumerate() {
                items.push(KeyedView::new(
                    format!("aime-{i}"),
                    self.aime_item(context, i, c, aime_item_w, aime_item_h),
                ));
            }
            let card_rows: Vec<KeyedView> = items
                .chunks(cols)
                .enumerate()
                .map(|(row, chunk)| {
                    let panel = StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(CARD_GAP)
                        .margin(Thickness::new(0.0, 0.0, 0.0, CARD_ROW_SPACING))
                        .keyed_children(chunk.to_vec());
                    KeyedView::new(format!("card-row-{row}"), panel)
                })
                .collect();
            wrap_rows(card_rows)
        } else {
            // IDAS 模式：「空白车卡」图框排最前，其后为扫描到的卡图列表
            // 汇总合成失败情况（命中缓存，成本仅为 HashMap 查找），有失败时提示一次到状态栏
            let mut failed = 0usize;
            let mut first_reason = String::new();
            for card in &self.cards.cards {
                if let Err(reason) = self.compose_card_image(&card.path, self.dpi_scale) {
                    failed += 1;
                    if first_reason.is_empty() {
                        first_reason = reason.to_string();
                    }
                }
            }
            if failed > 0 {
                let summary = if failed == 1 {
                    tf!("卡片无法显示：{}", "Card cannot be displayed: {}", first_reason)
                } else {
                    tf!("{} 张卡片无法显示：{}", "{} cards cannot be displayed: {}", failed, first_reason)
                };
                // 渲染期不能改状态：用 effect 发送一次消息；依赖未变则不重复提示
                let sender = context.sender();
                context.use_effect(
                    "card-compose-error",
                    (self.cards_game, summary.clone()),
                    move || {
                        let _ = sender.send(Message::CardComposeFailed(summary));
                        None
                    },
                );
            }

            // 空白车卡图框尺寸与其他车卡一致：底图物理像素 ÷ DPI × 0.8
            // （合成画布恒为底图尺寸，故所有卡显示尺寸相同）
            let (bw, bh) = if self.cards_id4 {
                (ID4_BASE_W, ID4_BASE_H)
            } else if self.cards_id6 {
                (ID6_BASE_W, ID6_BASE_H)
            } else if self.cards_id8 {
                (ID8_BASE_W, ID8_BASE_H)
            } else {
                (CARD_BASE_W, CARD_BASE_H)
            };
            let s = self.dpi_scale.max(0.01);
            let blank_w = bw / s * CARD_DISPLAY_SCALE;
            let blank_h = bh / s * CARD_DISPLAY_SCALE;
            let blank_active = self
                .cards
                .active
                .as_deref()
                .is_some_and(|a| a.starts_with("InidCrd") && a.ends_with(".png"));

            // 卡片尺寸固定，按窗口可用宽度动态计算一行放几张（默认 2 张，窗口变宽变多、变窄减少）
            let cols = self.card_columns(self.card_display_w());
            let mut items: Vec<KeyedView> = vec![KeyedView::new(
                "blank-card",
                self.blank_card_tile(context, blank_w, blank_h, blank_active),
            )];
            for (i, c) in self.cards.cards.iter().enumerate() {
                items.push(KeyedView::new(
                    format!("card-{i}"),
                    self.card_item(context, i, c),
                ));
            }
            let card_rows: Vec<KeyedView> = items
                .chunks(cols)
                .enumerate()
                .map(|(row, chunk)| {
                    let panel = StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(CARD_GAP)
                        .margin(Thickness::new(0.0, 0.0, 0.0, CARD_ROW_SPACING))
                        .keyed_children(chunk.to_vec());
                    KeyedView::new(format!("card-row-{row}"), panel)
                })
                .collect();
            wrap_rows(card_rows)
        };

        // 进入过渡动画进度：0→1，驱动左右滑入 + 淡入
        let active = self.card_slide;
        // 外层用 Grid 弹性行：标题行 Auto，卡片区 Star 填满剩余空间（窗口最大化后自动变大）
        let title: View = TextBlock::new()
            .text(t!("卡片选择", "Card select"))
            .font_size(26.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .into();
        let content = Grid::new()
            .margin(Thickness::new(28.0, 20.0, 28.0, 16.0))
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(8.0)
            .children((
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0)])
                    .column_spacing(16.0)
                    .grid_row(0)
                    .children((
                        Button::new()
                            .on_click(context.message(Message::BackToGames))
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(0)
                            .content(SymbolIcon::new().symbol(Symbol::Back)),
                        StackPanel::new()
                            .spacing(4.0)
                            .margin(Thickness::new(8.0, 0.0, 0.0, 0.0))
                            .grid_column(1)
                            .children((title,)),
                    )),
                Border::new().grid_row(1).content(body),
            ));

        // 进入过渡动画：随进度从右侧滑入（margin 80→0）+ 淡入（opacity 0→1），Win11 设置风格
        // 同时设置不透明背景，避免滚动时透出 Mica 动态背景造成颜色闪烁
        let slide = active;
        Border::new()
            .opacity(slide)
            .margin(Thickness::new((1.0 - slide) * 80.0, 0.0, 0.0, 0.0))
            .background(ThemeBrush::CardBackground)
            .content(content)
            .into()
    }

    /// 空白车卡图框：IDAS 卡片网格最前方，点击选用 InidCrd 随机编号的空白卡
    /// （启动时随 idas.ini 写入 [CardReader] cardfile）
    fn blank_card_tile(
        &self,
        context: &mut ViewContext<Self>,
        w: f64,
        h: f64,
        active: bool,
    ) -> View {
        Border::new()
            .width(w)
            .height(h)
            .corner_radius(6.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(if active {
                ThemeBrush::Accent
            } else {
                ThemeBrush::CardStroke
            })
            .border_thickness(if active { 2.0 } else { 1.0 })
            .on_pointer_pressed(context.callback(|_| Message::BlankCardSelect))
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center)
                    .spacing(6.0)
                    .children((
                        TextBlock::new()
                            .text(t!("空白车卡", "Blank card"))
                            .font_size(15.0)
                            .font_weight(FontWeight::SEMI_BOLD),
                        TextBlock::new()
                            .text(t!("点击选用", "Click to select"))
                            .font_size(11.0)
                            .opacity(0.6),
                    )),
            )
            .into()
    }

    /// aime 卡底图字节（编译期内嵌，OnceLock 缓存 Arc 避免每帧复制）
    fn aime_base_arc() -> Arc<[u8]> {
        static AIME: OnceLock<Arc<[u8]>> = OnceLock::new();
        AIME.get_or_init(|| Arc::from(AIME_BASE_PNG.to_vec())).clone()
    }

    /// 新建车卡图框：aime 模式卡片网格最前方的占位框，点击生成 20 位 access code
    fn aime_create_tile(&self, context: &mut ViewContext<Self>, w: f64, h: f64) -> View {
        Border::new()
            .width(w)
            .height(h)
            .corner_radius(6.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .on_pointer_pressed(context.callback(|_| Message::AimeCardCreate))
            .content(
                StackPanel::new()
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center)
                    .spacing(6.0)
                    .children((
                        TextBlock::new().text("＋").font_size(30.0).opacity(0.7),
                        TextBlock::new()
                            .text(t!("新建车卡", "New card"))
                            .font_size(13.0),
                    )),
            )
            .into()
    }

    /// 单张 aime 卡：aime.png 底图 + 左上角 access code（4 位一组空格分隔）；
    /// 选中态与 ID5 卡一致（强调色浅蓝染色 + 2px 边框），不显示文件名标签。
    fn aime_item(
        &self,
        context: &mut ViewContext<Self>,
        index: usize,
        card: &CardEntry,
        w: f64,
        h: f64,
    ) -> View {
        let active = self.cards.active_index() == Some(index);

        let content_view: View = Grid::new()
            .children((
                Image::new()
                    .source_data(EncodedImage::new(Self::aime_base_arc()))
                    .width(w)
                    .height(h)
                    .stretch(Stretch::Uniform),
                TextBlock::new()
                    .text(cards::format_access_code(&card.name))
                    .font_size(14.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    // 底图固定为白底，文字固定深色：不跟随系统深/浅色主题，
                    // 否则深色模式下浅色字压在白底上看不见。
                    .foreground(Brush::Solid(Color::argb(255, 0x1A, 0x1A, 0x1A)))
                    .margin(Thickness::new(14.0, 10.0, 0.0, 0.0))
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .vertical_alignment(VerticalAlignment::Top),
            ))
            .into();

        // 选中态：整卡盖一层系统强调色的半透明浅蓝（与蓝色边框共同标识当前卡）
        let content_view: View = if active {
            let tint: View = Border::new()
                .width(w)
                .height(h)
                .corner_radius(6.0)
                .background(ThemeBrush::Accent)
                .opacity(CARD_ACTIVE_TINT_OPACITY)
                .into();
            Grid::new().children((content_view, tint)).into()
        } else {
            content_view
        };

        Border::new()
            .width(w)
            .height(h)
            .corner_radius(6.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(if active {
                ThemeBrush::Accent
            } else {
                ThemeBrush::CardStroke
            })
            .border_thickness(if active { 2.0 } else { 1.0 })
            .on_pointer_pressed(context.callback(move |_| Message::CardClicked(index)))
            .content(content_view)
            .into()
    }

    /// aime 卡目录：当前卡片页游戏的 amdaemon.exe 所在目录下的 `DEVICE`；
    /// 未配置 amdaemon 时为 None。
    pub(in crate::reactor_app) fn aime_device_dir(&self) -> Option<PathBuf> {
        let gi = self.cards_game?;
        let rec = self.library.games.get(gi)?;
        let amdaemon = PathBuf::from(rec.amdaemon.as_deref()?);
        Some(amdaemon.parent()?.join("DEVICE"))
    }

    /// 识别库默认封面（exe 内嵌字节）按文件名查表；未命中返回 None（退回占位块）。
    pub(in crate::reactor_app) fn builtin_cover_bytes(name: &str) -> Option<&'static [u8]> {
        COVER_ASSETS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, bytes)| *bytes)
    }

    /// 确保底图已加载（纯内嵌字节，永不空白）：只解码一次并缓存结果（含失败原因）。
    /// 底图按当前卡片页模式选择：ID4（SBML）用 id4_base，ID6（SBUU / SBVW / SBWV）用 id6_base，
    /// ID8（SBZZ / SDAL）用 id8_base，其余用 ID5 底图（id5_base）。
    fn ensure_card_base(&self) {
        if self.base_cache.borrow().is_some() {
            return;
        }
        let png = if self.cards_id4 {
            ID4_BASE_PNG
        } else if self.cards_id6 {
            ID6_BASE_PNG
        } else if self.cards_id8 {
            ID8_BASE_PNG
        } else {
            CARD_BASE_PNG
        };
        *self.base_cache.borrow_mut() = Some(CardBaseCache {
            result: Self::decode_builtin_base(png),
        });
    }

    /// 从编译期嵌入的字节解码内置底图
    fn decode_builtin_base(png: &[u8]) -> Result<image::RgbaImage, String> {
        image::load_from_memory(png)
            .map(|img| img.to_rgba8())
            .map_err(|e| tf!("解码内置底图失败：{}", "Failed to decode built-in base image: {}", e))
    }

    /// 车卡相对底图居中位置的像素偏移（画板坐标，合成时换算为物理像素）：向右 / 向下。
    /// 头文字D4 因底图版式需要整体左移 1px（offset_x = -1），其余沿用默认（右移 0 / 下移 4）。
    fn card_car_offset(&self) -> (i64, i64) {
        if self.cards_id4 {
            (-5, CARD_CAR_OFFSET_Y)
        } else {
            (CARD_CAR_OFFSET_X, CARD_CAR_OFFSET_Y)
        }
    }

    /// 把车卡按原尺寸叠加到底图居中位置，再按画板偏移右移 / 下移（越界由 image 安全裁剪）
    fn overlay_car(
        mut base: image::RgbaImage,
        car: &image::RgbaImage,
        dpi_scale: f64,
        offset_x: i64,
        offset_y: i64,
    ) -> image::RgbaImage {
        // 位移从画板坐标换算为画布物理像素（乘以 DPI 缩放），与屏幕显示保持一致
        let scale = dpi_scale.max(0.01);
        let x = (base.width() as i64 - car.width() as i64) / 2
            + (offset_x as f64 * scale) as i64;
        let y = (base.height() as i64 - car.height() as i64) / 2
            + (offset_y as f64 * scale) as i64;
        image::imageops::overlay(&mut base, car, x, y);
        base
    }

    /// 在内存中合成底图 + 车卡为一张 PNG（带缓存：同一张卡只合成一次，失败原因同样缓存）
    fn compose_card_image(
        &self,
        car_path: &Path,
        dpi_scale: f64,
    ) -> Result<(Arc<[u8]>, f64, f64), Arc<str>> {
        // 缓存命中直接返回（成功与失败都缓存），避免窗口缩放/重渲染时重复解码编码
        if let Some(cached) = self.compose_cache.borrow().get(car_path) {
            return cached.clone();
        }
        // 记录失败原因，避免损坏的卡图被每帧反复解码
        let fail = |reason: String| {
            let err: Arc<str> = reason.into();
            self.compose_cache
                .borrow_mut()
                .insert(car_path.to_path_buf(), Err(err.clone()));
            Err(err)
        };

        // 先确保底图就绪（内部完成可变借用后再取用，避免 RefCell 借用冲突）
        self.ensure_card_base();
        let base = match self.base_cache.borrow().as_ref().map(|c| &c.result) {
            Some(Ok(img)) => img.clone(),
            Some(Err(e)) => return fail(tf!("底图加载失败：{}", "Base image load failed: {}", e)),
            None => return fail(t!("底图未加载", "Base image not loaded").to_string()),
        };
        let (canvas_w, canvas_h) = (base.width(), base.height());

        let car = match image::open(car_path) {
            Ok(img) => img.to_rgba8(),
            Err(e) => return fail(tf!("卡图解码失败：{}", "Card image decode failed: {}", e)),
        };

        let (ox, oy) = self.card_car_offset();
        let composed = Self::overlay_car(base, &car, dpi_scale, ox, oy);

        // 快速 PNG 编码（Fast 压缩 + 无滤波），降低合成耗时
        let mut out = Cursor::new(Vec::new());
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        );
        if let Err(e) = composed.write_with_encoder(encoder) {
            return fail(tf!("合成图编码失败：{}", "Composite image encode failed: {}", e));
        }

        let bytes: Arc<[u8]> = out.into_inner().into();
        let result = (bytes, canvas_w as f64, canvas_h as f64);

        self.compose_cache
            .borrow_mut()
            .insert(car_path.to_path_buf(), Ok(result.clone()));
        Ok(result)
    }

    /// 当前卡片的显示宽度（DIP）：合成画布像素 ÷ DPI × 0.8，合成失败时回退设计宽度。
    /// 仅读缓存（不触发合成）：卡片页进入时已分拍预热（warm_card_cache），
    /// 渲染时缓存必然命中；未命中（极少数边界）直接回退设计宽度，避免布局阶段在 UI 线程解码。
    fn card_display_w(&self) -> f64 {
        let s = self.dpi_scale.max(0.01);
        self.cards
            .cards
            .first()
            .and_then(|c| self.compose_cache.borrow().get(&c.path).cloned())
            .and_then(|r| r.ok())
            .map(|(_, w, _)| w / s * CARD_DISPLAY_SCALE)
            .unwrap_or(CARD_W * CARD_DISPLAY_SCALE)
    }

    /// 按窗口可用宽度计算一行能放几张卡（卡片尺寸固定，只让列数随窗口宽度变化）
    fn card_columns(&self, card_w: f64) -> usize {
        let avail = self.win_size.width - CARD_PAGE_MARGIN * 2.0 - CARD_SCROLL_RESERVE;
        // 首帧窗口尺寸未知（0）时回退默认 2 列
        if avail <= 0.0 || card_w <= 0.0 {
            return 2;
        }
        ((avail + CARD_GAP) / (card_w + CARD_GAP))
            .floor()
            .max(1.0) as usize
    }

    /// 合成失败时占位块的回退显示尺寸：按当前底图（ID5 698×441 / ID8 698×440）比例推导
    fn card_fallback_size(&self) -> (f64, f64) {
        let (bw, bh) = if self.cards_id4 {
            (ID4_BASE_W, ID4_BASE_H)
        } else if self.cards_id6 {
            (ID6_BASE_W, ID6_BASE_H)
        } else if self.cards_id8 {
            (ID8_BASE_W, ID8_BASE_H)
        } else {
            (CARD_BASE_W, CARD_BASE_H)
        };
        (
            CARD_W * CARD_DISPLAY_SCALE,
            CARD_W * bh / bw * CARD_DISPLAY_SCALE,
        )
    }

    /// 单张卡片：内存合成底图 + 车卡为一张图，按固定尺寸（设计稿 80%）显示；
    /// 选中时整卡叠加一层系统强调色的半透明浅蓝。
    fn card_item(&self, context: &mut ViewContext<Self>, index: usize, card: &CardEntry) -> View {
        let active = self.cards.active_index() == Some(index);
        let (fallback_w, fallback_h) = self.card_fallback_size();

        // 固定尺寸：合成图物理像素换算为 DIP（除以 DPI 缩放），再整体缩小 20%
        let s = self.dpi_scale.max(0.01);
        let (content_view, card_w, card_h): (View, f64, f64) =
            match self.compose_card_image(&card.path, self.dpi_scale) {
                Ok((bytes, w, h)) => {
                    let cw = w / s * CARD_DISPLAY_SCALE;
                    let ch = h / s * CARD_DISPLAY_SCALE;
                    (
                        Image::new()
                            .source_data(EncodedImage::new(bytes))
                            .width(cw)
                            .height(ch)
                            .stretch(Stretch::Uniform)
                            .into(),
                        cw,
                        ch,
                    )
                }
                Err(reason) => (
                    // 合成/解码失败：占位块上直接显示原因，方便排查（不再是无声灰块）
                    Border::new()
                        .width(fallback_w)
                        .height(fallback_h)
                        .padding(Thickness::new(10.0, 10.0, 10.0, 10.0))
                        .background(ThemeBrush::CardStroke)
                        .content(
                            TextBlock::new()
                                .text(tf!("无法显示\n{}", "Cannot display\n{}", reason))
                                .font_size(11.0)
                                .opacity(0.85)
                                .text_wrapping(TextWrapping::Wrap)
                                .text_trimming(TextTrimming::CharacterEllipsis)
                                .max_lines(4)
                                .horizontal_alignment(HorizontalAlignment::Center)
                                .vertical_alignment(VerticalAlignment::Center),
                        )
                        .into(),
                    fallback_w,
                    fallback_h,
                ),
            };

        // 选中态：整卡再盖一层系统强调色的半透明浅蓝（与蓝色边框共同标识当前卡）。
        // 叠加层尺寸、圆角与合成图一致，指针事件仍冒泡到外层边框，不影响点击选中。
        let content_view: View = if active {
            let tint: View = Border::new()
                .width(card_w)
                .height(card_h)
                .corner_radius(6.0)
                .background(ThemeBrush::Accent)
                .opacity(CARD_ACTIVE_TINT_OPACITY)
                .into();
            Grid::new().children((content_view, tint)).into()
        } else {
            content_view
        };

        let composed = Border::new()
            .width(card_w)
            .height(card_h)
            .corner_radius(6.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(if active {
                ThemeBrush::Accent
            } else {
                ThemeBrush::CardStroke
            })
            .border_thickness(if active { 2.0 } else { 1.0 })
            .on_pointer_pressed(context.callback(move |_| Message::CardClicked(index)))
            .content(content_view);

        let name_label: View = if active {
            TextBlock::new()
                .text(tf!("{}（当前使用）", "{} (in use)", card.name))
                .font_size(12.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into()
        } else {
            TextBlock::new()
                .text(card.name.clone())
                .font_size(12.0)
                .opacity(0.7)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into()
        };

        StackPanel::new()
            .spacing(6.0)
            .width(card_w)
            .children((composed, name_label))
            .into()
    }

    /// 进入卡片选择页：后台扫描该游戏目录的卡图
    pub(in crate::reactor_app) fn enter_cards(&mut self, index: usize, context: &ComponentContext<Self>) {
        let Some(rec) = self.library.games.get(index) else {
            return;
        };
        let path = PathBuf::from(&rec.path);
        self.cards_game = Some(index);
        self.nav_tag = "cards".to_string();
        self.card_slide = 0.0;
        self.card_enter_seq += 1;
        self.cards = CardScan::default();
        // 切换游戏时丢弃上一游戏的合成结果与底图缓存（ID5 / ID8 底图不同），避免内存随浏览累积
        self.compose_cache.borrow_mut().clear();
        *self.base_cache.borrow_mut() = None;
        // 进入页面先显示 loading，扫描 + 预热完成后再展示卡片
        self.cards_loading = true;
        self.cards_warm = 0;
        // segatools 的 SDDF / SDER / SDGT：aime 卡（amdaemon 目录 DEVICE\ 下的 20 位数字 .txt）
        self.cards_aime = rec.loader == Loader::Segatools
            && matches!(rec.game_id.as_deref(), Some("SDDF") | Some("SDER") | Some("SDGT"));
        // 头文字D4 / D4 kai（SBML，kitten 加载器）：车卡合成使用 ID4 专属底图（id4_base.png）
        self.cards_id4 = rec.loader == Loader::Kitten
            && matches!(rec.game_id.as_deref(), Some("SBML"));
        // 头文字D6 系列（SBUU / SBVW / SBWV，mice 加载器）：车卡合成使用 ID6 专属底图（id6_base.png）
        self.cards_id6 = rec.loader == Loader::Mice
            && matches!(rec.game_id.as_deref(), Some("SBUU") | Some("SBVW") | Some("SBWV"));
        // 头文字D8（SBZZ / SDAL）：车卡合成使用 ID8 专属底图（id8_base.png）
        self.cards_id8 = rec.loader == Loader::Mice
            && matches!(rec.game_id.as_deref(), Some("SBZZ") | Some("SDAL"));
        if self.cards_aime {
            let dir = self.aime_device_dir();
            context.spawn_background(move |_| {
                let scan = match dir {
                    Some(d) => cards::scan_aime_cards_in(&d),
                    None => CardScan::default(),
                };
                Message::CardsScanned(Box::new(scan))
            });
        } else {
            context
                .spawn_background(move |_| Message::CardsScanned(Box::new(cards::scan_cards(&path))));
        }
    }

    /// 卡片扫描结果回灌：aime 卡直接展示，ID5 卡先进入分拍预热
    pub(in crate::reactor_app) fn apply_cards_scanned(&mut self, mut scan: Box<CardScan>, context: &ComponentContext<Self>) {
        // 优先用本游戏记录里的卡片，未配置时回退旧全局 config.ini [card] cardfile
        scan.active = self
            .cards_game
            .and_then(|gi| self.library.games.get(gi))
            .and_then(|rec| rec.card.clone())
            .or_else(|| self.store.load_card());
        self.cards = *scan;
        // IDAS：目录下没有车卡、且本游戏从未选过卡片时，默认选中空白车卡
        // （cardfile = InidCrdXXXXX.png）。仅在没有任何已选卡片（active 为 None）时
        // 才默认空白，避免重入时把用户已选的真实卡覆盖成空白。默认卡片写入本游戏记录
        // （不再写全局），保证每个游戏独立。
        if !self.cards_aime
            && self.cards.cards.is_empty()
            && self.cards.active.is_none()
        {
            let name = cards::blank_card_name();
            if let Some(gi) = self.cards_game {
                if let Some(rec) = self.library.games.get_mut(gi) {
                    rec.card = Some(name.clone());
                    self.store.save_library(&self.library);
                }
            }
            self.cards.active = Some(name);
        }
        if self.cards_aime {
            // aime 卡渲染轻量（单张内嵌底图 + 文本），无需预热
            self.cards_loading = false;
        } else {
            // ID5 卡较多时逐拍预热合成缓存，期间卡片页显示 loading
            self.cards_loading = true;
            self.cards_warm = 0;
            context.spawn_background(|_| {
                std::thread::sleep(Duration::from_millis(30));
                Message::CardsLoadTick
            });
        }
    }

    /// 每拍预热 2 张卡的合成缓存，全部完成后结束 loading
    pub(in crate::reactor_app) fn warm_card_cache(&mut self, context: &ComponentContext<Self>) {
        if !self.cards_loading {
            return;
        }
        // 每拍预热 2 张卡的合成缓存，避免一次性解码全部卡图卡住 UI
        let len = self.cards.cards.len();
        let end = (self.cards_warm + 2).min(len);
        for i in self.cards_warm..end {
            let path = self.cards.cards[i].path.clone();
            let _ = self.compose_card_image(&path, self.dpi_scale);
        }
        self.cards_warm = end;
        if end >= len {
            self.cards_loading = false;
        } else {
            context.spawn_background(|_| {
                std::thread::sleep(Duration::from_millis(1));
                Message::CardsLoadTick
            });
        }
    }

    /// 新建 aime 车卡成功：重扫卡片目录（新卡 access code 不再单独提示，状态由卡片列表反映）
    pub(in crate::reactor_app) fn reload_aime_cards(&mut self, _code: String) {
        let mut scan = match self.aime_device_dir() {
            Some(d) => cards::scan_aime_cards_in(&d),
            None => CardScan::default(),
        };
        // 优先用本游戏记录的卡片，未配置时回退旧全局 config.ini [card] cardfile
        scan.active = self
            .cards_game
            .and_then(|gi| self.library.games.get(gi))
            .and_then(|rec| rec.card.clone())
            .or_else(|| self.store.load_card());
        self.cards = scan;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 内置底图必须可解码，且实际尺寸与 CARD_BASE_W/H 常量一致
    /// （替换 src/assets/id5_base.png 后若忘了改常量，此测试会失败）
    #[test]
    fn builtin_card_base_matches_constants() {
        let img = image::load_from_memory(CARD_BASE_PNG).expect("内置底图应能解码");
        assert_eq!(img.width(), CARD_BASE_W as u32, "底图宽度与常量不符");
        assert_eq!(img.height(), CARD_BASE_H as u32, "底图高度与常量不符");
    }

    /// ID8 内置底图必须可解码，且实际尺寸与 ID8_BASE_W/H 常量一致
    /// （替换 src/assets/id8_base.png 后若忘了改常量，此测试会失败）
    #[test]
    fn id8_card_base_matches_constants() {
        let img = image::load_from_memory(ID8_BASE_PNG).expect("ID8 底图应能解码");
        assert_eq!(img.width(), ID8_BASE_W as u32, "ID8 底图宽度与常量不符");
        assert_eq!(img.height(), ID8_BASE_H as u32, "ID8 底图高度与常量不符");
    }

    /// ID4 内置底图必须可解码，且实际尺寸与 ID4_BASE_W/H 常量一致
    /// （替换 src/assets/id4_base.png 后若忘了改常量，此测试会失败）
    #[test]
    fn id4_card_base_matches_constants() {
        let img = image::load_from_memory(ID4_BASE_PNG).expect("ID4 底图应能解码");
        assert_eq!(img.width(), ID4_BASE_W as u32, "ID4 底图宽度与常量不符");
        assert_eq!(img.height(), ID4_BASE_H as u32, "ID4 底图高度与常量不符");
    }

    /// ID6 内置底图必须可解码，且实际尺寸与 ID6_BASE_W/H 常量一致
    /// （替换 src/assets/id6_base.png 后若忘了改常量，此测试会失败）
    #[test]
    fn id6_card_base_matches_constants() {
        let img = image::load_from_memory(ID6_BASE_PNG).expect("ID6 底图应能解码");
        assert_eq!(img.width(), ID6_BASE_W as u32, "ID6 底图宽度与常量不符");
        assert_eq!(img.height(), ID6_BASE_H as u32, "ID6 底图高度与常量不符");
    }

    /// games_db.json 里引用的封面必须全部登记在 COVER_ASSETS，否则游戏行退回占位块
    /// （历史上 SBUU / SBVW 引用的 logo_D6.png 漏登记，游戏图标显示不出来）
    #[test]
    fn every_db_cover_is_embedded() {
        for name in crate::db::cover_images() {
            assert!(
                Launcher::builtin_cover_bytes(&name).is_some(),
                "games_db.json 引用了封面 {name}，但 COVER_ASSETS 未登记该图片"
            );
        }
    }

    /// 车卡按原尺寸居中叠加，并额外下移 CARD_CAR_OFFSET_Y
    #[test]
    fn car_is_centered_with_offset() {
        let base = image::RgbaImage::from_pixel(
            CARD_BASE_W as u32,
            CARD_BASE_H as u32,
            image::Rgba([255, 0, 0, 255]),
        );
        let car = image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 255, 255]));
        let out = Launcher::overlay_car(base, &car, 1.0, CARD_CAR_OFFSET_X, CARD_CAR_OFFSET_Y);

        let x = (CARD_BASE_W as i64 - 100) / 2;
        let y = (CARD_BASE_H as i64 - 100) / 2 + CARD_CAR_OFFSET_Y;
        // 车卡左上角应为车卡颜色
        assert_eq!(
            out.get_pixel(x as u32, y as u32),
            &image::Rgba([0, 0, 255, 255])
        );
        // 未偏移位置（上方 4px）应仍为底图颜色
        assert_eq!(
            out.get_pixel(x as u32, (y - CARD_CAR_OFFSET_Y) as u32),
            &image::Rgba([255, 0, 0, 255])
        );
    }

    /// 车卡比底图高时（如 512x512 卡图叠到 665x420 底图）应安全裁剪而不 panic
    #[test]
    fn car_taller_than_base_is_clipped_safely() {
        let base = image::RgbaImage::from_pixel(
            CARD_BASE_W as u32,
            CARD_BASE_H as u32,
            image::Rgba([255, 0, 0, 255]),
        );
        let car = image::RgbaImage::from_pixel(512, 512, image::Rgba([0, 0, 255, 255]));
        let out = Launcher::overlay_car(base, &car, 1.0, CARD_CAR_OFFSET_X, CARD_CAR_OFFSET_Y);

        // 画布尺寸不变，中心被车卡覆盖
        assert_eq!(out.width(), CARD_BASE_W as u32);
        assert_eq!(out.height(), CARD_BASE_H as u32);
        assert_eq!(
            out.get_pixel(CARD_BASE_W as u32 / 2, CARD_BASE_H as u32 / 2),
            &image::Rgba([0, 0, 255, 255])
        );
    }
}
