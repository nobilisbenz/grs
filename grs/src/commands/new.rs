//! `grs new <name>` — finalize the current open session and start a new one
//! with the given name.

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::store::RepoStore;
use grs_lib::util::time::now_ms;

#[derive(clap::Args, Clone, Debug)]
pub struct NewArgs {
    /// The name for the new session. Must be unique within the project.
    pub name: String,
}

pub async fn cmd_new(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &NewArgs,
) -> Result<(), CommandError> {
    let store: RepoStore = command.store_or_init().map_err(CommandError::from)?;
    crate::warnings::check_and_warn(store.root());
    let new_session = store
        .rotate_open_session(args.name.clone(), now_ms())
        .map_err(CommandError::from)?;
    let id_short: String = new_session.id.as_str().chars().take(10).collect();
    ui.say(&format!(
        "new session \"{}\" ({})",
        new_session.name, id_short
    ))
    .map_err(CommandError::internal_error)?;
    Ok(())
}
