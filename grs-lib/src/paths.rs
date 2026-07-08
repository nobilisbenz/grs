//! `.grs/` path resolution and repo-root discovery.
//!
//! `find_grs_root` walks up from `cwd` looking for a `.grs/` directory, like
//! git's rev-parse. `grs_paths` gives the well-known paths under a root.

use crate::error::{GrsError, Result};
use std::path::{Path, PathBuf};

pub const GRS_DIR: &str = ".grs";
pub const HEAD_FILE: &str = "HEAD";
pub const CONFIG_FILE: &str = "config.toml";
pub const SESSIONS_DIR: &str = "sessions";
pub const SNAPS_DIR: &str = "snaps";
pub const META_JSON: &str = "meta.json";
pub const LOCK_FILE: &str = ".lock";

/// Walk up from `start` looking for the first ancestor containing a `.grs/`
/// directory. Returns the project root (the directory containing `.grs/`).
pub fn find_grs_root(start: &Path) -> Result<PathBuf> {
    let mut current = canonicalize_or(start);
    loop {
        if current.join(GRS_DIR).is_dir() {
            return Ok(current);
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    Err(GrsError::NotInitialized)
}

/// Like `find_grs_root` but returns `Ok(None)` when no repo is found (used by
/// `grs status` to report gracefully outside a repo).
pub fn try_find_grs_root(start: &Path) -> Option<PathBuf> {
    find_grs_root(start).ok()
}

/// Canonicalize a path, falling back to the uncanonicalized form if the path
/// does not yet exist (e.g. on first run in a fresh dir).
fn canonicalize_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// All the well-known paths under a repo root.
#[derive(Clone, Debug)]
pub struct GrsPaths {
    pub root: PathBuf,
    pub grs_dir: PathBuf,
    pub head: PathBuf,
    pub config: PathBuf,
    pub sessions_dir: PathBuf,
}

impl GrsPaths {
    pub fn new(root: &Path) -> Self {
        let grs_dir = root.join(GRS_DIR);
        Self {
            root: root.to_path_buf(),
            head: grs_dir.join(HEAD_FILE),
            config: grs_dir.join(CONFIG_FILE),
            sessions_dir: grs_dir.join(SESSIONS_DIR),
            grs_dir,
        }
    }

    pub fn session_dir(&self, id: &crate::ulid::SessionId) -> PathBuf {
        self.sessions_dir.join(id.as_str())
    }

    pub fn session_meta(&self, id: &crate::ulid::SessionId) -> PathBuf {
        self.session_dir(id).join(META_JSON)
    }

    pub fn session_snaps(&self, id: &crate::ulid::SessionId) -> PathBuf {
        self.session_dir(id).join(SNAP_DIR_NAME)
    }

    pub fn session_lock(&self, id: &crate::ulid::SessionId) -> PathBuf {
        self.session_dir(id).join(LOCK_FILE)
    }
}

pub const SNAP_DIR_NAME: &str = "snaps";

/// Normalize a path to a repo-relative, forward-slash string (independent of
/// the platform separator) — used for `Snap.file_path`.
pub fn relativize(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| abs.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn finds_root_walking_up() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".grs")).unwrap();
        let sub = root.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(find_grs_root(&sub).unwrap(), *root);
    }

    #[test]
    fn errors_when_not_initialized() {
        let dir = tempdir().unwrap();
        assert!(find_grs_root(dir.path()).is_err());
    }

    #[test]
    fn relativize_forward_slashes() {
        let root = Path::new("/home/x/proj");
        assert_eq!(relativize(root, Path::new("/home/x/proj/src/a.go")), "src/a.go");
    }
}
