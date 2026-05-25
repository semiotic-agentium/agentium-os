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
use tracing::info;

const SERVICE: &str = "payments-api";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum FailureMode {
    #[default]
    Healthy,
    LatencySpike {
        latency_ms_p95: u64,
    },
    Fail {
        error_rate: f64,
    },
}

struct AppState {
    mode: RwLock<FailureMode>,
    metrics: Metrics,
}

struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    errors_total: IntCounterVec,
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
            failure_mode_gauge,
            injection_active,
        }
    }

    fn set_mode(&self, mode: &FailureMode) {
        for label in ["healthy", "latency_spike", "fail"] {
            self.failure_mode_gauge.with_label_values(&[label]).set(0);
            self.injection_active.with_label_values(&[label]).set(0);
        }
        let label = mode_label(mode);
        self.failure_mode_gauge.with_label_values(&[label]).set(1);
        if !matches!(mode, FailureMode::Healthy) {
            self.injection_active.with_label_values(&[label]).set(1);
        }
    }
}

fn mode_label(mode: &FailureMode) -> &'static str {
    match mode {
        FailureMode::Healthy => "healthy",
        FailureMode::LatencySpike { .. } => "latency_spike",
        FailureMode::Fail { .. } => "fail",
    }
}

#[derive(Deserialize)]
struct AuthorizeRequest {
    #[serde(default)]
    amount: f64,
    #[serde(default)]
    order_id: Option<String>,
}

#[derive(Serialize)]
struct AuthorizeResponse {
    authorization_id: String,
    amount: f64,
    status: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let metrics = Metrics::new();
    metrics.set_mode(&FailureMode::Healthy);

    let state = Arc::new(AppState {
        mode: RwLock::new(FailureMode::Healthy),
        metrics,
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/payments/authorize", post(authorize))
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

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    "ready"
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

async fn authorize(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AuthorizeRequest>,
) -> Response {
    let started = Instant::now();
    let route = "/payments/authorize";
    let mode = state.mode.read().clone();

    let (status_code, fail): (u16, bool) = match &mode {
        FailureMode::Healthy => (200, false),
        FailureMode::LatencySpike { latency_ms_p95 } => {
            let jitter = rand::thread_rng().gen_range(0.7..1.2);
            let sleep_ms = ((*latency_ms_p95 as f64) * jitter) as u64;
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
            (200, false)
        }
        FailureMode::Fail { error_rate } => {
            if rand::thread_rng().gen_bool((*error_rate).clamp(0.0, 1.0)) {
                (502, true)
            } else {
                (200, false)
            }
        }
    };

    let elapsed = started.elapsed().as_secs_f64();
    state
        .metrics
        .request_duration
        .with_label_values(&[route])
        .observe(elapsed);
    state
        .metrics
        .requests_total
        .with_label_values(&[route, &status_code.to_string()])
        .inc();
    if fail {
        state
            .metrics
            .errors_total
            .with_label_values(&["dependency_failure"])
            .inc();
    }

    let trace_id = uuid_like();
    info!(
        service = SERVICE,
        route,
        status = status_code,
        latency_ms = (elapsed * 1000.0) as u64,
        trace_id = %trace_id,
        failure_mode = mode_label(&mode),
        amount = req.amount,
        order_id = req.order_id.as_deref().unwrap_or(""),
        message = if fail { "payment authorization failed" } else { "payment authorization ok" },
    );

    if fail {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": "authorization_failed"})),
        )
            .into_response();
    }

    Json(AuthorizeResponse {
        authorization_id: trace_id,
        amount: req.amount,
        status: "authorized",
    })
    .into_response()
}

async fn set_failure_mode(
    State(state): State<Arc<AppState>>,
    Json(mode): Json<FailureMode>,
) -> Response {
    info!(service = SERVICE, mode = mode_label(&mode), "failure_mode set");
    state.metrics.set_mode(&mode);
    *state.mode.write() = mode;
    StatusCode::NO_CONTENT.into_response()
}

async fn reset_active(State(state): State<Arc<AppState>>) -> Response {
    info!(service = SERVICE, "failure_mode reset");
    state.metrics.set_mode(&FailureMode::Healthy);
    *state.mode.write() = FailureMode::Healthy;
    StatusCode::NO_CONTENT.into_response()
}

fn uuid_like() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

