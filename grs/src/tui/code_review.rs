//! Code review view — thin wrapper over `replay_view` for now.
//!
//! Step 4 keeps the underlying replay timelapse intact (so the TUI shell
//! compiles and runs end-to-end). Step 6 strips the replay-specific
//! actions (play, speed, `:N`, `g`/`G` as snap jumps, side-by-side) and
//! renames the type. Step 7 rewrites `render_snap` to a unified diff with
//! the prior text inline.

use crate::tui::file_view::FileViewState;
use crate::tui::highlight::HighlightEngine;
use crate::tui::input::{KeyAction, VimParser};
use crate::tui::replay_view::{self, ReplayOutcome, ReplayState};
use grs_lib::model::Session;
use grs_lib::store::RepoStore;
use ratatui::Frame;

/// Outcome of feeding a key into the code review view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeReviewCmd {
    Stay,
    /// Pop back to the session list.
    Pop,
}

pub struct CodeReviewState {
    pub inner: ReplayState,
}

impl CodeReviewState {
    pub fn load(store: RepoStore, session: Session) -> Self {
        Self {
            inner: ReplayState::load(store, session),
        }
    }

    pub fn file_view(&self) -> &FileViewState {
        &self.inner.file_view
    }

    pub fn on_action(&mut self, action: KeyAction, parser: &mut VimParser) -> CodeReviewCmd {
        match self.inner.on_action(action, parser) {
            ReplayOutcome::Stay => CodeReviewCmd::Stay,
            ReplayOutcome::Quit => CodeReviewCmd::Pop,
        }
    }
}

pub fn render(f: &mut Frame, state: &mut CodeReviewState, engine: &mut HighlightEngine) {
    replay_view::render(f, &mut state.inner, engine);
}
