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
        tracing::info!(schema_path = schema_path, "Loading BAML IL");

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

        tracing::info!(
            function_count = self.state.function_registry.len(),
            session_plan_manifest = self
                .state
                .session_plan_functions
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

        // Pass tool registry and interceptor registry to executor.
        // Build merged context tags (persisted history + intra-turn buffer) so the LLM
        // sees prior hops from this turn even when async provenance writes haven't landed.
        let interceptor_registry = Some(self.state.interceptor_registry.clone());
        let planning_step = resolve_planning_step(&self.state.execution_sessions, scope);
        let invocation_args = args.clone();
        let merged_tags = self.build_conversation_context_tags(scope).await?;
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
