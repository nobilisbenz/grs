//! `SessionStore` — read/write gates to the `sessions/<ulid>/meta.json` layout.

use crate::error::{GrsError, Result};
use crate::model::Session;
use crate::paths::GrsPaths;
use crate::ulid::SessionId;
use crate::util::time::{now_ms, Millis};
use std::path::PathBuf;

pub struct SessionStore {
    paths: GrsPaths,
}

impl SessionStore {
    pub fn new(paths: GrsPaths) -> Self {
        Self { paths }
    }

    /// List all sessions, newest-first (ULIDs sort chronologically).
    pub fn list(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        if !self.paths.sessions_dir.is_dir() {
            return Ok(sessions);
        }
        for entry in std::fs::read_dir(&self.paths.sessions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let id = match SessionId::parse(&name) {
                Ok(id) => id,
                Err(_) => continue, // skip non-ulid dirs (stray files)
            };
            if let Ok(s) = self.get(&id) {
                sessions.push(s);
            }
        }
        sessions.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(sessions)
    }

    /// Read one session's `meta.json`. Errors if the dir exists but meta is
    /// missing/corrupt.
    pub fn get(&self, id: &SessionId) -> Result<Session> {
        let meta = self.paths.session_meta(id);
        let text = std::fs::read_to_string(&meta).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GrsError::NotFound(format!("session {id} has no meta.json"))
            } else {
                GrsError::from(e)
            }
        })?;
        let s: Session = serde_json::from_str(&text)?;
        Ok(s)
    }

    /// Resolve a possibly-prefix id to a single session. Errors if it matches
    /// zero or multiple sessions.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<Session> {
        let candidates: Vec<Session> = self
            .list()?
            .into_iter()
            .filter(|s| s.id.as_str().starts_with(prefix))
            .collect();
        match candidates.len() {
            0 => Err(GrsError::NotFound(format!("no session matching \"{prefix}\""))),
            1 => Ok(candidates.into_iter().next().unwrap()),
            _ => Err(GrsError::AmbiguousId(prefix.to_string())),
        }
    }

    /// Create a brand-new open session: `<ulid>/meta.json` (open, no prompt)
    /// plus an empty `snaps/` dir. Returns the new session.
    pub fn create_new(&self, started_at: Millis) -> Result<Session> {
        let id = SessionId::new();
        let session = Session::new_open(id.clone(), started_at);
        let dir = self.paths.session_dir(&id);
        std::fs::create_dir_all(dir.join(crate::paths::SNAP_DIR_NAME))?;
        self.write_meta(&session)?;
        Ok(session)
    }

    /// Atomically (re)write a session's `meta.json`.
    pub fn write_meta(&self, session: &Session) -> Result<()> {
        let json = serde_json::to_vec_pretty(session)?;
        crate::util::fs::atomic_write(&self.paths.session_meta(&session.id), &json)?;
        Ok(())
    }

    /// Finalize a session: set `ended_at` and recompute counts from `snaps/`.
    /// Atomically rewrites `meta.json`.
    pub fn finalize(
        &self,
        id: &SessionId,
        ended_at: Millis,
        snap_count: u32,
        file_count: u32,
    ) -> Result<Session> {
        let mut session = self.get(id)?;
        session.ended_at = Some(ended_at);
        session.snap_count = snap_count;
        session.file_count = file_count;
        self.write_meta(&session)?;
        Ok(session)
    }

    /// The directory holding a session's snaps.
    pub fn snaps_dir(&self, id: &SessionId) -> PathBuf {
        self.paths.session_snaps(id)
    }
}

/// Helper: the current wall-clock ms (re-exported for commands).
pub fn fresh_started_at() -> Millis {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn store(dir: &Path) -> SessionStore {
        SessionStore::new(GrsPaths::new(dir))
    }

    #[test]
    fn create_and_get() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".grs/sessions")).unwrap();
        let st = store(dir.path());
        let s = st.create_new(1000).unwrap();
        assert!(s.is_open());
        let back = st.get(&s.id).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.started_at, 1000);
        assert!(back.ended_at.is_none());
    }

    #[test]
    fn list_newest_first() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".grs/sessions")).unwrap();
        let st = store(dir.path());
        let a = st.create_new(1000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = st.create_new(2000).unwrap();
        let list = st.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b.id); // newest first
        assert_eq!(list[1].id, a.id);
    }

    #[test]
    fn finalize_sets_counts() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".grs/sessions")).unwrap();
        let st = store(dir.path());
        let s = st.create_new(1000).unwrap();
        let fin = st.finalize(&s.id, 5000, 7, 3).unwrap();
        assert_eq!(fin.ended_at, Some(5000));
        assert_eq!(fin.snap_count, 7);
        assert_eq!(fin.file_count, 3);
        let back = st.get(&s.id).unwrap();
        assert_eq!(back.snap_count, 7);
    }

    #[test]
    fn resolve_prefix_unique() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".grs/sessions")).unwrap();
        let st = store(dir.path());
        let s = st.create_new(1000).unwrap();
        let prefix = &s.id.as_str()[..8];
        let resolved = st.resolve_prefix(prefix).unwrap();
        assert_eq!(resolved.id, s.id);
    }
}
