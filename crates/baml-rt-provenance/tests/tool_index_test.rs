//! Tool index tests using SurrealDB in-memory store.

use baml_rt_provenance::{SurrealStoreBuilder, index_tools};
use baml_rt_tools::{SecretRequest, ToolFunctionMetadataExport, ToolName, ToolTypeSpec};
use serde_json::{Value, json};

#[tokio::test]
async fn tool_index_creates_tool_nodes() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store");

    let name = ToolName::parse("support/get_weather").expect("valid tool name");
    let tools = vec![ToolFunctionMetadataExport {
        name: name.clone(),
        class_name: "SupportGetWeather".to_string(),
        description: "Fetch a weather report by location".to_string(),
        open_input_schema: json!({ "type": "object" }),
        input_schema: json!({ "type": "object", "properties": { "location": { "type": "string" } } }),
        output_schema: json!({ "type": "object", "properties": { "temperature": { "type": "number" } } }),
        open_input_type: ToolTypeSpec {
            name: "()".to_string(),
            ts_decl: None,
        },
        input_type: ToolTypeSpec {
            name: "WeatherInput".to_string(),
            ts_decl: None,
        },
        output_type: ToolTypeSpec {
            name: "WeatherOutput".to_string(),
            ts_decl: None,
        },
        baml_decl: None,
        extra_ts_decls: Vec::new(),
        tags: vec!["weather".to_string(), "forecast".to_string()],
        secret_requests: vec![SecretRequest::api_key(
            "WEATHER_KEY",
            "Required to call the weather provider",
            "Weather API key",
        )],
        config: None,
        access: None,
        origin: baml_rt_tools::ToolOrigin::Host,
        projection_semantics: None,
        event_sources: Vec::new(),
    }];

    index_tools(&store, &tools)
        .await
        .expect("index tools");

    let mut result = store
        .db()
        .query("SELECT node_id OMIT id FROM prov_node WHERE label = 'ToolFunction' LIMIT 5")
        .await
        .expect("query all");
    let rows: Vec<Value> = result.take(0).unwrap_or_default();
    assert!(
        !rows.is_empty(),
        "expected at least one ToolFunction node after index_tools"
    );

    let mut by_id = store
        .db()
        .query("SELECT * OMIT id FROM prov_node WHERE label = 'ToolFunction' AND node_id = $id LIMIT 1")
        .bind(("id", "support/get_weather"))
        .await
        .expect("query by id");
    let id_rows: Vec<Value> = by_id.take(0).unwrap_or_default();
    assert_eq!(
        id_rows.len(),
        1,
        "expected one ToolFunction node for support/get_weather"
    );
}
