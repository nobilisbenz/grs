//! Core data model — the "normalize everything into one model" structs.
//! All derive `serde::Serialize`/`Deserialize` and map 1:1 to the on-disk JSON.

use crate::ulid::SessionId;
use crate::util::time::Millis;
use serde::{Deserialize, Serialize};

/// On-disk schema version. Bump on breaking changes; the reader is
/// version-tolerant (unknown fields ignored, missing version => 1).
pub const STORAGE_VERSION: u32 = 2;

/// One session = one replay timeline. Sessions are lightweight containers
/// for snaps; there is no prompt/message metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: SessionId,
    pub started_at: Millis,
    /// `None` for the currently-open session.
    pub ended_at: Option<Millis>,
    pub file_count: u32,
    pub snap_count: u32,
}

impl Session {
    /// Create a freshly-opened session.
    pub fn new_open(id: SessionId, started_at: Millis) -> Self {
        Self {
            version: STORAGE_VERSION,
            id,
            started_at,
            ended_at: None,
            file_count: 0,
            snap_count: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// One micro-snapshot = one file-save event (debounced). Stored as one JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snap {
    pub version: u32,
    /// Zero-padded 4-digit sequence within the session.
    pub seq: u32,
    pub timestamp: Millis,
    /// ISO-8601 convenience for humans/GUI; canonical field is `timestamp`.
    pub timestamp_iso: String,
    /// Repo-relative, forward slashes.
    pub file_path: String,
    /// FULL file content at this point in time.
    pub content: String,
    pub diff: LineDiff,
    /// `seq` of the previous snap of this same file; `None` for the first.
    pub prev_seq: Option<u32>,
}

/// Precomputed line-level diff vs the previous snap of the same file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LineDiff {
    /// 1-based line numbers in `content` that are newly added.
    pub added_lines: Vec<usize>,
    /// 1-based line numbers in the PREVIOUS content that no longer exist.
    pub removed_lines: Vec<usize>,
    /// `seq` of the previous snap of this same file.
    pub prev_seq: Option<u32>,
}
