//! BAML runtime wrapper and function execution

use crate::baml_execution::BamlExecutor;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::types::FunctionSignature;
use baml_rt_tools::{ToolRegistry as ConcreteToolRegistry, ToolFunctionMetadataExport, ToolSessionId, ToolStep};
use crate::traits::{BamlFunctionExecutor, SchemaLoader};
use baml_rt_interceptor::{InterceptorRegistry, ToolCallContext};
use baml_rt_core::correlation::current_correlation_id;
use baml_rt_core::context;
use baml_rt_observability::metrics;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex as TokioMutex;

// BAML executes in Rust. We will implement execution of BAML functions
// in Rust, then map those function calls to QuickJS so JavaScript can invoke them.
// use baml;

/// Helper function for creating an empty open_input value.
/// 
/// This centralizes the pattern of using an empty JSON object as the default
/// open_input when none is provided.
fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Manages the BAML runtime and function registry
pub struct BamlRuntimeManager {
    function_registry: HashMap<String, FunctionSignature>,
    pub(crate) executor: Option<BamlExecutor>,
    tool_registry: Arc<TokioMutex<ConcreteToolRegistry>>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    tool_session_scopes: Arc<TokioMutex<HashMap<ToolSessionId, ToolSessionScope>>>,
    tool_session_states: Arc<TokioMutex<HashMap<ToolSessionId, ToolCallSessionState>>>,
}

#[derive(Debug, Clone)]
struct ToolCallSessionState {
    context: ToolCallContext,
    start: Instant,
}

#[derive(Debug, Clone)]
struct ToolSessionScope {
    tool_name: String,
    scope: Option<context::RuntimeScope>,
}

impl BamlRuntimeManager {
    /// Create a new BAML runtime manager
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing BAML runtime manager");

        Ok(Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(TokioMutex::new(ConcreteToolRegistry::new())),
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_states: Arc::new(TokioMutex::new(HashMap::new())),
        })
    }

    /// Check if a schema is loaded
    pub fn is_schema_loaded(&self) -> bool {
        self.executor.is_some()
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
            schema_path_obj.parent()
                .and_then(|p| p.parent())
        } else if schema_path_obj.file_name() == Some(std::ffi::OsStr::new("baml_src")) {
            schema_path_obj.parent()
        } else {
            Some(schema_path_obj)
        }
        .ok_or_else(|| BamlRtError::InvalidArgument("Invalid schema path".to_string()))?;

        let baml_src_dir = project_root.join("baml_src");
        if !baml_src_dir.exists() {
            return Err(BamlRtError::BamlRuntime(
                "baml_src directory not found".to_string()
            ));
        }

        // Load BAML IL into executor (pass tool registry)
        let tool_registry_clone = self.tool_registry.clone();
        let executor = BamlExecutor::load_il(&baml_src_dir, tool_registry_clone)?;

        // Discover functions from the BAML runtime
        let function_names = executor.list_functions();
        for func_name in function_names {
            // Register function signature
            self.function_registry.insert(
                func_name.clone(),
                FunctionSignature {
                    name: func_name.clone(),
                    input_types: vec![],
                    output_type: baml_rt_core::types::BamlType::String,
                },
            );
        }

        self.executor = Some(executor);

        tracing::info!(
            function_count = self.function_registry.len(),
            "Loaded BAML IL"
        );

        Ok(())
    }

    /// Get the signature of a function by name
    pub fn get_function_signature(&self, name: &str) -> Option<&FunctionSignature> {
        self.function_registry.get(name)
    }

    /// Execute a BAML function with the given arguments
    ///
    /// This is the main entry point for executing BAML functions.
    /// It validates the function exists and delegates to the executor.
    pub async fn invoke_function(
        &self,
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
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;

        // Execute the BAML function using the executor
        let executor = self.executor.as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        // Pass tool registry and interceptor registry to executor
        let interceptor_registry = Some(self.interceptor_registry.clone());
        executor.execute_function(function_name, args, interceptor_registry).await
    }

    /// Invoke a BAML function with streaming support
    ///
    /// Returns a stream that yields incremental results as the function executes.
    pub fn invoke_function_stream(
        &self,
        function_name: &str,
        args: serde_json::Value,
    ) -> Result<baml_runtime::FunctionResultStream> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            "Invoking BAML function with streaming"
        );

        // Verify function exists
        let _signature = self
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;

        // Execute the BAML function using the executor
        let executor = self.executor.as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        executor.execute_function_stream(function_name, args)
    }

    /// List all available BAML functions
    pub fn list_functions(&self) -> Vec<String> {
        self.function_registry.keys().cloned().collect()
    }

    /// Get the tool registry (for tool registration)
    pub fn tool_registry(&self) -> Arc<TokioMutex<ConcreteToolRegistry>> {
        self.tool_registry.clone()
    }

    /// Get the interceptor registry (for registering interceptors)
    pub fn interceptor_registry(&self) -> Arc<TokioMutex<InterceptorRegistry>> {
        self.interceptor_registry.clone()
    }

    /// Register an LLM interceptor
    pub async fn register_llm_interceptor<I: baml_rt_interceptor::LLMInterceptor>(&self, interceptor: I) {
        let mut registry = self.interceptor_registry.lock().await;
        registry.register_llm_interceptor(interceptor);
    }

    /// Register a tool interceptor
    pub async fn register_tool_interceptor<I: baml_rt_interceptor::ToolInterceptor>(&self, interceptor: I) {
        let mut registry = self.interceptor_registry.lock().await;
        registry.register_tool_interceptor(interceptor);
    }

    /// Register a tool that implements the BamlTool trait
    ///
    /// Tools can be called by LLMs during BAML function execution
    /// or directly from JavaScript via the QuickJS bridge.
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt::baml::BamlRuntimeManager;
    /// use baml_rt::tools::BamlTool;
    /// use async_trait::async_trait;
    /// use schemars::JsonSchema;
    /// use serde::{Deserialize, Serialize};
    /// use ts_rs::TS;
    ///
    /// struct MyTool;
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyInput {}
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyOutput {
    ///     result: String,
    /// }
    ///
    /// #[async_trait]
    /// impl BamlTool for MyTool {
    ///     const NAME: &'static str = "my_tool";
    ///     type Input = MyInput;
    ///     type Output = MyOutput;
    ///     fn description(&self) -> &'static str { "Does something" }
    ///     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    ///         Ok(MyOutput { result: "success".to_string() })
    ///     }
    /// }
    ///
    /// # tokio_test::block_on(async {
    /// let mut manager = BamlRuntimeManager::new()?;
    /// manager.register_tool(MyTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    pub async fn register_tool<T: baml_rt_tools::BamlTool>(&mut self, tool: T) -> Result<()> {
        let mut registry = self.tool_registry.lock().await;
        registry.register(tool)
    }

    /// Execute a tool function by name
    ///
    /// This will call tool interceptors before and after execution.
    pub async fn execute_tool(&self, name: &str, args: Value) -> Result<Value> {
        use baml_rt_interceptor::ToolCallContext;
        use std::time::Instant;

        let start = Instant::now();
        let correlation_id = current_correlation_id();
        let mut metadata_map = serde_json::Map::new();
        if let Some(correlation_id) = correlation_id {
            metadata_map.insert(
                "correlation_id".to_string(),
                Value::String(correlation_id.to_string()),
            );
        }
        if let Some(message_id) = context::current_message_id() {
            metadata_map.insert("message_id".to_string(), Value::String(message_id.as_str().to_string()));
        }
        let metadata = Value::Object(metadata_map);

        // Build context for interceptors
        let context = ToolCallContext {
            tool_name: name.to_string(),
            function_name: None, // Could be enhanced to track which function called this tool
            args: args.clone(),
            metadata,
            context_id: context::current_or_new(),
        };

        // Run interceptors before execution
        let interceptor_registry = self.interceptor_registry.lock().await;
        let _decision = interceptor_registry.intercept_tool_call(&context).await?;
        drop(interceptor_registry);

        // Handle interceptor decision
        // If we get here, the decision is Allow (blocking would have returned Err)
        let final_args = args;

        // Execute the tool
        let mut registry = self.tool_registry.lock().await;
        let result = registry.execute(name, final_args).await;
        drop(registry);

        // Calculate duration
        let duration = start.elapsed();
        let duration_ms = duration.as_millis() as u64;

        // Notify interceptors of completion
        let interceptor_registry = self.interceptor_registry.lock().await;
        interceptor_registry.notify_tool_call_complete(&context, &result, duration_ms).await;
        drop(interceptor_registry);

        let metric_result = if result.is_ok() { "success" } else { "error" };
        metrics::record_tool_invocation(name, metric_result, duration);

        result
    }

    /// List all registered tools
    pub async fn list_tools(&self) -> Vec<String> {
        let registry = self.tool_registry.lock().await;
        registry.list_tools()
    }

    pub async fn set_tool_allowlist(&self, allowlist: HashSet<String>) -> Result<()> {
        let mut registry = self.tool_registry.lock().await;
        registry.set_allowlist_from_strings(allowlist)?;
        Ok(())
    }

    pub async fn open_tool_session(&self, tool_name: &str, open_input: serde_json::Value) -> Result<ToolSessionId> {
        let mut registry = self.tool_registry.lock().await;
        let session_id = registry.open_session(tool_name, open_input).await?;
        drop(registry);
        let scope = context::current_scope();
        let mut scopes = self.tool_session_scopes.lock().await;
        scopes.insert(
            session_id.clone(),
            ToolSessionScope {
                tool_name: tool_name.to_string(),
                scope,
            },
        );
        Ok(session_id)
    }

    pub async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        use baml_rt_interceptor::InterceptorDecision;

        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };
        let session_scope = session_scope.ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "Unknown tool session {}",
                session_id.to_string()
            ))
        })?;

        let run = || async {
            let start = Instant::now();
            let correlation_id = current_correlation_id();
            let mut metadata_map = serde_json::Map::new();
            if let Some(correlation_id) = correlation_id {
                metadata_map.insert(
                    "correlation_id".to_string(),
                    Value::String(correlation_id.to_string()),
                );
            }
            if let Some(message_id) = context::current_message_id() {
                metadata_map.insert(
                    "message_id".to_string(),
                    Value::String(message_id.as_str().to_string()),
                );
            }
            let metadata = Value::Object(metadata_map);

            let context = ToolCallContext {
                tool_name: session_scope.tool_name.clone(),
                function_name: None,
                args: input.clone(),
                metadata,
                context_id: context::current_or_new(),
            };

            let interceptor_registry = self.interceptor_registry.lock().await;
            let _decision: InterceptorDecision =
                interceptor_registry.intercept_tool_call(&context).await?;
            drop(interceptor_registry);

            {
                let mut states = self.tool_session_states.lock().await;
                states.insert(
                    session_id.clone(),
                    ToolCallSessionState {
                        context: context.clone(),
                        start,
                    },
                );
            }

            let registry = self.tool_registry.lock().await;
            let result = registry.session_send(session_id, input).await;
            drop(registry);

            if result.is_err() {
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(BamlRtError::InvalidArgument(err.to_string())),
                };
                let duration_ms = start.elapsed().as_millis() as u64;
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                let mut states = self.tool_session_states.lock().await;
                states.remove(session_id);
                let mut scopes = self.tool_session_scopes.lock().await;
                scopes.remove(session_id);
            }

            result
        };

        if let Some(scope) = session_scope.scope.clone() {
            context::with_scope(scope, run()).await
        } else {
            run().await
        }
    }

    pub async fn tool_session_next(&self, session_id: &ToolSessionId) -> Result<ToolStep> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            let registry = self.tool_registry.lock().await;
            let result = registry.session_next(session_id).await;
            drop(registry);

            let completion = match &result {
                Ok(ToolStep::Done { output }) => Some(Ok(output.clone().unwrap_or(Value::Null))),
                Ok(ToolStep::Error { error }) => {
                    Some(Err(BamlRtError::InvalidArgument(format!(
                        "Tool failure ({:?}): {}",
                        error.kind, error.message
                    ))))
                }
                Err(err) => Some(Err(BamlRtError::InvalidArgument(err.to_string()))),
                _ => None,
            };

            if let Some(completion_result) = completion {
                if let Some(state) = {
                    let mut states = self.tool_session_states.lock().await;
                    states.remove(session_id)
                } {
                    let mut scopes = self.tool_session_scopes.lock().await;
                    scopes.remove(session_id);
                    let duration_ms = state.start.elapsed().as_millis() as u64;
                    let interceptor_registry = self.interceptor_registry.lock().await;
                    interceptor_registry
                        .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                        .await;
                    drop(interceptor_registry);
                    let metric_result =
                        if completion_result.is_ok() { "success" } else { "error" };
                    metrics::record_tool_invocation(
                        &state.context.tool_name,
                        metric_result,
                        state.start.elapsed(),
                    );
                }
            }

            result
        };

        if let Some(scope) = session_scope.and_then(|value| value.scope) {
            context::with_scope(scope, run()).await
        } else {
            run().await
        }
    }

    pub async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            let mut registry = self.tool_registry.lock().await;
            let result = registry.session_finish(session_id).await;
            drop(registry);

            if let Some(state) = {
                let mut states = self.tool_session_states.lock().await;
                states.remove(session_id)
            } {
                let mut scopes = self.tool_session_scopes.lock().await;
                scopes.remove(session_id);
                let duration_ms = state.start.elapsed().as_millis() as u64;
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(BamlRtError::InvalidArgument(err.to_string())),
                };
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                let metric_result =
                    if completion_result.is_ok() { "success" } else { "error" };
                metrics::record_tool_invocation(
                    &state.context.tool_name,
                    metric_result,
                    state.start.elapsed(),
                );
            }

            result
        };

        if let Some(scope) = session_scope.and_then(|value| value.scope) {
            context::with_scope(scope, run()).await
        } else {
            run().await
        }
    }

    pub async fn tool_session_abort(&self, session_id: &ToolSessionId, reason: Option<String>) -> Result<()> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            let mut registry = self.tool_registry.lock().await;
            let result = registry.session_abort(session_id, reason.clone()).await;
            drop(registry);

            if let Some(state) = {
                let mut states = self.tool_session_states.lock().await;
                states.remove(session_id)
            } {
                let mut scopes = self.tool_session_scopes.lock().await;
                scopes.remove(session_id);
                let duration_ms = state.start.elapsed().as_millis() as u64;
                let completion_result = Err(BamlRtError::InvalidArgument(
                    reason.unwrap_or_else(|| "Tool session aborted".to_string()),
                ));
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                metrics::record_tool_invocation(
                    &state.context.tool_name,
                    "error",
                    state.start.elapsed(),
                );
            }

            result
        };

        if let Some(scope) = session_scope.and_then(|value| value.scope) {
            context::with_scope(scope, run()).await
        } else {
            run().await
        }
    }

    /// Get tool metadata (export-safe shape)
    pub async fn get_tool_metadata(&self, name: &str) -> Option<ToolFunctionMetadataExport> {
        let registry = self.tool_registry.lock().await;
        registry
            .get_metadata(name)
            .map(ToolFunctionMetadataExport::from)
    }

    pub async fn export_tool_metadata(&self) -> Vec<ToolFunctionMetadataExport> {
        let registry = self.tool_registry.lock().await;
        registry.export_metadata_records()
    }

    pub async fn write_tool_metadata(&self, path: &Path) -> Result<()> {
        let metadata = self.export_tool_metadata().await;
        let payload = serde_json::json!({ "tools": metadata });
        let content = serde_json::to_string_pretty(&payload).map_err(BamlRtError::Json)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        fs::write(path, content).map_err(BamlRtError::Io)?;
        Ok(())
    }

    pub async fn write_tool_typescript(&self, path: &Path) -> Result<()> {
        let registry = self.tool_registry.lock().await;
        registry.write_typescript_declarations(path)
    }

    pub async fn validate_tool_allowlist_registered(&self) -> Result<()> {
        let registry = self.tool_registry.lock().await;
        registry.validate_allowlist_registered()
    }

    /// Execute a tool from a BAML result
    ///
    /// BAML returns either:
    /// - A `ToolSessionPlan` describing FSM steps, or
    /// - A `tool_name` payload for a one-shot session.
    ///
    /// The runtime executes host tools via the session FSM in Rust.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use baml_rt::baml::BamlRuntimeManager;
    /// # use baml_rt::tools::BamlTool;
    /// # use async_trait::async_trait;
    /// # use schemars::JsonSchema;
    /// # use serde::{Deserialize, Serialize};
    /// # use ts_rs::TS;
    /// # struct WeatherTool;
    /// # #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// # #[ts(export)]
    /// # struct WeatherInput { location: String }
    /// # #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// # #[ts(export)]
    /// # struct WeatherOutput { temperature: String }
    /// # #[async_trait]
    /// # impl BamlTool for WeatherTool {
    /// #     const NAME: &'static str = "support/get_weather";
    /// #     type Input = WeatherInput;
    /// #     type Output = WeatherOutput;
    /// #     fn description(&self) -> &'static str { "" }
    /// #     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    /// #         Ok(WeatherOutput { temperature: "22°C".to_string() })
    /// #     }
    /// # }
    /// # tokio_test::block_on(async {
    /// # let mut manager = BamlRuntimeManager::new()?;
    /// manager.register_tool(WeatherTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    /// Execute a tool from a BAML union type result
    ///
    /// Takes a BAML result (typed class or single-key object),
    /// derives the tool from the type name, and executes it.
    ///
    /// # Arguments
    /// * `baml_result` - The JSON result from BAML function (union variant)
    ///
    /// # Returns
    /// The result of executing the tool function
    pub async fn execute_tool_from_baml_result(&self, baml_result: Value) -> Result<Value> {
        let call = extract_tool_call(&baml_result)?
            .ok_or_else(|| BamlRtError::InvalidArgument("No tool call found in result".to_string()))?;
        let tool_name = self.resolve_tool_name_from_input(&call.args).await?;
        self.execute_tool(&tool_name, call.args).await
    }

    pub async fn execute_tool_from_baml_result_or_value(
        &self,
        baml_result: Value,
    ) -> Result<Value> {
        if let Some(plan) = extract_tool_session_plan(&baml_result)? {
            let tool_name = self.resolve_tool_name_from_plan_steps(&plan).await?;
            return self.execute_tool_session_plan(tool_name, plan).await;
        }
        if let Some(call) = extract_tool_call(&baml_result)? {
            let tool_name = self.resolve_tool_name_from_input(&call.args).await?;
            return self.execute_tool(&tool_name, call.args).await;
        }
        Ok(baml_result)
    }

    async fn resolve_tool_name_from_plan_steps(
        &self,
        steps: &[ToolSessionPlanStep],
    ) -> Result<String> {
        let input = steps.iter().find_map(|step| {
            step.initial_input
                .as_ref()
                .or_else(|| step.input.as_ref())
        });
        let input = input.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "ToolSessionPlan must include initial_input or input to bind a tool".to_string(),
            )
        })?;
        self.resolve_tool_name_from_input(input).await
    }

    async fn resolve_tool_name_from_input(&self, input: &Value) -> Result<String> {
        let registry = self.tool_registry.lock().await;
        let mut matches = registry
            .all_metadata()
            .into_iter()
            .filter_map(|metadata| {
                if input_matches_schema(input, &metadata.input_schema) {
                    Some(metadata.name.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        match matches.len() {
            1 => Ok(matches.pop().unwrap()),
            0 => Err(BamlRtError::InvalidArgument(format!(
                "No tool input schema matched input: {}",
                input
            ))),
            _ => Err(BamlRtError::InvalidArgument(format!(
                "Multiple tools matched input schema: {}",
                matches.join(", ")
            ))),
        }
    }

    async fn execute_tool_session_plan(
        &self,
        tool_name: String,
        steps: Vec<ToolSessionPlanStep>,
    ) -> Result<Value> {
        // Validate FSM: must start with Open before any Send
        let first_non_open = steps.iter().position(|s| s.op != "open");
        if let Some(pos) = first_non_open {
            if steps[pos].op == "send" {
                return Err(BamlRtError::InvalidArgument(format!(
                    "FSM violation: plan has '{}' step at position {} before any 'open' step. FSM requires Open before Send.",
                    steps[pos].op, pos
                )));
            }
        }
        if steps.is_empty() {
            return Err(BamlRtError::InvalidArgument("ToolSessionPlan must have at least one step".to_string()));
        }

        let mut session_id: Option<ToolSessionId> = None;
        let mut last_output: Option<Value> = None;
        let mut streaming_outputs: Vec<Value> = Vec::new();

        for step in steps {
            match step.op.as_str() {
                "open" => {
                    if session_id.is_some() {
                        return Err(BamlRtError::InvalidArgument(
                            "Tool session already open".to_string(),
                        ));
                    }
                    // For Open step, use initial_input if provided, otherwise empty object
                    let open_input = step.initial_input.clone().unwrap_or_else(empty_open_input);
                    let session = self.open_tool_session(&tool_name, open_input).await?;
                    session_id = Some(session.clone());
                    // If Open step has initial_input, automatically Send it
                    if let Some(initial_input) = step.initial_input {
                        let normalized = normalize_plan_input(initial_input)?;
                        self.tool_session_send(&session, normalized).await?;
                    }
                }
                "send" => {
                    let session = session_id.as_ref().ok_or_else(|| {
                        BamlRtError::InvalidArgument("send step before open: FSM requires Open before Send".to_string())
                    })?;
                    // Send steps must use 'input' field, not 'initial_input'
                    // Treat null/None as missing input
                    let input = step.input
                        .filter(|v| !v.is_null())
                        .ok_or_else(|| {
                            if step.initial_input.is_some() {
                                BamlRtError::InvalidArgument("send step must use 'input' field, not 'initial_input' (initial_input is only for Open steps)".to_string())
                            } else {
                                BamlRtError::InvalidArgument("send step missing input (input is null or missing)".to_string())
                            }
                        })?;
                    let normalized = normalize_plan_input(input)?;
                    self.tool_session_send(session, normalized).await?;
                }
                "next" => {
                    let session = session_id.as_ref().ok_or_else(|| {
                        BamlRtError::InvalidArgument("next step before open".to_string())
                    })?;
                    loop {
                        match self.tool_session_next(session).await? {
                            ToolStep::Streaming { output } => {
                                streaming_outputs.push(output);
                            }
                            ToolStep::Done { output } => {
                                last_output = output;
                                self.tool_session_finish(session).await?;
                                session_id = None;
                                break;
                            }
                            ToolStep::Error { error } => {
                                self.tool_session_abort(session, Some(error.message.clone())).await?;
                                return Err(BamlRtError::InvalidArgument(format!(
                                    "Tool failure ({:?}): {}",
                                    error.kind, error.message
                                )));
                            }
                        }
                    }
                }
                "finish" => {
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_finish(session).await?;
                        session_id = None;
                    }
                }
                "abort" => {
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_abort(session, step.reason).await?;
                        session_id = None;
                    }
                }
                other => {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "Unknown tool session op '{}'",
                        other
                    )));
                }
            }
        }

        // If session is still open and no explicit Next was called, call Next to get result
        if let Some(session) = session_id.as_ref() {
            loop {
                match self.tool_session_next(session).await? {
                    ToolStep::Streaming { output } => {
                        streaming_outputs.push(output);
                    }
                    ToolStep::Done { output } => {
                        last_output = output;
                        self.tool_session_finish(session).await?;
                        break;
                    }
                    ToolStep::Error { error } => {
                        self.tool_session_abort(session, Some(error.message.clone())).await?;
                        return Err(BamlRtError::InvalidArgument(format!(
                            "Tool failure ({:?}): {}",
                            error.kind, error.message
                        )));
                    }
                }
            }
        }

        if !streaming_outputs.is_empty() {
            if let Some(done) = last_output {
                streaming_outputs.push(done);
            }
            return Ok(Value::Array(streaming_outputs));
        }

        Ok(last_output.unwrap_or(Value::Null))
    }
}

// Implement traits for better abstraction
#[async_trait]
impl BamlFunctionExecutor for BamlRuntimeManager {
    async fn execute_function(&self, function_name: &str, args: Value) -> Result<Value> {
        self.invoke_function(function_name, args).await
    }

    fn list_functions(&self) -> Vec<String> {
        self.function_registry.keys().cloned().collect()
    }
}

impl SchemaLoader for BamlRuntimeManager {
    fn load_schema(&mut self, schema_path: &str) -> Result<()> {
        self.load_schema(schema_path)
    }

    fn is_schema_loaded(&self) -> bool {
        self.is_schema_loaded()
    }
}

impl Default for BamlRuntimeManager {
    fn default() -> Self {
        Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(TokioMutex::new(ConcreteToolRegistry::new())),
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_states: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Clone)]
struct ToolCall {
    args: Value,
}

fn extract_tool_call(result: &Value) -> Result<Option<ToolCall>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };

    if obj.contains_key("tool_name") {
        return Err(BamlRtError::InvalidArgument(
            "Tool call must not include tool_name; tool identity is derived from input schema"
                .to_string(),
        ));
    }

    if obj.get("__type").is_some() {
        let mut tool_args = serde_json::Map::new();
        for (key, value) in obj {
            if key != "__type" {
                tool_args.insert(key.clone(), value.clone());
            }
        }
        return Ok(Some(ToolCall {
            args: Value::Object(tool_args),
        }));
    }

    if obj.len() == 1 {
        let (_, value) = obj.iter().next().ok_or_else(|| {
            BamlRtError::InvalidArgument("Expected non-empty tool object".to_string())
        })?;
        if let Some(inner) = value.as_object() {
            if inner.contains_key("tool_name") {
                return Err(BamlRtError::InvalidArgument(
                    "Tool call must not include tool_name; tool identity is derived from input schema"
                        .to_string(),
                ));
            }
            let mut tool_args = serde_json::Map::new();
            for (key, value) in inner {
                if key != "__type" {
                    tool_args.insert(key.clone(), value.clone());
                }
            }
            return Ok(Some(ToolCall {
                args: Value::Object(tool_args),
            }));
        }
    }

    Ok(None)
}

fn input_matches_schema(input: &Value, schema: &Value) -> bool {
    let input_obj = match input.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    let schema_obj = match schema.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    if let Some(Value::String(schema_type)) = schema_obj.get("type") {
        if schema_type != "object" {
            return false;
        }
    }
    if let Some(required) = schema_obj.get("required").and_then(|v| v.as_array()) {
        for req in required {
            if let Some(req_key) = req.as_str() {
                if !input_obj.contains_key(req_key) {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Debug, Clone)]
struct ToolSessionPlanStep {
    op: String,
    initial_input: Option<Value>, // For Open step - initial input when opening session
    input: Option<Value>,         // For Send step - subsequent inputs
    reason: Option<String>,
}


fn extract_tool_session_plan(result: &Value) -> Result<Option<Vec<ToolSessionPlanStep>>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };
    let steps_value = match obj.get("steps") {
        Some(value) => value,
        None => return Ok(None),
    };
    let steps_array = steps_value.as_array().ok_or_else(|| {
        BamlRtError::InvalidArgument("ToolSessionPlan.steps must be an array".to_string())
    })?;

    let mut steps = Vec::new();
    for step_value in steps_array {
        let step_obj = step_value.as_object().ok_or_else(|| {
            BamlRtError::InvalidArgument("ToolSessionPlan step must be an object".to_string())
        })?;
        if step_obj.contains_key("tool_name") {
            return Err(BamlRtError::InvalidArgument(
                "ToolSessionPlan step must not include tool_name; tool identity is bound by plan type"
                    .to_string(),
            ));
        }
        
        // Determine step type from __type field (BAML union type discriminator)
        let step_type = step_obj.get("__type").and_then(|v| v.as_str());
        let op = step_obj
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("ToolSessionPlan step missing op".to_string())
            })?
            .to_ascii_lowercase();
        
        // Extract fields based on step type (union variant)
        let (initial_input, input) = match step_type {
            Some("SupportCalculateOpenStep") | Some(_) if op == "open" => {
                // Open step: has initial_input field
                (step_obj.get("initial_input").cloned(), None)
            }
            Some("SupportCalculateSendStep") | Some(_) if op == "send" => {
                // Send step: has input field (required)
                let input_val = step_obj.get("input").cloned().ok_or_else(|| {
                    BamlRtError::InvalidArgument("Send step missing required 'input' field".to_string())
                })?;
                (None, Some(input_val))
            }
            _ if op == "next" || op == "finish" || op == "abort" => {
                // Next/Finish/Abort steps: no input fields
                (None, None)
            }
            _ => {
                // Fallback: try to infer from op field
                if op == "open" {
                    (step_obj.get("initial_input").cloned(), None)
                } else if op == "send" {
                    let input_val = step_obj.get("input").cloned().ok_or_else(|| {
                        BamlRtError::InvalidArgument("Send step missing required 'input' field".to_string())
                    })?;
                    (None, Some(input_val))
                } else {
                    (None, None)
                }
            }
        };
        
        let reason = step_obj.get("reason").and_then(|v| v.as_str()).map(|s| s.to_string());
        steps.push(ToolSessionPlanStep {
            op,
            initial_input,
            input,
            reason,
        });
    }

    Ok(Some(steps))
}

fn normalize_plan_input(value: Value) -> Result<Value> {
    match value {
        Value::String(raw) => serde_json::from_str(&raw)
            .map_err(|e| BamlRtError::InvalidArgument(format!("Invalid plan input JSON: {}", e))),
        other => Ok(other),
    }
}
