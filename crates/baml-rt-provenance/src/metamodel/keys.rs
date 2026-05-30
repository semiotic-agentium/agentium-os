// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Sealed filter-key types used by [`crate::metamodel::query::GraphQuery`].
//!
//! Each filter key is a ZST that implements zero or more subject-specific
//! `*FilterKey` traits (sealed in `crate::metamodel::query`). The traits
//! deliberately omit cross-node relationship keys: `keys::ContextId` does
//! NOT implement `MessageFilterKey`, so attempting `.filter(keys::ContextId, …)`
//! on a `GraphQuery<Message, _>` is a compile error — the caller must use
//! the dedicated `scoped_to_ctx(ContextNodeId)` traversal instead.

use crate::{metamodel::sealed::Sealed, vocabulary::a2a};

/// Property keys are coupled to their typed value at compile time (so `.filter`
/// cannot accept the wrong value type for a given key).
pub trait FilterKey: Sealed {
    type Value;
    /// On-disk property column name (`prov_node.props.a2a_*`).
    const PROP_KEY: &'static str;
}

macro_rules! filter_key {
    ($name:ident, value: $value:ty, prop: $prop:expr, doc: $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name;
        impl Sealed for $name {}
        impl FilterKey for $name {
            type Value = $value;
            const PROP_KEY: &'static str = $prop;
        }
    };
}

filter_key!(MessageId, value: String, prop: a2a::MESSAGE_ID,
    doc: "On-disk `a2a_message_id` property of a `Message` node. Filterable on `Message` queries.");
filter_key!(Role, value: String, prop: a2a::ROLE,
    doc: "On-disk `a2a_role` property of a `Message` node.");
filter_key!(Direction, value: String, prop: a2a::DIRECTION,
    doc: "On-disk `a2a_direction` property of a `Message` node.");
filter_key!(ToolName, value: String, prop: a2a::TOOL_NAME,
    doc: "On-disk `a2a_tool_name` property of a `ToolCall` / `SessionStep` node.");
filter_key!(Model, value: String, prop: a2a::MODEL,
    doc: "On-disk `a2a_model` property of an `LlmCall` node.");
filter_key!(Client, value: String, prop: a2a::CLIENT,
    doc: "On-disk `a2a_client` property of an `LlmCall` node (the LLM provider id, e.g. \"openai\").");
filter_key!(Provider, value: String, prop: a2a::CLIENT,
    doc: "Alias of [`Client`] in the public ops vocabulary (the field is exposed as `provider`). \
          Same on-disk column.");
filter_key!(FunctionName, value: String, prop: a2a::FUNCTION_NAME,
    doc: "On-disk `a2a_function_name` property of an `LlmCall` node (BAML function variant).");
filter_key!(BamlPrompt, value: String, prop: a2a::PROMPT_NAME,
    doc: "On-disk `a2a_prompt_name` property of an `LlmCall` node (base BAML prompt name).");
filter_key!(ActivityOutcome, value: String, prop: a2a::ACTIVITY_OUTCOME,
    doc: "On-disk `a2a_activity_outcome` property of an LLM/Tool call.");

// ---------------------------------------------------------------------------
// Deliberately-prohibited filter keys.
//
// The following ZSTs exist as DOC-ONLY witnesses that capture the doctrinal
// reason a particular property is NOT a filterable key. They implement
// `Sealed` and `FilterKey` (so the prop string is recoverable) but do not
// implement any `*FilterKey` trait in `query.rs`. Their job is to make the
// "this is an edge, not a property filter" doctrine surface in `cargo doc`.
// ---------------------------------------------------------------------------

filter_key!(ContextId, value: String, prop: a2a::CONTEXT_ID,
    doc: "DOC-ONLY: `a2a_context_id` IS NOT a filterable property key on any \
          subject. Context membership is an EDGE (`SCOPED_TO`); use \
          `GraphQuery::scoped_to_ctx(ContextNodeId)` instead.");
filter_key!(TaskId, value: String, prop: a2a::TASK_ID,
    doc: "DOC-ONLY: `a2a_task_id` IS NOT a filterable property key on Message \
          / ToolCall / SessionStep. Task membership is an EDGE (`A2A_TASK_MESSAGE`, \
          `A2A_TASK_CALL`, `A2A_TASK_SESSION_STEP`).");
filter_key!(AgentId, value: String, prop: a2a::AGENT_ID,
    doc: "DOC-ONLY: `a2a_agent_id` IS NOT a filterable property key on Message. \
          Agent ownership is a two-hop EDGE traversal via `A2AMessageProcessing` \
          (`WAS_RECEIVED_BY` / `WAS_EMITTED_BY` then `WAS_EXECUTED_BY`). Use \
          `GraphQuery::emitted_by_agent_package(AgentPackage)` for the canonical \
          agent-scoped read.");
