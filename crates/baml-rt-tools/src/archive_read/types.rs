//! Newtypes for the archive read interface.
//!
//! Parse boundaries: validation happens at deserialization, not at query time.

use serde::{Deserialize, Serialize};

/// Short archive ref (`@N`) from a previous tool result.
/// Monotonic per conversation context, allocated by `RefAllocator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortRef(u32);

impl ShortRef {
    pub fn new(n: u32) -> Self {
        Self(n)
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.strip_prefix('@')?.parse::<u32>().ok().map(Self)
    }

    /// Parse `@N`, or the last `@N` suffix (e.g. episode-prefixed `abcd@8` in display strings).
    pub fn parse_loose(s: &str) -> Option<Self> {
        Self::parse(s.trim()).or_else(|| {
            let t = s.trim();
            let i = t.rfind('@')?;
            Self::parse(&t[i..])
        })
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for ShortRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}

impl Serialize for ShortRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ShortRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        ShortRef::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid archive ref: '{s}' (expected @N)"))
        })
    }
}

/// History ref (`#N`) for a message or tool-call description in the conversation.
/// Monotonic per conversation context, sharing the same `RefTable` counter as `ShortRef`.
/// Citation-only: cannot be Read. Used in `citations` fields on BAML wrapper types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HistoryRef(u32);

impl HistoryRef {
    pub fn new(n: u32) -> Self {
        Self(n)
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.strip_prefix('#')?.parse::<u32>().ok().map(Self)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for HistoryRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl Serialize for HistoryRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HistoryRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        HistoryRef::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid history ref: '{s}' (expected #N)"))
        })
    }
}

/// Line offset into rendered content. 0-based.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LineOffset(pub usize);

/// Maximum lines per page.
#[derive(Debug, Clone, Copy)]
pub struct PageLimit(usize);

impl PageLimit {
    /// Default when the LLM omits limit — large enough to fit typical tool results
    /// without forcing pagination, but bounded to keep prompts manageable.
    pub const DEFAULT: usize = 200;
    pub const MAX: usize = 500;

    pub fn new(n: usize) -> Self {
        Self(n.min(Self::MAX))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl Serialize for PageLimit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0 as u64)
    }
}

impl<'de> Deserialize<'de> for PageLimit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = usize::deserialize(deserializer)?;
        Ok(Self::new(n))
    }
}

/// Grep pattern with optional CLI-style flag prefix.
///
/// Parses `-F` (fixed string, default), `-E` (extended regex), `-i` (case
/// insensitive), combinable (`-iE`). The BAML schema sees a single `string?`
/// field; the flag decomposition is an implementation detail.
///
/// Examples: `"deploy"`, `"-i deploy"`, `"-E error|warn"`, `"-iE \\bauth\\b"`
#[derive(Debug, Clone)]
pub struct GrepPattern {
    pattern: String,
    /// Pre-lowercased pattern for the fixed+case-insensitive path — avoids
    /// one allocation per `matches()` call.
    pattern_lower: Option<String>,
    mode: GrepMode,
    case_insensitive: bool,
    compiled_regex: Option<regex::Regex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrepMode {
    Fixed,
    Regex,
}

impl GrepPattern {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("grep pattern is empty".to_string());
        }

        let (flags, pattern) = if input.starts_with('-') && input.len() > 1 {
            let space_pos = input.find(' ').unwrap_or(input.len());
            let flag_part = &input[1..space_pos];
            if flag_part.chars().all(|c| matches!(c, 'F' | 'E' | 'i')) {
                let pat = input[space_pos..].trim();
                if pat.is_empty() {
                    return Err("grep pattern is empty after flags".to_string());
                }
                (flag_part, pat)
            } else {
                ("", input)
            }
        } else {
            ("", input)
        };

        let mode = if flags.contains('E') {
            GrepMode::Regex
        } else {
            GrepMode::Fixed
        };
        let case_insensitive = flags.contains('i');

        let compiled_regex = if mode == GrepMode::Regex {
            let re_pattern = if case_insensitive {
                format!("(?i){pattern}")
            } else {
                pattern.to_string()
            };
            Some(regex::Regex::new(&re_pattern).map_err(|e| format!("invalid regex: {e}"))?)
        } else {
            None
        };

        let pattern_lower = if mode == GrepMode::Fixed && case_insensitive {
            Some(pattern.to_lowercase())
        } else {
            None
        };

        Ok(Self {
            pattern: pattern.to_string(),
            pattern_lower,
            mode,
            case_insensitive,
            compiled_regex,
        })
    }

    pub fn matches(&self, haystack: &str) -> bool {
        match (&self.mode, &self.compiled_regex) {
            (GrepMode::Regex, Some(re)) => re.is_match(haystack),
            _ if self.case_insensitive => haystack
                .to_lowercase()
                .contains(self.pattern_lower.as_deref().unwrap_or("")),
            _ => haystack.contains(self.pattern.as_str()),
        }
    }

    pub fn pattern_text(&self) -> &str {
        &self.pattern
    }
}

/// A line with its original position in the full rendered content.
#[derive(Debug, Clone)]
pub struct LineWithPosition {
    /// 1-based position in the full `RenderedContent`.
    pub original_line_number: usize,
    pub text: String,
}

/// Result of grep + paginate.
#[derive(Debug, Clone)]
pub struct GrepPage {
    pub lines: Vec<LineWithPosition>,
    pub total_matched: usize,
    pub has_more: bool,
    /// Offset to pass on the next Read call to get the following page.
    /// Equal to the input offset + lines returned. Zero when `has_more` is false.
    pub next_offset: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ref_parse() {
        assert_eq!(ShortRef::parse("@3").unwrap().as_u32(), 3);
        assert_eq!(ShortRef::parse("@0").unwrap().as_u32(), 0);
        assert!(ShortRef::parse("3").is_none());
        assert!(ShortRef::parse("@").is_none());
        assert!(ShortRef::parse("@abc").is_none());
        assert!(ShortRef::parse("").is_none());
    }

    #[test]
    fn short_ref_display() {
        assert_eq!(ShortRef::new(42).to_string(), "@42");
    }

    #[test]
    fn history_ref_parse() {
        assert_eq!(HistoryRef::parse("#1").unwrap().as_u32(), 1);
        assert_eq!(HistoryRef::parse("#42").unwrap().as_u32(), 42);
        assert!(HistoryRef::parse("1").is_none());
        assert!(HistoryRef::parse("#").is_none());
        assert!(HistoryRef::parse("#abc").is_none());
        assert!(HistoryRef::parse("").is_none());
    }

    #[test]
    fn history_ref_display() {
        assert_eq!(HistoryRef::new(7).to_string(), "#7");
    }

    #[test]
    fn history_ref_no_cross_parse_with_short_ref() {
        assert!(HistoryRef::parse("@3").is_none());
        assert!(ShortRef::parse("#3").is_none());
    }

    #[test]
    fn page_limit_clamps() {
        assert_eq!(PageLimit::new(1000).get(), 500);
        assert_eq!(PageLimit::new(50).get(), 50);
        assert_eq!(PageLimit::default().get(), 200);
    }

    #[test]
    fn grep_fixed_default() {
        let g = GrepPattern::parse("deploy").unwrap();
        assert!(g.matches("the deploy pipeline"));
        assert!(!g.matches("the DEPLOY pipeline"));
    }

    #[test]
    fn grep_case_insensitive() {
        let g = GrepPattern::parse("-i deploy").unwrap();
        assert!(g.matches("the DEPLOY pipeline"));
        assert!(g.matches("deploy"));
    }

    #[test]
    fn grep_regex() {
        let g = GrepPattern::parse("-E error|warn").unwrap();
        assert!(g.matches("this is an error"));
        assert!(g.matches("this is a warning"));
        assert!(!g.matches("this is fine"));
    }

    #[test]
    fn grep_regex_case_insensitive() {
        let g = GrepPattern::parse("-iE error|warn").unwrap();
        assert!(g.matches("ERROR occurred"));
        assert!(g.matches("Warning: low disk"));
    }

    #[test]
    fn grep_explicit_fixed() {
        let g = GrepPattern::parse("-F deploy.*pipeline").unwrap();
        assert!(g.matches("the deploy.*pipeline thing"));
        assert!(!g.matches("the deploy-pipeline thing"));
    }

    #[test]
    fn grep_dash_in_pattern_not_flag() {
        let g = GrepPattern::parse("-something").unwrap();
        assert_eq!(g.pattern_text(), "-something");
    }

    #[test]
    fn grep_empty_rejected() {
        assert!(GrepPattern::parse("").is_err());
        assert!(GrepPattern::parse("-E ").is_err());
    }

    #[test]
    fn grep_invalid_regex() {
        assert!(GrepPattern::parse("-E [invalid").is_err());
    }
}
