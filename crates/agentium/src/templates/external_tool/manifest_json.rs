// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Generator for scaffolded `tool-manifest.json`.
//!
//! The manifest is developer-authored source metadata only. Schemas come from
//! the tool's `tool/schema` implementation during discovery and are stored in
//! approved snapshots.

use baml_rt_tools::external_tools::{
    ExternalToolManifest, InvocationMode as RtInvocationMode, SandboxAdapterRuntimeSpec,
    SandboxRuntimeSpec, ToolRuntime,
};
use serde_json::json;

use super::{InvocationMode, Language, Runtime, ScaffoldContext};

fn default_adapter_spec(language: Language) -> SandboxAdapterRuntimeSpec {
    let (command, workdir) = language.default_adapter_command();
    SandboxAdapterRuntimeSpec {
        schema_version: 1,
        protocol: "jsonrpc-stdio".to_string(),
        command: command.iter().map(|s| (*s).to_string()).collect(),
        workdir: Some(workdir.to_string()),
    }
}

pub fn build_manifest(ctx: &ScaffoldContext<'_>) -> ExternalToolManifest {
    let runtime = match ctx.runtime {
        Runtime::Process => Some(ToolRuntime::default()),
        Runtime::Sandbox => {
            let image = ctx
                .sandbox_image
                .clone()
                .expect("sandbox runtime requires sandbox_image in scaffold context");
            Some(ToolRuntime::Sandbox(SandboxRuntimeSpec {
                image,
                entrypoint: ctx.sandbox_entrypoint.clone(),
                adapter: Some(default_adapter_spec(ctx.language)),
            }))
        }
    };

    ExternalToolManifest {
        tool_abi_version: "1".to_string(),
        name: ctx.tool_id(),
        description: ctx.description.to_string(),
        bundle: ctx.bundle.to_string(),
        local_name: ctx.name.to_string(),
        access_level: ctx.access.into(),
        tags: vec![
            ctx.bundle.to_string(),
            ctx.name.to_string(),
            "external".to_string(),
        ],
        event_sources: vec![],
        datasources: vec![],
        invocation_mode: match ctx.invocation_mode {
            InvocationMode::SingleShot => RtInvocationMode::SingleShot,
            InvocationMode::Session => RtInvocationMode::Session,
        },
        session_policy: Default::default(),
        secrets: vec![],
        secret_scope: Default::default(),
        capabilities: json!({}),
        config_bundle: None,
        runtime,
        coordination: None,
    }
}

pub fn generate(ctx: &ScaffoldContext<'_>) -> String {
    serde_json::to_string_pretty(&build_manifest(ctx)).expect("ExternalToolManifest serializes")
}
