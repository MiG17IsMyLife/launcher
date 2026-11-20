use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use windows_reactor::*;

use crate::db;
use crate::detect::{self, IdentifyResult};
use crate::icf;
use rust_i18n::t;
use crate::storage::GameRecord;

use crate::reactor_app::Launcher;
use crate::reactor_app::message::{CompanionKind, Dialog, IdentifyPayload, IcfPayload, Message, RowAction};
use crate::reactor_app::state::PendingAdd;
use crate::reactor_app::widgets::{make_id, now_secs, record_db_match, fallback_cover, row_chip, same_path, separator, settings_row, settings_row_sub, settings_toggle, spawn_anim_tick};

pub(in crate::reactor_app) const ROW_COVER_W: f64 = 104.0;
pub(in crate::reactor_app) const ROW_COVER_H: f64 = 68.0;

pub(in crate::reactor_app) const GAME_REGIONS: &[&str] = &["JPN", "EXP", "CHN"];

fn cover_file_exists(path: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    match guard.get(path) {
        Some(ok) => *ok,
        None => {
            let ok = Path::new(path).is_file();
            guard.insert(path.to_string(), ok);
            ok
        }
    }
}

/// Sort by series while keeping the original library index for every row action.
fn game_series_order(game: &GameRecord) -> (u8, u8) {
    let identified = db::lookup_record(game);
    let id = identified
        .and_then(|matched| matched.game_id.as_deref())
        .or(game.game_id.as_deref());
    let series = match id {
        Some("SBML") => 0,                          // D4 and D4 kai
        Some("SBQZ" | "SBTS") => 1,                // D5
        Some("SBUU" | "SBVW" | "SBWV") => 2,       // D6
        Some("SBYD" | "SBYY") => 3,                // D7
        Some("SBZZ" | "SDAL") => 4,                // D8
        Some("SDDF" | "SDER") => 5,                // D Zero
        Some("SDGT") => 6,                          // D The Arcade
        _ => 7,
    };
    // D4 and D4 kai share SBML; prefer the identified product, falling back to
    // the standard saved name for older records without a recognized CRC.
    let product_name = identified.map(|matched| matched.name.as_str()).unwrap_or(&game.name);
    let variant = u8::from(series == 0 && product_name.eq_ignore_ascii_case("InitialD Arcade Stage 4 kai JPN"));
    (series, variant)
}

fn ordered_games(games: &[GameRecord]) -> Vec<(usize, &GameRecord)> {
    let mut rows: Vec<_> = games.iter().enumerate().collect();
    // Stable sorting preserves the existing order within each series.
    rows.sort_by_key(|(_, game)| game_series_order(game));
    rows
}

impl Launcher {

    pub(in crate::reactor_app) fn games_page(&self, context: &mut ViewContext<Self>) -> View {
        let rows: Vec<KeyedView> = ordered_games(&self.library.games)
            .into_iter()
            .map(|(i, g)| self.game_row(context, i, g))
            .collect();

        let subtitle = t!("page_games.game_count", games = self.library.games.len()).to_string();

        let games_view: View = if rows.is_empty() {

            StackPanel::new()
                .spacing(8.0)
                .margin(Thickness::new(0.0, 48.0, 0.0, 0.0))
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text(t!("page_games.no_games_added_yet"))
                        .font_size(20.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new()
                        .text(t!("page_games.click_add_game_at_top"))
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

                            StackPanel::new()
                                .spacing(4.0)
                                .children((
                                    TextBlock::new()
                                        .text(t!("nav.games"))
                                        .font_size(26.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new().text(subtitle).font_size(12.0).opacity(0.6),
                                )),

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

    fn game_row(
        &self,
        context: &mut ViewContext<Self>,
        index: usize,
        game: &GameRecord,
    ) -> KeyedView {

        let cover: View = {
            let mut rendered: Option<View> = None;
            if let Some(path) = &game.image {
                if cover_file_exists(path) {
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
            rendered.unwrap_or_else(|| fallback_cover(ROW_COVER_W, ROW_COVER_H))
        };

        let mut parts: Vec<String> = Vec::new();
        parts.push(game.version.as_deref().unwrap_or(&t!("page_games.unidentified")).to_string());
        if let Some(id) = record_db_match(game).and_then(|m| m.game_id) {
            parts.push(id);
        }
        if game.serverbox.is_some() {
            parts.push(t!("page_games.serverbox_configured").to_string());
        }
        let sub = parts.join(" · ");

        let base_fill = || -> Brush { ThemeBrush::CardBackground.into() };

        let hover_fill = || -> Brush { ThemeBrush::SolidBackground.into() };

        let dropping = self.drop_index == Some(index);
        let drop_chevron = FontIcon::new().glyph(if dropping {
            "\u{E70D}"
        } else {
            "\u{E76C}"
        });

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

        let row: View = if dropping {
            let eased = {
                let t = self.drop_anim;
                t * (2.0 - t)
            };

            let file_name = |p: &str| -> String {
                Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string())
            };

            let sb_supported = record_db_match(game).and_then(|m| m.sb_crc).is_some();
            let features = crate::games::family_of(game).ui_features();

            let game_running_here = self.game_running
                && self.game_runtime.as_ref().map(|r| r.index) == Some(index);
            let sb_running_here = self
                .serverbox_runtime
                .as_ref()
                .map(|r| r.index)
                == Some(index);

            let cards_desc = match game.card.clone().or_else(|| self.store.load_card()) {
                Some(c) => {
                    let extension = features.card_extension;
                    let suffix = format!(".{extension}");
                    let file = if c.ends_with(&suffix) { c } else { format!("{c}{suffix}") };
                    t!("page_games.current_card", file = file).to_string()
                }
                None => t!("page_games.not_selected").to_string(),
            };

            let mut rows: Vec<KeyedView> = vec![
                KeyedView::new(
                    "drop-launch",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Play).into(),
                        &t!("common.launch_game"),

                        if game_running_here {
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .on_click(context.message(Message::LaunchStop(index)))
                                .content(t!("common.close").to_string())
                                .into()
                        } else if !features.test_mode {
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::Launch,
                                }))
                                .content(t!("page_games.launch").to_string())
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

                                        .content(t!("page_games.test_mode").to_string()),
                                    Button::new()
                                        .style(ButtonStyle::Accent)
                                        .on_click(context.message(Message::RowMenu {
                                            index,
                                            action: RowAction::Launch,
                                        }))
                                        .content(t!("page_games.launch").to_string()),
                                ))
                                .into()
                        },
                    ),
                ),
            ];

            if features.game_settings {
                rows.push(KeyedView::new("drop-sep-settings", separator()));
                rows.push(KeyedView::new(
                    "drop-settings",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::Setting).into(),
                        &t!("page_games.game_settings"),
                        Button::new()
                            .on_click(context.message(Message::RowMenu {
                                index,
                                action: RowAction::GameSettings,
                            }))
                            .content(t!("page_games.enter").to_string())
                            .into(),
                    ),
                ));
            }

            if features.cards {
                rows.push(KeyedView::new("drop-sep-cards", separator()));
                rows.push(KeyedView::new(
                    "drop-cards",
                    settings_row_sub(
                        SymbolIcon::new().symbol(Symbol::List).into(),
                        &t!("common.card_select"),
                        Some(cards_desc),
                        Button::new()
                            .on_click(context.message(Message::RowMenu {
                                index,
                                action: RowAction::Cards,
                            }))
                            .content(t!("page_games.enter").to_string()),
                    ),
                ));
            }

            if features.region {
                let region_items: Vec<String> = GAME_REGIONS.iter().map(|v| v.to_string()).collect();
                let region_idx = GAME_REGIONS
                    .iter()
                    .position(|v| Some(*v) == game.region.as_deref());
                rows.push(KeyedView::new("drop-sep-region", separator()));
                rows.push(KeyedView::new(
                    "drop-region",
                    settings_row(
                        SymbolIcon::new().symbol(Symbol::World).into(),
                        "Region",
                        ComboBox::new()
                            .width(180.0)
                            .items_source(region_items)
                            .selected_index(region_idx)
                            .placeholder_text(t!("page_games.select_region"))
                            .on_selection_changed(
                                context.callback(move |i| Message::GameRegionSelected(index, i)),
                            )
                            .vertical_alignment(VerticalAlignment::Center)
                            .into(),
                    ),
                ));
            }

                if features.graphics_patch {
                    rows.insert(1, KeyedView::new("drop-sep-gfx", separator()));
                    rows.insert(
                        2,
                        KeyedView::new(
                            "drop-gfx",
                            settings_row(
                            SymbolIcon::new().symbol(Symbol::Repair).into(),
                            &t!("page_games.graphics_patch"),
                            settings_toggle(
                                game.gfx_patch.unwrap_or(true),
                                true,
                                context.callback(move |on: bool| Message::GfxPatchToggled(index, on)),
                            ),
                        ),
                    ),
                );
                }

            if sb_supported {
                let sb_desc = game
                    .serverbox
                    .as_deref()
                    .map(|p| t!("page_games.configured", file_name = file_name(p)).to_string())
                    .unwrap_or_else(|| {
                        t!("page_games.serverbox_not_configured").to_string()
                    });
                let sb_control: View = if sb_running_here {

                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.message(Message::ServerBoxStop(index)))
                        .content(t!("common.close").to_string())
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
                                .content(t!("page_games.launch").to_string()),
                            Button::new()
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::AddServerBox,
                                }))
                                .content(t!("page_games.replace").to_string()),
                            Button::new()
                                .on_click(context.message(Message::RowMenu {
                                    index,
                                    action: RowAction::RemoveServerBox,
                                }))
                                .content(t!("common.remove").to_string()),
                        ))
                        .into()
                } else {
                    Button::new()
                        .on_click(context.message(Message::RowMenu {
                            index,
                            action: RowAction::AddServerBox,
                        }))
                        .content(t!("common.add").to_string())
                };
                rows.push(KeyedView::new("drop-sep-sb", separator()));
                rows.push(KeyedView::new(
                    "drop-serverbox",

                    settings_row_sub(
                        SymbolIcon::new().symbol(Symbol::Manage).into(),
                        "ServerBox",
                        Some(sb_desc),
                        sb_control,
                    ),
                ));
            }

            if features.companion_executables {
                let companion_row = |key: &str,
                                     title: String,
                                     configured: Option<&String>,
                                     add: RowAction,
                                     remove: RowAction|
                 -> (KeyedView, KeyedView) {
                    let desc = configured
                        .as_deref()
                        .map(|p| t!("page_games.saved", file_name = file_name(p)).to_string())
                        .unwrap_or_else(|| {
                            t!("page_games.file_not_configured").to_string()
                        });
                    let control: View = if configured.is_some() {
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .on_click(context.message(Message::RowMenu { index, action: add }))
                                    .content(t!("page_games.replace").to_string()),
                                Button::new()
                                    .on_click(context.message(Message::RowMenu { index, action: remove }))
                                    .content(t!("common.remove").to_string()),
                            ))
                            .into()
                    } else {
                        Button::new()
                            .on_click(context.message(Message::RowMenu { index, action: add }))
                            .content(t!("common.add").to_string())
                    };

                    let control: View = StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text(desc)
                                .font_size(12.0)
                                .opacity(0.7)
                                .vertical_alignment(VerticalAlignment::Center),
                            control,
                        ))
                        .into();
                    (
                        KeyedView::new(format!("drop-sep-{key}"), separator()),
                        KeyedView::new(
                            format!("drop-{key}"),
                            settings_row(
                                SymbolIcon::new().symbol(Symbol::Manage).into(),
                                &title,
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
                    t!("page_games.game_executable").to_string(),
                    game.game_exe.as_ref(),
                    RowAction::AddGameExe,
                    RowAction::RemoveGameExe,
                );
                rows.push(sep2);
                rows.push(row2);
            }

            rows.push(KeyedView::new("drop-sep-remove", separator()));
            rows.push(KeyedView::new(
                "drop-remove",
                settings_row(
                    FontIcon::new().glyph("\u{E74D}").into(),
                    &t!("page_games.remove_game"),
                    Button::new()
                        .on_click(context.message(Message::GameRemoveClicked(index)))
                        .content(t!("common.remove").to_string()),
                ),
            ));

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

        KeyedView::new(format!("game-{}", game.id), row)
    }

    pub(in crate::reactor_app) fn handle_file_picked(&mut self, path: PathBuf, context: &ComponentContext<Self>) {
        if self.library.games.iter().any(|g| same_path(&g.path, &path)) {
            self.dialog = Dialog::Error {
                message: t!("page_games.this_game_is_already_in").to_string(),
            };
            return;
        }

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
                            None => Err(t!("page_games.no_library_entry_found_for", id = info.id, version = info.version).to_string()),
                        }
                    });
                Message::IcfIdentified(Box::new(IcfPayload { path, result }))
            } else {
                let result = detect::identify(&path).map_err(|e| e.to_string());
                Message::Identified(Box::new(IdentifyPayload { path, result }))
            }
        });
    }

    pub(in crate::reactor_app) fn handle_identified(&mut self, payload: IdentifyPayload) {
        let IdentifyPayload { path, result } = payload;
        match result {
            Err(e) => self.dialog = Dialog::Error { message: t!("page_games.identification_failed", error = e).to_string() },
            Ok(ir) => {
                if let Some(d) = &ir.db {

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
                        reader: None,
                        amdaemon: None,
                        game_exe: None,
            gfx_patch: None,
                        test_mode: None,
                        card: None,
                        region: None,
                        play_count: 0,
                        last_played: None,
                        added_at: now_secs(),
                    });
                    self.store.save_library(&self.library);
                } else {

                    let default = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.pending = Some(PendingAdd { path, ir });
                    self.dialog = Dialog::Naming { name: default };
                }
            }
        }
    }

    pub(in crate::reactor_app) fn handle_icf_identified(&mut self, payload: IcfPayload) {
        let IcfPayload { path, result } = payload;
        match result {
            Err(e) => self.dialog = Dialog::Error { message: t!("page_games.identification_failed", error = e).to_string() },
            Ok((_info, m)) => {

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
                    region: None,
                    play_count: 0,
                    last_played: None,
                    added_at: now_secs(),
                });
                self.store.save_library(&self.library);
            }
        }
    }

    pub(in crate::reactor_app) fn toggle_row_drop(&mut self, index: usize, context: &ComponentContext<Self>) {

        if self.drop_index == Some(index) && self.drop_open {
            self.drop_open = false;
        } else {
            self.drop_index = Some(index);
            self.drop_open = true;
            self.drop_anim = 0.0;
        }

        self.drop_anim_gen += 1;
        let (epoch, step) = (self.drop_anim_gen, 0.016 / 0.2);
        spawn_anim_tick(context, move || Message::GameRowDropTick(epoch, step));
    }

    pub(in crate::reactor_app) fn tick_row_drop(&mut self, epoch: u32, step: f64, context: &ComponentContext<Self>) {

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

                self.drop_index = None;
            }
            self.drop_anim <= 0.0
        };
        if !done {
            spawn_anim_tick(context, move || Message::GameRowDropTick(epoch, step));
        }
    }

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
            RowAction::GameSettings => {
                self.dialog = Dialog::None;
                self.nav_tag = "wmmt3".to_string();
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

    pub(in crate::reactor_app) fn handle_companion_picked(
        &mut self,
        kind: CompanionKind,
        index: usize,
        path: PathBuf,
        context: &ComponentContext<Self>,
    ) {

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

                if let (Some(gd), Some(sd)) = (Path::new(&game.path).parent(), path.parent()) {
                    if same_path(&gd.to_string_lossy(), sd) {
                        self.dialog = Dialog::Error {
                            message: t!("page_games.serverbox_cannot_be_in_same")
                                .to_string(),
                        };
                        return;
                    }
                }

                match record_db_match(game).and_then(|m| m.sb_crc) {
                    None => {
                        self.dialog = Dialog::Error {
                            message: t!("page_games.currently_matched_version_of_this").to_string()
                        }
                    }
                    Some(expected) => match result {
                        Err(e) => self.dialog = Dialog::Error {
                            message: t!("page_games.serverbox_identification_failed", error = e).to_string(),
                        },
                        Ok(ir) if ir.crc == expected => {
                            self.library.games[index].serverbox =
                                Some(path.display().to_string());
                            self.store.save_library(&self.library);
                        }
                        Ok(ir) => {
                            self.dialog = Dialog::Error {
                                message: t!("page_games.selected_file_is_not_this", crc = ir.crc : {:08x}, expected = expected : {:08x}).to_string()
                            }
                        }
                    },
                }
            }

            CompanionKind::Amdaemon | CompanionKind::GameExe => {}
        }
    }

}

#[cfg(test)]
mod order_tests {
    use super::*;
    use crate::games::test_input::record;
    use crate::storage::Loader;

    #[test]
    fn sorts_series_but_keeps_original_indices_and_order_within_series() {
        let games = vec![
            record(Loader::Segatools, Some("SDGT")),
            record(Loader::Mice, Some("SBYY")),
            record(Loader::Kitten, Some("SBQZ")),
            record(Loader::Mice, Some("SBZZ")),
            record(Loader::Kitten, Some("SBML")),
            record(Loader::Segatools, Some("SDDF")),
            record(Loader::Mice, Some("SBUU")),
            record(Loader::Kitten, Some("SBTS")),
            record(Loader::Mice, None),
        ];
        let indices: Vec<_> = ordered_games(&games).iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, [4, 2, 7, 6, 1, 3, 5, 0, 8]);
    }

    #[test]
    fn uses_identified_build_for_older_records_without_a_game_id() {
        let mut game = record(Loader::Kitten, None);
        game.crc = Some("0xda04e5e0".into());
        assert_eq!(game_series_order(&game), (1, 0));
    }
    #[test]
    fn d4_precedes_kai_even_when_kai_was_added_first() {
        let mut kai = record(Loader::Kitten, Some("SBML"));
        kai.crc = Some("0x6cb19701".into());
        let mut d4 = record(Loader::Kitten, Some("SBML"));
        d4.crc = Some("0x361d1cbe".into());
        let games = vec![kai, d4];
        let indices: Vec<_> = ordered_games(&games).iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, [1, 0]);
    }

    #[test]
    fn saved_d4_kai_name_is_used_when_crc_is_missing() {
        let mut kai = record(Loader::Kitten, Some("SBML"));
        kai.name = "InitialD Arcade Stage 4 kai JPN".into();
        assert_eq!(game_series_order(&kai), (0, 1));
    }

}
