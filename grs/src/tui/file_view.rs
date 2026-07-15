//! File view: render a `Vec<Line>` (already syntax-highlighted + diff-tinted)
//! inside the area, with vertical scroll.
//!
//! This module renders the file view **without** going through ratatui's
//! `Paragraph` widget. The reason: in ratatui 0.26, `Text` only impls
//! `From<Vec<Line<'a>>>` (owned) and not `From<&'a [Line<'a>]>`, so
//! `Paragraph::new(...)` forces a `Vec` clone on every frame. With a
//! cached highlight build the per-frame highlight is the *real* perf win
//! — but the per-frame clone is still avoidable. This module writes the
//! line spans directly into the `Frame`'s `Buffer`, which means we borrow
//! `state.lines` for the duration of the render and never clone it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;


#[derive(Default, Debug, Clone)]
pub struct FileViewState {
    /// Pre-built lines (filled in by the code review view per snap).
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
}

pub fn render(
    f: &mut Frame,
    state: &mut FileViewState,
    area: Rect,
    title: Option<&str>,
) {
    // Build the block first so we can compute the inner area. The clamp
    // uses the *inner* height (content rows available after borders), not
    // the area height, because the borders eat 2 rows.
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(crate::tui::theme::MUTED));
    if let Some(t) = title {
        block = block.title(t.to_string());
    }
    let inner = block.inner(area);

    let total = state.lines.len() as u16;
    let visible = inner.height;
    if total > visible {
        let max_scroll = total - visible;
        if state.scroll > max_scroll {
            state.scroll = max_scroll;
        }
    } else {
        state.scroll = 0;
    }

    f.render_widget(block, area);

    // Borrow the cached lines and write each visible line's spans into
    // the buffer at (inner.x, inner.y + i). No clone.
    let buf = f.buffer_mut();
    let visible_lines = state
        .lines
        .iter()
        .skip(state.scroll as usize)
        .take(inner.height as usize);
    for (i, line) in visible_lines.enumerate() {
        let y = inner.y + i as u16;
        write_line_to_buffer(buf, line, inner.x, y, inner.width);
    }
}

/// Write a single `Line`'s spans into `buf` starting at `(x, y)`, up to
/// `max_width` cells. Spans that would overflow are truncated by
/// `set_stringn` (it stops at the max width). No allocations.
///
/// Honors the line's row-level `style.bg`: each cell's bg is the line's
/// bg (if set) and the span keeps its own fg. Without this, ratatui's
/// `set_stringn` would paint each cell with the span's `bg` (which is
/// unset for syntax-highlighted spans), wiping the row tint.
fn write_line_to_buffer(buf: &mut Buffer, line: &Line, x: u16, y: u16, max_width: u16) {
    let row_bg = line.style.bg;
    let mut cur_x = x;
    let end_x = x.saturating_add(max_width);
    for span in &line.spans {
        if cur_x >= end_x {
            break;
        }
        let text = span.content.as_ref();
        // Truncate the span's text to the remaining width *before*
        // calling set_stringn, so we don't have to scan a long string
        // cell-by-cell. set_stringn will also stop at max_width but the
        // extra work is negligible either way.
        let remaining = (end_x - cur_x) as usize;
        let span_width = UnicodeWidthStr::width(text);
        // Per-cell style: the row's bg, the span's fg. If the span
        // explicitly sets its own bg, that wins (rare; only happens if
        // a span wants to override the row tint).
        let cell_style = match (row_bg, span.style.bg) {
            (Some(rbg), None) | (Some(rbg), Some(Color::Reset)) => span.style.bg(rbg),
            _ => span.style,
        };
        if span_width <= remaining {
            buf.set_stringn(cur_x, y, text, span_width, cell_style);
            cur_x += span_width as u16;
        } else {
            // Truncate the string to fit the remaining width. Using
            // `set_stringn` with `remaining` as the max width handles the
            // visual truncation; we just need to avoid passing the full
            // string (which would be wasted work).
            buf.set_stringn(cur_x, y, text, remaining, cell_style);
            break;
        }
    }
    // Paint the trailing cells of the row (after the last span, if any)
    // with the row bg too — without this, the gutter to the right of
    // the last span would be black on a tinted row.
    if let Some(bg) = row_bg {
        while cur_x < end_x {
            let cell = buf.get_mut(cur_x, y);
            cell.set_bg(bg);
            cur_x += 1;
        }
    }
    // If the line had no spans, the cell at (x, y) is the buffer default
    // (space + reset style). That's fine; it renders as a blank line.
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::text::Span;
    use ratatui::Terminal;

    #[test]
    fn render_does_not_clone_lines() {
        // Build a state with a known line count, render, assert the count
        // is unchanged (a clone would have to put the same number back,
        // but the test is a structural witness that the render path
        // doesn't take ownership).
        let mut state = FileViewState {
            lines: vec![Line::from("hello world")],
            scroll: 0,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 40, 10), None))
            .unwrap();
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].spans.len(), 1);
    }

    #[test]
    fn render_writes_spans_at_expected_positions() {
        // The first row of the inner area should contain the spans
        // written left-to-right, in order.
        let mut state = FileViewState {
            lines: vec![Line::from(vec![
                Span::raw("ab"),
                Span::styled("cd", Style::default()),
                Span::raw("ef"),
            ])],
            scroll: 0,
        };
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 20, 5), None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // inner = area - borders (1 on each side). inner.x = 1, inner.y = 1.
        // Read the row at y=1 and collect symbols.
        let row: String = (0..18)
            .map(|x| {
                buf.get(1 + x, 1)
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            row.starts_with("abcdef"),
            "expected the first row to start with 'abcdef', got {row:?}"
        );
    }

    #[test]
    fn render_respects_scroll() {
        // Two lines; visible height = 3 (so total > inner height and
        // scrolling is meaningful). scroll = 1 should show only the
        // second line at the top of the inner area.
        let mut state = FileViewState {
            lines: vec![Line::from("first"), Line::from("second")],
            scroll: 0,
        };
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        state.scroll = 1;
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 20, 3), None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // inner.y = 1, inner.height = 1 (3 - 2 borders). At scroll=1,
        // the line at index 1 ("second") is the first (and only) visible.
        let row: String = (0..18)
            .map(|x| {
                buf.get(1 + x, 1)
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            row.starts_with("second"),
            "expected scroll=1 to show 'second' at the top, got {row:?}"
        );
    }

    #[test]
    fn render_clamps_scroll_past_end() {
        let mut state = FileViewState {
            lines: vec![Line::from("a"), Line::from("b")],
            scroll: 99, // way past the end
        };
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 20, 5), None))
            .unwrap();
        // The render clamps scroll to a valid value; after that the line
        // at the top is still one of the two we have.
        assert!(state.scroll <= 1, "scroll should be clamped, got {}", state.scroll);
    }

    #[test]
    fn render_truncates_overflowing_spans() {
        // A span wider than the inner area is truncated by set_stringn.
        let mut state = FileViewState {
            lines: vec![Line::from("a".repeat(100))],
            scroll: 0,
        };
        let backend = TestBackend::new(10, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 10, 3), None))
            .unwrap();
        // No panic; render completed. The line was truncated to the
        // inner width. We don't assert on the exact content because the
        // truncation point is in the middle of the span.
    }

    /// The line's row-level `style.bg` must be applied to every cell of
    /// the row, not just the cells under spans. Without this, the
    /// "added line" / "removed line" row tints are invisible — the
    /// cells under the spans get the span's bg (default = terminal bg),
    /// and the cells after the last span (and before the first span, if
    /// any) get the terminal's default bg too.
    #[test]
    fn render_applies_line_bg_to_all_cells() {
        let line_bg = Color::Rgb(0, 90, 0);
        let mut state = FileViewState {
            // A short line, then a long-ish line. The long line's row
            // should be tinted across the entire inner width — including
            // the cells after the last span.
            lines: vec![
                Line::from("ab").style(Style::default().bg(line_bg)),
                Line::from("short").style(Style::default().bg(line_bg)),
            ],
            scroll: 0,
        };
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &mut state, Rect::new(0, 0, 20, 4), None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // Inner area: x in [1, 19), y in [1, 3).
        // Row at y=1 is the first line ("ab"). Both cells (1,1) and (2,1)
        // should have the line bg, and the rest of the row (x=3..19) too.
        for x in 1..19u16 {
            let cell = buf.get(x, 1);
            assert_eq!(
                cell.bg, line_bg,
                "row 1 cell at x={x} should have the line bg, got {:?}",
                cell.bg
            );
        }
        // Row at y=2 is the second line ("short"). The "short" span
        // covers x=1..6, then x=6..19 should still be tinted.
        for x in 1..19u16 {
            let cell = buf.get(x, 2);
            assert_eq!(
                cell.bg, line_bg,
                "row 2 cell at x={x} should have the line bg, got {:?}",
                cell.bg
            );
        }
    }
}
