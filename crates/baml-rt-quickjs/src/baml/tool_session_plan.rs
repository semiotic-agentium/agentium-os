// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

// FSM execution for typed ToolSessionPlan fragments (Open/Send/Read/Finish/Abort).

use baml_rt_core::semantics::ErrorDisposition;
use baml_rt_tools::{ToolFailure, should_host_retry, tool_failure_to_baml_tool_execution_error};

use super::{BamlRuntimeManager, ToolSessionOp, ToolSessionPlan, manager_prelude::*, open_input};
use crate::{
    provenance_errors::map_archive_provenance_err,
    tool_session_handle::{SendResult, ToolSessionSendBlockingOutcome, send_signature},
};

/// Recoverable miss: LLM cited `@N` before it exists, wrong index, or stale session — return as
/// tool output instead of `Err` so the hop completes with `Outcome::Success` and the model can
/// self-correct (no provenance `PromptRejected` from tool execution failure).
fn archive_ref_not_found_tool_result(
    op: &'static str,
    archive_ref: baml_rt_tools::archive_read::ShortRef,
) -> Value {
    serde_json::json!({
        "status": "error",
        "op": op,
        "archive_ref": archive_ref.to_string(),
        "has_more": false,
        "next_offset": 0,
        "message": format!(
            "Archive ref {archive_ref} is not available in this session. \
             Use an @N handle from a prior tool Send in the transcript (only after that Send completes). \
             If the ref is stale or mistyped, pick the correct @N from the latest tool results and retry."
        ),
        "error": {
            "kind": "ArchiveRefNotFound",
            "code": "archive_ref_not_found",
            "disposition": "LlmCorrectable",
        },
    })
}

fn plan_send_tool_error_value(
    tool_name: &str,
    session_id: &ToolSessionId,
    failure: &ToolFailure,
) -> Value {
    serde_json::json!({
        "status": "error",
        "tool_name": tool_name,
        "session_id": session_id.to_string(),
        "error": {
            "kind": format!("{:?}", failure.kind),
            "message": failure.message,
            "retryable": bool::from(failure.retryability),
            "disposition": failure.classified.disposition,
            "code": failure.classified.code,
            "hint": failure.classified.hint,
            "retry_after_ms": failure.classified.retry_after_ms,
        },
        "result": Value::Null,
    })
}

/// Intent-level failure for a Send that could not be performed (synthetic open failed, or the tool
/// cannot be invoked directly). Surfaces as a tool result the model can act on — never a
/// lifecycle-phrased FSM error ("open before send", "session already has input"). `status: "error"`
/// flows through the step loop like Done, so the model sees the failure and can retry or route
/// elsewhere instead of the turn aborting.
fn send_unavailable_result(tool_name: &str, reason: &str) -> Value {
    serde_json::json!({
        "status": "error",
        "tool_name": tool_name,
        "has_more": false,
        "message": format!("Could not run {tool_name}: {reason}"),
        "error": {
            "kind": "ToolUnavailable",
            "code": "tool_unavailable",
            "disposition": "LlmCorrectable",
        },
        "result": Value::Null,
    })
}

fn send_done_json(send_result: &SendResult) -> Value {
    // `output` remains the archive header line; `result` carries typed tool JSON (e.g. calculator).
    serde_json::json!({
        "status": "done",
        "output": send_result.header,
        "archive_ref": send_result.archive_ref.to_string(),
        "result": send_result.output.clone(),
    })
}

fn duplicate_send_json(
    archive_ref: baml_rt_tools::archive_read::ShortRef,
    header: String,
) -> Value {
    serde_json::json!({
        "status": "duplicate",
        "op": "DuplicateSend",
        "archive_ref": archive_ref.to_string(),
        "output": header,
        "duplicate": true,
        "message": format!(
            "Identical Send already materialized as {archive_ref}; read it instead of re-issuing."
        ),
        "result": Value::Null,
    })
}

impl BamlRuntimeManager {
    /// Recover a stale session ID by looking up the live session for this scope + tool.
    async fn recover_stale_session(
        &self,
        plan_scope: &context::RuntimeScope,
        tool_name_str: &str,
        stale_session: &ToolSessionId,
        phase: &str,
    ) -> Result<ToolSessionId> {
        let refreshed = self
            .tool_session_handle()
            .find_existing_session_for_scope_and_tool(plan_scope, tool_name_str)
            .await;
        match refreshed {
            Some(ref fresh) if fresh != stale_session => {
                tracing::warn!(
                    tool = %tool_name_str,
                    stale_session_id = %stale_session,
                    refreshed_session_id = %fresh,
                    "Recovered stale session id for {} step via scope+tool lookup",
                    phase
                );
                Ok(fresh.clone())
            }
            _ => Err(BamlRtError::SessionLifecycle(
                SessionLifecycleError::ToolSessionNotFound {
                    session_id: stale_session.to_string(),
                },
            )),
        }
    }

    /// Live [`RefTable`] first; on miss, optional Surreal [`SurrealProvenanceStore`](baml_rt_provenance::SurrealProvenanceStore).
    async fn resolve_archive_entry(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        archive_ref: baml_rt_tools::archive_read::ShortRef,
    ) -> Result<baml_rt_tools::archive_refs::ArchiveEntry> {
        let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
            &self.state.archive_ref_tables,
            context_id.as_str(),
        );
        if let Some(cell) = ref_table.get(archive_ref) {
            return Ok(cell.value().clone());
        }
        if let Some(store) = self.state.archive_ref_store.as_ref() {
            let remote = store
                .archive_get_body(context_id, archive_ref)
                .await
                .map_err(map_archive_provenance_err)?;
            if let Some(e) = remote {
                ref_table.insert_at(archive_ref, e.clone());
                return Ok(e);
            }
        }
        Err(BamlRtError::InvalidArgument(format!(
            "archive ref {archive_ref} not found in session context"
        )))
    }

    pub(in crate::baml) async fn execute_archive_read_plan(
        &self,
        scope: &context::RuntimeScope,
        plan: ToolSessionPlan,
    ) -> Result<Value> {
        match plan.step {
            ToolSessionOp::SearchRead {
                archive_ref,
                offset,
                limit,
                grep,
                reason,
            } => {
                self.execute_archive_read_step(
                    scope,
                    "SearchRead",
                    archive_ref,
                    offset,
                    limit,
                    Some(grep),
                    reason,
                )
                .await
            }
            ToolSessionOp::PageRead {
                archive_ref,
                offset,
                limit,
                reason,
            } => {
                self.execute_archive_read_step(
                    scope,
                    "PageRead",
                    archive_ref,
                    offset,
                    limit,
                    None,
                    reason,
                )
                .await
            }
            other => Err(BamlRtError::InvalidArgument(format!(
                "global archive read executor received non-read op {}",
                other.op_name()
            ))),
        }
    }

    // Distinct grep vs no-grep paths share one implementation; keep args explicit for FSM clarity.
    #[expect(
        clippy::too_many_arguments,
        reason = "grep and no-grep paths share one impl; explicit args keep the FSM step readable"
    )]
    async fn execute_archive_read_step(
        &self,
        scope: &context::RuntimeScope,
        op_name: &'static str,
        archive_ref: baml_rt_tools::archive_read::ShortRef,
        offset: baml_rt_tools::archive_read::LineOffset,
        limit: baml_rt_tools::archive_read::PageLimit,
        grep: Option<baml_rt_tools::archive_read::GrepPattern>,
        reason: Option<String>,
    ) -> Result<Value> {
        tracing::debug!(
            archive_ref = %archive_ref,
            reason = ?reason,
            op = op_name,
            "FSM step: global archive read"
        );
        let entry = match self
            .resolve_archive_entry(scope.context_id(), archive_ref)
            .await
        {
            Ok(e) => e,
            Err(BamlRtError::InvalidArgument(_)) => {
                return Ok(archive_ref_not_found_tool_result(op_name, archive_ref));
            }
            Err(other) => return Err(other),
        };
        let archive_ref_str = archive_ref.to_string();
        let grep_raw = grep.as_ref().map(|g| g.pattern_text().to_string());
        let page = baml_rt_tools::archive_read::grep_paginate(
            &entry.content,
            grep.as_ref(),
            offset,
            limit,
        );
        let body = baml_rt_tools::archive_read::format_session_read_body_from_rendered(
            &entry.content,
            &archive_ref_str,
            grep_raw.as_deref(),
            offset.0,
            limit,
        );
        let header = entry.display_header(archive_ref);
        let read_output = serde_json::json!({
            "status": "done",
            "output": format!("{header}\n{body}"),
            "has_more": page.has_more,
            "next_offset": page.next_offset,
        });

        if let Some(emitter) = self.effect_emitter_for_tool_effects() {
            let op = match grep_raw.as_deref() {
                Some(grep_str) => baml_rt_core::bus::SessionStepOp::SearchRead {
                    archive_ref: archive_ref_str.clone(),
                    grep: grep_str.to_string(),
                    offset: offset.0,
                    limit: limit.get(),
                },
                None => baml_rt_core::bus::SessionStepOp::PageRead {
                    archive_ref: archive_ref_str.clone(),
                    offset: offset.0,
                    limit: limit.get(),
                },
            };
            let _ = emitter
                .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                    context_id: scope.context_id().clone(),
                    tool_name: entry.tool_name.clone(),
                    session_id: format!("global-archive-read:{}", scope.context_id()),
                    op,
                    task_id: scope.task_id_opt().cloned(),
                })
                .await;
        }

        Ok(read_output)
    }

    /// The plan is a sequence of typed `ToolSessionOp` operations that must follow FSM rules:
    /// - First operation must be Open
    /// - Subsequent operations must be Send/SearchRead/PageRead/Finish/Abort (after Open)
    /// - After Finish/Abort, session is closed
    pub(in crate::baml) async fn execute_tool_session_plan(
        &self,
        scope: &context::RuntimeScope,
        tool_name: baml_rt_tools::ToolName,
        plan: ToolSessionPlan,
        _source_baml_function: Option<&str>,
        _invocation_args: Option<&Value>,
    ) -> Result<Value> {
        let tool_name_str = tool_name.to_string();

        let first_step = plan.step;

        let plan_scope = scope.clone();
        let mut steps = vec![first_step];
        // Strict linear mode: exactly one fragment per invocation.
        // If this fragment is not Open, try to reuse an existing session.
        let mut session_id: Option<ToolSessionId> = self
            .tool_session_handle()
            .find_existing_session_for_scope_and_tool(&plan_scope, &tool_name_str)
            .await;
        if let Some(existing) = &session_id {
            tracing::debug!(
                tool_name = %tool_name_str,
                session_id = %existing,
                "Reusing existing session for single-fragment continuation",
            );
        }
        // True when the runtime synthesized the Open for a bare Send fragment. Scopes auto-finish
        // (below) to the entry-Send path: an explicitly model-opened session is the model's to Finish.
        let mut auto_opened = false;
        // Captured from the single auto-open metadata read below and reused at cleanup, so the
        // auto-finish decision can't disagree with the auto-open decision (and we avoid a second
        // registry lookup whose miss would silently skip the finish, leaking the session).
        let mut auto_open_one_shot_strict = false;
        if let Some(ToolSessionOp::Send { input, .. }) = steps.first() {
            let normalized = normalize_plan_input(input.clone())?;
            let signature = send_signature(&tool_name_str, &normalized);
            let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                &self.state.archive_ref_tables,
                plan_scope.context_id().as_str(),
            );
            if let Some((archive_ref, header)) = ref_table.find_by_send_signature(&signature) {
                tracing::warn!(
                    tool = %tool_name_str,
                    archive_ref = %archive_ref,
                    "FSM step: duplicate Send suppressed"
                );
                return Ok(duplicate_send_json(archive_ref, header));
            }
        }

        if let Some(first) = steps.first()
            && matches!(first, ToolSessionOp::Send { .. })
            && session_id.is_none()
        {
            let metadata = self.state.tool_registry.get_metadata(&tool_name_str);
            let can_auto_open = metadata
                .as_ref()
                .map(|m| open_input::schema_allows_empty_open_input(&m.open_input_schema))
                .unwrap_or(false);
            if !can_auto_open {
                // A bare Send to a tool that needs configuration to start. The builder should not
                // surface a direct Send for such tools, but if one slips through the model gets an
                // intent-level failure (route elsewhere), not a lifecycle-phrased FSM error.
                return Ok(send_unavailable_result(
                    &tool_name_str,
                    "this tool requires configuration before use and cannot be invoked directly",
                ));
            }
            // Same `metadata` clone drives the cleanup auto-finish gate (OneShot + Strict).
            auto_open_one_shot_strict = metadata
                .as_ref()
                .map(|m| {
                    m.capability == baml_rt_tools::ToolCapability::OneShot
                        && m.session_policy == baml_rt_tools::SessionPolicy::Strict
                })
                .unwrap_or(false);
            steps.insert(
                0,
                ToolSessionOp::Open {
                    initial_input: None,
                    reason: Some("auto-open for send fragment with no open session".to_string()),
                },
            );
            auto_opened = true;
        } else if let Some(first) = steps.first()
            && !matches!(first, ToolSessionOp::Open { .. })
            && !matches!(
                first,
                ToolSessionOp::SearchRead { .. } | ToolSessionOp::PageRead { .. }
            )
            && session_id.is_none()
        {
            // No live session for a non-Open step. Finish/Abort here is a no-op: the session was
            // already closed (e.g. auto-finished after a prior Send, or the agent finishes
            // defensively). Surface a closed status, never a lifecycle-phrased error. Anything else
            // is a malformed plan and gets an intent-level failure (not a hard FSM error).
            return match first {
                ToolSessionOp::Finish { .. } => Ok(serde_json::json!({ "status": "finished" })),
                ToolSessionOp::Abort { .. } => Ok(serde_json::json!({ "status": "aborted" })),
                _ => Ok(send_unavailable_result(
                    &tool_name_str,
                    "no open session for this step",
                )),
            };
        }

        let mut last_output: Option<Value> = None;
        // Set once a Send fragment reaches Done — gates the OneShot auto-finish after the loop.
        let mut send_completed = false;

        for step in steps {
            match step {
                ToolSessionOp::Open {
                    initial_input,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        reason = ?reason,
                        "FSM step: Open"
                    );
                    if let Some(existing) = session_id.as_ref() {
                        // Accept idempotent Open only for unit/null input. For non-empty input,
                        // treat Open as an explicit reopen request and rotate the session.
                        let unit_or_null_open = match initial_input.as_ref() {
                            None => true,
                            Some(v) if v.is_null() => true,
                            Some(Value::Object(map)) if map.is_empty() => true,
                            _ => false,
                        };
                        if unit_or_null_open {
                            tracing::warn!(
                                tool = %tool_name_str,
                                session_id = %existing,
                                reason = ?reason,
                                "FSM step Open while session already open with unit/null input; reusing existing session"
                            );
                            last_output = Some(serde_json::json!({
                                "status": "open",
                                "session_id": existing.to_string(),
                                "tool_name": tool_name_str
                            }));
                            continue;
                        }
                        let existing = existing.clone();
                        tracing::debug!(
                            tool = %tool_name_str,
                            previous_session_id = %existing,
                            reason = ?reason,
                            "FSM step Open with non-empty reopen input; aborting previous session before reopen"
                        );
                        self.tool_session_abort(
                            &existing,
                            Some(
                                "reopen requested by planner open with non-empty input".to_string(),
                            ),
                        )
                        .await?;
                    }
                    // For Open step, use initial_input if provided and non-null, otherwise empty object
                    let open_input = initial_input
                        .clone()
                        .filter(|v| !v.is_null())
                        .unwrap_or_else(open_input::empty_open_input);
                    let session = match self
                        .open_tool_session(&plan_scope, &tool_name_str, open_input)
                        .await
                    {
                        Ok(session) => session,
                        // A synthetic open (for a bare Send) that fails surfaces as an intent-level
                        // failed-Send result — no hard FSM error, and no session was created so
                        // there is nothing to clean up. An explicit model Open still propagates.
                        Err(e) if auto_opened => {
                            return Ok(send_unavailable_result(&tool_name_str, &e.to_string()));
                        }
                        Err(e) => return Err(e),
                    };
                    last_output = Some(serde_json::json!({
                        "status": "open",
                        "session_id": session.to_string(),
                        "tool_name": tool_name_str
                    }));
                    // Emit session step so conversation_context reflects Open synchronously.
                    if let Some(emitter) = self.effect_emitter_for_tool_effects() {
                        let _ = emitter
                            .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                context_id: plan_scope.context_id().clone(),
                                tool_name: tool_name_str.clone(),
                                session_id: session.to_string(),
                                op: baml_rt_core::bus::SessionStepOp::Open,
                                task_id: plan_scope.task_id_opt().cloned(),
                            })
                            .await;
                    }
                    session_id = Some(session.clone());
                }
                ToolSessionOp::Send { input, reason } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        reason = ?reason,
                        "FSM step: Send (blocking)"
                    );
                    let current_session = session_id.clone().ok_or_else(|| {
                        BamlRtError::InvalidArgument(
                            "send step before open: FSM requires Open before Send".to_string(),
                        )
                    })?;
                    let normalized = normalize_plan_input(input)?;
                    let signature = send_signature(&tool_name_str, &normalized);
                    let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                        &self.state.archive_ref_tables,
                        plan_scope.context_id().as_str(),
                    );
                    if let Some((archive_ref, header)) =
                        ref_table.find_by_send_signature(&signature)
                    {
                        tracing::warn!(
                            tool = %tool_name_str,
                            archive_ref = %archive_ref,
                            "FSM step: duplicate Send suppressed"
                        );
                        last_output = Some(duplicate_send_json(archive_ref, header));
                        send_completed = true;
                        continue;
                    }
                    let chunk_timeout = std::time::Duration::from_secs(300);
                    let mut active_session = current_session.clone();

                    let mut send_outcome = self
                        .tool_session_handle()
                        .tool_session_send_blocking(
                            &active_session,
                            normalized.clone(),
                            &plan_scope,
                            &self.state.archive_ref_tables,
                            chunk_timeout,
                        )
                        .await;

                    if let Err(BamlRtError::SessionLifecycle(
                        SessionLifecycleError::ToolSessionNotFound { .. },
                    )) = send_outcome
                    {
                        let fresh = self
                            .recover_stale_session(
                                &plan_scope,
                                &tool_name_str,
                                &active_session,
                                "Send",
                            )
                            .await?;
                        session_id = Some(fresh.clone());
                        active_session = fresh;
                        send_outcome = self
                            .tool_session_handle()
                            .tool_session_send_blocking(
                                &active_session,
                                normalize_plan_input(serde_json::Value::Null)?,
                                &plan_scope,
                                &self.state.archive_ref_tables,
                                chunk_timeout,
                            )
                            .await;
                    }

                    const MAX_HOST_RETRIES: u32 = 1u32;
                    let mut host_attempt: u32 = 0;
                    loop {
                        match send_outcome {
                            Ok(ToolSessionSendBlockingOutcome::Completed(send_result)) => {
                                last_output = Some(send_done_json(&send_result));
                                send_completed = true;
                                if let Some(emitter) = self.effect_emitter_for_tool_effects() {
                                    let _ = emitter
                                        .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                            context_id: plan_scope.context_id().clone(),
                                            tool_name: tool_name_str.clone(),
                                            session_id: active_session.to_string(),
                                            op: baml_rt_core::bus::SessionStepOp::SendDone {
                                                archive_ref: send_result.archive_ref.to_string(),
                                                header: send_result.header.clone(),
                                                informed_by: send_result
                                                    .informed_by_tool_activity_anchor
                                                    .as_str()
                                                    .to_string(),
                                            },
                                            task_id: plan_scope.task_id_opt().cloned(),
                                        })
                                        .await;
                                }
                                break;
                            }
                            Ok(ToolSessionSendBlockingOutcome::ToolFailed(failure)) => {
                                let retry_classified = should_host_retry(&failure.classified)
                                    && host_attempt < MAX_HOST_RETRIES;
                                if retry_classified {
                                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                    host_attempt += 1;
                                    send_outcome = self
                                        .tool_session_handle()
                                        .tool_session_send_blocking(
                                            &active_session,
                                            normalized.clone(),
                                            &plan_scope,
                                            &self.state.archive_ref_tables,
                                            chunk_timeout,
                                        )
                                        .await;
                                    continue;
                                }
                                match failure.classified.disposition {
                                    ErrorDisposition::Fatal => {
                                        return Err(tool_failure_to_baml_tool_execution_error(
                                            &failure,
                                        ));
                                    }
                                    ErrorDisposition::HostRetriable
                                    | ErrorDisposition::LlmCorrectable
                                    | ErrorDisposition::InformAndContinue => {
                                        last_output = Some(plan_send_tool_error_value(
                                            &tool_name_str,
                                            &active_session,
                                            &failure,
                                        ));
                                        break;
                                    }
                                }
                            }
                            Err(ref e)
                                if should_host_retry_baml_error(e)
                                    && host_attempt < MAX_HOST_RETRIES =>
                            {
                                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                host_attempt += 1;
                                send_outcome = self
                                    .tool_session_handle()
                                    .tool_session_send_blocking(
                                        &active_session,
                                        normalized.clone(),
                                        &plan_scope,
                                        &self.state.archive_ref_tables,
                                        chunk_timeout,
                                    )
                                    .await;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
                ToolSessionOp::SearchRead {
                    archive_ref,
                    offset,
                    limit,
                    grep,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        archive_ref = %archive_ref,
                        reason = ?reason,
                        "FSM step: SearchRead (archive line filter)"
                    );
                    let entry = match self
                        .resolve_archive_entry(plan_scope.context_id(), archive_ref)
                        .await
                    {
                        Ok(e) => e,
                        Err(BamlRtError::InvalidArgument(_)) => {
                            last_output =
                                Some(archive_ref_not_found_tool_result("SearchRead", archive_ref));
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    let page = baml_rt_tools::archive_read::grep_paginate(
                        &entry.content,
                        Some(&grep),
                        offset,
                        limit,
                    );
                    let formatted = baml_rt_tools::archive_read::format_cat_n(&page.lines);
                    let header = entry.display_header(archive_ref);
                    let line_range = if page.lines.is_empty() {
                        String::new()
                    } else {
                        let first = page
                            .lines
                            .first()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let last = page
                            .lines
                            .last()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let more = if page.has_more {
                            let o = page.next_offset;
                            let n = page.total_matched - page.next_offset;
                            format!(
                                "\n--- Not all lines shown — next step: SearchRead (grep) or PageRead {archive_ref} with offset={o} ({n} more lines — use offset={o} for next page) ---",
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "\nlines {first}-{last} of {}:\n{formatted}{more}",
                            page.total_matched
                        )
                    };
                    let read_output = serde_json::json!({
                        "status": "done",
                        "output": format!("{header}{line_range}"),
                        "has_more": page.has_more,
                        "next_offset": page.next_offset,
                    });
                    last_output = Some(read_output.clone());

                    if let Some(emitter) = self.effect_emitter_for_tool_effects() {
                        let grep_str = grep.pattern_text().to_string();
                        let read_args = serde_json::json!({
                            "op": "SearchRead",
                            "archive_ref": archive_ref.to_string(),
                            "grep": grep_str,
                            "offset": offset.0,
                            "limit": limit.get(),
                        });
                        let read_meta = baml_rt_core::bus::ToolEffectMetadata {
                            tool_name: tool_name_str.clone(),
                            function_name: None,
                            args: read_args,
                            metadata: crate::tool_execution::build_metadata_map_with_phase(
                                &plan_scope,
                                Some("search_read"),
                            ),
                            delegation_target: None,
                            tool_backend: None,
                            tool_digest: None,
                        };
                        if let Ok(token) = emitter
                            .start_tool(plan_scope.context_id().clone(), read_meta)
                            .await
                        {
                            let _ = token
                                .complete(
                                    emitter.as_ref(),
                                    0,
                                    baml_rt_core::semantics::Outcome::Success,
                                    Some(read_output),
                                )
                                .await;
                        }

                        if let Some(sid) = session_id.as_ref() {
                            let _ = emitter
                                .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                    context_id: plan_scope.context_id().clone(),
                                    tool_name: tool_name_str.clone(),
                                    session_id: sid.to_string(),
                                    op: baml_rt_core::bus::SessionStepOp::SearchRead {
                                        archive_ref: archive_ref.to_string(),
                                        grep: grep_str,
                                        offset: offset.0,
                                        limit: limit.get(),
                                    },
                                    task_id: plan_scope.task_id_opt().cloned(),
                                })
                                .await;
                        }
                    }
                }
                ToolSessionOp::PageRead {
                    archive_ref,
                    offset,
                    limit,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        archive_ref = %archive_ref,
                        reason = ?reason,
                        "FSM step: PageRead (archive paging)"
                    );
                    let entry = match self
                        .resolve_archive_entry(plan_scope.context_id(), archive_ref)
                        .await
                    {
                        Ok(e) => e,
                        Err(BamlRtError::InvalidArgument(_)) => {
                            last_output =
                                Some(archive_ref_not_found_tool_result("PageRead", archive_ref));
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    let page = baml_rt_tools::archive_read::grep_paginate(
                        &entry.content,
                        None,
                        offset,
                        limit,
                    );
                    let formatted = baml_rt_tools::archive_read::format_cat_n(&page.lines);
                    let header = entry.display_header(archive_ref);
                    let line_range = if page.lines.is_empty() {
                        String::new()
                    } else {
                        let first = page
                            .lines
                            .first()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let last = page
                            .lines
                            .last()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let more = if page.has_more {
                            let o = page.next_offset;
                            let n = page.total_matched - page.next_offset;
                            format!(
                                "\n--- Not all lines shown — next step: PageRead {archive_ref} with offset={o} ({n} more lines — use offset={o} for next page) ---",
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "\nlines {first}-{last} of {}:\n{formatted}{more}",
                            page.total_matched
                        )
                    };
                    let read_output = serde_json::json!({
                        "status": "done",
                        "output": format!("{header}{line_range}"),
                        "has_more": page.has_more,
                        "next_offset": page.next_offset,
                    });
                    last_output = Some(read_output.clone());

                    if let Some(emitter) = self.effect_emitter_for_tool_effects() {
                        let read_args = serde_json::json!({
                            "op": "PageRead",
                            "archive_ref": archive_ref.to_string(),
                            "offset": offset.0,
                            "limit": limit.get(),
                        });
                        let read_meta = baml_rt_core::bus::ToolEffectMetadata {
                            tool_name: tool_name_str.clone(),
                            function_name: None,
                            args: read_args,
                            metadata: crate::tool_execution::build_metadata_map_with_phase(
                                &plan_scope,
                                Some("page_read"),
                            ),
                            delegation_target: None,
                            tool_backend: None,
                            tool_digest: None,
                        };
                        if let Ok(token) = emitter
                            .start_tool(plan_scope.context_id().clone(), read_meta)
                            .await
                        {
                            let _ = token
                                .complete(
                                    emitter.as_ref(),
                                    0,
                                    baml_rt_core::semantics::Outcome::Success,
                                    Some(read_output),
                                )
                                .await;
                        }

                        if let Some(sid) = session_id.as_ref() {
                            let _ = emitter
                                .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                    context_id: plan_scope.context_id().clone(),
                                    tool_name: tool_name_str.clone(),
                                    session_id: sid.to_string(),
                                    op: baml_rt_core::bus::SessionStepOp::PageRead {
                                        archive_ref: archive_ref.to_string(),
                                        offset: offset.0,
                                        limit: limit.get(),
                                    },
                                    task_id: plan_scope.task_id_opt().cloned(),
                                })
                                .await;
                        }
                    }
                }
                ToolSessionOp::Finish { reason } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        reason = ?reason,
                        "FSM step: Finish"
                    );
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_finish(session).await?;
                        session_id = None;
                    }
                    // Preserve any Done output from a preceding Read step — Finish is
                    // session teardown, not a result-bearing operation.  Only write
                    // "finished" when there is no prior Done payload to return to the caller.
                    if last_output
                        .as_ref()
                        .and_then(|o| o.get("status"))
                        .and_then(serde_json::Value::as_str)
                        != Some("done")
                    {
                        last_output = Some(serde_json::json!({ "status": "finished" }));
                    }
                }
                ToolSessionOp::Abort { reason, .. } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        reason = ?reason,
                        "FSM step: Abort"
                    );
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_abort(session, reason).await?;
                        session_id = None;
                    }
                    last_output = Some(serde_json::json!({ "status": "aborted" }));
                }
            }
        }

        // Clean up a session the runtime auto-opened for a bare Send so it never dangles into the
        // next hop (a reused stale session would trigger a lifecycle error like "session already
        // has input" — which must never reach the model). An explicitly model-opened session is the
        // model's to Finish/Abort, so this is scoped to `auto_opened`.
        if auto_opened && let Some(session) = session_id.clone() {
            if send_completed {
                // Success. OneShot+Strict closes per the stateless-resend rule so each Send stands
                // alone; the Done output is preserved (status stays "done") and the loop continues
                // to a ReadOnlyFinish. MultiSend keeps the session open to accumulate further sends.
                // Streaming is excluded inherently (capability != OneShot). `@N` reads hit the
                // global ref table, not this session, so closing never blocks a later read.
                // `auto_open_one_shot_strict` is the snapshot taken at auto-open — same metadata
                // clone, so the finish decision can't drift from the open decision.
                if auto_open_one_shot_strict {
                    tracing::debug!(
                        tool = %tool_name_str,
                        session_id = %session,
                        "auto-finish after Send→Done; OneShot+Strict"
                    );
                    // Best-effort teardown: the Send already committed its effect and `last_output`
                    // holds the Done result. A failed close (store write error, session already
                    // gone) must NOT discard that result or surface as a lifecycle-phrased error —
                    // log and preserve the success. A genuinely lingering session is observable in
                    // logs/metrics; propagating here would *also* nuke the result, never fix it.
                    if let Err(e) = self.tool_session_finish(&session).await {
                        tracing::warn!(
                            tool = %tool_name_str,
                            session_id = %session,
                            error = %e,
                            "auto-finish teardown failed after successful Send; preserving Send result"
                        );
                    }
                }
            } else {
                // The auto-opened Send did not complete (failed and returned a failed-Send result).
                // Abort so no stale session lingers; the model already has the failure to act on and
                // its next Send auto-opens fresh. The abort is recorded in provenance with a reason.
                tracing::debug!(
                    tool = %tool_name_str,
                    session_id = %session,
                    "auto-abort after failed Send on auto-opened session"
                );
                // Best-effort, same as the success branch: `last_output` already carries the
                // failed-Send result the model acts on. A failed abort must not replace it with a
                // different hard error — log and preserve the intent-level failure.
                if let Err(e) = self
                    .tool_session_abort(
                        &session,
                        Some("auto-abort after failed Send on auto-opened session".to_string()),
                    )
                    .await
                {
                    tracing::warn!(
                        tool = %tool_name_str,
                        session_id = %session,
                        error = %e,
                        "auto-abort teardown failed after failed Send; preserving failed-Send result"
                    );
                }
            }
        }

        last_output.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "Tool session plan produced no output; expected at least one step to yield a result. \
                 This is a runtime invariant violation — every plan execution must produce a non-null tool_result."
                    .to_string(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent-level failure contract (G): a Send that cannot be performed surfaces as a tool result
    /// the model can act on (`status: "error"`, recoverable disposition), never a lifecycle-phrased
    /// FSM error. The loop treats `status: "error"` like Done, so the model sees it and recovers.
    #[test]
    fn send_unavailable_result_is_intent_level_not_lifecycle() {
        let v = send_unavailable_result("support/slack_notify", "service is unreachable");

        assert_eq!(v.get("status").and_then(Value::as_str), Some("error"));
        assert_eq!(
            v.get("error").and_then(|e| e.get("disposition")),
            Some(&Value::String("LlmCorrectable".to_string()))
        );

        let blob = v.to_string().to_lowercase();
        // Names the tool and the underlying reason (intent-level).
        assert!(blob.contains("support/slack_notify"));
        assert!(blob.contains("service is unreachable"));
        // Must not leak FSM/lifecycle phrasing to the model surface.
        for forbidden in [
            "open before send",
            "session already has input",
            "finish before send",
            "no open session",
            "fsm",
        ] {
            assert!(
                !blob.contains(forbidden),
                "failed-send result must not contain lifecycle phrasing {forbidden:?}: {v}"
            );
        }
    }
}
