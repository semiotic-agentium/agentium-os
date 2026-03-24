use super::{BamlRuntimeManager, manager_prelude::*, state::BamlRuntimeState};

impl BamlRuntimeManager {
    pub fn builder() -> BamlRuntimeManagerBuilder {
        BamlRuntimeManagerBuilder::default()
    }

    /// Create a new BAML runtime manager
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing BAML runtime manager");

        Ok(Self {
            state: BamlRuntimeState::default(),
        })
    }

    /// Resolve all known LLM secret keys from the resolver as a HashMap for BAML's env_vars.
    /// BAML schemas reference secrets as `api_key env.X`; BAML resolves these from env_vars
    /// passed to `BamlRuntime::from_directory`. By resolving via fnox here, we avoid
    /// depending on std::env::var — the fnox resolver is the single source of truth.
    pub(in crate::baml) fn resolve_secrets_as_env_vars(
        &self,
    ) -> std::collections::HashMap<String, String> {
        let Some(resolver) = &self.state.llm_secret_resolver else {
            return std::collections::HashMap::new();
        };
        let mut env_vars = std::collections::HashMap::new();
        for key in crate::llm_client_registry::LLM_SECRET_KEYS {
            if let Some((value, _)) = resolver.resolve_llm_api_key("default", key) {
                env_vars.insert((*key).to_string(), value);
            }
        }
        env_vars
    }

    /// Set the LLM secret resolver for ClientRegistry-based API key injection.
    /// When set, API keys are resolved via the resolver (e.g. fnox + llm mapping) and
    /// passed to BAML as ClientRegistry, not env vars. May be called before or after load_schema.
    pub fn set_llm_secret_resolver(&mut self, resolver: Arc<dyn LlmSecretResolver>) {
        self.state.llm_secret_resolver = Some(resolver.clone());
        if let Some(executor) = self.state.executor.as_mut() {
            executor.set_llm_secret_resolver(resolver);
        }
    }

    /// Set the effect emitter (for effects-first liveness).
    /// If the executor is already loaded, forwards the emitter to it so LLM/tool effects
    /// are emitted and the promise-polling loop can use effect-gated timeouts.
    pub fn set_effect_emitter(&mut self, emitter: Arc<dyn EffectEmitter>) {
        self.state.effect_emitter = Some(emitter.clone());
        if let Some(executor) = self.state.executor.as_mut() {
            executor.set_effect_emitter(emitter);
        }
    }

    pub fn set_conversation_context_provider(
        &mut self,
        provider: Arc<dyn ConversationContextProvider>,
    ) {
        self.state.conversation_context_provider = Some(provider.clone());
        if let Some(executor) = self.state.executor.as_mut() {
            executor.set_conversation_context_provider(provider);
        }
    }

    /// Set the policy for retrying BAML calls on parse failure. May be called before or after [`load_schema`](Self::load_schema).
    /// Use `ParseRetryPolicy { max_attempts: 1, .. }` in tests to avoid retry delay.
    pub fn set_parse_retry_policy(&mut self, policy: ParseRetryPolicy) {
        self.state.pending_parse_retry_policy = Some(policy.clone());
        if let Some(executor) = self.state.executor.as_mut() {
            executor.set_parse_retry_policy(policy);
        }
    }

    /// Set the session plan function map (for tests). Normally loaded from `session_plan_functions.json` when loading BAML.
    /// Also eagerly rebuilds the function→tool manifest.
    pub fn set_session_plan_functions(&mut self, map: Option<SessionPlanFunctionsMap>) {
        self.state.function_tool_manifest = Arc::new(
            map.as_ref()
                .map(|raw| FunctionToolManifest::build(raw, &self.state.tool_registry))
                .unwrap_or_default(),
        );
        self.state.session_plan_functions = map;
    }

    pub fn set_planning_resolver(&mut self, resolver: Arc<dyn PlanningResolver>) {
        self.state.planning_resolver = resolver;
    }

    /// Replace the execution_sessions map with a shared Arc from the bridge.
    /// Called during bridge registration so tool handles read the same state
    /// that the JS execution session commands write to.
    pub(crate) fn set_execution_sessions(
        &mut self,
        sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
    ) {
        self.state.execution_sessions = sessions;
    }

    pub(crate) fn tool_execution_context(&self) -> ToolExecutionContext {
        ToolExecutionContext {
            tool_registry: self.state.tool_registry.clone(),
            interceptor_registry: self.state.interceptor_registry.clone(),
            effect_emitter: self.state.effect_emitter.clone(),
            execution_sessions: self.state.execution_sessions.clone(),
            archive_ref_tables: self.state.archive_ref_tables.clone(),
        }
    }

    /// Returns a handle for session operations. Use this to avoid holding the runtime lock across awaits.
    pub fn tool_session_handle(&self) -> ToolSessionExecutionHandle {
        ToolSessionExecutionHandle {
            ctx: self.tool_execution_context(),
            tool_session_scopes: self.state.tool_session_scopes.clone(),
            tool_session_states: self.state.tool_session_states.clone(),
            tool_session_effect_tokens: self.state.tool_session_effect_tokens.clone(),
        }
    }
}
