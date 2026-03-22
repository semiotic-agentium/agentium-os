//! Structured tool error classification for LLM-visible payloads and host retry policy.

use std::sync::Arc;

use baml_rt_core::{
    BamlRtError, baml_error_disposition, retryability_for_a2a,
    semantics::{ErrorDisposition, Retryability},
};

/// Arc-wrapped callback for custom [`ClassifiedToolError`] mapping in a tool session.
pub type ToolExecutionClassifier = Arc<dyn Fn(&BamlRtError) -> ClassifiedToolError + Send + Sync>;

/// LLM-safe, structured classification for a failed tool execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClassifiedToolError {
    /// Short machine-readable code (e.g. `rate_limited`, `invalid_input`).
    pub code: String,
    pub disposition: ErrorDisposition,
    /// Message safe to show to the model (no secrets).
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ClassifiedToolError {
    /// Default classification from a [`BamlRtError`] using core heuristics.
    pub fn from_baml_error(err: &BamlRtError) -> Self {
        let disposition = baml_error_disposition(err);
        let code = code_for_error(err, disposition);
        let message = public_message(err);
        Self {
            code,
            disposition,
            message,
            hint: None,
            retry_after_ms: None,
        }
    }

    /// Retry hint aligned with A2A JSON-RPC `retryable` for whole-request classification.
    #[must_use]
    pub fn host_retryability(&self) -> Retryability {
        match self.disposition {
            ErrorDisposition::HostRetriable => Retryability::Retryable,
            _ => Retryability::Permanent,
        }
    }

    /// Serialize for embedding in `ToolExecution` / BAML-visible JSON.
    #[must_use]
    pub fn to_tool_error_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "code": self.code,
                "disposition": self.disposition,
                "message": self.message,
            })
        })
    }
}

/// True when the host may retry this failure without a new LLM turn (bounded policy).
#[must_use]
pub fn should_host_retry(classified: &ClassifiedToolError) -> bool {
    matches!(classified.disposition, ErrorDisposition::HostRetriable)
}

/// Classify a [`BamlRtError`] for one-shot host retry (e.g. rate limits) without a new LLM turn.
#[must_use]
pub fn should_host_retry_baml_error(err: &BamlRtError) -> bool {
    matches!(baml_error_disposition(err), ErrorDisposition::HostRetriable)
}

fn public_message(err: &BamlRtError) -> String {
    err.to_string()
}

fn code_for_error(err: &BamlRtError, disposition: ErrorDisposition) -> String {
    match err {
        BamlRtError::InvalidArgument(_)
        | BamlRtError::InvalidArgumentWithSource { .. }
        | BamlRtError::InvalidOpenInput { .. }
        | BamlRtError::Json(_)
        | BamlRtError::JsonWithRaw { .. }
        | BamlRtError::FunctionNotFound(_)
        | BamlRtError::TypeConversion(_) => "invalid_argument".to_string(),
        BamlRtError::ToolExecution(_) => match disposition {
            ErrorDisposition::HostRetriable => "transient_tool_execution".to_string(),
            ErrorDisposition::LlmCorrectable => "tool_execution".to_string(),
            ErrorDisposition::InformAndContinue => "tool_execution".to_string(),
            ErrorDisposition::Fatal => "tool_execution".to_string(),
        },
        BamlRtError::ExecutionFailed { .. } | BamlRtError::ParsedResultFailed { .. } => {
            match disposition {
                ErrorDisposition::HostRetriable => "transient_execution".to_string(),
                ErrorDisposition::LlmCorrectable => "execution_failed".to_string(),
                ErrorDisposition::InformAndContinue => "execution_failed".to_string(),
                ErrorDisposition::Fatal => "execution_failed".to_string(),
            }
        }
        BamlRtError::Io(_) => "io_error".to_string(),
        BamlRtError::QuickJs(_) | BamlRtError::QuickJsWithSource { .. } => "quickjs".to_string(),
        BamlRtError::SessionLifecycle(_) => "session_lifecycle".to_string(),
        _ => "runtime_error".to_string(),
    }
}

/// Classify using an optional per-session classifier, else [`ClassifiedToolError::from_baml_error`].
#[must_use]
pub fn classify_for_session(
    classifier: &Option<ToolExecutionClassifier>,
    err: &BamlRtError,
) -> ClassifiedToolError {
    classifier
        .as_ref()
        .map(|c| c(err))
        .unwrap_or_else(|| ClassifiedToolError::from_baml_error(err))
}

/// Map a [`BamlRtError`] to A2A retryability (single policy entry point).
#[must_use]
pub fn a2a_retryability(err: &BamlRtError) -> Retryability {
    retryability_for_a2a(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_tool_execution_is_host_retriable() {
        let err = BamlRtError::ToolExecution("slack rate_limited, retry later".to_string());
        let c = ClassifiedToolError::from_baml_error(&err);
        assert_eq!(c.disposition, ErrorDisposition::HostRetriable);
        assert!(should_host_retry(&c));
        assert_eq!(c.host_retryability(), Retryability::Retryable);
    }

    #[test]
    fn unauthorized_tool_execution_is_inform_continue() {
        let err = BamlRtError::ToolExecution("HTTP 401 unauthorized".to_string());
        let c = ClassifiedToolError::from_baml_error(&err);
        assert_eq!(c.disposition, ErrorDisposition::InformAndContinue);
        assert!(!should_host_retry(&c));
    }

    #[test]
    fn invalid_argument_llm_correctable() {
        let err = BamlRtError::InvalidArgument("bad field".to_string());
        let c = ClassifiedToolError::from_baml_error(&err);
        assert_eq!(c.disposition, ErrorDisposition::LlmCorrectable);
        assert_eq!(c.code, "invalid_argument");
    }
}
