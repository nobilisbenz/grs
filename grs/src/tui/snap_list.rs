//! The snap list pane (left side of the TUI).
//!
//! Loads the list of snaps for a session and tracks the cursor position.
//! Also pre-computes per-snap line diff counts for display in the list.

use grs_lib::model::SessionMeta;
use grs_lib::snap::SnapEntry;
use grs_lib::store::RepoStore;
use std::collections::HashMap;

pub struct SnapListState {
    pub entries: Vec<SnapEntry>,
    pub cursor: usize,
    /// Per-snap (added, removed) line counts, for display in the list.
    pub diffs: HashMap<u32, (usize, usize)>,
}

impl SnapListState {
    pub fn load(store: &RepoStore, session: &SessionMeta) -> Result<Self, grs_lib::error::GrsError> {
        let entries = store.snaps().list(&session.id)?;
        let mut diffs = HashMap::new();
        // Pre-compute line diffs for each snap (1, 2, ...) against its predecessor.
        for entry in &entries {
            if entry.n == 1 {
                diffs.insert(entry.n, (0, 0));
                continue;
            }
            let prev_dir = store.paths().snap_dir(&session.id, entry.n - 1);
            let cur_dir = store.paths().snap_dir(&session.id, entry.n);
            let snap_diff = grs_lib::snap::diff_snap_dirs(&prev_dir, &cur_dir)?;
            diffs.insert(entry.n, (snap_diff.added_lines, snap_diff.removed_lines));
        }
        let cursor = entries.len().saturating_sub(1);
        Ok(Self {
            entries,
            cursor,
            diffs,
        })
    }

    pub fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn cursor_down(&mut self) {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
    }

    pub fn cursor_to_top(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_to_bottom(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }
}
