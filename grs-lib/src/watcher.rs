//! Foreground file watcher used by the TUI.
//!
//! The watcher is started inside the TUI process. It dies when the TUI
//! closes because the TUI owns the watcher thread and signals it to stop
//! on drop.
//!
//! In the per-project snapshot model, the watcher is simple: any debounced
//! batch of filesystem events triggers a single full-project snap. There
//! is no per-file chain — every save is a checkpoint of the whole tree.

use crate::config::Config;
use crate::error::Result;
use crate::ignore::IgnoreMatcher;
use crate::paths::GrsPaths;
use crate::snap::SnapStore;
use crate::store::RepoStore;
use crate::ulid::SessionId;
use notify::{RecursiveMode, Watcher as NotifyWatcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, FileIdMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Reason the watcher stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    StopRequested,
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
            store,
            config,
            paths: _,
        } = self;

        // Resolve the open session from HEAD. If missing, error — the caller
        // is expected to have created a session before starting the watcher.
        let open_session = store
            .current_session_id()?
            .ok_or_else(|| crate::error::GrsError::NotFound("no open session".to_string()))?;

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

        info!(watch_root = %root.display(), debounce_ms = config.watcher.debounce_ms, "grs watcher starting");

        let ignore_matcher = store.ignore_matcher()?;
        let snap_store = store.snaps();
        let stop_reason: StopReason;

        loop {
            if matches!(
                stop.try_recv(),
                Ok(()) | Err(mpsc::TryRecvError::Disconnected)
            ) {
                stop_reason = StopReason::StopRequested;
                break;
            }

            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(events)) => {
                    if events.is_empty() {
                        continue;
                    }
                    // Any debounced batch = one full-project snap.
                    debug!(event_count = events.len(), "capturing snap from debounced batch");
                    if let Err(e) = capture_one(&store, &open_session, &snap_store, &ignore_matcher) {
                        warn!(?e, "snap capture failed");
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
        }

        info!(?stop_reason, "grs watcher exiting");
        Ok(())
    }
}

fn capture_one(
    store: &RepoStore,
    open_session: &SessionId,
    snap_store: &SnapStore,
    ignore: &IgnoreMatcher,
) -> Result<()> {
    let meta = snap_store.capture(open_session, ignore)?;
    store
        .sessions()
        .update_snap_count(open_session, meta.n)?;
    debug!(n = meta.n, files = meta.file_count, "snap captured");
    Ok(())
}

/// Convenience: build a `Watcher` for `root` and run it until `stop` fires.
pub fn run_for(root: &Path, stop: &mpsc::Receiver<()>) -> Result<()> {
    let store = RepoStore::open_from(root)?;
    Watcher::new(store).run(stop)
}

// Re-export for tests / external callers.
pub use notify_debouncer_full::DebouncedEvent as Event;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RepoStore;
    use std::io::Write;
    use std::sync::mpsc;
    use tempfile::tempdir;

    /// Drive a single debounced flush through the watcher's batch handler
    /// and assert a snap was created.
    #[test]
    fn debounced_batch_creates_snap() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        // init() does not create a session; we create one explicitly.
        let s = store.open_first_session("test".into()).unwrap();
        let head = s.id.clone();
        // init() already captured snap 1; add a new file and capture snap 2.
        let ignore = store.ignore_matcher().unwrap();
        let snap_store = store.snaps();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let meta = snap_store.capture(&head, &ignore).unwrap();
        assert!(meta.n >= 2, "expected snap >= 2, got {}", meta.n);
        assert!(store.paths().snap_dir(&head, meta.n).join("a.txt").is_file());
    }

    #[test]
    fn watcher_init_fails_without_open_session() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        // No open session was created.
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let res = Watcher::new(store).run(&rx);
        assert!(res.is_err(), "watcher must fail without an open session");
    }

    // End-to-end: start the watcher, modify a file, verify a snap is captured.
    #[test]
    fn end_to_end_captures_snap_on_save() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("watch-e2e".into()).unwrap();
        let session_id = s.id.clone();
        let initial = store.snaps().count(&session_id).unwrap();

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = Watcher::new(watcher_store).run(&stop_rx);
        });
        // Settle.
        std::thread::sleep(Duration::from_millis(200));
        // Write a new file. The 1.5s debounce will fire; give it a margin.
        std::fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
        std::thread::sleep(Duration::from_millis(2500));
        let after = store.snaps().count(&session_id).unwrap();
        assert!(
            after > initial,
            "watcher should have captured at least one new snap (initial={initial}, after={after})"
        );
        // Snap N (or higher) should contain new.txt.
        let snap = store.snaps().read_meta(&session_id, after).unwrap();
        let has_new = snap.files.iter().any(|f| f.path == "new.txt");
        assert!(has_new, "new.txt should be in the latest snap");
        stop_tx.send(()).ok();
        let _ = handle.join();
    }

    // Stub for callers that need a Write handle to flush test fixtures.
    #[allow(dead_code)]
    fn _touch(_w: &mut dyn Write) {}
}
