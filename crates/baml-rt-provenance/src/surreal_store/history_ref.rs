// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Durable `#N` history ref registry (idempotent per activity + source).

use baml_rt_core::ids::ContextId;
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::{ProvenanceError, Result},
    surreal_tables::{TBL_HISTORY_REF_REGISTRY, TBL_SESSION_REF_COUNTER},
};

fn hex_id(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(b"|");
    }
    format!("{:x}", h.finalize())
}

fn history_stable_key(activity_anchor: &str, source: &str) -> String {
    format!("{activity_anchor}\0{source}")
}

impl SurrealProvenanceStore {
    /// Existing `#N` for `(context, activity_anchor, source)`, if registered.
    pub async fn history_ref_lookup(
        &self,
        context_id: &ContextId,
        activity_anchor: &str,
        source: &str,
    ) -> Result<Option<u32>> {
        let ctx = context_id.as_str();
        let q = format!(
            "SELECT history_n FROM {TBL_HISTORY_REF_REGISTRY} \
             WHERE context_id = $ctx AND activity_anchor = $anchor AND source = $source LIMIT 1"
        );
        let response = self
            .db()
            .query(&q)
            .bind(("ctx", ctx.to_string()))
            .bind(("anchor", activity_anchor.to_string()))
            .bind(("source", source.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(n) = rows
            .first()
            .and_then(|r| r.get("history_n"))
            .and_then(|x| x.as_u64())
        else {
            return Ok(None);
        };
        u32::try_from(n)
            .map_err(|_| ProvenanceError::InvalidEvent {
                activity_anchor: activity_anchor.into(),
                reason: format!("history_n overflow: {n}"),
            })
            .map(Some)
    }

    /// Idempotent: return existing `#N` or allocate the next per-context ref index and persist.
    pub async fn history_ref_ensure(
        &self,
        context_id: &ContextId,
        activity_anchor: &str,
        source: &str,
    ) -> Result<u32> {
        if let Some(n) = self
            .history_ref_lookup(context_id, activity_anchor, source)
            .await?
        {
            return Ok(n);
        }
        let n = self.session_ref_next(context_id).await?;
        let ctx = context_id.as_str();
        let stable_key = history_stable_key(activity_anchor, source);
        let ins = format!(
            "INSERT INTO {TBL_HISTORY_REF_REGISTRY} {{ \
                context_id: $ctx, activity_anchor: $anchor, source: $source, \
                stable_key: $key, history_n: $n \
            }} RETURN history_n;"
        );
        let response = self
            .db()
            .query(&ins)
            .bind(("ctx", ctx.to_string()))
            .bind(("anchor", activity_anchor.to_string()))
            .bind(("source", source.to_string()))
            .bind(("key", stable_key))
            .bind(("n", n))
            .await;
        match response {
            Ok(r) => match check_and_take_zero(r, map_surreal_error) {
                Ok(out) => {
                    if let Some(row_n) = out
                        .first()
                        .and_then(|row| row.get("history_n"))
                        .and_then(|x| x.as_u64())
                    {
                        return u32::try_from(row_n).map_err(|_| ProvenanceError::InvalidEvent {
                            activity_anchor: activity_anchor.into(),
                            reason: format!("history_n overflow: {row_n}"),
                        });
                    }
                }
                Err(_) => {
                    if let Some(existing) = self
                        .history_ref_lookup(context_id, activity_anchor, source)
                        .await?
                    {
                        return Ok(existing);
                    }
                }
            },
            Err(e) if super::is_duplicate_record_write(&e) => {
                if let Some(existing) = self
                    .history_ref_lookup(context_id, activity_anchor, source)
                    .await?
                {
                    return Ok(existing);
                }
            }
            Err(e) => return Err(map_surreal_error(e)),
        }
        Ok(n)
    }

    /// All durable `#N` rows for a context (hydration).
    pub async fn history_ref_list_for_context(
        &self,
        context_id: &ContextId,
    ) -> Result<Vec<(String, String, u32)>> {
        let ctx = context_id.as_str();
        let q = format!(
            "SELECT activity_anchor, source, history_n FROM {TBL_HISTORY_REF_REGISTRY} \
             WHERE context_id = $ctx ORDER BY history_n ASC"
        );
        let response = self
            .db()
            .query(&q)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let anchor = row
                .get("activity_anchor")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let source = row
                .get("source")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let n = row
                .get("history_n")
                .and_then(|x| x.as_u64())
                .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
                    reason: "history_ref_registry row missing history_n".into(),
                })?;
            let n = u32::try_from(n).map_err(|_| ProvenanceError::InvalidEvent {
                activity_anchor: anchor.clone(),
                reason: format!("history_n overflow: {n}"),
            })?;
            out.push((anchor, source, n));
        }
        Ok(out)
    }

    /// Next monotonic ref index for `#N` and flat `@N` (`prefix = 1`) within a context.
    pub async fn session_ref_next(&self, context_id: &ContextId) -> Result<u32> {
        let ctx = context_id.as_str();
        let rid = hex_id(&[ctx, "src"]);
        for attempt in 0..super::WRITE_CONFLICT_MAX_ATTEMPTS {
            let upd = format!(
                "UPDATE type::record('{TBL_SESSION_REF_COUNTER}', $rid) \
                 SET next_ref += 1 RETURN next_ref;"
            );
            let mut uresp = self.db().query(&upd).bind(("rid", rid.clone())).await;
            match uresp {
                Ok(ref mut r) => {
                    let rows: std::result::Result<Vec<Value>, _> = r.take(0);
                    if let Ok(rs) = rows
                        && let Some(n) = rs
                            .first()
                            .and_then(|v| v.get("next_ref"))
                            .and_then(|x| x.as_u64())
                    {
                        return u32::try_from(n).map_err(|_| ProvenanceError::InvalidEvent {
                            activity_anchor: "session_ref_counter".into(),
                            reason: format!("next_ref overflow: {n}"),
                        });
                    }
                }
                Err(e) if super::is_transaction_conflict(&e) => {
                    if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: e.message().to_string(),
                        });
                    }
                    tokio::time::sleep(super::jittered_backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }

            let create = format!(
                "CREATE type::record('{TBL_SESSION_REF_COUNTER}', $rid) CONTENT {{ \
                    context_id: $ctx, next_ref: 1 \
                }} RETURN next_ref;"
            );
            let cresp = self
                .db()
                .query(&create)
                .bind(("rid", rid.clone()))
                .bind(("ctx", ctx.to_string()))
                .await;
            match cresp {
                Ok(cresp) => match check_and_take_zero(cresp, std::convert::identity) {
                    Ok(crs) => {
                        if let Some(n) = crs
                            .first()
                            .and_then(|v| v.get("next_ref"))
                            .and_then(Value::as_u64)
                        {
                            return u32::try_from(n).map_err(|_| ProvenanceError::InvalidEvent {
                                activity_anchor: "session_ref_counter".into(),
                                reason: format!("next_ref overflow: {n}"),
                            });
                        }
                    }
                    Err(e)
                        if super::is_transaction_conflict(&e)
                            || super::is_duplicate_record_write(&e) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(map_surreal_error(e)),
                },
                Err(e)
                    if super::is_transaction_conflict(&e)
                        || super::is_duplicate_record_write(&e) =>
                {
                    continue;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }
            if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                break;
            }
            tokio::time::sleep(super::jittered_backoff(attempt)).await;
        }
        Err(ProvenanceError::Contention {
            details: "session_ref_next: exhausted retries".into(),
        })
    }

    /// Register durable `#N` rows for message / tool_call projection items (DB before cache).
    pub async fn sync_history_refs_for_projection(
        &self,
        context_id: &ContextId,
        entries: &[(String, String)],
    ) -> Result<()> {
        for (anchor, source) in entries {
            self.history_ref_ensure(context_id, anchor, source).await?;
        }
        Ok(())
    }
}
