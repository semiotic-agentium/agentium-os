#[allow(dead_code, unused_imports)]
mod common;

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use common::e2e_serial_gate;
use reqwest::StatusCode;
use serde_json::Value;
use test_support::common::{
    agent_fixture, build_agent_package_archive_to_temp, chunks_from_responses,
    ensure_fixture_runtime_types, message_texts_from_chunks, send_stream_request_with_task,
};
use tokio::time::sleep;

struct TempFileCleanup {
    path: PathBuf,
}

impl TempFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct RunningRunnerProcess {
    base_url: String,
    child: Child,
    log_path: PathBuf,
    repository_dir: PathBuf,
    state_dir: PathBuf,
}

impl RunningRunnerProcess {
    async fn start(package_paths: &[PathBuf], event_poll_interval_secs: u64) -> Self {
        let reserved = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = reserved.local_addr().expect("local address");
        drop(reserved);

        let base_url = format!("http://{addr}");
        let repository_dir = std::env::temp_dir().join(format!(
            "callback-runner-repository-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let state_dir = std::env::temp_dir().join(format!(
            "callback-runner-state-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&repository_dir).expect("create temp repository dir");
        fs::create_dir_all(&state_dir).expect("create temp state dir");
        let log_path = std::env::temp_dir().join(format!(
            "callback-runner-{}-{}.log",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let stdout = fs::File::create(&log_path).expect("create runner log");
        let stderr = stdout.try_clone().expect("clone runner log handle");

        let mut command = Command::new(env!("CARGO_BIN_EXE_baml-agent-runner"));
        command
            .args(package_paths)
            .arg("--serve-http")
            .arg(addr.to_string())
            .arg("--repository-dir")
            .arg(&repository_dir)
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--event-poll-interval-secs")
            .arg(event_poll_interval_secs.to_string())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        let mut child = command.spawn().expect("spawn baml-agent-runner");
        let client = reqwest::Client::new();
        let agents_url = format!("{base_url}/agents");
        for _ in 0..300 {
            if let Some(status) = child.try_wait().expect("poll runner process") {
                let log = fs::read_to_string(&log_path).unwrap_or_else(|_| "<unreadable>".into());
                panic!("runner exited before serving HTTP (status: {status}). Log:\n{log}");
            }

            if let Ok(response) = client.get(&agents_url).send().await
                && response.status().is_success()
            {
                return Self {
                    base_url,
                    child,
                    log_path,
                    repository_dir,
                    state_dir,
                };
            }

            sleep(Duration::from_millis(200)).await;
        }

        let log = fs::read_to_string(&log_path).unwrap_or_else(|_| "<unreadable>".into());
        let _ = child.kill();
        let _ = child.wait();
        panic!("runner did not become ready. Log:\n{log}");
    }
}

impl Drop for RunningRunnerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log_path);
        let _ = fs::remove_dir_all(&self.repository_dir);
        let _ = fs::remove_dir_all(&self.state_dir);
    }
}

async fn post_a2a_sse_collect(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let mut response = client
        .post(url)
        .header("Accept", "text/event-stream")
        .json(body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {text}").into());
    }

    let mut responses = Vec::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = buffer.find('\n') {
            let line = buffer[..newline_idx].trim().to_string();
            buffer.drain(..=newline_idx);
            if !line.starts_with("data:") {
                continue;
            }
            let json_str = line.strip_prefix("data:").unwrap_or(&line).trim();
            if json_str.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(json_str) {
                let is_final = value
                    .get("result")
                    .and_then(|result| result.get("final"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                responses.push(value);
                if is_final {
                    return Ok(responses);
                }
            }
        }
    }

    Ok(responses)
}

async fn invoke_callback_schedule(
    client: &reqwest::Client,
    base_url: &str,
    task_id: &TaskId,
    context_id: &ContextId,
    message_id: &str,
    request_id: &str,
    mode: &str,
    token: &str,
) -> Vec<Value> {
    let request = send_stream_request_with_task(
        message_id,
        &format!("schedule-callback {mode} {token}"),
        request_id,
        Some(context_id.clone()),
        Some(task_id.clone()),
    );
    let a2a_url = format!("{base_url}/agents/dispatch-echo/default/a2a/sse");
    post_a2a_sse_collect(client, &a2a_url, &request)
        .await
        .expect("dispatch-echo callback schedule request")
}

async fn wait_for_provenance_tool_call_status(
    client: &reqwest::Client,
    base_url: &str,
    context_id: Option<&ContextId>,
    task_id: Option<&TaskId>,
    expected_has_rows: bool,
) -> Value {
    let start = Instant::now();
    let timeout = Duration::from_secs(15);
    let url = format!("{base_url}/provenance/tool-calls");

    loop {
        let mut query = vec![("toolName".to_string(), "system/discover_tools".to_string())];
        if let Some(context_id) = context_id {
            query.push(("contextId".to_string(), context_id.to_string()));
        }
        if let Some(task_id) = task_id {
            query.push(("taskId".to_string(), task_id.as_str().to_string()));
        }

        let response = client
            .get(&url)
            .query(&query)
            .send()
            .await
            .expect("provenance tool call query");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status == StatusCode::OK
            && let Ok(json) = serde_json::from_str::<Value>(&body)
        {
            let row_count = json
                .get("rows")
                .and_then(Value::as_array)
                .map(|rows| rows.len())
                .unwrap_or(0);
            let has_rows = row_count > 0;
            if has_rows == expected_has_rows {
                return json;
            }
        }

        if start.elapsed() >= timeout {
            panic!(
                "timed out waiting for provenance tool-call rows has_rows={expected_has_rows}; last status={} body={body}",
                status,
            );
        }

        sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn runner_callback_delivery_honors_continuation_policy() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();

    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _package_cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunningRunnerProcess::start(&[package_path], 1).await;
    let client = reqwest::Client::new();

    let detached_task_id = TaskId::from_external(ExternalId::new("dispatch-echo-detached-task"));
    let detached_context_id = ContextId::new(730, 1);
    let detached_token = format!("detached{}", uuid::Uuid::new_v4().simple());
    let detached_responses = invoke_callback_schedule(
        &client,
        &runner.base_url,
        &detached_task_id,
        &detached_context_id,
        "dispatch-echo-detached-msg",
        "corr-1735720000000-41",
        "detached",
        &detached_token,
    )
    .await;
    let detached_texts = message_texts_from_chunks(&chunks_from_responses(&detached_responses));
    assert!(
        detached_texts
            .iter()
            .any(|text| text.contains("scheduled callback detached")),
        "expected detached callback scheduling confirmation, got texts={detached_texts:?} responses={detached_responses:?}"
    );

    wait_for_provenance_tool_call_status(&client, &runner.base_url, None, None, true).await;
    wait_for_provenance_tool_call_status(
        &client,
        &runner.base_url,
        Some(&detached_context_id),
        None,
        false,
    )
    .await;
    wait_for_provenance_tool_call_status(
        &client,
        &runner.base_url,
        Some(&detached_context_id),
        Some(&detached_task_id),
        false,
    )
    .await;

    let resumed_task_id = TaskId::from_external(ExternalId::new("dispatch-echo-resume-task"));
    let resumed_context_id = ContextId::new(730, 2);
    let resumed_token = format!("resume{}", uuid::Uuid::new_v4().simple());
    let resumed_responses = invoke_callback_schedule(
        &client,
        &runner.base_url,
        &resumed_task_id,
        &resumed_context_id,
        "dispatch-echo-resume-msg",
        "corr-1735720000000-42",
        "resume_current_task",
        &resumed_token,
    )
    .await;
    let resumed_texts = message_texts_from_chunks(&chunks_from_responses(&resumed_responses));
    assert!(
        resumed_texts
            .iter()
            .any(|text| text.contains("scheduled callback resume_current_task")),
        "expected resume callback scheduling confirmation, got texts={resumed_texts:?} responses={resumed_responses:?}"
    );

    wait_for_provenance_tool_call_status(
        &client,
        &runner.base_url,
        Some(&resumed_context_id),
        None,
        true,
    )
    .await;
    wait_for_provenance_tool_call_status(
        &client,
        &runner.base_url,
        Some(&resumed_context_id),
        Some(&resumed_task_id),
        true,
    )
    .await;
}
