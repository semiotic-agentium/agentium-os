// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runner-side wiring helpers (`tool_sandbox.md` Workstream Y).
//!
//! Turns a runner bootstrap into a full [`SandboxRuntimeWiring`] ready for
//! registry-backed external-tool loading.
//!
//! What's *stub* in this pass (Workstream D will tighten):
//! - `NetworkPolicy` defaults to empty/deny-all — no allow rules compiled
//!   from metadata capabilities yet.
//! - Secrets default to empty — resolver integration is a D concern.
//! - Resource limits default to the §10.3 suggested values.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use baml_rt_core::{BamlRtError, Result};
use tracing::info;
use uuid::Uuid;

use super::{
    invoker::{SandboxCache, SandboxCacheKey, SandboxSpecBuilder},
    path_guard::canonicalize_bind_path,
    provider::SandboxProvider,
    spec::{PullPolicy, SandboxImageSource, SandboxSpec},
};
use crate::{
    ToolName,
    external_tools::{
        metadata::ExternalToolMetadata,
        resolver::{SandboxRuntimeWiring, SandboxSpecFactory},
        runtime::{SandboxImageRef, ToolRuntime},
    },
};

/// Default idle timeout (§10.3 suggested default — 5 min).
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// Default max duration (§10.3 suggested default — 1 h).
pub const DEFAULT_MAX_DURATION_SECS: u64 = 3600;

/// Fresh `runner_id` per process boot. Nothing persists it, so VMs a previous
/// boot left behind are never recognised as this runner's: stopped VMs keep
/// their rootfs until removed. Long-lived runners use
/// [`load_or_create_runner_id`] instead.
pub fn fresh_runner_id() -> String {
    Uuid::new_v4().to_string()
}

const RUNNER_ID_FILE: &str = "tool-runner-id";

/// Runner identity that survives restarts: read `<state_dir>/tool-runner-id`,
/// or create it with a fresh UUID. It owns this runner's tool VMs (VM names
/// and the owner label), so a restarted runner can reclaim what its previous
/// run left behind without touching VMs of another runner.
pub fn load_or_create_runner_id(state_dir: &std::path::Path) -> Result<String> {
    let path = state_dir.join(RUNNER_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let id = raw.trim();
            Uuid::parse_str(id).map_err(|_| {
                BamlRtError::InvalidArgument(format!(
                    "{} does not hold a runner UUID; remove it to mint a new identity",
                    path.display()
                ))
            })?;
            Ok(id.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(state_dir).map_err(|e| {
                BamlRtError::InvalidArgument(format!("create {}: {e}", state_dir.display()))
            })?;
            let id = fresh_runner_id();
            // Write-then-rename so a crash never leaves a truncated identity.
            let tmp = state_dir.join(format!("{RUNNER_ID_FILE}.tmp"));
            std::fs::write(&tmp, format!("{id}\n"))
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!("write {}: {e}", path.display()))
                })?;
            Ok(id)
        }
        Err(error) => Err(BamlRtError::InvalidArgument(format!(
            "read {}: {error}",
            path.display()
        ))),
    }
}

/// Stock [`SandboxSpecFactory`] that turns parsed metadata into a spec
/// builder, suitable for early-stage rollouts before policy/secrets
/// plumbing lands.
///
/// Behavior:
/// - Reads `meta.runtime` (must be `Sandbox`) for `image` + `entrypoint`.
/// - Uses deny-all `NetworkPolicy`, no secrets, no volumes.
/// - Uses §10.3 default idle / max durations.
///
/// Workstream D plugs policy compilation + secret resolution behind the
/// same `SandboxSpecFactory` surface — the resolver doesn't change.
pub fn default_spec_factory(cache: Arc<SandboxCache>) -> SandboxSpecFactory {
    default_spec_factory_with_bind_roots(cache, Vec::new())
}

pub fn default_spec_factory_with_bind_roots(
    cache: Arc<SandboxCache>,
    bind_roots: Vec<PathBuf>,
) -> SandboxSpecFactory {
    Arc::new(move |tool_name, meta| {
        build_default_spec_builder(cache.clone(), tool_name, meta, &bind_roots)
    })
}

fn build_default_spec_builder(
    cache: Arc<SandboxCache>,
    tool_name: &ToolName,
    meta: &ExternalToolMetadata,
    bind_roots: &[PathBuf],
) -> Result<SandboxSpecBuilder> {
    let Some(ToolRuntime::Sandbox(sandbox_spec)) = meta.runtime.as_ref() else {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool '{tool_name}' reached default sandbox spec factory without a sandbox runtime declaration"
        )));
    };

    let image = match &sandbox_spec.image {
        SandboxImageRef::Oci { r#ref } => {
            info!(tool = %tool_name, image_source = "oci", image_ref = %r#ref, "external tool sandbox image source selected");
            SandboxImageSource::Oci(r#ref.clone())
        }
        SandboxImageRef::Bind { path } => {
            let canonical = canonicalize_bind_path(path, bind_roots)?;
            info!(
                tool = %tool_name,
                image_source = "bind",
                bind_path_raw = %path.display(),
                bind_path_canonical = %canonical.display(),
                "external tool sandbox image source selected"
            );
            SandboxImageSource::Bind(canonical)
        }
    };
    let entrypoint = sandbox_spec.entrypoint.clone();
    let guest_workdir = sandbox_spec
        .adapter
        .as_ref()
        .and_then(|adapter| adapter.workdir.clone())
        .unwrap_or_else(|| "/".to_string());
    let secret_env = build_secret_env(meta);

    Ok(Arc::new(move |key: &SandboxCacheKey| {
        Ok(build_stock_spec(
            cache.encode_name(key),
            &image,
            &guest_workdir,
            &entrypoint,
            secret_env.clone(),
        ))
    }))
}

fn build_stock_spec(
    name: String,
    image: &SandboxImageSource,
    guest_workdir: &str,
    entrypoint: &[String],
    env: BTreeMap<String, String>,
) -> SandboxSpec {
    SandboxSpec {
        name,
        image: image.clone(),
        guest_workdir: guest_workdir.to_string(),
        cpus: 1,
        memory_mib: 512,
        env,
        volumes: Vec::new(),
        port_mappings: Vec::new(),
        network_policy: Default::default(),
        secrets: Vec::new(),
        scripts: Default::default(),
        idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
        max_duration: Duration::from_secs(DEFAULT_MAX_DURATION_SECS),
        detached: true,
        pull_policy: PullPolicy::IfMissing,
        entrypoint: entrypoint.to_vec(),
        policy_hash: None,
    }
}

fn build_secret_env(meta: &ExternalToolMetadata) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for secret_name in &meta.secrets {
        if let Ok(value) = std::env::var(secret_name)
            && !value.trim().is_empty()
        {
            out.insert(secret_name.clone(), value);
        }
    }
    out
}

/// Convenience: build a full [`SandboxRuntimeWiring`] for a given provider
/// using the stock spec factory and a fresh runner id. The runner calls this
/// once at boot when it has decided which [`SandboxProvider`] impl to use
/// (microsandbox, mock, future docker).
pub fn stock_wiring(
    provider: Arc<dyn SandboxProvider>,
    runner_id: impl Into<String>,
) -> SandboxRuntimeWiring {
    stock_wiring_with_bind_roots(provider, runner_id, Vec::new())
}

pub fn stock_wiring_with_bind_roots(
    provider: Arc<dyn SandboxProvider>,
    runner_id: impl Into<String>,
    bind_roots: Vec<PathBuf>,
) -> SandboxRuntimeWiring {
    let cache = Arc::new(SandboxCache::new(runner_id));
    let spec_factory = default_spec_factory_with_bind_roots(cache.clone(), bind_roots);
    SandboxRuntimeWiring {
        provider,
        cache,
        spec_factory,
    }
}

#[cfg(test)]
mod runner_id_tests {
    use super::*;

    #[test]
    fn runner_id_is_created_once_and_survives_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let first = load_or_create_runner_id(&state).unwrap();
        assert!(Uuid::parse_str(&first).is_ok());
        assert_eq!(load_or_create_runner_id(&state).unwrap(), first);
        assert!(!state.join(format!("{RUNNER_ID_FILE}.tmp")).exists());
    }

    #[test]
    fn corrupt_runner_id_fails_instead_of_minting_a_new_owner() {
        // A silently re-minted id would orphan every VM the old id owns.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RUNNER_ID_FILE), "not-a-uuid\n").unwrap();
        assert!(load_or_create_runner_id(dir.path()).is_err());
    }
}
