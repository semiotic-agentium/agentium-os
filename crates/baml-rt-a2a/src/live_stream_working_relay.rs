//! Pushes tool/status chunks into the **relay channel** so the collect path drains them in order (single stream).
//!
//! **HTTP A2A only** — for `message.sendStream` only. Chunks are pushed as raw `Value` to the
//! session's relay_tx; the collect path drains relay_rx each iteration and emits `RelayChunk`,
//! so delivery order matches causal order (no select! reordering).
//!
//! Session lookup is by [`LiveStreamSessionKey`](crate::live_stream::LiveStreamSessionKey) only.
//! The relay calls [`WorkingChunkPusher::push_relay_chunk`]; relay holds no session state (LS6).

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    BamlFunctionId,
    bus::{EffectEvent, EffectSubscriber},
    to_json_value,
};

use crate::{
    a2a_types::{StreamChunk, StreamChunkView},
    auto_status::{make_working_status_event, task_id_from_metadata, working_status_metadata_tool},
    live_stream::WorkingChunkPusher,
};

/// Key added to A2A stream chunk `result` when the chunk was relayed from the tool path.
/// Set by the transport when formatting a chunk that had `__toolStreamChunk` marker from the router.
pub const A2A_RESULT_TOOL_STREAM_CHUNK: &str = "toolStreamChunk";

/// Pushes raw chunks to the session's relay so the collect path emits them in order.
pub struct LiveStreamWorkingRelay {
    pusher: Arc<WorkingChunkPusher>,
}

impl LiveStreamWorkingRelay {
    pub fn new(pusher: Arc<WorkingChunkPusher>) -> Self {
        Self { pusher }
    }
}

#[async_trait]
impl EffectSubscriber for LiveStreamWorkingRelay {
    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        if let EffectEvent::ToolStreamChunk { context_id, chunk } = event {
            let view = StreamChunkView::new(chunk.clone());
            self.pusher
                .push_relay_chunk(context_id, view.task_id(), chunk.clone())
                .await;
            return Ok(());
        }

        let (context_id, task_id_opt, text, meta) = match event {
            EffectEvent::ToolStarted {
                context_id,
                metadata,
            } => (
                context_id.clone(),
                task_id_from_metadata(&metadata.metadata),
                format!("Invoking tool: {}", metadata.tool_name),
                Some(working_status_metadata_tool(&metadata.tool_name)),
            ),
            EffectEvent::LlmStarted {
                context_id,
                metadata,
            } => {
                let Some(task_id) = task_id_from_metadata(&metadata.metadata) else {
                    return Ok(());
                };
                (
                    context_id.clone(),
                    Some(task_id),
                    format!(
                        "Calling model: {} ({})",
                        metadata.model,
                        BamlFunctionId::parse(&metadata.function_name).prompt_name().as_str()
                    ),
                    None,
                )
            }
            _ => return Ok(()),
        };

        let status_ev = make_working_status_event(&context_id, task_id_opt.as_ref(), text, meta);
        let chunk_value = to_json_value(&StreamChunk::status_update(status_ev))
            .map_err(|e| baml_rt_core::BamlRtError::InvalidArgument(e.to_string()))?;
        self.pusher
            .push_relay_chunk(&context_id, task_id_opt.as_ref(), chunk_value)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use baml_rt_core::bus::{EffectEmitter, ToolEffectMetadata};
    use tokio::sync::{Mutex, mpsc};

    use super::*;
    use crate::live_stream::{LiveStreamSession, LiveStreamSessionKey};

    #[tokio::test]
    async fn relay_sends_working_chunk_when_session_exists() {
        let context_id = baml_rt_core::ids::ContextId::new(2, 1);
        let key = LiveStreamSessionKey::from_context_id(&context_id);
        let (relay_tx, mut relay_rx) = mpsc::channel(8);
        let (turn_tx, _) = async_channel::unbounded();
        let sessions: Arc<Mutex<HashMap<_, _>>> = Arc::new(Mutex::new(HashMap::from([(
            key,
            LiveStreamSession {
                turn_tx,
                relay_tx: Some(relay_tx),
                in_flight: false,
            },
        )])));
        let pusher = Arc::new(WorkingChunkPusher::new(sessions));
        let relay = Arc::new(LiveStreamWorkingRelay::new(pusher));
        let bus = Arc::new(baml_rt_core::bus::BusWithEffects::new());
        bus.subscribe_effect_subscriber(relay.clone()).await;

        let metadata = ToolEffectMetadata {
            tool_name: "support/calculate".to_string(),
            function_name: None,
            args: serde_json::json!({}),
            metadata: serde_json::json!({}),
            delegation_target: None,
        };
        bus.emit(EffectEvent::ToolStarted {
            context_id: context_id.clone(),
            metadata,
        })
        .await
        .expect("emit");

        let chunk = relay_rx.recv().await.expect("one relay chunk");
        let su = chunk.get("statusUpdate").expect("statusUpdate");
        let ev = su
            .get("statusUpdate")
            .or_else(|| su.get("status_update"))
            .unwrap_or(su);
        let state_str = ev
            .get("status")
            .and_then(|s| s.get("state"))
            .and_then(|v| v.as_str());
        assert_eq!(state_str, Some("TASK_STATE_WORKING"));
        let meta = ev.get("metadata").expect("metadata");
        assert_eq!(meta.get("kind").and_then(|v| v.as_str()), Some("tool"));
        assert_eq!(
            meta.get("toolName").and_then(|v| v.as_str()),
            Some("support/calculate")
        );
    }
}
