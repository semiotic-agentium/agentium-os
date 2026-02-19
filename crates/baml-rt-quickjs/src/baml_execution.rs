//! BAML function execution engine
//!
//! This module executes BAML functions using the compiled IL (Intermediate Language)
//! from the BAML compiler.

use crate::baml_collector::BamlLLMCollector;
use crate::baml_pre_execution::intercept_llm_call_pre_execution;
use async_trait::async_trait;
use baml_rt_core::bus::EffectEmitter;
use baml_rt_core::context;
use baml_rt_core::{BamlRtError, InvocationKind, Outcome, Result};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry};
use baml_rt_tools::ToolRegistry;
use baml_runtime::{BamlRuntime, FunctionResultStream, RuntimeContextManager};
use baml_types::BamlValue;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

/// Policy for retrying BAML calls when the LLM response fails to parse.
///
/// Allows tests to use `max_attempts: 1` to avoid retry delay and non-determinism.
#[derive(Clone, Debug)]
pub struct ParseRetryPolicy {
    /// Total attempts (initial call + retries). Must be at least 1.
    pub max_attempts: u32,
    /// Base delay in milliseconds between attempts. Delay for attempt N is `delay_ms * N`.
    pub delay_ms: u64,
}

impl Default for ParseRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            delay_ms: 300,
        }
    }
}

/// BAML execution engine that executes BAML IL
#[async_trait]
pub trait ConversationContextProvider: Send + Sync {
    /// Return conversation-history payload for the current runtime scope.
    ///
    /// The payload is injected as `ctx.tags.conversation_history` in BAML templates.
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>>;
}

pub struct BamlExecutor {
    runtime: Arc<BamlRuntime>,
    tool_registry: Arc<ToolRegistry>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    parse_retry_policy: ParseRetryPolicy,
}

impl BamlExecutor {
    /// Load BAML IL from the compiled output
    ///
    /// This loads the BAML runtime from the baml_src directory using from_directory
    pub fn load_il(baml_src_dir: &Path, tool_registry: Arc<ToolRegistry>) -> Result<Self> {
        tracing::info!(?baml_src_dir, "Loading BAML runtime from directory");

        // Use from_directory which handles feature flags internally
        // Load environment variables - BAML uses these for API keys
        let mut env_vars: HashMap<String, String> = HashMap::new();

        // Load OPENROUTER_API_KEY from environment if present
        if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
            env_vars.insert("OPENROUTER_API_KEY".to_string(), api_key);
            tracing::debug!("Loaded OPENROUTER_API_KEY from environment");
        }

        // Load other common API key environment variables
        for key in &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"] {
            if let Ok(value) = std::env::var(key) {
                env_vars.insert(key.to_string(), value);
                tracing::debug!(api_key = key, "Loaded API key from environment");
            }
        }

        let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();

        let runtime = BamlRuntime::from_directory(baml_src_dir, env_vars, feature_flags)
            .map_err(|e| BamlRtError::RuntimeLoadFailed { source: e })?;

        Ok(Self {
            runtime: Arc::new(runtime),
            tool_registry,
            effect_emitter: None,
            conversation_context_provider: None,
            parse_retry_policy: ParseRetryPolicy::default(),
        })
    }

    /// Set the policy for retrying on parse failure (e.g. use `max_attempts: 1` in tests).
    pub fn set_parse_retry_policy(&mut self, policy: ParseRetryPolicy) {
        self.parse_retry_policy = policy;
    }

    /// Set the effect emitter (for effects-first liveness)
    pub fn set_effect_emitter(&mut self, emitter: Arc<dyn EffectEmitter>) {
        self.effect_emitter = Some(emitter);
    }

    /// Set a provider that supplies conversation context for template tags.
    pub fn set_conversation_context_provider(
        &mut self,
        provider: Arc<dyn ConversationContextProvider>,
    ) {
        self.conversation_context_provider = Some(provider);
    }

    /// Execute a BAML function using the compiled IL
    pub async fn execute_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
        interceptor_registry: Option<Arc<Mutex<InterceptorRegistry>>>,
    ) -> Result<Value> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            "Executing BAML function from IL"
        );

        // Convert JSON args to BamlValue map
        let params = self.json_to_baml_map(&args)?;

        // Call the function
        // Load environment variables for API keys
        let mut env_vars = HashMap::new();
        if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
            env_vars.insert("OPENROUTER_API_KEY".to_string(), api_key);
        }
        for key in &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"] {
            if let Ok(value) = std::env::var(key) {
                env_vars.insert(key.to_string(), value);
            }
        }
        let tags = None;

        // Track execution start time for effect completion (our clock, not BAML trace)
        let start_time = Instant::now();

        // Create collector for LLM interception if registry is provided
        let mut collector: Option<BamlLLMCollector> = interceptor_registry
            .as_ref()
            .map(|registry| BamlLLMCollector::new(registry.clone(), function_name.to_string()));

        // Set effect emitter on collector if available
        if let Some(ref mut coll) = collector
            && let Some(ref emitter) = self.effect_emitter
        {
            coll.set_effect_emitter(emitter.clone());
        }

        // Pre-execution interception: intercept LLM calls before they're sent
        let context_tags = self.build_conversation_context_tags(scope).await?;
        let ctx_manager = self.create_ctx_manager_for_scope(scope, context_tags)?;
        if let Some(ref registry) = interceptor_registry {
            match intercept_llm_call_pre_execution(
                &self.runtime,
                scope,
                function_name,
                &params,
                &ctx_manager,
                registry,
                env_vars.clone(),
                InvocationKind::Invoke,
                self.effect_emitter.as_ref(),
                collector.as_ref(),
            )
            .await
            {
                Ok(InterceptorDecision::Allow) => {
                    // Allow the call to proceed
                }
                Ok(InterceptorDecision::Block(msg)) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Failure, 0)
                            .await;
                    }
                    return Err(BamlRtError::BamlRuntime(format!(
                        "LLM call blocked by interceptor: {}",
                        msg
                    )));
                }
                Ok(InterceptorDecision::Substitute(value)) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Success, 0)
                            .await;
                    }
                    return Ok(value);
                }
                Err(e) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Failure, 0)
                            .await;
                    }
                    return Err(e);
                }
            }
        }

        // Wire up the collector to track function execution
        // Note: We track the function call by passing the collector, but we also need
        // to manually track the call_id so we can process trace events later
        let collectors = if let Some(ref collector) = collector {
            Some(vec![collector.as_collector()])
        } else {
            None
        };

        let max_attempts = self.parse_retry_policy.max_attempts.max(1);
        let delay_ms = self.parse_retry_policy.delay_ms;
        let mut last_parse_err: Option<anyhow::Error> = None;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let backoff_ms = delay_ms * attempt as u64;
                tracing::warn!(
                    function = function_name,
                    attempt = attempt + 1,
                    delay_ms = backoff_ms,
                    "Parse failed, retrying BAML call"
                );
                sleep(Duration::from_millis(backoff_ms)).await;
            }

            tracing::info!(
                function = function_name,
                context_id = %scope.context_id().as_str(),
                message_id = %scope.message_id().as_str(),
                task_id = %scope.task_id_opt().map(|id| id.as_str()).unwrap_or("none"),
                attempt = attempt + 1,
                "BAML call_function: start"
            );
            let cancel_tripwire = baml_runtime::TripWire::new(None);
            // Use collectors only on first attempt to avoid duplicate trace events on retries
            let attempt_collectors = if attempt == 0 {
                collectors.clone()
            } else {
                None
            };
            let (result, _call_id) = self
                .runtime
                .call_function(
                    function_name.to_string(),
                    &params,
                    &ctx_manager,
                    None, // type_builder
                    None, // client_registry
                    attempt_collectors,
                    env_vars.clone(),
                    tags,
                    cancel_tripwire,
                )
                .await;

            if let Err(ref e) = result {
                tracing::warn!(
                    function = function_name,
                    error = ?e,
                    elapsed_ms = start_time.elapsed().as_millis() as u64,
                    "BAML call_function: error"
                );
                // Complete effect and return immediately; execution errors are not retried
                if let Some(ref collector) = collector {
                    collector
                        .complete_pending_effects(
                            Outcome::Failure,
                            start_time.elapsed().as_millis() as u64,
                        )
                        .await;
                }
                return Err(BamlRtError::ExecutionFailed {
                    source: result.unwrap_err(),
                });
            }

            let function_result = result.unwrap();
            let parsed_result = function_result.parsed().as_ref().ok_or_else(|| {
                BamlRtError::BamlRuntime("Function returned no parsed result".to_string())
            })?;

            match parsed_result.as_ref() {
                Ok(parsed) => {
                    tracing::info!(
                        function = function_name,
                        elapsed_ms = start_time.elapsed().as_millis() as u64,
                        "BAML call_function: ok"
                    );
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(
                                Outcome::Success,
                                start_time.elapsed().as_millis() as u64,
                            )
                            .await;
                    }
                    // Success path continues below with `parsed`
                    let json_value = serde_json::to_value(parsed.serialize_partial())
                        .map_err(BamlRtError::Json)?;

                    // Process trace events to notify LLM interceptors of completion
                    if let Some(ref collector) = collector
                        && let Err(e) = collector.process_trace_events(scope).await
                    {
                        tracing::warn!(error = ?e, "Failed to process trace events for LLM interception");
                    }

                    if let Some(tool_result) =
                        maybe_execute_tool_from_result(&self.tool_registry, &json_value).await?
                    {
                        return Ok(tool_result);
                    }
                    return Ok(json_value);
                }
                Err(e) => {
                    last_parse_err = Some(anyhow::Error::msg(e.to_string()));
                    if attempt + 1 >= max_attempts {
                        if let Some(ref collector) = collector {
                            collector
                                .complete_pending_effects(
                                    Outcome::Failure,
                                    start_time.elapsed().as_millis() as u64,
                                )
                                .await;
                        }
                        return Err(BamlRtError::ParsedResultFailed {
                            source: last_parse_err.unwrap(),
                        });
                    }
                }
            }
        }

        // Unreachable (loop returns on success or last attempt), but satisfy type checker
        Err(BamlRtError::ParsedResultFailed {
            source: last_parse_err.unwrap_or_else(|| anyhow::Error::msg("Parse failed")),
        })
    }

    /// Execute a BAML function with streaming support
    ///
    /// Returns a stream of incremental results as the function executes.
    pub fn execute_function_stream(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
    ) -> Result<FunctionResultStream> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            "Starting streaming execution of BAML function"
        );

        // Convert JSON args to BamlValue map
        let params = self.json_to_baml_map(&args)?;
        // Streaming path remains unchanged for now; conversation history is injected for
        // non-streaming BAML invocations used by the fixture agent/e2e flow.
        let context_tags = None;
        let ctx_manager = self.create_ctx_manager_for_scope(scope, context_tags)?;

        // Create stream function call
        // Load environment variables for API keys
        let mut env_vars = HashMap::new();
        if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
            env_vars.insert("OPENROUTER_API_KEY".to_string(), api_key);
        }
        for key in &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"] {
            if let Ok(value) = std::env::var(key) {
                env_vars.insert(key.to_string(), value);
            }
        }
        let tags = None;
        let cancel_tripwire = baml_runtime::TripWire::new(None);

        let stream = self
            .runtime
            .stream_function(
                function_name.to_string(),
                &params,
                &ctx_manager,
                None, // type_builder
                None, // client_registry
                None, // collectors
                env_vars,
                cancel_tripwire,
                tags,
            )
            .map_err(|e| BamlRtError::BamlRuntime(format!("Failed to create stream: {}", e)))?;

        Ok(stream)
    }

    /// Create a context manager tied to an explicit runtime scope.
    /// We pass BamlValue::Null for the "language"
    pub fn create_ctx_manager_for_scope(
        &self,
        scope: &context::RuntimeScope,
        extra_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<RuntimeContextManager> {
        let _ = scope; // scope used only for extra_tags from conversation context
        let ctx_manager = self.runtime.create_ctx_manager(BamlValue::Null, None);

        if let Some(tags) = extra_tags
            && !tags.is_empty()
        {
            ctx_manager.upsert_tags(tags);
        }

        Ok(ctx_manager)
    }

    async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let Some(provider) = self.conversation_context_provider.as_ref() else {
            return Ok(None);
        };

        let Some(payload) = provider.conversation_history_json(scope).await? else {
            return Ok(None);
        };

        let mut tags = HashMap::new();
        tags.insert(
            "conversation_history".to_string(),
            self.json_to_baml_value(&payload)?,
        );
        Ok(Some(tags))
    }

    /// List all available function names from the loaded BAML runtime
    pub fn list_functions(&self) -> Vec<String> {
        self.runtime
            .function_names()
            .map(|s| s.to_string())
            .collect()
    }

    /// Convert JSON Value to BamlMap<String, BamlValue>
    fn json_to_baml_map(&self, value: &Value) -> Result<baml_types::BamlMap<String, BamlValue>> {
        let obj = value
            .as_object()
            .ok_or_else(|| BamlRtError::InvalidArgument("Expected JSON object".to_string()))?;

        let mut map = baml_types::BamlMap::new();
        for (k, v) in obj {
            map.insert(k.clone(), self.json_to_baml_value(v)?);
        }
        Ok(map)
    }

    /// Convert JSON Value to BamlValue
    #[allow(clippy::only_used_in_recursion)]
    fn json_to_baml_value(&self, value: &Value) -> Result<BamlValue> {
        match value {
            Value::String(s) => Ok(BamlValue::String(s.clone())),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(BamlValue::Int(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(BamlValue::Float(f))
                } else {
                    Err(BamlRtError::TypeConversion(format!(
                        "Invalid number: {}",
                        n
                    )))
                }
            }
            Value::Bool(b) => Ok(BamlValue::Bool(*b)),
            Value::Array(arr) => {
                let mut vec = Vec::new();
                for item in arr {
                    vec.push(self.json_to_baml_value(item)?);
                }
                Ok(BamlValue::List(vec))
            }
            Value::Object(obj) => {
                let mut map = baml_types::BamlMap::new();
                for (k, v) in obj {
                    map.insert(k.clone(), self.json_to_baml_value(v)?);
                }
                Ok(BamlValue::Map(map))
            }
            Value::Null => Ok(BamlValue::Null),
        }
    }
}

async fn maybe_execute_tool_from_result(
    tool_registry: &Arc<ToolRegistry>,
    result: &Value,
) -> Result<Option<Value>> {
    let Some((tool_name, tool_args)) = extract_tool_call(result)? else {
        return Ok(None);
    };

    let tool_result = tool_registry.execute(&tool_name, tool_args).await?;
    Ok(Some(tool_result))
}

fn extract_tool_call(result: &Value) -> Result<Option<(String, Value)>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };

    if let Some(tool_name) = obj.get("tool_name") {
        return Ok(Some(parse_tool_call_object(obj, tool_name)?));
    }

    if obj.len() == 1 {
        let (_, value) = obj.iter().next().ok_or_else(|| {
            BamlRtError::InvalidArgument("Expected non-empty tool object".to_string())
        })?;
        if let Some(inner) = value.as_object()
            && let Some(tool_name) = inner.get("tool_name")
        {
            return Ok(Some(parse_tool_call_object(inner, tool_name)?));
        }
    }

    Ok(None)
}

fn parse_tool_call_object(
    obj: &serde_json::Map<String, Value>,
    tool_name_value: &Value,
) -> Result<(String, Value)> {
    let tool_name = tool_name_value
        .as_str()
        .ok_or_else(|| BamlRtError::InvalidArgument("tool_name must be a string".to_string()))?;

    let mut tool_args = serde_json::Map::new();
    for (key, value) in obj {
        if key != "tool_name" && key != "__type" {
            tool_args.insert(key.clone(), value.clone());
        }
    }

    Ok((tool_name.to_string(), Value::Object(tool_args)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use baml_rt_tools::BamlTool;
    use baml_rt_tools::bundles::BundleType;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use ts_rs::TS;

    // Test bundle for test tools
    struct Test;

    impl BundleType for Test {
        const NAME: &'static str = "test";
        fn description() -> &'static str {
            "Test tools for unit testing"
        }
    }

    struct EchoTool;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct EchoInput {
        message: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct EchoOutput {
        #[ts(type = "any")]
        echo: serde_json::Value,
    }

    #[async_trait]
    impl BamlTool for EchoTool {
        type Bundle = Test;
        const LOCAL_NAME: &'static str = "echo_tool";
        type OpenInput = ();
        type Input = EchoInput;
        type Output = EchoOutput;

        fn description(&self) -> &'static str {
            "Echoes the input payload."
        }

        async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
            Ok(EchoOutput {
                echo: json!({ "message": args.message }),
            })
        }
    }

    #[tokio::test]
    async fn executes_tool_when_explicit_variant_is_present() {
        use baml_rt_core::ids::{AgentId, UuidId};
        let registry = Arc::new(ToolRegistry::new());
        registry.register(EchoTool).unwrap();
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000040").unwrap());
        let _scope = baml_rt_core::context::InvocationScope::synthetic_message(agent_id);

        let result = json!({
            "tool_name": "test/echo_tool",
            "message": "hello"
        });

        let tool_result = maybe_execute_tool_from_result(&registry, &result)
            .await
            .unwrap()
            .expect("expected tool execution");

        assert_eq!(tool_result["echo"]["message"], "hello");
    }

    #[tokio::test]
    async fn leaves_non_tool_results_untouched() {
        use baml_rt_core::ids::{AgentId, UuidId};
        let registry = Arc::new(ToolRegistry::new());
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000041").unwrap());
        let _scope = baml_rt_core::context::InvocationScope::synthetic_message(agent_id);
        let result = json!({ "value": "not a tool" });
        let tool_result = maybe_execute_tool_from_result(&registry, &result)
            .await
            .unwrap();

        assert!(tool_result.is_none());
    }
}
