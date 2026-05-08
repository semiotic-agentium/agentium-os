//! Adversarial fixture suite (issue #341): exercises runner-readiness
//! invariants under deploy starvation.
//!
//! Each test pairs a specific fault with a specific runner invariant:
//!
//! - **T1 — synthetic CPU-peg agent.** Deploy a fixture whose top-level
//!   evaluation pegs the JS thread for ~5s, while polling `/readyz` and
//!   `/diagnose` at 100ms cadence. Asserts:
//!     1. every `/readyz` probe returns `200` within 1s wall-clock,
//!     2. no probe response is dropped at the TCP level
//!        (no `connection refused`, transport error, or timeout),
//!     3. `runtime_progress_lag_ms` from `/diagnose` exceeds 200ms for at
//!        least one sample (proves the meter is sensitive to a CPU-pegged
//!        deploy boot — and detects the regression where `spawn_blocking`
//!        only isolates the future-driver thread, not the QuickJS
//!        evaluation thread).
//!
//! - **T4 — listener-task-death.** Start the runner in K8s-pilot mode
//!   (`a2a_stdio=false, http_handle=Some`) and force the spawned
//!   `axum::serve` task to return `Ok(())` early via the
//!   `baml_rt_api::LISTENER_EXIT_AFTER_SECS_ENV` debug-only fault-injection
//!   hook. Asserts: the runner process exits with non-zero status (so the
//!   kubelet would restart it). Currently expected to fail because the
//!   K8s-pilot match arm in `main.rs` treats clean exit from `axum::serve`
//!   as a successful shutdown.
//!
//! T2 (cgroup-throttled deploy) lives in the e2e/k8s lane; T3 (SurrealDB
//! latency injection) lands in its own file alongside this one.

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::{TempFileCleanup, e2e_secs_ci_or_local, e2e_serial_gate, publish_fixture};
use reqwest::StatusCode;
use serde_json::Value;
use test_support::common::{
    agent_fixture, build_agent_package_archive_to_temp, ensure_fixture_runtime_types,
};
use tokio::time::sleep;

const RUNNER_TOKEN: &str = "test-runner-token-starvation";

/// One observation of a probe endpoint.
#[derive(Debug, Clone)]
struct ProbeSample {
    /// Wall-clock offset from start-of-probing to start-of-request. Helps
    /// align lag spikes with deploy lifecycle when triaging failures.
    poll_offset_ms: u128,
    /// Round-trip time of this probe.
    elapsed: Duration,
    /// HTTP status if the request reached the server; `None` when the
    /// transport itself failed (connection refused, timeout, reset).
    status: Option<StatusCode>,
    /// Transport-level error message; `None` on a clean HTTP exchange.
    transport_error: Option<String>,
    /// Decoded JSON body when the endpoint returns one (e.g. `/diagnose`).
    body: Option<Value>,
}

/// Standalone runner subprocess for adversarial tests. Mirrors the
/// minimum slice of `runner_cluster_test::RunnerProcess` needed for
/// single-runner deploy probes — no cluster, no surreal endpoint, no
/// runner-endpoint advertisement.
struct StandaloneRunner {
    base_url: String,
    child: Child,
    log_path: PathBuf,
    repository_dir: PathBuf,
    state_dir: PathBuf,
}

impl StandaloneRunner {
    async fn start() -> Self {
        Self::start_with_env(&[]).await
    }

    /// Start a runner subprocess with additional environment variables. The
    /// `extra_env` slice supplements the baseline cleanups (which strip CI/dev
    /// secrets and credentials so the test process inherits a deterministic
    /// environment).
    async fn start_with_env(extra_env: &[(&str, &str)]) -> Self {
        let reserved = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = reserved.local_addr().expect("local address");
        drop(reserved);

        let base_url = format!("http://{addr}");
        let uid = uuid::Uuid::new_v4();
        let pid = std::process::id();
        let repository_dir =
            std::env::temp_dir().join(format!("starvation-runner-repo-{pid}-{uid}"));
        let state_dir = std::env::temp_dir().join(format!("starvation-runner-state-{pid}-{uid}"));
        fs::create_dir_all(&repository_dir).expect("create temp repository dir");
        fs::create_dir_all(&state_dir).expect("create temp state dir");
        let log_path = std::env::temp_dir().join(format!("starvation-runner-{pid}-{uid}.log"));
        let stdout = fs::File::create(&log_path).expect("create runner log");
        let stderr = stdout.try_clone().expect("clone runner log handle");

        let repository_url = format!("http://{addr}/repository");
        let mut command = Command::new(env!("CARGO_BIN_EXE_baml-agent-runner"));
        command
            .current_dir(&state_dir)
            .arg("--serve-http")
            .arg(addr.to_string())
            .arg("--repository-url")
            .arg(&repository_url)
            .arg("--repository-dir")
            .arg(&repository_dir)
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--runner-token")
            .arg(RUNNER_TOKEN)
            .env_remove("BAML_FNOX_CONFIG")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("CLICKUP_API_KEY")
            .env_remove("NOTION_API_TOKEN")
            .env_remove("SLACK_BOT_TOKEN")
            .env_remove("SLACK_USER_TOKEN")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("spawn baml-agent-runner");
        let client = reqwest::Client::new();
        let agents_url = format!("{base_url}/agents");
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
                };
            }
            sleep(Duration::from_millis(200)).await;
        }

        let log = fs::read_to_string(&log_path).unwrap_or_else(|_| "<unreadable>".into());
        let _ = child.kill();
        let _ = child.wait();
        panic!("runner did not become ready. Log:\n{log}");
    }

    /// Poll until the child exits, returning `None` on timeout. The runner is
    /// killed by `Drop`, not here.
    async fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.child.try_wait().expect("poll runner process") {
                Some(status) => return Some(status),
                None => sleep(Duration::from_millis(50)).await,
            }
        }
        None
    }

    fn log_text(&self) -> String {
        fs::read_to_string(&self.log_path).expect("read runner log")
    }
}

impl Drop for StandaloneRunner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log_path);
        let _ = fs::remove_dir_all(&self.repository_dir);
        let _ = fs::remove_dir_all(&self.state_dir);
    }
}

/// Spawn a 100ms-cadence prober that hits an endpoint until `stop` flips.
/// Each request has a 1s timeout — the assertion budget the issue calls
/// out for `/readyz` — so a stalled response shows up as a transport
/// error rather than blocking the next poll.
fn spawn_prober(
    client: reqwest::Client,
    url: String,
    started_at: Instant,
    stop: Arc<std::sync::atomic::AtomicBool>,
    samples: Arc<Mutex<Vec<ProbeSample>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let poll_offset = started_at.elapsed().as_millis();
            let probe_start = Instant::now();
            let response = client
                .get(&url)
                .timeout(Duration::from_secs(1))
                .send()
                .await;
            let elapsed = probe_start.elapsed();
            let sample = match response {
                Ok(resp) => {
                    let status = resp.status();
                    // /readyz returns no body; the JSON decode silently fails and `body`
                    // stays `None`. Endpoints that do return JSON (e.g. /diagnose)
                    // populate it.
                    let body = resp.json::<Value>().await.ok();
                    ProbeSample {
                        poll_offset_ms: poll_offset,
                        elapsed,
                        status: Some(status),
                        transport_error: None,
                        body,
                    }
                }
                Err(err) => ProbeSample {
                    poll_offset_ms: poll_offset,
                    elapsed,
                    status: None,
                    transport_error: Some(err.to_string()),
                    body: None,
                },
            };
            samples.lock().expect("samples mutex poisoned").push(sample);
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// T1 — synthetic CPU-peg agent
// ═══════════════════════════════════════════════════════════════════════════

/// **Regression target — currently expected to panic.**
///
/// Invariants 1 and 2 (every `/readyz` probe is `200` within 1s, no
/// transport-level probe drops) pass today: PR #344 wraps the deploy boot
/// in `spawn_blocking`, so the future-driver thread cannot starve the
/// probe handlers.
///
/// Invariant 3 (`runtime_progress_lag_ms > 200` for at least one sample)
/// **fails today**, which is the signal this test is designed to surface.
/// Two structural factors keep the meter from seeing CPU-pegged deploy
/// boot work:
///
///   * The QuickJS evaluation thread is its own thread and is *not*
///     isolated by `spawn_blocking` — `spawn_blocking` only protects the
///     future-driver. The meter never sees the peg unless the runtime
///     itself stops making progress.
///   * Even on the future-driver side, the meter is documented as having a
///     "blind spot" (`crates/baml-rt-api/src/runtime_progress.rs`): on a
///     multi-worker tokio runtime, a single wedged worker with others
///     available does not register as lag.
///
/// `#[should_panic]` keeps CI green while preserving this as a regression
/// target. When the underlying isolation lands (extending #335/#337 to the
/// QuickJS thread, or making the meter sensitive enough to flag per-thread
/// peg under multi-worker tokio), this test will fail with "did not panic
/// as expected" — at which point whoever fixed the bug should remove
/// `#[should_panic]` so the test asserts the invariant directly.
#[tokio::test]
#[should_panic(expected = "expected runtime_progress_lag_ms > 200")]
async fn t1_cpu_peg_deploy_keeps_probes_responsive() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    ensure_fixture_runtime_types();

    // Build the agent archive while the runner subprocess is coming up — both
    // are CPU-bound and independent, and the runner readiness wait dominates
    // wall-clock on cold runs.
    let (package_path, runner) = tokio::join!(
        build_agent_package_archive_to_temp(agent_fixture("cpu-peg-agent"), "cpu-peg-agent"),
        StandaloneRunner::start(),
    );
    let _cleanup = TempFileCleanup::new(package_path.clone());
    let client = reqwest::Client::new();

    let hash = publish_fixture(
        &client,
        &runner.base_url,
        &package_path,
        RUNNER_TOKEN,
        "runner_starvation_test",
    )
    .await;

    // Drain any baseline lag accumulated during boot before we start probing,
    // so the measurements only reflect the deploy-time CPU peg. Sized for a
    // loaded CI worker: too short a drain risks the first probe samples
    // reading boot-residual lag > 200ms, which would flip invariant 3 from
    // "expected fail" to "unexpected pass" and trip `#[should_panic]`.
    sleep(Duration::from_secs(1)).await;

    let started_at = Instant::now();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readyz_samples: Arc<Mutex<Vec<ProbeSample>>> = Arc::new(Mutex::new(Vec::new()));
    let diagnose_samples: Arc<Mutex<Vec<ProbeSample>>> = Arc::new(Mutex::new(Vec::new()));

    let readyz_handle = spawn_prober(
        client.clone(),
        format!("{}/readyz", runner.base_url),
        started_at,
        Arc::clone(&stop),
        Arc::clone(&readyz_samples),
    );
    let diagnose_handle = spawn_prober(
        client.clone(),
        format!("{}/diagnose", runner.base_url),
        started_at,
        Arc::clone(&stop),
        Arc::clone(&diagnose_samples),
    );

    let deploy_resp = client
        .post(format!("{}/deploy", runner.base_url))
        .header("X-Runner-Token", RUNNER_TOKEN)
        .json(&serde_json::json!({ "hash": hash }))
        .send()
        .await
        .expect("POST /deploy");
    let deploy_status = deploy_resp.status();
    let deploy_text = deploy_resp.text().await.unwrap_or_default();
    assert!(
        deploy_status.is_success(),
        "/deploy of cpu-peg-agent failed ({deploy_status}): {deploy_text}"
    );

    // One extra polling window so /diagnose has a chance to read post-deploy lag.
    sleep(Duration::from_secs(1)).await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = readyz_handle.await;
    let _ = diagnose_handle.await;

    let readyz_samples = readyz_samples
        .lock()
        .expect("readyz samples mutex poisoned")
        .clone();
    let diagnose_samples = diagnose_samples
        .lock()
        .expect("diagnose samples mutex poisoned")
        .clone();
    assert!(!readyz_samples.is_empty(), "no /readyz samples");
    assert!(!diagnose_samples.is_empty(), "no /diagnose samples");

    // ── Invariant 1: every /readyz probe returns 200 within 1s ──────────────
    for sample in &readyz_samples {
        assert!(
            sample.transport_error.is_none(),
            "/readyz probe at offset {}ms hit transport error (TCP-level drop): {}",
            sample.poll_offset_ms,
            sample.transport_error.as_deref().unwrap_or("?")
        );
        assert_eq!(
            sample.status,
            Some(StatusCode::OK),
            "/readyz probe at offset {}ms returned {:?}; expected 200",
            sample.poll_offset_ms,
            sample.status,
        );
        assert!(
            sample.elapsed < Duration::from_secs(1),
            "/readyz probe at offset {}ms took {:?}; must respond within 1s",
            sample.poll_offset_ms,
            sample.elapsed,
        );
    }

    // ── Invariant 2: no transport-level drops on either endpoint ────────────
    for sample in &diagnose_samples {
        assert!(
            sample.transport_error.is_none(),
            "/diagnose probe at offset {}ms hit transport error (TCP-level drop): {}",
            sample.poll_offset_ms,
            sample.transport_error.as_deref().unwrap_or("?")
        );
    }

    // ── Invariant 3: at least one /diagnose sample shows lag > 200ms ────────
    let lag_samples: Vec<u64> = diagnose_samples
        .iter()
        .filter_map(|s| {
            s.body
                .as_ref()
                .and_then(|b| b.get("runtime_progress_lag_ms"))
                .and_then(Value::as_u64)
        })
        .collect();
    assert!(
        !lag_samples.is_empty(),
        "no runtime_progress_lag_ms values parsed from /diagnose; body shape may have drifted"
    );
    let max_lag = lag_samples.iter().copied().max().unwrap_or(0);
    assert!(
        max_lag > 200,
        "expected runtime_progress_lag_ms > 200 in at least one /diagnose sample during \
         cpu-peg-agent deploy, but max observed was {max_lag}ms across {n_samples} samples. \
         If this assertion fails it means the runtime-progress meter did not detect the \
         CPU-pegged QuickJS thread — likely because the QuickJS evaluation thread is not \
         isolated from the tokio runtime (only the future-driver thread is, via spawn_blocking).",
        n_samples = lag_samples.len(),
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// T4 — listener-task-death
// ═══════════════════════════════════════════════════════════════════════════

/// **Regression target — currently expected to panic.**
///
/// The K8s-pilot match arm in `crates/baml-agent-runner/src/main.rs`
/// (`(false, Some(handle)) => handle.await??`) treats a clean `Ok(())` from
/// the spawned `axum::serve` task as a successful shutdown and propagates it
/// out of `main`, so the process exits with status 0. Under K8s, the kubelet
/// then declares the pod terminated normally and does **not** restart it —
/// silently masking a dead listener.
///
/// This test forces that exact failure: the runner is started in K8s-pilot
/// mode (`--serve-http`, no stdio) with the debug-only fault-injection env
/// var `AGENTIUM_TEST_LISTENER_EXIT_AFTER_SECS=2`. After ~2s the listener
/// task gracefully shuts down and `axum::serve(...).await` returns `Ok(())`.
/// The invariant is that this should propagate as a non-zero exit status,
/// because nothing requested the shutdown.
///
/// Today the runner exits with status 0, so the assertion below fails. The
/// `#[should_panic]` keeps CI green while preserving this as a regression
/// target. When the K8s-pilot arm is fixed (e.g. by treating `Ok(())` from
/// the listener task as an `Err` exit, or by making the listener loop
/// indefinitely until an explicit shutdown channel fires), this test will
/// fail with "did not panic as expected" — at which point the fixer should
/// remove `#[should_panic]` so the assertion runs directly.
#[tokio::test]
#[should_panic(expected = "expected runner to exit with non-zero status")]
async fn t4_listener_task_death_exits_nonzero() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let listener_lifetime_secs = 2u64;
    let mut runner = StandaloneRunner::start_with_env(&[(
        baml_rt_api::LISTENER_EXIT_AFTER_SECS_ENV,
        &listener_lifetime_secs.to_string(),
    )])
    .await;

    let exit_deadline = Duration::from_secs(e2e_secs_ci_or_local(60, 30));
    let status = runner
        .wait_for_exit(exit_deadline)
        .await
        .unwrap_or_else(|| {
            let log = runner.log_text();
            panic!(
                "runner did not exit within {exit_deadline:?} after listener fault injection. \
                 Log:\n{log}"
            )
        });

    let log = runner.log_text();
    assert!(
        !status.success(),
        "expected runner to exit with non-zero status after axum::serve returned Ok(()) \
         (a dead listener should propagate as a process-level failure so the kubelet restarts \
         the pod), but the runner exited cleanly: {status}. This proves the K8s-pilot arm in \
         main.rs (`(false, Some(handle)) => handle.await??`) treats clean exit from the \
         listener task as a successful shutdown. Log:\n{log}"
    );
}
