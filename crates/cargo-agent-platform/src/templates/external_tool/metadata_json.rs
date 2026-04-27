//! Generator for the scaffolded `tool-metadata.json`.
//!
//! Builds the runtime's typed [`ExternalToolMetadata`] and delegates
//! serialization to it. That way the schema on disk cannot drift from the
//! shape the runtime parses: a field rename in the runtime crate is a
//! compile-time failure here, not a silent mismatch at load.

use baml_rt_tools::external_tools::{
    ExternalToolMetadata, InvocationMode as RtInvocationMode, SandboxAdapterRuntimeSpec,
    SandboxRuntimeSpec, ToolRuntime,
};
use serde_json::json;

use super::{
    InvocationMode, Language, Runtime, STARTER_INPUT_KEY, STARTER_OUTPUT_KEY, ScaffoldContext,
};

/// Generate `tool-metadata.json` content for an external tool scaffold.
///
/// The starter input/output schemas use the `STARTER_*` keys from the scaffold
/// layer since those are DX defaults; the metadata envelope (`name`, `bundle`,
/// `access_level`, `invocation_mode`, …) flows through the runtime's typed
/// constructor so there's one source of truth for the schema.
fn default_adapter_spec(language: Language) -> SandboxAdapterRuntimeSpec {
    let (command, workdir) = language.default_adapter_command();
    SandboxAdapterRuntimeSpec {
        schema_version: 1,
        protocol: "jsonrpc-stdio".to_string(),
        command: command.iter().map(|s| (*s).to_string()).collect(),
        workdir: Some(workdir.to_string()),
    }
}

pub fn build_metadata(ctx: &ScaffoldContext<'_>) -> ExternalToolMetadata {
    let tool_id = ctx.tool_id();

    let input_schema = json!({
        "type": "object",
        "properties": {
            STARTER_INPUT_KEY: {
                "type": "string",
                "description": "Arbitrary text input for the starter scaffold."
            }
        },
        "required": [STARTER_INPUT_KEY],
        "additionalProperties": false
    });

    let output_schema = json!({
        "type": "object",
        "properties": {
            STARTER_OUTPUT_KEY: {
                "type": "string",
                "description": "Echoed message from tool input."
            }
        },
        "required": [STARTER_OUTPUT_KEY],
        "additionalProperties": false
    });

    let mut meta = ExternalToolMetadata::new(
        tool_id,
        ctx.bundle,
        ctx.name,
        ctx.access.into(),
        ctx.description,
        input_schema,
        output_schema,
    )
    .with_tags(vec![
        ctx.bundle.to_string(),
        ctx.name.to_string(),
        "external".to_string(),
    ]);
    meta.invocation_mode = match ctx.invocation_mode {
        InvocationMode::SingleShot => RtInvocationMode::SingleShot,
        InvocationMode::Session => RtInvocationMode::Session,
    };

    // Emit runtime blocks explicitly so scaffolds always match the current
    // metadata schema while preserving process-wrapper defaults.
    match ctx.runtime {
        Runtime::Process => {
            meta.runtime = Some(ToolRuntime::default());
            meta.runtime_digest = None;
        }
        Runtime::Sandbox => {
            let image = ctx
                .sandbox_image
                .clone()
                .expect("sandbox runtime requires sandbox_image in scaffold context");
            let runtime_digest = ctx
                .runtime_digest
                .clone()
                .expect("sandbox runtime requires runtime_digest in scaffold context");
            meta.runtime = Some(ToolRuntime::Sandbox(SandboxRuntimeSpec {
                image,
                entrypoint: ctx.sandbox_entrypoint.clone(),
                adapter: Some(default_adapter_spec(ctx.language)),
            }));
            meta.runtime_digest = Some(runtime_digest);
        }
    }

    meta
}

pub fn generate(ctx: &ScaffoldContext<'_>) -> String {
    // Hand-rolled schema above is static JSON; serialization is infallible for
    // `ExternalToolMetadata` + schema Value payloads. Propagate errors via
    // expect so any future serialization break surfaces at scaffold time.
    build_metadata(ctx)
        .to_pretty_json()
        .expect("ExternalToolMetadata serializes")
}
