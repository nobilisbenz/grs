//! Syntax highlighting + diff-overlay rendering.
//!
//! Per the plan (`07-tui-design.md`):
//!   - Foreground (text color) comes from `syntect`'s parsed runs.
//!   - Background (full row tint) comes from the diff overlay — the
//!     `Line`'s `Style.bg` covers the row, and the `Span`s' syntax `fg`
//!     stays readable on top.
//!   - The two color systems don't fight because ratatui's `Style` carries
//!     `fg` and `bg` independently.

use crate::tui::theme::{ADDED_BG, MUTED, REMOVED_BG};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Loaded once into `App.highlight`; reused for every snap.
pub struct HighlightEngine {
    pub syntax_set: SyntaxSet,
    pub theme_set: ThemeSet,
    pub theme: Theme,
    pub theme_name: String,
    /// Cache: file extension -> SyntaxReference. `None` means we tried and
    /// didn't find a matching syntax (so we just render plain).
    syntax_cache: HashMap<String, Option<SyntaxReference>>,
}

impl HighlightEngine {
    pub fn new(theme_name: &str) -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set
            .themes
            .get(theme_name)
            .cloned()
            .or_else(|| theme_set.themes.get("base16-eighties.dark").cloned())
            .or_else(|| theme_set.themes.values().next().cloned())
            .expect("syntect theme set has at least one theme");
        let resolved_name = theme.name.clone().unwrap_or_else(|| theme_name.to_string());
        Self {
            syntax_set,
            theme_set,
            theme,
            theme_name: resolved_name,
            syntax_cache: HashMap::new(),
        }
    }

    /// Find a syntax for `path` (by extension), caching the result.
    pub fn syntax_for(&mut self, path: &str) -> Option<SyntaxReference> {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if let Some(cached) = self.syntax_cache.get(&ext) {
            return cached.clone();
        }
        let found = self
            .syntax_set
            .find_syntax_by_extension(&ext)
            .or_else(|| self.syntax_set.find_syntax_by_first_line(path))
            .cloned();
        self.syntax_cache.insert(ext, found.clone());
        found
    }

    /// Convert a single line of text into a `Vec<Span>` with syntax foreground
    /// colors applied. If no syntax is found, returns a single plain span.
    pub fn highlight_line(
        &self,
        line: &str,
        syntax: Option<&SyntaxReference>,
    ) -> Vec<Span<'static>> {
        let Some(syn) = syntax else {
            return vec![Span::raw(line.to_string())];
        };
        let mut highlighter = HighlightLines::new(syn, &self.theme);
        let ranges = highlighter
            .highlight_line(line, &self.syntax_set)
            .unwrap_or_default();
        ranges
            .into_iter()
            .map(|(style, text)| {
                let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                Span::styled(text.to_string(), Style::default().fg(fg))
            })
            .collect()
    }
}

/// Render a snap's full content into a `Vec<Line>` ready to hand to a
/// `Paragraph`. Adds the diff background to the lines listed in
/// `added_lines` (1-based) and `removed_lines` (1-based, shown as a
/// placeholder marker line). When `with_line_numbers` is true, each line
/// is prefixed with a right-aligned line number gutter (e.g. `  1 │ code`).
pub fn render_snap(
    engine: &mut HighlightEngine,
    content: &str,
    file_path: &str,
    added_lines: &[usize],
    removed_lines: &[usize],
    with_line_numbers: bool,
) -> Vec<Line<'static>> {
    let syntax = engine.syntax_for(file_path);
    let content_lines: Vec<&str> = LinesWithEndings::from(content).collect();
    let total = content_lines.len();
    let gutter_width = if with_line_numbers {
        total.max(1).to_string().len()
    } else {
        0
    };
    let gutter_style = Style::default().fg(MUTED);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, raw) in content_lines.iter().enumerate() {
        let line_no = i + 1;
        let text = raw.trim_end_matches('\n');
        let spans = engine.highlight_line(text, syntax.as_ref());
        let bg = if added_lines.contains(&line_no) {
            Some(ADDED_BG)
        } else {
            None
        };
        let mut all_spans: Vec<Span<'static>> = Vec::new();
        if with_line_numbers {
            let gutter = format!("{line_no:>gutter_width$} │ ");
            all_spans.push(Span::styled(gutter, gutter_style));
        }
        all_spans.extend(spans);
        let line = if let Some(bg) = bg {
            Line::from(all_spans).style(Style::default().bg(bg))
        } else {
            Line::from(all_spans)
        };
        lines.push(line);
    }
    if !removed_lines.is_empty() {
        let marker = format!(
            "[− {} line(s) removed: {:?}]",
            removed_lines.len(),
            removed_lines
        );
        if with_line_numbers {
            let gutter = format!("{empty:>gutter_width$} │ ", empty = "");
            let mut spans = vec![Span::styled(gutter, gutter_style)];
            spans.push(Span::styled(marker, Style::default().bg(REMOVED_BG)));
            lines.push(Line::from(spans).style(Style::default().bg(REMOVED_BG)));
        } else {
            lines.push(
                Line::from(Span::styled(marker, Style::default().bg(REMOVED_BG)))
                    .style(Style::default().bg(REMOVED_BG)),
            );
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use grs_lib::model::LineDiff;

    #[test]
    fn render_snap_adds_bg_to_added_lines() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let content = "alpha\nbeta\ngamma\n";
        let diff = LineDiff {
            added_lines: vec![2],
            removed_lines: vec![],
            prev_seq: None,
        };
        let lines = render_snap(&mut engine, content, "a.txt", &diff.added_lines, &diff.removed_lines, true);
        assert_eq!(lines.len(), 3);
        // Line 2 (beta) should have ADDED_BG; lines 1 and 3 should not.
        assert_eq!(lines[1].style.bg, Some(ADDED_BG));
        assert_ne!(lines[0].style.bg, Some(ADDED_BG));
        assert_ne!(lines[2].style.bg, Some(ADDED_BG));
    }

    #[test]
    fn render_snap_removed_lines_appends_marker() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let content = "alpha\nbeta\n";
        let diff = LineDiff {
            added_lines: vec![],
            removed_lines: vec![1],
            prev_seq: None,
        };
        let lines = render_snap(&mut engine, content, "a.txt", &diff.added_lines, &diff.removed_lines, true);
        assert_eq!(lines.len(), 3); // 2 content + 1 marker
        let marker_text: String = lines[2]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(marker_text.contains("removed"));
    }

    #[test]
    fn empty_content_renders_empty() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let lines = render_snap(&mut engine, "", "a.txt", &[], &[], true);
        assert!(lines.is_empty());
    }

    #[test]
    fn render_snap_adds_line_numbers_when_enabled() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let content = "alpha\nbeta\ngamma\n";
        let lines = render_snap(&mut engine, content, "a.txt", &[], &[], true);
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let text: String = line
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect();
            let expected = format!("{} │ ", i + 1);
            assert!(
                text.starts_with(&expected),
                "line {} should start with {:?}, got {:?}",
                i + 1,
                expected,
                text
            );
        }
    }

    #[test]
    fn render_snap_omits_line_numbers_when_disabled() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let content = "alpha\nbeta\n";
        let lines = render_snap(&mut engine, content, "a.txt", &[], &[], false);
        assert_eq!(lines.len(), 2);
        for (i, line) in lines.iter().enumerate() {
            let text: String = line
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect();
            let content_text = ["alpha", "beta"][i];
            assert_eq!(text, content_text);
        }
    }
}
