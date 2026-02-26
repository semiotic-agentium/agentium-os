//! Query-facing context metrics service contract and response DTOs.

use std::{error::Error, fmt};

use serde::Serialize;
use utoipa::ToSchema;

/// Errors from the context metrics service.
#[derive(Debug)]
pub enum ContextMetricsError {
    /// No metrics-relevant graph data found for the given context.
    NotFound,
    /// Service or store unavailable.
    Unavailable,
    /// Other error (e.g. storage/query failure).
    Other(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for ContextMetricsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContextMetricsError::NotFound => write!(f, "no metrics found for the given context"),
            ContextMetricsError::Unavailable => write!(f, "context metrics service unavailable"),
            ContextMetricsError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl Error for ContextMetricsError {}

/// Token totals for a turn or session.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenUsageDto {
    #[serde(rename = "in")]
    pub input: u64,
    #[serde(rename = "out")]
    pub output: u64,
    pub total: u64,
}

/// Turn-level metrics scoped by `(context_id, message_id)`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextTurnMetricsDto {
    pub message_id: String,
    pub user_prompt_count: u64,
    pub llm_call_count: u64,
    pub llm_duration_ms_total: u64,
    pub tokens: TokenUsageDto,
}

/// Session-level aggregate metrics for one context id.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextSessionMetricsDto {
    pub turns_total: u64,
    pub user_prompts_total: u64,
    pub llm_calls_total: u64,
    pub llm_duration_ms_total: u64,
    pub tokens_total: TokenUsageDto,
}

/// API response for `GET /context/{context_id}/metrics`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextMetricsResponseDto {
    pub context_id: String,
    pub turns: Vec<ContextTurnMetricsDto>,
    pub session: ContextSessionMetricsDto,
}

/// Service that computes context metrics from provenance data.
#[async_trait::async_trait]
pub trait ContextMetricsService: Send + Sync {
    /// Return turn-level and session-level metrics for a context.
    async fn metrics_for_context(
        &self,
        context_id: &str,
    ) -> Result<ContextMetricsResponseDto, ContextMetricsError>;
}
