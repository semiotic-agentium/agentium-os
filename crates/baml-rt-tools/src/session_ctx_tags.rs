// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Archive / citation micro-grammar prepended to **generated** step-executor phase prompts.
//!
//! The policy text is embedded **literally** at the start of each per-phase `prompt` body (see
//! [`SESSION_STEP_STABLE_PREFIX_BAML`]).
//!
//! All BAML invocations on `BamlRuntimeManager` — both plain `invoke_function` and the
//! step-executor `invoke_function_with_intra` path — receive `ctx.tags['tool_schema_prelude']`
//! when the agent package ships a rendered catalog sidecar
//! ([`TOOL_SCHEMA_CATALOG_SIDECAR_FILE`]). The prelude is the JSON-shape catalog text built at
//! build time by walking the compiled BAML IR for stable tool / operation vocabulary. Tag
//! injection is centralised in
//! `BamlRuntimeManager::enrich_with_tool_schema_prelude` so adding a new invocation surface
//! cannot accidentally drop the catalog prefix.

/// Place at the **start** of generated step-executor `prompt` bodies. This is the stable
/// pre-history archive/citation grammar; operation field shapes live in the IR-derived
/// `tool_schema_prelude` vocabulary, not in this prose.
pub const SESSION_STEP_STABLE_PREFIX_BAML: &str = "Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.\n\
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.\n\n";

/// Alias for callers that referred to the injected-string constant by value name.
pub const SESSION_STEP_STABLE_PREFIX_VALUE: &str = SESSION_STEP_STABLE_PREFIX_BAML;

/// `ctx.tags` key for the rendered agent-wide tool schema catalog on step-executor hops.
///
/// The value is the IR-derived stable tool / operation vocabulary rendered at build time into
/// `_baml_tool_schema_catalog.txt`. Stable per agent package — used as the cacheable prefix of
/// every step-executor prompt.
pub const TOOL_SCHEMA_PRELUDE_TAG: &str = "tool_schema_prelude";

/// Filename of the rendered tool schema catalog sidecar inside an agent's `baml_src/` directory.
///
/// Written by the builder after the final compile pass (see `runtime_type_gen.rs`) and loaded
/// by the runtime in `schema_invoke.rs` into `state.tool_schema_prelude`. Keeping the filename
/// shared between writer and reader avoids drift.
pub const TOOL_SCHEMA_CATALOG_SIDECAR_FILE: &str = "_baml_tool_schema_catalog.txt";

/// `ctx.tags` key for the projected conversation transcript on intra-turn / step-executor hops.
pub const CONVERSATION_TRANSCRIPT_TAG: &str = "conversation_transcript";
