//! Template fragments merged into [`super::GENERATED_BAML_PRELUDE_FILE`] (prelude + per-tool cards).

use std::collections::{HashMap, HashSet};

use baml_rt_tools::{
    OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY, tool_catalog::resolve_manifest_tools,
    tools::ToolFunctionMetadata,
};
use baml_tools_calculator as _;
use serde_json::Value;

use super::{
    escape::escape_baml_description, prelude::generated_tools_prelude, prompt_copy,
    writer::BamlWriter,
};
use crate::builder::{error::Result, schema_to_baml};

/// Generate BAML tool interface file with FSM-aware prompting hints.
pub fn render_baml_tool_interfaces(tool_names: &[String]) -> Result<String> {
    // Force link so inventory sees these metadata registrations (regen_fixtures + builder).
    let _ = baml_tools_calculator::support_calculate_metadata;
    let _ = baml_rt_tools_claude::metadata::claude_dev_metadata;
    let _ = baml_tools_system::metadata::system_internal_a2a_metadata;
    #[cfg(feature = "clickup")]
    let _ = baml_tools_clickup::ClickUpTool::new;
    #[cfg(feature = "notion")]
    let _ = baml_tools_notion::NotionTool::new;
    #[cfg(feature = "slack")]
    let _ = baml_tools_slack::SlackTool::new;

    let tool_metadata = resolve_manifest_tools(tool_names)?;
    let mut w = BamlWriter::new();
    w.push_block(&generated_tools_prelude());
    let out = w.as_mut_string();

    // --- Domain type generation ---
    let mut emitted_decl_types: HashSet<String> = HashSet::new();
    let mut macro_decls = String::new();

    for tool in &tool_metadata {
        if let Some(decl) = &tool.baml_decl {
            emitted_decl_types.insert(tool.input_type.name.clone());
            emitted_decl_types.insert(tool.output_type.name.clone());
            if tool.open_input_type.name != "()" {
                emitted_decl_types.insert(tool.open_input_type.name.clone());
            }

            if !macro_decls.is_empty() {
                macro_decls.push('\n');
            }
            macro_decls.push_str(decl);
            macro_decls.push('\n');
        }
    }

    if !macro_decls.is_empty() {
        out.push_str("// Domain types generated from #[derive(BamlType)]\n");
        out.push_str(&macro_decls);
        if !macro_decls.ends_with('\n') {
            out.push('\n');
        }
    }

    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    for tool in &tool_metadata {
        if tool.baml_decl.is_some() {
            continue;
        }

        extract_nested_schemas(&tool.input_schema, &mut type_names);
        extract_nested_schemas(&tool.output_schema, &mut type_names);
        if tool.open_input_type.name != "()" {
            extract_nested_schemas(&tool.open_input_schema, &mut type_names);
        }

        if !emitted_decl_types.contains(&tool.input_type.name) {
            schemas.insert(tool.input_type.name.clone(), tool.input_schema.clone());
            type_names.insert(tool.input_type.name.clone(), tool.input_type.name.clone());
        }

        if !emitted_decl_types.contains(&tool.output_type.name) {
            schemas.insert(tool.output_type.name.clone(), tool.output_schema.clone());
            type_names.insert(tool.output_type.name.clone(), tool.output_type.name.clone());
        }

        if tool.open_input_type.name != "()"
            && !emitted_decl_types.contains(&tool.open_input_type.name)
        {
            schemas.insert(
                tool.open_input_type.name.clone(),
                tool.open_input_schema.clone(),
            );
            type_names.insert(
                tool.open_input_type.name.clone(),
                tool.open_input_type.name.clone(),
            );
        }
    }

    if !schemas.is_empty() {
        let domain_types = schema_to_baml::generate_baml_types_from_schemas(&schemas, &type_names)?;
        if !domain_types.is_empty() {
            out.push_str("// Domain types generated from JSON schemas\n");
            out.push_str(&domain_types);
            if !domain_types.ends_with('\n') {
                out.push('\n');
            }
        }
    }

    for tool in &tool_metadata {
        generate_tool_card_baml(out, tool)?;
        out.push('\n');
        generate_tool_baml_interface(out, tool)?;
        out.push('\n');
    }

    Ok(w.into_string())
}

fn generate_tool_card_baml(output: &mut String, tool: &ToolFunctionMetadata) -> Result<()> {
    use crate::builder::error::write_line;

    let class_name = &tool.class_name;
    let card_name = format!("{class_name}ToolCard");
    let tool_name = tool.name.to_string();
    let description = escape_baml_description(&tool.description);
    let policy = format!("{:?}", tool.session_policy);
    let input_summary = summarize_input_schema(&tool.input_schema);

    write_line(
        output,
        &format!("/// Tool card for {tool_name}: metadata for polymorphic Open selection."),
    )?;
    write_line(output, &format!("class {card_name} {{"))?;
    write_line(output, &format!("  tool_name \"{tool_name}\""))?;
    write_line(output, &format!("  description \"{description}\""))?;
    write_line(output, &format!("  session_policy \"{policy}\""))?;
    write_line(
        output,
        &format!(
            "  input_summary \"{}\"",
            escape_baml_description(&input_summary)
        ),
    )?;
    if !tool.tags.is_empty() {
        let tags_desc = tool.tags.join(", ");
        write_line(
            output,
            &format!("  tags string[] @description(\"Tool tags: {tags_desc}\")"),
        )?;
    }
    write_line(output, "}")?;
    Ok(())
}

fn summarize_input_schema(schema: &Value) -> String {
    let mut visited_refs = HashSet::new();
    summarize_schema_shape(schema, schema, &mut visited_refs)
}

fn summarize_schema_shape(
    schema: &Value,
    root_schema: &Value,
    visited_refs: &mut HashSet<String>,
) -> String {
    let Some(obj) = schema.as_object() else {
        return "any".to_string();
    };
    if let Some(ref_path) = obj.get("$ref").and_then(Value::as_str) {
        return summarize_ref_schema(ref_path, root_schema, visited_refs);
    }
    if obj
        .get(OPAQUE_JSON_SCHEMA_MARKER_KEY)
        .and_then(Value::as_str)
        .is_some_and(|marker| marker == OPAQUE_JSON_BAML_TYPE)
    {
        return OPAQUE_JSON_BAML_TYPE.to_string();
    }
    if let Some(const_value) = obj.get("const") {
        return summarize_const_value(const_value);
    }
    if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
        return summarize_union_schema("oneOf", one_of, root_schema, visited_refs);
    }
    if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
        return summarize_union_schema("anyOf", any_of, root_schema, visited_refs);
    };
    if let Some(enum_values) = obj.get("enum").and_then(Value::as_array) {
        return summarize_enum_values(enum_values);
    }
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        return summarize_object_schema(props, obj, root_schema, visited_refs);
    }
    if let Some(items) = obj.get("items") {
        return format!(
            "{}[]",
            summarize_schema_shape(items, root_schema, visited_refs)
        );
    }
    if let Some(additional_properties) = obj.get("additionalProperties") {
        return format!(
            "map<string, {}>",
            summarize_schema_shape(additional_properties, root_schema, visited_refs)
        );
    }
    if let Some(type_array) = obj.get("type").and_then(Value::as_array) {
        let mut parts = type_array
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        parts.sort();
        parts.dedup();
        return if parts.is_empty() {
            "any".to_string()
        } else {
            parts.join(" | ")
        };
    }
    obj.get("type")
        .and_then(Value::as_str)
        .unwrap_or("any")
        .to_string()
}

fn summarize_object_schema(
    props: &serde_json::Map<String, Value>,
    schema_obj: &serde_json::Map<String, Value>,
    root_schema: &Value,
    visited_refs: &mut HashSet<String>,
) -> String {
    let required: HashSet<&str> = schema_obj
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    let mut sorted_keys: Vec<&String> = props.keys().collect();
    sorted_keys.sort();
    for key in sorted_keys {
        let prop = &props[key];
        let ty = summarize_schema_shape(prop, root_schema, visited_refs);
        let optional = if required.contains(key.as_str()) {
            ""
        } else {
            "?"
        };
        parts.push(format!("{key}{optional}: {ty}"));
    }
    format!("{{ {} }}", parts.join(", "))
}

fn summarize_union_schema(
    kind: &str,
    variants: &[Value],
    root_schema: &Value,
    visited_refs: &mut HashSet<String>,
) -> String {
    let parts = variants
        .iter()
        .map(|variant| summarize_schema_shape(variant, root_schema, visited_refs))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "any".to_string()
    } else {
        format!("{kind}({})", parts.join(" | "))
    }
}

fn summarize_ref_schema(
    ref_path: &str,
    root_schema: &Value,
    visited_refs: &mut HashSet<String>,
) -> String {
    if !ref_path.starts_with('#') {
        return summarize_ref_name(ref_path);
    }
    if !visited_refs.insert(ref_path.to_string()) {
        return summarize_ref_name(ref_path);
    }
    let summary = root_schema
        .pointer(ref_path.trim_start_matches('#'))
        .map(|resolved| summarize_schema_shape(resolved, root_schema, visited_refs))
        .unwrap_or_else(|| summarize_ref_name(ref_path));
    visited_refs.remove(ref_path);
    summary
}

fn summarize_ref_name(ref_path: &str) -> String {
    ref_path
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or("$ref")
        .to_string()
}

fn summarize_const_value(value: &Value) -> String {
    match value {
        Value::String(value) => format!("\"{value}\""),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
        _ => "const".to_string(),
    }
}

fn summarize_enum_values(values: &[Value]) -> String {
    let mut rendered = values.iter().map(summarize_const_value).collect::<Vec<_>>();
    rendered.sort();
    rendered.dedup();
    if rendered.is_empty() {
        "enum".to_string()
    } else {
        format!("enum({})", rendered.join(" | "))
    }
}

fn schema_allows_empty_or_null_open_input(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => {
            if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
                && any_of.iter().any(schema_allows_empty_or_null_open_input)
            {
                return true;
            }
            if let Some(one_of) = map.get("oneOf").and_then(Value::as_array)
                && one_of.iter().any(schema_allows_empty_or_null_open_input)
            {
                return true;
            }
            if map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "null")
            {
                return true;
            }

            let type_allows_object = match map.get("type") {
                Some(Value::String(t)) => t == "object",
                Some(Value::Array(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| t == "object" || t == "null"),
                Some(_) => false,
                None => map.contains_key("properties") || map.contains_key("required"),
            };
            if !type_allows_object {
                return false;
            }

            let has_required = map
                .get("required")
                .and_then(Value::as_array)
                .map(|arr| !arr.is_empty())
                .unwrap_or(false);
            if has_required {
                return false;
            }

            let min_properties = map
                .get("minProperties")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            min_properties == 0
        }
        _ => false,
    }
}

fn generate_tool_baml_interface(output: &mut String, tool: &ToolFunctionMetadata) -> Result<()> {
    use crate::builder::error::write_line;

    let class_name = &tool.class_name;
    let open_input_type_name = &tool.open_input_type.name;
    let input_type_name = &tool.input_type.name;

    let open_step_name = format!("{}OpenStep", class_name);
    let send_step_name = format!("{}SendStep", class_name);
    let read_step_name = format!("{}ReadStep", class_name);
    let finish_step_name = format!("{}FinishStep", class_name);
    let abort_step_name = format!("{}AbortStep", class_name);
    let step_union_name = format!("{}SessionStep", class_name);
    let plan_type_name = format!("{}SessionPlan", class_name);
    let access_note = tool
        .access
        .map(|access| format!(" Access: {}.", access))
        .unwrap_or_default();

    let tool_name = tool.name.to_string();
    let is_claude_or_a2a = tool_name.starts_with("claude/")
        || tool_name.contains("/a2a")
        || tool_name.contains("_a2a");

    let send_input_desc = if is_claude_or_a2a {
        "Conversational message payload. Set text to a non-empty string. Never null, never omit text."
    } else {
        "Payload for this step."
    };
    let step_desc = if is_claude_or_a2a {
        prompt_copy::STEP_DESC_CLAUDE_OR_A2A
    } else {
        prompt_copy::STEP_DESC_DEFAULT
    };

    write_line(output, &format!("class {} {{", open_step_name))?;
    write_line(output, "  op \"Open\"")?;
    write_line(
        output,
        &format!("  tool_name \"{tool_name}\" @description(\"Tool to open\")"),
    )?;
    let open_schema_has_properties = tool
        .open_input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if open_input_type_name != "()"
        && open_input_type_name != "null"
        && open_input_type_name != "void"
        && open_schema_has_properties
    {
        let open_is_optional = schema_allows_empty_or_null_open_input(&tool.open_input_schema);
        let optional_suffix = if open_is_optional { "?" } else { "" };
        let initial_input_desc = if open_is_optional {
            "Optional open payload."
        } else {
            "Required open payload."
        };
        write_line(
            output,
            &format!(
                "  initial_input {}{} @description(\"{}\")",
                open_input_type_name, optional_suffix, initial_input_desc
            ),
        )?;
    }
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", send_step_name))?;
    write_line(output, "  op \"Send\"")?;
    write_line(
        output,
        &format!(
            "  input {} @description(\"{}\")",
            input_type_name, send_input_desc
        ),
    )?;
    write_line(
        output,
        &format!(
            "  citations string[] @description(\"{}\")",
            escape_baml_description(prompt_copy::CITATIONS_SEND_STEP)
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", read_step_name))?;
    write_line(output, "  op \"Read\"")?;
    write_line(
        output,
        &format!(
            "  input ArchiveReadInput @description(\"{}\")",
            escape_baml_description(prompt_copy::READ_STEP_INPUT_DESCRIPTION)
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", finish_step_name))?;
    write_line(output, "  op \"Finish\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", abort_step_name))?;
    write_line(output, "  op \"Abort\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(
        output,
        &format!(
            "type {} = {} | {} | {} | {} | {}",
            step_union_name,
            open_step_name,
            send_step_name,
            read_step_name,
            finish_step_name,
            abort_step_name
        ),
    )?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", plan_type_name))?;
    write_line(
        output,
        &format!(
            "  step {} @description(\"{}{}\")",
            step_union_name, step_desc, access_note
        ),
    )?;
    write_line(
        output,
        &format!(
            "  citations string[] @description(\"{}\")",
            escape_baml_description(prompt_copy::CITATIONS_DECISION_OR_SYNTHESIS)
        ),
    )?;
    write_line(output, "}")?;

    Ok(())
}

pub(crate) fn extract_nested_schemas(schema: &Value, type_names: &mut HashMap<String, String>) {
    if let Some(schema_obj) = schema.as_object() {
        if let Some(defs) = schema_obj.get("$defs").and_then(|v| v.as_object()) {
            for def_name in defs.keys() {
                type_names.insert(def_name.clone(), def_name.clone());
            }
        }

        if let Some(defs) = schema_obj.get("definitions").and_then(|v| v.as_object()) {
            for def_name in defs.keys() {
                type_names.insert(def_name.clone(), def_name.clone());
            }
        }

        for value in schema_obj.values() {
            extract_nested_schemas(value, type_names);
        }
    } else if let Some(schema_array) = schema.as_array() {
        for item in schema_array {
            extract_nested_schemas(item, type_names);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::summarize_input_schema;

    #[test]
    fn summarize_input_schema_resolves_local_refs() {
        let schema = json!({
            "type": "object",
            "properties": {
                "payload": { "$ref": "#/$defs/Payload" }
            },
            "required": ["payload"],
            "$defs": {
                "Payload": {
                    "type": "object",
                    "properties": {
                        "count": { "type": "integer" },
                        "kind": { "type": "string" }
                    },
                    "required": ["count"]
                }
            }
        });

        assert_eq!(
            summarize_input_schema(&schema),
            "{ payload: { count: integer, kind?: string } }"
        );
    }
}
