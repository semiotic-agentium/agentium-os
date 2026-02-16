use crate::a2a;
use crate::a2a_types::JSONRPCId;
use crate::error_mapping;
use baml_rt_core::BamlRtError;
use baml_rt_core::StreamResult;
use serde_json::Value;

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
