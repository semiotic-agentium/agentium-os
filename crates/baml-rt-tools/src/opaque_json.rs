use std::collections::BTreeMap;

use baml_derive_core::{
    BamlClassDef, BamlDefinition, BamlFieldDef, BamlType, JsonSchemaType, TsType,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const OPAQUE_JSON_BAML_TYPE: &str = "OpaqueJson";
pub const OPAQUE_JSON_SCHEMA_MARKER_KEY: &str = "x-baml-type";
pub const OPAQUE_JSON_WRAPPER_FIELD: &str = "__baml_opaque_json";

/// Opaque host-managed JSON payload.
///
/// Runtime deserialization accepts either:
/// - any raw JSON value directly
/// - a wrapper object of the form `{ "__baml_opaque_json": "<serialized json>" }`
///   for generated BAML interfaces that need an explicit transport shape
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpaqueJson(Value);

impl OpaqueJson {
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    pub fn into_inner(self) -> Value {
        self.0
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for OpaqueJson {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl From<OpaqueJson> for Value {
    fn from(value: OpaqueJson) -> Self {
        value.0
    }
}

impl Serialize for OpaqueJson {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OpaqueJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_opaque_json_value(value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl BamlType for OpaqueJson {
    fn baml_type_name() -> &'static str {
        OPAQUE_JSON_BAML_TYPE
    }

    fn baml_definition() -> BamlDefinition {
        BamlDefinition::Class(BamlClassDef {
            name: OPAQUE_JSON_BAML_TYPE,
            doc: Some(
                "Opaque JSON transport wrapper. Generated BAML callers pass serialized JSON in `__baml_opaque_json`, while the runtime still accepts arbitrary raw JSON from direct host-side callers.",
            ),
            fields: vec![BamlFieldDef {
                name: "opaque_json",
                baml_type: "string".to_string(),
                alias: Some(OPAQUE_JSON_WRAPPER_FIELD),
                description: Some("Serialized JSON payload."),
                skip: false,
            }],
            dynamic: false,
        })
    }
}

impl TsType for OpaqueJson {
    fn ts_type_name() -> &'static str {
        OPAQUE_JSON_BAML_TYPE
    }

    fn ts_decl() -> Option<String> {
        Some("export type OpaqueJson = JsonValue;".to_string())
    }
}

impl JsonSchemaType for OpaqueJson {
    fn json_schema_inline() -> Value {
        serde_json::json!({
            OPAQUE_JSON_SCHEMA_MARKER_KEY: OPAQUE_JSON_BAML_TYPE,
            "description": "Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper."
        })
    }
}

pub fn opaque_json_map_from_object(value: Value) -> BTreeMap<String, OpaqueJson> {
    value
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(key, value)| (key.clone(), OpaqueJson::from(value.clone())))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_opaque_json_value(value: Value) -> Result<Value, String> {
    let Value::Object(mut map) = value else {
        return Ok(value);
    };

    if map.len() != 1 {
        return Ok(Value::Object(map));
    }

    if let Some(raw_json) = map.remove(OPAQUE_JSON_WRAPPER_FIELD) {
        return parse_raw_json_wrapper(raw_json, OPAQUE_JSON_WRAPPER_FIELD);
    }

    Ok(Value::Object(map))
}

fn parse_raw_json_wrapper(raw_json: Value, field_name: &str) -> Result<Value, String> {
    let Value::String(raw_json) = raw_json else {
        return Err(format!(
            "OpaqueJson wrapper field `{field_name}` must contain a JSON string"
        ));
    };
    serde_json::from_str(&raw_json).map_err(|error| {
        format!(
            "OpaqueJson wrapper field `{field_name}` must contain valid serialized JSON: {error}"
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::OpaqueJson;

    #[test]
    fn opaque_json_accepts_raw_json_values() {
        let parsed: OpaqueJson = serde_json::from_value(json!({
            "kind": "callback",
            "count": 2
        }))
        .expect("raw json should deserialize");

        assert_eq!(
            parsed.into_inner(),
            json!({
                "kind": "callback",
                "count": 2
            })
        );
    }

    #[test]
    fn opaque_json_accepts_wrapper_shape() {
        let parsed: OpaqueJson = serde_json::from_value(json!({
            "__baml_opaque_json": "{\"kind\":\"callback\",\"count\":2}"
        }))
        .expect("wrapper json should deserialize");

        assert_eq!(
            parsed.into_inner(),
            json!({
                "kind": "callback",
                "count": 2
            })
        );
    }
}
