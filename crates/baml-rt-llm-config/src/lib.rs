//! Centralised LLM client configuration for BAML runtime.
//!
//! Config defines clients, default, and overrides (agent, agent_function).
//! Resolution: overrides.agent_function["agent:fn"] → overrides.agent["agent"] → default.
//! BAML only has `client Default`; model selection is fully decoupled and host-controlled.

mod client_snippet;
mod config;
mod loader;
mod provider_requirements;
mod registry_builder;
mod resolver;
mod secret_resolver;
mod test_model;

pub use client_snippet::{CLIENT_DEFAULT_FALLBACK_BAML, client_default_baml_snippet};
pub use config::{
    ClientDef, LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, LlmOverrides, LlmProvider, RetryPolicyDef,
};
pub use provider_requirements::{
    DEFAULT_OPENROUTER_BASE_URL, ensure_base_url_for_provider_config, provider_requires_base_url,
    require_base_url_if_required,
};
pub use registry_builder::build_client_registry;
pub use resolver::{LlmClientResolver, StaticResolver};
pub use secret_resolver::{
    EmptySecretResolver, FnoxFileSecretResolver, OverlaySecretResolver, RuntimeSecretStore,
    SECRET_LINKS_CONFIG_KEY, SecretLinksState, SecretName, SecretRequestName, SecretResolver,
    SecretSourcePolicy, SecretValue, StoreKey, apply_secret_links_state, placeholder_to_key,
    strip_placeholder_prefix,
};
pub use test_model::{FALLBACK_TEST_MODEL, test_model_default};
