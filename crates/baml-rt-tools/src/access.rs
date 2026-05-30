// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Optional cluster-wide cap on host-tool access classes (read / write / delete).
//!
//! Tool access is gated by two layers, both of which must permit a tool:
//!
//! 1. **Per-agent manifest allowlist** — the deny-by-default gate. An agent
//!    can only use tools it explicitly lists in its `manifest.json`.
//! 2. **Access-level cap** (this module) — an optional operator control that
//!    forbids whole classes of tools cluster-wide. Configure with the
//!    `BAML_TOOL_ACCESS_ALLOWLIST` environment variable, e.g.
//!    `BAML_TOOL_ACCESS_ALLOWLIST=read,write` to disallow delete tools.
//!    When the variable is unset, the cap imposes no extra restriction; the
//!    manifest allowlist still applies.
//!
//! Use the phrase "deny-by-default per agent via the manifest" rather than a
//! bare "deny-by-default tool access" so the two layers stay distinct.

use std::collections::HashSet;

use baml_rt_core::{BamlRtError, Result};
use tracing::warn;

use crate::{
    ToolName,
    tool_catalog::{InventoryCatalog, ToolCatalog},
    tools::ToolAccess,
};

/// Environment variable that configures the cluster-wide access-class cap.
/// See [`parse_access_allowlist`] and the module-level docs for semantics.
pub const ACCESS_ALLOWLIST_ENV: &str = "BAML_TOOL_ACCESS_ALLOWLIST";

/// Operator-configured cap on the host-tool access classes a runner will
/// expose. Composed with the per-agent manifest allowlist; see the module
/// docs for the full model.
#[derive(Debug, Clone)]
pub enum ToolAccessPolicy {
    /// Permit only tools whose declared access class is in this set.
    /// Empty set means no class is permitted by the cap.
    PermitOnly(HashSet<ToolAccess>),
}

impl ToolAccessPolicy {
    /// No cap: every access class (read, write, delete) is permitted by this
    /// layer. The manifest allowlist still applies.
    pub fn permit_all() -> Self {
        Self::PermitOnly(ToolAccess::ALL.iter().copied().collect())
    }

    /// Strict cap: forbid every access class. Tools that declare a class will
    /// be rejected; only access-less tools pass.
    pub fn deny_all() -> Self {
        Self::PermitOnly(HashSet::new())
    }

    pub fn permitted(&self) -> &HashSet<ToolAccess> {
        match self {
            ToolAccessPolicy::PermitOnly(set) => set,
        }
    }

    /// True when the cap permits every access class — i.e. the cap is a no-op
    /// and only the manifest allowlist gates tool exposure. Useful for
    /// startup logs that surface the active policy to operators.
    pub fn is_unrestricted(&self) -> bool {
        // `permitted` is `HashSet<ToolAccess>` (unique elements drawn from
        // `ToolAccess`), so size-equality with `ALL` is sufficient.
        self.permitted().len() == ToolAccess::ALL.len()
    }
}

impl Default for ToolAccessPolicy {
    /// No cap. Tool exposure is still deny-by-default per agent through the
    /// manifest allowlist; this layer just adds nothing on top. Operators who
    /// want a tighter cap set `BAML_TOOL_ACCESS_ALLOWLIST` explicitly.
    fn default() -> Self {
        Self::permit_all()
    }
}

/// Read the access-class cap from `BAML_TOOL_ACCESS_ALLOWLIST`.
///
/// - Unset: no cap (manifest allowlist still applies).
/// - Comma-separated list of `read` / `write` / `delete`: cap to those
///   classes. Unknown tokens are warned and ignored. An entirely unknown or
///   empty value caps to the empty set (every classed tool is rejected).
pub fn parse_access_allowlist() -> ToolAccessPolicy {
    parse_access_allowlist_from(std::env::var(ACCESS_ALLOWLIST_ENV).ok().as_deref())
}

/// Pure form of [`parse_access_allowlist`] that takes the raw env value as a
/// parameter. `None` means the variable was unset; `Some("")` means it was
/// set to an empty string (which caps to the empty set).
pub fn parse_access_allowlist_from(value: Option<&str>) -> ToolAccessPolicy {
    let raw = match value {
        Some(s) => s,
        None => return ToolAccessPolicy::default(),
    };
    let mut set = HashSet::new();
    for token in raw.split(',') {
        let value = token.trim().to_lowercase();
        let access = match value.as_str() {
            "read" => ToolAccess::Read,
            "write" => ToolAccess::Write,
            "delete" => ToolAccess::Delete,
            "" => continue,
            other => {
                warn!(
                    value = other,
                    "Unknown access in BAML_TOOL_ACCESS_ALLOWLIST"
                );
                continue;
            }
        };
        set.insert(access);
    }
    ToolAccessPolicy::PermitOnly(set)
}

/// Enforce access policy for a tool. All tools (including system) must be gated; always runs the check.
pub fn enforce_tool_access(tool_name: &str, policy: &ToolAccessPolicy) -> Result<()> {
    let permitted = policy.permitted();
    let catalog = InventoryCatalog::new();
    let name = ToolName::parse(tool_name)?;
    if let Some(metadata) = catalog.by_name(&name) {
        if let Some(access) = metadata.access {
            if !permitted.contains(&access) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{tool_name}' access '{access}' is not allowed by BAML_TOOL_ACCESS_ALLOWLIST"
                )));
            }
        } else {
            warn!(
                tool = tool_name,
                "Tool has no declared access; allowing due to policy"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permit_all_is_unrestricted() {
        assert!(ToolAccessPolicy::permit_all().is_unrestricted());
    }

    #[test]
    fn deny_all_is_not_unrestricted() {
        assert!(!ToolAccessPolicy::deny_all().is_unrestricted());
    }

    #[test]
    fn partial_cap_is_not_unrestricted() {
        let policy = ToolAccessPolicy::PermitOnly([ToolAccess::Read].into_iter().collect());
        assert!(!policy.is_unrestricted());
    }

    #[test]
    fn permit_all_membership_matches_tool_access_all() {
        let permitted = ToolAccessPolicy::permit_all();
        for access in ToolAccess::ALL {
            assert!(
                permitted.permitted().contains(access),
                "permit_all is missing {access:?}; ToolAccess::ALL and permit_all are out of sync"
            );
        }
        assert_eq!(permitted.permitted().len(), ToolAccess::ALL.len());
    }

    #[test]
    fn parse_unset_returns_default() {
        let policy = parse_access_allowlist_from(None);
        assert!(policy.is_unrestricted());
    }

    #[test]
    fn parse_single_class_caps_to_that_class() {
        let policy = parse_access_allowlist_from(Some("read"));
        assert_eq!(policy.permitted().len(), 1);
        assert!(policy.permitted().contains(&ToolAccess::Read));
        assert!(!policy.is_unrestricted());
    }

    #[test]
    fn parse_comma_list_admits_each_listed_class() {
        let policy = parse_access_allowlist_from(Some("read, write"));
        assert!(policy.permitted().contains(&ToolAccess::Read));
        assert!(policy.permitted().contains(&ToolAccess::Write));
        assert!(!policy.permitted().contains(&ToolAccess::Delete));
    }

    #[test]
    fn parse_full_list_is_unrestricted() {
        let policy = parse_access_allowlist_from(Some("read,write,delete"));
        assert!(policy.is_unrestricted());
    }

    #[test]
    fn parse_empty_string_caps_to_empty_set() {
        let policy = parse_access_allowlist_from(Some(""));
        assert!(policy.permitted().is_empty());
    }

    #[test]
    fn parse_unknown_tokens_are_ignored_not_admitted() {
        let policy = parse_access_allowlist_from(Some("read,bogus,write"));
        assert!(policy.permitted().contains(&ToolAccess::Read));
        assert!(policy.permitted().contains(&ToolAccess::Write));
        assert_eq!(policy.permitted().len(), 2);
    }

    #[test]
    fn parse_is_case_insensitive_and_trims_whitespace() {
        let policy = parse_access_allowlist_from(Some("  READ , Write "));
        assert!(policy.permitted().contains(&ToolAccess::Read));
        assert!(policy.permitted().contains(&ToolAccess::Write));
    }
}
