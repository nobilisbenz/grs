//! `RepoStore` — the top-level facade the CLI talks to.

use crate::config::Config;
use crate::error::{GrsError, Result};
use crate::ignore::{IgnoreMatcher, DEFAULT_GRSIGNORE};
use crate::model::SessionMeta;
use crate::paths::{GrsPaths, GRS_DIR};
use crate::session::SessionStore;
use crate::snap::SnapStore;
use crate::ulid::SessionId;
use crate::util::fs::{atomic_write_str, read_to_string_or};
use crate::util::time::{now_ms, Millis};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RepoStore {
    root: PathBuf,
    paths: GrsPaths,
    config: Config,
}

impl RepoStore {
    /// Open an existing repo: find `.grs/` walking up from `start`.
    pub fn open_from(start: &Path) -> Result<Self> {
        let root = crate::paths::find_grs_root(start)?;
        Self::open(&root)
    }

    /// Open an existing repo at an exact root that already has `.grs/`.
    pub fn open(root: &Path) -> Result<Self> {
        if !root.join(GRS_DIR).is_dir() {
            return Err(GrsError::NotInitialized);
        }
        let paths = GrsPaths::new(root);
        let user = Config::load_user();
        let repo = Config::load_repo(root)?;
        let config = user.merged_with(repo);
        Ok(Self {
            root: root.to_path_buf(),
            paths,
            config,
        })
    }

    /// Open the repo at `root`, initializing it on the fly if `.grs/` is
    /// missing.
    pub fn open_or_init(root: &Path) -> Result<Self> {
        if root.join(GRS_DIR).is_dir() {
            Self::open(root)
        } else {
            Self::init(root)
        }
    }

    /// Initialize a brand-new repo at `root`: create the full `.grs/` tree,
    /// write the default `.grsignore`, prompt the user for a session name
    /// (out-of-band), create the first open session, set HEAD, capture
    /// snap 1.
    pub fn init(root: &Path) -> Result<Self> {
        if root.join(GRS_DIR).exists() {
            return Err(GrsError::AlreadyInitialized);
        }
        std::fs::create_dir_all(root.join(GRS_DIR).join(crate::paths::SESSIONS_DIR))?;

        let paths = GrsPaths::new(root);
        atomic_write_str(&paths.config, Config::default_toml())?;
        atomic_write_str(&root.join(".grsignore"), DEFAULT_GRSIGNORE)?;

        let store = RepoStore {
            root: root.to_path_buf(),
            paths,
            config: Config::default(),
        };
        // Caller is expected to follow up with `rotate_open_session` (or
        // pass a name into a higher-level helper) — `init` only sets up
        // the on-disk tree. The TUI prompts the user for a name when
        // it sees no open session.
        Ok(store)
    }

    /// Create the first open session, capture snap 1, and set HEAD. Used
    /// by the TUI right after `init` / when no open session exists.
    pub fn open_first_session(&self, name: String) -> Result<SessionMeta> {
        let session = self.sessions().create_new(name, now_ms())?;
        self.set_head(&session.id)?;
        // Capture snap 1 = state of project at session start. A save
        // that touches N files produces N snaps; the count is the
        // number written.
        let ignore = self.ignore_matcher()?;
        let initial_snaps = self.snaps().capture(&session.id, &ignore)?;
        // Re-read so the in-memory copy reflects the updated snap_count.
        let mut session = self.sessions().get(&session.id)?;
        session.snap_count = initial_snaps.len() as u32;
        if session.snap_count == 0 {
            // Empty project: still allocate snap-1 with no files, so
            // the session has at least one record and downstream
            // dedupe / view logic can rely on the existence of a snap.
            let ts = crate::util::time::now_ms();
            let snap = crate::model::SnapJson {
                version: crate::model::STORAGE_VERSION,
                n: 1,
                timestamp: ts,
                file_path: String::new(),
                tree_sha: String::new(),
                files: Vec::new(),
            };
            let dest = self.paths().snap_file(&session.id, 1);
            crate::snap::write_snap_json_pub(&dest, &snap)?;
            session.snap_count = 1;
        }
        self.sessions().write_meta(&session)?;
        Ok(session)
    }

    /// Finalize the current open session (if HEAD points to one) and
    /// create a new open session with `name`, moving HEAD to it and
    /// capturing snap 1. Returns the new session.
    pub fn rotate_open_session(&self, name: String, now: Millis) -> Result<SessionMeta> {
        if let Some(head_id) = self.head()? {
            if let Ok(session) = self.sessions().get(&head_id) {
                if session.is_open() {
                    let snap_count = self.snaps().count(&head_id).unwrap_or(0);
                    self.sessions().finalize(&head_id, now, snap_count)?;
                }
            }
        }
        self.open_first_session(name)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn paths(&self) -> &GrsPaths {
        &self.paths
    }
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn sessions(&self) -> SessionStore {
        SessionStore::new(self.paths.clone())
    }
    pub fn snaps(&self) -> SnapStore {
        SnapStore::new(self.paths.clone())
    }

    /// Build an ignore matcher for this repo (with config's extra patterns).
    pub fn ignore_matcher(&self) -> Result<IgnoreMatcher> {
        IgnoreMatcher::new(&self.root, &self.config.watcher.ignore_extra)
    }

    /// Read `.grs/HEAD` → the currently-open session id.
    pub fn head(&self) -> Result<Option<SessionId>> {
        let text = read_to_string_or(&self.paths.head, "")?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        // HEAD contents are `<slug>_<ulid>` (the folder name) for stability
        // against the meta-only case.
        if let Some(id) = crate::paths::parse_session_folder(trimmed) {
            return Ok(Some(id));
        }
        // Tolerate a bare ULID too.
        SessionId::parse(trimmed).map(Some)
    }

    /// Atomically write `.grs/HEAD`.
    pub fn set_head(&self, id: &SessionId) -> Result<()> {
        let folder = self.sessions().session_dir(id);
        let folder_name = folder.file_name().unwrap().to_string_lossy().to_string();
        atomic_write_str(&self.paths.head, &format!("{folder_name}\n"))?;
        Ok(())
    }

    /// The currently-open session id (from HEAD), if any.
    pub fn current_session_id(&self) -> Result<Option<SessionId>> {
        self.head()
    }

    /// The currently-open session, if any.
    pub fn current_session(&self) -> Result<Option<SessionMeta>> {
        match self.head()? {
            Some(id) => Ok(Some(self.sessions().get(&id)?)),
            None => Ok(None),
        }
    }

    /// Remove a session's directory. Errors with `GrsError::SessionOpen` if
    /// it's still open.
    pub fn delete_session(&self, id: &SessionId) -> Result<()> {
        let session = self
            .sessions()
            .get(id)
            .map_err(|_| GrsError::NotFound(format!("session {id}")))?;
        if session.is_open() {
            return Err(GrsError::SessionOpen(id.clone()));
        }
        let dir = self.paths.session_dir(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Acquire the project lock. Returns a `LockGuard` that releases the
    /// lock on drop. Errors with `GrsError::AlreadyRunning` if another
    /// TUI is already running on this project.
    pub fn lock(&self) -> Result<crate::lockfile::LockGuard> {
        crate::lockfile::LockGuard::acquire(&self.paths.lock)
    }
}

/// `now_ms` re-export for commands that need a fresh timestamp.
pub fn fresh_now() -> Millis {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_session(dir: &Path) -> (RepoStore, SessionMeta) {
        let store = RepoStore::init(dir).unwrap();
        let s = store.open_first_session("first".into()).unwrap();
        (store, s)
    }

    #[test]
    fn init_creates_tree_but_no_session() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        assert!(dir.path().join(".grs/config.toml").exists());
        assert!(dir.path().join(".grsignore").exists());
        assert!(dir.path().join(".grs/sessions").is_dir());
        // No session yet.
        assert!(store.head().unwrap().is_none());
    }

    #[test]
    fn init_twice_errors() {
        let dir = tempdir().unwrap();
        RepoStore::init(dir.path()).unwrap();
        let err = RepoStore::init(dir.path()).unwrap_err();
        assert!(matches!(err, GrsError::AlreadyInitialized));
    }

    #[test]
    fn open_finds_existing() {
        let dir = tempdir().unwrap();
        RepoStore::init(dir.path()).unwrap();
        let store = RepoStore::open(dir.path()).unwrap();
        assert_eq!(store.root(), dir.path());
    }

    #[test]
    fn open_first_session_captures_snap_1() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "world\n").unwrap();
        let (store, s) = open_session(dir.path());
        assert!(s.is_open());
        // With one-file-per-snap, the first save (which has 2 files)
        // produces 2 snaps. snap_count tracks the highest snap number.
        assert_eq!(s.snap_count, 2);
        // Snap 1 and snap 2 are separate JSON files, each with one file.
        let snap1 = store.snaps().read(&s.id, 1).unwrap();
        let snap2 = store.snaps().read(&s.id, 2).unwrap();
        assert_eq!(snap1.files.len(), 1);
        assert_eq!(snap2.files.len(), 1);
        let paths: std::collections::HashSet<&str> =
            [snap1.file_path.as_str(), snap2.file_path.as_str()].into_iter().collect();
        assert!(paths.contains("a.txt"));
        assert!(paths.contains("b.txt"));
        assert!(store.paths().snap_file(&s.id, 1).is_file());
        assert!(store.paths().snap_file(&s.id, 2).is_file());
    }

    #[test]
    fn open_first_session_unique_name() {
        let dir = tempdir().unwrap();
        let (store, _) = open_session(dir.path());
        let err = store.open_first_session("first".into()).unwrap_err();
        assert!(matches!(err, GrsError::NameInUse(_)));
    }

    #[test]
    fn rotate_open_session_finalizes_old_and_starts_new() {
        let dir = tempdir().unwrap();
        let (store, first) = open_session(dir.path());
        let original_id = first.id.clone();
        // Add a new file and capture snap 2.
        std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
        let snap = store.snaps().capture(&original_id, &store.ignore_matcher().unwrap()).unwrap();
        let last_n = snap.last().map(|s| s.n).unwrap_or(0);
        store.sessions().update_snap_count(&original_id, last_n).unwrap();

        // Rotate: finalize old, start new.
        let new = store.rotate_open_session("second".into(), 5000).unwrap();
        assert_ne!(new.id, original_id);
        assert!(new.is_open());
        assert_eq!(store.head().unwrap(), Some(new.id.clone()));

        // Old session is finalized.
        let finalized = store.sessions().get(&original_id).unwrap();
        assert!(!finalized.is_open());
        assert_eq!(finalized.ended_at, Some(5000));
        assert!(finalized.snap_count >= 2);
    }

    #[test]
    fn delete_session_refuses_open() {
        let dir = tempdir().unwrap();
        let (store, s) = open_session(dir.path());
        let err = store.delete_session(&s.id).unwrap_err();
        assert!(matches!(err, GrsError::SessionOpen(_)));
    }

    #[test]
    fn delete_session_removes_closed() {
        let dir = tempdir().unwrap();
        let (store, first) = open_session(dir.path());
        // Force the first session closed by overwriting HEAD with a
        // different id, then call delete_session on the original.
        let first_id = first.id.clone();
        let new = store.rotate_open_session("second".into(), 1000).unwrap();
        let _ = new;
        // Now first is closed, deletable.
        let session_dir = store.paths().session_dir(&first_id);
        assert!(session_dir.is_dir());
        store.delete_session(&first_id).unwrap();
        assert!(!session_dir.exists());
    }

    #[test]
    fn lock_acquired_and_released() {
        let dir = tempdir().unwrap();
        let (store, _) = open_session(dir.path());
        let _g = store.lock().unwrap();
        // Second attempt must fail.
        let err = store.lock().unwrap_err();
        assert!(matches!(err, GrsError::AlreadyRunning(_)));
    }
}
