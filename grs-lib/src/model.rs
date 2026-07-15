//! Core data model for grs.
//!
//! - One **session** = one user-named observation period. Lifetime = TUI process.
//! - One **snap** = one full project tree, captured at a moment in time.
//!
//! Storage layout (per project):
//! ```text
//! <project>/
//!   .grs/
//!     sessions/
//!       <slug>_<ulid>/
//!         meta.toml              # SessionMeta (TOML)
//!         snap-0001.json         # SnapJson (one file per snap; whole project tree)
//!         snap-0002.json
//!         ...
//! ```
//!
//! Snaps are no longer stored as a folder of file copies + a per-snap
//! `meta.toml`. They are stored as a single JSON file per snap, holding:
//!
//! - the full text of every tracked file at this snap (so the file view
//!   can render without re-reading from disk), and
//! - per-file diff metadata vs. the previous snap (added line numbers,
//!   and the text of removed lines keyed by their line number in the
//!   previous file).
//!
//! All structures `Serialize`/`Deserialize` and map 1:1 to the on-disk
//! `meta.toml` (session) and `snap-NNNN.json` (snap) files.

use crate::ulid::SessionId;
use crate::util::time::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema version. Bump on breaking changes.
///
/// - v1: per-snap `meta.toml` + tree of file copies under `snap-N/`.
/// - v2: per-snap `snap-NNNN.json` carrying the full file content and
///   diff metadata for **multiple files in one snap**.
/// - v3: per-snap `snap-NNNN.json` with **exactly one file per snap**
///   (the file path at the top level as `file_path`, the file data in
///   a single-entry `files` array). A save that touches N files
///   produces N snaps. v1/v2 dirs are not auto-migrated.
pub const STORAGE_VERSION: u32 = 3;

// -----------------------------------------------------------------------------
// Session
// -----------------------------------------------------------------------------

/// A session = one observation period, one project tree timeline.
///
/// Folder layout: `<project>/.grs/sessions/<slug>_<ulid>/`
/// - `meta.toml` (this struct)
/// - `snap-1/`, `snap-2/`, ... (whole project tree per snap)
///   - `meta.toml` (per-snap metadata: `SnapMeta`)
///   - full file tree copy
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    /// Human-given name. Must be unique per project. Renameable.
    pub name: String,
    pub id: SessionId,
    /// Original slug (lowercase, hyphens) derived from the name at session
    /// start. Stays stable across renames.
    pub slug: String,
    pub started_at: Millis,
    /// `None` for the currently-open session.
    pub ended_at: Option<Millis>,
    /// Number of snaps captured (snap-1, snap-2, ...).
    pub snap_count: u32,
}

impl SessionMeta {
    pub fn is_open(&self) -> bool {
        self.ended_at.is_none()
    }

    /// Create a freshly-opened session.
    pub fn new_open(name: String, id: SessionId, slug: String, started_at: Millis) -> Self {
        Self {
            version: STORAGE_VERSION,
            name,
            id,
            slug,
            started_at,
            ended_at: None,
            snap_count: 0,
        }
    }
}

// -----------------------------------------------------------------------------
// Snap
// -----------------------------------------------------------------------------

/// One snap = one JSON file on disk: `<session>/snap-NNNN.json`.
///
/// Each snap captures the change to **exactly one file** (new,
/// modified, or deleted). A save that touches N files produces N
/// consecutive snaps. The `file_path` field is the path of the file
/// that changed; the `files` array always has exactly one entry
/// (kept as a vec for shape stability with the v2 schema and to carry
/// the per-file data — `content`, `prev_content`, `added_lines`, etc.).
///
/// The `tree_sha` field carries a SHA-256 of the **full** project tree
/// at the moment this snap was captured. All snaps produced by the
/// same save share the same `tree_sha` (the tree state is the same
/// for all of them). The watcher's dedupe compares the current
/// tree's `tree_sha` to the most recent snap's `tree_sha` — if equal,
/// no new capture is needed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapJson {
    pub version: u32,
    /// 1-based snap number within the session (snap 1, snap 2, ...).
    pub n: u32,
    pub timestamp: Millis,
    /// Repo-relative path of the file that changed in this snap.
    /// Promoted to the top level for easy access (the TUI title, the
    /// per-snap header in the chronological view).
    pub file_path: String,
    /// SHA-256 of the sorted `path\tsha256` lines for every tracked
    /// file at the moment this snap was captured. Used for
    /// watch-event dedupe. Empty means "treat as always-changed".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tree_sha: String,
    /// Per-file entries. Always exactly one entry in v3 (kept as a
    /// vec for schema stability).
    pub files: Vec<SnapFileJson>,
}

/// Per-file entry in a snap JSON.
///
/// - `content` is the full text of the file at this snap (UTF-8 for text
///   files; binary files use a `(binary file, N bytes)` placeholder, see
///   `util::fs::read_content_or_binary_placeholder`).
/// - For a brand-new file or a file whose content did not change vs. the
///   previous snap, only `path` and `content` are set.
/// - For a file that did change, `prev_content` carries the text of the
///   file at the previous snap, `added_lines` lists the 1-based line
///   numbers in `content` that are new in this snap, and `removed_lines`
///   is a map from 1-based line number in `prev_content` to the text of
///   that line (the text is inlined so the file view can render removed
///   rows without referring back to the previous snap's JSON).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapFileJson {
    /// Repo-relative path, forward slashes.
    pub path: String,
    /// Full text of the file at this snap. For a deleted file, this is
    /// empty (the file no longer exists) and `prev_content` carries the
    /// prior text.
    pub content: String,
    /// True if the file looks binary.
    #[serde(default)]
    pub binary: bool,
    /// File size in bytes (of the on-disk file at capture time).
    #[serde(default)]
    pub size: u64,
    /// True if the file was removed in this snap (existed at N-1, gone
    /// at N). `content` is empty; `prev_content` carries the prior text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// The text of the file at the previous snap. Present only when this
    /// file changed between snap N-1 and snap N (modification or
    /// deletion).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prev_content: Option<String>,
    /// 1-based line numbers in `content` that are new in this snap.
    /// Present only when this file changed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub added_lines: Option<Vec<u32>>,
    /// 1-based line number in `prev_content` -> text of that line.
    /// Present only when this file changed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub removed_lines: Option<BTreeMap<u32, String>>,
}
