//! `SnapStore` — read/write gate to the `sessions/<id>/snap-NNNN.json` layout.
//!
//! A snap is **one JSON file** at `<session>/snap-NNNN.json` (4-digit,
//! zero-padded, lexicographically sortable). The JSON carries the full
//! text of every tracked file at this snap plus the per-file diff
//! metadata vs. the previous snap, so the TUI can open a snap with a
//! single disk read.
//!
//! Snap numbering is 1-based: `snap-0001` is the baseline captured at
//! session start (the project's state at that moment), `snap-0002` is
//! the first save after that, etc.

use crate::error::{GrsError, Result};
use crate::ignore::IgnoreMatcher;
use crate::model::{SnapFileJson, SnapJson, STORAGE_VERSION};
use crate::paths::{relativize, GrsPaths};
use crate::ulid::SessionId;
use crate::util::fs::{atomic_write_str, is_binary_file};
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
    /// Path to the snap JSON file.
    pub path: PathBuf,
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
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = parse_snap_file_name(&name) {
                entries.push(SnapEntry {
                    n,
                    timestamp: 0,
                    path: entry.path(),
                });
            }
        }
        entries.sort_by_key(|e| e.n);
        // Backfill timestamps lazily.
        for e in &mut entries {
            if let Ok(meta) = read_snap_json_at(&e.path) {
                e.timestamp = meta.timestamp;
            }
        }
        Ok(entries)
    }

    /// Read a snap JSON by session + n.
    pub fn read(&self, id: &SessionId, n: u32) -> Result<SnapJson> {
        let path = self.paths.snap_file(id, n);
        read_snap_json_at(&path)
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
    ) -> Result<Option<Vec<SnapJson>>> {
        if self.tree_matches_last_snap(id, ignore)? {
            debug!("tree unchanged since last snap — skipping capture");
            return Ok(None);
        }
        Ok(Some(self.capture(id, ignore)?))
    }

    /// True if the current project tree (filtered by `ignore`) would
    /// produce a snap identical to the most recent one. Uses the
    /// previous snap's `tree_sha` (a SHA-256 fingerprint of every
    /// tracked file's content at that snap) to compare against the
    /// current tree's fingerprint. If the two fingerprints match, no
    /// capture is needed.
    fn tree_matches_last_snap(
        &self,
        id: &SessionId,
        ignore: &IgnoreMatcher,
    ) -> Result<bool> {
        let last = match self.list(id)?.into_iter().last() {
            Some(e) => e,
            None => return Ok(false), // no prior snap — caller will write snap-1
        };
        let prev = read_snap_json_at(&last.path)?;
        if prev.tree_sha.is_empty() {
            // Older snap (pre-tree_sha). We can't dedupe safely without
            // a fingerprint. Capture.
            return Ok(false);
        }
        let current_pairs = build_tree_pairs(ignore);
        let current_tree_sha = compute_tree_sha(&current_pairs);
        Ok(prev.tree_sha == current_tree_sha)
    }

    /// Capture the current state of the project (filtered by `ignore`) as
    /// one or more snaps. **One snap per changed file**: a save that
    /// touches N files produces N consecutive snaps (snap-N, snap-N+1,
    /// ...), each carrying that file's delta. A save with no changes
    /// returns an empty vec.
    ///
    /// Each snap has the full `tree_sha` (fingerprint of the entire
    /// project tree at the moment of capture) so the watcher's dedupe
    /// can compare the current tree against the most recent snap and
    /// skip the whole batch when nothing has changed.
    ///
    /// The "previous content" used to compute the diff is the file's
    /// content from the **most recent snap that mentioned it**, which
    /// is not necessarily the immediately previous snap (n-1) — with
    /// per-file snaps, a file's history is interleaved with other
    /// files'. We walk back through prior snaps to find it.
    pub fn capture(
        &self,
        id: &SessionId,
        ignore: &IgnoreMatcher,
    ) -> Result<Vec<SnapJson>> {
        let tree_pairs = build_tree_pairs(ignore);
        let tree_sha = compute_tree_sha(&tree_pairs);

        // Build a map of `path -> (content, binary, size)` from the
        // most recent snap that mentioned each path. Walk newest to
        // oldest; stop early when every current-tree path has been
        // accounted for.
        //
        // A file whose most recent snap is a `removed: true` entry is
        // **fully gone** from the project — the on-disk file no longer
        // exists, and the next time it appears it will be a brand-new
        // file (no prior content to diff against). We deliberately do
        // NOT include such files in `prev_by_path`: otherwise every
        // subsequent capture would see the file missing from the
        // current tree, find it in `prev_by_path` with `content: ""`
        // (the removal snap's content is empty by design), and emit a
        // duplicate removal snap with `prev_content: ""`. Those
        // duplicates showed up as "removed files still showing empty
        // multiple times in snaps" — one removal snap per save, each
        // with the prior content blanked out.
        //
        // Note: we must remember "removed" across the whole walk, not
        // just per-snap. The walk is newest -> oldest; a removal snap
        // (most recent) might be followed by an older "add" snap for
        // the same path. Without remembering the removal, the older
        // add would be picked up as the prev and the duplicate would
        // re-emerge.
        let current_paths: std::collections::HashSet<String> =
            tree_pairs.iter().map(|(p, _)| p.clone()).collect();
        let mut prev_by_path: std::collections::HashMap<String, PrevEntry> =
            std::collections::HashMap::new();
        let mut removed_paths: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for entry in self.list(id)?.into_iter().rev() {
            let snap = read_snap_json_at(&entry.path)?;
            for f in &snap.files {
                if prev_by_path.contains_key(&f.path) || removed_paths.contains(&f.path) {
                    continue;
                }
                if f.removed {
                    // Most recent mention of this path is a removal —
                    // the file is gone. Remember it and skip so
                    // future captures don't re-emit the removal (and
                    // so older non-removed snaps for the same path
                    // don't sneak in as the "prev").
                    removed_paths.insert(f.path.clone());
                    continue;
                }
                prev_by_path.insert(
                    f.path.clone(),
                    PrevEntry {
                        content: f.content.clone(),
                        binary: f.binary,
                        size: f.size,
                    },
                );
            }
            // Early exit: if every current path has a prev entry, no
            // need to keep walking.
            if current_paths.iter().all(|p| prev_by_path.contains_key(p)) {
                break;
            }
        }

        // Build the new snaps. One per changed file (modified or new)
        // plus one per deleted file.
        let mut next_n = self.next_n(id)?;
        let ts = now_ms();
        let root = ignore.root().to_path_buf();
        let mut seen_in_current: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut snaps: Vec<SnapJson> = Vec::new();

        // 1. Modified and new files: for each file in the current tree,
        //    if its SHA differs from the most recent prev, emit a snap.
        for (rel, current_sha) in &tree_pairs {
            seen_in_current.insert(rel.clone());
            let prev = prev_by_path.get(rel);
            let prev_sha = prev.map(|p| sha256_of_content(&p.content));
            if prev_sha.as_deref() == Some(current_sha.as_str()) {
                // Unchanged. Skip.
                continue;
            }
            // Read the file content. A vanished/unreadable file is
            // dropped silently; the next capture will reflect the
            // new state.
            let abs = root.join(rel);
            let Ok(bytes) = std::fs::read(&abs) else {
                continue;
            };
            let binary = is_binary_file(&abs) || bytes.iter().take(8192).any(|b| *b == 0);
            let size = bytes.len() as u64;
            let content = if binary {
                format!("(binary file, {size} bytes)")
            } else {
                String::from_utf8(bytes.clone())
                    .unwrap_or_else(|_| format!("(binary file, {size} bytes)"))
            };
            let entry = build_file_entry(
                rel,
                content,
                binary,
                size,
                prev,
            );
            let snap = SnapJson {
                version: STORAGE_VERSION,
                n: next_n,
                timestamp: ts,
                file_path: rel.clone(),
                tree_sha: tree_sha.clone(),
                files: vec![entry],
            };
            next_n += 1;
            snaps.push(snap);
        }

        // 2. Deletions: paths in the prior history but not in the
        //    current tree.
        for (path, prev) in &prev_by_path {
            if seen_in_current.contains(path) {
                continue;
            }
            let entry = SnapFileJson {
                path: path.clone(),
                content: String::new(),
                binary: prev.binary,
                size: prev.size,
                removed: true,
                prev_content: Some(prev.content.clone()),
                added_lines: None,
                removed_lines: None,
            };
            let snap = SnapJson {
                version: STORAGE_VERSION,
                n: next_n,
                timestamp: ts,
                file_path: path.clone(),
                tree_sha: tree_sha.clone(),
                files: vec![entry],
            };
            next_n += 1;
            snaps.push(snap);
        }

        // 3. Persist each snap to disk. We do this in a second pass so
        //    all snap numbers are assigned before any file is written
        //    (makes the on-disk state consistent if a write fails
        //    partway through).
        for snap in &snaps {
            let dest = self.paths.snap_file(id, snap.n);
            write_snap_json(&dest, snap)?;
        }

        Ok(snaps)
    }

    /// Remove a snap file. Used when cleaning up a partial capture or
    /// removing a session's last snap.
    pub fn remove(&self, id: &SessionId, n: u32) -> Result<()> {
        let path = self.paths.snap_file(id, n);
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Count snaps in a session.
    pub fn count(&self, id: &SessionId) -> Result<u32> {
        self.list(id).map(|v| v.len() as u32)
    }
}

// -----------------------------------------------------------------------------
// File I/O
// -----------------------------------------------------------------------------

fn write_snap_json(path: &Path, snap: &SnapJson) -> Result<()> {
    let text = serde_json::to_string_pretty(snap).map_err(GrsError::from)?;
    atomic_write_str(path, &text)?;
    Ok(())
}

/// Public wrapper around `write_snap_json` for callers (the `RepoStore`)
/// that need to write a snap outside of `capture`.
pub fn write_snap_json_pub(path: &Path, snap: &SnapJson) -> Result<()> {
    write_snap_json(path, snap)
}

fn read_snap_json_at(path: &Path) -> Result<SnapJson> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GrsError::NotFound(format!("snap {}: no json", path.display()))
        } else {
            GrsError::from(e)
        }
    })?;
    let m: SnapJson = serde_json::from_str(&text)?;
    if m.version != STORAGE_VERSION {
        return Err(GrsError::UnsupportedVersion(m.version));
    }
    Ok(m)
}

/// SHA-256 of a file on disk. Returns `None` if the file can't be read.
fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(sha256_of_bytes(&bytes))
}

/// One file's prior state, as remembered from the most recent snap that
/// mentioned it. Used by `capture` to build the prev-side of the diff
/// without re-reading the file from disk.
#[derive(Clone, Debug)]
struct PrevEntry {
    content: String,
    binary: bool,
    size: u64,
}

/// Build a `SnapFileJson` for a single file's change. The `prev` arg
/// is the file's state from the most recent prior snap (if any).
fn build_file_entry(
    rel: &str,
    content: String,
    binary: bool,
    size: u64,
    prev: Option<&PrevEntry>,
) -> SnapFileJson {
    match (prev, binary) {
        // Text modification (or new file with text content). Compute
        // the line-level diff and inline the prev text.
        (Some(p), false) if !p.binary => {
            let line_d = crate::diff::line_diff(&p.content, &content);
            use std::collections::BTreeMap;
            let prev_text = &p.content;
            let removed_lines: BTreeMap<u32, String> = line_d
                .removed_lines
                .iter()
                .filter_map(|n| {
                    prev_text.lines().nth(n - 1).map(|line| {
                        let mut s = line.to_string();
                        if !s.ends_with('\n') {
                            s.push('\n');
                        }
                        (*n as u32, s)
                    })
                })
                .collect();
            SnapFileJson {
                path: rel.to_string(),
                content,
                binary: false,
                size,
                removed: false,
                prev_content: Some(p.content.clone()),
                added_lines: Some(
                    line_d.added_lines.iter().map(|n| *n as u32).collect(),
                ),
                removed_lines: Some(removed_lines),
            }
        }
        // Binary change, text/binary transition, or a brand-new
        // binary file. Just record the new content; no line-level
        // diff.
        (Some(_), _) | (None, _) => SnapFileJson {
            path: rel.to_string(),
            content,
            binary,
            size,
            removed: false,
            prev_content: None,
            added_lines: None,
            removed_lines: None,
        },
    }
}

/// Compute the `tree_sha` for a set of tracked files: SHA-256 of the
/// sorted `path\tsha256\n` lines. Used to fingerprint the full project
/// state for dedupe. Empty input returns an empty string (so the JSON
/// omits the field).
fn compute_tree_sha(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let mut sorted = pairs.to_vec();
    sorted.sort();
    let mut h = Sha256::new();
    for (path, sha) in sorted {
        h.update(path.as_bytes());
        h.update(b"\t");
        h.update(sha.as_bytes());
        h.update(b"\n");
    }
    format!("{:x}", h.finalize())
}

/// Build the `(path, sha256)` pairs for the current project tree. Used
/// both for `tree_sha` in `capture` and for the dedupe comparison in
/// `tree_matches_last_snap`.
fn build_tree_pairs(ignore: &IgnoreMatcher) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let root = ignore.root().to_path_buf();
    for abs in ignore.files() {
        let rel = relativize(&root, &abs);
        if rel.is_empty() {
            continue;
        }
        if let Some(sha) = hash_file(&abs) {
            pairs.push((rel, sha));
        }
        // A vanished/unreadable file is its own state change — the
        // caller will see a different tree_sha and capture. We don't
        // include it in the pairs (since we have no SHA), so its
        // absence will change the fingerprint.
    }
    pairs
}

fn sha256_of_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn sha256_of_content(s: &str) -> String {
    sha256_of_bytes(s.as_bytes())
}

/// Parse `snap-NNNN.json` into `Some(N)`. Only accepts 4-digit, zero-padded
/// numbers (so the on-disk file name is always the same width, and a
/// lexicographic sort matches a numeric sort). Returns None for any other
/// name.
fn parse_snap_file_name(name: &str) -> Option<u32> {
    let n = name.strip_prefix("snap-")?.strip_suffix(".json")?;
    if n.len() != 4 || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

// -----------------------------------------------------------------------------
// Diff (kept in the public API for back-compat with tests/clients; the new
// JSON carries the diff inline, so this is a thin convenience that re-
// extracts it).
// -----------------------------------------------------------------------------

/// A file-level change between two consecutive snaps.
///
/// In v2 the diff is stored inline in the snap JSON. This enum is kept
/// for callers that want a structured view (e.g. the TUI's diff overlay
/// could use it, though the highlight engine currently re-derives the
/// same info from `prev_content` + `content` via `similar`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Added { path: String, size: u64 },
    Removed { path: String, size: u64 },
    Modified { path: String, old_size: u64, new_size: u64 },
    Renamed { from: String, to: String },
    RenamedAndModified { from: String, to: String, old_size: u64, new_size: u64 },
}

#[derive(Clone, Debug, Default)]
pub struct SnapDiff {
    pub changes: Vec<FileChange>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// Compute the diff between two snap JSONs (the same data structure the
/// on-disk JSON uses). Kept for any caller that wants the structured
/// view; the JSON already carries `added_lines` and `removed_lines` so
/// most callers won't need this.
pub fn diff_snap_meta(prev: &SnapJson, cur: &SnapJson) -> SnapDiff {
    use std::collections::HashMap;
    let prev_by_path: HashMap<&str, &SnapFileJson> =
        prev.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let cur_by_path: HashMap<&str, &SnapFileJson> =
        cur.files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut changes = Vec::new();
    let mut added_lines_total = 0usize;
    let mut removed_lines_total = 0usize;

    for cur_file in &cur.files {
        match prev_by_path.get(cur_file.path.as_str()) {
            None => changes.push(FileChange::Added {
                path: cur_file.path.clone(),
                size: cur_file.size,
            }),
            Some(prev_file) => {
                let prev_sha = sha256_of_content(&prev_file.content);
                let cur_sha = sha256_of_content(&cur_file.content);
                if prev_sha != cur_sha {
                    changes.push(FileChange::Modified {
                        path: cur_file.path.clone(),
                        old_size: prev_file.size,
                        new_size: cur_file.size,
                    });
                    if !cur_file.binary {
                        let line_d = crate::diff::line_diff(&prev_file.content, &cur_file.content);
                        added_lines_total += line_d.added_lines.len();
                        removed_lines_total += line_d.removed_lines.len();
                    }
                }
            }
        }
    }
    for prev_file in &prev.files {
        if !cur_by_path.contains_key(prev_file.path.as_str()) {
            changes.push(FileChange::Removed {
                path: prev_file.path.clone(),
                size: prev_file.size,
            });
        }
    }
    SnapDiff {
        changes,
        added_lines: added_lines_total,
        removed_lines: removed_lines_total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_snap_file_name_accepts_4_digit_padded() {
        assert_eq!(parse_snap_file_name("snap-0001.json"), Some(1));
        assert_eq!(parse_snap_file_name("snap-0042.json"), Some(42));
        assert_eq!(parse_snap_file_name("snap-9999.json"), Some(9999));
    }

    #[test]
    fn parse_snap_file_name_rejects_other_names() {
        assert_eq!(parse_snap_file_name("snap-1.json"), None);   // not 4-digit
        assert_eq!(parse_snap_file_name("snap-00001.json"), None); // 5 digits
        assert_eq!(parse_snap_file_name("meta.toml"), None);
        assert_eq!(parse_snap_file_name("snap-0001"), None);     // no .json
        assert_eq!(parse_snap_file_name("snap-abcd.json"), None);
    }

    #[test]
    fn capture_writes_one_snap_per_file() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("hello.txt"), "hello\nworld\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths.clone());
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();

        let snaps = store.capture(&id, &ignore).unwrap();
        // Two files, two snaps, each with exactly one file.
        assert_eq!(snaps.len(), 2);
        for (i, snap) in snaps.iter().enumerate() {
            assert_eq!(snap.n, (i + 1) as u32);
            assert_eq!(snap.files.len(), 1, "snap {} has {} files, expected 1", snap.n, snap.files.len());
        }
        // The two file paths are distinct.
        let paths_set: std::collections::HashSet<&str> =
            snaps.iter().map(|s| s.file_path.as_str()).collect();
        assert_eq!(paths_set.len(), 2);
        assert!(paths_set.contains("hello.txt"));
        assert!(paths_set.contains("src/main.rs"));
        // On-disk: snap-0001.json and snap-0002.json both exist.
        assert!(paths.snap_file(&id, 1).is_file());
        assert!(paths.snap_file(&id, 2).is_file());
        // JSON round-trips.
        for snap in &snaps {
            let back = read_snap_json_at(&paths.snap_file(&id, snap.n)).unwrap();
            assert_eq!(back.file_path, snap.file_path);
            assert_eq!(back.files.len(), 1);
            // First capture: no prev_content.
            assert!(back.files[0].prev_content.is_none());
        }
        // Content of hello.txt round-trips.
        let hello_snap = snaps.iter().find(|s| s.file_path == "hello.txt").unwrap();
        assert_eq!(hello_snap.files[0].content, "hello\nworld\n");
    }

    #[test]
    fn capture_writes_one_snap_per_changed_file() {
        // A save that modifies 2 files produces 2 snaps (one per file).
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta\n").unwrap();
        std::fs::write(root.join("c.txt"), "gamma\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        // First capture: 3 files, 3 snaps.
        let s1 = store.capture(&id, &ignore).unwrap();
        assert_eq!(s1.len(), 3);
        assert_eq!(s1.iter().map(|s| s.n).collect::<Vec<_>>(), vec![1, 2, 3]);

        // Modify a.txt and b.txt; c.txt is unchanged. 2 snaps.
        std::fs::write(root.join("a.txt"), "alpha2\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta2\n").unwrap();
        let s2 = store.capture(&id, &ignore).unwrap();
        assert_eq!(s2.len(), 2);
        assert_eq!(s2[0].n, 4);
        assert_eq!(s2[1].n, 5);
        let s2_paths: std::collections::HashSet<&str> =
            s2.iter().map(|s| s.file_path.as_str()).collect();
        assert!(s2_paths.contains("a.txt"));
        assert!(s2_paths.contains("b.txt"));
        assert!(!s2_paths.contains("c.txt"));
    }

    #[test]
    fn second_change_to_same_file_walks_back_for_prev_content() {
        // When a file is changed again after another file was changed
        // in between, the new snap must walk back to find the prior
        // content (not the immediately previous snap, which has a
        // different file).
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(root.join("b.txt"), "first\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        // Capture 1: 2 files, 2 snaps. Let's not assume order.
        let _ = store.capture(&id, &ignore).unwrap();

        // Modify b.txt, then a.txt. The a.txt snap must still know
        // its prior content (from the very first capture), not from
        // the just-captured b.txt snap.
        std::fs::write(root.join("b.txt"), "second\n").unwrap();
        let s_b = store.capture(&id, &ignore).unwrap();
        assert_eq!(s_b.len(), 1);
        assert_eq!(s_b[0].file_path, "b.txt");

        std::fs::write(root.join("a.txt"), "alpha\nBETA\ngamma\ndelta\n").unwrap();
        let s_a = store.capture(&id, &ignore).unwrap();
        assert_eq!(s_a.len(), 1);
        assert_eq!(s_a[0].file_path, "a.txt");
        // The prev_content must be the original a.txt, not b.txt.
        assert_eq!(
            s_a[0].files[0].prev_content.as_deref(),
            Some("alpha\nbeta\ngamma\n")
        );
        assert!(!s_a[0].files[0].removed);
    }

    /// A file that exists at N-1 but is gone at N must produce its
    /// own snap with `removed: true`, with `prev_content` carrying the
    /// prior text.
    #[test]
    fn deleted_file_appears_in_next_snap() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("ephemeral.txt"), "goodbye\n").unwrap();
        std::fs::write(root.join("keep.txt"), "stable\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        store.capture(&id, &ignore).unwrap();

        std::fs::remove_file(root.join("ephemeral.txt")).unwrap();
        let s2 = store.capture(&id, &ignore).unwrap();

        // Exactly one snap, for the deleted file.
        assert_eq!(s2.len(), 1);
        assert_eq!(s2[0].file_path, "ephemeral.txt");
        let removed = &s2[0].files[0];
        assert!(removed.removed, "deleted file should have removed: true");
        assert!(removed.content.is_empty());
        assert_eq!(removed.prev_content.as_deref(), Some("goodbye\n"));
        // keep.txt unchanged — must NOT be in s2.
        assert!(s2.iter().all(|s| s.file_path != "keep.txt"));
    }

    /// Regression: once a file has been captured as removed, subsequent
    /// captures must NOT re-emit a removal snap for it. Previously,
    /// the most recent snap for the deleted file (its own removal
    /// snap, with `content: ""`) was being pulled into `prev_by_path`
    /// on every capture, so any later save that triggered a capture
    /// would produce another removal snap — with `prev_content: ""`
    /// because the prior mention was itself a removal. The user saw
    /// this as "removed files still showing empty multiple times in
    /// snaps".
    #[test]
    fn deleted_file_does_not_reappear_in_subsequent_captures() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("doomed.txt"), "goodbye\n").unwrap();
        std::fs::write(root.join("keep.txt"), "stable\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        store.capture(&id, &ignore).unwrap();

        // Delete the doomed file and capture: one removal snap with
        // the original content preserved in `prev_content`.
        std::fs::remove_file(root.join("doomed.txt")).unwrap();
        let s1 = store.capture(&id, &ignore).unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].file_path, "doomed.txt");
        assert!(s1[0].files[0].removed);
        assert_eq!(s1[0].files[0].prev_content.as_deref(), Some("goodbye\n"));

        // A no-op capture (tree unchanged) must produce zero snaps.
        // The deleted file must NOT reappear as an empty removal.
        let s2 = store.capture(&id, &ignore).unwrap();
        assert!(
            s2.is_empty(),
            "no-op capture must not re-emit the removal snap, got {} snap(s) for {:?}",
            s2.len(),
            s2.iter().map(|s| (&s.file_path, s.files[0].removed)).collect::<Vec<_>>()
        );

        // A subsequent save to a *different* file must not drag the
        // deleted file back in either.
        std::fs::write(root.join("keep.txt"), "stable2\n").unwrap();
        let s3 = store.capture(&id, &ignore).unwrap();
        assert_eq!(s3.len(), 1, "expected exactly one snap (for keep.txt), got {}", s3.len());
        assert_eq!(s3[0].file_path, "keep.txt");
        assert!(
            s3.iter().all(|s| s.file_path != "doomed.txt"),
            "deleted file must not reappear in subsequent snaps"
        );

        // A re-creation of the deleted path with new content should
        // be captured as a brand-new file (no prev_content) — not as
        // a modification against the old removal's empty content.
        std::fs::write(root.join("doomed.txt"), "back from the dead\n").unwrap();
        let s4 = store.capture(&id, &ignore).unwrap();
        let doomed = s4
            .iter()
            .find(|s| s.file_path == "doomed.txt")
            .expect("recreated file must produce a snap");
        assert!(!doomed.files[0].removed);
        assert!(doomed.files[0].prev_content.is_none(),
            "recreated file is a brand-new file: prev_content must be None, got {:?}",
            doomed.files[0].prev_content);
        assert_eq!(doomed.files[0].content, "back from the dead\n");
    }

    /// Every snap carries a `tree_sha` that fingerprints the full
    /// project tree at that moment. All snaps in the same save cycle
    /// share the same `tree_sha`.
    #[test]
    fn tree_sha_is_stable_for_same_tree() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        let s1 = store.capture(&id, &ignore).unwrap();
        // All snaps from the first save cycle share the same tree_sha.
        assert!(!s1.is_empty());
        let first_sha = &s1[0].tree_sha;
        for snap in &s1 {
            assert_eq!(&snap.tree_sha, first_sha);
        }
        // A second capture with no changes produces no snaps.
        let s2 = store.capture(&id, &ignore).unwrap();
        assert!(s2.is_empty(), "second capture of unchanged tree must produce no snaps");
    }

    /// The dedupe `capture_if_changed` returns None when the tree
    /// matches the previous snap's `tree_sha`, even if the tree is
    /// non-empty.
    #[test]
    fn dedupe_uses_tree_sha() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        let s1 = store.capture_if_changed(&id, &ignore).unwrap();
        assert!(s1.is_some());
        // Same tree — capture_if_changed must return None.
        let s2 = store.capture_if_changed(&id, &ignore).unwrap();
        assert!(s2.is_none(), "capture_if_changed must dedupe via tree_sha");
        // Change the file — capture_if_changed must return Some.
        std::fs::write(root.join("a.txt"), "alpha2\n").unwrap();
        let s3 = store.capture_if_changed(&id, &ignore).unwrap();
        assert!(s3.is_some());
    }

    #[test]
    fn capture_if_changed_dedupes_unchanged() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".grs/sessions")).unwrap();
        let id = SessionId::new();
        std::fs::create_dir_all(root.join(".grs/sessions").join(id.as_str())).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();

        let paths = GrsPaths::new(&root);
        let store = SnapStore::new(paths);
        let ignore = IgnoreMatcher::new(&root, &[]).unwrap();
        let s1 = store.capture_if_changed(&id, &ignore).unwrap();
        assert!(s1.is_some());
        // Calling again with the same tree must return None.
        let s2 = store.capture_if_changed(&id, &ignore).unwrap();
        assert!(s2.is_none(), "second capture_if_changed on unchanged tree must be a no-op");
    }
}
