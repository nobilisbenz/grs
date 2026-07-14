//! Replay view — the timelapse. One file at a time, one snap at a time,
//! with syntax highlighting and diff tinting.

use crate::tui::file_view::{self, FileViewState};
use crate::tui::highlight::{render_snap, HighlightEngine};
use crate::tui::input::{KeyAction, VimParser};
use crate::tui::theme::{ACCENT, MUTED, SCRUBBER_BG, STATUS_BG, STATUS_FG};
use grs_lib::model::Session;
use grs_lib::snap::SnapEntry;
use grs_lib::store::RepoStore;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;
use std::time::Instant;

pub struct ReplayState {
    pub store: RepoStore,
    pub session: Session,
    /// All snap *entries* for the session, sorted by capture timestamp
    /// (i.e. real wall-clock time). Sorting by `seq` is *not* enough because
    /// the watcher historically assigned per-file seqs and produced
    /// collisions; the timestamp is the only field that reflects when a
    /// change actually happened, which is what the timelapse needs.
    pub entries: Vec<SnapEntry>,
    /// Index into `entries` — the scrubber position.
    pub cur_snap_idx: usize,
    /// Distinct files (in first-appearance order); `cur_file_idx` cycles
    /// through these.
    pub files: Vec<String>,
    pub cur_file_idx: usize,
    /// Currently-loaded snap (the one being viewed).
    pub current_snap: Option<grs_lib::model::Snap>,
    /// Previous snap of the *same file* (for side-by-side rendering).
    pub prev_snap: Option<grs_lib::model::Snap>,
    /// Per-file state: which snap index (within `entries`) is the first
    /// one of that file — used by `tab` to jump to a file's first snap.
    pub file_first_idx: Vec<usize>,
    pub file_view: FileViewState,
    pub playing: bool,
    pub speed_ms: u64,
    pub last_tick: Instant,
    /// `s` toggles side-by-side view.
    pub side_by_side: bool,
    /// Seq of the last snap we actually loaded into `current_snap`. Used to
    /// detect *real* snap changes (vs. the periodic `refresh()` from the
    /// TUI's tick) so we don't yank the viewport back to the top on every
    /// background re-list.
    pub last_loaded_seq: Option<u32>,
}

impl ReplayState {
    pub fn load(store: RepoStore, session: Session) -> Self {
        let mut entries = store.snaps().list(&session.id).unwrap_or_default();
        // Sort by capture timestamp (real wall-clock order) instead of seq,
        // which only reflects per-file counters and collides across files.
        sort_entries_by_time(&mut entries);
        // Distinct files in first-appearance order.
        let mut files = Vec::new();
        let mut file_first_idx = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            if let Ok(snap) = grs_lib::snap::SnapStore::read_path(&entry.path) {
                if !files.contains(&snap.file_path) {
                    file_first_idx.push(i);
                    files.push(snap.file_path);
                }
            }
        }
        let speed_ms = 600_u64.max(50);
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
            playing: false,
            speed_ms,
            last_tick: Instant::now(),
            side_by_side: false,
            last_loaded_seq: None,
        };
        s.refresh_current();
        s
    }

    pub fn refresh(&mut self) {
        let old_len = self.entries.len();
        self.entries = self.store.snaps().list(&self.session.id).unwrap_or_default();
        sort_entries_by_time(&mut self.entries);
        self.files.clear();
        self.file_first_idx.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            if let Ok(snap) = grs_lib::snap::SnapStore::read_path(&entry.path) {
                if !self.files.contains(&snap.file_path) {
                    self.file_first_idx.push(i);
                    self.files.push(snap.file_path);
                }
            }
        }
        if self.cur_snap_idx >= self.entries.len() {
            self.cur_snap_idx = self.entries.len().saturating_sub(1);
        }
        // If new snaps arrived while playing, keep playing.
        if self.playing && old_len > 0 && self.entries.len() > old_len && self.cur_snap_idx + 1 >= old_len {
            self.last_tick = Instant::now();
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
            .and_then(|e| grs_lib::snap::SnapStore::read_path(&e.path).ok());
        let new_seq = new_snap.as_ref().map(|s| s.seq);
        if new_snap.is_none() {
            self.current_snap = None;
            self.file_view.lines.clear();
            self.prev_snap = None;
            self.last_loaded_seq = None;
            return;
        }
        // Find the previous snap of the same file (if any) for side-by-side.
        let prev_seq = new_snap.as_ref().and_then(|s| s.prev_seq);
        self.prev_snap = prev_seq.and_then(|seq| {
            self.entries
                .iter()
                .find(|e| e.seq == seq)
                .and_then(|e| grs_lib::snap::SnapStore::read_path(&e.path).ok())
        });
        // Only reset scroll when the snap actually changes. The TUI calls
        // `refresh()` once a second just to pick up live captures; without
        // this guard, every periodic refresh would yank the viewport back
        // to the top of the file and the user can't read anything.
        if new_seq != self.last_loaded_seq {
            self.file_view.scroll = 0;
        }
        self.current_snap = new_snap;
        self.last_loaded_seq = new_seq;
    }

    /// Auto-advance for play mode. Returns true if we moved.
    pub fn tick(&mut self) -> bool {
        if !self.playing {
            return false;
        }
        if self.last_tick.elapsed().as_millis() as u64 >= self.speed_ms {
            self.last_tick = Instant::now();
            if self.cur_snap_idx + 1 < self.entries.len() {
                self.cur_snap_idx += 1;
                self.refresh_current();
                return true;
            } else {
                self.playing = false;
            }
        }
        false
    }

    pub fn on_action(
        &mut self,
        action: KeyAction,
        parser: &mut VimParser,
    ) -> ReplayOutcome {
        match action {
            KeyAction::StepFwd => {
                parser.reset();
                if self.cur_snap_idx + 1 < self.entries.len() {
                    self.cur_snap_idx += 1;
                    self.refresh_current();
                } else {
                    self.playing = false;
                }
            }
            KeyAction::StepBack => {
                parser.reset();
                if self.cur_snap_idx > 0 {
                    self.cur_snap_idx -= 1;
                    self.refresh_current();
                }
            }
            KeyAction::Down => {
                parser.reset();
                self.file_view.scroll = self.file_view.scroll.saturating_add(1);
            }
            KeyAction::Up => {
                parser.reset();
                self.file_view.scroll = self.file_view.scroll.saturating_sub(1);
            }
            KeyAction::PlayPause => {
                parser.reset();
                if !self.entries.is_empty() {
                    self.playing = !self.playing;
                    if self.playing {
                        self.last_tick = Instant::now();
                        // If at end, restart from beginning.
                        if self.cur_snap_idx + 1 >= self.entries.len() {
                            self.cur_snap_idx = 0;
                            self.refresh_current();
                        }
                    }
                }
            }
            KeyAction::Faster => {
                parser.reset();
                self.speed_ms = (self.speed_ms.saturating_sub(100)).max(50);
            }
            KeyAction::Slower => {
                parser.reset();
                self.speed_ms = (self.speed_ms + 100).min(5_000);
            }
            KeyAction::GotoFirst => {
                parser.reset();
                self.cur_snap_idx = 0;
                self.refresh_current();
            }
            KeyAction::GotoLast => {
                parser.reset();
                if !self.entries.is_empty() {
                    self.cur_snap_idx = self.entries.len() - 1;
                    self.refresh_current();
                }
            }
            KeyAction::GotoSnap(n) => {
                parser.reset();
                if n >= 1 && n <= self.entries.len() {
                    self.cur_snap_idx = n - 1;
                    self.refresh_current();
                }
            }
            KeyAction::TabFile => {
                parser.reset();
                if !self.files.is_empty() {
                    self.cur_file_idx = (self.cur_file_idx + 1) % self.files.len();
                    if let Some(&idx) = self.file_first_idx.get(self.cur_file_idx) {
                        self.cur_snap_idx = idx;
                        self.refresh_current();
                    }
                }
            }
            KeyAction::Refresh => {
                parser.reset();
                self.refresh();
            }
            KeyAction::SideBySide => {
                parser.reset();
                self.side_by_side = !self.side_by_side;
            }
            KeyAction::Quit | KeyAction::Back => {
                parser.reset();
                return ReplayOutcome::Quit;
            }
            _ => {}
        }
        ReplayOutcome::Stay
    }
}

pub enum ReplayOutcome {
    Stay,
    Quit,
}

pub fn render(
    f: &mut Frame,
    state: &mut ReplayState,
    engine: &mut HighlightEngine,
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
    let play = if state.playing { "▶" } else { "⏸" };
    let step = state.cur_snap_idx + 1;
    let total = state.entries.len();
    let file_idx = state.cur_file_idx + 1;
    let file_total = state.files.len();
    let cur_file = state.current_file().unwrap_or("?");
    let id_short: String = state.session.id.as_str().chars().take(10).collect();

    let scrubber = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" replay {id_short} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{cur_file}  ")),
        Span::styled(
            format!("step {step}/{total}  {play} {}ms  tab {file_idx}/{file_total}", state.speed_ms),
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
        if state.side_by_side {
            render_side_by_side(f, engine, state, &snap, file_area);
        } else {
            let lines = render_snap(
                engine,
                &snap.content,
                &snap.file_path,
                &snap.diff.added_lines,
                &snap.diff.removed_lines,
                true,
            );
            state.file_view.lines = lines;
            file_view::render(
                f,
                &mut state.file_view,
                file_area,
                Some(&format!(" {} (seq {}) ", snap.file_path, snap.seq)),
            );
        }
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
    let sbs_label = if state.side_by_side { "on" } else { "off" };
    let status = Line::from(vec![
        Span::styled("j/k", Style::default().fg(ACCENT)),
        Span::raw(" scroll  "),
        Span::styled("h/l", Style::default().fg(ACCENT)),
        Span::raw(" step  "),
        Span::styled("space", Style::default().fg(ACCENT)),
        Span::raw(" play  "),
        Span::styled("+/-", Style::default().fg(ACCENT)),
        Span::raw(" speed  "),
        Span::styled("gg/G", Style::default().fg(ACCENT)),
        Span::raw(" jump  "),
        Span::styled(":N", Style::default().fg(ACCENT)),
        Span::raw(" goto  "),
        Span::styled("tab", Style::default().fg(ACCENT)),
        Span::raw(" file  "),
        Span::styled("s", Style::default().fg(ACCENT)),
        Span::raw(format!(" split:{sbs_label}  ")),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::raw(" quit"),
    ]);
    f.render_widget(
        Paragraph::new(status).style(Style::default().bg(STATUS_BG).fg(STATUS_FG)),
        chunks[3],
    );
}

/// Side-by-side: render `prev_snap` (or empty) on the left half, `current_snap`
/// on the right half, each with their own diff tints.
fn render_side_by_side(
    f: &mut Frame,
    engine: &mut HighlightEngine,
    state: &mut ReplayState,
    cur: &grs_lib::model::Snap,
    area: ratatui::layout::Rect,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let right_lines = render_snap(
        engine,
        &cur.content,
        &cur.file_path,
        &cur.diff.added_lines,
        &cur.diff.removed_lines,
        true,
    );

    if let Some(prev) = &state.prev_snap {
        let left_lines = render_snap(
            engine,
            &prev.content,
            &prev.file_path,
            &prev.diff.added_lines,
            &prev.diff.removed_lines,
            true,
        );
        let left = pad_lines(left_lines, right_lines.len());
        let right = pad_lines(right_lines, left.len());
        let title_l = format!(" prev (seq {}) ", prev.seq);
        let title_r = format!(" current (seq {}) ", cur.seq);
        f.render_widget(
            Paragraph::new(left)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title_l)
                        .border_style(Style::default().fg(MUTED)),
                )
                .scroll((state.file_view.scroll, 0)),
            cols[0],
        );
        f.render_widget(
            Paragraph::new(right)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title_r)
                        .border_style(Style::default().fg(ACCENT)),
                )
                .scroll((state.file_view.scroll, 0)),
            cols[1],
        );
    } else {
        f.render_widget(
            Paragraph::new(right_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" current (seq {}) ", cur.seq))
                        .border_style(Style::default().fg(ACCENT)),
                )
                .scroll((state.file_view.scroll, 0)),
            area,
        );
    }
}

fn pad_lines(mut lines: Vec<ratatui::text::Line<'static>>, n: usize) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::Line;
    while lines.len() < n {
        lines.push(Line::from(""));
    }
    lines
}

/// Sort snap entries by capture timestamp (then seq as a tiebreaker for
/// snaps that share a millisecond). This is the order the timelapse plays
/// them in; relying on `seq` is wrong because the watcher can issue the
/// same `seq` to different files.
fn sort_entries_by_time(entries: &mut [SnapEntry]) {
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.seq.cmp(&b.seq)));
}
