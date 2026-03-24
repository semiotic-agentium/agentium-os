use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::Result;
use serde::{Deserialize, Serialize};

use crate::tools::SessionReadMode;

/// Projection mode for addressed materials.
///
/// These names are runtime-wide and intentionally not provenance-specific.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialProjection {
    Identity,
    Summary,
    Detail,
}

impl MaterialProjection {
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, String> {
        match raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("summary") => Ok(Self::Summary),
            Some("identity") => Ok(Self::Identity),
            Some("detail") => Ok(Self::Detail),
            Some(other) => Err(format!("unsupported read.projection '{other}'")),
        }
    }
}

/// The material family behind a stable addressed ref.
///
/// This remains intentionally conservative for now. We can grow it as more
/// runtime-managed materials become first-class.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialKind {
    Text,
    Json,
    Image,
    Audio,
    Video,
    Pdf,
    Spreadsheet,
    Binary,
    Unknown,
}

/// Policy for whether a material may be projected into prompt text directly.
///
/// `PromptTextAllowed` means bounded textual projection is allowed. The other
/// variants make the contract explicit that a host-side transform or out-of-band
/// handling is required before the material can be shown to the model as text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialAdmissionPolicy {
    PromptTextAllowed,
    HostTransformRequired,
    OutOfBandOnly,
}

impl MaterialAdmissionPolicy {
    pub const fn for_kind(kind: MaterialKind) -> Self {
        match kind {
            MaterialKind::Text
            | MaterialKind::Json
            | MaterialKind::Image
            | MaterialKind::Audio
            | MaterialKind::Video => Self::PromptTextAllowed,
            MaterialKind::Pdf | MaterialKind::Spreadsheet => Self::HostTransformRequired,
            MaterialKind::Binary | MaterialKind::Unknown => Self::OutOfBandOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct MaterialRetrievalBudget {
    pub calls_used: u32,
    pub calls_cap: u32,
    pub bytes_used: u32,
    pub bytes_cap: u32,
    pub items_used: u32,
    pub items_cap: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct MaterialSummary {
    pub ref_id: String,
    pub material_kind: MaterialKind,
    pub admission_policy: MaterialAdmissionPolicy,
    pub item_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct MaterialRecord {
    pub ref_id: String,
    pub material_kind: MaterialKind,
    pub admission_policy: MaterialAdmissionPolicy,
    pub item_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_count: Option<u32>,
    /// Resolver-owned structured detail payload for `detail` projection.
    ///
    /// Keeping the envelope generic while leaving the record payload structured
    /// as JSON avoids baking provenance-specific schema into the runtime-wide
    /// addressed-material contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct MaterialReadResult {
    pub mode: SessionReadMode,
    pub ref_id: String,
    pub projection: MaterialProjection,
    pub refs: Vec<String>,
    pub material_kind: MaterialKind,
    pub admission_policy: MaterialAdmissionPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<MaterialSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<MaterialRecord>,
}

/// Internal resolved material representation used by runtime adapters.
#[derive(Debug, Clone)]
pub struct ResolvedMaterialRecord {
    pub ref_id: String,
    pub refs: Vec<String>,
    pub material_kind: MaterialKind,
    pub admission_policy: MaterialAdmissionPolicy,
    pub item_count: u32,
    pub source_types: Vec<String>,
    pub byte_count: Option<u32>,
    pub detail_json: Option<String>,
}

impl ResolvedMaterialRecord {
    pub fn summary(&self) -> MaterialSummary {
        MaterialSummary {
            ref_id: self.ref_id.clone(),
            material_kind: self.material_kind,
            admission_policy: self.admission_policy,
            item_count: self.item_count,
            source_types: self.source_types.clone(),
            byte_count: self.byte_count,
        }
    }

    pub fn record(&self) -> MaterialRecord {
        MaterialRecord {
            ref_id: self.ref_id.clone(),
            material_kind: self.material_kind,
            admission_policy: self.admission_policy,
            item_count: self.item_count,
            source_types: self.source_types.clone(),
            byte_count: self.byte_count,
            detail_json: self.detail_json.clone(),
        }
    }

    pub fn to_read_result(
        &self,
        mode: SessionReadMode,
        projection: MaterialProjection,
    ) -> MaterialReadResult {
        MaterialReadResult {
            mode,
            ref_id: self.ref_id.clone(),
            projection,
            refs: self.refs.clone(),
            material_kind: self.material_kind,
            admission_policy: self.admission_policy,
            summary: match projection {
                MaterialProjection::Identity => None,
                MaterialProjection::Summary | MaterialProjection::Detail => Some(self.summary()),
            },
            record: match projection {
                MaterialProjection::Detail => Some(self.record()),
                MaterialProjection::Identity | MaterialProjection::Summary => None,
            },
        }
    }
}

#[async_trait]
pub trait AddressedMaterialResolver: Send + Sync {
    async fn resolve_material_ref(
        &self,
        material_ref: &str,
    ) -> Result<Option<ResolvedMaterialRecord>>;
}

#[cfg(test)]
mod tests {
    use super::{MaterialAdmissionPolicy, MaterialKind, MaterialProjection};

    #[test]
    fn projection_defaults_to_summary() {
        assert_eq!(
            MaterialProjection::parse(None).expect("default projection"),
            MaterialProjection::Summary
        );
    }

    #[test]
    fn pdf_and_spreadsheet_require_host_transform() {
        assert_eq!(
            MaterialAdmissionPolicy::for_kind(MaterialKind::Pdf),
            MaterialAdmissionPolicy::HostTransformRequired
        );
        assert_eq!(
            MaterialAdmissionPolicy::for_kind(MaterialKind::Spreadsheet),
            MaterialAdmissionPolicy::HostTransformRequired
        );
    }

    #[test]
    fn binary_is_out_of_band_only() {
        assert_eq!(
            MaterialAdmissionPolicy::for_kind(MaterialKind::Binary),
            MaterialAdmissionPolicy::OutOfBandOnly
        );
    }
}
