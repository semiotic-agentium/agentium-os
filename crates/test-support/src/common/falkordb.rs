//! Shared FalkorDB test container helpers.
//!
//! Timeout behaviour:
//! - **Container start:** up to 120s per attempt, 3 attempts (with 5s delay between).
//! - **Port discovery:** up to 25 × 200ms after start.
//! - **Ready poll:** up to `WAIT_READY_ATTEMPTS` × (query timeout + 1s sleep). Each query is
//!   bounded by `WAIT_READY_QUERY_TIMEOUT` so a single TCP/connect hang cannot block indefinitely.

use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use text_to_cypher::core::execute_cypher_query;
use tokio::sync::OnceCell;
use tokio::time::{Duration, sleep, timeout};

struct SharedFalkorDb {
    _container: testcontainers::ContainerAsync<GenericImage>,
    connection: String,
}

static SHARED: OnceCell<SharedFalkorDb> = OnceCell::const_new();

/// Returns a connection string to a FalkorDB instance. Singleton is **per process**:
/// within one test binary all tests share one container; across binaries (e.g. nextest
/// runs each integration test binary in a separate process) we get one init per binary.
///
/// **CI:** Image is pre-pulled in CI; init is still Docker create/start + readiness
/// poll, so each of the ~5 FalkorDB-using binaries pays that cost once per run. To
/// avoid repeated inits, CI can start one container and set `FALKORDB_CONNECTION`;
/// when set, we use it and skip starting testcontainers (see below).
///
/// All tests within a binary share this single container, so each test **must**
/// use a distinct graph name to avoid cross-contamination.
const FALKORDB_CONNECTION_ENV: &str = "FALKORDB_CONNECTION";

static ENV_CONNECTION: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();

/// Returns a connection string to a shared FalkorDB container that lives for the
/// entire test process, or the URL from `FALKORDB_CONNECTION` if set (CI single-container mode).
///
/// When `FALKORDB_CONNECTION` is set, we use it and do not start testcontainers.
/// That allows CI to start one container once and run all FalkorDB tests against it.
/// The instance at that URL is assumed already ready (CI must start and wait before tests).
pub async fn shared_falkordb() -> &'static str {
    let env_url = ENV_CONNECTION.get_or_init(|| {
        std::env::var(FALKORDB_CONNECTION_ENV).ok().and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                let leaked: &'static str = Box::leak(t.to_string().into_boxed_str());
                Some(leaked)
            }
        })
    });
    if let Some(url) = env_url {
        eprintln!(
            "shared_falkordb: using FALKORDB_CONNECTION from environment (no testcontainers)"
        );
        return url;
    }
    let shared = SHARED
        .get_or_init(|| async {
            let t0 = std::time::Instant::now();
            let (container, connection) = start_falkordb().await;
            let start_elapsed = t0.elapsed();
            tracing::debug!(
                start_elapsed_ms = start_elapsed.as_millis() as u64,
                "FalkorDB container started"
            );
            wait_for_falkordb(&connection, "_shared_init").await;
            tracing::info!(
                total_elapsed_ms = t0.elapsed().as_millis() as u64,
                start_ms = start_elapsed.as_millis() as u64,
                "FalkorDB shared init ready (singleton per process; nextest runs each binary in a new process)"
            );
            SharedFalkorDb {
                _container: container,
                connection,
            }
        })
        .await;
    &shared.connection
}

const START_RETRIES: u32 = 3;
const START_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Max attempts to poll FalkorDB with "RETURN 1" before giving up.
const WAIT_READY_ATTEMPTS: u32 = 120;
/// Per-query timeout so a single connect/query cannot hang indefinitely.
const WAIT_READY_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

async fn start_falkordb() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let mut last_err: Option<String> = None;
    for attempt in 1..=START_RETRIES {
        let image = GenericImage::new("falkordb/falkordb", "latest")
            .with_exposed_port(ContainerPort::Tcp(6379))
            .with_startup_timeout(Duration::from_secs(120));
        match image.start().await {
            Ok(container) => {
                let mut port_attempts = 0;
                let host_port = loop {
                    match container.get_host_port_ipv4(6379).await {
                        Ok(port) => break port,
                        Err(err) => {
                            port_attempts += 1;
                            assert!(port_attempts <= 25, "get falkordb port after start: {err}");
                            sleep(Duration::from_millis(200)).await;
                        }
                    }
                };
                return (container, format!("falkor://127.0.0.1:{host_port}"));
            }
            Err(e) => {
                last_err = Some(format!("{e:?}"));
                if attempt < START_RETRIES {
                    tracing::warn!(
                        attempt,
                        "FalkorDB container start failed (e.g. RequestTimeoutError), retrying in {:?}",
                        START_RETRY_DELAY
                    );
                    sleep(START_RETRY_DELAY).await;
                }
            }
        }
    }
    panic!(
        "start falkordb container (after {} attempts): {:?}. \
         Transient Docker/network slowness can cause CreateContainer RequestTimeoutError; \
         ensure Docker is running and consider increasing CI timeout or reducing parallelism for falkordb-tests.",
        START_RETRIES, last_err
    );
}

/// Polls FalkorDB until a simple query succeeds. Each attempt is bounded by
/// `WAIT_READY_QUERY_TIMEOUT` so a single hung connect/query does not block indefinitely.
async fn wait_for_falkordb(connection: &str, graph: &str) {
    for attempt in 1..=WAIT_READY_ATTEMPTS {
        let ok = timeout(
            WAIT_READY_QUERY_TIMEOUT,
            execute_cypher_query("RETURN 1", graph, connection, false),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some();
        if ok {
            return;
        }
        if attempt % 15 == 0 {
            tracing::debug!(
                attempt,
                max = WAIT_READY_ATTEMPTS,
                "FalkorDB not ready yet (query timeout {:?} per attempt)",
                WAIT_READY_QUERY_TIMEOUT
            );
        }
        sleep(Duration::from_secs(1)).await;
    }
    panic!(
        "falkordb did not become ready after {} attempts (each query bounded by {:?})",
        WAIT_READY_ATTEMPTS, WAIT_READY_QUERY_TIMEOUT
    );
}
