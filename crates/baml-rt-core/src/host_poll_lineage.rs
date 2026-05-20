//! Stable poll-root identifiers for host ingress (`host.source-records.v1`).

use serde::{Deserialize, Serialize};

use crate::ids::{ContextId, CorrelationId};

/// Inputs used to mint stable `context_id` / `correlation_id` for one poll window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PollLineageSeed {
    pub source_kind: String,
    pub source_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_message_ts: Vec<String>,
}

/// Minted lineage ids for one host poll batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPollLineage {
    pub context_id: ContextId,
    pub correlation_id: CorrelationId,
    pub source_cursor: Option<String>,
    pub source_message_ts: Vec<String>,
    /// Stable poll-batch id for `ProducedEvent.message_id` and graph anchors.
    pub poll_batch_id: String,
}

/// Mint stable lineage for a poll window. Preserves legacy hash namespaces for `context_id`
/// / `correlation_id` (`td-external-*`).
pub fn mint_host_poll_lineage(seed: &PollLineageSeed) -> Option<HostPollLineage> {
    let source_cursor = seed.source_cursor.as_deref()?;
    let seed_parts = [
        seed.source_kind.as_str(),
        seed.source_key.as_str(),
        source_cursor,
    ];
    let (upper, lower) = stable_id_parts("td-external-context", &seed_parts);
    let context_id = ContextId::new(upper, lower);
    let (corr_upper, corr_lower) = stable_id_parts("td-external-correlation", &seed_parts);
    let correlation_id = CorrelationId::new(corr_upper, corr_lower);
    let poll_batch_id =
        poll_batch_message_id(&seed.source_key, source_cursor, &seed.source_message_ts);
    Some(HostPollLineage {
        context_id,
        correlation_id,
        source_cursor: Some(source_cursor.to_string()),
        source_message_ts: seed.source_message_ts.clone(),
        poll_batch_id,
    })
}

/// Stable `ProducedEvent.message_id` for a poll batch (distinct from `correlation_id`).
pub fn poll_batch_message_id(
    source_key: &str,
    source_cursor: &str,
    source_message_ts: &[String],
) -> String {
    let first_ts = source_message_ts.first().map(String::as_str).unwrap_or("");
    let last_ts = source_message_ts.last().map(String::as_str).unwrap_or("");
    let count = source_message_ts.len().to_string();
    hash_event_id(
        "td-poll-batch",
        &[source_key, source_cursor, first_ts, last_ts, count.as_str()],
    )
}

fn hash_event_id(prefix: &str, parts: &[&str]) -> String {
    let digest = hash_digest(parts);
    format!("{prefix}-{digest:016x}")
}

/// Stable external id string for host dispatch unit scopes and related anchors.
#[must_use]
pub fn stable_external_id(namespace: &str, parts: &[&str]) -> String {
    hash_event_id(namespace, parts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableIdLane {
    Upper,
    Lower,
}

impl StableIdLane {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }
}

fn stable_id_parts(namespace: &str, parts: &[&str]) -> (u64, u64) {
    let upper_raw = hash_digest_with_namespace(namespace, StableIdLane::Upper, parts);
    let lower_raw = hash_digest_with_namespace(namespace, StableIdLane::Lower, parts);
    (upper_raw.max(1), lower_raw.max(1))
}

fn hash_digest_with_namespace(namespace: &str, lane: StableIdLane, parts: &[&str]) -> u64 {
    let mut digest = hash_digest(&[namespace, lane.as_str()]);
    for part in parts {
        digest = hash_digest_extend(digest, part);
    }
    digest
}

fn hash_digest(parts: &[&str]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    let mut digest = FNV_OFFSET_BASIS;
    for part in parts {
        digest = hash_digest_extend(digest, part);
    }
    digest
}

fn hash_digest_extend(mut digest: u64, part: &str) -> u64 {
    const FNV_PRIME: u64 = 0x00000100000001B3;
    const HASH_PART_SEPARATOR: u8 = 0x1f;
    for &byte in part.as_bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(FNV_PRIME);
    }
    digest ^= u64::from(HASH_PART_SEPARATOR);
    digest.wrapping_mul(FNV_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_stable_for_same_poll_seed() {
        let seed = PollLineageSeed {
            source_kind: "slack".to_string(),
            source_key: "slack:C123".to_string(),
            source_cursor: Some("slack:1735689600.000000:1735689700.000000:2".to_string()),
            source_message_ts: vec![
                "1735689600.000000".to_string(),
                "1735689700.000000".to_string(),
            ],
        };
        let one = mint_host_poll_lineage(&seed).expect("lineage");
        let two = mint_host_poll_lineage(&seed).expect("lineage");
        assert_eq!(one.context_id, two.context_id);
        assert_eq!(one.correlation_id, two.correlation_id);
        assert_eq!(one.poll_batch_id, two.poll_batch_id);
    }

    #[test]
    fn pins_known_context_and_correlation_ids() {
        let seed = PollLineageSeed {
            source_kind: "slack".to_string(),
            source_key: "slack:C123".to_string(),
            source_cursor: Some("slack:1735689600.000000:1735689700.000000:2".to_string()),
            source_message_ts: vec![
                "1735689600.000000".to_string(),
                "1735689700.000000".to_string(),
            ],
        };
        let lineage = mint_host_poll_lineage(&seed).expect("lineage");
        assert_eq!(
            (lineage.context_id.as_str(), lineage.correlation_id.as_str(),),
            (
                "ctx-7548386120284784534-8799862099676914443",
                "corr-6129901457429418597-2178675600574945132",
            )
        );
        assert_eq!(lineage.poll_batch_id, "td-poll-batch-0297d83c9ea13c63");
    }
}
