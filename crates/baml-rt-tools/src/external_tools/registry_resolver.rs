// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Resolver for approved v2 external tools: bundled toolsets served by
//! [`super::host::ToolHost`] and Service references served by
//! [`super::host::ServiceHost`].

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};

use super::{ExternalToolSnapshot, snapshot::validate_external_tool_snapshot};
use crate::{
    ExternalToolResolver, ToolName,
    tools::{ToolFunctionMetadata, ToolHandler},
};

pub struct ExternalRegistryResolver {
    entries: std::collections::HashMap<ToolName, RegistryToolEntry>,
}

struct RegistryToolEntry {
    metadata: ToolFunctionMetadata,
    handler: Arc<dyn ToolHandler>,
}

impl ExternalRegistryResolver {
    /// Register repository-loaded v2 bundles through the host authority.
    pub fn from_v2_bundles(
        host: Arc<super::host::ToolHost>,
        bundles: Vec<(ExternalToolSnapshot, Arc<[u8]>)>,
    ) -> Result<Self> {
        let mut entries = std::collections::HashMap::new();
        for (snapshot, bytes) in bundles {
            let name = ToolName::parse(&snapshot.tool.name)?;
            let handler = host.handler(snapshot, bytes)?;
            let metadata = handler.metadata().clone();
            if entries
                .insert(
                    name.clone(),
                    RegistryToolEntry {
                        metadata,
                        handler: Arc::new(handler),
                    },
                )
                .is_some()
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool snapshot '{name}'"
                )));
            }
        }
        Ok(Self { entries })
    }

    /// Register approved Service references through a client-backed host.
    /// Unlike bundled v2 tools, this path does not load or verify executable
    /// bytes. The ServiceHost performs the lazy live-schema check on invoke.
    pub fn from_v2_services(
        host: Arc<super::host::ServiceHost>,
        snapshots: Vec<ExternalToolSnapshot>,
    ) -> Result<Self> {
        let mut entries = std::collections::HashMap::new();
        for snapshot in snapshots {
            if !snapshot.approval.state.is_approved() {
                continue;
            }
            validate_external_tool_snapshot(&snapshot)?;
            let name = ToolName::parse(&snapshot.tool.name)?;
            let handler = host.handler(snapshot)?;
            let metadata = handler.metadata().clone();
            if entries
                .insert(
                    name.clone(),
                    RegistryToolEntry {
                        metadata,
                        handler: Arc::new(handler),
                    },
                )
                .is_some()
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool snapshot '{name}'"
                )));
            }
        }
        Ok(Self { entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ExternalToolResolver for ExternalRegistryResolver {
    fn resolve(
        &self,
        name: &ToolName,
    ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>> {
        let Some(entry) = self.entries.get(name) else {
            return Ok(None);
        };
        Ok(Some((entry.metadata.clone(), entry.handler.clone())))
    }
}
