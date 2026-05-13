//! Archive / read-before-repeat policy prepended to **generated** step-executor phase prompts.
//!
//! The policy text is embedded **literally** at the start of each per-phase `prompt` body (see
//! [`SESSION_STEP_STABLE_PREFIX_BAML`]).
//!
//! All BAML invocations on `BamlRuntimeManager` — both plain `invoke_function` and the
//! step-executor `invoke_function_with_intra` path — receive `ctx.tags['tool_schema_prelude']`
//! when the agent package ships a rendered catalog sidecar
//! ([`TOOL_SCHEMA_CATALOG_SIDECAR_FILE`]). The prelude is the JSON-shape catalog text built at
//! build time from the synthetic `AgentToolSchemaCatalog__bamlrt` BAML function via
//! `{{ ctx.output_format }}`. Tag injection is centralised in
//! `BamlRuntimeManager::enrich_with_tool_schema_prelude` so adding a new invocation surface
//! cannot accidentally drop the catalog prefix.

/// Place at the **start** of generated step-executor `prompt` bodies (before the phase cue and
/// parent IR template). Same prose historically injected under `session_step_stable_prefix`; now
/// inlined so `ctx.tags` carries history only via `conversation_transcript`.
pub const SESSION_STEP_STABLE_PREFIX_BAML: &str = "Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.\n\n\
     For `op: \"Open\"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.\n\n";

/// Alias for callers that referred to the injected-string constant by value name.
pub const SESSION_STEP_STABLE_PREFIX_VALUE: &str = SESSION_STEP_STABLE_PREFIX_BAML;

/// `ctx.tags` key for the rendered agent-wide tool schema catalog on step-executor hops.
///
/// The value is the JSON-shape catalog text rendered at build time from the synthetic
/// `__AgentToolSchemaCatalog__` BAML function via BAML's standard `{{ ctx.output_format }}`
/// machinery. Stable per agent package — used as the cacheable prefix of every step-executor
/// prompt. Replaces the legacy practice of dumping `_baml_runtime.baml` source text here.
pub const TOOL_SCHEMA_PRELUDE_TAG: &str = "tool_schema_prelude";

/// Filename of the rendered tool schema catalog sidecar inside an agent's `baml_src/` directory.
///
/// Written by the builder after the final compile pass (see `runtime_type_gen.rs`) and loaded
/// by the runtime in `schema_invoke.rs` into `state.tool_schema_prelude`. Keeping the filename
/// shared between writer and reader avoids drift.
pub const TOOL_SCHEMA_CATALOG_SIDECAR_FILE: &str = "_baml_tool_schema_catalog.txt";

/// `ctx.tags` key for the projected conversation transcript on intra-turn / step-executor hops.
pub const CONVERSATION_TRANSCRIPT_TAG: &str = "conversation_transcript";
