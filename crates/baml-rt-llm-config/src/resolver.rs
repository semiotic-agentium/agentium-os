//! LlmClientResolver trait and StaticResolver implementation.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{Result, context};
use baml_runtime::client_registry::ClientRegistry;

use crate::{
    config::LlmClientConfig, registry_builder::build_client_registry,
    secret_resolver::SecretResolver,
};

/// Resolves ClientRegistry for a BAML invocation from scope and function name.
/// When config is used, BAML's client Default is overridden; model selection is host-controlled.
#[async_trait]
pub trait LlmClientResolver: Send + Sync {
    /// Resolve ClientRegistry for this invocation. None = use BAML built-in (no overlay).
    async fn resolve(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
    ) -> Result<Option<ClientRegistry>>;
}

/// Static resolver: uses config overrides (agent, agent_function) and default; builds registry from config.
pub struct StaticResolver {
    config: Arc<LlmClientConfig>,
    secret_resolver: Arc<dyn SecretResolver>,
}

impl StaticResolver {
    pub fn new(config: Arc<LlmClientConfig>, secret_resolver: Arc<dyn SecretResolver>) -> Self {
        Self {
            config,
            secret_resolver,
        }
    }
}

#[async_trait]
impl LlmClientResolver for StaticResolver {
    async fn resolve(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
    ) -> Result<Option<ClientRegistry>> {
        let agent_package = Some(scope.agent_id().as_str());
        let primary = self.config.resolve(agent_package, function_name);
        if primary.is_empty() {
            return Ok(None);
        }
        if !self.config.clients.contains_key(primary) {
            let available: Vec<&str> = self.config.clients.keys().map(String::as_str).collect();
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "LLM routing override references client '{primary}' which does not exist; \
                 available clients: {available:?}"
            )));
        }
        let effective = primary.to_string();
        let registry = build_client_registry(
            self.config.as_ref(),
            &effective,
            self.secret_resolver.as_ref(),
        )
        .map_err(|e| {
            baml_rt_core::BamlRtError::InvalidArgument(format!("LLM registry build: {e}"))
        })?;
        Ok(Some(registry))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use baml_rt_core::{
        context::RuntimeScope,
        ids::{AgentId, ContextId, MessageId, UuidId},
    };

    use super::{LlmClientResolver, StaticResolver};
    use crate::{
        config::{ClientDef, LlmClientConfig},
        secret_resolver::EmptySecretResolver,
    };

    /// Resolver → ClientRegistry: StaticResolver with config containing "Default" returns
    /// Some(registry) for a scope, so the runner can pass it to BAML and override the schema client.
    #[tokio::test]
    async fn static_resolver_returns_registry_for_default_client() {
        let mut options = HashMap::new();
        options.insert("model".to_string(), "openai/gpt-4o-mini".to_string());
        let client = ClientDef {
            name: "Default".to_string(),
            provider: crate::config::LlmProvider::Openrouter,
            options,
            retry_policy: None,
        };
        let mut clients = HashMap::new();
        clients.insert("Default".to_string(), client);
        let config = LlmClientConfig {
            default: "Default".to_string(),
            clients,
            ..Default::default()
        };
        let resolver = StaticResolver::new(Arc::new(config), Arc::new(EmptySecretResolver));
        let scope = RuntimeScope::message_scope(
            ContextId::new(1, 1),
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap()),
            MessageId::from("msg-1"),
        );
        let registry_opt = resolver.resolve(&scope, "AddNumbers").await.unwrap();
        assert!(
            registry_opt.is_some(),
            "Resolver must return Some(registry) so BAML substitution uses config client"
        );
        assert!(
            !registry_opt.unwrap().is_empty(),
            "Substitution registry must be non-empty"
        );
    }

    /// An override that references a non-existent client is a configuration error — the resolver
    /// must return an error rather than silently falling back, so misconfigured overrides surface
    /// at call time instead of being masked.
    #[tokio::test]
    async fn static_resolver_errors_when_override_references_missing_client() {
        let mut options = HashMap::new();
        options.insert("model".to_string(), "openai/gpt-4o-mini".to_string());
        let client = ClientDef {
            name: "Default".to_string(),
            provider: crate::config::LlmProvider::Openrouter,
            options,
            retry_policy: None,
        };
        let mut clients = HashMap::new();
        clients.insert("Default".to_string(), client);
        let mut overrides = crate::config::LlmOverrides::default();
        let agent_id_str = "00000000-0000-0000-0000-000000000001";
        overrides
            .agent
            .insert(agent_id_str.to_string(), "MissingClient".to_string());
        let config = LlmClientConfig {
            default: "Default".to_string(),
            clients,
            overrides,
            ..Default::default()
        };
        let resolver = StaticResolver::new(Arc::new(config), Arc::new(EmptySecretResolver));
        let scope = RuntimeScope::message_scope(
            ContextId::new(1, 1),
            AgentId::from_uuid(UuidId::parse_str(agent_id_str).unwrap()),
            MessageId::from("msg-1"),
        );
        let result = resolver.resolve(&scope, "AddNumbers").await;
        assert!(
            result.is_err(),
            "Resolver must return an error when an override references a non-existent client"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("MissingClient"),
            "Error must name the missing client; got: {err}"
        );
    }
}
