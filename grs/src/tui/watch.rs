//! TUI foreground watcher glue.
//!
//! `WatcherGuard::start` starts the file watcher in a background thread
//! bound to the TUI's lifetime: when the guard is dropped, the watcher
//! is signaled to stop and joined. Capture happens whenever the TUI is
//! running in a terminal — there is no separate daemon process and no
//! opt-out gate. If the TUI is not running, no capture happens.

use grs_lib::store::RepoStore;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

pub struct WatcherGuard {
    stop: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl WatcherGuard {
    /// Start the watcher for `store` in a background thread. The guard
    /// stops and joins the thread on drop.
    pub fn start(store: RepoStore) -> Self {
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
