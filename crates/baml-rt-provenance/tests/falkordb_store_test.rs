use baml_rt_core::ids::{
    AgentId, ArtifactId, ContextId, EventId, ExternalId, MessageId, TaskId, UuidId,
};
use baml_rt_provenance::{
    AgentType, CallScope, FalkorDbProvenanceConfig, FalkorDbProvenanceWriter, GlobalEvent,
    LlmUsage, ProvEvent, ProvEventData, ProvenanceWriter, TaskScopedEvent,
};
use insta::assert_json_snapshot;
use serde_json::{Value, json};
use testcontainers::GenericImage;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use text_to_cypher::core::execute_cypher_query;
use tokio::time::{Duration, sleep};

async fn start_falkordb() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let image = GenericImage::new("falkordb/falkordb", "latest")
        .with_exposed_port(ContainerPort::Tcp(6379));

    let container = image.start().await.expect("start falkordb container");
    let mut attempts = 0;
    let host_port = loop {
        match container.get_host_port_ipv4(6379).await {
            Ok(port) => break port,
            Err(err) => {
                attempts += 1;
                if attempts > 25 {
                    panic!("get falkordb port: {err}");
                }
                sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let connection = format!("falkor://127.0.0.1:{host_port}");
    (container, connection)
}

async fn wait_for_falkordb(connection: &str, graph: &str) {
    sleep(Duration::from_secs(1)).await;
    let mut attempts = 0;
    loop {
        match execute_cypher_query("RETURN 1", graph, connection, false).await {
            Ok(_) => return,
            Err(err) => {
                let error_message = err.to_string();
                attempts += 1;
                if attempts > 120 {
                    panic!("falkordb did not become ready; last error: {error_message}");
                }
            }
        }
        sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn falkordb_writer_persists_task_and_artifact() {
    let (_container, connection) = start_falkordb().await;
    let graph = "baml_prov_test";
    wait_for_falkordb(&connection, graph).await;

    let writer =
        FalkorDbProvenanceWriter::new(FalkorDbProvenanceConfig::new(connection.clone(), graph));
    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    let agent_booted = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(0),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_000_000,
        data: ProvEventData::AgentBooted {
            agent_id: agent_id.clone(),
            agent_type: AgentType::new("tony").expect("agent_type"),
            agent_version: "1.0.0".to_string(),
            archive_path: "tony@1.0.0".to_string(),
        },
    });

    let task_created = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(1),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_000_000,
        data: ProvEventData::TaskCreated {
            task_id: task_id.clone(),
            agent_id: agent_id.clone(),
        },
    });
    let task_artifact_generated = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(2),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_000_100,
        data: ProvEventData::TaskArtifactGenerated {
            task_id: task_id.clone(),
            artifact_id: Some(ArtifactId::from_external(ExternalId::new("artifact-1"))),
            artifact_type: Some("result".to_string()),
        },
    });
    writer
        .add_event(agent_booted)
        .await
        .expect("write agent_booted");
    writer
        .add_event(task_created)
        .await
        .expect("write task_created");
    writer
        .add_event(task_artifact_generated)
        .await
        .expect("write task_artifact_generated");

    let task_count = execute_cypher_query(
        "MATCH (t:A2ATask {name: \"task:task-1\"}) RETURN COUNT(t)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query task count");
    assert_eq!(task_count.trim(), "1");

    let edge_count = execute_cypher_query(
        "MATCH (:A2ATask {name: \"task:task-1\"})-[:WAS_GENERATED_BY]->(:Artifact) RETURN COUNT(*)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query artifact edge count");
    assert_eq!(edge_count.trim(), "1");

    let graph_snapshot = execute_cypher_query(
        "MATCH (n)-[r]->(m) \
         RETURN labels(n), n.name, properties(n), \
                type(r), properties(r), \
                labels(m), m.name, properties(m) \
         ORDER BY n.name, type(r), m.name",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query graph snapshot");
    assert_json_snapshot!(
        "falkordb_graph_snapshot",
        graph_snapshot_json(&graph_snapshot)
    );
}

#[tokio::test]
async fn falkordb_writer_persists_large_document() {
    let (_container, connection) = start_falkordb().await;
    let graph = "baml_prov_large_test";
    wait_for_falkordb(&connection, graph).await;

    let writer =
        FalkorDbProvenanceWriter::new(FalkorDbProvenanceConfig::new(connection.clone(), graph));
    let context_id = ContextId::new(2, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-42"));
    let agent_id = "00000000-0000-0000-0000-000000000010";
    let agent_uuid = AgentId::from_uuid(UuidId::parse_str(agent_id).unwrap());

    let agent_booted = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(2),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_000_900,
        data: ProvEventData::AgentBooted {
            agent_id: agent_uuid.clone(),
            agent_type: AgentType::new("tony").expect("agent_type"),
            agent_version: "1.0.0".to_string(),
            archive_path: "tony@1.0.0".to_string(),
        },
    });

    let message_received = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(3),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_000,
        data: ProvEventData::MessageReceived {
            id: MessageId::from_external(ExternalId::new("msg-1")),
            role: "user".to_string(),
            content: vec!["Hi Tony".to_string(), "It's the ducks.".to_string()],
            metadata: Some(std::collections::HashMap::from([
                ("channel".to_string(), "stdio".to_string()),
                ("agent_id".to_string(), agent_id.to_string()),
            ])),
        },
    });
    let message_sent = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(4),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_500,
        data: ProvEventData::MessageSent {
            id: MessageId::from_external(ExternalId::new("msg-2")),
            role: "assistant".to_string(),
            content: vec!["Tell me about those ducks.".to_string()],
            metadata: Some(std::collections::HashMap::from([(
                "agent_id".to_string(),
                agent_id.to_string(),
            )])),
        },
    });
    let task_status_changed = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(5),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_250,
        data: ProvEventData::TaskStatusChanged {
            task_id: task_id.clone(),
            old_status: None,
            new_status: Some("working".to_string()),
        },
    });

    let llm_call_started = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(6),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_600,
        data: ProvEventData::LlmCallStarted {
            scope: CallScope::Task {
                task_id: task_id.clone(),
            },
            client: "TonyOpenRouter".to_string(),
            model: "deepseek/deepseek-chat".to_string(),
            function_name: "TonyShrinkChat".to_string(),
            prompt: json!({
                "messages": [
                    {"role": "system", "content": "You are Tony."},
                    {"role": "user", "content": "Hi Tony"}
                ],
                "temperature": 0.2
            }),
            metadata: json!({"request_id": "req-1", "message_id": "msg-1"}),
        },
    });
    let llm_call_completed = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(7),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_700,
        data: ProvEventData::LlmCallCompleted {
            scope: CallScope::Task {
                task_id: task_id.clone(),
            },
            client: "TonyOpenRouter".to_string(),
            model: "deepseek/deepseek-chat".to_string(),
            function_name: "TonyShrinkChat".to_string(),
            prompt: json!({
                "messages": [
                    {"role": "system", "content": "You are Tony."},
                    {"role": "user", "content": "Hi Tony"}
                ],
                "temperature": 0.2
            }),
            metadata: json!({"usage": {"prompt": 10, "completion": 20}}),
            usage: LlmUsage::Known {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            },
            duration_ms: 1234,
            success: true,
        },
    });
    let tool_call_started = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(8),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_800,
        data: ProvEventData::ToolCallStarted {
            scope: CallScope::Task {
                task_id: task_id.clone(),
            },
            tool_name: "memory/tony".to_string(),
            function_name: Some("ChooseTonyMemoryTool".to_string()),
            args: json!({
                "limit": 6,
                "memory": ["user: Hi Tony", "assistant: Hey, what's on your mind?"]
            }),
            metadata: json!({"source": "baml"}),
        },
    });
    let tool_call_completed = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(9),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_900,
        data: ProvEventData::ToolCallCompleted {
            scope: CallScope::Task {
                task_id: task_id.clone(),
            },
            tool_name: "memory/tony".to_string(),
            function_name: Some("ChooseTonyMemoryTool".to_string()),
            args: json!({
                "limit": 6,
                "memory": ["user: Hi Tony", "assistant: Hey, what's on your mind?"]
            }),
            metadata: json!({"result": {"count": 2, "tokens": [1, 2, 3]}}),
            duration_ms: 25,
            success: true,
        },
    });

    let task_created = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(10),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_000_050,
        data: ProvEventData::TaskCreated {
            task_id: task_id.clone(),
            agent_id: agent_uuid.clone(),
        },
    });
    let task_artifact_generated = ProvEvent::Task(TaskScopedEvent {
        id: EventId::from_counter(11),
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        timestamp_ms: 1_700_000_001_950,
        data: ProvEventData::TaskArtifactGenerated {
            task_id: task_id.clone(),
            artifact_id: Some(ArtifactId::from_external(ExternalId::new("artifact-99"))),
            artifact_type: Some("text".to_string()),
        },
    });

    let events = vec![
        agent_booted,
        task_created,
        task_status_changed,
        message_received,
        message_sent,
        llm_call_started,
        llm_call_completed,
        tool_call_started,
        tool_call_completed,
        task_artifact_generated,
    ];

    writer.add_events(events).await.expect("write events");

    let isolated_count = execute_cypher_query(
        "MATCH (n) \
         WHERE n.`a2a:context_id` = \"ctx-2-1\" AND NOT (n)--() \
         RETURN COUNT(n)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query isolated node count");
    assert_eq!(isolated_count.trim(), "0");

    let node_count = execute_cypher_query("MATCH (n) RETURN COUNT(n)", graph, &connection, true)
        .await
        .expect("query node count");
    assert!(node_count.trim().parse::<usize>().unwrap_or_default() > 5);

    let edge_count =
        execute_cypher_query("MATCH ()-[r]->() RETURN COUNT(r)", graph, &connection, true)
            .await
            .expect("query edge count");
    assert!(edge_count.trim().parse::<usize>().unwrap_or_default() > 5);

    let graph_snapshot = execute_cypher_query(
        "MATCH (n)-[r]->(m) \
         RETURN labels(n), n.name, properties(n), \
                type(r), properties(r), \
                labels(m), m.name, properties(m) \
         ORDER BY n.name, type(r), m.name",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query graph snapshot");
    assert_json_snapshot!(
        "falkordb_large_document_snapshot",
        graph_snapshot_json(&graph_snapshot)
    );
}

#[tokio::test]
async fn falkordb_writer_persists_send_message_calls_without_task() {
    let (_container, connection) = start_falkordb().await;
    let graph = "baml_prov_send_message_test";
    wait_for_falkordb(&connection, graph).await;

    let writer =
        FalkorDbProvenanceWriter::new(FalkorDbProvenanceConfig::new(connection.clone(), graph));
    let context_id = ContextId::new(3, 1);
    let agent_id = "00000000-0000-0000-0000-000000000010";
    let agent_uuid = AgentId::from_uuid(UuidId::parse_str(agent_id).unwrap());

    let agent_booted = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(11),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_001_900,
        data: ProvEventData::AgentBooted {
            agent_id: agent_uuid.clone(),
            agent_type: AgentType::new("tony").expect("agent_type"),
            agent_version: "1.0.0".to_string(),
            archive_path: "tony@1.0.0".to_string(),
        },
    });

    let message_received = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(12),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_000,
        data: ProvEventData::MessageReceived {
            id: MessageId::from_external(ExternalId::new("msg-10")),
            role: "user".to_string(),
            content: vec!["Ping".to_string()],
            metadata: Some(std::collections::HashMap::from([
                ("agent".to_string(), "tony".to_string()),
                ("agent_id".to_string(), agent_id.to_string()),
            ])),
        },
    });
    let message_sent = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(13),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_200,
        data: ProvEventData::MessageSent {
            id: MessageId::from_external(ExternalId::new("msg-11")),
            role: "assistant".to_string(),
            content: vec!["Pong".to_string()],
            metadata: Some(std::collections::HashMap::from([(
                "agent_id".to_string(),
                agent_id.to_string(),
            )])),
        },
    });
    let llm_call_started = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(14),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_050,
        data: ProvEventData::LlmCallStarted {
            scope: CallScope::Message {
                message_id: MessageId::from_external(ExternalId::new("msg-10")),
            },
            client: "TonyOpenRouter".to_string(),
            model: "deepseek/deepseek-chat".to_string(),
            function_name: "TonyShrinkChat".to_string(),
            prompt: json!({
                "messages": [
                    {"role": "user", "content": "Ping"}
                ]
            }),
            metadata: json!({"message_id": "msg-10", "agent_id": agent_id}),
        },
    });
    let llm_call_completed = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(15),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_120,
        data: ProvEventData::LlmCallCompleted {
            scope: CallScope::Message {
                message_id: MessageId::from_external(ExternalId::new("msg-10")),
            },
            client: "TonyOpenRouter".to_string(),
            model: "deepseek/deepseek-chat".to_string(),
            function_name: "TonyShrinkChat".to_string(),
            prompt: json!({
                "messages": [
                    {"role": "user", "content": "Ping"}
                ]
            }),
            metadata: json!({
                "message_id": "msg-10",
                "usage": {"prompt": 4, "completion": 6}
            }),
            usage: LlmUsage::Known {
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
            },
            duration_ms: 70,
            success: true,
        },
    });
    let tool_call_started = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(16),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_060,
        data: ProvEventData::ToolCallStarted {
            scope: CallScope::Message {
                message_id: MessageId::from_external(ExternalId::new("msg-10")),
            },
            tool_name: "memory/tony".to_string(),
            function_name: Some("ChooseTonyMemoryTool".to_string()),
            args: json!({"limit": 3}),
            metadata: json!({"message_id": "msg-10"}),
        },
    });
    let tool_call_completed = ProvEvent::Global(GlobalEvent {
        id: EventId::from_counter(17),
        context_id: context_id.clone(),
        timestamp_ms: 1_700_000_002_110,
        data: ProvEventData::ToolCallCompleted {
            scope: CallScope::Message {
                message_id: MessageId::from_external(ExternalId::new("msg-10")),
            },
            tool_name: "memory/tony".to_string(),
            function_name: Some("ChooseTonyMemoryTool".to_string()),
            args: json!({"limit": 3}),
            metadata: json!({"message_id": "msg-10", "result": {"count": 0}, "agent_id": agent_id}),
            duration_ms: 40,
            success: true,
        },
    });

    let events = vec![
        agent_booted,
        message_received,
        llm_call_started,
        tool_call_started,
        tool_call_completed,
        llm_call_completed,
        message_sent,
    ];
    writer.add_events(events).await.expect("write events");

    let task_count = execute_cypher_query(
        "MATCH (t:A2ATask) WHERE t.`a2a:context_id` = \"ctx-3-1\" RETURN COUNT(t)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query task count");
    assert_eq!(task_count.trim(), "0");

    let llm_link_count = execute_cypher_query(
        "MATCH (:A2AMessageProcessing {name: \"message_processing:msg-10\"})-[:WAS_INVOKED_BY]->(:LlmCall) RETURN COUNT(*)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query llm link count");
    assert_eq!(llm_link_count.trim(), "2");

    let tool_link_count = execute_cypher_query(
        "MATCH (:A2AMessageProcessing {name: \"message_processing:msg-10\"})-[:WAS_EXECUTED_BY]->(:ToolCall) RETURN COUNT(*)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query tool link count");
    assert_eq!(tool_link_count.trim(), "2");

    let isolated_count = execute_cypher_query(
        "MATCH (n) \
         WHERE n.`a2a:context_id` = \"ctx-3-1\" AND NOT (n)--() \
         RETURN COUNT(n)",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query isolated node count");
    assert_eq!(isolated_count.trim(), "0");

    let graph_snapshot = execute_cypher_query(
        "MATCH (n)-[r]->(m) \
         RETURN labels(n), n.name, properties(n), \
                type(r), properties(r), \
                labels(m), m.name, properties(m) \
         ORDER BY n.name, type(r), m.name",
        graph,
        &connection,
        true,
    )
    .await
    .expect("query graph snapshot");
    assert_json_snapshot!(
        "falkordb_send_message_snapshot",
        graph_snapshot_json(&graph_snapshot)
    );
}

fn graph_snapshot_json(raw: &str) -> Value {
    parse_graph_snapshot(raw)
        .map(normalize_value)
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn normalize_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_value).collect()),
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut normalized = serde_json::Map::new();
            for (key, value) in entries {
                normalized.insert(key, normalize_value(value));
            }
            Value::Object(normalized)
        }
        Value::String(value) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(&value) {
                match parsed {
                    Value::Array(_) | Value::Object(_) => {
                        let normalized = normalize_value(parsed);
                        serde_json::to_string(&normalized)
                            .map(Value::String)
                            .unwrap_or(Value::String(value))
                    }
                    _ => Value::String(value),
                }
            } else {
                Value::String(value)
            }
        }
        other => other,
    }
}

fn parse_graph_snapshot(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() || raw.trim() == "No results returned." {
        return Some(Value::Array(Vec::new()));
    }
    let mut rows = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record_str = if let Some(idx) = trimmed.find('[') {
            &trimmed[idx..]
        } else {
            trimmed
        };
        let record_str = record_str.trim();
        if !record_str.starts_with('[') || !record_str.ends_with(']') {
            return None;
        }
        let inner = &record_str[1..record_str.len() - 1];
        let parts = split_top_level(inner, ',');
        let mut values = Vec::new();
        for part in parts {
            values.push(parse_debug_value(part.trim())?);
        }
        rows.push(Value::Array(values));
    }
    Some(Value::Array(rows))
}

fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth_bracket: usize = 0;
    let mut depth_brace: usize = 0;
    let mut depth_paren: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if in_string {
            current.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                current.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                current.push(ch);
            }
            ']' => {
                depth_bracket = depth_bracket.saturating_sub(1);
                current.push(ch);
            }
            '{' => {
                depth_brace += 1;
                current.push(ch);
            }
            '}' => {
                depth_brace = depth_brace.saturating_sub(1);
                current.push(ch);
            }
            '(' => {
                depth_paren += 1;
                current.push(ch);
            }
            ')' => {
                depth_paren = depth_paren.saturating_sub(1);
                current.push(ch);
            }
            _ if ch == delimiter && depth_bracket == 0 && depth_brace == 0 && depth_paren == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn parse_debug_value(input: &str) -> Option<Value> {
    let value = input.trim();
    if value.starts_with("Map(") {
        return parse_debug_map(value);
    }
    if value.starts_with("Array(") {
        return parse_debug_array(value);
    }
    if value.starts_with("String(") {
        return parse_debug_string(value).map(Value::String);
    }
    if value.starts_with("I64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("F64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return serde_json::Number::from_f64(inner.parse::<f64>().ok()?).map(Value::Number);
    }
    if value.starts_with("Bool(") && value.ends_with(')') {
        let inner = &value[5..value.len() - 1];
        return inner.parse::<bool>().ok().map(Value::Bool);
    }
    if value == "Null" || value == "null" {
        return Some(Value::Null);
    }
    if value.starts_with('[') && value.ends_with(']') {
        return parse_bracket_array(value);
    }
    if value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str::<String>(value)
            .ok()
            .map(Value::String);
    }
    Some(Value::String(value.to_string()))
}

fn parse_debug_string(value: &str) -> Option<String> {
    if !value.starts_with("String(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[7..value.len() - 1];
    if inner.starts_with('"') && inner.ends_with('"') {
        serde_json::from_str::<String>(inner).ok()
    } else {
        let wrapped = format!("\"{}\"", inner);
        serde_json::from_str::<String>(&wrapped).ok()
    }
}

fn parse_debug_array(value: &str) -> Option<Value> {
    if !value.starts_with("Array(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[6..value.len() - 1];
    parse_bracket_array(inner)
}

fn parse_bracket_array(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Some(Value::Array(Vec::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut values = Vec::new();
    for part in parts {
        values.push(parse_debug_value(part.trim())?);
    }
    Some(Value::Array(values))
}

fn parse_debug_map(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    if !trimmed.starts_with("Map(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed[4..trimmed.len() - 1];
    let inner = inner.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    if inner.trim().is_empty() {
        return Some(Value::Object(serde_json::Map::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut map = serde_json::Map::new();
    for part in parts {
        let mut iter = split_top_level(part.trim(), ':').into_iter();
        let key_raw = iter.next()?.trim().to_string();
        let value_raw = iter.collect::<Vec<String>>().join(":");
        let key = serde_json::from_str::<String>(&key_raw).ok()?;
        let value = parse_debug_value(value_raw.trim())?;
        map.insert(key, value);
    }
    Some(Value::Object(map))
}
