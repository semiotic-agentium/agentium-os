// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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

/// A **validated** ref-table citation string exactly as emitted by the model / shim
/// (`#1`, `@4:L2`, `@4:L2-L5`, …).
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
    /// `@N`, `@prefix/local`, `@N:L`, `@prefix/local:L1-L2` (and `!`-prefixed negation forms).
    Archive {
        /// Archive namespace (`1` for legacy flat `@N`).
        prefix: u32,
        /// Monotonic index within `prefix`.
        local: u32,
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
            let (ref_part, line_part) = match rest.split_once(':') {
                Some((ref_part, line_part)) => (ref_part, Some(line_part)),
                None => (rest, None),
            };
            let (prefix, local) = parse_archive_ref_part(ref_part).map_err(|reason| {
                CitationParseError::invalid(format!("invalid archive ref in '{s}': {reason}"))
            })?;
            let lines = line_part.map(parse_line_range).transpose().map_err(|e| {
                CitationParseError::invalid(format!("invalid line range in '{s}': {e}"))
            })?;
            return Ok(Self::Archive {
                prefix,
                local,
                lines,
                negated,
            });
        }

        Err(CitationParseError::invalid(format!(
            "citation must start with '#' or '!#' (history) or '@' or '!@' (archive); got: '{s}'"
        )))
    }

    /// The raw ref number (the `N` in `#N`, or the local index in `@N` / `@prefix/local`).
    #[must_use]
    pub fn n(&self) -> u32 {
        match self {
            Self::History { n, .. } => *n,
            Self::Archive { local, .. } => *local,
        }
    }

    /// Archive namespace (`1` for legacy flat `@N`).
    #[must_use]
    pub fn archive_prefix(&self) -> Option<u32> {
        match self {
            Self::History { .. } => None,
            Self::Archive { prefix, .. } => Some(*prefix),
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

/// Parse `"L"`, `"L1-L2"`, `"1"`, or `"1-2"` into a 1-based inclusive range.
///
/// The `L` prefix is canonical in current prompt surfaces (`@N:L1-L2`), but the bare
/// numeric form remains accepted for backward compatibility with older stored citations.
fn parse_archive_ref_part(part: &str) -> Result<(u32, u32), String> {
    if part.is_empty() {
        return Err("empty archive ref".to_string());
    }
    if let Some((prefix_raw, local_raw)) = part.split_once('/') {
        if prefix_raw.is_empty() || local_raw.is_empty() {
            return Err(format!("invalid composite archive ref: '{part}'"));
        }
        let prefix = prefix_raw
            .parse::<u32>()
            .map_err(|_| format!("bad archive prefix: '{prefix_raw}'"))?;
        let local = local_raw
            .parse::<u32>()
            .map_err(|_| format!("bad archive local index: '{local_raw}'"))?;
        return Ok((prefix, local));
    }
    let local = part
        .parse::<u32>()
        .map_err(|_| format!("bad archive ref number: '{part}'"))?;
    Ok((1, local))
}

fn parse_line_range(s: &str) -> Result<RangeInclusive<usize>, String> {
    let normalized = s
        .strip_prefix('L')
        .or_else(|| s.strip_prefix('l'))
        .unwrap_or(s);
    if let Some((l1_raw, l2_raw)) = normalized.split_once('-') {
        let l1 = l1_raw
            .strip_prefix('L')
            .or_else(|| l1_raw.strip_prefix('l'))
            .unwrap_or(l1_raw);
        let l2 = l2_raw
            .strip_prefix('L')
            .or_else(|| l2_raw.strip_prefix('l'))
            .unwrap_or(l2_raw);
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
        let l = normalized
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

/// Scan free text for wire citation tokens (`#N`, `@N`, `@N:L`, `@p/k`, optional `!` prefix).
///
/// Uses longest-match at each `#` / `@` start so `@2/5` is not truncated to `@2`.
#[must_use]
pub fn scan_wire_citations(text: &str) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(start) = citation_start(bytes, i)
            && let Some(token) = longest_citation_at(text, start)
        {
            let advance = token.len();
            found.insert(token);
            i = start.saturating_add(advance.max(1));
            continue;
        }
        i += 1;
    }
    found.into_iter().collect()
}

fn citation_start(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes[i] == b'!' && i + 1 < bytes.len() {
        return match bytes[i + 1] {
            b'#' | b'@' => Some(i),
            _ => None,
        };
    }
    if bytes[i] == b'#' || bytes[i] == b'@' {
        return Some(i);
    }
    None
}

fn longest_citation_at(text: &str, start: usize) -> Option<String> {
    let max_end = (start + 48).min(text.len());
    let mut best: Option<String> = None;
    for end in (start + 2)..=max_end {
        if end < text.len() && is_citation_body_char(text.as_bytes()[end]) {
            continue;
        }
        let candidate = &text[start..end];
        if is_valid_wire_citation_token(candidate) {
            best = Some(candidate.to_string());
        }
    }
    best
}

fn is_citation_body_char(b: u8) -> bool {
    b.is_ascii_digit() || b == b'/' || b == b':' || b == b'-' || b == b'L' || b == b'l'
}

fn is_valid_wire_citation_token(token: &str) -> bool {
    ParsedCitation::parse(token).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_wire_citations_matrix() {
        assert_eq!(scan_wire_citations("plain text"), Vec::<String>::new());
        assert_eq!(
            scan_wire_citations("user: #1 and @2 @12:L3"),
            vec!["#1".to_string(), "@12:L3".to_string(), "@2".to_string()]
        );
        assert_eq!(
            scan_wire_citations("see @2/5 and !@3/7"),
            vec!["!@3/7".to_string(), "@2/5".to_string()]
        );
        assert_eq!(
            scan_wire_citations("negated !#4 ref"),
            vec!["!#4".to_string()]
        );
    }

    #[test]
    fn citation_parse_and_try_new_matrix() {
        let c = Citation::try_new("#1").unwrap();
        assert_eq!(c.as_str(), "#1");
        assert_eq!(
            c.parsed(),
            ParsedCitation::History {
                n: 1,
                negated: false
            }
        );
        for bad in ["msg-1", "e"] {
            assert!(Citation::try_new(bad).is_err(), "try_new {bad:?}");
        }

        let range = ParsedCitation::parse("@4:L2-L5").unwrap();
        assert_eq!(
            range,
            ParsedCitation::Archive {
                prefix: 1,
                local: 4,
                lines: Some(2..=5),
                negated: false
            }
        );
        assert_eq!(ParsedCitation::parse("@4:2-5").unwrap(), range);
        assert_eq!(
            ParsedCitation::parse("@4:L2").unwrap(),
            ParsedCitation::Archive {
                prefix: 1,
                local: 4,
                lines: Some(2..=2),
                negated: false
            }
        );
        assert_eq!(
            ParsedCitation::parse("@2/5").unwrap(),
            ParsedCitation::Archive {
                prefix: 2,
                local: 5,
                lines: None,
                negated: false
            }
        );
        assert_eq!(ParsedCitation::parse("@4:2-L5").unwrap(), range);
        assert_eq!(ParsedCitation::parse("@4:l2-l5").unwrap(), range);
        assert_eq!(
            ParsedCitation::parse("#1").unwrap(),
            ParsedCitation::History {
                n: 1,
                negated: false
            }
        );
    }
}
