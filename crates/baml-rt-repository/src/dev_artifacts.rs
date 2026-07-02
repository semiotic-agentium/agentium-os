// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Server-generated dev artifacts (BAML prelude + TypeScript stubs) keyed by package hash.

use std::str::FromStr;

use baml_rt_hash::ContentHash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::AgentName;

/// Bundle captured during repository publish build.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DevArtifactsBundle {
    pub baml_runtime: String,
    pub baml_runtime_dts: String,
}

/// Deterministic blob key for dev artifacts associated with a package content hash.
pub fn dev_artifacts_blob_hash(package_hash: &ContentHash) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(b"agentium:dev-artifacts:v1\0");
    hasher.update(package_hash.as_str().as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ContentHash::from_str(&hex).expect("sha256 hex is valid ContentHash")
}

/// Resolve package hash from query parameters (explicit hash or latest publish for agent name).
pub async fn resolve_package_hash(
    svc: &crate::service::RepositoryService,
    agent: Option<&str>,
    hash: Option<&str>,
) -> crate::Result<Option<ContentHash>> {
    if let Some(h) = hash {
        return Ok(Some(ContentHash::from_str(h).map_err(|_| {
            crate::error::RepositoryError::InvalidSourceBundle {
                reason: format!("invalid content hash: {h}"),
            }
        })?));
    }
    if let Some(name) = agent {
        let agent_name = AgentName::from_str(name).map_err(|e| {
            crate::error::RepositoryError::InvalidSourceBundle {
                reason: format!("invalid agent name: {e}"),
            }
        })?;
        let latest = svc.get_latest(&agent_name).await?;
        return Ok(latest.map(|entry| entry.hash));
    }
    Ok(None)
}
