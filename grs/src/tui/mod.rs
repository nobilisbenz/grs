//! TUI — the two-pane shell.
//!
//! Top-level entry points:
//! - `run_tui(store)`: the live TUI. Starts a watcher in a background thread,
//!   runs the input loop, and the watcher calls `request_redraw` on every
//!   snap. On `q`, the watcher is stopped, the session is finalized, and
//!   we exit.
//! - `run_viewer(store, session)`: read-only viewer for a past session.
//!   Same two-pane layout, no watcher, no name prompt.

pub mod diff_view;
pub mod snap_list;
pub mod theme;
pub mod watcher;

use crate::command_error::CommandError;
use crate::tui::diff_view::DiffViewState;
use crate::tui::snap_list::SnapListState;
use crate::tui::theme::Theme;
use crate::tui::watcher::WatcherHandle;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use grs_lib::model::SessionMeta;
use grs_lib::snap::SnapEntry;
use grs_lib::store::RepoStore;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// Tick rate for the input poll loop.
const TICK: Duration = Duration::from_millis(100);

/// State that drives the live TUI.
pub struct LiveTui {
    pub store: RepoStore,
    pub session: SessionMeta,
    pub snap_list: SnapListState,
    pub diff_view: DiffViewState,
    /// Set to true when a watcher event arrives; cleared after redraw.
    pub needs_redraw: bool,
    /// Channel from the watcher thread to signal a new snap was captured.
    pub watch_rx: mpsc::Receiver<WatchEvent>,
    /// The watcher thread handle.
    pub watcher: WatcherHandle,
}

#[derive(Clone, Debug)]
pub enum WatchEvent {
    SnapCaptured { n: u32 },
}

/// Open the live TUI. If the project has no open session, runs the
/// name-prompt TUI first; on confirm, creates the session and proceeds.
pub fn run_tui(store: RepoStore) -> Result<(), CommandError> {
    // One-time gitignore warning (suppressed after first run by a marker).
    crate::warnings::check_and_warn(store.root());

    let mut terminal = setup_terminal().map_err(CommandError::internal_error)?;
    let result = run_tui_inner(&mut terminal, store);
    teardown_terminal(&mut terminal).map_err(CommandError::internal_error)?;
    result
}

fn run_tui_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: RepoStore,
) -> Result<(), CommandError> {
    let session: SessionMeta = if let Some(s) = store
        .current_session()
        .map_err(CommandError::from)?
    {
        s
    } else {
        // Name-prompt flow.
        let name = run_name_prompt(terminal, &store)
            .map_err(CommandError::internal_error)?;
        match name {
            Some(name) => store
                .open_first_session(name)
                .map_err(CommandError::from)?,
            None => {
                // User pressed Esc. Exit gracefully.
                return Ok(());
            }
        }
    };

    // Acquire the project lock and start the watcher.
    let _guard = store.lock().map_err(CommandError::from)?;
    let (watch_tx, watch_rx) = mpsc::channel();
    let watcher = WatcherHandle::spawn(store.clone(), watch_tx);

    let snap_list = SnapListState::load(&store, &session).map_err(CommandError::from)?;
    let diff_view = DiffViewState::new();

    let mut tui = LiveTui {
        store: store.clone(),
        session: session.clone(),
        snap_list,
        diff_view,
        needs_redraw: true,
        watch_rx,
        watcher,
    };

    let result = tui_loop(terminal, &mut tui);
    tui.watcher.stop();
    finalize_session(&tui.store, &tui.session.id).map_err(CommandError::from)?;
    result
}

/// Read-only TUI viewer for a past session. No watcher, no name prompt.
pub fn run_viewer(store: RepoStore, session: SessionMeta) -> Result<(), CommandError> {
    let mut snap_list = SnapListState::load(&store, &session).map_err(CommandError::from)?;
    let mut diff_view = DiffViewState::new();
    if let Some(entry) = snap_list.entries.get(snap_list.cursor) {
        let _ = diff_view.load(&store, &session, entry);
    }

    let mut terminal = setup_terminal().map_err(CommandError::internal_error)?;
    let result = viewer_loop(&mut terminal, &store, &session, &mut snap_list, &mut diff_view);
    teardown_terminal(&mut terminal).map_err(CommandError::internal_error)?;
    result
}


fn run_name_prompt(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &RepoStore,
) -> io::Result<Option<String>> {
    let mut input = String::new();
    let tick = Duration::from_millis(50);
    loop {
        terminal.draw(|f| draw_name_prompt(f, f.size(), &input, store.root()))?;
        if event::poll(tick)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                // Ctrl-C quits.
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
                            continue; // don't accept empty
                        }
                        return Ok(Some(trimmed));
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            input.push(c);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn finalize_session(store: &RepoStore, id: &grs_lib::SessionId) -> Result<(), CommandError> {
    let snap_count = store.snaps().count(id).map_err(CommandError::from)?;
    store
        .sessions()
        .finalize(id, grs_lib::util::time::now_ms(), snap_count)
        .map_err(CommandError::from)?;
    Ok(())
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

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Live TUI event loop
// -----------------------------------------------------------------------------

fn tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    tui: &mut LiveTui,
) -> Result<(), CommandError> {
    loop {
        // Drain watcher events.
        while let Ok(ev) = tui.watch_rx.try_recv() {
            match ev {
                WatchEvent::SnapCaptured { .. } => {
                    // Reload the snap list.
                    if let Ok(list) = SnapListState::load(&tui.store, &tui.session) {
                        tui.snap_list = list;
                    }
                    tui.needs_redraw = true;
                }
            }
        }

        if tui.needs_redraw {
            // Refresh the current diff if the cursor moved.
            if let Some(entry) = tui.snap_list.entries.get(tui.snap_list.cursor) {
                if tui.diff_view.current_snap_n != Some(entry.n) {
                    let _ = tui.diff_view.load(&tui.store, &tui.session, entry);
                }
            }
            terminal
                .draw(|f| draw_live(f, tui))
                .map_err(CommandError::internal_error)?;
            tui.needs_redraw = false;
        }

        if event::poll(TICK).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                match handle_live_key(tui, key)? {
                    LiveKeyResult::Continue => {}
                    LiveKeyResult::Quit => return Ok(()),
                }
            }
        }
    }
}

enum LiveKeyResult {
    Continue,
    Quit,
}

fn handle_live_key(tui: &mut LiveTui, key: KeyEvent) -> Result<LiveKeyResult, CommandError> {
    // Ctrl-C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(LiveKeyResult::Quit);
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(LiveKeyResult::Quit),
        KeyCode::Char('j') | KeyCode::Down => {
            tui.snap_list.cursor_down();
            tui.needs_redraw = true;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            tui.snap_list.cursor_up();
            tui.needs_redraw = true;
        }
        KeyCode::Char('g') => {
            tui.snap_list.cursor_to_top();
            tui.needs_redraw = true;
        }
        KeyCode::Char('G') => {
            tui.snap_list.cursor_to_bottom();
            tui.needs_redraw = true;
        }
        KeyCode::Char('r') => {
            // Manual refresh.
            if let Ok(list) = SnapListState::load(&tui.store, &tui.session) {
                tui.snap_list = list;
            }
            tui.needs_redraw = true;
        }
        _ => {}
    }
    Ok(LiveKeyResult::Continue)
}

fn draw_live(f: &mut ratatui::Frame, tui: &LiveTui) {
    let area = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Min(3),    // main panes
        ])
        .split(area);

    draw_status_bar(f, chunks[0], tui);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(chunks[1]);

    draw_snap_list(f, panes[0], &tui.snap_list);
    draw_diff(f, panes[1], &tui.diff_view);
}

fn draw_status_bar(f: &mut ratatui::Frame, area: Rect, tui: &LiveTui) {
    let theme = Theme::default();
    let title = format!(" grs — session: {} ", tui.session.name);
    let hint = "  j/k snap   g/G top/bottom   r refresh   q quit ";
    let line = Line::from(vec![
        Span::styled(title, theme.status_title()),
        Span::styled(hint, theme.status_hint()),
    ]);
    let p = Paragraph::new(line).style(theme.status_bar());
    f.render_widget(p, area);
}

fn draw_snap_list(f: &mut ratatui::Frame, area: Rect, list: &SnapListState) {
    let theme = Theme::default();
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(theme.border())
        .title(Span::styled(" snaps ", theme.title()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows: Vec<Line> = list
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| draw_snap_row(list.cursor == i, e, list.diffs.get(&e.n)))
        .collect();
    let p = Paragraph::new(rows);
    f.render_widget(p, inner);
}

fn draw_snap_row(selected: bool, entry: &SnapEntry, diff: Option<&(usize, usize)>) -> Line<'static> {
    let theme = Theme::default();
    let style = if selected {
        theme.selected()
    } else {
        theme.normal()
    };
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(format!("{:>4} ", entry.n), theme.snap_number()));
    spans.push(Span::styled(
        format_timestamp(entry.timestamp),
        theme.timestamp(),
    ));
    if let Some((add, rem)) = diff {
        spans.push(Span::styled(
            format!("  {:>+4} ", add),
            theme.added(),
        ));
        spans.push(Span::styled(
            format!("{:>-4} ", rem),
            theme.removed(),
        ));
    } else {
        spans.push(Span::styled("         ", theme.normal()));
    }
    Line::from(spans).style(style)
}

fn draw_diff(f: &mut ratatui::Frame, area: Rect, view: &DiffViewState) {
    let theme = Theme::default();
    let block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(
            view.title(),
            theme.title(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if view.lines.is_empty() {
        let placeholder = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(view.placeholder(), theme.muted())),
        ]);
        f.render_widget(placeholder, inner);
        return;
    }
    let p = Paragraph::new(view.lines.clone()).scroll((view.scroll as u16, 0));
    f.render_widget(p, inner);
}

fn draw_name_prompt(f: &mut ratatui::Frame, area: Rect, input: &str, root: &Path) {
    let theme = Theme::default();
    let popup = centered_rect(60, 7, area);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border())
        .title(Span::styled(" session name ", theme.title()));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let project = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");
    let lines = vec![
        Line::from(Span::styled(
            format!("project: {project}"),
            theme.muted(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", theme.normal()),
            Span::styled(input, theme.snap_number().add_modifier(Modifier::BOLD)),
            Span::styled("_", theme.muted()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter to start, esc to cancel",
            theme.muted(),
        )),
    ];
    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_w = (area.width * percent_x) / 100;
    let popup_h = height;
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    Rect::new(x, y, popup_w, popup_h)
}

fn format_timestamp(ms: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "??:??:??".to_string())
}

fn matches_key_kind(key: &KeyEvent) -> bool {
    matches!(
        key.kind,
        KeyEventKind::Press | KeyEventKind::Repeat
    )
}

// -----------------------------------------------------------------------------
// Viewer loop (read-only)
// -----------------------------------------------------------------------------

fn viewer_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &RepoStore,
    session: &SessionMeta,
    list: &mut SnapListState,
    diff_view: &mut DiffViewState,
) -> Result<(), CommandError> {
    loop {
        terminal
            .draw(|f| draw_viewer(f, list, diff_view, session))
            .map_err(CommandError::internal_error)?;

        if event::poll(TICK).map_err(CommandError::internal_error)? {
            if let Ok(Event::Key(key)) = event::read() {
                if !matches_key_kind(&key) {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    return Ok(());
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => list.cursor_down(),
                    KeyCode::Char('k') | KeyCode::Up => list.cursor_up(),
                    KeyCode::Char('g') => list.cursor_to_top(),
                    KeyCode::Char('G') => list.cursor_to_bottom(),
                    _ => {}
                }
                if let Some(entry) = list.entries.get(list.cursor) {
                    let _ = diff_view.load(store, session, entry);
                }
            }
        }
    }
}

fn draw_viewer(
    f: &mut ratatui::Frame,
    list: &SnapListState,
    diff_view: &DiffViewState,
    session: &SessionMeta,
) {
    let area = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
        ])
        .split(area);
    let title = format!(
        " grs — viewing: {}   (read-only) ",
        session.name
    );
    let line = Line::from(vec![
        Span::styled(title, Theme::default().status_title()),
        Span::styled(
            "  j/k snap   q back ",
            Theme::default().status_hint(),
        ),
    ]);
    f.render_widget(Paragraph::new(line).style(Theme::default().status_bar()), chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(chunks[1]);
    draw_snap_list(f, panes[0], list);
    draw_diff(f, panes[1], diff_view);
}
