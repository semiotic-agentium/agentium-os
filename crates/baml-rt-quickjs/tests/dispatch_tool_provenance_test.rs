// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    EventSchemaVersion,
    bus::{BusWithEffects, EffectEmitter, EffectLiveness},
    context::{InvocationScope, RuntimeScope},
    dispatch::{AgentDispatchRequest, AgentDispatchRoutingKey},
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    ProvenanceEffectSubscriber, ProvenanceOpsFilters, ProvenanceOpsQuery,
    ProvenanceOpsQueryRequest, ProvenanceOpsResource, SurrealStoreBuilder,
};
use baml_rt_quickjs::{BridgeHandle, QuickJSBridge, invoke_optional_js_function_handover};
use baml_rt_tools::{BamlTool, bundles::BundleType};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug)]
struct ScopeEchoTool;

struct TestBundle;

impl BundleType for TestBundle {
    const NAME: &'static str = "test";

    fn description() -> &'static str {
        "Test bundle for dispatch provenance regression"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct ScopeEchoInput {
    #[serde(default)]
    context_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
}

impl baml_rt_tools::DescribeAction for ScopeEchoInput {
    fn describe(&self) -> String {
        format!(
            "ScopeEchoInput(context_id={:?}, message_id={:?}, task_id={:?})",
            self.context_id, self.message_id, self.task_id
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct ScopeEchoOutput {
    context_id: Option<String>,
    message_id: Option<String>,
    task_id: Option<String>,
}

#[async_trait]
impl BamlTool for ScopeEchoTool {
    type Bundle = TestBundle;
    const LOCAL_NAME: &'static str = "scope_echo";
    type OpenInput = ();
    type Input = ScopeEchoInput;
    type Output = ScopeEchoOutput;

    fn description(&self) -> &'static str {
        "Echoes current runtime scope."
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(ScopeEchoOutput {
            context_id: args.context_id,
            message_id: args.message_id,
            task_id: args.task_id,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn on_dispatch_tool_session_materializes_tool_calls_in_provenance() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");
    let effect_bus = Arc::new(BusWithEffects::new());
    effect_bus
        .subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register tool");
    manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);

    let manager = Arc::new(RwLock::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000d155").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id.clone())
        .await
        .expect("quickjs bridge");
    bridge.set_effect_liveness(effect_bus.clone() as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");
    bridge
        .eval_sync(
            r#"
            globalThis.onDispatch = async function(_request) {
                const session = await openToolSession("test/scope_echo");
                await session.send({
                    context_id: globalThis.__bamlScope?.context_id ?? null,
                    message_id: globalThis.__bamlScope?.message_id ?? null,
                    task_id: globalThis.__bamlScope?.task_id ?? null,
                });
                const step = await session.continue();
                await session.finish();
                return { accepted: true, detail: JSON.stringify(step && step.output ? step.output : {}) };
            };
            "#,
        )
        .await
        .expect("install onDispatch");

    let bridge = Arc::new(Mutex::new(bridge));
    let handle = Arc::new(BridgeHandle::new(bridge, "dispatch-prov-test"));

    let context_id = ContextId::new(1_778_675_700_000, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
    let scope = InvocationScope::new(RuntimeScope::task_scope(
        context_id.clone(),
        agent_id,
        "system/callback:test-dispatch".into(),
        task_id.clone(),
    ));
    let request = AgentDispatchRequest {
        routing_key: AgentDispatchRoutingKey::parse("system:callback").expect("routing key"),
        message_type: EventSchemaVersion::parse("system.callback.v1").expect("schema version"),
        messages: vec![json!({ "payload": { "token": "dispatch-probe" } })],
        context_id: Some(context_id.clone()),
        task_id: Some(task_id.clone()),
        message_id: Some("system/callback:test-dispatch".to_string()),
        source_kind: None,
        source_key: None,
        producer_key: None,
        metadata: None,
    };

    let ack = invoke_optional_js_function_handover(
        handle.as_ref(),
        scope,
        "onDispatch",
        serde_json::to_value(request).expect("dispatch request json"),
    )
    .await
    .expect("invoke onDispatch");
    assert!(ack.is_some(), "onDispatch should be registered");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id),
                task_id: Some(task_id),
                tool_name: Some("test/scope_echo".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;

    assert!(
        !rows.is_empty(),
        "onDispatch host tool session should materialize tool_calls rows"
    );
}
