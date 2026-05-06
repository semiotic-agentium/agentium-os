//! Context metrics service.

use std::{collections::HashMap, sync::Arc};

use baml_rt_provenance::context_metrics_queries;
use serde_json::Value;

pub(crate) struct ContextMetricsServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ContextMetricsServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

pub(crate) fn value_as_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().map(|v| v.max(0) as u64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn value_as_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

#[async_trait::async_trait]
impl baml_rt_api::ContextMetricsService for ContextMetricsServiceImpl {
    async fn metrics_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<baml_rt_api::ContextMetricsResponseDto, baml_rt_api::ContextMetricsError>
    {
        let turn_rows = context_metrics_queries::turn_totals_by_context(&self.store, context_id)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
            })?;
        let session_rows =
            context_metrics_queries::session_totals_by_context(&self.store, context_id)
                .await
                .map_err(|e| {
                    baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                        e.to_string(),
                    )))
                })?;
        let prompt_rows = context_metrics_queries::user_prompts_by_context(&self.store, context_id)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
            })?;

        let session_prompt_tail =
            context_metrics_queries::session_prompt_context_tail(&self.store, context_id, None)
                .await
                .map_err(|e| {
                    baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                        e.to_string(),
                    )))
                })?;
        let session_prompt_bytes_current = value_as_u64(
            session_prompt_tail
                .as_ref()
                .and_then(|r| r.get("prompt_context_bytes_current")),
        );

        let mut prompt_count_by_message: HashMap<String, u64> = HashMap::new();
        for row in prompt_rows {
            let message_id = value_as_string(row.get("message_id"));
            if message_id.is_empty() {
                continue;
            }
            prompt_count_by_message.insert(message_id, value_as_u64(row.get("user_prompt_count")));
        }

        let mut turns = Vec::with_capacity(turn_rows.len());
        for row in turn_rows {
            let message_id = value_as_string(row.get("message_id"));
            if message_id.is_empty() {
                continue;
            }
            let user_prompt_count = prompt_count_by_message.remove(&message_id).unwrap_or(0);
            turns.push(baml_rt_api::ContextTurnMetricsDto {
                message_id,
                user_prompt_count,
                llm_call_count: value_as_u64(row.get("llm_call_count")),
                llm_duration_ms_total: value_as_u64(row.get("llm_duration_ms_total")),
                prompt_context_bytes_current: value_as_u64(row.get("prompt_context_bytes_current")),
                tokens: baml_rt_api::TokenUsageDto {
                    input: value_as_u64(row.get("tokens_in")),
                    output: value_as_u64(row.get("tokens_out")),
                    total: value_as_u64(row.get("tokens_total")),
                },
            });
        }

        let mut prompt_only: Vec<(String, u64)> = prompt_count_by_message.into_iter().collect();
        prompt_only.sort_by(|(l, _), (r, _)| l.cmp(r));
        for (message_id, user_prompt_count) in prompt_only {
            turns.push(baml_rt_api::ContextTurnMetricsDto {
                message_id,
                user_prompt_count,
                llm_call_count: 0,
                llm_duration_ms_total: 0,
                prompt_context_bytes_current: 0,
                tokens: baml_rt_api::TokenUsageDto {
                    input: 0,
                    output: 0,
                    total: 0,
                },
            });
        }

        let session = session_rows.first();
        let session_tokens_in = value_as_u64(session.and_then(|r| r.get("tokens_in")));
        let session_tokens_out = value_as_u64(session.and_then(|r| r.get("tokens_out")));
        let session_tokens_total = value_as_u64(session.and_then(|r| r.get("tokens_total")));
        let session_llm_calls = value_as_u64(session.and_then(|r| r.get("llm_call_count")));
        let session_llm_duration_ms =
            value_as_u64(session.and_then(|r| r.get("llm_duration_ms_total")));
        let user_prompts_total = turns.iter().map(|t| t.user_prompt_count).sum();
        let turns_total = turns.len() as u64;

        Ok(baml_rt_api::ContextMetricsResponseDto {
            context_id: context_id.to_string(),
            turns,
            session: baml_rt_api::ContextSessionMetricsDto {
                turns_total,
                user_prompts_total,
                llm_calls_total: session_llm_calls,
                llm_duration_ms_total: session_llm_duration_ms,
                prompt_context_bytes_current: session_prompt_bytes_current,
                tokens_total: baml_rt_api::TokenUsageDto {
                    input: session_tokens_in,
                    output: session_tokens_out,
                    total: session_tokens_total,
                },
            },
        })
    }
}
