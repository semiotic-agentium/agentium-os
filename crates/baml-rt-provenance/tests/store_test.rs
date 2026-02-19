use std::collections::BTreeMap;

use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
use baml_rt_provenance::{ProvEvent, normalize_event};
use serde_json::{Value, json};

#[tokio::test]
async fn test_normalize_event_snapshot_for_tool_call_started() {
    let event = ProvEvent::tool_call_started_global(
        ContextId::new(1, 1),
        MessageId::from_external(ExternalId::new("msg-1")),
        "tool".to_string(),
        None,
        json!({"input": "value"}),
        json!({"message_id": "msg-1", "agent_id": "00000000-0000-0000-0000-000000000010"}),
    );

    assert_eq!(event.context_id(), &ContextId::new(1, 1));

    let normalized = normalize_event(&event).expect("normalize event");
    let snapshot = snapshot_value(&normalized);
    let expected = json!({
        "activities": {
            format!("tool_call:{}", event.id().as_str()): {
                "a2a:context_id": "ctx-1-1",
                "a2a:event_id": event.id().as_str(),
                "a2a:metadata": {
                    "message_id": "msg-1",
                    "agent_id": "00000000-0000-0000-0000-000000000010"
                },
                "a2a:tool_name": "tool",
                "prov:startTime": event.timestamp_ms(),
                "prov:type": "a2a:ToolCall"
            },
            "message_processing:msg-1": {
                "a2a:context_id": "ctx-1-1",
                "a2a:message_id": "msg-1",
                "prov:type": "a2a:A2AMessageProcessing"
            }
        },
        "entities": {
            "message:msg-1": {
                "a2a:context_id": "ctx-1-1",
                "a2a:event_id": event.id().as_str(),
                "a2a:message_id": "msg-1",
                "prov:type": "a2a:Message"
            },
            format!("tool_args:{}", event.id().as_str()): {
                "a2a:args": {"input":"value"},
                "a2a:context_id": "ctx-1-1",
                "a2a:event_id": event.id().as_str(),
                "prov:type": "a2a:ToolArgs"
            }
        },
        "agents": {},
        "used": [
            {
                "activity": format!("tool_call:{}", event.id().as_str()),
                "entity": "message:msg-1",
                "role": "input_message"
            },
            {
                "activity": format!("tool_call:{}", event.id().as_str()),
                "entity": format!("tool_args:{}", event.id().as_str()),
                "role": "a2a:args"
            }
        ],
        "was_generated_by": [],
        "qualified_generation": [],
        "was_associated_with": [],
        "was_derived_from": [],
        "a2a_relations": [
            {
                "relation": "A2A_MESSAGE_CALL",
                "from": "message_processing:msg-1",
                "to": format!("tool_call:{}", event.id().as_str()),
                "attributes": {
                    "a2a:context_id": "ctx-1-1",
                    "a2a:timestamp_ms": event.timestamp_ms()
                }
            }
        ]
    });
    assert_eq!(sort_value(snapshot), sort_value(expected));
}

fn snapshot_value(normalized: &baml_rt_provenance::NormalizedProv) -> Value {
    let mut activities = BTreeMap::new();
    for (id, activity) in normalized.document.activities() {
        let mut map = BTreeMap::new();
        if let Some(start_time_ms) = activity.start_time_ms {
            map.insert(
                "prov:startTime".to_string(),
                Value::Number(start_time_ms.into()),
            );
        }
        if let Some(end_time_ms) = activity.end_time_ms {
            map.insert(
                "prov:endTime".to_string(),
                Value::Number(end_time_ms.into()),
            );
        }
        if let Some(prov_type) = &activity.prov_type {
            map.insert("prov:type".to_string(), Value::String(prov_type.clone()));
        }
        for (key, value) in &activity.attributes {
            map.insert(key.clone(), value.clone());
        }
        activities.insert(
            id.as_str().to_string(),
            Value::Object(map.into_iter().collect()),
        );
    }

    let mut entities = BTreeMap::new();
    for (id, entity) in normalized.document.entities() {
        let mut map = BTreeMap::new();
        if let Some(prov_type) = &entity.prov_type {
            map.insert("prov:type".to_string(), Value::String(prov_type.clone()));
        }
        for (key, value) in &entity.attributes {
            map.insert(key.clone(), value.clone());
        }
        entities.insert(
            id.as_str().to_string(),
            Value::Object(map.into_iter().collect()),
        );
    }

    let mut agents = BTreeMap::new();
    for (id, agent) in normalized.document.agents() {
        let mut map = BTreeMap::new();
        if let Some(prov_type) = &agent.prov_type {
            map.insert("prov:type".to_string(), Value::String(prov_type.clone()));
        }
        for (key, value) in &agent.attributes {
            map.insert(key.clone(), value.clone());
        }
        agents.insert(
            id.as_str().to_string(),
            Value::Object(map.into_iter().collect()),
        );
    }

    let mut used = Vec::new();
    for (_, used_rel) in normalized.document.used() {
        used.push(json!({
            "activity": used_rel.activity.as_str(),
            "entity": used_rel.entity.as_str(),
            "role": used_rel.role
        }));
    }

    let mut was_generated_by = Vec::new();
    for (_, rel) in normalized.document.was_generated_by() {
        was_generated_by.push(json!({
            "entity": rel.entity.id(),
            "activity": rel.activity.as_str(),
            "time_ms": rel.time_ms
        }));
    }

    let mut qualified_generation = Vec::new();
    for (_, rel) in normalized.document.qualified_generation() {
        qualified_generation.push(json!({
            "entity": rel.entity.id(),
            "activity": rel.activity.as_str(),
            "time_ms": rel.time_ms
        }));
    }

    let mut was_associated_with = Vec::new();
    for (_, rel) in normalized.document.was_associated_with() {
        was_associated_with.push(json!({
            "activity": rel.activity.as_str(),
            "agent": rel.agent.as_str(),
            "role": rel.role
        }));
    }

    let mut was_derived_from = Vec::new();
    for (_, rel) in normalized.document.was_derived_from() {
        was_derived_from.push(json!({
            "generated_entity": rel.generated_entity.as_str(),
            "used_entity": rel.used_entity.as_str(),
            "activity": rel.activity.as_ref().map(|id| id.as_str().to_string()),
            "prov_type": rel.prov_type
        }));
    }

    let mut a2a_relations = Vec::new();
    for rel in &normalized.derived_relations {
        a2a_relations.push(json!({
            "relation": rel.relation.as_str(),
            "from": rel.from.id(),
            "to": rel.to.id(),
            "attributes": rel.attributes
        }));
    }

    json!({
        "activities": activities,
        "entities": entities,
        "agents": agents,
        "used": used,
        "was_generated_by": was_generated_by,
        "qualified_generation": qualified_generation,
        "was_associated_with": was_associated_with,
        "was_derived_from": was_derived_from,
        "a2a_relations": a2a_relations
    })
}

fn sort_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            let mut sorted: Vec<Value> = values.into_iter().map(sort_value).collect();
            sorted.sort_by_key(|value| value.to_string());
            Value::Array(sorted)
        }
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key, sort_value(value));
            }
            Value::Object(sorted.into_iter().collect())
        }
        other => other,
    }
}
