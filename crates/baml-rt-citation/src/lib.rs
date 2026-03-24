//! Ref-table **citation** types: validated wire strings (`#N`, `@N`, …) and their parsed form.
//!
//! Use [`Citation`] at API and event boundaries so invalid ref strings are rejected at
//! deserialization / construction time. Use [`ParsedCitation`] for structured access
//! (`n`, `negated`, history vs archive, line ranges).

use std::{ops::RangeInclusive, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Failure to construct a [`Citation`] or parse a ref string into [`ParsedCitation`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CitationParseError(pub String);

impl CitationParseError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// A **validated** ref-table citation string exactly as emitted by the model / shim (`#1`, `@4:2`, …).
///
/// Invariant: [`Citation::as_str`] always parses successfully as [`ParsedCitation`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Citation(String);

impl Citation {
    /// Validate `s` and wrap. Trims surrounding ASCII whitespace.
    pub fn try_new(s: impl Into<String>) -> Result<Self, CitationParseError> {
        let s = s.into().trim().to_string();
        ParsedCitation::parse(&s)?;
        Ok(Self(s))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Structured view (cheap re-parse; invariant guarantees success).
    #[must_use]
    pub fn parsed(&self) -> ParsedCitation {
        ParsedCitation::parse(&self.0).expect("Citation invariant violated")
    }
}

impl std::fmt::Display for Citation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for Citation {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Citation {
    type Err = CitationParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Citation::try_new(s.to_string())
    }
}

impl TryFrom<String> for Citation {
    type Error = CitationParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Citation::try_new(value)
    }
}

impl TryFrom<&str> for Citation {
    type Error = CitationParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Citation::try_new(value.to_string())
    }
}

impl Serialize for Citation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Citation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Citation::try_new(s).map_err(serde::de::Error::custom)
    }
}

/// A citation parsed from a string in a `citations` field on a BAML wrapper type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCitation {
    /// `#N` or `!#N` — refers to a message or tool-call description entry in the `RefTable`.
    History {
        n: u32,
        /// `true` when the `!` prefix is present: this entry *contradicts* the decision.
        negated: bool,
    },
    /// `@N`, `@N:L`, `@N:L1-L2` (and their `!`-prefixed negation forms).
    Archive {
        n: u32,
        /// Line range (1-based, inclusive). `None` = entire archive content.
        lines: Option<RangeInclusive<usize>>,
        /// `true` when the `!` prefix is present: this archive entry *contradicts* the decision.
        negated: bool,
    },
}

impl ParsedCitation {
    /// Parse a citation string. Returns `Err` on invalid format.
    pub fn parse(s: &str) -> Result<Self, CitationParseError> {
        let (negated, body) = if let Some(inner) = s.strip_prefix('!') {
            (true, inner)
        } else {
            (false, s)
        };

        if let Some(rest) = body.strip_prefix('#') {
            let n = rest
                .parse::<u32>()
                .map_err(|_| CitationParseError::invalid(format!("invalid history ref: '{s}'")))?;
            return Ok(Self::History { n, negated });
        }

        if let Some(rest) = body.strip_prefix('@') {
            if let Some((ref_part, line_part)) = rest.split_once(':') {
                let n = ref_part.parse::<u32>().map_err(|_| {
                    CitationParseError::invalid(format!("invalid archive ref number in: '{s}'"))
                })?;
                let lines = parse_line_range(line_part).map_err(|e| {
                    CitationParseError::invalid(format!("invalid line range in '{s}': {e}"))
                })?;
                return Ok(Self::Archive {
                    n,
                    lines: Some(lines),
                    negated,
                });
            }
            let n = rest
                .parse::<u32>()
                .map_err(|_| CitationParseError::invalid(format!("invalid archive ref: '{s}'")))?;
            return Ok(Self::Archive {
                n,
                lines: None,
                negated,
            });
        }

        Err(CitationParseError::invalid(format!(
            "citation must start with '#' or '!#' (history) or '@' or '!@' (archive); got: '{s}'"
        )))
    }

    /// The raw ref number (the `N` in `#N` or `@N`).
    #[must_use]
    pub fn n(&self) -> u32 {
        match self {
            Self::History { n, .. } | Self::Archive { n, .. } => *n,
        }
    }

    /// Whether this citation refers to the history map (`#N`) rather than the archive map (`@N`).
    #[must_use]
    pub fn is_history(&self) -> bool {
        matches!(self, Self::History { .. })
    }

    /// Whether this is a counter-evidence citation (`!#N` or `!@N`).
    #[must_use]
    pub fn is_negated(&self) -> bool {
        match self {
            Self::History { negated, .. } | Self::Archive { negated, .. } => *negated,
        }
    }
}

/// Parse `"L"` or `"L1-L2"` into a 1-based inclusive range.
fn parse_line_range(s: &str) -> Result<RangeInclusive<usize>, String> {
    if let Some((l1, l2)) = s.split_once('-') {
        let start = l1
            .parse::<usize>()
            .map_err(|_| format!("bad start line: '{l1}'"))?;
        let end = l2
            .parse::<usize>()
            .map_err(|_| format!("bad end line: '{l2}'"))?;
        if start == 0 || end == 0 {
            return Err("line numbers are 1-based; 0 is not valid".to_string());
        }
        if start > end {
            return Err(format!("start line {start} > end line {end}"));
        }
        Ok(start..=end)
    } else {
        let l = s
            .parse::<usize>()
            .map_err(|_| format!("bad line number: '{s}'"))?;
        if l == 0 {
            return Err("line numbers are 1-based; 0 is not valid".to_string());
        }
        Ok(l..=l)
    }
}

/// Whether a citation points to a history entry (`#N`) or archive entry (`@N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitationKind {
    History,
    Archive,
}

/// Parse a slice of raw citation strings, collecting successes and failures separately.
pub fn parse_citations(raw: &[String]) -> (Vec<ParsedCitation>, Vec<String>) {
    let mut ok = Vec::with_capacity(raw.len());
    let mut errors = Vec::new();
    for s in raw {
        match ParsedCitation::parse(s) {
            Ok(c) => ok.push(c),
            Err(e) => errors.push(e.0),
        }
    }
    (ok, errors)
}

/// Parse a slice of validated [`Citation`] values into [`ParsedCitation`] (always succeeds).
#[must_use]
pub fn parsed_citations(citations: &[Citation]) -> Vec<ParsedCitation> {
    citations.iter().map(Citation::parsed).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_try_new_round_trip() {
        let c = Citation::try_new("#1").unwrap();
        assert_eq!(c.as_str(), "#1");
        assert_eq!(
            c.parsed(),
            ParsedCitation::History {
                n: 1,
                negated: false
            }
        );
    }

    #[test]
    fn citation_try_new_rejects_garbage() {
        assert!(Citation::try_new("msg-1").is_err());
        assert!(Citation::try_new("e").is_err());
    }

    #[test]
    fn parse_history_ref() {
        assert_eq!(
            ParsedCitation::parse("#1").unwrap(),
            ParsedCitation::History {
                n: 1,
                negated: false
            }
        );
    }

    #[test]
    fn parse_archive_ref_line_range() {
        assert_eq!(
            ParsedCitation::parse("@4:2-5").unwrap(),
            ParsedCitation::Archive {
                n: 4,
                lines: Some(2..=5),
                negated: false
            }
        );
    }
}
