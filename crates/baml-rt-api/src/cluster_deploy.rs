//! `POST /cluster/deploy` — fan out an agent deploy to every runner in
//! the cluster with a single request.
//!
//! Without this route, upgrading an agent across a multi-runner deployment
//! requires the operator to port-forward into each pod and call its local
//! `POST /deploy`. kube-proxy routes the API service ClusterIP to a
//! single runner per request, so a naive cluster-level deploy lands on
//! one runner while the others stay at the old hash — the version-skew
//! scenario the `GET /cluster/agents` view was added to surface (issue
//! #387).
//!
//! Behaviour mirrors the read-side fan-out in [`crate::cluster_agents`]:
//! resolve runners through the [`ClusterDirectoryService`], short-circuit
//! the loopback HTTP call for the local runner (deploy in-process via the
//! [`DeploymentManager`]), and aggregate per-runner outcomes into one
//! response. Standalone mode returns `404` — the route is only meaningful
//! in cluster mode.

use std::{sync::Arc, time::Instant};

use axum::{Json, extract::State};
use baml_rt_core::DeploymentContentHash;
use futures_util::stream::{FuturesUnordered, StreamExt};
use http_api_problem::HttpApiProblem;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use utoipa::ToSchema;

use crate::{
    ApiState,
    cluster_agents::ClusterRunnerInfo,
    handlers, metrics,
    openapi::{DeployRequestDto, DeployResponseDto},
};

const PEER_DEPLOY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// Cluster-wide deploy result (`POST /cluster/deploy` response body).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterDeployResponseDto {
    /// Resolved content hash that was fanned out to every runner.
    pub hash: String,
    /// One entry per runner the deploy was attempted on.
    pub runners: Vec<ClusterDeployRunnerResultDto>,
    /// `true` when every runner reports success. False when any runner
    /// failed — partial deploys do not roll back successes, since
    /// `POST /deploy` is idempotent on subsequent attempts.
    pub all_succeeded: bool,
}

/// Per-runner deploy outcome.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterDeployRunnerResultDto {
    pub runner_id: String,
    pub service_instance_id: String,
    pub endpoint: String,
    /// `true` when the runner accepted (or already had) the deploy.
    pub ok: bool,
    /// `Some(true)` when the runner reported the hash was already deployed.
    /// `Some(false)` when the runner accepted a fresh deploy. `None` when
    /// the runner failed (see [`Self::error`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_deployed: Option<bool>,
    /// Error detail when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /cluster/deploy` — fan out one deploy to every runner in the cluster.
///
/// Resolves the request hash (or name/version → hash via the local
/// repository) once, then forwards the resolved hash to every runner
/// known to the [`ClusterDirectoryService`]. The local runner is served
/// from the in-process [`DeploymentManager`] without a loopback HTTP
/// round-trip. Peer runners get a `POST /deploy` request with the local
/// runner token forwarded — the cluster shares one token, so this is
/// authenticated cluster-internal traffic.
///
/// Partial failures do not roll back: `POST /deploy` is idempotent, so
/// an operator retrying after a transient peer failure converges. The
/// response carries per-runner detail; clients should inspect
/// `all_succeeded` plus the `runners` array rather than relying on a
/// single status code.
#[utoipa::path(
    post,
    path = "/cluster/deploy",
    tag = "cluster",
    request_body = DeployRequestDto,
    security(("RunnerToken" = [])),
    responses(
        (status = 200, description = "Per-runner deploy results (inspect all_succeeded)", body = ClusterDeployResponseDto),
        (status = 400, description = "Invalid request (missing hash or name+version, bad hash format)"),
        (status = 401, description = "Missing or invalid runner token (cluster mode)"),
        (status = 404, description = "Standalone mode — no cluster directory"),
        (status = 500, description = "Cluster directory backend failed"),
    ),
)]
pub async fn post_cluster_deploy(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<DeployRequestDto>,
) -> Result<Json<ClusterDeployResponseDto>, HttpApiProblem> {
    let start = Instant::now();
    let directory = match state.cluster.cluster_handles() {
        Some((directory, _heartbeat)) => directory,
        None => {
            metrics::record_request("post_cluster_deploy", "not_found", start.elapsed());
            return Err(handlers::problem(
                404,
                "Not Found",
                "cluster mode is not enabled; /cluster/deploy is only available in cluster mode",
            ));
        }
    };

    // Resolve to a single hash BEFORE fanning out so every peer receives
    // exactly the same content, and so name/version validation surfaces
    // as an early 400 instead of a peer-side 4xx repeated N times.
    let hash = handlers::resolve_deploy_hash(&state, body)
        .await
        .inspect_err(|p| {
            metrics::record_request(
                "post_cluster_deploy",
                metrics::http_problem_result_label(p),
                start.elapsed(),
            );
        })?;
    let content_hash = hash.parse::<DeploymentContentHash>().map_err(|e| {
        metrics::record_request("post_cluster_deploy", "bad_request", start.elapsed());
        handlers::problem(400, "Bad Request", format!("invalid hash: {e}"))
    })?;

    let runners = directory.list_runners().await.map_err(|e| {
        metrics::record_request("post_cluster_deploy", "internal", start.elapsed());
        handlers::problem(500, "Internal Server Error", format!("list runners: {e}"))
    })?;

    let local_runner_id = directory.local_runner_id().to_string();
    let runner_token = state.runner_token.clone();
    let response = fan_out_deploy(
        &state,
        &local_runner_id,
        runner_token.as_deref(),
        &runners,
        &hash,
        &content_hash,
    )
    .await;

    metrics::record_request("post_cluster_deploy", "success", start.elapsed());
    Ok(Json(response))
}

/// Per-runner deploy strategy. Parameterised so unit tests can inject a
/// deterministic peer-call stub without spinning up real HTTP servers.
pub(crate) type DeployOutcome = Result<bool, String>;

/// Boxed pinned future that yields `(runner, outcome)` once the per-runner
/// deploy completes. Used so the local short-circuit and the remote
/// fan-out paths share one `FuturesUnordered` queue and therefore poll
/// concurrently instead of serializing.
type DeployFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = (ClusterRunnerInfo, DeployOutcome)> + Send>>;

/// Build the response by dispatching one deploy per runner: local through
/// the in-process [`DeploymentManager`], remote via HTTP `POST /deploy`.
async fn fan_out_deploy(
    state: &Arc<ApiState>,
    local_runner_id: &str,
    runner_token: Option<&str>,
    runners: &[ClusterRunnerInfo],
    hash: &str,
    content_hash: &DeploymentContentHash,
) -> ClusterDeployResponseDto {
    // Local and remote deploys share one FuturesUnordered queue so every
    // runner's deploy runs concurrently, keeping wall time proportional
    // to the slowest single runner rather than the runner count.
    let mut in_flight: FuturesUnordered<DeployFuture> = FuturesUnordered::new();
    for runner in runners.iter().cloned() {
        if runner.runner_id == local_runner_id {
            let state = Arc::clone(state);
            let content_hash = content_hash.clone();
            in_flight.push(Box::pin(async move {
                let outcome = deploy_local(&state, &content_hash, &runner).await;
                (runner, outcome)
            }));
        } else {
            let url = runner.endpoint.clone();
            let token = runner_token.map(str::to_string);
            let hash_for_peer = hash.to_string();
            in_flight.push(Box::pin(async move {
                let outcome = deploy_remote(&url, token.as_deref(), &hash_for_peer, &runner).await;
                (runner, outcome)
            }));
        }
    }

    let mut results: Vec<ClusterDeployRunnerResultDto> = Vec::with_capacity(runners.len());
    while let Some((runner, outcome)) = in_flight.next().await {
        results.push(result_from_outcome(runner, outcome));
    }
    results.sort_by(|a, b| a.runner_id.cmp(&b.runner_id));
    let all_succeeded = results.iter().all(|r| r.ok);
    ClusterDeployResponseDto {
        hash: hash.to_string(),
        runners: results,
        all_succeeded,
    }
}

fn result_from_outcome(
    runner: ClusterRunnerInfo,
    outcome: DeployOutcome,
) -> ClusterDeployRunnerResultDto {
    match outcome {
        Ok(already_deployed) => ClusterDeployRunnerResultDto {
            runner_id: runner.runner_id,
            service_instance_id: runner.service_instance_id,
            endpoint: runner.endpoint,
            ok: true,
            already_deployed: Some(already_deployed),
            error: None,
        },
        Err(err) => ClusterDeployRunnerResultDto {
            runner_id: runner.runner_id,
            service_instance_id: runner.service_instance_id,
            endpoint: runner.endpoint,
            ok: false,
            already_deployed: None,
            error: Some(err),
        },
    }
}

/// Self-deploy through the in-process [`DeploymentManager`].
///
/// `DeploymentManager::deploy_by_hash` returns a `!Send` future (boot
/// touches runtime internals that aren't `Send` across await points), so
/// it has to run on a `spawn_blocking` thread with its own `block_on`.
/// Same pattern the public `POST /deploy` uses — keeps the handler's
/// outer future `Send`, which axum 0.8 requires.
async fn deploy_local(
    state: &Arc<ApiState>,
    content_hash: &DeploymentContentHash,
    runner: &ClusterRunnerInfo,
) -> DeployOutcome {
    let manager = match state.deployment_manager.as_ref() {
        Some(m) => Arc::clone(m),
        None => return Err("local DeploymentManager is not configured".to_string()),
    };
    let span = tracing::info_span!(
        "cluster_deploy_runner",
        target_runner_id = %runner.runner_id,
        target_service_instance_id = %runner.service_instance_id,
        destination = "local",
    );
    let deploy_hash = content_hash.clone();
    let started = Instant::now();
    let join = async move {
        // Reuse `handlers::run_off_worker` so the "deploy boot is `!Send`,
        // run on a blocking thread" invariant lives in one place. The
        // worker future returns Result<DeployResult, BamlRtError>; we
        // flatten that plus the JoinError into the local `DeployOutcome`
        // string-error shape.
        handlers::run_off_worker(move || async move { manager.deploy_by_hash(&deploy_hash).await })
            .await
    }
    .instrument(span)
    .await;
    let elapsed = started.elapsed();
    let outcome = match join {
        Ok(Ok(result)) => Ok(result.already_deployed),
        Ok(Err(e)) => Err(format!("local deploy: {e}")),
        Err(problem) => Err(format!(
            "local deploy task failed: {detail}",
            detail = problem.detail.unwrap_or_default(),
        )),
    };
    let label = match &outcome {
        Ok(true) => "already_deployed",
        Ok(false) => "deployed",
        Err(_) => "failed",
    };
    metrics::record_request("cluster_deploy_runner", label, elapsed);
    outcome
}

async fn deploy_remote(
    endpoint: &str,
    token: Option<&str>,
    hash: &str,
    runner: &ClusterRunnerInfo,
) -> DeployOutcome {
    let span = tracing::info_span!(
        "cluster_deploy_runner",
        target_runner_id = %runner.runner_id,
        target_service_instance_id = %runner.service_instance_id,
        destination_endpoint = %endpoint,
    );
    async move {
        let started = Instant::now();
        let outcome = deploy_remote_inner(endpoint, token, hash).await;
        let label = match &outcome {
            Ok(true) => "already_deployed",
            Ok(false) => "deployed",
            Err(_) => "failed",
        };
        metrics::record_request("cluster_deploy_runner", label, started.elapsed());
        outcome
    }
    .instrument(span)
    .await
}

async fn deploy_remote_inner(endpoint: &str, token: Option<&str>, hash: &str) -> DeployOutcome {
    let (client, deploy_url) =
        baml_rt_router::ssrf::build_validated_peer_client(endpoint, "deploy", PEER_DEPLOY_TIMEOUT)
            .await
            .map_err(|e| format!("peer client setup: {e}"))?;

    let mut req = client.post(deploy_url.as_str()).json(&serde_json::json!({
        "hash": hash,
    }));
    if let Some(t) = token {
        req = req.header("X-Runner-Token", t);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return Err(format!("request: {e}")),
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<body read error: {e}>"));
        let body = baml_rt_router::ssrf::truncate_body(&body, 512);
        return Err(format!("status {status}: {body}"));
    }
    match resp.json::<DeployResponseDto>().await {
        Ok(dto) => Ok(dto.already_deployed),
        Err(e) => Err(format!("decode /deploy body: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster_agents::ClusterRunnerInfo;

    fn runner(id: &str, sid: &str) -> ClusterRunnerInfo {
        ClusterRunnerInfo {
            runner_id: id.to_string(),
            endpoint: format!("http://{id}:18080"),
            service_instance_id: sid.to_string(),
            last_heartbeat_ms: Some(1_000),
        }
    }

    /// When every runner returns `Ok(false)` the response reports
    /// `all_succeeded=true` and one entry per runner. The local runner is
    /// distinguished only by `runner_id`; both paths produce the same DTO.
    #[test]
    fn aggregates_all_success_outcomes() {
        let runners = vec![
            (runner("r0", "agentium-runner-0"), Ok(false)),
            (runner("r1", "agentium-runner-1"), Ok(false)),
        ];
        let mut results: Vec<ClusterDeployRunnerResultDto> = runners
            .into_iter()
            .map(|(r, o)| result_from_outcome(r, o))
            .collect();
        results.sort_by(|a, b| a.runner_id.cmp(&b.runner_id));
        let all_succeeded = results.iter().all(|r| r.ok);
        assert!(all_succeeded);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.already_deployed == Some(false)));
        assert!(results.iter().all(|r| r.error.is_none()));
    }

    /// Partial failures: one runner Errs, the other Oks. Response must
    /// flag `all_succeeded=false` and carry the error on the failed row.
    /// The successful row is not rolled back — POST /deploy is idempotent.
    #[test]
    fn partial_failure_flags_all_succeeded_false() {
        let results = [
            result_from_outcome(runner("r0", "agentium-runner-0"), Ok(true)),
            result_from_outcome(
                runner("r1", "agentium-runner-1"),
                Err("connection refused".to_string()),
            ),
        ];
        let all_succeeded = results.iter().all(|r| r.ok);
        assert!(!all_succeeded);
        let r0 = results.iter().find(|r| r.runner_id == "r0").unwrap();
        assert!(r0.ok);
        assert_eq!(r0.already_deployed, Some(true));
        let r1 = results.iter().find(|r| r.runner_id == "r1").unwrap();
        assert!(!r1.ok);
        assert_eq!(r1.already_deployed, None);
        assert_eq!(r1.error.as_deref(), Some("connection refused"));
    }

    /// `already_deployed=Some(true)` round-trips through the result mapping.
    #[test]
    fn already_deployed_round_trips() {
        let result = result_from_outcome(runner("r0", "agentium-runner-0"), Ok(true));
        assert!(result.ok);
        assert_eq!(result.already_deployed, Some(true));
        assert!(result.error.is_none());
    }
}
