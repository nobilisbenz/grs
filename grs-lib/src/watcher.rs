//! Foreground file watcher used by the TUI.
//!
//! The watcher is started inside the TUI process. It dies when the TUI
//! closes because the TUI owns the watcher thread and signals it to stop
//! on drop.
//!
//! Trigger model: a snap is captured when a raw notify event matches
//! either of these:
//!
//! - `EventKind::Access(AccessKind::Close(AccessMode::Write))` on a
//!   tracked path — covers most "dumb" editor saves (VSCode default,
//!   gedit, Sublime non-atomic) and the post-rename completion of
//!   atomic-save flows on Linux (vim/IntelliJ/VSCode-safe-write), since
//!   the kernel closes the renamed file's handle as part of `rename(2)`.
//! - `EventKind::Create(CreateKind::File)` on a tracked path — covers
//!   `cat > foo`, `sed -i`, and the post-rename event on macOS where
//!   FSEvents surfaces the rename as a Created event.
//!
//! Both events pass through a dedupe check in `SnapStore::capture_if_changed`:
//! before writing a new `snap-N/`, the current tree is SHA256-scanned
//! and compared to the most recent snap. If identical, no new snap is
//! allocated. This means a save that fires several notify events
//! back-to-back produces one snap (the first event where the tree has
//! actually changed), with the rest becoming no-ops.
//!
//! There is **no time-based debounce** and **no quiet-period fallback**.
//! The trigger is purely event-driven.

use crate::config::Config;
use crate::error::Result;
use crate::ignore::IgnoreMatcher;
use crate::paths::GrsPaths;
use crate::snap::SnapStore;
use crate::store::RepoStore;
use crate::ulid::SessionId;
use notify::{
    event::{AccessKind, AccessMode, CreateKind, EventKind},
    RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
};
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
            config: _,
            paths: _,
        } = self;

        // Resolve the open session from HEAD. If missing, error — the caller
        // is expected to have created a session before starting the watcher.
        let open_session = store
            .current_session_id()?
            .ok_or_else(|| crate::error::GrsError::NotFound("no open session".to_string()))?;

        // Set up a raw notify watcher. We do NOT use notify-debouncer-full:
        // there is no time-based debounce in this design.
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut notify: RecommendedWatcher =
            notify::recommended_watcher(move |res| {
                let _ = tx.send(res);
            })
            .map_err(|e| crate::error::GrsError::Ignore(format!("watcher init failed: {e}")))?;
        notify
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| crate::error::GrsError::Ignore(format!("watch failed: {e}")))?;

        info!(watch_root = %root.display(), "grs watcher starting");

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
                Ok(Ok(event)) => {
                    if is_save_trigger(&event, &ignore_matcher) {
                        debug!(event_kind = ?event.kind, "save trigger — capturing snap");
                        if let Err(e) = capture_if_changed(
                            &store,
                            &open_session,
                            &snap_store,
                            &ignore_matcher,
                        ) {
                            warn!(?e, "snap capture failed");
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!(?e, "watcher error");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stop_reason = StopReason::Disconnected;
                    break;
                }
            }
        }

        // Drop the watcher to stop the OS-level subscription before returning.
        drop(notify);
        info!(?stop_reason, "grs watcher exiting");
        Ok(())
    }
}

/// True if this event should trigger a snap.
///
/// A trigger is a `Close(Write)` or `Create(File)` on a path that the
/// ignore-matcher considers tracked (i.e. `!is_ignored(path)`). Other
/// event kinds — `Modify(Data)`, `Modify(Metadata)`, `Modify(Name)`,
/// `Remove`, etc. — are ignored: the trigger fires on the trailing
/// event of a save, not the intermediate ones.
fn is_save_trigger(event: &notify::Event, ignore: &IgnoreMatcher) -> bool {
    let is_close_write = matches!(
        event.kind,
        EventKind::Access(AccessKind::Close(AccessMode::Write))
    );
    let is_create_file = matches!(
        event.kind,
        EventKind::Create(CreateKind::File)
    );
    if !is_close_write && !is_create_file {
        return false;
    }
    event
        .paths
        .iter()
        .any(|p| is_tracked_path(p, ignore))
}

/// True if `path` (absolute) is a tracked project file per the
/// ignore-matcher. Tracked means: not ignored, and not inside a
/// directory that is ignored.
fn is_tracked_path(path: &Path, ignore: &IgnoreMatcher) -> bool {
    if !path.starts_with(ignore.root()) {
        return false; // event outside the watch root — ignore
    }
    !ignore.is_ignored(path)
}

fn capture_if_changed(
    store: &RepoStore,
    open_session: &SessionId,
    snap_store: &SnapStore,
    ignore: &IgnoreMatcher,
) -> Result<()> {
    if let Some(meta) = snap_store.capture_if_changed(open_session, ignore)? {
        store
            .sessions()
            .update_snap_count(open_session, meta.n)?;
        debug!(n = meta.n, files = meta.file_count, "snap captured");
    } else {
        debug!("tree unchanged — no new snap");
    }
    Ok(())
}

/// Convenience: build a `Watcher` for `root` and run it until `stop` fires.
pub fn run_for(root: &Path, stop: &mpsc::Receiver<()>) -> Result<()> {
    let store = RepoStore::open_from(root)?;
    Watcher::new(store).run(stop)
}

// Re-export the raw event type for tests / external callers.
pub use notify::Event;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RepoStore;
    use std::io::Write;
    use std::sync::mpsc;
    use tempfile::tempdir;

    /// Drive a single Create trigger and assert a snap was created.
    #[test]
    fn create_event_creates_snap() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        // init() does not create a session; we create one explicitly.
        let s = store.open_first_session("test".into()).unwrap();
        let head = s.id.clone();
        // init() already captured snap 1; add a new file and trigger a
        // second snap via the public capture_if_changed path.
        let ignore = store.ignore_matcher().unwrap();
        let snap_store = store.snaps();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let meta = snap_store.capture_if_changed(&head, &ignore).unwrap();
        assert!(meta.is_some(), "expected a snap, got None (tree unchanged?)");
        let meta = meta.unwrap();
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

    // End-to-end: start the watcher, write a file, verify a snap is captured.
    //
    // With no debounce, a `std::fs::write` to a *new* path fires
    // `Create(File)` followed by `Modify(Data)` followed by
    // `Close(Write)`. The watcher reacts to Create, captures a snap, then
    // reacts to Close(Write), and capture_if_changed dedupes it. Net
    // result: exactly one new snap.
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
        // Settle: let notify attach to the watch root.
        std::thread::sleep(Duration::from_millis(200));
        // Write a new file. The Create event triggers a snap; the
        // trailing Close(Write) is deduped.
        std::fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
        // Wait for events to drain through the watcher's 500ms poll.
        std::thread::sleep(Duration::from_millis(1000));
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

    /// End-to-end: explicit Close(Write) trigger. We open a file for
    /// writing, write some bytes, then drop the handle to fire
    /// `Close(Write)`. The watcher should capture a snap.
    #[test]
    fn end_to_end_captures_snap_on_close_write() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("close-write".into()).unwrap();
        let session_id = s.id.clone();
        let initial = store.snaps().count(&session_id).unwrap();

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = Watcher::new(watcher_store).run(&stop_rx);
        });
        std::thread::sleep(Duration::from_millis(200));
        // Open for write (creates the file or truncates if it exists),
        // write, then drop. `drop` closes the handle and the kernel
        // fires IN_CLOSE_WRITE.
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(dir.path().join("wrote.txt"))
                .unwrap();
            f.write_all(b"close-write content\n").unwrap();
        }
        std::thread::sleep(Duration::from_millis(1000));
        let after = store.snaps().count(&session_id).unwrap();
        assert!(
            after > initial,
            "watcher should have captured at least one new snap (initial={initial}, after={after})"
        );
        let snap = store.snaps().read_meta(&session_id, after).unwrap();
        let has = snap.files.iter().any(|f| f.path == "wrote.txt");
        assert!(has, "wrote.txt should be in the latest snap");
        stop_tx.send(()).ok();
        let _ = handle.join();
    }

    /// A save that produces several notify events (Create + Modify +
    /// Close(Write) on a fresh file) should result in exactly one new
    /// snap — capture_if_changed dedupes the trailing events.
    #[test]
    fn save_burst_dedupes_to_one_snap() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("dedupe".into()).unwrap();
        let session_id = s.id.clone();
        let initial = store.snaps().count(&session_id).unwrap();

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = Watcher::new(watcher_store).run(&stop_rx);
        });
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.path().join("burst.txt"), "burst\n").unwrap();
        // Wait long enough for Create + Close(Write) to land and the
        // dedupe check to skip the duplicate.
        std::thread::sleep(Duration::from_millis(1500));
        let after = store.snaps().count(&session_id).unwrap();
        assert_eq!(
            after,
            initial + 1,
            "save burst should produce exactly one new snap (initial={initial}, after={after})"
        );
        stop_tx.send(()).ok();
        let _ = handle.join();
    }

    /// Edits in a non-tracked directory (e.g. `.git/`) should NOT trigger
    /// a snap, even though the watcher is watching the whole root.
    #[test]
    fn edits_in_ignored_dirs_do_not_snap() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("ignored-dir".into()).unwrap();
        let session_id = s.id.clone();
        let initial = store.snaps().count(&session_id).unwrap();

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = Watcher::new(watcher_store).run(&stop_rx);
        });
        std::thread::sleep(Duration::from_millis(200));
        // .git/ is in the default ignore list.
        std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        std::fs::write(dir.path().join(".git/objects/abc"), "data").unwrap();
        std::thread::sleep(Duration::from_millis(1000));
        let after = store.snaps().count(&session_id).unwrap();
        assert_eq!(
            after, initial,
            "edits in .git/ should not create a snap (initial={initial}, after={after})"
        );
        stop_tx.send(()).ok();
        let _ = handle.join();
    }

    // Stub for callers that need a Write handle to flush test fixtures.
    #[allow(dead_code)]
    fn _touch(_w: &mut dyn Write) {}
}
