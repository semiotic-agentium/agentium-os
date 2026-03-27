use std::sync::Arc;

use baml_rt_core::{
    BamlRtError, Result,
    event_subscription::EventSourceKey,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_tools::{
    ToolBundle, ToolBundleMetadata, ToolHandler,
    tools::{
        ToolFunctionMetadata, ToolSessionContext, create_one_shot_tool_from_async_with_context,
    },
};
use tracing::debug;

use crate::{
    callback_store::{CancelCallbackSelector, ScheduleCallbackRequest, require_callback_store},
    callback_time::callback_now_unix_ms,
    metadata::system_callback_metadata,
    tools::{
        CallbackCancelInput, CallbackCancelledOutput, CallbackScheduleInput,
        CallbackScheduledOutput, CallbackToolInput, CallbackToolOutput,
    },
};

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
    create_one_shot_tool_from_async_with_context::<(), CallbackToolInput, CallbackToolOutput, _>(
        metadata,
        move |session_ctx, input| {
            Box::pin(async move {
                let store = require_callback_store()?;
                match input {
                    CallbackToolInput::Schedule(schedule) => {
                        schedule_callback(store.as_ref(), &session_ctx, schedule).await
                    }
                    CallbackToolInput::Cancel(cancel) => {
                        cancel_callback(store.as_ref(), cancel).await
                    }
                }
            })
        },
    )
}

async fn schedule_callback(
    store: &dyn crate::callback_store::CallbackStore,
    session_ctx: &ToolSessionContext,
    input: CallbackScheduleInput,
) -> Result<CallbackToolOutput> {
    let requested_at_unix_ms = callback_now_unix_ms("system_callback_schedule");
    let scheduled_for_unix_ms = requested_at_unix_ms.saturating_add(input.after_ms);
    let request = ScheduleCallbackRequest {
        source_key: normalize_source_key(&input.source_key)?,
        dedupe_key: normalize_optional_text(input.dedupe_key)?,
        payload: input.payload,
        scheduled_for_unix_ms,
        requested_at_unix_ms,
        context_id: parse_context_id(input.context_id)?
            .or_else(|| Some(session_ctx.context_id.clone())),
        task_id: parse_task_id(input.task_id)?.or_else(|| session_ctx.task_id.clone()),
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
    Ok(CallbackToolOutput::Scheduled(CallbackScheduledOutput {
        callback_id: scheduled.callback.callback_id,
        source_key: scheduled.callback.source_key,
        scheduled_for_unix_ms: scheduled.callback.scheduled_for_unix_ms,
        deduped: !scheduled.created,
    }))
}

async fn cancel_callback(
    store: &dyn crate::callback_store::CallbackStore,
    input: CallbackCancelInput,
) -> Result<CallbackToolOutput> {
    let callback_id = normalize_optional_text(input.callback_id)?;
    let source_key = normalize_optional_text(input.source_key)?;
    let dedupe_key = normalize_optional_text(input.dedupe_key)?;
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
            BamlRtError::InvalidArgument("system/callback sourceKey is required".to_string())
        })
}

fn normalize_optional_text(raw: Option<String>) -> Result<Option<String>> {
    raw.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(BamlRtError::InvalidArgument(
                "system/callback fields must not be empty strings".to_string(),
            ))
        } else {
            Ok(trimmed.to_string())
        }
    })
    .transpose()
}

fn parse_context_id(raw: Option<String>) -> Result<Option<ContextId>> {
    normalize_optional_text(raw).map(|value| value.map(|raw| ContextId::from(raw.as_str())))
}

fn parse_task_id(raw: Option<String>) -> Result<Option<TaskId>> {
    normalize_optional_text(raw)
        .map(|value| value.map(|raw| TaskId::from_external(ExternalId::new(raw))))
}
