//! Core data model for grs.
//!
//! - One **session** = one user-named observation period. Lifetime = TUI process.
//! - One **snap** = one full project tree, captured at a moment in time.
//! - Diffs are always consecutive (snap N vs snap N-1).
//!
//! All structures `Serialize`/`Deserialize` and map 1:1 to the on-disk
//! `meta.toml` and `snap-N/meta.toml` files.

use crate::ulid::SessionId;
use crate::util::time::Millis;
use serde::{Deserialize, Serialize};

/// On-disk schema version. Bump on breaking changes; the reader is
/// version-tolerant (unknown fields ignored, missing version => 1).
pub const STORAGE_VERSION: u32 = 1;

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

/// Metadata for one snap. Stored at `<session>/snap-N/meta.toml` alongside
/// the full project tree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapMeta {
    pub version: u32,
    /// 1-based snap number within the session (snap 1, snap 2, ...).
    pub n: u32,
    pub timestamp: Millis,
    /// Convenience: total file count in this snap.
    pub file_count: u32,
    /// Convenience: total byte count of all text files in this snap.
    pub total_bytes: u64,
    /// Per-file (path, sha256) for rename detection.
    /// Paths are repo-relative with forward slashes.
    pub files: Vec<SnapFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapFile {
    /// Repo-relative path, forward slashes.
    pub path: String,
    /// SHA-256 of the file content. For binary files, still hashed; the
    /// `binary` flag is set.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
    /// True if the file looks binary (NUL byte in first 8KiB).
    pub binary: bool,
}
