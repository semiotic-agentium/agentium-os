//! Conversation-history UI resume hints derived from the provenance
//! graph.
//!
//! Provenance conversation rows do not encode `TASK_STATE_INPUT_REQUIRED`;
//! the `A2ATaskState` head pointer (Phase A0/A5) names the most-recent
//! task status. The runner merges these hints into the conversation
//! history page DTO so clients can restore the awaiting-input
//! affordance after a full snapshot reload.
//!
//! After the A2A relational shadow excision, this module reads
//! exclusively through the typed [`TaskGraphReader`] surface — no
//! `a2a_task` / `a2a_message` involvement.

use baml_rt_core::ids::{ContextId, ExternalId, TaskId};

use crate::{
    error::Result, metamodel::TaskStatusKind, surreal_store::SurrealProvenanceStore,
    task_graph_reader::TaskGraphReader,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationResumeUiHints {
    /// Task id to echo when the HTTP request omitted `task_id` (latest
    /// task for the context).
    pub effective_task_id: Option<String>,
    pub awaiting_input: bool,
    pub input_required_prompt: Option<String>,
}

/// Resolve resume hints. Picks the requested task id when supplied;
/// otherwise the most recent task in the context per
/// [`TaskGraphReader::latest_in_context`]. The awaiting-input flag and
/// the prompt are derived from the head [`TaskStatusKind`] node on the
/// graph (Phase A5 head-pointer doctrine).
pub async fn resolve_resume_ui_hints(
    store: &SurrealProvenanceStore,
    context_id: &str,
    request_task_id: Option<&str>,
) -> Result<ConversationResumeUiHints> {
    let ctx = ContextId::parse_temporal(context_id).unwrap_or_else(|| ContextId::from(context_id));

    // Resolve the effective task: explicit request id wins when it
    // refers to a task that is actually scoped to this context;
    // otherwise the latest task in the context.
    let scoped = match request_task_id {
        Some(t) if !t.is_empty() => {
            let tid = TaskId::from_external(ExternalId::new(t));
            store.resolve_scoped(&ctx, &tid).await?
        }
        _ => store.latest_in_context(&ctx).await?,
    };

    let Some(scoped) = scoped else {
        return Ok(ConversationResumeUiHints::default());
    };

    // Strip canonical "task:" prefix for the wire representation.
    let effective_task_id_wire = scoped
        .task_node_id()
        .strip_prefix("task:")
        .unwrap_or(scoped.task_node_id())
        .to_string();

    let Some(state) = store.latest_state(scoped.clone()).await? else {
        return Ok(ConversationResumeUiHints {
            effective_task_id: Some(effective_task_id_wire),
            ..Default::default()
        });
    };

    match state.new_status {
        TaskStatusKind::InputRequired { prompt } => Ok(ConversationResumeUiHints {
            effective_task_id: Some(effective_task_id_wire),
            awaiting_input: true,
            input_required_prompt: Some(prompt),
        }),
        _ => Ok(ConversationResumeUiHints {
            effective_task_id: Some(effective_task_id_wire),
            ..Default::default()
        }),
    }
}
