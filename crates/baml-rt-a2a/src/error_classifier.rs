use baml_rt_core::BamlRtError;

pub trait ErrorClassifier: Send + Sync {
    fn classify(&self, error: &BamlRtError) -> &'static str;
}

pub struct A2aErrorClassifier;

impl ErrorClassifier for A2aErrorClassifier {
    fn classify(&self, error: &BamlRtError) -> &'static str {
        match error {
            BamlRtError::InvalidArgument(_) => "invalid_argument",
            BamlRtError::FunctionNotFound(_) => "function_not_found",
            BamlRtError::QuickJs(_) => "quickjs",
            BamlRtError::Json(_) => "json",
            BamlRtError::ToolExecution(_) => "tool_execution",
            BamlRtError::ProvenanceContextRead { .. } => "provenance",
            _ => "internal",
        }
    }
}
