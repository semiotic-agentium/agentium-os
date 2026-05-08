//! Integration tests for the `/diagnose` endpoint.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::api_router;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentDispatchAck, AgentDispatchRequest,
    AgentLister, AgentRouteKey, BamlRtError, BusStream, Result,
};
use serde_json::Value;
use tower::ServiceExt;

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

async fn diagnose_body(app: axum::Router) -> Value {
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
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn diagnose_returns_expected_shape() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    let app = api_router(registry, None, None).await;

    let body = diagnose_body(app).await;

    assert!(
        body.get("runtime_progress_lag_ms")
            .and_then(Value::as_u64)
            .is_some(),
        "body missing runtime_progress_lag_ms: {body}"
    );
    assert!(
        body.get("event_producers_loaded")
            .and_then(Value::as_bool)
            .is_some(),
        "body missing event_producers_loaded: {body}"
    );
    assert!(
        body.as_object().is_some_and(|o| o.len() == 2),
        "diagnose body shape changed; update tests and downstream consumers: {body}"
    );
}

#[tokio::test]
async fn diagnose_lag_stays_small_under_no_load() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(StubRegistry);
    let app = api_router(registry, None, None).await;

    tokio::time::sleep(Duration::from_millis(150)).await;

    let body = diagnose_body(app).await;
    let lag = body
        .get("runtime_progress_lag_ms")
        .and_then(Value::as_u64)
        .expect("lag field");

    // Looser bound than the in-process unit test: an axum handler round-trip
    // adds scheduling jitter, and CI workers can be noisy.
    assert!(
        lag < 400,
        "runtime_progress_lag_ms should stay under one interval period plus handler jitter under no load (got {lag})"
    );
}
