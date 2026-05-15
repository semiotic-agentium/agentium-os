//! `GET /cluster/agents` — cluster-wide agent view with version-skew detection.
//!
//! Operators querying a single runner only see that runner's local `/agents`.
//! In a multi-runner deployment, the same package may be deployed at different
//! content hashes on different runners (issue #387). This endpoint fans out to
//! each registered runner's `/agents`, falls back to the placement table for
//! runners that are unreachable, and reports a `version_skew` flag per package
//! when more than one distinct `content_hash` is observed.

use std::{collections::BTreeMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use axum::{Json, extract::State, http::StatusCode as AxumStatus};
use futures_util::stream::{FuturesUnordered, StreamExt};
use http_api_problem::HttpApiProblem;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiState, metrics, openapi::AgentDiscoveryEntryDto};

/// Cluster directory: lists registered runners and known placements.
///
/// Implemented by the runner crate against the shared `cluster_runners` /
/// `cluster_agent_placements` tables. `None` in standalone mode.
#[async_trait]
pub trait ClusterDirectoryService: Send + Sync {
    /// Live runners (heartbeat within the placement TTL window).
    async fn list_runners(&self) -> Result<Vec<ClusterRunnerInfo>, ClusterDirectoryError>;

    /// Active placements, regardless of runner liveness — used as a fallback
    /// source for runners whose `/agents` fan-out fetch failed.
    async fn list_placements(&self) -> Result<Vec<ClusterPlacementInfo>, ClusterDirectoryError>;
}

/// Failures from the cluster directory backend (e.g. SurrealDB transport).
#[derive(Debug)]
pub struct ClusterDirectoryError(pub String);

impl std::fmt::Display for ClusterDirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClusterDirectoryError {}

/// One row from the `cluster_runners` table.
#[derive(Debug, Clone)]
pub struct ClusterRunnerInfo {
    pub runner_id: String,
    pub endpoint: String,
    pub service_instance_id: String,
    pub pod_name: Option<String>,
    pub last_heartbeat_ms: Option<i64>,
}

/// One row from the `cluster_agent_placements` table.
#[derive(Debug, Clone)]
pub struct ClusterPlacementInfo {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub runner_id: String,
    pub runner_endpoint: String,
    pub content_hash: String,
    pub status: String,
    pub updated_at_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// Cluster-wide agent view (`GET /cluster/agents` response body).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterAgentsResponseDto {
    /// One entry per known runner, with reachability for this fan-out.
    pub runners: Vec<ClusterRunnerStatusDto>,
    /// One entry per `(agent_package, agent_instance_id)` observed in the
    /// cluster, with the per-runner placements that back it.
    pub agents: Vec<ClusterAgentRowDto>,
}

/// Status of a runner during this fan-out attempt.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterRunnerStatusDto {
    pub runner_id: String,
    pub service_instance_id: String,
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_ms: Option<i64>,
    /// `true` if this runner's `/agents` fetch succeeded.
    pub reachable: bool,
    /// Error detail when `reachable` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One row in the cluster-wide agent view, aggregated by package+instance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterAgentRowDto {
    pub agent_package: String,
    pub agent_instance_id: String,
    /// `true` when the placements report more than one distinct `content_hash`.
    pub version_skew: bool,
    pub placements: Vec<ClusterAgentPlacementDto>,
}

/// One placement of an agent on a specific runner.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterAgentPlacementDto {
    pub runner_id: String,
    pub service_instance_id: String,
    pub endpoint: String,
    /// `None` when the runner's agent card had no content hash. Such rows
    /// are excluded from the `version_skew` calculation so an unknown hash
    /// never collapses with another unknown one and falsely reports parity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Display name from the runner's agent card; absent when only the
    /// placement table reported this entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Display version from the runner's agent card; absent when only the
    /// placement table reported this entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `"runner"` when the data came from a successful `/agents` fan-out;
    /// `"placement"` when only the placement table knew about it.
    pub source: PlacementSourceDto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlacementSourceDto {
    Runner,
    Placement,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `GET /cluster/agents` — list every package known to the cluster.
///
/// The response is the union of each runner's local `/agents` (for reachable
/// runners) plus the `cluster_agent_placements` table (as a fallback for
/// unreachable ones). `version_skew` is set per row when more than one
/// distinct `content_hash` is observed across the runners hosting that
/// package+instance.
///
/// Returns `404` in standalone mode (no cluster directory).
#[utoipa::path(
    get,
    path = "/cluster/agents",
    tag = "cluster",
    responses(
        (status = 200, description = "Cluster-wide agent view with version skew", body = ClusterAgentsResponseDto),
        (status = 404, description = "Standalone mode — no cluster directory"),
        (status = 500, description = "Cluster directory backend failed"),
    ),
)]
pub async fn get_cluster_agents(
    State(state): State<Arc<ApiState>>,
) -> Result<(AxumStatus, Json<ClusterAgentsResponseDto>), HttpApiProblem> {
    let start = Instant::now();
    let directory = state.cluster_directory.as_ref().ok_or_else(|| {
        metrics::record_request("get_cluster_agents", "not_found", start.elapsed());
        HttpApiProblem::try_new(404)
            .expect("404 is valid")
            .title("Not Found")
            .detail(
                "cluster mode is not enabled; /cluster/agents is only available in cluster mode",
            )
    })?;

    let (runners, placements) =
        tokio::try_join!(directory.list_runners(), directory.list_placements()).map_err(|e| {
            metrics::record_request("get_cluster_agents", "internal", start.elapsed());
            HttpApiProblem::try_new(500)
                .expect("500 is valid")
                .title("Internal Server Error")
                .detail(format!("cluster directory query: {e}"))
        })?;

    let body = build_cluster_view(&runners, &placements, fetch_runner_agents).await;
    metrics::record_request("get_cluster_agents", "success", start.elapsed());
    Ok((AxumStatus::OK, Json(body)))
}

/// Outcome of a single `/agents` fan-out fetch, parameterised so tests can
/// inject deterministic responses without spinning up real HTTP servers.
pub(crate) enum FanOutOutcome {
    Reachable(Vec<AgentDiscoveryEntryDto>),
    Unreachable(String),
}

/// Build the response from cluster directory state and a per-runner fan-out
/// strategy. Pure with respect to network I/O — the `fetch` closure decides
/// how to talk to peer runners (real HTTP in production, deterministic stubs
/// in unit tests).
pub(crate) async fn build_cluster_view<F, Fut>(
    runners: &[ClusterRunnerInfo],
    placements: &[ClusterPlacementInfo],
    fetch: F,
) -> ClusterAgentsResponseDto
where
    F: Fn(ClusterRunnerInfo) -> Fut + Clone,
    Fut: std::future::Future<Output = FanOutOutcome>,
{
    // Fan-out concurrency is unbounded by design: pilot clusters are O(10)
    // runners and each request carries its own 5 s timeout, so an explicit
    // semaphore would add complexity without benefit. Revisit if cluster
    // size grows.
    let mut in_flight = FuturesUnordered::new();
    for runner in runners.iter().cloned() {
        let fetch = fetch.clone();
        in_flight.push(async move {
            let outcome = fetch(runner.clone()).await;
            (runner, outcome)
        });
    }

    let mut runner_statuses: Vec<ClusterRunnerStatusDto> = Vec::with_capacity(runners.len());
    // Aggregate keyed on `(package, instance, runner_id)` to dedupe between
    // the runner fan-out result and the placement table fallback.
    let mut rows: BTreeMap<(String, String), BTreeMap<String, ClusterAgentPlacementDto>> =
        BTreeMap::new();

    while let Some((runner, outcome)) = in_flight.next().await {
        match outcome {
            FanOutOutcome::Reachable(entries) => {
                runner_statuses.push(ClusterRunnerStatusDto {
                    runner_id: runner.runner_id.clone(),
                    service_instance_id: runner.service_instance_id.clone(),
                    endpoint: runner.endpoint.clone(),
                    last_heartbeat_ms: runner.last_heartbeat_ms,
                    reachable: true,
                    error: None,
                });
                for entry in entries {
                    let content_hash = entry.agent_card.content_hash.clone();
                    let key = (entry.agent_package.clone(), entry.agent_instance_id.clone());
                    let placement = ClusterAgentPlacementDto {
                        runner_id: runner.runner_id.clone(),
                        service_instance_id: runner.service_instance_id.clone(),
                        endpoint: runner.endpoint.clone(),
                        content_hash,
                        name: Some(entry.name.clone()),
                        version: Some(entry.version.clone()),
                        source: PlacementSourceDto::Runner,
                    };
                    rows.entry(key)
                        .or_default()
                        .insert(runner.runner_id.clone(), placement);
                }
            }
            FanOutOutcome::Unreachable(err) => {
                runner_statuses.push(ClusterRunnerStatusDto {
                    runner_id: runner.runner_id.clone(),
                    service_instance_id: runner.service_instance_id.clone(),
                    endpoint: runner.endpoint.clone(),
                    last_heartbeat_ms: runner.last_heartbeat_ms,
                    reachable: false,
                    error: Some(err),
                });
            }
        }
    }

    // For every (package, instance, runner) appearing in the placement table
    // but missing from the runner fan-out (because that runner was
    // unreachable, or because it has not been registered yet), fall back to
    // the placement row. Reachable runners are authoritative — never
    // overwrite their entries with possibly-stale placement data.
    for placement in placements {
        if placement.status != "active" {
            continue;
        }
        let key = (
            placement.agent_package.clone(),
            placement.agent_instance_id.clone(),
        );
        let per_runner = rows.entry(key).or_default();
        if per_runner.contains_key(&placement.runner_id) {
            continue;
        }
        // Look up the runner's service_instance_id from the runners list if
        // possible; otherwise fall back to runner_id.
        let service_instance_id = runners
            .iter()
            .find(|r| r.runner_id == placement.runner_id)
            .map(|r| r.service_instance_id.clone())
            .unwrap_or_else(|| placement.runner_id.clone());
        let content_hash = if placement.content_hash.is_empty() {
            None
        } else {
            Some(placement.content_hash.clone())
        };
        per_runner.insert(
            placement.runner_id.clone(),
            ClusterAgentPlacementDto {
                runner_id: placement.runner_id.clone(),
                service_instance_id,
                endpoint: placement.runner_endpoint.clone(),
                content_hash,
                name: None,
                version: None,
                source: PlacementSourceDto::Placement,
            },
        );
    }

    let mut agents: Vec<ClusterAgentRowDto> = rows
        .into_iter()
        .map(|((agent_package, agent_instance_id), per_runner)| {
            let placements: Vec<ClusterAgentPlacementDto> = per_runner.into_values().collect();
            // Unknown hashes (`None`) are excluded so a missing-hash row never
            // pairs with another missing-hash row to falsely report parity.
            let mut hashes: Vec<&str> = placements
                .iter()
                .filter_map(|p| p.content_hash.as_deref())
                .collect();
            hashes.sort();
            hashes.dedup();
            let version_skew = hashes.len() > 1;
            ClusterAgentRowDto {
                agent_package,
                agent_instance_id,
                version_skew,
                placements,
            }
        })
        .collect();
    agents.sort_by(|a, b| {
        a.agent_package
            .cmp(&b.agent_package)
            .then(a.agent_instance_id.cmp(&b.agent_instance_id))
    });
    runner_statuses.sort_by(|a, b| a.runner_id.cmp(&b.runner_id));

    ClusterAgentsResponseDto {
        runners: runner_statuses,
        agents,
    }
}

/// Production fan-out: hit a peer runner's `/agents` over HTTP with SSRF
/// validation and a tight timeout. Returns `Unreachable` on any failure so
/// the row falls back to the placement table.
async fn fetch_runner_agents(runner: ClusterRunnerInfo) -> FanOutOutcome {
    let target =
        match baml_rt_router::ssrf::resolve_and_validate_cluster_endpoint(&runner.endpoint).await {
            Ok((url, addrs)) => (url, addrs),
            Err(e) => return FanOutOutcome::Unreachable(format!("endpoint validation: {e}")),
        };
    let (validated_url, resolved_addrs) = target;
    let host = match validated_url.host() {
        Some(url::Host::Domain(d)) => d.to_string(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => return FanOutOutcome::Unreachable("endpoint has no host".to_string()),
    };
    // Build `<origin>/agents` from the validated URL — strips any
    // attacker-controlled path/query/fragment and percent-encodes the segment.
    let mut agents_url = validated_url.clone();
    agents_url.set_query(None);
    agents_url.set_fragment(None);
    if agents_url.path_segments_mut().is_err() {
        return FanOutOutcome::Unreachable("endpoint URL is not a base".to_string());
    }
    agents_url
        .path_segments_mut()
        .expect("path_segments_mut succeeded above")
        .clear()
        .push("agents");

    let client = match reqwest::Client::builder()
        .connect_timeout(FETCH_TIMEOUT)
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &resolved_addrs)
        .build()
    {
        Ok(c) => c,
        Err(e) => return FanOutOutcome::Unreachable(format!("client build: {e}")),
    };

    let resp = match client.get(agents_url.as_str()).send().await {
        Ok(r) => r,
        Err(e) => return FanOutOutcome::Unreachable(format!("request: {e}")),
    };
    if !resp.status().is_success() {
        return FanOutOutcome::Unreachable(format!("status {status}", status = resp.status()));
    }
    match resp.json::<Vec<AgentDiscoveryEntryDto>>().await {
        Ok(entries) => FanOutOutcome::Reachable(entries),
        Err(e) => FanOutOutcome::Unreachable(format!("decode /agents body: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::AgentCardDto;

    fn runner(id: &str, sid: &str) -> ClusterRunnerInfo {
        ClusterRunnerInfo {
            runner_id: id.to_string(),
            endpoint: format!("http://{id}:18080"),
            service_instance_id: sid.to_string(),
            pod_name: None,
            last_heartbeat_ms: Some(1_000),
        }
    }

    fn entry(pkg: &str, inst: &str, hash: &str, version: &str) -> AgentDiscoveryEntryDto {
        AgentDiscoveryEntryDto {
            agent_package: pkg.to_string(),
            agent_instance_id: inst.to_string(),
            name: pkg.to_string(),
            version: version.to_string(),
            agent_card: AgentCardDto {
                name: pkg.to_string(),
                version: version.to_string(),
                content_hash: Some(hash.to_string()),
                repository_version: None,
                agent_package: pkg.to_string(),
                agent_instance_id: inst.to_string(),
                tools: Vec::new(),
                baml_functions: Vec::new(),
                description: None,
                capabilities: Vec::new(),
                tags: Vec::new(),
                subscriptions: Vec::new(),
            },
        }
    }

    /// Both runners report the same content_hash → no skew, two placements
    /// per agent row.
    #[tokio::test]
    async fn no_skew_when_hashes_match() {
        let runners = vec![
            runner("r0", "agentium-runner-0"),
            runner("r1", "agentium-runner-1"),
        ];
        let placements = Vec::new();
        let fetch = |_r: ClusterRunnerInfo| async move {
            FanOutOutcome::Reachable(vec![entry("echo", "default", "hash-A", "v1")])
        };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        assert_eq!(view.runners.len(), 2);
        assert!(view.runners.iter().all(|r| r.reachable));
        assert_eq!(view.agents.len(), 1);
        let row = &view.agents[0];
        assert_eq!(row.placements.len(), 2);
        assert!(!row.version_skew, "matching hashes must not flag skew");
    }

    /// Two runners host the same package at different hashes → `version_skew=true`.
    /// This is the exact scenario described in issue #387.
    #[tokio::test]
    async fn skew_when_hashes_differ() {
        let runners = vec![
            runner("r0", "agentium-runner-0"),
            runner("r1", "agentium-runner-1"),
        ];
        let placements = Vec::new();
        let fetch = |r: ClusterRunnerInfo| async move {
            let hash = if r.runner_id == "r0" {
                "hash-v2"
            } else {
                "hash-v1"
            };
            let ver = if r.runner_id == "r0" { "v2" } else { "v1" };
            FanOutOutcome::Reachable(vec![entry("dispatch-echo", "default", hash, ver)])
        };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        assert_eq!(view.agents.len(), 1);
        let row = &view.agents[0];
        assert!(
            row.version_skew,
            "two distinct content_hashes must flag version_skew"
        );
        assert_eq!(row.placements.len(), 2);
        let hashes: Vec<&str> = row
            .placements
            .iter()
            .filter_map(|p| p.content_hash.as_deref())
            .collect();
        assert!(hashes.contains(&"hash-v1") && hashes.contains(&"hash-v2"));
        assert!(
            row.placements
                .iter()
                .all(|p| p.source == PlacementSourceDto::Runner),
            "both runners answered the fan-out, so every placement source is `runner`"
        );
    }

    /// An unreachable runner falls back to the placement table; its row is
    /// tagged `source=placement`. Skew detection still works on the union.
    #[tokio::test]
    async fn unreachable_runner_falls_back_to_placement_table() {
        let runners = vec![
            runner("r0", "agentium-runner-0"),
            runner("r1", "agentium-runner-1"),
        ];
        let placements = vec![ClusterPlacementInfo {
            agent_package: "dispatch-echo".to_string(),
            agent_instance_id: "default".to_string(),
            runner_id: "r1".to_string(),
            runner_endpoint: "http://r1:18080".to_string(),
            content_hash: "hash-from-placement".to_string(),
            status: "active".to_string(),
            updated_at_ms: Some(500),
        }];
        let fetch = |r: ClusterRunnerInfo| async move {
            if r.runner_id == "r0" {
                FanOutOutcome::Reachable(vec![entry(
                    "dispatch-echo",
                    "default",
                    "hash-from-runner",
                    "v2",
                )])
            } else {
                FanOutOutcome::Unreachable("connection refused".to_string())
            }
        };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        let r1 = view
            .runners
            .iter()
            .find(|r| r.runner_id == "r1")
            .expect("r1 status present");
        assert!(!r1.reachable);
        assert_eq!(r1.error.as_deref(), Some("connection refused"));

        let row = view
            .agents
            .iter()
            .find(|a| a.agent_package == "dispatch-echo")
            .unwrap();
        assert_eq!(row.placements.len(), 2);
        assert!(row.version_skew, "runner hash ≠ placement hash flags skew");
        let placement_row = row
            .placements
            .iter()
            .find(|p| p.runner_id == "r1")
            .expect("r1 placement entry");
        assert_eq!(placement_row.source, PlacementSourceDto::Placement);
        assert_eq!(
            placement_row.content_hash.as_deref(),
            Some("hash-from-placement")
        );
        assert!(placement_row.name.is_none());
    }

    /// Reachable runner is authoritative: the placement table is not allowed
    /// to overwrite the live `content_hash` reported by `/agents`.
    #[tokio::test]
    async fn reachable_runner_overrides_placement_table_hash() {
        let runners = vec![runner("r0", "agentium-runner-0")];
        let placements = vec![ClusterPlacementInfo {
            agent_package: "echo".to_string(),
            agent_instance_id: "default".to_string(),
            runner_id: "r0".to_string(),
            runner_endpoint: "http://r0:18080".to_string(),
            content_hash: "stale-hash".to_string(),
            status: "active".to_string(),
            updated_at_ms: Some(500),
        }];
        let fetch = |_r: ClusterRunnerInfo| async move {
            FanOutOutcome::Reachable(vec![entry("echo", "default", "fresh-hash", "v3")])
        };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        assert_eq!(view.agents.len(), 1);
        let row = &view.agents[0];
        assert_eq!(row.placements.len(), 1);
        assert_eq!(
            row.placements[0].content_hash.as_deref(),
            Some("fresh-hash")
        );
        assert_eq!(row.placements[0].source, PlacementSourceDto::Runner);
        assert!(!row.version_skew);
    }

    /// Two runners report the same agent without a content_hash. Skew is
    /// not detectable — but the prior implementation collapsed both `None`s
    /// to the sentinel `"unknown"` and falsely reported parity. Verify
    /// `None` is excluded from skew calculation and round-trips honestly.
    #[tokio::test]
    async fn missing_content_hashes_do_not_collapse_to_false_parity() {
        let runners = vec![
            runner("r0", "agentium-runner-0"),
            runner("r1", "agentium-runner-1"),
        ];
        let placements = Vec::new();
        let fetch = |_r: ClusterRunnerInfo| async move {
            let mut e = entry("echo", "default", "", "v1");
            e.agent_card.content_hash = None;
            FanOutOutcome::Reachable(vec![e])
        };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        let row = &view.agents[0];
        assert_eq!(row.placements.len(), 2);
        assert!(row.placements.iter().all(|p| p.content_hash.is_none()));
        assert!(
            !row.version_skew,
            "unknown hashes can never prove skew (but also must not falsely deny it)"
        );
    }

    /// Placement rows with non-active status (e.g. tombstones) are ignored.
    #[tokio::test]
    async fn inactive_placements_are_skipped() {
        let runners = vec![runner("r0", "agentium-runner-0")];
        let placements = vec![ClusterPlacementInfo {
            agent_package: "ghost".to_string(),
            agent_instance_id: "default".to_string(),
            runner_id: "r0".to_string(),
            runner_endpoint: "http://r0:18080".to_string(),
            content_hash: "tombstone-hash".to_string(),
            status: "removed".to_string(),
            updated_at_ms: Some(500),
        }];
        let fetch = |_r: ClusterRunnerInfo| async move { FanOutOutcome::Reachable(Vec::new()) };

        let view = build_cluster_view(&runners, &placements, fetch).await;
        assert!(
            view.agents.is_empty(),
            "non-active placements must not appear"
        );
    }
}
