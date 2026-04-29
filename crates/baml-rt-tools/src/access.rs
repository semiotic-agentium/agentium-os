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
        Self::PermitOnly(
            [ToolAccess::Read, ToolAccess::Write, ToolAccess::Delete]
                .into_iter()
                .collect(),
        )
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
        self.permitted() == Self::permit_all().permitted()
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
    let raw = match std::env::var(ACCESS_ALLOWLIST_ENV) {
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
