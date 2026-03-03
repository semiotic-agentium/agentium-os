#![cfg(feature = "llm-tests")]

//! Integration eval: daemon extraction → format_coordinator_prompt → verify prompt structure.
//!
//! Tests the full pipeline from mock Slack messages through LLM extraction to
//! coordinator-ready prompt formatting.

use baml_task_daemon::{
    ExtractionMode, ProjectContext, SlackMessage, SourceReference, TaskExtractor,
    format_coordinator_prompt,
};

fn make_message(text: &str, ts: &str) -> SlackMessage {
    SlackMessage {
        channel_name: "agentium-eng".to_string(),
        channel_id: "CTEST123".to_string(),
        ts: ts.to_string(),
        thread_ts: None,
        user_id: Some("UALICE".to_string()),
        user_name: Some("Alice".to_string()),
        text: text.to_string(),
        subtype: None,
        source: SourceReference {
            reference: format!("slack:CTEST123:{ts}"),
            permalink: Some(format!("https://acme.slack.com/archives/CTEST123/p{ts}")),
            channel_id: Some("CTEST123".to_string()),
            message_ts: Some(ts.to_string()),
            thread_ts: None,
        },
    }
}

fn integration_messages() -> Vec<SlackMessage> {
    vec![
        make_message(
            "Team: we need to create GitHub issues for all the backlog items discussed this week.",
            "1735720000.000000",
        ),
        make_message(
            "TODO: @carol set up the CI pipeline for the new github-client crate.",
            "1735720100.000000",
        ),
        make_message(
            "TODO: @dave write integration tests for the A2A sink bridging daemon to coordinator.",
            "1735720200.000000",
        ),
        make_message(
            "Decision: we'll use the coordinator agent to fan out task creation across ClickUp and GitHub.",
            "1735720300.000000",
        ),
    ]
}

/// Full pipeline test: extract from mock messages, then format as coordinator prompt.
#[tokio::test]
async fn daemon_extracts_and_formats_coordinator_prompt() {
    let extractor = match TaskExtractor::with_mode(20, ExtractionMode::Llm) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Skipping integration eval: LLM not configured: {err}");
            return;
        }
    };

    let project = ProjectContext {
        project_key: "agent-platform".to_string(),
        repo_available: true,
        repo_path: Some("/repo/agent-platform".to_string()),
    };

    let messages = integration_messages();
    let batch = extractor
        .extract_slack_runtime("#agentium-eng", &project, &messages)
        .await
        .expect("extraction should succeed");

    assert!(
        !batch.interpretation.executive_summary.trim().is_empty(),
        "extraction must produce a summary"
    );
    assert!(
        !batch.derived_tasks.is_empty(),
        "extraction must produce derived tasks from TODO items"
    );

    let prompt = format_coordinator_prompt(&batch);

    assert!(
        prompt.contains("#agentium-eng"),
        "prompt must reference source label. Got:\n{prompt}"
    );
    assert!(
        prompt.contains("agent-platform"),
        "prompt must reference project key. Got:\n{prompt}"
    );
    assert!(
        prompt.contains("Tasks to create"),
        "prompt must contain task list header. Got:\n{prompt}"
    );
    assert!(
        prompt.contains("Please create these as tasks"),
        "prompt must contain action instruction. Got:\n{prompt}"
    );

    let task_count = batch.derived_tasks.len();
    assert!(
        prompt.contains(&format!("{task_count} items")),
        "prompt must contain accurate task count. Got:\n{prompt}"
    );

    // Verify each derived task appears in the prompt
    for task in &batch.derived_tasks {
        assert!(
            prompt.contains(&task.title),
            "prompt must contain task title '{title}'. Got:\n{prompt}",
            title = task.title,
        );
    }
}
