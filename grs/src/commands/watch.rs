//! `grs watch` — run the file watcher headless, no TUI, no terminal.
//!
//! Intended for non-interactive callers (the pi extension, scripts, CI
//! loops) that want a long-lived capture process for a single project.
//! The command:
//!
//! 1. Resolves the project root (`--root` > `--repo` > cwd).
//! 2. Opens (or initialises) the repo at that root.
//! 3. Ensures an open session exists; creates one with `--session-name`
//!    (or a name derived from the project) if not.
//! 4. Acquires the project lock so no TUI runs on the same project
//!    concurrently.
//! 5. Runs the in-process `Watcher` event loop on a blocking task, with
//!    a tokio signal listener that sends stop on SIGINT/SIGTERM.
//!
//! On exit (signal or clean), the lock is released by `LockGuard`'s
//! `Drop`. The session stays open across runs — the caller is expected
//! to manage session lifecycle separately (`grs new <name>` rotates).

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::error::GrsError;
use grs_lib::store::RepoStore;
use std::path::PathBuf;
use std::sync::mpsc;
use tracing::{info, warn};

#[derive(clap::Args, Clone, Debug)]
pub struct WatchArgs {
    /// Project root to watch. Defaults to `--repo` or the current directory.
    #[arg(long, value_hint = clap::ValueHint::DirPath)]
    pub root: Option<PathBuf>,

    /// Do not auto-initialize the repo at the resolved root.
    /// Use this when you want `grs watch` to fail loudly instead of
    /// creating `.grs/` on first run.
    #[arg(long)]
    pub no_init: bool,

    /// Name for the auto-created session, if no open session exists.
    /// If omitted, the session is named after the project's directory.
    #[arg(long)]
    pub session_name: Option<String>,
}

pub async fn cmd_watch(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &WatchArgs,
) -> Result<(), CommandError> {
    // 1. Resolve the project root.
    let root = match &args.root {
        Some(p) => p.clone(),
        None => match command.root() {
            Some(r) => r.to_path_buf(),
            None => std::env::current_dir().map_err(CommandError::internal_error)?,
        },
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    info!(root = %root.display(), "grs watch: resolved root");

    // 2. Open (or init) the repo.
    let store = if args.no_init {
        RepoStore::open(&root).map_err(|e| match e {
            GrsError::NotInitialized => CommandError::user_error(format!(
                "no .grs/ at {}",
                root.display()
            ))
            .hinted("drop --no-init to auto-initialize, or run `grs` once first."),
            other => CommandError::from(other),
        })?
    } else {
        RepoStore::open_or_init(&root).map_err(CommandError::from)?
    };

    // 3. Ensure an open session exists.
    if store.current_session().map_err(CommandError::from)?.is_none() {
        let name = match &args.session_name {
            Some(n) => n.clone(),
            None => {
                let dir_name = root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("watch");
                dir_name.to_string()
            }
        };
        let session = store
            .open_first_session(name)
            .map_err(CommandError::from)?;
        info!(session = %session.name, id = %session.id, "grs watch: created session");
    }

    // 4. Acquire the project lock. This is the same lock the TUI
    //    acquires, so a TUI and `grs watch` cannot run on the same
    //    project concurrently.
    let _lock = store.lock().map_err(CommandError::from)?;

    // Print a status line the user can see. The TUI is gone, so this
    // is the only "I am running" signal besides the process.
    let session_id_short: String = store
        .current_session_id()
        .map_err(CommandError::from)?
        .map(|id| id.as_str().chars().take(8).collect())
        .unwrap_or_default();
    ui.status(&format!(
        "watching {} (session {}) — press Ctrl+C to stop",
        store.root().display(),
        session_id_short
    ))
    .ok();

    // 5. Run the watcher on a blocking task, with a tokio signal
    //    listener racing it. The lock guard lives in this async frame
    //    and is dropped on return, releasing the lock.
    let (stop_tx, stop_rx) = mpsc::channel();
    let signal_handle = tokio::spawn(async move {
        wait_for_termination_signal().await;
        let _ = stop_tx.send(());
    });

    let store_for_watch = store.clone();
    let watch_join = tokio::task::spawn_blocking(move || {
        grs_lib::watcher::Watcher::new(store_for_watch).run(&stop_rx)
    });

    let watch_result = watch_join.await;
    // The signal task is no longer needed once the watcher has returned.
    signal_handle.abort();

    match watch_result {
        Ok(Ok(())) => {
            info!("grs watch: stopped cleanly");
            Ok(())
        }
        Ok(Err(e)) => {
            warn!(?e, "grs watch: stopped with error");
            Err(CommandError::from(e))
        }
        Err(join_err) => {
            warn!(?join_err, "grs watch: blocking task panicked");
            Err(CommandError::internal_error(join_err))
        }
    }
}

/// Wait for SIGINT (Ctrl+C) on all platforms, plus SIGTERM on Unix.
async fn wait_for_termination_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let sigterm = signal(SignalKind::terminate());
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::select! {
            _ = async {
                match sigterm {
                    Ok(mut s) => { let _ = s.recv().await; }
                    Err(_) => { let _ = tokio::signal::ctrl_c().await; }
                }
            } => {}
            _ = ctrl_c => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::tempdir;

    /// End-to-end: spawn a blocking task running the watcher, write a
    /// file, verify a snap is created. Mirrors the in-process TUI test
    /// but goes through the headless entry point.
    #[test]
    fn headless_watcher_captures_snap_on_save() {
        let dir = tempdir().unwrap();
        let store = RepoStore::init(dir.path()).unwrap();
        let s = store.open_first_session("headless".into()).unwrap();
        let session_id = s.id.clone();
        let initial = store.snaps().count(&session_id).unwrap();

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher_store = store.clone();
        let handle = std::thread::spawn(move || {
            let _ = grs_lib::watcher::Watcher::new(watcher_store).run(&stop_rx);
        });
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.path().join("hello.txt"), "world\n").unwrap();
        std::thread::sleep(Duration::from_millis(1000));
        let after = store.snaps().count(&session_id).unwrap();
        assert!(
            after > initial,
            "headless watcher should capture at least one new snap (initial={initial}, after={after})"
        );
        let snap = store.snaps().read(&session_id, after).unwrap();
        assert_eq!(snap.file_path, "hello.txt");
        stop_tx.send(()).ok();
        let _ = handle.join();
    }
}
