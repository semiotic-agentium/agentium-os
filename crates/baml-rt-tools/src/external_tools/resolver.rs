// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared resolver plumbing for external-tool sandbox runtimes.
//!
//! Source-dir loading now flows through [`super::registry_resolver::ExternalRegistryResolver`]
//! and approved snapshots. This module intentionally contains no dev-mode
//! filesystem resolver and no `tool-manifest.json` loading path.

use std::sync::Arc;

use baml_rt_core::Result;

use super::{
    ExternalLifecycleRecorder, ExternalSessionToolHandler,
    drift::DriftGuard,
    metadata::{ExternalToolMetadata, InvocationMode},
    policy::DEFAULT_INVOKE_TIMEOUT,
    sandbox::{
        SandboxCache, SandboxProvider, SandboxSessionInvoker, SandboxSessionInvokerConfig,
        SandboxSpecBuilder, SandboxToolHandler, SessionPool, SessionPoolConfig,
    },
};
use crate::{
    ToolName,
    tools::{ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

/// Per-tool callback the runner invokes to build a [`SandboxSpecBuilder`] from
/// snapshot metadata. Workstream D plugs in policy compilation, secret
/// resolution, and runtime-digest selection behind this type.
pub type SandboxSpecFactory = Arc<
    dyn Fn(&ToolName, &ExternalToolMetadata) -> Result<SandboxSpecBuilder> + Send + Sync + 'static,
>;

/// Plumbing the runner passes in when sandbox-declared tools should route
/// through [`SandboxToolHandler`]. Without this wiring, registry-backed sandbox
/// tools fail closed when loaded.
#[derive(Clone)]
pub struct SandboxRuntimeWiring {
    pub provider: Arc<dyn SandboxProvider>,
    pub cache: Arc<SandboxCache>,
    pub spec_factory: SandboxSpecFactory,
}

pub(super) fn build_sandbox_tool_handler(
    metadata: ToolFunctionMetadata,
    meta: &ExternalToolMetadata,
    wiring: &SandboxRuntimeWiring,
    spec_builder: SandboxSpecBuilder,
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
    drift_guard: Option<Arc<DriftGuard>>,
) -> Arc<dyn ToolHandler> {
    match meta.invocation_mode {
        InvocationMode::SingleShot => {
            let mut handler_builder = SandboxToolHandler::new(
                metadata,
                wiring.provider.clone(),
                wiring.cache.clone(),
                spec_builder,
                DEFAULT_INVOKE_TIMEOUT,
            )
            .with_capabilities(meta.capabilities.clone());
            if let Some(recorder) = lifecycle_recorder {
                handler_builder = handler_builder.with_lifecycle_recorder(recorder.clone());
            }
            if let Some(guard) = drift_guard {
                handler_builder = handler_builder.with_drift_guard(guard);
            }
            Arc::new(handler_builder)
        }
        InvocationMode::Session => {
            // TODO(phase-4 sandbox-streaming §7.2/§9.4): wire per-tool
            // configuration from metadata into pool/invoker instead of using
            // defaults here.
            let pool = Arc::new(SessionPool::new(
                wiring.cache.runner_id().to_string(),
                wiring.provider.clone(),
                spec_builder,
                SessionPoolConfig::default(),
            ));
            let invoker_config = SandboxSessionInvokerConfig::default();
            let invoker_factory = {
                let pool = pool.clone();
                Arc::new(move |ctx: &ToolSessionContext| {
                    Arc::new(SandboxSessionInvoker::new(
                        pool.clone(),
                        ctx.agent_id.clone(),
                        ctx.context_id.clone(),
                        invoker_config.clone(),
                    )) as Arc<dyn super::SessionToolInvoker>
                })
            };

            Arc::new(
                ExternalSessionToolHandler::new_with_factory(
                    metadata,
                    invoker_factory,
                    DEFAULT_INVOKE_TIMEOUT,
                )
                .with_capabilities(meta.capabilities.clone())
                .with_secret_scope(meta.secret_scope),
            )
        }
    }
}
