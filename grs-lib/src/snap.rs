//! `SnapStore` — read/write gate to the `sessions/<id>/snaps/NNNN_<ts>.json`
//! layout. Snaps are written atomically and sorted by `seq`.

use crate::error::Result;
use crate::model::{LineDiff, Snap};
use crate::paths::GrsPaths;
use crate::ulid::SessionId;
use crate::util::time::{now_ms, iso, Millis};
use std::path::PathBuf;

pub struct SnapStore {
    paths: GrsPaths,
}

/// A lightweight listing entry (seq + timestamp + filename) without reading
/// the full snap content — used by replay's lazy load (`grs replay` only
/// fully reads the *current* snap).
#[derive(Clone, Debug)]
pub struct SnapEntry {
    pub seq: u32,
    pub timestamp: Millis,
    pub path: PathBuf,
}

impl SnapStore {
    pub fn new(paths: GrsPaths) -> Self {
        Self { paths }
    }

    fn snaps_dir(&self, id: &SessionId) -> PathBuf {
        self.paths.session_snaps(id)
    }

    /// List snap entries for a session, sorted by `seq` ascending. Reads only
    /// filenames (no content) — instant.
    pub fn list(&self, id: &SessionId) -> Result<Vec<SnapEntry>> {
        let dir = self.snaps_dir(id);
        let mut entries = Vec::new();
        if !dir.is_dir() {
            return Ok(entries);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            // Filename: `NNNN_<timestamp>.json`. The seq is parsed from JSON
            // in `read`, but for a cheap listing we parse the leading seq.
            let (seq, ts) = parse_filename(&name).unwrap_or((0, 0));
            entries.push(SnapEntry {
                seq,
                timestamp: ts,
                path: entry.path(),
            });
        }
        entries.sort_by_key(|e| e.seq);
        Ok(entries)
    }

    /// Read one snap's full JSON.
    pub fn read(&self, id: &SessionId, seq: u32) -> Result<Snap> {
        // Find the file matching the seq (filename leading digits).
        let dir = self.snaps_dir(id);
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some((s, _)) = parse_filename(&name) {
                if s == seq {
                    let text = std::fs::read_to_string(entry.path())?;
                    let snap: Snap = serde_json::from_str(&text)?;
                    return Ok(snap);
                }
            }
        }
        Err(crate::error::GrsError::NotFound(format!(
            "snap {seq} not found in session {id}"
        )))
    }

    /// Read a snap by its file path directly (used by replay, which already
    /// has the listing).
    pub fn read_path(path: &std::path::Path) -> Result<Snap> {
        let text = std::fs::read_to_string(path)?;
        let snap: Snap = serde_json::from_str(&text)?;
        Ok(snap)
    }

    /// The next sequence number for a session (one past the current max).
    pub fn next_seq(&self, id: &SessionId) -> Result<u32> {
        Ok(self
            .list(id)?
            .into_iter()
            .map(|e| e.seq)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0))
    }

    /// Write a new snap atomically. The `seq`, `timestamp`, and `timestamp_iso`
    /// are filled in if zero/empty. Returns the written snap.
    pub fn write(&self, id: &SessionId, mut snap: Snap) -> Result<Snap> {
        if snap.timestamp == 0 {
            snap.timestamp = now_ms();
        }
        if snap.timestamp_iso.is_empty() {
            snap.timestamp_iso = iso(snap.timestamp);
        }
        snap.version = crate::model::STORAGE_VERSION;
        let dir = self.snaps_dir(id);
        std::fs::create_dir_all(&dir)?;
        let filename = format!("{:04}_{}.json", snap.seq, snap.timestamp);
        let path = dir.join(filename);
        let json = serde_json::to_vec_pretty(&snap)?;
        crate::util::fs::atomic_write(&path, &json)?;
        crate::util::fs::fsync_dir(&dir);
        Ok(snap)
    }

    /// Build a snap struct from the watcher's per-file state.
    pub fn build_snap(
        seq: u32,
        file_path: String,
        content: String,
        diff: LineDiff,
        prev_seq: Option<u32>,
    ) -> Snap {
        let ts = now_ms();
        Snap {
            version: crate::model::STORAGE_VERSION,
            seq,
            timestamp: ts,
            timestamp_iso: iso(ts),
            file_path,
            content,
            diff,
            prev_seq,
        }
    }

    /// Count snaps in a session.
    pub fn count(&self, id: &SessionId) -> Result<u32> {
        self.list(id).map(|v| v.len() as u32)
    }

    /// Distinct file paths in a session, in first-appearance order.
    pub fn distinct_files(&self, id: &SessionId) -> Result<Vec<String>> {
        let mut files = Vec::new();
        for entry in self.list(id)? {
            let snap = self.read(id, entry.seq)?;
            if !files.contains(&snap.file_path) {
                files.push(snap.file_path);
            }
        }
        Ok(files)
    }
}

/// Parse `NNNN_<timestamp>.json` into `(seq, timestamp)`.
fn parse_filename(name: &str) -> Option<(u32, Millis)> {
    let stem = name.strip_suffix(".json")?;
    let (seq_part, ts_part) = stem.split_once('_')?;
    let seq: u32 = seq_part.parse().ok()?;
    let ts: Millis = ts_part.parse().ok()?;
    Some((seq, ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn store(dir: &Path) -> SnapStore {
        SnapStore::new(GrsPaths::new(dir))
    }

    fn make_session(dir: &Path) -> SessionId {
        let id = SessionId::new();
        std::fs::create_dir_all(dir.join(".grs/sessions").join(id.as_str()).join("snaps")).unwrap();
        id
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let st = store(dir.path());
        let id = make_session(dir.path());
        let snap = SnapStore::build_snap(
            0,
            "a.txt".into(),
            "hello\n".into(),
            LineDiff::default(),
            None,
        );
        let written = st.write(&id, snap).unwrap();
        assert_eq!(written.seq, 0);
        let back = st.read(&id, 0).unwrap();
        assert_eq!(back.content, "hello\n");
        assert_eq!(back.file_path, "a.txt");
    }

    #[test]
    fn next_seq_increments() {
        let dir = tempdir().unwrap();
        let st = store(dir.path());
        let id = make_session(dir.path());
        assert_eq!(st.next_seq(&id).unwrap(), 0);
        st.write(
            &id,
            SnapStore::build_snap(0, "a".into(), "x".into(), LineDiff::default(), None),
        )
        .unwrap();
        assert_eq!(st.next_seq(&id).unwrap(), 1);
        st.write(
            &id,
            SnapStore::build_snap(1, "a".into(), "y".into(), LineDiff::default(), Some(0)),
        )
        .unwrap();
        assert_eq!(st.next_seq(&id).unwrap(), 2);
    }

    #[test]
    fn list_sorted_by_seq() {
        let dir = tempdir().unwrap();
        let st = store(dir.path());
        let id = make_session(dir.path());
        for s in [2u32, 0, 1] {
            st.write(
                &id,
                SnapStore::build_snap(s, "a".into(), "x".into(), LineDiff::default(), None),
            )
            .unwrap();
        }
        let list = st.list(&id).unwrap();
        let seqs: Vec<u32> = list.into_iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn distinct_files_first_appearance() {
        let dir = tempdir().unwrap();
        let st = store(dir.path());
        let id = make_session(dir.path());
        for (s, f) in [(0u32, "a"), (1, "b"), (2, "a")] {
            st.write(
                &id,
                SnapStore::build_snap(s, f.into(), "x".into(), LineDiff::default(), None),
            )
            .unwrap();
        }
        let files = st.distinct_files(&id).unwrap();
        assert_eq!(files, vec!["a".to_string(), "b".to_string()]);
    }
}
