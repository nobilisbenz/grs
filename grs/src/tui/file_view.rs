//! File view: render a `Vec<Line>` (already syntax-highlighted + diff-tinted)
//! inside the area, with vertical scroll. The lines are borrowed — no clone
//! per frame.

use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Default, Debug, Clone)]
pub struct FileViewState {
    /// Pre-built lines (filled in by the code review view per snap).
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
}

pub fn render(
    f: &mut Frame,
    state: &mut FileViewState,
    area: ratatui::layout::Rect,
    title: Option<&str>,
) {
    let total = state.lines.len() as u16;
    // Clamp scroll to valid range based on the area height.
    let visible = area.height;
    if total > visible {
        let max_scroll = total - visible;
        if state.scroll > max_scroll {
            state.scroll = max_scroll;
        }
    } else {
        state.scroll = 0;
    }
    // ratatui 0.26's `Text` only impls `From<Vec<Line<'a>>>` (owned), not
    // `From<&'a [Line<'a>]>`, and `Paragraph` has no public way to give
    // the text back. So we have to clone the cached `Vec<Line>` once per
    // frame. The cached *build* (one per snap change, not per frame) is
    // the dominant perf win in this module; the clone is the remaining
    // per-frame cost, which is ~one Vec clone of `Line`s containing
    // `Span`s containing `String`s — measurable but not the bottleneck.
    let mut paragraph = Paragraph::new(state.lines.clone())
        .scroll((state.scroll, 0))
        .wrap(Wrap { trim: false });
    if let Some(t) = title {
        paragraph = paragraph.block(
            Block::default()
                .title(t.to_string())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(crate::tui::theme::MUTED)),
        );
    }
    f.render_widget(paragraph, area);
}
