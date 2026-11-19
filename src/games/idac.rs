//! Initial D The Arcade (ALLS), run by segatools with `idachook.dll`.
//!
//! Launching one: write `segatools.ini` next to amdaemon.exe (with the hook's
//! `config_hook.json` and the season's spike file), then start amdaemon and
//! `GameProject-Win64-Shipping.exe`. It has no ServerBox.

use std::process::Child;

use rust_i18n::t;

use super::{GameFamily, LaunchInput, LaunchProblem, Prepared, Started, alls};

pub struct Idac;

impl GameFamily for Idac {
    fn has_serverbox(&self) -> bool {
        false
    }

    fn network_configured(&self, input: &LaunchInput) -> bool {
        alls::network_configured(input)
    }

    fn check(&self, input: &LaunchInput) -> Result<(), LaunchProblem> {
        alls::check(input)
    }

    fn prepare(&self, input: &LaunchInput) -> Result<Prepared, LaunchProblem> {
        alls::prepare(input)
    }

    fn start(&self, input: &LaunchInput, prepared: Prepared) -> Result<Started, LaunchProblem> {
        alls::start(input, prepared, None)
    }

    fn launch_serverbox(&self, _input: &LaunchInput) -> Result<Child, String> {
        Err(t!("common.segatools_has_no_serverbox").to_string())
    }
}
