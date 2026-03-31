//! Internal state for [`super::BamlRuntimeManager`].
//!
//! Fields are `pub(in crate::baml)` so real submodules under `baml` can access them without
//! widening visibility to the whole crate.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{
    bus::{EffectEmitter, EffectStartToken, ToolKind},
    ids::ActivityAnchorId,
    types::FunctionSignature,
};
use baml_rt_interceptor::InterceptorRegistry;
use baml_rt_llm_config::LlmClientResolver;
use baml_rt_tools::{ToolRegistry as ConcreteToolRegistry, ToolSessionId};
use dashmap::DashMap;
use tokio::sync::Mutex as TokioMutex;

use super::SessionPlanFunctionsMap;
use crate::{
    baml_execution::{BamlExecutor, ConversationContextProvider, ParseRetryPolicy},
    function_tool_manifest::FunctionToolManifest,
    llm_client_registry::LlmSecretResolver,
    planning::{DefaultPlanningResolver, PlanningResolver},
    tool_session_handle::{ToolCallSessionState, ToolSessionScope},
};

/// All [`BamlRuntimeManager`](super::BamlRuntimeManager) fields in one place.
pub(in crate::baml) struct BamlRuntimeState {
    pub(in crate::baml) function_registry: HashMap<String, FunctionSignature>,
    pub(in crate::baml) executor: Option<BamlExecutor>,
    pub(in crate::baml) tool_registry: Arc<ConcreteToolRegistry>,
    pub(in crate::baml) session_plan_functions: Option<SessionPlanFunctionsMap>,
    pub(in crate::baml) tool_step_executors: Option<HashMap<String, String>>,
    pub(in crate::baml) function_tool_manifest: Arc<FunctionToolManifest>,
    pub(in crate::baml) interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    pub(in crate::baml) tool_session_scopes: Arc<DashMap<ToolSessionId, ToolSessionScope>>,
    pub(in crate::baml) tool_session_states: Arc<DashMap<ToolSessionId, ToolCallSessionState>>,
    pub(in crate::baml) tool_session_effect_tokens:
        Arc<DashMap<ToolSessionId, EffectStartToken<ToolKind>>>,
    /// Latest read-phase tool completion anchor per session (pairs `SendDone` with graph `WAS_INFORMED_BY`).
    pub(in crate::baml) read_completion_tool_anchors: Arc<DashMap<ToolSessionId, ActivityAnchorId>>,
    pub(in crate::baml) archive_ref_tables: Arc<baml_rt_tools::archive_refs::ContextRefTables>,
    pub(in crate::baml) effect_emitter: Option<Arc<dyn EffectEmitter>>,
    pub(in crate::baml) conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    pub(in crate::baml) pending_parse_retry_policy: Option<ParseRetryPolicy>,
    pub(in crate::baml) llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
    pub(in crate::baml) llm_client_resolver: Option<Arc<dyn LlmClientResolver>>,
    pub(in crate::baml) planning_resolver: Arc<dyn PlanningResolver>,
    pub(in crate::baml) execution_sessions:
        Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
}

impl Default for BamlRuntimeState {
    fn default() -> Self {
        Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(ConcreteToolRegistry::new()),
            session_plan_functions: None,
            tool_step_executors: None,
            function_tool_manifest: Arc::new(FunctionToolManifest::default()),
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(DashMap::new()),
            tool_session_states: Arc::new(DashMap::new()),
            tool_session_effect_tokens: Arc::new(DashMap::new()),
            read_completion_tool_anchors: Arc::new(DashMap::new()),
            archive_ref_tables: Arc::new(baml_rt_tools::archive_refs::ContextRefTables::new()),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
            llm_secret_resolver: None,
            llm_client_resolver: None,
            planning_resolver: Arc::new(DefaultPlanningResolver),
            execution_sessions: Arc::new(DashMap::new()),
        }
    }
}
