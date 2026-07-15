//! `.grs/` path resolution and repo-root discovery.
//!
//! `find_grs_root` walks up from `cwd` looking for a `.grs/` directory, like
//! git's rev-parse. `GrsPaths` gives the well-known paths under a root.
//!
//! Layout (per project):
//! ```text
//! <project>/
//!   .grs/
//!     sessions/
//!       <slug>_<ulid>/          # one session folder
//!         meta.toml              # SessionMeta (TOML)
//!         snap-0001.json         # SnapJson (one file per snap; whole project tree)
//!         snap-0002.json
//!         ...
//!     .lock                      # prevents concurrent TUI on same project
//!   .grsignore                   # ignore patterns
//!   config.toml                  # in .grs/, optional
//! ```

use crate::error::{GrsError, Result};
use crate::ulid::SessionId;
use std::path::{Path, PathBuf};

pub const GRS_DIR: &str = ".grs";
pub const CONFIG_FILE: &str = "config.toml";
pub const SESSIONS_DIR: &str = "sessions";
pub const LOCK_FILE: &str = ".lock";
pub const META_FILE: &str = "meta.toml";

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
    pub lock: PathBuf,
}

impl GrsPaths {
    pub fn new(root: &Path) -> Self {
        let grs_dir = root.join(GRS_DIR);
        Self {
            root: root.to_path_buf(),
            head: grs_dir.join("HEAD"),
            lock: grs_dir.join(LOCK_FILE),
            config: grs_dir.join(CONFIG_FILE),
            sessions_dir: grs_dir.join(SESSIONS_DIR),
            grs_dir,
        }
    }

    /// Folder for one session: `.grs/sessions/<slug>_<ulid>/`.
    pub fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.sessions_dir.join(id.as_str())
    }

    /// `.grs/sessions/<slug>_<ulid>/meta.toml`.
    pub fn session_meta(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join(META_FILE)
    }

    /// `.grs/sessions/<slug>_<ulid>/snap-NNNN.json`.
    pub fn snap_file(&self, id: &SessionId, n: u32) -> PathBuf {
        self.session_dir(id).join(format!("snap-{n:04}.json"))
    }
}

/// Normalize a path to a repo-relative, forward-slash string (independent of
/// the platform separator) — used for `SnapFile.path`.
pub fn relativize(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| abs.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

/// Parse a session folder name back to the `SessionId`. Accepts either:
/// - a bare 26-char ULID (the canonical form)
/// - `<slug>_<26-char-ulid>` for human-readable folder names
pub fn parse_session_folder(folder: &str) -> Option<crate::ulid::SessionId> {
    if folder.len() == 26 {
        return crate::ulid::SessionId::parse(folder).ok();
    }
    if folder.len() > 27 && folder.as_bytes().get(folder.len() - 27) == Some(&b'_') {
        let ulid_part = &folder[folder.len() - 26..];
        return crate::ulid::SessionId::parse(ulid_part).ok();
    }
    None
}

/// Convert a user-given session name to a filesystem-safe slug:
/// lowercase, spaces/specials to `-`, dedupe `-`, trim leading/trailing `-`,
/// cap at 60 chars.
pub fn slugify(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        let lc = ch.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            s.push(lc);
            prev_dash = false;
        } else if !prev_dash {
            s.push('-');
            prev_dash = true;
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "session".to_string()
    } else if s.len() > 60 {
        s[..60].trim_end_matches('-').to_string()
    } else {
        s
    }
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

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("Refactor Auth Flow"), "refactor-auth-flow");
        assert_eq!(slugify("  hello   world  "), "hello-world");
        assert_eq!(slugify("foo!!bar##baz"), "foo-bar-baz");
        assert_eq!(slugify("___"), "session");
        // 60-char cap with trailing dash trimmed.
        let long = "a very long name that goes on and on and on and on and on and on and on";
        let slug = slugify(long);
        assert!(slug.len() <= 60, "slug too long: {} ({} chars)", slug, slug.len());
        assert!(!slug.ends_with('-'), "slug must not end with dash: {slug}");
    }
}
