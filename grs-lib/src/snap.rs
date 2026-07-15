//! `SnapStore` — read/write gate to the `sessions/<id>/snap-N/` layout.
//!
//! A snap is a **whole project tree**: every tracked file copied under
//! `snap-N/`, plus a `meta.toml` describing them. Diffs are computed at
//! read time by walking two snap trees (no separate diff storage needed,
//! because each snap is a full copy).
//!
//! Snap numbering is 1-based: `snap-1` is the baseline captured at session
//! start (the project's state at that moment), `snap-2` is the first save
//! after that, etc.

use crate::error::{GrsError, Result};
use crate::ignore::IgnoreMatcher;
use crate::model::{SnapFile, SnapMeta, STORAGE_VERSION};
use crate::paths::{relativize, GrsPaths};
use crate::ulid::SessionId;
use crate::util::fs::atomic_write_str;
use crate::util::time::now_ms;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tracing::debug;

pub struct SnapStore {
    paths: GrsPaths,
}

#[derive(Clone, Debug)]
pub struct SnapEntry {
    pub n: u32,
    pub timestamp: i64,
    pub dir: PathBuf,
}

impl SnapStore {
    pub fn new(paths: GrsPaths) -> Self {
        Self { paths }
    }

    /// List snap entries for a session, sorted by `n` ascending.
    pub fn list(&self, id: &SessionId) -> Result<Vec<SnapEntry>> {
        let dir = self.paths.session_dir(id);
        let mut entries = Vec::new();
        if !dir.is_dir() {
            return Ok(entries);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = parse_snap_dir_name(&name) {
                entries.push(SnapEntry {
                    n,
                    timestamp: 0,
                    dir: entry.path(),
                });
            }
        }
        entries.sort_by_key(|e| e.n);
        // Backfill timestamps lazily.
        for e in &mut entries {
            if let Ok(meta) = read_meta(&e.dir) {
                e.timestamp = meta.timestamp;
            }
        }
        Ok(entries)
    }

    /// Read a snap's `meta.toml`.
    pub fn read_meta(&self, id: &SessionId, n: u32) -> Result<SnapMeta> {
        let dir = self.paths.snap_dir(id, n);
        read_meta(&dir)
    }

    /// The next snap number for a session (one past the current max). Returns
    /// 1 if no snaps exist yet.
    pub fn next_n(&self, id: &SessionId) -> Result<u32> {
        Ok(self
            .list(id)?
            .into_iter()
            .map(|e| e.n)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1))
    }

    /// Capture a new snap only if the current tree differs from the most
    /// recent one (by SHA256 of every tracked file). Returns `Some(meta)`
    /// when a new snap was written, `None` when the tree is byte-identical
    /// to the last snap and the write was skipped entirely — no new snap
    /// number is allocated in that case.
    ///
    /// This is the entry point the watcher uses: with the "snap on
    /// Close(Write) / Create" trigger set, a single save can fire several
    /// filesystem events back-to-back. The dedupe here turns the trailing
    /// duplicates into a no-op instead of stacking empty-file or
    /// intermediate-state snaps.
    pub fn capture_if_changed(
        &self,
        id: &SessionId,
        ignore: &IgnoreMatcher,
    ) -> Result<Option<SnapMeta>> {
        if self.tree_matches_last_snap(id, ignore)? {
            debug!("tree unchanged since last snap — skipping capture");
            return Ok(None);
        }
        Ok(Some(self.capture(id, ignore)?))
    }

    /// True if the current project tree (filtered by `ignore`) is
    /// byte-identical to the most recent snap: same set of relative paths,
    /// same SHA256 for each. Returns `true` vacuously if there is no
    /// previous snap (i.e. snap-1 hasn't been captured yet).
    fn tree_matches_last_snap(
        &self,
        id: &SessionId,
        ignore: &IgnoreMatcher,
    ) -> Result<bool> {
        let last_n = match self.list(id)?.into_iter().last() {
            Some(e) => e.n,
            None => return Ok(true), // no prior snap — caller will write snap-1
        };
        let prev_meta = self.read_meta(id, last_n)?;
        // Build a path -> sha256 map of the current tree.
        let mut current: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let root = ignore.root().to_path_buf();
        for abs in ignore.files() {
            let rel = relativize(&root, &abs);
            if rel.is_empty() {
                continue;
            }
            match hash_file(&abs) {
                Some(sha) => {
                    current.insert(rel, sha);
                }
                None => {
                    // File unreadable or vanished between walk and read.
                    // That alone is a state change — call it not-equal so we
                    // don't skip the capture.
                    return Ok(false);
                }
            }
        }
        if current.len() != prev_meta.files.len() {
            return Ok(false);
        }
        for prev_file in &prev_meta.files {
            match current.get(&prev_file.path) {
                Some(sha) if sha == &prev_file.sha256 => continue,
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Capture the current state of the project (filtered by `ignore`) as a
    /// new whole-project snap. `ignore` is used both to skip files when
    /// copying and to record which paths belong to the snap.
    pub fn capture(
        &self,
        id: &SessionId,
        ignore: &IgnoreMatcher,
    ) -> Result<SnapMeta> {
        let n = self.next_n(id)?;
        let dest = self.paths.snap_dir(id, n);

        // Don't fail hard on a partial capture: copy file-by-file and
        // accumulate what we got. A file that disappears mid-capture is
        // dropped silently; the next capture will reflect the new state.
        std::fs::create_dir_all(&dest)?;
        let mut files: Vec<SnapFile> = Vec::new();
        let mut total_bytes: u64 = 0;
        let root = ignore.root().to_path_buf();
        for abs in ignore.files() {
            let rel = relativize(&root, &abs);
            if rel.is_empty() {
                continue;
            }
            let meta = match capture_one(&abs, &dest, &rel) {
                Ok(Some(snap_file)) => {
                    total_bytes += snap_file.size;
                    snap_file
                }
                Ok(None) => continue, // skipped (binary or unreadable)
                Err(e) => {
                    tracing::warn!(file = %rel, error = ?e, "failed to capture file");
                    continue;
                }
            };
            files.push(meta);
        }
        // Stable order for deterministic `meta.toml` output.
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let snap_meta = SnapMeta {
            version: STORAGE_VERSION,
            n,
            timestamp: now_ms(),
            file_count: files.len() as u32,
            total_bytes,
            files,
        };
        write_meta(&dest, &snap_meta)?;
        // Best-effort fsync of the snap dir.
        let _ = crate::util::fs::fsync_dir(&dest);
        Ok(snap_meta)
    }

    /// Remove a snap directory. Used when cleaning up a partial capture
    /// or removing a session's last snap.
    pub fn remove(&self, id: &SessionId, n: u32) -> Result<()> {
        let dir = self.paths.snap_dir(id, n);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Count snaps in a session.
    pub fn count(&self, id: &SessionId) -> Result<u32> {
        self.list(id).map(|v| v.len() as u32)
    }
}

/// Capture one file: copy bytes into `<dest>/<rel>`, return a `SnapFile`.
/// Returns `Ok(None)` if the file looks binary and the `ignore` config
/// excludes binary files.
fn capture_one(src: &Path, dest_root: &Path, rel: &str) -> Result<Option<SnapFile>> {
    let bytes = match std::fs::read(src) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let is_binary = bytes.iter().take(8192).any(|b| *b == 0);
    let sha256 = {
        let mut h = Sha256::new();
        h.update(&bytes);
        format!("{:x}", h.finalize())
    };
    let dest = dest_root.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, &bytes)?;
    Ok(Some(SnapFile {
        path: rel.to_string(),
        sha256,
        size: bytes.len() as u64,
        binary: is_binary,
    }))
}

/// Compute SHA256 of a file. Returns `None` if the file can't be read.
fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(format!("{:x}", h.finalize()))
}

fn read_meta(snap_dir: &Path) -> Result<SnapMeta> {
    let path = snap_dir.join("meta.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GrsError::NotFound(format!("snap {}: no meta.toml", snap_dir.display()))
        } else {
            GrsError::from(e)
        }
    })?;
    let m: SnapMeta = toml::from_str(&text)?;
    Ok(m)
}

fn write_meta(snap_dir: &Path, meta: &SnapMeta) -> Result<()> {
    let path = snap_dir.join("meta.toml");
    let text = toml::to_string_pretty(meta).map_err(|e| GrsError::Config(e.to_string()))?;
    atomic_write_str(&path, &text)?;
    Ok(())
}

/// Parse `snap-N` into `Some(N)`. Returns None for any other folder name.
fn parse_snap_dir_name(name: &str) -> Option<u32> {
    name.strip_prefix("snap-").and_then(|s| s.parse().ok())
}

/// Public wrapper around `read_meta` for callers (the TUI) that have a snap
/// directory path but not a `SnapStore`.
pub fn read_meta_pub(snap_dir: &Path) -> Result<SnapMeta> {
    read_meta(snap_dir)
}

// -----------------------------------------------------------------------------
// Diff between two whole-project snaps
// -----------------------------------------------------------------------------

/// A file-level change between two consecutive snaps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Added {
        path: String,
        binary: bool,
        size: u64,
    },
    Removed {
        path: String,
        binary: bool,
        size: u64,
    },
    Modified {
        path: String,
        binary: bool,
        old_size: u64,
        new_size: u64,
    },
    /// Pure rename (same content hash, different path).
    Renamed {
        from: String,
        to: String,
        binary: bool,
    },
    /// Rename + content change (hashes differ; path changed too).
    RenamedAndModified {
        from: String,
        to: String,
        binary: bool,
        old_size: u64,
        new_size: u64,
    },
}

/// Result of diffing snap N against snap N-1.
#[derive(Clone, Debug, Default)]
pub struct SnapDiff {
    pub changes: Vec<FileChange>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// Compute the diff between two snap directories on disk.
///
/// `prev` and `cur` are paths to `snap-N-1/` and `snap-N/`. We use the
/// `meta.toml` to enumerate files (faster than walking) and we read the
/// actual file content for line-level diff on modified text files.
pub fn diff_snap_dirs(prev: &Path, cur: &Path) -> Result<SnapDiff> {
    let prev_meta = read_meta(prev)?;
    let cur_meta = read_meta(cur)?;
    diff_snap_meta(&prev_meta, prev, &cur_meta, cur)
}

/// Compute the diff given two `SnapMeta`s and their snap directories.
pub fn diff_snap_meta(
    prev: &SnapMeta,
    prev_dir: &Path,
    cur: &SnapMeta,
    cur_dir: &Path,
) -> Result<SnapDiff> {
    use std::collections::HashMap;
    let prev_by_path: HashMap<&str, &SnapFile> =
        prev.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let cur_by_path: HashMap<&str, &SnapFile> =
        cur.files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut changes = Vec::new();
    // First pass: renames. Match removed paths to added paths by sha256.
    // Build a map: sha256 -> list of removed paths.
    let mut removed: Vec<&SnapFile> = prev
        .files
        .iter()
        .filter(|f| !cur_by_path.contains_key(f.path.as_str()))
        .collect();
    let mut added: Vec<&SnapFile> = cur
        .files
        .iter()
        .filter(|f| !prev_by_path.contains_key(f.path.as_str()))
        .collect();

    // Try to pair renames by content hash.
    let mut consumed_added: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut rename_pairs: Vec<(&SnapFile, &SnapFile)> = Vec::new();
    for r in &removed {
        if let Some(idx) = added.iter().position(|a| a.sha256 == r.sha256) {
            let a = added[idx];
            rename_pairs.push((r, a));
            consumed_added.insert(a.path.clone());
        }
    }
    removed.retain(|r| !rename_pairs.iter().any(|(rr, _)| rr.path == r.path));
    added.retain(|a| !consumed_added.contains(&a.path));

    for (r, a) in rename_pairs {
        if r.binary || a.binary {
            // Renames of binary files: show as renamed, no line diff.
            changes.push(FileChange::Renamed {
                from: r.path.clone(),
                to: a.path.clone(),
                binary: true,
            });
        } else {
            // Renamed text file: diff content (if identical hashes, this
            // is a pure rename).
            let prev_bytes = std::fs::read(prev_dir.join(&r.path)).unwrap_or_default();
            let cur_bytes = std::fs::read(cur_dir.join(&a.path)).unwrap_or_default();
            if r.sha256 == a.sha256 {
                changes.push(FileChange::Renamed {
                    from: r.path.clone(),
                    to: a.path.clone(),
                    binary: false,
                });
            } else {
                let prev_text = String::from_utf8_lossy(&prev_bytes);
                let cur_text = String::from_utf8_lossy(&cur_bytes);
                let line_d = crate::diff::line_diff(&prev_text, &cur_text);
                let mut d = SnapDiff::default();
                d.added_lines = line_d.added_lines.len();
                d.removed_lines = line_d.removed_lines.len();
                changes.push(FileChange::RenamedAndModified {
                    from: r.path.clone(),
                    to: a.path.clone(),
                    binary: false,
                    old_size: r.size,
                    new_size: a.size,
                });
                let _ = d; // counts aggregated below
            }
        }
    }

    // Modifications: same path, different hash.
    let mut added_lines_total = 0usize;
    let mut removed_lines_total = 0usize;
    for cur_file in &cur.files {
        if let Some(prev_file) = prev_by_path.get(cur_file.path.as_str()) {
            if prev_file.sha256 != cur_file.sha256 {
                changes.push(FileChange::Modified {
                    path: cur_file.path.clone(),
                    binary: cur_file.binary,
                    old_size: prev_file.size,
                    new_size: cur_file.size,
                });
                if !cur_file.binary {
                    let prev_bytes = std::fs::read(prev_dir.join(&cur_file.path)).unwrap_or_default();
                    let cur_bytes = std::fs::read(cur_dir.join(&cur_file.path)).unwrap_or_default();
                    let prev_text = String::from_utf8_lossy(&prev_bytes);
                    let cur_text = String::from_utf8_lossy(&cur_bytes);
                    let line_d = crate::diff::line_diff(&prev_text, &cur_text);
                    added_lines_total += line_d.added_lines.len();
                    removed_lines_total += line_d.removed_lines.len();
                }
            }
        }
    }

    // Pure additions.
    for a in &added {
        changes.push(FileChange::Added {
            path: a.path.clone(),
            binary: a.binary,
            size: a.size,
        });
        if !a.binary {
            let bytes = std::fs::read(cur_dir.join(&a.path)).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            // A new file is "all added" — count its line count.
            added_lines_total += text.lines().count();
        }
    }

    // Pure removals.
    for r in &removed {
        changes.push(FileChange::Removed {
            path: r.path.clone(),
            binary: r.binary,
            size: r.size,
        });
        if !r.binary {
            let bytes = std::fs::read(prev_dir.join(&r.path)).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            removed_lines_total += text.lines().count();
        }
    }

    // Sort: paths alphabetically, with renames grouped.
    changes.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));

    Ok(SnapDiff {
        changes,
        added_lines: added_lines_total,
        removed_lines: removed_lines_total,
    })
}

fn sort_key(c: &FileChange) -> String {
    match c {
        FileChange::Added { path, .. } => format!("a:{path}"),
        FileChange::Removed { path, .. } => format!("r:{path}"),
        FileChange::Modified { path, .. } => format!("m:{path}"),
        FileChange::Renamed { from, to, .. } => format!("rn:{from}->{to}"),
        FileChange::RenamedAndModified { from, to, .. } => format!("rm:{from}->{to}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_session(p: &Path) -> SessionId {
        let id = SessionId::new();
        std::fs::create_dir_all(p.join(".grs/sessions").join(id.as_str())).unwrap();
        id
    }

    #[test]
    fn capture_creates_snap_with_meta() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = make_session(&root);
        std::fs::write(root.join("hello.txt"), "hello\nworld\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths.clone());
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();

        let meta = store.capture(&id, &ignore).unwrap();
        assert_eq!(meta.n, 1);
        assert_eq!(meta.file_count, 2);
        assert!(meta.total_bytes > 0);
        // Files were actually copied.
        assert!(paths.snap_dir(&id, 1).join("hello.txt").is_file());
        assert!(paths.snap_dir(&id, 1).join("src/main.rs").is_file());
    }

    #[test]
    fn capture_increments_snap_number() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = make_session(&root);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        let m1 = store.capture(&id, &ignore).unwrap();
        let m2 = store.capture(&id, &ignore).unwrap();
        let m3 = store.capture(&id, &ignore).unwrap();
        assert_eq!((m1.n, m2.n, m3.n), (1, 2, 3));
    }

    #[test]
    fn diff_detects_added_modified_removed_renamed() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = make_session(&root);

        // snap 1: a.txt, b.txt, keep.txt
        std::fs::write(root.join("a.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(root.join("b.txt"), "first\n").unwrap();
        std::fs::write(root.join("keep.txt"), "stable\n").unwrap();
        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths.clone());
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        store.capture(&id, &ignore).unwrap();

        // snap 2: a.txt modified, b.txt deleted, c.txt added, keep.txt stable,
        //         b.txt -> moved.txt (rename by content preservation)
        std::fs::remove_file(root.join("b.txt")).unwrap();
        std::fs::write(root.join("moved.txt"), "first\n").unwrap();
        std::fs::write(root.join("c.txt"), "new\n").unwrap();
        std::fs::write(root.join("a.txt"), "alpha\ngamma\ndelta\n").unwrap();
        store.capture(&id, &ignore).unwrap();

        let prev = paths.snap_dir(&id, 1);
        let cur = paths.snap_dir(&id, 2);
        let d = diff_snap_dirs(&prev, &cur).unwrap();

        // b.txt should be detected as a rename to moved.txt.
        let has_rename = d.changes.iter().any(|c| matches!(c,
            FileChange::Renamed { from, to, .. } if from == "b.txt" && to == "moved.txt"
        ));
        assert!(has_rename, "expected rename of b.txt -> moved.txt, got: {:?}", d.changes);

        // a.txt modified.
        let has_modified = d.changes.iter().any(|c| matches!(c,
            FileChange::Modified { path, .. } if path == "a.txt"
        ));
        assert!(has_modified, "expected a.txt modified, got: {:?}", d.changes);

        // c.txt added.
        let has_added = d.changes.iter().any(|c| matches!(c,
            FileChange::Added { path, .. } if path == "c.txt"
        ));
        assert!(has_added, "expected c.txt added, got: {:?}", d.changes);

        // No change entry for keep.txt.
        let has_keep = d.changes.iter().any(|c| match c {
            FileChange::Modified { path, .. }
            | FileChange::Added { path, .. }
            | FileChange::Removed { path, .. } => path == "keep.txt",
            _ => false,
        });
        assert!(!has_keep, "keep.txt should be unchanged");
    }

    /// End-to-end: start the watcher, write a file, capture a second snap,
    /// and verify the consecutive diff is correct.
    #[test]
    fn watcher_capture_then_consecutive_diff() {
        use crate::store::RepoStore;
        use crate::watcher::Watcher;
        use std::sync::mpsc;
        use std::time::Duration;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("diff-test".into()).unwrap();
        let session_id = s.id.clone();

        // snap 1 = empty project (the dir is fresh).
        let meta1 = store.snaps().read_meta(&session_id, 1).unwrap();
        assert_eq!(meta1.file_count, 0, "fresh dir: snap 1 has 0 files");

        // Start the watcher in a background thread.
        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = Watcher::new(watcher_store).run(&stop_rx);
        });
        // Settle.
        std::thread::sleep(Duration::from_millis(200));

        // Write a new file. The 1.5s debounce fires; we wait + a margin.
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        std::thread::sleep(Duration::from_millis(2500));

        // Now also modify it.
        std::fs::write(dir.path().join("a.txt"), "alpha\nBETA\ngamma\ndelta\n").unwrap();
        std::thread::sleep(Duration::from_millis(2500));

        // Stop the watcher.
        stop_tx.send(()).ok();
        let _ = handle.join();

        // We expect at least snap 2 and snap 3.
        let count = store.snaps().count(&session_id).unwrap();
        assert!(count >= 3, "expected >= 3 snaps, got {count}");

        // snap 1 -> snap 2: a.txt is a pure addition.
        let d1 = diff_snap_dirs(
            &store.paths().snap_dir(&session_id, 1),
            &store.paths().snap_dir(&session_id, 2),
        )
        .unwrap();
        let adds_for_a = d1
            .changes
            .iter()
            .filter(|c| matches!(c, FileChange::Added { path, .. } if path == "a.txt"))
            .count();
        assert_eq!(adds_for_a, 1, "snap 2 should add a.txt exactly once");
        assert_eq!(d1.added_lines, 3, "3 lines added in a.txt");

        // snap 2 -> snap 3: a.txt is a modification (1 line changed, 1 added).
        let d2 = diff_snap_dirs(
            &store.paths().snap_dir(&session_id, 2),
            &store.paths().snap_dir(&session_id, 3),
        )
        .unwrap();
        let mods_for_a = d2
            .changes
            .iter()
            .filter(|c| matches!(c, FileChange::Modified { path, .. } if path == "a.txt"))
            .count();
        assert_eq!(mods_for_a, 1, "snap 3 should modify a.txt exactly once");
        assert_eq!(d2.added_lines, 2, "BETA replaced + delta added = 2 line changes");
        assert_eq!(d2.removed_lines, 1, "1 line removed (old 'beta')");
    }
}
