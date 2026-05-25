use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const SERVICE: &str = "checkout-api";
const DEPENDENCY: &str = "payments-api";
const OVERLAP_LEAD_SECONDS: i64 = 30;
const OVERLAP_TRAIL_SECONDS: i64 = 90;

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    http: reqwest::Client,
    checkout_url: String,
    grafana: Option<GrafanaConfig>,
}

#[derive(Clone)]
struct GrafanaConfig {
    url: String,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FailureRequest {
    mode: FailureMode,
    #[serde(default = "default_duration_seconds")]
    duration_seconds: u64,
    #[serde(default = "default_latency_ms_p95")]
    latency_ms_p95: u64,
    #[serde(default)]
    error_rate: Option<f64>,
    incident_id: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FailureMode {
    LatencySpike,
}

#[derive(Debug, Serialize)]
struct LedgerRow {
    incident_id: String,
    service: String,
    dependency: String,
    mode: String,
    started_at: String,
    ended_at: Option<String>,
    expected_evidence: serde_json::Value,
    overlap_window: OverlapWindow,
}

#[derive(Debug, Serialize)]
struct OverlapWindow {
    starts_at: String,
    ends_at: Option<String>,
    lead_seconds: i64,
    trail_seconds: i64,
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    status: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let db_path = std::env::var("LEDGER_DB_PATH")
        .unwrap_or_else(|_| "/data/failure-harness/ledger.sqlite".to_string());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    init_db(&conn)?;

    let checkout_url = std::env::var("CHECKOUT_API_URL")
        .unwrap_or_else(|_| "http://checkout-api:8080".to_string());
    let grafana = std::env::var("GRAFANA_URL").ok().map(|url| GrafanaConfig {
        url,
        token: std::env::var("GRAFANA_API_TOKEN").ok(),
    });
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let state = Arc::new(AppState {
        db: Arc::new(Mutex::new(conn)),
        http,
        checkout_url,
        grafana,
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/admin/failure-mode", post(start_failure_mode))
        .route(
            "/admin/failure-mode/:incident_id/stop",
            post(stop_failure_mode),
        )
        .route("/admin/reset-active", post(reset_active))
        .route("/admin/ledger", get(list_ledger))
        .route("/admin/ledger/:incident_id", get(get_ledger))
        .route("/admin/reset-ledger", post(reset_ledger))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(service = "failure-harness", %addr, "listening");

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

fn init_db(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ledger (
            incident_id TEXT PRIMARY KEY,
            service TEXT NOT NULL,
            dependency TEXT NOT NULL,
            mode TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            expected_evidence TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ledger_started_at ON ledger(started_at);
        "#,
    )?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    "ready"
}

async fn start_failure_mode(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FailureRequest>,
) -> Response {
    if req.mode != FailureMode::LatencySpike {
        return (StatusCode::BAD_REQUEST, "only latency_spike supported").into_response();
    }
    if req.incident_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "incident_id required").into_response();
    }

    let started_at = Utc::now();
    let planned_end = started_at + ChronoDuration::seconds(req.duration_seconds as i64);
    let expected_evidence = expected_evidence(req.latency_ms_p95);

    if let Err(err) = insert_ledger(&state, &req, started_at, &expected_evidence) {
        warn!(error = %err, incident_id = req.incident_id, "ledger insert failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "ledger insert failed").into_response();
    }

    if let Err(err) = activate_checkout(&state, &req).await {
        warn!(error = %err, incident_id = req.incident_id, "checkout activation failed");
        let _ = finalize_ledger(&state, &req.incident_id, Utc::now());
        return (StatusCode::BAD_GATEWAY, "checkout activation failed").into_response();
    }

    let _ = write_window_annotation(&state, &req.incident_id, started_at, planned_end).await;
    let _ = write_trace_annotation(&state, &req.incident_id, started_at, req.latency_ms_p95).await;

    let state_for_stop = state.clone();
    let incident_for_stop = req.incident_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(req.duration_seconds)).await;
        if let Err(err) = stop_incident(&state_for_stop, &incident_for_stop).await {
            warn!(error = %err, incident_id = incident_for_stop, "auto stop failed");
        }
    });

    info!(
        incident_id = req.incident_id,
        duration_seconds = req.duration_seconds,
        error_rate = req.error_rate.unwrap_or(0.0),
        "latency_spike started"
    );
    (StatusCode::ACCEPTED, Json(ApiMessage { status: "started" })).into_response()
}

async fn stop_failure_mode(
    State(state): State<Arc<AppState>>,
    Path(incident_id): Path<String>,
) -> Response {
    match stop_incident(&state, &incident_id).await {
        Ok(()) => (StatusCode::OK, Json(ApiMessage { status: "stopped" })).into_response(),
        Err(err) => {
            warn!(error = %err, incident_id, "stop failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "stop failed").into_response()
        }
    }
}

async fn reset_active(State(state): State<Arc<AppState>>) -> Response {
    if let Err(err) = reset_checkout(&state).await {
        warn!(error = %err, "checkout reset failed");
        return (StatusCode::BAD_GATEWAY, "checkout reset failed").into_response();
    }

    let now = Utc::now();
    let active_ids = match active_incidents(&state) {
        Ok(ids) => ids,
        Err(err) => {
            warn!(error = %err, "active ledger query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "active ledger query failed",
            )
                .into_response();
        }
    };
    for id in active_ids {
        let _ = finalize_ledger(&state, &id, now);
        let _ = write_resolved_annotation(&state, &id, now).await;
    }
    (StatusCode::OK, Json(ApiMessage { status: "reset" })).into_response()
}

async fn list_ledger(State(state): State<Arc<AppState>>) -> Response {
    match read_ledger(&state, None) {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => {
            warn!(error = %err, "ledger read failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "ledger read failed").into_response()
        }
    }
}

async fn get_ledger(
    State(state): State<Arc<AppState>>,
    Path(incident_id): Path<String>,
) -> Response {
    match read_ledger(&state, Some(&incident_id)) {
        Ok(mut rows) if !rows.is_empty() => Json(rows.remove(0)).into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, incident_id, "ledger read failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "ledger read failed").into_response()
        }
    }
}

async fn reset_ledger(State(state): State<Arc<AppState>>) -> Response {
    let conn = state.db.lock();
    match conn.execute("DELETE FROM ledger", []) {
        Ok(_) => (StatusCode::OK, Json(ApiMessage { status: "cleared" })).into_response(),
        Err(err) => {
            warn!(error = %err, "ledger reset failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "ledger reset failed").into_response()
        }
    }
}

fn insert_ledger(
    state: &AppState,
    req: &FailureRequest,
    started_at: DateTime<Utc>,
    expected_evidence: &serde_json::Value,
) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let evidence = serde_json::to_string(expected_evidence)?;
    let conn = state.db.lock();
    conn.execute(
        r#"
        INSERT INTO ledger (incident_id, service, dependency, mode, started_at, ended_at, expected_evidence, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?7)
        ON CONFLICT(incident_id) DO UPDATE SET
            mode=excluded.mode,
            started_at=excluded.started_at,
            ended_at=NULL,
            expected_evidence=excluded.expected_evidence,
            updated_at=excluded.updated_at
        "#,
        params![
            req.incident_id,
            SERVICE,
            DEPENDENCY,
            "latency_spike",
            started_at.to_rfc3339(),
            evidence,
            now,
        ],
    )?;
    Ok(())
}

fn finalize_ledger(
    state: &AppState,
    incident_id: &str,
    ended_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let conn = state.db.lock();
    conn.execute(
        "UPDATE ledger SET ended_at = COALESCE(ended_at, ?2), updated_at = ?2 WHERE incident_id = ?1",
        params![incident_id, ended_at.to_rfc3339()],
    )?;
    Ok(())
}

fn active_incidents(state: &AppState) -> anyhow::Result<Vec<String>> {
    let conn = state.db.lock();
    let mut stmt = conn.prepare("SELECT incident_id FROM ledger WHERE ended_at IS NULL")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn read_ledger(state: &AppState, incident_id: Option<&str>) -> anyhow::Result<Vec<LedgerRow>> {
    let conn = state.db.lock();
    let mut rows = Vec::new();

    let sql = if incident_id.is_some() {
        "SELECT incident_id, service, dependency, mode, started_at, ended_at, expected_evidence FROM ledger WHERE incident_id = ?1 ORDER BY started_at DESC"
    } else {
        "SELECT incident_id, service, dependency, mode, started_at, ended_at, expected_evidence FROM ledger ORDER BY started_at DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut query = if let Some(id) = incident_id {
        stmt.query(params![id])?
    } else {
        stmt.query([])?
    };

    while let Some(row) = query.next()? {
        let started_at: String = row.get(4)?;
        let ended_at: Option<String> = row.get(5)?;
        let expected: String = row.get(6)?;
        rows.push(LedgerRow {
            incident_id: row.get(0)?,
            service: row.get(1)?,
            dependency: row.get(2)?,
            mode: row.get(3)?,
            overlap_window: overlap_window(&started_at, ended_at.as_deref())?,
            started_at,
            ended_at,
            expected_evidence: serde_json::from_str(&expected)?,
        });
    }
    Ok(rows)
}

fn overlap_window(started_at: &str, ended_at: Option<&str>) -> anyhow::Result<OverlapWindow> {
    let start = DateTime::parse_from_rfc3339(started_at)?.with_timezone(&Utc);
    let end = ended_at
        .map(DateTime::parse_from_rfc3339)
        .transpose()?
        .map(|dt| dt.with_timezone(&Utc));
    Ok(OverlapWindow {
        starts_at: (start - ChronoDuration::seconds(OVERLAP_LEAD_SECONDS)).to_rfc3339(),
        ends_at: end.map(|dt| (dt + ChronoDuration::seconds(OVERLAP_TRAIL_SECONDS)).to_rfc3339()),
        lead_seconds: OVERLAP_LEAD_SECONDS,
        trail_seconds: OVERLAP_TRAIL_SECONDS,
    })
}

async fn activate_checkout(state: &AppState, req: &FailureRequest) -> anyhow::Result<()> {
    let url = format!("{}/admin/failure-mode", state.checkout_url);
    let body = serde_json::json!({
        "mode": "latency_spike",
        "latency_ms_p95": req.latency_ms_p95,
        "incident_id": req.incident_id,
    });
    let resp = state.http.post(url).json(&body).send().await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "checkout returned {}",
        resp.status()
    );
    Ok(())
}

async fn reset_checkout(state: &AppState) -> anyhow::Result<()> {
    let url = format!("{}/admin/reset-active", state.checkout_url);
    let resp = state.http.post(url).send().await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "checkout returned {}",
        resp.status()
    );
    Ok(())
}

async fn stop_incident(state: &AppState, incident_id: &str) -> anyhow::Result<()> {
    let is_active = {
        let conn = state.db.lock();
        conn.query_row(
            "SELECT 1 FROM ledger WHERE incident_id = ?1 AND ended_at IS NULL",
            params![incident_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    };
    if !is_active {
        return Ok(());
    }

    reset_checkout(state).await?;
    let ended_at = Utc::now();
    finalize_ledger(state, incident_id, ended_at)?;
    write_resolved_annotation(state, incident_id, ended_at).await?;
    info!(incident_id, "incident stopped");
    Ok(())
}

async fn write_window_annotation(
    state: &AppState,
    incident_id: &str,
    started_at: DateTime<Utc>,
    planned_end: DateTime<Utc>,
) -> anyhow::Result<()> {
    post_annotation(
        state,
        serde_json::json!({
            "time": started_at.timestamp_millis(),
            "timeEnd": planned_end.timestamp_millis(),
            "tags": ["agentium-demo", format!("incident={incident_id}"), format!("service={SERVICE}"), "kind=window"],
            "text": serde_json::json!({
                "incident_id": incident_id,
                "service": SERVICE,
                "kind": "window",
                "started_at": started_at.to_rfc3339(),
                "planned_end_at": planned_end.to_rfc3339(),
            }).to_string(),
        }),
    )
    .await
}

async fn write_trace_annotation(
    state: &AppState,
    incident_id: &str,
    at: DateTime<Utc>,
    latency_ms_p95: u64,
) -> anyhow::Result<()> {
    post_annotation(
        state,
        serde_json::json!({
            "time": at.timestamp_millis(),
            "tags": ["agentium-demo", format!("incident={incident_id}"), format!("service={SERVICE}"), "kind=trace"],
            "text": serde_json::json!({
                "incident_id": incident_id,
                "service": SERVICE,
                "kind": "trace",
                "trace_id": uuid::Uuid::new_v4().simple().to_string(),
                "span_id": uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
                "parent_span_id": "0000000000000000",
                "operation": "POST /payments/authorize",
                "dependency": DEPENDENCY,
                "duration_ms": latency_ms_p95,
                "status": "slow",
                "note": "slow dependency span",
            }).to_string(),
        }),
    )
    .await
}

async fn write_resolved_annotation(
    state: &AppState,
    incident_id: &str,
    ended_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    post_annotation(
        state,
        serde_json::json!({
            "time": ended_at.timestamp_millis(),
            "tags": ["agentium-demo", format!("incident={incident_id}"), format!("service={SERVICE}"), "kind=window", "status=resolved"],
            "text": serde_json::json!({
                "incident_id": incident_id,
                "service": SERVICE,
                "kind": "window",
                "status": "resolved",
                "ended_at": ended_at.to_rfc3339(),
            }).to_string(),
        }),
    )
    .await
}

async fn post_annotation(state: &AppState, body: serde_json::Value) -> anyhow::Result<()> {
    let Some(grafana) = &state.grafana else {
        warn!("GRAFANA_URL unset; annotation skipped");
        return Ok(());
    };
    let url = format!("{}/api/annotations", grafana.url.trim_end_matches('/'));
    let mut req = state.http.post(url).json(&body);
    if let Some(token) = &grafana.token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        warn!(status = %resp.status(), "grafana annotation write failed");
    }
    Ok(())
}

fn expected_evidence(latency_ms_p95: u64) -> serde_json::Value {
    serde_json::json!([
        {
            "kind": "metric",
            "name": "p95_latency",
            "required_query_regex": "histogram_quantile\\(0\\.95.*demo_service_request_duration_seconds_bucket",
            "required_labels": {"service": SERVICE},
            "expected_value_min": 0.75,
            "expected_value_unit": "seconds",
            "time_overlap_required": true
        },
        {
            "kind": "log",
            "name": "slow_dependency_log",
            "required_substrings": ["payment authorization ok"],
            "required_labels": {"service": DEPENDENCY},
            "min_count": 1,
            "time_overlap_required": true
        },
        {
            "kind": "trace",
            "name": "slow_payment_span",
            "required_substrings": ["POST /payments/authorize", "slow dependency span"],
            "required_labels": {"service": SERVICE, "dependency": DEPENDENCY},
            "min_count": 1,
            "expected_duration_ms_min": latency_ms_p95,
            "time_overlap_required": true
        }
    ])
}

fn default_duration_seconds() -> u64 {
    300
}

fn default_latency_ms_p95() -> u64 {
    1800
}
