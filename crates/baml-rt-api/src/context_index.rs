//! Context picker index service contract and API DTOs.

use std::{error::Error, fmt};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Service errors for context-index reads.
pub type ContextIndexError = crate::service_error::ServiceError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextIndexCursorToken(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ContextIndexCursorStateV1 {
    v: u8,
    offset: usize,
    agent_package: Option<String>,
}

impl ContextIndexCursorToken {
    pub fn encode_v1(offset: usize, agent_package: Option<&str>) -> Self {
        let state = ContextIndexCursorStateV1 {
            v: 1,
            offset,
            agent_package: agent_package.map(str::to_string),
        };
        let bytes = serde_json::to_vec(&state).unwrap_or_default();
        Self(format!("v1.{:x}", HexBytes(bytes)))
    }

    fn decode_v1(&self) -> Result<ContextIndexCursorStateV1, ContextIndexRequestParseError> {
        let payload = self.0.strip_prefix("v1.").ok_or_else(|| {
            ContextIndexRequestParseError::InvalidCursor("missing v1 prefix".to_string())
        })?;
        let bytes = decode_hex(payload)
            .map_err(|e| ContextIndexRequestParseError::InvalidCursor(e.to_string()))?;
        let state: ContextIndexCursorStateV1 = serde_json::from_slice(&bytes)
            .map_err(|e| ContextIndexRequestParseError::InvalidCursor(e.to_string()))?;
        if state.v != 1 {
            return Err(ContextIndexRequestParseError::UnknownCursorVersion(
                state.v as u32,
            ));
        }
        Ok(state)
    }
}

struct HexBytes(Vec<u8>);
impl fmt::LowerHex for HexBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    if !input.len().is_multiple_of(2) {
        return Err("hex payload must have even length".into());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex digit".to_string())?;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex digit".to_string())?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextIndexQueryParams {
    pub agent_package: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextIndexRequest {
    pub agent_package: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug)]
pub enum ContextIndexRequestParseError {
    InvalidLimit(u32),
    InvalidCursor(String),
    UnknownCursorVersion(u32),
    CursorScopeMismatch,
}

impl fmt::Display for ContextIndexRequestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit(v) => write!(f, "limit must be in range [1, 200], got {v}"),
            Self::InvalidCursor(e) => write!(f, "invalid cursor: {e}"),
            Self::UnknownCursorVersion(v) => write!(f, "unknown cursor version {v}"),
            Self::CursorScopeMismatch => {
                write!(
                    f,
                    "cursor does not match agentPackage scope for this request"
                )
            }
        }
    }
}

impl Error for ContextIndexRequestParseError {}

impl ContextIndexRequest {
    pub fn from_query(
        params: ContextIndexQueryParams,
    ) -> Result<Self, ContextIndexRequestParseError> {
        let raw_limit = params.limit.unwrap_or(50);
        if !(1..=200).contains(&raw_limit) {
            return Err(ContextIndexRequestParseError::InvalidLimit(raw_limit));
        }
        let agent_package = params
            .agent_package
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let offset = match params.cursor {
            None => 0,
            Some(raw) => {
                let cursor = ContextIndexCursorToken(raw);
                let state = cursor.decode_v1()?;
                if state.agent_package.as_deref() != agent_package.as_deref() {
                    return Err(ContextIndexRequestParseError::CursorScopeMismatch);
                }
                state.offset
            }
        };

        Ok(Self {
            agent_package,
            limit: raw_limit as usize,
            offset,
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextPickerItemDto {
    pub context_id: String,
    pub latest_timestamp_ms: u64,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextPickerPageDto {
    pub items: Vec<ContextPickerItemDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait ContextIndexService: Send + Sync {
    async fn page(
        &self,
        request: &ContextIndexRequest,
    ) -> Result<ContextPickerPageDto, ContextIndexError>;
}
