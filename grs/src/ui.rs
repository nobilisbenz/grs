//! `Ui` — stdout/stderr/status/warning/hint writers. Mirrors jj's rule:
//! status/warnings/hints go to **stderr** (so piping `grs log` gives clean
//! stdout); only primary output goes to stdout. `--quiet` suppresses status.

use std::io::{self, Write};

pub struct Ui {
    pub quiet: bool,
    #[allow(dead_code)]
    pub color: ColorChoice,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Always,
    Never,
    #[default]
    Auto,
}

impl Ui {
    pub fn new(quiet: bool, color: ColorChoice) -> Self {
        Self { quiet, color }
    }

    /// Primary output -> stdout.
    pub fn stdout(&mut self) -> impl Write + '_ {
        io::stdout().lock()
    }

    /// Write a line to stdout.
    pub fn say(&mut self, line: &str) -> io::Result<()> {
        let mut out = io::stdout().lock();
        writeln!(out, "{line}")?;
        Ok(())
    }

    /// Status -> stderr, suppressed by `--quiet`.
    pub fn status(&mut self, line: &str) -> io::Result<()> {
        if self.quiet {
            return Ok(());
        }
        let mut err = io::stderr().lock();
        writeln!(err, "{line}")?;
        Ok(())
    }

    /// Warning -> stderr with "Warning: " prefix.
    pub fn warning(&mut self, line: &str) -> io::Result<()> {
        let mut err = io::stderr().lock();
        writeln!(err, "Warning: {line}")?;
        Ok(())
    }

    /// Hint -> stderr with "Hint: " prefix.
    pub fn hint(&mut self, line: &str) -> io::Result<()> {
        let mut err = io::stderr().lock();
        writeln!(err, "Hint: {line}")?;
        Ok(())
    }

    /// Plain stderr write (no newline).
    pub fn stderr(&mut self, s: &str) -> io::Result<()> {
        let mut err = io::stderr().lock();
        write!(err, "{s}")?;
        Ok(())
    }

    /// Phase 2: request a pager for long stdout output. Phase 1: no-op.
    pub fn request_pager(&mut self) {}
}
