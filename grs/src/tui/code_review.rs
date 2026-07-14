//! Code review view — the TUI's diff viewer over a session's snaps.
//!
//! One file at a time, one snap at a time. Renders the current snap's
//! content with diff tints (added lines in green, removed lines in red
//! with the prior text inline — the unified-diff rendering is in
//! `crate::tui::highlight::render_snap`, rewritten in step 7).
//!
//! The keymap (handled in `CodeReviewState::on_action`):
//! - `j`/`k`               line down/up
//! - `J`/`K` (shift+j/k)   10-line jump
//! - `gg` / `G`            viewport top/bottom of the current snap's content
//! - `[` / `]`             prev / next snap in the session
//! - `tab`                 next file's first snap
//! - `r`                   manual refresh
//! - `?`                   help overlay (handled in the session list view;
//!                         code review just ignores it for now)
//! - `q` / `Esc`           back to the session list
//!
//! `n` / `N` are owned by the parser as `KeyAction::NewSession` /
//! `NewSessionAndOpen` and reach this view as no-ops today; step 7
//! repurposes them to next/prev-change-row jumps.

use crate::tui::file_view::{self, FileViewState};
use crate::tui::highlight::HighlightEngine;
use crate::tui::input::{KeyAction, VimParser};
use crate::tui::theme::{ACCENT, MUTED, SCRUBBER_BG, STATUS_BG, STATUS_FG};
use grs_lib::model::{Session, Snap};
use grs_lib::snap::{SnapEntry, SnapStore};
use grs_lib::store::RepoStore;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

/// Outcome of feeding a key into the code review view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeReviewCmd {
    Stay,
    /// Pop back to the session list.
    Pop,
}

pub struct CodeReviewState {
    pub store: RepoStore,
    pub session: Session,
    /// All snap *entries* for the session, sorted by capture timestamp.
    pub entries: Vec<SnapEntry>,
    /// Index into `entries` — the scrubber position.
    pub cur_snap_idx: usize,
    /// Distinct files (in first-appearance order); `cur_file_idx` cycles
    /// through these.
    pub files: Vec<String>,
    pub cur_file_idx: usize,
    /// Currently-loaded snap (the one being viewed).
    pub current_snap: Option<Snap>,
    /// Previous snap of the *same file* (used by `render_snap` to source
    /// the text of removed lines).
    pub prev_snap: Option<Snap>,
    /// Per-file state: which snap index (within `entries`) is the first
    /// one of that file — used by `tab` to jump to a file's first snap.
    pub file_first_idx: Vec<usize>,
    pub file_view: FileViewState,
    /// Cached rendered lines for `current_snap` (built once per snap, reused
    /// on every frame). When `None`, the view is empty. Built by
    /// `refresh_current` whenever the snap actually changes; never rebuilt
    /// on a no-op `refresh()`.
    pub cached_lines: Option<Vec<ratatui::text::Line<'static>>>,
    /// Seq of the last snap we actually loaded into `current_snap`. Used to
    /// detect *real* snap changes (vs. the periodic `refresh()` from the
    /// TUI's tick) so we don't yank the viewport back to the top on every
    /// background re-list.
    pub last_loaded_seq: Option<u32>,
    /// Cached mtime (seconds) of the session's snaps dir, used to short-
    /// circuit `refresh()` when no new snaps have landed.
    pub last_snaps_mtime: Option<i64>,
}

impl CodeReviewState {
    pub fn load(store: RepoStore, session: Session) -> Self {
        let mut entries = store.snaps().list(&session.id).unwrap_or_default();
        sort_entries_by_time(&mut entries);
        // Distinct files in first-appearance order.
        let mut files = Vec::new();
        let mut file_first_idx = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            if let Ok(snap) = SnapStore::read_path(&entry.path) {
                if !files.contains(&snap.file_path) {
                    file_first_idx.push(i);
                    files.push(snap.file_path);
                }
            }
        }
        let mut s = Self {
            store,
            session,
            entries,
            cur_snap_idx: 0,
            cur_file_idx: 0,
            files,
            current_snap: None,
            prev_snap: None,
            file_first_idx,
            file_view: FileViewState::default(),
            cached_lines: None,
            last_loaded_seq: None,
            last_snaps_mtime: None,
        };
        s.refresh_current();
        s
    }

    /// Re-list snaps from disk. Preserves the current snap by id, the file
    /// cursor, and the viewport (the file_view cache rebuild is driven by
    /// `refresh_current` only when the snap actually changes).
    ///
    /// Cheap fast path: stat the session's snaps dir and short-circuit if
    /// its mtime hasn't changed since the last refresh. This is O(1) on a
    /// no-op (one syscall) vs. O(N) for a full re-list with parse.
    pub fn refresh(&mut self) {
        let snaps_dir = self.store.paths().session_snaps(&self.session.id);
        let mtime = std::fs::metadata(&snaps_dir)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        if let (Some(prev), Some(now)) = (self.last_snaps_mtime, mtime) {
            if prev == now {
                return;
            }
        }
        self.last_snaps_mtime = mtime;

        let current_file = self.current_file().map(|s| s.to_string());
        let current_seq = self.last_loaded_seq;
        self.entries = self.store.snaps().list(&self.session.id).unwrap_or_default();
        sort_entries_by_time(&mut self.entries);
        self.files.clear();
        self.file_first_idx.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            if let Ok(snap) = SnapStore::read_path(&entry.path) {
                if !self.files.contains(&snap.file_path) {
                    self.file_first_idx.push(i);
                    self.files.push(snap.file_path);
                }
            }
        }
        // Keep the cursor on the same snap if it still exists.
        if let Some(seq) = current_seq {
            if let Some(pos) = self.entries.iter().position(|e| e.seq == seq) {
                self.cur_snap_idx = pos;
            } else {
                self.cur_snap_idx = self.entries.len().saturating_sub(1);
            }
        } else if self.cur_snap_idx >= self.entries.len() {
            self.cur_snap_idx = self.entries.len().saturating_sub(1);
        }
        // Keep the file cursor pointing at the same file if possible.
        if let Some(file) = &current_file {
            if let Some(pos) = self.files.iter().position(|f| f == file) {
                self.cur_file_idx = pos;
            }
        }
        self.refresh_current();
    }

    pub fn current_file(&self) -> Option<&str> {
        self.current_snap.as_ref().map(|s| s.file_path.as_str())
    }

    pub fn refresh_current(&mut self) {
        let new_snap = self
            .entries
            .get(self.cur_snap_idx)
            .and_then(|e| SnapStore::read_path(&e.path).ok());
        let new_seq = new_snap.as_ref().map(|s| s.seq);
        if new_snap.is_none() {
            self.current_snap = None;
            self.file_view.lines.clear();
            self.cached_lines = None;
            self.prev_snap = None;
            self.last_loaded_seq = None;
            return;
        }
        let prev_seq = new_snap.as_ref().and_then(|s| s.prev_seq);
        self.prev_snap = prev_seq.and_then(|seq| {
            self.entries
                .iter()
                .find(|e| e.seq == seq)
                .and_then(|e| SnapStore::read_path(&e.path).ok())
        });
        // Only rebuild the rendered line vec when the snap actually
        // changes. The TUI calls `refresh()` periodically to pick up new
        // captures; without this guard, every refresh would yank the
        // viewport back to the top AND re-run highlight on every line of
        // the file — which is the dominant per-frame cost.
        if new_seq != self.last_loaded_seq {
            self.file_view.scroll = 0;
            // new_snap is Some(_) here (we returned early on None above).
            let snap_ref = new_snap.as_ref().expect("new_snap is Some after the early return");
            let prev = self.prev_snap.clone();
            self.rebuild_cached_lines(snap_ref, &prev);
        }
        self.current_snap = new_snap;
        self.last_loaded_seq = new_seq;
    }

    fn rebuild_cached_lines(
        &mut self,
        snap: &Snap,
        prev_snap: &Option<Snap>,
    ) {
        let prev_content = prev_snap
            .as_ref()
            .filter(|p| p.file_path == snap.file_path)
            .map(|p| p.content.as_str())
            .unwrap_or("");
        // A fresh HighlightEngine is needed to call `syntax_for` (mutable
        // due to the cache). We don't keep the engine on CodeReviewState
        // because the shell owns it for the entire view lifetime; the
        // rebuild path is on snap change (rare), so constructing one here
        // is fine.
        //
        // ...actually, the render path is given the engine, and we want to
        // avoid constructing one here. Instead, expose the build as a free
        // function and have `render` call it when the cache is empty.
        // For now, keep the engine out of state by inlining the build
        // here with a local engine — this is a one-shot per snap change.
        let mut engine = crate::tui::highlight::HighlightEngine::new(
            &self.store.config().tui.syntax_theme,
        );
        let lines = crate::tui::highlight::render_snap(
            &mut engine,
            prev_content,
            &snap.content,
            &snap.file_path,
            true,
        );
        self.cached_lines = Some(lines.clone());
        self.file_view.lines = lines;
    }

    pub fn on_action(
        &mut self,
        action: KeyAction,
        _parser: &mut VimParser,
    ) -> CodeReviewCmd {
        match action {
            KeyAction::Down => {
                self.file_view.scroll = self.file_view.scroll.saturating_add(1);
                CodeReviewCmd::Stay
            }
            KeyAction::Up => {
                self.file_view.scroll = self.file_view.scroll.saturating_sub(1);
                CodeReviewCmd::Stay
            }
            KeyAction::JumpDown10 => {
                self.file_view.scroll = self.file_view.scroll.saturating_add(10);
                CodeReviewCmd::Stay
            }
            KeyAction::JumpUp10 => {
                self.file_view.scroll = self.file_view.scroll.saturating_sub(10);
                CodeReviewCmd::Stay
            }
            KeyAction::GotoFirst => {
                self.file_view.scroll = 0;
                CodeReviewCmd::Stay
            }
            KeyAction::GotoLast => {
                // Bottom of the *content*, not the snap list. We don't know
                // the exact content length here (render_snap builds it on
                // demand); the render pass clamps to a valid scroll anyway.
                self.file_view.scroll = u16::MAX;
                CodeReviewCmd::Stay
            }
            KeyAction::PrevSnap => {
                if self.cur_snap_idx > 0 {
                    self.cur_snap_idx -= 1;
                    self.refresh_current();
                }
                CodeReviewCmd::Stay
            }
            KeyAction::NextSnap => {
                if self.cur_snap_idx + 1 < self.entries.len() {
                    self.cur_snap_idx += 1;
                    self.refresh_current();
                }
                CodeReviewCmd::Stay
            }
            KeyAction::TabFile => {
                if !self.files.is_empty() {
                    self.cur_file_idx = (self.cur_file_idx + 1) % self.files.len();
                    if let Some(&idx) = self.file_first_idx.get(self.cur_file_idx) {
                        self.cur_snap_idx = idx;
                        self.refresh_current();
                    }
                }
                CodeReviewCmd::Stay
            }
            KeyAction::Refresh => {
                self.refresh();
                CodeReviewCmd::Stay
            }
            KeyAction::Quit | KeyAction::Back => CodeReviewCmd::Pop,
            // n/N: next/prev change row in the current snap. The parser
            // emits these as NewSession/NewSessionAndOpen (the names are
            // accurate for the session list view; the code review view
            // reinterprets them). No-op at the boundary (no status change).
            KeyAction::NewSession => {
                self.file_view.scroll = jump_to_change(
                    &self.file_view.lines,
                    self.file_view.scroll,
                    true,
                );
                CodeReviewCmd::Stay
            }
            KeyAction::NewSessionAndOpen => {
                self.file_view.scroll = jump_to_change(
                    &self.file_view.lines,
                    self.file_view.scroll,
                    false,
                );
                CodeReviewCmd::Stay
            }
            _ => CodeReviewCmd::Stay,
        }
    }
}

/// Find the next (or previous) change row in `lines` relative to `cur`,
/// and set `file_view.scroll` so that row is at the top of the viewport.
/// Change rows are any with a non-default background style (added or
/// removed). At the boundary: no-op.
fn jump_to_change(lines: &[ratatui::text::Line<'_>], cur: u16, forward: bool) -> u16 {
    use ratatui::style::Color;
    let is_change = |l: &ratatui::text::Line<'_>| -> bool {
        matches!(l.style.bg, Some(c) if c != Color::Reset)
    };
    if forward {
        if let Some((i, _)) = lines
            .iter()
            .enumerate()
            .skip((cur as usize) + 1)
            .find(|(_, l)| is_change(l))
        {
            return i as u16;
        }
    } else {
        let cur_us = cur as usize;
        for i in (0..cur_us).rev() {
            if is_change(&lines[i]) {
                return i as u16;
            }
        }
    }
    cur
}

pub fn render(
    f: &mut Frame,
    state: &mut CodeReviewState,
    _engine: &mut HighlightEngine,
) {
    let area = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // scrubber
            Constraint::Length(1), // progress
            Constraint::Min(5),    // file
            Constraint::Length(1), // status
        ])
        .split(area);

    // Scrubber
    let step = state.cur_snap_idx + 1;
    let total = state.entries.len();
    let file_idx = state.cur_file_idx + 1;
    let file_total = state.files.len();
    let cur_file = state.current_file().unwrap_or("?");
    let id_short: String = state.session.id.as_str().chars().take(10).collect();

    let scrubber = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" code {id_short} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{cur_file}  ")),
        Span::styled(
            format!("snap {step}/{total}  file {file_idx}/{file_total}"),
            Style::default().fg(STATUS_FG),
        ),
    ]))
    .style(Style::default().bg(SCRUBBER_BG));
    f.render_widget(scrubber, chunks[0]);

    // Progress gauge.
    let ratio = if total == 0 {
        0.0
    } else {
        step as f64 / total as f64
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::NONE))
        .gauge_style(Style::default().fg(ACCENT).bg(STATUS_BG))
        .ratio(ratio)
        .label(format!("{step}/{total}"));
    f.render_widget(gauge, chunks[1]);

    // File content
    let file_area = chunks[2];
    if let Some(snap) = state.current_snap.clone() {
        // Reuse the cached lines from refresh_current. If for any reason
        // the cache is empty (e.g. on the very first frame after load
        // before refresh_current has been called by the shell), fall back
        // to a one-shot render. (In practice refresh_current is called
        // from `load`, so the cache is always populated by the time we
        // get here.)
        if state.cached_lines.is_none() {
            let prev = state.prev_snap.clone();
            state.rebuild_cached_lines(&snap, &prev);
        }
        file_view::render(
            f,
            &mut state.file_view,
            file_area,
            Some(&format!(" {} (seq {}) ", snap.file_path, snap.seq)),
        );
    } else {
        f.render_widget(
            Paragraph::new("(no snap at this position)")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(MUTED)),
                )
                .style(Style::default().fg(MUTED)),
            file_area,
        );
    }

    // Status
    let status = Line::from(vec![
        Span::styled("j/k", Style::default().fg(ACCENT)),
        Span::raw(" scroll  "),
        Span::styled("J/K", Style::default().fg(ACCENT)),
        Span::raw(" 10-line  "),
        Span::styled("gg/G", Style::default().fg(ACCENT)),
        Span::raw(" jump  "),
        Span::styled("[/]", Style::default().fg(ACCENT)),
        Span::raw(" snap  "),
        Span::styled("tab", Style::default().fg(ACCENT)),
        Span::raw(" file  "),
        Span::styled("r", Style::default().fg(ACCENT)),
        Span::raw(" refresh  "),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::raw(" back"),
    ]);
    f.render_widget(
        Paragraph::new(status).style(Style::default().bg(STATUS_BG).fg(STATUS_FG)),
        chunks[3],
    );
}

fn sort_entries_by_time(entries: &mut [SnapEntry]) {
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.seq.cmp(&b.seq)));
}
