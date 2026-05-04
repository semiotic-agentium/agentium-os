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

use std::collections::HashMap;

use anyhow::Result;
use baml_rt_llm_config::{
    DEFAULT_OPENROUTER_BASE_URL, placeholder_to_key, require_base_url_if_required,
};
use baml_runtime::{
    BamlRuntime, InternalRuntimeInterface,
    client_registry::{ClientProperty, ClientRegistry},
};
use baml_types::{BamlMap, BamlValue, EvaluationContext};
use internal_llm_client::{ClientProvider, ResolvedClientProperty};

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
        // BAML IR may expose placeholders as "env.OPENROUTER_API_KEY" or "vault:OPENROUTER_API_KEY";
        // normalise to the bare key for lookup.
        let mut api_key_value: Option<String> = None;
        for key in required_env_vars.iter() {
            let key_for_check = placeholder_to_key(key.as_str());
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

        // Some providers need extra options (e.g. openrouter is openai variant but same options).
        if is_vertex_or_aws(&provider) {
            // Skip Vertex/AWS for now; they need credentials/location. Could extend later.
            continue;
        }

        // Preserve user-declared options (model, base_url, headers, arbitrary extras like
        // temperature/top_p) from the IR. Fall back to provider defaults only when the user
        // omitted the field. `fill_missing_env_vars=true` lets unresolved env refs (e.g. the
        // api_key env var, which we overlay below) become placeholders instead of aborting.
        let empty_env: HashMap<String, String> = HashMap::new();
        let eval_ctx = EvaluationContext::new(&empty_env, true);
        let user_parts = match client.options().resolve(&provider, &eval_ctx) {
            Ok(resolved) => resolved_property_to_parts(&resolved),
            Err(e) => {
                tracing::warn!(
                    client = %name,
                    error = %e,
                    "failed to resolve IR client options; falling back to provider defaults"
                );
                ResolvedClientParts::default()
            }
        };

        let options = build_client_options(&provider, api_key, user_parts);

        // Validate: OpenRouter/openai-generic must have base_url (make invalid state impossible).
        let requires_base_url = is_openrouter_or_generic(&provider);
        require_base_url_if_required(&options, requires_base_url)?;

        let retry_policy = client.elem().retry_policy_id.clone();
        let client_property = ClientProperty::new(name.clone(), provider, retry_policy, options);
        acc.add_client(name, client_property);
    }

    Ok(acc.finish())
}

/// Build the registry-ready `BamlMap` of options for a single client. Preserves
/// user-declared model/base_url/headers/properties, overlays `api_key` from the secret
/// resolver, and falls back to provider defaults only when the user omitted `model`.
///
/// Fallback order for model:
/// 1. `user_parts.model` — resolved from IR (top-level or `properties["model"]`).
/// 2. `user_parts.properties["model"]` — any stray user-declared model key.
/// 3. `default_model_for_provider(provider)` — last resort.
fn build_client_options(
    provider: &ClientProvider,
    api_key: String,
    user_parts: ResolvedClientParts,
) -> BamlMap<String, BamlValue> {
    // Start from user-declared properties so arbitrary extras (temperature, top_p,
    // anthropic-version, etc.) survive into the registry. Overlay model + api_key below.
    let mut options: BamlMap<String, BamlValue> = user_parts
        .properties
        .into_iter()
        .map(|(k, v)| (k, json_to_baml_value(v)))
        .collect();

    let model = user_parts
        .model
        .or_else(|| {
            options.get("model").and_then(|v| match v {
                BamlValue::String(s) => Some(s.clone()),
                _ => None,
            })
        })
        .unwrap_or_else(|| default_model_for_provider(provider).to_string());
    options.insert("model".to_string(), BamlValue::String(model));

    options.insert("api_key".to_string(), BamlValue::String(api_key));

    if !user_parts.headers.is_empty() {
        let hmap: BamlMap<String, BamlValue> = user_parts
            .headers
            .into_iter()
            .map(|(k, v)| (k, BamlValue::String(v)))
            .collect();
        options.insert("headers".to_string(), BamlValue::Map(hmap));
    }

    let requires_base_url = is_openrouter_or_generic(provider);
    if requires_base_url {
        let base_url = user_parts
            .base_url
            .unwrap_or_else(|| DEFAULT_OPENROUTER_BASE_URL.to_string());
        options.insert("base_url".to_string(), BamlValue::String(base_url));
    } else if let Some(base_url) = user_parts.base_url {
        options.insert("base_url".to_string(), BamlValue::String(base_url));
    }

    options
}

/// Subset of resolved client options we forward into the registry. Strategy clients
/// (RoundRobin/Fallback) resolve to no single model/base_url — callers get `default()`.
#[derive(Default)]
struct ResolvedClientParts {
    model: Option<String>,
    base_url: Option<String>,
    properties: Vec<(String, serde_json::Value)>,
    headers: Vec<(String, String)>,
}

/// Pull model/base_url/properties/headers out of a resolved client property. Strategy
/// clients (RoundRobin/Fallback) don't have a single model; they return empty parts.
fn resolved_property_to_parts(resolved: &ResolvedClientProperty) -> ResolvedClientParts {
    match resolved {
        ResolvedClientProperty::OpenAI(r) => ResolvedClientParts {
            model: r
                .properties
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            base_url: Some(r.base_url.clone()),
            properties: r
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            headers: r
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        },
        ResolvedClientProperty::Anthropic(r) => ResolvedClientParts {
            model: r
                .properties
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            base_url: Some(r.base_url.clone()),
            properties: r
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            headers: r
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        },
        ResolvedClientProperty::GoogleAI(r) => ResolvedClientParts {
            model: Some(r.model.clone()),
            base_url: Some(r.base_url.clone()),
            properties: r
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            headers: r
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        },
        // Vertex/AWS are filtered upstream by `is_vertex_or_aws`. RoundRobin/Fallback don't
        // expose a single model; leave fields empty and let callers fall through to defaults.
        ResolvedClientProperty::AWSBedrock(_)
        | ResolvedClientProperty::Vertex(_)
        | ResolvedClientProperty::RoundRobin(_)
        | ResolvedClientProperty::Fallback(_) => ResolvedClientParts::default(),
    }
}

/// Convert a `serde_json::Value` from resolved BAML properties into a `BamlValue` suitable
/// for the ClientRegistry options map.
fn json_to_baml_value(v: serde_json::Value) -> BamlValue {
    match v {
        serde_json::Value::Null => BamlValue::Null,
        serde_json::Value::Bool(b) => BamlValue::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                BamlValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                BamlValue::Float(f)
            } else {
                BamlValue::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => BamlValue::String(s),
        serde_json::Value::Array(a) => {
            BamlValue::List(a.into_iter().map(json_to_baml_value).collect())
        }
        serde_json::Value::Object(o) => BamlValue::Map(
            o.into_iter()
                .map(|(k, v)| (k, json_to_baml_value(v)))
                .collect(),
        ),
    }
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
    use internal_llm_client::OpenAIClientProviderVariant;

    use super::*;

    #[test]
    fn llm_secret_keys_contains_expected() {
        assert!(LLM_SECRET_KEYS.contains(&"OPENAI_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"ANTHROPIC_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"OPENROUTER_API_KEY"));
        assert!(LLM_SECRET_KEYS.contains(&"GOOGLE_API_KEY"));
    }

    fn openrouter_provider() -> ClientProvider {
        ClientProvider::OpenAI(OpenAIClientProviderVariant::OpenRouter)
    }

    fn assert_string_option(options: &BamlMap<String, BamlValue>, key: &str, expected: &str) {
        match options.get(key) {
            Some(BamlValue::String(v)) => assert_eq!(v, expected, "option `{key}`"),
            other => panic!("expected string option `{key}` = {expected:?}, got {other:?}"),
        }
    }

    /// Regression: user-declared `model` in the BAML client MUST be preserved in the registry
    /// options. Before the fix, `build_llm_client_registry` overwrote it with
    /// `default_model_for_provider` (grok), causing agents declaring gpt-4o-mini to silently
    /// run on grok-4.1-fast.
    #[test]
    fn registry_preserves_user_declared_model() {
        let provider = openrouter_provider();
        let user_parts = ResolvedClientParts {
            model: Some("openai/gpt-4o-mini".to_string()),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            properties: vec![(
                "model".to_string(),
                serde_json::Value::String("openai/gpt-4o-mini".to_string()),
            )],
            headers: Vec::new(),
        };

        let options = build_client_options(&provider, "sk-test".to_string(), user_parts);

        assert_string_option(&options, "model", "openai/gpt-4o-mini");
        assert_string_option(&options, "api_key", "sk-test");
        assert_string_option(&options, "base_url", "https://openrouter.ai/api/v1");
    }

    /// Regression: arbitrary user-declared options (temperature, top_p, etc.) must survive
    /// into the registry. Ensures the bug cannot reappear in another form where non-model
    /// fields get silently dropped.
    #[test]
    fn registry_preserves_arbitrary_user_options() {
        let provider = openrouter_provider();
        let user_parts = ResolvedClientParts {
            model: Some("openai/gpt-4o-mini".to_string()),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            properties: vec![
                (
                    "model".to_string(),
                    serde_json::Value::String("openai/gpt-4o-mini".to_string()),
                ),
                (
                    "temperature".to_string(),
                    serde_json::Value::Number(serde_json::Number::from_f64(0.2).unwrap()),
                ),
                (
                    "top_p".to_string(),
                    serde_json::Value::Number(serde_json::Number::from_f64(0.9).unwrap()),
                ),
            ],
            headers: Vec::new(),
        };

        let options = build_client_options(&provider, "sk-test".to_string(), user_parts);

        match options.get("temperature") {
            Some(BamlValue::Float(f)) => assert!((f - 0.2).abs() < 1e-9),
            other => panic!("expected Float temperature, got {other:?}"),
        }
        match options.get("top_p") {
            Some(BamlValue::Float(f)) => assert!((f - 0.9).abs() < 1e-9),
            other => panic!("expected Float top_p, got {other:?}"),
        }
        assert_string_option(&options, "model", "openai/gpt-4o-mini");
    }

    /// Regression: when the user declares custom headers on the client, they must be copied
    /// into `options["headers"]` so PropertyHandler::ensure_headers picks them up.
    #[test]
    fn registry_preserves_user_headers() {
        let provider = openrouter_provider();
        let user_parts = ResolvedClientParts {
            model: Some("openai/gpt-4o-mini".to_string()),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            properties: Vec::new(),
            headers: vec![("X-Custom".to_string(), "hello".to_string())],
        };

        let options = build_client_options(&provider, "sk-test".to_string(), user_parts);

        match options.get("headers") {
            Some(BamlValue::Map(m)) => match m.get("X-Custom") {
                Some(BamlValue::String(v)) => assert_eq!(v, "hello"),
                other => panic!("expected String header value, got {other:?}"),
            },
            other => panic!("expected Map headers, got {other:?}"),
        }
    }

    /// When the user omits `model`, we fall back to `default_model_for_provider`. This
    /// preserves the historical behavior for agents that don't declare a model.
    #[test]
    fn registry_falls_back_to_provider_default_when_model_missing() {
        let provider = openrouter_provider();
        let user_parts = ResolvedClientParts {
            model: None,
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            properties: Vec::new(),
            headers: Vec::new(),
        };

        let options = build_client_options(&provider, "sk-test".to_string(), user_parts);

        assert_string_option(&options, "model", default_model_for_provider(&provider));
    }

    /// When user omits `base_url` for an OpenRouter/openai-generic client, the default
    /// OpenRouter URL is injected so `require_base_url_if_required` cannot fail.
    #[test]
    fn registry_injects_default_base_url_for_openrouter_when_missing() {
        let provider = openrouter_provider();
        let user_parts = ResolvedClientParts {
            model: Some("openai/gpt-4o-mini".to_string()),
            base_url: None,
            properties: Vec::new(),
            headers: Vec::new(),
        };

        let options = build_client_options(&provider, "sk-test".to_string(), user_parts);

        assert_string_option(&options, "base_url", DEFAULT_OPENROUTER_BASE_URL);
    }
}
