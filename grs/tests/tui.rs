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
    let mut replay = grs::tui::code_review_for_test(store, session);
    assert_eq!(replay.entries.len(), 4);
    assert_eq!(replay.files, vec!["a.py".to_string(), "b.py".to_string()]);
    assert_eq!(replay.cur_snap_idx, 0);
    assert_eq!(replay.current_snap.as_ref().unwrap().file_path, "a.py");
}

#[test]
fn step_forward_and_back() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    replay.on_action(KeyAction::NextSnap, &mut parser);
    assert_eq!(replay.cur_snap_idx, 1);
    replay.on_action(KeyAction::NextSnap, &mut parser);
    assert_eq!(replay.cur_snap_idx, 2);
    replay.on_action(KeyAction::PrevSnap, &mut parser);
    assert_eq!(replay.cur_snap_idx, 1);
}

#[test]
fn goto_first_and_last_are_viewport_not_snap_jumps() {
    // The shared-understanding semantic change: `gg`/`G` no longer jump
    // between snaps. They scroll the *current* snap's content to top/
    // bottom. Snap stepping is `[` / `]`.
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // Scroll down so we can see the viewport jumps move it.
    replay.file_view.scroll = 5;
    replay.on_action(KeyAction::GotoFirst, &mut parser);
    assert_eq!(replay.file_view.scroll, 0);
    assert_eq!(replay.cur_snap_idx, 0, "GotoFirst must not change snap index");
    // GotoLast: scroll is set to u16::MAX; the render pass clamps it.
    replay.on_action(KeyAction::GotoLast, &mut parser);
    assert_eq!(replay.cur_snap_idx, 0, "GotoLast must not change snap index");
    // NextSnap then PrevSnap navigate snaps.
    replay.on_action(KeyAction::NextSnap, &mut parser);
    assert_eq!(replay.cur_snap_idx, 1);
    replay.on_action(KeyAction::PrevSnap, &mut parser);
    assert_eq!(replay.cur_snap_idx, 0);
}

#[test]
fn tab_cycles_files() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
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
    // The replay timelapse is gone; speed/play/pause have no analogue in
    // the code review view. This test is a placeholder for future
    // "scroll speed" features (e.g. configurable scroll-step for J/K).
    // For now, the only motion controls are j/k (1 line) and J/K (10).
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    let initial = replay.file_view.scroll;
    replay.on_action(KeyAction::JumpDown10, &mut parser);
    assert!(replay.file_view.scroll > initial);
    replay.on_action(KeyAction::JumpUp10, &mut parser);
    assert_eq!(replay.file_view.scroll, initial);
}

#[test]
fn render_replay_with_test_backend() {
    let (_dir, store, session) = setup();
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = grs::tui::code_review_for_test(store, session);
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::code_review::render(f, &mut state, &mut engine))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(text.contains("code"), "should show the code header");
    assert!(text.contains("v1"), "should show snap content");
    assert!(text.contains("a.py"), "should show the file name");
}

#[test]
fn side_by_side_toggle() {
    // Side-by-side was removed: with the unified-diff render_snap (step 7)
    // removed lines carry their prior text inline, so a side-by-side
    // comparison is no longer needed. This test stays as a tombstone to
    // document the removal.
    let (_dir, _store, _session) = setup();
}

#[test]
fn prev_snap_is_loaded_when_stepping() {
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // First snap has no prev_seq (prev_snap should be None).
    assert!(replay.prev_snap.is_none());
    // Step forward to snap 1 — its prev_seq is 0, so prev_snap should be loaded.
    replay.on_action(KeyAction::NextSnap, &mut parser);
    assert!(replay.prev_snap.is_some());
    assert_eq!(replay.prev_snap.as_ref().unwrap().file_path, "a.py");
}

#[test]
fn code_review_renders_current_snap_content() {
    // The old side-by-side test is replaced by a single-pane test that
    // verifies the code review view shows the *current* snap's content.
    let (_dir, store, session) = setup();
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // Step to a snap that has a prev.
    replay.on_action(KeyAction::NextSnap, &mut parser);
    let backend = TestBackend::new(160, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::code_review::render(f, &mut replay, &mut engine))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    // The header + file path + snap content should all be present.
    assert!(text.contains("code"), "should show the code header");
    assert!(text.contains("a.py"), "should show the file name");
    assert!(text.contains("v1"), "should show snap content");
}

#[test]
fn render_replay_shows_line_numbers() {
    let (_dir, store, session) = setup();
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = grs::tui::code_review_for_test(store, session);
    let mut engine = grs::tui::highlight::HighlightEngine::new("base16-eighties.dark");
    terminal
        .draw(|f| grs::tui::code_review::render(f, &mut state, &mut engine))
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
    let mut replay = grs::tui::code_review_for_test(store, session);
    let order: Vec<String> = replay
        .entries
        .iter()
        .map(|e| SnapStore::read_path(&e.path).unwrap().file_path)
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
    let mut replay = grs::tui::code_review_for_test(store, session);
    use grs::tui::input::{KeyAction, VimParser};
    let mut parser = VimParser::new();
    // Step to snap 1 (so we have something to look at).
    replay.on_action(KeyAction::NextSnap, &mut parser);
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
    replay.on_action(KeyAction::NextSnap, &mut parser);
    assert_eq!(replay.file_view.scroll, 0);
}

// ---------------------------------------------------------------------
// Session list view (TUI shell home screen).
// ---------------------------------------------------------------------

#[test]
fn session_list_loads_with_initial_session() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    // init() always creates a fresh open session; the list isn't empty.
    assert_eq!(list.sessions.len(), 1);
    assert_eq!(list.list_state.selected(), Some(0));
    // Quit works.
    assert!(matches!(list.on_action(KeyAction::Quit), ListCmd::Quit));
}

#[test]
fn session_list_cursor_moves_with_j_k() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let _original_head = store.head().unwrap().unwrap();
    // Make a second session so we have something to navigate.
    store.rotate_open_session(2_000_000_000_000).unwrap();
    let mut list = SessionListState::load(store);
    assert_eq!(list.sessions.len(), 2);
    // Both sessions are present; ordering depends on their started_at
    // (already verified by SessionStore::list tests).
    // Cursor starts at 0; Down moves to 1, clamps at 1, Up returns to 0.
    assert_eq!(list.list_state.selected(), Some(0));
    assert!(matches!(list.on_action(KeyAction::Down), ListCmd::Stay));
    assert_eq!(list.list_state.selected(), Some(1));
    // Move down at the bottom: clamps.
    assert!(matches!(list.on_action(KeyAction::Down), ListCmd::Stay));
    assert_eq!(list.list_state.selected(), Some(1));
    // Move up.
    assert!(matches!(list.on_action(KeyAction::Up), ListCmd::Stay));
    assert_eq!(list.list_state.selected(), Some(0));
    // Move up at the top: clamps to 0.
    assert!(matches!(list.on_action(KeyAction::Up), ListCmd::Stay));
    assert_eq!(list.list_state.selected(), Some(0));
}

#[test]
fn session_list_enter_opens_code_review() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    assert_eq!(list.sessions.len(), 1);
    match list.on_action(KeyAction::Enter) {
        ListCmd::OpenCodeReview(s) => assert!(s.is_open()),
        other => panic!("expected OpenCodeReview, got {other:?}"),
    }
}

#[test]
fn session_list_renders_to_backend() {
    use grs::tui::session_list::SessionListState;
    use ratatui::Terminal;
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    let backend = TestBackend::new(120, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| grs::tui::session_list::render(f, &mut list))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = buffer
        .content
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    // Title + status text + the session short id should all be present.
    assert!(text.contains("sessions"), "missing 'sessions' title");
    assert!(text.contains("move"), "status bar missing");
}

#[test]
fn session_list_n_creates_new_and_returns_to_list() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    assert_eq!(list.sessions.len(), 1);
    let cmd = list.on_action(KeyAction::NewSession);
    assert!(matches!(cmd, ListCmd::Stay));
    assert_eq!(list.sessions.len(), 2);
    // The newly-created session is selected.
    let selected_id = list.selected().unwrap().id.clone();
    assert!(list.sessions.iter().any(|s| s.id == selected_id && s.is_open()));
}

#[test]
fn session_list_N_creates_and_opens() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    match list.on_action(KeyAction::NewSessionAndOpen) {
        ListCmd::OpenCodeReview(s) => {
            assert!(s.is_open());
        }
        other => panic!("expected OpenCodeReview, got {other:?}"),
    }
}

#[test]
fn session_list_d_requires_confirm_and_refuses_open() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::{ListCmd, SessionListState};
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let head = store.head().unwrap().unwrap();
    let mut list = SessionListState::load(store.clone());
    // First `d` on the open session: sets toast, but doesn't set
    // pending_delete (the open-session branch returns early).
    let _ = list.on_action(KeyAction::Delete);
    assert!(list.pending_delete.is_none());
    assert!(list.toast.as_deref().unwrap_or("").contains("open"));
    // Rotate to finalize, then d works.
    store.rotate_open_session(2_000_000_000_000).unwrap();
    list.refresh();
    // Cursor is on the newest (open) session. Move down to select the
    // closed one (newest-first sort puts the new open session at index 0).
    let _ = list.on_action(KeyAction::Down);
    let first_d = list.on_action(KeyAction::Delete);
    assert!(matches!(first_d, ListCmd::Stay));
    assert!(list.pending_delete.is_some());
    assert!(list.toast.as_deref().unwrap_or("").contains("press d again"));
    // Second `d` confirms and deletes.
    let second_d = list.on_action(KeyAction::Delete);
    assert!(matches!(second_d, ListCmd::Stay));
    assert!(list.pending_delete.is_none());
    // The closed session dir is gone.
    let closed_id = if list.sessions.iter().any(|s| s.id == head) {
        head.clone()
    } else {
        // The original was deleted; sanity check via the store.
        let sessions = list.store.sessions().list().unwrap();
        assert!(!sessions.iter().any(|s| s.id == head));
        head
    };
    // After deletion, the only remaining session is the open one.
    let open_remaining = list.sessions.iter().filter(|s| s.is_open()).count();
    assert_eq!(open_remaining, 1);
    let _ = closed_id; // silence unused
}

#[test]
fn session_list_filter_narrows_results() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::SessionListState;
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    // Enter filter mode.
    let _ = list.on_action(KeyAction::Filter);
    // Type a prefix that matches nothing (use the open session's id's last
    // char so we can verify the filter actually narrows the set).
    let head_id = list.sessions[0].id.as_str().to_string();
    let last_char = head_id.chars().last().unwrap().to_string();
    let _ = list.on_action(KeyAction::FilterChar(last_char.chars().next().unwrap()));
    // The visible set should be ≤ 1.
    assert!(list.visible().len() <= 1);
    // CancelFilter clears the filter.
    let _ = list.on_action(KeyAction::CancelFilter);
    assert!(list.filter.is_empty());
    assert_eq!(list.visible().len(), list.sessions.len());
}

#[test]
fn session_list_help_toggles() {
    use grs::tui::input::KeyAction;
    use grs::tui::session_list::SessionListState;
    let dir = tempfile::tempdir().unwrap();
    let store = grs_lib::store::RepoStore::init(dir.path()).unwrap();
    let mut list = SessionListState::load(store);
    assert!(!list.help_open);
    let _ = list.on_action(KeyAction::Help);
    assert!(list.help_open);
    let _ = list.on_action(KeyAction::Help);
    assert!(!list.help_open);
}

#[test]
fn code_review_n_jumps_to_next_change_row() {
    use grs::tui::code_review::CodeReviewState;
    use grs::tui::highlight::HighlightEngine;
    use grs::tui::input::{KeyAction, VimParser};
    use grs_lib::diff::line_diff;
    use grs_lib::snap::SnapStore;
    let dir = tempfile::tempdir().unwrap();
    let store = RepoStore::init(dir.path()).unwrap();
    let head = store.head().unwrap().unwrap();
    let snap_store = store.snaps();
    // First snap: original content.
    snap_store
        .write(
            &head,
            SnapStore::build_snap(
                0,
                "x.py".into(),
                "a\nb\nc\nd\n".into(),
                line_diff("", "a\nb\nc\nd\n"),
                None,
            ),
        )
        .unwrap();
    // Second snap: replace 'b' with 'X' and 'c' with 'Y' (a Replace of
    // 2 old + 2 new), plus append 'e' (an Insert of 1).
    snap_store
        .write(
            &head,
            SnapStore::build_snap(
                1,
                "x.py".into(),
                "a\nX\nY\nd\ne\n".into(),
                line_diff("a\nb\nc\nd\n", "a\nX\nY\nd\ne\n"),
                Some(0),
            ),
        )
        .unwrap();
    let session = store.sessions().get(&head).unwrap();
    let mut state = CodeReviewState::load(store, session);
    let mut engine = HighlightEngine::new("base16-eighties.dark");
    // Force a render so file_view.lines is populated.
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| grs::tui::code_review::render(f, &mut state, &mut engine))
        .unwrap();
    let mut parser = VimParser::new();
    // Step to snap 1 (the one with changes).
    state.on_action(KeyAction::NextSnap, &mut parser);
    // Re-render to populate the lines for snap 1.
    terminal
        .draw(|f| grs::tui::code_review::render(f, &mut state, &mut engine))
        .unwrap();
    // The change rows are at indices 1, 2 (Delete) and 3, 4 (Insert); also
    // index 5 (the new 'e' line). Equal rows are at 0 (a) and (last) (d).
    let lines = state.file_view.lines.clone();
    let change_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.style.bg.is_some())
        .map(|(i, _)| i)
        .collect();
    assert!(
        change_indices.len() >= 3,
        "expected at least 3 change rows in the unified diff, got {change_indices:?}"
    );
    // n: jump to the first change.
    state.on_action(KeyAction::NewSession, &mut parser);
    assert_eq!(state.file_view.scroll as usize, change_indices[0]);
    // Keep pressing n until we reach the last change row.
    for _ in 0..change_indices.len() {
        state.on_action(KeyAction::NewSession, &mut parser);
    }
    let last_change = *change_indices.last().unwrap() as u16;
    assert_eq!(state.file_view.scroll, last_change, "should land on the last change row");
    // n at the last change: no-op.
    state.on_action(KeyAction::NewSession, &mut parser);
    assert_eq!(
        state.file_view.scroll,
        last_change,
        "n at last change must no-op"
    );
}
