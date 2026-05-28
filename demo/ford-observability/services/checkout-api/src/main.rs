use std::{net::SocketAddr, sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use parking_lot::RwLock;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Registry, TextEncoder,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const SERVICE: &str = "checkout-api";
const DEPENDENCY: &str = "payments-api";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum FailureMode {
    #[default]
    Healthy,
    LatencySpike {
        latency_ms_p95: u64,
        #[serde(default)]
        error_rate: Option<f64>,
        #[serde(default)]
        incident_id: Option<String>,
    },
    DependencyTimeout {
        #[serde(default)]
        incident_id: Option<String>,
    },
    BriefOffline {
        #[serde(default)]
        incident_id: Option<String>,
    },
}

impl FailureMode {
    fn label(&self) -> &'static str {
        match self {
            FailureMode::Healthy => "healthy",
            FailureMode::LatencySpike { .. } => "latency_spike",
            FailureMode::DependencyTimeout { .. } => "dependency_timeout",
            FailureMode::BriefOffline { .. } => "brief_offline",
        }
    }

    fn incident_id(&self) -> Option<&str> {
        match self {
            FailureMode::LatencySpike { incident_id, .. }
            | FailureMode::DependencyTimeout { incident_id, .. }
            | FailureMode::BriefOffline { incident_id, .. } => incident_id.as_deref(),
            FailureMode::Healthy => None,
        }
    }
}

struct AppState {
    mode: RwLock<FailureMode>,
    metrics: Metrics,
    http: reqwest::Client,
    payments_url: String,
}

struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    errors_total: IntCounterVec,
    dependency_latency: HistogramVec,
    failure_mode_gauge: IntGaugeVec,
    injection_active: IntGaugeVec,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();
        let requests_total = IntCounterVec::new(
            prometheus::Opts::new("demo_service_requests_total", "Total HTTP requests")
                .const_label("service", SERVICE),
            &["route", "status"],
        )
        .unwrap();
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "demo_service_request_duration_seconds",
                "Request duration seconds",
            )
            .const_label("service", SERVICE)
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0,
            ]),
            &["route"],
        )
        .unwrap();
        let errors_total = IntCounterVec::new(
            prometheus::Opts::new("demo_service_errors_total", "Total errors")
                .const_label("service", SERVICE),
            &["error_type"],
        )
        .unwrap();
        let dependency_latency = HistogramVec::new(
            HistogramOpts::new(
                "demo_service_dependency_latency_seconds",
                "Dependency call latency seconds",
            )
            .const_label("service", SERVICE)
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0,
            ]),
            &["dependency"],
        )
        .unwrap();
        let failure_mode_gauge = IntGaugeVec::new(
            prometheus::Opts::new("demo_service_failure_mode", "Active failure mode flag")
                .const_label("service", SERVICE),
            &["mode"],
        )
        .unwrap();
        let injection_active = IntGaugeVec::new(
            prometheus::Opts::new("demo_service_injection_active", "Injection active flag")
                .const_label("service", SERVICE),
            &["mode"],
        )
        .unwrap();

        registry.register(Box::new(requests_total.clone())).unwrap();
        registry
            .register(Box::new(request_duration.clone()))
            .unwrap();
        registry.register(Box::new(errors_total.clone())).unwrap();
        registry
            .register(Box::new(dependency_latency.clone()))
            .unwrap();
        registry
            .register(Box::new(failure_mode_gauge.clone()))
            .unwrap();
        registry
            .register(Box::new(injection_active.clone()))
            .unwrap();

        Self {
            registry,
            requests_total,
            request_duration,
            errors_total,
            dependency_latency,
            failure_mode_gauge,
            injection_active,
        }
    }

    fn set_mode(&self, mode: &FailureMode) {
        for label in ["healthy", "latency_spike", "dependency_timeout", "brief_offline"] {
            self.failure_mode_gauge.with_label_values(&[label]).set(0);
            self.injection_active.with_label_values(&[label]).set(0);
        }
        let label = mode.label();
        self.failure_mode_gauge.with_label_values(&[label]).set(1);
        if !matches!(mode, FailureMode::Healthy) {
            self.injection_active.with_label_values(&[label]).set(1);
        }
    }
}

#[derive(Serialize)]
struct CheckoutResponse {
    order_id: String,
    authorization_id: Option<String>,
    status: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let metrics = Metrics::new();
    metrics.set_mode(&FailureMode::Healthy);

    let payments_url = std::env::var("PAYMENTS_URL")
        .unwrap_or_else(|_| "http://payments-api:8080".to_string());

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let state = Arc::new(AppState {
        mode: RwLock::new(FailureMode::Healthy),
        metrics,
        http,
        payments_url,
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/checkout", get(checkout))
        .route("/api/orders", get(orders))
        .route("/metrics", get(metrics_handler))
        .route("/admin/failure-mode", post(set_failure_mode))
        .route("/admin/reset-active", post(reset_active))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(service = SERVICE, %addr, "listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_current_span(false))
        .init();
}

async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    if matches!(*state.mode.read(), FailureMode::BriefOffline { .. }) {
        return (StatusCode::SERVICE_UNAVAILABLE, "offline").into_response();
    }
    (StatusCode::OK, "ok").into_response()
}

async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    if matches!(*state.mode.read(), FailureMode::BriefOffline { .. }) {
        return (StatusCode::SERVICE_UNAVAILABLE, "offline").into_response();
    }
    (StatusCode::OK, "ready").into_response()
}

async fn metrics_handler(State(state): State<Arc<AppState>>) -> Response {
    let mfs = state.metrics.registry.gather();
    let mut buf = Vec::new();
    TextEncoder::new().encode(&mfs, &mut buf).ok();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        buf,
    )
        .into_response()
}

async fn orders(State(state): State<Arc<AppState>>) -> Response {
    let started = Instant::now();
    let route = "/api/orders";
    let body = serde_json::json!({"orders": []});
    record_request(&state, route, 200, started, None, "orders listed");
    (StatusCode::OK, Json(body)).into_response()
}

async fn checkout(State(state): State<Arc<AppState>>) -> Response {
    let started = Instant::now();
    let route = "/api/checkout";
    let mode = state.mode.read().clone();
    let trace_id = hex_id(16);
    let order_id = hex_id(8);

    if let FailureMode::BriefOffline { .. } = &mode {
        record_request(&state, route, 503, started, Some(&trace_id), "service offline");
        return (StatusCode::SERVICE_UNAVAILABLE, "offline").into_response();
    }

    let span_started = Instant::now();
    let payments_url = format!("{}/payments/authorize", state.payments_url);

    let timeout = match &mode {
        FailureMode::DependencyTimeout { .. } => std::time::Duration::from_millis(250),
        _ => std::time::Duration::from_secs(5),
    };

    let payment_result = tokio::time::timeout(
        timeout,
        state
            .http
            .post(&payments_url)
            .json(&serde_json::json!({
                "amount": 42.0,
                "order_id": order_id,
            }))
            .send(),
    )
    .await;

    let span_elapsed_ms = span_started.elapsed().as_millis() as u64;
    state
        .metrics
        .dependency_latency
        .with_label_values(&[DEPENDENCY])
        .observe(span_started.elapsed().as_secs_f64());

    let (status_code, message, authorization_id, span_status) = match payment_result {
        Err(_) => {
            state
                .metrics
                .errors_total
                .with_label_values(&["dependency_timeout"])
                .inc();
            warn!(
                service = SERVICE,
                route,
                trace_id = %trace_id,
                dependency = DEPENDENCY,
                failure_mode = mode.label(),
                message = "dependency timeout calling payments-api",
            );
            (504u16, "dependency timeout", None, "timeout")
        }
        Ok(Err(err)) => {
            state
                .metrics
                .errors_total
                .with_label_values(&["dependency_error"])
                .inc();
            warn!(
                service = SERVICE,
                route,
                trace_id = %trace_id,
                dependency = DEPENDENCY,
                error = %err,
                message = "dependency error calling payments-api",
            );
            (502u16, "dependency error", None, "error")
        }
        Ok(Ok(resp)) => {
            let status = resp.status();
            if status.is_success() {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                let auth_id = body
                    .get("authorization_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                (200u16, "checkout ok", auth_id, "ok")
            } else {
                state
                    .metrics
                    .errors_total
                    .with_label_values(&["dependency_failure"])
                    .inc();
                (502u16, "dependency failure", None, "error")
            }
        }
    };

    if !matches!(mode, FailureMode::Healthy) {
        emit_span_record(SpanRecord {
            trace_id: &trace_id,
            span_id: &hex_id(8),
            parent_span_id: "00000000",
            service: SERVICE,
            operation: "POST /payments/authorize",
            dependency: DEPENDENCY,
            duration_ms: span_elapsed_ms,
            status: span_status,
            incident_id: mode.incident_id(),
        });
    }

    record_request(&state, route, status_code, started, Some(&trace_id), message);

    if status_code != 200 {
        return (
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(serde_json::json!({"error": message, "order_id": order_id})),
        )
            .into_response();
    }

    Json(CheckoutResponse {
        order_id,
        authorization_id,
        status: "ok",
    })
    .into_response()
}

fn record_request(
    state: &AppState,
    route: &'static str,
    status: u16,
    started: Instant,
    trace_id: Option<&str>,
    message: &str,
) {
    let elapsed = started.elapsed().as_secs_f64();
    state
        .metrics
        .request_duration
        .with_label_values(&[route])
        .observe(elapsed);
    state
        .metrics
        .requests_total
        .with_label_values(&[route, &status.to_string()])
        .inc();
    let mode = state.mode.read().clone();
    info!(
        service = SERVICE,
        route,
        status,
        latency_ms = (elapsed * 1000.0) as u64,
        trace_id = trace_id.unwrap_or(""),
        failure_mode = mode.label(),
        message,
    );
}

struct SpanRecord<'a> {
    trace_id: &'a str,
    span_id: &'a str,
    parent_span_id: &'a str,
    service: &'a str,
    operation: &'a str,
    dependency: &'a str,
    duration_ms: u64,
    status: &'a str,
    incident_id: Option<&'a str>,
}

fn emit_span_record(span: SpanRecord<'_>) {
    let now = chrono::Utc::now().to_rfc3339();
    let line = serde_json::json!({
        "log_kind": "span",
        "timestamp": now,
        "trace_id": span.trace_id,
        "span_id": span.span_id,
        "parent_span_id": span.parent_span_id,
        "service": span.service,
        "operation": span.operation,
        "dependency": span.dependency,
        "duration_ms": span.duration_ms,
        "status": span.status,
        "incident_id": span.incident_id.unwrap_or(""),
    });
    println!("{line}");
}

async fn set_failure_mode(
    State(state): State<Arc<AppState>>,
    Json(mode): Json<FailureMode>,
) -> Response {
    let label = mode.label();
    info!(service = SERVICE, mode = label, incident_id = mode.incident_id().unwrap_or(""), "failure_mode set");
    state.metrics.set_mode(&mode);

    if let FailureMode::LatencySpike { latency_ms_p95, error_rate, .. } = &mode {
        let _ = propagate_payments_mode(
            &state,
            serde_json::json!({
                "mode": "latency_spike",
                "latency_ms_p95": latency_ms_p95,
                "error_rate": error_rate,
            }),
        )
        .await;
    } else if let FailureMode::Healthy = &mode {
        let _ = propagate_payments_mode(&state, serde_json::json!({"mode": "healthy"})).await;
    }

    *state.mode.write() = mode;
    StatusCode::NO_CONTENT.into_response()
}

async fn reset_active(State(state): State<Arc<AppState>>) -> Response {
    info!(service = SERVICE, "failure_mode reset");
    state.metrics.set_mode(&FailureMode::Healthy);
    let _ = propagate_payments_mode(&state, serde_json::json!({"mode": "healthy"})).await;
    *state.mode.write() = FailureMode::Healthy;
    StatusCode::NO_CONTENT.into_response()
}

async fn propagate_payments_mode(state: &AppState, body: serde_json::Value) -> anyhow::Result<()> {
    let url = format!("{}/admin/failure-mode", state.payments_url);
    let resp = state.http.post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        warn!(target = "checkout-api", url, status = %resp.status(), "propagate payments mode failed");
    }
    Ok(())
}

fn hex_id(bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..bytes).map(|_| format!("{:02x}", rng.r#gen::<u8>())).collect()
}
