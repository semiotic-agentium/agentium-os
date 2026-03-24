use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    AddressedMaterialResolver, MaterialAdmissionPolicy, MaterialKind, ResolvedMaterialRecord,
};

use crate::{ProvenanceArchivePayload, ProvenanceArchiveRecord, ProvenanceOpsQuery};

#[derive(Clone)]
pub struct ProvenanceAddressedMaterialResolver {
    query: Arc<dyn ProvenanceOpsQuery>,
}

impl ProvenanceAddressedMaterialResolver {
    pub fn new(query: Arc<dyn ProvenanceOpsQuery>) -> Self {
        Self { query }
    }
}

fn payload_source_name(payload: &ProvenanceArchivePayload) -> &'static str {
    match payload {
        ProvenanceArchivePayload::LlmCall { .. } => "llm_call",
        ProvenanceArchivePayload::LlmResult { .. } => "llm_result",
        ProvenanceArchivePayload::ToolCall { .. } => "tool_call",
        ProvenanceArchivePayload::ToolResult { .. } => "tool_result",
    }
}

fn collect_refs_with_prefix(prefix_ref: &str, record: &ProvenanceArchiveRecord) -> Vec<String> {
    let mut refs = vec![prefix_ref.to_string(), record.archive_ref.0.clone()];
    for payload in &record.payloads {
        match payload {
            ProvenanceArchivePayload::LlmCall {
                payload_ref,
                activity_ref,
                ..
            }
            | ProvenanceArchivePayload::LlmResult {
                payload_ref,
                activity_ref,
                ..
            }
            | ProvenanceArchivePayload::ToolResult {
                payload_ref,
                activity_ref,
                ..
            } => {
                refs.push(payload_ref.0.clone());
                refs.push(activity_ref.0.clone());
            }
            ProvenanceArchivePayload::ToolCall {
                payload_ref,
                activity_ref,
                ..
            } => {
                refs.push(payload_ref.0.clone());
                refs.push(activity_ref.0.clone());
            }
        }
    }
    refs.sort();
    refs.dedup();
    if refs.first().map(|value| value.as_str()) != Some(prefix_ref) {
        refs.retain(|value| value != prefix_ref);
        refs.insert(0, prefix_ref.to_string());
    }
    refs
}

fn record_to_material(
    requested_ref: &str,
    record: ProvenanceArchiveRecord,
) -> Result<ResolvedMaterialRecord> {
    let byte_count = serde_json::to_vec(&record)
        .map(|buf| u32::try_from(buf.len()).unwrap_or(u32::MAX))
        .map_err(BamlRtError::Json)?;
    let source_types = record
        .payloads
        .iter()
        .map(payload_source_name)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let item_count = u32::try_from(record.payloads.len()).unwrap_or(u32::MAX);
    let refs = collect_refs_with_prefix(requested_ref, &record);
    let detail_json = serde_json::to_string(&record).map_err(BamlRtError::Json)?;

    Ok(ResolvedMaterialRecord {
        ref_id: requested_ref.to_string(),
        refs,
        material_kind: MaterialKind::Json,
        admission_policy: MaterialAdmissionPolicy::for_kind(MaterialKind::Json),
        item_count,
        source_types,
        byte_count: Some(byte_count),
        detail_json: Some(detail_json),
    })
}

#[async_trait]
impl AddressedMaterialResolver for ProvenanceAddressedMaterialResolver {
    async fn resolve_material_ref(
        &self,
        material_ref: &str,
    ) -> Result<Option<ResolvedMaterialRecord>> {
        let Some(record) = self
            .query
            .resolve_archive_ref(material_ref)
            .await
            .map_err(|error| BamlRtError::InvalidArgument(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(record_to_material(material_ref, record)?))
    }
}
