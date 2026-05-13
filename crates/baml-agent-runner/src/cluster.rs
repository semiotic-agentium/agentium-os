//! Cluster registration and agent placement for multi-runner deployments.
//!
//! When connected to a shared SurrealDB, each runner registers itself and
//! records which agents it hosts. The `PlacementResolver` implements
//! [`ClusterEndpointResolver`] so the router can forward A2A requests to
//! the runner currently hosting a given agent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_api::ClusterHeartbeatHealth;
use baml_rt_core::{AgentRouteKey, BamlRtError, DeploymentContentHash, HeartbeatErrorKind};
use baml_rt_observability::UNKNOWN_SERVICE_INSTANCE_ID;
use surrealdb::{Surreal, engine::any::Any};

use crate::routing::{ClusterEndpointResolver, Placement};

/// Heartbeat tick cadence. The health-staleness threshold is owned by
/// [`ClusterHeartbeatHealth::STALE_LAG_MULTIPLIER`].
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Map a SurrealDB error onto the operator-visible [`HeartbeatErrorKind`].
///
/// SurrealDB v3 exposes typed `ErrorDetails` covering connection, query,
/// permission, and other classes; we project them onto the four heartbeat
/// kinds so `/diagnose` shows a stable label even as upstream variants grow.
fn classify_surreal_error(err: &surrealdb::Error) -> HeartbeatErrorKind {
    use surrealdb::types::ErrorDetails;
    match err.details() {
        ErrorDetails::Connection(_) => HeartbeatErrorKind::Connection,
        ErrorDetails::Query(_) => HeartbeatErrorKind::Query,
        ErrorDetails::NotAllowed(_) => HeartbeatErrorKind::NotAllowed,
        _ => HeartbeatErrorKind::Other,
    }
}

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
///
/// `runner_id` is an internal cluster UUID used for same-runner detection
/// during placement lookup. `service_instance_id` is the canonical OTEL
/// `service.instance.id` value emitted on the runner's resource attributes
/// and on spans/metrics; it is persisted in `cluster_runners.service_instance_id`
/// so peer runners can surface it as `target_service_instance_id` in
/// forwarding telemetry.
///
/// The two are deliberately separate: `runner_id` is an opaque UUID internal
/// to the registry; `service_instance_id` is a pilot-facing identity that
/// resolves to pod name in K8s and can be overridden via
/// `OTEL_RESOURCE_ATTRIBUTES` in other deployments.
#[derive(Debug, Clone)]
pub(crate) struct RunnerIdentity {
    pub(crate) runner_id: RunnerId,
    pub(crate) endpoint: String,
    pub(crate) service_instance_id: String,
}

impl RunnerIdentity {
    pub(crate) fn new(endpoint: String, service_instance_id: String) -> Self {
        Self {
            runner_id: RunnerId::new_random(),
            endpoint,
            service_instance_id,
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
    placement_ttl_ms: u64,
}

impl PlacementResolver {
    pub(crate) fn new(
        db: Arc<Surreal<Any>>,
        local_runner_id: RunnerId,
        placement_ttl_ms: u64,
    ) -> Self {
        Self {
            db,
            local_runner_id,
            placement_ttl_ms,
        }
    }
}

#[async_trait]
impl ClusterEndpointResolver for PlacementResolver {
    async fn resolve(&self, key: &AgentRouteKey) -> baml_rt_core::Result<Option<Placement>> {
        let pkg = key.agent_package.as_str();
        let inst = key.agent_instance_id.as_str();
        let ttl = self.placement_ttl_ms as i64;
        // Two-stage lookup: fetch the placement, then the serving runner's
        // `service_instance_id`. A correlated sub-select on `cluster_runners`
        // inside the projection of the first query does not resolve
        // `runner_id` from the outer row in SurrealDB v3. `ORDER BY id ASC`
        // makes the LIMIT 1 pick stable when multiple rows match.
        let result: Result<Vec<serde_json::Value>, _> = self
            .db
            .query(
                "SELECT * FROM cluster_agent_placements \
                 WHERE agent_package = $pkg \
                 AND agent_instance_id = $inst \
                 AND status = 'active' \
                 AND runner_id IN (\
                   SELECT VALUE runner_id FROM cluster_runners \
                   WHERE last_heartbeat_ms > (time::millis(time::now()) - $ttl)\
                 ) \
                 ORDER BY id ASC \
                 LIMIT 1",
            )
            .bind(("pkg", pkg.to_string()))
            .bind(("inst", inst.to_string()))
            .bind(("ttl", ttl))
            .await
            .and_then(|mut r| r.take(0));

        let row = match result {
            Ok(rows) => rows.into_iter().next(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    agent = %pkg,
                    instance = %inst,
                    "cluster placement lookup failed"
                );
                return Err(BamlRtError::Io(std::io::Error::other(format!(
                    "cluster placement lookup: {e}"
                ))));
            }
        };
        let Some(row) = row else {
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

        let runner_row: Option<serde_json::Value> = self
            .db
            .query(
                "SELECT service_instance_id, pod_name \
                 FROM ONLY type::record('cluster_runners', $rid)",
            )
            .bind(("rid", runner_id.to_string()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "cluster runner identity lookup for {runner_id}: {e}"
                )))
            })?;
        // Rollout-safe degradation. Post-PR runners write `service_instance_id`
        // directly. Pre-PR runners only wrote `pod_name`, so mid-rollout peers
        // still resolve — the observability label is imprecise until the old
        // runner restarts, but forwarding never fails purely because a peer
        // hasn't been redeployed yet.
        let field = |key: &str| -> Option<&str> {
            runner_row
                .as_ref()
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
        };
        let service_instance_id = match field("service_instance_id") {
            Some(sid) => sid.to_string(),
            None => {
                let fallback = field("pod_name").unwrap_or(UNKNOWN_SERVICE_INSTANCE_ID);
                tracing::warn!(
                    runner_id = runner_id,
                    fallback = fallback,
                    "cluster_runners row missing service_instance_id; falling back \
                     (likely a peer running a pre-rollout build — re-register on restart)"
                );
                fallback.to_string()
            }
        };

        Ok(Some(Placement {
            endpoint: endpoint.to_string(),
            service_instance_id,
        }))
    }
}

// ---------------------------------------------------------------------------
// Cluster operations (register runner, record/remove placements, heartbeat)
// ---------------------------------------------------------------------------

/// Manages cluster state in shared SurrealDB.
pub(crate) struct ClusterManager {
    db: Arc<Surreal<Any>>,
    identity: RunnerIdentity,
    placement_ttl_ms: u64,
}

impl ClusterManager {
    pub(crate) async fn new(
        db: Arc<Surreal<Any>>,
        identity: RunnerIdentity,
        placement_ttl_ms: u64,
    ) -> Result<Self, BamlRtError> {
        let mgr = Self {
            db,
            identity,
            placement_ttl_ms,
        };
        mgr.init_schema().await?;
        mgr.register_runner().await?;
        Ok(mgr)
    }

    async fn init_schema(&self) -> Result<(), BamlRtError> {
        // `REMOVE INDEX IF EXISTS idx_placement_agent` retires the
        // (agent_package, agent_instance_id) UNIQUE that collapsed
        // multi-runner placements. The wider replacement is safe to apply
        // in place: the narrower UNIQUE prevented any row that would now
        // violate the new key.
        self.db
            .query(
                "DEFINE TABLE IF NOT EXISTS cluster_runners SCHEMALESS;\
                 DEFINE TABLE IF NOT EXISTS cluster_agent_placements SCHEMALESS;\
                 REMOVE INDEX IF EXISTS idx_placement_agent ON cluster_agent_placements;\
                 DEFINE INDEX IF NOT EXISTS idx_placement_agent_runner ON cluster_agent_placements FIELDS agent_package, agent_instance_id, runner_id UNIQUE",
            )
            .await
            .map_err(|e| BamlRtError::Io(std::io::Error::other(format!("cluster: schema init transport: {e}"))))?
            .check()
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "cluster: schema init statement: {e}"
                )))
            })?;
        Ok(())
    }

    async fn register_runner(&self) -> Result<(), BamlRtError> {
        // `pod_name` is a best-effort HOSTNAME snapshot for consumers that
        // want the container hostname specifically. Peer observability reads
        // `service_instance_id`, the canonical OTEL `service.instance.id`,
        // which may diverge from `pod_name` when an operator sets
        // `OTEL_RESOURCE_ATTRIBUTES=service.instance.id=…`.
        let pod_name = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
        let mut resp = self
            .db
            .query(
                "UPSERT type::record('cluster_runners', $runner_id) SET \
                 runner_id = $runner_id, \
                 endpoint = $endpoint, \
                 pod_name = $pod_name, \
                 service_instance_id = $service_instance_id, \
                 last_heartbeat_ms = time::millis(time::now())",
            )
            .bind(("runner_id", self.identity.runner_id.to_string()))
            .bind(("endpoint", self.identity.endpoint.clone()))
            .bind(("pod_name", pod_name))
            .bind((
                "service_instance_id",
                self.identity.service_instance_id.clone(),
            ))
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
        // `/` is rejected by both ID validators and `runner_id` is a UUID,
        // so the triple round-trips unambiguously.
        let placement_key = format!(
            "{pkg}/{inst}/{runner}",
            pkg = key.agent_package.as_str(),
            inst = key.agent_instance_id.as_str(),
            runner = self.identity.runner_id,
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
        PlacementResolver::new(
            self.db.clone(),
            self.identity.runner_id.clone(),
            self.placement_ttl_ms,
        )
    }

    /// Spawn a background heartbeat task (5s interval) that records its
    /// success/failure state on the supplied [`ClusterHeartbeatHealth`] so
    /// `GET /diagnose` can surface degraded heartbeats to operators.
    /// Send on the returned sender (or drop it) to stop the heartbeat.
    pub(crate) fn spawn_heartbeat(
        &self,
        health: Arc<ClusterHeartbeatHealth>,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let db = self.db.clone();
        let runner_id = self.identity.runner_id.to_string();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
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
                match result {
                    Ok(_) => health.record_ok(),
                    Err(e) => {
                        let kind = classify_surreal_error(&e);
                        let err = BamlRtError::ClusterHeartbeat {
                            kind,
                            message: e.to_string(),
                        };
                        tracing::warn!(error = %err, "cluster heartbeat failed");
                        health.record_error(kind);
                    }
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

    /// Generous TTL for tests that don't exercise heartbeat staleness.
    const TEST_TTL_MS: u64 = 300_000;

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

    fn identity(n: u8) -> RunnerIdentity {
        RunnerIdentity::new(
            format!("http://runner-{n}:18080"),
            format!("runner-{n}-sid"),
        )
    }

    #[tokio::test]
    async fn two_runners_register_independently() {
        let db = test_db().await;
        let id1 = identity(1);
        let id2 = identity(2);

        let _mgr1 = ClusterManager::new(db.clone(), id1, TEST_TTL_MS)
            .await
            .unwrap();
        let _mgr2 = ClusterManager::new(db.clone(), id2, TEST_TTL_MS)
            .await
            .unwrap();

        let rows: Vec<serde_json::Value> = db
            .query("SELECT * FROM cluster_runners")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 2, "both runners should be registered");
    }

    #[tokio::test]
    async fn placement_resolver_returns_placement_with_service_instance_id() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity, TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner, TEST_TTL_MS);
        let placement = resolver
            .resolve(&key)
            .await
            .unwrap()
            .expect("remote placement should resolve");
        assert_eq!(placement.endpoint, "http://runner-1:18080");
        assert_eq!(
            placement.service_instance_id, "runner-1-sid",
            "service_instance_id must come from RunnerIdentity, not from HOSTNAME"
        );

        // `pod_name` (HOSTNAME snapshot) and `service_instance_id` (canonical
        // OTEL identity) are independent fields on `cluster_runners`; a future
        // OTEL override path must not silently overwrite `pod_name`.
        let rows: Vec<serde_json::Value> = db
            .query("SELECT pod_name, service_instance_id FROM cluster_runners")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("service_instance_id").and_then(|v| v.as_str()),
            Some("runner-1-sid"),
            "registry must persist the canonical service.instance.id"
        );
        assert!(
            rows[0].get("pod_name").and_then(|v| v.as_str()).is_some(),
            "pod_name field must still be present (HOSTNAME-sourced best-effort)"
        );
    }

    #[tokio::test]
    async fn placement_resolver_returns_none_for_local() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity, TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        let resolver = mgr.resolver();
        let placement = resolver.resolve(&key).await.unwrap();
        assert_eq!(placement, None);
    }

    #[tokio::test]
    async fn remove_placement_clears_record() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity, TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();
        mgr.remove_placement(&key).await.unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner, TEST_TTL_MS);
        assert_eq!(resolver.resolve(&key).await.unwrap(), None);
    }

    /// Distinct runners hosting the same agent produce distinct placement rows.
    #[tokio::test]
    async fn placements_coexist_for_distinct_runners() {
        let db = test_db().await;
        let mgr1 = ClusterManager::new(db.clone(), identity(1), TEST_TTL_MS)
            .await
            .unwrap();
        let mgr2 = ClusterManager::new(db.clone(), identity(2), TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr1.record_placement(&key, &hash).await.unwrap();
        mgr2.record_placement(&key, &hash).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query(
                "SELECT runner_endpoint FROM cluster_agent_placements \
                 WHERE agent_package = $pkg AND agent_instance_id = $inst \
                 ORDER BY runner_endpoint ASC",
            )
            .bind(("pkg", key.agent_package.as_str().to_string()))
            .bind(("inst", key.agent_instance_id.as_str().to_string()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let endpoints: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("runner_endpoint").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            endpoints,
            vec!["http://runner-1:18080", "http://runner-2:18080"]
        );
    }

    /// Resolver picks the same placement on every call when cluster state is
    /// unchanged, so cross-runner routing does not flap between requests.
    #[tokio::test]
    async fn placement_resolver_is_deterministic_across_calls() {
        let db = test_db().await;
        let mgr1 = ClusterManager::new(db.clone(), identity(1), TEST_TTL_MS)
            .await
            .unwrap();
        let mgr2 = ClusterManager::new(db.clone(), identity(2), TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr1.record_placement(&key, &hash).await.unwrap();
        mgr2.record_placement(&key, &hash).await.unwrap();

        let observer = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), observer, TEST_TTL_MS);
        let first = resolver.resolve(&key).await.unwrap().unwrap();
        for _ in 0..5 {
            let next = resolver.resolve(&key).await.unwrap().unwrap();
            assert_eq!(
                next.endpoint, first.endpoint,
                "resolver must not flap between calls when cluster state is unchanged"
            );
        }
    }

    #[tokio::test]
    async fn resolve_excludes_stale_runner() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity, TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        // Backdate the runner's heartbeat so it falls outside a short TTL.
        db.query(
            "UPDATE cluster_runners SET last_heartbeat_ms = 0 \
             WHERE runner_id = $rid",
        )
        .bind(("rid", mgr.identity.runner_id.to_string()))
        .await
        .unwrap();

        // With a 1ms TTL the backdated heartbeat is stale — resolve returns None.
        let other = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other, 1);
        assert_eq!(
            resolver.resolve(&key).await.unwrap(),
            None,
            "stale runner should be excluded from placement resolution",
        );
    }

    /// During a rolling deploy, a new ingress runner may forward to a peer
    /// that registered before this PR added `cluster_runners.service_instance_id`.
    /// Forwarding must not break purely because the peer hasn't restarted yet;
    /// the resolver should degrade to `pod_name` and log a warning.
    #[tokio::test]
    async fn placement_resolver_falls_back_to_pod_name_for_pre_rollout_row() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity.clone(), TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        // Simulate a pre-rollout registration: strip the new field from the
        // row so the shape matches what an older runner would have written.
        db.query("UPDATE cluster_runners SET service_instance_id = NONE WHERE runner_id = $rid")
            .bind(("rid", identity.runner_id.to_string()))
            .await
            .unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner, TEST_TTL_MS);
        let placement = resolver
            .resolve(&key)
            .await
            .expect("resolve must succeed despite missing service_instance_id")
            .expect("remote placement should resolve");
        assert_eq!(placement.endpoint, "http://runner-1:18080");
        // Test-only env: `pod_name` defaults to `HOSTNAME` or `unknown`. Either
        // way it must come from `pod_name`, not surface as an Err.
        assert!(
            !placement.service_instance_id.is_empty(),
            "fallback produced an empty service_instance_id"
        );
        assert_ne!(
            placement.service_instance_id, "runner-1-sid",
            "the canonical field was stripped; resolver must not have found it"
        );
    }

    /// Last-resort degradation: when neither `service_instance_id` nor
    /// `pod_name` is present on the peer's row, the resolver falls back to
    /// the shared `UNKNOWN_SERVICE_INSTANCE_ID` sentinel so telemetry stays
    /// bounded and routing still succeeds.
    #[tokio::test]
    async fn placement_resolver_falls_back_to_unknown_when_all_identity_fields_missing() {
        let db = test_db().await;
        let identity = identity(1);
        let mgr = ClusterManager::new(db.clone(), identity.clone(), TEST_TTL_MS)
            .await
            .unwrap();

        let key = test_route_key();
        let hash = test_hash();
        mgr.record_placement(&key, &hash).await.unwrap();

        db.query(
            "UPDATE cluster_runners SET service_instance_id = NONE, pod_name = NONE \
             WHERE runner_id = $rid",
        )
        .bind(("rid", identity.runner_id.to_string()))
        .await
        .unwrap();

        let other_runner = RunnerId::new_random();
        let resolver = PlacementResolver::new(db.clone(), other_runner, TEST_TTL_MS);
        let placement = resolver
            .resolve(&key)
            .await
            .expect("resolve must succeed despite missing identity fields")
            .expect("remote placement should resolve");
        assert_eq!(placement.endpoint, "http://runner-1:18080");
        assert_eq!(placement.service_instance_id, UNKNOWN_SERVICE_INSTANCE_ID);
    }
}
