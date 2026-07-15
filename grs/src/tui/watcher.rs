//! Watcher handle: spawns the engine watcher in a background thread and
//! forwards snap events back to the TUI.

use crate::tui::WatchEvent;
use grs_lib::store::RepoStore;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tracing::warn;

pub struct WatcherHandle {
    stop_tx: mpsc::Sender<()>,
    _thread: Option<thread::JoinHandle<()>>,
}

impl WatcherHandle {
    pub fn spawn(store: RepoStore, watch_tx: mpsc::Sender<WatchEvent>) -> Self {
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("grs-watcher".into())
            .spawn(move || {
                if let Err(e) = run_watcher(store, watch_tx, stop_rx) {
                    warn!(?e, "watcher exited with error");
                }
            })
            .expect("failed to spawn watcher thread");
        Self {
            stop_tx,
            _thread: Some(thread),
        }
    }

    pub fn stop(self) {
        let _ = self.stop_tx.send(());
        // Drop the handle, which joins the thread.
    }
}

fn run_watcher(
    store: RepoStore,
    watch_tx: mpsc::Sender<WatchEvent>,
    stop_rx: mpsc::Receiver<()>,
) -> grs_lib::error::Result<()> {
    use grs_lib::watcher::Watcher;

    // Run the engine watcher in a side thread; it consumes `stop_rx` by
    // value. We poll the snap store in this thread to detect new snaps and
    // forward them to the TUI.
    let store_for_watcher = store.clone();
    let watcher_handle = thread::spawn(move || {
        let _ = Watcher::new(store_for_watcher).run(&stop_rx);
    });

    let session_id = match store.current_session_id()? {
        Some(id) => id,
        None => return Ok(()),
    };
    let mut last_n: u32 = store.snaps().count(&session_id).unwrap_or(0);

    // Poll for snap changes. The watcher thread is the one that actually
    // captures snaps; we just observe the snap count and emit events.
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(n) = store.snaps().count(&session_id) {
            if n != last_n {
                last_n = n;
                if watch_tx.send(WatchEvent::SnapCaptured { n }).is_err() {
                    break; // TUI is gone
                }
            }
        }
        // Exit if the watcher thread has finished.
        if watcher_handle.is_finished() {
            break;
        }
    }

    let _ = watcher_handle.join();
    Ok(())
}
