//! `grs replay <session>` — launch the TUI replay timelapse.

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::store::RepoStore;

#[derive(clap::Args, Clone, Debug)]
pub struct ReplayArgs {
    /// Session id (ULID; prefix-match allowed). Defaults to the current session.
    pub session: Option<String>,
}

pub async fn cmd_replay(
    _ui: &mut Ui,
    command: &CommandHelper,
    args: &ReplayArgs,
) -> Result<(), CommandError> {
    let store: RepoStore = command.store_or_init().map_err(CommandError::from)?;
    let session = match &args.session {
        Some(s) => store.sessions().resolve_prefix(s).map_err(CommandError::from)?,
        None => store.current_session().map_err(CommandError::from)?,
    };
    crate::tui::run_replay(store, session)
}
