//! Semantic edge labels for normalized PROV → Surreal writes (shared by batch planner).

use crate::{graph_model::GraphNodeLabel, vocabulary::semantic_labels};

pub(crate) fn semantic_used_label(from_label: &str, role: Option<&str>) -> &'static str {
    use crate::vocabulary::a2a_roles;
    match role {
        Some(r) if r == a2a_roles::INPUT_MESSAGE => match from_label {
            l if l == GraphNodeLabel::TaskExecution.as_str() => semantic_labels::WAS_SPAWNED_BY,
            l if l == GraphNodeLabel::MessageProcessing.as_str() => {
                semantic_labels::WAS_RECEIVED_BY
            }
            l if l == GraphNodeLabel::LlmCall.as_str() => semantic_labels::WAS_CONSUMED_BY,
            l if l == GraphNodeLabel::ToolCall.as_str() => semantic_labels::WAS_CONSUMED_BY,
            _ => semantic_labels::WAS_USED_BY,
        },
        Some(r) if r == a2a_roles::TASK_STATE => semantic_labels::WAS_UPDATED_BY,
        Some(r) if r == a2a_roles::PROMPT => semantic_labels::WAS_USED_BY,
        Some(r) if r == a2a_roles::ARGS => semantic_labels::WAS_USED_BY,
        Some(r) if r == a2a_roles::ARCHIVE => semantic_labels::WAS_BOOTSTRAPPED_BY,
        Some(r) if r == a2a_roles::REJECTED_OUTPUT => semantic_labels::WAS_USED_BY,
        Some(r) if r == a2a_roles::DELEGATION_TARGET => semantic_labels::WAS_DELEGATED_TO,
        Some(r) if r == a2a_roles::FAILURE_CLASSIFICATION || r == a2a_roles::FAILURE_EVIDENCE => {
            semantic_labels::WAS_USED_BY
        }
        _ => semantic_labels::WAS_USED_BY,
    }
}

pub(crate) fn semantic_generated_by_label(from_label: &str, to_label: &str) -> &'static str {
    match (from_label, to_label) {
        (f, t)
            if f == GraphNodeLabel::Message.as_str()
                && t == GraphNodeLabel::MessageProcessing.as_str() =>
        {
            semantic_labels::WAS_EMITTED_BY
        }
        (f, t)
            if f == GraphNodeLabel::Artifact.as_str()
                && t == GraphNodeLabel::TaskExecution.as_str() =>
        {
            semantic_labels::WAS_GENERATED_BY
        }
        (f, t)
            if f == GraphNodeLabel::Task.as_str()
                && t == GraphNodeLabel::TaskExecution.as_str() =>
        {
            semantic_labels::WAS_CREATED_BY
        }
        (f, t)
            if f == GraphNodeLabel::AgentRuntimeInstance.as_str()
                && t == GraphNodeLabel::AgentBoot.as_str() =>
        {
            semantic_labels::WAS_SPAWNED_BY
        }
        _ => semantic_labels::WAS_GENERATED_BY,
    }
}

pub(crate) fn semantic_associated_with_label(role: Option<&str>) -> &'static str {
    use crate::vocabulary::prov_roles;
    match role {
        Some(r) if r == prov_roles::EXECUTING_AGENT => semantic_labels::WAS_EXECUTED_BY,
        Some(r) if r == prov_roles::INVOKING_AGENT => semantic_labels::WAS_INVOKED_BY,
        Some(r) if r == prov_roles::CALLING_AGENT => semantic_labels::WAS_CALLED_BY,
        _ => crate::vocabulary::prov_relations::WAS_ASSOCIATED_WITH,
    }
}

pub(crate) fn semantic_derived_from_label(prov_type: Option<&str>) -> &'static str {
    use crate::vocabulary::{a2a_relation_types, a2a_relations};
    match prov_type {
        Some(t) if t == a2a_relation_types::STATUS_TRANSITION => {
            semantic_labels::WAS_TRANSITIONED_FROM
        }
        Some(t) if t == a2a_relations::INFORMED_BY_OBSERVATION => semantic_labels::WAS_INFORMED_BY,
        _ => crate::vocabulary::prov_relations::WAS_DERIVED_FROM,
    }
}
