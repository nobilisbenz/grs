//! Theme / styles for the TUI.

use ratatui::style::{Color, Modifier, Style};

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
        Style::default().fg(Color::DarkGray)
    }
    pub fn border(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }
    pub fn title(&self) -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
    pub fn status_bar(&self) -> Style {
        Style::default().bg(Color::DarkGray)
    }
    pub fn status_title(&self) -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
    pub fn status_hint(&self) -> Style {
        Style::default().fg(Color::Gray)
    }
    pub fn selected(&self) -> Style {
        Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
    }
    pub fn snap_number(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }
    pub fn timestamp(&self) -> Style {
        Style::default().fg(Color::DarkGray)
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
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
}
