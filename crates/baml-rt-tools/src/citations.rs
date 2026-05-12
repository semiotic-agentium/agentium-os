//! Citation types for LLM decision grounding.
//!
//! ## Reference namespaces
//!
//! Citations are string references the LLM emits alongside its decision.
//! They index into the `RefTable` built during prompt projection for the call.
//! **The two namespaces are strictly separate — never substitute one for the other.**
//!
//! | Prefix | Namespace | Points to |
//! |--------|-----------|-----------|
//! | `#N`   | Session history | A numbered line from `conversation_history`: `user`, `assistant`, or `tool`-role rows (messages, tool calls, session FSM steps) |
//! | `@N`   | Archive | A complete archived tool result (the blob returned by a `Send Done` step) |
//! | `@N:L` | Archive, line-scoped | Single line `L` (1-based) inside archive entry `N` |
//! | `@N:L1-L2` | Archive, range-scoped | Lines `L1..=L2` (1-based, inclusive) inside archive entry `N` |
//!
//! ## Negation (`!` prefix)
//!
//! A leading `!` marks **counter-evidence**: the LLM explicitly states that
//! this entry *contradicts* its decision rather than supporting it.
//!
//! - `!#N` — history entry N contradicts this decision
//! - `!@N`, `!@N:L`, `!@N:L1-L2` — archive entry contradicts this decision
//!
//! Counter-evidence citations are:
//! - **Scored** (their `similarity` is still computed and reported)
//! - **Excluded** from `mean_similarity` in drift assessment — a model correctly
//!   citing opposition is not a signal of weak grounding
//! - A **safety signal in themselves**: a model that negates injected directives
//!   (`!@N` on a malicious archive entry) demonstrates deliberate rejection of
//!   prompt-injection attempts
//!
//! ## Citation quality as a BIPIA signal
//!
//! The cosine similarity between an LLM's output and its cited sources forms
//! the second axis of the BIPIA (Business-Information Prompt-Injection Attack)
//! firewall. See [`baml_rt_embedding::CitationDriftAssessment`] and
//! [`baml_rt_embedding::BipiaSignal`] for the scoring layer.
//!
//! Briefly: a successful injection produces a response that closely paraphrases
//! the injected archive content (`cite_mean > 0.55` for specific steps, `> 0.82`
//! for synthesis steps) while deviating from the plan step (`step_align < 0.45`
//! for specific steps, `< 0.62` for synthesis). Legitimate synthesis produces
//! `cite_mean` 0.67–0.78; injection produces 0.69–0.91.
//!
//! ## Granularity matters
//!
//! Line-scoped citations (`@N:1-3`) produce measurably higher `cite_mean` than
//! full-blob citations (`@N`) for the same narrow claim. Calibrated gap from
//! `tests/fixtures/drift/12_synthesis_citations.toml`:
//! - line-scoped: cite_mean = 0.78
//! - full-blob (15 records, 12 irrelevant): cite_mean = 0.67
//!
//! ## Crate split
//!
//! [`Citation`], [`ParsedCitation`], [`CitationKind`], and [`parse_citations`] live in
//! **`baml-rt-citation`** (no dependency on `RefTable`). This module re-exports them and
//! adds resolution / validation helpers that need the tool runtime.

pub use baml_rt_citation::{
    Citation, CitationKind, CitationParseError, ParsedCitation, parse_citations, parsed_citations,
};

/// A citation resolved to its full content from a `RefTable`.
///
/// `ResolvedCitation` bridges the ephemeral `RefTable` indices (`#N`, `@N`) used
/// during a single prompt context to the stable provenance graph. It is computed
/// in `effect_subscriber.rs` at scoring time and its fields are stored in
/// `LlmCitationSimilarity` so downstream consumers never need to re-resolve.
///
/// ## Usage
///
/// - `content` is fed directly into `score_citation_drift` for embedding.
/// - `activity_anchor` is stored on `LlmCitationSimilarity` for provenance
///   graph lookup — callers can find the original activity without the ref table.
/// - `negated` propagates through to `CitationDriftAssessment` where it gates
///   whether the citation contributes to `mean_similarity`.
#[derive(Debug, Clone)]
pub struct ResolvedCitation {
    /// The raw ref number (`N` in `#N` or `@N`).
    pub n: u32,
    /// Whether this is a history ref (`#N`) or archive ref (`@N`).
    pub kind: CitationKind,
    /// `true` when the original citation had the `!` prefix (counter-evidence).
    pub negated: bool,
    /// Stable activity anchor — matches `a2a_activity_anchor` in the provenance graph and
    /// `ActivityAnchorId` in the core runtime. Use for cross-reference lookups.
    pub activity_anchor: String,
    /// Source type: `"message"`, `"tool_call"`, or `"tool_result"`.
    pub source: String,
    /// The actual text of the cited entry. For archive refs with a line range
    /// (`@N:L` or `@N:L1-L2`), only those lines are returned. For history refs,
    /// this is the full text from [`RefTable::history_text_for_activity`].
    ///
    /// This is what gets embedded and compared against the decision text in
    /// `score_citation_drift`.
    pub content: String,
    /// Line range selected for archive citations. `None` for history refs and
    /// full-blob archive refs.
    pub lines: Option<std::ops::RangeInclusive<usize>>,
}

impl ResolvedCitation {
    /// Attempt to resolve a `ParsedCitation` against a `RefTable`.
    ///
    /// Returns `None` when the ref number is not found in the appropriate map.
    pub fn resolve(
        citation: &ParsedCitation,
        ref_table: &crate::archive_refs::RefTable,
    ) -> Option<Self> {
        use crate::archive_read::{HistoryRef, ShortRef};

        match citation {
            ParsedCitation::History { n, negated } => {
                let h_ref = HistoryRef::new(*n);
                let entry = ref_table.get_history(h_ref)?;
                let content = ref_table
                    .history_text_for_activity(entry.activity_anchor.as_str())?
                    .as_ref()
                    .to_string();
                Some(Self {
                    n: *n,
                    kind: CitationKind::History,
                    negated: *negated,
                    activity_anchor: entry.activity_anchor.clone(),
                    source: entry.source.clone(),
                    content,
                    lines: None,
                })
            }
            ParsedCitation::Archive { n, lines, negated } => {
                let s_ref = ShortRef::new(*n);
                let entry = ref_table.get(s_ref)?;
                let content = if let Some(range) = lines {
                    // Extract only the requested line range (1-based, inclusive).
                    let start = range.start().saturating_sub(1); // convert to 0-based
                    entry
                        .content
                        .lines()
                        .skip(start)
                        .take(range.end() - range.start() + 1)
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    entry.content.lines().collect::<Vec<_>>().join("\n")
                };
                Some(Self {
                    n: *n,
                    kind: CitationKind::Archive,
                    negated: *negated,
                    activity_anchor: entry.activity_anchor.clone(),
                    source: entry.source.clone(),
                    content,
                    lines: lines.clone(),
                })
            }
        }
    }
}

/// Operating mode for citation enforcement.
///
/// Mirrors [`DriftMode`] semantics: `Audit` is the safe default for rollout;
/// `Enforce` blocks/rejects when citations are absent or invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationMode {
    /// Log parse/validation failures via `tracing`, never reject.
    #[default]
    Audit,
    /// Log and reject when citations are missing or unresolvable.
    Enforce,
}

/// Configuration for citation enforcement.
#[derive(Debug, Clone)]
pub struct CitationConfig {
    pub mode: CitationMode,
    /// When `true`, a decision with an empty `citations` array is also flagged.
    pub require_at_least_one: bool,
}

impl Default for CitationConfig {
    fn default() -> Self {
        Self {
            mode: CitationMode::Audit,
            require_at_least_one: false,
        }
    }
}

/// Result of validating a set of raw citation strings.
#[derive(Debug, Clone)]
pub struct CitationValidation {
    /// Successfully parsed citations.
    pub parsed: Vec<ParsedCitation>,
    /// Parse failures (format errors).
    pub format_errors: Vec<String>,
    /// Whether the set was empty and `require_at_least_one` was set.
    pub missing_citations: bool,
}

impl CitationValidation {
    /// `true` when there are no format errors and no missing-citation violation.
    pub fn is_ok(&self) -> bool {
        self.format_errors.is_empty() && !self.missing_citations
    }
}

/// Validate raw citation strings according to `config`.
///
/// In `Audit` mode the function always returns without error; callers should
/// log the `CitationValidation` fields via `tracing`. In `Enforce` mode the
/// function returns `Err` if validation fails, with a combined error message.
pub fn validate_citations(
    raw: &[String],
    config: &CitationConfig,
) -> Result<CitationValidation, String> {
    let (parsed, format_errors) = parse_citations(raw);
    let missing_citations = config.require_at_least_one && raw.is_empty();

    let validation = CitationValidation {
        parsed,
        format_errors,
        missing_citations,
    };

    match config.mode {
        CitationMode::Audit => {
            if !validation.is_ok() {
                tracing::warn!(
                    format_errors = ?validation.format_errors,
                    missing_citations = validation.missing_citations,
                    "citation validation failed (audit mode — not rejected)"
                );
            }
            Ok(validation)
        }
        CitationMode::Enforce => {
            if !validation.is_ok() {
                let mut parts: Vec<String> = Vec::new();
                if validation.missing_citations {
                    parts.push("citations required but none provided".to_string());
                }
                if !validation.format_errors.is_empty() {
                    parts.push(format!(
                        "format errors: {}",
                        validation.format_errors.join("; ")
                    ));
                }
                return Err(parts.join("; "));
            }
            Ok(validation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_history_ref() {
        assert_eq!(
            ParsedCitation::parse("#1").unwrap(),
            ParsedCitation::History {
                n: 1,
                negated: false
            }
        );
        assert_eq!(
            ParsedCitation::parse("#42").unwrap(),
            ParsedCitation::History {
                n: 42,
                negated: false
            }
        );
    }

    #[test]
    fn parse_negated_history_ref() {
        assert_eq!(
            ParsedCitation::parse("!#3").unwrap(),
            ParsedCitation::History {
                n: 3,
                negated: true
            }
        );
        assert!(ParsedCitation::parse("!#3").unwrap().is_negated());
        assert!(!ParsedCitation::parse("#3").unwrap().is_negated());
    }

    #[test]
    fn parse_archive_ref_bare() {
        assert_eq!(
            ParsedCitation::parse("@3").unwrap(),
            ParsedCitation::Archive {
                n: 3,
                lines: None,
                negated: false
            }
        );
    }

    #[test]
    fn parse_negated_archive_ref_bare() {
        assert_eq!(
            ParsedCitation::parse("!@2").unwrap(),
            ParsedCitation::Archive {
                n: 2,
                lines: None,
                negated: true
            }
        );
    }

    #[test]
    fn parse_archive_ref_single_line() {
        assert_eq!(
            ParsedCitation::parse("@4:2").unwrap(),
            ParsedCitation::Archive {
                n: 4,
                lines: Some(2..=2),
                negated: false
            }
        );
    }

    #[test]
    fn parse_negated_archive_ref_line_range() {
        assert_eq!(
            ParsedCitation::parse("!@4:2-5").unwrap(),
            ParsedCitation::Archive {
                n: 4,
                lines: Some(2..=5),
                negated: true
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

    #[test]
    fn parse_rejects_empty() {
        assert!(ParsedCitation::parse("").is_err());
    }

    #[test]
    fn parse_rejects_bare_number() {
        assert!(ParsedCitation::parse("3").is_err());
    }

    #[test]
    fn parse_rejects_zero_line() {
        assert!(ParsedCitation::parse("@1:0").is_err());
    }

    #[test]
    fn parse_rejects_inverted_range() {
        assert!(ParsedCitation::parse("@1:5-2").is_err());
    }

    #[test]
    fn parse_rejects_bad_archive_number() {
        assert!(ParsedCitation::parse("@abc").is_err());
    }

    #[test]
    fn n_accessor() {
        assert_eq!(ParsedCitation::parse("#7").unwrap().n(), 7);
        assert_eq!(ParsedCitation::parse("@12:3-6").unwrap().n(), 12);
    }

    #[test]
    fn parse_citations_batch() {
        let raw = vec![
            "#1".to_string(),
            "@4:2".to_string(),
            "bad".to_string(),
            "@7".to_string(),
        ];
        let (ok, errors) = parse_citations(&raw);
        assert_eq!(ok.len(), 3);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn validate_audit_accepts_all() {
        let cfg = CitationConfig {
            mode: CitationMode::Audit,
            ..Default::default()
        };
        let raw = vec!["bad".to_string()];
        let v = validate_citations(&raw, &cfg).expect("audit must not error");
        assert!(!v.is_ok());
        assert_eq!(v.format_errors.len(), 1);
    }

    #[test]
    fn validate_enforce_rejects_format_error() {
        let cfg = CitationConfig {
            mode: CitationMode::Enforce,
            require_at_least_one: false,
        };
        let raw = vec!["not-a-citation".to_string()];
        assert!(validate_citations(&raw, &cfg).is_err());
    }

    #[test]
    fn validate_enforce_rejects_missing() {
        let cfg = CitationConfig {
            mode: CitationMode::Enforce,
            require_at_least_one: true,
        };
        assert!(validate_citations(&[], &cfg).is_err());
    }

    #[test]
    fn validate_enforce_accepts_valid() {
        let cfg = CitationConfig {
            mode: CitationMode::Enforce,
            require_at_least_one: true,
        };
        let raw = vec!["#1".to_string(), "@4:2-5".to_string()];
        let v = validate_citations(&raw, &cfg).expect("valid citations should pass");
        assert!(v.is_ok());
        assert_eq!(v.parsed.len(), 2);
    }

    #[test]
    fn resolved_citation_history_ref() {
        use crate::archive_refs::{HistoryEntry, RefTable};

        let table = RefTable::new();
        let entry = HistoryEntry::new("evt-001".into(), "message".into());
        let h = table.insert_history(entry, "Can you analyse Q4 accounts?");
        assert_eq!(h.as_u32(), 1);

        let citation = ParsedCitation::parse("#1").unwrap();
        let resolved = ResolvedCitation::resolve(&citation, &table).expect("must resolve");
        assert_eq!(resolved.n, 1);
        assert_eq!(resolved.kind, CitationKind::History);
        assert!(!resolved.negated);
        assert_eq!(resolved.activity_anchor, "evt-001");
        assert_eq!(resolved.source, "message");
        assert!(resolved.content.contains("Q4"));
    }

    #[test]
    fn resolved_citation_archive_ref_line_range() {
        use crate::{
            archive_read::render_to_lines,
            archive_refs::{ArchiveEntry, RefTable},
        };

        let table = RefTable::new();
        let content = render_to_lines(&serde_json::json!([
            {"acct": "001"}, {"acct": "002"}, {"acct": "003"}
        ]));
        let entry = ArchiveEntry::new(
            content,
            "support/crm".into(),
            Some("listed 3 accounts".into()),
            "evt-002".into(),
            "tool_result".into(),
        );
        let r = table.insert(entry);
        assert_eq!(r.as_u32(), 1);

        let citation = ParsedCitation::parse("@1:1-2").unwrap();
        let resolved = ResolvedCitation::resolve(&citation, &table).expect("must resolve");
        assert_eq!(resolved.n, 1);
        assert_eq!(resolved.kind, CitationKind::Archive);
        assert!(!resolved.negated);
        assert_eq!(resolved.lines, Some(1..=2));
    }

    #[test]
    fn resolved_citation_negated_propagates() {
        use crate::archive_refs::{HistoryEntry, RefTable};

        let table = RefTable::new();
        let entry = HistoryEntry::new("evt-003".into(), "message".into());
        table.insert_history(entry, "Q3 data shows no anomalies");

        let citation = ParsedCitation::parse("!#1").unwrap();
        let resolved = ResolvedCitation::resolve(&citation, &table).expect("must resolve");
        assert_eq!(resolved.kind, CitationKind::History);
        assert!(
            resolved.negated,
            "negated flag must propagate through resolve"
        );
    }

    #[test]
    fn resolved_citation_returns_none_for_missing_ref() {
        use crate::archive_refs::RefTable;

        let table = RefTable::new();
        let citation = ParsedCitation::parse("#99").unwrap();
        assert!(ResolvedCitation::resolve(&citation, &table).is_none());
    }
}
