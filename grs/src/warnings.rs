//! One-time startup warnings. The big one: "you put `.grs/` in a git repo but
//! didn't gitignore it; it's going to balloon your commits."

use std::path::Path;

/// Check the project setup and print a warning to stderr if anything looks
/// off. Idempotent — safe to call every TUI launch; we gate on a per-project
/// marker file so the warning fires at most once per project (the user can
/// re-trigger by deleting the marker).
pub fn check_and_warn(root: &Path) {
    let Some(warning) = check_gitignore(root) else {
        return;
    };
    // The marker file lives inside `.grs/` and is itself gitignored.
    let marker = root.join(".grs").join(".warned-gitignore");
    if marker.exists() {
        return;
    }
    eprintln!("\n\x1b[33mWarning:\x1b[0m {warning}");
    eprintln!("\x1b[33mHint:\x1b[0m add a line to .gitignore:");
    eprintln!("    .grs/");
    eprintln!();
    if let Err(e) = std::fs::write(&marker, "warned-once\n") {
        tracing::warn!(?e, "failed to write gitignore-warning marker");
    }
}

/// Returns `Some(message)` if the project is a git repo whose `.gitignore`
/// doesn't mention `.grs/`.
fn check_gitignore(root: &Path) -> Option<String> {
    // Not a git repo at all → nothing to warn about.
    if !root.join(".git").exists() {
        return None;
    }
    // No .gitignore → can't be excluding `.grs/`.
    let gi = root.join(".gitignore");
    if !gi.is_file() {
        return Some(
            "this is a git repo, but there's no .gitignore — your .grs/ folder will be committed."
                .to_string(),
        );
    }
    let Ok(text) = std::fs::read_to_string(&gi) else {
        return None;
    };
    if has_grs_ignore_entry(&text) {
        return None;
    }
    Some(
        "this is a git repo, but .grs/ is not in .gitignore — your session data will be committed."
            .to_string(),
    )
}

fn has_grs_ignore_entry(gitignore_text: &str) -> bool {
    for line in gitignore_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Strip a trailing `/` and a leading `/` for the match — we just
        // want to know if `.grs` (with or without `/`) appears as a pattern.
        let pattern = trimmed.trim_end_matches('/').trim_start_matches('/');
        if pattern == ".grs" || pattern == "**/.grs" || pattern.ends_with("/.grs") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn no_warning_for_non_git_repo() {
        let dir = tempdir().unwrap();
        assert!(check_gitignore(dir.path()).is_none());
    }

    #[test]
    fn warns_when_git_repo_has_no_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert!(check_gitignore(dir.path()).is_some());
    }

    #[test]
    fn warns_when_gitignore_omits_grs() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        assert!(check_gitignore(dir.path()).is_some());
    }

    #[test]
    fn silent_when_gitignore_excludes_grs() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".grs/\n").unwrap();
        assert!(check_gitignore(dir.path()).is_none());
    }

    #[test]
    fn matches_grs_without_trailing_slash() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".grs\n").unwrap();
        assert!(check_gitignore(dir.path()).is_none());
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "# this is a comment\n\n  # indented comment\n.grs/\n",
        )
        .unwrap();
        assert!(check_gitignore(dir.path()).is_none());
    }

    #[test]
    fn marker_suppresses_repeat_warnings() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".grs")).unwrap();
        check_and_warn(dir.path());
        // The marker should exist now.
        assert!(dir.path().join(".grs/.warned-gitignore").exists());
        // Calling again should be a no-op.
        check_and_warn(dir.path());
    }
}
