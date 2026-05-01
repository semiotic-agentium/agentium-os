//! First concrete [`SandboxProvider`] — thin wrapper over the `microsandbox`
//! crate (`tool_sandbox.md` §7.3).
//!
//! Behind the `sandbox-provider` Cargo feature. When the feature is off
//! (the default), [`MicrosandboxProvider::new`] returns an error explaining
//! sandbox support wasn't compiled in. This is a pragmatic beta-containment
//! measure and does not contradict §15's "opt-in by metadata declaration"
//! rule — metadata still drives activation; the feature gate only controls
//! whether the provider can be built at all.
//!
//! ## Platform constraints (§15)
//! - Linux with KVM (primary)
//! - Apple Silicon macOS (dev)
//! - Intel Mac / Windows: unsupported
//!
//! ## API mapping to microsandbox 0.3.13
//! | Trait method | microsandbox API |
//! |---|---|
//! | `create` | `Sandbox::builder(name)...create_detached()` (or `.create()`) |
//! | `rpc_channel` | `Sandbox::exec_stream_with(.., stdin_pipe)` → adapter |
//! | `teardown` | `Sandbox::stop_and_wait()` with `kill()` fallback |
//! | `reattach` | `Sandbox::start(name)` + env-var metadata recovery |
//! | `list_owned` | `Sandbox::list()` filtered by `baml:<runner_id>:` prefix |
//!
//! ## Reattach metadata stash (§9.4)
//!
//! `runtime_digest` and `policy_hash` must survive a cache rebuild so the
//! §9.4 validation checklist can run. We stash them as guest env vars at
//! create time:
//!
//! - `BAML_RUNTIME_DIGEST`
//! - `BAML_POLICY_HASH`
//! - `BAML_MAX_DURATION_SECS`
//!
//! On reattach, the provider does a lightweight `exec` into the guest to
//! read them back. This is cheap and avoids adding a new sidecar protocol.

#[cfg(feature = "sandbox-provider")]
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
#[cfg(feature = "sandbox-provider")]
use dashmap::DashMap;
use futures_util::stream::{self, BoxStream};
#[cfg(feature = "sandbox-provider")]
use tracing::{debug, info, warn};

use super::{
    channel::TsrpcChannel,
    provider::SandboxProvider,
    spec::{SandboxEvent, SandboxHandle, SandboxSpec},
};
#[cfg(feature = "sandbox-provider")]
use super::{
    exec_adapter::exec_handle_into_channel,
    spec::{PullPolicy, SandboxImageSource, SecretBindingMode},
};
#[cfg(feature = "sandbox-provider")]
use crate::external_tools::{read_sidecar_bundle, verify_runtime_digest};

/// Env-var names used to stash reattach metadata inside the guest. Chosen
/// under the `BAML_` prefix so they don't collide with tool-author vars.
#[cfg(feature = "sandbox-provider")]
const STASH_RUNTIME_DIGEST: &str = "BAML_RUNTIME_DIGEST";
#[cfg(feature = "sandbox-provider")]
const STASH_POLICY_HASH: &str = "BAML_POLICY_HASH";
#[cfg(feature = "sandbox-provider")]
const STASH_MAX_DURATION_SECS: &str = "BAML_MAX_DURATION_SECS";
#[cfg(feature = "sandbox-provider")]
const SIDECAR_BUNDLE_PATH: &str = "etc/agentium/tool-bundle.json";

/// How long to wait for `stop_and_wait` before falling back to `kill`.
#[cfg(feature = "sandbox-provider")]
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Microsandbox-backed provider. Construction is fallible because sandbox
/// support may be compiled out (`sandbox-provider` feature).
pub struct MicrosandboxProvider {
    #[cfg(feature = "sandbox-provider")]
    live: Arc<DashMap<String, microsandbox::Sandbox>>,
    #[cfg(not(feature = "sandbox-provider"))]
    _unused: std::marker::PhantomData<()>,
}

/// Verify that the bind rootfs sidecar bundle at
/// `/etc/agentium/tool-bundle.json` advertises the same `runtime_digest`
/// the host metadata carries.
///
/// ## What this catches
/// Staleness: rootfs was materialized against older metadata and never
/// re-synced with `sandbox-bind-sync`. This is the common failure mode
/// (developer edits metadata, forgets to re-sync) and gives a clear error
/// instead of a downstream tool/invoke timeout.
///
/// ## What this does NOT catch
/// Tampering. The sidecar itself embeds the expected digest value, so an
/// attacker who can rewrite rootfs contents can also rewrite the digest
/// field to match whatever they want. The trust anchor is `expected_digest`
/// — which comes from **host-side tool metadata**, not from the sidecar —
/// so this check is only as strong as the host metadata's integrity.
/// For tamper-resistance, sign the metadata or ship a signed sidecar.
#[cfg(feature = "sandbox-provider")]
fn verify_bind_sidecar_runtime_digest(
    bind_root: &Path,
    expected_digest: Option<&str>,
) -> Result<()> {
    let Some(expected) = expected_digest else {
        return Ok(());
    };

    let sidecar = bind_root.join(SIDECAR_BUNDLE_PATH);
    let bundle = read_sidecar_bundle(&sidecar)?;
    verify_runtime_digest(&bundle, expected).map_err(|e| {
        BamlRtError::InvalidArgument(format!(
            "bind sidecar runtime_digest mismatch at {}: {e}",
            sidecar.display()
        ))
    })
}

impl MicrosandboxProvider {
    pub fn new() -> Result<Self> {
        #[cfg(feature = "sandbox-provider")]
        {
            Ok(Self {
                live: Arc::new(DashMap::new()),
            })
        }
        #[cfg(not(feature = "sandbox-provider"))]
        {
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider requires the 'sandbox-provider' feature to be enabled"
                    .to_string(),
            ))
        }
    }
}

#[cfg(feature = "sandbox-provider")]
fn to_rt_err<E: std::fmt::Display>(context: &str, err: E) -> BamlRtError {
    BamlRtError::InvalidArgument(format!("{context}: {err}"))
}

#[cfg(feature = "sandbox-provider")]
fn clamp_cpus(cpus: u32) -> u8 {
    // microsandbox's `.cpus` takes u8; our spec uses u32. Clamp with a warn
    // so oversized requests don't silently wrap.
    if cpus == 0 {
        warn!(requested = cpus, "cpus=0 requested; clamping to 1");
        1
    } else if cpus > u8::MAX as u32 {
        warn!(
            requested = cpus,
            max = u8::MAX,
            "cpus above u8::MAX; clamping"
        );
        u8::MAX
    } else {
        cpus as u8
    }
}

// PullPolicy mapping lives inline in `create()` because the concrete
// microsandbox_image::PullPolicy type isn't re-exported from microsandbox
// 0.3.13, and adding it as a direct dep would widen our surface without
// value. The builder's default is `IfMissing`, which is also our default,
// so non-default policies fall through to a no-op until §D.

#[cfg(feature = "sandbox-provider")]
#[async_trait]
impl SandboxProvider for MicrosandboxProvider {
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle> {
        let name = spec.name.clone();
        let max_duration = spec.max_duration;
        let runtime_digest = spec.runtime_digest.clone();
        let policy_hash = spec.policy_hash.clone();

        if matches!(
            (&spec.image, spec.pull_policy),
            (SandboxImageSource::Bind(_), PullPolicy::Always)
        ) {
            return Err(BamlRtError::InvalidArgument(
                "pull_policy=always is invalid for bind sandbox images".to_string(),
            ));
        }

        let mut builder = microsandbox::Sandbox::builder(&name)
            .cpus(clamp_cpus(spec.cpus))
            .memory(spec.memory_mib)
            .workdir("/home/sandbox/workspace")
            .idle_timeout(spec.idle_timeout.as_secs())
            .max_duration(spec.max_duration.as_secs())
            .replace(); // idempotent by name-replace per trait contract

        builder = match &spec.image {
            SandboxImageSource::Oci(image) => {
                info!(sandbox = %name, image_source = "oci", image_ref = %image, "creating sandbox with OCI image");
                builder.image(image.as_str())
            }
            SandboxImageSource::Bind(path) => {
                verify_bind_sidecar_runtime_digest(path, runtime_digest.as_deref())?;
                info!(sandbox = %name, image_source = "bind", bind_path = %path.display(), "creating sandbox with bind rootfs");
                builder.image(path.clone())
            }
        };

        let _ = spec.pull_policy; // see comment above map_pull_policy

        for (k, v) in &spec.env {
            builder = builder.env(k, v);
        }

        // Match the working probe's baseline runtime env unless the tool spec
        // already set an explicit value.
        for (k, v) in [
            ("HOME", "/home/sandbox"),
            ("USER", "sandbox"),
            ("PWD", "/home/sandbox/workspace"),
            ("XDG_CONFIG_HOME", "/home/sandbox/.config"),
            ("XDG_CACHE_HOME", "/home/sandbox/.cache"),
            ("TMPDIR", "/tmp"),
        ] {
            if !spec.env.contains_key(k) {
                builder = builder.env(k, v);
            }
        }

        // Reattach-metadata stash (§9.4 — runtime_digest / policy_hash /
        // max_duration recovered via env inside the guest on reattach).
        if let Some(digest) = &runtime_digest {
            builder = builder.env(STASH_RUNTIME_DIGEST, digest);
        }
        if let Some(hash) = &policy_hash {
            builder = builder.env(STASH_POLICY_HASH, hash);
        }
        builder = builder.env(
            STASH_MAX_DURATION_SECS,
            spec.max_duration.as_secs().to_string(),
        );

        // Secrets — create-time egress-bound bindings injected via
        // microsandbox placeholder substitution (§10.1 primary path).
        // Per-invoke bindings are carried by the runner on invoke payload,
        // not here.
        for s in &spec.secrets {
            if let SecretBindingMode::EgressBound { allow_hosts } = &s.binding {
                for host in allow_hosts {
                    builder = builder.secret_env(&s.env_var, &s.value, host);
                }
            }
        }

        // Entrypoint override (when metadata declares one).
        if !spec.entrypoint.is_empty() {
            builder = builder.entrypoint(spec.entrypoint.iter().cloned());
        }

        // Ports.
        for port in &spec.port_mappings {
            builder = builder.port(port.host, port.guest);
        }

        // Scripts — optional `/.msb/scripts/` entries.
        for (name, content) in &spec.scripts {
            builder = builder.script(name, content);
        }

        // NetworkPolicy: until metadata/runner-policy compilation is wired,
        // default to `public_only()` so outbound internet APIs work in
        // sandboxed tools while still denying loopback/link-local/private/
        // metadata destinations.
        builder = builder.network(|n| n.policy(microsandbox::NetworkPolicy::public_only()));

        let sandbox = if spec.detached {
            builder.create_detached().await.map_err(|e| {
                to_rt_err(&format!("microsandbox create_detached '{name}' failed"), e)
            })?
        } else {
            builder
                .create()
                .await
                .map_err(|e| to_rt_err(&format!("microsandbox create '{name}' failed"), e))?
        };

        self.live.insert(name.clone(), sandbox);
        debug!(sandbox = %name, detached = spec.detached, "sandbox created");

        Ok(SandboxHandle {
            name,
            created_at: SystemTime::now(),
            runtime_digest,
            policy_hash,
            max_duration,
        })
    }

    async fn rpc_channel(&self, handle: &SandboxHandle) -> Result<TsrpcChannel> {
        let sandbox = self.live.get(&handle.name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "MicrosandboxProvider has no live sandbox named '{}'; call create/reattach first",
                handle.name
            ))
        })?;

        // TSRPC transport contract: run the tool adapter with stdin_pipe
        // enabled so it can stream JSON-RPC responses back.
        //
        // Bind-rootfs images often place the binary at `/tool-adapter`
        // (distroless style) without PATH entries; prefer absolute path and
        // fall back to PATH lookup for compatibility.
        let exec_handle = match sandbox
            .exec_stream_with("/tool-adapter", |opts| {
                opts.cwd("/home/sandbox/workspace").stdin_pipe()
            })
            .await
        {
            Ok(h) => h,
            Err(abs_err) => {
                warn!(
                    sandbox = %handle.name,
                    error = ?abs_err,
                    "exec '/tool-adapter' failed; retrying with PATH lookup 'tool-adapter'"
                );
                sandbox
                    .exec_stream_with("tool-adapter", |opts| {
                        opts.cwd("/home/sandbox/workspace").stdin_pipe()
                    })
                    .await
                    .map_err(|e| {
                        to_rt_err(
                            &format!(
                                "microsandbox exec_stream for sandbox '{}' failed",
                                handle.name
                            ),
                            e,
                        )
                    })?
            }
        };

        exec_handle_into_channel(exec_handle).map_err(|msg| {
            BamlRtError::InvalidArgument(format!(
                "sandbox '{}' exec-to-channel adapter failed: {msg}",
                handle.name
            ))
        })
    }

    async fn teardown(&self, handle: &SandboxHandle) -> Result<()> {
        let Some((_, sandbox)) = self.live.remove(&handle.name) else {
            // "Already gone" is fine per SandboxProvider contract.
            return Ok(());
        };

        match tokio::time::timeout(STOP_TIMEOUT, sandbox.stop_and_wait()).await {
            Ok(Ok(_status)) => {
                debug!(sandbox = %handle.name, "sandbox stopped cleanly");
                Ok(())
            }
            Ok(Err(err)) => {
                warn!(sandbox = %handle.name, ?err, "stop_and_wait failed; falling back to kill");
                if let Err(k) = sandbox.kill().await {
                    warn!(sandbox = %handle.name, ?k, "kill fallback also failed");
                }
                // teardown is best-effort per trait contract
                Ok(())
            }
            Err(_timeout) => {
                warn!(
                    sandbox = %handle.name,
                    timeout_secs = STOP_TIMEOUT.as_secs(),
                    "stop_and_wait timed out; falling back to kill"
                );
                if let Err(k) = sandbox.kill().await {
                    warn!(sandbox = %handle.name, ?k, "kill fallback failed after timeout");
                }
                Ok(())
            }
        }
    }

    fn events(&self, _handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
        // microsandbox 0.3.13 doesn't expose a per-sandbox lifecycle event
        // stream as a public API. Until it lands, consumers rely on
        // observability spans emitted by our provider methods + the
        // ExternalLifecycleRecorder on the resolver.
        Box::pin(stream::empty())
    }

    async fn list_owned(&self, runner_id: &str) -> Result<Vec<SandboxHandle>> {
        let long_prefix = format!("baml:{runner_id}:");
        let short_prefix = format!("baml:{}:", runner_id.chars().take(8).collect::<String>());
        let all = microsandbox::Sandbox::list()
            .await
            .map_err(|e| to_rt_err("microsandbox Sandbox::list() failed", e))?;
        Ok(all
            .into_iter()
            .filter(|h| {
                let n = h.name();
                n.starts_with(&long_prefix) || n.starts_with(&short_prefix)
            })
            .map(|h| SandboxHandle {
                name: h.name().to_string(),
                created_at: h
                    .created_at()
                    .map(|dt| {
                        SystemTime::UNIX_EPOCH + Duration::from_secs(dt.timestamp().max(0) as u64)
                    })
                    .unwrap_or_else(SystemTime::now),
                // Metadata stash recovery is done lazily on reattach(); list
                // returns name-only handles.
                runtime_digest: None,
                policy_hash: None,
                max_duration: Duration::from_secs(0),
            })
            .collect())
    }

    async fn reattach(&self, name: &str) -> Result<SandboxHandle> {
        // `Sandbox::start(name)` reconnects to a detached sandbox. If it was
        // never detached or is gone, this errors — caller treats as a
        // reattach miss and cold-creates.
        let sandbox = microsandbox::Sandbox::start(name)
            .await
            .map_err(|e| to_rt_err(&format!("microsandbox Sandbox::start('{name}') failed"), e))?;

        // Recover stashed metadata by running `printenv` on the guest. One
        // exec, parse the block, bail gracefully if anything is missing.
        let (runtime_digest, policy_hash, max_duration) = recover_reattach_metadata(&sandbox)
            .await
            .unwrap_or_else(|e| {
                debug!(?e, sandbox = %name, "reattach metadata recovery failed; using defaults");
                (None, None, Duration::from_secs(0))
            });

        self.live.insert(name.to_string(), sandbox);

        Ok(SandboxHandle {
            name: name.to_string(),
            // `created_at` on reattach is "best-effort now" — the real
            // creation time can be recovered via Sandbox::get(name) if
            // callers need it later; §9.4 age check uses max_duration anyway.
            created_at: SystemTime::now(),
            runtime_digest,
            policy_hash,
            max_duration,
        })
    }
}

/// Run `env` / `printenv` inside the guest to recover the stashed reattach
/// metadata (§9.4). Failure is treated as "no metadata available" so the
/// caller can fall back to cold-create semantics.
#[cfg(feature = "sandbox-provider")]
async fn recover_reattach_metadata(
    sandbox: &microsandbox::Sandbox,
) -> Result<(Option<String>, Option<String>, Duration)> {
    let output = sandbox
        .exec("printenv", std::iter::empty::<&str>())
        .await
        .map_err(|e| to_rt_err("printenv exec failed", e))?;
    let stdout = output
        .stdout()
        .map_err(|e| to_rt_err("printenv stdout not valid utf-8", e))?;

    let mut runtime_digest = None;
    let mut policy_hash = None;
    let mut max_duration_secs: u64 = 0;

    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix(&format!("{STASH_RUNTIME_DIGEST}="))
            && !v.is_empty()
        {
            runtime_digest = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix(&format!("{STASH_POLICY_HASH}="))
            && !v.is_empty()
        {
            policy_hash = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix(&format!("{STASH_MAX_DURATION_SECS}="))
            && let Ok(n) = v.parse()
        {
            max_duration_secs = n;
        }
    }

    Ok((
        runtime_digest,
        policy_hash,
        Duration::from_secs(max_duration_secs),
    ))
}

#[cfg(not(feature = "sandbox-provider"))]
#[async_trait]
impl SandboxProvider for MicrosandboxProvider {
    async fn create(&self, _spec: SandboxSpec) -> Result<SandboxHandle> {
        Err(BamlRtError::InvalidArgument(
            "sandbox-provider feature is disabled".to_string(),
        ))
    }
    async fn rpc_channel(&self, _handle: &SandboxHandle) -> Result<TsrpcChannel> {
        Err(BamlRtError::InvalidArgument(
            "sandbox-provider feature is disabled".to_string(),
        ))
    }
    async fn teardown(&self, _handle: &SandboxHandle) -> Result<()> {
        Ok(())
    }
    fn events(&self, _handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
        Box::pin(stream::empty())
    }
    async fn list_owned(&self, _runner_id: &str) -> Result<Vec<SandboxHandle>> {
        Ok(Vec::new())
    }
    async fn reattach(&self, _name: &str) -> Result<SandboxHandle> {
        Err(BamlRtError::InvalidArgument(
            "sandbox-provider feature is disabled".to_string(),
        ))
    }
}
