//! TUI — minimal replay timelapse.
//!
//! Public entry point:
//!   - `run_replay(store, session)` — opens the replay screen.
//!
//! The watcher runs in a background thread for the TUI's lifetime.

pub mod file_view;
pub mod highlight;
pub mod input;
pub mod replay_view;
pub mod theme;
pub mod watch;

use crate::command_error::CommandError;
use crate::tui::highlight::HighlightEngine;
use crate::tui::input::{KeyOutcome, VimParser};
use crate::tui::replay_view::{ReplayOutcome, ReplayState};
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use grs_lib::store::RepoStore;
use grs_lib::ulid::SessionId;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::{Duration, Instant};

/// Open directly in the replay screen for `session`.
#[allow(clippy::needless_pass_by_value)]
pub fn run_replay(
    store: RepoStore,
    session: grs_lib::model::Session,
) -> Result<(), CommandError> {
    let _guard = watch::spawn(store.clone());
    let engine = HighlightEngine::new(&store.config().replay.syntax_theme);
    let state = ReplayState::load(store, session);
    let mut terminal = setup_terminal().map_err(CommandError::internal_error)?;
    let result = replay_event_loop(&mut terminal, state, engine);
    teardown_terminal(&mut terminal).map_err(CommandError::internal_error)?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn teardown_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn replay_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut state: ReplayState,
    mut engine: HighlightEngine,
) -> Result<(), CommandError> {
    let mut parser = VimParser::new();
    let tick = Duration::from_millis(16); // ~60fps
    let mut last_refresh = Instant::now();
    loop {
        terminal
            .draw(|f| replay_view::render(f, &mut state, &mut engine))
            .map_err(CommandError::internal_error)?;

        // Periodic refresh so live capture shows up without pressing `r`.
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            last_refresh = Instant::now();
            state.refresh();
        }

        // Animation tick every frame, regardless of input.
        state.tick();

        if event::poll(tick).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_filter_key_kind(&key) {
                    continue;
                }
                match parser.feed(key) {
                    KeyOutcome::Action(action) => {
                        if let ReplayOutcome::Quit = state.on_action(action, &mut parser) {
                            return Ok(());
                        }
                    }
                    KeyOutcome::Pending(_) | KeyOutcome::Cleared => {}
                }
            }
        }
    }
}

fn matches_filter_key_kind(key: &KeyEvent) -> bool {
    // Crossterm emits both Press and Release in some configs; only handle Press.
    matches!(key.kind, KeyEventKind::Press)
}

/// Helper to resolve a session id (prefix) and load the session.
pub fn resolve_session(store: &RepoStore, id: &str) -> Result<grs_lib::model::Session, CommandError> {
    store
        .sessions()
        .resolve_prefix(id)
        .map_err(CommandError::from)
}

/// Test-only constructor that takes a pre-built store.
pub fn replay_view_for_test(
    store: RepoStore,
    session: grs_lib::model::Session,
) -> replay_view::ReplayState {
    replay_view::ReplayState::load(store, session)
}

#[allow(dead_code)]
fn _unused_id(_: SessionId) {}
