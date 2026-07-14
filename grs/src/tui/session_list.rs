//! Session list view — the TUI's home screen.
//!
//! Two-line rows, newest first, with a movable selection cursor. Step 4
//! ships the minimal keymap (`j`/`k`/`Enter`/`q`); step 5 layers on
//! `n`/`N` (new), `d` (delete with confirm), `/` (filter), `?` (help).

use crate::tui::input::KeyAction;
use crate::tui::theme::{ACCENT, MUTED, STATUS_BG, STATUS_FG};
use grs_lib::model::Session;
use grs_lib::store::RepoStore;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

/// Outcome of feeding a key into the session list.
#[derive(Clone, Debug)]
pub enum ListCmd {
    Stay,
    Quit,
    /// Open the selected session in the code-review view.
    OpenCodeReview(Session),
}

pub struct SessionListState {
    pub store: RepoStore,
    pub sessions: Vec<Session>,
    pub list_state: ListState,
}

impl SessionListState {
    pub fn load(store: RepoStore) -> Self {
        let mut sessions = store.sessions().list().unwrap_or_default();
        // SessionStore::list already returns newest-first by started_at; the
        // explicit re-sort keeps the invariant local if a caller passes a
        // pre-built list.
        sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        let mut list_state = ListState::default();
        if !sessions.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            store,
            sessions,
            list_state,
        }
    }

    /// Re-list sessions from disk. Preserves the cursor where possible.
    pub fn refresh(&mut self) {
        let cursor_id = self
            .list_state
            .selected()
            .and_then(|i| self.sessions.get(i).map(|s| s.id.clone()));
        self.sessions = self.store.sessions().list().unwrap_or_default();
        self.sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        let new_idx = cursor_id
            .as_ref()
            .and_then(|id| self.sessions.iter().position(|s| &s.id == id));
        if new_idx.is_none() && !self.sessions.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(new_idx);
        }
    }

    pub fn selected(&self) -> Option<&Session> {
        self.list_state.selected().and_then(|i| self.sessions.get(i))
    }

    pub fn on_action(&mut self, action: KeyAction) -> ListCmd {
        match action {
            KeyAction::Down => {
                if !self.sessions.is_empty() {
                    let i = self.list_state.selected().unwrap_or(0);
                    let next = (i + 1).min(self.sessions.len() - 1);
                    self.list_state.select(Some(next));
                }
                ListCmd::Stay
            }
            KeyAction::Up => {
                if !self.sessions.is_empty() {
                    let i = self.list_state.selected().unwrap_or(0);
                    let next = i.saturating_sub(1);
                    self.list_state.select(Some(next));
                }
                ListCmd::Stay
            }
            KeyAction::Enter => {
                if let Some(s) = self.selected().cloned() {
                    ListCmd::OpenCodeReview(s)
                } else {
                    ListCmd::Stay
                }
            }
            KeyAction::Quit | KeyAction::Back => ListCmd::Quit,
            _ => ListCmd::Stay,
        }
    }
}

pub fn render(f: &mut Frame, state: &mut SessionListState) {
    let area = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(5),   // list
            Constraint::Length(1), // status
        ])
        .split(area);

    // Title
    let title = Paragraph::new(Line::from(vec![Span::styled(
        " sessions ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )]))
    .style(Style::default().bg(STATUS_BG));
    f.render_widget(title, chunks[0]);

    // List
    if state.sessions.is_empty() {
        f.render_widget(
            Paragraph::new("(no sessions yet — make a save in your editor to start one)")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(MUTED)),
                )
                .style(Style::default().fg(MUTED)),
            chunks[1],
        );
    } else {
        let items: Vec<ListItem> = state
            .sessions
            .iter()
            .map(row_for_session)
            .collect();
        let mut list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(MUTED))
                    .title(" sessions "),
            )
            .highlight_style(
                Style::default()
                    .bg(STATUS_BG)
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        list = list.style(Style::default().fg(STATUS_FG));
        f.render_stateful_widget(list, chunks[1], &mut state.list_state);
    }

    // Status bar
    let status = Line::from(vec![
        Span::styled("j/k", Style::default().fg(ACCENT)),
        Span::raw(" move  "),
        Span::styled("Enter", Style::default().fg(ACCENT)),
        Span::raw(" open  "),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::raw(" quit"),
    ]);
    f.render_widget(
        Paragraph::new(status).style(Style::default().bg(STATUS_BG).fg(STATUS_FG)),
        chunks[2],
    );
}

fn row_for_session(s: &Session) -> ListItem<'static> {
    let id_short: String = s.id.as_str().chars().take(10).collect();
    let status = if s.is_open() { "open  " } else { "closed" };
    let started = format_started(s.started_at);
    let summary = format!(
        "{id_short}  {status}  {} files  {} snaps  started {started}",
        s.file_count, s.snap_count
    );
    let line = Line::from(Span::raw(summary));
    ListItem::new(vec![line])
}

/// Render a millis-since-epoch timestamp as a short local-time string. If
/// the conversion fails (e.g. way out of range), fall back to the raw millis.
fn format_started(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => ms.to_string(),
    }
}
