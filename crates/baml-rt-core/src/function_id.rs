//! Structured BAML function identity.
//!
//! BAML compiles each user-authored prompt function into multiple narrowed
//! FSM phase variants for the step executor:
//!
//! | Variant pattern                  | Example                                           |
//! |----------------------------------|---------------------------------------------------|
//! | `{Base}__entry`                  | `GetDiscoverAgentsPlan__entry` (archive reuse or Open) |
//! | `{Base}__active__{tool_slug}`    | `GetDiscoverAgentsPlan__active__system_discover_agents` |
//! | `{Base}__consume__{tool_slug}`   | Reserved for future consume-phase codegen (not emitted today) |
//!
//! Legacy parsed names (no longer generated): `__select`, `__act__*`, `__continue__*`.
//!
//! These narrowed names are a runtime implementation detail. For display,
//! configuration, and provenance attribution the **logical prompt name**
//! (`GetDiscoverAgentsPlan`) is what matters.
//!
//! [`BamlFunctionId`] is the single source of truth: it carries both the
//! base prompt name and the optional variant phase, eliminates ad-hoc
//! string splitting across consumers, and enables config inheritance from
//! a base prompt to all its variants.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The logical BAML prompt name as authored by the user.
///
/// Identical to the `function Foo(...)` declaration name in BAML source.
/// Never contains phase suffixes such as `__entry` or `__active__`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BamlPromptName(Arc<str>);

impl BamlPromptName {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BamlPromptName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BamlPromptName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for BamlPromptName {
    fn from(s: String) -> Self {
        Self::new(s.as_str())
    }
}

/// The FSM phase of a narrowed step-executor variant.
///
/// Mirrors `SessionTypeNames` in `baml-rt-tools` — keep naming in sync.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VariantPhase {
    /// Entry hop: archive reads, read-only finish, or Open (`__entry`).
    Entry,
    /// Active tool session after Open (`__active__{slug}`).
    Active { tool_slug: String },
    /// Legacy: parsed from `{base}__select` (historical codegen).
    Select,
    /// Legacy: parsed from `{base}__act__{slug}`.
    Act { tool_slug: String },
    /// Output-consumption phase (`__consume__{slug}`) — reserved; builder does not emit these yet.
    Consume { tool_slug: String },
    /// Legacy: parsed from `{base}__continue__{slug}`.
    Continue { tool_slug: String },
}

impl VariantPhase {
    /// The suffix appended to the base name to form the full variant name.
    pub fn suffix(&self) -> String {
        match self {
            Self::Entry => "__entry".to_string(),
            Self::Active { tool_slug } => format!("__active__{tool_slug}"),
            Self::Select => "__select".to_string(),
            Self::Act { tool_slug } => format!("__act__{tool_slug}"),
            Self::Consume { tool_slug } => format!("__consume__{tool_slug}"),
            Self::Continue { tool_slug } => format!("__continue__{tool_slug}"),
        }
    }
}

/// Structured identity for a BAML function — either a base prompt or a
/// narrowed FSM variant.
///
/// ## Parse-once, carry-everywhere
///
/// Construct via [`BamlFunctionId::parse`] at every string ingress point.
/// All downstream consumers then call:
/// - `.prompt_name()` — for display, config keys, agent card, provenance display
/// - `.full_name()` — for BAML runtime invocation, provenance debug detail
///
/// ## Serde
///
/// Serializes as the full variant string (backward-compatible with stored events).
/// Deserializes via `parse()`, which recovers the base name automatically.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BamlFunctionId {
    base: BamlPromptName,
    phase: Option<VariantPhase>,
}

impl BamlFunctionId {
    /// Construct a base (non-narrowed) function identity.
    pub fn base(name: impl Into<Arc<str>>) -> Self {
        Self {
            base: BamlPromptName::new(name),
            phase: None,
        }
    }

    /// Construct a narrowed variant.
    pub fn variant(base: BamlPromptName, phase: VariantPhase) -> Self {
        Self {
            base,
            phase: Some(phase),
        }
    }

    /// Parse from a raw function name string.
    ///
    /// Recognises `__entry`, `__active__<slug>`, and legacy `__select` / `__act__` / `__continue__`.
    /// Falls back to base (non-narrowed) if no suffix matches.
    pub fn parse(raw: &str) -> Self {
        // Try __entry (before legacy __select)
        if let Some(base) = raw.strip_suffix("__entry")
            && !base.is_empty()
        {
            return Self::variant(BamlPromptName::new(base), VariantPhase::Entry);
        }

        // Try __active__<slug> before __act__<slug> (__active__ does not contain __act__ as substring)
        if let Some(pos) = raw.find("__active__") {
            let (base, rest) = raw.split_at(pos);
            let slug = &rest["__active__".len()..];
            if !base.is_empty() && !slug.is_empty() {
                return Self::variant(
                    BamlPromptName::new(base),
                    VariantPhase::Active {
                        tool_slug: slug.to_string(),
                    },
                );
            }
        }

        // Legacy: __select
        if let Some(base) = raw.strip_suffix("__select")
            && !base.is_empty()
        {
            return Self::variant(BamlPromptName::new(base), VariantPhase::Select);
        }

        // Legacy: __act__<slug>
        if let Some(pos) = raw.find("__act__") {
            let (base, rest) = raw.split_at(pos);
            let slug = &rest["__act__".len()..];
            if !base.is_empty() && !slug.is_empty() {
                return Self::variant(
                    BamlPromptName::new(base),
                    VariantPhase::Act {
                        tool_slug: slug.to_string(),
                    },
                );
            }
        }

        // Try __consume__<slug> (before __continue__ — distinct markers)
        if let Some(pos) = raw.find("__consume__") {
            let (base, rest) = raw.split_at(pos);
            let slug = &rest["__consume__".len()..];
            if !base.is_empty() && !slug.is_empty() {
                return Self::variant(
                    BamlPromptName::new(base),
                    VariantPhase::Consume {
                        tool_slug: slug.to_string(),
                    },
                );
            }
        }

        // Legacy: __continue__<slug>
        if let Some(pos) = raw.find("__continue__") {
            let (base, rest) = raw.split_at(pos);
            let slug = &rest["__continue__".len()..];
            if !base.is_empty() && !slug.is_empty() {
                return Self::variant(
                    BamlPromptName::new(base),
                    VariantPhase::Continue {
                        tool_slug: slug.to_string(),
                    },
                );
            }
        }

        Self::base(raw)
    }

    /// The logical prompt name — for display, config keys, and grouping.
    pub fn prompt_name(&self) -> &BamlPromptName {
        &self.base
    }

    /// The full variant name — for BAML runtime invocation and debug provenance.
    pub fn full_name(&self) -> String {
        match &self.phase {
            None => self.base.as_str().to_string(),
            Some(phase) => format!("{}{}", self.base, phase.suffix()),
        }
    }

    /// Whether this is a narrowed variant (not the base prompt).
    pub fn is_variant(&self) -> bool {
        self.phase.is_some()
    }

    /// The variant phase, if narrowed.
    pub fn phase(&self) -> Option<&VariantPhase> {
        self.phase.as_ref()
    }
}

impl fmt::Display for BamlFunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full_name())
    }
}

impl From<&str> for BamlFunctionId {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

impl From<String> for BamlFunctionId {
    fn from(s: String) -> Self {
        Self::parse(&s)
    }
}

impl Serialize for BamlFunctionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.full_name().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BamlFunctionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::parse(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_base_name() {
        let id = BamlFunctionId::parse("GetDiscoverAgentsPlan");
        assert_eq!(id.prompt_name().as_str(), "GetDiscoverAgentsPlan");
        assert_eq!(id.full_name(), "GetDiscoverAgentsPlan");
        assert!(!id.is_variant());
    }

    #[test]
    fn parse_entry() {
        let id = BamlFunctionId::parse("GetDiscoverAgentsPlan__entry");
        assert_eq!(id.prompt_name().as_str(), "GetDiscoverAgentsPlan");
        assert_eq!(id.full_name(), "GetDiscoverAgentsPlan__entry");
        assert!(id.is_variant());
        assert_eq!(id.phase(), Some(&VariantPhase::Entry));
    }

    #[test]
    fn parse_active() {
        let id = BamlFunctionId::parse("GetDiscoverAgentsPlan__active__system_discover_agents");
        assert_eq!(id.prompt_name().as_str(), "GetDiscoverAgentsPlan");
        assert_eq!(
            id.phase(),
            Some(&VariantPhase::Active {
                tool_slug: "system_discover_agents".to_string()
            })
        );
    }

    #[test]
    fn parse_select_legacy() {
        let id = BamlFunctionId::parse("GetDiscoverAgentsPlan__select");
        assert_eq!(id.prompt_name().as_str(), "GetDiscoverAgentsPlan");
        assert_eq!(id.full_name(), "GetDiscoverAgentsPlan__select");
        assert!(id.is_variant());
        assert_eq!(id.phase(), Some(&VariantPhase::Select));
    }

    #[test]
    fn parse_act_legacy() {
        let id = BamlFunctionId::parse("GetDiscoverAgentsPlan__act__system_discover_agents");
        assert_eq!(id.prompt_name().as_str(), "GetDiscoverAgentsPlan");
        assert_eq!(
            id.full_name(),
            "GetDiscoverAgentsPlan__act__system_discover_agents"
        );
        assert_eq!(
            id.phase(),
            Some(&VariantPhase::Act {
                tool_slug: "system_discover_agents".to_string()
            })
        );
    }

    #[test]
    fn parse_continue_legacy() {
        let id = BamlFunctionId::parse("BuildExtrospectionPlan__continue__system_extrospection");
        assert_eq!(id.prompt_name().as_str(), "BuildExtrospectionPlan");
        assert_eq!(
            id.phase(),
            Some(&VariantPhase::Continue {
                tool_slug: "system_extrospection".to_string()
            })
        );
    }

    #[test]
    fn parse_consume() {
        let id = BamlFunctionId::parse("FooPlan__consume__support_calculate");
        assert_eq!(id.prompt_name().as_str(), "FooPlan");
        assert_eq!(
            id.phase(),
            Some(&VariantPhase::Consume {
                tool_slug: "support_calculate".to_string()
            })
        );
        assert_eq!(id.full_name(), "FooPlan__consume__support_calculate");
    }

    #[test]
    fn serde_round_trip() {
        let original = "DetermineExtrospectionIntent__entry";
        let id: BamlFunctionId = serde_json::from_str(&format!("\"{original}\"")).unwrap();
        let serialized = serde_json::to_string(&id).unwrap();
        assert_eq!(serialized, format!("\"{original}\""));
        assert_eq!(id.prompt_name().as_str(), "DetermineExtrospectionIntent");
    }

    #[test]
    fn base_name_only_no_false_match() {
        // Names with underscores but no double-underscore phase suffix
        let id = BamlFunctionId::parse("My_Custom_Prompt");
        assert_eq!(id.prompt_name().as_str(), "My_Custom_Prompt");
        assert!(!id.is_variant());
    }
}
