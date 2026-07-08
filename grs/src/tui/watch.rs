//! TUI foreground watcher glue.
//!
//! `spawn` starts the file watcher in a background thread bound to the
//! TUI's lifetime: when the TUI exits, the watcher is signaled to stop
//! and joined. Capture happens whenever the TUI is running in a terminal
//! — there is no separate daemon process and no opt-out gate. If the
//! TUI is not running, no capture happens.

use grs_lib::store::RepoStore;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

pub struct WatcherGuard {
    stop: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Start the watcher for `store` in a background thread. The returned
/// `WatcherGuard` stops and joins the thread on drop.
pub fn spawn(store: RepoStore) -> WatcherGuard {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let watcher = grs_lib::watcher::Watcher::new(store);
        if let Err(e) = watcher.run(&stop_rx) {
            tracing::warn!(?e, "watcher exited with error");
        }
    });
    WatcherGuard {
        stop: Some(stop_tx),
        handle: Some(handle),
    }
}
