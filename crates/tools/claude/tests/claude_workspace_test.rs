// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use baml_rt_tools_claude::AgentWorkspaceRegistry;

#[tokio::test]
async fn workspace_resolution_uses_default_and_named_subdirectories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let registry = AgentWorkspaceRegistry::new(temp.path().to_path_buf());

    let (default_name, default_path) = registry
        .resolve_workspace("agent-1", None)
        .await
        .expect("default workspace");
    assert_eq!(default_name, "default");
    let expected_default = std::fs::canonicalize(temp.path().join("agent-1").join("default"))
        .expect("default workspace dir exists");
    assert_eq!(
        default_path, expected_default,
        "default workspace must be base/agent/default"
    );
    assert!(
        default_path.exists(),
        "default workspace dir must be created"
    );

    let (named, named_path) = registry
        .resolve_workspace("agent-1", Some("project-a"))
        .await
        .expect("named workspace");
    assert_eq!(named, "project-a");
    let expected_named = std::fs::canonicalize(temp.path().join("agent-1").join("project-a"))
        .expect("named workspace dir exists");
    assert_eq!(
        named_path, expected_named,
        "named workspace must be base/agent/workspace"
    );
    assert!(named_path.exists(), "named workspace dir must be created");
}

#[tokio::test]
async fn workspace_registry_isolates_agents_for_same_workspace_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let registry = Arc::new(AgentWorkspaceRegistry::new(temp.path().to_path_buf()));

    let (_, a_default) = registry
        .resolve_workspace("agent-a", Some("default"))
        .await
        .expect("agent-a");
    let (_, b_default) = registry
        .resolve_workspace("agent-b", Some("default"))
        .await
        .expect("agent-b");

    assert_ne!(
        a_default, b_default,
        "agents must never share workspace paths"
    );
    let expected_a = std::fs::canonicalize(temp.path().join("agent-a").join("default"))
        .expect("agent-a default dir exists");
    let expected_b = std::fs::canonicalize(temp.path().join("agent-b").join("default"))
        .expect("agent-b default dir exists");
    assert_eq!(a_default, expected_a);
    assert_eq!(b_default, expected_b);
}
