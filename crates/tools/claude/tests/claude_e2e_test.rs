// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "claude-e2e")]

use std::sync::Arc;

use baml_rt_core::{
    context,
    ids::{AgentId, UuidId},
};
use baml_rt_tools::{ToolRegistry, ToolStep};
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};

#[tokio::test]
async fn claude_e2e_smoke_query() {
    let workspace_root = std::env::var("BAML_CLAUDE_WORKSPACES_BASE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("baml-claude-workspaces-e2e"));

    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_bundle(ClaudeSessionBundle::new(Arc::new(
            AgentWorkspaceRegistry::new(workspace_root),
        )))
        .expect("register claude bundle");

    let scope = baml_rt_core::context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000007").expect("uuid"),
    ));
    context::with_scope(scope.as_scope().clone(), async {
        let session_id = registry
            .open_session(
                "claude/dev",
                serde_json::json!({}),
                scope.as_scope().context_id(),
                scope.as_scope().agent_id(),
            )
            .await
            .expect("open");
        registry
            .session_send(
                &session_id,
                serde_json::json!({ "prompt": "What is 2 + 2?" }),
            )
            .await
            .expect("send");
        let step = registry
            .session_read(&session_id, serde_json::Value::Null)
            .await
            .expect("next");
        match step {
            ToolStep::Streaming { .. } | ToolStep::Done { .. } | ToolStep::Suspended { .. } => {}
            ToolStep::Error { error } => panic!("unexpected error: {}", error.message),
        }
    })
    .await;
}
