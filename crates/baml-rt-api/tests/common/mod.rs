//! Shared helpers for `baml-rt-api` integration tests.
//!
//! Each integration-test file is a separate compilation unit, so cargo
//! warns about items that one file uses but the other doesn't. The
//! `#[allow(dead_code)]` lets a single helper live here without being
//! referenced from every test file.
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_api::{
    ClusterDirectoryError, ClusterDirectoryService, ClusterHeartbeatHealth, ClusterMode,
    ClusterPlacementInfo, ClusterRunnerInfo, ClusterTopology,
};

/// Empty `ClusterDirectoryService` used by tests that need to construct
/// `ClusterTopology::Cluster` (auth-tier and `/diagnose` boundary tests)
/// without spinning up a real SurrealDB. Returns empty lists from both
/// read methods. Lives here so a future trait method addition lands in
/// one place rather than silently bit-rotting two separate stubs.
pub struct StubClusterDirectory;

#[async_trait]
impl ClusterDirectoryService for StubClusterDirectory {
    fn local_runner_id(&self) -> &str {
        ""
    }

    async fn list_runners(&self) -> Result<Vec<ClusterRunnerInfo>, ClusterDirectoryError> {
        Ok(Vec::new())
    }

    async fn list_placements(&self) -> Result<Vec<ClusterPlacementInfo>, ClusterDirectoryError> {
        Ok(Vec::new())
    }
}

/// Project a `ClusterMode` knob (the only auth-relevant axis for these
/// tests) into a `ClusterTopology` with stubs in cluster mode. The
/// returned `Cluster` variant carries a fresh heartbeat handle and the
/// shared `StubClusterDirectory`.
pub fn cluster_topology_for_test(mode: ClusterMode) -> ClusterTopology {
    match mode {
        ClusterMode::Standalone => ClusterTopology::Standalone,
        ClusterMode::Cluster => ClusterTopology::Cluster {
            directory: Arc::new(StubClusterDirectory),
            heartbeat: ClusterHeartbeatHealth::new(std::time::Duration::from_secs(5)),
        },
    }
}
