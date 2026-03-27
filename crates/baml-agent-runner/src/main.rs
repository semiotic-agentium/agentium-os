//! BAML Agent Runner
//!
//! Starts the runner, restores previously-deployed agents from --state-dir,
//! and serves HTTP / stdio / event poll loop.

#![recursion_limit = "256"]

mod agent_package;
mod builder;
mod config;
mod deployment_state;
mod package;
mod routing;
mod runner;
mod services;
mod stdio;

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use async_trait::async_trait;
use baml_rt_core::{DeploymentManager, DeploymentStatus};
use baml_rt_llm_config::{
    FnoxFileSecretResolver, OverlaySecretResolver, SECRET_LINKS_CONFIG_KEY, SecretLinksState,
    apply_secret_links_state,
};
use baml_rt_observability::tracing_setup;
use baml_rt_provenance::ToolIndexConfig;
use baml_rt_repository::{
    BlobStore, LineageStore, MetadataStore, RepositoryService, SearchStore, SurrealStore,
};
use baml_rt_tools::{
    InventoryCatalog, ProducerCheckpoint, load_configured_event_producers_with_checkpoints,
    parse_access_allowlist,
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
use baml_tools_system::callback_delivery_gate::{
    CallbackDeliveryGate, install_callback_delivery_gate,
};
use clap::Parser;
use config::{Cli, ProvenanceDb, provenance_config_builder};
use serde_json::Value;
use services::{
    ContextMetricsServiceImpl, EpisodeServiceImpl, MermaidServiceImpl, PlanningServiceImpl,
    ProvenanceOpsServiceImpl,
};
use stdio::unix_timestamp_secs;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_setup::init_tracing();
    match dotenvy::dotenv() {
        Ok(path) => tracing::debug!(path = ?path, "Loaded .env"),
        Err(err) => tracing::debug!(error = ?err, "No .env loaded"),
    }

    info!("BAML Agent Runner starting");

    let config = Cli::parse()
        .into_config()
        .context("Failed to parse arguments")?;

    if let Some(ref base) = config.claude_workspaces_base {
        let absolute = if base.is_absolute() {
            base.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "current_dir failed, using .");
                    PathBuf::from(".")
                })
                .join(base)
        };
        if let Err(e) = std::fs::create_dir_all(&absolute) {
            eprintln!(
                "Error: Cannot create Claude workspaces base {}: {}",
                absolute.display(),
                e
            );
            std::process::exit(1);
        }
        let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|e| {
            tracing::warn!(path = %absolute.display(), error = %e, "canonicalize failed");
            absolute
        });
        // SAFETY: single-threaded at this point; no other thread reads this var.
        unsafe {
            std::env::set_var(
                "BAML_CLAUDE_WORKSPACES_BASE",
                canonical.to_string_lossy().to_string(),
            );
        }
        info!(base = %canonical.display(), "Claude workspaces base set from --claude-workspaces-base");
    }

    match &config.provenance_db {
        ProvenanceDb::InMemory => info!(
            "Provenance backend: in-memory (:memory:). External graph_exporter cannot read this."
        ),
        ProvenanceDb::File(path) => {
            info!(path = %path.display(), "Provenance backend: SurrealKV directory")
        }
    }

    let config_service: Arc<dyn baml_rt_config::ConfigService> = match &config.provenance_db {
        ProvenanceDb::InMemory => Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .context("Failed to create in-memory config store")?,
        ),
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

    let provenance_config = provenance_config_builder(&config.provenance_db)
        .await
        .context("Failed to initialize provenance storage")?
        .with_config_service(config_service)
        .with_llm_secret_resolver(overlay.clone())
        .with_runtime_secret_store(Some(overlay))
        .build()
        .context("Failed to build provenance config")?;

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
    let repository_store = Arc::new(SurrealStore::open(&repository_db_path).await.with_context(
        || {
            format!(
                "Failed to initialize repository DB at {}",
                repository_db_path.display()
            )
        },
    )?);
    baml_tools_system::callback_store::install_callback_store(
        deployment_state.clone() as Arc<dyn baml_tools_system::callback_store::CallbackStore>,
    );
    let repository_service = Arc::new(RepositoryService::new(
        repository_store.clone() as Arc<dyn BlobStore>,
        repository_store.clone() as Arc<dyn MetadataStore>,
        repository_store.clone() as Arc<dyn LineageStore>,
        repository_store as Arc<dyn SearchStore>,
    ));
    let existing_deployments = deployment_state
        .list_deployments()
        .await
        .context("Failed to read runner deployment state records")?;
    info!(
        state_dir = %config.state_dir.display(),
        state_db = %state_db_path.display(),
        repository_dir = %config.repository_dir.display(),
        repository_db = %repository_db_path.display(),
        existing_deployments = existing_deployments.len(),
        "Runner deployment + repository backends initialized"
    );

    let access_allowlist = parse_access_allowlist();
    let tool_index = match &config.provenance_db {
        ProvenanceDb::InMemory => Some(ToolIndexConfig::in_memory()),
        ProvenanceDb::File(path) => Some(ToolIndexConfig::new(path)),
    };
    let builder = builder::RunnerBuilder::<builder::Loading>::new(
        provenance_config,
        deployment_state,
        tool_index,
        access_allowlist,
        config.stream_idle_secs,
        config.repository_url.clone(),
    );

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
                warn!(
                    error = %err,
                    content_hash = %deployment.content_hash.as_str(),
                    "Failed to restore deployment; continuing startup"
                );
            }
        }
    }

    let ready = builder.build();
    install_callback_delivery_gate(Arc::new(RunnerCallbackDeliveryGate {
        runner: ready.runner(),
    }));

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

    // --- Event producer poll loop ---
    let dispatcher_handle = if let Some(interval) = config.event_poll_interval {
        let registry = ready.registry();
        let runner = ready.runner();
        let deployment_state_for_poll = runner.deployment_state().clone();
        let mut dispatcher =
            baml_rt_a2a::EventDispatcher::new(registry as Arc<dyn baml_rt_a2a::AgentRegistry>);
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
    };

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
        let web_dir = config.web_dir.clone();
        info!(
            bind = %bind,
            web_dir = ?web_dir,
            "A2A server mode: exposing HTTP API (GET /agents, POST /agents/.../a2a/sse, GET /config, GET /contexts/.../mermaid, GET /tasks/.../mermaid, GET /contexts/.../metrics, GET /provenance/..., GET /openapi.json)"
        );
        let runtime_secret_store = prov_config.runtime_secret_store();
        Some(tokio::spawn(async move {
            baml_rt_api::serve_with_services_and_deploy(
                registry_impl,
                &bind,
                mermaid,
                context_metrics,
                provenance_ops,
                planning,
                episode,
                Some(runner.clone() as Arc<dyn DeploymentManager>),
                Some(config.repository_url.clone()),
                Some(repository_service.clone()),
                tool_catalog,
                config_service,
                secret_resolver,
                runtime_secret_store,
                web_dir.as_deref(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("HTTP API server: {e}"))
        }))
    } else {
        None
    };

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
            handle.await??;
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

struct RunnerCallbackDeliveryGate {
    runner: Arc<crate::runner::AgentRunner>,
}

#[async_trait]
impl CallbackDeliveryGate for RunnerCallbackDeliveryGate {
    async fn can_emit_callback(
        &self,
        callback: &baml_tools_system::callback_store::StoredCallback,
    ) -> baml_rt_core::Result<bool> {
        let Some(requesting_agent_id) = callback.requesting_agent_id.as_deref() else {
            return Ok(true);
        };
        let (Some(context_id), Some(task_id)) = (&callback.context_id, &callback.task_id) else {
            return Ok(true);
        };
        Ok(!self
            .runner
            .requesting_task_still_in_flight(requesting_agent_id, context_id, task_id)
            .await)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_a2a::{A2aAgent, A2aRequestHandler as _, AgentRegistry as _};
    use baml_rt_api::PlanningService;
    use baml_rt_core::{
        AgentInstanceId, AgentLister as _, AgentPackageName, AgentRouteKey, BamlRtError,
        DeploymentManager as _, Result,
        bus::{BusWithEffects, EffectEmitter as _, Subscriber},
        ids::{ContextId, ExternalId, IntentId, MessageId, PlanId, TaskId, UuidId},
        route_key_from_request,
    };
    use baml_rt_llm_config::EmptySecretResolver;
    use baml_rt_provenance::{ProvenanceOpsFilters, ProvenanceOpsQuery as _, SurrealStoreBuilder};
    use serde_json::json;

    use crate::{
        agent_package::{BootedAgent, SnapshotAgentLister},
        config::{ProvenanceConfig, ProvenanceDb, provenance_config_builder},
        deployment_state,
        routing::{
            InternalA2aRouter, RunnerRegistry, ScopedInternalA2aRouter,
            extract_internal_a2a_target, scope_from_request,
        },
        runner::AgentRunner,
        services::PlanningServiceImpl,
        stdio::{
            MESSAGE_COUNTER, is_a2a_method, map_a2a_error, select_implicit_stdio_agent,
            serialize_a2a_response, split_agent_method, strip_stream_suffix, unix_timestamp_secs,
            wrap_plaintext_message,
        },
    };

    async fn test_provenance_config() -> ProvenanceConfig {
        let config_service = Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .expect("in-memory config"),
        );
        provenance_config_builder(&ProvenanceDb::InMemory)
            .await
            .expect("provenance builder")
            .with_config_service(config_service)
            .with_llm_secret_resolver(Arc::new(EmptySecretResolver))
            .build()
            .expect("provenance config")
    }

    async fn test_deployment_state() -> Arc<deployment_state::DeploymentStateStore> {
        Arc::new(
            deployment_state::DeploymentStateStore::open_in_memory()
                .await
                .expect("in-memory deployment state"),
        )
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
            content_hash: None,
            repository_version: None,
        }
    }

    fn make_runner(
        prov: ProvenanceConfig,
        state: Arc<deployment_state::DeploymentStateStore>,
    ) -> Arc<AgentRunner> {
        let runner = Arc::new(AgentRunner::new(
            prov,
            state,
            None,
            baml_rt_tools::ToolAccessPolicy::default(),
            None,
            "http://127.0.0.1:8080/repository".to_string(),
        ));
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
        let state = test_deployment_state().await;
        let runner = make_runner(prov, state);
        let registry = RunnerRegistry(Arc::clone(&runner));
        use baml_rt_core::AgentLister as _;
        assert!(registry.list_agents().is_empty());
    }

    #[tokio::test]
    async fn test_internal_a2a_router_self_routing_rejected() {
        use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};
        let prov = test_provenance_config().await;
        let state = test_deployment_state().await;
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
        let state = test_deployment_state().await;
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

    // ── runner prepare_a2a_request ────────────────────────────────────────────

    #[tokio::test]
    async fn test_prepare_a2a_request_explicit_agent_name() {
        let prov = test_provenance_config().await;
        let state = test_deployment_state().await;
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
        let state = test_deployment_state().await;
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
        let state = test_deployment_state().await;
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
        let state = test_deployment_state().await;
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
        let state = test_deployment_state().await;
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
            content_hash: None,
            repository_version: None,
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
    async fn test_planning_service_not_found_for_empty_context() {
        let prov = test_provenance_config().await;
        let svc = PlanningServiceImpl::new(prov.store().clone());
        let result = svc.planning_for_context("ctx-nonexistent-000").await;
        assert!(
            matches!(result, Err(baml_rt_api::PlanningError::NotFound)),
            "Expected NotFound for empty context, got: {:?}",
            result
        );
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
}
