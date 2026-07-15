//! Project-level lockfile. Used to ensure only one TUI is running on a
//! project at a time.
//!
//! The lock is a PID file at `.grs/.lock`. On startup, we check the PID
//! inside: if it's still alive, refuse to start (another TUI is running);
//! if it's dead (stale lock), remove the file and try again.

use crate::error::{GrsError, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A held lock. The lock is released when this is dropped.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    /// Acquire the project lock. Errors with `AlreadyRunning` if another
    /// live process holds it.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Try a few times in case of a race between stale-lock detection
        // and the new acquirer.
        for _ in 0..3 {
            // Existing lock?
            if path.exists() {
                let pid = read_pid(path);
                if let Some(pid) = pid {
                    if is_process_alive(pid) {
                        return Err(GrsError::AlreadyRunning(format!(
                            "pid {pid} holds the lock at {}",
                            path.display()
                        )));
                    }
                    // Stale — remove and retry.
                    let _ = std::fs::remove_file(path);
                    continue;
                }
                // File exists but unparseable; treat as stale.
                let _ = std::fs::remove_file(path);
                continue;
            }

            // No existing lock — try to create one.
            match write_our_pid(path) {
                Ok(()) => return Ok(Self { path: path.to_path_buf() }),
                Err(_) => {
                    // Lost a race; loop and re-check.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        Err(GrsError::AlreadyRunning(format!(
            "could not acquire lock at {} after 3 attempts",
            path.display()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Only remove if it still contains our PID (so we don't
        // accidentally remove a lock acquired by a successor process).
        if let Some(ours) = read_pid(&self.path) {
            if ours == std::process::id() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.lines().next()?.trim().parse().ok()
}

fn write_our_pid(path: &Path) -> Result<()> {
    // O_EXCL-style create: open with create_new to fail if it already exists.
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| GrsError::from(e))?;
    writeln!(f, "{}", std::process::id())?;
    f.sync_all()?;
    Ok(())
}

/// Heuristic: is the given PID still alive? On Unix, signal 0 is a no-op
/// "check existence" probe. Returns true on platforms where we don't have
/// a way to check (safer default).
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is well-defined as an existence check.
    unsafe {
        // Tiny shim so we don't need to add a `libc` dep just for kill(2).
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid as i32, 0) == 0
    }
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lock_blocks_second_acquire_when_pid_is_self() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".lock");
        let _g = LockGuard::acquire(&path).unwrap();
        // Second attempt must fail (our own PID is still alive).
        let err = LockGuard::acquire(&path).unwrap_err();
        assert!(matches!(err, GrsError::AlreadyRunning(_)));
    }

    #[test]
    fn lock_released_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".lock");
        {
            let _g = LockGuard::acquire(&path).unwrap();
        }
        // After drop, the file is removed.
        assert!(!path.exists());
        // And a fresh acquire works.
        let _g2 = LockGuard::acquire(&path).unwrap();
    }

    #[test]
    fn stale_lock_is_removed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".lock");
        // Write a clearly-dead PID (1 is usually init, but PID 0 is invalid;
        // we'll use a very large number that almost certainly doesn't exist).
        std::fs::write(&path, "999999\n").unwrap();
        // The lock is stale, so acquire should succeed and overwrite.
        let _g = LockGuard::acquire(&path).unwrap();
        // After acquire, the file should contain our PID.
        let pid = read_pid(&path).unwrap();
        assert_eq!(pid, std::process::id());
    }
}
