// FSM execution for typed ToolSessionPlan fragments (Open/Send/Read/Finish/Abort).

use baml_rt_core::semantics::ErrorDisposition;
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
    #[allow(clippy::too_many_arguments)]
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
        let context_id = scope.context_id().as_str().to_string();
        let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
            &self.state.archive_ref_tables,
            &context_id,
        );
        let entry = ref_table
            .get(archive_ref)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "{op_name} step: archive ref {archive_ref} not found in session context"
                ))
            })?;
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

        if let Some(emitter) = self.state.effect_emitter.as_ref() {
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
                    session_id: format!("global-archive-read:{context_id}"),
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
        if let Some(first) = steps.first()
            && matches!(first, ToolSessionOp::Send { .. })
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
            && !matches!(
                first,
                ToolSessionOp::SearchRead { .. } | ToolSessionOp::PageRead { .. }
            )
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
                    let context_id = plan_scope.context_id().as_str().to_string();
                    let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                        &self.state.archive_ref_tables,
                        &context_id,
                    );
                    let entry = ref_table.get(archive_ref).ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "SearchRead step: archive ref {archive_ref} not found in session context"
                        ))
                    })?;
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

                    if let Some(emitter) = self.state.effect_emitter.as_ref() {
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
                    let context_id = plan_scope.context_id().as_str().to_string();
                    let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                        &self.state.archive_ref_tables,
                        &context_id,
                    );
                    let entry = ref_table.get(archive_ref).ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "PageRead step: archive ref {archive_ref} not found in session context"
                        ))
                    })?;
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

                    if let Some(emitter) = self.state.effect_emitter.as_ref() {
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

        last_output.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "Tool session plan produced no output; expected at least one step to yield a result. \
                 This is a runtime invariant violation — every plan execution must produce a non-null tool_result."
                    .to_string(),
            )
        })
    }
}
