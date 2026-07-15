// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conformance scenarios mirrored from `sc-review/plugin/tests/test_hooks.py`.

use std::sync::{Arc, OnceLock};

use baml_rt_core::{
    context::RuntimeScope,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_interceptor::{InterceptorDecision, ToolCallContext, ToolInterceptor};
use baml_rt_semiotic::{
    config::{SemioticConfig, SemioticMode, SemioticPolicy},
    gate::{AmbiguityAwareGate, GateAction, GatePolicy},
    global::set_global_semiotic_config,
    interceptor::SemioticToolInterceptor,
    schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact, Postcondition},
    store::GroundingStore,
    tier::Tier,
};
use baml_rt_tools::tools::ToolAccess;
use serde_json::json;

fn conformance_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn grounded_artifact() -> ParseArtifact {
    ParseArtifact {
        instruction: "Archive users inactive per agreed definition in prod db".into(),
        template: "agentic_execution".into(),
        env: EnvSignals {
            environment: "prod".into(),
            verb_class: "mutating".into(),
            reversible: true,
            external_visibility: false,
        },
        nodes: vec![
            Node {
                name: "OBJECT".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "users".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "schema users(id,last_login_at,status,deleted_at)".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "TARGET".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Index,
                    content: "prod app.db".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "ACTION".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "archive".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "set deleted_at=now".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "SCOPE".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Icon,
                    content: "last_login_at older than 90 days, per user answer".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "METHOD".into(),
                anchors: vec![],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "CRITERION".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Verify,
                    content: "rowcount(users where deleted_at set) >= 1".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
        ],
        covers: vec!["UPDATE users".into(), "app\\.db".into()],
        postconditions: vec![Postcondition {
            cmd: "true".into(),
            desc: "placeholder assertion".into(),
        }],
    }
}

fn trojan_artifact() -> ParseArtifact {
    let mut art = grounded_artifact();
    art.nodes[2] = Node {
        name: "ACTION".into(),
        anchors: vec![Anchor {
            sign: AnchorSign::Symbol,
            content: "clean up".into(),
            source: "user".into(),
        }],
        interpretations: vec!["hard_delete".into(), "soft_delete".into(), "archive".into()],
        trojan: Some("clean up".into()),
    };
    art.nodes[3] = Node {
        name: "SCOPE".into(),
        anchors: vec![Anchor {
            sign: AnchorSign::Symbol,
            content: "inactive".into(),
            source: "user".into(),
        }],
        interpretations: vec!["90d".into(), "status".into()],
        trojan: Some("inactive".into()),
    };
    art
}

fn runtime_scope() -> RuntimeScope {
    RuntimeScope::task_scope(
        ContextId::from("ctx-1"),
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap()),
        MessageId::from("msg-1"),
        TaskId::from_external(ExternalId::new(String::from("task-1"))),
    )
}

fn tool_ctx(tool_name: &str, args: serde_json::Value, access: ToolAccess) -> ToolCallContext {
    let access_str = match access {
        ToolAccess::Read => "read",
        ToolAccess::Write => "write",
        ToolAccess::Delete => "delete",
    };
    ToolCallContext {
        tool_name: tool_name.into(),
        function_name: None,
        args,
        runtime_scope: runtime_scope(),
        metadata: json!({ "access_level": access_str }),
        agent_package: None,
        delegation_target: None,
    }
}

fn tool_ctx_for_agent(
    tool_name: &str,
    args: serde_json::Value,
    access: ToolAccess,
    agent_package: &str,
) -> ToolCallContext {
    let mut ctx = tool_ctx(tool_name, args, access);
    ctx.agent_package = Some(agent_package.to_string());
    ctx
}

fn enforced_interceptor(store: Arc<GroundingStore>) -> SemioticToolInterceptor {
    set_global_semiotic_config(SemioticConfig {
        default: SemioticPolicy {
            enabled: true,
            mode: SemioticMode::Enforce,
            enforce_min_tier: 2,
            ..Default::default()
        },
        overrides: Default::default(),
    });
    SemioticToolInterceptor::new(store)
}

async fn intercept(
    interceptor: &SemioticToolInterceptor,
    tool_name: &str,
    args: serde_json::Value,
    access: ToolAccess,
) -> InterceptorDecision {
    interceptor
        .intercept_tool_call(&tool_ctx(tool_name, args, access))
        .await
        .expect("intercept")
}

#[tokio::test]
async fn tier0_passes_silently() {
    let _guard = conformance_lock().lock().await;
    let store = Arc::new(GroundingStore::new());
    let gate = enforced_interceptor(store);
    let decision = intercept(
        &gate,
        "bash",
        json!({"command": "ls -la && git status"}),
        ToolAccess::Read,
    )
    .await;
    assert!(matches!(decision, InterceptorDecision::Allow));
}

#[tokio::test]
async fn no_artifact_denies_tier3() {
    let _guard = conformance_lock().lock().await;
    let store = Arc::new(GroundingStore::new());
    let gate = enforced_interceptor(store);
    let decision = intercept(
        &gate,
        "bash",
        json!({"command": "sqlite3 prod app.db 'DELETE FROM users'"}),
        ToolAccess::Delete,
    )
    .await;
    match decision {
        InterceptorDecision::Block(msg) => {
            assert!(
                msg.to_lowercase().contains("artifact") || msg.to_lowercase().contains("ground")
            );
        }
        other => panic!("expected block, got {other:?}"),
    }
}

#[tokio::test]
async fn trojan_nodes_deficient() {
    let gate = AmbiguityAwareGate::default();
    let decision = gate.decide(&trojan_artifact(), Tier::Mutating);
    assert_ne!(decision.action, GateAction::ExecuteFlagged);
    assert!(decision.requests.iter().any(|n| n == "ACTION"));
}

#[tokio::test]
async fn grounded_tier2_passes() {
    let _guard = conformance_lock().lock().await;
    let store = Arc::new(GroundingStore::new());
    store.submit(&runtime_scope(), grounded_artifact(), None);
    let gate = enforced_interceptor(store);
    let t2_cmd = json!({
        "command": "sqlite3 prod app.db \"UPDATE users SET deleted_at=datetime('now') WHERE last_login_at < date('now','-90 day')\""
    });
    let decision = intercept(&gate, "bash", t2_cmd, ToolAccess::Write).await;
    assert!(matches!(decision, InterceptorDecision::Allow));
}

#[tokio::test]
async fn grounded_tier3_requires_authorization() {
    let _guard = conformance_lock().lock().await;
    let store = Arc::new(GroundingStore::new());
    store.submit(&runtime_scope(), grounded_artifact(), None);
    let gate = enforced_interceptor(store);
    let t3_cmd = json!({
        "command": "sqlite3 prod app.db \"DELETE FROM users WHERE last_login_at < date('now','-90 day')\""
    });
    let decision = intercept(&gate, "bash", t3_cmd, ToolAccess::Delete).await;
    match decision {
        InterceptorDecision::RequireAuthorization(prompt) => {
            assert!(prompt.contains("authorization"));
            assert!(prompt.contains("Postconditions"));
        }
        other => panic!("expected RequireAuthorization, got {other:?}"),
    }
}

#[tokio::test]
async fn per_agent_override_enforces_while_global_dry_run() {
    let _guard = conformance_lock().lock().await;
    let store = Arc::new(GroundingStore::new());
    set_global_semiotic_config(SemioticConfig {
        default: SemioticPolicy {
            enabled: true,
            mode: SemioticMode::DryRun,
            enforce_min_tier: 2,
            ..Default::default()
        },
        overrides: {
            let mut agent = std::collections::HashMap::new();
            agent.insert(
                "deploy-agent".to_string(),
                SemioticPolicy {
                    enabled: true,
                    mode: SemioticMode::Enforce,
                    enforce_min_tier: 2,
                    ..Default::default()
                },
            );
            baml_rt_semiotic::config::SemioticOverrides { agent }
        },
    });
    let gate = SemioticToolInterceptor::new(store);
    let t2_cmd = json!({"command": "rm -rf /tmp/test"});
    let decision = gate
        .intercept_tool_call(&tool_ctx_for_agent(
            "bash",
            t2_cmd,
            ToolAccess::Write,
            "deploy-agent",
        ))
        .await
        .expect("intercept");
    match decision {
        InterceptorDecision::Block(_) => {}
        other => panic!("per-agent enforce expected block without artifact, got {other:?}"),
    }

    let dry_run = gate
        .intercept_tool_call(&tool_ctx_for_agent(
            "bash",
            json!({"command": "rm -rf /tmp/other"}),
            ToolAccess::Write,
            "other-agent",
        ))
        .await
        .expect("intercept");
    assert!(matches!(dry_run, InterceptorDecision::Allow));
}
