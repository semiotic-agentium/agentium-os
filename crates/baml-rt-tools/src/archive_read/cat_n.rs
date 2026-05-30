// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! GNU `cat -n` style line number formatting.
//!
//! Right-justified number, tab separator, 6-char minimum width.
//! Uses original line positions from `LineWithPosition`, not sequential numbering.

use std::fmt::Write;

use super::types::LineWithPosition;

const MIN_WIDTH: usize = 6;

fn line_number_width(max_line: usize) -> usize {
    let mut n = max_line.max(1);
    let mut digits = 0;
    while n > 0 {
        n /= 10;
        digits += 1;
    }
    digits.max(MIN_WIDTH)
}

/// Format lines with `cat -n` style numbering using original positions.
///
/// Output: `{number}\t{content}\n` where number is right-justified.
/// Line numbers come from `LineWithPosition.original_line_number`, not
/// sequential — so grep output preserves the original positions.
pub fn format_cat_n(lines: &[LineWithPosition]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let max_num = lines
        .iter()
        .map(|l| l.original_line_number)
        .max()
        .unwrap_or(1);
    let width = line_number_width(max_num);

    let mut out = String::with_capacity(lines.len() * 80);
    for line in lines {
        let _ = writeln!(out, "{:>width$}\t{}", line.original_line_number, line.text);
    }
    out
}

/// Format lines with sequential numbering starting from `start_line`.
/// Used when there's no grep filtering and lines are contiguous.
pub fn format_cat_n_sequential(lines: &[&str], start_line: usize) -> String {
    let positioned: Vec<LineWithPosition> = lines
        .iter()
        .enumerate()
        .map(|(i, &text)| LineWithPosition {
            original_line_number: start_line + i,
            text: text.to_string(),
        })
        .collect();
    format_cat_n(&positioned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert_eq!(format_cat_n(&[]), "");
    }

    #[test]
    fn single_line() {
        let lines = vec![LineWithPosition {
            original_line_number: 1,
            text: "hello world".to_string(),
        }];
        assert_eq!(format_cat_n(&lines), "     1\thello world\n");
    }

    #[test]
    fn sequential_lines() {
        let lines = vec![
            LineWithPosition {
                original_line_number: 1,
                text: "first".to_string(),
            },
            LineWithPosition {
                original_line_number: 2,
                text: "second".to_string(),
            },
            LineWithPosition {
                original_line_number: 3,
                text: "third".to_string(),
            },
        ];
        assert_eq!(
            format_cat_n(&lines),
            "     1\tfirst\n     2\tsecond\n     3\tthird\n"
        );
    }

    #[test]
    fn non_sequential_preserves_originals() {
        let lines = vec![
            LineWithPosition {
                original_line_number: 3,
                text: "matched line A".to_string(),
            },
            LineWithPosition {
                original_line_number: 14,
                text: "matched line B".to_string(),
            },
            LineWithPosition {
                original_line_number: 31,
                text: "matched line C".to_string(),
            },
        ];
        let output = format_cat_n(&lines);
        assert!(output.contains("     3\tmatched line A"));
        assert!(output.contains("    14\tmatched line B"));
        assert!(output.contains("    31\tmatched line C"));
    }

    #[test]
    fn width_grows_for_large_numbers() {
        let lines = vec![LineWithPosition {
            original_line_number: 100_000,
            text: "deep".to_string(),
        }];
        assert_eq!(format_cat_n(&lines), "100000\tdeep\n");
    }

    #[test]
    fn seven_digit_number() {
        let lines = vec![LineWithPosition {
            original_line_number: 1_000_000,
            text: "very deep".to_string(),
        }];
        assert_eq!(format_cat_n(&lines), "1000000\tvery deep\n");
    }

    #[test]
    fn sequential_format() {
        let lines = vec!["alpha", "beta", "gamma"];
        assert_eq!(
            format_cat_n_sequential(&lines, 21),
            "    21\talpha\n    22\tbeta\n    23\tgamma\n"
        );
    }
}
