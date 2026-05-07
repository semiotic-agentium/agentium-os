//! Runner-side wiring helpers (`tool_sandbox.md` Workstream Y).
//!
//! Turns a runner bootstrap into a full [`SandboxRuntimeWiring`] ready to
//! hand to [`DevModeResolver::from_dirs_with_sandbox`](super::super::resolver::DevModeResolver::from_dirs_with_sandbox).
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

/// Fresh `runner_id` per process boot. No persistence — microsandbox's
/// `idle_timeout` reaps the previous runner's orphans in the background
/// (§9.2).
pub fn fresh_runner_id() -> String {
    Uuid::new_v4().to_string()
}

/// Stock [`SandboxSpecFactory`] that turns parsed metadata into a spec
/// builder, suitable for early-stage rollouts before policy/secrets
/// plumbing lands.
///
/// Behavior:
/// - Reads `meta.runtime` (must be `Sandbox`) for `image` + `entrypoint`.
/// - Carries `meta.runtime_digest` onto the spec for the §9.4 reattach
///   checklist.
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
    let runtime_digest = meta.runtime_digest.clone();
    let secret_env = build_secret_env(meta);

    Ok(Arc::new(move |key: &SandboxCacheKey| {
        Ok(build_stock_spec(
            cache.encode_name(key),
            &image,
            &guest_workdir,
            &entrypoint,
            runtime_digest.clone(),
            secret_env.clone(),
        ))
    }))
}

fn build_stock_spec(
    name: String,
    image: &SandboxImageSource,
    guest_workdir: &str,
    entrypoint: &[String],
    runtime_digest: Option<String>,
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
        runtime_digest,
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
