//! The diff view pane (right side of the TUI).
//!
//! Loads the diff between the current snap and its predecessor, then
//! renders it as a unified-diff-style list of lines with green/red colors.
//!
//! For snap 1 (the baseline), there is no predecessor — the pane shows
//! a placeholder.

use grs_lib::model::SessionMeta;
use grs_lib::snap::{diff_snap_dirs, FileChange, SnapEntry};
use grs_lib::store::RepoStore;
use ratatui::text::{Line, Span};
use std::path::Path;

use super::theme::Theme;

pub struct DiffViewState {
    /// The currently-displayed snap number (for title display).
    pub current_snap_n: Option<u32>,
    /// Pre-rendered diff lines.
    pub lines: Vec<Line<'static>>,
    /// Vertical scroll offset (in lines).
    pub scroll: usize,
}

impl DiffViewState {
    pub fn new() -> Self {
        Self {
            current_snap_n: None,
            lines: Vec::new(),
            scroll: 0,
        }
    }

    /// Load the diff for `entry` (snap N vs N-1) and render it.
    pub fn load(
        &mut self,
        store: &RepoStore,
        session: &SessionMeta,
        entry: &SnapEntry,
    ) -> Result<(), grs_lib::error::GrsError> {
        self.current_snap_n = Some(entry.n);
        self.scroll = 0;

        if entry.n == 1 {
            self.lines = vec![];
            return Ok(());
        }

        let prev_dir = store.paths().snap_dir(&session.id, entry.n - 1);
        let cur_dir = store.paths().snap_dir(&session.id, entry.n);
        let prev_meta = grs_lib::snap::read_meta_pub(&prev_dir).ok();
        let cur_meta = grs_lib::snap::read_meta_pub(&cur_dir).ok();
        let snap_diff = diff_snap_dirs(&prev_dir, &cur_dir)?;
        self.lines = render_diff(&snap_diff.changes, prev_meta.as_ref(), cur_meta.as_ref(), &prev_dir, &cur_dir);
        Ok(())
    }

    pub fn title(&self) -> String {
        match self.current_snap_n {
            Some(n) if n == 1 => " snap 1 · baseline ".to_string(),
            Some(n) => format!(" snap {n} · diff vs snap {} ", n - 1),
            None => " diff ".to_string(),
        }
    }

    pub fn placeholder(&self) -> String {
        match self.current_snap_n {
            Some(1) => "── Snap 1 · baseline ──\n\nProject state at session start.\nSelect snap 2+ to see changes.".to_string(),
            _ => "(no snap selected)".to_string(),
        }
    }
}

fn render_diff(
    changes: &[FileChange],
    _prev_meta: Option<&grs_lib::model::SnapMeta>,
    _cur_meta: Option<&grs_lib::model::SnapMeta>,
    prev_dir: &Path,
    cur_dir: &Path,
) -> Vec<Line<'static>> {
    let theme = Theme::default();
    let mut out: Vec<Line<'static>> = Vec::new();
    for change in changes {
        match change {
            FileChange::Added { path, binary, size } => {
                out.push(Line::from(vec![
                    Span::styled("─── added: ", theme.file_header()),
                    Span::styled(path.clone(), theme.file_header()),
                    if *binary {
                        Span::styled(format!(" (binary, {size} bytes) "), theme.muted())
                    } else {
                        Span::styled(" ".to_string(), theme.muted())
                    },
                ]));
                if !*binary {
                    let bytes = std::fs::read(cur_dir.join(path)).unwrap_or_default();
                    let text = String::from_utf8_lossy(&bytes);
                    for (i, line) in text.lines().enumerate() {
                        out.push(Line::from(vec![
                            Span::styled(format!("  +{:>4}  ", i + 1), theme.added_line_no()),
                            Span::styled(line.to_string(), theme.added()),
                        ]));
                    }
                }
            }
            FileChange::Removed { path, binary, size } => {
                out.push(Line::from(vec![
                    Span::styled("─── removed: ", theme.file_header()),
                    Span::styled(path.clone(), theme.file_header()),
                    if *binary {
                        Span::styled(format!(" (binary, {size} bytes) "), theme.muted())
                    } else {
                        Span::styled(" ".to_string(), theme.muted())
                    },
                ]));
                if !*binary {
                    let bytes = std::fs::read(prev_dir.join(path)).unwrap_or_default();
                    let text = String::from_utf8_lossy(&bytes);
                    for (i, line) in text.lines().enumerate() {
                        out.push(Line::from(vec![
                            Span::styled(format!("  -{:>4}  ", i + 1), theme.removed_line_no()),
                            Span::styled(line.to_string(), theme.removed()),
                        ]));
                    }
                }
            }
            FileChange::Modified {
                path,
                binary,
                old_size,
                new_size,
            } => {
                out.push(Line::from(vec![
                    Span::styled("─── modified: ", theme.file_header()),
                    Span::styled(path.clone(), theme.file_header()),
                    Span::styled(
                        format!(" ({old_size} → {new_size} bytes) "),
                        theme.muted(),
                    ),
                ]));
                if !*binary {
                    let prev_bytes = std::fs::read(prev_dir.join(path)).unwrap_or_default();
                    let cur_bytes = std::fs::read(cur_dir.join(path)).unwrap_or_default();
                    let prev_text = String::from_utf8_lossy(&prev_bytes);
                    let cur_text = String::from_utf8_lossy(&cur_bytes);
                    let line_d = grs_lib::diff::line_diff(&prev_text, &cur_text);
                    for n in &line_d.removed_lines {
                        if let Some(line) = prev_text.lines().nth(n - 1) {
                            out.push(Line::from(vec![
                                Span::styled(format!("  -{:>4}  ", n), theme.removed_line_no()),
                                Span::styled(line.to_string(), theme.removed()),
                            ]));
                        }
                    }
                    for n in &line_d.added_lines {
                        if let Some(line) = cur_text.lines().nth(n - 1) {
                            out.push(Line::from(vec![
                                Span::styled(format!("  +{:>4}  ", n), theme.added_line_no()),
                                Span::styled(line.to_string(), theme.added()),
                            ]));
                        }
                    }
                }
            }
            FileChange::Renamed { from, to, binary } => {
                out.push(Line::from(vec![
                    Span::styled("─── renamed: ", theme.file_header()),
                    Span::styled(from.clone(), theme.file_header()),
                    Span::styled(" → ", theme.muted()),
                    Span::styled(to.clone(), theme.file_header()),
                    if *binary {
                        Span::styled(" (binary) ".to_string(), theme.muted())
                    } else {
                        Span::styled(" ".to_string(), theme.muted())
                    },
                ]));
            }
            FileChange::RenamedAndModified {
                from,
                to,
                binary,
                old_size,
                new_size,
            } => {
                out.push(Line::from(vec![
                    Span::styled("─── renamed + modified: ", theme.file_header()),
                    Span::styled(from.clone(), theme.file_header()),
                    Span::styled(" → ", theme.muted()),
                    Span::styled(to.clone(), theme.file_header()),
                    Span::styled(
                        format!(" ({old_size} → {new_size} bytes) "),
                        theme.muted(),
                    ),
                ]));
                if !*binary {
                    let prev_bytes = std::fs::read(prev_dir.join(from)).unwrap_or_default();
                    let cur_bytes = std::fs::read(cur_dir.join(to)).unwrap_or_default();
                    let prev_text = String::from_utf8_lossy(&prev_bytes);
                    let cur_text = String::from_utf8_lossy(&cur_bytes);
                    let line_d = grs_lib::diff::line_diff(&prev_text, &cur_text);
                    for n in &line_d.removed_lines {
                        if let Some(line) = prev_text.lines().nth(n - 1) {
                            out.push(Line::from(vec![
                                Span::styled(format!("  -{:>4}  ", n), theme.removed_line_no()),
                                Span::styled(line.to_string(), theme.removed()),
                            ]));
                        }
                    }
                    for n in &line_d.added_lines {
                        if let Some(line) = cur_text.lines().nth(n - 1) {
                            out.push(Line::from(vec![
                                Span::styled(format!("  +{:>4}  ", n), theme.added_line_no()),
                                Span::styled(line.to_string(), theme.added()),
                            ]));
                        }
                    }
                }
            }
        }
    }
    out
}
