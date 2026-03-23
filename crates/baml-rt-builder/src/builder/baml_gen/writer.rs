//! Small line-oriented buffer for emitting BAML source.
//!
//! This keeps formatting consistent (`\n` only, no `writeln!` `Result` noise for the happy path).

/// Incrementally builds BAML text with guaranteed trailing newlines on logical lines.
#[derive(Default)]
pub struct BamlWriter(String);

impl BamlWriter {
    pub fn new() -> Self {
        Self(String::new())
    }

    /// One logical line (newline appended).
    #[inline]
    pub fn line(&mut self, s: impl AsRef<str>) {
        self.0.push_str(s.as_ref());
        self.0.push('\n');
    }

    #[inline]
    pub fn blank(&mut self) {
        self.0.push('\n');
    }

    /// Raw fragment (no automatic newline). Prefer [`Self::line`] for whole lines.
    #[inline]
    pub fn push_str(&mut self, s: &str) {
        self.0.push_str(s);
    }

    /// Append a multi-line block; ensures exactly one trailing newline at EOF.
    pub fn push_block(&mut self, block: &str) {
        self.0.push_str(block);
        if !block.ends_with('\n') {
            self.0.push('\n');
        }
    }

    #[inline]
    pub fn into_string(self) -> String {
        self.0
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[inline]
    pub fn as_mut_string(&mut self) -> &mut String {
        &mut self.0
    }
}

impl From<BamlWriter> for String {
    fn from(w: BamlWriter) -> String {
        w.into_string()
    }
}
