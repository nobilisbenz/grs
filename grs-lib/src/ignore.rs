//! `.grsignore` matching, wrapping the `ignore` (ripgrep) crate.
//!
//! `IgnoreMatcher` answers "is this path ignored?" by chaining `.grsignore`
//! files like git does (parent dirs first, deeper overrides). It also handles
//! the default ignores (`.git/`, `target/`, `.grs/`, etc.) and the config's
//! `watcher.ignore_extra` patterns.

use crate::error::Result;
use ignore::{Match, WalkBuilder};
use std::path::{Path, PathBuf};

/// The default `.grsignore` content written when the repo is first initialized
/// (happens implicitly the first time `grs` is run in a directory).
pub const DEFAULT_GRSIGNORE: &str = "# grs defaults\n\
.git/\n\
node_modules/\n\
target/\n\
dist/\n\
build/\n\
.grs/\n\
.grsignore\n\
# editor transient files — would otherwise create a snap per save\n\
*~\n\
.*~\n\
*.swp\n\
*.swo\n\
*.swn\n\
*.bak\n\
*.tmp\n\
*.orig\n\
*.rej\n\
.#*\n\
.DS_Store\n\
*.lock\n\
*.lockb\n";

pub struct IgnoreMatcher {
    root: PathBuf,
    extra_patterns: Vec<String>,
}

impl IgnoreMatcher {
    pub fn new(root: &Path, extra_patterns: &[String]) -> Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
            extra_patterns: extra_patterns.to_vec(),
        })
    }

    /// The repo root this matcher is rooted at.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Is `path` (absolute or repo-relative) ignored?
    pub fn is_ignored(&self, path: &Path) -> bool {
        let rel = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == ".grs" || rel.starts_with(".grs/") {
            return true;
        }
        let matcher = match self.build_matcher(path) {
            Some(m) => m,
            None => return false,
        };
        let rel_path = std::path::Path::new(&rel);
        let comps: Vec<_> = rel_path.components().collect();
        let mut acc = PathBuf::new();
        for (i, comp) in comps.iter().enumerate() {
            acc.push(comp);
            let is_last = i + 1 == comps.len();
            let is_dir = !is_last;
            if matches!(matcher.matched(&acc, is_dir), Match::Ignore(_)) {
                return true;
            }
        }
        false
    }

    /// Build a `Gitignore` matcher for `path`: built-in defaults + chained
    /// `.grsignore` files from root to the path's parent + config extras.
    fn build_matcher(&self, path: &Path) -> Option<ignore::gitignore::Gitignore> {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(&self.root);
        for line in DEFAULT_GRSIGNORE.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                let _ = builder.add_line(None, line);
            }
        }
        if let Some(parent) = path.parent() {
            for ancestor in ancestors_from(&self.root, parent) {
                let gi = ancestor.join(".grsignore");
                if gi.is_file() {
                    if let Ok(text) = std::fs::read_to_string(&gi) {
                        for line in text.lines() {
                            let line = line.trim();
                            if !line.is_empty() && !line.starts_with('#') {
                                let _ = builder.add_line(Some(ancestor.clone()), line);
                            }
                        }
                    }
                }
            }
        }
        for pat in &self.extra_patterns {
            let _ = builder.add_line(None, pat);
        }
        builder.build().ok()
    }

    /// Iterate over all non-ignored files in the tree. Yields absolute paths.
    /// Prunes the common heavy ignored dirs by name so we don't descend into
    /// `target/` etc., then post-filters each file with `is_ignored`.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .standard_filters(false)
            .add_custom_ignore_filename(".grsignore");

        let root = self.root.clone();
        builder.filter_entry(move |entry| {
            if entry.path() == root.as_path() {
                return true;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    if matches!(
                        name,
                        "target" | "node_modules" | "dist" | "build" | ".git" | ".grs"
                    ) {
                        return false;
                    }
                }
            }
            true
        });

        let mut out = Vec::new();
        for entry in builder.build().flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let p = entry.into_path();
                if !self.is_ignored(&p) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// Walk `from` up to (but not past) `stop`, yielding ancestor dirs inclusive
/// of `from`, root first.
fn ancestors_from(stop: &Path, from: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = from.to_path_buf();
    while cur.starts_with(stop) {
        out.push(cur.clone());
        match cur.parent() {
            Some(p) if p == stop || (stop.parent().is_some() && p == stop.parent().unwrap()) => {
                cur = p.to_path_buf()
            }
            Some(p) if p.starts_with(stop) => cur = p.to_path_buf(),
            _ => break,
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn grs_dir_always_ignored() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".grs")).unwrap();
        let m = IgnoreMatcher::new(dir.path(), &[]).unwrap();
        assert_eq!(m.root(), dir.path());
        assert!(m.is_ignored(&dir.path().join(".grs/config.toml")));
        assert!(m.is_ignored(&dir.path().join(".grs")));
    }

    #[test]
    fn default_ignores_target() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/a.o"), "x").unwrap();
        let m = IgnoreMatcher::new(dir.path(), &[]).unwrap();
        assert!(m.is_ignored(&dir.path().join("target/a.o")));
    }

    #[test]
    fn grsignore_is_respected() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".grsignore"), "*.log\n").unwrap();
        std::fs::write(dir.path().join("a.log"), "x").unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let m = IgnoreMatcher::new(dir.path(), &[]).unwrap();
        assert!(m.is_ignored(&dir.path().join("a.log")));
        assert!(!m.is_ignored(&dir.path().join("a.txt")));
    }

    #[test]
    fn files_excludes_ignored() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".grsignore"), "*.log\n").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "x").unwrap();
        std::fs::write(dir.path().join("drop.log"), "x").unwrap();
        let m = IgnoreMatcher::new(dir.path(), &[]).unwrap();
        let names: Vec<String> = m
            .files()
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"keep.txt".to_string()));
        assert!(!names.contains(&"drop.log".to_string()));
    }

    /// Editor transient files (vim `*~`, swap files, emacs lock files,
    /// etc.) must be ignored by default. Otherwise a single editor save
    /// creates a snap for each transient file the editor creates and
    /// removes mid-save.
    #[test]
    fn editor_transients_are_ignored_by_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "x").unwrap();
        for transient in [
            "foo~", "foo.swp", "foo.swo", ".foo.swp", ".foo.swo",
            "foo.bak", "foo.tmp", "foo.orig", "foo.rej",
            ".#foo", "foo.lock", "foo.lockb", ".DS_Store",
        ] {
            std::fs::write(dir.path().join(transient), "junk").unwrap();
        }
        let m = IgnoreMatcher::new(dir.path(), &[]).unwrap();
        let names: Vec<String> = m
            .files()
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"keep.txt".to_string()));
        for transient in [
            "foo~", "foo.swp", "foo.swo", ".foo.swp", ".foo.swo",
            "foo.bak", "foo.tmp", "foo.orig", "foo.rej",
            ".#foo", "foo.lock", "foo.lockb", ".DS_Store",
        ] {
            assert!(
                !names.contains(&transient.to_string()),
                "transient {transient:?} should be ignored, but it leaked into the file list: {names:?}"
            );
        }
    }
}
