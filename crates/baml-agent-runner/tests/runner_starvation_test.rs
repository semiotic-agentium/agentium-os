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
//! - **T3 — SurrealDB latency injection** *(gated behind `cluster-tests`)*.
//!   Front a real SurrealDB endpoint (testcontainer) with an in-process
//!   TCP latency-injecting forwarder that delays every server→client byte
//!   chunk by 5s, then boot the runner pointed at the proxy in cluster
//!   mode. Asserts: `/diagnose` surfaces a *named* cluster-heartbeat
//!   health field so operators can detect heartbeat degradation. Currently
//!   expected to fail because the heartbeat task is fire-and-forget — its
//!   only error signal is `tracing::warn!(error = %e, "cluster heartbeat
//!   failed")` and `/diagnose` has no `cluster_heartbeat_*` field.
//!
//! T2 (cgroup-throttled deploy) lives in the e2e/k8s lane.

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
    #[allow(dead_code)] // T1/T4 use the full `log_text` (small logs); only T3 needs a tail.
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

// ═══════════════════════════════════════════════════════════════════════════
// T3 — SurrealDB latency injection
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "cluster-tests")]
mod surrealdb_latency {
    use std::net::SocketAddr;

    use common::{CLUSTER_SURREALDB_IMAGE_TAG, FAKE_CLUSTER_RUNNER_ENDPOINT};
    use testcontainers_modules::{
        surrealdb::{SURREALDB_PORT, SurrealDb},
        testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener as TokioTcpListener, TcpStream},
    };

    use super::*;

    /// Latency injected on every server→client byte chunk by the proxy. Matches
    /// the issue's "5s latency to all queries" — bytes flowing from SurrealDB
    /// back to the runner are held for this long, so each `db.query(...).await`
    /// experiences ~5s of artificial round-trip latency.
    const LATENCY_SECS: u64 = 5;

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
    struct LatencyProxy {
        endpoint: String,
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

            let listener = TokioTcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind latency proxy listener");
            let local = listener.local_addr().expect("proxy local addr");
            let endpoint = format!("ws://{local}");

            let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let accept_handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown_rx => return,
                        accept = listener.accept() => {
                            let Ok((inbound, _)) = accept else {
                                continue;
                            };
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
                                    inbound, outbound, latency,
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
                shutdown: Some(shutdown_tx),
                accept_handle: Some(accept_handle),
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
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
    /// server→client is delayed by `latency` per chunk, simulating "the
    /// SurrealDB server takes `latency` to begin replying to each query".
    async fn forward_with_response_latency(
        inbound: TcpStream,
        outbound: TcpStream,
        latency: Duration,
    ) -> std::io::Result<()> {
        let (mut in_r, mut in_w) = inbound.into_split();
        let (mut out_r, mut out_w) = outbound.into_split();

        // Client → server: no extra latency.
        let upstream = tokio::spawn(async move {
            let res = tokio::io::copy(&mut in_r, &mut out_w).await;
            let _ = out_w.shutdown().await;
            res.map(|_| ())
        });

        // Server → client: per-chunk latency injection.
        let downstream = tokio::spawn(async move {
            let mut buf = vec![0u8; 8 * 1024];
            loop {
                let n = match out_r.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => return Err(e),
                };
                tokio::time::sleep(latency).await;
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

    /// **Regression target — currently expected to panic.**
    ///
    /// The cluster heartbeat task in
    /// `crates/baml-agent-runner/src/cluster.rs` (`spawn_heartbeat`) is
    /// fire-and-forget. On error it only does
    /// `tracing::warn!(error = %e, "cluster heartbeat failed")` — there is
    /// no `BamlRtError::ClusterHeartbeat` variant, no Rust-side state, and
    /// no operator-visible surface that exposes heartbeat health. The
    /// `last_heartbeat_ms` field lives only in SurrealDB's `cluster_runners`
    /// row; the runner does not read it back. So when SurrealDB goes
    /// degraded, the runner has no way to tell operators "my heartbeats are
    /// failing" short of reading the runner pod's stderr.
    ///
    /// This test forces that exact failure. We:
    ///
    ///   1. Start a real SurrealDB testcontainer.
    ///   2. Front it with an in-process TCP proxy that delays every
    ///      server→client byte chunk by 5s — emulating toxiproxy's `latency`
    ///      toxin in the response direction.
    ///   3. Boot the runner in cluster mode pointed at the proxy.
    ///   4. Wait long enough for at least one heartbeat round-trip to land
    ///      under the latency injection.
    ///   5. Assert that `/diagnose` exposes a *named* cluster-heartbeat
    ///      health field (e.g. `cluster_heartbeat_status`,
    ///      `cluster_heartbeat_lag_ms`, or `cluster_heartbeat_last_ok_ms`)
    ///      so an operator can detect the degradation.
    ///
    /// The assertion fails today because `/diagnose` only returns
    /// `runtime_progress_lag_ms` and `event_producers_loaded`. The
    /// `#[should_panic]` keeps CI green while preserving this as a
    /// regression target. When the heartbeat task is fixed (a named
    /// `BamlRtError::ClusterHeartbeat{...}` error variant *and* a
    /// `/diagnose` field that surfaces stale heartbeats) this test will
    /// fail with "did not panic as expected" — at which point the fixer
    /// should remove `#[should_panic]` and tighten the assertion to check
    /// the field's *value* under degradation, not just its presence.
    ///
    /// Gated behind `cluster-tests` because it spins up a SurrealDB
    /// container and matches the same Docker/Podman requirement as the
    /// existing cluster suite. Sized for a loaded CI worker: the runner
    /// makes ~10–20 SurrealDB queries during boot (provenance schema +
    /// cluster registration + config service), each delayed by 5s, so the
    /// readiness deadline is widened accordingly.
    #[tokio::test]
    #[should_panic(expected = "expected /diagnose to surface a cluster-heartbeat health field")]
    async fn t3_surrealdb_latency_named_heartbeat_health() {
        let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

        let surreal = SurrealContainer::start().await;
        let proxy = LatencyProxy::start(&surreal.endpoint, Duration::from_secs(LATENCY_SECS)).await;

        // Cluster mode: --surreal-endpoint pointed at the proxy, plus a
        // routable advertised endpoint (10.x is RFC1918, the same trick the
        // existing cluster suite uses to satisfy the SSRF validator). The
        // runner won't actually accept cross-runner traffic on that address —
        // the heartbeat task only writes UPSERTs against `cluster_runners`,
        // which is what we're stress-testing.
        let runner = StandaloneRunner::start_with_options(
            &[("SURREAL_USER", "root"), ("SURREAL_PASS", "root")],
            &[
                "--surreal-endpoint",
                proxy.endpoint(),
                "--runner-endpoint",
                FAKE_CLUSTER_RUNNER_ENDPOINT,
            ],
            // Boot has to traverse ~10–20 server→client chunks each held for
            // LATENCY_SECS, plus the testcontainer pull on cold runners.
            Some(e2e_secs_ci_or_local(600, 360)),
        )
        .await;

        // Wait for at least one heartbeat round-trip under latency so the
        // task has had a chance to surface either a named error or a stale
        // signal. Heartbeat interval is 5s; with 5s of injected latency the
        // first heartbeat completes around T+10s of cluster-mgr lifetime.
        sleep(Duration::from_secs(LATENCY_SECS * 3)).await;

        let client = reqwest::Client::new();
        let diagnose: Value = client
            .get(format!("{}/diagnose", runner.base_url))
            .send()
            .await
            .expect("GET /diagnose")
            .json()
            .await
            .expect("decode /diagnose body");

        let exposes_heartbeat_health = ["cluster_heartbeat_status", "cluster_heartbeat_lag_ms"]
            .iter()
            .any(|key| diagnose.get(*key).is_some());

        // Bounded log tail keeps the panic payload small even when the
        // runner has been booting for minutes under per-query latency
        // (full log can grow to many MB with debug tracing on each query).
        let log_tail = runner.log_tail(100);
        assert!(
            exposes_heartbeat_health,
            "expected /diagnose to surface a cluster-heartbeat health field \
             (e.g. cluster_heartbeat_status / cluster_heartbeat_lag_ms) so \
             operators can detect heartbeat degradation under SurrealDB \
             latency, but only got: {diagnose}. Today the heartbeat task \
             only logs `tracing::warn!(error = %e, \"cluster heartbeat \
             failed\")` — there is no named error variant on BamlRtError \
             and no operator-visible surface that exposes stale \
             last_heartbeat_ms. Runner log tail:\n{log_tail}"
        );
    }
}
