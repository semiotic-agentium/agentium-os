//! Build BAML ClientRegistry from LlmClientConfig.

use anyhow::Result;
use baml_runtime::client_registry::{ClientProperty, ClientRegistry};
use baml_types::{BamlMap, BamlValue};
use internal_llm_client::ClientProvider;

use crate::{
    config::{LlmClientConfig, LlmProvider},
    secret_resolver::SecretResolver,
};

/// Build a ClientRegistry from config with the given primary client.
/// Injects api_key from options via secret_resolver when option value looks like a placeholder (env.VAR).
pub fn build_client_registry(
    config: &LlmClientConfig,
    primary_client_name: &str,
    secret_resolver: &dyn SecretResolver,
) -> Result<ClientRegistry> {
    let mut registry = ClientRegistry::new();

    for (name, client) in &config.clients {
        let provider = client_provider_from_llm(client.provider);

        let mut options = BamlMap::new();
        for (k, v) in &client.options {
            let is_secret_ref = v.starts_with("env.") || v.starts_with("vault:");
            let value = if is_secret_ref {
                match secret_resolver.resolve(v) {
                    Some(s) => BamlValue::String(s.into_string()),
                    None => {
                        tracing::warn!(
                            client = %name,
                            option = %k,
                            placeholder = %v,
                            "secret not resolved; LLM calls using this client will likely fail"
                        );
                        BamlValue::String(String::new())
                    }
                }
            } else {
                BamlValue::String(v.clone())
            };
            options.insert(k.clone(), value);
        }

        // Ensure model is set if not present (some providers require it)
        if !options.contains_key("model") {
            options.insert(
                "model".to_string(),
                BamlValue::String(default_model_for_provider(&provider)),
            );
        }

        // base_url for OpenRouter/openai-generic is ensured at config load/update (normalize), not here.
        let retry_policy = client.retry_policy.clone();
        let client_property = ClientProperty::new(name.clone(), provider, retry_policy, options);
        registry.add_client(client_property);
    }

    if !registry.is_empty() && config.clients.contains_key(primary_client_name) {
        registry.set_primary(primary_client_name.to_string());
    }

    Ok(registry)
}

fn client_provider_from_llm(p: LlmProvider) -> ClientProvider {
    use internal_llm_client::OpenAIClientProviderVariant;
    match p {
        LlmProvider::Openai | LlmProvider::OpenaiBase => {
            ClientProvider::OpenAI(OpenAIClientProviderVariant::Base)
        }
        LlmProvider::OpenaiGeneric => ClientProvider::OpenAI(OpenAIClientProviderVariant::Generic),
        LlmProvider::OpenaiResponses => {
            ClientProvider::OpenAI(OpenAIClientProviderVariant::Responses)
        }
        LlmProvider::AzureOpenai => ClientProvider::OpenAI(OpenAIClientProviderVariant::Azure),
        LlmProvider::Ollama => ClientProvider::OpenAI(OpenAIClientProviderVariant::Ollama),
        LlmProvider::Openrouter => ClientProvider::OpenAI(OpenAIClientProviderVariant::OpenRouter),
        LlmProvider::Anthropic => ClientProvider::Anthropic,
        LlmProvider::GoogleAi => ClientProvider::GoogleAi,
        LlmProvider::Vertex => ClientProvider::Vertex,
        LlmProvider::AwsBedrock => ClientProvider::AwsBedrock,
    }
}

fn default_model_for_provider(provider: &ClientProvider) -> String {
    use internal_llm_client::OpenAIClientProviderVariant;
    match provider {
        ClientProvider::OpenAI(variant) => match variant {
            // OpenRouter and openai-generic both route via OpenRouter — use grok.
            OpenAIClientProviderVariant::OpenRouter | OpenAIClientProviderVariant::Generic => {
                "x-ai/grok-4.1-fast".to_string()
            }
            // Native OpenAI base, Azure, Responses, Ollama stay on the OpenAI model family.
            OpenAIClientProviderVariant::Base
            | OpenAIClientProviderVariant::Responses
            | OpenAIClientProviderVariant::Azure
            | OpenAIClientProviderVariant::Ollama => "gpt-4o-mini".to_string(),
        },
        ClientProvider::Anthropic => "claude-3-5-sonnet-20241022".to_string(),
        ClientProvider::GoogleAi => "gemini-2.0-flash".to_string(),
        // Vertex/AWS are not injected; value is unreachable in practice.
        ClientProvider::Vertex | ClientProvider::AwsBedrock | ClientProvider::Strategy(_) => {
            "gpt-4o-mini".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::build_client_registry;
    use crate::{
        config::{ClientDef, LlmClientConfig, LlmProvider},
        secret_resolver::EmptySecretResolver,
    };

    /// Config → ClientRegistry substitution: build_client_registry from config with client "Default"
    /// yields a non-empty registry so the host can override the schema's minimal client at runtime.
    #[test]
    fn build_client_registry_from_config_substitutes_default_client() {
        let mut options = HashMap::new();
        options.insert("model".to_string(), "openai/gpt-4o-mini".to_string());
        let client = ClientDef {
            name: "Default".to_string(),
            provider: LlmProvider::Openrouter,
            options,
            retry_policy: None,
        };
        let mut clients = HashMap::new();
        clients.insert("Default".to_string(), client);
        let mut config = LlmClientConfig {
            default: "Default".to_string(),
            clients,
            ..Default::default()
        };
        config.normalize(); // OpenRouter gets base_url at config level, not at registry-build time.
        let resolver = EmptySecretResolver;
        let registry = build_client_registry(&config, "Default", &resolver).unwrap();
        assert!(
            !registry.is_empty(),
            "Registry from config must be non-empty so BAML uses it to override schema client"
        );
    }
}
