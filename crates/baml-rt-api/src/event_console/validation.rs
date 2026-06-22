// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Validation for Event Console dispatch drafts.

use std::sync::atomic::{AtomicU64, Ordering};

use baml_rt_a2a::AgentRegistry;
use baml_rt_core::{
    AgentDispatchRequest, AgentDispatchRoutingKey, AgentInstanceId, AgentPackageName,
    AgentRouteKey, ContextId, EventSchemaVersion, EventSourceKind, TaskId,
    context::generate_context_id,
    event_subscription::{
        EventSourceKey, EventSubscription, PublishedEvent, subscriptions_match_published_event,
    },
    ids::ExternalId,
};
use jsonschema::JSONSchema;
use serde_json::{Value, json};

use super::{
    registry::find_message_shape_by_wire,
    types::{
        EventDispatchScopeDto, EventDispatchValidateRequestDto, EventValidationIssueDto,
        EventValidationReportDto,
    },
};

pub fn validate_draft(
    registry: &dyn AgentRegistry,
    body: &EventDispatchValidateRequestDto,
) -> EventValidationReportDto {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let package = match AgentPackageName::parse(&body.agent_package) {
        Some(p) => p,
        None => {
            errors.push(issue(
                "invalid_agent_package",
                "agent_package must match [A-Za-z0-9_-]",
                None,
            ));
            return report(false, false, errors, warnings, None);
        }
    };
    let instance = match AgentInstanceId::parse(&body.agent_instance_id) {
        Some(i) => i,
        None => {
            errors.push(issue(
                "invalid_agent_instance",
                "agent_instance_id must match [A-Za-z0-9_-]",
                None,
            ));
            return report(false, false, errors, warnings, None);
        }
    };

    let routing_key = match AgentDispatchRoutingKey::parse(&body.routing_key) {
        Some(k) => k,
        None => {
            errors.push(issue(
                "invalid_routing_key",
                "routing_key must be non-empty",
                None,
            ));
            return report(false, false, errors, warnings, None);
        }
    };
    let message_type = match EventSchemaVersion::parse(&body.message_type) {
        Some(v) => v,
        None => {
            errors.push(issue(
                "invalid_message_type",
                "message_type must be non-empty",
                None,
            ));
            return report(false, false, errors, warnings, None);
        }
    };

    if body.messages.is_empty() {
        errors.push(issue(
            "empty_batch",
            "messages must contain at least one event payload",
            None,
        ));
    }

    validate_scope(&body.scope, &mut errors);

    if let Some(entry) =
        find_message_shape_by_wire(body.message_type.as_str(), body.source_kind.as_deref())
    {
        if entry.delivery_defaults.routing_key != body.routing_key.as_str() {
            warnings.push(issue(
                "routing_key_not_in_registry",
                format!(
                    "routing_key does not match delivery default for {}",
                    entry.display_name
                ),
                None,
            ));
        }
        if let Some(ref sk) = body.source_kind {
            if EventSourceKind::parse(sk).is_none() {
                errors.push(issue(
                    "invalid_source_kind",
                    "source_kind must be non-empty",
                    None,
                ));
            } else if sk != &entry.source_kind {
                warnings.push(issue(
                    "source_kind_not_in_registry",
                    format!(
                        "source_kind does not match delivery default for {}",
                        entry.display_name
                    ),
                    None,
                ));
            }
        }
        validate_messages_against_schema(&entry.payload_schema, &body.messages, &mut errors);
        if body.message_type == entry.wire_schema_version
            && entry.wire_schema_version == "host.source-records.v1"
        {
            validate_source_records_nonempty(&body.messages, &mut errors);
        }
    } else {
        warnings.push(issue(
            "unknown_message_shape",
            "message type is not in the deliverable message-shape registry; only structural checks were applied",
            None,
        ));
    }

    let published = build_published_event(body);
    let route_key = AgentRouteKey::new(package.clone(), instance.clone());
    let fleet_matches = count_matching_subscribers(registry, &published);
    let matched_subscription = fleet_matches > 0;
    let selected_matches = registry
        .list_agents()
        .into_iter()
        .find(|e| {
            e.agent_package == route_key.agent_package.as_str()
                && e.agent_instance_id == route_key.agent_instance_id.as_str()
        })
        .is_some_and(|entry| {
            entry
                .agent_card
                .subscriptions
                .iter()
                .any(|sub| subscription_matches_event(sub, &published))
        });

    let label = find_message_shape_by_wire(body.message_type.as_str(), body.source_kind.as_deref())
        .map(|s| s.display_name)
        .unwrap_or_else(|| body.message_type.clone());

    if !matched_subscription {
        errors.push(issue(
            "no_matching_subscribers",
            format!(
                "no deployed agent subscribes to {label} (message_type={}, source_kind={})",
                body.message_type,
                body.source_kind.as_deref().unwrap_or("—")
            ),
            None,
        ));
    } else if !selected_matches {
        warnings.push(issue(
            "selected_agent_not_matched",
            format!(
                "selected agent {}/{} does not subscribe to {label}; publish will fan out to {fleet_matches} subscriber(s)",
                route_key.agent_package.as_str(),
                route_key.agent_instance_id.as_str(),
                fleet_matches = fleet_matches
            ),
            None,
        ));
    }

    let preview_produced_event = if errors.is_empty() {
        Some(build_produced_event_json(body, routing_key, message_type))
    } else {
        None
    };

    let valid = errors.is_empty();
    report(
        valid,
        matched_subscription,
        errors,
        warnings,
        preview_produced_event,
    )
}

fn count_matching_subscribers(registry: &dyn AgentRegistry, published: &PublishedEvent) -> usize {
    registry
        .list_agents()
        .iter()
        .filter(|entry| {
            entry
                .agent_card
                .subscriptions
                .iter()
                .any(|sub| subscription_matches_event(sub, published))
        })
        .count()
}

fn subscription_matches_event(sub: &EventSubscription, published: &PublishedEvent) -> bool {
    subscriptions_match_published_event(std::slice::from_ref(sub), published)
}

fn build_published_event(body: &EventDispatchValidateRequestDto) -> PublishedEvent {
    let source_kind = body
        .source_kind
        .as_deref()
        .and_then(EventSourceKind::parse)
        .unwrap_or_else(|| EventSourceKind::parse("unknown").expect("unknown source kind"));
    let source_key = body
        .source_key
        .as_deref()
        .and_then(EventSourceKey::parse)
        .unwrap_or_else(|| EventSourceKey::parse("unknown:unknown").expect("unknown source key"));
    let schema = EventSchemaVersion::parse(&body.message_type)
        .unwrap_or_else(|| EventSchemaVersion::parse("unknown.v1").expect("unknown schema"));
    PublishedEvent {
        schema_version: schema,
        source_kind,
        source_key,
    }
}

fn validate_scope(scope: &EventDispatchScopeDto, errors: &mut Vec<EventValidationIssueDto>) {
    match scope {
        EventDispatchScopeDto::NewContext => {}
        EventDispatchScopeDto::ExistingContext { context_id } => {
            if context_id.trim().is_empty() {
                errors.push(issue(
                    "invalid_scope",
                    "context_id is required for existing_context scope",
                    None,
                ));
            }
        }
        EventDispatchScopeDto::ExistingTask {
            context_id,
            task_id,
        } => {
            if context_id.trim().is_empty() || task_id.trim().is_empty() {
                errors.push(issue(
                    "invalid_scope",
                    "context_id and task_id are required for existing_task scope",
                    None,
                ));
            }
        }
    }
}

fn validate_source_records_nonempty(messages: &[Value], errors: &mut Vec<EventValidationIssueDto>) {
    for (index, message) in messages.iter().enumerate() {
        let records = message.get("records").and_then(Value::as_array);
        match records {
            Some(arr) if arr.is_empty() => {
                errors.push(issue(
                    "empty_records",
                    "messages[].records must contain at least one source record — \
                     publish would accept with no agent work",
                    Some(format!("/messages/{index}/records")),
                ));
            }
            None => {
                errors.push(issue(
                    "missing_records",
                    "host.source-records.v1 batch must include a records array",
                    Some(format!("/messages/{index}/records")),
                ));
            }
            _ => {}
        }
    }
}

fn validate_messages_against_schema(
    schema: &Value,
    messages: &[Value],
    errors: &mut Vec<EventValidationIssueDto>,
) {
    let compiled = match JSONSchema::compile(schema) {
        Ok(c) => c,
        Err(err) => {
            errors.push(issue(
                "invalid_message_shape_schema",
                format!("message-shape JSON Schema failed to compile: {err}"),
                None,
            ));
            return;
        }
    };

    for (index, message) in messages.iter().enumerate() {
        if let Err(iter) = compiled.validate(message) {
            for err in iter {
                let pointer = err.instance_path.to_string();
                let ptr = if pointer.is_empty() {
                    format!("/messages/{index}")
                } else {
                    format!("/messages/{index}{pointer}")
                };
                errors.push(issue("schema_validation", err.to_string(), Some(ptr)));
            }
        }
    }
}

pub fn build_agent_dispatch_request(
    body: &EventDispatchValidateRequestDto,
) -> Result<AgentDispatchRequest, String> {
    let routing_key = AgentDispatchRoutingKey::parse(&body.routing_key)
        .ok_or_else(|| "routing_key must be non-empty".to_string())?;
    let message_type = EventSchemaVersion::parse(&body.message_type)
        .ok_or_else(|| "message_type must be non-empty".to_string())?;

    let (context_id, task_id, message_id) = resolve_dispatch_scope(&body.scope, &body.message_id)?;

    let mut metadata = body.metadata.clone().unwrap_or_else(|| json!({}));
    if let Some(obj) = metadata.as_object_mut() {
        obj.entry("origin".to_string())
            .or_insert_with(|| Value::String("operator-eval-console".into()));
    }

    Ok(AgentDispatchRequest {
        routing_key,
        message_type,
        messages: body.messages.clone(),
        context_id,
        task_id,
        message_id: Some(message_id),
        source_kind: None,
        source_key: None,
        producer_key: None,
        metadata: Some(metadata),
    })
}

static CONSOLE_MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn mint_console_message_id(body_message_id: &Option<String>) -> String {
    if let Some(id) = body_message_id {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let n = CONSOLE_MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evt-console-msg-{n}")
}

/// Resolve scope ids for operator-console dispatch. `new_context` mints observable ids for provenance.
fn resolve_dispatch_scope(
    scope: &EventDispatchScopeDto,
    body_message_id: &Option<String>,
) -> Result<(Option<ContextId>, Option<TaskId>, String), String> {
    let message_id = mint_console_message_id(body_message_id);
    match scope {
        EventDispatchScopeDto::NewContext => Ok((Some(generate_context_id()), None, message_id)),
        EventDispatchScopeDto::ExistingContext { context_id } => {
            Ok((Some(ContextId::from(context_id.as_str())), None, message_id))
        }
        EventDispatchScopeDto::ExistingTask {
            context_id,
            task_id,
        } => Ok((
            Some(ContextId::from(context_id.as_str())),
            Some(TaskId::from_external(ExternalId::new(task_id.clone()))),
            message_id,
        )),
    }
}

fn build_produced_event_json(
    body: &EventDispatchValidateRequestDto,
    routing_key: AgentDispatchRoutingKey,
    schema_version: EventSchemaVersion,
) -> Value {
    let (context_id, task_id, message_id) = resolve_dispatch_scope(&body.scope, &body.message_id)
        .unwrap_or((None, None, String::new()));
    let source_kind = body
        .source_kind
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let source_key = body
        .source_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown:unknown");
    json!({
        "routing_key": routing_key.as_str(),
        "schema_version": schema_version.as_str(),
        "source_kind": source_kind,
        "source_key": source_key,
        "messages": body.messages,
        "context_id": context_id.map(|c| c.to_string()),
        "task_id": task_id.map(|t| t.to_string()),
        "message_id": message_id,
        "metadata": body.metadata.clone().unwrap_or_else(|| json!({ "origin": "operator-event-console" })),
    })
}

fn issue(
    code: &str,
    message: impl Into<String>,
    json_pointer: Option<String>,
) -> EventValidationIssueDto {
    EventValidationIssueDto {
        code: code.to_string(),
        message: message.into(),
        json_pointer,
    }
}

fn report(
    valid: bool,
    matched_subscription: bool,
    errors: Vec<EventValidationIssueDto>,
    warnings: Vec<EventValidationIssueDto>,
    preview_produced_event: Option<Value>,
) -> EventValidationReportDto {
    EventValidationReportDto {
        valid,
        matched_subscription,
        errors,
        warnings,
        preview_produced_event,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use baml_rt_core::{
        A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister, BusStream,
    };

    use super::*;

    struct EmptyRegistry;

    impl AgentLister for EmptyRegistry {
        fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
            vec![AgentDiscoveryEntry {
                agent_package: "dispatch-echo".into(),
                agent_instance_id: "default".into(),
                name: "dispatch-echo".into(),
                version: "1.0.0".into(),
                agent_card: AgentCard {
                    name: "dispatch-echo".into(),
                    version: "1.0.0".into(),
                    content_hash: None,
                    repository_version: None,
                    agent_package: "dispatch-echo".into(),
                    agent_instance_id: "default".into(),
                    tools: vec![],
                    baml_functions: vec![],
                    description: None,
                    capabilities: vec![],
                    tags: vec![],
                    subscriptions: vec![EventSubscription {
                        schema_versions: vec![
                            EventSchemaVersion::parse("host.source-records.v1").unwrap(),
                        ],
                        source_kinds: vec![EventSourceKind::parse("clickup").unwrap()],
                        ..Default::default()
                    }],
                },
            }]
        }
    }

    #[async_trait]
    impl AgentRegistry for EmptyRegistry {
        async fn handle_a2a_stream(
            &self,
            _key: &AgentRouteKey,
            _request: A2aWireRequest,
        ) -> baml_rt_core::Result<BusStream<A2aStreamChunk>> {
            Err(baml_rt_core::BamlRtError::InvalidArgument(
                "not supported".into(),
            ))
        }

        async fn handle_dispatch(
            &self,
            _key: &AgentRouteKey,
            _request: AgentDispatchRequest,
        ) -> baml_rt_core::Result<baml_rt_core::AgentDispatchAck> {
            Err(baml_rt_core::BamlRtError::InvalidArgument(
                "not supported".into(),
            ))
        }
    }

    #[test]
    fn validate_matching_subscription_for_dispatch_echo() {
        let body = EventDispatchValidateRequestDto {
            agent_package: "dispatch-echo".into(),
            agent_instance_id: "default".into(),
            routing_key: "event:intake".into(),
            message_type: "host.source-records.v1".into(),
            source_kind: Some("clickup".into()),
            source_key: Some("clickup:list-1".into()),
            messages: vec![json!({
                "schema_version": "host.source-records.v1",
                "emitted_at_unix": 1_735_720_000u64,
                "source": {
                    "source_kind": "clickup",
                    "source_key": "clickup:list-1",
                    "source_label": "list"
                },
                "records": [{
                    "record_kind": "clickup.lifecycle_event",
                    "key": "clickup-created:1",
                    "event": "created",
                    "task_id": "t1",
                    "list_id": "list-1",
                    "snapshot": { "name": "Fix ingress" },
                    "revision": 1
                }]
            })],
            scope: EventDispatchScopeDto::NewContext,
            message_id: None,
            metadata: None,
        };
        let report = validate_draft(&EmptyRegistry, &body);
        assert!(report.valid, "errors={:?}", report.errors);
        assert!(report.matched_subscription);
        let preview = report
            .preview_produced_event
            .as_ref()
            .expect("valid draft should include preview_produced_event");
        assert!(
            preview.get("context_id").and_then(Value::as_str).is_some(),
            "new_context preview must mint context_id: {preview}"
        );
        assert!(
            preview.get("message_id").and_then(Value::as_str).is_some(),
            "preview must include message_id: {preview}"
        );
        assert_eq!(
            preview.get("schema_version").and_then(Value::as_str),
            Some("host.source-records.v1")
        );
        assert!(
            preview.get("routing_key").and_then(Value::as_str).is_some(),
            "preview must include routing_key: {preview}"
        );
    }
}
