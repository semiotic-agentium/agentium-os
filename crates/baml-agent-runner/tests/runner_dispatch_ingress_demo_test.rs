#![cfg(feature = "llm-tests")]

mod common;

use std::sync::Arc;

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    dispatch_ingress::dispatch_unit_task_id,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceContextReader, ProvenanceWriter};
use common::{
    TempDirCleanup, build_agent_dir_to_temp_async, e2e_serial_gate,
    quickjs_config_with_host_ingress, start_runner_api_server, try_load_dotenv_for_tests,
};
use serde_json::{Value, json};
use test_support::common::{
    agent_fixture, require_api_key, test_surreal_store, workspace_fnox_path,
};
use tokio::time::{Duration, timeout};

fn clickup_ingress_batch_message() -> Value {
    json!({
        "schema_version": "host.source-records.v1",
        "emitted_at_unix": 1_735_720_000,
        "source": {
            "source_kind": "clickup",
            "source_key": "clickup:list-demo",
            "source_label": "Demo list"
        },
        "records": [
            {
                "record_kind": "clickup.task",
                "key": "clickup-created:task-a:1",
                "title": "First ingress unit",
                "description": "Host wrote this slice into unit task history",
                "priority": "normal"
            },
            {
                "record_kind": "clickup.task",
                "key": "clickup-created:task-b:1",
                "title": "Second ingress unit",
                "description": "Separate withTask scope",
                "priority": "high"
            }
        ]
    })
}

async fn setup_dispatch_ingress_demo_agent(
    built: &std::path::Path,
) -> (
    baml_rt::A2aAgent,
    Arc<baml_rt_provenance::SurrealProvenanceStore>,
) {
    try_load_dotenv_for_tests();
    let _api_key = require_api_key();

    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");

    let provenance = test_surreal_store().await;
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("dispatch-ingress-demo").expect("agent type"),
            "1.0.0".to_string(),
            "dispatch-ingress-demo@1.0.0".to_string(),
        ))
        .await
        .expect("agent booted");

    let agent_code =
        std::fs::read_to_string(built.join("dist").join("index.js")).expect("dist index.js");
    let store = Arc::clone(&provenance);
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_quickjs_config(quickjs_config_with_host_ingress(Arc::clone(&store)))
        .with_surreal_store(store.clone())
        .build()
        .await
        .expect("build agent");

    (agent, store)
}

#[tokio::test]
async fn dispatch_ingress_demo_with_task_per_lifecycle_unit() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let built = build_agent_dir_to_temp_async(
        agent_fixture("dispatch-ingress-demo"),
        "dispatch-ingress-demo",
    )
    .await;
    let _built_guard = TempDirCleanup::new(built.clone());
    let (agent, store) = setup_dispatch_ingress_demo_agent(&built).await;
    let runner_api = start_runner_api_server("dispatch-ingress-demo", agent, store.clone())
        .await
        .expect("runner api");

    let dispatch_url = format!(
        "{}/agents/dispatch-ingress-demo/default/dispatch",
        runner_api.base_url.trim_end_matches('/')
    );
    let body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [clickup_ingress_batch_message()]
    });

    let client = reqwest::Client::new();
    let response = timeout(
        Duration::from_secs(120),
        client.post(&dispatch_url).json(&body).send(),
    )
    .await
    .expect("dispatch timed out")
    .expect("dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("ack json");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("clickup-created:task-a:1") && detail.contains("clickup-created:task-b:1"),
        "expected both unit keys in ack detail, got: {ack:?}"
    );

    let context_id = ack
        .get("context_id")
        .and_then(Value::as_str)
        .map(ContextId::from)
        .expect("dispatch ack context_id");
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    let user_texts: Vec<String> = items
        .iter()
        .filter(|i| i.role == "user")
        .filter_map(|i| match &i.content {
            baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        user_texts.iter().any(|t| t.contains("First ingress unit")),
        "expected unit-a user transcript, items={items:?}"
    );
    assert!(
        user_texts.iter().any(|t| t.contains("Second ingress unit")),
        "expected unit-b user transcript, items={items:?}"
    );

    for (unit_key, needle) in [
        ("clickup-created:task-a:1", "First ingress unit"),
        ("clickup-created:task-b:1", "Second ingress unit"),
    ] {
        let task_id = dispatch_unit_task_id(&context_id, unit_key);
        let scoped = store
            .conversation_context_with_task(&context_id, None, Some(&task_id))
            .await
            .expect("task-scoped conversation_context");
        let scoped_user: Vec<String> = scoped
            .iter()
            .filter(|i| i.role == "user")
            .filter_map(|i| match &i.content {
                baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            scoped_user.len(),
            1,
            "unit {unit_key} must have exactly one ingress user line in task scope: {scoped:?}"
        );
        assert!(
            scoped_user[0].contains(needle),
            "unit {unit_key} transcript must contain {needle:?}, got {}",
            scoped_user[0]
        );
    }

    runner_api.stop().await;
}
