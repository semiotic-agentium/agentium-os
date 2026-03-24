//! Single-buffer rendered content with line index.
//!
//! One heap allocation for the entire rendered text. Line boundaries
//! tracked as byte offsets for zero-copy iteration.

/// Rendered text content with efficient line access.
///
/// Stores the full rendered text in a single `String` buffer with a
/// line-end index for O(1) line count and zero-copy line iteration.
#[derive(Debug, Clone)]
pub struct RenderedContent {
    buf: String,
    /// Byte offsets of each `\n` in `buf`. Length == line count.
    line_ends: Vec<usize>,
}

impl RenderedContent {
    /// Build from a list of lines. Each line must not contain embedded newlines.
    pub fn from_lines(lines: impl IntoIterator<Item = String>) -> Self {
        let mut buf = String::new();
        let mut line_ends = Vec::new();
        for line in lines {
            debug_assert!(
                !line.contains('\n'),
                "RenderedContent lines must not contain embedded newlines"
            );
            buf.push_str(&line);
            buf.push('\n');
            line_ends.push(buf.len());
        }
        Self { buf, line_ends }
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.line_ends.len()
    }

    /// Total byte size of the rendered content.
    pub fn byte_count(&self) -> usize {
        self.buf.len()
    }

    /// Iterate lines (without trailing newline).
    pub fn lines(&self) -> impl Iterator<Item = &str> + '_ {
        let mut start = 0;
        self.line_ends.iter().map(move |&end| {
            let line = &self.buf[start..end - 1]; // exclude trailing \n
            start = end;
            line
        })
    }

    /// Get a specific line by 0-based index.
    pub fn get_line(&self, index: usize) -> Option<&str> {
        if index >= self.line_ends.len() {
            return None;
        }
        let start = if index == 0 {
            0
        } else {
            self.line_ends[index - 1]
        };
        let end = self.line_ends[index] - 1; // exclude \n
        Some(&self.buf[start..end])
    }

    /// True if no content.
    pub fn is_empty(&self) -> bool {
        self.line_ends.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let rc = RenderedContent::from_lines(std::iter::empty::<String>());
        assert!(rc.is_empty());
        assert_eq!(rc.line_count(), 0);
        assert_eq!(rc.byte_count(), 0);
        assert_eq!(rc.lines().count(), 0);
    }

    #[test]
    fn single_line() {
        let rc = RenderedContent::from_lines(vec!["hello world".to_string()]);
        assert_eq!(rc.line_count(), 1);
        assert_eq!(rc.lines().collect::<Vec<_>>(), vec!["hello world"]);
        assert_eq!(rc.get_line(0), Some("hello world"));
        assert_eq!(rc.get_line(1), None);
    }

    #[test]
    fn multiple_lines() {
        let rc = RenderedContent::from_lines(vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ]);
        assert_eq!(rc.line_count(), 3);
        let lines: Vec<&str> = rc.lines().collect();
        assert_eq!(lines, vec!["first", "second", "third"]);
        assert_eq!(rc.get_line(0), Some("first"));
        assert_eq!(rc.get_line(1), Some("second"));
        assert_eq!(rc.get_line(2), Some("third"));
    }

    #[test]
    fn byte_count_includes_newlines() {
        let rc = RenderedContent::from_lines(vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!(rc.byte_count(), 6); // "ab\ncd\n"
    }
}
