#![cfg(feature = "llm-tests")]

//! LLM-gated evaluation of the daemon's extraction pipeline.
//!
//! Uses curated Slack message fixtures and asserts structural properties
//! of the LLM output (non-empty summary, reasonable task count, key fields).

use baml_task_daemon::{
    ExtractionMode, ProjectContext, SlackMessage, SourceReference, TaskBatch, TaskExtractor,
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

fn engineering_discussion_messages() -> Vec<SlackMessage> {
    vec![
        make_message(
            "Team, we need to investigate the Slack daemon reliability issues reported last week. Multiple channels are seeing dropped messages during backfill.",
            "1735689600.000000",
        ),
        make_message(
            "I think the root cause is the pagination cursor handling. When we hit rate limits mid-backfill, the cursor state isn't preserved correctly.",
            "1735689700.000000",
        ),
        make_message(
            "TODO: @Bob please review the backfill state machine and propose a fix. We need this resolved before the next release.",
            "1735689800.000000",
        ),
        make_message(
            "Also, we should add monitoring for message delivery latency. Right now we have no visibility into how stale our data is.",
            "1735689900.000000",
        ),
        make_message(
            "Decision: we'll use OpenTelemetry metrics for the monitoring dashboard rather than custom Prometheus exporters. Less maintenance burden.",
            "1735690000.000000",
        ),
        make_message(
            "Risk: if we don't fix the backfill issue before deploying to production, we could lose task extraction coverage for high-volume channels.",
            "1735690100.000000",
        ),
    ]
}

fn feature_planning_messages() -> Vec<SlackMessage> {
    vec![
        make_message(
            "Let's plan the GitHub issue sync feature. Users want to create GitHub issues directly from Slack discussions.",
            "1735700000.000000",
        ),
        make_message(
            "We need a GitHub client crate following the clickup-client pattern. Token from GITHUB_TOKEN env var.",
            "1735700100.000000",
        ),
        make_message(
            "TODO: implement GithubIssueSink in the task daemon. Should support dry-run mode like ClickUp.",
            "1735700200.000000",
        ),
        make_message(
            "Question: should we support creating issues across multiple repos or just one per daemon instance?",
            "1735700300.000000",
        ),
    ]
}

fn minimal_todo_messages() -> Vec<SlackMessage> {
    vec![
        make_message(
            "TODO: update the deployment docs with the new environment variables",
            "1735710000.000000",
        ),
        make_message(
            "TODO: run the integration test suite before merging the PR",
            "1735710100.000000",
        ),
    ]
}

struct EvalFailure {
    fixture: String,
    assertion: String,
}

#[tokio::test]
async fn extraction_eval_curated_fixtures() {
    let extractor = match TaskExtractor::with_mode(20, ExtractionMode::Llm) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Skipping extraction eval: LLM extractor not configured: {err}");
            return;
        }
    };

    let project = ProjectContext {
        project_key: "agent-platform".to_string(),
        repo_available: true,
        repo_path: Some("/repo/agent-platform".to_string()),
    };

    let fixtures: Vec<(&str, Vec<SlackMessage>)> = vec![
        ("engineering_discussion", engineering_discussion_messages()),
        ("feature_planning", feature_planning_messages()),
        ("minimal_todos", minimal_todo_messages()),
    ];

    let mut failures: Vec<EvalFailure> = Vec::new();

    for (name, messages) in &fixtures {
        let batch = extractor
            .extract_slack_runtime("#agentium-eng", &project, messages)
            .await;

        let batch = match batch {
            Ok(b) => b,
            Err(err) => {
                failures.push(EvalFailure {
                    fixture: name.to_string(),
                    assertion: format!("extraction failed: {err}"),
                });
                continue;
            }
        };

        check_batch(name, &batch, &mut failures);
    }

    if !failures.is_empty() {
        let report = failures
            .iter()
            .map(|f| {
                format!(
                    "  [{fixture}] {assertion}",
                    fixture = f.fixture,
                    assertion = f.assertion
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        panic!("Extraction eval failures:\n{report}");
    }
}

fn check_batch(fixture: &str, batch: &TaskBatch, failures: &mut Vec<EvalFailure>) {
    if batch.interpretation.executive_summary.trim().is_empty() {
        failures.push(EvalFailure {
            fixture: fixture.to_string(),
            assertion: "executive_summary is empty".to_string(),
        });
    }

    if batch.derived_tasks.is_empty() {
        failures.push(EvalFailure {
            fixture: fixture.to_string(),
            assertion: "no derived tasks produced".to_string(),
        });
    }

    for task in &batch.derived_tasks {
        if task.title.trim().is_empty() {
            failures.push(EvalFailure {
                fixture: fixture.to_string(),
                assertion: format!("task '{key}' has empty title", key = task.key),
            });
        }
        if task.description.trim().is_empty() {
            failures.push(EvalFailure {
                fixture: fixture.to_string(),
                assertion: format!("task '{key}' has empty description", key = task.key),
            });
        }
    }

    if batch.derived_tasks.len() > 20 {
        failures.push(EvalFailure {
            fixture: fixture.to_string(),
            assertion: format!(
                "too many derived tasks: {count} (expected <= 20)",
                count = batch.derived_tasks.len(),
            ),
        });
    }
}
