//! Integration tests for the `/diagnose` endpoint.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::{
    ApiServerConfig, ClusterHeartbeatHealth, ClusterMode, HeartbeatErrorKind,
    READYZ_LAG_THRESHOLD_MS, RuntimeProgressMeter, api_router, api_router_with_services_and_deploy,
};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentDispatchAck, AgentDispatchRequest,
    AgentLister, AgentRouteKey, BamlRtError, BusStream, Result,
};
use serde::Deserialize;
use tower::ServiceExt;

/// Test mirror of `DiagnoseResponse`. `deny_unknown_fields` makes the test
/// fail if a new field is added to the production response without a
/// corresponding test update; missing fields fail too because the inner
/// fields are concrete (no defaulting).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnose {
    runtime_progress_lag_ms: u64,
    event_producers_loaded: bool,
    cluster_heartbeat: Option<ClusterHeartbeat>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterHeartbeat {
    status: String,
    lag_ms: Option<u64>,
    #[serde(default)]
    last_error_kind: Option<String>,
}

struct StubRegistry;

impl AgentLister for StubRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        Vec::new()
    }
}

#[async_trait]
impl AgentRegistry for StubRegistry {
    async fn handle_a2a_stream(
        &self,
        _key: &AgentRouteKey,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        Err(BamlRtError::AgentNotFound("stub".to_string()))
    }

    async fn handle_dispatch(
        &self,
        _key: &AgentRouteKey,
        _request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        Err(BamlRtError::AgentNotFound("stub".to_string()))
    }
}

async fn diagnose_body(app: axum::Router) -> Diagnose {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/diagnose")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        let raw = std::str::from_utf8(&bytes).unwrap_or("<non-utf8>");
        panic!("/diagnose body shape changed; update Diagnose schema: {err}\nbody: {raw}")
    })
}

#[tokio::test]
async fn diagnose_returns_expected_shape() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    let app = api_router(registry, None, None).await;

    let body = diagnose_body(app).await;

    assert!(
        body.cluster_heartbeat.is_none(),
        "standalone-mode /diagnose must omit cluster_heartbeat entirely: {body:?}"
    );
    // `runtime_progress_lag_ms` and `event_producers_loaded` deserialized
    // successfully (concrete u64 / bool); `deny_unknown_fields` on
    // `Diagnose` ensures no extra fields are present.
    let _ = body.runtime_progress_lag_ms;
    let _ = body.event_producers_loaded;
}

/// Build the baseline `ApiServerConfig` shared by every router-level test
/// in this file (in-memory config store, empty tool catalog, no-op secret
/// resolver). Callers override the meter and any cluster-mode fields via
/// struct-update syntax.
async fn base_router_config(meter: Arc<RuntimeProgressMeter>) -> ApiServerConfig {
    let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> =
        Arc::new(baml_rt_tools::InventoryCatalog::new());
    let config_service: Arc<dyn baml_rt_config::ConfigService> = Arc::new(
        baml_rt_config::SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store"),
    );
    let secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver> =
        Arc::new(baml_rt_llm_config::EmptySecretResolver);
    ApiServerConfig::empty(tool_catalog, config_service, secret_resolver, meter)
}

async fn cluster_router_with_heartbeat(health: Arc<ClusterHeartbeatHealth>) -> axum::Router {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    let config = ApiServerConfig {
        cluster_mode: ClusterMode::Cluster,
        cluster_heartbeat: Some(health),
        ..base_router_config(RuntimeProgressMeter::spawn_in_current_runtime()).await
    };
    api_router_with_services_and_deploy(registry, config)
}

#[tokio::test]
async fn diagnose_surfaces_cluster_heartbeat_fields_when_health_is_wired() {
    let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
    let app = cluster_router_with_heartbeat(health.clone()).await;

    // Fresh meter — no attempt yet — reports `starting`.
    let body = diagnose_body(app.clone()).await;
    let hb = body
        .cluster_heartbeat
        .as_ref()
        .expect("cluster_heartbeat object must be present in cluster mode");
    assert_eq!(
        hb.status, "starting",
        "fresh health (no heartbeat attempt yet) must report starting: {body:?}"
    );
    assert_eq!(
        hb.lag_ms, None,
        "lag_ms must be null before any successful heartbeat: {body:?}"
    );
    assert_eq!(
        hb.last_error_kind, None,
        "last_error_kind must be omitted when no failure has been observed: {body:?}"
    );

    // Successful heartbeat → `ok`, lag is a u64, no error kind yet.
    health.record_ok();
    let body = diagnose_body(app.clone()).await;
    let hb = body
        .cluster_heartbeat
        .as_ref()
        .expect("cluster_heartbeat present");
    assert_eq!(
        hb.status, "ok",
        "after record_ok, status flips to ok: {body:?}"
    );
    assert!(
        hb.lag_ms.is_some(),
        "after record_ok, lag_ms is a u64: {body:?}"
    );
    assert_eq!(
        hb.last_error_kind, None,
        "last_error_kind stays absent on the all-success path: {body:?}"
    );

    // Failed heartbeat → status flips to `degraded`, kind is surfaced.
    health.record_error(HeartbeatErrorKind::Connection);
    let body = diagnose_body(app).await;
    let hb = body
        .cluster_heartbeat
        .as_ref()
        .expect("cluster_heartbeat present");
    assert_eq!(
        hb.status, "degraded",
        "after record_error, status flips to degraded: {body:?}"
    );
    assert_eq!(
        hb.last_error_kind.as_deref(),
        Some("connection"),
        "last_error_kind must surface the typed kind code: {body:?}"
    );
}

/// Pull the bare HTTP status code from `/readyz` without parsing the body.
async fn readyz_status(app: axum::Router) -> StatusCode {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    response.status()
}

/// Build a router whose `RuntimeProgressMeter` is the caller-supplied
/// instance, so a test can drive lag deterministically via `tick()` and
/// `register_probe()` rather than relying on wall-clock timing.
async fn router_with_meter(meter: Arc<RuntimeProgressMeter>) -> axum::Router {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    api_router_with_services_and_deploy(registry, base_router_config(meter).await)
}

/// `/readyz=200` requires both the boot latch and a healthy runtime-progress
/// meter. With a freshly-ticked meter (lag ≈ 0) and the latch defaulted to
/// `true` by `ApiServerConfig::empty`, the gate opens.
#[tokio::test]
async fn readyz_returns_200_when_meter_lag_is_below_threshold() {
    let meter = RuntimeProgressMeter::new_without_ticker();
    meter.tick();
    let app = router_with_meter(meter).await;

    assert_eq!(readyz_status(app).await, StatusCode::OK);
}

/// Regression target for issue #339: a stalled runtime must flip `/readyz`
/// to `503` even though the boot latch is set, so kubelet (eventually)
/// stops routing new work to a wedged pod.
#[tokio::test]
async fn readyz_returns_503_when_runtime_progress_lag_exceeds_threshold() {
    use std::sync::Weak;

    use baml_rt_core::ProgressProbe;

    #[derive(Debug)]
    struct FixedLagProbe(u64);
    impl ProgressProbe for FixedLagProbe {
        fn lag_millis(&self) -> u64 {
            self.0
        }
    }

    let meter = RuntimeProgressMeter::new_without_ticker();
    meter.tick();
    // Pin the meter's reported lag above the gate threshold via a probe so
    // the test does not rely on a sleep.
    let probe: Arc<dyn ProgressProbe> =
        Arc::new(FixedLagProbe(READYZ_LAG_THRESHOLD_MS.saturating_add(500)));
    let probe_weak: Weak<dyn ProgressProbe> = Arc::downgrade(&probe);
    meter.register_probe(probe_weak);

    let app = router_with_meter(meter).await;

    assert_eq!(
        readyz_status(app).await,
        StatusCode::SERVICE_UNAVAILABLE,
        "/readyz must reflect runtime-progress lag, not just the boot latch"
    );
    // Keep the probe alive past the assertion so the weak reference inside
    // the meter resolves; dropping it before the read would clear the lag
    // and undermine the test.
    drop(probe);
}

#[tokio::test]
async fn diagnose_lag_stays_small_under_no_load() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    let app = api_router(registry, None, None).await;

    tokio::time::sleep(Duration::from_millis(150)).await;

    let body = diagnose_body(app).await;
    let lag = body.runtime_progress_lag_ms;

    // Looser bound than the in-process unit test: an axum handler round-trip
    // adds scheduling jitter, and CI workers can be noisy.
    assert!(
        lag < 400,
        "runtime_progress_lag_ms should stay under one interval period plus handler jitter under no load (got {lag})"
    );
}
