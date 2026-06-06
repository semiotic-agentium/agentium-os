// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Per-tool runtime lock sidecar (`tool-manifest.lock.json`).
//!
//! Carries host-resolved fields that must NOT live in the committed
//! `tool-manifest.json` source: the canonical absolute bind rootfs path. This
//! value is local to whoever ran `sandbox-bind-sync`, so committing it would
//! force every contributor's checkout to drift.
//!
//! Distinct from [`super::lockfile`], which is a *workspace-level* supply-chain
//! lock that pins package digests across the agent build. This sidecar is
//! per-tool and only carries runtime-launch state.
//!
//! Lifecycle:
//! - written by the `sandbox-bind-sync` CLI command;
//! - read by CLI/runtime helpers when a bind sandbox needs host-local path
//!   resolution;
//! - never required by builder/codegen; schemas/types live in approved snapshots.
//!
//! No version field: the lock is a regenerable per-host cache, not a stable
//! wire contract. If the layout ever needs to change incompatibly, users
//! re-run `sandbox-bind-sync` to regenerate it. The source `tool-manifest.json`
//! already carries `tool_abi_version` for the contract surface.

use std::{
    fs,
    path::{Path, PathBuf},
};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};

/// Sibling file name; lives next to `tool-manifest.json`.
pub const RUNTIME_LOCK_FILE_NAME: &str = "tool-manifest.lock.json";

/// On-disk shape for the per-tool runtime lock.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolRuntimeLock {
    /// Canonical absolute path of the bind rootfs (only set for bind sandbox
    /// runtimes). Other runtime kinds may omit this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path_abs: Option<PathBuf>,
}

impl ToolRuntimeLock {
    pub fn new_bind(image_path_abs: PathBuf) -> Self {
        Self {
            image_path_abs: Some(image_path_abs),
        }
    }

    pub fn write_to_dir(&self, tool_dir: &Path) -> Result<()> {
        let path = lock_path(tool_dir);
        let body = serde_json::to_string_pretty(self).map_err(BamlRtError::Json)?;
        fs::write(&path, format!("{body}\n")).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to write {}", path.display()),
                source: Box::new(e),
            }
        })?;
        Ok(())
    }
}

/// Absolute path to the lock sidecar for a given tool directory.
pub fn lock_path(tool_dir: &Path) -> PathBuf {
    tool_dir.join(RUNTIME_LOCK_FILE_NAME)
}

/// Read the lock sidecar if it exists. Missing file is not an error — callers
/// fall back to source defaults.
pub fn read_runtime_lock(tool_dir: &Path) -> Result<Option<ToolRuntimeLock>> {
    let path = lock_path(tool_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to read {}", path.display()),
        source: Box::new(e),
    })?;
    let parsed: ToolRuntimeLock =
        serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to parse {}", path.display()),
            source: Box::new(e),
        })?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    #[test]
    fn round_trip_bind_lock() {
        let tmp = unique_tmp("runtime-lock-rt");
        let lock = ToolRuntimeLock::new_bind(tmp.join("rootfs"));
        lock.write_to_dir(&tmp).expect("write");

        let read = read_runtime_lock(&tmp)
            .expect("read")
            .expect("lock present");
        assert_eq!(read.image_path_abs.unwrap(), tmp.join("rootfs"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_lock_is_none() {
        let tmp = unique_tmp("runtime-lock-missing");
        assert!(read_runtime_lock(&tmp).expect("ok").is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_unknown_fields() {
        let tmp = unique_tmp("runtime-lock-unknown");
        fs::write(
            lock_path(&tmp),
            r#"{"legacy_digest":"sha256:abcd","unknown":"x"}"#,
        )
        .unwrap();
        let err = read_runtime_lock(&tmp).expect_err("must reject unknown field");
        assert!(err.to_string().contains("unknown"));
        let _ = fs::remove_dir_all(&tmp);
    }
}
