//! `grs new` — finalize the current open session and start a new one.
//!
//! `HEAD` is moved to the new session; if a watcher is running (e.g. the TUI),
//! the caller is responsible for tearing it down and respawning it against
//! the new `RepoStore` (see `tui::run_tui`).

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::store::RepoStore;
use grs_lib::util::time::now_ms;

#[derive(clap::Args, Clone, Debug)]
pub struct NewArgs {}

pub async fn cmd_new(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &NewArgs,
) -> Result<(), CommandError> {
    let store: RepoStore = command.store_or_init().map_err(CommandError::from)?;
    let new_session = store
        .rotate_open_session(now_ms())
        .map_err(CommandError::from)?;
    let id_short: String = new_session.id.as_str().chars().take(10).collect();
    ui.say(&format!("new session {id_short}"))
        .map_err(CommandError::internal_error)?;
    Ok(())
}
