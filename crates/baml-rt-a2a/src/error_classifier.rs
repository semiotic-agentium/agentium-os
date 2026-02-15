use baml_rt_core::BamlRtError;

use crate::error_mapping;

pub trait ErrorClassifier: Send + Sync {
    fn classify(&self, error: &BamlRtError) -> &'static str;
}

pub struct A2aErrorClassifier;

impl ErrorClassifier for A2aErrorClassifier {
    fn classify(&self, error: &BamlRtError) -> &'static str {
        error_mapping::map_error(error).classifier
    }
}
