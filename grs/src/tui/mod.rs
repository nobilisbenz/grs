//! TUI — the full-screen shell.
//!
//! One screen at a time:
//! - **Session list** (`session_list`): the home view. New, list, delete sessions.
//! - **Code review** (`code_review`): one session's snaps, one file at a time.
//!
//! The TUI spawns a file-watcher thread while it's running. When the user
//! quits, the watcher is stopped and the open session is finalized.
//!
//! Top-level entry point: `run_tui(store)`.

pub mod code_review;
pub mod file_view;
pub mod highlight;
pub mod input;
pub mod session_list;
pub mod theme;

mod watch;

use crate::command_error::CommandError;
use crate::tui::code_review::{CodeReviewCmd, CodeReviewState};
use crate::tui::input::{KeyAction, KeyOutcome, VimParser};
use crate::tui::session_list::{ListCmd, SessionListState};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use grs_lib::store::RepoStore;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use std::io;
use std::time::Duration;

/// Tick rate for the input poll loop.
const TICK: Duration = Duration::from_millis(100);

/// Open the live TUI. If the project has no open session, runs a name
/// prompt first; on confirm, creates the session and proceeds.
///
/// `with_watcher=false` opens the TUI in read-only mode: no project
/// lock, no internal watcher. Use this when another `grs watch`
/// process is already capturing the project (e.g. the pi extension's
/// background watcher) and you want to browse the journal without
/// contending for the lock.
pub fn run_tui(store: RepoStore, with_watcher: bool) -> Result<(), CommandError> {
    crate::warnings::check_and_warn(store.root());

    let mut terminal = setup_terminal().map_err(CommandError::internal_error)?;
    let result = run_tui_inner(&mut terminal, store, with_watcher);
    teardown_terminal(&mut terminal).map_err(CommandError::internal_error)?;
    result
}

fn run_tui_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: RepoStore,
    with_watcher: bool,
) -> Result<(), CommandError> {
    // Ensure a session exists. If not, prompt for a name and create one.
    if store.current_session().map_err(CommandError::from)?.is_none() {
        let name = run_name_prompt(terminal, &store).map_err(CommandError::internal_error)?;
        match name {
            Some(name) => {
                store.open_first_session(name).map_err(CommandError::from)?;
            }
            None => return Ok(()), // user pressed Esc
        }
    }

    // Acquire the project lock and start the watcher, unless the
    // caller asked for read-only mode (in which case another `grs
    // watch` is presumably already capturing).
    let _guard;
    let _watcher;
    if with_watcher {
        _guard = Some(store.lock().map_err(CommandError::from)?);
        _watcher = Some(watch::WatcherGuard::start(store.clone()));
    } else {
        _guard = None;
        _watcher = None;
    }

    // Run the session list, then optionally the code review.
    let mut list = SessionListState::load(store.clone());
    let mut parser = VimParser::new();
    list_loop(terminal, &mut list, &mut parser)
}

/// Loop: render the session list, drain keys, dispatch to the
/// session_list state. If the user picks a session, run the code review
/// loop and return when they pop back.
fn list_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut SessionListState,
    parser: &mut VimParser,
) -> Result<(), CommandError> {
    loop {
        terminal
            .draw(|f| session_list::render(f, state))
            .map_err(CommandError::internal_error)?;

        if event::poll(TICK).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                // When the new-session name prompt is up, the parser's
                // keymap doesn't apply — every char (including letters
                // that would be navigation in the list, like `j`/`k`)
                // is input to the buffer. Translate the raw key to the
                // right KeyAction directly and skip the parser.
                let action = if state.new_session_prompt {
                    prompt_key_to_action(key)
                } else {
                    let outcome = parser.feed(key);
                    match outcome {
                        KeyOutcome::Action(a) => a,
                        KeyOutcome::Pending(_) => continue,
                        KeyOutcome::Cleared => continue,
                    }
                };
                match state.on_action(action) {
                    ListCmd::Stay => {}
                    ListCmd::Quit => return Ok(()),
                    ListCmd::OpenCodeReview(session) => {
                        let mut cr = CodeReviewState::load(state.store.clone(), session);
                        let outcome = code_review_loop(terminal, &mut cr, parser);
                        // After pop, refresh the list (the user may have
                        // deleted or rotated sessions).
                        state.refresh();
                        outcome?;
                    }
                }
            }
        }
    }
}

/// Translate a raw `KeyEvent` into a `KeyAction` for the new-session name
/// prompt. The prompt's keymap is: any char -> append; Backspace -> pop;
/// Enter -> confirm; Esc -> cancel. The parser's normal keymap does not
/// apply while the prompt is up.
fn prompt_key_to_action(key: KeyEvent) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::CancelFilter;
    }
    match key.code {
        KeyCode::Esc => KeyAction::CancelFilter,
        KeyCode::Enter => KeyAction::ConfirmFilter,
        KeyCode::Backspace => KeyAction::FilterBackspace,
        KeyCode::Char(c) => KeyAction::FilterChar(c),
        _ => KeyAction::None,
    }
}

/// Loop: render the code review, drain keys, dispatch to the
/// code_review state. Returns when the user pops back.
fn code_review_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut CodeReviewState,
    parser: &mut VimParser,
) -> Result<(), CommandError> {
    let mut engine = crate::tui::highlight::HighlightEngine::new(&state.theme_name());
    loop {
        terminal
            .draw(|f| code_review::render(f, state, &mut engine))
            .map_err(CommandError::internal_error)?;

        if event::poll(TICK).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                let action = parser.feed(key);
                let action = match action {
                    KeyOutcome::Action(a) => a,
                    KeyOutcome::Pending(_) => continue,
                    KeyOutcome::Cleared => continue,
                };
                if matches!(state.on_action(action, parser), CodeReviewCmd::Pop) {
                    return Ok(());
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Name prompt (only used when no session exists yet)
// -----------------------------------------------------------------------------

fn run_name_prompt(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &RepoStore,
) -> io::Result<Option<String>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    use crate::tui::theme::{ACCENT, MUTED, STATUS_FG};

    let mut input = String::new();
    let tick = Duration::from_millis(50);
    loop {
        terminal.draw(|f| {
            let area = f.size();
            let popup = centered_rect(60, 7, area);
            f.render_widget(Clear, popup);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(MUTED))
                .title(Span::styled(" session name ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let project = store
                .root()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("(unknown)");
            let lines = vec![
                Line::from(Span::styled(format!("project: {project}"), Style::default().fg(MUTED))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(STATUS_FG)),
                    Span::styled(input.clone(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                    Span::styled("_", Style::default().fg(MUTED)),
                ]),
                Line::from(""),
                Line::from(Span::styled("enter to start, esc to cancel", Style::default().fg(MUTED))),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        })?;
        if event::poll(tick)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c')
                {
                    return Ok(None);
                }
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        let trimmed = input.trim().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }
                        return Ok(Some(trimmed));
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.push(c);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_w = (area.width * percent_x) / 100;
    let popup_h = height;
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    Rect::new(x, y, popup_w, popup_h)
}

// -----------------------------------------------------------------------------
// Terminal setup
// -----------------------------------------------------------------------------

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn matches_key_kind(key: &KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}
