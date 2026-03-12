//! Tool index tests using GraphQLite (temp path per test).

use baml_rt_provenance::{ToolIndexConfig, index_tools_into_connection};
use baml_rt_tools::{SecretRequest, ToolFunctionMetadataExport, ToolName, ToolTypeSpec};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn tool_index_creates_tool_nodes() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance.db");
    let config = ToolIndexConfig::new(&path);

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
    }];

    let conn = index_tools_into_connection(&config, &tools)
        .await
        .expect("index tools");
    let all: graphqlite::CypherResult = conn
        .cypher("MATCH (t:ToolFunction) RETURN t.id AS tool_id LIMIT 5")
        .expect("query all");
    let rows: Vec<_> = all.iter().collect();
    assert!(
        !rows.is_empty(),
        "expected at least one ToolFunction node after index_tools"
    );
    let result = conn
        .cypher_builder("MATCH (t:ToolFunction) WHERE t.id = $id RETURN t LIMIT 1")
        .params(&serde_json::json!({ "id": "support/get_weather" }))
        .run()
        .expect("query by id");
    assert_eq!(
        result.iter().count(),
        1,
        "expected one ToolFunction node for support/get_weather; first node id: {:?}",
        rows[0].get::<String>("tool_id")
    );
}
