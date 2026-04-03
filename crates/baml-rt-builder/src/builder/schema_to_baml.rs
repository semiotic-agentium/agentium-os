//! JSON Schema to BAML type generation
//!
//! Converts JSON Schema definitions into BAML type definitions (classes, enums, etc.)

use std::collections::{HashMap, HashSet};

use baml_rt_tools::{OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY};
use serde_json::Value;

use crate::builder::error::{BamlBuilderError, Result, write_line};

fn escape_baml_string(value: &str) -> String {
    value.chars().flat_map(|c| c.escape_default()).collect()
}

fn custom_baml_type(schema_obj: &serde_json::Map<String, Value>) -> Option<&str> {
    schema_obj
        .get(OPAQUE_JSON_SCHEMA_MARKER_KEY)
        .and_then(Value::as_str)
        .filter(|value| *value == OPAQUE_JSON_BAML_TYPE)
}

fn schema_snippet(schema: &Value) -> String {
    let compact =
        serde_json::to_string(schema).unwrap_or_else(|_| "<unserializable schema>".to_string());
    const MAX_LEN: usize = 200;
    if compact.len() <= MAX_LEN {
        compact
    } else {
        format!("{prefix}...", prefix = &compact[..MAX_LEN - 3])
    }
}

fn unsupported_json_schema(detail: impl AsRef<str>, schema: &Value) -> BamlBuilderError {
    let detail = detail.as_ref();
    let schema = schema_snippet(schema);
    BamlBuilderError::InvalidArgument(format!(
        "unsupported JSON Schema for generated BAML: {detail}; schema={schema}. Use baml_rt_tools::OpaqueJson for opaque JSON payloads."
    ))
}

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
        BamlBuilderError::InvalidArgument(format!("Schema for {} must be an object", type_name))
    })?;

    if let Some(custom_type) = custom_baml_type(schema_obj) {
        write_line(output, &format!("type {type_name} = {custom_type}"))?;
        write_line(output, "")?;
        return Ok(());
    }

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

        // General oneOf union (non-enum): generate variant classes for object members
        // and emit a type alias union.
        let mut variants = Vec::new();
        let alt_field_names: Vec<String> = (0..one_of.len()).map(|i| format!("Alt{}", i)).collect();
        for (idx, variant) in one_of.iter().enumerate() {
            let is_object_like = variant.as_object().is_some_and(|obj| {
                obj.contains_key("properties")
                    || obj
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t == "object")
            });
            if is_object_like {
                let variant_name = format!("{type_name}Variant{}", idx + 1);
                generate_baml_type(
                    output,
                    &variant_name,
                    variant,
                    generated,
                    all_schemas,
                    type_names,
                )?;
                variants.push(variant_name);
            } else {
                variants.push(json_schema_to_baml_type(
                    output,
                    variant,
                    generated,
                    all_schemas,
                    type_names,
                    Some((type_name, alt_field_names[idx].as_str())),
                )?);
            }
        }
        write_line(
            output,
            &format!("type {type_name} = {}", variants.join(" | ")),
        )?;
        write_line(output, "")?;
        return Ok(());
    }

    if let Some(any_of) = schema_obj.get("anyOf").and_then(|v| v.as_array()) {
        let mut variants = Vec::new();
        let alt_field_names: Vec<String> = (0..any_of.len()).map(|i| format!("Alt{}", i)).collect();
        for (idx, variant) in any_of.iter().enumerate() {
            let is_object_like = variant.as_object().is_some_and(|obj| {
                obj.contains_key("properties")
                    || obj
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t == "object")
            });
            if is_object_like {
                let variant_name = format!("{type_name}Variant{}", idx + 1);
                generate_baml_type(
                    output,
                    &variant_name,
                    variant,
                    generated,
                    all_schemas,
                    type_names,
                )?;
                variants.push(variant_name);
            } else {
                variants.push(json_schema_to_baml_type(
                    output,
                    variant,
                    generated,
                    all_schemas,
                    type_names,
                    Some((type_name, alt_field_names[idx].as_str())),
                )?);
            }
        }
        write_line(
            output,
            &format!("type {type_name} = {}", variants.join(" | ")),
        )?;
        write_line(output, "")?;
        return Ok(());
    }

    // Check if it's an object/class
    if let Some(Value::String(schema_type)) = schema_obj.get("type")
        && schema_type == "object"
    {
        if is_map_schema(schema_obj) {
            let baml_type = json_schema_to_baml_type(
                output,
                schema,
                generated,
                all_schemas,
                type_names,
                Some((type_name, "Value")),
            )?;
            write_line(output, &format!("type {type_name} = {baml_type}"))?;
            write_line(output, "")?;
            return Ok(());
        }
        if !is_inline_object_schema(schema_obj) {
            return Err(unsupported_json_schema(
                format!(
                    "Cannot generate BAML type for {type_name}: object schema without properties or additionalProperties"
                ),
                schema,
            ));
        }
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
        if !is_inline_object_schema(schema_obj) {
            return Err(unsupported_json_schema(
                format!(
                    "Cannot generate BAML type for {type_name}: object schema declares empty properties"
                ),
                schema,
            ));
        }
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

    Err(BamlBuilderError::InvalidArgument(format!(
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
            .map(|s| format!(" @description(\"{}\")", escape_baml_string(s)))
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
    let properties = schema_obj.get("properties").and_then(|v| v.as_object());
    // Skip generating classes with no properties — BAML doesn't allow empty classes
    // and the initial_input field is also skipped for these in baml_gen.rs.
    let properties = match properties {
        Some(p) if !p.is_empty() => p,
        _ => return Ok(()),
    };

    for (prop_name, prop_schema) in properties.iter() {
        preemit_nested_inline_classes(
            output,
            prop_schema,
            Some((class_name, prop_name.as_str())),
            generated,
            all_schemas,
            type_names,
        )?;
    }

    write_line(output, &format!("class {} {{", class_name))?;

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
        let prop_type = json_schema_to_baml_type(
            output,
            prop_schema,
            generated,
            all_schemas,
            type_names,
            Some((class_name, prop_name.as_str())),
        )?;
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
            .map(|s| format!("@description(\"{}\")", escape_baml_string(s)))
            .unwrap_or_default();
        if description.is_empty() {
            write_line(output, &format!("  {} {}", prop_name, type_str))?;
        } else {
            write_line(
                output,
                &format!("  {} {} {}", prop_name, type_str, description),
            )?;
        }
    }

    write_line(output, "}")?;
    write_line(output, "")?;
    Ok(())
}

fn is_inline_object_schema(schema_obj: &serde_json::Map<String, Value>) -> bool {
    let Some(props) = schema_obj.get("properties").and_then(Value::as_object) else {
        return false;
    };
    !props.is_empty()
}

fn is_map_schema(schema_obj: &serde_json::Map<String, Value>) -> bool {
    schema_obj
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|schema_type| schema_type == "object")
        && schema_obj.get("additionalProperties").is_some()
        && !is_inline_object_schema(schema_obj)
}

/// Stable BAML class name for an inline `type: object` schema (nested struct fields).
fn nested_class_name_for_inline_object(
    schema_obj: &serde_json::Map<String, Value>,
    inline_name_hint: Option<(&str, &str)>,
) -> Result<String> {
    if let Some(Value::String(title)) = schema_obj.get("title")
        && !title.is_empty()
    {
        return Ok(title.clone());
    }
    let Some((parent, field)) = inline_name_hint else {
        return Err(BamlBuilderError::InvalidArgument(
            "inline object schema has no title and no parent/field hint for BAML class name"
                .to_string(),
        ));
    };
    Ok(format!("{}{}", parent, to_pascal_case(field)))
}

/// Emit nested inline object classes referenced by `schema` before the caller writes its own class.
fn preemit_nested_inline_classes(
    output: &mut String,
    schema: &Value,
    inline_name_hint: Option<(&str, &str)>,
    generated: &mut HashSet<String>,
    all_schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<()> {
    if schema.is_boolean() {
        return Ok(());
    }
    let Some(schema_obj) = schema.as_object() else {
        return Ok(());
    };
    if schema_obj.contains_key("$ref") {
        return Ok(());
    }

    if is_map_schema(schema_obj) {
        if let Some(value_schema) = schema_obj.get("additionalProperties") {
            preemit_nested_inline_classes(
                output,
                value_schema,
                inline_name_hint,
                generated,
                all_schemas,
                type_names,
            )?;
        }
        return Ok(());
    }

    if is_inline_object_schema(schema_obj) {
        let nested_name = nested_class_name_for_inline_object(schema_obj, inline_name_hint)?;
        if !generated.contains(&nested_name) {
            generate_baml_type(
                output,
                &nested_name,
                schema,
                generated,
                all_schemas,
                type_names,
            )?;
        }
        return Ok(());
    }

    if let Some(Value::Array(type_array)) = schema_obj.get("type") {
        for value in type_array {
            match value {
                Value::String(s) if s == "array" => {
                    if let Some(items) = schema_obj.get("items") {
                        preemit_nested_inline_classes(
                            output,
                            items,
                            inline_name_hint,
                            generated,
                            all_schemas,
                            type_names,
                        )?;
                    }
                }
                Value::Object(_) => {
                    preemit_nested_inline_classes(
                        output,
                        value,
                        inline_name_hint,
                        generated,
                        all_schemas,
                        type_names,
                    )?;
                }
                _ => {}
            }
        }
        return Ok(());
    }

    if schema_obj
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == "array")
        && let Some(items) = schema_obj.get("items")
    {
        preemit_nested_inline_classes(
            output,
            items,
            inline_name_hint,
            generated,
            all_schemas,
            type_names,
        )?;
        return Ok(());
    }

    if let Some(any_of) = schema_obj.get("anyOf").and_then(Value::as_array) {
        for variant in any_of {
            preemit_nested_inline_classes(
                output,
                variant,
                inline_name_hint,
                generated,
                all_schemas,
                type_names,
            )?;
        }
    }
    if let Some(one_of) = schema_obj.get("oneOf").and_then(Value::as_array) {
        for variant in one_of {
            preemit_nested_inline_classes(
                output,
                variant,
                inline_name_hint,
                generated,
                all_schemas,
                type_names,
            )?;
        }
    }

    Ok(())
}

/// Convert JSON schema type to BAML type string.
///
/// Handles both scalar `"type": "string"` and nullable array
/// `"type": ["string", "null"]` forms produced by schemars 1.x for `Option<T>`.
///
/// For inline object schemas with `properties`, emits a nested BAML `class` first
/// (named from `"title"` or `parent_field` hint).
fn json_schema_to_baml_type(
    output: &mut String,
    schema: &Value,
    generated: &mut HashSet<String>,
    all_schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
    inline_name_hint: Option<(&str, &str)>,
) -> Result<String> {
    if schema.is_boolean() {
        return Err(unsupported_json_schema(
            "boolean schemas cannot be represented in generated BAML",
            schema,
        ));
    }
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| BamlBuilderError::InvalidArgument("Schema must be an object".to_string()))?;

    if let Some(custom_type) = custom_baml_type(schema_obj) {
        return Ok(custom_type.to_string());
    }

    // Handle $ref - extract nested types from definitions
    if let Some(Value::String(ref_path)) = schema_obj.get("$ref") {
        // Extract type name from #/$defs/TypeName or #/definitions/TypeName
        if let Some(type_name) = ref_path.split('/').next_back() {
            return Ok(type_name.to_string());
        }
    }

    // Inline object with properties (BamlType nested structs embed full object schema).
    if is_inline_object_schema(schema_obj) {
        let nested_name = nested_class_name_for_inline_object(schema_obj, inline_name_hint)?;
        if !generated.contains(&nested_name) {
            generate_baml_type(
                output,
                &nested_name,
                schema,
                generated,
                all_schemas,
                type_names,
            )?;
        }
        return Ok(nested_name);
    }

    if is_map_schema(schema_obj) {
        let value_type = if let Some(value_schema) = schema_obj.get("additionalProperties") {
            json_schema_to_baml_type(
                output,
                value_schema,
                generated,
                all_schemas,
                type_names,
                inline_name_hint,
            )?
        } else {
            "string".to_string()
        };
        return Ok(format!("map<string, {value_type}>"));
    }

    // Handle nullable types represented as type: ["string", "null"]
    if let Some(Value::Array(type_array)) = schema_obj.get("type") {
        let mut mapped = Vec::new();
        for value in type_array {
            let mapped_type = match value {
                Value::String(type_str) => {
                    if type_str == "null" {
                        continue;
                    }
                    match type_str.as_str() {
                        "string" => "string".to_string(),
                        "integer" => "int".to_string(),
                        "number" => "float".to_string(),
                        "boolean" => "bool".to_string(),
                        "object" => {
                            if is_inline_object_schema(schema_obj) {
                                let nested_name = nested_class_name_for_inline_object(
                                    schema_obj,
                                    inline_name_hint,
                                )?;
                                if !generated.contains(&nested_name) {
                                    generate_baml_type(
                                        output,
                                        &nested_name,
                                        schema,
                                        generated,
                                        all_schemas,
                                        type_names,
                                    )?;
                                }
                                nested_name
                            } else if is_map_schema(schema_obj) {
                                if let Some(value_schema) = schema_obj.get("additionalProperties") {
                                    let value_type = json_schema_to_baml_type(
                                        output,
                                        value_schema,
                                        generated,
                                        all_schemas,
                                        type_names,
                                        inline_name_hint,
                                    )?;
                                    format!("map<string, {value_type}>")
                                } else {
                                    "map<string, string>".to_string()
                                }
                            } else {
                                return Err(unsupported_json_schema(
                                    "object unions without properties or additionalProperties cannot be represented in generated BAML",
                                    schema,
                                ));
                            }
                        }
                        "array" => {
                            if let Some(items) = schema_obj.get("items") {
                                let item_type = json_schema_to_baml_type(
                                    output,
                                    items,
                                    generated,
                                    all_schemas,
                                    type_names,
                                    inline_name_hint,
                                )?;
                                format!("{}[]", item_type)
                            } else {
                                return Err(unsupported_json_schema(
                                    "array schema is missing `items`",
                                    schema,
                                ));
                            }
                        }
                        other => {
                            return Err(unsupported_json_schema(
                                format!("unknown JSON Schema type `{other}`"),
                                schema,
                            ));
                        }
                    }
                }
                Value::Object(_) => json_schema_to_baml_type(
                    output,
                    value,
                    generated,
                    all_schemas,
                    type_names,
                    inline_name_hint,
                )?,
                _ => {
                    return Err(unsupported_json_schema(
                        "non-string entry inside `type` array",
                        value,
                    ));
                }
            };
            mapped.push(mapped_type);
        }
        if mapped.is_empty() {
            return Ok("null".to_string());
        }
        if mapped.len() == 1 {
            return Ok(mapped.remove(0));
        }
        return Ok(mapped.join(" | "));
    }

    // Handle anyOf/oneOf (union types)
    if let Some(any_of) = schema_obj.get("anyOf").and_then(|v| v.as_array()) {
        let mut types = Vec::new();
        for variant in any_of {
            types.push(json_schema_to_baml_type(
                output,
                variant,
                generated,
                all_schemas,
                type_names,
                inline_name_hint,
            )?);
        }
        return Ok(types.join(" | "));
    }

    // Handle oneOf (union types)
    if let Some(one_of) = schema_obj.get("oneOf").and_then(|v| v.as_array()) {
        // #[baml(vec_or_one)] wire schema: oneOf [ item, { type: array, items: item } ] with the same
        // `item` Value twice — expand once to avoid duplicate inline classes (e.g. T | T | … | T[]).
        if one_of.len() == 2
            && let Some(second) = one_of[1].as_object()
            && second.get("type").and_then(|t| t.as_str()) == Some("array")
            && let Some(items) = second.get("items")
            && items == &one_of[0]
        {
            let t = json_schema_to_baml_type(
                output,
                items,
                generated,
                all_schemas,
                type_names,
                inline_name_hint,
            )?;
            return Ok(format!("({t} | {t}[])"));
        }
        let mut types = Vec::new();
        for variant in one_of {
            types.push(json_schema_to_baml_type(
                output,
                variant,
                generated,
                all_schemas,
                type_names,
                inline_name_hint,
            )?);
        }
        // Multiple JSON Schema arms may map to the same generated BAML name (e.g. inline oneOf
        // object variants sharing a nested class title); keep a stable deduped union.
        let mut seen = HashSet::new();
        let mut deduped: Vec<String> = Vec::new();
        for t in types {
            if seen.insert(t.clone()) {
                deduped.push(t);
            }
        }
        return Ok(deduped.join(" | "));
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
                let item_type = json_schema_to_baml_type(
                    output,
                    items,
                    generated,
                    all_schemas,
                    type_names,
                    inline_name_hint,
                )?;
                return Ok(format!("{}[]", item_type));
            }
            return Err(unsupported_json_schema(
                "array schema is missing `items`",
                schema,
            ));
        }

        // Handle scalar primitive types
        if type_str == "object" {
            if is_inline_object_schema(schema_obj) {
                let nested_name =
                    nested_class_name_for_inline_object(schema_obj, inline_name_hint)?;
                if !generated.contains(&nested_name) {
                    generate_baml_type(
                        output,
                        &nested_name,
                        schema,
                        generated,
                        all_schemas,
                        type_names,
                    )?;
                }
                return Ok(nested_name);
            }
            if is_map_schema(schema_obj)
                && let Some(value_schema) = schema_obj.get("additionalProperties")
            {
                let value_type = json_schema_to_baml_type(
                    output,
                    value_schema,
                    generated,
                    all_schemas,
                    type_names,
                    inline_name_hint,
                )?;
                return Ok(format!("map<string, {value_type}>"));
            }
            return Err(unsupported_json_schema(
                "object schema without properties or additionalProperties cannot be represented in generated BAML",
                schema,
            ));
        }

        return Ok(match type_str {
            "string" => "string".to_string(),
            // JSON Schema "integer" is always integral; "number" is always floating-point.
            // Previous code only mapped format=="int64" to int, causing u8/i32/etc. to
            // become float. The JSON Schema spec guarantees "integer" excludes fractions.
            "integer" => "int".to_string(),
            "number" => "float".to_string(),
            "boolean" => "bool".to_string(),
            "null" => "null".to_string(),
            other => {
                return Err(unsupported_json_schema(
                    format!("unknown JSON Schema type `{other}`"),
                    schema,
                ));
            }
        });
    }

    // Handle enum
    if schema_obj.contains_key("enum") {
        return Err(unsupported_json_schema(
            "enum schema could not be normalized into a generated BAML enum",
            schema,
        ));
    }

    Err(unsupported_json_schema(
        "schema is missing a supported type discriminator",
        schema,
    ))
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
