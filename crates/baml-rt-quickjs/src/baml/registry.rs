use super::{BamlRuntimeManager, manager_prelude::*, tool_extraction};

impl BamlRuntimeManager {
    /// The archive ref tables for this runtime instance.
    pub fn archive_ref_tables(&self) -> Arc<baml_rt_tools::archive_refs::ContextRefTables> {
        self.state.archive_ref_tables.clone()
    }

    /// Build conversation context tags for the given scope.
    /// Provenance is the single source of truth — writes are synchronous so this always
    /// reflects the current state of the conversation.
    pub async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        match &self.state.executor {
            Some(exec) => exec.build_conversation_context_tags(scope).await,
            None => Ok(None),
        }
    }

    /// List all available BAML functions
    pub fn list_functions(&self) -> Vec<String> {
        self.state.function_registry.keys().cloned().collect()
    }

    /// Get the tool registry (for tool registration)
    pub fn tool_registry(&self) -> Arc<ConcreteToolRegistry> {
        self.state.tool_registry.clone()
    }

    /// Get the session plan functions map (function name → plan type).
    pub fn session_plan_functions(&self) -> Option<SessionPlanFunctionsMap> {
        self.state.session_plan_functions.clone()
    }

    /// Get the eagerly resolved function→tool manifest.
    pub fn function_tool_manifest(&self) -> Arc<FunctionToolManifest> {
        self.state.function_tool_manifest.clone()
    }

    /// Rebuild the function→tool manifest from the current session_plan_functions
    /// and tool_registry. Must be called AFTER tools are registered.
    pub fn rebuild_function_tool_manifest(&mut self) {
        self.state.function_tool_manifest = Arc::new(
            self.state
                .session_plan_functions
                .as_ref()
                .map(|raw| FunctionToolManifest::build(raw, &self.state.tool_registry))
                .unwrap_or_default(),
        );
        tracing::info!(
            function_tool_bindings = self.state.function_tool_manifest.len(),
            "Function-tool manifest built"
        );
    }

    /// Resolve the `SessionPolicy` for a bound tool (after Open has selected it).
    ///
    /// Unified policy resolution path used by both single-tool and polymorphic
    /// functions. Returns `Strict` (the safe default) when the tool is not found.
    pub fn resolve_session_policy_for_tool(
        &self,
        tool_name: &baml_rt_tools::ToolName,
    ) -> baml_rt_tools::SessionPolicy {
        self.state
            .tool_registry
            .get_metadata_by_name(tool_name)
            .map(|meta| meta.session_policy)
            .unwrap_or_default()
    }

    /// Resolve the single-tool step executor function name for a tool.
    /// Used by the shim's `__resolve_tool_step_executor` host helper for
    /// polymorphic auto-narrowing after Open.
    pub fn resolve_tool_step_executor(&self, tool_name: &str) -> Option<String> {
        self.state
            .tool_step_executors
            .as_ref()
            .and_then(|map| map.get(tool_name).cloned())
    }

    /// Resolve the `SessionPolicy` for a BAML step-executor function name.
    ///
    /// For single-tool functions (one candidate), resolves that tool's policy.
    /// For polymorphic functions (multiple candidates), returns `Strict` as safe
    /// default — the shim should use `resolve_session_policy_for_tool` with the
    /// selected tool name after Open.
    /// Resolve policy for a step-executor hop: prefer the bound tool after Open,
    /// otherwise fall back to function-level resolution (single-tool vs polymorphic).
    pub fn resolve_session_policy_for_step_executor(
        &self,
        function_name: &str,
        selected_tool: Option<&str>,
    ) -> baml_rt_tools::SessionPolicy {
        if let Some(raw) = selected_tool {
            match baml_rt_tools::ToolName::parse(raw) {
                Ok(tool_name) => return self.resolve_session_policy_for_tool(&tool_name),
                Err(e) => {
                    tracing::warn!(
                        selected_tool = raw,
                        error = %e,
                        "resolve_session_policy_for_step_executor: invalid tool name, falling back"
                    );
                }
            }
        }
        self.resolve_session_policy_for_function(function_name)
    }

    pub fn resolve_session_policy_for_function(
        &self,
        func_name: &str,
    ) -> baml_rt_tools::SessionPolicy {
        let candidates = match self
            .state
            .session_plan_functions
            .as_ref()
            .and_then(|m| m.get(func_name))
        {
            Some(c) => c,
            None => {
                tracing::debug!(
                    func = func_name,
                    "session policy: no candidates in session_plan_functions"
                );
                return baml_rt_tools::SessionPolicy::default();
            }
        };
        if candidates.len() == 1 {
            let tool_name = match tool_extraction::resolve_tool_name_from_plan_type_with_registry(
                &self.state.tool_registry,
                candidates[0].as_str(),
            ) {
                Ok(name) => name,
                Err(e) => {
                    tracing::debug!(func = func_name, plan_type = candidates[0].as_str(), error = %e, "session policy: tool resolution failed");
                    return baml_rt_tools::SessionPolicy::default();
                }
            };
            let policy = self.resolve_session_policy_for_tool(&tool_name);
            tracing::debug!(func = func_name, tool = %tool_name, policy = ?policy, "session policy: resolved from tool");
            policy
        } else {
            tracing::debug!(
                func = func_name,
                count = candidates.len(),
                "session policy: polymorphic, defaulting to Strict"
            );
            baml_rt_tools::SessionPolicy::default()
        }
    }

    /// Get the interceptor registry (for registering interceptors)
    pub fn interceptor_registry(&self) -> Arc<TokioMutex<InterceptorRegistry>> {
        self.state.interceptor_registry.clone()
    }

    /// Register an LLM interceptor
    pub async fn register_llm_interceptor<I: baml_rt_interceptor::LLMInterceptor>(
        &self,
        interceptor: I,
    ) {
        let mut registry = self.state.interceptor_registry.lock().await;
        registry.register_llm_interceptor(interceptor);
    }

    /// Register a tool interceptor
    pub async fn register_tool_interceptor<I: baml_rt_interceptor::ToolInterceptor>(
        &self,
        interceptor: I,
    ) {
        let mut registry = self.state.interceptor_registry.lock().await;
        registry.register_tool_interceptor(interceptor);
    }
}
