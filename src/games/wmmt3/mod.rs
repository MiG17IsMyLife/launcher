//! Wangan Midnight Maximum Tune 3 / 3DX / 3DX+ (Namco System N2), run by kitten.
//!
//! Launching one: copy kitten's files into the game folder (next to the game's `main`),
//! write the launcher's settings into `kitten\config.ini`, then start `kitten.exe -b main`.
//!
//! KittenTools keeps every setting of these games in the `[wmmt3]` section of its
//! `config.ini`, apart from `[sysconf] log_level`. The launcher offers them on the Input
//! page's WMMT3 tab and on the WMMT3 settings page ([`settings`]), and writes only the ones
//! set there; the rest of the file stays as it is, so it can still be edited by hand.
//!
//! These games need no keychip, region or subnet, and have no card reader program, no
//! ServerBox and no test mode switch (the test button is a key). Linked play is not offered
//! through the launcher's rooms.

use std::path::{Path, PathBuf};
use std::process::Child;

use rust_i18n::t;

use super::{
    CardKind, GameFamily, LaunchInput, LaunchProblem, Prepared, Started, UiFeatures, game_dir_of,
};
use crate::ini;
use crate::loaders::{self, kitten};
use crate::storage::GameRecord;

pub mod settings;

pub use settings::Wmmt3Settings;

/// The game ids the game database gives these games (the start of Namco's game ids).
const GAME_IDS: [&str; 3] = ["WM3", "W3X", "W3P"];

/// Whether a database game id is one of these games.
pub fn is_wmmt3(game_id: Option<&str>) -> bool {
    game_id.is_some_and(|id| GAME_IDS.contains(&id))
}

/// Whether a library record is one of these games.
pub fn is_wmmt3_game(record: &GameRecord) -> bool {
    record.loader == crate::storage::Loader::Kitten && is_wmmt3(record.game_id.as_deref())
}

pub struct Wmmt3;

impl GameFamily for Wmmt3 {
    fn ui_features(&self) -> UiFeatures {
        UiFeatures {
            test_mode: false,
            graphics_patch: false,
            companion_executables: false,
            region: false,
            cards: false,
            game_settings: true,
            card_extension: "bin",
        }
    }

    fn card_kind(&self, _record: &GameRecord) -> CardKind {
        // The game row offers no card page for these games yet.
        CardKind::default()
    }

    fn has_serverbox(&self) -> bool {
        false
    }

    fn check(&self, _input: &LaunchInput) -> Result<(), LaunchProblem> {
        Ok(())
    }

    fn prepare(&self, input: &LaunchInput) -> Result<Prepared, LaunchProblem> {
        let deploy = || -> Result<PathBuf, String> {
            let kitten_exe = loaders::resolve_loader(&input.record, &input.settings, &input.loaders)?;
            let tool_dir = kitten_exe.parent().ok_or_else(|| {
                t!("launch.cannot_determine_kitten_tool_directory", loader = kitten_exe.display()).to_string()
            })?;
            let loader = kitten::install(tool_dir, Path::new(&input.record.path))?;
            write_settings(&kitten::config_path(&game_dir_of(&input.record)), input)?;
            Ok(loader)
        };
        let loader = deploy().map_err(|e| LaunchProblem::failed(t!("page_launch.game_launch_failed", error = e)))?;
        Ok(Prepared { loader, card_reader: None })
    }

    fn start(&self, input: &LaunchInput, prepared: Prepared) -> Result<Started, LaunchProblem> {
        let game = loaders::start_game(&input.record, &prepared.loader)
            .map_err(|e| LaunchProblem::failed(t!("page_launch.game_launch_failed", error = e)))?;
        Ok(Started { game, helpers: Vec::new(), kill_on_exit: Vec::new() })
    }

    fn launch_serverbox(&self, _input: &LaunchInput) -> Result<Child, String> {
        Err(t!("wmmt3.no_serverbox").to_string())
    }
}

/// The keys of kitten's `config.ini` the launcher sets for these games: the log level of
/// the System page, and every WMMT3 setting set in the launcher.
fn config_keys(input: &LaunchInput) -> Vec<(&'static str, &'static str, String)> {
    let mut keys = Vec::new();
    if let Some(level) = input.system.loglevel.as_deref().and_then(kitten::log_level_num) {
        keys.push(("sysconf", "log_level", level.to_string()));
    }
    for (key, value) in input.wmmt3.set_values() {
        keys.push((settings::SECTION, key, value.to_string()));
    }
    keys
}

/// Sets the launcher's keys in kitten's `config.ini`, keeping everything else in it.
fn write_settings(config: &Path, input: &LaunchInput) -> Result<(), String> {
    let failed = |e: std::io::Error| t!("wmmt3.failed_to_write_config", path = config.display(), error = e).to_string();
    if let Some(dir) = config.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let keys = config_keys(input);
    let entries: Vec<(&str, &str, &str)> = keys.iter().map(|(s, k, v)| (*s, *k, v.as_str())).collect();
    ini::set_values(config, &entries).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_util::test_dir::{fresh, remove, write};
    use crate::games::test_input;
    use crate::storage::Loader;

    #[test]
    fn game_ids() {
        assert!(is_wmmt3(Some("WM3")));
        assert!(is_wmmt3(Some("W3X")));
        assert!(is_wmmt3(Some("W3P")));
        assert!(!is_wmmt3(Some("SBQZ")));
        assert!(!is_wmmt3(None));
    }

    #[test]
    fn only_set_settings_are_written() {
        let mut input = test_input::input(Loader::Kitten, Some("W3P"));
        input.system.loglevel = None;
        assert!(config_keys(&input).is_empty());

        input.system.loglevel = Some("debug".into());
        input.wmmt3 = Wmmt3Settings::from_pairs(&[("free_play", "true")]);
        assert_eq!(
            config_keys(&input),
            vec![("sysconf", "log_level", "1".to_string()), ("wmmt3", "free_play", "true".to_string())]
        );
    }

    #[test]
    fn the_coin_page_does_not_decide_free_play() {
        let mut input = test_input::input(Loader::Kitten, Some("W3P"));
        input.system.loglevel = None;
        input.coin.freeplay = Some(true);
        assert!(config_keys(&input).is_empty());
    }

    #[test]
    fn writing_keeps_the_rest_of_the_file() {
        let dir = fresh("wmmt3_config");
        let config = dir.join("kitten").join("config.ini");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        write(&config, b"; The main config file\n[wmmt3]\n; Stretch\nstretch = true\nfree_play = false\n");

        let mut input = test_input::input(Loader::Kitten, Some("W3P"));
        input.wmmt3 = Wmmt3Settings::from_pairs(&[("free_play", "true")]);
        write_settings(&config, &input).unwrap();

        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("; Stretch\nstretch = true\n"), "the other keys and comments stay:\n{text}");
        assert!(text.contains("free_play = true"), "{text}");
        assert!(!text.contains("free_play = false"), "{text}");
        remove(&dir);
    }

    #[test]
    fn prepare_installs_kitten_next_to_main() {
        let root = fresh("wmmt3_prepare");
        let tools = root.join("tools").join("kitten");
        let game = root.join("game");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&game).unwrap();
        write(&tools.join("kitten.exe"), b"kitten");
        write(&game.join("main"), b"elf");

        let mut input = test_input::input(Loader::Kitten, Some("W3P"));
        input.record.path = game.join("main").to_string_lossy().into_owned();
        input.settings.kitten_path = Some(tools.join("kitten.exe").to_string_lossy().into_owned());
        input.wmmt3 = Wmmt3Settings::from_pairs(&[("input_mode", "2")]);

        let prepared = Wmmt3.prepare(&input).unwrap_or_else(|p| panic!("{}", p.message));
        assert_eq!(prepared.loader, game.join("kitten.exe"));
        assert!(game.join("kitten.exe").is_file());
        let config = std::fs::read_to_string(game.join("kitten").join("config.ini")).unwrap();
        assert!(config.contains("input_mode = 2"), "{config}");
        remove(&root);
    }

    #[test]
    fn writing_creates_the_config_folder() {
        let dir = fresh("wmmt3_new_config");
        let config = dir.join("kitten").join("config.ini");
        let mut input = test_input::input(Loader::Kitten, Some("W3P"));
        input.wmmt3 = Wmmt3Settings::from_pairs(&[("free_play", "false")]);
        write_settings(&config, &input).unwrap();
        assert!(std::fs::read_to_string(&config).unwrap().contains("free_play = false"));
        remove(&dir);
    }
}
