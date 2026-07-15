//! Syntax highlighting + diff-overlay rendering.
//!
//! Per the plan (`07-tui-design.md`):
//!   - Foreground (text color) comes from `syntect`'s parsed runs.
//!   - Background (full row tint) comes from the diff overlay — the
//!     `Line`'s `Style.bg` covers the row, and the `Span`s' syntax `fg`
//!     stays readable on top.
//!   - The two color systems don't fight because ratatui's `Style` carries
//!     `fg` and `bg` independently.
//!
//! `render_snap` is op-driven: it walks `similar::TextDiff` ops over
//! `prev_content` and `content` and emits a unified-diff row stream —
//! removed lines (red, with their actual prior text) interleaved with
//! added lines (green). Each row's gutter shows `+ N`, `- M`, or `  N`
//! matching git-diff conventions.

use crate::tui::theme::{ADDED_BG, MUTED, REMOVED_BG};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

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

/// Render a snap as a unified-diff row stream.
///
/// `prev_content` is the *previous* snap's content (or `""` for the first
/// snap of a file). The function walks `TextDiff::from_lines(prev, cur)`
/// and emits, in order:
///
/// - `Equal`: one row per line in the new range, plain background, gutter
///   `  N` (1-based in `content`).
/// - `Insert`: one row per line in the new range, ADDED_BG, gutter
///   `+ N` (1-based in `content`).
/// - `Delete`: one row per line in the *old* range, REMOVED_BG, gutter
///   `- M` (1-based in `prev_content`), and the **actual text** of the
///   removed line.
/// - `Replace`: emits the old lines first (red), then the new lines
///   (green).
///
/// When `with_line_numbers` is true, every row is prefixed with the gutter
/// (right-aligned to the wider of the two files' max line counts) and a
/// `│` separator.
#[allow(clippy::too_many_arguments)]
pub fn render_snap(
    engine: &mut HighlightEngine,
    prev_content: &str,
    content: &str,
    file_path: &str,
    with_line_numbers: bool,
) -> Vec<Line<'static>> {
    let syntax = engine.syntax_for(file_path);
    let diff = TextDiff::from_lines(prev_content, content);
    let prev_line_count = prev_content.lines().count();
    let cur_line_count = content.lines().count();
    let gutter_width = if with_line_numbers {
        prev_line_count.max(cur_line_count).max(1).to_string().len()
    } else {
        0
    };
    let gutter_style = Style::default().fg(MUTED);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for change in diff.iter_all_changes() {
        let tag = change.tag();
        // The actual text of the change, stripped of the trailing newline
        // (TextDiff includes the newline as part of the value).
        let text = change.value().trim_end_matches('\n');
        let (sign, line_no, bg, marker_fg) = match tag {
            ChangeTag::Equal => (" ", change.new_index().map(|i| i + 1), None, None),
            ChangeTag::Insert => (
                "+",
                change.new_index().map(|i| i + 1),
                Some(ADDED_BG),
                Some(Color::LightGreen),
            ),
            ChangeTag::Delete => (
                "-",
                change.old_index().map(|i| i + 1),
                Some(REMOVED_BG),
                Some(Color::LightRed),
            ),
        };
        // Highlight with syntax; the bg tint is applied at the line level
        // so it covers the full row.
        let spans: Vec<Span<'static>> = {
            let mut s = Vec::new();
            if with_line_numbers {
                let n = line_no.unwrap_or(0);
                let gutter = format!("{sign} {n:>gutter_width$} │ ");
                let style = match (bg, marker_fg) {
                    (Some(_), Some(mfg)) => Style::default().fg(mfg).add_modifier(Modifier::BOLD),
                    (Some(_), None) => Style::default().fg(MUTED),
                    (None, _) => gutter_style,
                };
                s.push(Span::styled(gutter, style));
            }
            s.extend(engine.highlight_line(text, syntax.as_ref()));
            s
        };
        let line = if let Some(bg) = bg {
            Line::from(spans).style(Style::default().bg(bg))
        } else {
            Line::from(spans)
        };
        lines.push(line);
    }

    if with_line_numbers && !gutter_width_known_to_match(gutter_width, &lines) {
        // no-op; the gutter width was computed up front.
    }
    lines
}

fn gutter_width_known_to_match(_width: usize, _lines: &[Line<'_>]) -> bool {
    // Reserved for future use; the gutter width is set up front and applied
    // to every row in `render_snap`. Kept as a function for tests below.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn render_snap_marks_added_lines_with_green_bg() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "alpha\nbeta\n";
        let content = "alpha\nbeta\ngamma\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        // 3 rows: 2 equal, 1 insert.
        assert_eq!(lines.len(), 3);
        // The third row (gamma) is the new line: ADDED_BG.
        assert_eq!(lines[2].style.bg, Some(ADDED_BG));
        // The first two rows are equal: no bg.
        assert_ne!(lines[0].style.bg, Some(ADDED_BG));
        assert_ne!(lines[1].style.bg, Some(ADDED_BG));
    }

    #[test]
    fn render_snap_marks_removed_lines_with_red_bg_and_shows_text() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "alpha\nbeta\ngamma\n";
        let content = "alpha\ngamma\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        // Op stream: Equal(alpha), Delete(beta), Equal(gamma).
        assert_eq!(lines.len(), 3);
        // Row 1 (beta) is removed: REMOVED_BG, and the prior text is in the spans.
        assert_eq!(lines[1].style.bg, Some(REMOVED_BG));
        let text: String = lines[1].spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("beta"), "removed row should show the prior text, got {text:?}");
    }

    #[test]
    fn render_snap_replace_emits_old_then_new() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "a\nb\nc\nd\n";
        let content = "a\nX\nY\nd\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        // Op stream: Equal(a), Delete(b), Delete(c), Insert(X), Insert(Y), Equal(d).
        assert_eq!(lines.len(), 6);
        // Rows 1, 2 are the old (red) with prior text "b", "c".
        for (i, expected) in [(1usize, "b"), (2usize, "c")] {
            assert_eq!(lines[i].style.bg, Some(REMOVED_BG));
            let text: String = lines[i].spans.iter().map(|s| s.content.to_string()).collect();
            assert!(text.contains(expected), "row {i} should contain {expected:?}, got {text:?}");
        }
        // Rows 3, 4 are the new (green) with text "X", "Y".
        for (i, expected) in [(3usize, "X"), (4usize, "Y")] {
            assert_eq!(lines[i].style.bg, Some(ADDED_BG));
            let text: String = lines[i].spans.iter().map(|s| s.content.to_string()).collect();
            assert!(text.contains(expected), "row {i} should contain {expected:?}, got {text:?}");
        }
    }

    #[test]
    fn render_snap_no_change_is_all_equal() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "a\nb\n";
        let content = "a\nb\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert!(l.style.bg.is_none() || l.style.bg == Some(Color::Reset));
        }
    }

    #[test]
    fn render_snap_first_snap_of_file_renders_all_added() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let lines = render_snap(&mut engine, "", "a\nb\nc\n", "a.txt", true);
        // All three are inserts.
        assert_eq!(lines.len(), 3);
        for l in &lines {
            assert_eq!(l.style.bg, Some(ADDED_BG));
        }
    }

    #[test]
    fn render_snap_gutter_uses_plus_minus_space_signs() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "a\nb\n";
        let content = "a\nc\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        // Equal(a), Delete(b), Insert(c).
        let collect = |i: usize| -> String {
            lines[i].spans.iter().map(|s| s.content.to_string()).collect()
        };
        assert!(collect(0).contains("  1 │"), "equal row should have '  N │' gutter, got {:?}", collect(0));
        assert!(collect(1).contains("- 2 │"), "delete row should have '- M │' gutter, got {:?}", collect(1));
        assert!(collect(2).contains("+ 2 │"), "insert row should have '+ N │' gutter, got {:?}", collect(2));
    }

    #[test]
    fn render_snap_omits_gutter_when_disabled() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let lines = render_snap(&mut engine, "", "a\nb\n", "a.txt", false);
        assert_eq!(lines.len(), 2);
        for (i, l) in lines.iter().enumerate() {
            let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
            let expected = ["a", "b"][i];
            assert_eq!(text, expected);
        }
    }

    // Verify that any pre-computed `added_lines`/`removed_lines`
    // metadata is *not* consulted at render time — render_snap only
    // uses prev_content + content via `similar`.
    #[test]
    fn render_snap_derives_diff_from_content() {
        let mut engine = HighlightEngine::new("base16-eighties.dark");
        let prev = "x\ny\n";
        let content = "x\ny\nz\n";
        let lines = render_snap(&mut engine, prev, content, "a.txt", true);
        // 3 rows: 2 equal, 1 insert at the actual position (line 3).
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].style.bg, Some(ADDED_BG));
        // The first two rows are equal: no added bg.
        assert_ne!(lines[0].style.bg, Some(ADDED_BG));
        assert_ne!(lines[1].style.bg, Some(ADDED_BG));
    }
}
