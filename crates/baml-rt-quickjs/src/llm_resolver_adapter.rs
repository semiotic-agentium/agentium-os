//! Adapts `baml_rt_llm_config::SecretResolver` to `LlmSecretResolver` so fnox (or other
//! backends) can supply LLM API keys to the QuickJS bridge without env.

use std::sync::Arc;

use baml_rt_llm_config::SecretResolver;

use crate::llm_client_registry::LlmSecretResolver;

/// Wraps a config crate `SecretResolver` (e.g. FnoxFileSecretResolver) so the QuickJS
/// bridge can resolve LLM API keys from fnox.toml or other backends.
pub struct SecretResolverToLlmAdapter {
    inner: Arc<dyn SecretResolver>,
}

impl SecretResolverToLlmAdapter {
    pub fn new(inner: Arc<dyn SecretResolver>) -> Self {
        Self { inner }
    }
}

impl LlmSecretResolver for SecretResolverToLlmAdapter {
    fn resolve_llm_api_key(&self, _scope_id: &str, env_key: &str) -> Option<(String, String)> {
        let value = self
            .inner
            .resolve(&format!("env.{env_key}"))
            .or_else(|| self.inner.resolve(env_key))?;
        Some((value.into_string(), env_key.to_string()))
    }
}
