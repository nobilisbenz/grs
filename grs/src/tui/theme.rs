//! TUI theme: colors for syntax, diff overlay, and chrome.

use ratatui::style::Color;

/// Diff overlay backgrounds — bg tint only, syntax foreground stays readable.
pub const ADDED_BG: Color = Color::Rgb(28, 68, 38);
pub const REMOVED_BG: Color = Color::Rgb(72, 28, 28);
pub const SCRUBBER_BG: Color = Color::Rgb(30, 30, 45);
pub const STATUS_BG: Color = Color::Rgb(20, 20, 30);
pub const STATUS_FG: Color = Color::Rgb(200, 200, 200);
pub const ACCENT: Color = Color::Rgb(100, 200, 255);
pub const MUTED: Color = Color::Rgb(100, 100, 120);
