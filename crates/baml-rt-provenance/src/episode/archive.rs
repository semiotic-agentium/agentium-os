//! Cached [`super::Episode`] assembly and replay [`RefTable`] for archive read paths.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use baml_rt_conversation::{
    timeline::TimelineKind,
    view::{ConversationItemContent, SessionStepOp},
};
use baml_rt_core::ids::{ContextId, TaskId};
use baml_rt_tools::{
    archive_read::{RenderedContent, ShortRef, render_to_lines},
    archive_refs::{ArchiveEntry, HistoryEntry, RefTable},
};

use super::{Episode, reader::EpisodeReader};
use crate::{error::Result, surreal_store::SurrealProvenanceStore};

const CACHE_CAP: usize = 64;

/// Episode plus a [`RefTable`] replayed with [`insert_virtual_archive`](RefTable::insert_virtual_archive) /
/// [`insert_virtual_history`](RefTable::insert_virtual_history) at the same indices as the live session.
pub struct CachedEpisode {
    pub episode: Episode,
    pub ref_table: Arc<RefTable>,
}

struct LruCache {
    order: VecDeque<String>,
    map: HashMap<String, Arc<CachedEpisode>>,
}

impl LruCache {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<CachedEpisode>> {
        let v = self.map.get(key).cloned()?;
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).expect("position valid");
            self.order.push_back(k);
        }
        Some(v)
    }

    fn insert(&mut self, key: String, v: Arc<CachedEpisode>) {
        self.map.remove(&key);
        self.order.retain(|k| k != &key);
        self.map.insert(key.clone(), v);
        self.order.push_back(key);
        while self.order.len() > CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

/// LRU-cached `(context_id, task_id)` → [`CachedEpisode`].
pub struct EpisodeArchiveSource {
    store: Arc<SurrealProvenanceStore>,
    cache: Mutex<LruCache>,
}

impl EpisodeArchiveSource {
    #[must_use]
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self {
            store,
            cache: Mutex::new(LruCache::new()),
        }
    }

    /// Load or return cached episode + replay ref table.
    pub async fn load_cached(
        &self,
        context_id: &ContextId,
        task_id: &TaskId,
    ) -> Result<Arc<CachedEpisode>> {
        let key = format!("{}::{}", context_id.as_str(), task_id.as_str());
        {
            let mut g = self.cache.lock().expect("episode cache mutex poisoned");
            if let Some(hit) = g.get(&key) {
                return Ok(hit);
            }
        }
        let reader = EpisodeReader::new(Arc::clone(&self.store));
        let (episode, merged) = reader.read_with_timeline(context_id, task_id).await?;
        let ref_table = Arc::new(episode_ref_table_with_merged(&episode, &merged));
        let bundle = Arc::new(CachedEpisode { episode, ref_table });
        let mut g = self.cache.lock().expect("episode cache mutex poisoned");
        if let Some(hit) = g.get(&key) {
            return Ok(hit);
        }
        g.insert(key, Arc::clone(&bundle));
        Ok(bundle)
    }
}

fn wire_archive_slot_from_ref(archive_ref: &str) -> Option<ShortRef> {
    let s = archive_ref.trim();
    ShortRef::parse(s).or_else(|| {
        s.rfind('@').and_then(|i| {
            let tail = s[i..].strip_prefix('@')?;
            if let Some((p, rest)) = tail.split_once('/') {
                let prefix = p.parse().ok()?;
                let end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                if end == 0 {
                    return None;
                }
                let local = rest[..end].parse().ok()?;
                return Some(ShortRef::new_prefixed(prefix, local));
            }
            let end = tail
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(tail.len());
            if end == 0 {
                return None;
            }
            let local = tail[..end].parse().ok()?;
            Some(ShortRef::new(local))
        })
    })
}

fn insert_virtual_for_wire(
    t: &RefTable,
    wire_filled: &mut HashSet<u64>,
    wire_r: ShortRef,
    tool_name: &str,
    v: &serde_json::Value,
    activity_anchor: &str,
) {
    let rendered = render_to_lines(v);
    let summary = format!("{tool_name} result");
    let entry = ArchiveEntry::new(
        rendered,
        tool_name.to_string(),
        summary,
        activity_anchor.to_string(),
        "tool_result".into(),
    );
    t.insert_virtual_archive_ref(wire_r, entry);
    wire_filled.insert(wire_r.cell_key());
}

/// Graph-backed `SendDone` replay payloads (`send_done_replay_payload`) — same slots as
/// [`episode_ref_table_with_merged`] pass one.
pub(crate) fn seed_replay_payload_slots_from_merged(
    t: &RefTable,
    merged: &[TimelineKind],
    wire_filled: &mut HashSet<u64>,
) {
    if merged.is_empty() {
        return;
    }
    for m in merged {
        let TimelineKind::Conv(item, _) = m else {
            continue;
        };
        if let ConversationItemContent::SessionStep(ss) = &item.content
            && let SessionStepOp::SendDone { archive_ref, .. } = &ss.op
            && let Some(wire_r) = wire_archive_slot_from_ref(archive_ref)
            && let Some(ref payload) = ss.send_done_replay_payload
        {
            insert_virtual_for_wire(
                t,
                wire_filled,
                wire_r,
                ss.tool_name.as_str(),
                payload,
                item.activity_anchor.as_str(),
            );
        }
    }
}

/// One row of [`episode_ref_table_with_merged`] pass two — keeps transcript assembly and
/// session-history [`ArchiveReader`] aligned when the table is built incrementally.
pub(crate) fn absorb_episode_entry_into_ref_table(
    t: &RefTable,
    wire_filled: &mut HashSet<u64>,
    e: &super::EpisodeEntry,
) {
    use super::EpisodeContent;

    let n = e.seq;
    if n == 0 {
        return;
    }
    match &e.content {
        EpisodeContent::Text(body) => {
            t.insert_virtual_history(
                n,
                HistoryEntry::new(e.activity_anchor.clone(), "message".into()),
                body.as_str(),
            );
        }
        EpisodeContent::ToolInvocation {
            tool_name: _,
            description,
        } => {
            t.insert_virtual_history(
                n,
                HistoryEntry::new(e.activity_anchor.clone(), "tool_call".into()),
                description.as_str(),
            );
        }
        EpisodeContent::ToolOutput {
            tool_name,
            summary,
            lines,
            ..
        } => {
            let wire_key = ShortRef::new(n).cell_key();
            if wire_filled.contains(&wire_key) {
                return;
            }
            let rc = RenderedContent::from_lines(lines.iter().cloned());
            let entry = ArchiveEntry::new(
                rc,
                tool_name.clone(),
                summary.clone(),
                e.activity_anchor.clone(),
                "tool_result".into(),
            );
            t.insert_virtual_archive(n, entry);
        }
        EpisodeContent::StatusChange { old, new, message } => {
            let text = if let Some(m) = message {
                format!("{old} -> {new}: {m}")
            } else {
                format!("{old} -> {new}")
            };
            t.insert_virtual_history(
                n,
                HistoryEntry::new(e.activity_anchor.clone(), "status".into()),
                text,
            );
        }
        EpisodeContent::Artifact {
            name, media_type, ..
        } => {
            let text = match media_type {
                Some(mt) => format!("artifact {name} ({mt})"),
                None => format!("artifact {name}"),
            };
            t.insert_virtual_history(
                n,
                HistoryEntry::new(e.activity_anchor.clone(), "artifact".into()),
                text,
            );
        }
        EpisodeContent::PlanRevisionRef { summary } => {
            t.insert_virtual_history(
                n,
                HistoryEntry::new(e.activity_anchor.clone(), "plan".into()),
                summary.as_str(),
            );
        }
    }
}

/// Build a [`RefTable`] for `#N` / `@N` resolution when the merged timeline is unavailable.
/// Prefer [`episode_ref_table_with_merged`] from episode assembly so `@N` matches **wire** indices.
#[must_use]
pub fn episode_ref_table(ep: &Episode) -> RefTable {
    episode_ref_table_with_merged(ep, &[])
}

/// Replay ref table: **O(n)** over `merged` + **O(m)** over episode rows (`n,m` ≈ transcript size).
///
/// Live `@N` ids come from the session ref allocator; episode [`EpisodeEntry::seq`] also counts
/// status/artifact rows, so indices diverge.
///
/// **Graph-backed replay:** [`SessionStepContent::send_done_replay_payload`] is filled in
/// [`conversation_context`](crate::store::ProvenanceContextReader::conversation_context) by
/// traversing `WAS_INFORMED_BY` (`a2a:informed_by_tool_invocation`) from the `SessionStep` entity
/// to the `ToolCall` activity, then loading that call’s `tool_result` payload. When that field is
/// present, `@N` replay uses it directly.
///
/// **Legacy graphs** (no influence edge / hydration): fall back to pairing `SendDone` with the next
/// matching `ToolResult` by [`tool_names_match_for_archive`].
#[must_use]
pub(crate) fn episode_ref_table_with_merged(ep: &Episode, merged: &[TimelineKind]) -> RefTable {
    let t = RefTable::new();
    let mut wire_filled: HashSet<u64> = HashSet::new();
    seed_replay_payload_slots_from_merged(&t, merged, &mut wire_filled);

    let mut rows: Vec<_> = ep
        .prior_context
        .iter()
        .chain(ep.transcript.iter())
        .collect();
    rows.sort_by_key(|e| e.seq);
    for e in rows {
        absorb_episode_entry_into_ref_table(&t, &mut wire_filled, e);
    }
    t
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use baml_rt_conversation::{
        timeline::TimelineKind,
        view::{
            ConversationItemContent, ProvenanceConversationContextItem, SessionStepContent,
            SessionStepOp, ToolOutcome, ToolResultContent, ToolSessionPhase,
        },
    };
    use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, TaskId, UuidId};
    use baml_rt_tools::{
        archive_read::{ShortRef, VirtualArchiveSource},
        archive_refs::RefTable,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        absorb_episode_entry_into_ref_table, episode_ref_table, episode_ref_table_with_merged,
        seed_replay_payload_slots_from_merged,
    };
    use crate::episode::{
        Episode, EpisodeContent, EpisodeDuration, EpisodeEntry, EpisodeOutcome, EpisodeRefPrefix,
        StepType, TerminalStatus, TokenSummary,
    };

    #[test]
    fn wire_replay_prefers_send_done_graph_payload_over_fifo_tool_results() {
        let task_id = TaskId::from_external(ExternalId::new("t-wire"));
        let ep = Episode {
            task_id: task_id.clone(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
            ref_prefix: EpisodeRefPrefix::from_task_id(&task_id),
            status: TerminalStatus::Completed,
            started_timestamp_ms: 0,
            duration: EpisodeDuration::default(),
            token_summary: TokenSummary::default(),
            prior_context: vec![],
            goal: EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("x".into()),
                activity_anchor: "g".into(),
                citation_strings: vec![],
            },
            transcript: vec![],
            intents: vec![],
            plans: vec![],
            outcome: EpisodeOutcome {
                final_message: None,
                artifacts: vec![],
                citation_strings: vec![],
                token_summary: TokenSummary::default(),
                duration: EpisodeDuration::default(),
            },
            session_history: vec![],
            drift_summary: None,
            drift_calls: vec![],
        };

        let merged = vec![
            TimelineKind::Conv(
                ProvenanceConversationContextItem {
                    timestamp_ms: 1,
                    activity_anchor: ActivityAnchorId::from("sd1"),
                    role: "tool".into(),
                    content: ConversationItemContent::SessionStep(SessionStepContent {
                        tool_name: "system/discover_agents".into(),
                        op: SessionStepOp::SendDone {
                            archive_ref: "@8".into(),
                            header: "@8 · \"…\" · 2L · 1B".into(),
                            informed_by: "test-anchor".into(),
                        },
                        send_done_replay_payload: Some(json!([
                            {"name": "alpha", "description": "big"},
                            {"name": "beta", "description": "payload"},
                        ])),
                        read_replay_lines: None,
                    }),
                },
                false,
            ),
            TimelineKind::Conv(
                ProvenanceConversationContextItem {
                    timestamp_ms: 2,
                    activity_anchor: ActivityAnchorId::from("tr-other"),
                    role: "tool".into(),
                    content: ConversationItemContent::ToolResult(ToolResultContent {
                        tool_name: "a2a/execution_session_step".into(),
                        fsm_phase: ToolSessionPhase::Execute,
                        outcome: ToolOutcome::Result(json!({"citations": []})),
                    }),
                },
                false,
            ),
        ];

        let rt = episode_ref_table_with_merged(&ep, &merged);
        let row = rt
            .archive_row(ShortRef::new(8))
            .expect("@8 must map to discover payload, not a2a");
        assert!(
            row.content.line_count() >= 2,
            "expected multi-line discover render, got {} lines",
            row.content.line_count()
        );
    }

    #[test]
    fn episode_ref_table_maps_tool_result_to_archive_namespace() {
        let task_id = TaskId::from_external(ExternalId::new("t1"));
        let ep = crate::episode::Episode {
            task_id: task_id.clone(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
            ref_prefix: EpisodeRefPrefix::from_task_id(&task_id),
            status: TerminalStatus::Completed,
            started_timestamp_ms: 0,
            duration: EpisodeDuration::default(),
            token_summary: TokenSummary::default(),
            prior_context: vec![],
            goal: EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("hi".into()),
                activity_anchor: "m1".into(),
                citation_strings: vec![],
            },
            transcript: vec![
                EpisodeEntry {
                    seq: 1,
                    step_type: StepType::Message,
                    role: "user".into(),
                    elapsed_ms: 0,
                    content: EpisodeContent::Text("hi".into()),
                    activity_anchor: "m1".into(),
                    citation_strings: vec![],
                },
                EpisodeEntry {
                    seq: 2,
                    step_type: StepType::ToolResult,
                    role: "agent".into(),
                    elapsed_ms: 1,
                    content: EpisodeContent::ToolOutput {
                        tool_name: "calc".into(),
                        summary: "ok".into(),
                        line_count: 2,
                        byte_count: 10,
                        lines: vec!["a".into(), "b".into()],
                    },
                    activity_anchor: "r1".into(),
                    citation_strings: vec![],
                },
            ],
            intents: vec![],
            plans: vec![],
            outcome: EpisodeOutcome {
                final_message: None,
                artifacts: vec![],
                citation_strings: vec![],
                token_summary: TokenSummary::default(),
                duration: EpisodeDuration::default(),
            },
            session_history: vec![],
            drift_summary: None,
            drift_calls: vec![],
        };
        let rt = episode_ref_table(&ep);
        assert!(rt.history_row(1).is_some());
        assert!(rt.archive_row(ShortRef::new(2)).is_some());
        assert!(rt.archive_row(ShortRef::new(1)).is_none());
    }

    #[test]
    fn incremental_ref_table_matches_batch_for_replay_seed_and_rows() {
        let task_id = TaskId::from_external(ExternalId::new("t-incr"));
        let ep = Episode {
            task_id: task_id.clone(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
            ref_prefix: EpisodeRefPrefix::from_task_id(&task_id),
            status: TerminalStatus::Completed,
            started_timestamp_ms: 0,
            duration: EpisodeDuration::default(),
            token_summary: TokenSummary::default(),
            prior_context: vec![],
            goal: EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("x".into()),
                activity_anchor: "g".into(),
                citation_strings: vec![],
            },
            transcript: vec![],
            intents: vec![],
            plans: vec![],
            outcome: EpisodeOutcome {
                final_message: None,
                artifacts: vec![],
                citation_strings: vec![],
                token_summary: TokenSummary::default(),
                duration: EpisodeDuration::default(),
            },
            session_history: vec![],
            drift_summary: None,
            drift_calls: vec![],
        };

        let merged = vec![
            TimelineKind::Conv(
                ProvenanceConversationContextItem {
                    timestamp_ms: 1,
                    activity_anchor: ActivityAnchorId::from("sd1"),
                    role: "tool".into(),
                    content: ConversationItemContent::SessionStep(SessionStepContent {
                        tool_name: "system/discover_agents".into(),
                        op: SessionStepOp::SendDone {
                            archive_ref: "@8".into(),
                            header: "@8 · \"…\" · 2L · 1B".into(),
                            informed_by: "test-anchor".into(),
                        },
                        send_done_replay_payload: Some(json!([
                            {"name": "alpha", "description": "big"},
                            {"name": "beta", "description": "payload"},
                        ])),
                        read_replay_lines: None,
                    }),
                },
                false,
            ),
            TimelineKind::Conv(
                ProvenanceConversationContextItem {
                    timestamp_ms: 2,
                    activity_anchor: ActivityAnchorId::from("tr-other"),
                    role: "tool".into(),
                    content: ConversationItemContent::ToolResult(ToolResultContent {
                        tool_name: "a2a/execution_session_step".into(),
                        fsm_phase: ToolSessionPhase::Execute,
                        outcome: ToolOutcome::Result(json!({"citations": []})),
                    }),
                },
                false,
            ),
        ];

        let batch = episode_ref_table_with_merged(&ep, &merged);

        let incr = RefTable::new();
        let mut wire_filled = HashSet::new();
        seed_replay_payload_slots_from_merged(&incr, &merged, &mut wire_filled);
        let mut rows: Vec<_> = ep
            .prior_context
            .iter()
            .chain(ep.transcript.iter())
            .collect();
        rows.sort_by_key(|e| e.seq);
        for e in rows {
            absorb_episode_entry_into_ref_table(&incr, &mut wire_filled, e);
        }

        let at8 = ShortRef::new(8);
        assert_eq!(
            batch.get(at8).map(|r| r.content.line_count()),
            incr.get(at8).map(|r| r.content.line_count()),
            "incremental replay ref table must match monolithic episode_ref_table_with_merged"
        );
    }
}
