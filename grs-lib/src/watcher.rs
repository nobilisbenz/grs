//! Foreground file watcher used by the TUI.
//!
//! The watcher is started inside the `grs` / `grs replay` process. It dies
//! automatically when the TUI closes because the TUI owns the watcher thread
//! and signals it to stop on drop.

use crate::config::Config;
use crate::error::Result;
use crate::paths::GrsPaths;
use crate::store::RepoStore;
use crate::ulid::SessionId;
use crate::util::time::now_ms;
use notify::{RecursiveMode, Watcher as NotifyWatcher};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, FileIdMap,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Reason the watcher stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The TUI sent a stop signal.
    StopRequested,
    /// Watcher channel disconnected.
    Disconnected,
}

pub struct Watcher {
    pub root: PathBuf,
    pub store: RepoStore,
    pub config: Config,
    pub paths: GrsPaths,
}

impl Watcher {
    pub fn new(store: RepoStore) -> Self {
        let root = store.root().to_path_buf();
        let paths = store.paths().clone();
        let config = store.config().clone();
        Self {
            root,
            store,
            config,
            paths,
        }
    }

    /// Run the watcher event loop until `stop` fires or the notify channel
    /// disconnects. Blocks.
    pub fn run(self, stop: &mpsc::Receiver<()>) -> Result<()> {
        self.run_inner(stop)
    }

    fn run_inner(self, stop: &mpsc::Receiver<()>) -> Result<()> {
        let Watcher {
            root,
            mut store,
            config,
            paths: _,
        } = self;

        // Resolve the open session from HEAD. If missing, create one.
        let mut open_session = match store.head()? {
            Some(id) => id,
            None => {
                let s = store.sessions().create_new(now_ms())?;
                store.set_head(&s.id)?;
                s.id
            }
        };

        // Build the initial state: last_content + per-file last_seq by
        // replaying the current session's snaps.
        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        for entry in store.snaps().list(&open_session)? {
            match crate::snap::SnapStore::read_path(&entry.path) {
                Ok(snap) => {
                    file_last_seq.insert(snap.file_path.clone(), snap.seq);
                    last_content.insert(snap.file_path, snap.content);
                }
                Err(e) => warn!(?e, "failed to read snap during init"),
            }
        }

        // Set up the notify watcher (debounced).
        let (tx, rx) = mpsc::channel::<DebounceEventResult>();
        let debounce = Duration::from_millis(config.watcher.debounce_ms);
        let mut debouncer: Debouncer<notify::RecommendedWatcher, FileIdMap> =
            new_debouncer(debounce, None, tx).map_err(|e| {
                crate::error::GrsError::Ignore(format!("watcher init failed: {e}"))
            })?;
        debouncer
            .watcher()
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| crate::error::GrsError::Ignore(format!("watch failed: {e}")))?;
        let _ = debouncer.cache();

        info!(watch_root = %root.display(), "grs watcher starting");

        // Main event loop.
        let mut last_head_check: u64 = 0;
        let mut last_ignore_check: u64 = 0;
        let mut ignore_matcher = store.ignore_matcher()?;
        let stop_reason: StopReason;

        loop {
            if matches!(
                stop.try_recv(),
                Ok(()) | Err(mpsc::TryRecvError::Disconnected)
            ) {
                stop_reason = StopReason::StopRequested;
                break;
            }

            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(events)) => {
                    for ev in events {
                        for path in &ev.paths {
                            if let Err(e) = handle_path_event(
                                path,
                                &root,
                                &ignore_matcher,
                                &open_session,
                                &mut last_content,
                                &mut file_last_seq,
                                &mut store,
                            ) {
                                warn!(?e, path = %path.display(), "event handler error");
                            }
                        }
                    }
                }
                Ok(Err(errs)) => {
                    for e in errs {
                        warn!(?e, "watcher error");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stop_reason = StopReason::Disconnected;
                    break;
                }
            }

            let tick_ms = now_ms() as u64;
            if tick_ms - last_head_check > 250 {
                last_head_check = tick_ms;
                if let Some(new_head) = store.head()? {
                    if new_head != open_session {
                        info!(from = %open_session, to = %new_head, "HEAD changed; switching session");
                        open_session = new_head;
                        last_content.clear();
                        file_last_seq.clear();
                        for entry in store.snaps().list(&open_session).unwrap_or_default() {
                            if let Ok(snap) = crate::snap::SnapStore::read_path(&entry.path) {
                                file_last_seq.insert(snap.file_path.clone(), snap.seq);
                                last_content.insert(snap.file_path, snap.content);
                            }
                        }
                    }
                }
            }

            if tick_ms - last_ignore_check > 1000 {
                last_ignore_check = tick_ms;
                let _ = reload_ignore(&root, &config, &mut ignore_matcher);
            }
        }

        info!(?stop_reason, "grs watcher exiting");
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_path_event(
    path: &Path,
    root: &Path,
    ignore: &crate::ignore::IgnoreMatcher,
    open_session: &SessionId,
    last_content: &mut HashMap<String, String>,
    file_last_seq: &mut HashMap<String, u32>,
    store: &mut RepoStore,
) -> Result<()> {
    if ignore.is_ignored(path) {
        return Ok(());
    }
    let rel = crate::paths::relativize(root, path);
    if rel.is_empty() || rel == ".grs" || rel.starts_with(".grs/") {
        return Ok(());
    }
    if path.is_dir() {
        return Ok(());
    }

    if !path.exists() {
        // File deleted — record a tombstone snap only if we used to track it.
        // For files we had been ignoring as binary, `last_content` was never
        // populated, so this is a no-op for them.
        let prev = last_content.remove(&rel);
        let prev_seq = file_last_seq.remove(&rel);
        if let Some(prev_content) = prev {
            let diff = crate::diff::line_diff(&prev_content, "");
            let mut diff = diff;
            diff.prev_seq = prev_seq;
            let seq = next_seq(store, open_session);
            let mut snap = crate::snap::SnapStore::build_snap(
                seq,
                rel.clone(),
                String::new(),
                diff,
                prev_seq,
            );
            snap.timestamp = now_ms();
            snap.timestamp_iso = crate::util::time::iso(snap.timestamp);
            store.snaps().write(open_session, snap)?;
            crate::store::update_session_counts(store, open_session)?;
        }
        return Ok(());
    }

    // Skip binary content entirely — no snap, no diff. A built `cargo`
    // artifact or compiled `.pyc` would otherwise spam the session with
    // `(binary file, N bytes)` placeholders on every recompile. Also drop
    // any stale state for the path so a transient text→binary→text flip
    // (rare but possible) starts clean.
    if crate::util::fs::is_binary_file(path) {
        last_content.remove(&rel);
        file_last_seq.remove(&rel);
        debug!(file = %rel, "skipping binary file");
        return Ok(());
    }

    let new_content = crate::util::fs::read_content_or_binary_placeholder(path)?;
    let prev_content = last_content.get(&rel).cloned();

    // Dedup: editors sometimes fire Modify-after-Create for the same write.
    // If the content hasn't actually changed, skip the snap.
    if let Some(prev) = &prev_content {
        if prev == &new_content {
            return Ok(());
        }
    }

    let prev_seq = file_last_seq.get(&rel).copied();
    let diff = crate::diff::line_diff(prev_content.as_deref().unwrap_or(""), &new_content);
    let mut diff = diff;
    diff.prev_seq = prev_seq;
    let seq = next_seq(store, open_session);
    let mut snap = crate::snap::SnapStore::build_snap(
        seq,
        rel.clone(),
        new_content.clone(),
        diff,
        prev_seq,
    );
    snap.timestamp = now_ms();
    snap.timestamp_iso = crate::util::time::iso(snap.timestamp);
    store.snaps().write(open_session, snap)?;
    crate::store::update_session_counts(store, open_session)?;

    last_content.insert(rel.clone(), new_content);
    file_last_seq.insert(rel, seq);
    debug!(seq, "snap written");
    Ok(())
}

/// Allocate the next globally-unique seq for `session`. `prev_for_file`
/// stays in the per-file `prev_seq` field on the snap (so the diff chain
/// remains correct), but the snap's own `seq` must be a session-wide
/// monotonic counter — otherwise two different files can both land on
/// seq `n`, and a "play by seq" sort interleaves them in filesystem
/// order rather than the real time order the user expects.
fn next_seq(store: &RepoStore, session: &SessionId) -> u32 {
    store.snaps().next_seq(session).unwrap_or(0)
}

fn reload_ignore(
    root: &Path,
    config: &Config,
    matcher: &mut crate::ignore::IgnoreMatcher,
) -> Result<()> {
    *matcher = crate::ignore::IgnoreMatcher::new(root, &config.watcher.ignore_extra)?;
    Ok(())
}

/// Convenience: build a `Watcher` for `root` and run it until `stop` fires.
pub fn run_for(root: &Path, stop: &mpsc::Receiver<()>) -> Result<()> {
    let store = RepoStore::open_from(root)?;
    Watcher::new(store).run(stop)
}

// Re-export for tests / external callers.
pub use notify_debouncer_full::DebouncedEvent as Event;

#[allow(dead_code)]
fn _unused(_: &DebouncedEvent) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RepoStore;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn handle_path_event_writes_snap_then_updates_state() {
        let dir = tempdir().unwrap();
        let mut store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "hello\n").unwrap();

        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        let ignore = store.ignore_matcher().unwrap();

        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();

        assert_eq!(file_last_seq.get("a.txt").copied(), Some(0));
        assert_eq!(last_content.get("a.txt").map(|s| s.as_str()), Some("hello\n"));
        let snap = store.snaps().read(&head, 0).unwrap();
        assert_eq!(snap.content, "hello\n");
        assert_eq!(snap.prev_seq, None);
        assert_eq!(snap.diff.added_lines, vec![1]);

        // Modify: should produce snap 1 with prev_seq=0 and the right diff.
        std::fs::write(&target, "hello world\n").unwrap();
        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();
        assert_eq!(file_last_seq.get("a.txt").copied(), Some(1));
        let snap = store.snaps().read(&head, 1).unwrap();
        assert_eq!(snap.prev_seq, Some(0));
        assert_eq!(snap.content, "hello world\n");
    }

    #[test]
    fn handle_path_event_skips_ignored() {
        let dir = tempdir().unwrap();
        let mut store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let target = dir.path().join("a.log");
        std::fs::write(&target, "noise\n").unwrap();
        std::fs::write(dir.path().join(".grsignore"), "*.log\n").unwrap();

        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        let ignore = store.ignore_matcher().unwrap();

        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();
        assert!(file_last_seq.is_empty());
        assert_eq!(store.snaps().list(&head).unwrap().len(), 0);
    }

    #[test]
    fn handle_path_event_skips_binary() {
        let dir = tempdir().unwrap();
        let mut store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        // Pretend compiled binary: header + lots of NULs.
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
        bytes.extend(std::iter::repeat(0u8).take(1024));
        let target = dir.path().join("artifact.bin");
        std::fs::write(&target, &bytes).unwrap();

        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        let ignore = store.ignore_matcher().unwrap();

        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();
        assert!(file_last_seq.is_empty(), "binary must not be tracked");
        assert!(last_content.is_empty());
        assert_eq!(store.snaps().list(&head).unwrap().len(), 0);
    }

    #[test]
    fn handle_path_event_drops_stale_state_for_now_binary_file() {
        let dir = tempdir().unwrap();
        let mut store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let target = dir.path().join("a.txt");
        // First we write text and snap it.
        std::fs::write(&target, "hello\n").unwrap();

        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        let ignore = store.ignore_matcher().unwrap();

        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();
        assert_eq!(file_last_seq.get("a.txt").copied(), Some(0));
        assert_eq!(last_content.get("a.txt").map(|s| s.as_str()), Some("hello\n"));

        // Now the file becomes binary (e.g. some build step overwrites it).
        let mut bytes = vec![0u8; 256];
        bytes[0] = 0x7f;
        std::fs::write(&target, &bytes).unwrap();
        handle_path_event(
            &target,
            dir.path(),
            &ignore,
            &head,
            &mut last_content,
            &mut file_last_seq,
            &mut store,
        )
        .unwrap();
        // Stale state must be cleared; no second snap should be written.
        assert!(!file_last_seq.contains_key("a.txt"));
        assert!(!last_content.contains_key("a.txt"));
        assert_eq!(store.snaps().list(&head).unwrap().len(), 1);
    }

    #[test]
    fn handle_path_event_uses_global_seq_across_files() {
        // Reproduces the bug the timelapse "file-specific" order was caused
        // by: a -> b -> a produced seqs 0, 1, 1 in the old per-file scheme
        // (collision on the second `a`). The new global scheme must hand
        // out 0, 1, 2 so a `sort_by_key(|e| e.seq)` truly gives time order.
        let dir = tempdir().unwrap();
        let mut store = RepoStore::init(dir.path()).unwrap();
        let head = store.head().unwrap().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");

        let mut last_content: HashMap<String, String> = HashMap::new();
        let mut file_last_seq: HashMap<String, u32> = HashMap::new();
        let ignore = store.ignore_matcher().unwrap();

        // Each edit changes the file's content so the watcher's
        // content-dedup doesn't suppress it. Order matches a real session
        // where the user bounces between two files.
        let edits: &[(&std::path::Path, &str)] = &[
            (&a, "a1\n"),
            (&b, "b1\n"),
            (&a, "a2\n"),
        ];
        for (path, content) in edits {
            std::fs::write(path, content).unwrap();
            handle_path_event(
                path,
                dir.path(),
                &ignore,
                &head,
                &mut last_content,
                &mut file_last_seq,
                &mut store,
            )
            .unwrap();
        }
        let seqs: Vec<u32> = store
            .snaps()
            .list(&head)
            .unwrap()
            .into_iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![0, 1, 2], "seqs must be globally unique");
        let files: Vec<String> = store
            .snaps()
            .list(&head)
            .unwrap()
            .into_iter()
            .map(|e| {
                crate::snap::SnapStore::read_path(&e.path)
                    .unwrap()
                    .file_path
            })
            .collect();
        assert_eq!(files, vec!["a.txt", "b.txt", "a.txt"]);
    }
}
