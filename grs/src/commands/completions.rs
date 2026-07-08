//! `grs completions <shell>` — generate a shell completion script.
//! `grs man` — generate a man page (roff).
//!
//! Usage: `grs completions bash > /etc/bash_completion.d/grs`
//!        `grs completions zsh > "${fpath[1]}/_grs"`
//!        `grs man > man/man1/grs.1`

use crate::command_error::CommandError;
use crate::commands::Args;
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;

#[derive(clap::Args, Clone, Debug)]
pub struct CompletionsArgs {
    /// The shell to generate completions for.
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn cmd_completions(args: &CompletionsArgs) -> Result<(), CommandError> {
    let mut cmd = Args::command();
    let bin = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, bin, &mut io::stdout());
    Ok(())
}

pub fn cmd_man() -> Result<(), CommandError> {
    let cmd = Args::command();
    let man = clap_mangen::Man::new(cmd);
    let mut out = io::stdout().lock();
    man.render(&mut out)
        .map_err(|e| CommandError::user_error(format!("render man: {e}")))?;
    Ok(())
}
