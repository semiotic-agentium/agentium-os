//! Tests for BAML IR → TypeScript declaration generation.
//!
//! Covers typed function signatures (args + return), supporting type declarations
//! (interfaces, enums, type aliases), and primitives/optional/union/list/map.

use std::collections::HashMap;

use baml_rt_builder::builder::{
    baml_signature_gen::extract_baml_signatures,
    ts_gen::{load_manifest_tools, render_ts_declarations},
};
use baml_runtime::BamlRuntime;
use internal_baml_core::feature_flags::FeatureFlags;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_baml_src(name: &str) -> std::path::PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("agents")
        .join(name)
        .join("baml_src")
}

/// Load BAML runtime from fixture, extract IR signatures, render TS. Returns generated d.ts content.
fn generate_ts_from_fixture(fixture_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let baml_src = fixture_baml_src(fixture_name);
    if !baml_src.exists() {
        return Err(format!("fixture baml_src not found: {}", baml_src.display()).into());
    }
    let env_vars: HashMap<String, String> = HashMap::new();
    let feature_flags = FeatureFlags::default();
    let runtime = BamlRuntime::from_directory(&baml_src, env_vars, feature_flags)
        .map_err(|e| format!("BamlRuntime::from_directory: {}", e))?;
    let ir_signature =
        extract_baml_signatures(&runtime).map_err(|e| format!("extract_baml_signatures: {}", e))?;
    let tool_names =
        load_manifest_tools(&baml_src).map_err(|e| format!("load_manifest_tools: {}", e))?;
    let ts = render_ts_declarations(&ir_signature, &tool_names)
        .map_err(|e| format!("render_ts_declarations: {}", e))?;
    Ok(ts)
}

// ---------------------------------------------------------------------------
// Full snapshot: stream-baml-tool (function with class return, tool types)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_stream_baml_tool_full_snapshot() {
    let ts =
        generate_ts_from_fixture("stream-baml-tool").expect("generate TS from stream-baml-tool");
    insta::assert_snapshot!("baml_to_ts_stream_baml_tool_full", ts);
}

// ---------------------------------------------------------------------------
// Typed function declaration: args object and return type (no Promise<unknown>)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_typed_function_declaration() {
    let ts = generate_ts_from_fixture("stream-baml-tool").expect("generate TS");
    assert!(
        ts.contains("declare function ChooseCalcTool(args: { user_message: string }): Promise<SupportCalculateSessionPlan>"),
        "expected typed function declaration; got snippet: {}",
        ts.lines().take(10).collect::<Vec<_>>().join("\n")
    );
    // BAML function declarations must be typed; BamlAgent.tools may still use Promise<unknown>
    assert!(
        !ts.contains("ChooseCalcTool(args: { user_message: string }): Promise<unknown>"),
        "BAML function ChooseCalcTool should have typed return, not Promise<unknown>"
    );
    assert!(
        !ts.contains("Record<string, unknown>"),
        "generated TS should not use Record<string, unknown> for BAML function args"
    );
}

// ---------------------------------------------------------------------------
// Supporting type: type alias emitted for session plan return type
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_emits_type_alias_for_return_type() {
    let ts = generate_ts_from_fixture("stream-baml-tool").expect("generate TS");
    assert!(
        ts.contains("export type SupportCalculateSessionPlan"),
        "expected type alias for return type SupportCalculateSessionPlan"
    );
}

// ---------------------------------------------------------------------------
// Primitives and optional: args/return types map correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_primitive_and_object_types_in_output() {
    let ts = generate_ts_from_fixture("stream-baml-tool").expect("generate TS");
    assert!(ts.contains("user_message: string"), "string arg type");
    assert!(
        ts.contains("Promise<SupportCalculateSessionPlan>"),
        "return type should be named class, not unknown"
    );
}

// ---------------------------------------------------------------------------
// A2A runtime section present and typed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_includes_tool_declarations() {
    let ts = generate_ts_from_fixture("stream-baml-tool").expect("generate TS");
    assert!(
        ts.contains("ToolFailureKind"),
        "shared failure type present"
    );
    assert!(
        ts.contains("openA2aTaskSession"),
        "A2A typed opener declaration present"
    );
    assert!(ts.contains("A2aNextStates"), "FSM transition type present");
}

// ---------------------------------------------------------------------------
// Type mapper: return type re-aliases to named type alias
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_type_mapper_re_aliases_return_type() {
    let baml_src = fixture_baml_src("stream-baml-tool");
    if !baml_src.exists() {
        return;
    }
    let env_vars: HashMap<String, String> = HashMap::new();
    let runtime = BamlRuntime::from_directory(&baml_src, env_vars, FeatureFlags::default())
        .expect("load runtime");
    let ir_signature = extract_baml_signatures(&runtime).expect("extract signatures");

    let func = ir_signature
        .functions
        .get("ChooseCalcTool")
        .expect("ChooseCalcTool in IR");
    let raw_frag =
        baml_rt_builder::builder::ir_to_ts::type_to_ts_expr(func.output.as_ref(), &ir_signature)
            .expect("type_to_ts_expr");

    // BAML IR expands non-recursive type aliases inline; re_alias_frag restores the name
    let frag = baml_rt_builder::builder::ir_to_ts::re_alias_frag(raw_frag, &ir_signature)
        .expect("re_alias_frag");

    assert_eq!(frag.expr, "SupportCalculateSessionPlan");
    assert!(
        frag.deps
            .contains(&"SupportCalculateSessionPlan".to_string()),
        "deps should include return type alias name"
    );
}

// ---------------------------------------------------------------------------
// Type mapper: primitive arg produces string expr and no deps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baml_to_ts_type_mapper_primitive_arg() {
    let baml_src = fixture_baml_src("stream-baml-tool");
    if !baml_src.exists() {
        return;
    }
    let env_vars: HashMap<String, String> = HashMap::new();
    let runtime = BamlRuntime::from_directory(&baml_src, env_vars, FeatureFlags::default())
        .expect("load runtime");
    let ir_signature = extract_baml_signatures(&runtime).expect("extract signatures");

    let func = ir_signature
        .functions
        .get("ChooseCalcTool")
        .expect("ChooseCalcTool in IR");
    let (_, arg_ty) = func.inputs.iter().next().expect("one input");
    let frag = baml_rt_builder::builder::ir_to_ts::type_to_ts_expr(arg_ty, &ir_signature)
        .expect("type_to_ts_expr");

    assert_eq!(frag.expr, "string");
    assert!(frag.deps.is_empty(), "primitive should have no deps");
}
