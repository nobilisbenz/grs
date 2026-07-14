//! TUI — two-view shell (session list ↔ code review).
//!
//! Public entry point: `run_tui(store)` opens the shell with the session
//! list as the home screen. The watcher (see `watch::spawn`) follows HEAD
//! on its own; we don't need to tear it down on `grs new`.
//!
//! The replay timelapse is still reachable via `grs replay <id>` (and via
//! `run_replay`); step 6 deletes both.

pub mod code_review;
pub mod file_view;
pub mod highlight;
pub mod input;
pub mod replay_view;
pub mod session_list;
pub mod theme;
pub mod watch;

use crate::command_error::CommandError;
use crate::tui::code_review::{CodeReviewCmd, CodeReviewState};
use crate::tui::highlight::HighlightEngine;
use crate::tui::input::{KeyAction, KeyOutcome, VimParser};
use crate::tui::replay_view::ReplayState;
use crate::tui::session_list::{ListCmd, SessionListState};
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use grs_lib::model::Session;
use grs_lib::store::RepoStore;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::{Duration, Instant};

/// What a view returns in response to a key. The shell applies the effect
/// at the top of the view stack.
#[derive(Clone, Debug)]
pub enum ViewCmd {
    Stay,
    /// Pop the top view (back to the previous one).
    Pop,
    /// Push a brand-new code-review view for the given session.
    PushCodeReview(Session),
    /// Quit the TUI entirely.
    Quit,
}

/// One slot in the view stack. The shell draws + dispatches the top slot.
enum ViewSlot {
    SessionList(SessionListState),
    CodeReview(CodeReviewState, HighlightEngine),
}

/// Open the TUI shell starting at the session list.
#[allow(clippy::needless_pass_by_value)]
pub fn run_tui(store: RepoStore) -> Result<(), CommandError> {
    let _guard = watch::spawn(store.clone());
    let mut engine = HighlightEngine::new(&store.config().tui.syntax_theme);
    let mut terminal = setup_terminal().map_err(CommandError::internal_error)?;
    let mut stack: Vec<ViewSlot> = vec![ViewSlot::SessionList(SessionListState::load(store))];
    let result = shell_event_loop(&mut terminal, &mut stack, &mut engine);
    teardown_terminal(&mut terminal).map_err(CommandError::internal_error)?;
    result
}

/// Open directly in the replay screen for `session` (used by `grs replay`).
/// Step 6 deletes this entry point.
#[allow(clippy::needless_pass_by_value)]
pub fn run_replay(
    store: RepoStore,
    session: grs_lib::model::Session,
) -> Result<(), CommandError> {
    let _guard = watch::spawn(store.clone());
    let engine = HighlightEngine::new(&store.config().tui.syntax_theme);
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

fn shell_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    stack: &mut Vec<ViewSlot>,
    engine: &mut HighlightEngine,
) -> Result<(), CommandError> {
    let mut parser = VimParser::new();
    let tick = Duration::from_millis(16);
    let mut last_refresh = Instant::now();
    loop {
        // Draw whatever is on top.
        terminal
            .draw(|f| draw_top(f, stack))
            .map_err(CommandError::internal_error)?;

        // Periodic refresh of the top view (lets live capture show up
        // without pressing `r`).
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            last_refresh = Instant::now();
            if let Some(slot) = stack.last_mut() {
                refresh_top(slot);
            }
        }

        // Animation tick for the code review view (today: play mode).
        if let Some(ViewSlot::CodeReview(cr, _)) = stack.last_mut() {
            cr.inner.tick();
        }

        if event::poll(tick).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_filter_key_kind(&key) {
                    continue;
                }
                match parser.feed(key) {
                    KeyOutcome::Action(action) => {
                        let cmd = dispatch(stack, engine, action, &mut parser);
                        match cmd {
                            ViewCmd::Stay => {}
                            ViewCmd::Pop => {
                                if stack.len() > 1 {
                                    stack.pop();
                                } else {
                                    return Ok(());
                                }
                            }
                            ViewCmd::PushCodeReview(session) => {
                                let store = store_for_new_view(stack);
                                let new_engine =
                                    HighlightEngine::new(&store.config().tui.syntax_theme);
                                stack.push(ViewSlot::CodeReview(
                                    CodeReviewState::load(store, session),
                                    new_engine,
                                ));
                            }
                            ViewCmd::Quit => return Ok(()),
                        }
                    }
                    KeyOutcome::Pending(_) | KeyOutcome::Cleared => {}
                }
            }
        }
    }
}

fn draw_top(f: &mut ratatui::Frame, stack: &mut [ViewSlot]) {
    if let Some(slot) = stack.last_mut() {
        match slot {
            ViewSlot::SessionList(s) => session_list::render(f, s),
            ViewSlot::CodeReview(cr, eng) => code_review::render(f, cr, eng),
        }
    }
}

fn refresh_top(slot: &mut ViewSlot) {
    match slot {
        ViewSlot::SessionList(s) => s.refresh(),
        ViewSlot::CodeReview(cr, _) => cr.inner.refresh(),
    }
}

fn dispatch(
    stack: &mut [ViewSlot],
    _engine: &mut HighlightEngine,
    action: KeyAction,
    parser: &mut VimParser,
) -> ViewCmd {
    if let Some(slot) = stack.last_mut() {
        match slot {
            ViewSlot::SessionList(s) => match s.on_action(action) {
                ListCmd::Stay => ViewCmd::Stay,
                ListCmd::Quit => ViewCmd::Quit,
                ListCmd::OpenCodeReview(session) => ViewCmd::PushCodeReview(session),
            },
            ViewSlot::CodeReview(cr, _) => match cr.on_action(action, parser) {
                CodeReviewCmd::Stay => ViewCmd::Stay,
                CodeReviewCmd::Pop => ViewCmd::Pop,
            },
        }
    } else {
        ViewCmd::Quit
    }
}

/// Borrow the store out of the top view slot to seed a new code-review view.
/// Today only the SessionList slot holds a `RepoStore` directly. If the
/// top slot is a CodeReview view, we clone out of the inner ReplayState
/// (which also owns a RepoStore).
fn store_for_new_view(stack: &mut [ViewSlot]) -> RepoStore {
    match stack.last_mut() {
        Some(ViewSlot::SessionList(s)) => s.store.clone(),
        Some(ViewSlot::CodeReview(cr, _)) => cr.inner.store.clone(),
        None => panic!("store_for_new_view called with empty stack"),
    }
}

fn replay_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut state: ReplayState,
    mut engine: HighlightEngine,
) -> Result<(), CommandError> {
    let mut parser = VimParser::new();
    let tick = Duration::from_millis(16);
    let mut last_refresh = Instant::now();
    loop {
        terminal
            .draw(|f| replay_view::render(f, &mut state, &mut engine))
            .map_err(CommandError::internal_error)?;

        if last_refresh.elapsed() >= Duration::from_secs(1) {
            last_refresh = Instant::now();
            state.refresh();
        }
        state.tick();

        if event::poll(tick).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_filter_key_kind(&key) {
                    continue;
                }
                match parser.feed(key) {
                    KeyOutcome::Action(action) => {
                        if let crate::tui::replay_view::ReplayOutcome::Quit =
                            state.on_action(action, &mut parser)
                        {
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
    matches!(key.kind, KeyEventKind::Press)
}

/// Test-only constructor that returns a freshly-loaded `SessionListState`.
pub fn session_list_for_test(store: RepoStore) -> SessionListState {
    SessionListState::load(store)
}

/// Test-only constructor that returns a freshly-loaded `ReplayState`
/// (used by the existing TUI integration tests). Step 6 deletes this.
pub fn replay_view_for_test(
    store: RepoStore,
    session: grs_lib::model::Session,
) -> ReplayState {
    ReplayState::load(store, session)
}

#[allow(dead_code)]
fn _unused_session(_: Session) {}
