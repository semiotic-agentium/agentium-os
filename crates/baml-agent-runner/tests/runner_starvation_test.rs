//! Adversarial fixture suite (issue #341): exercises runner-readiness
//! invariants under deploy starvation.
//!
//! Each test pairs a specific fault with a specific runner invariant:
//!
//! - **T1 — synthetic CPU-peg agent.** Deploy a fixture whose top-level
//!   evaluation pegs the JS thread for ~5s, while polling `/readyz` and
//!   `/diagnose` at 100ms cadence. Asserts:
//!     1. every `/readyz` probe receives an HTTP response within 1s
//!        wall-clock with status in `{200, 503}`. Under the
//!        runtime-progress-gated contract (#339), 503 during the peg is
//!        the correct operator-visible signal of stall; the assertion
//!        targets the listener's transport-level liveness and timing,
//!        not the gate's verdict,
//!     2. no probe response is dropped at the TCP level
//!        (no `connection refused`, transport error, or timeout),
//!     3. `runtime_progress_lag_ms` from `/diagnose` exceeds 200ms for at
//!        least one sample (proves the meter is sensitive to a CPU-pegged
//!        deploy boot — and detects the regression where `spawn_blocking`
//!        only isolates the future-driver thread, not the QuickJS
//!        evaluation thread). Since I1 was relaxed from `== 200` to
//!        `{200, 503}` for the #339 contract, I3 is now the sole
//!        regression guard for meter sensitivity — a probe-unwiring bug
//!        would leave the gate open at `200`, pass I1, but fail I3. Do
//!        not weaken I3 without replacing that coverage.
//!
//! - **T4 — listener-task-death.** Start the runner in K8s-pilot mode
//!   (`a2a_stdio=false, http_handle=Some`) and force the spawned
//!   `axum::serve` task to return `Ok(())` early via the
//!   `baml_rt_api::LISTENER_EXIT_AFTER_SECS_ENV` debug-only fault-injection
//!   hook. Asserts: the runner process exits with non-zero status (so the
//!   kubelet restarts it). The K8s-pilot match arm in `main.rs` enforces
//!   this by treating any return from the listener task as a failure.
//!
//! - **T3 — SurrealDB latency injection** *(gated behind `cluster-tests`)*.
//!   Front a real SurrealDB endpoint (testcontainer) with an in-process
//!   TCP latency-injecting forwarder, boot the runner pointed at the
//!   proxy in cluster mode (latency disabled so boot is fast), then
//!   enable per-chunk latency on the proxy and poll `/diagnose`.
//!   Asserts: `cluster_heartbeat_status` / `cluster_heartbeat_lag_ms` /
//!   `cluster_heartbeat_last_error_kind` reflect degradation under
//!   latency. Forward-going regression for the production fix that
//!   wires `ClusterHeartbeatHealth` into `spawn_heartbeat` and `ApiState`
//!   so a `BamlRtError::ClusterHeartbeat` surfaces in the HTTP contract,
//!   not just pod stderr.
//!
//! T2 (cgroup-throttled deploy) lives in the e2e/k8s lane.

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    fs,
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
    reserve_ephemeral_addr,
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
        Self::start_with_options(&[], &[], None).await
    }

    /// Start a runner subprocess with additional environment variables. The
    /// `extra_env` slice supplements the baseline cleanups (which strip CI/dev
    /// secrets and credentials so the test process inherits a deterministic
    /// environment).
    async fn start_with_env(extra_env: &[(&str, &str)]) -> Self {
        Self::start_with_options(extra_env, &[], None).await
    }

    /// Start a runner subprocess with optional extra env, extra CLI args, and
    /// an explicit readiness deadline override.
    ///
    /// `extra_args` is appended to the runner command line *after* the baseline
    /// flags (`--serve-http`, `--repository-url`, `--repository-dir`,
    /// `--state-dir`, `--runner-token`), letting cluster-mode tests inject
    /// `--surreal-endpoint`/`--runner-endpoint` without forking this harness.
    /// `readiness_secs` overrides the default boot deadline; cluster-mode tests
    /// that route every SurrealDB query through a latency-injecting proxy need
    /// far more boot time than the standalone deploy boot the default sizes.
    async fn start_with_options(
        extra_env: &[(&str, &str)],
        extra_args: &[&str],
        readiness_secs: Option<u64>,
    ) -> Self {
        let addr = reserve_ephemeral_addr("127.0.0.1");

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
        for arg in extra_args {
            command.arg(arg);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("spawn baml-agent-runner");
        let client = reqwest::Client::new();
        let agents_url = format!("{base_url}/agents");
        let deadline_secs = readiness_secs.unwrap_or_else(|| e2e_secs_ci_or_local(240, 60));
        let readiness_deadline = Instant::now() + Duration::from_secs(deadline_secs);
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

    /// Tail of the runner log, last `lines` lines. Use in panic messages on
    /// long-running adversarial tests so the assert payload stays bounded
    /// even when the runner has been booting for minutes under fault
    /// injection (T3 boots can run >5min with per-query latency).
    // `#[allow]` not `#[expect]`: only the T3 path calls this (T1/T4 use the full `log_text`),
    // so whether `dead_code` fires depends on which test binaries/toolchain are in scope.
    #[allow(dead_code)]
    fn log_tail(&self, lines: usize) -> String {
        let text = fs::read_to_string(&self.log_path).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(lines).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
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

/// Regression target for issue #341 / #352: deploying a CPU-pegged agent
/// must keep `/readyz` and `/diagnose` responsive (transport-level), and
/// `runtime_progress_lag_ms` must surface the QuickJS-thread peg with at
/// least one sample above 200ms.
#[tokio::test]
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
    // so the measurements only reflect the deploy-time CPU peg.
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

    // ── Invariant 1: every /readyz probe gets an HTTP response within 1s ────
    for sample in &readyz_samples {
        assert!(
            sample.transport_error.is_none(),
            "/readyz probe at offset {}ms hit transport error (TCP-level drop): {}",
            sample.poll_offset_ms,
            sample.transport_error.as_deref().unwrap_or("?")
        );
        let status = sample.status;
        assert!(
            matches!(
                status,
                Some(StatusCode::OK) | Some(StatusCode::SERVICE_UNAVAILABLE)
            ),
            "/readyz probe at offset {}ms returned {:?}; expected 200 or 503 (gate verdict)",
            sample.poll_offset_ms,
            status,
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

/// In K8s-pilot mode (`--serve-http`, no stdio) the only long-running task in
/// the runner process is the HTTP listener. Nothing in `main` ever requests a
/// listener shutdown, so any return from `axum::serve(...).await` — clean or
/// erroring — means the listener task died and the process must exit with a
/// non-zero status so the kubelet restarts the pod.
///
/// This test forces a clean listener exit via the debug-only fault-injection
/// env var `AGENTIUM_TEST_LISTENER_EXIT_AFTER_SECS=2`. After ~2s the listener
/// task gracefully shuts down and `axum::serve(...).await` returns `Ok(())`.
/// The K8s-pilot match arm in `crates/baml-agent-runner/src/main.rs` must
/// translate that into a non-zero process exit; this test asserts the
/// resulting status is not `success()`.
///
/// Scoped to the silent-success path (`Ok(Ok(()))`): the `Ok(Err)` and
/// `Err(JoinError)` arms already exited non-zero under the prior `handle.await??`,
/// so the regression target here is the `Ok(())`-as-success masking that the
/// kubelet observed as a clean shutdown. No separate fixture for the other arms.
#[tokio::test]
async fn t4_listener_task_death_exits_nonzero() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    // Give the harness enough time to observe at least one successful HTTP response
    // before the injected listener shutdown fires, especially on slower CI boots.
    let listener_lifetime_secs = e2e_secs_ci_or_local(10, 5);
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
         the pod), but the runner exited cleanly: {status}. The K8s-pilot arm in main.rs \
         must translate any return from the listener task into a non-zero process exit. \
         Log:\n{log}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// T3 — SurrealDB latency injection
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "cluster-tests")]
mod surrealdb_latency {
    use std::net::SocketAddr;

    use common::{CLUSTER_SURREALDB_IMAGE_TAG, FAKE_CLUSTER_RUNNER_ENDPOINT};
    use serde::Deserialize;
    use test_support::common::bind_ephemeral_tokio;
    use testcontainers_modules::{
        surrealdb::{SURREALDB_PORT, SurrealDb},
        testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    /// Latency injected on every server→client byte chunk by the proxy when
    /// it is enabled. Sized to push per-tick wall-clock latency past
    /// `ClusterHeartbeatHealth::STALE_LAG_MULTIPLIER × heartbeat interval`
    /// (currently 2 × 5s = 10s) so status reliably flips to `degraded` and
    /// lag crosses [`LAG_DEGRADED_THRESHOLD_MS`]. The original 5s injection
    /// — matching the issue's literal "5s latency to all queries" — kept
    /// heartbeats inside the `Ok` envelope (single-chunk responses, so
    /// per-tick latency tops out at ~one interval) and produced a false
    /// pass. Boot is not paid through the proxy: latency is toggled on
    /// after the runner has registered.
    const LATENCY_SECS: u64 = 12;

    /// Typed slice of `/diagnose` that T3 inspects. Only the heartbeat
    /// object is checked for schema drift; full-envelope drift is owned by
    /// the dedicated integration test in `baml-rt-api`.
    #[derive(Debug, Deserialize)]
    struct DiagnoseSnapshot {
        cluster_heartbeat: Option<HeartbeatSnapshot>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HeartbeatSnapshot {
        status: String,
        lag_ms: Option<u64>,
        #[serde(default)]
        last_error_kind: Option<String>,
    }

    /// Single SurrealDB container started as a `testcontainers` image.
    /// Mirrors `runner_cluster_test::cluster::SurrealContainer` deliberately
    /// — duplication is preferable to a third shared module while only T3
    /// (here) and the cluster suite (there) need it. If a fourth caller
    /// shows up, factor into `tests/common/`.
    struct SurrealContainer {
        endpoint: String,
        _container: ContainerAsync<SurrealDb>,
    }

    impl SurrealContainer {
        async fn start() -> Self {
            // Retry transient Docker daemon timeouts (`CreateContainer(RequestTimeoutError)`)
            // observed under CI runner contention when the cluster-tests
            // lane brings up many SurrealDB containers in close succession.
            const MAX_ATTEMPTS: u32 = 3;
            let mut last_err: Option<String> = None;
            for attempt in 1..=MAX_ATTEMPTS {
                match SurrealDb::default()
                    .with_tag(CLUSTER_SURREALDB_IMAGE_TAG)
                    .start()
                    .await
                {
                    Ok(container) => {
                        let port = container
                            .get_host_port_ipv4(SURREALDB_PORT)
                            .await
                            .expect("get SurrealDB mapped port");
                        return Self {
                            endpoint: format!("ws://127.0.0.1:{port}"),
                            _container: container,
                        };
                    }
                    Err(e) => {
                        let msg = format!("{e}");
                        eprintln!(
                            "SurrealContainer::start attempt {attempt}/{MAX_ATTEMPTS} failed: {msg}"
                        );
                        last_err = Some(msg);
                        if attempt < MAX_ATTEMPTS {
                            sleep(Duration::from_secs(2 * u64::from(attempt))).await;
                        }
                    }
                }
            }
            panic!(
                "start SurrealDB container — cluster-tests require Docker or Podman (failed after {MAX_ATTEMPTS} attempts): {}",
                last_err.unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    /// In-process TCP forwarder that sits between the runner and SurrealDB
    /// and delays every server→client byte chunk by `latency`. Closely
    /// emulates toxiproxy's `latency` toxin in the response direction:
    /// queries flow upstream immediately, but each WS frame the SurrealDB
    /// server writes back is held for `latency` before being forwarded —
    /// so each `db.query(...).await` round trip on the runner takes roughly
    /// `latency` longer than it would against SurrealDB directly.
    ///
    /// Pure tokio, no external `toxiproxy-server` binary or HTTP control
    /// plane: the issue says "toxiproxy" but the contract is "5s latency on
    /// all queries", and this implementation pins that contract without
    /// adding a non-Rust runtime dependency to the test crate.
    ///
    /// Latency injection is gated by an atomic flag so the test can let
    /// boot traffic pass through unimpeded (otherwise the runner spends
    /// minutes traversing schema-init / cluster-registration queries
    /// through the proxy) and only inject latency once `/diagnose` polling
    /// is about to start. Use [`LatencyProxy::enable`] / [`disable`].
    struct LatencyProxy {
        endpoint: String,
        latency: Duration,
        enabled: Arc<std::sync::atomic::AtomicBool>,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        accept_handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl LatencyProxy {
        async fn start(upstream_ws_endpoint: &str, latency: Duration) -> Self {
            let upstream_addr: SocketAddr = upstream_ws_endpoint
                .strip_prefix("ws://")
                .expect("upstream endpoint must be ws://host:port")
                .parse()
                .expect("upstream endpoint must parse as SocketAddr");

            let (listener, local) = bind_ephemeral_tokio("127.0.0.1")
                .await
                .expect("bind latency proxy listener");
            let endpoint = format!("ws://{local}");

            let enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let accept_enabled = enabled.clone();
            let accept_handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown_rx => return,
                        accept = listener.accept() => {
                            let Ok((inbound, _)) = accept else {
                                continue;
                            };
                            let session_enabled = accept_enabled.clone();
                            tokio::spawn(async move {
                                let outbound = match TcpStream::connect(upstream_addr).await {
                                    Ok(stream) => stream,
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            upstream = %upstream_addr,
                                            "latency proxy: upstream dial failed"
                                        );
                                        return;
                                    }
                                };
                                if let Err(e) = forward_with_response_latency(
                                    inbound, outbound, latency, session_enabled,
                                )
                                .await
                                {
                                    // EOF and reset are normal at session end; warn
                                    // for anything else so a flaky proxy is loud.
                                    if e.kind() != std::io::ErrorKind::UnexpectedEof
                                        && e.kind() != std::io::ErrorKind::BrokenPipe
                                        && e.kind() != std::io::ErrorKind::ConnectionReset
                                    {
                                        tracing::warn!(error = %e, "latency proxy session error");
                                    }
                                }
                            });
                        }
                    }
                }
            });

            Self {
                endpoint,
                latency,
                enabled,
                shutdown: Some(shutdown_tx),
                accept_handle: Some(accept_handle),
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        fn enable(&self) {
            self.enabled
                .store(true, std::sync::atomic::Ordering::Release);
            tracing::info!(
                latency_ms = self.latency.as_millis() as u64,
                "latency proxy enabled"
            );
        }
    }

    impl Drop for LatencyProxy {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.accept_handle.take() {
                handle.abort();
            }
        }
    }

    /// Copy bytes in both directions. Client→server is forwarded immediately;
    /// server→client is delayed by `latency` per chunk *only when `enabled`
    /// is true* — when disabled the proxy is a transparent forwarder, so the
    /// runner can boot through it at full speed and the test only pays the
    /// latency cost during the polling window.
    async fn forward_with_response_latency(
        inbound: TcpStream,
        outbound: TcpStream,
        latency: Duration,
        enabled: Arc<std::sync::atomic::AtomicBool>,
    ) -> std::io::Result<()> {
        let (mut in_r, mut in_w) = inbound.into_split();
        let (mut out_r, mut out_w) = outbound.into_split();

        // Client → server: no extra latency.
        let upstream = tokio::spawn(async move {
            let res = tokio::io::copy(&mut in_r, &mut out_w).await;
            let _ = out_w.shutdown().await;
            res.map(|_| ())
        });

        // Server → client: per-chunk latency injection (only while enabled).
        let downstream = tokio::spawn(async move {
            let mut buf = vec![0u8; 8 * 1024];
            loop {
                let n = match out_r.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => return Err(e),
                };
                if enabled.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::time::sleep(latency).await;
                }
                in_w.write_all(&buf[..n]).await?;
            }
            in_w.shutdown().await
        });

        // Resolve `JoinError`s on both arms before checking inner results, so
        // an error on one direction never silently drops the other arm's
        // result. Concretely: if `upstream` errored and we returned `up?`
        // immediately, `down`'s `Result` would be discarded. With this
        // ordering we surface the first inner error via `?` but always
        // observe both `JoinError` outcomes first.
        let (up, down) = tokio::join!(upstream, downstream);
        let up_inner = up.map_err(|e| std::io::Error::other(format!("upstream task: {e}")))?;
        let down_inner =
            down.map_err(|e| std::io::Error::other(format!("downstream task: {e}")))?;
        up_inner?;
        down_inner?;
        Ok(())
    }

    /// Issue #341 T3 — `/diagnose` exposes `cluster_heartbeat_status` and
    /// `cluster_heartbeat_lag_ms` reflecting heartbeat health under a
    /// degraded SurrealDB.
    ///
    /// Setup: real SurrealDB testcontainer fronted by an in-process TCP
    /// proxy that, **once enabled**, delays every server→client byte chunk
    /// by [`LATENCY_SECS`] (emulates toxiproxy's `latency` toxin). The
    /// proxy is a transparent forwarder during boot so the runner can
    /// register and start its heartbeat at full speed; latency is enabled
    /// only after boot, before polling starts. This keeps the per-test
    /// wall-clock inside nextest's slow-test budget while still inducing
    /// observable heartbeat degradation.
    ///
    /// Polls `/diagnose` over a window sized to capture multiple
    /// heartbeat ticks under latency; at least one sample must show
    /// `cluster_heartbeat.status == "degraded"`, OR a typed
    /// `cluster_heartbeat.last_error_kind`, OR `lag_ms` past the
    /// staleness threshold. Multi-sample polling avoids a false-pass when
    /// a heartbeat happens to land just before a single-shot snapshot.
    ///
    /// Gated behind `cluster-tests`: needs Docker/Podman like the rest of
    /// the cluster suite.
    #[tokio::test]
    async fn t3_surrealdb_latency_named_heartbeat_health() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

        let surreal = SurrealContainer::start().await;
        let proxy = LatencyProxy::start(&surreal.endpoint, Duration::from_secs(LATENCY_SECS)).await;

        // Cluster mode: --surreal-endpoint pointed at the proxy, plus a
        // routable advertised endpoint (10.x is RFC1918, the same trick the
        // existing cluster suite uses to satisfy the SSRF validator). The
        // runner won't actually accept cross-runner traffic on that address —
        // the heartbeat task only writes UPSERTs against `cluster_runners`,
        // which is what we're stress-testing. Boot runs without latency
        // injection (proxy in transparent-forward mode), so the readiness
        // budget mirrors the cluster suite's standard budget.
        let runner = StandaloneRunner::start_with_options(
            &[("SURREAL_USER", "root"), ("SURREAL_PASS", "root")],
            &[
                "--surreal-endpoint",
                proxy.endpoint(),
                "--runner-endpoint",
                FAKE_CLUSTER_RUNNER_ENDPOINT,
            ],
            None,
        )
        .await;

        // Enable latency injection now that the runner is up and the
        // heartbeat task is ticking. Subsequent ticks pay LATENCY_SECS per
        // server→client chunk, which pushes per-tick wall-clock past
        // STALE_LAG_MULTIPLIER × interval and flips status to `degraded`.
        proxy.enable();

        let client = reqwest::Client::new();
        // Threshold sits between the healthy ceiling (lag oscillates
        // 0..interval = 0..5000ms) and the under-latency ceiling
        // (per-tick wall-clock ≥ LATENCY_SECS).
        const LAG_DEGRADED_THRESHOLD_MS: u64 = 8000;
        // Window = LATENCY_SECS + 2 × interval gives the heartbeat task at
        // least two full ticks under injection.
        let poll_window = Duration::from_secs(LATENCY_SECS + 10);
        let poll_interval = Duration::from_millis(500);
        let deadline = std::time::Instant::now() + poll_window;

        let mut saw_degraded_status = false;
        let mut saw_error_kind: Option<String> = None;
        let mut max_lag_ms: u64 = 0;
        let mut last_sample: Option<DiagnoseSnapshot> = None;

        while std::time::Instant::now() < deadline {
            let diagnose: DiagnoseSnapshot = client
                .get(format!("{}/diagnose", runner.base_url))
                .send()
                .await
                .expect("GET /diagnose")
                .json()
                .await
                .expect("decode /diagnose body — schema drift?");

            if let Some(hb) = &diagnose.cluster_heartbeat {
                if hb.status == "degraded" {
                    saw_degraded_status = true;
                }
                if let Some(kind) = &hb.last_error_kind {
                    saw_error_kind = Some(kind.clone());
                }
                if let Some(lag) = hb.lag_ms {
                    max_lag_ms = max_lag_ms.max(lag);
                }
            }
            last_sample = Some(diagnose);

            if saw_degraded_status
                || saw_error_kind.is_some()
                || max_lag_ms > LAG_DEGRADED_THRESHOLD_MS
            {
                break;
            }
            sleep(poll_interval).await;
        }

        // Bounded log tail keeps the failure payload small even when the
        // runner has been booting for minutes under per-query latency
        // (full log can grow to many MB with debug tracing on each query).
        let log_tail = runner.log_tail(100);
        assert!(
            saw_degraded_status
                || saw_error_kind.is_some()
                || max_lag_ms > LAG_DEGRADED_THRESHOLD_MS,
            "expected /diagnose to surface heartbeat degradation under \
             SurrealDB latency injection (status='degraded', or a typed \
             cluster_heartbeat.last_error_kind, or \
             cluster_heartbeat.lag_ms > {LAG_DEGRADED_THRESHOLD_MS}ms) over a \
             {poll_window:?} polling window. \
             saw_degraded_status={saw_degraded_status}, \
             saw_error_kind={saw_error_kind:?}, \
             max_lag_ms={max_lag_ms}, last sample: {last_sample:?}. \
             Runner log tail:\n{log_tail}"
        );
    }
}
