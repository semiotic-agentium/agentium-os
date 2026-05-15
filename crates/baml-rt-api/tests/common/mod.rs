//! Shared scaffolding for `baml-rt-api` integration tests.
//!
//! `StubClusterDirectory` lets tests build a `ClusterTopology::Cluster`
//! without standing up a real SurrealDB-backed directory; auth-boundary and
//! heartbeat tests never hit `/cluster/agents`, so empty listings suffice.
//!
//! Each integration test file in `tests/` is a separate binary that compiles
//! this module independently; items only used by some files would otherwise
//! trip `dead_code` in others.
#![allow(dead_code)]

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_api::{
    ClusterDirectoryError, ClusterDirectoryService, ClusterHeartbeatHealth, ClusterMode,
    ClusterPlacementInfo, ClusterRunnerInfo, ClusterTopology,
};

pub struct StubClusterDirectory;

#[async_trait]
impl ClusterDirectoryService for StubClusterDirectory {
    fn local_runner_id(&self) -> &str {
        "stub-runner"
    }
    async fn list_runners(
        &self,
    ) -> std::result::Result<Vec<ClusterRunnerInfo>, ClusterDirectoryError> {
        Ok(Vec::new())
    }
    async fn list_placements(
        &self,
    ) -> std::result::Result<Vec<ClusterPlacementInfo>, ClusterDirectoryError> {
        Ok(Vec::new())
    }
}

/// Build a `ClusterTopology` from a `ClusterMode` dial. Cluster mode wires in
/// a stub directory and a fresh heartbeat handle so the typestate invariant —
/// `Cluster` always carries both dependencies — holds at the type level.
pub fn topology_for_test_mode(mode: ClusterMode) -> ClusterTopology {
    match mode {
        ClusterMode::Standalone => ClusterTopology::Standalone,
        ClusterMode::Cluster => ClusterTopology::Cluster {
            directory: Arc::new(StubClusterDirectory),
            heartbeat: ClusterHeartbeatHealth::new(Duration::from_secs(5)),
        },
    }
}
