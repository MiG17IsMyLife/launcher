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

pub(in crate::reactor_app) const CARD_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const CARD_BASE_H: f64 = 441.0;

pub(in crate::reactor_app) const CARD_W: f64 = 380.0;

pub(in crate::reactor_app) const CARD_DISPLAY_SCALE: f64 = 0.8;

pub(in crate::reactor_app) const CARD_GAP: f64 = 24.0;
pub(in crate::reactor_app) const CARD_ROW_SPACING: f64 = 16.0;

pub(in crate::reactor_app) const CARD_PAGE_MARGIN: f64 = 28.0;

pub(in crate::reactor_app) const CARD_SCROLL_RESERVE: f64 = 20.0;

pub(in crate::reactor_app) const CARD_ACTIVE_TINT_OPACITY: f64 = 0.14;

pub(in crate::reactor_app) const CARD_BASE_PNG: &[u8] = include_bytes!("../../assets/id5_base.png");

pub(in crate::reactor_app) const AIME_BASE_PNG: &[u8] = include_bytes!("../../assets/aime.png");
pub(in crate::reactor_app) const AIME_BASE_W: f64 = 682.0;
pub(in crate::reactor_app) const AIME_BASE_H: f64 = 432.0;

pub(in crate::reactor_app) const ID8_BASE_PNG: &[u8] = include_bytes!("../../assets/id8_base.png");
pub(in crate::reactor_app) const ID8_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID8_BASE_H: f64 = 440.0;

pub(in crate::reactor_app) const ID4_BASE_PNG: &[u8] = include_bytes!("../../assets/id4_base.png");
pub(in crate::reactor_app) const ID4_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID4_BASE_H: f64 = 441.0;

pub(in crate::reactor_app) const ID6_BASE_PNG: &[u8] = include_bytes!("../../assets/id6_base.png");
pub(in crate::reactor_app) const ID6_BASE_W: f64 = 698.0;
pub(in crate::reactor_app) const ID6_BASE_H: f64 = 440.0;

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

pub(in crate::reactor_app) const CARD_CAR_OFFSET_X: i64 = 0;
pub(in crate::reactor_app) const CARD_CAR_OFFSET_Y: i64 = 4;

impl Launcher {

    pub(in crate::reactor_app) fn cards_page(&self, context: &mut ViewContext<Self>) -> View {

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

        let aime_item_w = CARD_W * CARD_DISPLAY_SCALE;
        let aime_item_h = aime_item_w * AIME_BASE_H / AIME_BASE_W;

        let wrap_rows = |rows: Vec<KeyedView>| -> View {
            ScrollViewer::new()
                .margin(Thickness::new(0.0, 12.0, 0.0, 0.0))
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Visible)

                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)

                .content(
                    ItemsRepeater::new()
                        .items(rows),
                )
                .into()
        };

        let body: View = if self.cards_loading {

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

        let active = self.card_slide;

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

        let slide = active;
        Border::new()
            .opacity(slide)
            .margin(Thickness::new((1.0 - slide) * 80.0, 0.0, 0.0, 0.0))
            .background(ThemeBrush::CardBackground)
            .content(content)
            .into()
    }

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
                    .children([TextBlock::new()
                        .text(t!("空白车卡", "Blank card"))
                        .font_size(15.0)
                        .font_weight(FontWeight::SEMI_BOLD)]),
            )
            .into()
    }

    fn aime_base_arc() -> Arc<[u8]> {
        static AIME: OnceLock<Arc<[u8]>> = OnceLock::new();
        AIME.get_or_init(|| Arc::from(AIME_BASE_PNG.to_vec())).clone()
    }

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

                    .foreground(Brush::Solid(Color::argb(255, 0x1A, 0x1A, 0x1A)))
                    .margin(Thickness::new(14.0, 10.0, 0.0, 0.0))
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .vertical_alignment(VerticalAlignment::Top),
            ))
            .into();

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

    pub(in crate::reactor_app) fn aime_device_dir(&self) -> Option<PathBuf> {
        let gi = self.cards_game?;
        let rec = self.library.games.get(gi)?;
        let amdaemon = PathBuf::from(rec.amdaemon.as_deref()?);
        Some(amdaemon.parent()?.join("DEVICE"))
    }

    pub(in crate::reactor_app) fn builtin_cover_bytes(name: &str) -> Option<&'static [u8]> {
        COVER_ASSETS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, bytes)| *bytes)
    }

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

    fn decode_builtin_base(png: &[u8]) -> Result<image::RgbaImage, String> {
        image::load_from_memory(png)
            .map(|img| img.to_rgba8())
            .map_err(|e| tf!("解码内置底图失败：{}", "Failed to decode built-in base image: {}", e))
    }

    fn card_car_offset(&self) -> (i64, i64) {
        if self.cards_id4 {
            (-5, CARD_CAR_OFFSET_Y)
        } else {
            (CARD_CAR_OFFSET_X, CARD_CAR_OFFSET_Y)
        }
    }

    fn overlay_car(
        mut base: image::RgbaImage,
        car: &image::RgbaImage,
        dpi_scale: f64,
        offset_x: i64,
        offset_y: i64,
    ) -> image::RgbaImage {

        let scale = dpi_scale.max(0.01);
        let x = (base.width() as i64 - car.width() as i64) / 2
            + (offset_x as f64 * scale) as i64;
        let y = (base.height() as i64 - car.height() as i64) / 2
            + (offset_y as f64 * scale) as i64;
        image::imageops::overlay(&mut base, car, x, y);
        base
    }

    fn compose_card_image(
        &self,
        car_path: &Path,
        dpi_scale: f64,
    ) -> Result<(Arc<[u8]>, f64, f64), Arc<str>> {

        if let Some(cached) = self.compose_cache.borrow().get(car_path) {
            return cached.clone();
        }

        let fail = |reason: String| {
            let err: Arc<str> = reason.into();
            self.compose_cache
                .borrow_mut()
                .insert(car_path.to_path_buf(), Err(err.clone()));
            Err(err)
        };

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

    fn card_columns(&self, card_w: f64) -> usize {
        let avail = self.win_size.width - CARD_PAGE_MARGIN * 2.0 - CARD_SCROLL_RESERVE;

        if avail <= 0.0 || card_w <= 0.0 {
            return 2;
        }
        ((avail + CARD_GAP) / (card_w + CARD_GAP))
            .floor()
            .max(1.0) as usize
    }

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

    fn card_item(&self, context: &mut ViewContext<Self>, index: usize, card: &CardEntry) -> View {
        let active = self.cards.active_index() == Some(index);
        let (fallback_w, fallback_h) = self.card_fallback_size();

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

        self.compose_cache.borrow_mut().clear();
        *self.base_cache.borrow_mut() = None;

        self.cards_loading = true;
        self.cards_warm = 0;

        self.cards_aime = rec.loader == Loader::Segatools
            && matches!(rec.game_id.as_deref(), Some("SDDF") | Some("SDER") | Some("SDGT"));

        self.cards_id4 = rec.loader == Loader::Kitten
            && matches!(rec.game_id.as_deref(), Some("SBML"));

        self.cards_id6 = rec.loader == Loader::Mice
            && matches!(rec.game_id.as_deref(), Some("SBUU") | Some("SBVW") | Some("SBWV"));

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

    pub(in crate::reactor_app) fn apply_cards_scanned(&mut self, mut scan: Box<CardScan>, context: &ComponentContext<Self>) {

        scan.active = self
            .cards_game
            .and_then(|gi| self.library.games.get(gi))
            .and_then(|rec| rec.card.clone())
            .or_else(|| self.store.load_card());
        self.cards = *scan;

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

            self.cards_loading = false;
        } else {

            self.cards_loading = true;
            self.cards_warm = 0;
            context.spawn_background(|_| {
                std::thread::sleep(Duration::from_millis(30));
                Message::CardsLoadTick
            });
        }
    }

    pub(in crate::reactor_app) fn warm_card_cache(&mut self, context: &ComponentContext<Self>) {
        if !self.cards_loading {
            return;
        }

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

    pub(in crate::reactor_app) fn reload_aime_cards(&mut self, _code: String) {
        let mut scan = match self.aime_device_dir() {
            Some(d) => cards::scan_aime_cards_in(&d),
            None => CardScan::default(),
        };

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

    #[test]
    fn builtin_card_base_matches_constants() {
        let img = image::load_from_memory(CARD_BASE_PNG).expect("the built-in base image should decode");
        assert_eq!(img.width(), CARD_BASE_W as u32, "the base image width does not match the constant");
        assert_eq!(img.height(), CARD_BASE_H as u32, "the base image height does not match the constant");
    }

    #[test]
    fn id8_card_base_matches_constants() {
        let img = image::load_from_memory(ID8_BASE_PNG).expect("the ID8 base image should decode");
        assert_eq!(img.width(), ID8_BASE_W as u32, "the ID8 base image width does not match the constant");
        assert_eq!(img.height(), ID8_BASE_H as u32, "the ID8 base image height does not match the constant");
    }

    #[test]
    fn id4_card_base_matches_constants() {
        let img = image::load_from_memory(ID4_BASE_PNG).expect("the ID4 base image should decode");
        assert_eq!(img.width(), ID4_BASE_W as u32, "the ID4 base image width does not match the constant");
        assert_eq!(img.height(), ID4_BASE_H as u32, "the ID4 base image height does not match the constant");
    }

    #[test]
    fn id6_card_base_matches_constants() {
        let img = image::load_from_memory(ID6_BASE_PNG).expect("the ID6 base image should decode");
        assert_eq!(img.width(), ID6_BASE_W as u32, "the ID6 base image width does not match the constant");
        assert_eq!(img.height(), ID6_BASE_H as u32, "the ID6 base image height does not match the constant");
    }

    #[test]
    fn every_db_cover_is_embedded() {
        for name in crate::db::cover_images() {
            assert!(
                Launcher::builtin_cover_bytes(&name).is_some(),
                "games_db.json refers to the cover {name}, but COVER_ASSETS does not list that image"
            );
        }
    }

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

        assert_eq!(
            out.get_pixel(x as u32, y as u32),
            &image::Rgba([0, 0, 255, 255])
        );

        assert_eq!(
            out.get_pixel(x as u32, (y - CARD_CAR_OFFSET_Y) as u32),
            &image::Rgba([255, 0, 0, 255])
        );
    }

    #[test]
    fn car_taller_than_base_is_clipped_safely() {
        let base = image::RgbaImage::from_pixel(
            CARD_BASE_W as u32,
            CARD_BASE_H as u32,
            image::Rgba([255, 0, 0, 255]),
        );
        let car = image::RgbaImage::from_pixel(512, 512, image::Rgba([0, 0, 255, 255]));
        let out = Launcher::overlay_car(base, &car, 1.0, CARD_CAR_OFFSET_X, CARD_CAR_OFFSET_Y);

        assert_eq!(out.width(), CARD_BASE_W as u32);
        assert_eq!(out.height(), CARD_BASE_H as u32);
        assert_eq!(
            out.get_pixel(CARD_BASE_W as u32 / 2, CARD_BASE_H as u32 / 2),
            &image::Rgba([0, 0, 255, 255])
        );
    }
}
