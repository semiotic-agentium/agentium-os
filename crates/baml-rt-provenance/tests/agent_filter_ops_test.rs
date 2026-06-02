// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Graph-query tests for call-activity agent edge traversal.

use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use baml_rt_provenance::metamodel::{ContextNodeId, GraphQuery, TaskExecutionNodeId, labels};

fn task_id() -> TaskId {
    TaskId::from_external(ExternalId::new("dispatch-unit-test"))
}

#[test]
fn llm_call_agent_instances_use_forward_two_hop_or_clause() {
    let instances = vec!["agent_instance:abc".to_string()];
    let (sql, _) = GraphQuery::<labels::LlmCall, _>::new()
        .scoped_to_ctx(ContextNodeId::for_context_id(&ContextId::from("ctx-test")))
        .for_agent_instances(&instances)
        .into_surreal();
    assert!(
        sql.contains("rel_type = 'A2A_MESSAGE_CALL' OR rel_type = 'A2A_TASK_CALL'"),
        "forward semi-join must OR call edge rel_types (Surreal has no IN list): {sql}"
    );
    assert!(
        sql.contains("WAS_EXECUTED_BY"),
        "must walk WAS_EXECUTED_BY to agent instances: {sql}"
    );
    assert!(
        !sql.contains("props.a2a_agent_id"),
        "agent ownership must stay edge-based: {sql}"
    );
}

#[test]
fn llm_call_task_execution_composes_without_message_call_or_when_task_only() {
    let (sql, _) = GraphQuery::<labels::LlmCall, _>::new()
        .scoped_to_ctx(ContextNodeId::for_context_id(&ContextId::from("ctx-test")))
        .for_task_execution(TaskExecutionNodeId::for_task_id(&task_id()))
        .into_surreal();
    assert!(sql.contains("A2A_TASK_CALL"));
    assert!(
        !sql.contains("A2A_MESSAGE_CALL"),
        "task-only query must not pull message-call arm: {sql}"
    );
}
