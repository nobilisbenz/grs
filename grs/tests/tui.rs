//! End-to-end TUI tests using ratatui's `TestBackend` (no real TTY needed).
//! Drives the replay state machine + input parser to verify stepping,
//! playback, jumps, tabs, and rendering.

use grs_lib::model::Session;
use grs_lib::snap::SnapStore;
use grs_lib::store::RepoStore;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn setup() -> (tempfile::TempDir, RepoStore, Session) {
    let dir = tempfile::tempdir().unwrap();
    let store = RepoStore::init(dir.path()).unwrap();
    let head = store.head().unwrap().unwrap();

    // Write 4 snaps: 3 of a.py, 1 of b.py.
    for (seq, path, content, prev) in [
        (0u32, "a.py", "v1\n".to_string(), None),
        (1, "a.py", "v1\nv2\n".to_string(), Some(0u32)),
        (2, "a.py", "v1\nv2\nv3\n".to_string(), Some(1u32)),
        (3, "b.py", "x\n".to_string(), None),
    ] {
        let mut diff = grs_lib::diff::line_diff("", &content);
        diff.prev_seq = prev;
        let snap = SnapStore::build_snap(seq, path.into(), content, diff, prev);
        store.snaps().write(&head, snap).unwrap();
    }

    let session = Session {
        version: 1,
        id: head.clone(),
        started_at: 0,
        ended_at: Some(0),
        file_count: 2,
        snap_count: 4,
    };
    (dir, store, session)
}

#[test]
fn replay_state_loads_4_snaps_and_2_files() {
    let (_dir, store, session) = setup();
    let replay = grs::tui::replay_view_for_test(store, session);
    assert_eq!(replay.entries.len(), 4);
    assert_eq!(replay.files, vec!["a.py".to_string(), "b.py".to_string()]);
    assert_eq!(replay.cur_snap_idx, 0);
    assert_eq!(replay.current_snap.as_ref().unwrap().file_path, "a.py");
}

#[test]
fn step_forward_and_back() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    replay.on_action(KeyAction::StepFwd, &mut parser);
    assert_eq!(replay.cur_snap_idx, 1);
    replay.on_action(KeyAction::StepFwd, &mut parser);
    assert_eq!(replay.cur_snap_idx, 2);
    replay.on_action(KeyAction::StepBack, &mut parser);
    assert_eq!(replay.cur_snap_idx, 1);
}

#[test]
fn goto_first_last_and_colon_jump() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    replay.on_action(KeyAction::GotoLast, &mut parser);
    assert_eq!(replay.cur_snap_idx, 3);
    replay.on_action(KeyAction::GotoFirst, &mut parser);
    assert_eq!(replay.cur_snap_idx, 0);
    replay.on_action(KeyAction::GotoSnap(3), &mut parser);
    assert_eq!(replay.cur_snap_idx, 2); // 1-based
}

#[test]
fn tab_cycles_files() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    assert_eq!(replay.current_snap.as_ref().unwrap().file_path, "a.py");
    replay.on_action(KeyAction::TabFile, &mut parser);
    assert_eq!(replay.current_snap.as_ref().unwrap().file_path, "b.py");
    assert_eq!(replay.cur_file_idx, 1);
    replay.on_action(KeyAction::TabFile, &mut parser);
    assert_eq!(replay.current_snap.as_ref().unwrap().file_path, "a.py");
    assert_eq!(replay.cur_file_idx, 0);
}

#[test]
fn speed_up_and_slow_down() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    let initial = replay.speed_ms;
    replay.on_action(KeyAction::Faster, &mut parser);
    assert!(replay.speed_ms < initial);
    replay.on_action(KeyAction::Slower, &mut parser);
    assert_eq!(replay.speed_ms, initial);
}

#[test]
fn play_pause_advances_snaps() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    replay.on_action(KeyAction::PlayPause, &mut parser);
    assert!(replay.playing);
    // Force a tick by rewinding last_tick.
    replay.last_tick = std::time::Instant::now() - std::time::Duration::from_millis(10_000);
    let moved = replay.tick();
    assert!(moved);
    assert_eq!(replay.cur_snap_idx, 1);
    replay.on_action(KeyAction::PlayPause, &mut parser);
    assert!(!replay.playing);
}

#[test]
fn render_replay_with_test_backend() {
    let (_dir, store, session) = setup();
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = grs::tui::replay_view_for_test(store, session);
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::replay_view::render(f, &mut state, &mut engine))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(text.contains("replay"), "should show the replay header");
    assert!(text.contains("v1"), "should show snap content");
    assert!(text.contains("a.py"), "should show the file name");
}

#[test]
fn side_by_side_toggle() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();

    // Initially off.
    assert!(!replay.side_by_side);
    // Toggle on.
    replay.on_action(KeyAction::SideBySide, &mut parser);
    assert!(replay.side_by_side);
    // Toggle off.
    replay.on_action(KeyAction::SideBySide, &mut parser);
    assert!(!replay.side_by_side);
}

#[test]
fn prev_snap_is_loaded_when_stepping() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // First snap has no prev_seq (prev_snap should be None).
    assert!(replay.prev_snap.is_none());
    // Step forward to snap 1 — its prev_seq is 0, so prev_snap should be loaded.
    replay.on_action(KeyAction::StepFwd, &mut parser);
    assert!(replay.prev_snap.is_some());
    assert_eq!(replay.prev_snap.as_ref().unwrap().file_path, "a.py");
}

#[test]
fn side_by_side_renders_both_panes() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // Step to a snap that has a prev.
    replay.on_action(KeyAction::StepFwd, &mut parser);
    replay.side_by_side = true;
    let backend = TestBackend::new(160, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::replay_view::render(f, &mut replay, &mut engine))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    // Both pane titles should be present.
    assert!(text.contains("prev"), "should show prev pane title");
    assert!(text.contains("current"), "should show current pane title");
}

#[test]
fn render_replay_shows_line_numbers() {
    let (_dir, store, session) = setup();
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = grs::tui::replay_view_for_test(store, session);
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::replay_view::render(f, &mut state, &mut engine))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    // The gutter should appear for the first content line of the current snap.
    assert!(
        text.contains("1 │") || text.contains("1\u{2502}"),
        "expected a line-number gutter in the rendered output, got:\n{text:?}"
    );
}

#[test]
fn entries_are_sorted_by_time_not_seq() {
    // Build a session with intentionally out-of-order timestamps vs. seqs
    // (e.g. a stale on-disk file where two files share a seq from the old
    // per-file scheme). The replay should still play them in real time.
    let dir = tempfile::tempdir().unwrap();
    let store = RepoStore::init(dir.path()).unwrap();
    let head = store.head().unwrap().unwrap();
    let snap = |seq: u32, file: &str, ts: i64, prev: Option<u32>| {
        let mut s = SnapStore::build_snap(
            seq,
            file.into(),
            format!("v{seq}\n"),
            grs_lib::diff::line_diff("", &format!("v{seq}\n")),
            prev,
        );
        s.timestamp = ts;
        s.timestamp_iso = format!("ts-{ts}");
        s
    };
    // B has the earliest timestamp (50), then a@100, then a@200.
    // The replay should play them in that order even though their seqs
    // are 0, 1, 2 — i.e. a stale on-disk layout where seqs collides with
    // the old per-file scheme must not change the playback order.
    store.snaps().write(&head, snap(0, "a.py", 100, None)).unwrap();
    store.snaps().write(&head, snap(1, "b.py", 50, None)).unwrap();
    store.snaps().write(&head, snap(2, "a.py", 200, Some(0))).unwrap();

    let session = Session {
        version: 1,
        id: head.clone(),
        started_at: 0,
        ended_at: Some(0),
        file_count: 2,
        snap_count: 3,
    };
    let replay = grs::tui::replay_view_for_test(store, session);
    let order: Vec<String> = replay
        .entries
        .iter()
        .map(|e| {
            SnapStore::read_path(&e.path)
                .unwrap()
                .file_path
        })
        .collect();
    assert_eq!(
        order,
        vec!["b.py".to_string(), "a.py".to_string(), "a.py".to_string()],
        "entries must be ordered by capture timestamp, not seq"
    );
}

#[test]
fn periodic_refresh_does_not_reset_scroll() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::replay_view_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // Step to snap 1 (so we have something to look at).
    replay.on_action(KeyAction::StepFwd, &mut parser);
    // Scroll down a few lines.
    replay.file_view.scroll = 7;
    let before = replay.file_view.scroll;
    // The TUI calls `refresh()` once a second; that must NOT yank the
    // viewport back to the top when the current snap hasn't changed.
    replay.refresh();
    assert_eq!(
        replay.file_view.scroll, before,
        "scroll must be preserved across a no-op refresh"
    );

    // But stepping to a new snap *should* reset scroll — that's the
    // documented behaviour and is what the user expects.
    replay.on_action(KeyAction::StepFwd, &mut parser);
    assert_eq!(replay.file_view.scroll, 0);
}
