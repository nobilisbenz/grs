//! Session list view — the TUI's home screen.
//!
//! Two-line rows, newest first, with a movable selection cursor, an
//! id-prefix filter (`/`), session creation (`n`/`N`), closed-session
//! deletion with a one-key confirm (`d` then `d`), a help overlay
//! (`?`), and a manual refresh (`r`).

use crate::tui::input::KeyAction;
use crate::tui::theme::{ACCENT, MUTED, SCRUBBER_BG, STATUS_BG, STATUS_FG, WARNING};
use grs_lib::error::GrsError;
use grs_lib::model::SessionMeta;
use grs_lib::store::RepoStore;
use grs_lib::ulid::SessionId;
use grs_lib::util::time::now_ms;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

/// Outcome of feeding a key into the session list.
#[derive(Clone, Debug)]
pub enum ListCmd {
    Stay,
    Quit,
    /// Open the selected session in the code-review view.
    OpenCodeReview(SessionMeta),
}

pub struct SessionListState {
    pub store: RepoStore,
    pub sessions: Vec<SessionMeta>,
    pub list_state: ListState,
    /// Id-prefix filter; empty string means "no filter".
    pub filter: String,
    /// Session awaiting delete confirmation. `Some(id)` after a first `d`
    /// press; cleared on the second `d` (confirm) or on any other action.
    pub pending_delete: Option<SessionId>,
    /// True while the help overlay is shown.
    pub help_open: bool,
    /// Last user-facing message (e.g. "deleted session ..."), cleared on
    /// the next keypress that mutates the list. Shown briefly in the
    /// status bar.
    pub toast: Option<String>,
    /// True while the new-session name prompt is showing. The prompt
    /// buffers chars in `name_buf`.
    pub new_session_prompt: bool,
    pub name_buf: String,
    /// Last error from the new-session name prompt (e.g. "name in use").
    pub name_error: Option<String>,
    /// When the prompt is up, on confirm open the new session immediately
    /// (`N` keystroke); otherwise just rotate and return to the list
    /// (`n` keystroke).
    pub _new_and_open_on_confirm: bool,
}

impl SessionListState {
    pub fn load(store: RepoStore) -> Self {
        let mut sessions = store.sessions().list().unwrap_or_default();
        sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        let mut list_state = ListState::default();
        if !sessions.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            store,
            sessions,
            list_state,
            filter: String::new(),
            pending_delete: None,
            help_open: false,
            toast: None,
            new_session_prompt: false,
            name_buf: String::new(),
            name_error: None,
            _new_and_open_on_confirm: false,
        }
    }

    /// Re-list sessions from disk. Preserves the cursor where possible.
    pub fn refresh(&mut self) {
        let cursor_id = self
            .list_state
            .selected()
            .and_then(|i| self.visible().get(i).map(|s| s.id.clone()));
        self.sessions = self.store.sessions().list().unwrap_or_default();
        self.sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        let new_idx = cursor_id
            .as_ref()
            .and_then(|id| self.visible().iter().position(|s| &s.id == id));
        if new_idx.is_none() {
            let pick = if self.visible().is_empty() { None } else { Some(0) };
            self.list_state.select(pick);
        } else {
            self.list_state.select(new_idx);
        }
    }

    /// Sessions matching the current id-prefix filter (or all if filter is
    /// empty), in their stored (newest-first) order.
    pub fn visible(&self) -> Vec<&SessionMeta> {
        if self.filter.is_empty() {
            return self.sessions.iter().collect();
        }
        self.sessions
            .iter()
            .filter(|s| s.id.as_str().starts_with(&self.filter))
            .collect()
    }

    pub fn selected(&self) -> Option<&SessionMeta> {
        self.list_state.selected().and_then(|i| self.visible().get(i).copied())
    }

    pub fn on_action(&mut self, action: KeyAction) -> ListCmd {
        // If the new-session prompt is up, route keys there.
        if self.new_session_prompt {
            return self.handle_name_prompt(action);
        }
        // Any non-Delete action cancels a pending delete. (Including filter
        // keys, navigation, etc. — pressing `d` then `j` cancels the
        // pending delete and moves the cursor, which is the obvious UX.)
        if !matches!(action, KeyAction::Delete) {
            self.pending_delete = None;
        }

        match action {
            KeyAction::Down => {
                let v = self.visible();
                if !v.is_empty() {
                    let i = self.list_state.selected().unwrap_or(0);
                    let next = (i + 1).min(v.len() - 1);
                    self.list_state.select(Some(next));
                }
                ListCmd::Stay
            }
            KeyAction::Up => {
                let v = self.visible();
                if !v.is_empty() {
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
            KeyAction::NewSession | KeyAction::NewSessionAndOpen => {
                // Open a name prompt instead of rotating immediately.
                self.new_session_prompt = true;
                self.name_buf.clear();
                self.name_error = None;
                self.toast = None;
                self._new_and_open_on_confirm = matches!(action, KeyAction::NewSessionAndOpen);
                ListCmd::Stay
            }
            KeyAction::Delete => {
                let Some(session) = self.selected().cloned() else {
                    return ListCmd::Stay;
                };
                if session.is_open() {
                    self.toast = Some(format!(
                        "{} is open; run `grs new` (or press n) to close it first",
                        short_id(&session.id)
                    ));
                    return ListCmd::Stay;
                }
                match self.pending_delete.clone() {
                    None => {
                        self.pending_delete = Some(session.id.clone());
                        self.toast = Some(format!(
                            "press d again to delete {}",
                            short_id(&session.id)
                        ));
                    }
                    Some(pending_id) if pending_id == session.id => {
                        match self.store.delete_session(&session.id) {
                            Ok(()) => {
                                self.toast = Some(format!("deleted {}", short_id(&session.id)));
                                self.pending_delete = None;
                                self.refresh();
                            }
                            Err(GrsError::SessionOpen(_)) => {
                                self.toast = Some("session is open".to_string());
                                self.pending_delete = None;
                            }
                            Err(e) => {
                                self.toast = Some(format!("delete failed: {e}"));
                                self.pending_delete = None;
                            }
                        }
                    }
                    Some(_) => {
                        // Different session was pending; switch to this one.
                        self.pending_delete = Some(session.id.clone());
                        self.toast = Some(format!(
                            "press d again to delete {}",
                            short_id(&session.id)
                        ));
                    }
                }
                ListCmd::Stay
            }
            KeyAction::Filter => {
                self.filter.clear();
                self.toast = Some("filter: (type to filter by id prefix)".to_string());
                ListCmd::Stay
            }
            KeyAction::FilterChar(c) => {
                self.filter.push(c);
                // Keep the cursor on a row that's still visible.
                let v = self.visible();
                self.list_state
                    .select(if v.is_empty() { None } else { Some(0) });
                ListCmd::Stay
            }
            KeyAction::FilterBackspace => {
                self.filter.pop();
                let v = self.visible();
                self.list_state
                    .select(if v.is_empty() { None } else { Some(0) });
                ListCmd::Stay
            }
            KeyAction::ConfirmFilter => {
                self.toast = None;
                ListCmd::Stay
            }
            KeyAction::CancelFilter => {
                self.filter.clear();
                self.toast = None;
                let v = self.visible();
                self.list_state
                    .select(if v.is_empty() { None } else { Some(0) });
                ListCmd::Stay
            }
            KeyAction::Refresh => {
                self.refresh();
                self.toast = Some("refreshed".to_string());
                ListCmd::Stay
            }
            KeyAction::Help => {
                self.help_open = !self.help_open;
                ListCmd::Stay
            }
            KeyAction::Quit | KeyAction::Back => ListCmd::Quit,
            _ => ListCmd::Stay,
        }
    }

    /// Handle keys while the new-session name prompt is up. Enter
    /// confirms; Esc cancels; backspace pops; any char appends.
    fn handle_name_prompt(&mut self, action: KeyAction) -> ListCmd {
        match action {
            KeyAction::CancelFilter | KeyAction::Quit | KeyAction::Back => {
                self.new_session_prompt = false;
                self.name_buf.clear();
                self.name_error = None;
                self._new_and_open_on_confirm = false;
                ListCmd::Stay
            }
            KeyAction::ConfirmFilter | KeyAction::Enter => {
                let name = self.name_buf.trim().to_string();
                if name.is_empty() {
                    self.name_error = Some("name cannot be empty".to_string());
                    return ListCmd::Stay;
                }
                let now = now_ms();
                let result = self.store.rotate_open_session(name.clone(), now);
                self.new_session_prompt = false;
                self.name_buf.clear();
                let and_open = self._new_and_open_on_confirm;
                self._new_and_open_on_confirm = false;
                match result {
                    Ok(new_session) => {
                        self.refresh();
                        if let Some(idx) =
                            self.visible().iter().position(|s| s.id == new_session.id)
                        {
                            self.list_state.select(Some(idx));
                        }
                        self.toast = Some(format!("new session {}", short_id(&new_session.id)));
                        if and_open {
                            ListCmd::OpenCodeReview(new_session)
                        } else {
                            ListCmd::Stay
                        }
                    }
                    Err(e) => {
                        self.name_error = Some(format!("{e}"));
                        ListCmd::Stay
                    }
                }
            }
            KeyAction::FilterBackspace => {
                self.name_buf.pop();
                self.name_error = None;
                ListCmd::Stay
            }
            KeyAction::FilterChar(c) => {
                self.name_buf.push(c);
                self.name_error = None;
                ListCmd::Stay
            }
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
    .style(Style::default().bg(SCRUBBER_BG));
    f.render_widget(title, chunks[0]);

    // List
    let visible = state.visible();
    if visible.is_empty() {
        let msg = if state.sessions.is_empty() {
            "(no sessions yet — make a save in your editor to start one)"
        } else {
            "(no sessions match the filter)"
        };
        f.render_widget(
            Paragraph::new(msg)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(MUTED)),
                )
                .style(Style::default().fg(MUTED)),
            chunks[1],
        );
    } else {
        let items: Vec<ListItem> = visible.into_iter().map(row_for_session).collect();
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
    f.render_widget(status_line(state), chunks[2]);

    if state.help_open {
        render_help(f, area);
    }
    if state.new_session_prompt {
        render_name_prompt(f, area, state);
    }
}

fn render_name_prompt(f: &mut Frame, area: Rect, state: &SessionListState) {
    let popup_w = (area.width as i32 - 8).max(40) as u16;
    let popup_h = 7u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_w)) / 2;
    let y = (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(x, y, popup_w, popup_h);
    f.render_widget(Clear, popup);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("new session name", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(STATUS_FG)),
            Span::styled(state.name_buf.clone(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled("_", Style::default().fg(MUTED)),
        ]),
        Line::from(""),
    ];
    if let Some(err) = &state.name_error {
        lines.push(Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "enter to create, esc to cancel",
            Style::default().fg(MUTED),
        )));
    }
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT))
            .title(" new session "),
    );
    f.render_widget(p, popup);
}

fn status_line(state: &SessionListState) -> Paragraph<'static> {
    if let Some(toast) = &state.toast {
        let spans = vec![
            Span::styled(toast.clone(), Style::default().fg(ACCENT)),
            Span::raw("    "),
            Span::styled("j/k", Style::default().fg(ACCENT)),
            Span::raw(" move  "),
            Span::styled("Enter", Style::default().fg(ACCENT)),
            Span::raw(" open  "),
            Span::styled("n/N", Style::default().fg(ACCENT)),
            Span::raw(" new  "),
            Span::styled("d", Style::default().fg(ACCENT)),
            Span::raw(" del  "),
            Span::styled("/", Style::default().fg(ACCENT)),
            Span::raw(" filter  "),
            Span::styled("r", Style::default().fg(ACCENT)),
            Span::raw(" refresh  "),
            Span::styled("?", Style::default().fg(ACCENT)),
            Span::raw(" help  "),
            Span::styled("q", Style::default().fg(ACCENT)),
            Span::raw(" quit"),
        ];
        let style = if state.pending_delete.is_some() {
            Style::default().bg(STATUS_BG).fg(WARNING)
        } else {
            Style::default().bg(STATUS_BG).fg(STATUS_FG)
        };
        return Paragraph::new(Line::from(spans)).style(style);
    }
    if !state.filter.is_empty() {
        let spans = vec![
            Span::styled(" /", Style::default().fg(ACCENT)),
            Span::raw(state.filter.clone()),
            Span::styled("_", Style::default().fg(ACCENT)),
            Span::raw("    "),
            Span::styled("Esc", Style::default().fg(ACCENT)),
            Span::raw(" clear"),
        ];
        return Paragraph::new(Line::from(spans))
            .style(Style::default().bg(STATUS_BG).fg(STATUS_FG));
    }
    let spans = vec![
        Span::styled("j/k", Style::default().fg(ACCENT)),
        Span::raw(" move  "),
        Span::styled("Enter", Style::default().fg(ACCENT)),
        Span::raw(" open  "),
        Span::styled("n/N", Style::default().fg(ACCENT)),
        Span::raw(" new  "),
        Span::styled("d", Style::default().fg(ACCENT)),
        Span::raw(" del  "),
        Span::styled("/", Style::default().fg(ACCENT)),
        Span::raw(" filter  "),
        Span::styled("r", Style::default().fg(ACCENT)),
        Span::raw(" refresh  "),
        Span::styled("?", Style::default().fg(ACCENT)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::raw(" quit"),
    ];
    Paragraph::new(Line::from(spans)).style(Style::default().bg(STATUS_BG).fg(STATUS_FG))
}

fn render_help(f: &mut Frame, area: Rect) {
    let popup_w = (area.width as i32 - 8).max(20) as u16;
    let popup_h = 14u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_w)) / 2;
    let y = (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(x, y, popup_w, popup_h);

    f.render_widget(Clear, popup);

    let body = "\
Session list keys

  j / k         move down / up
  Enter         open the selected session in the code review
  n             create a new session, return to the list
  N             create a new session, open it immediately
  d             delete the selected closed session (press d again to confirm)
  /             start an id-prefix filter (Esc to clear)
  r             refresh the list from disk
  ?             toggle this help
  q / Esc       quit the TUI

In the code review view: j/k scroll, J/K 10-line jump,
gg/G top/bottom of the current snap, n/N next/prev change,
h/l prev/next snap, tab next file, q / Esc back to the list.
";
    let paragraph = Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(" help "),
        )
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(STATUS_FG));
    f.render_widget(paragraph, popup);
}

fn row_for_session(s: &SessionMeta) -> ListItem<'static> {
    let id_short: String = s.id.as_str().chars().take(10).collect();
    let status = if s.is_open() { "open  " } else { "closed" };
    let started = format_started(s.started_at);
    // Two-line row: name (bold, prominent) on top, id + status + snap
    // count + started on the bottom. The name was missing before; the
    // id-only row made it impossible to tell sessions apart at a
    // glance.
    let name_line = Line::from(Span::styled(
        s.name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let meta_line = Line::from(Span::styled(
        format!("{id_short}  {status}  {:>3} snaps  {started}", s.snap_count),
        Style::default().fg(MUTED),
    ));
    ListItem::new(vec![name_line, meta_line])
}

fn short_id(id: &SessionId) -> String {
    id.as_str().chars().take(10).collect()
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
