//! Top-level clap command enum + explicit `match` dispatch.
//! Every leaf command has the universal signature:
//! `async fn cmd_x(ui, command, args) -> Result<(), CommandError>`.
//!
//! `grs` is a minimal TUI replay tool. Running it with no subcommand opens
//! the replay for the current session. Capture runs only while the TUI is
//! running in a terminal.

use crate::command_error::CommandError;
use crate::cli_util::CommandHelper;
use crate::ui::Ui;

pub mod completions;
pub mod config;
pub mod new;

#[derive(clap::Parser, Clone, Debug)]
#[command(
    name = "grs",
    version,
    about = "Minimal replay timelapse for file edits",
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
    /// Generate a shell completion script (bash, zsh, fish, elvish, powershell).
    Completions(completions::CompletionsArgs),
    /// Generate a man page (roff) for the `grs` command.
    Man,
    /// View and edit the layered grs config (user / repo / effective).
    Config(config::ConfigArgs),
}

pub async fn run_command(ui: &mut Ui, command: &CommandHelper, args: &Args) -> Result<(), CommandError> {
    let cmd = match &args.command {
        Some(c) => c,
        None => {
            // No subcommand: open the TUI shell at the session list.
            let store = command.store_or_init().map_err(CommandError::from)?;
            return crate::tui::run_tui(store);
        }
    };
    match cmd {
        Command::New(a) => new::cmd_new(ui, command, a).await,
        Command::Completions(a) => completions::cmd_completions(a),
        Command::Config(a) => config::cmd_config(ui, command, a).await,
        Command::Man => completions::cmd_man(),
    }
}
