// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host dispatch ingress: stable unit scopes and `withTask` prelude contract.

use serde_json::Value;

use crate::{
    context::RuntimeScope,
    error::{BamlRtError, Result},
    host_poll_lineage::stable_external_id,
    host_source_records_body::{IngressPollBody, format_source_records_unit_body},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId},
};

/// Agent-supplied stable key for one `withTask` work unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchUnitKey(pub String);

/// Non-empty record slice passed into `withTask`; host formats into the unit `user` line.
#[derive(Debug, Clone)]
pub struct DispatchWorkUnit {
    pub unit_key: DispatchUnitKey,
    pub records: Vec<Value>,
}

impl DispatchWorkUnit {
    pub fn new(unit_key: impl Into<String>, records: Vec<Value>) -> Result<Self> {
        let unit_key = DispatchUnitKey(unit_key.into());
        if unit_key.0.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "withTask unitKey must be non-empty".into(),
            ));
        }
        if records.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "withTask records must be non-empty".into(),
            ));
        }
        Ok(Self { unit_key, records })
    }
}

/// Forked task scope after `with_task_prelude` (unit `#1` already written).
#[derive(Debug, Clone)]
pub struct WithTaskPrelude {
    pub unit_key: String,
    pub scope: RuntimeScope,
    /// Always `1` for the host-written unit user line in task-scoped reads.
    pub unit_history_ref: u32,
}

/// Stable task id for a dispatch unit under a poll context.
#[must_use]
pub fn dispatch_unit_task_id(context_id: &ContextId, unit_key: &str) -> TaskId {
    TaskId::from_external(ExternalId::new(stable_external_id(
        "dispatch-unit",
        &[context_id.as_str(), unit_key],
    )))
}

/// Stable message id for a dispatch unit under a poll context.
#[must_use]
pub fn dispatch_unit_message_id(context_id: &ContextId, unit_key: &str) -> MessageId {
    MessageId::from(stable_external_id(
        "dispatch-unit-msg",
        &[context_id.as_str(), unit_key],
    ))
}

/// Build task scope for a dispatch unit (idempotent per `(context_id, unit_key)`).
#[must_use]
pub fn dispatch_unit_runtime_scope(
    context_id: ContextId,
    agent_id: AgentId,
    unit_key: &str,
) -> RuntimeScope {
    let message_id = dispatch_unit_message_id(&context_id, unit_key);
    let task_id = dispatch_unit_task_id(&context_id, unit_key);
    RuntimeScope::task_scope(context_id, agent_id, message_id, task_id)
}

/// Format a unit slice into an actionable user line.
#[must_use]
pub fn format_unit_ingress_body(unit: &DispatchWorkUnit) -> IngressPollBody {
    format_source_records_unit_body(&unit.records)
}
