// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Context picker index service contract and API DTOs.

use std::{error::Error, fmt};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Service errors for context-index reads.
pub type ContextIndexError = crate::service_error::ServiceError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextIndexCursorToken(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPickerIngressFilter {
    All,
    EventOnly,
    ChatOnly,
}

impl ContextPickerIngressFilter {
    pub fn from_flags(
        event_only: bool,
        chat_only: bool,
    ) -> Result<Self, ContextIndexRequestParseError> {
        match (event_only, chat_only) {
            (true, true) => Err(ContextIndexRequestParseError::ConflictingIngressFilters),
            (true, false) => Ok(Self::EventOnly),
            (false, true) => Ok(Self::ChatOnly),
            (false, false) => Ok(Self::All),
        }
    }

    pub fn event_only(self) -> bool {
        matches!(self, Self::EventOnly)
    }

    pub fn chat_only(self) -> bool {
        matches!(self, Self::ChatOnly)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ContextIndexCursorStateV1 {
    v: u8,
    offset: usize,
    agent_package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_only: Option<bool>,
}

impl ContextIndexCursorToken {
    pub fn encode_v1(
        offset: usize,
        agent_package: Option<&str>,
        ingress_filter: ContextPickerIngressFilter,
    ) -> Self {
        let state = ContextIndexCursorStateV1 {
            v: 1,
            offset,
            agent_package: agent_package.map(str::to_string),
            event_only: match ingress_filter {
                ContextPickerIngressFilter::EventOnly => Some(true),
                _ => None,
            },
            chat_only: match ingress_filter {
                ContextPickerIngressFilter::ChatOnly => Some(true),
                _ => None,
            },
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
    /// When true, only contexts with host ingress transcript rows (event dispatch runs).
    pub event_only: Option<bool>,
    /// When true, only conversational contexts without host ingress rows (Chat picker).
    pub chat_only: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ContextIndexRequest {
    pub agent_package: Option<String>,
    pub limit: usize,
    pub offset: usize,
    pub ingress_filter: ContextPickerIngressFilter,
}

#[derive(Debug)]
pub enum ContextIndexRequestParseError {
    InvalidLimit(u32),
    InvalidCursor(String),
    UnknownCursorVersion(u32),
    CursorScopeMismatch,
    ConflictingIngressFilters,
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
                    "cursor does not match agentPackage or ingress filter scope for this request"
                )
            }
            Self::ConflictingIngressFilters => {
                write!(f, "eventOnly and chatOnly are mutually exclusive")
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
                let ingress_filter = ContextPickerIngressFilter::from_flags(
                    params.event_only.filter(|&v| v).is_some(),
                    params.chat_only.filter(|&v| v).is_some(),
                )?;
                let cursor_event = state.event_only.unwrap_or(false);
                let cursor_chat = state.chat_only.unwrap_or(false);
                if cursor_event != ingress_filter.event_only()
                    || cursor_chat != ingress_filter.chat_only()
                {
                    return Err(ContextIndexRequestParseError::CursorScopeMismatch);
                }
                state.offset
            }
        };

        let ingress_filter = ContextPickerIngressFilter::from_flags(
            params.event_only.filter(|&v| v).is_some(),
            params.chat_only.filter(|&v| v).is_some(),
        )?;

        Ok(Self {
            agent_package,
            limit: raw_limit as usize,
            offset,
            ingress_filter,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_only_and_event_only_are_mutually_exclusive() {
        let err = ContextIndexRequest::from_query(ContextIndexQueryParams {
            agent_package: None,
            limit: Some(50),
            cursor: None,
            event_only: Some(true),
            chat_only: Some(true),
        })
        .expect_err("expected conflict");
        assert!(matches!(
            err,
            ContextIndexRequestParseError::ConflictingIngressFilters
        ));
    }

    #[test]
    fn chat_only_request_parses() {
        let req = ContextIndexRequest::from_query(ContextIndexQueryParams {
            agent_package: Some("extrospection-agent".to_string()),
            limit: Some(50),
            cursor: None,
            event_only: None,
            chat_only: Some(true),
        })
        .expect("parse");
        assert_eq!(req.ingress_filter, ContextPickerIngressFilter::ChatOnly);
        assert_eq!(req.agent_package.as_deref(), Some("extrospection-agent"));
    }
}
