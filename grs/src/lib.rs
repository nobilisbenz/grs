//! grs — the CLI/TUI binary crate, a thin orchestration layer over `grs-lib`.

pub mod cli_util;
pub mod command_error;
pub mod commands;
pub mod tui;
pub mod ui;

use std::path::PathBuf;

/// Global options shared by every subcommand (mirrors jj's `GlobalArgs`).
#[derive(clap::Args, Clone, Debug)]
#[command(next_help_heading = "Global Options")]
pub struct GlobalArgs {
    /// Run as if started in this directory.
    #[arg(long, short = 'C', global = true, value_hint = clap::ValueHint::DirPath)]
    pub repo: Option<PathBuf>,

    /// Suppress status/warning messages (stderr).
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Color output: always | never | auto.
    #[arg(long, global = true, value_enum)]
    pub color: Option<crate::ui::ColorChoice>,

    /// Enable debug logging.
    #[arg(long, global = true)]
    pub verbose: bool,
}
