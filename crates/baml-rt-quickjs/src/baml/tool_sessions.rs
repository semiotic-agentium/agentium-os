use super::{BamlRuntimeManager, manager_prelude::*};

impl BamlRuntimeManager {
    /// Execute a tool function by name with an explicit scope.
    ///
    /// Use this when you have a [`RuntimeScope`](context::RuntimeScope) in hand (e.g. in tests or
    /// at runtime boundaries). Runs the tool inside `context::with_scope(scope, ...)` so
    /// nested calls see the scope.
    pub async fn execute_tool(
        &self,
        scope: &context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        self.tool_execution_context()
            .execute_tool(scope, name, args)
            .await
    }

    /// Backward-compatible alias for explicit-scope tool execution.
    pub async fn execute_tool_with_scope(
        &self,
        scope: &context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        self.execute_tool(scope, name, args).await
    }

    /// List all registered tools
    pub async fn list_tools(&self) -> Vec<String> {
        self.state.tool_registry.list_tools()
    }

    pub async fn set_tool_allowlist(&self, allowlist: HashSet<String>) -> Result<()> {
        self.state
            .tool_registry
            .set_allowlist_from_strings(allowlist)?;
        Ok(())
    }

    pub async fn open_tool_session(
        &self,
        scope: &context::RuntimeScope,
        tool_name: &str,
        open_input: serde_json::Value,
    ) -> Result<ToolSessionId> {
        self.tool_session_handle()
            .open_tool_session(scope, tool_name, open_input)
            .await
    }

    pub async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        self.tool_session_handle()
            .tool_session_send(session_id, input)
            .await
    }

    pub async fn tool_session_read(
        &self,
        session_id: &ToolSessionId,
        input: Value,
    ) -> Result<ToolStep> {
        self.tool_session_handle()
            .tool_session_read(session_id, input)
            .await
    }

    pub async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        self.tool_session_handle()
            .tool_session_finish(session_id)
            .await
    }

    pub async fn tool_session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()> {
        self.tool_session_handle()
            .tool_session_abort(session_id, reason)
            .await
    }

    /// True when any tool session is open across all contexts.
    /// Used by the drain mechanism to avoid tearing down agents mid-tool-execution.
    pub fn has_any_open_tool_sessions(&self) -> bool {
        !self.tool_session_handle().tool_session_scopes.is_empty()
    }

    /// Number of open tool sessions for this context. Used by tests to assert no leak after teardown.
    pub async fn open_session_count_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> usize {
        self.tool_session_handle()
            .collect_session_ids_for_context(context_id)
            .await
            .len()
    }

    /// Number of open tool sessions for a specific task scope, or for the full context
    /// when `task_id` is `None`.
    pub async fn open_session_count_for_scope(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        task_id: Option<&baml_rt_core::ids::TaskId>,
    ) -> usize {
        let handle = self.tool_session_handle();
        match task_id {
            Some(task_id) => handle
                .collect_session_ids_for_task_scope(context_id, task_id)
                .await
                .len(),
            None => handle
                .collect_session_ids_for_context(context_id)
                .await
                .len(),
        }
    }

    /// Close all tool sessions for this context (teardown). Call when an invocation ends
    /// so sessions are not leaked. Best-effort: logs but does not fail on individual finish errors.
    pub async fn close_sessions_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> Result<()> {
        self.close_sessions_for_scope(context_id, None).await
    }

    /// Close tool sessions scoped to a specific task branch, or all sessions for the context
    /// when `task_id` is `None` (legacy/message-scope).
    ///
    /// **Task-scoped teardown:** When `task_id` is `Some`, only sessions whose scope matches
    /// *both* `context_id` and `task_id` are closed. This prevents parallel sibling branches
    /// from having their sessions torn down when one branch finalizes.
    pub async fn close_sessions_for_scope(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        task_id: Option<&baml_rt_core::ids::TaskId>,
    ) -> Result<()> {
        let handle = self.tool_session_handle();
        let to_close = match task_id {
            Some(tid) => {
                handle
                    .collect_session_ids_for_task_scope(context_id, tid)
                    .await
            }
            None => handle.collect_session_ids_for_context(context_id).await,
        };
        for id in &to_close {
            if let Err(e) = self.tool_session_finish(id).await {
                tracing::warn!(
                    session_id = %id,
                    context_id = %context_id,
                    task_id = ?task_id,
                    error = %e,
                    "Teardown: tool session finish failed",
                );
            }
        }
        if !to_close.is_empty() {
            tracing::debug!(
                context_id = %context_id,
                task_id = ?task_id,
                count = to_close.len(),
                "Teardown: closed tool sessions for scope",
            );
        }
        Ok(())
    }

    /// Get tool metadata (export-safe shape)
    pub async fn get_tool_metadata(&self, name: &str) -> Option<ToolFunctionMetadataExport> {
        self.state
            .tool_registry
            .get_metadata(name)
            .map(|metadata| ToolFunctionMetadataExport::from(&metadata))
    }

    pub async fn export_tool_metadata(&self) -> Vec<ToolFunctionMetadataExport> {
        self.state.tool_registry.export_metadata_records()
    }

    pub async fn write_tool_metadata(&self, path: &Path) -> Result<()> {
        let metadata = self.export_tool_metadata().await;
        let payload = serde_json::json!({ "tools": metadata });
        let content = serde_json::to_string_pretty(&payload).map_err(BamlRtError::Json)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        fs::write(path, content).map_err(BamlRtError::Io)?;
        Ok(())
    }

    pub async fn write_tool_typescript(&self, path: &Path) -> Result<()> {
        self.state.tool_registry.write_typescript_declarations(path)
    }

    pub async fn validate_tool_allowlist_registered(&self) -> Result<()> {
        self.state.tool_registry.validate_allowlist_registered()
    }
}
