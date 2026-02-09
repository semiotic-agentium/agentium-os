use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::tool_catalog::resolve_manifest_tools;
use baml_rt_tools::ts_gen::render_tool_typescript;
use genco::lang::js;
use genco::prelude::*;
use std::fs;
use std::path::Path;

pub fn load_manifest_tools(baml_src: &Path) -> Result<Vec<String>> {
    let agent_dir = baml_src.parent().ok_or_else(|| {
        BamlRtError::InvalidArgument("baml_src has no parent directory".to_string())
    })?;
    let manifest_path = agent_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
    let manifest_json: serde_json::Value =
        serde_json::from_str(&content).map_err(BamlRtError::Json)?;
    let tools = manifest_json
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(tools)
}

pub fn render_ts_declarations(function_names: &[String], tool_names: &[String]) -> Result<String> {
    let mut tokens: js::Tokens = quote!(
        // TypeScript declarations for BAML runtime host functions
        // This file is auto-generated - do not edit manually
        // Generated from BAML runtime
    );
    tokens.line();

    for function_name in function_names {
        quote_in!(tokens =>
            // $(function_name) BAML function
            declare function $(function_name)(args?: Record<string, unknown>): Promise<unknown>;
        );
        tokens.line();
    }

    let tool_metadata = resolve_manifest_tools(tool_names)?;
    let tool_ts = render_tool_typescript(&tool_metadata)?;
    for line in tool_ts.lines() {
        quote_in!(tokens => $(line));
        tokens.push();
    }

    tokens
        .to_file_string()
        .map_err(|e| BamlRtError::InvalidArgument(format!("TypeScript render error: {}", e)))
}
