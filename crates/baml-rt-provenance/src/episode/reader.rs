//! Assemble [`super::Episode`] from Surreal-backed provenance.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::ids::{AgentId, ContextId, TaskId, UuidId};
use baml_rt_tools::{
    archive_read::{
        format_session_read_body_from_json_value, format_session_read_body_from_rendered,
    },
    prompt_projection::ProjectionRenderOptions,
    tools::ToolRegistry,
};
use futures_util::future::try_join_all;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use super::{
    ArtifactSummary, Episode, EpisodeContent, EpisodeEntry, EpisodeOutcome, EpisodeRefPrefix,
    IntentRevision, PlanRevision, PlanStepEntry, SessionHistoryLine, StepType, TerminalStatus,
    from_graph::{ArtifactRow, StatusRow, episode_metadata_from_task_graph},
    renderer::{prefix_wire_citation, prefix_wire_citations_in_text},
    timeline::TimelineKind,
};
use crate::{
    citation_queries::query_plan_citations,
    error::{ProvenanceError, Result},
    graph_export::export_graph_for_task,
    store::{
        ConversationItemContent, ProvenanceConversationContextItem, ProvenancePlanningQuery,
        ProvenanceQueryApi, SessionStepOp, ToolOutcome,
    },
    surreal_store::SurrealProvenanceStore,
};

/// Reads a completed-task episode from the provenance store.
pub struct EpisodeReader {
    store: Arc<SurrealProvenanceStore>,
    tool_registry: Arc<ToolRegistry>,
}

impl EpisodeReader {
    #[must_use]
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self {
            store,
            tool_registry: Arc::new(ToolRegistry::new()),
        }
    }

    #[must_use]
    pub fn with_tool_registry(
        store: Arc<SurrealProvenanceStore>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            store,
            tool_registry,
        }
    }

    /// Build an [`Episode`] for `task_id` in `context_id`. Fails if the task has no terminal
    /// `A2ATaskState` in the graph.
    pub async fn read(&self, context_id: &ContextId, task_id: &TaskId) -> Result<Episode> {
        self.read_inner(context_id, task_id, true)
            .await
            .map(|(e, _)| e)
    }

    /// Build an [`Episode`] for `task_id` in `context_id`, allowing in-progress tasks without a terminal state.
    /// Returns `TerminalStatus::Other("in_progress")` when no terminal state exists.
    pub async fn read_snapshot(&self, context_id: &ContextId, task_id: &TaskId) -> Result<Episode> {
        self.read_inner(context_id, task_id, false)
            .await
            .map(|(e, _)| e)
    }

    /// Build an episode from `task_id` alone — resolves `context_id` internally.
    /// Avoids the double graph-export round trip.
    ///
    /// Times out after [`EPISODE_SNAPSHOT_TIMEOUT_SECS`] and returns an
    /// `InvalidEvent` error (treated as `NotFound` / 404 by the API layer)
    /// rather than a slow 500 when the graph is still being populated.
    pub async fn read_snapshot_by_task_id(&self, task_id: &TaskId) -> Result<Episode> {
        let timeout = tokio::time::Duration::from_secs(EPISODE_SNAPSHOT_TIMEOUT_SECS);
        match tokio::time::timeout(timeout, self.read_snapshot_by_task_id_inner(task_id)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!(
                    "episode snapshot timed out after {EPISODE_SNAPSHOT_TIMEOUT_SECS}s for task_id={}; \
                     task graph may still be populating",
                    task_id.as_str()
                ),
            }),
        }
    }

    async fn read_snapshot_by_task_id_inner(&self, task_id: &TaskId) -> Result<Episode> {
        let tid = task_id.as_str();
        let context_id_str = crate::graph_export::task_context_id(Arc::clone(&self.store), tid)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!("no context_id found for task_id={tid}"),
            })?;
        let context_id = ContextId::from(context_id_str.as_str());
        self.read_inner(&context_id, task_id, false)
            .await
            .map(|(e, _)| e)
    }

    /// Same as [`Self::read`], plus the merged timeline used for wire-aligned `@N` replay.
    pub(crate) async fn read_with_timeline(
        &self,
        context_id: &ContextId,
        task_id: &TaskId,
    ) -> Result<(Episode, Vec<TimelineKind>)> {
        self.read_inner(context_id, task_id, true).await
    }

    async fn read_inner(
        &self,
        context_id: &ContextId,
        task_id: &TaskId,
        require_terminal: bool,
    ) -> Result<(Episode, Vec<TimelineKind>)> {
        let tid = task_id.as_str();
        let task_graph = export_graph_for_task(Arc::clone(&self.store), tid).await?;
        let graph_meta = episode_metadata_from_task_graph(&task_graph);

        // Fail fast before issuing parallel queries if a terminal state is required but absent.
        let (status_str, terminal_ts) = match graph_meta.terminal {
            Some((status, ts)) => (status, ts),
            None if require_terminal => {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: String::new(),
                    reason: format!(
                        "episode requires a terminal task state; none found for task_id={tid}"
                    ),
                });
            }
            None => {
                // For snapshots: use "in_progress" status and current time as terminal bound
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                ("in_progress".to_string(), now_ms)
            }
        };

        // All six queries are independent of each other — run them concurrently.
        // Drift errors are non-fatal: log and continue without drift section.
        let (
            (token_summary, llm_earliest_ms),
            agent_id_resolution,
            mut items,
            intents_db,
            plans_db,
            drift_result,
        ) = tokio::try_join!(
            async {
                tokio::try_join!(
                    super::aggregates::token_summary_for_task(&self.store, tid),
                    super::aggregates::llm_earliest_timestamp_ms(&self.store, tid),
                )
            },
            self.store.get_task_agent_id(task_id),
            self.store
                .query_conversation_context(context_id, None, Some(task_id)),
            self.store.query_intent_history(task_id, None),
            self.store.query_plan_history(task_id, None),
            async {
                Ok::<_, ProvenanceError>(
                    super::drift::aggregate_task_drift(
                        self.store.as_ref(),
                        context_id,
                        task_id,
                    )
                    .await
                    .map_err(|e| {
                        tracing::warn!(
                            error = %e,
                            task_id = %task_id,
                            "Drift aggregation failed; episode will render without drift section"
                        );
                        e
                    })
                    .ok(),
                )
            },
        )?;
        let (drift_summary, drift_calls) = drift_result.unwrap_or((None, Vec::new()));

        let status = map_terminal_status(&status_str);
        let earliest_status_ms = graph_meta
            .status_rows
            .iter()
            .map(|r| r.timestamp_ms)
            .filter(|&t| t > 0)
            .min();
        let task_start_ms = graph_meta
            .task_start_ms
            .or(llm_earliest_ms)
            .or(earliest_status_ms)
            .unwrap_or(terminal_ts);

        let ref_prefix = EpisodeRefPrefix::from_task_id(task_id);
        let agent_id = agent_id_resolution
            .into_option()
            .unwrap_or_else(|| AgentId::from_uuid(UuidId::new(Uuid::nil())));

        items.sort_by_key(|i| i.timestamp_ms);

        // Conversation rows use activity-anchor order keys as `timestamp_ms`, not wall clock.
        // Split prior vs task transcript by first task status anchor (graph), not by ms.
        let anchor_cutoff: Option<u64> = conv_anchor_cutoff_u64(&graph_meta.status_rows);
        let (prior_items, task_items): (Vec<_>, Vec<_>) = match anchor_cutoff {
            None => (Vec::new(), items),
            Some(cut) => items
                .into_iter()
                .partition(|i| i.timestamp_ms > 0 && i.timestamp_ms < cut),
        };

        let status_rows = graph_meta.status_rows;
        let artifact_rows = graph_meta.artifact_rows;
        let intents: Vec<IntentRevision> = intents_db
            .iter()
            .map(|r| IntentRevision {
                intent_id: r.intent_id.clone(),
                description: r.description.clone(),
                activity_anchor: r.activity_anchor_id.as_str().to_string(),
                timestamp_ms: r.event_order,
                superseded_by_next: r.superseded_by_next.is_some(),
                supersession_from_previous: r.supersession_from_previous.map(|k| format!("{k:?}")),
                derived_citation_strings: graph_meta
                    .intent_citations
                    .get(r.activity_anchor_id.as_str())
                    .map(|v| {
                        v.iter()
                            .map(|s| prefix_wire_citation(s, &ref_prefix))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();

        // Fetch plan citations for all plans concurrently (one query per plan).
        let citation_futures: Vec<_> = plans_db
            .iter()
            .map(|p| {
                let store = Arc::clone(&self.store);
                let tid_owned = tid.to_string();
                let plan_id = p.plan_id.clone();
                async move { query_plan_citations(&store, &tid_owned, &plan_id).await }
            })
            .collect();
        let all_plan_citations = try_join_all(citation_futures).await?;

        let mut plans: Vec<PlanRevision> = Vec::new();
        for (p, all_cites) in plans_db.iter().zip(all_plan_citations) {
            let mut by_step: HashMap<String, Vec<String>> = HashMap::new();
            for e in all_cites {
                let key = e
                    .step_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_default();
                if key.is_empty() {
                    continue;
                }
                by_step.insert(
                    key,
                    e.citations
                        .iter()
                        .map(|c| prefix_wire_citation(c.as_str(), &ref_prefix))
                        .collect(),
                );
            }
            let steps: Vec<PlanStepEntry> = p
                .steps
                .iter()
                .map(|s| PlanStepEntry {
                    step_id: s.step_id.clone(),
                    description: s.description.clone(),
                    status: s.status.clone(),
                    timestamp_ms: None,
                    citation_strings: by_step.get(&s.step_id).cloned().unwrap_or_default(),
                })
                .collect();
            plans.push(PlanRevision {
                plan_id: p.plan_id.clone(),
                intent_id: p.intent_id.clone(),
                activity_anchor: p.activity_anchor_id.as_str().to_string(),
                timestamp_ms: p.event_order,
                superseded_by_next: p.superseded_by_next.is_some(),
                steps,
            });
        }

        let wait_ms = compute_input_required_wait_ms(&status_rows, task_start_ms, terminal_ts);
        let wall_clock_ms = terminal_ts.saturating_sub(task_start_ms);
        let active_ms = wall_clock_ms.saturating_sub(wait_ms);

        let mut merged: Vec<TimelineKind> = Vec::new();
        for i in &prior_items {
            merged.push(TimelineKind::Conv(i.clone(), true));
        }
        for i in &task_items {
            merged.push(TimelineKind::Conv(i.clone(), false));
        }
        for s in status_rows {
            merged.push(TimelineKind::Status(s));
        }
        for a in artifact_rows {
            merged.push(TimelineKind::Artifact(a));
        }

        merged.sort_by_key(|k| match k {
            TimelineKind::Conv(i, _) => (i.timestamp_ms, 0u8),
            TimelineKind::Status(s) => (s.event_order, 1u8),
            TimelineKind::Artifact(a) => (a.event_order, 2u8),
        });

        let projection_opts =
            baml_rt_tools::prompt_projection::episode_session_history_projection_options();

        let mut seq: u32 = 0;
        let mut prior_context: Vec<EpisodeEntry> = Vec::new();
        let mut transcript: Vec<EpisodeEntry> = Vec::new();
        let mut prior_line: i64 = 0;
        let mut task_line: i64 = 0;

        for m in &merged {
            match m {
                TimelineKind::Conv(item, is_prior) => {
                    let tick_ms = if *is_prior {
                        prior_line += 1;
                        -(prior_line * EPISODE_LINE_TICK_MS)
                    } else {
                        task_line += 1;
                        task_line * EPISODE_LINE_TICK_MS
                    };
                    let entries = conv_item_to_entries(
                        item,
                        tick_ms,
                        &mut seq,
                        &ref_prefix,
                        &projection_opts,
                    )?;
                    if *is_prior {
                        prior_context.extend(entries);
                    } else {
                        transcript.extend(entries);
                    }
                }
                TimelineKind::Status(s) => {
                    task_line += 1;
                    let tick_ms = task_line * EPISODE_LINE_TICK_MS;
                    transcript.push(status_to_entry(s, tick_ms, &mut seq)?);
                }
                TimelineKind::Artifact(a) => {
                    task_line += 1;
                    let tick_ms = task_line * EPISODE_LINE_TICK_MS;
                    transcript.push(artifact_to_entry(a, tick_ms, &mut seq)?);
                }
            }
        }

        let mut step_citation_index: HashMap<(String, String), Vec<String>> = HashMap::new();
        for p in &plans {
            for s in &p.steps {
                if !s.citation_strings.is_empty() {
                    step_citation_index.insert(
                        (p.plan_id.clone(), s.step_id.clone()),
                        s.citation_strings.clone(),
                    );
                }
            }
        }
        // Episode view-model only: merge plan-step graph citations into synthetic
        // `a2a/execution_session_step` tool JSON (store conversation rows stay unmerged).
        enrich_execution_session_tool_outputs_with_plan_citations(
            &mut prior_context,
            &step_citation_index,
        );
        enrich_execution_session_tool_outputs_with_plan_citations(
            &mut transcript,
            &step_citation_index,
        );

        // Suppress the `citations: []` payload display on execution_session_step ToolResult entries
        // that have no citation grounding after enrichment. The entries STAY in the transcript
        // (removing them would break seq numbering and dangle citation refs from message entries).
        suppress_empty_execution_session_step_payload(&mut prior_context);
        suppress_empty_execution_session_step_payload(&mut transcript);

        // Citations on agent message entries are now populated directly from the
        // conversation context (ConversationItemContent::Message.citations), which is
        // populated by the context reader traversing CITED graph edges from Message nodes.
        // No secondary HashMap join needed.

        let goal = transcript
            .iter()
            .find(|e| e.step_type == StepType::Message && role_is_user(&e.role))
            .cloned()
            .or_else(|| transcript.first().cloned())
            .unwrap_or_else(|| EpisodeEntry {
                seq: 0,
                step_type: StepType::Message,
                role: "user".to_string(),
                elapsed_ms: 0,
                content: EpisodeContent::Text(String::new()),
                activity_anchor: String::new(),
                citation_strings: Vec::new(),
            });

        let final_message = transcript
            .iter()
            .rev()
            .find(|e| e.step_type == StepType::Message && !role_is_user(&e.role))
            .and_then(|e| match &e.content {
                EpisodeContent::Text(t) => Some(t.clone()),
                _ => None,
            });

        let artifacts: Vec<ArtifactSummary> = transcript
            .iter()
            .filter(|e| e.step_type == StepType::ArtifactEmitted)
            .filter_map(|e| match &e.content {
                EpisodeContent::Artifact {
                    name, media_type, ..
                } => Some(ArtifactSummary {
                    name: name.clone(),
                    media_type: media_type.clone(),
                }),
                _ => None,
            })
            .collect();

        let outcome = EpisodeOutcome {
            final_message,
            artifacts,
            citation_strings: intents
                .last()
                .map(|i| i.derived_citation_strings.clone())
                .unwrap_or_default(),
            token_summary: token_summary.clone(),
            duration: super::EpisodeDuration {
                active_ms,
                wait_ms,
                wall_clock_ms,
            },
        };

        let mut episode = Episode {
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            agent_id,
            ref_prefix,
            status,
            started_timestamp_ms: task_start_ms,
            duration: outcome.duration.clone(),
            token_summary,
            prior_context,
            goal,
            transcript,
            session_history: Vec::new(),
            intents,
            plans,
            outcome,
            drift_summary,
            drift_calls,
        };
        episode.session_history = assemble_session_history(
            &merged,
            &episode,
            &episode.ref_prefix,
            self.tool_registry.as_ref(),
            &projection_opts,
        );
        Ok((episode, merged))
    }
}

/// `SendDone` session line: render `cat -n` / range footer from graph-hydrated replay JSON (not the
/// episode ref table), so BAML session history matches the tool result backing `@N` even when wire
/// slots and episode `seq` diverge.
fn session_history_body_from_send_done_replay(
    payload: &serde_json::Value,
    archive_ref: &str,
    limit: usize,
) -> Option<String> {
    use baml_rt_tools::archive_read::PageLimit;
    format_session_read_body_from_json_value(payload, archive_ref, None, 0, PageLimit::new(limit))
}

fn assemble_session_history(
    merged: &[TimelineKind],
    episode: &Episode,
    ref_prefix: &EpisodeRefPrefix,
    tool_registry: &ToolRegistry,
    opts: &ProjectionRenderOptions,
) -> Vec<SessionHistoryLine> {
    use baml_rt_tools::{
        archive_read::{PageLimit, ShortRef},
        archive_refs::RefTable,
        prompt_projection::projection_history_pairs,
    };

    let vtable = super::archive::episode_ref_table_with_merged(episode, merged);
    let scratch = RefTable::new();

    let archive_reader = move |archive_ref_str: &str,
                               grep_str: Option<&str>,
                               offset: usize,
                               limit: usize|
          -> Option<String> {
        let short_ref = ShortRef::parse_loose(archive_ref_str)?;
        let entry = vtable.get(short_ref)?;
        Some(format_session_read_body_from_rendered(
            &entry.content,
            archive_ref_str,
            grep_str,
            offset,
            PageLimit::new(limit),
        ))
    };

    let mut out = Vec::new();
    for m in merged {
        match m {
            TimelineKind::Conv(item, _) => {
                // SendDone: when `send_done_replay_payload` is present, the read body is derived from
                // that JSON (same source the graph hydrated). Otherwise [`projection_history_pairs`]
                // resolves the archive via `archive_reader` over the episode ref table.
                if let ConversationItemContent::SessionStep(ss) = &item.content
                    && let SessionStepOp::SendDone {
                        archive_ref,
                        header,
                        ..
                    } = &ss.op
                    && let Some(payload) = ss.send_done_replay_payload.as_ref()
                    && let Some(body) = session_history_body_from_send_done_replay(
                        payload,
                        archive_ref.as_str(),
                        opts.send_done.get(),
                    )
                {
                    out.push(SessionHistoryLine {
                        role: item.role.clone(),
                        content: prefix_wire_citations_in_text(header, ref_prefix),
                    });
                    out.push(SessionHistoryLine {
                        role: "read".into(),
                        content: prefix_wire_citations_in_text(&body, ref_prefix),
                    });
                    continue;
                }
                if let Some(proj) = crate::provenance_item_to_projection_item(item.clone()) {
                    for (role, content) in projection_history_pairs(
                        &proj,
                        tool_registry,
                        &scratch,
                        Some(&archive_reader),
                        *opts,
                    ) {
                        out.push(SessionHistoryLine {
                            role,
                            content: prefix_wire_citations_in_text(&content, ref_prefix),
                        });
                    }
                }
            }
            TimelineKind::Status(s) => {
                let content = format!("{} → {}", s.old_status, s.new_status);
                out.push(SessionHistoryLine {
                    role: "system".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                });
            }
            TimelineKind::Artifact(a) => {
                let mt = a.media_type.as_deref().unwrap_or("?");
                let content = format!("artifact {} ({})", a.name, mt);
                out.push(SessionHistoryLine {
                    role: "agent".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                });
            }
        }
    }
    out
}

fn role_is_user(role: &str) -> bool {
    let r = role.to_ascii_lowercase();
    r.contains("user")
}

fn map_terminal_status(s: &str) -> TerminalStatus {
    let n = s.to_ascii_lowercase();
    match n.as_str() {
        "task_state_completed" | "completed" => TerminalStatus::Completed,
        "task_state_failed" | "failed" => TerminalStatus::Failed,
        "task_state_canceled" | "canceled" | "task_state_cancelled" | "cancelled" => {
            TerminalStatus::Canceled
        }
        "task_state_rejected" | "rejected" => TerminalStatus::Rejected,
        "in_progress" => TerminalStatus::Other("in_progress".to_string()),
        _ => TerminalStatus::Other(s.to_string()),
    }
}

/// Spacing between synthetic episode line timestamps (not wall clock).
const EPISODE_LINE_TICK_MS: i64 = 100;

/// Timeout for a single episode snapshot read (context-id lookup + graph export).
/// Tasks whose provenance graph is still being populated will hit this and surface
/// as 404 to the API layer, preventing long-running reads from contending with
/// concurrent agent writes in the embedded SurrealKV store.
const EPISODE_SNAPSHOT_TIMEOUT_SECS: u64 = 8;

/// First task status row event_order — conversation lines with lower order are "prior context".
fn conv_anchor_cutoff_u64(status_rows: &[StatusRow]) -> Option<u64> {
    let mut keys: Vec<u64> = status_rows
        .iter()
        .map(|s| s.event_order)
        .filter(|&k| k > 0)
        .collect();
    keys.sort_unstable();
    keys.first().copied()
}

const EXECUTION_SESSION_STEP_TOOL: &str = "a2a/execution_session_step";

fn parse_execution_session_plan_step(description: &str) -> Option<(String, String)> {
    let v: JsonValue = serde_json::from_str(description).ok()?;
    let plan_id = v.get("plan_id")?.as_str()?.to_string();
    let step_id = v.get("step_id")?.as_str()?.to_string();
    Some((plan_id, step_id))
}

/// When the synthetic `a2a/execution_session_step` tool result only records `citations: []`,
/// replace it with citations stored on the matching [`PlanStep`](crate::store) graph row (if any).
///
/// **Invariant:** expects each synthetic call to be immediately followed by its `ToolResult` entry
/// in the slice (same order as [`conv_item_to_entries`] emits).
fn enrich_execution_session_tool_outputs_with_plan_citations(
    entries: &mut [EpisodeEntry],
    step_cites: &HashMap<(String, String), Vec<String>>,
) {
    let mut i = 0usize;
    while i + 1 < entries.len() {
        if entries[i].step_type == StepType::ToolCall
            && entries[i + 1].step_type == StepType::ToolResult
        {
            let tool_key = match &entries[i].content {
                EpisodeContent::ToolInvocation {
                    tool_name,
                    description,
                } => {
                    if tool_name == EXECUTION_SESSION_STEP_TOOL
                        || tool_name.ends_with(EXECUTION_SESSION_STEP_TOOL)
                    {
                        parse_execution_session_plan_step(description)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some((plan_id, step_id)) = tool_key
                && let Some(cites) = step_cites.get(&(plan_id, step_id))
                && !cites.is_empty()
            {
                let out = &mut entries[i + 1];
                if let EpisodeContent::ToolOutput { lines, .. } = &mut out.content {
                    let joined = lines.join("\n");
                    if let Ok(mut val) = serde_json::from_str::<JsonValue>(&joined) {
                        let cites_empty = match val.get("citations") {
                            None => true,
                            Some(JsonValue::Array(a)) => a.is_empty(),
                            _ => false,
                        };
                        if cites_empty && let Some(obj) = val.as_object_mut() {
                            obj.insert(
                                "citations".to_string(),
                                JsonValue::Array(
                                    cites.iter().map(|s| JsonValue::String(s.clone())).collect(),
                                ),
                            );
                            *lines = vec![val.to_string()];
                        }
                    }
                }
            }
        }
        i += 1;
    }
}

/// Suppress the noisy `citations: []` payload on `a2a/execution_session_step` ToolResult
/// entries when no citation grounding was found after enrichment.
///
/// The entries are **kept** in the transcript so seq numbering remains stable and citation
/// refs in adjacent message entries (e.g. `#8`) continue to resolve. Only the ToolOutput
/// `lines` are cleared; the summary header and line/byte counts are updated to reflect the
/// empty content.
fn suppress_empty_execution_session_step_payload(entries: &mut [EpisodeEntry]) {
    let mut i = 0usize;
    while i + 1 < entries.len() {
        if entries[i].step_type != StepType::ToolCall
            || entries[i + 1].step_type != StepType::ToolResult
        {
            i += 1;
            continue;
        }
        let is_session_step = match &entries[i].content {
            EpisodeContent::ToolInvocation { tool_name, .. } => {
                tool_name == EXECUTION_SESSION_STEP_TOOL
                    || tool_name.ends_with(EXECUTION_SESSION_STEP_TOOL)
            }
            _ => false,
        };
        if !is_session_step {
            i += 1;
            continue;
        }
        // If citations are empty after enrichment, clear the payload so the `| citations: []`
        // line is not rendered — plan step status is already visible in the plans section.
        if let EpisodeContent::ToolOutput {
            lines,
            line_count,
            byte_count,
            ..
        } = &mut entries[i + 1].content
        {
            let joined = lines.join("\n");
            let citations_empty = serde_json::from_str::<JsonValue>(&joined)
                .ok()
                .and_then(|v| v.get("citations").cloned())
                .and_then(|c| c.as_array().cloned())
                .is_none_or(|a| a.is_empty());
            if citations_empty {
                lines.clear();
                *line_count = 0;
                *byte_count = 0;
            }
        }
        i += 1;
    }
}

fn compute_input_required_wait_ms(rows: &[StatusRow], task_start_ms: u64, terminal_ts: u64) -> u64 {
    const IR: &str = "TASK_STATE_INPUT_REQUIRED";
    let mut wait: u64 = 0;
    let mut i = 0usize;
    while i < rows.len() {
        if rows[i].new_status == IR {
            let t0 = rows[i].timestamp_ms.max(task_start_ms);
            let mut t1 = terminal_ts;
            for row in rows.iter().skip(i + 1) {
                if row.timestamp_ms > t0 {
                    t1 = row.timestamp_ms;
                    break;
                }
            }
            wait = wait.saturating_add(t1.saturating_sub(t0));
        }
        i += 1;
    }
    wait
}

fn tool_result_lines_from_json_value(
    v: &JsonValue,
    ref_prefix: &EpisodeRefPrefix,
) -> (Vec<String>, usize, usize) {
    let rendered = baml_rt_tools::archive_read::render_to_lines(v);
    let joined = rendered.lines().collect::<Vec<_>>().join("\n");
    let prefixed = prefix_wire_citations_in_text(&joined, ref_prefix);
    let lines: Vec<String> = prefixed.lines().map(str::to_string).collect();
    let line_count = lines.len();
    let byte_count: usize = lines.iter().map(|s| s.len().saturating_add(1)).sum();
    (lines, line_count, byte_count)
}

fn conv_item_to_entries(
    item: &ProvenanceConversationContextItem,
    elapsed_ms: i64,
    seq: &mut u32,
    ref_prefix: &EpisodeRefPrefix,
    projection_opts: &ProjectionRenderOptions,
) -> Result<Vec<EpisodeEntry>> {
    if !item.content.is_meaningful() {
        return Ok(Vec::new());
    }
    let anchor = item.activity_anchor.as_str().to_string();

    // Message entries carry citations directly from the graph (CITED edges on the
    // Message entity, resolved in the context reader). Emit them as a separate entry.
    if let ConversationItemContent::Message { text, citations } = &item.content {
        let ep_text = prefix_wire_citations_in_text(text, ref_prefix);
        let ep_cites: Vec<String> = citations
            .iter()
            .map(|c| prefix_wire_citations_in_text(c.as_str(), ref_prefix))
            .collect();
        *seq += 1;
        return Ok(vec![EpisodeEntry {
            seq: *seq,
            step_type: StepType::Message,
            role: item.role.clone(),
            elapsed_ms,
            content: EpisodeContent::Text(ep_text),
            activity_anchor: anchor,
            citation_strings: ep_cites,
        }]);
    }

    let (step_type, content) = match &item.content {
        ConversationItemContent::Message { .. } => unreachable!("handled above"),
        ConversationItemContent::ToolCall(tc) => (
            StepType::ToolCall,
            EpisodeContent::ToolInvocation {
                tool_name: tc.tool_name.clone(),
                description: serde_json::to_string(&tc.args).unwrap_or_default(),
            },
        ),
        ConversationItemContent::ToolResult(tr) => {
            let (summary, lines, lc, bc) = match &tr.outcome {
                ToolOutcome::Result(v) => {
                    let (lines, lc, bc) = tool_result_lines_from_json_value(v, ref_prefix);
                    (format!("{} result", tr.tool_name), lines, lc, bc)
                }
                ToolOutcome::Error(v) => {
                    let (lines, lc, bc) = tool_result_lines_from_json_value(v, ref_prefix);
                    (format!("{} error", tr.tool_name), lines, lc, bc)
                }
                ToolOutcome::StatusOnly => return Ok(Vec::new()),
            };
            (
                StepType::ToolResult,
                EpisodeContent::ToolOutput {
                    tool_name: tr.tool_name.clone(),
                    summary,
                    line_count: lc,
                    byte_count: bc,
                    lines,
                },
            )
        }
        ConversationItemContent::SessionStep(ss) => match &ss.op {
            SessionStepOp::Open => (
                StepType::ToolCall,
                EpisodeContent::ToolInvocation {
                    tool_name: ss.tool_name.clone(),
                    description: format!("{} session opened", ss.tool_name),
                },
            ),
            SessionStepOp::SendDone {
                header,
                archive_ref,
                ..
            } => {
                let header_prefixed = prefix_wire_citations_in_text(header, ref_prefix);
                if let Some(payload) = &ss.send_done_replay_payload {
                    let send_limit = projection_opts.send_done.get();
                    if let Some(raw_body) = session_history_body_from_send_done_replay(
                        payload,
                        archive_ref.as_str(),
                        send_limit,
                    ) {
                        let body = prefix_wire_citations_in_text(&raw_body, ref_prefix);
                        let lines: Vec<String> = body.lines().map(str::to_string).collect();
                        let line_count = lines.len();
                        let byte_count: usize =
                            lines.iter().map(|s| s.len().saturating_add(1)).sum();
                        let summary = lines
                            .first()
                            .cloned()
                            .unwrap_or_else(|| format!("cat -n {}", archive_ref.as_str()));
                        *seq += 1;
                        let header_entry = EpisodeEntry {
                            seq: *seq,
                            step_type: StepType::ToolResult,
                            role: item.role.clone(),
                            elapsed_ms,
                            content: EpisodeContent::ToolOutput {
                                tool_name: ss.tool_name.clone(),
                                summary: header_prefixed.clone(),
                                line_count: 1,
                                byte_count: header_prefixed.len(),
                                lines: vec![header_prefixed.clone()],
                            },
                            activity_anchor: anchor.clone(),
                            citation_strings: Vec::new(),
                        };
                        *seq += 1;
                        let body_entry = EpisodeEntry {
                            seq: *seq,
                            step_type: StepType::ToolRead,
                            role: "read".into(),
                            elapsed_ms,
                            content: EpisodeContent::ToolOutput {
                                tool_name: ss.tool_name.clone(),
                                summary,
                                line_count,
                                byte_count,
                                lines,
                            },
                            activity_anchor: anchor,
                            citation_strings: Vec::new(),
                        };
                        return Ok(vec![header_entry, body_entry]);
                    }
                }
                (
                    StepType::ToolResult,
                    EpisodeContent::ToolOutput {
                        tool_name: ss.tool_name.clone(),
                        summary: header_prefixed.clone(),
                        line_count: 1,
                        byte_count: header_prefixed.len(),
                        lines: vec![header_prefixed],
                    },
                )
            }
            SessionStepOp::Read {
                archive_ref,
                grep,
                offset,
                limit,
            } => {
                if let Some(raw_lines) = ss.read_replay_lines.as_ref().filter(|l| !l.is_empty()) {
                    let joined = raw_lines.join("\n");
                    let body = prefix_wire_citations_in_text(&joined, ref_prefix);
                    let lines: Vec<String> = body.lines().map(str::to_string).collect();
                    let line_count = lines.len();
                    let byte_count: usize = lines.iter().map(|s| s.len().saturating_add(1)).sum();
                    let summary = lines.first().cloned().unwrap_or_else(|| {
                        match grep.as_deref().filter(|g| !g.is_empty()) {
                            Some(g) => format!(
                                "grep {archive_ref} pattern={g:?} offset={offset} limit={limit}"
                            ),
                            None => format!("cat -n {archive_ref} offset={offset} limit={limit}"),
                        }
                    });
                    *seq += 1;
                    return Ok(vec![EpisodeEntry {
                        seq: *seq,
                        step_type: StepType::ToolRead,
                        role: "read".into(),
                        elapsed_ms,
                        content: EpisodeContent::ToolOutput {
                            tool_name: ss.tool_name.clone(),
                            summary,
                            line_count,
                            byte_count,
                            lines,
                        },
                        activity_anchor: anchor,
                        citation_strings: Vec::new(),
                    }]);
                }
                (
                    StepType::ToolRead,
                    EpisodeContent::ToolInvocation {
                        tool_name: ss.tool_name.clone(),
                        description: prefix_wire_citations_in_text(
                            &format!(
                                "read {archive_ref} grep={grep:?} offset={offset} limit={limit}"
                            ),
                            ref_prefix,
                        ),
                    },
                )
            }
        },
    };

    *seq += 1;
    Ok(vec![EpisodeEntry {
        seq: *seq,
        step_type,
        role: item.role.clone(),
        elapsed_ms,
        content,
        activity_anchor: anchor,
        citation_strings: Vec::new(),
    }])
}

fn status_to_entry(row: &StatusRow, elapsed_ms: i64, seq: &mut u32) -> Result<EpisodeEntry> {
    *seq += 1;
    Ok(EpisodeEntry {
        seq: *seq,
        step_type: StepType::StatusTransition,
        role: "system".to_string(),
        elapsed_ms,
        content: EpisodeContent::StatusChange {
            old: row.old_status.clone(),
            new: row.new_status.clone(),
            message: None,
        },
        activity_anchor: row.activity_anchor.clone(),
        citation_strings: Vec::new(),
    })
}

fn artifact_to_entry(row: &ArtifactRow, elapsed_ms: i64, seq: &mut u32) -> Result<EpisodeEntry> {
    *seq += 1;
    Ok(EpisodeEntry {
        seq: *seq,
        step_type: StepType::ArtifactEmitted,
        role: "agent".to_string(),
        elapsed_ms,
        content: EpisodeContent::Artifact {
            name: row.name.clone(),
            media_type: row.media_type.clone(),
            size_bytes: None,
        },
        activity_anchor: row.activity_anchor.clone(),
        citation_strings: Vec::new(),
    })
}
