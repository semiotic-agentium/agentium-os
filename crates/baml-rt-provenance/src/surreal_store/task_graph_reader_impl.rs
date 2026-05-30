// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Graph-backed [`TaskGraphReader`] implementation for
//! [`SurrealProvenanceStore`].
//!
//! Every read flows through the typed metamodel surface
//! ([`GraphQuery`] / [`EdgeProjection`]); task materialization is derived
//! from graph structure rather than from mirror tables.
//!
//! ## Query budget
//!
//! [`Self::hydrate_batch`] issues a fixed number of round-trips
//! independent of the input size:
//!
//! 1. `Task` nodes by id (batch).
//! 2. `A2A_TASK_MESSAGE` edges from the task ids → message ids.
//! 3. `Message` nodes by id (batch, ordered by `prov_time`).
//! 4. `A2A_TASK_ARTIFACT` edges from the task ids → artifact ids.
//! 5. `Artifact` nodes by id (batch).
//! 6. `WAS_LAST_TRANSITIONED_TO` head-pointer edges → `TaskState`
//!    ids (single indexed-edge IN-list lookup; no `ORDER BY`).
//! 7. `TaskState` nodes by id (batch).
//!
//! The head-pointer lookup in stage 6 collapses latest-state resolution
//! to a single indexed edge hop.

use std::collections::HashMap;

use async_trait::async_trait;
use baml_rt_core::ids::{ActivityAnchorId, ArtifactId, ContextId, ExternalId, MessageId, TaskId};
use futures_util::stream::{self, BoxStream};
use serde_json::Value;

use super::SurrealProvenanceStore;
use crate::{
    error::ProvenanceError,
    metamodel::{
        A2ATaskStateProps, ContextNodeId, EdgeProjection, GraphQuery, NonEmptyString,
        ScopedTaskRef, SemanticEdge, SortDir, SortKey, TaskExecutionNodeId, TaskNodeId,
        TaskStatusKind, labels,
    },
    task_graph_reader::{
        ArtifactRef, HydratedTask, MessageRef, ReplayError, TaskGraphReader, TaskReplayCursor,
        TaskReplayEvent,
    },
    vocabulary::{a2a, message_directions},
};

#[async_trait]
impl TaskGraphReader for SurrealProvenanceStore {
    async fn resolve_scoped(
        &self,
        ctx: &ContextId,
        task_id: &TaskId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError> {
        let ctx_node = ContextNodeId::for_context_id(ctx);
        let task_node = TaskNodeId::for_task_id(task_id);

        // Single round-trip: typed `scoped_to_ctx` + `by_node_ids`
        // proves both (a) the Task node exists and (b) it is `SCOPED_TO`
        // `ctx` via the context-scope subquery emitted by
        // `ScopedToContext::scope_where_clause`. Anything else (no
        // node, or node scoped to a different context) returns zero
        // rows.
        let (sql, binds) = GraphQuery::<labels::Task, _>::new()
            .scoped_to_ctx(ctx_node.clone())
            .by_node_ids(&[task_node.as_str().to_string()])
            .into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(ScopedTaskRef::new_proven(ctx_node, task_node)))
    }

    async fn resolve_by_task_id(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError> {
        // Two typed reads:
        //
        // 1. Confirm the Task node exists (`by_node_ids`).
        // 2. Walk the `SCOPED_TO` edge from the Task to the owning
        //    Context node (typed `EdgeProjection`).
        //
        // The `SCOPED_TO` edge is written by the normalizer for every
        // Task (`ensure_task_entity` + scope-attribution write); a
        // missing edge means the graph-write path is broken and we
        // return `Ok(None)` rather than falling back to a string
        // heuristic.
        let task_node = TaskNodeId::for_task_id(task_id);
        let (task_sql, task_binds) = GraphQuery::<labels::Task, _>::new()
            .all()
            .by_node_ids(&[task_node.as_str().to_string()])
            .into_surreal();
        let task_rows = self.execute_typed_query(&task_sql, &task_binds).await?;
        if task_rows.is_empty() {
            return Ok(None);
        }
        // The Context node label (`"Context"` on disk) does not have a
        // typed `labels::*` marker, so the `with_to_label::<...>()`
        // filter is intentionally omitted; the typed projection still
        // pins the edge by `rel_type` and `from_id`.
        let (scope_sql, scope_binds) = EdgeProjection::for_edge(SemanticEdge::ScopedTo)
            .from_id_in(&[task_node.as_str().to_string()])
            .into_surreal();
        let scope_rows = self.execute_typed_query(&scope_sql, &scope_binds).await?;
        let Some(ctx_node_str) = scope_rows
            .into_iter()
            .find_map(|r| r.get("to_id").and_then(Value::as_str).map(str::to_string))
        else {
            return Ok(None);
        };
        let ctx_node = ContextNodeId::new(ctx_node_str);
        Ok(Some(ScopedTaskRef::new_proven(ctx_node, task_node)))
    }

    async fn hydrate(
        &self,
        scoped: ScopedTaskRef,
        history_cap: Option<usize>,
    ) -> Result<HydratedTask, ProvenanceError> {
        let mut out = self.hydrate_batch(&[scoped], history_cap).await?;
        // `hydrate_batch` always returns one entry per input scoped ref,
        // even when the underlying graph reads come back empty.
        Ok(out.swap_remove(0))
    }

    async fn hydrate_batch(
        &self,
        scoped: &[ScopedTaskRef],
        history_cap: Option<usize>,
    ) -> Result<Vec<HydratedTask>, ProvenanceError> {
        if scoped.is_empty() {
            return Ok(Vec::new());
        }

        let task_node_ids: Vec<String> = scoped
            .iter()
            .map(|s| s.task_node_id().to_string())
            .collect();

        // Stage 1: load Task nodes themselves. Used only to confirm
        // existence and to lift the wire `(ContextId, TaskId)` from the
        // canonical node-id encoding (we already trust the
        // `ScopedTaskRef`s, so this stage degrades to a confirmation
        // read; absent rows surface as `HydratedTask` with no edges
        // attached).
        let (task_sql, task_binds) = GraphQuery::<labels::Task, _>::new()
            .all()
            .by_node_ids(&task_node_ids)
            .into_surreal();
        let _task_rows = self.execute_typed_query(&task_sql, &task_binds).await?;

        // Stage 2 + 3: messages. Edge → ids → node hydration ordered by
        // prov_time so the message slice is wire-stable.
        let (msg_edge_sql, msg_edge_binds) = EdgeProjection::for_edge(SemanticEdge::A2aTaskMessage)
            .from_id_in(&task_node_ids)
            .with_to_label::<labels::Message>()
            .into_surreal();
        let msg_edge_rows = self
            .execute_typed_query(&msg_edge_sql, &msg_edge_binds)
            .await?;

        let mut messages_by_task: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_message_ids: Vec<String> = Vec::new();
        for row in &msg_edge_rows {
            let from_id = row.get("from_id").and_then(Value::as_str).unwrap_or("");
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or("");
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            messages_by_task
                .entry(from_id.to_string())
                .or_default()
                .push(to_id.to_string());
            all_message_ids.push(to_id.to_string());
        }
        all_message_ids.sort();
        all_message_ids.dedup();

        let messages_by_node = if all_message_ids.is_empty() {
            HashMap::<String, MessageNodeRow>::new()
        } else {
            let (m_sql, m_binds) = GraphQuery::<labels::Message, _>::new()
                .all()
                .by_node_ids(&all_message_ids)
                .order_by(SortKey::ProvTime, SortDir::Asc)
                .into_surreal();
            let rows = self.execute_typed_query(&m_sql, &m_binds).await?;
            rows.into_iter()
                .filter_map(|row| MessageNodeRow::from_row(&row))
                .map(|r| (r.node_id.clone(), r))
                .collect()
        };

        // Stage 4 + 5: artifacts.
        let (art_edge_sql, art_edge_binds) =
            EdgeProjection::for_edge(SemanticEdge::A2aTaskArtifact)
                .from_id_in(&task_node_ids)
                .with_to_label::<labels::Artifact>()
                .into_surreal();
        let art_edge_rows = self
            .execute_typed_query(&art_edge_sql, &art_edge_binds)
            .await?;

        let mut artifacts_by_task: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_artifact_ids: Vec<String> = Vec::new();
        for row in &art_edge_rows {
            let from_id = row.get("from_id").and_then(Value::as_str).unwrap_or("");
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or("");
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            artifacts_by_task
                .entry(from_id.to_string())
                .or_default()
                .push(to_id.to_string());
            all_artifact_ids.push(to_id.to_string());
        }
        all_artifact_ids.sort();
        all_artifact_ids.dedup();

        let artifacts_by_node = if all_artifact_ids.is_empty() {
            HashMap::<String, ArtifactNodeRow>::new()
        } else {
            let (a_sql, a_binds) = GraphQuery::<labels::Artifact, _>::new()
                .all()
                .by_node_ids(&all_artifact_ids)
                .into_surreal();
            let rows = self.execute_typed_query(&a_sql, &a_binds).await?;
            rows.into_iter()
                .filter_map(|row| ArtifactNodeRow::from_row(&row))
                .map(|r| (r.node_id.clone(), r))
                .collect()
        };

        // Stage 6 + 7: latest TaskState head-pointer + TaskState
        // hydration. The head-pointer lookup is a single indexed edge
        // hop; no `ORDER BY`, no `LIMIT`.
        let (head_sql, head_binds) = EdgeProjection::for_edge(SemanticEdge::WasLastTransitionedTo)
            .from_id_in(&task_node_ids)
            .with_to_label::<labels::TaskState>()
            .into_surreal();
        let head_rows = self.execute_typed_query(&head_sql, &head_binds).await?;

        let mut head_state_by_task: HashMap<String, String> = HashMap::new();
        let mut all_state_ids: Vec<String> = Vec::new();
        for row in &head_rows {
            let from_id = row.get("from_id").and_then(Value::as_str).unwrap_or("");
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or("");
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            head_state_by_task.insert(from_id.to_string(), to_id.to_string());
            all_state_ids.push(to_id.to_string());
        }
        all_state_ids.sort();
        all_state_ids.dedup();

        let states_by_node = if all_state_ids.is_empty() {
            HashMap::<String, TaskStateNodeRow>::new()
        } else {
            let (s_sql, s_binds) = GraphQuery::<labels::TaskState, _>::new()
                .all()
                .by_node_ids(&all_state_ids)
                .into_surreal();
            let rows = self.execute_typed_query(&s_sql, &s_binds).await?;
            let mut out = HashMap::with_capacity(rows.len());
            for row in rows {
                let decoded = TaskStateNodeRow::from_row(&row)?;
                out.insert(decoded.node_id.clone(), decoded);
            }
            out
        };

        // Compose the wire-shaped HydratedTask per input scoped ref.
        let mut out = Vec::with_capacity(scoped.len());
        for sref in scoped {
            let task_key = sref.task_node_id().to_string();
            let task_id = task_id_from_node_id(sref.task());
            let context_id = context_id_from_node_id(sref.ctx());

            let mut messages: Vec<MessageRef> = messages_by_task
                .get(&task_key)
                .map(|ids| {
                    let mut msgs: Vec<MessageNodeRow> = ids
                        .iter()
                        .filter_map(|id| messages_by_node.get(id).cloned())
                        .collect();
                    msgs.sort_by(|left, right| left.cursor.cmp(&right.cursor));
                    msgs.into_iter()
                        .map(|m| MessageRef {
                            context_id: context_id.clone(),
                            message_id: m.message_id,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(cap) = history_cap {
                if cap == 0 {
                    messages.clear();
                } else if messages.len() > cap {
                    let split = messages.len() - cap;
                    messages = messages.split_off(split);
                }
            }

            let artifacts: Vec<ArtifactRef> = artifacts_by_task
                .get(&task_key)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| artifacts_by_node.get(id).cloned())
                        .map(|a| ArtifactRef {
                            task_id: task_id.clone(),
                            artifact_id: a.artifact_id,
                            artifact_type: a.artifact_type,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let status = head_state_by_task
                .get(&task_key)
                .and_then(|state_id| states_by_node.get(state_id))
                .map(|s| state_to_props(sref.task().clone(), s));

            out.push(HydratedTask {
                context_id,
                task_id,
                status,
                messages,
                artifacts,
            });
        }

        Ok(out)
    }

    async fn list_scoped(&self, ctx: &ContextId) -> Result<Vec<ScopedTaskRef>, ProvenanceError> {
        let ctx_node = ContextNodeId::for_context_id(ctx);
        let (sql, binds) = GraphQuery::<labels::Task, _>::new()
            .scoped_to_ctx(ctx_node.clone())
            .order_by(SortKey::ProvTime, SortDir::Desc)
            .into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        let mut out: Vec<ScopedTaskRef> = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(node_id) = row.get("node_id").and_then(Value::as_str) else {
                continue;
            };
            out.push(ScopedTaskRef::new_proven(
                ctx_node.clone(),
                TaskNodeId::new(node_id),
            ));
        }
        Ok(out)
    }

    async fn list_all(&self) -> Result<Vec<ScopedTaskRef>, ProvenanceError> {
        // Two typed reads:
        //
        // 1. Every Task node in `prov_node`, ordered by `prov_time DESC`.
        // 2. The `SCOPED_TO` edge from each task → its owning Context.
        //    Batched as a single IN-list emission so the round-trip
        //    count is independent of the number of tasks.
        let (task_sql, task_binds) = GraphQuery::<labels::Task, _>::new()
            .all()
            .order_by(SortKey::ProvTime, SortDir::Desc)
            .into_surreal();
        let task_rows = self.execute_typed_query(&task_sql, &task_binds).await?;
        let task_node_ids: Vec<String> = task_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(str::to_string))
            .collect();
        if task_node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let (scope_sql, scope_binds) = EdgeProjection::for_edge(SemanticEdge::ScopedTo)
            .from_id_in(&task_node_ids)
            .into_surreal();
        let scope_rows = self.execute_typed_query(&scope_sql, &scope_binds).await?;
        let mut task_to_ctx: HashMap<String, String> = HashMap::with_capacity(scope_rows.len());
        for row in scope_rows {
            let Some(from_id) = row.get("from_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(to_id) = row.get("to_id").and_then(Value::as_str) else {
                continue;
            };
            task_to_ctx
                .entry(from_id.to_string())
                .or_insert_with(|| to_id.to_string());
        }
        let mut out: Vec<ScopedTaskRef> = Vec::with_capacity(task_node_ids.len());
        for task_node_str in &task_node_ids {
            let Some(ctx_node_str) = task_to_ctx.get(task_node_str) else {
                continue;
            };
            out.push(ScopedTaskRef::new_proven(
                ContextNodeId::new(ctx_node_str.clone()),
                TaskNodeId::new(task_node_str.clone()),
            ));
        }
        Ok(out)
    }

    async fn latest_in_context(
        &self,
        ctx: &ContextId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError> {
        let ctx_node = ContextNodeId::for_context_id(ctx);
        let (sql, binds) = GraphQuery::<labels::Task, _>::new()
            .scoped_to_ctx(ctx_node.clone())
            .order_by(SortKey::ProvTime, SortDir::Desc)
            .paginate(0, 1)
            .into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let Some(node_id) = row.get("node_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        Ok(Some(ScopedTaskRef::new_proven(
            ctx_node,
            TaskNodeId::new(node_id),
        )))
    }

    async fn latest_state(
        &self,
        scoped: ScopedTaskRef,
    ) -> Result<Option<A2ATaskStateProps>, ProvenanceError> {
        let task_node_id = scoped.task_node_id().to_string();
        let (head_sql, head_binds) = EdgeProjection::for_edge(SemanticEdge::WasLastTransitionedTo)
            .from_id_in(&[task_node_id])
            .with_to_label::<labels::TaskState>()
            .into_surreal();
        let head_rows = self.execute_typed_query(&head_sql, &head_binds).await?;
        let Some(state_id) = head_rows
            .into_iter()
            .filter_map(|r| {
                r.get("to_id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            })
            .next()
        else {
            return Ok(None);
        };

        let (s_sql, s_binds) = GraphQuery::<labels::TaskState, _>::new()
            .all()
            .by_node_ids(&[state_id])
            .into_surreal();
        let rows = self.execute_typed_query(&s_sql, &s_binds).await?;
        let Some(state_row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let parsed = TaskStateNodeRow::from_row(&state_row)?;
        Ok(Some(state_to_props(scoped.task().clone(), &parsed)))
    }

    async fn replay_since(
        &self,
        scoped: ScopedTaskRef,
        since: Option<TaskReplayCursor>,
    ) -> Result<BoxStream<'_, Result<TaskReplayEvent, ReplayError>>, ProvenanceError> {
        let ctx = context_id_from_node_id(scoped.ctx());
        let task = task_id_from_node_id(scoped.task());
        let task_node = scoped.task().clone();
        let task_exec = TaskExecutionNodeId::for_task_id(&task);

        let messages = self
            .query_replay_messages(task_node.clone(), ctx.clone(), since.as_ref())
            .await?;
        let artifacts = self
            .query_replay_artifacts(task_node.clone(), task.clone(), since.as_ref())
            .await?;
        let statuses = self
            .query_replay_statuses(task_exec, scoped.task().clone(), since.as_ref())
            .await?;

        let mut frames = Vec::with_capacity(messages.len() + artifacts.len() + statuses.len());
        frames.extend(messages);
        frames.extend(artifacts);
        frames.extend(statuses);
        frames.sort_by(|left, right| left.cursor().cmp(right.cursor()));

        let stream = stream::iter(frames.into_iter().map(Ok::<_, ReplayError>));
        Ok(Box::pin(stream))
    }
}

impl SurrealProvenanceStore {
    /// Bind the JSON object produced by [`GraphQuery::into_surreal`] /
    /// [`EdgeProjection::into_surreal`] and execute the query.
    pub(crate) async fn execute_typed_query(
        &self,
        sql: &str,
        binds: &Value,
    ) -> Result<Vec<Value>, ProvenanceError> {
        let mut q = self.db().query(sql);
        if let Some(obj) = binds.as_object() {
            for (k, v) in obj {
                q = q.bind((k.clone(), v.clone()));
            }
        }
        let response = q.await.map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        super::check_and_take_zero(response, |e| ProvenanceError::Storage(Box::new(e)))
    }

    async fn query_replay_messages(
        &self,
        task_node: TaskNodeId,
        context_id: ContextId,
        since: Option<&TaskReplayCursor>,
    ) -> Result<Vec<TaskReplayEvent>, ProvenanceError> {
        let mut query = GraphQuery::<labels::Message, _>::new()
            .all()
            .for_task(task_node)
            .order_by(SortKey::EventOrder, SortDir::Asc);
        if let Some(cursor) = since {
            query = query.after_event_cursor(cursor.event_order(), cursor.anchor());
        }
        let (sql, binds) = query.into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(decoded) = MessageNodeRow::from_row(&row) else {
                continue;
            };
            let event = match decoded.direction.as_str() {
                message_directions::RECEIVED => TaskReplayEvent::MessageReceived {
                    message: MessageRef {
                        context_id: context_id.clone(),
                        message_id: decoded.message_id,
                    },
                    cursor: decoded.cursor,
                },
                message_directions::SENT => TaskReplayEvent::MessageSent {
                    message: MessageRef {
                        context_id: context_id.clone(),
                        message_id: decoded.message_id,
                    },
                    cursor: decoded.cursor,
                },
                _ => continue,
            };
            out.push(event);
        }
        Ok(out)
    }

    async fn query_replay_artifacts(
        &self,
        task_node: TaskNodeId,
        task_id: TaskId,
        since: Option<&TaskReplayCursor>,
    ) -> Result<Vec<TaskReplayEvent>, ProvenanceError> {
        let mut query = GraphQuery::<labels::Artifact, _>::new()
            .all()
            .for_task(task_node)
            .order_by(SortKey::EventOrder, SortDir::Asc);
        if let Some(cursor) = since {
            query = query.after_event_cursor(cursor.event_order(), cursor.anchor());
        }
        let (sql, binds) = query.into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(decoded) = ArtifactNodeRow::from_row(&row) else {
                continue;
            };
            out.push(TaskReplayEvent::ArtifactGenerated {
                artifact: ArtifactRef {
                    task_id: task_id.clone(),
                    artifact_id: decoded.artifact_id,
                    artifact_type: decoded.artifact_type,
                },
                cursor: decoded.cursor,
            });
        }
        Ok(out)
    }

    async fn query_replay_statuses(
        &self,
        task_exec: TaskExecutionNodeId,
        task_node: TaskNodeId,
        since: Option<&TaskReplayCursor>,
    ) -> Result<Vec<TaskReplayEvent>, ProvenanceError> {
        let mut query = GraphQuery::<labels::TaskState, _>::new()
            .all()
            .for_task_execution(task_exec)
            .order_by(SortKey::EventOrder, SortDir::Asc);
        if let Some(cursor) = since {
            query = query.after_event_cursor(cursor.event_order(), cursor.anchor());
        }
        let (sql, binds) = query.into_surreal();
        let rows = self.execute_typed_query(&sql, &binds).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let decoded = TaskStateNodeRow::from_row(&row)?;
            out.push(TaskReplayEvent::StatusTransition {
                state: state_to_props(task_node.clone(), &decoded),
                cursor: decoded.cursor,
            });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Row decoders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct MessageNodeRow {
    node_id: String,
    message_id: MessageId,
    direction: String,
    cursor: TaskReplayCursor,
}

impl MessageNodeRow {
    fn from_row(row: &Value) -> Option<Self> {
        let node_id = row.get("node_id").and_then(Value::as_str)?.to_string();
        let props = row.get("props").and_then(Value::as_object)?;
        let mid = props
            .get("a2a_message_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        let message_id = MessageId::from_external(ExternalId::new(mid));
        let direction = props
            .get(prop_column(a2a::DIRECTION).as_str())
            .and_then(Value::as_str)
            .map(str::to_string)?;
        let cursor = cursor_from_props(props)?;
        Some(Self {
            node_id,
            message_id,
            direction,
            cursor,
        })
    }
}

#[derive(Debug, Clone)]
struct ArtifactNodeRow {
    node_id: String,
    artifact_id: Option<ArtifactId>,
    artifact_type: Option<String>,
    cursor: TaskReplayCursor,
}

impl ArtifactNodeRow {
    fn from_row(row: &Value) -> Option<Self> {
        let node_id = row.get("node_id").and_then(Value::as_str)?.to_string();
        let props = row.get("props").and_then(Value::as_object)?;
        let artifact_id = props
            .get(prop_column(a2a::ARTIFACT_ID).as_str())
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| ArtifactId::from_external(ExternalId::new(s)));
        let artifact_type = props
            .get(prop_column(a2a::ARTIFACT_TYPE).as_str())
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let cursor = cursor_from_props(props)?;
        Some(Self {
            node_id,
            artifact_id,
            artifact_type,
            cursor,
        })
    }
}

#[derive(Debug, Clone)]
struct TaskStateNodeRow {
    node_id: String,
    new_status: TaskStatusKind,
    old_status: Option<TaskStatusKind>,
    transitioned_at_ms: u64,
    cursor: TaskReplayCursor,
}

impl TaskStateNodeRow {
    fn from_row(row: &Value) -> Result<Self, ProvenanceError> {
        let node_id = row
            .get("node_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| task_state_row_error("<unknown>", "missing node_id"))?;
        let props = row
            .get("props")
            .and_then(Value::as_object)
            .ok_or_else(|| task_state_row_error(&node_id, "missing props object"))?;
        let new_status = parse_task_state_kind(
            props,
            a2a::TASK_STATE,
            a2a::INPUT_REQUIRED_PROMPT,
            a2a::REASON,
            &node_id,
        )?;
        let old_status = parse_optional_task_state_kind(
            props,
            a2a::OLD_STATUS,
            a2a::OLD_INPUT_REQUIRED_PROMPT,
            a2a::OLD_REASON,
            &node_id,
        )?;
        let transitioned_at_ms = props
            .get(prop_column(a2a::TASK_STATE_TIME).as_str())
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cursor = cursor_from_props(props)
            .ok_or_else(|| task_state_row_error(&node_id, "missing replay cursor"))?;
        Ok(Self {
            node_id,
            new_status,
            old_status,
            transitioned_at_ms,
            cursor,
        })
    }
}

fn state_to_props(task: TaskNodeId, row: &TaskStateNodeRow) -> A2ATaskStateProps {
    A2ATaskStateProps::new(
        task,
        row.new_status.clone(),
        row.old_status.clone(),
        row.transitioned_at_ms,
        row.cursor.anchor().clone(),
    )
}

/// Vocabulary keys are stored on disk with `:` rewritten to `_`
/// (mirrors `crate::surreal_sql::storage_safe_props`). The typed
/// surface emits the rewritten form in `WHERE` clauses; row decoders
/// must mirror it when reading `props.*` back.
fn prop_column(vocab_key: &str) -> String {
    vocab_key.replace(':', "_")
}

fn parse_status_kind_from_tag_only(raw: &str) -> Option<TaskStatusKind> {
    match raw {
        "TASK_STATE_SUBMITTED" | "submitted" => Some(TaskStatusKind::Submitted),
        "TASK_STATE_WORKING" | "working" => Some(TaskStatusKind::Working),
        "TASK_STATE_AUTH_REQUIRED" | "auth-required" | "auth_required" => {
            Some(TaskStatusKind::AuthRequired)
        }
        "TASK_STATE_COMPLETED" | "completed" => Some(TaskStatusKind::Completed),
        "TASK_STATE_CANCELED" | "canceled" | "cancelled" => Some(TaskStatusKind::Canceled),
        "TASK_STATE_REJECTED" | "rejected" => Some(TaskStatusKind::Rejected),
        _ => None,
    }
}

fn parse_task_state_kind(
    props: &serde_json::Map<String, Value>,
    state_key: &str,
    prompt_key: &str,
    reason_key: &str,
    node_id: &str,
) -> Result<TaskStatusKind, ProvenanceError> {
    let raw = props
        .get(prop_column(state_key).as_str())
        .and_then(Value::as_str)
        .ok_or_else(|| task_state_row_error(node_id, format!("missing {state_key}")))?;
    match raw {
        "TASK_STATE_INPUT_REQUIRED" | "input-required" | "input_required" => {
            let prompt = props
                .get(prop_column(prompt_key).as_str())
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    task_state_row_error(
                        node_id,
                        format!("{state_key}={raw} requires {prompt_key} payload"),
                    )
                })?;
            Ok(TaskStatusKind::InputRequired {
                prompt: prompt.to_string(),
            })
        }
        "TASK_STATE_FAILED" | "failed" => {
            let reason = props
                .get(prop_column(reason_key).as_str())
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    task_state_row_error(
                        node_id,
                        format!("{state_key}={raw} requires {reason_key} payload"),
                    )
                })?;
            let reason = NonEmptyString::new(reason.to_string()).map_err(|_| {
                task_state_row_error(
                    node_id,
                    format!("{reason_key} must be non-empty for {state_key}={raw}"),
                )
            })?;
            Ok(TaskStatusKind::Failed { reason })
        }
        _ => parse_status_kind_from_tag_only(raw).ok_or_else(|| {
            task_state_row_error(node_id, format!("unknown task status tag {raw:?}"))
        }),
    }
}

fn parse_optional_task_state_kind(
    props: &serde_json::Map<String, Value>,
    state_key: &str,
    prompt_key: &str,
    reason_key: &str,
    node_id: &str,
) -> Result<Option<TaskStatusKind>, ProvenanceError> {
    match props
        .get(prop_column(state_key).as_str())
        .and_then(Value::as_str)
    {
        Some(_) => {
            parse_task_state_kind(props, state_key, prompt_key, reason_key, node_id).map(Some)
        }
        None => Ok(None),
    }
}

fn task_state_row_error(node_id: &str, reason: impl Into<String>) -> ProvenanceError {
    ProvenanceError::InvalidEvent {
        activity_anchor: node_id.to_string(),
        reason: format!("invalid TaskState row: {}", reason.into()),
    }
}

fn cursor_from_props(props: &serde_json::Map<String, Value>) -> Option<TaskReplayCursor> {
    let event_order = props
        .get(prop_column(a2a::EVENT_ORDER).as_str())
        .and_then(Value::as_u64)?;
    let anchor = props
        .get(prop_column(a2a::ACTIVITY_ANCHOR).as_str())
        .and_then(Value::as_str)
        .map(ActivityAnchorId::from)?;
    TaskReplayCursor::try_new(event_order, anchor).ok()
}

/// Recover a wire `ContextId` from a `ContextNodeId`. The on-disk
/// encoding is `"context:<context_id>"` (see
/// `crate::id_semantics::context_entity_id_string`); the inverse is
/// purely syntactic.
fn context_id_from_node_id(ctx: &ContextNodeId) -> ContextId {
    ctx.to_context_id()
}

/// Recover a wire `TaskId` from a `TaskNodeId`. On-disk encoding is
/// `"task:<task_id>"` (`crate::id_semantics::task_entity_id_string_raw`).
fn task_id_from_node_id(task: &TaskNodeId) -> TaskId {
    task.to_task_id()
}
