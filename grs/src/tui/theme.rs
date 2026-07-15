//! Theme / styles for the TUI.

use ratatui::style::{Color, Modifier, Style};

// Raw color constants exported for modules that build spans without
// going through the `Theme` methods.
pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
pub const SCRUBBER_BG: Color = Color::Reset;
pub const STATUS_BG: Color = Color::DarkGray;
pub const STATUS_FG: Color = Color::White;
pub const WARNING: Color = Color::Yellow;

/// Background tints for diff rows. These are *full row* tints (applied to
/// the `Line`'s `style.bg`); syntax `fg` colors stay readable on top.
///
/// Picked to be visible on a default black-background terminal without
/// overpowering the syntax colors. Green/red on black are the most
/// readable, and the row tint is dark enough that white-ish text
/// (typical syntax fg) stays legible.
pub const ADDED_BG: Color = Color::Rgb(0, 90, 0);    // clear dark green
pub const REMOVED_BG: Color = Color::Rgb(110, 30, 30); // clear dark red

#[derive(Clone, Copy, Debug)]
pub struct Theme;

impl Default for Theme {
    fn default() -> Self {
        Theme
    }
}

impl Theme {
    pub fn normal(&self) -> Style {
        Style::default()
    }
    pub fn muted(&self) -> Style {
        Style::default().fg(MUTED)
    }
    pub fn border(&self) -> Style {
        Style::default().fg(MUTED)
    }
    pub fn title(&self) -> Style {
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD)
    }
    pub fn status_bar(&self) -> Style {
        Style::default().bg(STATUS_BG).fg(STATUS_FG)
    }
    pub fn status_title(&self) -> Style {
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD)
    }
    pub fn status_hint(&self) -> Style {
        Style::default().fg(Color::Gray)
    }
    pub fn selected(&self) -> Style {
        Style::default().bg(STATUS_BG).add_modifier(Modifier::BOLD)
    }
    pub fn snap_number(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }
    pub fn timestamp(&self) -> Style {
        Style::default().fg(MUTED)
    }
    pub fn added(&self) -> Style {
        Style::default().fg(Color::Green)
    }
    pub fn removed(&self) -> Style {
        Style::default().fg(Color::Red)
    }
    pub fn added_line_no(&self) -> Style {
        Style::default().fg(Color::LightGreen)
    }
    pub fn removed_line_no(&self) -> Style {
        Style::default().fg(Color::LightRed)
    }
    pub fn file_header(&self) -> Style {
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD)
    }
    pub fn added_bg(&self) -> Color {
        ADDED_BG
    }
    pub fn removed_bg(&self) -> Color {
        REMOVED_BG
    }
}

