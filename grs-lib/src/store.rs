//! `RepoStore` — the top-level facade the CLI talks to (mirrors jj's
//! `WorkspaceCommandHelper` as the thing every command starts by obtaining).

use crate::config::Config;
use crate::error::{GrsError, Result};
use crate::ignore::{IgnoreMatcher, DEFAULT_GRSIGNORE};
use crate::model::Session;
use crate::paths::{GrsPaths, GRS_DIR, SNAP_DIR_NAME};
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
    /// missing. Used by TUI entry points so just running `grs` in any
    /// directory starts a fresh capture session.
    pub fn open_or_init(root: &Path) -> Result<Self> {
        if root.join(GRS_DIR).is_dir() {
            Self::open(root)
        } else {
            Self::init(root)
        }
    }

    /// Initialize a brand-new repo at `root`: create the full `.grs/` tree,
    /// write the default `.grsignore`, create the first open session, set HEAD.
    /// Errors with `AlreadyInitialized` if `.grs/` already exists.
    pub fn init(root: &Path) -> Result<Self> {
        if root.join(GRS_DIR).exists() {
            return Err(GrsError::AlreadyInitialized);
        }
        std::fs::create_dir_all(root.join(GRS_DIR).join(crate::paths::SESSIONS_DIR))?;

        let paths = GrsPaths::new(root);
        // config.toml with the documented defaults.
        atomic_write_str(&paths.config, Config::default_toml())?;
        // .grsignore with the default ignores (in the project root, not .grs/).
        atomic_write_str(&root.join(".grsignore"), DEFAULT_GRSIGNORE)?;

        // First open session + HEAD pointing at it.
        let store = RepoStore {
            root: root.to_path_buf(),
            paths: GrsPaths::new(root),
            config: Config::default(),
        };
        let session = store.sessions().create_new(now_ms())?;
        store.set_head(&session.id)?;
        // Re-load config (now that the file exists) so the returned store has it.
        let user = Config::load_user();
        let repo_cfg = Config::load_repo(root)?;
        Ok(RepoStore {
            root: store.root,
            paths: store.paths,
            config: user.merged_with(repo_cfg),
        })
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
        SessionId::parse(trimmed).map(Some)
    }

    /// Atomically write `.grs/HEAD`.
    pub fn set_head(&self, id: &SessionId) -> Result<()> {
        atomic_write_str(&self.paths.head, &format!("{id}\n"))?;
        Ok(())
    }

    /// The currently-open session (from HEAD), if any.
    pub fn current_session(&self) -> Result<Session> {
        let id = self.head()?.ok_or_else(|| {
            GrsError::NotFound("HEAD is missing; just run `grs` to open the TUI".to_string())
        })?;
        self.sessions().get(&id)
    }
}

/// Write a snap for a single file in the currently-open session. Used by
/// `grs add` (baseline) and the foreground watcher (one debounced write event).
///
/// `prev_content` is the file's content from the previous snap of this same
/// file (or `None` to diff against whatever the session already has for this
/// file). The `prev_seq` is always looked up from the session's existing snaps
/// for this file so the chain stays correct. Returns the snap seq used.
pub fn write_file_snap(
    store: &RepoStore,
    session_id: &crate::ulid::SessionId,
    file_path: &str,
    new_content: &str,
    prev_content: Option<&str>,
) -> Result<u32> {
    let snap_store = store.snaps();
    // Look up the most recent snap for this file in the session (if any).
    let prev_seq: Option<u32> = snap_store
        .list(session_id)?
        .into_iter()
        .filter(|e| e.path.file_name().map(|n| n.to_string_lossy().to_string())
            .and_then(|name| {
                let p = std::path::Path::new(&name);
                p.file_stem().map(|s| s.to_string_lossy().to_string())
            })
            .is_some())
        .filter_map(|e| snap_store.read(session_id, e.seq).ok())
        .filter(|s| s.file_path == file_path)
        .map(|s| s.seq)
        .max();
    // Decide what to diff against: caller-supplied prev_content (authoritative
    // for in-memory state) or the last snap on disk.
    let diff_prev: String = if let Some(p) = prev_content {
        p.to_string()
    } else {
        prev_seq
            .and_then(|s| snap_store.read(session_id, s).ok())
            .map(|s| s.content)
            .unwrap_or_default()
    };
    let diff = crate::diff::line_diff(&diff_prev, new_content);
    let mut diff = diff;
    diff.prev_seq = prev_seq;
    let seq = snap_store.next_seq(session_id)?;
    let snap = crate::snap::SnapStore::build_snap(
        seq,
        file_path.to_string(),
        new_content.to_string(),
        diff,
        prev_seq,
    );
    snap_store.write(session_id, snap)?;
    update_session_counts(store, session_id)?;
    Ok(seq)
}

/// Recompute and persist a session's `snap_count`/`file_count` from `snaps/`.
/// Called after every micro-snapshot so `grs status`, `grs log`, and the TUI
/// reflect current state for the open session.
pub fn update_session_counts(store: &RepoStore, id: &SessionId) -> Result<()> {
    let (snap_count, file_count) = recompute_counts(store, id)?;
    let sessions = store.sessions();
    let mut session = sessions.get(id)?;
    session.snap_count = snap_count;
    session.file_count = file_count;
    sessions.write_meta(&session)
}

/// Recompute a session's `snap_count`/`file_count` from `snaps/`.
pub fn recompute_counts(store: &RepoStore, id: &SessionId) -> Result<(u32, u32)> {
    let snaps = store.snaps().list(id)?;
    let snap_count = snaps.len() as u32;
    let mut files = std::collections::HashSet::new();
    for entry in snaps {
        if let Ok(snap) = store.snaps().read(id, entry.seq) {
            files.insert(snap.file_path);
        }
    }
    Ok((snap_count, files.len() as u32))
}

/// `now_ms` re-export for commands that need a fresh timestamp.
pub fn fresh_now() -> Millis {
    now_ms()
}

/// Sentinel used by tests/fixtures to ensure the snaps dir is in place.
pub fn ensure_snaps_dir(paths: &GrsPaths, id: &SessionId) -> Result<()> {
    std::fs::create_dir_all(paths.session_dir(id).join(SNAP_DIR_NAME))
        .map_err(GrsError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_tree_and_head() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        assert!(dir.path().join(".grs/HEAD").exists());
        assert!(dir.path().join(".grs/config.toml").exists());
        assert!(dir.path().join(".grsignore").exists());
        assert!(dir.path().join(".grs/sessions").is_dir());
        let head = store.head().unwrap().expect("HEAD set");
        let session = store.sessions().get(&head).unwrap();
        assert!(session.is_open());
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
    fn set_and_read_head() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.sessions().create_new(1234).unwrap();
        store.set_head(&s.id).unwrap();
        assert_eq!(store.head().unwrap().unwrap(), s.id);
    }

    #[test]
    fn write_file_snap_first_then_diff() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let s0 = write_file_snap(&store, &head, "a.txt", "hello\n", None).unwrap();
        assert_eq!(s0, 0);
        let s1 = write_file_snap(&store, &head, "a.txt", "hello world\n", Some("hello\n")).unwrap();
        assert_eq!(s1, 1);
        let snap = store.snaps().read(&head, 1).unwrap();
        assert_eq!(snap.prev_seq, Some(0));
        assert_eq!(snap.content, "hello world\n");
        assert_eq!(snap.diff.removed_lines, vec![1]);
        assert_eq!(snap.diff.added_lines, vec![1]);
    }

    #[test]
    fn write_file_snap_two_files_have_separate_seqs() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let a0 = write_file_snap(&store, &head, "a.txt", "a\n", None).unwrap();
        let b0 = write_file_snap(&store, &head, "b.txt", "b\n", None).unwrap();
        let a1 = write_file_snap(&store, &head, "a.txt", "a2\n", Some("a\n")).unwrap();
        assert_eq!(a0, 0);
        assert_eq!(b0, 1);
        assert_eq!(a1, 2);
    }

    #[test]
    fn recompute_counts_after_snaps() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        write_file_snap(&store, &head, "a.txt", "a\n", None).unwrap();
        write_file_snap(&store, &head, "b.txt", "b\n", None).unwrap();
        write_file_snap(&store, &head, "a.txt", "a2\n", Some("a\n")).unwrap();
        let (n, f) = recompute_counts(&store, &head).unwrap();
        assert_eq!(n, 3);
        assert_eq!(f, 2);
    }

    #[test]
    fn write_file_snap_updates_session_meta_counts() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        write_file_snap(&store, &head, "a.txt", "a\n", None).unwrap();
        let s = store.sessions().get(&head).unwrap();
        assert_eq!(s.snap_count, 1);
        assert_eq!(s.file_count, 1);
        write_file_snap(&store, &head, "a.txt", "a2\n", Some("a\n")).unwrap();
        let s = store.sessions().get(&head).unwrap();
        assert_eq!(s.snap_count, 2);
        assert_eq!(s.file_count, 1);
        write_file_snap(&store, &head, "b.txt", "b\n", None).unwrap();
        let s = store.sessions().get(&head).unwrap();
        assert_eq!(s.snap_count, 3);
        assert_eq!(s.file_count, 2);
    }

    #[test]
    fn open_or_init_initializes_when_missing() {
        let dir = tempdir().unwrap();
        assert!(!dir.path().join(".grs").exists());
        let store = RepoStore::open_or_init(dir.path()).unwrap();
        assert!(dir.path().join(".grs").is_dir());
        assert!(store.head().unwrap().is_some());
    }

    #[test]
    fn open_or_init_opens_existing() {
        let dir = tempdir().unwrap();
        let first = RepoStore::init(dir.path()).unwrap();
        let second = RepoStore::open_or_init(dir.path()).unwrap();
        assert_eq!(first.root(), second.root());
    }
}
