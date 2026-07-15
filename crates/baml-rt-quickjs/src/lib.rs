// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! BAML runtime with QuickJS integration.

#![recursion_limit = "256"]

pub mod a2a_chat_surface;
pub mod a2a_stream;
pub mod baml;
pub mod baml_collector;
pub mod baml_execution;
pub mod baml_pre_execution;
pub mod context;
pub mod execution_session_types;
pub use execution_session_types::{IntentSubmissionWire, PlanSubmissionWire};
pub mod function_tool_manifest;
pub mod js_event_loop_probe;
pub mod js_value_converter;
pub mod llm_client_registry;
pub(crate) mod llm_json_salvage;
pub mod llm_resolver_adapter;
pub mod planning;
pub(crate) mod provenance_errors;
pub mod quickjs_bridge;
pub mod runtime;
pub mod step_executor_loop;
pub(crate) mod step_executor_outcome_bridge;
pub(crate) mod tool_effect_metadata;
pub(crate) mod tool_execution;
pub mod tool_session_handle;
pub mod traits;

pub use a2a_chat_surface::A2A_CHAT_HOST_GLOBALS;
pub use a2a_stream::{
    A2aYieldSessionComplete, A2aYieldSessionReady, BridgeHandle, HandoverSender, StreamOutput,
    begin_a2a_yield_session, collect_into_channel_owned, invoke_js_function_handover,
    invoke_optional_js_function_handover, invoke_tool_handover, spawn_stream_handover,
};
pub use baml::{BamlRuntimeManager, SessionPlanTypeName, ToolSessionExecutionHandle};
pub use context::{BamlContext, ContextMetadata};
pub use js_event_loop_probe::JsEventLoopProbe;
pub use llm_client_registry::{
    LLM_SECRET_KEYS, LlmRegistryBuildResult, LlmSecretResolver, build_llm_client_registry,
};
pub use llm_resolver_adapter::SecretResolverToLlmAdapter;
pub use quickjs_bridge::QuickJSBridge;
pub use runtime::{QuickJSConfig, Runtime, RuntimeBuilder, RuntimeConfig};
pub use traits::{
    BamlFunctionExecutor, BamlGateway, JsRuntimeHost, SchemaLoader, ToolRegistryTrait,
};
