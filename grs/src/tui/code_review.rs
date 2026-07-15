//! Code review view — the TUI's diff viewer over a session's snaps.
//!
//! Each snap is shown as a chronological stream of all the changes
//! (adds/removes/deletes) across every file that changed in that
//! moment. The view is **not** file-by-file: every change in the snap
//! is laid out top-to-bottom, with a `─── file: path ───` header
//! before each file's diff. Unchanged files do not appear at all
//! (they're not in the snap JSON, by storage design).
//!
//! Navigation:
//! - `j`/`k`               line down/up
//! - `J`/`K` (shift+j/k)   10-line jump
//! - `gg` / `G`            viewport top/bottom of the current snap
//! - `[` / `]` or `h`/`l`  prev / next snap in the session
//! - `n` / `N`             next / prev change row
//! - `r`                   manual refresh
//! - `?`                   help overlay
//! - `q` / `Esc`           back to the session list
//!
//! When the view opens, it lands on the **latest** snap (newest). The
//! user can `h` to walk backward in time.

use crate::tui::file_view::{self, FileViewState};
use crate::tui::highlight::HighlightEngine;
use crate::tui::input::{KeyAction, VimParser};
use crate::tui::theme::{
    ACCENT, MUTED, REMOVED_BG, ADDED_BG, SCRUBBER_BG, STATUS_BG, STATUS_FG,
};
use grs_lib::model::{SessionMeta, SnapJson};
use grs_lib::store::RepoStore;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;
// use std::collections::BTreeMap; // (removed: no longer needed)

/// Outcome of feeding a key into the code review view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeReviewCmd {
    Stay,
    /// Pop back to the session list.
    Pop,
}

pub struct CodeReviewState {
    pub store: RepoStore,
    pub session: SessionMeta,
    /// All snap entries for the session, sorted by `n` ascending.
    pub entries: Vec<grs_lib::snap::SnapEntry>,
    /// Index into `entries` — the scrubber position. Lands on the
    /// **latest** snap when the view opens.
    pub cur_snap_idx: usize,
    /// Cached mtime (seconds) of the session dir, used to short-circuit
    /// `refresh()` when no new snaps have landed.
    pub last_session_mtime: Option<i64>,
    /// Cached rendered lines for the current snap. Rebuilt only when
    /// the snap actually changes.
    pub cached_lines: Option<Vec<Line<'static>>>,
    /// `n` of the snap whose lines are in `cached_lines`. `None` means
    /// the cache is empty.
    pub cached_snap_n: Option<u32>,
    /// `file_path` of the snap whose lines are in `cached_lines`.
    /// Used as the title in the file-view header.
    pub cached_file_path: Option<String>,
    pub file_view: FileViewState,
    /// True while the help overlay is shown.
    pub help_open: bool,
}

impl CodeReviewState {
    pub fn load(store: RepoStore, session: SessionMeta) -> Self {
        let mut s = Self {
            store,
            session,
            entries: Vec::new(),
            // Default to the latest snap. `refresh` clamps this to
            // `entries.len().saturating_sub(1)` once the list is known.
            cur_snap_idx: 0,
            last_session_mtime: None,
            cached_lines: None,
            cached_snap_n: None,
            cached_file_path: None,
            file_view: FileViewState::default(),
            help_open: false,
        };
        s.refresh();
        s
    }

    /// Re-list snaps from disk. Preserves the cursor where possible.
    /// Cheap fast path: stat the session dir mtime and skip if unchanged.
    pub fn refresh(&mut self) {
        let session_dir = self.store.paths().session_dir(&self.session.id);
        let mtime = std::fs::metadata(&session_dir)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        if let (Some(prev), Some(now)) = (self.last_session_mtime, mtime) {
            if prev == now {
                return;
            }
        }
        self.last_session_mtime = mtime;

        let prev_cached_n = self.cached_snap_n;
        self.entries = self.store.snaps().list(&self.session.id).unwrap_or_default();
        // Keep the cursor on the same snap if possible; otherwise jump
        // to the **latest** snap (the user's natural "what just
        // changed" entry point).
        if let Some(n) = prev_cached_n {
            if let Some(pos) = self.entries.iter().position(|e| e.n == n) {
                self.cur_snap_idx = pos;
            } else if self.entries.is_empty() {
                self.cur_snap_idx = 0;
            } else {
                self.cur_snap_idx = self.entries.len() - 1;
            }
        } else if self.entries.is_empty() {
            self.cur_snap_idx = 0;
        } else {
            self.cur_snap_idx = self.entries.len() - 1;
        }
        self.refresh_current();
    }

    pub fn theme_name(&self) -> String {
        self.store.config().tui.syntax_theme.clone()
    }

    /// Reload the current snap from disk and rebuild the rendered line
    /// stream. Cheap if the snap hasn't actually changed.
    pub fn refresh_current(&mut self) {
        let snap_n = self.entries.get(self.cur_snap_idx).map(|e| e.n);
        if snap_n == self.cached_snap_n && self.cached_lines.is_some() {
            return;
        }
        let Some(snap_n) = snap_n else {
            self.cached_lines = None;
            self.cached_snap_n = None;
            self.cached_file_path = None;
            self.file_view.lines.clear();
            return;
        };
        let snap = match self.store.snaps().read(&self.session.id, snap_n) {
            Ok(s) => s,
            Err(_) => {
                self.cached_lines = None;
                self.cached_snap_n = None;
                self.cached_file_path = None;
                self.file_view.lines.clear();
                return;
            }
        };
        // Each snap is one file's change. prev_content (if any) is
        // inlined in the JSON — no need to walk back for the
        // immediately previous snap.
        let lines = render_snap_stream(&snap);
        self.file_view.lines = lines.clone();
        self.cached_lines = Some(lines);
        self.cached_snap_n = Some(snap_n);
        self.cached_file_path = Some(snap.file_path.clone());
        // Reset scroll on a real change.
        self.file_view.scroll = 0;
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
            KeyAction::TabFile => CodeReviewCmd::Stay, // no-op; per-file cursor is gone
            KeyAction::Refresh => {
                self.refresh();
                CodeReviewCmd::Stay
            }
            KeyAction::Quit => CodeReviewCmd::Pop,
            KeyAction::Help => {
                self.help_open = !self.help_open;
                CodeReviewCmd::Stay
            }
            KeyAction::NewSession | KeyAction::NewSessionAndOpen => {
                // Reinterpreted in this view: jump to the next / prev
                // change row.
                let forward = matches!(action, KeyAction::NewSession);
                if let Some(lines) = self.cached_lines.as_ref() {
                    self.file_view.scroll = jump_to_change(
                        lines,
                        self.file_view.scroll,
                        forward,
                    );
                }
                CodeReviewCmd::Stay
            }
            _ => CodeReviewCmd::Stay,
        }
    }
}

/// Find the next (or previous) change row in `lines` relative to `cur`.
/// A change row is one whose background is `ADDED_BG` or `REMOVED_BG`.
fn jump_to_change(lines: &[Line<'_>], cur: u16, forward: bool) -> u16 {
    let is_change = |l: &Line<'_>| -> bool {
        matches!(l.style.bg, Some(c) if c == ADDED_BG || c == REMOVED_BG)
    };
    if forward {
        if let Some((i, _)) = lines
            .iter()
            .enumerate()
            .skip((cur as usize).saturating_add(1))
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

/// Render a snap's single file as a stream of lines. Each snap is
/// one file's change (new, modified, or deleted). The snap's
/// `file_path` is the title; `files[0]` carries the data.
fn render_snap_stream(
    snap: &SnapJson,
) -> Vec<Line<'static>> {
    let Some(file) = snap.files.first() else {
        // Empty snap (no files changed). Just show a placeholder.
        return vec![Line::from(Span::styled(
            "(no change)",
            Style::default().fg(MUTED),
        ))];
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    if file.removed {
        // Render the prior content as red "removed" rows (the user
        // wanted to see what was here).
        let prev_text = file.prev_content.as_deref().unwrap_or("");
        for (i, line) in prev_text.lines().enumerate() {
            out.push(Line::from(Span::styled(
                format!("{:>4} - {}", i + 1, line),
                Style::default().fg(Color::LightRed).bg(REMOVED_BG),
            )));
        }
        return out;
    }
    if file.binary {
        // Just a placeholder; no line-level diff.
        out.push(Line::from(Span::styled(
            format!("(binary file, {} bytes)", file.size),
            Style::default().fg(MUTED),
        )));
        return out;
    }
    // Text modification: the prev_content is in the JSON. (For
    // binary transitions or new files where prev_content is None,
    // the diff shows everything as added.)
    let prev_text = file.prev_content.as_deref().unwrap_or("");
    out.extend(render_unified_diff(prev_text, &file.content));
    out
}

/// Render a unified-diff style block between `prev` and `cur`. The
/// output is one line per change:
/// - `+ line` for added lines (green bg)
/// - `- line` for removed lines (red bg)
/// - `  line` for unchanged lines (no bg)
fn render_unified_diff(prev: &str, cur: &str) -> Vec<Line<'static>> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(prev, cur);
    let mut out: Vec<Line<'static>> = Vec::new();
    for change in diff.iter_all_changes() {
        let tag = change.tag();
        let text = change.value().trim_end_matches('\n');
        let (sign, line_no, bg) = match tag {
            ChangeTag::Equal => (" ", change.new_index().map(|i| i + 1), None),
            ChangeTag::Insert => ("+", change.new_index().map(|i| i + 1), Some(ADDED_BG)),
            ChangeTag::Delete => ("-", change.old_index().map(|i| i + 1), Some(REMOVED_BG)),
        };
        let n = line_no.unwrap_or(0);
        let prefix = format!("{n:>4} {sign} ");
        let style = match bg {
            Some(c) => Style::default().bg(c),
            None => Style::default(),
        };
        let spans = vec![
            Span::styled(prefix, style),
            Span::styled(text.to_string(), style),
        ];
        out.push(Line::from(spans).style(style));
    }
    out
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
            Constraint::Length(1), // scrubber
            Constraint::Length(1), // progress
            Constraint::Min(5),    // file
            Constraint::Length(1), // status
        ])
        .split(area);

    // Scrubber
    let step = state.cur_snap_idx + 1;
    let total = state.entries.len();
    let id_short: String = state.session.id.as_str().chars().take(10).collect();

    let scrubber_text = if total == 0 {
        format!(" code {id_short}  (no snaps) ")
    } else {
        format!(" code {id_short}  snap {step}/{total} ")
    };
    let scrubber = Paragraph::new(Line::from(vec![Span::styled(
        scrubber_text,
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )]))
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

    // Snap content
    let file_area = chunks[2];
    if state.cached_lines.is_none() {
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
    } else {
        let snap_n = state.cached_snap_n.unwrap_or(0);
        let path = state.cached_file_path.as_deref().unwrap_or("?");
        file_view::render(
            f,
            &mut state.file_view,
            file_area,
            Some(&format!(" snap {snap_n} · {path} ")),
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
        Span::styled("h/l", Style::default().fg(ACCENT)),
        Span::raw(" snap  "),
        Span::styled("n/N", Style::default().fg(ACCENT)),
        Span::raw(" change  "),
        Span::styled("?", Style::default().fg(ACCENT)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::raw(" back"),
    ]);
    f.render_widget(
        Paragraph::new(status).style(Style::default().bg(STATUS_BG).fg(STATUS_FG)),
        chunks[3],
    );

    if state.help_open {
        render_help(f, area);
    }
}

fn render_help(f: &mut Frame, area: Rect) {
    let popup_w = (area.width as i32 - 8).max(40) as u16;
    let popup_h = 18u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_w)) / 2;
    let y = (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(x, y, popup_w, popup_h);

    f.render_widget(Clear, popup);

    let body = "\
Code review keys

  j / k         scroll one line down / up
  J / K         10-line jump (shift + j / k)
  gg / G        top / bottom of the current snap
  n / N         next / prev change row
  h / l         prev / next snap in the session
  [ / ]         prev / next snap (alternative)
  r             refresh from disk
  ?             toggle this help
  q / Esc       back to the session list

Each snap shows all the changes captured at
that moment, in time order across files.
Unchanged files do not appear.

  Green rows = added lines.
  Red rows   = removed lines (with the prior text).
  Red header = the whole file was deleted.
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
