//! JSON Schema to BAML type generation
//!
//! Converts JSON Schema definitions into BAML type definitions (classes, enums, etc.)

use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

/// Generate BAML type definitions from JSON schemas
pub fn generate_baml_types_from_schemas(
    schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>, // Maps JSON schema ref/name to BAML type name
) -> Result<String> {
    let mut output = String::new();
    let mut generated = HashSet::new();

    // First pass: extract all nested types from $defs in all schemas
    let mut all_nested_schemas = HashMap::new();
    for schema in schemas.values() {
        extract_defs(schema, &mut all_nested_schemas);
    }

    // Merge nested schemas into main schemas map
    let mut all_schemas = schemas.clone();
    for (def_name, def_schema) in &all_nested_schemas {
        if !all_schemas.contains_key(def_name) {
            all_schemas.insert(def_name.clone(), def_schema.clone());
        }
    }

    // Generate types in dependency order (nested types first)
    // Collect all type names that need to be generated
    let mut types_to_generate: Vec<(String, String)> = Vec::new();
    for schema_name in all_schemas.keys() {
        if let Some(baml_name) = type_names.get(schema_name) {
            types_to_generate.push((baml_name.clone(), schema_name.clone()));
        } else if all_nested_schemas.contains_key(schema_name) {
            // Nested type not yet mapped - use schema name as BAML name
            types_to_generate.push((schema_name.clone(), schema_name.clone()));
        }
    }

    // Sort by BAML name for deterministic output
    types_to_generate.sort_by(|a, b| a.0.cmp(&b.0));

    // Generate types
    for (baml_name, schema_key) in types_to_generate {
        if !generated.contains(&baml_name)
            && let Some(schema) = all_schemas.get(&schema_key)
        {
            generate_baml_type(
                &mut output,
                &baml_name,
                schema,
                &mut generated,
                &all_schemas,
                type_names,
            )?;
        }
    }

    Ok(output)
}

/// Extract nested schemas from $defs or definitions
fn extract_defs(schema: &Value, defs: &mut HashMap<String, Value>) {
    if let Some(schema_obj) = schema.as_object() {
        // Check $defs (JSON Schema 2020-12)
        if let Some(defs_obj) = schema_obj.get("$defs").and_then(|v| v.as_object()) {
            for (def_name, def_schema) in defs_obj {
                defs.insert(def_name.clone(), def_schema.clone());
            }
        }

        // Check definitions (JSON Schema draft-07)
        if let Some(defs_obj) = schema_obj.get("definitions").and_then(|v| v.as_object()) {
            for (def_name, def_schema) in defs_obj {
                defs.insert(def_name.clone(), def_schema.clone());
            }
        }

        // Recursively check nested objects
        for value in schema_obj.values() {
            extract_defs(value, defs);
        }
    } else if let Some(schema_array) = schema.as_array() {
        for item in schema_array {
            extract_defs(item, defs);
        }
    }
}

/// Generate a single BAML type from JSON schema
fn generate_baml_type(
    output: &mut String,
    type_name: &str,
    schema: &Value,
    generated: &mut HashSet<String>,
    all_schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<()> {
    if generated.contains(type_name) {
        return Ok(());
    }
    generated.insert(type_name.to_string());

    let schema_obj = schema.as_object().ok_or_else(|| {
        BamlRtError::InvalidArgument(format!("Schema for {} must be an object", type_name))
    })?;

    // Check if it's an enum (oneOf with const values or enum field)
    if let Some(enum_values) = schema_obj.get("enum")
        && let Some(enum_array) = enum_values.as_array()
    {
        generate_baml_enum(output, type_name, enum_array, schema_obj)?;
        return Ok(());
    }

    // schemars 1.x represents enums with doc comments as
    // {"oneOf": [{"const": "Variant", "description": "..."}, ...]}
    // instead of a flat {"enum": ["Variant", ...]}.
    // Detect this pattern and generate a BAML enum with @description on each variant.
    if let Some(one_of) = schema_obj.get("oneOf").and_then(|v| v.as_array()) {
        let all_const = one_of
            .iter()
            .all(|v| v.as_object().is_some_and(|o| o.contains_key("const")));
        if all_const {
            generate_baml_enum_from_one_of(output, type_name, one_of)?;
            return Ok(());
        }
    }

    // Check if it's an object/class
    if let Some(Value::String(schema_type)) = schema_obj.get("type")
        && schema_type == "object"
    {
        generate_baml_class(
            output,
            type_name,
            schema_obj,
            generated,
            all_schemas,
            type_names,
        )?;
        return Ok(());
    }

    // Fallback: try to infer from properties
    if schema_obj.contains_key("properties") {
        generate_baml_class(
            output,
            type_name,
            schema_obj,
            generated,
            all_schemas,
            type_names,
        )?;
        return Ok(());
    }

    Err(BamlRtError::InvalidArgument(format!(
        "Cannot generate BAML type for {}: unsupported schema format",
        type_name
    )))
}

/// Generate BAML enum from schemars 1.x `oneOf` with `const` + optional `description`.
fn generate_baml_enum_from_one_of(
    output: &mut String,
    enum_name: &str,
    variants: &[Value],
) -> Result<()> {
    write_line(output, &format!("enum {} {{", enum_name))?;

    for variant in variants {
        let obj = match variant.as_object() {
            Some(o) => o,
            None => continue, // defensive: skip non-objects
        };

        let const_val = match obj.get("const").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };

        let variant_name = to_pascal_case(const_val);

        let desc = obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| {
                format!(
                    " @description(\"{}\")",
                    s.replace('"', "\\\"")
                )
            })
            .unwrap_or_default();

        write_line(output, &format!("  {}{}", variant_name, desc))?;

        if const_val != variant_name {
            write_line(output, &format!("    @alias(\"{}\")", const_val))?;
        }
    }

    write_line(output, "}")?;
    write_line(output, "")?;
    Ok(())
}

/// Generate BAML enum from JSON schema enum
fn generate_baml_enum(
    output: &mut String,
    enum_name: &str,
    enum_values: &[Value],
    _schema_obj: &serde_json::Map<String, Value>,
) -> Result<()> {
    write_line(output, &format!("enum {} {{", enum_name))?;

    for value in enum_values {
        if let Some(str_val) = value.as_str() {
            // Convert string to PascalCase variant name
            let variant_name = to_pascal_case(str_val);
            write_line(output, &format!("  {}", variant_name))?;

            // Add @alias if the string value differs from variant name
            if str_val != variant_name {
                write_line(output, &format!("    @alias(\"{}\")", str_val))?;
            }
        } else if let Some(num) = value.as_i64() {
            let variant_name = format!("Variant{}", num);
            write_line(output, &format!("  {}", variant_name))?;
            write_line(output, &format!("    @alias(\"{}\")", num))?;
        }
    }

    write_line(output, "}")?;
    write_line(output, "")?;
    Ok(())
}

/// Generate BAML class from JSON schema object
fn generate_baml_class(
    output: &mut String,
    class_name: &str,
    schema_obj: &serde_json::Map<String, Value>,
    generated: &mut HashSet<String>,
    all_schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<()> {
    write_line(output, &format!("class {} {{", class_name))?;

    let properties = schema_obj
        .get("properties")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("Class {} must have properties", class_name))
        })?;

    let required = schema_obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    for (prop_name, prop_schema) in properties {
        let prop_type = json_schema_to_baml_type(prop_schema, generated, all_schemas, type_names)?;
        let is_optional = !required.contains(prop_name.as_str());
        let type_str = if is_optional {
            format!("{}?", prop_type)
        } else {
            prop_type
        };

        // Get description if available
        let description = prop_schema
            .as_object()
            .and_then(|obj| obj.get("description"))
            .and_then(|v| v.as_str())
            .map(|s| format!(" @description(\"{}\")", s.replace('"', "\\\"")))
            .unwrap_or_default();

        write_line(
            output,
            &format!("  {} {} {}", prop_name, type_str, description),
        )?;
    }

    write_line(output, "}")?;
    write_line(output, "")?;
    Ok(())
}

/// Convert JSON schema type to BAML type string.
///
/// Handles both scalar `"type": "string"` and nullable array
/// `"type": ["string", "null"]` forms produced by schemars 1.x for `Option<T>`.
fn json_schema_to_baml_type(
    schema: &Value,
    _generated: &mut HashSet<String>,
    _all_schemas: &HashMap<String, Value>,
    _type_names: &HashMap<String, String>,
) -> Result<String> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| BamlRtError::InvalidArgument("Schema must be an object".to_string()))?;

    // Handle $ref - extract nested types from definitions
    if let Some(Value::String(ref_path)) = schema_obj.get("$ref") {
        // Extract type name from #/$defs/TypeName or #/definitions/TypeName
        if let Some(type_name) = ref_path.split('/').next_back() {
            return Ok(type_name.to_string());
        }
    }

    // Handle oneOf (union types)
    if let Some(one_of) = schema_obj.get("oneOf").and_then(|v| v.as_array()) {
        let mut types = Vec::new();
        for variant in one_of {
            types.push(json_schema_to_baml_type(
                variant,
                _generated,
                _all_schemas,
                _type_names,
            )?);
        }
        return Ok(types.join(" | "));
    }

    // Resolve the effective scalar type string.
    //
    // schemars 1.x represents `Option<T>` as `"type": ["T", "null"]`.
    // We extract the single non-null element so downstream matching
    // works identically for both nullable and non-nullable schemas.
    let effective_type: Option<&str> = match schema_obj.get("type") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Array(arr)) => {
            // Filter out "null" and expect exactly one remaining type.
            let non_null: Vec<&str> = arr
                .iter()
                .filter_map(Value::as_str)
                .filter(|t| *t != "null")
                .collect();
            match non_null.len() {
                1 => Some(non_null[0]),
                // No non-null type (pure null) or ambiguous union — fall through.
                _ => None,
            }
        }
        _ => None,
    };

    if let Some(type_str) = effective_type {
        // Handle array
        if type_str == "array" {
            if let Some(items) = schema_obj.get("items") {
                let item_type =
                    json_schema_to_baml_type(items, _generated, _all_schemas, _type_names)?;
                return Ok(format!("{}[]", item_type));
            }
            return Ok("any[]".to_string());
        }

        // Handle scalar primitive types
        return Ok(match type_str {
            "string" => "string".to_string(),
            // JSON Schema "integer" is always integral; "number" is always floating-point.
            // Previous code only mapped format=="int64" to int, causing u8/i32/etc. to
            // become float. The JSON Schema spec guarantees "integer" excludes fractions.
            "integer" => "int".to_string(),
            "number" => "float".to_string(),
            "boolean" => "bool".to_string(),
            "null" => "null".to_string(),
            "object" => "object".to_string(),
            _ => format!("any /* {} */", type_str),
        });
    }

    // Handle enum
    if schema_obj.contains_key("enum") {
        // This should have been handled by generate_baml_enum
        return Ok("string".to_string()); // Fallback
    }

    Ok("any".to_string())
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for ch in s.chars() {
        if ch == '_' || ch == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_uppercase().next().unwrap_or(ch));
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }

    result
}

fn write_line(output: &mut String, line: &str) -> Result<()> {
    writeln!(output, "{}", line)
        .map_err(|e| BamlRtError::InvalidArgument(format!("Format error: {}", e)))
}
