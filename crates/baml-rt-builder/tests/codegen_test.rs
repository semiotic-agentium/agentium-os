//! Unit tests for BAML and TypeScript code generation
//!
//! Uses insta snapshots to verify generated output, reducing round trips
//! during development and catching regressions.
#![recursion_limit = "256"]

use std::{collections::HashMap, path::PathBuf};

use baml_rt_builder::builder::{
    a2a_shim_gen::render_a2a_shim,
    baml_gen::render_baml_tool_interfaces,
    baml_signature_gen::{extract_baml_signatures, session_plan_functions_map},
    schema_to_baml::generate_baml_types_from_schemas,
    ts_gen::{load_manifest_tools, render_ts_declarations},
};
use baml_rt_core::{
    bus::EffectEvent,
    context::{self, InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_runtime::BamlRuntime;
use internal_baml_core::feature_flags::FeatureFlags;
use serde_json::Value;
use test_support::common::make_capturing_bridge;

#[cfg(feature = "clickup")]
#[test]
fn test_clickup_tool_baml_interfaces() {
    let tool_names = vec!["support/clickup".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate ClickUp BAML interfaces");

    insta::assert_snapshot!("clickup_baml_tool_interfaces", baml_output);
}

#[cfg(feature = "notion")]
#[test]
fn test_notion_tool_baml_interfaces() {
    let tool_names = vec!["support/notion".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate Notion BAML interfaces");

    assert!(
        baml_output.contains("type NotionInput"),
        "Expected NotionInput union in generated BAML"
    );
    assert!(
        baml_output.contains("class NotionOutput"),
        "Expected NotionOutput class in generated BAML"
    );
    assert!(
        baml_output.contains("class SupportNotionOpenStep"),
        "Expected SupportNotion session scaffolding in generated BAML"
    );
}

#[cfg(feature = "slack")]
#[test]
fn test_slack_tool_baml_interfaces() {
    let tool_names = vec!["support/slack".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate Slack BAML interfaces");

    assert!(
        baml_output.contains("type SlackInput"),
        "Expected SlackInput union in generated BAML"
    );
    assert!(
        baml_output.contains("class SlackOutput"),
        "Expected SlackOutput class in generated BAML"
    );
    assert!(
        baml_output.contains("class SupportSlackOpenStep"),
        "Expected SupportSlack session scaffolding in generated BAML"
    );
}

// ---------------------------------------------------------------------------
// ClickUp tool codegen — validates that schemars 1.x nullable types
// (Option<T> → "type": ["T", "null"]) and u8 integers are mapped correctly.
// ---------------------------------------------------------------------------

#[test]
fn test_schema_to_baml_nullable_types() {
    // Regression test: schemars 1.x encodes Option<T> as "type": ["T", "null"]
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    let schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "required_string": { "type": "string" },
            "nullable_string": { "type": ["string", "null"] },
            "nullable_int":    { "type": ["integer", "null"], "format": "uint8" },
            "nullable_array":  { "type": ["array", "null"], "items": { "type": "string" } },
            "plain_int":       { "type": "integer", "format": "int32" },
            "plain_u8":        { "type": "integer", "format": "uint8" }
        },
        "required": ["required_string", "plain_int", "plain_u8"]
    });

    schemas.insert("NullableTest".to_string(), schema);
    type_names.insert("NullableTest".to_string(), "NullableTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML for nullable types");

    insta::assert_snapshot!("baml_nullable_types", baml_output);
}

// Single-tool BAML interface snapshot. For multiple tools, add a test with a distinct tool list.
#[test]
fn test_baml_tool_interface_generation() {
    let tool_names = vec!["support/calculate".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate BAML tool interfaces");

    insta::assert_snapshot!("baml_tool_interfaces", baml_output);
}

fn fixture_baml_src(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("tests")
        .join("fixtures")
        .join("agents")
        .join(name)
        .join("baml_src")
}

// Full TS output (typed BAML functions + tool declarations). BAML→TS behavior is also
// asserted in baml_to_ts_test.rs (snapshot + typed/tool assertions).
#[tokio::test]
async fn test_typescript_declaration_generation() {
    let baml_src = fixture_baml_src("stream-baml-tool");
    if !baml_src.exists() {
        eprintln!(
            "Skipping: fixture stream-baml-tool not found at {}",
            baml_src.display()
        );
        return;
    }
    let env_vars: HashMap<String, String> = HashMap::new();
    let runtime = BamlRuntime::from_directory(&baml_src, env_vars, FeatureFlags::default())
        .expect("load BAML runtime from fixture");
    let ir_signature = extract_baml_signatures(&runtime).expect("extract IR signatures");
    let tool_names = load_manifest_tools(&baml_src).expect("load manifest tools");

    let session_plan_map = session_plan_functions_map(&ir_signature);
    let ts_output = render_ts_declarations(&ir_signature, &tool_names, &session_plan_map)
        .expect("Should generate TypeScript declarations");

    insta::assert_snapshot!("typescript_declarations", ts_output);
}

#[test]
fn test_schema_to_baml_enum_generation() {
    // Test enum generation from JSON schema
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create a simple enum schema
    let enum_schema: Value = serde_json::json!({
        "enum": ["Add", "Subtract", "Multiply", "Divide"],
        "type": "string"
    });

    schemas.insert("MathOperation".to_string(), enum_schema);
    type_names.insert("MathOperation".to_string(), "MathOperation".to_string());

    let baml_output =
        generate_baml_types_from_schemas(&schemas, &type_names).expect("Should generate BAML enum");

    insta::assert_snapshot!("baml_enum_from_schema", baml_output);
}

#[test]
fn test_schema_to_baml_class_generation() {
    // Test class generation from JSON schema
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create a simple class schema
    let class_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "left": {
                "type": "integer",
                "format": "int64"
            },
            "right": {
                "type": "integer",
                "format": "int64"
            },
            "optional_field": {
                "type": "string"
            }
        },
        "required": ["left", "right"]
    });

    schemas.insert("Expression".to_string(), class_schema);
    type_names.insert("Expression".to_string(), "Expression".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class");

    insta::assert_snapshot!("baml_class_from_schema", baml_output);
}

#[test]
fn test_schema_to_baml_nested_types() {
    // Test nested type generation with $ref
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Create nested schemas (simulating $defs structure)
    let expression_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "left": {
                "type": "integer",
                "format": "int64"
            },
            "operation": {
                "$ref": "#/$defs/MathOperation"
            },
            "right": {
                "type": "integer",
                "format": "int64"
            }
        },
        "required": ["left", "operation", "right"],
        "$defs": {
            "MathOperation": {
                "enum": ["Add", "Subtract", "Multiply", "Divide"],
                "type": "string"
            }
        }
    });

    let math_op_schema: Value = serde_json::json!({
        "enum": ["Add", "Subtract", "Multiply", "Divide"],
        "type": "string"
    });

    schemas.insert("Expression".to_string(), expression_schema);
    schemas.insert("MathOperation".to_string(), math_op_schema);
    type_names.insert("Expression".to_string(), "Expression".to_string());
    type_names.insert("MathOperation".to_string(), "MathOperation".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate nested BAML types");

    insta::assert_snapshot!("baml_nested_types_from_schema", baml_output);
}

#[test]
fn test_calculator_tool_metadata_schemas() {
    // Test with actual calculator tool metadata to verify real-world schemas
    use baml_rt_tools::tool_catalog::resolve_manifest_tools;

    let tool_names = vec!["support/calculate".to_string()];
    let tool_metadata = resolve_manifest_tools(&tool_names).expect("Should resolve tool metadata");

    assert_eq!(tool_metadata.len(), 1);
    let tool = &tool_metadata[0];

    // Create a summary string for snapshot
    let metadata_summary = format!(
        "name: {}\nclass_name: {}\ninput_type: {}\noutput_type: {}\nopen_input_type: {}\nhas_input_schema: {}\nhas_output_schema: {}\nhas_open_input_schema: {}",
        tool.name,
        tool.class_name,
        tool.input_type.name,
        tool.output_type.name,
        tool.open_input_type.name,
        tool.input_schema.is_object(),
        tool.output_schema.is_object(),
        tool.open_input_schema.is_object()
    );

    insta::assert_snapshot!("calculator_tool_metadata", metadata_summary);
}

#[test]
fn test_baml_generation_with_unit_open_input() {
    // Test that unit type () for open_input is handled correctly
    let tool_names = vec!["support/calculate".to_string()];
    let baml_output =
        render_baml_tool_interfaces(&tool_names).expect("Should generate BAML tool interfaces");

    // Extract just the OpenStep class to verify unit type handling
    let open_step_start = baml_output.find("class SupportCalculateOpenStep").unwrap();
    let open_step_end = baml_output[open_step_start..]
        .find("class SupportCalculateSendStep")
        .unwrap();
    let open_step_section = &baml_output[open_step_start..open_step_start + open_step_end];

    insta::assert_snapshot!("baml_open_step_unit_input", open_step_section);
}

#[test]
fn test_schema_to_baml_array_types() {
    // Test array type generation - arrays are used within classes, not as standalone types
    // So we test by creating a class with an array property
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    let class_with_array: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "string"
                }
            }
        },
        "required": ["items"]
    });

    schemas.insert("ArrayTest".to_string(), class_with_array);
    type_names.insert("ArrayTest".to_string(), "ArrayTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with array");

    insta::assert_snapshot!("baml_array_types", baml_output);
}

#[test]
fn test_schema_to_baml_description_escaping() {
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    let class_schema: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "plain": {
                "type": "string",
                "description": "A plain description"
            },
            "quoted": {
                "type": "string",
                "description": "Field with \"quoted\" value"
            },
            "backslash": {
                "type": "string",
                "description": "Path like C:\\Users\\test"
            },
            "mixed": {
                "type": "string",
                "description": "She said \"hello\\world\""
            }
        },
        "required": ["plain", "quoted", "backslash", "mixed"]
    });

    schemas.insert("EscapeTest".to_string(), class_schema);
    type_names.insert("EscapeTest".to_string(), "EscapeTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with escaped descriptions");

    insta::assert_snapshot!("baml_description_escaping", baml_output);
}

#[test]
fn test_schema_to_baml_primitive_types() {
    // Test that primitive types are correctly converted
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    // Test various primitive schemas (not used directly, but tests conversion logic)
    let _int_schema: Value = serde_json::json!({
        "type": "integer",
        "format": "int64"
    });

    let _float_schema: Value = serde_json::json!({
        "type": "number"
    });

    let _string_schema: Value = serde_json::json!({
        "type": "string"
    });

    let _bool_schema: Value = serde_json::json!({
        "type": "boolean"
    });

    // Note: Primitives aren't typically generated as standalone types,
    // but this tests the conversion logic in json_schema_to_baml_type
    // The actual test is that they're used correctly in class properties
    let class_with_primitives: Value = serde_json::json!({
        "type": "object",
        "properties": {
            "int_field": {
                "type": "integer",
                "format": "int64"
            },
            "float_field": {
                "type": "number"
            },
            "string_field": {
                "type": "string"
            },
            "bool_field": {
                "type": "boolean"
            }
        },
        "required": ["int_field", "float_field", "string_field", "bool_field"]
    });

    schemas.insert("PrimitiveTest".to_string(), class_with_primitives);
    type_names.insert("PrimitiveTest".to_string(), "PrimitiveTest".to_string());

    let baml_output = generate_baml_types_from_schemas(&schemas, &type_names)
        .expect("Should generate BAML class with primitives");

    insta::assert_snapshot!("baml_primitives_in_class", baml_output);
}

#[tokio::test]
async fn test_generated_a2a_shim_emits_planning_effects() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000bb10").unwrap());
    let (mut bridge, capture) = make_capturing_bridge(agent_id.clone()).await;

    let shim_js = render_a2a_shim().expect("render generated shim");
    bridge
        .eval_sync(&shim_js)
        .await
        .expect("evaluate generated a2a shim");

    let context_id = ContextId::new(7_001, 1);
    let message_id = MessageId::from_external(ExternalId::new("msg-shim-1"));
    let task_id = TaskId::from_external(ExternalId::new("task-shim-1"));
    let scope = RuntimeScope::task_scope(context_id, agent_id, message_id, task_id);
    let invoke_scope = InvocationScope::new(scope.clone());

    let js = r#"
      (async function() {
        const session = await openA2aExecutionSession("tok-generated-shim");
        await session.submitIntent({
          intentId: "intent-shim-1",
          description: "Generated shim planning intent",
          derivedFromMessageIds: ["msg-shim-1"]
        });
        await session.submitPlan({
          intentId: "intent-shim-1",
          planId: "plan-shim-1",
          steps: [
            { stepId: "step-shim-1", description: "Execute generated step", order: 0, dependsOn: [] }
          ]
        });
        await session.startStep("step-shim-1", "begin generated step execution");
        await session.completeStep("step-shim-1", "generated step finished successfully");
        return { ok: true };
      })()
    "#;

    let result = context::with_scope(scope, async {
        bridge
            .eval_scoped(&invoke_scope, js)
            .await
            .expect("run execution session through generated shim")
    })
    .await;
    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(true));

    let events = capture.events.lock().await.clone();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EffectEvent::IntentResolved { intent_id, .. } if intent_id.as_str() == "intent-shim-1")),
        "expected IntentResolved effect from generated shim; got {events:?}"
    );
    assert!(
        events.iter().any(|e| {
            matches!(
                e,
                EffectEvent::PlanGenerated {
                    intent_id,
                    plan_id,
                    ..
                } if intent_id.as_str() == "intent-shim-1" && plan_id.as_str() == "plan-shim-1"
            )
        }),
        "expected PlanGenerated effect from generated shim; got {events:?}"
    );
    assert!(
        events.iter().any(|e| {
            matches!(
                e,
                EffectEvent::PlanStepStatusChanged {
                    plan_id,
                    step_id,
                    new_status,
                    ..
                } if plan_id.as_str() == "plan-shim-1" && step_id.as_str() == "step-shim-1" && new_status == "completed"
            )
        }),
        "expected PlanStepStatusChanged(completed) from generated shim; got {events:?}"
    );
}

#[tokio::test]
async fn test_generated_a2a_shim_aborts_planning_on_run_failure() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000bb11").unwrap());
    let (mut bridge, capture) = make_capturing_bridge(agent_id.clone()).await;
    bridge
        .eval_sync("globalThis.__chat_yield = function(_chunk) {};")
        .await
        .expect("install dummy chat yield host");

    let shim_js = render_a2a_shim().expect("render generated shim");
    bridge
        .eval_sync(&shim_js)
        .await
        .expect("evaluate generated a2a shim");

    let context_id = ContextId::new(7_002, 1);
    let message_id = MessageId::from_external(ExternalId::new("msg-shim-fail-1"));
    let task_id = TaskId::from_external(ExternalId::new("task-shim-fail-1"));
    let scope = RuntimeScope::task_scope(context_id, agent_id, message_id, task_id);
    let invoke_scope = InvocationScope::new(scope.clone());

    let js = r#"
      (async function() {
        __chat_register({
          run: async function(ctx) {
            const session = await openA2aExecutionSession("tok-generated-fail");
            const intent = await session.submitIntent({
              intentId: "intent-shim-fail",
              description: "Generated shim failure path",
              derivedFromMessageIds: ["msg-shim-fail-1"]
            });
            const executable = await intent.submitPlan({
              intentId: "intent-shim-fail",
              planId: "plan-shim-fail",
              steps: [
                { stepId: "step-shim-fail-1", description: "Run then fail", order: 0, dependsOn: [] }
              ]
            });
            await executable.startStep("step-shim-fail-1", "step started before failure");
            throw new Error("forced failure for generated shim abort path");
          }
        });
        await onChatMessage({
          messageId: "msg-shim-fail-1",
          taskId: "task-shim-fail-1",
          parts: [{ text: "trigger fail" }]
        });
        return { ok: true };
      })()
    "#;

    let result = context::with_scope(scope, async {
        bridge
            .eval_scoped(&invoke_scope, js)
            .await
            .expect("run generated shim failure flow")
    })
    .await;
    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(true));

    let events = capture.events.lock().await.clone();
    assert!(
        events.iter().any(|e| {
            matches!(
                e,
                EffectEvent::PlanStepStatusChanged {
                    plan_id,
                    step_id,
                    new_status,
                    ..
                } if plan_id.as_str() == "plan-shim-fail" && step_id.as_str() == "step-shim-fail-1" && new_status == "aborted"
            )
        }),
        "expected PlanStepStatusChanged(aborted) from generated shim failure path; got {events:?}"
    );
}

#[test]
fn test_generated_a2a_shim_planner_loop_uses_session_context_only() {
    let shim_js = render_a2a_shim().expect("render generated shim");
    assert!(
        shim_js.contains("session_context"),
        "step-executor loop contract must include session_context"
    );
    assert!(
        !shim_js.contains("session_context_v1"),
        "step-executor loop contract must not include versioned session_context_v1 alias"
    );
    assert!(
        !shim_js.contains("last_tool_output"),
        "step-executor loop contract must not expose legacy last_tool_output"
    );
    assert!(
        !shim_js.contains("last_output_mode"),
        "step-executor loop contract must not expose legacy last_output_mode"
    );
    assert!(
        !shim_js.contains("hopMetrics"),
        "step-executor loop contract must not expose hopMetrics in step-executor args"
    );
}
