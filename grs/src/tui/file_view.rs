//! File view: render a `Vec<Line>` (already syntax-highlighted + diff-tinted)
//! inside the area, with vertical scroll.

use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Default, Debug, Clone)]
pub struct FileViewState {
    /// Pre-built lines (filled in by the replay view each draw).
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
