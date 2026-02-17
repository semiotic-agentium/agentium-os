#![cfg(feature = "falkordb-tests")]

use baml_rt_provenance::{ToolIndexConfig, index_tools};
use baml_rt_tools::{ToolFunctionMetadataExport, ToolName, ToolSecretRequirement, ToolTypeSpec};
use serde_json::json;
use test_support::common::shared_falkordb;
use text_to_cypher::core::execute_cypher_query;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn tool_index_creates_nodes_and_fulltext() {
    let connection = shared_falkordb().await;
    let graph = "baml_tool_index_test";

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
        session_plan_group: None,
        secret_requirements: vec![ToolSecretRequirement {
            name: "WEATHER_KEY".to_string(),
            description: "Weather API key".to_string(),
            reason: "call provider".to_string(),
        }],
        access: None,
        origin: baml_rt_tools::ToolOrigin::Host,
    }];

    let config = ToolIndexConfig::new(connection, graph);
    index_tools(&config, &tools).await.expect("index tools");

    let node_count = execute_cypher_query(
        "MATCH (t:ToolFunction {name: \"support/get_weather\"}) RETURN COUNT(t)",
        graph,
        connection,
        true,
    )
    .await
    .expect("query tool count");
    assert_eq!(node_count.trim(), "1");

    let mut attempts = 0;
    let search_count = loop {
        let search_count = execute_cypher_query(
            "CALL db.idx.fulltext.queryNodes('ToolFunction', 'weather') YIELD node RETURN COUNT(node)",
            graph,
            connection,
            true,
        )
        .await
        .expect("query tool index");
        if search_count.trim() != "0" || attempts >= 10 {
            break search_count;
        }
        attempts += 1;
        sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(
        search_count.trim(),
        "0",
        "expected fulltext search to find tool node, got: {search_count}"
    );
}
