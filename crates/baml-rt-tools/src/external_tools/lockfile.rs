// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! The agent's external-tool lockfile.
//!
//! Version 2 pins a toolset's executable digest separately from the interface
//! digest of each tool it exposes. Agent identity uses the latter. This lets an
//! operator replace an implementation without rebuilding an agent when the
//! MCP contract is unchanged.

use std::{collections::HashMap, fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};

use crate::{ToolName, external_tools::ExternalToolSnapshot, mcp_snapshot::Digest};

pub const EXTERNAL_TOOLS_LOCKFILE_NAME: &str = "external_tools.lock.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolsLockfile {
    pub version: String,
    pub toolsets: Vec<ExternalToolsetLockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolsetLockEntry {
    pub name: String,
    pub version: String,
    pub requirement: String,
    pub tier: String,
    pub tool_digest: String,
    pub tools: Vec<ExternalToolInterfaceLockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolInterfaceLockEntry {
    pub name: String,
    pub descriptor_digest: String,
}

impl ExternalToolsLockfile {
    pub fn empty() -> Self {
        Self {
            version: "2".to_string(),
            toolsets: Vec::new(),
        }
    }

    pub fn read_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", path.display()),
            source: Box::new(e),
        })?;
        let parsed: Self =
            serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to parse {}", path.display()),
                source: Box::new(e),
            })?;
        if parsed.version != "2" {
            return Err(BamlRtError::InvalidArgument(format!(
                "unsupported external tools lockfile version '{}' (expected '2')",
                parsed.version
            )));
        }
        validate(&parsed)?;
        Ok(parsed)
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        let mut value = self.clone();
        value.version = "2".to_string();
        validate(&value)?;
        let json = serde_json::to_string_pretty(&value).map_err(BamlRtError::Json)?;
        fs::write(path, json).map_err(BamlRtError::Io)?;
        Ok(())
    }

    /// Find the toolset entry containing a fully-qualified tool name.
    pub fn by_name(&self, name: &ToolName) -> Option<&ExternalToolInterfaceLockEntry> {
        let qualified = name.to_string();
        self.toolsets
            .iter()
            .find(|set| set.name == name.bundle().as_str())
            .and_then(|set| {
                set.tools
                    .iter()
                    .find(|entry| entry.name == name.local().as_str() || entry.name == qualified)
            })
    }

    pub fn toolset_for(&self, name: &ToolName) -> Option<&ExternalToolsetLockEntry> {
        let qualified = name.to_string();
        self.toolsets.iter().find(|set| {
            set.name == name.bundle().as_str()
                && set
                    .tools
                    .iter()
                    .any(|tool| tool.name == name.local().as_str() || tool.name == qualified)
        })
    }

    /// Verify the interface pin carried by a packaged approved snapshot. The
    /// executable digest is intentionally not required to equal the lockfile
    /// pin here: an approved same-interface implementation swap is allowed at
    /// deploy time and is recorded by the host.
    pub fn verify_snapshot(&self, snapshot: &ExternalToolSnapshot) -> Result<()> {
        let Some(v2) = snapshot.v2.as_ref() else {
            return Err(BamlRtError::InvalidArgument(format!(
                "lockfile v2 cannot verify legacy external tool '{}'",
                snapshot.tool.name
            )));
        };
        let name = ToolName::parse(&v2.descriptor.name)?;
        let Some(entry) = self.by_name(&name) else {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' is missing from lockfile v2",
                v2.descriptor.name
            )));
        };
        if entry.descriptor_digest != v2.descriptor_digest.to_string() {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' descriptor digest mismatch: lockfile {}, snapshot {}",
                v2.descriptor.name, entry.descriptor_digest, v2.descriptor_digest
            )));
        }
        Ok(())
    }

    /// Resolve an approved implementation for one locked interface. The pinned
    /// executable is preferred, but an operator-approved implementation with
    /// the same descriptor is a valid swap.
    pub fn select_approved_snapshot<'a>(
        &self,
        name: &ToolName,
        snapshots: &'a [ExternalToolSnapshot],
    ) -> Result<&'a ExternalToolSnapshot> {
        let qualified = name.to_string();
        let entry = self.by_name(name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "external tool '{qualified}' is missing from lockfile v2"
            ))
        })?;
        let matching: Vec<&ExternalToolSnapshot> = snapshots
            .iter()
            .filter(|snapshot| {
                snapshot.tool.name == qualified
                    && snapshot.approval.state.is_approved()
                    && snapshot.v2.as_ref().is_some_and(|v2| {
                        v2.descriptor_digest.to_string() == entry.descriptor_digest
                    })
            })
            .collect();
        if let Some(snapshot) = matching.iter().find(|snapshot| {
            snapshot.tool_digest().is_some_and(|digest| {
                self.toolset_for(name)
                    .is_some_and(|set| set.tool_digest == digest.to_string())
            })
        }) {
            return Ok(snapshot);
        }
        matching.into_iter().next().ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "no approved implementation for external tool '{qualified}' matches descriptor digest {}",
                entry.descriptor_digest
            ))
        })
    }
}

fn validate(lockfile: &ExternalToolsLockfile) -> Result<()> {
    let mut names = HashMap::<String, ()>::new();
    for set in &lockfile.toolsets {
        if set.name.trim().is_empty()
            || set.tool_digest.parse::<Digest>().is_err()
            || set.version.trim().is_empty()
        {
            return Err(BamlRtError::InvalidArgument(
                "lockfile v2 toolsets require valid name, version, and tool_digest".to_string(),
            ));
        }
        for tool in &set.tools {
            let key = format!("{}/{}", set.name, tool.name);
            if names.insert(key.clone(), ()).is_some() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool '{}' in lockfile v2",
                    key
                )));
            }
            if tool.name.trim().is_empty() || tool.descriptor_digest.parse::<Digest>().is_err() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "invalid lockfile v2 tool entry '{}'",
                    tool.name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ExternalToolInterfaceLockEntry, ExternalToolsLockfile, ExternalToolsetLockEntry};

    #[test]
    fn v1_is_rejected() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), br#"{"version":"1","tools":[]}"#).unwrap();
        assert!(ExternalToolsLockfile::read_from_path(path.path()).is_err());
    }

    #[test]
    fn same_interface_pin_is_independent_of_tool_digest() {
        let lock = ExternalToolsLockfile {
            version: "2".into(),
            toolsets: vec![ExternalToolsetLockEntry {
                name: "support".into(),
                version: "1.0.0".into(),
                requirement: "^1".into(),
                tier: "sandbox".into(),
                tool_digest: "sha256:old".into(),
                tools: vec![ExternalToolInterfaceLockEntry {
                    name: "support/echo".into(),
                    descriptor_digest: "sha256:interface".into(),
                }],
            }],
        };
        assert_eq!(
            lock.toolsets[0].tools[0].descriptor_digest,
            "sha256:interface"
        );
    }
}
