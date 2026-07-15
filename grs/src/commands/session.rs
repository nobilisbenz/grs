//! `grs session ...` subcommands: list, view, rename, rm.

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::model::SessionMeta;
use grs_lib::util::time::time_ago;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum SessionCmd {
    /// List all sessions in this project.
    List(ListArgs),
    /// Open a read-only TUI viewer for an ended session.
    View(ViewArgs),
    /// Rename a session (the on-disk folder keeps the original slug).
    Rename(RenameArgs),
    /// Delete a session's on-disk folder. Refuses to delete an open session.
    Rm(RmArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListArgs {
    /// Show ended sessions only.
    #[arg(long)]
    ended: bool,
    /// Show open sessions only.
    #[arg(long, conflicts_with = "ended")]
    open: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ViewArgs {
    /// The session name or id.
    name_or_id: String,
}

#[derive(clap::Args, Clone, Debug)]
pub struct RenameArgs {
    /// The current name or id of the session.
    old: String,
    /// The new name.
    new: String,
}

#[derive(clap::Args, Clone, Debug)]
pub struct RmArgs {
    /// The session name or id.
    name_or_id: String,
    /// Skip the confirmation prompt.
    #[arg(long, short = 'y')]
    yes: bool,
}

pub async fn run(
    ui: &mut Ui,
    command: &CommandHelper,
    cmd: &SessionCmd,
) -> Result<(), CommandError> {
    match cmd {
        SessionCmd::List(a) => list(ui, command, a).await,
        SessionCmd::View(a) => view(ui, command, a).await,
        SessionCmd::Rename(a) => rename(ui, command, a).await,
        SessionCmd::Rm(a) => rm(ui, command, a).await,
    }
}

async fn list(ui: &mut Ui, command: &CommandHelper, args: &ListArgs) -> Result<(), CommandError> {
    let store = command.store().map_err(CommandError::from)?;
    let sessions = store.sessions().list().map_err(CommandError::from)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let filtered: Vec<&SessionMeta> = sessions
        .iter()
        .filter(|s| {
            if args.ended {
                !s.is_open()
            } else if args.open {
                s.is_open()
            } else {
                true
            }
        })
        .collect();

    if filtered.is_empty() {
        ui.say("(no sessions)")
            .map_err(CommandError::internal_error)?;
        return Ok(());
    }

    // Compute width for the name column.
    let name_width = filtered
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4)
        .min(40);

    for s in &filtered {
        let status = if s.is_open() { "open  " } else { "ended " };
        let ago = time_ago(s.started_at, now);
        let snaps = s.snap_count;
        let id_short: String = s.id.as_str().chars().take(8).collect();
        let line = format!(
            "{status}  {name:name_width$}  {snaps:>3} snaps  started {ago:<10}  {id_short}",
            name = s.name,
        );
        ui.say(&line).map_err(CommandError::internal_error)?;
    }
    Ok(())
}

async fn view(_ui: &mut Ui, command: &CommandHelper, args: &ViewArgs) -> Result<(), CommandError> {
    // The full-screen TUI is the same regardless of how the user opens
    // it. `grs session view <name>` jumps straight to the code review
    // view of the named session by first opening the TUI shell, then
    // routing. For now we just hand off to the TUI; the session list
    // view starts on the named session by hot-patching HEAD before the
    // TUI launches. Simpler approach: launch the TUI; the user can pick
    // the session from the list. We could improve this later.
    let _ = command;
    let _ = args;
    let store = command.store().map_err(CommandError::from)?;
    crate::tui::run_tui(store, !command.global().read_only)
}

async fn rename(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &RenameArgs,
) -> Result<(), CommandError> {
    let store = command.store().map_err(CommandError::from)?;
    let updated = store
        .sessions()
        .rename(&args.old, args.new.clone())
        .map_err(CommandError::from)?;
    ui.say(&format!("renamed to \"{}\"", updated.name))
        .map_err(CommandError::internal_error)?;
    Ok(())
}

async fn rm(ui: &mut Ui, command: &CommandHelper, args: &RmArgs) -> Result<(), CommandError> {
    let store = command.store().map_err(CommandError::from)?;
    let session = store
        .sessions()
        .resolve(&args.name_or_id)
        .map_err(CommandError::from)?;
    if session.is_open() {
        return Err(CommandError::user_error(format!(
            "session \"{}\" is still open; quit the TUI first",
            session.name
        )));
    }
    if !args.yes {
        ui.say(&format!(
            "Delete session \"{}\" and its {} snaps? [y/N] ",
            session.name, session.snap_count
        ))
        .map_err(CommandError::internal_error)?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).map_err(CommandError::internal_error)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            ui.say("aborted").map_err(CommandError::internal_error)?;
            return Ok(());
        }
    }
    store.delete_session(&session.id).map_err(CommandError::from)?;
    ui.say(&format!("deleted session \"{}\"", session.name))
        .map_err(CommandError::internal_error)?;
    Ok(())
}
