use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use baml_rt_core::{
    BamlRtError, Result, callback_scheduling_scopes_differ_from_dispatch, clock_events,
    event_subscription::EventSourceKey,
    ids::{ContextId, ExternalId, TaskId},
    now_unix_ms,
};
use baml_rt_tools::{
    ToolBundle, ToolBundleMetadata, ToolHandler,
    tools::{
        ToolFunctionMetadata, ToolSessionContext, create_one_shot_tool_from_async_with_context,
    },
};
use tracing::debug;
use uuid::Uuid;

use crate::{
    callback_store::{
        CancelCallbackSelector, ScheduleCallbackRequest, StoredCallback, require_callback_store,
    },
    metadata::system_callback_metadata,
    tools::{
        CallbackCancelInput, CallbackCancelledOutput, CallbackContinuationMode,
        CallbackScheduleInput, CallbackScheduledOutput, CallbackToolInput, CallbackToolOutput,
    },
};

/// Monotonic suffix for [`ContextId`] minting on detached callback dispatch scope.
static CALLBACK_DISPATCH_CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(1);

fn mint_dispatch_context_id() -> ContextId {
    let millis = baml_rt_core::now_unix_ms(clock_events::CALLBACK_DISPATCH_CONTEXT);
    let counter = CALLBACK_DISPATCH_CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
    ContextId::new(millis, counter)
}

fn mint_dispatch_task_id() -> TaskId {
    TaskId::from_external(ExternalId::new(Uuid::new_v4().to_string()))
}

#[derive(Default)]
pub struct CallbackBundle;

impl CallbackBundle {
    pub fn new() -> Self {
        Self
    }
}

impl ToolBundle for CallbackBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = system_callback_metadata().bundle().clone();
        ToolBundleMetadata {
            name,
            description: "System callback scheduling tool.".to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        vec![callback_handler(system_callback_metadata())]
    }
}

pub fn callback_handler(metadata: ToolFunctionMetadata) -> Arc<dyn ToolHandler> {
    create_one_shot_tool_from_async_with_context::<(), CallbackToolInput, CallbackToolOutput, _, _>(
        metadata,
        move |session_ctx, input| async move {
            let store = require_callback_store()?;
            match input {
                CallbackToolInput::Schedule(schedule) => {
                    schedule_callback(store.as_ref(), &session_ctx, schedule).await
                }
                CallbackToolInput::Cancel(cancel) => cancel_callback(store.as_ref(), cancel).await,
            }
        },
    )
}

async fn schedule_callback(
    store: &dyn crate::callback_store::CallbackStore,
    session_ctx: &ToolSessionContext,
    input: CallbackScheduleInput,
) -> Result<CallbackToolOutput> {
    let requested_at_unix_ms = now_unix_ms(clock_events::SYSTEM_CALLBACK_SCHEDULE);
    let scheduled_for_unix_ms = requested_at_unix_ms.saturating_add(input.after_ms);
    let dedupe_key = normalize_optional_text(input.dedupe_key, "dedupeKey")?;
    let scheduling_task_id = session_ctx.task_id.clone().ok_or_else(|| {
        BamlRtError::InvalidArgument(
            "system/callback schedule requires an active task scope (scheduling deferral)"
                .to_string(),
        )
    })?;
    let scheduling_context_id = session_ctx.context_id.clone();

    let (context_id, task_id) = match input.continuation.unwrap_or_default() {
        CallbackContinuationMode::Detached => (
            Some(mint_dispatch_context_id()),
            Some(mint_dispatch_task_id()),
        ),
        CallbackContinuationMode::ResumeCurrentTask => {
            if dedupe_key.is_none() {
                return Err(BamlRtError::InvalidArgument(
                    "system/callback continuation=resume_current_task requires dedupeKey"
                        .to_string(),
                ));
            }
            (
                Some(scheduling_context_id.clone()),
                Some(scheduling_task_id.clone()),
            )
        }
    };
    let request = ScheduleCallbackRequest {
        source_key: normalize_source_key(&input.source_key)?,
        dedupe_key,
        payload: input.payload.into_inner(),
        scheduled_for_unix_ms,
        requested_at_unix_ms,
        context_id: context_id.clone(),
        task_id: task_id.clone(),
        scheduling_context_id: Some(scheduling_context_id.clone()),
        scheduling_task_id: Some(scheduling_task_id.clone()),
        requesting_agent_id: Some(session_ctx.agent_id.as_str().to_string()),
        requesting_message_id: None,
    };

    let scheduled = store.schedule_callback(request).await?;
    debug!(
        callback_id = %scheduled.callback.callback_id,
        source_key = %scheduled.callback.source_key,
        deduped = !scheduled.created,
        scheduled_for_unix_ms = scheduled.callback.scheduled_for_unix_ms,
        "system/callback scheduled callback"
    );
    Ok(CallbackToolOutput::Scheduled(scheduled_output(
        &scheduled.callback,
        !scheduled.created,
    )))
}

fn scheduled_output(callback: &StoredCallback, deduped: bool) -> CallbackScheduledOutput {
    let detached_dispatch = match (
        &callback.context_id,
        &callback.task_id,
        &callback.scheduling_context_id,
        &callback.scheduling_task_id,
    ) {
        (Some(dc), Some(dt), Some(sc), Some(st))
            if callback_scheduling_scopes_differ_from_dispatch(sc, st, dc, dt) =>
        {
            (Some(dc.as_str().to_string()), Some(dt.as_str().to_string()))
        }
        _ => (None, None),
    };
    CallbackScheduledOutput {
        callback_id: callback.callback_id.clone(),
        source_key: callback.source_key.clone(),
        scheduled_for_unix_ms: callback.scheduled_for_unix_ms,
        deduped,
        dispatch_context_id: detached_dispatch.0,
        dispatch_task_id: detached_dispatch.1,
        scheduling_context_id: callback
            .scheduling_context_id
            .as_ref()
            .map(|c| c.as_str().to_string()),
        scheduling_task_id: callback
            .scheduling_task_id
            .as_ref()
            .map(|t| t.as_str().to_string()),
    }
}

async fn cancel_callback(
    store: &dyn crate::callback_store::CallbackStore,
    input: CallbackCancelInput,
) -> Result<CallbackToolOutput> {
    let callback_id = normalize_optional_text(input.callback_id, "callbackId")?;
    let source_key = normalize_optional_text(input.source_key, "sourceKey")?;
    let dedupe_key = normalize_optional_text(input.dedupe_key, "dedupeKey")?;
    let selector = match (callback_id, source_key, dedupe_key) {
        (Some(callback_id), None, None) => CancelCallbackSelector::CallbackId(callback_id),
        (Some(_), Some(_), None) | (Some(_), None, Some(_)) | (Some(_), Some(_), Some(_)) => {
            return Err(BamlRtError::InvalidArgument(
                "system/callback cancel accepts either callbackId or sourceKey + dedupeKey, not both"
                    .to_string(),
            ));
        }
        (None, Some(_), None) => {
            return Err(BamlRtError::InvalidArgument(
                "system/callback cancel requires dedupeKey when sourceKey is provided".to_string(),
            ));
        }
        (None, None, Some(_)) => {
            return Err(BamlRtError::InvalidArgument(
                "system/callback cancel requires sourceKey when dedupeKey is provided".to_string(),
            ));
        }
        (None, Some(source_key), Some(dedupe_key)) => CancelCallbackSelector::DedupeKey {
            source_key: normalize_source_key(&source_key)?,
            dedupe_key,
        },
        (None, None, None) => {
            return Err(BamlRtError::InvalidArgument(
                "system/callback cancel requires callbackId or sourceKey + dedupeKey".to_string(),
            ));
        }
    };

    let cancelled = store.cancel_callback(selector).await?;
    debug!(
        callback_id = ?cancelled.as_ref().map(|callback| callback.callback_id.as_str()),
        source_key = ?cancelled.as_ref().map(|callback| callback.source_key.as_str()),
        cancelled = cancelled.is_some(),
        "system/callback cancel completed"
    );
    Ok(CallbackToolOutput::Cancelled(CallbackCancelledOutput {
        cancelled: cancelled.is_some(),
        callback_id: cancelled
            .as_ref()
            .map(|callback| callback.callback_id.clone()),
        source_key: cancelled
            .as_ref()
            .map(|callback| callback.source_key.clone()),
        dedupe_key: cancelled.and_then(|callback| callback.dedupe_key),
    }))
}

fn normalize_source_key(raw: &str) -> Result<String> {
    EventSourceKey::parse(raw)
        .map(|key| key.as_str().to_string())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "system/callback sourceKey '{raw}' is missing or has an invalid format"
            ))
        })
}

fn normalize_optional_text(raw: Option<String>, field_name: &str) -> Result<Option<String>> {
    raw.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(BamlRtError::InvalidArgument(format!(
                "system/callback {field_name} must not be an empty string"
            )))
        } else {
            Ok(trimmed.to_string())
        }
    })
    .transpose()
}
