//! `SessionStore` — read/write gate to the `sessions/<slug>_<ulid>/meta.toml`
//! layout.
//!
//! Sessions are user-named. The on-disk folder is `<slug>_<ulid>/` so two
//! sessions with the same slug (e.g. two "refactor" sessions on the same
//! project, started in the same millisecond) cannot collide.

use crate::error::{GrsError, Result};
use crate::model::SessionMeta;
use crate::paths::{slugify, GrsPaths};
use crate::ulid::SessionId;
use crate::util::fs::atomic_write_str;
use crate::util::time::{now_ms, Millis};
use std::path::PathBuf;

pub struct SessionStore {
    paths: GrsPaths,
}

impl SessionStore {
    pub fn new(paths: GrsPaths) -> Self {
        Self { paths }
    }

    /// List all sessions, newest-first by `started_at`.
    pub fn list(&self) -> Result<Vec<SessionMeta>> {
        let mut sessions = Vec::new();
        if !self.paths.sessions_dir.is_dir() {
            return Ok(sessions);
        }
        for entry in std::fs::read_dir(&self.paths.sessions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            // Folder name format: `<slug>_<26-char-ulid>`. We just need the
            // ULID prefix to load meta.toml; the rest is decoration.
            let folder = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = parse_ulid_from_folder(&folder) {
                if let Ok(s) = self.get(&id) {
                    sessions.push(s);
                }
            }
        }
        sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(sessions)
    }

    /// Read one session's `meta.toml` by id.
    pub fn get(&self, id: &SessionId) -> Result<SessionMeta> {
        let meta = self.paths.session_meta(id);
        let text = std::fs::read_to_string(&meta).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GrsError::NotFound(format!("session {id} has no meta.toml"))
            } else {
                GrsError::from(e)
            }
        })?;
        let s: SessionMeta = toml::from_str(&text)?;
        Ok(s)
    }

    /// Resolve a name or id (or unique prefix of either) to a single session.
    /// Errors if zero or multiple sessions match.
    pub fn resolve(&self, name_or_id: &str) -> Result<SessionMeta> {
        let all = self.list()?;
        // Exact id match wins.
        if let Ok(id) = SessionId::parse(name_or_id) {
            if let Ok(s) = self.get(&id) {
                return Ok(s);
            }
        }
        // Exact name match wins.
        let mut exact_name: Option<SessionMeta> = None;
        let mut prefix_matches: Vec<SessionMeta> = Vec::new();
        for s in all {
            if s.name == name_or_id {
                if exact_name.is_some() {
                    return Err(GrsError::AmbiguousId(name_or_id.to_string()));
                }
                exact_name = Some(s.clone());
            } else if s.name.starts_with(name_or_id) || s.id.as_str().starts_with(name_or_id) {
                prefix_matches.push(s);
            }
        }
        if let Some(s) = exact_name {
            return Ok(s);
        }
        match prefix_matches.len() {
            0 => Err(GrsError::NotFound(format!("no session matching \"{name_or_id}\""))),
            1 => Ok(prefix_matches.into_iter().next().unwrap()),
            _ => Err(GrsError::AmbiguousId(name_or_id.to_string())),
        }
    }

    /// True if any existing session already has `name`.
    pub fn name_taken(&self, name: &str) -> Result<bool> {
        for s in self.list()? {
            if s.name == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Create a brand-new open session. Errors if the name is already in use.
    pub fn create_new(&self, name: String, started_at: Millis) -> Result<SessionMeta> {
        if name.trim().is_empty() {
            return Err(GrsError::InvalidName(name));
        }
        if self.name_taken(&name)? {
            return Err(GrsError::NameInUse(name));
        }
        let id = SessionId::new();
        let slug = slugify(&name);
        let session = SessionMeta::new_open(name, id.clone(), slug.clone(), started_at);
        std::fs::create_dir_all(self.paths.session_dir(&id))?;
        self.write_meta(&session)?;
        Ok(session)
    }

    /// Atomically (re)write a session's `meta.toml`.
    pub fn write_meta(&self, session: &SessionMeta) -> Result<()> {
        let text = toml::to_string_pretty(session).map_err(|e| GrsError::Config(e.to_string()))?;
        atomic_write_str(&self.paths.session_meta(&session.id), &text)?;
        Ok(())
    }

    /// Rename a session. The on-disk folder name does NOT change (the slug
    /// is part of the folder, but the folder keeps its original slug even
    /// after rename — the current name is in `meta.toml`).
    pub fn rename(&self, name_or_id: &str, new_name: String) -> Result<SessionMeta> {
        if new_name.trim().is_empty() {
            return Err(GrsError::InvalidName(new_name));
        }
        let mut session = self.resolve(name_or_id)?;
        if session.name == new_name {
            return Ok(session);
        }
        if self.name_taken(&new_name)? {
            return Err(GrsError::NameInUse(new_name));
        }
        session.name = new_name;
        self.write_meta(&session)?;
        Ok(session)
    }

    /// Finalize a session: set `ended_at` and persist.
    pub fn finalize(
        &self,
        id: &SessionId,
        ended_at: Millis,
        snap_count: u32,
    ) -> Result<SessionMeta> {
        let mut session = self.get(id)?;
        session.ended_at = Some(ended_at);
        session.snap_count = snap_count;
        self.write_meta(&session)?;
        Ok(session)
    }

    /// Update only the snap count (called after each snap capture).
    pub fn update_snap_count(&self, id: &SessionId, snap_count: u32) -> Result<()> {
        let mut session = self.get(id)?;
        session.snap_count = snap_count;
        self.write_meta(&session)
    }

    /// Path to a session's directory.
    pub fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.paths.session_dir(id)
    }
}

/// Parse a session folder name back to the `SessionId`. Accepts either:
/// - a bare 26-char ULID (the canonical form)
/// - `<slug>_<26-char-ulid>` for human-readable folder names
fn parse_ulid_from_folder(folder: &str) -> Option<SessionId> {
    if folder.len() == 26 {
        return SessionId::parse(folder).ok();
    }
    if folder.len() > 27 && folder.as_bytes().get(folder.len() - 27) == Some(&b'_') {
        let ulid_part = &folder[folder.len() - 26..];
        return SessionId::parse(ulid_part).ok();
    }
    None
}

/// Helper: current wall-clock ms (re-exported for commands).
pub fn fresh_started_at() -> Millis {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store(dir: &PathBuf) -> SessionStore {
        SessionStore::new(GrsPaths::new(dir))
    }

    fn setup(dir: &PathBuf) {
        std::fs::create_dir_all(dir.join(".grs/sessions")).unwrap();
    }

    #[test]
    fn create_and_get() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let s = st.create_new("refactor".into(), 1000).unwrap();
        assert!(s.is_open());
        assert_eq!(s.name, "refactor");
        assert_eq!(s.slug, "refactor");
        let back = st.get(&s.id).unwrap();
        assert_eq!(back.name, "refactor");
        assert_eq!(back.started_at, 1000);
        assert!(back.ended_at.is_none());
    }

    #[test]
    fn list_newest_first() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let a = st.create_new("alpha".into(), 1000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = st.create_new("beta".into(), 2000).unwrap();
        let list = st.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
    }

    #[test]
    fn name_must_be_unique() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        st.create_new("foo".into(), 1000).unwrap();
        let err = st.create_new("foo".into(), 2000).unwrap_err();
        assert!(matches!(err, GrsError::NameInUse(_)));
    }

    #[test]
    fn empty_name_rejected() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let err = st.create_new("   ".into(), 1000).unwrap_err();
        assert!(matches!(err, GrsError::InvalidName(_)));
    }

    #[test]
    fn rename_keeps_id_and_folder() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let s = st.create_new("old name".into(), 1000).unwrap();
        let folder = st.session_dir(&s.id);
        let folder_str = folder.to_string_lossy().to_string();
        let renamed = st.rename("old name", "new name".into()).unwrap();
        assert_eq!(renamed.id, s.id);
        assert_eq!(renamed.name, "new name");
        // Folder is untouched (still uses original slug).
        assert!(folder.is_dir());
        assert_eq!(
            folder.to_string_lossy().to_string(),
            folder_str,
            "rename must not move the folder",
        );
    }

    #[test]
    fn rename_rejects_duplicate_name() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        st.create_new("alpha".into(), 1000).unwrap();
        st.create_new("beta".into(), 2000).unwrap();
        let err = st.rename("alpha", "beta".into()).unwrap_err();
        assert!(matches!(err, GrsError::NameInUse(_)));
    }

    #[test]
    fn resolve_by_name_id_and_prefix() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let s = st.create_new("refactor-auth".into(), 1000).unwrap();
        // by exact name
        assert_eq!(st.resolve("refactor-auth").unwrap().id, s.id);
        // by exact id
        assert_eq!(st.resolve(&s.id.as_str()).unwrap().id, s.id);
        // by id prefix
        let prefix = &s.id.as_str()[..8];
        assert_eq!(st.resolve(prefix).unwrap().id, s.id);
    }

    #[test]
    fn finalize_sets_ended_at_and_snap_count() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        setup(&p);
        let st = store(&p);
        let s = st.create_new("foo".into(), 1000).unwrap();
        let fin = st.finalize(&s.id, 5000, 7).unwrap();
        assert_eq!(fin.ended_at, Some(5000));
        assert_eq!(fin.snap_count, 7);
        let back = st.get(&s.id).unwrap();
        assert!(!back.is_open());
    }
}
