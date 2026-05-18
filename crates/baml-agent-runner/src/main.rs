//! BAML Agent Runner
//!
//! Starts the runner, restores previously-deployed agents from --state-dir,
//! and serves HTTP / stdio / event poll loop.

#![recursion_limit = "256"]

mod agent_package;
mod builder;
mod callback_delivery;
mod cluster;
mod config;
mod deployment_restore;
mod deployment_state;
mod package;
mod routing;
mod runner;
mod runner_config_file;
mod services;
mod stdio;

use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use baml_rt_api::RuntimeProgressMeter;
use baml_rt_core::{
    CallbackStore, DeploymentManager, DeploymentStatus, ExponentialBackoff, IngressStore,
};
use baml_rt_llm_config::{
    FnoxFileSecretResolver, OverlaySecretResolver, SECRET_LINKS_CONFIG_KEY, SecretLinksState,
    apply_secret_links_state,
};
use baml_rt_observability::{otel_env, tracing_setup};
use baml_rt_repository::{
    BlobStore, LineageStore, MetadataStore, RepositoryService, SearchStore, SurrealStore,
};
use baml_rt_tools::{
    ACCESS_ALLOWLIST_ENV, InventoryCatalog, ProducerCheckpoint,
    load_configured_event_producers_with_checkpoints, parse_access_allowlist,
};
use baml_tools_calculator as _;
#[cfg(feature = "clickup")]
use baml_tools_clickup as _;
#[cfg(feature = "memory")]
use baml_tools_memory as _;
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system::callback_delivery_gate::install_callback_delivery_gate;
use callback_delivery::RunnerCallbackDeliveryGate;
use clap::Parser;
use config::{Cli, ProvenanceDb, parse_surreal_credentials, provenance_config_builder};
use serde_json::Value;
use services::{
    ContextIndexServiceImpl, ContextMetricsServiceImpl, ConversationHistoryEventServiceImpl,
    ConversationHistoryServiceImpl, EpisodeServiceImpl, MermaidServiceImpl, PlanningServiceImpl,
    ProvenanceOpsServiceImpl,
};
use stdio::unix_timestamp_secs;
use tracing::{error, info, warn};

/// Resolve `--claude-workspaces-base` to a canonical path before the async
/// runtime starts, then pass the resolved value through config (no env vars).
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let claude_workspaces_base = cli.claude_workspaces_base.as_ref().map(|base| {
        let absolute = if base.is_absolute() {
            base.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(base)
        };
        if let Err(e) = std::fs::create_dir_all(&absolute) {
            let path = absolute.display();
            eprintln!("Error: Cannot create Claude workspaces base {path}: {e}");
            std::process::exit(1);
        }
        std::fs::canonicalize(&absolute).unwrap_or(absolute)
    });
    tokio_main(cli, claude_workspaces_base)
}

/// Reject cluster endpoints that are not routable from other pods.
/// Called during cluster setup — a poisoned placement table is worse than
/// refusing to start.
fn validate_cluster_endpoint(endpoint: &str) -> anyhow::Result<()> {
    if let Ok(url) = url::Url::parse(endpoint) {
        match url.host_str() {
            Some("0.0.0.0") => anyhow::bail!(
                "cluster endpoint {endpoint} uses 0.0.0.0 which is not routable from other pods; \
                 set RUNNER_ENDPOINT (or --runner-endpoint) to a pod-specific address \
                 (e.g. http://$(POD_NAME).runner.agentium.svc:18080)"
            ),
            Some("127.0.0.1") | Some("localhost") => anyhow::bail!(
                "cluster endpoint {endpoint} uses a loopback address which is not routable from \
                 other pods; set RUNNER_ENDPOINT (or --runner-endpoint) to a pod-specific address \
                 (e.g. http://$(POD_NAME).runner.agentium.svc:18080)"
            ),
            _ => {}
        }
    }
    Ok(())
}

#[tokio::main]
async fn tokio_main(cli: Cli, claude_workspaces_base: Option<PathBuf>) -> anyhow::Result<()> {
    // Load dotenv before tracing/OTEL bootstrap so OTEL_* from .env are visible
    // to `init_tracing()` and exporter wiring.
    let dotenv_result = dotenvy::dotenv();
    tracing_setup::init_tracing_with_resource(otel_env::build_runner_resource());
    match dotenv_result {
        Ok(path) => tracing::debug!(path = ?path, "Loaded .env"),
        Err(err) => tracing::debug!(error = ?err, "No .env loaded"),
    }

    info!("BAML Agent Runner starting");

    // Constructed before the runner so deploy boots — including the restore
    // loop, which evaluates agent top-level JS — register their JS-event-loop
    // probes against the same meter the HTTP API will publish via /diagnose.
    let runtime_progress = RuntimeProgressMeter::spawn_in_current_runtime();

    let config = cli
        .into_config(claude_workspaces_base)
        .context("Failed to parse arguments")?;
    if let Some(ref base) = config.claude_workspaces_base {
        info!(base = %base.display(), "Claude workspaces base set from --claude-workspaces-base");
    }

    match &config.provenance_db {
        ProvenanceDb::InMemory => {
            info!("Provenance backend: in-memory (:memory:), not persisted to disk")
        }
        ProvenanceDb::File(path) => {
            info!(path = %path.display(), "Provenance backend: SurrealKV directory")
        }
        ProvenanceDb::Remote { endpoint, .. } => {
            info!(endpoint = %endpoint, "Provenance backend: remote SurrealDB")
        }
    }

    let config_service: Arc<dyn baml_rt_config::ConfigService> = match &config.provenance_db {
        ProvenanceDb::InMemory => Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .context("Failed to create in-memory config store")?,
        ),
        ProvenanceDb::Remote {
            endpoint,
            username,
            password,
        } => {
            let credentials = parse_surreal_credentials(username.as_deref(), password.as_deref())
                .context("config store")?;
            Arc::new(
                connect_remote_config_store_with_retry(
                    endpoint,
                    credentials,
                    REMOTE_CONNECT_MAX_ATTEMPTS,
                    REMOTE_CONNECT_INITIAL_DELAY,
                    REMOTE_CONNECT_MAX_DELAY,
                )
                .await
                .context("Failed to create remote config store")?,
            )
        }
        ProvenanceDb::File(path) => Arc::new(
            baml_rt_config::SurrealConfigStore::open(
                path.parent()
                    .unwrap_or_else(|| {
                        tracing::debug!(path = %path.display(), "no parent, using path as config base");
                        path.as_ref()
                    })
                    .join("config.db"),
            )
            .await
            .context("Failed to open config store (config.db)")?,
        ),
    };
    let fnox_resolver = Arc::new(FnoxFileSecretResolver::default_path_resolver());
    let overlay = Arc::new(OverlaySecretResolver::new(fnox_resolver.clone()));
    let link_state: SecretLinksState =
        match config_service.get_internal(SECRET_LINKS_CONFIG_KEY).await {
            Ok(Some(v)) => serde_json::from_value(v).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "secret link state parse failed; using default");
                SecretLinksState::default()
            }),
            Ok(None) => SecretLinksState::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load secret link state; using default");
                SecretLinksState::default()
            }
        };
    apply_secret_links_state(&link_state, overlay.as_ref(), fnox_resolver.as_ref());

    let provenance_config =
        provenance_config_builder(&config.provenance_db, config_service, overlay.clone())
            .await
            .context("Failed to initialize provenance storage")?
            .with_runtime_secret_store(Some(overlay))
            .build();

    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create runner state directory {}",
            config.state_dir.display()
        )
    })?;
    let state_db_path = config.state_dir.join("state.db");
    let deployment_state = Arc::new(
        deployment_state::DeploymentStateStore::open(&state_db_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to initialize runner deployment state DB at {}",
                    state_db_path.display()
                )
            })?,
    );
    std::fs::create_dir_all(&config.repository_dir).with_context(|| {
        format!(
            "Failed to create repository directory {}",
            config.repository_dir.display()
        )
    })?;
    let repository_db_path = config.repository_dir.join("repository.db");
    let repository_store = Arc::new(
        open_repository_store_with_retry(&repository_db_path, 12, Duration::from_millis(250))
            .await
            .with_context(|| {
                format!(
                    "Failed to initialize repository DB at {}",
                    repository_db_path.display()
                )
            })?,
    );
    // Trait/object type: `baml_rt_core::CallbackStore`; process-wide slot: `baml_tools_system::callback_store`.
    baml_tools_system::callback_store::install_callback_store(
        deployment_state.clone() as Arc<dyn CallbackStore>
    );
    baml_rt_tools::ingress_store::install_ingress_store(
        deployment_state.clone() as Arc<dyn IngressStore>
    );
    let repository_service = Arc::new(RepositoryService::new(
        repository_store.clone() as Arc<dyn BlobStore>,
        repository_store.clone() as Arc<dyn MetadataStore>,
        repository_store.clone() as Arc<dyn LineageStore>,
        repository_store.clone() as Arc<dyn SearchStore>,
        repository_store as Arc<dyn baml_rt_repository::McpRegistryStore>,
    ));
    let raw_deployments = deployment_state
        .list_deployments()
        .await
        .context("Failed to read runner deployment state records")?;
    let (existing_deployments, superseded_hashes) =
        deployment_restore::dedupe_deployments_for_restore(raw_deployments);
    if !superseded_hashes.is_empty() {
        info!(
            removed = superseded_hashes.len(),
            "Deduplicating deployment state: removing superseded records for the same agent package"
        );
        for hash in &superseded_hashes {
            match deployment_state.remove_deployment(hash).await {
                Ok(true) => info!(
                    content_hash = %hash.as_str(),
                    "Removed superseded deployment record"
                ),
                Ok(false) => warn!(
                    content_hash = %hash.as_str(),
                    "Superseded deployment record already absent"
                ),
                Err(e) => warn!(
                    error = %e,
                    content_hash = %hash.as_str(),
                    "Failed to remove superseded deployment record"
                ),
            }
        }
    }
    info!(
        state_dir = %config.state_dir.display(),
        state_db = %state_db_path.display(),
        repository_dir = %config.repository_dir.display(),
        repository_db = %repository_db_path.display(),
        existing_deployments = existing_deployments.len(),
        "Runner deployment + repository backends initialized"
    );

    let access_allowlist = parse_access_allowlist();
    let env_set = std::env::var(ACCESS_ALLOWLIST_ENV).is_ok();
    let mut permitted: Vec<&'static str> = access_allowlist
        .permitted()
        .iter()
        .map(|a| a.as_str())
        .collect();
    permitted.sort_unstable();
    info!(
        env_set,
        unrestricted = access_allowlist.is_unrestricted(),
        permitted = ?permitted,
        "Tool access cap resolved (per-agent manifest allowlist still gates tool exposure)"
    );
    let builder = builder::RunnerBuilder::<builder::Loading>::new(runner::AgentRunnerConfig {
        provenance_config,
        deployment_state,
        access_policy: access_allowlist,
        stream_idle_secs: config.stream_idle_secs,
        claude_workspaces_base: config.claude_workspaces_base,
        repository_url: config.repository_url.clone(),
        embedded_repository: Some(repository_service.clone()),
        external_tools_dirs: config.external_tools_dirs.clone(),
        sandbox_bind_roots: config.sandbox_bind_roots.clone(),
        runtime_progress: runtime_progress.clone(),
    })
    .map_err(|e| anyhow::anyhow!("runner builder init: {e}"))?;

    // --- Cluster registration (remote SurrealDB mode only) ---
    // Constructed before the restore loop so that deploy_by_hash records
    // cluster placements for restored agents (otherwise they are invisible
    // to the cluster after a pod restart).
    let cluster_mgr = if let ProvenanceDb::Remote {
        ref endpoint,
        ref username,
        ref password,
    } = config.provenance_db
    {
        let credentials = parse_surreal_credentials(username.as_deref(), password.as_deref())
            .context("cluster")?;
        let cluster_db = surrealdb::engine::any::connect(endpoint)
            .await
            .context("cluster: failed to connect to shared SurrealDB")?;
        if let Some((user, pass)) = credentials {
            cluster_db
                .signin(surrealdb::opt::auth::Root {
                    username: user.to_string(),
                    password: pass.to_string(),
                })
                .await
                .context("cluster: failed to sign in to shared SurrealDB")?;
        }
        cluster_db
            .use_ns("cluster")
            .use_db("registry")
            .await
            .context("cluster: failed to select namespace")?;

        if config.serve_http.is_none() {
            tracing::warn!(
                "cluster mode active (--surreal-endpoint set) but --serve-http not specified; \
                 runner will register as http://127.0.0.1:18080 which is unreachable from other pods"
            );
        }
        let serve_addr = config.serve_http.as_deref().unwrap_or("127.0.0.1:18080");
        let runner_http_endpoint = config
            .runner_endpoint
            .clone()
            .unwrap_or_else(|| format!("http://{serve_addr}"));
        validate_cluster_endpoint(&runner_http_endpoint)?;
        let service_instance_id = baml_rt_observability::service_instance_id().to_string();
        let identity = match baml_rt_observability::pod_identity() {
            Some((namespace, pod_name)) => {
                tracing::info!(
                    namespace = %namespace,
                    pod_name = %pod_name,
                    "deriving runner_id from (POD_NAMESPACE, POD_NAME)"
                );
                cluster::RunnerIdentity::derived(
                    runner_http_endpoint,
                    service_instance_id,
                    &namespace,
                    &pod_name,
                )
            }
            None => {
                tracing::warn!("POD_NAMESPACE/POD_NAME unset; runner_id is random per process");
                cluster::RunnerIdentity::new(runner_http_endpoint, service_instance_id)
            }
        };
        let cluster_db = std::sync::Arc::new(cluster_db);
        let mgr = Arc::new(
            cluster::ClusterManager::new(cluster_db.clone(), identity, config.placement_ttl_ms)
                .await
                .map_err(|e| anyhow::anyhow!("cluster manager init: {e}"))?,
        );

        // Wire cluster resolver into the A2A router for cross-pod routing.
        let resolver = Arc::new(mgr.resolver());
        builder.runner.internal_a2a_router().set_cluster(resolver);
        if builder.runner.cluster_manager.set(mgr.clone()).is_err() {
            tracing::warn!("cluster manager already set on runner; ignoring duplicate");
        }
        info!("cluster mode enabled: runner registered before deployment restore");
        Some(mgr)
    } else {
        None
    };

    for mut deployment in existing_deployments {
        match builder
            .runner
            .deploy_by_hash(&deployment.content_hash)
            .await
        {
            Ok(result) => {
                info!(
                    content_hash = %deployment.content_hash.as_str(),
                    already_deployed = result.already_deployed,
                    "Restored deployment from runner state"
                );
            }
            Err(err) => {
                deployment.status = DeploymentStatus::Failed;
                deployment.last_error = Some(err.to_string());
                deployment.last_attempt_at = Some(unix_timestamp_secs());
                deployment.failure_count = deployment.failure_count.saturating_add(1);
                if let Err(save_err) = builder
                    .runner
                    .deployment_state()
                    .save_deployment(&deployment)
                    .await
                {
                    error!(
                        error = %save_err,
                        content_hash = %deployment.content_hash.as_str(),
                        "Failed to persist restore failure state"
                    );
                }
                error!(
                    error = %err,
                    content_hash = %deployment.content_hash.as_str(),
                    agent = %deployment.agent_name,
                    "Failed to restore deployment; agent will be unavailable until operator redeploys (re-import MCP snapshot / refresh external_tools.lock / re-publish as needed)"
                );
            }
        }
    }

    let ready = builder.build();
    // Used by GET /readyz only; keep false until event-producer registration finishes.
    let readyz = Arc::new(AtomicBool::new(false));

    install_callback_delivery_gate(Arc::new(RunnerCallbackDeliveryGate {
        runner: ready.runner(),
    }));

    let (cluster_heartbeat_health, _cluster_heartbeat_handles) = match cluster_mgr.as_ref() {
        Some(mgr) => {
            let health = baml_rt_api::ClusterHeartbeatHealth::new(cluster::HEARTBEAT_INTERVAL);
            let handles = mgr.spawn_heartbeat(health.clone());
            info!("cluster heartbeat started");
            (Some(health), Some(handles))
        }
        None => (None, None),
    };

    if let Some((agent_name, function_name, json_args)) = config.invoke {
        let args_value: Value =
            serde_json::from_str(&json_args).context("Invalid JSON arguments")?;
        let result = ready
            .invoke(&agent_name, &function_name, args_value)
            .await
            .context("Function invocation failed")?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let agents = ready.list_agents();
    if agents.is_empty() {
        info!(
            "No agents deployed yet; runner is ready to receive deploy requests via POST /deploy"
        );
    } else {
        println!("✅ Loaded {} agent(s):", agents.len());
        for agent_name in &agents {
            println!("  - {}", agent_name);
        }
    }

    let http_handle = if let Some(bind) = config.serve_http.clone() {
        let runner = ready.runner();
        let prov_config = runner.provenance_config();
        let store = prov_config.store().clone();
        let config_service = prov_config.config_service();
        let secret_resolver = prov_config.llm_secret_resolver();
        let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> = Arc::new(InventoryCatalog::new());

        let mermaid = Some(Arc::new(MermaidServiceImpl::new(
            store.clone(),
            prov_config.mermaid_cache(),
        )) as Arc<dyn baml_rt_api::MermaidService>);
        let context_metrics = Some(Arc::new(ContextMetricsServiceImpl::new(store.clone()))
            as Arc<dyn baml_rt_api::ContextMetricsService>);
        let provenance_ops = Some(Arc::new(ProvenanceOpsServiceImpl::new(store.clone()))
            as Arc<dyn baml_rt_api::ProvenanceOpsService>);
        let planning = Some(Arc::new(PlanningServiceImpl::new(store.clone()))
            as Arc<dyn baml_rt_api::PlanningService>);
        let registry_impl = ready.registry();
        let episode =
            Some(Arc::new(EpisodeServiceImpl::new(store)) as Arc<dyn baml_rt_api::EpisodeService>);
        let conversation_history = Some(Arc::new(ConversationHistoryServiceImpl::new(
            runner.provenance_config().store().clone(),
        ))
            as Arc<dyn baml_rt_api::ConversationHistoryService>);
        let conversation_history_events = Some(Arc::new(ConversationHistoryEventServiceImpl::new(
            runner.clone(),
        ))
            as Arc<dyn baml_rt_api::ConversationHistoryEventService>);
        let context_index = Some(Arc::new(ContextIndexServiceImpl::new(
            runner.provenance_config().store().clone(),
        )) as Arc<dyn baml_rt_api::ContextIndexService>);
        let web_dir = config.web_dir.clone();
        info!(
            bind = %bind,
            web_dir = ?web_dir,
            "HTTP API binding early (GET /healthz always OK; GET /readyz is 503 until event producers finish loading)"
        );
        let runtime_secret_store = prov_config.runtime_secret_store();
        let readyz_for_http = readyz.clone();
        let runner_token = config.runner_token.clone();
        // Pair the directory + heartbeat so cluster mode is a single value.
        // `Standalone` covers the no-SurrealDB path; mismatched fields are
        // no longer representable.
        let cluster = match (cluster_mgr.as_ref(), cluster_heartbeat_health) {
            (Some(mgr), Some(heartbeat)) => baml_rt_api::ClusterTopology::Cluster {
                directory: Arc::new(mgr.directory())
                    as Arc<dyn baml_rt_api::ClusterDirectoryService>,
                heartbeat,
            },
            _ => baml_rt_api::ClusterTopology::Standalone,
        };
        let api_config = baml_rt_api::ApiServerConfig {
            mermaid,
            context_metrics,
            provenance_ops,
            planning,
            episode,
            conversation_history,
            conversation_history_events,
            context_index,
            deployment_manager: Some(runner.clone() as Arc<dyn DeploymentManager>),
            repository_url: Some(config.repository_url.clone()),
            repository_service: Some(repository_service.clone()),
            runtime_secret_store,
            ready: readyz_for_http,
            runner_token,
            cluster,
            web_dir,
            ..baml_rt_api::ApiServerConfig::empty(
                tool_catalog,
                config_service,
                secret_resolver,
                runtime_progress.clone(),
            )
        };
        Some(tokio::spawn(async move {
            baml_rt_api::serve_with_services_and_deploy(registry_impl, &bind, api_config)
                .await
                .map_err(|e| anyhow::anyhow!("HTTP API server: {e}"))
        }))
    } else {
        None
    };

    // --- Event producer poll loop ---
    //
    // Always load producers so push-ingress transports (e.g. Socket Mode) can
    // start their background tasks during build. If producers are returned but
    // no explicit poll interval was set, fall back to a 1-second drain loop so
    // inbox items enqueued by push transports still get delivered.
    let dispatcher_handle = {
        let registry = ready.registry();
        let runner = ready.runner();
        let deployment_state_for_poll = runner.deployment_state().clone();
        let persisted_checkpoints: std::collections::HashMap<String, ProducerCheckpoint> =
            deployment_state_for_poll
                .list_event_producer_checkpoints()
                .await
                .context("loading persisted event producer checkpoints")?
                .into_iter()
                .map(|(producer_key, checkpoint)| {
                    (producer_key, ProducerCheckpoint::some(checkpoint))
                })
                .collect();

        let configured_producers = load_configured_event_producers_with_checkpoints(
            &InventoryCatalog::new(),
            Some(runner.provenance_config().config_service()),
            persisted_checkpoints.clone(),
        )
        .await
        .context("loading configured event producers")?;

        // Determine effective interval:
        // - Explicit --event-poll-interval-secs > 0 → use that
        // - Producers loaded but no explicit interval → 1s drain fallback
        // - No producers and no interval → skip poll loop
        let effective_interval = config.event_poll_interval.or_else(|| {
            if configured_producers.is_empty() {
                None
            } else {
                info!("push-ingress producers detected; enabling 1s inbox drain loop");
                Some(std::time::Duration::from_secs(1))
            }
        });

        if let Some(interval) = effective_interval {
            let mut dispatcher =
                baml_rt_a2a::EventDispatcher::new(registry as Arc<dyn baml_rt_a2a::AgentRegistry>);
            for producer in configured_producers {
                let producer_key = producer.producer_key().to_string();
                let checkpoint = persisted_checkpoints
                    .get(&producer_key)
                    .cloned()
                    .unwrap_or_else(ProducerCheckpoint::none);
                dispatcher
                    .register_producer_with_checkpoint(producer, checkpoint)
                    .with_context(|| format!("registering event producer {producer_key}"))?;
                info!(producer_key = %producer_key, "registered event producer");
            }

            info!(
                interval_secs = interval.as_secs(),
                "event producer poll loop enabled"
            );
            Some(tokio::spawn(async move {
                run_event_poll_loop(dispatcher, deployment_state_for_poll, interval).await;
            }))
        } else {
            None
        }
    };

    readyz.store(true, Ordering::Release);
    tracing::info!("readyz probe: ready (event producers loaded)");

    match (config.a2a_stdio, http_handle) {
        (true, Some(mut handle)) => {
            let stdio_fut = ready.run_a2a_stdio();
            tokio::pin!(stdio_fut);
            let mut http_exited = false;
            loop {
                tokio::select! {
                    stdio_result = &mut stdio_fut => {
                        if !http_exited && !handle.is_finished() {
                            info!("A2A stdio loop ended; stopping HTTP API server task");
                            handle.abort();
                        }
                        stdio_result?;
                        break;
                    }
                    http_result = &mut handle, if !http_exited => {
                        match http_result {
                            Ok(Ok(())) => warn!("HTTP API server exited; continuing A2A stdio loop"),
                            Ok(Err(err)) => warn!(error = %err, "HTTP API server exited with error; continuing A2A stdio loop"),
                            Err(join_err) if join_err.is_cancelled() => info!("HTTP API server task was cancelled; continuing A2A stdio loop"),
                            Err(join_err) => warn!("HTTP API server task join error: {join_err}; continuing A2A stdio loop"),
                        }
                        http_exited = true;
                    }
                }
            }
        }
        (true, None) => {
            ready.run_a2a_stdio().await?;
        }
        (false, Some(handle)) => {
            // K8s-pilot mode: no stdio, only the HTTP listener. Nothing in this
            // process requests a listener shutdown, so any return — clean or
            // erroring — means the listener task died. Propagate as a non-zero
            // exit so the kubelet restarts the pod (issue #341 T4).
            match handle.await {
                Ok(Ok(())) => {
                    error!(
                        "HTTP API listener task returned Ok(()) without a shutdown request; \
                         in --serve-http mode this means the listener died silently. \
                         Exiting non-zero so the kubelet restarts the pod"
                    );
                    anyhow::bail!("HTTP API listener task exited without a shutdown request");
                }
                Ok(Err(err)) => {
                    error!(error = %err, "HTTP API listener task failed");
                    return Err(err);
                }
                Err(join_err) => {
                    error!(error = %join_err, "HTTP API listener task join error");
                    return Err(anyhow::anyhow!(
                        "HTTP API listener task join error: {join_err}"
                    ));
                }
            }
        }
        (false, None) => {
            if let Some(handle) = dispatcher_handle {
                if let Err(err) = handle.await {
                    error!(error = %err, "event producer poll loop terminated unexpectedly");
                }
                return Ok(());
            }
        }
    }

    if let Some(handle) = dispatcher_handle {
        handle.abort();
    }

    info!("Agent Runner completed successfully");
    Ok(())
}

/// Tunables for the initial remote SurrealDB connection retry. Sized so the
/// runner pod absorbs the standard Kubernetes pod-startup race against the
/// SurrealDB pod (DNS not yet resolvable / WebSocket port not yet accepting
/// connections) without exiting and triggering a kubelet `BackOff` event.
/// `PER_ATTEMPT_TIMEOUT` bounds each individual attempt so a half-open TCP
/// (SYN-ACK with no WebSocket reply) can't hang the whole retry budget —
/// `SurrealConfigStore::remote` has no built-in handshake timeout.
const REMOTE_CONNECT_MAX_ATTEMPTS: NonZeroUsize = NonZeroUsize::new(6).unwrap();
const REMOTE_CONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const REMOTE_CONNECT_MAX_DELAY: Duration = Duration::from_secs(8);
const REMOTE_CONNECT_PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

async fn connect_remote_config_store_with_retry(
    endpoint: &str,
    credentials: Option<(&str, &str)>,
    max_attempts: NonZeroUsize,
    initial_delay: Duration,
    max_delay: Duration,
) -> anyhow::Result<baml_rt_config::SurrealConfigStore> {
    let max_attempts = max_attempts.get();
    let mut backoff = ExponentialBackoff::new(initial_delay, max_delay);
    let log_endpoint = redact_endpoint_credentials(endpoint);

    for attempt in 1..=max_attempts {
        let outcome = match tokio::time::timeout(
            REMOTE_CONNECT_PER_ATTEMPT_TIMEOUT,
            baml_rt_config::SurrealConfigStore::remote(endpoint, credentials),
        )
        .await
        {
            Ok(result) => result.map_err(anyhow::Error::from),
            Err(_) => Err(anyhow::anyhow!(
                "connect attempt exceeded {timeout:?}",
                timeout = REMOTE_CONNECT_PER_ATTEMPT_TIMEOUT,
            )),
        };

        let err = match outcome {
            Ok(store) => {
                if attempt > 1 {
                    info!(
                        attempt,
                        endpoint = %log_endpoint,
                        "remote config store connected after retry"
                    );
                }
                return Ok(store);
            }
            Err(err) => err,
        };

        if attempt == max_attempts {
            warn!(
                attempt,
                max_attempts,
                endpoint = %log_endpoint,
                error = %err,
                "remote config store connect failed; retries exhausted"
            );
            return Err(err);
        }

        let delay = backoff.next_delay();
        warn!(
            attempt,
            max_attempts,
            endpoint = %log_endpoint,
            delay_secs = delay.as_secs_f64(),
            error = %err,
            "remote config store connect failed; retrying after delay"
        );
        tokio::time::sleep(delay).await;
    }

    unreachable!("loop returns on the final attempt; NonZeroUsize guarantees at least one")
}

/// Strip any `userinfo` (username/password) from a URL so it is safe to
/// include in logs and error messages. Falls back to a sentinel on parse
/// failure rather than echoing the raw input. Matches the existing pattern
/// in `crates/baml-agent-runner/src/config.rs` for the provenance store
/// error path.
fn redact_endpoint_credentials(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .map(|mut u| {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        })
        .unwrap_or_else(|_| "<invalid URL>".to_string())
}

/// Open repository store with bounded retries to absorb transient embedded
/// SurrealKV startup errors seen under CI/process churn.
async fn open_repository_store_with_retry(
    path: &Path,
    max_attempts: usize,
    delay: Duration,
) -> anyhow::Result<SurrealStore> {
    let mut last_error: Option<baml_rt_repository::RepositoryError> = None;

    for attempt in 1..=max_attempts {
        match SurrealStore::open(path).await {
            Ok(store) => return Ok(store),
            Err(err) => {
                tracing::warn!(
                    attempt,
                    max_attempts,
                    path = %path.display(),
                    error = %err,
                    "repository store open failed; retrying"
                );
                last_error = Some(err);
                if attempt < max_attempts {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    match last_error {
        Some(err) => Err(anyhow::anyhow!(err))
            .with_context(|| format!("repository store open retries exhausted ({max_attempts})")),
        None => Err(anyhow::anyhow!(
            "repository store open retries exhausted without attempts"
        )),
    }
}

/// Background poll loop for registered event producers.
///
/// Polls all producers, delivers events to matched subscribers, and logs
/// outcomes. Silent when no producers are registered.
async fn run_event_poll_loop(
    mut dispatcher: baml_rt_a2a::EventDispatcher,
    deployment_state: Arc<deployment_state::DeploymentStateStore>,
    interval: std::time::Duration,
) {
    loop {
        tokio::time::sleep(interval).await;
        run_event_poll_cycle(&mut dispatcher, deployment_state.as_ref()).await;
    }
}

async fn run_event_poll_cycle(
    dispatcher: &mut baml_rt_a2a::EventDispatcher,
    deployment_state: &deployment_state::DeploymentStateStore,
) {
    let results = dispatcher.poll_and_deliver().await;
    for (producer_key, outcome) in &results {
        match outcome {
            Ok(delivery) if delivery.failures.is_empty() => {
                if delivery.subscribers_matched > 0 {
                    info!(
                        producer_key = %producer_key,
                        matched = delivery.subscribers_matched,
                        accepted = delivery.subscribers_accepted,
                        "event delivery complete"
                    );
                }
                if let Some(checkpoint) = dispatcher.checkpoint(producer_key).value()
                    && let Err(err) = deployment_state
                        .save_event_producer_checkpoint(producer_key, checkpoint)
                        .await
                {
                    warn!(
                        producer_key = %producer_key,
                        error = %err,
                        "failed to persist event producer checkpoint"
                    );
                }
            }
            Ok(delivery) => {
                warn!(
                    producer_key = %producer_key,
                    matched = delivery.subscribers_matched,
                    accepted = delivery.subscribers_accepted,
                    failures = delivery.failures.len(),
                    "event delivery partial failure"
                );
                if let Some(checkpoint) = dispatcher.checkpoint(producer_key).value()
                    && let Err(err) = deployment_state
                        .save_event_producer_checkpoint(producer_key, checkpoint)
                        .await
                {
                    warn!(
                        producer_key = %producer_key,
                        error = %err,
                        "failed to persist event producer checkpoint after partial failure"
                    );
                }
            }
            Err(err) => {
                warn!(
                    producer_key = %producer_key,
                    error = %err,
                    "event delivery failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_api::PlanningService;
    use baml_rt_core::{BamlRtError, bus::BusWithEffects};
    use baml_rt_llm_config::EmptySecretResolver;
    use baml_rt_provenance::SurrealStoreBuilder;
    use serde_json::json;

    use crate::{
        agent_package::BootedAgent,
        config::{ProvenanceConfig, ProvenanceConfigBuilder},
        deployment_state,
        routing::{RunnerRegistry, ScopedInternalA2aRouter, extract_internal_a2a_target},
        runner::{AgentRunner, AgentRunnerConfig},
        services::PlanningServiceImpl,
        stdio::{
            is_a2a_method, map_a2a_error, select_implicit_stdio_agent, serialize_a2a_response,
            strip_stream_suffix, unix_timestamp_secs, wrap_plaintext_message,
        },
    };

    async fn test_provenance_config() -> ProvenanceConfig {
        let config_service = Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .expect("in-memory config"),
        );
        let store = SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("isolated in-memory store");
        ProvenanceConfigBuilder::new(store, None, config_service, Arc::new(EmptySecretResolver))
            .build()
    }

    async fn test_deployment_state() -> (
        tempfile::TempDir,
        Arc<deployment_state::DeploymentStateStore>,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runner_state.db");
        let state = Arc::new(
            deployment_state::DeploymentStateStore::open(&path)
                .await
                .expect("deployment state"),
        );
        (dir, state)
    }

    async fn build_test_agent() -> baml_rt_a2a::A2aAgent {
        let manager = BamlRuntimeManager::builder()
            .build()
            .expect("create runtime manager");
        let store = SurrealStoreBuilder::in_memory()
            .build()
            .await
            .expect("in-memory store for test agent");
        let code = r#"
globalThis.onChatMessage = async function(_message) {
  __chat_yield({ message: { parts: [{ text: "ok" }] } });
  __chat_yield({ final: true });
};
"#;
        baml_rt_a2a::A2aAgent::builder()
            .with_runtime_manager(manager)
            .with_init_js(code)
            .with_effect_emitter(Arc::new(BusWithEffects::new()))
            .with_surreal_store(store)
            .build()
            .await
            .expect("build test agent")
    }

    fn make_booted(agent: baml_rt_a2a::A2aAgent, name: &str) -> BootedAgent {
        use baml_rt_core::AgentManifest;
        let manifest = AgentManifest {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            entry_point: "dist/index.js".to_string(),
            tools: vec![],
            discovery: None,
            tags: vec![],
            signature: String::new(),
        };
        BootedAgent {
            agent,
            manifest,
            baml_functions: vec![],
            provenance: crate::agent_package::DeploymentProvenance::Ephemeral,
            lifecycle: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
        }
    }

    fn make_runner(
        prov: ProvenanceConfig,
        state: Arc<deployment_state::DeploymentStateStore>,
    ) -> Arc<AgentRunner> {
        let runner = Arc::new(
            AgentRunner::new(AgentRunnerConfig {
                provenance_config: prov,
                deployment_state: state,
                access_policy: baml_rt_tools::ToolAccessPolicy::default(),
                stream_idle_secs: None,
                claude_workspaces_base: None,
                repository_url: "http://127.0.0.1:18080/repository".to_string(),
                embedded_repository: None,
                external_tools_dirs: Vec::new(),
                sandbox_bind_roots: Vec::new(),
                runtime_progress: baml_rt_api::RuntimeProgressMeter::new_without_ticker(),
            })
            .expect("test runner construction"),
        );
        runner.internal_a2a_router().set_runner(Arc::clone(&runner));
        runner
    }

    // ── stdio protocol helpers ────────────────────────────────────────────────

    #[test]
    fn test_strip_stream_suffix_removes_suffix() {
        assert_eq!(
            strip_stream_suffix("message/send/stream"),
            ("message/send".to_string(), true)
        );
        assert_eq!(
            strip_stream_suffix("method.stream"),
            ("method".to_string(), true)
        );
        assert_eq!(
            strip_stream_suffix("method:stream"),
            ("method".to_string(), true)
        );
        assert_eq!(strip_stream_suffix("method"), ("method".to_string(), false));
    }

    #[test]
    fn test_is_a2a_method() {
        assert!(is_a2a_method("message/send"));
        assert!(is_a2a_method("tasks/get"));
        assert!(!is_a2a_method("myFunction"));
    }

    #[test]
    fn test_serialize_a2a_response_valid_json() {
        let v = json!({"result": "ok"});
        let s = serialize_a2a_response(&v);
        assert!(s.contains("result"));
    }

    #[test]
    fn test_map_a2a_error_agent_not_found() {
        let v = map_a2a_error(None, BamlRtError::AgentNotFound("x".to_string()));
        assert_eq!(v["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn test_wrap_plaintext_message_valid_jsonrpc() {
        let v = wrap_plaintext_message("hello world").expect("wrap");
        assert_eq!(v["method"], "message.sendStream");
        assert!(v["params"]["message"]["parts"][0]["text"] == "hello world");
    }

    #[test]
    fn test_unix_timestamp_secs_is_numeric() {
        let ts = unix_timestamp_secs();
        ts.parse::<u64>().expect("numeric timestamp");
    }

    // ── A2A routing ───────────────────────────────────────────────────────────

    #[test]
    fn test_extract_internal_a2a_target_valid() {
        let request = json!({
            "params": {
                "metadata": {
                    "target": {
                        "agent_package": "coordinator-agent",
                        "agent_instance_id": "default"
                    }
                }
            }
        });
        let key = extract_internal_a2a_target(&request);
        assert!(key.is_some());
        let key = key.unwrap();
        assert_eq!(key.agent_package.as_str(), "coordinator-agent");
    }

    #[test]
    fn test_extract_internal_a2a_target_missing() {
        let request = json!({"params": {}});
        assert!(extract_internal_a2a_target(&request).is_none());
    }

    #[tokio::test]
    async fn test_runner_registry_list_agents_empty() {
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);
        let registry = RunnerRegistry(Arc::clone(&runner));
        use baml_rt_core::AgentLister as _;
        assert!(registry.list_agents().is_empty());
    }

    #[tokio::test]
    async fn test_internal_a2a_router_self_routing_rejected() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);
        let router = runner.internal_a2a_router().clone();
        let pkg = AgentPackageName::parse("my-agent").unwrap();
        let key = AgentRouteKey::new(pkg, AgentInstanceId::default());
        let request_val = json!({
            "jsonrpc": "2.0",
            "method": "message.sendStream",
            "id": "corr-1-1",
            "params": {
                "metadata": {
                    "target": {"agent_package": "my-agent", "agent_instance_id": "default"}
                },
                "message": {
                    "messageId": "msg-1",
                    "role": "user",
                    "parts": [{"text": "hi"}]
                }
            }
        });
        let wire = baml_rt_core::A2aWireRequest::from(request_val);
        let result = router.route_from(&key, wire).await;
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("self-routing"),
            "expected self-routing error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_scoped_internal_router_routes_to_agent() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey, collect_a2a_stream};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent.clone(), "target-agent");
        let pkg = AgentPackageName::parse("target-agent").unwrap();
        let route_key = AgentRouteKey::new(pkg, AgentInstanceId::default());
        {
            let mut routed = runner.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key.clone(), agent);
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("target-agent".to_string(), booted);
        }

        let caller_pkg = AgentPackageName::parse("caller-agent").unwrap();
        let caller_key = AgentRouteKey::new(caller_pkg, AgentInstanceId::default());
        let scoped = ScopedInternalA2aRouter::new(caller_key, runner.internal_a2a_router().clone());
        use baml_rt_a2a::A2aRequestHandler as _;
        let request_val = json!({
            "jsonrpc": "2.0", "method": "message.sendStream",
            "id": "corr-42-1",
            "params": {
                "metadata": {
                    "target": {"agent_package": "target-agent", "agent_instance_id": "default"}
                },
                "message": {
                    "messageId": "msg-1", "role": "user", "parts": [{"text": "hello"}]
                }
            }
        });
        let wire = baml_rt_core::A2aWireRequest::from(request_val);
        let stream = scoped.handle_a2a_stream(wire).await.expect("stream");
        let chunks = collect_a2a_stream(stream).await;
        assert!(!chunks.is_empty());
    }

    // ── drain gate ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_draining_agent_rejects_a2a_request() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent.clone(), "drain-test");
        let pkg = AgentPackageName::parse("drain-test").unwrap();
        let route_key = AgentRouteKey::new(pkg, AgentInstanceId::default());
        {
            let mut routed = runner.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key.clone(), agent);
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("drain-test".to_string(), booted.clone());
        }

        // Set draining — subsequent dispatch must be rejected.
        booted.set_draining();

        let request_val = json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": "corr-drain-1",
            "params": {
                "message": {
                    "messageId": "msg-1",
                    "role": "user",
                    "parts": [{"text": "hi"}]
                }
            }
        });
        let wire = baml_rt_core::A2aWireRequest::from(request_val);
        let result = runner.handle_a2a_by_key(&route_key, wire).await;
        assert!(result.is_err(), "draining agent should reject requests");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("draining"),
            "error should mention draining, got: {msg}"
        );
    }

    // ── cluster resolver fallback ────────────────────────────────────────────

    /// Mock resolver: returns a fixed result for any key.
    struct MockResolver(baml_rt_core::Result<Option<crate::routing::Placement>>);

    #[async_trait::async_trait]
    impl crate::routing::ClusterEndpointResolver for MockResolver {
        async fn resolve(
            &self,
            _key: &baml_rt_core::AgentRouteKey,
        ) -> baml_rt_core::Result<Option<crate::routing::Placement>> {
            match &self.0 {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(baml_rt_core::BamlRtError::Io(std::io::Error::other(
                    e.to_string(),
                ))),
            }
        }
    }

    #[tokio::test]
    async fn test_route_cluster_resolver_none_returns_not_found() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        // Wire a mock resolver that returns None (agent not in cluster).
        let resolver: Arc<dyn crate::routing::ClusterEndpointResolver> =
            Arc::new(MockResolver(Ok(None)));
        runner.internal_a2a_router().set_cluster(resolver);

        let caller_pkg = AgentPackageName::parse("caller-agent").unwrap();
        let caller_key = AgentRouteKey::new(caller_pkg, AgentInstanceId::default());

        let request_val = json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": "corr-cluster-1",
            "params": {
                "metadata": {
                    "target": {"agent_package": "remote-agent", "agent_instance_id": "default"}
                },
                "message": {
                    "messageId": "msg-1",
                    "role": "user",
                    "parts": [{"text": "hi"}]
                }
            }
        });
        let wire = baml_rt_core::A2aWireRequest::from(request_val);
        let result = runner
            .internal_a2a_router()
            .route_from(&caller_key, wire)
            .await;
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("not found locally or in cluster"),
            "expected 'not found locally or in cluster', got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_route_cluster_resolver_error_propagates() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        // Wire a mock resolver that returns a transient error.
        let resolver: Arc<dyn crate::routing::ClusterEndpointResolver> =
            Arc::new(MockResolver(Err(baml_rt_core::BamlRtError::Io(
                std::io::Error::other("connection refused"),
            ))));
        runner.internal_a2a_router().set_cluster(resolver);

        let caller_pkg = AgentPackageName::parse("caller-agent").unwrap();
        let caller_key = AgentRouteKey::new(caller_pkg, AgentInstanceId::default());

        let request_val = json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": "corr-cluster-2",
            "params": {
                "metadata": {
                    "target": {"agent_package": "unreachable-agent", "agent_instance_id": "default"}
                },
                "message": {
                    "messageId": "msg-1",
                    "role": "user",
                    "parts": [{"text": "hi"}]
                }
            }
        });
        let wire = baml_rt_core::A2aWireRequest::from(request_val);
        let result = runner
            .internal_a2a_router()
            .route_from(&caller_key, wire)
            .await;
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("placement lookup failed"),
            "expected 'placement lookup failed', got: {msg}"
        );
    }

    // ── deploy idempotency guard ────────────────────────────────────────────

    #[tokio::test]
    async fn test_deploy_by_hash_returns_already_deployed_for_existing() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey, DeploymentManager};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        // Manually insert a booted agent with a known content_hash to simulate
        // a prior deploy. The idempotency guard checks the in-memory map before
        // hitting the repository, so this exercises the fast-path.
        let hash: baml_rt_core::DeploymentContentHash = "a".repeat(64).parse().expect("valid hash");
        let agent = build_test_agent().await;
        let booted = crate::agent_package::BootedAgent {
            agent: agent.clone(),
            manifest: baml_rt_core::AgentManifest {
                name: "idempotent-agent".to_string(),
                version: "1.0.0".to_string(),
                entry_point: "dist/index.js".to_string(),
                tools: vec![],
                discovery: None,
                tags: vec![],
                signature: String::new(),
            },
            baml_functions: vec![],
            provenance: crate::agent_package::DeploymentProvenance::Repository {
                content_hash: hash.clone(),
                version: 1,
            },
            lifecycle: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
        };
        let pkg = AgentPackageName::parse("idempotent-agent").unwrap();
        let rk = AgentRouteKey::new(pkg, AgentInstanceId::default());
        {
            let mut agents = runner.agents.write().expect("RwLock poison");
            let mut routed = runner.routed_agents.write().expect("RwLock poison");
            routed.insert(rk, agent);
            agents.insert("idempotent-agent".to_string(), booted);
        }

        // deploy_by_hash should short-circuit with already_deployed.
        let result = runner.deploy_by_hash(&hash).await.expect("deploy");
        assert!(
            result.already_deployed,
            "deploy_by_hash must return already_deployed when agent is in memory"
        );
    }

    #[tokio::test]
    async fn restore_dedupe_removes_superseded_deployment_row() {
        use baml_rt_core::{DeploymentContentHash, DeploymentRecord, DeploymentStatus};

        let (_dir, store) = test_deployment_state().await;
        let h_old: DeploymentContentHash = "c".repeat(64).parse().expect("hash");
        let h_new: DeploymentContentHash = "d".repeat(64).parse().expect("hash");

        store
            .save_deployment(&DeploymentRecord {
                content_hash: h_old.clone(),
                agent_name: "dup-agent".into(),
                deployed_at: "100".into(),
                status: DeploymentStatus::Active,
                last_error: None,
                last_attempt_at: None,
                failure_count: 0,
            })
            .await
            .expect("save old");
        store
            .save_deployment(&DeploymentRecord {
                content_hash: h_new.clone(),
                agent_name: "dup-agent".into(),
                deployed_at: "200".into(),
                status: DeploymentStatus::Active,
                last_error: None,
                last_attempt_at: None,
                failure_count: 0,
            })
            .await
            .expect("save new");

        let raw = store.list_deployments().await.expect("list");
        assert_eq!(raw.len(), 2);

        let (winners, superseded) = crate::deployment_restore::dedupe_deployments_for_restore(raw);
        assert_eq!(winners.len(), 1);
        assert_eq!(superseded.len(), 1);
        assert_eq!(winners[0].content_hash, h_new);

        for h in &superseded {
            store.remove_deployment(h).await.expect("remove superseded");
        }
        let left = store.list_deployments().await.expect("list after purge");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content_hash, h_new);
    }

    // ── runner prepare_a2a_request ────────────────────────────────────────────

    #[tokio::test]
    async fn test_prepare_a2a_request_explicit_agent_name() {
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent, "my-agent");
        {
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("my-agent".to_string(), booted);
        }

        let mut request = json!({
            "jsonrpc": "2.0", "id": "corr-1-1", "method": "doSomething",
            "params": { "agent": "my-agent", "key": "value" }
        });
        let (agent_name, prepared) = runner.prepare_a2a_request(&mut request).unwrap();
        assert_eq!(agent_name, "my-agent");
        assert_eq!(prepared["method"], "doSomething");
        assert_eq!(prepared["params"]["key"], "value");
    }

    #[tokio::test]
    async fn test_prepare_a2a_request_method_prefix_routing() {
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent, "my-agent");
        {
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("my-agent".to_string(), booted);
        }

        let mut request = json!({
            "jsonrpc": "2.0", "id": "corr-1-2", "method": "my-agent/doSomething",
            "params": {}
        });
        let (agent_name, prepared) = runner.prepare_a2a_request(&mut request).unwrap();
        assert_eq!(agent_name, "my-agent");
        assert_eq!(prepared["method"], "doSomething");
    }

    #[tokio::test]
    async fn test_prepare_a2a_request_implicit_single_agent() {
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent, "my-agent");
        {
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("my-agent".to_string(), booted);
        }

        let mut request = json!({
            "jsonrpc": "2.0", "id": "corr-1-3", "method": "doSomething", "params": {}
        });
        let (agent_name, _) = runner.prepare_a2a_request(&mut request).unwrap();
        assert_eq!(agent_name, "my-agent");
    }

    // ── HTTP run_a2a_loop ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_a2a_loop_handles_valid_request() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent.clone(), "my-agent");
        let pkg = AgentPackageName::parse("my-agent").unwrap();
        let route_key = AgentRouteKey::new(pkg, AgentInstanceId::default());
        {
            let mut routed = runner.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key.clone(), agent);
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("my-agent".to_string(), booted);
        }

        let request = json!({
            "jsonrpc": "2.0", "id": "corr-test-1", "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-test-1", "role": "user",
                    "parts": [{"text": "hello"}],
                    "metadata": { "agent": "my-agent" }
                }
            }
        });
        let input = format!("{}\n", serde_json::to_string(&request).unwrap());
        let reader = tokio::io::BufReader::new(input.as_bytes());
        let mut output = Vec::new();
        runner.run_a2a_loop(reader, &mut output).await.unwrap();
        let output_str = String::from_utf8(output).unwrap();
        assert!(
            !output_str.is_empty(),
            "Expected at least one response line"
        );
        for line in output_str.lines() {
            if !line.trim().is_empty() {
                let parsed: serde_json::Value =
                    serde_json::from_str(line).expect("Response is valid JSON");
                assert!(parsed["result"].is_object() || parsed["error"].is_object());
            }
        }
    }

    #[tokio::test]
    async fn test_run_a2a_loop_plaintext_wraps_message() {
        let prov = test_provenance_config().await;
        let (_dir, state) = test_deployment_state().await;
        let runner = make_runner(prov, state);

        let agent = build_test_agent().await;
        let booted = make_booted(agent.clone(), "my-agent");
        {
            let mut agents = runner.agents.write().expect("RwLock poison");
            agents.insert("my-agent".to_string(), booted);
            use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
            let pkg = AgentPackageName::parse("my-agent").unwrap();
            let key = AgentRouteKey::new(pkg, AgentInstanceId::default());
            let mut routed = runner.routed_agents.write().expect("RwLock poison");
            routed.insert(key, agent);
        }

        let input = "hello world\n";
        let reader = tokio::io::BufReader::new(input.as_bytes());
        let mut output = Vec::new();
        runner.run_a2a_loop(reader, &mut output).await.unwrap();
        let output_str = String::from_utf8(output).unwrap();
        assert!(
            !output_str.is_empty(),
            "Expected at least one response line"
        );
    }

    // ── select_implicit_stdio_agent ───────────────────────────────────────────

    #[tokio::test]
    async fn test_select_implicit_stdio_agent_coordinator_preferred() {
        use baml_rt_core::AgentManifest;
        let agent1 = build_test_agent().await;
        let agent2 = build_test_agent().await;
        let make = |agent: baml_rt_a2a::A2aAgent, name: &str| BootedAgent {
            agent,
            manifest: AgentManifest {
                name: name.to_string(),
                version: "1".to_string(),
                entry_point: "dist/index.js".to_string(),
                tools: vec![],
                discovery: None,
                tags: vec![],
                signature: String::new(),
            },
            baml_functions: vec![],
            provenance: crate::agent_package::DeploymentProvenance::Ephemeral,
            lifecycle: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
        };
        let mut agents = std::collections::HashMap::new();
        agents.insert("other-agent".to_string(), make(agent1, "other-agent"));
        agents.insert(
            "coordinator-agent".to_string(),
            make(agent2, "coordinator-agent"),
        );
        assert_eq!(
            select_implicit_stdio_agent(&agents),
            Some("coordinator-agent".to_string())
        );
    }

    // ── planning service ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_planning_service_empty_snapshot_for_unknown_context() {
        let prov = test_provenance_config().await;
        let svc = PlanningServiceImpl::new(prov.store().clone());
        let result = svc
            .planning_for_context("ctx-nonexistent-000")
            .await
            .expect("planning ok");
        assert!(result.tasks.is_empty());
        assert!(result.all_task_ids.is_empty());
        assert_eq!(result.context_id, "ctx-nonexistent-000");
    }

    #[tokio::test]
    async fn test_planning_service_all_task_ids_includes_tasks_without_planning() {
        use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
        use baml_rt_provenance::{ProvEvent, ProvenanceWriter};

        let prov = test_provenance_config().await;
        let store = prov.store().clone();
        let writer = store.clone() as Arc<dyn ProvenanceWriter>;

        let context_id = ContextId::new(42, 2);
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

        let task_with_planning =
            TaskId::from_external(ExternalId::new("planning-all-tasks-a".to_string()));
        let task_plain = TaskId::from_external(ExternalId::new("planning-all-tasks-b".to_string()));

        writer
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_with_planning.clone(),
                MessageId::from_external(ExternalId::new("msg-all-tasks-1".to_string())),
                "user".to_string(),
                vec!["first turn".to_string()],
                None,
                agent_id.clone(),
                100,
            ))
            .await
            .expect("message 1");

        writer
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_plain.clone(),
                MessageId::from_external(ExternalId::new("msg-all-tasks-2".to_string())),
                "user".to_string(),
                vec!["second turn".to_string()],
                None,
                agent_id.clone(),
                200,
            ))
            .await
            .expect("message 2");

        writer
            .add_event(ProvEvent::intent_resolved(
                context_id.clone(),
                task_with_planning.clone(),
                "intent-all-tasks-test",
                "Do something".to_string(),
                vec![],
                None,
                None,
            ))
            .await
            .expect("intent");

        let svc = PlanningServiceImpl::new(store);
        let result = svc
            .planning_for_context(context_id.as_str())
            .await
            .expect("planning ok");

        assert_eq!(result.all_task_ids.len(), 2);
        assert!(
            result
                .all_task_ids
                .contains(&task_with_planning.to_string())
                && result.all_task_ids.contains(&task_plain.to_string()),
            "all_task_ids={:?}",
            result.all_task_ids
        );
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].task_id, task_with_planning.to_string());
    }

    // TODO: update to current EffectEvent API (bus shapes changed after this test was written)
    #[allow(dead_code)]
    #[cfg(any())]
    #[tokio::test]
    async fn test_planning_service_returns_tasks_with_provenance_data() {
        use baml_rt_core::{
            Outcome,
            bus::{BusWithEffects, EffectEmitter},
            ids::{ContextId, ExternalId, TaskId},
        };
        use baml_rt_provenance::ProvenanceWriter;
        let prov = test_provenance_config().await;
        let store = prov.store().clone();
        let context_id = ContextId::new(42, 1);
        let message_id = MessageId::from_external(ExternalId::new("msg-test-001".to_string()));
        let task_id = TaskId::from_external(ExternalId::new(format!(
            "live-task:{}:{}",
            context_id.as_str(),
            message_id.as_str()
        )));

        let agent_id = baml_rt_core::ids::AgentId::from_uuid(
            baml_rt_core::ids::UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
        );

        // Write a message received event to create a provenance trail.
        let writer = store.clone() as Arc<dyn ProvenanceWriter>;
        let event = baml_rt_provenance::ProvEvent::message_received_task(
            context_id.clone(),
            message_id.clone(),
            task_id.clone(),
            "ROLE_USER".to_string(),
            vec!["What is 2+2?".to_string()],
            None,
            agent_id,
            baml_rt_provenance::events::allocate_activity_anchor(),
        );
        writer.add_event(event).await.expect("write event");

        // Write intent via provenance event directly (bypasses bus effect system,
        // which has a different API shape to what was originally written here).
        let intent_id = IntentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        let bus = Arc::new(BusWithEffects::new());
        bus.emit(baml_rt_core::bus::EffectEvent::IntentResolved {
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            intent_id: intent_id.clone(),
            description: "Compute the answer".to_string(),
            citations: vec![],
            supersession: None,
            epoch: None,
        })
        .await
        .unwrap();

        let plan_id = PlanId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        bus.emit(baml_rt_core::bus::EffectEvent::PlanGenerated {
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            plan_id: plan_id.clone(),
            intent_id: intent_id.clone(),
            steps: serde_json::json!([{"step_id": "s1", "description": "Add 2 and 2", "status": "pending"}]),
            supersession: None,
            epoch: None,
        })
        .await
        .unwrap();

        let svc = PlanningServiceImpl::new(store);
        let result = svc.planning_for_context(context_id.as_str()).await;
        assert!(result.is_ok(), "Expected OK, got: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response.context_id, context_id.as_str());
        assert!(!response.tasks.is_empty(), "Expected non-empty tasks");
    }

    // ── surreal provenance ────────────────────────────────────────────────────

    // TODO: update to current EffectEvent API (bus shapes changed after this test was written)
    #[cfg(any())]
    #[tokio::test]
    async fn surreal_runtime_store_insert_message_records_provenance_message_event() {
        use baml_rt_core::{
            bus::EffectEmitter,
            ids::{ContextId, ExternalId, MessageId, TaskId},
        };
        use baml_rt_provenance::{ProvenanceOpsFilters, ProvenanceOpsQuery};

        let provenance = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("in-memory provenance store");
        let bus = Arc::new(baml_rt_core::bus::BusWithEffects::new());
        let prov_subscriber =
            baml_rt_provenance::ProvenanceBusSubscriber::new(Arc::new(provenance.clone()));
        bus.subscribe(Arc::new(prov_subscriber)).await;

        let context_id = ContextId::new(99, 1);
        let message_id = MessageId::from_external(ExternalId::new("msg-1".to_string()));
        let task_id = TaskId::from_external(ExternalId::new("task-1".to_string()));
        let agent_id = baml_rt_core::ids::AgentId::from_uuid(UuidId::new_v4());

        bus.emit(baml_rt_core::bus::EffectEvent::A2aStarted {
            context_id: context_id.clone(),
            metadata: baml_rt_core::bus::A2aEffectMetadata {
                agent_id: agent_id.clone(),
                method: "message.sendStream".to_string(),
                request_id: Some(message_id.as_str().to_string()),
                liveness_role: baml_rt_core::bus::A2aLivenessRole::Command,
                metadata: serde_json::json!({}),
            },
        })
        .await
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let query_result = provenance
            .query_ops(baml_rt_provenance::ProvenanceOpsQueryRequest {
                resource: baml_rt_provenance::ProvenanceOpsResource::Messages,
                filters: ProvenanceOpsFilters {
                    context_id: Some(context_id.clone()),
                    ..Default::default()
                },
                page_size: Some(50),
                ..Default::default()
            })
            .await
            .expect("query succeeded");
        assert!(
            !query_result.rows.is_empty(),
            "Expected at least one message row"
        );
    }

    #[test]
    fn cluster_endpoint_rejects_wildcard() {
        let result = super::validate_cluster_endpoint("http://0.0.0.0:18080");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("0.0.0.0"),
            "error should mention the address: {msg}"
        );
    }

    #[test]
    fn cluster_endpoint_rejects_loopback_ip() {
        let result = super::validate_cluster_endpoint("http://127.0.0.1:18080");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("loopback"),
            "error should mention loopback: {msg}"
        );
    }

    #[test]
    fn cluster_endpoint_rejects_localhost() {
        let result = super::validate_cluster_endpoint("http://localhost:18080");
        assert!(result.is_err());
    }

    #[test]
    fn cluster_endpoint_accepts_pod_dns() {
        let result = super::validate_cluster_endpoint("http://runner-0.runner.agentium.svc:18080");
        assert!(result.is_ok());
    }

    #[test]
    fn cluster_endpoint_accepts_cluster_ip() {
        let result = super::validate_cluster_endpoint("http://10.43.0.5:18080");
        assert!(result.is_ok());
    }

    #[test]
    fn redact_endpoint_credentials_strips_userinfo() {
        let redacted =
            super::redact_endpoint_credentials("ws://root:hunter2@surreal.svc:8000/path");
        assert!(
            !redacted.contains("hunter2") && !redacted.contains("root"),
            "redacted endpoint must not echo username or password ({redacted})"
        );
        assert!(
            redacted.contains("surreal.svc:8000"),
            "redacted endpoint must preserve host:port for triage ({redacted})"
        );
    }

    #[test]
    fn redact_endpoint_credentials_falls_back_for_invalid_url() {
        let redacted = super::redact_endpoint_credentials("not a url");
        assert_eq!(redacted, "<invalid URL>");
    }

    /// Connecting to a closed port should still return Err after exhausting
    /// retries — operators rely on a definite exit when SurrealDB is genuinely
    /// unavailable rather than an indefinite hang.
    #[tokio::test]
    async fn remote_config_store_retry_exhausts_on_closed_port() {
        let start = std::time::Instant::now();
        let result = super::connect_remote_config_store_with_retry(
            "ws://127.0.0.1:1",
            None,
            std::num::NonZeroUsize::new(3).unwrap(),
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(40),
        )
        .await;
        assert!(
            result.is_err(),
            "retry against closed port must surface Err once attempts are exhausted"
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "retry helper must sleep between attempts (elapsed: {elapsed:?})"
        );
    }
}
