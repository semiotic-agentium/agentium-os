use baml_rt_tools::TOOL_SCHEMA_PRELUDE_TAG;

use super::{BamlRuntimeManager, manager_prelude::*, tool_extraction};

impl BamlRuntimeManager {
    /// The archive ref tables for this runtime instance.
    pub fn archive_ref_tables(&self) -> Arc<baml_rt_tools::archive_refs::ContextRefTables> {
        self.state.archive_ref_tables.clone()
    }

    /// Build conversation context tags for the given scope.
    ///
    /// Graph / provider provenance only (the projection `ConversationContextProvider` exposes),
    /// plus the agent-wide `ctx.tags['tool_schema_prelude']` injection when a rendered catalog
    /// sidecar is loaded. Step-executor FSM uses
    /// [`BamlRuntimeManager::invoke_function_with_intra`] to merge a loop-local supplement
    /// with the same provider; both code paths share
    /// [`Self::enrich_with_tool_schema_prelude`] so plain and step-executor invocations
    /// produce byte-identical opening tags.
    pub async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let Some(ref exec) = self.state.executor else {
            return Ok(None);
        };
        let mut tags = exec.build_conversation_context_tags(scope).await?;
        self.enrich_with_tool_schema_prelude(&mut tags);
        Ok(tags)
    }

    /// Insert `ctx.tags['tool_schema_prelude']` from the loaded catalog sidecar (when present)
    /// into the supplied tag map, allocating the map only when the prelude is available.
    /// Single source of truth for both plain `invoke_function` and step-executor
    /// `invoke_function_with_intra` paths — adding either path to the catalog requires changing
    /// nothing else.
    pub(in crate::baml) fn enrich_with_tool_schema_prelude(
        &self,
        tags: &mut Option<HashMap<String, BamlValue>>,
    ) {
        let Some(prelude) = self.state.tool_schema_prelude.as_ref() else {
            return;
        };
        let map = tags.get_or_insert_with(HashMap::new);
        map.insert(
            TOOL_SCHEMA_PRELUDE_TAG.to_string(),
            BamlValue::String(prelude.as_ref().to_string()),
        );
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
        tracing::debug!(
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
                &candidates[0],
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

#[cfg(test)]
mod tool_schema_prelude_tests {
    //! `enrich_with_tool_schema_prelude` is the single source of truth that lifts the catalog
    //! sidecar text into `ctx.tags['tool_schema_prelude']` for every invocation surface — plain
    //! `invoke_function`, step-executor `invoke_function_with_intra`, and the streaming bridge.
    //! These unit tests exercise the helper in isolation so a regression in either invocation
    //! path is caught at the seam where the tag is produced.

    use std::{collections::HashMap, sync::Arc};

    use baml_rt_tools::TOOL_SCHEMA_PRELUDE_TAG;
    use baml_types::BamlValue;

    use super::super::BamlRuntimeManager;

    fn manager_with_prelude(prelude: Option<Arc<str>>) -> BamlRuntimeManager {
        let mut manager = BamlRuntimeManager::default();
        manager.state.tool_schema_prelude = prelude;
        manager
    }

    #[test]
    fn enrich_inserts_prelude_when_sidecar_loaded() {
        let prelude_text: Arc<str> = Arc::from("RENDERED CATALOG TEXT");
        let manager = manager_with_prelude(Some(prelude_text.clone()));

        let mut tags: Option<HashMap<String, BamlValue>> = None;
        manager.enrich_with_tool_schema_prelude(&mut tags);

        let tags = tags.expect("enrichment must allocate a tag map when prelude is present");
        match tags.get(TOOL_SCHEMA_PRELUDE_TAG) {
            Some(BamlValue::String(s)) => {
                assert_eq!(
                    s.as_str(),
                    prelude_text.as_ref(),
                    "tag value must match catalog sidecar text byte-for-byte"
                );
            }
            other => panic!("expected string tag, got {other:?}"),
        }
    }

    #[test]
    fn enrich_preserves_existing_tags_when_sidecar_loaded() {
        let manager = manager_with_prelude(Some(Arc::from("CATALOG")));

        let mut tags: Option<HashMap<String, BamlValue>> = Some({
            let mut m = HashMap::new();
            m.insert(
                "conversation_transcript".to_string(),
                BamlValue::String("user: hi".into()),
            );
            m
        });
        manager.enrich_with_tool_schema_prelude(&mut tags);

        let tags = tags.expect("tag map must remain Some when prelude is present");
        assert_eq!(
            tags.len(),
            2,
            "transcript tag must be preserved alongside prelude: {tags:?}"
        );
        assert!(tags.contains_key("conversation_transcript"));
        assert!(tags.contains_key(TOOL_SCHEMA_PRELUDE_TAG));
    }

    #[test]
    fn enrich_is_noop_when_no_sidecar_loaded() {
        let manager = manager_with_prelude(None);

        let mut tags: Option<HashMap<String, BamlValue>> = None;
        manager.enrich_with_tool_schema_prelude(&mut tags);
        assert!(
            tags.is_none(),
            "no-prelude path must not allocate a tag map (cache discipline)"
        );

        let mut tags: Option<HashMap<String, BamlValue>> = Some({
            let mut m = HashMap::new();
            m.insert("k".to_string(), BamlValue::String("v".into()));
            m
        });
        manager.enrich_with_tool_schema_prelude(&mut tags);
        let tags = tags.expect("pre-existing map must remain");
        assert_eq!(
            tags.len(),
            1,
            "no-prelude path must not insert any tag: {tags:?}"
        );
        assert!(!tags.contains_key(TOOL_SCHEMA_PRELUDE_TAG));
    }
}
