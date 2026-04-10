// FSM execution for typed ToolSessionPlan fragments (Open/Send/Read/Finish/Abort).

use baml_rt_core::semantics::ErrorDisposition;
use baml_rt_observability::metrics;
use baml_rt_tools::{ToolFailure, should_host_retry, tool_failure_to_baml_tool_execution_error};

use super::{BamlRuntimeManager, ToolSessionOp, ToolSessionPlan, manager_prelude::*, open_input};
use crate::tool_session_handle::{SendResult, ToolSessionSendBlockingOutcome};

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

fn send_done_json(send_result: &SendResult) -> Value {
    serde_json::json!({
        "status": "done",
        "output": send_result.header,
        "archive_ref": send_result.archive_ref.to_string(),
        "result": send_result.output.clone(),
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

    /// The plan is a sequence of typed `ToolSessionOp` operations that must follow FSM rules:
    /// - First operation must be Open
    /// - Subsequent operations must be Send/Read/Finish/Abort (after Open)
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
        let session_tool_exec_started_at = std::time::Instant::now();
        let mut session_tool_exec_open_count: u64 = 0;
        let mut session_tool_exec_send_count: u64 = 0;
        let mut session_tool_exec_read_count: u64 = 0;
        let mut session_tool_exec_finish_count: u64 = 0;
        let mut session_tool_exec_abort_count: u64 = 0;
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
        if let Some(first) = steps.first()
            && matches!(
                first,
                ToolSessionOp::Send { .. } | ToolSessionOp::Read { .. }
            )
            && session_id.is_none()
        {
            let can_auto_open = self
                .state
                .tool_registry
                .get_metadata(&tool_name_str)
                .map(|m| open_input::schema_allows_empty_open_input(&m.open_input_schema))
                .unwrap_or(false);
            if !can_auto_open {
                return Err(BamlRtError::InvalidArgument(
                    "step rejected: no open session; strict auto-open is allowed only when tool open_input is empty/optional"
                        .to_string(),
                ));
            }
            steps.insert(
                0,
                ToolSessionOp::Open {
                    initial_input: None,
                    reason: Some("auto-open for send fragment with no open session".to_string()),
                },
            );
        } else if let Some(first) = steps.first()
            && !matches!(first, ToolSessionOp::Open { .. })
            && session_id.is_none()
        {
            return Err(BamlRtError::InvalidArgument(
                "session fragment rejected: no open session for non-Open step".to_string(),
            ));
        }

        let mut last_output: Option<Value> = None;

        for step in steps {
            match step {
                ToolSessionOp::Open {
                    initial_input,
                    reason,
                } => {
                    session_tool_exec_open_count += 1;
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
                        tracing::info!(
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
                        .and_then(|v| if v.is_null() { None } else { Some(v) })
                        .unwrap_or_else(open_input::empty_open_input);
                    let session = self
                        .open_tool_session(&plan_scope, &tool_name_str, open_input)
                        .await?;
                    last_output = Some(serde_json::json!({
                        "status": "open",
                        "session_id": session.to_string(),
                        "tool_name": tool_name_str
                    }));
                    // Emit session step so conversation_context reflects Open synchronously.
                    if let Some(emitter) = self.state.effect_emitter.as_ref() {
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
                    session_tool_exec_send_count += 1;
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
                                if let Some(emitter) = self.state.effect_emitter.as_ref() {
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
                ToolSessionOp::Read {
                    archive_ref,
                    offset,
                    limit,
                    grep,
                    reason,
                } => {
                    session_tool_exec_read_count += 1;
                    tracing::debug!(
                        tool = %tool_name_str,
                        archive_ref = %archive_ref,
                        reason = ?reason,
                        "FSM step: Read (archive deref)"
                    );
                    // Pure archive deref — no tool I/O. Look up the archived entry and paginate.
                    let context_id = plan_scope.context_id().as_str().to_string();
                    let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                        &self.state.archive_ref_tables,
                        &context_id,
                    );
                    let entry = ref_table.get(archive_ref).ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "Read step: archive ref {} not found in session context",
                            archive_ref
                        ))
                    })?;
                    let grep_text = grep
                        .as_ref()
                        .map(|g| g.pattern_text().trim().to_string())
                        .filter(|g| !g.is_empty());
                    let exhausted_key = format!(
                        "{context_id}|{archive_ref}|{}|{}|{}",
                        grep_text.as_deref().unwrap_or(""),
                        offset.0,
                        limit.get()
                    );

                    if self.state.exhausted_read_views.contains_key(&exhausted_key) {
                        let output = format!(
                            "{}\n--- read view already exhausted (archive_ref={}, grep={:?}, offset={}, limit={}) ---",
                            entry.display_header(archive_ref),
                            archive_ref,
                            grep_text,
                            offset.0,
                            limit.get()
                        );
                        let read_output = serde_json::json!({
                            "status": "done",
                            "output": output,
                            "has_more": false,
                            "next_offset": offset.0,
                        });
                        last_output = Some(read_output.clone());

                        tracing::info!(
                            tool = %tool_name_str,
                            archive_ref = %archive_ref,
                            grep = ?grep_text,
                            offset = offset.0,
                            limit = limit.get(),
                            "Read guard: skipped duplicate exhausted read view"
                        );

                        continue;
                    }

                    let page = baml_rt_tools::archive_read::grep_paginate(
                        &entry.content,
                        grep.as_ref(),
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
                            format!(
                                "\n--- {} more lines (Read @{} offset={} for next page) ---",
                                page.total_matched - page.next_offset,
                                archive_ref,
                                page.next_offset,
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
                    if !page.has_more {
                        self.state.exhausted_read_views.insert(exhausted_key, ());
                    }
                    last_output = Some(read_output.clone());

                    // Emit ToolStarted/ToolCompleted for the Read FSM step so the FE
                    // can display archive_ref, grep, offset as tool call args.
                    if let Some(emitter) = self.state.effect_emitter.as_ref() {
                        let grep_str = grep_text.clone();
                        let read_args = serde_json::json!({
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
                                Some("read"),
                            ),
                            delegation_target: None,
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

                        // Emit ToolSessionStep::Read only when a session is active, to
                        // avoid creating SessionStep entities with empty session_id strings.
                        if let Some(sid) = session_id.as_ref() {
                            let _ = emitter
                                .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                    context_id: plan_scope.context_id().clone(),
                                    tool_name: tool_name_str.clone(),
                                    session_id: sid.to_string(),
                                    op: baml_rt_core::bus::SessionStepOp::Read {
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
                ToolSessionOp::Finish { reason } => {
                    session_tool_exec_finish_count += 1;
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
                    session_tool_exec_abort_count += 1;
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

        let output = last_output.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "Tool session plan produced no output; expected at least one step to yield a result. \
                 This is a runtime invariant violation — every plan execution must produce a non-null tool_result."
                    .to_string(),
            )
        })?;

        let session_tool_exec_elapsed = session_tool_exec_started_at.elapsed();
        metrics::record_tool_session_plan_duration(&tool_name_str, session_tool_exec_elapsed);
        metrics::record_tool_session_plan_op(&tool_name_str, "open", session_tool_exec_open_count);
        metrics::record_tool_session_plan_op(&tool_name_str, "send", session_tool_exec_send_count);
        metrics::record_tool_session_plan_op(&tool_name_str, "read", session_tool_exec_read_count);
        metrics::record_tool_session_plan_op(
            &tool_name_str,
            "finish",
            session_tool_exec_finish_count,
        );
        metrics::record_tool_session_plan_op(
            &tool_name_str,
            "abort",
            session_tool_exec_abort_count,
        );

        tracing::info!(
            tool = %tool_name_str,
            context_id = %plan_scope.context_id().as_str(),
            message_id = %plan_scope.message_id().as_str(),
            session_tool_exec_latency_ms_total = session_tool_exec_elapsed.as_millis() as u64,
            session_tool_exec_open_count,
            session_tool_exec_send_count,
            session_tool_exec_read_count,
            session_tool_exec_finish_count,
            session_tool_exec_abort_count,
            "tool_session_plan: execution summary"
        );

        Ok(output)
    }
}
