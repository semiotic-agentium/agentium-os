#[allow(dead_code, unused_imports)]
mod common;

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use common::{TempFileCleanup, e2e_secs_ci_or_local, e2e_serial_gate, publish_fixture};
use reqwest::StatusCode;
use serde_json::Value;
use test_support::common::{
    agent_fixture, build_agent_package_archive_to_temp, ensure_fixture_runtime_types,
};
use tokio::time::sleep;

const DEFAULT_TOKEN: &str = "test-runner-token-cluster";

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

/// Builder for spawning a `baml-agent-runner` subprocess with configurable options.
struct RunnerProcessConfig {
    token: Option<String>,
    surreal_endpoint: Option<String>,
    runner_endpoint: Option<String>,
    bind_addr: Option<String>,
    fnox_config: Option<PathBuf>,
}

impl RunnerProcessConfig {
    fn standalone() -> Self {
        Self {
            token: Some(DEFAULT_TOKEN.to_string()),
            surreal_endpoint: None,
            runner_endpoint: None,
            bind_addr: None,
            fnox_config: None,
        }
    }

    fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    fn without_token(mut self) -> Self {
        self.token = None;
        self
    }

    #[cfg(feature = "cluster-tests")]
    fn with_surreal(mut self, endpoint: &str) -> Self {
        self.surreal_endpoint = Some(endpoint.to_string());
        self
    }

    #[cfg(feature = "cluster-tests")]
    fn with_runner_endpoint(mut self, endpoint: &str) -> Self {
        self.runner_endpoint = Some(endpoint.to_string());
        self
    }

    #[cfg(feature = "cluster-tests")]
    fn with_bind_addr(mut self, addr: &str) -> Self {
        self.bind_addr = Some(addr.to_string());
        self
    }

    #[cfg(feature = "cluster-tests")]
    fn with_fnox_config(mut self, path: &Path) -> Self {
        self.fnox_config = Some(path.to_path_buf());
        self
    }
}

struct RunnerProcess {
    base_url: String,
    child: Child,
    log_path: PathBuf,
    repository_dir: PathBuf,
    state_dir: PathBuf,
}

struct PreparedRunner {
    base_url: String,
    repository_dir: PathBuf,
    state_dir: PathBuf,
    log_path: PathBuf,
    command: Command,
}

impl PreparedRunner {
    fn into_runner(self, child: Child) -> RunnerProcess {
        RunnerProcess {
            base_url: self.base_url,
            child,
            log_path: self.log_path,
            repository_dir: self.repository_dir,
            state_dir: self.state_dir,
        }
    }
}

fn prepare_runner_subprocess(config: &RunnerProcessConfig) -> PreparedRunner {
    // Cross-runner tests (`with_bind_addr`) need the bound port to equal the
    // port in the advertised `runner_endpoint`; loopback tests share a
    // validation-only placeholder URL (#454) and must bind ephemeral.
    let cross_runner = config.bind_addr.is_some();
    let bind_host = config.bind_addr.as_deref().unwrap_or("127.0.0.1");
    let desired_port = if cross_runner {
        config
            .runner_endpoint
            .as_deref()
            .and_then(|ep| url::Url::parse(ep).ok())
            .and_then(|u| u.port())
            .unwrap_or(0)
    } else {
        0
    };
    let reserved =
        TcpListener::bind(format!("{bind_host}:{desired_port}")).expect("bind ephemeral port");
    let addr = reserved.local_addr().expect("local address");
    drop(reserved);

    // In cluster mode, bind 0.0.0.0 so cross-runner traffic routed via the
    // advertised non-loopback IP still reaches the socket when the host
    // application firewall refuses inbound on specific-IP binds.
    let serve_bind = if cross_runner {
        format!("0.0.0.0:{}", addr.port())
    } else {
        addr.to_string()
    };

    let base_url = format!("http://{addr}");
    let uid = uuid::Uuid::new_v4();
    let pid = std::process::id();
    let repository_dir = std::env::temp_dir().join(format!("cluster-runner-repo-{pid}-{uid}"));
    let state_dir = std::env::temp_dir().join(format!("cluster-runner-state-{pid}-{uid}"));
    fs::create_dir_all(&repository_dir).expect("create temp repository dir");
    fs::create_dir_all(&state_dir).expect("create temp state dir");
    let log_path = std::env::temp_dir().join(format!("cluster-runner-{pid}-{uid}.log"));
    let stdout = fs::File::create(&log_path).expect("create runner log");
    let stderr = stdout.try_clone().expect("clone runner log handle");

    let repository_url = format!("http://{addr}/repository");
    let mut command = Command::new(env!("CARGO_BIN_EXE_baml-agent-runner"));
    command
        .current_dir(&state_dir)
        .arg("--serve-http")
        .arg(&serve_bind)
        .arg("--repository-url")
        .arg(&repository_url)
        .arg("--repository-dir")
        .arg(&repository_dir)
        .arg("--state-dir")
        .arg(&state_dir)
        .env_remove("BAML_FNOX_CONFIG")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("CLICKUP_API_KEY")
        .env_remove("NOTION_API_TOKEN")
        .env_remove("SLACK_BOT_TOKEN")
        .env_remove("SLACK_USER_TOKEN")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    if let Some(token) = &config.token {
        command.arg("--runner-token").arg(token);
    }
    if let Some(surreal) = &config.surreal_endpoint {
        command
            .arg("--surreal-endpoint")
            .arg(surreal)
            .arg("--surreal-username")
            .arg("root")
            .arg("--surreal-password")
            .arg("root");
    }
    if let Some(runner_ep) = &config.runner_endpoint {
        command.arg("--runner-endpoint").arg(runner_ep);
    }
    if let Some(fnox_path) = &config.fnox_config {
        command.env("BAML_FNOX_CONFIG", fnox_path);
    }

    PreparedRunner {
        base_url,
        repository_dir,
        state_dir,
        log_path,
        command,
    }
}

impl RunnerProcess {
    async fn start(config: RunnerProcessConfig) -> Self {
        let mut prepared = prepare_runner_subprocess(&config);
        let mut child = prepared.command.spawn().expect("spawn baml-agent-runner");
        let client = reqwest::Client::new();
        let agents_url = format!("{}/agents", prepared.base_url);
        let readiness_deadline =
            Instant::now() + Duration::from_secs(e2e_secs_ci_or_local(240, 60));
        while Instant::now() < readiness_deadline {
            if let Some(status) = child.try_wait().expect("poll runner process") {
                let log = fs::read_to_string(&prepared.log_path)
                    .unwrap_or_else(|_| "<unreadable>".into());
                panic!("runner exited before serving HTTP (status: {status}). Log:\n{log}");
            }
            if let Ok(response) = client.get(&agents_url).send().await
                && response.status().is_success()
            {
                return prepared.into_runner(child);
            }
            sleep(Duration::from_millis(200)).await;
        }

        let log = fs::read_to_string(&prepared.log_path).unwrap_or_else(|_| "<unreadable>".into());
        let _ = child.kill();
        let _ = child.wait();
        panic!("runner did not become ready. Log:\n{log}");
    }

    /// Spawn a runner without waiting for readiness (for testing readyz/healthz probes).
    fn spawn_no_wait(config: RunnerProcessConfig) -> (Self, String) {
        let mut prepared = prepare_runner_subprocess(&config);
        let child = prepared.command.spawn().expect("spawn baml-agent-runner");
        let url = prepared.base_url.clone();
        (prepared.into_runner(child), url)
    }
}

impl Drop for RunnerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log_path);
        let _ = fs::remove_dir_all(&self.repository_dir);
        let _ = fs::remove_dir_all(&self.state_dir);
    }
}

/// Deploy a previously published hash.
async fn deploy_hash(
    client: &reqwest::Client,
    base_url: &str,
    hash: &str,
    token: &str,
) -> reqwest::Response {
    client
        .post(format!("{base_url}/deploy"))
        .header("X-Runner-Token", token)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /deploy")
}

/// Publish and deploy a fixture in one step. Returns the content hash.
async fn publish_and_deploy(
    client: &reqwest::Client,
    base_url: &str,
    tar_path: &Path,
    token: &str,
) -> String {
    let hash = publish_fixture(client, base_url, tar_path, token, "runner_cluster_test").await;
    let resp = deploy_hash(client, base_url, &hash, token).await;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        panic!("deploy failed: {text}");
    }
    hash
}

/// Collect SSE A2A responses until the final response.
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
                    .and_then(|r| r.get("final"))
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

/// Build an A2A sendStream JSON-RPC request body.
fn send_stream_request(text: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message.sendStream",
        "params": {
            "message": {
                "messageId": uuid::Uuid::new_v4().to_string(),
                "role": "user",
                "parts": [{ "kind": "text", "text": text }]
            }
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 1 — Standalone deploy lifecycle
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn deploy_by_hash_returns_success() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();

    let hash = publish_fixture(
        &client,
        &runner.base_url,
        &package_path,
        DEFAULT_TOKEN,
        "runner_cluster_test",
    )
    .await;
    let resp = deploy_hash(&client, &runner.base_url, &hash, DEFAULT_TOKEN).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("deploy response JSON");
    assert_eq!(body["already_deployed"], false);
    assert_eq!(body["hash"], hash);

    // Verify agent appears in discovery.
    let agents: Vec<Value> = client
        .get(format!("{}/agents", runner.base_url))
        .send()
        .await
        .expect("GET /agents")
        .json()
        .await
        .expect("/agents JSON");
    assert!(
        agents
            .iter()
            .any(|a| a["agent_package"].as_str() == Some("dispatch-echo")),
        "deployed agent should appear in /agents: {agents:?}"
    );
}

#[tokio::test]
async fn deploy_idempotent_returns_already_deployed() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();

    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    // Second deploy of the same hash.
    let resp = deploy_hash(&client, &runner.base_url, &hash, DEFAULT_TOKEN).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("deploy response JSON");
    assert_eq!(body["already_deployed"], true);

    // Only one agent entry.
    let agents: Vec<Value> = client
        .get(format!("{}/agents", runner.base_url))
        .send()
        .await
        .expect("GET /agents")
        .json()
        .await
        .expect("/agents JSON");
    let dispatch_count = agents
        .iter()
        .filter(|a| a["agent_package"].as_str() == Some("dispatch-echo"))
        .count();
    assert_eq!(
        dispatch_count, 1,
        "should have exactly one dispatch-echo entry"
    );
}

#[tokio::test]
async fn undeploy_removes_agent() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();

    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    // Undeploy.
    let resp = client
        .post(format!("{}/undeploy", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /undeploy");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("undeploy response JSON");
    assert_eq!(body["removed"], true);

    // Agent gone from discovery.
    let agents: Vec<Value> = client
        .get(format!("{}/agents", runner.base_url))
        .send()
        .await
        .expect("GET /agents")
        .json()
        .await
        .expect("/agents JSON");
    assert!(
        !agents
            .iter()
            .any(|a| a["agent_package"].as_str() == Some("dispatch-echo")),
        "undeployed agent should not appear in /agents"
    );

    // Second undeploy returns 404.
    let resp2 = client
        .post(format!("{}/undeploy", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("second POST /undeploy");
    assert_eq!(resp2.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deploy_requires_auth_when_token_configured() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner =
        RunnerProcess::start(RunnerProcessConfig::standalone().with_token("secret-token-123"))
            .await;
    let client = reqwest::Client::new();
    let hash = publish_fixture(
        &client,
        &runner.base_url,
        &package_path,
        "secret-token-123",
        "runner_cluster_test",
    )
    .await;

    // No token header.
    let resp = client
        .post(format!("{}/deploy", runner.base_url))
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("deploy without token");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "no token should be 401"
    );

    // Wrong token.
    let resp = client
        .post(format!("{}/deploy", runner.base_url))
        .header("X-Runner-Token", "wrong-token")
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("deploy with wrong token");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "wrong token should be 401"
    );

    // Correct token.
    let resp = deploy_hash(&client, &runner.base_url, &hash, "secret-token-123").await;
    assert_eq!(resp.status(), StatusCode::OK, "correct token should be 200");
}

#[tokio::test]
async fn readyz_returns_503_then_200() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (mut runner, base_url) =
        RunnerProcess::spawn_no_wait(RunnerProcessConfig::standalone().without_token());
    let client = reqwest::Client::new();
    let readyz_url = format!("{base_url}/readyz");

    // Try to catch the 503 window. The runner may boot so fast that we only see 200.
    let mut saw_503 = false;
    let probe_deadline = Instant::now() + Duration::from_secs(e2e_secs_ci_or_local(240, 60));
    while Instant::now() < probe_deadline {
        if let Some(status) = runner.child.try_wait().expect("poll runner") {
            let log =
                fs::read_to_string(&runner.log_path).unwrap_or_else(|_| "<unreadable>".into());
            panic!("runner exited (status: {status}). Log:\n{log}");
        }
        match client.get(&readyz_url).send().await {
            Ok(resp) if resp.status() == StatusCode::SERVICE_UNAVAILABLE => {
                saw_503 = true;
            }
            Ok(resp) if resp.status() == StatusCode::OK => {
                // Runner is ready now.
                break;
            }
            _ => {}
        }
        sleep(Duration::from_millis(50)).await;
    }

    // After readiness, readyz must return 200.
    let resp = client
        .get(&readyz_url)
        .send()
        .await
        .expect("GET /readyz after ready");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "readyz should be 200 once ready"
    );

    // Log whether we observed the 503 window (informational, not a hard failure).
    if saw_503 {
        eprintln!("readyz_returns_503_then_200: caught 503 before readiness");
    } else {
        eprintln!(
            "readyz_returns_503_then_200: runner booted before first probe (503 window missed)"
        );
    }
}

#[tokio::test]
async fn healthz_always_200() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (mut runner, base_url) =
        RunnerProcess::spawn_no_wait(RunnerProcessConfig::standalone().without_token());
    let client = reqwest::Client::new();
    let healthz_url = format!("{base_url}/healthz");

    // Poll until we get a response (server needs to start listening first).
    let deadline = Instant::now() + Duration::from_secs(e2e_secs_ci_or_local(240, 60));
    loop {
        if let Some(status) = runner.child.try_wait().expect("poll runner") {
            let log =
                fs::read_to_string(&runner.log_path).unwrap_or_else(|_| "<unreadable>".into());
            panic!("runner exited (status: {status}). Log:\n{log}");
        }
        if let Ok(resp) = client.get(&healthz_url).send().await {
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "healthz should always be 200"
            );
            return;
        }
        assert!(Instant::now() < deadline, "healthz never responded");
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn get_deployments_lists_active() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();

    let pkg_echo =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _c1 = TempFileCleanup::new(pkg_echo.clone());
    let pkg_block = build_agent_package_archive_to_temp(
        agent_fixture("emit-plan-then-block"),
        "emit-plan-then-block",
    )
    .await;
    let _c2 = TempFileCleanup::new(pkg_block.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();

    let hash1 = publish_and_deploy(&client, &runner.base_url, &pkg_echo, DEFAULT_TOKEN).await;
    let hash2 = publish_and_deploy(&client, &runner.base_url, &pkg_block, DEFAULT_TOKEN).await;

    // Both should appear in /deployments.
    let deps: Vec<Value> = client
        .get(format!("{}/deployments", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .send()
        .await
        .expect("GET /deployments")
        .json()
        .await
        .expect("/deployments JSON");
    assert_eq!(deps.len(), 2, "should have 2 deployments: {deps:?}");
    let hashes: Vec<&str> = deps
        .iter()
        .filter_map(|d| d["content_hash"].as_str())
        .collect();
    assert!(hashes.contains(&hash1.as_str()), "hash1 should be listed");
    assert!(hashes.contains(&hash2.as_str()), "hash2 should be listed");

    // Undeploy one.
    let resp = client
        .post(format!("{}/undeploy", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .json(&serde_json::json!({ "hash": hash1 }))
        .send()
        .await
        .expect("POST /undeploy");
    assert_eq!(resp.status(), StatusCode::OK);

    // Only one remaining.
    let deps: Vec<Value> = client
        .get(format!("{}/deployments", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .send()
        .await
        .expect("GET /deployments after undeploy")
        .json()
        .await
        .expect("/deployments JSON");
    assert_eq!(deps.len(), 1, "should have 1 deployment after undeploy");
    assert_eq!(deps[0]["content_hash"].as_str(), Some(hash2.as_str()));
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 2 — Cluster mode
// ═══════════════════════════════════════════════════════════════════════════

// Test 10: SSRF rejection does NOT need cluster mode — just a token-protected runner.
#[tokio::test]
async fn migrate_rejects_ssrf_targets() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();
    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    let bad_targets = [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1:9999",
        "http://metadata.google.internal",
    ];

    for target in &bad_targets {
        let resp = client
            .post(format!("{}/control/migrate", runner.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&serde_json::json!({
                "hash": hash,
                "target_runner_endpoint": target,
            }))
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST /control/migrate to {target}: {e}"));
        assert!(
            resp.status() == StatusCode::BAD_REQUEST
                || resp.status() == StatusCode::UNPROCESSABLE_ENTITY,
            "migrate to {target} should be rejected, got {}",
            resp.status()
        );
    }

    // Agent must still be deployed after all rejected attempts.
    let agents: Vec<Value> = client
        .get(format!("{}/agents", runner.base_url))
        .send()
        .await
        .expect("GET /agents")
        .json()
        .await
        .expect("/agents JSON");
    assert!(
        agents
            .iter()
            .any(|a| a["agent_package"].as_str() == Some("dispatch-echo")),
        "agent should remain deployed after SSRF rejections"
    );
}

// ── Cluster tests requiring a SurrealDB container ───────────────────────

#[cfg(feature = "cluster-tests")]
mod cluster {
    use common::CLUSTER_SURREALDB_IMAGE_TAG;
    use testcontainers_modules::{
        surrealdb::{SURREALDB_PORT, SurrealDb},
        testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
    };

    use super::*;

    struct SurrealContainer {
        endpoint: String,
        _container: ContainerAsync<SurrealDb>,
    }

    impl SurrealContainer {
        async fn start() -> Self {
            let container = SurrealDb::default()
                .with_tag(CLUSTER_SURREALDB_IMAGE_TAG)
                .start()
                .await
                .expect("start SurrealDB container — cluster-tests require Docker or Podman");
            let port = container
                .get_host_port_ipv4(SURREALDB_PORT)
                .await
                .expect("get SurrealDB mapped port");
            Self {
                endpoint: format!("ws://127.0.0.1:{port}"),
                _container: container,
            }
        }
    }

    /// Detect a non-loopback private IP suitable for cross-runner communication.
    /// Returns None if the machine has no routable private IP (e.g., no network).
    fn detect_non_loopback_ip() -> Option<std::net::IpAddr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("8.8.8.8:80").ok()?;
        let ip = socket.local_addr().ok()?.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            return None;
        }
        Some(ip)
    }

    #[tokio::test]
    async fn cluster_mode_rejects_unauthenticated_deploy() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

        let surreal = SurrealContainer::start().await;

        // Start runner in cluster mode WITHOUT a runner-token.
        // Use a fake runner endpoint that passes validation (10.x is RFC1918).
        let runner = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .without_token()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(common::FAKE_CLUSTER_RUNNER_ENDPOINT),
        )
        .await;
        let client = reqwest::Client::new();

        // In cluster mode without a token, ALL operator endpoints fail-closed
        // with 401 — including publish. Use a dummy hash to verify deploy
        // rejects before checking the hash.
        let resp = client
            .post(format!("{}/deploy", runner.base_url))
            .json(&serde_json::json!({ "hash": "dummy" }))
            .send()
            .await
            .expect("deploy without token in cluster mode");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "cluster mode should reject unauthenticated deploy"
        );
    }

    #[tokio::test]
    async fn get_deployments_requires_auth_in_cluster_mode() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
        ensure_fixture_runtime_types();

        let surreal = SurrealContainer::start().await;

        // Cluster mode without a token → control endpoints fail-closed.
        let runner = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .without_token()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(common::FAKE_CLUSTER_RUNNER_ENDPOINT),
        )
        .await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("{}/deployments", runner.base_url))
            .send()
            .await
            .expect("GET /deployments without token in cluster mode");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "cluster mode should reject unauthenticated GET /deployments"
        );
    }

    #[tokio::test]
    async fn cross_runner_a2a_forwarding() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
        ensure_fixture_runtime_types();

        let ip = match detect_non_loopback_ip() {
            Some(ip) => ip,
            None => {
                eprintln!(
                    "SKIPPED cross_runner_a2a_forwarding: no non-loopback private IP detected"
                );
                return;
            }
        };
        let bind_addr = ip.to_string();

        let package_path =
            build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo")
                .await;
        let _cleanup = TempFileCleanup::new(package_path.clone());

        let surreal = SurrealContainer::start().await;

        // Allocate ports for both runners.
        let reserved_a = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-A port");
        let port_a = reserved_a.local_addr().expect("runner-A addr").port();
        drop(reserved_a);
        let reserved_b = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-B port");
        let port_b = reserved_b.local_addr().expect("runner-B addr").port();
        drop(reserved_b);

        let endpoint_a = format!("http://{bind_addr}:{port_a}");
        let endpoint_b = format!("http://{bind_addr}:{port_b}");

        let runner_a = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_a)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr),
        )
        .await;

        let client = reqwest::Client::new();

        // Publish to both runners (they need the archive in their local repository).
        publish_fixture(
            &client,
            &runner_a.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;
        let hash = publish_fixture(
            &client,
            &runner_b.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;

        // Deploy ONLY on runner-B.
        let resp = deploy_hash(&client, &runner_b.base_url, &hash, DEFAULT_TOKEN).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "deploy on runner-B should succeed"
        );

        // Send A2A request to runner-A for dispatch-echo (hosted on runner-B).
        let a2a_url = format!("{}/agents/dispatch-echo/default/a2a", runner_a.base_url);
        let body = send_stream_request("hello from cross-runner test");
        let resp = client
            .post(&a2a_url)
            .json(&body)
            .send()
            .await
            .expect("A2A to runner-A for agent on runner-B");
        assert!(
            resp.status().is_success(),
            "cross-runner A2A should succeed, got {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }

    #[tokio::test]
    async fn migrate_moves_agent_between_runners() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
        ensure_fixture_runtime_types();

        let ip = match detect_non_loopback_ip() {
            Some(ip) => ip,
            None => {
                eprintln!(
                    "SKIPPED migrate_moves_agent_between_runners: no non-loopback private IP"
                );
                return;
            }
        };
        let bind_addr = ip.to_string();

        let package_path =
            build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo")
                .await;
        let _cleanup = TempFileCleanup::new(package_path.clone());

        let surreal = SurrealContainer::start().await;

        let reserved_a = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-A port");
        let port_a = reserved_a.local_addr().expect("runner-A addr").port();
        drop(reserved_a);
        let reserved_b = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-B port");
        let port_b = reserved_b.local_addr().expect("runner-B addr").port();
        drop(reserved_b);

        let endpoint_a = format!("http://{bind_addr}:{port_a}");
        let endpoint_b = format!("http://{bind_addr}:{port_b}");

        let runner_a = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_a)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let client = reqwest::Client::new();

        // Publish to both runners. Content-addressable hashing ensures the
        // first publish on each runner produces the same hash; avoid
        // publishing twice on runner-A (which would create a new version hash
        // that runner-B doesn't know about).
        let hash = publish_fixture(
            &client,
            &runner_a.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;
        publish_fixture(
            &client,
            &runner_b.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;

        // Deploy on runner-A.
        let deploy_resp = deploy_hash(&client, &runner_a.base_url, &hash, DEFAULT_TOKEN).await;
        assert!(
            deploy_resp.status().is_success(),
            "deploy on runner-A should succeed: {}",
            deploy_resp.text().await.unwrap_or_default()
        );

        // Migrate to runner-B.
        let resp = client
            .post(format!("{}/control/migrate", runner_a.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&serde_json::json!({
                "hash": hash,
                "target_runner_endpoint": endpoint_b,
            }))
            .send()
            .await
            .expect("POST /control/migrate");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "migrate should succeed: {}",
            resp.text().await.unwrap_or_default()
        );

        // Agent should be gone from runner-A.
        let agents_a: Vec<Value> = client
            .get(format!("{}/agents", runner_a.base_url))
            .send()
            .await
            .expect("GET /agents on runner-A")
            .json()
            .await
            .expect("/agents JSON");
        assert!(
            !agents_a
                .iter()
                .any(|a| a["agent_package"].as_str() == Some("dispatch-echo")),
            "agent should be removed from runner-A after migrate"
        );

        // Agent should appear on runner-B.
        let agents_b: Vec<Value> = client
            .get(format!("{}/agents", runner_b.base_url))
            .send()
            .await
            .expect("GET /agents on runner-B")
            .json()
            .await
            .expect("/agents JSON");
        assert!(
            agents_b
                .iter()
                .any(|a| a["agent_package"].as_str() == Some("dispatch-echo")),
            "agent should appear on runner-B after migrate"
        );
    }

    /// Two runners can each host the same agent and serve A2A traffic.
    #[tokio::test]
    async fn multi_runner_deploy_succeeds_with_coexisting_placements() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
        ensure_fixture_runtime_types();

        let ip = match detect_non_loopback_ip() {
            Some(ip) => ip,
            None => {
                eprintln!(
                    "SKIPPED multi_runner_deploy_succeeds_with_coexisting_placements: no non-loopback private IP"
                );
                return;
            }
        };
        let bind_addr = ip.to_string();

        let package_path =
            build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo")
                .await;
        let _cleanup = TempFileCleanup::new(package_path.clone());

        let surreal = SurrealContainer::start().await;

        let reserved_a = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-A port");
        let port_a = reserved_a.local_addr().expect("runner-A addr").port();
        drop(reserved_a);
        let reserved_b = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-B port");
        let port_b = reserved_b.local_addr().expect("runner-B addr").port();
        drop(reserved_b);

        let endpoint_a = format!("http://{bind_addr}:{port_a}");
        let endpoint_b = format!("http://{bind_addr}:{port_b}");

        let runner_a = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_a)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let client = reqwest::Client::new();

        // Publish to both runners.
        let hash_a = publish_fixture(
            &client,
            &runner_a.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;
        let hash_b = publish_fixture(
            &client,
            &runner_b.base_url,
            &package_path,
            DEFAULT_TOKEN,
            "runner_cluster_test",
        )
        .await;
        assert_eq!(hash_a, hash_b, "same fixture should produce same hash");

        deploy_hash(&client, &runner_a.base_url, &hash_a, DEFAULT_TOKEN).await;
        deploy_hash(&client, &runner_b.base_url, &hash_b, DEFAULT_TOKEN).await;

        let a2a_url = format!("{}/agents/dispatch-echo/default/a2a", runner_a.base_url);
        let body = send_stream_request("test multi-runner placement");
        let resp = client
            .post(&a2a_url)
            .json(&body)
            .send()
            .await
            .expect("A2A to runner-A");
        assert!(
            resp.status().is_success(),
            "A2A via runner-A should succeed, got {status}",
            status = resp.status()
        );
    }

    /// Prove the #222 config persistence contract:
    /// 1. Config written through runner-A is readable through runner-B (cross-runner visibility)
    /// 2. Config survives runner-B restart (persistence)
    #[tokio::test]
    async fn cluster_config_shared_across_runners() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

        let ip = match detect_non_loopback_ip() {
            Some(ip) => ip,
            None => {
                eprintln!(
                    "SKIPPED cluster_config_shared_across_runners: no non-loopback private IP"
                );
                return;
            }
        };
        let bind_addr = ip.to_string();

        let surreal = SurrealContainer::start().await;

        let reserved_a = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-A port");
        let port_a = reserved_a.local_addr().expect("runner-A addr").port();
        drop(reserved_a);
        let reserved_b = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-B port");
        let port_b = reserved_b.local_addr().expect("runner-B addr").port();
        drop(reserved_b);

        let endpoint_a = format!("http://{bind_addr}:{port_a}");
        let endpoint_b = format!("http://{bind_addr}:{port_b}");

        let runner_a = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_a)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let mut runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr),
        )
        .await;
        let client = reqwest::Client::new();

        // --- Write config through runner-A ---
        let config_body = serde_json::json!({
            "default": "TestClient",
            "clients": {
                "TestClient": {
                    "name": "TestClient",
                    "provider": "openrouter",
                    "options": { "model": "openai/gpt-4o-mini" }
                }
            },
            "overrides": {},
            "retry_policies": {}
        });
        let put_resp = client
            .put(format!("{}/config/llm", runner_a.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&config_body)
            .send()
            .await
            .expect("PUT /config/llm on runner-A");
        assert!(
            put_resp.status().is_success(),
            "PUT config on runner-A failed: {}",
            put_resp.status()
        );

        // --- Read config through runner-B (cross-runner visibility) ---
        let get_resp = client
            .get(format!("{}/config/llm", runner_b.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .send()
            .await
            .expect("GET /config/llm on runner-B");
        assert!(
            get_resp.status().is_success(),
            "GET config on runner-B failed: {}",
            get_resp.status()
        );
        let config_from_b: Value = get_resp.json().await.expect("parse config JSON from B");
        assert_eq!(
            config_from_b
                .get("config")
                .and_then(|c| c.get("default"))
                .and_then(Value::as_str),
            Some("TestClient"),
            "runner-B must see config written by runner-A; got: {config_from_b}"
        );
        let version_from_b = config_from_b.get("version").and_then(Value::as_u64);
        assert!(
            version_from_b.is_some_and(|v| v >= 1),
            "config version must be >= 1; got: {version_from_b:?}"
        );

        // --- Restart runner-B and verify persistence ---
        let _ = runner_b.child.kill();
        let _ = runner_b.child.wait();

        // RunnerProcess::start allocates a fresh ephemeral port; the
        // restarted runner reads config from the shared SurrealDB regardless
        // of which HTTP port it binds.
        runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr),
        )
        .await;

        let get_resp2 = client
            .get(format!("{}/config/llm", runner_b.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .send()
            .await
            .expect("GET /config/llm on restarted runner-B");
        assert!(
            get_resp2.status().is_success(),
            "GET config on restarted runner-B failed: {}",
            get_resp2.status()
        );
        let config_after_restart: Value = get_resp2
            .json()
            .await
            .expect("parse config JSON after restart");
        assert_eq!(
            config_after_restart
                .get("config")
                .and_then(|c| c.get("default"))
                .and_then(Value::as_str),
            Some("TestClient"),
            "config must survive runner-B restart; got: {config_after_restart}"
        );
    }

    /// Prove the #222 secret-link convergence contract:
    /// A secret linked on runner-A is visible as satisfied on runner-B
    /// via secrets-overview (without runner-B restart).
    #[tokio::test]
    async fn cluster_secret_link_converges_across_runners() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

        let ip = match detect_non_loopback_ip() {
            Some(ip) => ip,
            None => {
                eprintln!(
                    "SKIPPED cluster_secret_link_converges_across_runners: \
                     no non-loopback private IP"
                );
                return;
            }
        };
        let bind_addr = ip.to_string();

        // Write a temp fnox.toml with a test secret that has a default value.
        let fnox_dir = tempfile::tempdir().expect("temp dir for fnox");
        let fnox_path = fnox_dir.path().join("fnox.toml");
        fs::write(
            &fnox_path,
            "[secrets.TEST_SECRET]\n\
             description = \"cluster test secret\"\n\
             default = \"test-value-for-linking\"\n",
        )
        .expect("write temp fnox.toml");

        let surreal = SurrealContainer::start().await;

        let reserved_a = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-A port");
        let port_a = reserved_a.local_addr().expect("runner-A addr").port();
        drop(reserved_a);
        let reserved_b = TcpListener::bind(format!("{bind_addr}:0")).expect("bind runner-B port");
        let port_b = reserved_b.local_addr().expect("runner-B addr").port();
        drop(reserved_b);

        let endpoint_a = format!("http://{bind_addr}:{port_a}");
        let endpoint_b = format!("http://{bind_addr}:{port_b}");

        let runner_a = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_a)
                .with_bind_addr(&bind_addr)
                .with_fnox_config(&fnox_path),
        )
        .await;
        let runner_b = RunnerProcess::start(
            RunnerProcessConfig::standalone()
                .with_surreal(&surreal.endpoint)
                .with_runner_endpoint(&endpoint_b)
                .with_bind_addr(&bind_addr)
                .with_fnox_config(&fnox_path),
        )
        .await;
        let client = reqwest::Client::new();

        // --- Create a secret requirement via LLM config on runner-A ---
        let config_body = serde_json::json!({
            "default": "TestClient",
            "clients": {
                "TestClient": {
                    "name": "TestClient",
                    "provider": "openrouter",
                    "options": {
                        "model": "openai/gpt-4o-mini",
                        "api_key": "vault:TEST_SECRET"
                    }
                }
            },
            "overrides": {},
            "retry_policies": {}
        });
        let put_config = client
            .put(format!("{}/config/llm", runner_a.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&config_body)
            .send()
            .await
            .expect("PUT /config/llm on runner-A");
        assert!(
            put_config.status().is_success(),
            "PUT config on runner-A failed: {}",
            put_config.status()
        );

        // --- Link the secret on runner-A ---
        let put_secret = client
            .put(format!("{}/config/secrets/TEST_SECRET", runner_a.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&serde_json::json!({ "link_from": "TEST_SECRET" }))
            .send()
            .await
            .expect("PUT /config/secrets/TEST_SECRET on runner-A");
        assert!(
            put_secret.status().is_success(),
            "PUT secret link on runner-A failed: {} — {}",
            put_secret.status(),
            put_secret
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".into())
        );

        // --- Verify runner-B sees the link as satisfied ---
        let overview_resp = client
            .get(format!("{}/config/secrets-overview", runner_b.base_url))
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .send()
            .await
            .expect("GET /config/secrets-overview on runner-B");
        assert!(
            overview_resp.status().is_success(),
            "secrets-overview on runner-B failed: {}",
            overview_resp.status()
        );
        let overview: Vec<Value> = overview_resp
            .json()
            .await
            .expect("parse secrets-overview JSON");
        let test_entry = overview
            .iter()
            .find(|e| e.get("name").and_then(Value::as_str) == Some("TEST_SECRET"));
        assert!(
            test_entry.is_some(),
            "TEST_SECRET must appear in runner-B's secrets-overview; got: {overview:?}"
        );
        let entry = test_entry.unwrap();
        assert_eq!(
            entry.get("satisfied").and_then(Value::as_bool),
            Some(true),
            "TEST_SECRET must be satisfied on runner-B after link on runner-A; got: {entry}"
        );
        assert_eq!(
            entry.get("linked_to").and_then(Value::as_str),
            Some("TEST_SECRET"),
            "TEST_SECRET must show linked_to on runner-B; got: {entry}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 3 — Drain and lifecycle
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn undeploy_drains_in_flight_request() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();
    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    // Start an A2A request in background.
    let bg_client = client.clone();
    let bg_url = format!("{}/agents/dispatch-echo/default/a2a/sse", runner.base_url);
    let bg_body = send_stream_request("in-flight message");
    let bg_handle =
        tokio::spawn(async move { post_a2a_sse_collect(&bg_client, &bg_url, &bg_body).await });

    // Give the A2A request a moment to reach the runner.
    sleep(Duration::from_millis(100)).await;

    // Undeploy while the request may be in-flight.
    let undeploy_resp = client
        .post(format!("{}/undeploy", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /undeploy during in-flight");
    assert!(
        undeploy_resp.status().is_success(),
        "undeploy should complete without error"
    );

    // Background request should have completed (or failed gracefully).
    let bg_result = bg_handle.await.expect("background A2A task");
    // We don't assert on the content — the request may have completed before drain,
    // or may have been rejected. The key assertion is no crash/panic.
    drop(bg_result);

    // New requests after undeploy should fail with 404.
    let a2a_url = format!("{}/agents/dispatch-echo/default/a2a/sse", runner.base_url);
    let body = send_stream_request("post-undeploy message");
    let resp = client
        .post(&a2a_url)
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("A2A after undeploy");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "A2A after undeploy should be 404"
    );
}

#[tokio::test]
async fn draining_agent_rejects_new_requests() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();
    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    // Send one A2A message to create agent state.
    let a2a_url = format!("{}/agents/dispatch-echo/default/a2a/sse", runner.base_url);
    let body = send_stream_request("warmup message");
    let _ = post_a2a_sse_collect(&client, &a2a_url, &body).await;

    // Start undeploy in background.
    let undeploy_client = client.clone();
    let undeploy_url = format!("{}/undeploy", runner.base_url);
    let undeploy_hash = hash.clone();
    let undeploy_handle = tokio::spawn(async move {
        undeploy_client
            .post(&undeploy_url)
            .header("X-Runner-Token", DEFAULT_TOKEN)
            .json(&serde_json::json!({ "hash": undeploy_hash }))
            .send()
            .await
    });

    // Rapidly send A2A requests hoping to hit the drain window.
    let mut saw_draining_or_404 = false;
    for _ in 0..20 {
        let body = send_stream_request("during-drain probe");
        match client
            .post(&a2a_url)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                // Agent is either draining or already removed.
                let text = resp.text().await.unwrap_or_default();
                saw_draining_or_404 = true;
                // Informational: check if the response mentions draining.
                if text.contains("draining") {
                    eprintln!("draining_agent_rejects_new_requests: caught drain window");
                }
                break;
            }
            Ok(resp) if resp.status().is_success() => {
                // Agent still active, try again.
                sleep(Duration::from_millis(5)).await;
            }
            _ => {
                sleep(Duration::from_millis(5)).await;
            }
        }
    }

    let _ = undeploy_handle.await;

    // After undeploy completes, requests must be rejected.
    let body = send_stream_request("after-drain message");
    let resp = client
        .post(&a2a_url)
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("A2A after drain complete");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "requests after undeploy must return 404"
    );

    if !saw_draining_or_404 {
        eprintln!(
            "draining_agent_rejects_new_requests: drain window too narrow to observe \
             (dispatch-echo completes instantly). This is expected."
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 4 — Provenance integration
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn undeploy_emits_agent_stopped_event() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();
    let package_path =
        build_agent_package_archive_to_temp(agent_fixture("dispatch-echo"), "dispatch-echo").await;
    let _cleanup = TempFileCleanup::new(package_path.clone());

    let runner = RunnerProcess::start(RunnerProcessConfig::standalone()).await;
    let client = reqwest::Client::new();
    let hash = publish_and_deploy(&client, &runner.base_url, &package_path, DEFAULT_TOKEN).await;

    // Send one A2A message to create provenance context.
    let a2a_url = format!("{}/agents/dispatch-echo/default/a2a/sse", runner.base_url);
    let body = send_stream_request("provenance test message");
    let _ = post_a2a_sse_collect(&client, &a2a_url, &body).await;

    // Undeploy — this should emit an AgentStopped provenance event.
    let resp = client
        .post(format!("{}/undeploy", runner.base_url))
        .header("X-Runner-Token", DEFAULT_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /undeploy");
    assert_eq!(resp.status(), StatusCode::OK);

    // Poll lifecycle events endpoint for the AgentStopped event.
    let lifecycle_url = format!("{}/provenance/lifecycle-events", runner.base_url);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let resp = client
            .get(&lifecycle_url)
            .send()
            .await
            .expect("GET /provenance/lifecycle-events");

        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"error": "non-JSON response"}));

        if status.is_success()
            && let Some(rows) = body.get("rows").and_then(Value::as_array)
        {
            let has_stop = rows.iter().any(|row| {
                row.get("a2a_stop_reason")
                    .and_then(Value::as_str)
                    .is_some_and(|r| r == "undeploy")
            });
            if has_stop {
                return; // Success!
            }
        }

        if Instant::now() >= deadline {
            // Dump runner log for diagnosis.
            let log =
                fs::read_to_string(&runner.log_path).unwrap_or_else(|_| "<unreadable>".into());
            let stop_lines: Vec<&str> = log
                .lines()
                .filter(|l| {
                    l.contains("AgentStopped")
                        || l.contains("agent_stopped")
                        || l.contains("failed to write")
                        || l.contains("undeploy")
                })
                .collect();
            panic!(
                "AgentStopped event with reason 'undeploy' not found within 10s.\n\
                 Last response (status {status}): {body}\n\
                 Relevant runner log lines:\n{}",
                stop_lines.join("\n")
            );
        }
        sleep(Duration::from_millis(500)).await;
    }
}
