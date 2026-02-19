use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use baml_rt_core::{
    context::RuntimeScope,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_observability::spans;
use tracing::{
    Id, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    layer::{Context, Layer},
    prelude::*,
    registry::LookupSpan,
};

#[derive(Debug, Default)]
struct FieldCapture {
    fields: HashMap<String, String>,
}

impl Visit for FieldCapture {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct CaptureLayer {
    seen: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

type CaptureLayerNewResult = (CaptureLayer, Arc<Mutex<Vec<HashMap<String, String>>>>);

impl CaptureLayer {
    fn new() -> CaptureLayerNewResult {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut capture = FieldCapture::default();
        attrs.record(&mut capture);
        if let Some(span) = ctx.span(id) {
            capture.fields.insert(
                "__span_name".to_string(),
                span.metadata().name().to_string(),
            );
        }
        self.seen.lock().expect("capture lock").push(capture.fields);
    }
}

fn normalize(value: Option<&String>) -> Option<String> {
    value.map(|v| v.trim_matches('"').to_string())
}

fn message_id(value: &str) -> MessageId {
    MessageId::from_external(ExternalId::new(value.to_string()))
}

fn task_id(value: &str) -> TaskId {
    TaskId::from_external(ExternalId::new(value.to_string()))
}

#[test]
fn prop_root_spans_map_runtime_scope_fields() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000123").unwrap());
    let scopes = vec![
        RuntimeScope::task_scope(
            ContextId::new(100, 1),
            agent_id.clone(),
            message_id("msg-1"),
            task_id("task-1"),
        ),
        RuntimeScope::message_scope(
            ContextId::new(101, 2),
            agent_id.clone(),
            message_id("msg-2"),
        ),
        RuntimeScope::message_scope(
            ContextId::new(102, 3),
            agent_id.clone(),
            message_id("msg-3"),
        ),
    ];

    for scope in scopes {
        let (layer, seen) = CaptureLayer::new();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = spans::invoke_function(Some(&scope), "agent", "onChatMessage");
            let _guard = span.enter();
        });

        let captured = seen.lock().expect("seen lock");
        let invoke = captured
            .iter()
            .find(|entry| {
                entry.get("__span_name").map(String::as_str) == Some("baml_rt.invoke_function")
            })
            .expect("invoke_function span captured");

        assert_eq!(
            normalize(invoke.get("context_id")),
            Some(scope.context_id().as_str().to_string())
        );
        assert_eq!(
            normalize(invoke.get("message_id")),
            Some(scope.message_id().as_str().to_string())
        );
        assert_eq!(
            normalize(invoke.get("task_id")),
            Some(
                scope
                    .task_id_opt()
                    .map(|id| id.as_str().to_string())
                    .unwrap_or_else(|| "none".to_string())
            )
        );
    }
}

#[test]
fn prop_a2a_spans_map_runtime_scope_fields() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000456").unwrap());
    let scope = RuntimeScope::task_scope(
        ContextId::new(200, 9),
        agent_id,
        message_id("msg-a2a"),
        task_id("task-a2a"),
    );

    let (layer, seen) = CaptureLayer::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let request = spans::a2a_request(Some(&scope), "message/send", "corr-1");
        let _request_guard = request.enter();
        let stream = spans::a2a_stream(Some(&scope), "message/stream", "corr-2");
        let _stream_guard = stream.enter();
        let cancel = spans::a2a_cancel(Some(&scope), "task-a2a", "corr-3");
        let _cancel_guard = cancel.enter();
    });

    let captured = seen.lock().expect("seen lock");
    for span_name in [
        "baml_rt.a2a_request",
        "baml_rt.a2a_stream",
        "baml_rt.a2a_cancel",
    ] {
        let entry = captured
            .iter()
            .find(|entry| entry.get("__span_name").map(String::as_str) == Some(span_name))
            .unwrap_or_else(|| panic!("{span_name} span captured"));
        assert_eq!(
            normalize(entry.get("context_id")),
            Some(scope.context_id().as_str().to_string())
        );
        assert_eq!(
            normalize(entry.get("message_id")),
            Some(scope.message_id().as_str().to_string())
        );
        assert_eq!(
            normalize(entry.get("task_id")),
            scope.task_id_opt().map(|id| id.as_str().to_string())
        );
    }
}
