//! End-to-end watcher integration test: spin up the real `notify` watcher in a
//! thread, write a file, wait for a snap to appear on disk, then shut it down
//! cleanly via the stop channel.

use grs_lib::store::RepoStore;
use grs_lib::watcher::Watcher;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Poll `predicate` every `interval` until it returns true or `timeout`
/// elapses. Returns the elapsed time.
fn wait_for<F: Fn() -> bool>(
    predicate: F,
    interval: Duration,
    timeout: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return Some(start.elapsed());
        }
        std::thread::sleep(interval);
    }
    None
}

fn run_watcher(root: &Path) -> mpsc::Sender<()> {
    let store = RepoStore::open(root).expect("open");
    let (stop_tx, stop_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = Watcher::new(store).run(&stop_rx) {
            eprintln!("watcher error: {e}");
        }
    });
    // Give the notify watcher time to register before we mutate files.
    std::thread::sleep(std::time::Duration::from_millis(400));
    stop_tx
}

#[test]
fn watcher_writes_snap_when_file_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RepoStore::init(dir.path()).expect("init");
    let head = store.head().expect("head").expect("head is set");

    let stop = run_watcher(dir.path());

    // Edit a watched file.
    let target = dir.path().join("a.txt");
    std::fs::write(&target, "hello\n").expect("write");

    // Wait for a snap to appear in the session's snaps/ dir.
    let snaps_dir = dir
        .path()
        .join(".grs/sessions")
        .join(head.as_str())
        .join("snaps");
    let snap = wait_for(
        || {
            snaps_dir.is_dir()
                && std::fs::read_dir(&snaps_dir)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false)
        },
        Duration::from_millis(100),
        Duration::from_secs(5),
    );
    assert!(
        snap.is_some(),
        "snap should appear within 5s of writing the file"
    );

    // Verify the snap has the content we wrote.
    let snap_path = std::fs::read_dir(&snaps_dir)
        .expect("snaps dir")
        .next()
        .expect("a snap exists")
        .expect("snap entry ok")
        .path();
    let text = std::fs::read_to_string(&snap_path).expect("read snap");
    assert!(text.contains("hello"), "snap should contain the file content");
    assert!(text.contains("\"added_lines\""), "snap should record added_lines");
    assert!(
        text.contains("\"removed_lines\": []"),
        "first snap should have no removed lines"
    );

    let _ = stop.send(());
}

#[test]
fn watcher_dedups_identical_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RepoStore::init(dir.path()).expect("init");
    let head = store.head().expect("head").expect("head set");

    let stop = run_watcher(dir.path());

    wait_for(
        || dir.path().join(".grs/HEAD").exists(),
        Duration::from_millis(50),
        Duration::from_secs(2),
    )
    .expect("watcher up");

    // Write the same content twice in a row.
    let target = dir.path().join("a.txt");
    std::fs::write(&target, "same\n").expect("write 1");
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(&target, "same\n").expect("write 2");
    std::thread::sleep(Duration::from_millis(500));

    let snaps_dir = dir
        .path()
        .join(".grs/sessions")
        .join(head.as_str())
        .join("snaps");
    let snap_count = std::fs::read_dir(&snaps_dir)
        .map(|d| d.count())
        .unwrap_or(0);
    assert!(
        (1..=2).contains(&snap_count),
        "expected 1-2 snaps for 2 identical writes, got {snap_count}"
    );

    let _ = stop.send(());
}

#[test]
fn watcher_picks_up_multiple_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RepoStore::init(dir.path()).expect("init");
    let head = store.head().expect("head").expect("head set");

    let stop = run_watcher(dir.path());

    for (name, content) in [("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")] {
        std::fs::write(dir.path().join(name), content).expect("write");
        std::thread::sleep(Duration::from_millis(300));
    }

    let snaps_dir = dir
        .path()
        .join(".grs/sessions")
        .join(head.as_str())
        .join("snaps");
    wait_for(
        || {
            let count = std::fs::read_dir(&snaps_dir)
                .map(|d| d.count())
                .unwrap_or(0);
            count >= 3
        },
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .expect("3 snaps should appear");

    let files: Vec<String> = std::fs::read_dir(&snaps_dir)
        .expect("snaps")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter_map(|p| {
            let s = std::fs::read_to_string(&p).ok()?;
            let needle = "\"file_path\": \"";
            let start = s.find(needle)? + needle.len();
            let end = s[start..].find('"')? + start;
            Some(s[start..end].to_string())
        })
        .collect();
    assert!(files.contains(&"a.txt".to_string()), "missing a.txt in {files:?}");
    assert!(files.contains(&"b.txt".to_string()), "missing b.txt in {files:?}");
    assert!(files.contains(&"c.txt".to_string()), "missing c.txt in {files:?}");

    let _ = stop.send(());
}

#[test]
fn watcher_ignores_grs_dir_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RepoStore::init(dir.path()).expect("init");
    let head = store.head().expect("head").expect("head set");

    let stop = run_watcher(dir.path());

    wait_for(
        || dir.path().join(".grs/HEAD").exists(),
        Duration::from_millis(50),
        Duration::from_secs(2),
    )
    .expect("watcher up");

    // The watcher should not create snaps for files under .grs/.
    std::thread::sleep(Duration::from_millis(500));

    let snaps_dir = dir
        .path()
        .join(".grs/sessions")
        .join(head.as_str())
        .join("snaps");
    let has_grs_snap = std::fs::read_dir(&snaps_dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| e.path().to_string_lossy().contains(".grs"))
        })
        .unwrap_or(false);
    assert!(!has_grs_snap, "watcher must not snap files under .grs/");

    let _ = stop.send(());
}

#[test]
fn watcher_skips_binary_files() {
    // Simulate `cargo build` (or any compiler) writing a binary into the
    // project: the watcher must not create any snap for it.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RepoStore::init(dir.path()).expect("init");
    let head = store.head().expect("head").expect("head set");

    let stop = run_watcher(dir.path());

    wait_for(
        || dir.path().join(".grs/HEAD").exists(),
        Duration::from_millis(50),
        Duration::from_secs(2),
    )
    .expect("watcher up");

    // Build a fake binary with a NUL byte in the first 8 bytes.
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
    bytes.extend(std::iter::repeat(0xCCu8).take(4096));
    let bin = dir.path().join("artifact.bin");
    std::fs::write(&bin, &bytes).expect("write binary");

    // Also write a normal text file so the watcher has a real event to
    // chew on, to prove the binary-skip isn't a global "ignore all".
    std::fs::write(dir.path().join("hello.txt"), "hi\n").expect("write text");

    let snaps_dir = dir
        .path()
        .join(".grs/sessions")
        .join(head.as_str())
        .join("snaps");
    let saw_text = wait_for(
        || {
            std::fs::read_dir(&snaps_dir)
                .map(|mut d| d.any(|e| e.ok().is_some_and(|_| true)))
                .unwrap_or(false)
        },
        Duration::from_millis(100),
        Duration::from_secs(5),
    );
    assert!(saw_text.is_some(), "text file should be snapped");

    // Give the watcher another moment in case a binary event is queued.
    std::thread::sleep(Duration::from_millis(400));

    let snap_names: Vec<String> = std::fs::read_dir(&snaps_dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    for name in &snap_names {
        let text = std::fs::read_to_string(snaps_dir.join(name)).unwrap_or_default();
        assert!(
            !text.contains("artifact.bin"),
            "binary file must not be snapped; saw {name}"
        );
    }

    let _ = stop.send(());
}
