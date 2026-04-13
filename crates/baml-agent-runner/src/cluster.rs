//! Cluster registration and agent placement for multi-runner deployments.
//!
//! When connected to a shared SurrealDB, each runner registers itself and
//! records which agents it hosts. The `PlacementResolver` implements
//! [`ClusterEndpointResolver`] so the router can forward A2A requests to
//! the runner currently hosting a given agent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{AgentRouteKey, BamlRtError, DeploymentContentHash};
use surrealdb::{Surreal, engine::any::Any};

use crate::routing::ClusterEndpointResolver;

// ---------------------------------------------------------------------------
// Runner identity
// ---------------------------------------------------------------------------

/// Identity of a runner instance within the cluster.
#[derive(Debug, Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct RunnerId(String);

impl RunnerId {
    pub(crate) fn new_random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity of this runner instance within the cluster.
#[derive(Debug, Clone)]
pub(crate) struct RunnerIdentity {
    pub(crate) runner_id: RunnerId,
    pub(crate) endpoint: String,
}

impl RunnerIdentity {
    pub(crate) fn new(endpoint: String) -> Self {
        Self {
            runner_id: RunnerId::new_random(),
            endpoint,
        }
    }
}

// ---------------------------------------------------------------------------
// Placement resolver (implements ClusterEndpointResolver)
// ---------------------------------------------------------------------------

/// Resolves agent placements by querying the shared SurrealDB `cluster_agent_placements` table.
pub(crate) struct PlacementResolver {
    db: Arc<Surreal<Any>>,
    local_runner_id: RunnerId,
}

impl PlacementResolver {
    pub(crate) fn new(db: Arc<Surreal<Any>>, local_runner_id: RunnerId) -> Self {
        Self {
            db,
            local_runner_id,
        }
    }
}

#[async_trait]
impl ClusterEndpointResolver for PlacementResolver {
    async fn resolve(&self, key: &AgentRouteKey) -> baml_rt_core::Result<Option<String>> {
        let pkg = key.agent_package.as_str();
        let inst = key.agent_instance_id.as_str();
        let result: Result<Vec<serde_json::Value>, _> = self
            .db
            .query("SELECT * FROM cluster_agent_placements WHERE agent_package = $pkg AND agent_instance_id = $inst AND status = 'active' LIMIT 1")
            .bind(("pkg", pkg.to_string()))
            .bind(("inst", inst.to_string()))
            .await
            .and_then(|mut r| r.take(0));

        match result {
            Ok(rows) => {
                let Some(row) = rows.into_iter().next() else {
                    return Ok(None);
                };
                let Some(runner_id) = row.get("runner_id").and_then(|v| v.as_str()) else {
                    return Ok(None);
                };
                if runner_id == self.local_runner_id.as_str() {
                    return Ok(None);
                }
                let endpoint = row
                    .get("runner_endpoint")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        BamlRtError::Io(std::io::Error::other(format!(
                            "placement for {pkg}/{inst} on runner {runner_id} missing runner_endpoint (data corruption)"
                        )))
                    })?;
                Ok(Some(endpoint.to_string()))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    agent = %pkg,
                    instance = %inst,
                    "cluster placement lookup failed"
                );
                Err(BamlRtError::Io(std::io::Error::other(format!(
                    "cluster placement lookup: {e}"
                ))))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cluster operations (register runner, record/remove placements, heartbeat)
// ---------------------------------------------------------------------------

/// Manages cluster state in shared SurrealDB.
pub(crate) struct ClusterManager {
    db: Arc<Surreal<Any>>,
    identity: RunnerIdentity,
}

impl ClusterManager {
    pub(crate) async fn new(
        db: Arc<Surreal<Any>>,
        identity: RunnerIdentity,
    ) -> Result<Self, BamlRtError> {
        let mgr = Self { db, identity };
        mgr.init_schema().await?;
        mgr.register_runner().await?;
        Ok(mgr)
    }

    async fn init_schema(&self) -> Result<(), BamlRtError> {
        let mut resp = self
            .db
            .query(
                "DEFINE TABLE IF NOT EXISTS cluster_runners SCHEMALESS;\
                 DEFINE TABLE IF NOT EXISTS cluster_agent_placements SCHEMALESS;\
                 DEFINE INDEX IF NOT EXISTS idx_placement_agent ON cluster_agent_placements FIELDS agent_package, agent_instance_id UNIQUE",
            )
            .await
            .map_err(|e| BamlRtError::Io(std::io::Error::other(format!("cluster: schema init transport: {e}"))))?;

        // Check each DDL statement individually — transport-level Ok does not
        // guarantee individual statements succeeded.
        for i in 0..3usize {
            resp.take::<Option<serde_json::Value>>(i).map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "cluster: schema init statement {i}: {e}"
                )))
            })?;
        }
        Ok(())
    }

    async fn register_runner(&self) -> Result<(), BamlRtError> {
        let pod_name = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
        let mut resp = self
            .db
            .query(
                "UPSERT type::record('cluster_runners', $runner_id) SET \
                 runner_id = $runner_id, \
                 endpoint = $endpoint, \
                 pod_name = $pod_name, \
                 last_heartbeat_ms = time::millis(time::now())",
            )
            .bind(("runner_id", self.identity.runner_id.to_string()))
            .bind(("endpoint", self.identity.endpoint.clone()))
            .bind(("pod_name", pod_name))
            .await
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "cluster: runner registration transport: {e}"
                )))
            })?;
        resp.take::<Option<serde_json::Value>>(0).map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "cluster: runner registration query: {e}"
            )))
        })?;
        tracing::info!(
            runner_id = %self.identity.runner_id,
            endpoint = %self.identity.endpoint,
            "registered runner in cluster"
        );
        Ok(())
    }

    /// Record that this runner now hosts the given agent.
    pub(crate) async fn record_placement(
        &self,
        key: &AgentRouteKey,
        content_hash: &DeploymentContentHash,
    ) -> Result<(), BamlRtError> {
        // Use `/` as separator: it is rejected by AgentPackageName and AgentInstanceId
        // validators so it cannot appear in either component, preventing ambiguity.
        let placement_key = format!(
            "{pkg}/{inst}",
            pkg = key.agent_package.as_str(),
            inst = key.agent_instance_id.as_str(),
        );
        let mut resp = self
            .db
            .query(
                "UPSERT type::record('cluster_agent_placements', $placement_key) SET \
                 agent_package = $pkg, \
                 agent_instance_id = $inst, \
                 content_hash = $hash, \
                 runner_id = $runner_id, \
                 runner_endpoint = $endpoint, \
                 status = 'active'",
            )
            .bind(("placement_key", placement_key))
            .bind(("pkg", key.agent_package.as_str().to_string()))
            .bind(("inst", key.agent_instance_id.as_str().to_string()))
            .bind(("hash", content_hash.as_str().to_string()))
            .bind(("runner_id", self.identity.runner_id.to_string()))
            .bind(("endpoint", self.identity.endpoint.clone()))
            .await
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "cluster: record placement transport: {e}"
                )))
            })?;
        resp.take::<Option<serde_json::Value>>(0).map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "cluster: record placement query: {e}"
            )))
        })?;
        tracing::info!(
            agent = %key.agent_package.as_str(),
            runner = %self.identity.runner_id,
            "recorded agent placement in cluster"
        );
        Ok(())
    }

    /// Remove the placement record for an agent on this runner.
    pub(crate) async fn remove_placement(&self, key: &AgentRouteKey) -> Result<(), BamlRtError> {
        let mut resp = self
            .db
            .query(
                "DELETE cluster_agent_placements WHERE agent_package = $pkg AND agent_instance_id = $inst AND runner_id = $runner_id",
            )
            .bind(("pkg", key.agent_package.as_str().to_string()))
            .bind(("inst", key.agent_instance_id.as_str().to_string()))
            .bind(("runner_id", self.identity.runner_id.to_string()))
            .await
            .map_err(|e| BamlRtError::Io(std::io::Error::other(format!("cluster: remove placement transport: {e}"))))?;
        resp.take::<Option<serde_json::Value>>(0).map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "cluster: remove placement query: {e}"
            )))
        })?;
        Ok(())
    }

    /// Build a `PlacementResolver` that shares this manager's DB connection.
    pub(crate) fn resolver(&self) -> PlacementResolver {
        PlacementResolver::new(self.db.clone(), self.identity.runner_id.clone())
    }

    /// Spawn a background heartbeat task (5s interval).
    /// Send on the returned sender (or drop it) to stop the heartbeat.
    pub(crate) fn spawn_heartbeat(
        &self,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let db = self.db.clone();
        let runner_id = self.identity.runner_id.to_string();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => {
                        tracing::info!("cluster heartbeat stopping (shutdown requested)");
                        break;
                    }
                    _ = interval.tick() => {}
                }
                let result = db
                    .query(
                        "UPDATE type::record('cluster_runners', $runner_id) SET \
                         last_heartbeat_ms = time::millis(time::now())",
                    )
                    .bind(("runner_id", runner_id.clone()))
                    .await
                    .and_then(|mut r| r.take::<Option<serde_json::Value>>(0));
                if let Err(e) = result {
                    tracing::warn!(error = %e, "cluster heartbeat failed");
                }
            }
        });
        (stop_tx, handle)
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{AgentInstanceId, AgentPackageName};

    use super::*;
    use crate::routing::ClusterEndpointResolver;

    async fn test_db() -> Arc<Surreal<Any>> {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("connect");
        db.use_ns("test").use_db("cluster").await.expect("use ns");
        Arc::new(db)
    }

    fn test_route_key() -> AgentRouteKey {
        AgentRouteKey::new(
            AgentPackageName::parse("test-agent").unwrap(),
            AgentInstanceId::default(),
        )
    }

    fn test_hash() -> DeploymentContentHash {
        "a".repeat(64).parse::<DeploymentContentHash>().unwrap()
    }

    #[tokio::test]
    async fn two_runners_register_independently() {
        let db = test_db().await;
        let id1 = RunnerIdentity::new("http://runner-1:18080".into());
        let id2 = RunnerIdentity::new("http://runner-2:18080".into());

        let _mgr1 = ClusterManager::new(db.clone(), id1).await.unwrap();
        let _mgr2 = ClusterManager::new(db.clone(), id2).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query("SELECT * FROM cluster_runners")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 2, "both runners should be registered");
    }

    #[tokio::test]
    async fn placement_resolver_returns_remote_endpoint() {
        let db = test_db().await;
        let identity = RunnerIdentity::new("http://runner-1:18080".into());
        let mgr = ClusterManager::new(db.clone(), identity).await.unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner);
        let endpoint = resolver.resolve(&key).await.unwrap();
        assert_eq!(endpoint, Some("http://runner-1:18080".to_string()));
    }

    #[tokio::test]
    async fn placement_resolver_returns_none_for_local() {
        let db = test_db().await;
        let identity = RunnerIdentity::new("http://runner-1:18080".into());
        let mgr = ClusterManager::new(db.clone(), identity).await.unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        let resolver = mgr.resolver();
        let endpoint = resolver.resolve(&key).await.unwrap();
        assert_eq!(endpoint, None);
    }

    #[tokio::test]
    async fn remove_placement_clears_record() {
        let db = test_db().await;
        let identity = RunnerIdentity::new("http://runner-1:18080".into());
        let mgr = ClusterManager::new(db.clone(), identity).await.unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();
        mgr.remove_placement(&key).await.unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner);
        assert_eq!(resolver.resolve(&key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn placement_overwrite_last_writer_wins() {
        let db = test_db().await;
        let id1 = RunnerIdentity::new("http://runner-1:18080".into());
        let id2 = RunnerIdentity::new("http://runner-2:18080".into());
        let mgr1 = ClusterManager::new(db.clone(), id1).await.unwrap();
        let mgr2 = ClusterManager::new(db.clone(), id2).await.unwrap();

        let key = test_route_key();
        let hash = test_hash();

        // Runner 1 places first, runner 2 overwrites.
        mgr1.record_placement(&key, &hash).await.unwrap();
        mgr2.record_placement(&key, &hash).await.unwrap();

        // Observer sees runner 2's endpoint (last writer wins via UPSERT).
        let observer = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), observer);
        assert_eq!(
            resolver.resolve(&key).await.unwrap(),
            Some("http://runner-2:18080".to_string()),
        );
    }

    #[tokio::test]
    async fn resolve_returns_endpoint_without_recent_heartbeat() {
        let db = test_db().await;
        let identity = RunnerIdentity::new("http://runner-1:18080".into());
        let mgr = ClusterManager::new(db.clone(), identity).await.unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        // No heartbeat sent after initial registration — resolver still
        // returns the endpoint because there is no heartbeat TTL check.
        let other = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other);
        assert_eq!(
            resolver.resolve(&key).await.unwrap(),
            Some("http://runner-1:18080".to_string()),
        );
    }
}
