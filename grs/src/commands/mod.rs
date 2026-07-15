//! Top-level clap command enum + dispatch.
//!
//! Bare `grs` opens the TUI shell. Subcommands:
//!
//! - `grs session list`        list all sessions in the project
//! - `grs session view <name>` open a read-only TUI of an ended session
//! - `grs session rename`      rename a session
//! - `grs session rm`          delete a closed session's folder
//! - `grs new <name>`          finalize current open session, start a new one
//! - `grs watch`               run the file watcher headless (no TUI)
//! - `grs completions <shell>` generate shell completions
//! - `grs man`                 generate man page
//! - `grs config`              view/edit layered config

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

pub mod completions;
pub mod config;
pub mod new;
pub mod session;
pub mod watch;

#[derive(clap::Parser, Clone, Debug)]
#[command(
    name = "grs",
    version,
    about = "grs — watch your project grow, one snap at a time",
    long_about = None,
)]
pub struct Args {
    #[command(flatten)]
    pub global_args: super::GlobalArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Command {
    /// Finalize the current open session and start a new one.
    New(new::NewArgs),
    /// Run the file watcher headless (no TUI). For non-interactive
    /// callers that want a long-lived capture process on a project.
    Watch(watch::WatchArgs),
    /// Manage sessions (list, view, rename, remove).
    #[command(subcommand)]
    Session(session::SessionCmd),
    /// Generate a shell completion script.
    Completions(completions::CompletionsArgs),
    /// Generate a man page (roff) for the `grs` command.
    Man,
    /// View and edit the layered grs config.
    Config(config::ConfigArgs),
}

pub async fn run_command(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &Args,
) -> Result<(), CommandError> {
    let cmd = match &args.command {
        Some(c) => c,
        None => {
            // No subcommand: open the TUI shell.
            let store = command.store_or_init().map_err(CommandError::from)?;
            return crate::tui::run_tui(store, !command.global().read_only);
        }
    };
    match cmd {
        Command::New(a) => new::cmd_new(ui, command, a).await,
        Command::Watch(a) => watch::cmd_watch(ui, command, a).await,
        Command::Session(s) => session::run(ui, command, s).await,
        Command::Completions(a) => completions::cmd_completions(a),
        Command::Config(a) => config::cmd_config(ui, command, a).await,
        Command::Man => completions::cmd_man(),
    }
}
