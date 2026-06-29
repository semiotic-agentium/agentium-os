// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Drains QuickJS stream handover output: normalize, persist, forward, and emit settlement.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{
    bus::{ContextHistorySettlementKind, EffectEmitter},
    context::InvocationScope,
    ids::AgentId,
    stream_completion::StreamCompletion,
};
use baml_rt_observability::metrics;
use baml_rt_quickjs::a2a_stream::StreamOutput;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{a2a, a2a_types::StreamResponse, result_pipeline::ResultStoragePipeline};

/// Outbound chunk for chat stream consumers: wire chunk, index, optional terminal completion.
pub type ChatStreamChunk = (StreamResponse, usize, Option<StreamCompletion>);

fn normalized_to_stream_response(normalized: Value) -> StreamResponse {
    serde_json::from_value(normalized).unwrap_or_default()
}

fn task_id_from_chunk(value: &Value) -> Option<String> {
    value
        .get("task")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            value
                .get("statusUpdate")
                .and_then(|s| s.get("taskId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .or_else(|| {
            value
                .get("message")
                .and_then(|m| m.get("taskId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

fn make_submitted_chunk(context_id: &str, task_id: &str) -> StreamResponse {
    serde_json::from_value(serde_json::json!({
        "statusUpdate": {
            "status": { "state": "TASK_STATE_SUBMITTED" },
            "taskId": task_id,
            "contextId": context_id
        },
        "task": {
            "id": task_id,
            "contextId": context_id,
            "status": { "state": "TASK_STATE_SUBMITTED" }
        }
    }))
    .expect("submitted chunk static JSON deserializes to StreamResponse")
}

async fn emit_chat_stream_history_settled(
    emitter: &Arc<dyn EffectEmitter>,
    scope: &InvocationScope,
    agent_id: &AgentId,
    function_name: Option<String>,
) {
    emitter
        .emit_context_history_settled(
            scope.context_id().clone(),
            agent_id.clone(),
            ContextHistorySettlementKind::ChatStream,
            function_name,
        )
        .await;
}

/// Configuration for [`run_chat_stream_collector`].
pub struct ChatStreamCollectorConfig {
    pub scope: InvocationScope,
    pub agent_id: AgentId,
    pub pipeline: Arc<dyn ResultStoragePipeline>,
    pub effect_emitter: Arc<dyn EffectEmitter>,
    pub compaction_function_name: Option<String>,
    /// When true, inject TASK_STATE_SUBMITTED before the first agent chunk (message.sendStream).
    pub inject_submitted: bool,
}

/// Normalize handover chunks, persist via the result pipeline, forward to `tx`, and emit
/// `ContextHistorySettled` on stream terminal, error, or channel-closed without prior terminal.
///
/// Settlement on `InputRequired` is intentional: compaction evaluates the sealed prefix while
/// the live tail (including the in-progress turn) stays verbatim until resume completes.
pub async fn run_chat_stream_collector(
    mut chunk_rx: mpsc::Receiver<StreamOutput>,
    tx: mpsc::Sender<ChatStreamChunk>,
    config: ChatStreamCollectorConfig,
) {
    let ChatStreamCollectorConfig {
        scope,
        agent_id,
        pipeline,
        effect_emitter,
        compaction_function_name,
        inject_submitted,
    } = config;

    let router_pipeline_start = Instant::now();
    let mut first_stream_output = true;
    let mut normalizer = a2a::JsChunkNormalizer::new(&scope);
    let mut index = 0_usize;
    let mut submitted_sent = false;
    let mut history_settled = false;

    while let Some(output) = chunk_rx.recv().await {
        if first_stream_output {
            first_stream_output = false;
            let wait = router_pipeline_start.elapsed();
            metrics::record_live_stream_phase_duration("router_first_handover_output", wait);
            metrics::record_live_stream_event("router_first_js_output");
            tracing::debug!(
                context_id = %scope.context_id().as_str(),
                wait_ms = wait.as_millis(),
                "stream router: first output from QuickJS handover channel"
            );
        }
        let (raw_chunk, completion, is_relay) = match &output {
            StreamOutput::Chunk(v) => (v.clone(), None, false),
            StreamOutput::RelayChunk(v) => (v.clone(), None, true),
            StreamOutput::Terminal(v, c) => (v.clone(), Some(*c), false),
        };
        match normalizer.normalize_value(raw_chunk) {
            Ok(mut normalized) => {
                if is_relay && let Some(obj) = normalized.as_object_mut() {
                    obj.insert(
                        "__toolStreamChunk".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if inject_submitted && !submitted_sent {
                    let context_id_str = scope.context_id().as_str();
                    let task_id_opt = scope
                        .task_id_opt()
                        .map(|t| t.as_str().to_string())
                        .or_else(|| task_id_from_chunk(&normalized));
                    if let Some(ref task_id_str) = task_id_opt {
                        let submitted_chunk = make_submitted_chunk(context_id_str, task_id_str);
                        let submitted_wire =
                            serde_json::to_value(&submitted_chunk).unwrap_or(Value::Null);
                        if pipeline.store_result(&submitted_wire).await.is_ok()
                            && tx.send((submitted_chunk, 0, None)).await.is_ok()
                        {
                            submitted_sent = true;
                            index = 1;
                        }
                    }
                }
                if let Err(e) = pipeline.store_result(&normalized).await {
                    tracing::warn!(
                        error = %e,
                        "stream: store_result failed for chunk; still forwarding to client"
                    );
                }
                let sr = normalized_to_stream_response(normalized.clone());
                if tx.send((sr, index, completion)).await.is_err() {
                    break;
                }
                index += 1;
                if completion.is_some() {
                    emit_chat_stream_history_settled(
                        &effect_emitter,
                        &scope,
                        &agent_id,
                        compaction_function_name.clone(),
                    )
                    .await;
                    history_settled = true;
                }
            }
            Err(e) => {
                let err_normalized = serde_json::json!({"error": e.to_string()});
                if let Err(store_err) = pipeline.store_result(&err_normalized).await {
                    tracing::warn!(
                        error = %store_err,
                        "stream: store_result failed for error chunk"
                    );
                }
                let err_chunk = normalized_to_stream_response(err_normalized);
                if tx
                    .send((err_chunk, index, Some(StreamCompletion::SemanticFinal)))
                    .await
                    .is_err()
                {
                    tracing::debug!("stream error send failed (receiver dropped)");
                }
                emit_chat_stream_history_settled(
                    &effect_emitter,
                    &scope,
                    &agent_id,
                    compaction_function_name.clone(),
                )
                .await;
                history_settled = true;
                break;
            }
        }
        if completion.as_ref().is_some_and(|c| c.is_wire_final()) {
            break;
        }
    }

    if !tx.is_closed() {
        let channel_closed_sent = tx
            .send((
                StreamResponse::default(),
                index,
                Some(StreamCompletion::ChannelClosed),
            ))
            .await
            .is_ok();
        if channel_closed_sent && !history_settled {
            emit_chat_stream_history_settled(
                &effect_emitter,
                &scope,
                &agent_id,
                compaction_function_name,
            )
            .await;
        } else if !channel_closed_sent {
            tracing::debug!("stream channel-closed send failed (receiver dropped)");
        }
    }
}
