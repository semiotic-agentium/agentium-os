//! Shared imports for [`super::BamlRuntimeManager`] impl shards.
//!
//! Each submodule does `use super::manager_prelude::*` plus `use super::BamlRuntimeManager`.
//! Private sibling modules (`open_input`, `planning_emit`, …) are imported per-shard via
//! `use super::open_input` etc. — they cannot be re-exported through this prelude.
#![allow(unused_imports)]

pub(crate) use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
};

pub(crate) use async_trait::async_trait;
pub(crate) use baml_rt_core::{
    BamlRtError, Outcome, Result, SessionLifecycleError,
    bus::{EffectEmitter, EffectStartToken, PlanningSupersessionKind, ToolKind},
    context,
    correlation::current_correlation_id,
    types::FunctionSignature,
};
pub(crate) use baml_rt_interceptor::InterceptorRegistry;
pub(crate) use baml_rt_tools::{
    ToolFunctionMetadataExport, ToolRegistry as ConcreteToolRegistry, ToolSessionId, ToolStep,
    should_host_retry_baml_error,
};
pub(crate) use baml_types::BamlValue;
pub(crate) use dashmap::DashMap;
pub(crate) use serde_json::Value;
pub(crate) use tokio::sync::Mutex as TokioMutex;

pub(crate) use super::{
    IntentSubmission, SessionPlanFunctionsMap, builder::BamlRuntimeManagerBuilder,
    extract_tool_call, normalize_plan_input, resolve_tool_name_from_input_with_registry,
};
pub(crate) use crate::{
    baml_execution::{
        BamlExecutor, BamlStreamInvocation, ConversationContextProvider, ParseRetryPolicy,
    },
    function_tool_manifest::FunctionToolManifest,
    llm_client_registry::LlmSecretResolver,
    planning::PlanningResolver,
    tool_execution::{ToolExecutionContext, resolve_planning_step},
    tool_session_handle::ToolSessionExecutionHandle,
    traits::{BamlFunctionExecutor, SchemaLoader},
};
