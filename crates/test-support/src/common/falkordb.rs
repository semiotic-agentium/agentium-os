//! Shared FalkorDB test container helpers.

use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use text_to_cypher::core::execute_cypher_query;
use tokio::sync::OnceCell;
use tokio::time::{Duration, sleep};

struct SharedFalkorDb {
    _container: testcontainers::ContainerAsync<GenericImage>,
    connection: String,
}

static SHARED: OnceCell<SharedFalkorDb> = OnceCell::const_new();

/// Returns a connection string to a shared FalkorDB container that lives for the
/// entire test process. The container is started lazily on first call and reused
/// by all subsequent callers.
///
/// All tests within a binary share this single container, so each test **must**
/// use a distinct graph name to avoid cross-contamination.
pub async fn shared_falkordb() -> &'static str {
    let shared = SHARED
        .get_or_init(|| async {
            let (container, connection) = start_falkordb().await;
            wait_for_falkordb(&connection, "_shared_init").await;
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

/// Polls FalkorDB until a simple query succeeds, up to 120 attempts at 1-second intervals.
async fn wait_for_falkordb(connection: &str, graph: &str) {
    for _ in 0..120 {
        if execute_cypher_query("RETURN 1", graph, connection, false)
            .await
            .is_ok()
        {
            return;
        }
        sleep(Duration::from_secs(1)).await;
    }
    panic!("falkordb did not become ready after 120 attempts");
}
