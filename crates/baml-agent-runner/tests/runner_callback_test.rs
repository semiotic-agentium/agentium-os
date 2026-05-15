#[allow(dead_code, unused_imports)]
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    str::FromStr,
    time::{Duration, Instant},
};

use baml_rt_core::ids::{ContextId, ExternalId, MessageId, TaskId};
use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin, PublishResult},
    entry::ChangeRationale,
    ids::AgentName,
    package::source_bundle_from_tar_gz,
};
use common::{e2e_secs_ci_or_local, e2e_serial_gate};

const TEST_RUNNER_TOKEN: &str = "test-runner-token-callback";
use reqwest::StatusCode;
use serde_json::Value;
use test_support::common::{
    agent_fixture, build_agent_package_archive_to_temp, chunks_from_responses,
    ensure_fixture_runtime_types, message_texts_from_chunks, reserve_ephemeral_addr,
    send_stream_request_with_task,
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
    pub log_path: PathBuf,
    repository_dir: PathBuf,
    state_dir: PathBuf,
    provenance_dir: PathBuf,
}

impl RunningRunnerProcess {
    async fn start(event_poll_interval_secs: u64) -> Self {
        let addr = reserve_ephemeral_addr("127.0.0.1");

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
        let provenance_dir = std::env::temp_dir().join(format!(
            "callback-runner-provenance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&repository_dir).expect("create temp repository dir");
        fs::create_dir_all(&state_dir).expect("create temp state dir");
        fs::create_dir_all(&provenance_dir).expect("create temp provenance dir");
        let log_path = std::env::temp_dir().join(format!(
            "callback-runner-{}-{}.log",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let stdout = fs::File::create(&log_path).expect("create runner log");
        let stderr = stdout.try_clone().expect("clone runner log handle");

        let repository_url = format!("http://{addr}/repository");
        let mut command = Command::new(env!("CARGO_BIN_EXE_baml-agent-runner"));
        command
            // Keep this subprocess hermetic: these tests exercise host callback
            // delivery and do not need workspace-level secrets or config.
            .current_dir(&state_dir)
            .arg("--serve-http")
            .arg(addr.to_string())
            .arg("--repository-url")
            .arg(&repository_url)
            .arg("--repository-dir")
            .arg(&repository_dir)
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--provenance-db")
            .arg(&provenance_dir)
            .arg("--runner-token")
            .arg(TEST_RUNNER_TOKEN)
            .arg("--event-poll-interval-secs")
            .arg(event_poll_interval_secs.to_string())
            // Child overrides parent RUST_LOG; include provenance + effect-bus warn so
            // `add_event_with_logging` / subscriber failures surface when debugging flakes.
            .env(
                "RUST_LOG",
                "info,baml_rt_provenance=warn,baml_rt_core::bus=warn",
            )
            .env_remove("BAML_FNOX_CONFIG")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("CLICKUP_API_KEY")
            .env_remove("NOTION_API_TOKEN")
            .env_remove("SLACK_BOT_TOKEN")
            .env_remove("SLACK_USER_TOKEN")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        let mut child = command.spawn().expect("spawn baml-agent-runner");
        let client = reqwest::Client::new();
        let agents_url = format!("{base_url}/agents");
        // CI runner startup can legitimately take well over a minute when the
        // child process is compiling/loading runtime assets under load. Keep the
        // local budget tight, but leave enough headroom in CI to avoid racing a
        // runner that is actually about to become ready.
        let readiness_deadline =
            Instant::now() + Duration::from_secs(e2e_secs_ci_or_local(240, 60));
        while Instant::now() < readiness_deadline {
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
                    provenance_dir,
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
        let _ = fs::remove_dir_all(&self.provenance_dir);
    }
}

/// Publishes a built `.tar.gz` through the runner's `/repository/publish` then deploys via `POST /deploy`.
async fn assert_dispatch_echo_callback_subscription_visible(
    client: &reqwest::Client,
    base_url: &str,
) {
    let url = format!("{base_url}/agents");
    let resp = client.get(&url).send().await.expect("GET /agents");
    assert!(
        resp.status().is_success(),
        "GET /agents failed: {}",
        resp.status()
    );
    let json: Value = resp.json().await.expect("GET /agents JSON");
    let entries = json.as_array().expect("/agents must return a JSON array");
    let echo = entries
        .iter()
        .find(|e| {
            e.get("agent_package")
                .and_then(Value::as_str)
                .is_some_and(|p| p == "dispatch-echo")
        })
        .unwrap_or_else(|| panic!("dispatch-echo not in /agents: {json}"));
    let subs = echo
        .get("agent_card")
        .and_then(|c| c.get("subscriptions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_callback = subs.iter().any(|s| {
        s.get("schema_versions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|v| v.as_str() == Some("system.callback.v1"))
            && s.get("source_kinds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|v| v.as_str() == Some("system/callback"))
    });
    assert!(
        has_callback,
        "dispatch-echo must subscribe to system.callback.v1 + system/callback for runner callback tests; subs={subs:?}"
    );
}

async fn publish_and_deploy_fixture(client: &reqwest::Client, base_url: &str, tar_path: &Path) {
    let bytes = fs::read(tar_path).expect("read package tar");
    let (_, source) =
        source_bundle_from_tar_gz(&bytes).expect("parse package as repository source bundle");
    let name_str = source.manifest.name().expect("manifest name in package");
    let cmd = PublishCommand {
        name: AgentName::from_str(name_str).expect("valid AgentName"),
        source,
        rationale: ChangeRationale::new("runner_callback_test").expect("non-empty rationale"),
        origin: PublishOrigin::Original,
    };
    let publish_url = format!("{base_url}/repository/publish");
    let resp = client
        .post(&publish_url)
        .header("X-Runner-Token", TEST_RUNNER_TOKEN)
        .json(&cmd)
        .send()
        .await
        .expect("POST /repository/publish");
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        panic!("publish failed: {text}");
    }
    let result: PublishResult = resp.json().await.expect("PublishResult JSON");
    let hash = result.hash.to_string();

    let deploy_url = format!("{base_url}/deploy");
    let deploy_resp = client
        .post(&deploy_url)
        .header("X-Runner-Token", TEST_RUNNER_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /deploy");
    if !deploy_resp.status().is_success() {
        let text = deploy_resp.text().await.unwrap_or_default();
        panic!("deploy failed: {text}");
    }
}

async fn post_a2a_sse_collect(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let request_url = url.replace("/a2a/sse", "/a2a");
    let response = client.post(&request_url).json(body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {text}").into());
    }
    let text = response.text().await?;
    baml_rt_core::parse_a2a_sse_json_rpc_chunks(&text)
        .map_err(|e| format!("Invalid A2A SSE response: {e}").into())
}

struct CallbackScheduleRequest<'a> {
    task_id: &'a TaskId,
    context_id: &'a ContextId,
    message_id: &'a str,
    request_id: &'a str,
    mode: &'a str,
    token: &'a str,
}

async fn invoke_callback_schedule(
    client: &reqwest::Client,
    base_url: &str,
    callback: CallbackScheduleRequest<'_>,
) -> Vec<Value> {
    let request = send_stream_request_with_task(
        callback.message_id,
        &format!(
            "schedule-callback {mode} {token}",
            mode = callback.mode,
            token = callback.token
        ),
        callback.request_id,
        Some(callback.context_id.clone()),
        Some(callback.task_id.clone()),
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
    // Callback delivery + discover_tools session + Surreal indexing can exceed 15s on cold CI
    // runners (especially under parallel `cargo test --workspace` load).
    let timeout = Duration::from_secs(60);
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
                "timed out waiting for provenance tool-call rows has_rows={expected_has_rows}; last status={status} body={body}"
            );
        }

        sleep(Duration::from_millis(150)).await;
    }
}

fn parse_labelled_field<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let start = message.find(&needle)? + needle.len();
    let rest = &message[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then_some(value)
}

/// Wire task id the runner mints for `message.sendStream` when the message carries
/// `context_id` + `message_id` (see `TaskId::for_live_stream`).
fn live_stream_task_id(context_id: &ContextId, message_id: &str) -> TaskId {
    TaskId::for_live_stream(
        context_id,
        &MessageId::from_external(ExternalId::new(message_id.to_string())),
    )
}

fn parse_minted_dispatch_scope(text: &str) -> Option<(ContextId, TaskId)> {
    let ctx = parse_labelled_field(text, "dispatchContextId")?;
    let task = parse_labelled_field(text, "dispatchTaskId")?;
    Some((
        ContextId::from(ctx),
        TaskId::from_external(ExternalId::new(task.to_string())),
    ))
}

#[tokio::test]
async fn runner_callback_delivery_honors_continuation_policy() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();

    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _package_cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunningRunnerProcess::start(1).await;
    let base_url = runner.base_url.clone();
    let client = reqwest::Client::new();
    publish_and_deploy_fixture(&client, &base_url, &package_path).await;
    assert_dispatch_echo_callback_subscription_visible(&client, &base_url).await;

    let detached_context_id = ContextId::new(730, 1);
    let detached_message_id = "dispatch-echo-detached-msg";
    let detached_scheduling_task = live_stream_task_id(&detached_context_id, detached_message_id);
    let detached_token = format!("detached{}", uuid::Uuid::new_v4().simple());
    let detached_responses = invoke_callback_schedule(
        &client,
        &base_url,
        CallbackScheduleRequest {
            task_id: &detached_scheduling_task,
            context_id: &detached_context_id,
            message_id: detached_message_id,
            request_id: "corr-1735720000000-41",
            mode: "detached",
            token: &detached_token,
        },
    )
    .await;
    let detached_texts = message_texts_from_chunks(&chunks_from_responses(&detached_responses));
    assert!(
        detached_texts
            .iter()
            .any(|text| text.contains("scheduled callback detached")),
        "expected detached callback scheduling confirmation, got texts={detached_texts:?} responses={detached_responses:?}"
    );

    let schedule_line = detached_texts
        .iter()
        .find(|t| t.contains("dispatchContextId=") && t.contains("dispatchTaskId="))
        .expect("detached schedule should surface minted dispatch scope in assistant text");
    let (child_ctx, child_task) = parse_minted_dispatch_scope(schedule_line)
        .expect("parse minted dispatch context_id and task_id");

    sleep(Duration::from_secs(3)).await;

    wait_for_provenance_tool_call_status(
        &client,
        &base_url,
        Some(&child_ctx),
        Some(&child_task),
        true,
    )
    .await;
    wait_for_provenance_tool_call_status(
        &client,
        &base_url,
        Some(&detached_context_id),
        None,
        false,
    )
    .await;
    wait_for_provenance_tool_call_status(
        &client,
        &base_url,
        Some(&detached_context_id),
        Some(&detached_scheduling_task),
        false,
    )
    .await;

    let resumed_context_id = ContextId::new(730, 2);
    let resumed_message_id = "dispatch-echo-resume-msg";
    let resumed_scheduling_task = live_stream_task_id(&resumed_context_id, resumed_message_id);
    let resumed_token = format!("resume{}", uuid::Uuid::new_v4().simple());
    let resumed_responses = invoke_callback_schedule(
        &client,
        &base_url,
        CallbackScheduleRequest {
            task_id: &resumed_scheduling_task,
            context_id: &resumed_context_id,
            message_id: resumed_message_id,
            request_id: "corr-1735720000000-42",
            mode: "resume_current_task",
            token: &resumed_token,
        },
    )
    .await;
    let resumed_texts = message_texts_from_chunks(&chunks_from_responses(&resumed_responses));
    assert!(
        resumed_texts
            .iter()
            .any(|text| text.contains("scheduled callback resume_current_task")),
        "expected resume callback scheduling confirmation, got texts={resumed_texts:?} responses={resumed_responses:?}"
    );

    wait_for_provenance_tool_call_status(&client, &base_url, Some(&resumed_context_id), None, true)
        .await;
    wait_for_provenance_tool_call_status(
        &client,
        &base_url,
        Some(&resumed_context_id),
        Some(&resumed_scheduling_task),
        true,
    )
    .await;
}

#[tokio::test]
async fn runner_callback_resume_current_task_defers_immediate_delivery_until_turn_completes() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();

    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _package_cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunningRunnerProcess::start(1).await;
    let base_url = runner.base_url.clone();
    let client = reqwest::Client::new();
    publish_and_deploy_fixture(&client, &base_url, &package_path).await;
    assert_dispatch_echo_callback_subscription_visible(&client, &base_url).await;

    let context_id = ContextId::new(731, 1);
    let message_id = "dispatch-echo-immediate-resume-msg";
    let task_id = live_stream_task_id(&context_id, message_id);
    let token = format!("immediateresume{}", uuid::Uuid::new_v4().simple());

    // The fixture schedules this callback with afterMs=0. The host must defer
    // delivery until the scheduling stream has quiesced, otherwise the original
    // request can stall before returning its final assistant message.
    let responses = invoke_callback_schedule(
        &client,
        &base_url,
        CallbackScheduleRequest {
            task_id: &task_id,
            context_id: &context_id,
            message_id,
            request_id: "corr-1735720000000-43",
            mode: "resume_current_task",
            token: &token,
        },
    )
    .await;
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    assert!(
        texts
            .iter()
            .any(|text| text.contains("scheduled callback resume_current_task")),
        "expected immediate resume callback scheduling confirmation, got texts={texts:?} responses={responses:?}"
    );

    wait_for_provenance_tool_call_status(
        &client,
        &base_url,
        Some(&context_id),
        Some(&task_id),
        true,
    )
    .await;
}
