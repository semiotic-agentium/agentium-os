//! Shared FalkorDB test container helpers.

use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt, ReuseDirective};
use text_to_cypher::core::execute_cypher_query;
use tokio::sync::OnceCell;
use tokio::time::{Duration, sleep};
use tracing::warn;

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

async fn start_falkordb() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let base_image = GenericImage::new("falkordb/falkordb", "latest")
        .with_exposed_port(ContainerPort::Tcp(6379));
    let mut attempt = 0;
    let container: testcontainers::ContainerAsync<GenericImage> = loop {
        attempt += 1;
        let image = base_image
            .clone()
            .with_container_name("baml-falkordb-tests")
            .with_reuse(ReuseDirective::Always)
            .with_startup_timeout(Duration::from_secs(180));
        match image.start().await {
            Ok(container) => break container,
            Err(err) => {
                if attempt >= 4 {
                    panic!("start falkordb container: {err}");
                }
                warn!(
                    attempt,
                    "start falkordb container failed; retrying after delay: {err}"
                );
                sleep(Duration::from_secs(3 * attempt as u64)).await;
            }
        }
    };
    let mut attempts = 0;
    let host_port = loop {
        match container.get_host_port_ipv4(6379).await {
            Ok(port) => break port,
            Err(err) => {
                attempts += 1;
                assert!(attempts <= 25, "get falkordb port: {err}");
                sleep(Duration::from_millis(200)).await;
            }
        }
    };
    (container, format!("falkor://127.0.0.1:{host_port}"))
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
