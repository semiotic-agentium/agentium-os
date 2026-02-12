//! Shared FalkorDB test container helpers.

use testcontainers::GenericImage;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use text_to_cypher::core::execute_cypher_query;
use tokio::time::{Duration, sleep};

/// Starts a FalkorDB container and returns the container handle and connection string.
/// The container handle must be held alive for the duration of the test.
pub async fn start_falkordb() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let image = GenericImage::new("falkordb/falkordb", "latest")
        .with_exposed_port(ContainerPort::Tcp(6379));
    let container = image.start().await.expect("start falkordb container");
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
pub async fn wait_for_falkordb(connection: &str, graph: &str) {
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
