//! Build BAML `ClientRegistry` with LLM API keys from a secret resolver (not env vars).
//!
//! When a resolver is provided, we iterate the BAML IR clients, resolve API keys via
//! the llm mapping, and build a `ClientRegistry` with `ClientProperty` entries whose
//! `options` include `api_key`. LLM API keys are never read from the environment.
//!
//! ## Call-site migration (return type change)
//!
//! `build_llm_client_registry` returns [`LlmRegistryBuildResult`] instead of
//! `(Option<ClientRegistry>, Vec<String>)`. To migrate:
//! - **Minimal change:** `let result = build_llm_client_registry(...)?; let (client_registry_opt, _) = result.into_legacy_tuple();`
//! - **Preferred:** use `result.registry()` for `Option<&ClientRegistry>` and `result.secret_keys_accessed()` for provenance; or match on `LlmRegistryBuildResult::NoRegistry` / `WithRegistry`.

use anyhow::Result;
use baml_rt_llm_config::{DEFAULT_OPENROUTER_BASE_URL, require_base_url_if_required};
use baml_runtime::{
    BamlRuntime, InternalRuntimeInterface,
    client_registry::{ClientProperty, ClientRegistry},
};
use baml_types::{BamlMap, BamlValue};
use internal_llm_client::ClientProvider;

/// Env-key names used for LLM API keys (mapped in secret_mapping with scope_type "llm").
pub const LLM_SECRET_KEYS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GOOGLE_API_KEY",
];

/// Resolves LLM API keys for registry injection (e.g. from fnox + llm mapping).
///
/// When a key is mapped for the given scope, returns `Some((api_key_value, registered_key))`.
/// The `registered_key` is recorded for provenance (`secret_keys_accessed`).
pub trait LlmSecretResolver: Send + Sync {
    /// Resolve an LLM env key for the given scope (e.g. "default" or agent_id).
    /// Returns (secret_value, registered_key_for_provenance) when mapped.
    fn resolve_llm_api_key(&self, scope_id: &str, env_key: &str) -> Option<(String, String)>;
}

// ---------------------------------------------------------------------------
// Type-safe build result: registry + primary + secret_keys_accessed as one product.
// Invalid state "registry with clients but no primary" is unrepresentable.
// ---------------------------------------------------------------------------

/// Result of building an LLM client registry from a secret resolver.
///
/// Discriminates between "no registry" (no resolver or no clients added) and
/// "registry with at least one client and a primary". The primary is the
/// first client added; that invariant is enforced by construction.
#[derive(Debug, Clone)]
pub enum LlmRegistryBuildResult {
    /// No resolver was provided or no clients could be added (e.g. no keys resolved, or only Vertex/AWS).
    NoRegistry { secret_keys_accessed: Vec<String> },
    /// At least one client was added; `primary_client_name` is the first added and is set on the registry.
    WithRegistry {
        registry: ClientRegistry,
        primary_client_name: String,
        secret_keys_accessed: Vec<String>,
    },
}

impl LlmRegistryBuildResult {
    /// Reference to the registry if present.
    pub fn registry(&self) -> Option<&ClientRegistry> {
        match self {
            Self::NoRegistry { .. } => None,
            Self::WithRegistry { registry, .. } => Some(registry),
        }
    }

    /// Slice of secret key names that were accessed (for provenance).
    pub fn secret_keys_accessed(&self) -> &[String] {
        match self {
            Self::NoRegistry {
                secret_keys_accessed,
            } => secret_keys_accessed,
            Self::WithRegistry {
                secret_keys_accessed,
                ..
            } => secret_keys_accessed,
        }
    }

    /// Primary client name when a registry was built (first added client).
    pub fn primary_client_name(&self) -> Option<&str> {
        match self {
            Self::NoRegistry { .. } => None,
            Self::WithRegistry {
                primary_client_name,
                ..
            } => Some(primary_client_name),
        }
    }

    /// Consume and return the registry if present (e.g. for storing in `BamlStreamInvocation`).
    pub fn into_registry(self) -> Option<ClientRegistry> {
        match self {
            Self::NoRegistry { .. } => None,
            Self::WithRegistry { registry, .. } => Some(registry),
        }
    }

    /// Convert into the legacy `(Option<ClientRegistry>, Vec<String>)` for call sites that have not yet been updated.
    pub fn into_legacy_tuple(self) -> (Option<ClientRegistry>, Vec<String>) {
        match self {
            Self::NoRegistry {
                secret_keys_accessed,
            } => (None, secret_keys_accessed),
            Self::WithRegistry {
                registry,
                secret_keys_accessed,
                ..
            } => (Some(registry), secret_keys_accessed),
        }
    }
}

/// Accumulator used during registry build: collects clients, tracks the first added as primary, and secret keys.
/// Invariant: when `primary_client_name` is `Some`, the registry is non-empty and that client was set as primary.
struct RegistryBuildAccumulator {
    registry: ClientRegistry,
    primary_client_name: Option<String>,
    secret_keys_accessed: Vec<String>,
}

impl RegistryBuildAccumulator {
    fn new() -> Self {
        Self {
            registry: ClientRegistry::new(),
            primary_client_name: None,
            secret_keys_accessed: Vec::new(),
        }
    }

    /// Record a secret key that was accessed (deduplicated).
    fn record_secret_key(&mut self, registered_key: String) {
        if !self.secret_keys_accessed.contains(&registered_key) {
            self.secret_keys_accessed.push(registered_key);
        }
    }

    /// Add a client to the registry. The first client added becomes the primary (set on the registry).
    fn add_client(&mut self, client_name: String, client_property: ClientProperty) {
        let is_first = self.primary_client_name.is_none();
        if is_first {
            self.primary_client_name = Some(client_name.clone());
        }
        self.registry.add_client(client_property);
        if is_first {
            self.registry.set_primary(client_name);
        }
    }

    /// Finalize into a build result. WithRegistry is only produced when at least one client was added.
    fn finish(self) -> LlmRegistryBuildResult {
        match self.primary_client_name {
            None => LlmRegistryBuildResult::NoRegistry {
                secret_keys_accessed: self.secret_keys_accessed,
            },
            Some(primary_client_name) => LlmRegistryBuildResult::WithRegistry {
                registry: self.registry,
                primary_client_name,
                secret_keys_accessed: self.secret_keys_accessed,
            },
        }
    }
}

/// Build a `ClientRegistry` with API keys from the resolver (not env vars).
///
/// Uses `runtime.ir().walk_clients()` to get client names and providers; for each
/// client's `required_env_vars` that are in `LLM_SECRET_KEYS`, resolves via the
/// resolver and builds `ClientProperty` with `api_key` in options.
///
/// Returns [`LlmRegistryBuildResult`]: either no registry (no resolver or no clients added)
/// or a registry with primary client and `secret_keys_accessed` for provenance.
pub fn build_llm_client_registry(
    runtime: &BamlRuntime,
    resolver: Option<&dyn LlmSecretResolver>,
    scope_id: &str,
) -> Result<LlmRegistryBuildResult> {
    let Some(resolver) = resolver else {
        return Ok(LlmRegistryBuildResult::NoRegistry {
            secret_keys_accessed: vec![],
        });
    };

    let ir = runtime.ir();
    let mut acc = RegistryBuildAccumulator::new();

    for client in ir.walk_clients() {
        let name = client.name().to_string();
        let provider = client.elem().provider.clone();
        let required_env_vars = client.required_env_vars();

        // Resolve first LLM secret we have a mapping for (one api_key per client).
        // BAML IR may expose placeholders as "env.OPENROUTER_API_KEY"; normalize to "OPENROUTER_API_KEY" for lookup.
        let mut api_key_value: Option<String> = None;
        for key in required_env_vars.iter() {
            let key_for_check = key.strip_prefix("env.").unwrap_or(key.as_str());
            if LLM_SECRET_KEYS.contains(&key_for_check)
                && let Some((value, registered_key)) =
                    resolver.resolve_llm_api_key(scope_id, key_for_check)
            {
                api_key_value = Some(value);
                acc.record_secret_key(registered_key);
                break;
            }
        }

        let Some(api_key) = api_key_value else {
            continue;
        };

        let model = default_model_for_provider(&provider);
        let mut options: BamlMap<String, BamlValue> = [
            ("model".to_string(), BamlValue::String(model.to_string())),
            ("api_key".to_string(), BamlValue::String(api_key)),
        ]
        .into_iter()
        .collect();
        let requires_base_url = is_openrouter_or_generic(&provider);
        if requires_base_url {
            options.insert(
                "base_url".to_string(),
                BamlValue::String(DEFAULT_OPENROUTER_BASE_URL.to_string()),
            );
        }

        // Some providers need extra options (e.g. openrouter is openai variant but same options).
        if is_vertex_or_aws(&provider) {
            // Skip Vertex/AWS for now; they need credentials/location. Could extend later.
            continue;
        }

        // Validate: OpenRouter/openai-generic must have base_url (make invalid state impossible).
        require_base_url_if_required(&options, requires_base_url)?;

        let retry_policy = client.elem().retry_policy_id.clone();
        let client_property = ClientProperty::new(name.clone(), provider, retry_policy, options);
        acc.add_client(name, client_property);
    }

    Ok(acc.finish())
}

fn default_model_for_provider(provider: &ClientProvider) -> &'static str {
    use internal_llm_client::OpenAIClientProviderVariant;
    match provider {
        ClientProvider::OpenAI(variant) => match variant {
            // OpenRouter and openai-generic both route through OpenRouter — use grok.
            OpenAIClientProviderVariant::OpenRouter | OpenAIClientProviderVariant::Generic => {
                "x-ai/grok-4.1-fast"
            }
            // Native OpenAI base, Azure, Responses, Ollama stay on the OpenAI model family.
            OpenAIClientProviderVariant::Base
            | OpenAIClientProviderVariant::Responses
            | OpenAIClientProviderVariant::Azure
            | OpenAIClientProviderVariant::Ollama => "gpt-4o-mini",
        },
        ClientProvider::Anthropic => "claude-3-5-sonnet-20241022",
        ClientProvider::GoogleAi => "gemini-2.0-flash",
        // Vertex/AWS are skipped before this function is called; value is unreachable.
        ClientProvider::Vertex | ClientProvider::AwsBedrock | ClientProvider::Strategy(_) => {
            "gpt-4o-mini"
        }
    }
}

fn is_vertex_or_aws(provider: &ClientProvider) -> bool {
    matches!(
        provider,
        ClientProvider::Vertex | ClientProvider::AwsBedrock
    )
}

/// OpenRouter and openai-generic (used for OpenRouter in BAML) require base_url in options.
fn is_openrouter_or_generic(provider: &ClientProvider) -> bool {
    use internal_llm_client::OpenAIClientProviderVariant;
    matches!(
        provider,
        ClientProvider::OpenAI(
            OpenAIClientProviderVariant::OpenRouter | OpenAIClientProviderVariant::Generic
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_secret_keys_contains_expected() {
        assert!(LLM_SECRET_KEYS.contains(&"OPENAI_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"ANTHROPIC_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"OPENROUTER_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"GOOGLE_API_KEY"));
    }
}
