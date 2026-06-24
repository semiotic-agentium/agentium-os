// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use super::{BamlRuntimeManager, manager_prelude::*, runtime_io};

impl BamlRuntimeManager {
    /// Check if a schema is loaded
    pub fn is_schema_loaded(&self) -> bool {
        self.state.executor.is_some()
    }

    /// Load a compiled BAML schema/configuration
    ///
    /// This loads the BAML IL (Intermediate Language) from the baml_src directory
    /// and registers all available functions.
    ///
    /// The schema_path should point to the baml_src directory.
    pub fn load_schema(&mut self, schema_path: &str) -> Result<()> {
        tracing::debug!(schema_path = schema_path, "Loading BAML IL");

        use std::path::Path;

        // Find project root
        let schema_path_obj = Path::new(schema_path);
        let project_root = if schema_path_obj.is_file() {
            schema_path_obj.parent().and_then(|p| p.parent())
        } else if schema_path_obj.file_name() == Some(std::ffi::OsStr::new("baml_src")) {
            schema_path_obj.parent()
        } else {
            Some(schema_path_obj)
        }
        .ok_or_else(|| BamlRtError::InvalidArgument("Invalid schema path".to_string()))?;

        let baml_src_dir = project_root.join("baml_src");
        if !baml_src_dir.exists() {
            return Err(BamlRtError::BamlRuntime(
                "baml_src directory not found".to_string(),
            ));
        }

        // Resolve LLM secrets from fnox resolver and inject as env_vars so BAML schema's
        // `api_key env.X` references resolve without relying on std::env::var.
        let env_vars = self.resolve_secrets_as_env_vars();
        let mut executor = BamlExecutor::load_il(&baml_src_dir, env_vars)?;

        // Load the rendered agent-wide tool schema catalog produced by the builder. This is
        // the JSON-shape catalog text emitted via BAML's `{{ ctx.output_format }}` renderer
        // over the synthetic `__AgentToolSchemaCatalog__` function — never the raw
        // `_baml_runtime.baml` source. Old packages without the sidecar simply load no
        // prelude (graceful degradation; no source dump fallback).
        let catalog_path = baml_src_dir.join(baml_rt_tools::TOOL_SCHEMA_CATALOG_SIDECAR_FILE);
        self.state.tool_schema_prelude = match std::fs::read_to_string(&catalog_path) {
            Ok(s) => Some(std::sync::Arc::<str>::from(s)),
            Err(e) => {
                tracing::debug!(
                    path = %catalog_path.display(),
                    error = %e,
                    "tool_schema_prelude: rendered catalog sidecar not present — step prompts will omit the prelude block"
                );
                None
            }
        };

        // Set effect emitter if available
        if let Some(ref emitter) = self.state.effect_emitter {
            executor.set_effect_emitter(emitter.clone());
        }
        if let Some(ref provider) = self.state.conversation_context_provider {
            executor.set_conversation_context_provider(provider.clone());
        }
        if let Some(policy) = self.state.pending_parse_retry_policy.take() {
            executor.set_parse_retry_policy(policy);
        }
        if let Some(ref resolver) = self.state.llm_secret_resolver {
            executor.set_llm_secret_resolver(resolver.clone());
        }
        if let Some(ref resolver) = self.state.llm_client_resolver {
            executor.set_llm_client_resolver(resolver.clone());
        }

        // Discover functions from the BAML runtime
        let function_names = executor.list_functions();
        for func_name in function_names {
            // Register function signature
            self.state.function_registry.insert(
                func_name.clone(),
                FunctionSignature {
                    name: func_name.clone(),
                    input_types: vec![],
                    output_type: baml_rt_core::types::BamlType::String,
                },
            );
        }

        self.state.executor = Some(executor);

        self.state.session_plan_functions = runtime_io::load_build_manifest::<
            SessionPlanFunctionsMap,
        >(project_root, "session_plan_functions.json");
        self.state.tool_step_executors = runtime_io::load_build_manifest::<
            std::collections::HashMap<String, String>,
        >(project_root, "tool_step_executors.json");
        self.state.unified_step_executor_functions =
            runtime_io::load_build_manifest::<baml_rt_tools::UnifiedStepExecutorFunctionsMap>(
                project_root,
                "unified_step_executor_functions.json",
            );

        tracing::debug!(
            function_count = self.state.function_registry.len(),
            session_plan_manifest = self
                .state
                .session_plan_functions
                .as_ref()
                .map(|m| m.len())
                .unwrap_or(0),
            unified_step_executor_roots = self
                .state
                .unified_step_executor_functions
                .as_ref()
                .map(|m| m.len())
                .unwrap_or(0),
            "Loaded BAML IL"
        );

        Ok(())
    }

    /// Get the signature of a function by name
    pub fn get_function_signature(&self, name: &str) -> Option<&FunctionSignature> {
        self.state.function_registry.get(name)
    }

    /// Execute a BAML function with the given arguments
    ///
    /// This is the main entry point for executing BAML functions.
    /// It validates the function exists and delegates to the executor.
    pub async fn invoke_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.log_invoke_baml_start(function_name, &args);
        // Verify function exists
        self.require_function_name(function_name)?;
        let merged_tags = self.build_conversation_context_tags(scope).await?;
        self.invoke_baml_core(scope, function_name, args, merged_tags)
            .await
    }

    /// Host utility BAML: no conversation context tags, no tool-result follow-up.
    pub async fn invoke_host_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.log_invoke_baml_start(function_name, &args);
        self.require_function_name(function_name)?;
        let empty_tags = HashMap::new();
        let executor = self
            .state
            .executor
            .as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;
        let (result, completion) = executor
            .execute_function(
                scope,
                function_name,
                args,
                Some(self.state.interceptor_registry.clone()),
                None,
                &self.state.function_tool_manifest,
                Some(empty_tags),
            )
            .await?;
        if let Some(h) = completion {
            h.complete(Outcome::Success, None).await;
        }
        Ok(result)
    }

    /// Step-executor hop: same as [`Self::invoke_function`], with loop-local
    /// `conversation_intra_supplement` rows merged after the graph provider (only
    /// when not already in the provider slice, then tail-capped like normal tags).
    pub async fn invoke_function_with_intra(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
        conversation_intra_supplement: &[serde_json::Value],
    ) -> Result<serde_json::Value> {
        self.log_invoke_baml_start(function_name, &args);
        self.require_function_name(function_name)?;
        let merged_tags = self
            .build_conversation_context_tags_with_intra(scope, conversation_intra_supplement)
            .await?;
        self.invoke_baml_core(scope, function_name, args, merged_tags)
            .await
    }

    fn log_invoke_baml_start(&self, function_name: &str, args: &serde_json::Value) {
        let correlation_id = current_correlation_id();
        if let Some(correlation_id) = correlation_id.as_ref().map(|id| id.as_str()) {
            tracing::debug!(
                function = function_name,
                args = ?args,
                correlation_id = correlation_id,
                "Invoking BAML function"
            );
        } else {
            tracing::debug!(
                function = function_name,
                args = ?args,
                "Invoking BAML function"
            );
        }
    }

    fn require_function_name(&self, function_name: &str) -> Result<()> {
        self.state
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;
        Ok(())
    }

    async fn invoke_baml_core(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
        merged_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<serde_json::Value> {
        // Execute the BAML function using the executor
        let executor = self
            .state
            .executor
            .as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        // `conversation_transcript` via [`BamlRuntimeManager::build_conversation_context_tags`]
        // (graph) or with supplement via [`Self::invoke_function_with_intra`].
        let interceptor_registry = Some(self.state.interceptor_registry.clone());
        let planning_step = resolve_planning_step(&self.state.execution_sessions, scope);
        let invocation_args = args.clone();
        let (result, completion) = executor
            .execute_function(
                scope,
                function_name,
                args,
                interceptor_registry,
                planning_step,
                &self.state.function_tool_manifest,
                merged_tags,
            )
            .await?;
        // If the BAML function returned a session plan (e.g. GetDiscoverAgentsPlan) or tool call, execute it and return the tool output so JS gets e.g. { agents, done } not the raw plan.
        match self
            .execute_tool_from_baml_result_or_value(
                scope,
                result,
                Some(function_name),
                Some(&invocation_args),
            )
            .await
        {
            Ok(v) => {
                if let Some(h) = completion {
                    h.complete(Outcome::Success, None).await;
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(h) = completion {
                    h.complete(Outcome::Failure, Some(e.to_string())).await;
                }
                Err(e)
            }
        }
    }

    /// Invoke a BAML function with streaming support
    ///
    /// Returns a [`BamlStreamInvocation`] bundling stream, context manager, client registry, and env vars.
    /// Run with `inv.stream.run(..., &inv.ctx_manager, None, inv.client_registry_opt.as_ref(), inv.env_vars)`.
    pub fn invoke_function_stream(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
        context_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<BamlStreamInvocation> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            has_context_tags = context_tags.as_ref().map_or(0, |m| m.len()) > 0,
            "Invoking BAML function with streaming"
        );

        // Verify function exists
        let _signature = self
            .state
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;

        // Execute the BAML function using the executor
        let executor = self
            .state
            .executor
            .as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        executor.execute_function_stream(scope, function_name, args, context_tags)
    }
}
