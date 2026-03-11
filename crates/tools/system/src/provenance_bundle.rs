use std::sync::Arc;

use baml_rt_provenance::ProvenanceOpsQuery;
use baml_rt_tools::{ToolBundle, ToolBundleMetadata};

use crate::provenance_session_tools::{extrospection_handler, introspection_handler};

pub struct ProvenanceBundle {
    query: Arc<dyn ProvenanceOpsQuery>,
}

impl ProvenanceBundle {
    pub fn new(query: Arc<dyn ProvenanceOpsQuery>) -> Self {
        Self { query }
    }
}

impl ToolBundle for ProvenanceBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = baml_rt_tools::BundleName::new("system".to_string())
            .expect("system bundle name is valid");
        ToolBundleMetadata {
            name,
            description: "System provenance query tools (introspection, extrospection)."
                .to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn baml_rt_tools::ToolHandler>> {
        vec![
            introspection_handler(self.query.clone()),
            extrospection_handler(self.query.clone()),
        ]
    }
}
