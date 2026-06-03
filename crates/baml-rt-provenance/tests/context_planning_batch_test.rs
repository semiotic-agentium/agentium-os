// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Batched planning reads — index-authoritative contract.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{
    PlanStepSpec, ProvEvent, ProvenanceWriter, surreal_store::PlanningScopeQuery,
};
use test_support::testing::provenance_fixtures::build_isolated_store;
use uuid::Uuid;

#[tokio::test]
async fn planning_batch_reads_from_write_maintained_index() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_200, 1);
    let task_id = TaskId::from_external(ExternalId::new("indexed-planning-task"));
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            baml_rt_provenance::AgentType::new("planning-agent").expect("type"),
            "1.0.0".to_string(),
            "planning@1.0.0".to_string(),
        ))
        .await
        .expect("boot");
    store
        .add_event(ProvEvent::task_exists(ctx.clone(), task_id.clone()))
        .await
        .expect("task");
    store
        .add_event(ProvEvent::task_execution_started(
            ctx.clone(),
            task_id.clone(),
            agent_id,
        ))
        .await
        .expect("execution");
    store
        .add_event(ProvEvent::intent_resolved(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "Resolve ingress".to_string(),
            vec![],
            None,
            None,
        ))
        .await
        .expect("intent");
    store
        .add_event(ProvEvent::plan_generated(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "plan-1",
            vec![PlanStepSpec {
                step_id: "s1".into(),
                description: "Fetch".to_string(),
                order: 1,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan");

    let scope = PlanningScopeQuery {
        context_id: ctx,
        task_id: None,
        agent_package: None,
        agent_id: None,
        history_limit: 5,
    };
    let (all_task_ids, tasks) = store.query_planning_batch(&scope).await.expect("batch");
    assert!(
        all_task_ids.iter().any(|id| id == task_id.as_str()),
        "scoped picker must list task: {all_task_ids:?}"
    );
    assert_eq!(
        tasks.len(),
        1,
        "index must surface planning task: {tasks:?}"
    );
    assert_eq!(tasks[0].task_id, task_id.as_str());
    assert!(
        tasks[0].current_intent.is_some(),
        "index must hydrate current intent"
    );
    assert!(
        tasks[0].current_plan.is_some(),
        "index must hydrate current plan"
    );
}

#[tokio::test]
async fn planning_batch_empty_when_index_cleared() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(1_900_000_000_201, 1);
    let task_id = TaskId::from_external(ExternalId::new("cleared-index-task"));
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            baml_rt_provenance::AgentType::new("planning-agent").expect("type"),
            "1.0.0".to_string(),
            "planning@1.0.0".to_string(),
        ))
        .await
        .expect("boot");
    store
        .add_event(ProvEvent::task_exists(ctx.clone(), task_id.clone()))
        .await
        .expect("task");
    store
        .add_event(ProvEvent::task_execution_started(
            ctx.clone(),
            task_id.clone(),
            agent_id,
        ))
        .await
        .expect("execution");
    store
        .add_event(ProvEvent::intent_resolved(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "Resolve ingress".to_string(),
            vec![],
            None,
            None,
        ))
        .await
        .expect("intent");

    store
        .db()
        .query("DELETE context_planning_index")
        .await
        .expect("delete index")
        .check()
        .expect("delete ok");

    let scope = PlanningScopeQuery {
        context_id: ctx,
        task_id: None,
        agent_package: None,
        agent_id: None,
        history_limit: 5,
    };
    let (_all_task_ids, tasks) = store.query_planning_batch(&scope).await.expect("batch");
    assert!(
        tasks.is_empty(),
        "missing index rows must yield empty planning slice (no graph-head fallback): {tasks:?}"
    );
}
