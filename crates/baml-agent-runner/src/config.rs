//! CLI definition, runner configuration, and provenance store wiring.

use std::{path::PathBuf, sync::Arc};

use baml_rt_core::{BamlRtError, Result};
use baml_rt_provenance::{RemoteConfig, RemoteCredentials, SurrealStoreBuilder};
use clap::Parser;

/// Provenance store backend selection.
#[derive(Debug, Clone)]
pub(crate) enum ProvenanceDb {
    InMemory,
    File(PathBuf),
    /// Remote SurrealDB server (WebSocket endpoint).
    Remote {
        endpoint: String,
        username: Option<String>,
        password: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerConfig {
    pub(crate) repository_url: String,
    pub(crate) repository_dir: PathBuf,
    pub(crate) invoke: Option<(String, String, String)>,
    pub(crate) a2a_stdio: bool,
    pub(crate) serve_http: Option<String>,
    pub(crate) web_dir: Option<PathBuf>,
    pub(crate) provenance_db: ProvenanceDb,
    pub(crate) state_dir: PathBuf,
    /// Resolved canonical path for Claude workspaces root, if provided via CLI.
    pub(crate) claude_workspaces_base: Option<PathBuf>,
    /// Stream collector idle timeout in seconds.
    pub(crate) stream_idle_secs: Option<u64>,
    /// Event producer poll interval. `None` disables the poll loop.
    pub(crate) event_poll_interval: Option<std::time::Duration>,
    /// Shared secret for authenticating control-plane requests.
    pub(crate) runner_token: Option<String>,
    /// Override the endpoint URL this runner registers in the cluster.
    pub(crate) runner_endpoint: Option<String>,
    /// Placement TTL in milliseconds: placements on runners whose last heartbeat
    /// is older than this are excluded from resolution.
    pub(crate) placement_ttl_ms: u64,
}

#[derive(Debug, Parser)]
#[command(name = "baml-agent-runner")]
#[command(
    about = "Run the BAML agent server. Agents are deployed exclusively via the repository API.",
    long_about = "Starts the runner and restores any previously-deployed agents from --state-dir.\n\
                  \nTo add agents, use `baml-agent-builder publish` or POST /deploy.\n\
                  Positional package paths are no longer accepted — all deployment goes through the repository."
)]
pub(crate) struct Cli {
    /// Repository base URL used for hash-based deploy/restore (e.g. http://127.0.0.1:18080/repository).
    #[arg(
        long,
        value_name = "URL",
        default_value = "http://127.0.0.1:18080/repository"
    )]
    pub(crate) repository_url: String,

    /// Local repository data directory (embedded SurrealKV backing /repository routes).
    #[arg(long, value_name = "DIR", default_value = "./.repository")]
    pub(crate) repository_dir: PathBuf,

    /// Invoke a JS function: <agent> <function> <json-args>
    #[arg(long, num_args = 3, value_names = ["AGENT", "FUNCTION", "JSON_ARGS"])]
    pub(crate) invoke: Option<Vec<String>>,

    /// Run an A2A JSON-RPC loop over stdio.
    #[arg(long)]
    pub(crate) a2a_stdio: bool,

    /// Bind HTTP API (discovery + A2A routing) on the given address (e.g. 127.0.0.1:18080).
    #[arg(long, value_name = "ADDR")]
    pub(crate) serve_http: Option<String>,

    /// Directory containing built web UI assets (e.g. web/dist).
    #[arg(long, value_name = "DIR")]
    pub(crate) web_dir: Option<PathBuf>,

    /// Provenance storage path: `:memory:` or a directory for embedded SurrealKV.
    #[arg(long, value_name = "PATH", default_value = ":memory:")]
    pub(crate) provenance_db: String,

    /// Runner-local deployment state directory (embedded SurrealKV for deployment metadata/state).
    #[arg(long, value_name = "DIR", default_value = "./.runner-state")]
    pub(crate) state_dir: PathBuf,

    /// Claude workspaces root directory. When set, overrides BAML_CLAUDE_WORKSPACES_BASE.
    #[arg(long, value_name = "DIR")]
    pub(crate) claude_workspaces_base: Option<PathBuf>,

    /// Stream collector idle timeout (seconds). Default 900.
    #[arg(long, value_name = "SECS", default_value = "900")]
    pub(crate) stream_idle_secs: u64,

    /// Event producer poll interval (seconds). 0 disables (default).
    #[arg(long, value_name = "SECS", default_value = "0")]
    pub(crate) event_poll_interval_secs: u64,

    /// Remote SurrealDB endpoint (e.g. ws://surrealdb:8000). Overrides --provenance-db.
    #[arg(long, value_name = "URL")]
    pub(crate) surreal_endpoint: Option<String>,

    /// SurrealDB authentication username (for remote mode).
    #[arg(long, value_name = "USER", env = "SURREAL_USER")]
    pub(crate) surreal_username: Option<String>,

    /// SurrealDB authentication password (for remote mode).
    #[arg(long, value_name = "PASS", env = "SURREAL_PASS")]
    pub(crate) surreal_password: Option<String>,

    /// Shared secret for authenticating control-plane requests (e.g. /control/migrate).
    #[arg(long, value_name = "TOKEN", env = "RUNNER_TOKEN")]
    pub(crate) runner_token: Option<String>,

    /// Override the endpoint URL this runner registers in the cluster (e.g. https://runner-0:18080).
    /// Defaults to http://{serve-http address}.
    #[arg(long, value_name = "URL", env = "RUNNER_ENDPOINT")]
    pub(crate) runner_endpoint: Option<String>,

    /// Placement TTL in milliseconds. Placements on runners whose last heartbeat
    /// is older than this are excluded from resolution (default: 90000 = 90s).
    #[arg(
        long,
        value_name = "MS",
        default_value = "90000",
        env = "PLACEMENT_TTL_MS"
    )]
    pub(crate) placement_ttl_ms: u64,
}

impl Cli {
    pub(crate) fn into_config(
        self,
        claude_workspaces_base: Option<PathBuf>,
    ) -> anyhow::Result<RunnerConfig> {
        let invoke = self
            .invoke
            .map(|values| (values[0].clone(), values[1].clone(), values[2].clone()));

        let provenance_db = if let Some(endpoint) = self.surreal_endpoint {
            ProvenanceDb::Remote {
                endpoint,
                username: self.surreal_username,
                password: self.surreal_password,
            }
        } else if self.provenance_db == ":memory:" {
            ProvenanceDb::InMemory
        } else {
            ProvenanceDb::File(PathBuf::from(self.provenance_db))
        };

        Ok(RunnerConfig {
            repository_url: self.repository_url,
            repository_dir: self.repository_dir,
            invoke,
            a2a_stdio: self.a2a_stdio,
            serve_http: self.serve_http,
            web_dir: self.web_dir,
            provenance_db,
            state_dir: self.state_dir,
            claude_workspaces_base,
            stream_idle_secs: Some(self.stream_idle_secs),
            event_poll_interval: if self.event_poll_interval_secs > 0 {
                Some(std::time::Duration::from_secs(
                    self.event_poll_interval_secs,
                ))
            } else {
                None
            },
            runner_token: self
                .runner_token
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty()),
            runner_endpoint: self.runner_endpoint,
            placement_ttl_ms: self.placement_ttl_ms,
        })
    }
}

/// Provenance configuration: SurrealDB store with required config and secret services.
pub(crate) enum ProvenanceConfig {
    Surreal {
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
        config_service: Arc<dyn baml_rt_config::ConfigService>,
        llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
        runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
    },
}

impl ProvenanceConfig {
    pub(crate) fn store(&self) -> &Arc<baml_rt_provenance::SurrealProvenanceStore> {
        let ProvenanceConfig::Surreal { store, .. } = self;
        store
    }

    pub(crate) fn mermaid_cache(&self) -> Option<Arc<baml_rt_provenance::MermaidCache>> {
        let ProvenanceConfig::Surreal { mermaid_cache, .. } = self;
        mermaid_cache.clone()
    }

    pub(crate) fn config_service(&self) -> Arc<dyn baml_rt_config::ConfigService> {
        let ProvenanceConfig::Surreal { config_service, .. } = self;
        config_service.clone()
    }

    pub(crate) fn llm_secret_resolver(&self) -> Arc<dyn baml_rt_llm_config::SecretResolver> {
        let ProvenanceConfig::Surreal {
            llm_secret_resolver,
            ..
        } = self;
        llm_secret_resolver.clone()
    }

    pub(crate) fn runtime_secret_store(
        &self,
    ) -> Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>> {
        let ProvenanceConfig::Surreal {
            runtime_secret_store,
            ..
        } = self;
        runtime_secret_store.clone()
    }
}

/// Linear builder for provenance config.
pub(crate) struct ProvenanceConfigBuilder {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    config_service: Arc<dyn baml_rt_config::ConfigService>,
    llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
    runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
}

impl ProvenanceConfigBuilder {
    pub(crate) fn new(
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
        config_service: Arc<dyn baml_rt_config::ConfigService>,
        llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
    ) -> Self {
        Self {
            store,
            mermaid_cache,
            config_service,
            llm_secret_resolver,
            runtime_secret_store: None,
        }
    }

    pub(crate) fn with_runtime_secret_store(
        mut self,
        runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
    ) -> Self {
        self.runtime_secret_store = runtime_secret_store;
        self
    }

    pub(crate) fn build(self) -> ProvenanceConfig {
        ProvenanceConfig::Surreal {
            store: self.store,
            mermaid_cache: self.mermaid_cache,
            config_service: self.config_service,
            llm_secret_resolver: self.llm_secret_resolver,
            runtime_secret_store: self.runtime_secret_store,
        }
    }
}

/// Build the store and a linear builder for provenance config.
pub(crate) async fn provenance_config_builder(
    db: &ProvenanceDb,
    config_service: Arc<dyn baml_rt_config::ConfigService>,
    llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
) -> Result<ProvenanceConfigBuilder> {
    match db {
        ProvenanceDb::InMemory => {
            let store = SurrealStoreBuilder::in_memory()
                .build()
                .await
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!(
                        "Provenance in-memory store failed to build: {e}",
                    ))
                })?;
            Ok(ProvenanceConfigBuilder::new(
                store,
                None,
                config_service,
                llm_secret_resolver,
            ))
        }
        ProvenanceDb::File(path) => {
            let cache = baml_rt_provenance::MermaidCache::new();
            let store = SurrealStoreBuilder::file(path)
                .with_mermaid_cache(cache.clone())
                .build()
                .await
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!(
                        "Provenance file store failed to build at {}: {:#}",
                        path.display(),
                        anyhow::Error::from(e),
                    ))
                })?;
            Ok(ProvenanceConfigBuilder::new(
                store,
                Some(cache),
                config_service,
                llm_secret_resolver,
            ))
        }
        ProvenanceDb::Remote {
            endpoint,
            username,
            password,
        } => {
            let credentials = match (username, password) {
                (Some(u), Some(p)) => Some(RemoteCredentials {
                    username: u.clone(),
                    password: p.clone(),
                }),
                (None, None) => None,
                (Some(_), None) => {
                    return Err(BamlRtError::InvalidArgument(
                        "partial SurrealDB credentials: --surreal-password is required when --surreal-username is set".into(),
                    ));
                }
                (None, Some(_)) => {
                    return Err(BamlRtError::InvalidArgument(
                        "partial SurrealDB credentials: --surreal-username is required when --surreal-password is set".into(),
                    ));
                }
            };
            let cache = baml_rt_provenance::MermaidCache::new();
            let backend = baml_rt_provenance::SurrealBackend::Remote(RemoteConfig {
                endpoint: endpoint.clone(),
                namespace: "provenance".to_string(),
                database: "store".to_string(),
                credentials,
            });
            let store = SurrealStoreBuilder::backend(backend)
                .with_mermaid_cache(cache.clone())
                .build()
                .await
                .map_err(|e| {
                    // Strip credentials from endpoint URL before including in error text.
                    let safe_endpoint = url::Url::parse(endpoint)
                        .map(|mut u| {
                            let _ = u.set_username("");
                            let _ = u.set_password(None);
                            u.to_string()
                        })
                        .unwrap_or_else(|_| "<invalid URL>".to_string());
                    BamlRtError::InvalidArgument(format!(
                        "Provenance remote store failed to connect to {safe_endpoint}: {:#}",
                        anyhow::Error::from(e),
                    ))
                })?;
            Ok(ProvenanceConfigBuilder::new(
                store,
                Some(cache),
                config_service,
                llm_secret_resolver,
            ))
        }
    }
}
