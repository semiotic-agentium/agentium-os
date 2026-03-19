//! Tests for tool registration (Rust and JavaScript)

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::tools::BamlTool;
use baml_rt_core::{
    context::InvocationScope,
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_tools::{
    ToolCapability, ToolHandler, ToolMetadataBuilder, ToolName, ToolOrigin, ToolSession,
    ToolSessionError, ToolStep, TypeBasedMetadataBuilder,
    bundles::BundleType,
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use test_support::common::{
    assert_tool_registered_in_js, setup_baml_runtime_default, setup_baml_runtime_manager_default,
    setup_bridge,
};
use tokio::sync::Mutex;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}

// Simple test tools
struct AddNumbersTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct AddNumbersInput {
    a: f64,
    b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct AddNumbersOutput {
    result: f64,
}

#[async_trait]
impl BamlTool for AddNumbersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "add_numbers";
    type OpenInput = ();
    type Input = AddNumbersInput;
    type Output = AddNumbersOutput;

    fn description(&self) -> &'static str {
        "Adds two numbers together"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(AddNumbersOutput {
            result: args.a + args.b,
        })
    }
}

struct GreetTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct GreetInput {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct GreetOutput {
    greeting: String,
}

#[async_trait]
impl BamlTool for GreetTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "greet";
    type OpenInput = ();
    type Input = GreetInput;
    type Output = GreetOutput;

    fn description(&self) -> &'static str {
        "Returns a greeting message"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(GreetOutput {
            greeting: format!("Hello, {}!", args.name),
        })
    }
}

struct StreamLettersTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct StreamLettersInput {
    word: String,
}

struct SyntheticSessionEvalTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct SyntheticSessionEvalInput {
    retrieve_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct SyntheticSessionEvalOutput {
    refs: Vec<String>,
    items: Vec<String>,
}

#[async_trait]
impl BamlTool for SyntheticSessionEvalTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "synthetic_session_eval";
    type OpenInput = ();
    type Input = SyntheticSessionEvalInput;
    type Output = SyntheticSessionEvalOutput;

    fn description(&self) -> &'static str {
        "Deterministic test-only corpus slices"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        let corpus = ["slice-0", "slice-1", "slice-2"];
        if let Some(retrieve_ref) = args.retrieve_ref {
            let idx = retrieve_ref
                .strip_prefix("ref:")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            return Ok(SyntheticSessionEvalOutput {
                refs: vec![format!("ref:{idx}")],
                items: vec![corpus.get(idx).unwrap_or(&corpus[0]).to_string()],
            });
        }
        Ok(SyntheticSessionEvalOutput {
            refs: (0..corpus.len()).map(|idx| format!("ref:{idx}")).collect(),
            items: vec![],
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct StreamLettersOutput {
    letters: Vec<String>,
    count: usize,
}

#[async_trait]
impl BamlTool for StreamLettersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "stream_letters";
    type OpenInput = ();
    type Input = StreamLettersInput;
    type Output = StreamLettersOutput;

    fn description(&self) -> &'static str {
        "Streams letters of a word one by one"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        use tokio::time::{Duration, sleep};

        // Simulate streaming by waiting a bit
        sleep(Duration::from_millis(10)).await;

        // Return all letters as an array (in a real streaming scenario,
        // this would be a stream, but for now we return the result)
        let letters: Vec<String> = args.word.chars().map(|c| c.to_string()).collect();
        Ok(StreamLettersOutput {
            count: letters.len(),
            letters,
        })
    }
}

struct ScopeCaptureHandler {
    metadata: ToolFunctionMetadata,
    captures: Arc<StdMutex<Vec<Option<TaskId>>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct ScopeCaptureInput {}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct ScopeCaptureOutput {
    task_id: Option<String>,
}

struct ScopeCaptureSession {
    task_id: Option<TaskId>,
}

#[async_trait]
impl ToolSession for ScopeCaptureSession {
    async fn send(
        &mut self,
        _input: serde_json::Value,
    ) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let output = ScopeCaptureOutput {
            task_id: self.task_id.as_ref().map(|id| id.as_str().to_string()),
        };
        Ok(ToolStep::Done {
            output: Some(serde_json::to_value(output).expect("scope capture output JSON")),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }
}

#[async_trait]
impl ToolHandler for ScopeCaptureHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        _open_input: serde_json::Value,
    ) -> baml_rt::Result<Box<dyn ToolSession>> {
        self.captures.lock().unwrap().push(ctx.task_id.clone());
        Ok(Box::new(ScopeCaptureSession {
            task_id: ctx.task_id,
        }))
    }
}

#[tokio::test]
async fn test_register_and_execute_tool_rust() {
    // Create BAML runtime manager
    let baml_manager = setup_baml_runtime_default();

    // Register a simple calculator tool using the trait
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(AddNumbersTool).await.unwrap();
    }

    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-00000000000d").unwrap(),
    ));
    // Test executing the tool directly from Rust (scope required)
    {
        let manager = baml_manager.lock().await;
        let result = manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "test/add_numbers",
                json!({"a": 5, "b": 3}),
            )
            .await
            .unwrap();

        let result_obj = result.as_object().expect("Expected object");
        let sum = result_obj
            .get("result")
            .and_then(|v| v.as_f64())
            .expect("Expected 'result' number");

        assert_eq!(sum, 8.0, "5 + 3 should equal 8");
    }

    // Test listing tools
    {
        let manager = baml_manager.lock().await;
        let tools = manager.list_tools().await;
        assert!(
            tools.contains(&"test/add_numbers".to_string()),
            "Should list registered tool"
        );
    }
}

#[tokio::test]
async fn test_register_and_execute_tool_js() {
    // Create BAML runtime manager
    let baml_manager = setup_baml_runtime_default();

    // Register a tool using the trait
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(GreetTool).await.unwrap();
    }

    // Create QuickJS bridge and register functions
    let mut bridge = setup_bridge(baml_manager.clone()).await;

    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-00000000000b").unwrap(),
    ));
    // Test that tool is registered in QuickJS (scope required for Rust tools via openToolSession)
    assert_tool_registered_in_js(&mut bridge, "test/greet", &scope).await;

    // Test executing the tool directly from Rust to verify it works end-to-end
    {
        let manager = baml_manager.lock().await;
        let result = manager
            .execute_tool_with_scope(scope.as_scope(), "test/greet", json!({"name": "World"}))
            .await
            .unwrap();

        let result_obj = result.as_object().expect("Expected object");
        let greeting = result_obj.get("greeting").and_then(|g| g.as_str()).unwrap();
        assert_eq!(greeting, "Hello, World!", "Should return correct greeting");
    }
}

#[tokio::test]
async fn test_async_streaming_tool() {
    // Create BAML runtime manager
    let baml_manager = setup_baml_runtime_default();

    // Register an async streaming tool using the trait
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(StreamLettersTool).await.unwrap();
    }

    // Test executing the streaming tool (scope required)
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-00000000000c").unwrap(),
    ));
    {
        let manager = baml_manager.lock().await;
        let result = manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "test/stream_letters",
                json!({"word": "test"}),
            )
            .await
            .unwrap();

        let result_obj = result.as_object().expect("Expected object");
        let letters = result_obj
            .get("letters")
            .and_then(|v| v.as_array())
            .expect("Expected 'letters' array");
        let count = result_obj
            .get("count")
            .and_then(|v| v.as_u64())
            .expect("Expected 'count' number");

        assert_eq!(count, 4, "Word 'test' has 4 letters");
        assert_eq!(letters.len(), 4, "Should return 4 letters");
    }
}

#[tokio::test]
async fn test_register_js_tool() {
    tracing::info!("Test: Register JavaScript tool");

    // Set up BAML runtime and bridge
    let baml_manager = setup_baml_runtime_default();

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Register a simple JavaScript tool
    bridge
        .register_js_tool(
            "js/greet",
            r#"
        async function(args) {
            return { greeting: `Hello, ${args.name}!` };
        }
    "#,
        )
        .await
        .unwrap();

    // Verify it's listed
    let js_tools = bridge.list_js_tools();
    assert!(
        js_tools.contains(&"js/greet".to_string()),
        "Should list js/greet tool"
    );

    // Verify it's callable from JavaScript
    let _js_code = r#"
        (async () => {
            try {
                const result = await invokeTool("js/greet", { name: "World" });
                return JSON.stringify({
                    success: true,
                    greeting: result.greeting
                });
            } catch (e) {
                return JSON.stringify({
                    success: false,
                    error: e.toString()
                });
            }
        })()
    "#;

    // assert_tool_registered_in_js runs async IIFE that returns a promise; evaluate() requires scope to poll.
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000050").unwrap(),
    ));
    assert_tool_registered_in_js(&mut bridge, "js/greet", &scope).await;

    let check_code = r#"
        (() => JSON.stringify({
            isAsync: typeof invokeTool === 'function'
        }))()
    "#;

    let result = bridge.eval_sync(check_code).await.unwrap();
    let obj = result.as_object().unwrap();
    assert!(
        obj.get("isAsync")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "invokeTool should be available"
    );

    tracing::info!("✅ JavaScript tool registered successfully");
}

#[tokio::test]
async fn test_register_js_tool_with_complex_logic() {
    tracing::info!("Test: Register JavaScript tool with complex logic");

    let baml_manager = setup_baml_runtime_default();

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Register a more complex JavaScript tool
    bridge.register_js_tool("js/calculate", r#"
        async function(args) {
            try {
                // Simple calculator using eval (for testing only - would use safer parser in production)
                const result = Function('"use strict"; return (' + args.expression + ')')();
                return {
                    expression: args.expression,
                    result: result,
                    formatted: `${args.expression} = ${result}`
                };
            } catch (e) {
                return {
                    expression: args.expression,
                    error: e.message
                };
            }
        }
    "#).await.unwrap();

    // Verify it exists
    let js_tools = bridge.list_js_tools();
    assert!(
        js_tools.contains(&"js/calculate".to_string()),
        "Should list js/calculate tool"
    );

    // assert_tool_registered_in_js runs async IIFE that returns a promise; evaluate() requires scope to poll.
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000051").unwrap(),
    ));
    assert_tool_registered_in_js(&mut bridge, "js/calculate", &scope).await;

    tracing::info!("✅ Complex JavaScript tool registered successfully");
}

#[tokio::test]
async fn test_js_tool_not_available_in_rust() {
    tracing::info!("Test: JavaScript tools are not available in Rust");

    let baml_manager = setup_baml_runtime_default();

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Register a JavaScript tool
    bridge
        .register_js_tool(
            "js/only",
            r#"
        async function() {
            return { from: "javascript" };
        }
    "#,
        )
        .await
        .unwrap();

    // Verify it's NOT in the Rust tool registry
    let manager = baml_manager.lock().await;
    let rust_tools = manager.list_tools().await;
    assert!(
        !rust_tools.contains(&"js/only".to_string()),
        "JS tool should NOT be in Rust tool registry"
    );

    // Verify it IS a JS tool
    assert!(
        bridge.is_js_tool("js/only"),
        "Should identify js/only as a JavaScript tool"
    );

    tracing::info!("✅ JavaScript tools correctly isolated from Rust");
}

#[tokio::test]
#[allow(unnameable_test_items)]
async fn test_js_tool_name_conflict_with_rust_tool() {
    tracing::info!("Test: JavaScript tool name conflict detection");

    // Create a Rust tool
    struct TestRustTool;

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    struct ConflictInput {}

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    struct ConflictOutput {
        from: String,
    }

    #[async_trait]
    impl BamlTool for TestRustTool {
        type Bundle = Test;
        const LOCAL_NAME: &'static str = "conflict_tool";
        type OpenInput = ();
        type Input = ConflictInput;
        type Output = ConflictOutput;

        fn description(&self) -> &'static str {
            "A Rust tool"
        }

        async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
            Ok(ConflictOutput {
                from: "rust".to_string(),
            })
        }
    }

    let mut baml_manager = setup_baml_runtime_manager_default();

    // Register Rust tool first
    baml_manager.register_tool(TestRustTool).await.unwrap();

    let baml_manager = Arc::new(Mutex::new(baml_manager));
    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Try to register a JS tool with the same name - should fail
    let result = bridge
        .register_js_tool(
            "test/conflict_tool",
            r#"
        async function() {
            return { from: "javascript" };
        }
    "#,
        )
        .await;

    assert!(
        result.is_err(),
        "Should reject JS tool with conflicting name"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("conflicts with existing Rust tool"),
        "Error should mention conflict with Rust tool"
    );

    tracing::info!("✅ JavaScript tool name conflict correctly detected");
}

#[tokio::test]
async fn test_register_multiple_js_tools() {
    tracing::info!("Test: Register multiple JavaScript tools");

    let baml_manager = setup_baml_runtime_default();

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Register multiple JS tools
    bridge
        .register_js_tool("js/tool1", r#"async function() { return { id: 1 }; }"#)
        .await
        .unwrap();
    bridge
        .register_js_tool("js/tool2", r#"async function() { return { id: 2 }; }"#)
        .await
        .unwrap();
    bridge
        .register_js_tool("js/tool3", r#"async function() { return { id: 3 }; }"#)
        .await
        .unwrap();

    // Verify all are listed
    let js_tools = bridge.list_js_tools();
    assert_eq!(js_tools.len(), 3, "Should have 3 JS tools");
    assert!(js_tools.contains(&"js/tool1".to_string()));
    assert!(js_tools.contains(&"js/tool2".to_string()));
    assert!(js_tools.contains(&"js/tool3".to_string()));

    tracing::info!("✅ Multiple JavaScript tools registered successfully");
}

#[tokio::test]
async fn test_invalid_open_input_deserialization() {
    tracing::info!("Test: Invalid open_input deserialization preserves error source");

    let registry = baml_rt_tools::ToolRegistry::new();
    registry.register(AddNumbersTool).unwrap();

    // Try to open a session with invalid open_input (should be empty object for unit type)
    let invalid_open_input = serde_json::json!({"invalid": "data"});
    let context_id = ContextId::new(1, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let result = registry
        .open_session(
            "test/add_numbers",
            invalid_open_input,
            &context_id,
            &agent_id,
        )
        .await;

    assert!(result.is_err(), "Should fail with invalid open_input");
    let error = result.unwrap_err();

    // Verify it's the InvalidOpenInput variant with source preserved
    match error {
        baml_rt::BamlRtError::InvalidOpenInput { source } => {
            // Verify the source is a serde_json::Error by checking error message
            let error_msg = source.to_string();
            assert!(
                error_msg.contains("invalid") || error_msg.contains("expected"),
                "Source should be a serde_json::Error, got: {}",
                error_msg
            );
            tracing::info!(
                "✅ InvalidOpenInput error preserves serde_json::Error source: {}",
                error_msg
            );
        }
        _ => panic!("Expected InvalidOpenInput error, got: {:?}", error),
    }
}

#[tokio::test]
async fn test_open_session_with_initial_input() {
    tracing::info!("Test: open_session with initial_input parameter");

    use baml_rt_tools::ToolRegistry;
    use serde_json::json;

    let registry = ToolRegistry::new();
    registry.register(AddNumbersTool).unwrap();

    // Test opening a session with empty initial_input (for unit type OpenInput)
    let empty_input = json!({});
    let context_id = ContextId::new(1, 2);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let session_id = registry
        .open_session("test/add_numbers", empty_input, &context_id, &agent_id)
        .await
        .unwrap();

    // Verify session was created
    assert!(
        !session_id.as_str().is_empty(),
        "Session ID should not be empty"
    );

    // Test that we can send input and get a result
    let send_input = json!({"a": 10.0, "b": 20.0});
    registry
        .session_send(&session_id, send_input)
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match step {
        baml_rt_tools::ToolStep::Done { output } => {
            let result = output.unwrap();
            let result_obj = result.as_object().expect("Expected object");
            let result_value = result_obj.get("result").and_then(|v| v.as_f64()).unwrap();
            assert_eq!(result_value, 30.0, "Should return correct sum");
        }
        _ => panic!("Expected Done step"),
    }

    registry.session_finish(&session_id).await.unwrap();

    tracing::info!("✅ open_session with initial_input works correctly");
}

#[tokio::test]
async fn test_open_session_scoped_propagates_task_id_and_legacy_open_does_not() {
    let registry = baml_rt_tools::ToolRegistry::new();
    let captures = Arc::new(StdMutex::new(Vec::<Option<TaskId>>::new()));

    let tool_name = ToolName::parse("test/scope_capture").unwrap();
    let metadata = TypeBasedMetadataBuilder::<(), ScopeCaptureInput, ScopeCaptureOutput>::new(
        tool_name.clone(),
        ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local()),
        "Captures ToolSessionContext task_id".to_string(),
    )
    .with_origin(ToolOrigin::Host)
    .build_metadata();
    registry
        .register_dynamic(
            metadata.clone(),
            Arc::new(ScopeCaptureHandler {
                metadata,
                captures: captures.clone(),
            }),
        )
        .unwrap();

    let context_id = ContextId::new(10, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap());
    let task_id = TaskId::from_external(ExternalId::new("task-scope-propagation"));

    let scoped_session = registry
        .open_session_scoped(
            "test/scope_capture",
            json!({}),
            &context_id,
            &agent_id,
            Some(&task_id),
        )
        .await
        .unwrap();
    registry
        .session_send(&scoped_session, json!({}))
        .await
        .unwrap();
    let _ = registry
        .session_read(&scoped_session, serde_json::Value::Null)
        .await
        .unwrap();
    registry.session_finish(&scoped_session).await.unwrap();

    let legacy_session = registry
        .open_session("test/scope_capture", json!({}), &context_id, &agent_id)
        .await
        .unwrap();
    registry
        .session_send(&legacy_session, json!({}))
        .await
        .unwrap();
    let _ = registry
        .session_read(&legacy_session, serde_json::Value::Null)
        .await
        .unwrap();
    registry.session_finish(&legacy_session).await.unwrap();

    let seen = captures.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "expected two session opens to be captured");
    assert_eq!(seen[0].as_ref().map(TaskId::as_str), Some(task_id.as_str()));
    assert!(
        seen[1].is_none(),
        "legacy open_session should not set task_id"
    );
}

#[tokio::test]
async fn test_synthetic_session_eval_tool_supports_explicit_ref_progression() {
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager
            .register_tool(SyntheticSessionEvalTool)
            .await
            .unwrap();
    }
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000ab").unwrap(),
    ));
    let first = {
        let manager = baml_manager.lock().await;
        manager
            .execute_tool_with_scope(scope.as_scope(), "test/synthetic_session_eval", json!({}))
            .await
            .unwrap()
    };
    let refs = first
        .get("refs")
        .and_then(|v| v.as_array())
        .expect("refs array");
    assert_eq!(refs.len(), 3, "expected deterministic refs");
    let first_ref = refs
        .first()
        .and_then(|v| v.as_str())
        .expect("first ref")
        .to_string();

    let second = {
        let manager = baml_manager.lock().await;
        manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "test/synthetic_session_eval",
                json!({ "retrieve_ref": first_ref }),
            )
            .await
            .unwrap()
    };
    let items = second
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    assert_eq!(
        items.len(),
        1,
        "explicit retrieval should return one bounded item"
    );
}
