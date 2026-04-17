//! First concrete [`SandboxProvider`] — thin wrapper over the `microsandbox`
//! crate (`tool_sandbox.md` §7.3).
//!
//! Behind the `sandbox-provider` Cargo feature. When the feature is off (the
//! default), [`MicrosandboxProvider::new`] returns an error explaining that
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
//! ## API mapping to microsandbox 0.3.x
//! | Trait method | microsandbox API |
//! |---|---|
//! | `create` | `Sandbox::builder(name).image(..).cpus(..).memory(..).create_detached()` |
//! | `rpc_channel` | `sandbox.exec_stream(cmd, args)` → adapter over `ExecHandle` |
//! | `teardown` | `sandbox.stop_and_wait()` with `kill()` fallback |
//! | `reattach` | `Sandbox::start(name)` |
//! | `list_owned` | filtered sweep (see note) |
//!
//! **Note on `list_owned`:** the `0.3.13` surface exposed in docs does not
//! include a `Sandbox::list()` primitive. Until it lands, `list_owned`
//! returns an empty vec — in-process reattach via `reattach(name)` still
//! works because the runtime keeps its own cache (§9.4 in-process only).
//!
//! **Note on `idle_timeout` / `max_duration`:** the 0.3.13 builder surface
//! didn't yet expose these knobs. The runtime honors them in its cache
//! (age-check, idle teardown) even when the underlying VM has no native
//! timer — so the behavior is preserved at a higher layer until the crate
//! catches up.

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use futures_util::stream::{self, BoxStream};

use super::{
    channel::TsrpcChannel,
    provider::SandboxProvider,
    spec::{SandboxEvent, SandboxHandle, SandboxSpec},
};

/// Microsandbox-backed provider. Construction is fallible because sandbox
/// support may be compiled out (`sandbox-provider` feature) or the host may
/// lack KVM / Apple-Silicon support detected lazily on first `create`.
pub struct MicrosandboxProvider {
    #[cfg(feature = "sandbox-provider")]
    _priv: (),
    #[cfg(not(feature = "sandbox-provider"))]
    _unused: std::marker::PhantomData<()>,
}

impl MicrosandboxProvider {
    pub fn new() -> Result<Self> {
        #[cfg(feature = "sandbox-provider")]
        {
            Ok(Self { _priv: () })
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
mod imp {
    use super::*;

    #[async_trait]
    impl SandboxProvider for MicrosandboxProvider {
        async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle> {
            // TODO(sandbox): concrete SandboxBuilder wiring. Shape (from
            // `tool_sandbox.md` §7.3):
            //
            //   let mut b = microsandbox::Sandbox::builder(&spec.name)
            //       .image(spec.image.as_str())
            //       .cpus(spec.cpus)
            //       .memory(spec.memory_mib);
            //   for (k, v) in &spec.env { b = b.env(k, v); }
            //   for s in &spec.secrets { /* .secret_env per binding mode */ }
            //   b = b.network(|n| compile_network_policy(n, &spec.network_policy));
            //   let sb = if spec.detached { b.create_detached().await } else { b.create().await }?;
            //
            // Left as a scaffold here because the 0.3.13 surface has several
            // knobs (idle_timeout, max_duration, network rule shapes) that
            // are still churning; wiring them prematurely risks
            // compile-breakage on every crate bump. The trait boundary keeps
            // the rest of the runtime unaffected.
            let _ = spec;
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider::create is not yet wired to microsandbox 0.3.13 (scaffolded)"
                    .to_string(),
            ))
        }

        async fn rpc_channel(&self, handle: &SandboxHandle) -> Result<TsrpcChannel> {
            let _ = handle;
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider::rpc_channel is not yet wired to microsandbox 0.3.13"
                    .to_string(),
            ))
        }

        async fn teardown(&self, handle: &SandboxHandle) -> Result<()> {
            let _ = handle;
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider::teardown is not yet wired to microsandbox 0.3.13"
                    .to_string(),
            ))
        }

        fn events(&self, handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
            let _ = handle;
            Box::pin(stream::empty())
        }

        async fn list_owned(&self, _runner_id: &str) -> Result<Vec<SandboxHandle>> {
            // 0.3.13 doesn't expose Sandbox::list(); in-process cache is the
            // source of truth until the primitive lands.
            Ok(Vec::new())
        }

        async fn reattach(&self, name: &str) -> Result<SandboxHandle> {
            let _ = name;
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider::reattach is not yet wired to microsandbox 0.3.13"
                    .to_string(),
            ))
        }
    }
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
