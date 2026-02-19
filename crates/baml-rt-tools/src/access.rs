//! Tool access gating: all tools (including system) must be checked against policy.
//! No bare Option; use ToolAccessPolicy (DU) for explicit semantics.

use crate::ToolName;
use crate::tool_catalog::{InventoryCatalog, ToolCatalog};
use crate::tools::ToolAccess;
use baml_rt_core::{BamlRtError, Result};
use std::collections::HashSet;
use tracing::warn;

/// Access policy for host tools. All tools must be gated; there is no "unrestricted" path.
#[derive(Debug, Clone)]
pub enum ToolAccessPolicy {
    /// Only tools whose required access level is in this set may be registered.
    /// Empty set = no access permitted.
    PermitOnly(HashSet<ToolAccess>),
}

impl ToolAccessPolicy {
    /// Permit all known access levels (read, write, delete). Use when env is unset and you want permissive default.
    pub fn permit_all() -> Self {
        Self::PermitOnly(
            [ToolAccess::Read, ToolAccess::Write, ToolAccess::Delete]
                .into_iter()
                .collect(),
        )
    }

    /// Deny all (empty set). Use when env is unset and you want strict default.
    pub fn deny_all() -> Self {
        Self::PermitOnly(HashSet::new())
    }

    pub fn permitted(&self) -> &HashSet<ToolAccess> {
        match self {
            ToolAccessPolicy::PermitOnly(set) => set,
        }
    }
}

impl Default for ToolAccessPolicy {
    /// Default: permit all levels (backward compat when env var not set).
    fn default() -> Self {
        Self::permit_all()
    }
}

/// Parse policy from BAML_TOOL_ACCESS_ALLOWLIST env var.
/// When unset or empty, returns default (permit all). Call sites may use ToolAccessPolicy::deny_all() for strict default.
pub fn parse_access_allowlist() -> ToolAccessPolicy {
    let raw = match std::env::var("BAML_TOOL_ACCESS_ALLOWLIST") {
        Ok(s) => s,
        Err(_) => return ToolAccessPolicy::default(),
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
