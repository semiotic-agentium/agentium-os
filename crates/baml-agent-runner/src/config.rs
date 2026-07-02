// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! CLI definition, runner configuration, and provenance store wiring.

use std::{path::PathBuf, sync::Arc};

use baml_rt_core::{BamlRtError, Result};
use baml_rt_provenance::{RemoteConfig, RemoteCredentials, SurrealStoreBuilder};
use clap::Parser;
use tracing::info;

use crate::runner_config_file::{self, DatasourceActivation, FileConfig};

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

/// SurrealDB authentication is all-or-nothing: a username without a password
/// (or vice versa) is always a misconfiguration.
pub(crate) fn parse_surreal_credentials<'a>(
    username: Option<&'a str>,
    password: Option<&'a str>,
) -> anyhow::Result<Option<(&'a str, &'a str)>> {
    match (username, password) {
        (Some(u), Some(p)) => Ok(Some((u, p))),
        (None, None) => Ok(None),
        _ => anyhow::bail!(
            "partial SurrealDB credentials: both --surreal-username and \
             --surreal-password are required"
        ),
    }
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
    /// Resolved external tool package directories (file → env precedence).
    pub(crate) external_tools_dirs: Vec<PathBuf>,
    /// Datasource activation map from runner.toml.
    pub(crate) external_datasources:
        std::collections::HashMap<String, std::collections::HashMap<String, DatasourceActivation>>,
    /// Resolved sandbox bind allowlist roots (file → env precedence).
    pub(crate) sandbox_bind_roots: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(name = "agentium serve")]
#[command(version)]
#[command(
    about = "Run the Agentium platform. Agents are deployed exclusively via the repository API.",
    long_about = "Starts the platform and restores any previously-deployed agents from --state-dir.\n\
                  \nTo add agents, use `agentium install agent` or POST /deploy.\n\
                  Positional package paths are no longer accepted — all deployment goes through the repository.\n\
                  \nDeployment rows (what GET /agents lists after boot) live under --state-dir/state.db.\n\
                  Clearing or resetting --provenance-db alone does not undeploy agents; use POST /undeploy,\n\
                  remove state.db, or wipe the entire --state-dir."
)]
pub struct Cli {
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

    /// Provenance graph store (`:memory:` or a directory for embedded SurrealKV, unless --surreal-endpoint).
    /// Independent of the deployment registry: resetting this does not remove deployed agents from GET /agents.
    #[arg(long, value_name = "PATH", default_value = ":memory:")]
    pub(crate) provenance_db: String,

    /// Directory holding the deployment registry (`state.db`, embedded SurrealKV).
    /// On startup the runner replays active deployments from here (same set as GET /agents). To drop
    /// every deployed agent without POST /undeploy per hash, stop the runner and remove this directory or `state.db` inside it.
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

    /// Path to a runner.toml with optional `[external_tools]` and
    /// `[sandbox.bind]` sections. Values are merged with the legacy env vars
    /// (`BAML_EXTERNAL_TOOLS_DIR`, `BAML_SANDBOX_BIND_ROOTS`); env wins when set.
    #[arg(long, value_name = "FILE", env = "BAML_RUNNER_CONFIG")]
    pub(crate) runner_config: Option<PathBuf>,
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

        let file_config = match &self.runner_config {
            Some(path) => FileConfig::load(path)?,
            None => FileConfig::default(),
        };
        let resolved = runner_config_file::resolve_paths(&file_config);
        info!(
            count = resolved.external_tools_dirs.len(),
            source = resolved.external_tools_source.as_str(),
            "external_tools.dirs resolved"
        );
        info!(
            count = resolved.sandbox_bind_roots.len(),
            source = resolved.sandbox_bind_source.as_str(),
            "sandbox.bind.roots resolved"
        );

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
            external_tools_dirs: resolved.external_tools_dirs,
            external_datasources: file_config.external_datasources,
            sandbox_bind_roots: resolved.sandbox_bind_roots,
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
            let credentials = parse_surreal_credentials(username.as_deref(), password.as_deref())
                .map_err(|e| BamlRtError::InvalidArgument(format!("provenance: {e}")))?
                .map(|(u, p)| RemoteCredentials {
                    username: u.to_string(),
                    password: p.to_string(),
                });
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

#[cfg(test)]
mod tests {
    use super::parse_surreal_credentials;

    #[test]
    fn both_present_yields_pair() {
        let creds = parse_surreal_credentials(Some("root"), Some("hunter2")).unwrap();
        assert_eq!(creds, Some(("root", "hunter2")));
    }

    #[test]
    fn both_absent_yields_none() {
        let creds = parse_surreal_credentials(None, None).unwrap();
        assert_eq!(creds, None);
    }

    #[test]
    fn username_without_password_is_rejected() {
        let err = parse_surreal_credentials(Some("root"), None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--surreal-username"), "{msg}");
        assert!(msg.contains("--surreal-password"), "{msg}");
    }

    #[test]
    fn password_without_username_is_rejected() {
        let err = parse_surreal_credentials(None, Some("hunter2")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--surreal-username"), "{msg}");
        assert!(msg.contains("--surreal-password"), "{msg}");
    }
}
