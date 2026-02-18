use crate::a2a;
use crate::a2a_types::JSONRPCId;
use crate::error_mapping;
use baml_rt_core::BamlRtError;
use baml_rt_core::StreamResult;
use baml_rt_core::stream_completion::StreamCompletion;
use serde_json::{Value, json};

pub trait ResponseFormatter: Send + Sync {
    fn format_success(&self, id: Option<JSONRPCId>, result: Value) -> Value;
    fn format_stream(&self, id: Option<JSONRPCId>, result: &StreamResult) -> Vec<Value>;
    fn format_error(&self, id: Option<JSONRPCId>, error: &BamlRtError) -> Value;
}

pub struct JsonRpcResponseFormatter;

impl ResponseFormatter for JsonRpcResponseFormatter {
    fn format_success(&self, id: Option<JSONRPCId>, result: Value) -> Value {
        a2a::success_response(id, result)
    }

    fn format_stream(&self, id: Option<JSONRPCId>, result: &StreamResult) -> Vec<Value> {
        let total = result.chunks.len();
        if total == 0
            && let Some(chunk) = synthetic_terminal_chunk_for_empty_stream(result.completion)
        {
            return vec![a2a::stream_chunk_response(id, chunk, 0, true)];
        }
        let mark_last_final = result.is_semantically_final();
        let mut responses = Vec::with_capacity(total);
        for (idx, chunk) in result.chunks.iter().cloned().enumerate() {
            responses.push(a2a::stream_chunk_response(
                id.clone(),
                chunk,
                idx,
                mark_last_final && idx + 1 == total,
            ));
        }
        responses
    }

    fn format_error(&self, id: Option<JSONRPCId>, error: &BamlRtError) -> Value {
        let m = error_mapping::map_error(error);
        a2a::error_response(id, m.code, m.message, m.data)
    }
}

fn synthetic_terminal_chunk_for_empty_stream(completion: StreamCompletion) -> Option<Value> {
    match completion {
        StreamCompletion::Timeout => Some(json!({
            "task": {
                "status": {
                    "state": "TASK_STATE_FAILED",
                    "message": {
                        "parts": [
                            {"text": "Request timed out before the agent produced a terminal response."}
                        ]
                    }
                }
            }
        })),
        StreamCompletion::ChannelClosed => Some(json!({
            "task": {
                "status": {
                    "state": "TASK_STATE_FAILED",
                    "message": {
                        "parts": [
                            {"text": "Stream closed before the agent produced a terminal response."}
                        ]
                    }
                }
            }
        })),
        StreamCompletion::SemanticFinal | StreamCompletion::InputRequired => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baml_rt_core::stream_completion::{StreamCompletion, StreamResult};

    #[test]
    fn format_stream_emits_terminal_chunk_on_timeout_when_empty() {
        let formatter = JsonRpcResponseFormatter;
        let result = StreamResult {
            chunks: vec![],
            completion: StreamCompletion::Timeout,
        };

        let out = formatter.format_stream(None, &result);
        assert_eq!(
            out.len(),
            1,
            "timeout with empty chunks should emit one terminal chunk"
        );
        assert_eq!(
            out[0]
                .get("result")
                .and_then(|r| r.get("final"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            out[0]
                .get("result")
                .and_then(|r| r.get("chunk"))
                .and_then(|c| c.get("task"))
                .and_then(|t| t.get("status"))
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str),
            Some("TASK_STATE_FAILED")
        );
    }

    #[test]
    fn format_stream_emits_terminal_chunk_on_channel_closed_when_empty() {
        let formatter = JsonRpcResponseFormatter;
        let result = StreamResult {
            chunks: vec![],
            completion: StreamCompletion::ChannelClosed,
        };

        let out = formatter.format_stream(None, &result);
        assert_eq!(
            out.len(),
            1,
            "channel closed with empty chunks should emit one terminal chunk"
        );
        assert_eq!(
            out[0]
                .get("result")
                .and_then(|r| r.get("final"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            out[0]
                .get("result")
                .and_then(|r| r.get("chunk"))
                .and_then(|c| c.get("task"))
                .and_then(|t| t.get("status"))
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str),
            Some("TASK_STATE_FAILED")
        );
    }
}
