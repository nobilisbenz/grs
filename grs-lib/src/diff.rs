//! Line-level diff computation. Pure and tiny so it's trivially unit-testable.
//!
//! Uses `similar::TextDiff::from_lines` and inspects each `DiffOp`'s
//! old/new ranges to derive 1-based line numbers of added/removed lines vs
//! the previous content. The diff is computed once at snap time and stored
//! in the snap JSON, so replay never diffs on the hot path (see `plan/02`).

use crate::model::LineDiff;
use similar::{DiffTag, TextDiff};

/// Compute the line-level diff of `cur` against `prev`.
///
/// - `added_lines`: 1-based line numbers in `cur` that are newly added.
/// - `removed_lines`: 1-based line numbers in `prev` that no longer exist.
///
/// When `prev` is empty, every line of `cur` is "added" (first snap of a
/// file). When both are identical, both vectors are empty.
pub fn line_diff(prev: &str, cur: &str) -> LineDiff {
    let diff = TextDiff::from_lines(prev, cur);
    let mut added_lines = Vec::new();
    let mut removed_lines = Vec::new();

    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Insert => {
                for i in op.new_range().start..op.new_range().end {
                    added_lines.push(i + 1);
                }
            }
            DiffTag::Delete => {
                for i in op.old_range().start..op.old_range().end {
                    removed_lines.push(i + 1);
                }
            }
            DiffTag::Replace => {
                for i in op.new_range().start..op.new_range().end {
                    added_lines.push(i + 1);
                }
                for i in op.old_range().start..op.old_range().end {
                    removed_lines.push(i + 1);
                }
            }
        }
    }

    LineDiff {
        added_lines,
        removed_lines,
        prev_seq: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change() {
        let d = line_diff("a\nb\nc\n", "a\nb\nc\n");
        assert!(d.added_lines.is_empty());
        assert!(d.removed_lines.is_empty());
    }

    #[test]
    fn append_lines() {
        let d = line_diff("a\nb\n", "a\nb\nc\nd\n");
        assert_eq!(d.added_lines, vec![3, 4]);
        assert!(d.removed_lines.is_empty());
    }

    #[test]
    fn delete_lines() {
        let d = line_diff("a\nb\nc\nd\n", "a\nd\n");
        assert!(d.added_lines.is_empty());
        assert_eq!(d.removed_lines, vec![2, 3]);
    }

    #[test]
    fn insert_in_middle() {
        let d = line_diff("a\nb\nc\n", "a\nX\nb\nc\n");
        assert_eq!(d.added_lines, vec![2]);
        assert!(d.removed_lines.is_empty());
    }

    #[test]
    fn replace_block() {
        // "b\nc" -> "X\nY"
        let d = line_diff("a\nb\nc\nd\n", "a\nX\nY\nd\n");
        assert_eq!(d.added_lines, vec![2, 3]);
        assert_eq!(d.removed_lines, vec![2, 3]);
    }

    #[test]
    fn first_snap_all_added() {
        let d = line_diff("", "a\nb\nc\n");
        assert_eq!(d.added_lines, vec![1, 2, 3]);
        assert!(d.removed_lines.is_empty());
    }

    #[test]
    fn empty_to_empty() {
        let d = line_diff("", "");
        assert!(d.added_lines.is_empty());
        assert!(d.removed_lines.is_empty());
    }

    #[test]
    fn trailing_newline_replaces_last_line() {
        // `similar::from_lines` treats "b" and "b\n" as different lines, so
        // adding a trailing newline is a replace of the last line, not an
        // append.
        let d = line_diff("a\nb", "a\nb\n");
        assert_eq!(d.added_lines, vec![2]);
        assert_eq!(d.removed_lines, vec![2]);
    }
}
