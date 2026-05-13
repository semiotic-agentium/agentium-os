//! LLM client configuration types: clients, overrides, default, and resolution.

use std::collections::HashMap;

use baml_rt_core::BamlFunctionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_requirements::ensure_base_url_for_provider_config;

/// Default env placeholder for api_key when the UI does not send one (secrets are linked in Link UI).
fn default_api_key_placeholder(p: LlmProvider) -> Option<&'static str> {
    match p {
        LlmProvider::Openrouter | LlmProvider::OpenaiGeneric => Some("env.OPENROUTER_API_KEY"),
        LlmProvider::Openai
        | LlmProvider::OpenaiBase
        | LlmProvider::OpenaiResponses
        | LlmProvider::AzureOpenai => Some("env.OPENAI_API_KEY"),
        LlmProvider::Ollama => None,
        LlmProvider::Anthropic => Some("env.ANTHROPIC_API_KEY"),
        LlmProvider::GoogleAi | LlmProvider::Vertex => Some("env.GOOGLE_API_KEY"),
        LlmProvider::AwsBedrock => Some("env.AWS_ACCESS_KEY_ID"),
    }
}

/// Bundle name used to store LLM client config in the persistent config store (ConfigReader/ConfigWriter).
/// Save with `store.set(BundleName::new(LLM_CONFIG_BUNDLE_NAME)?, serde_json::to_value(&config)?)`.
pub const LLM_CONFIG_BUNDLE_NAME: &str = "llm";

/// Supported LLM provider identifier. Serializes as kebab-case string; deserializes from kebab-case or snake_case aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmProvider {
    #[serde(alias = "openai")]
    Openai,
    OpenaiBase,
    #[serde(alias = "openai_generic")]
    OpenaiGeneric,
    OpenaiResponses,
    AzureOpenai,
    Ollama,
    Openrouter,
    Anthropic,
    #[serde(alias = "google_ai")]
    GoogleAi,
    #[serde(alias = "vertex_ai")]
    Vertex,
    #[serde(alias = "aws_bedrock")]
    AwsBedrock,
}

impl LlmProvider {
    /// Canonical string for options normalization (e.g. base_url check).
    pub fn as_str(&self) -> &'static str {
        match self {
            LlmProvider::Openai => "openai",
            LlmProvider::OpenaiBase => "openai-base",
            LlmProvider::OpenaiGeneric => "openai-generic",
            LlmProvider::OpenaiResponses => "openai-responses",
            LlmProvider::AzureOpenai => "azure-openai",
            LlmProvider::Ollama => "ollama",
            LlmProvider::Openrouter => "openrouter",
            LlmProvider::Anthropic => "anthropic",
            LlmProvider::GoogleAi => "google-ai",
            LlmProvider::Vertex => "vertex",
            LlmProvider::AwsBedrock => "aws-bedrock",
        }
    }

    /// All variants in display order (for API schema and UI dropdowns).
    pub fn all() -> &'static [LlmProvider] {
        &[
            LlmProvider::Openai,
            LlmProvider::OpenaiBase,
            LlmProvider::OpenaiGeneric,
            LlmProvider::OpenaiResponses,
            LlmProvider::AzureOpenai,
            LlmProvider::Ollama,
            LlmProvider::Openrouter,
            LlmProvider::Anthropic,
            LlmProvider::GoogleAi,
            LlmProvider::Vertex,
            LlmProvider::AwsBedrock,
        ]
    }
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Definition of a single LLM client (provider + options).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDef {
    pub name: String,
    /// Provider (strict enum); serializes as kebab-case string.
    pub provider: LlmProvider,
    /// Options (model, base_url, api_key placeholder). api_key may be "env.OPENROUTER_API_KEY".
    #[serde(default)]
    pub options: HashMap<String, String>,
    /// Optional retry policy name (references retry_policies map).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<String>,
}

/// Overrides: context → client name. Resolution order: agent_function then agent then default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmOverrides {
    /// Agent package name → client name.
    #[serde(default)]
    pub agent: HashMap<String, String>,
    /// "agent_package:function_name" → client name.
    #[serde(default)]
    pub agent_function: HashMap<String, String>,
}

/// Retry policy definition (structure per plan; full strategy types can be extended later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicyDef {
    #[serde(default)]
    pub max_retries: u32,
    #[serde(default)]
    pub strategy: Option<serde_json::Value>,
}

/// Full LLM client config: clients, default, overrides, retry policies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmClientConfig {
    pub default: String,
    pub clients: HashMap<String, ClientDef>,
    pub overrides: LlmOverrides,
    pub retry_policies: HashMap<String, RetryPolicyDef>,
}

impl LlmClientConfig {
    /// Sensible default for UI and API when no config is stored: OpenRouter
    /// (OpenAPI-compatible) plus a second OpenRouter client whose model is the
    /// env-controlled test model (see [`crate::test_model_default`] and issue
    /// #429). Default client is "OpenRouter".
    pub fn sensible_default() -> Self {
        let mut clients = HashMap::new();
        clients.insert(
            "OpenRouter".to_string(),
            ClientDef {
                name: "OpenRouter".to_string(),
                provider: LlmProvider::Openrouter,
                options: [
                    ("model".to_string(), "openai/gpt-4o-mini".to_string()),
                    ("api_key".to_string(), "env.OPENROUTER_API_KEY".to_string()),
                ]
                .into_iter()
                .collect(),
                retry_policy: None,
            },
        );
        clients.insert(
            "TestModel".to_string(),
            ClientDef {
                name: "TestModel".to_string(),
                provider: LlmProvider::Openrouter,
                options: [
                    ("model".to_string(), crate::test_model_default()),
                    ("api_key".to_string(), "env.OPENROUTER_API_KEY".to_string()),
                ]
                .into_iter()
                .collect(),
                retry_policy: None,
            },
        );
        let mut config = Self {
            default: "OpenRouter".to_string(),
            clients,
            overrides: LlmOverrides::default(),
            retry_policies: HashMap::new(),
        };
        config.normalize();
        config
    }

    /// Resolve client name for (agent_package, function_name).
    ///
    /// Inheritance order:
    /// 1. `overrides.agent_function["agent:variant"]` — exact FSM variant match
    /// 2. `overrides.agent_function["agent:base_prompt"]` — base prompt (inherits to all variants)
    /// 3. `overrides.agent["agent"]` — agent-level default
    /// 4. `self.default` — global default
    pub fn resolve(&self, agent_package: Option<&str>, function_name: &str) -> &str {
        if let Some(agent) = agent_package {
            // 1. Exact variant key match
            let variant_key = format!("{agent}:{function_name}");
            if let Some(name) = self.overrides.agent_function.get(&variant_key) {
                return name.as_str();
            }
            // 2. Base prompt key match (inherited by all variants)
            let fid = BamlFunctionId::parse(function_name);
            if fid.is_variant() {
                let base_key = format!("{agent}:{}", fid.prompt_name());
                if let Some(name) = self.overrides.agent_function.get(&base_key) {
                    return name.as_str();
                }
            }
            // 3. Agent-level default
            if let Some(name) = self.overrides.agent.get(agent) {
                return name.as_str();
            }
        }
        self.default.as_str()
    }

    /// Get client definition by name.
    pub fn get_client(&self, name: &str) -> Option<&ClientDef> {
        self.clients.get(name)
    }

    /// Normalize client options: ensure `base_url` for providers that require it, and inject
    /// default `api_key` placeholder when missing (so UI need not send it; secrets are linked in Link UI).
    pub fn normalize(&mut self) {
        for client in self.clients.values_mut() {
            ensure_base_url_for_provider_config(&mut client.options, client.provider.as_str());
            if !client.options.contains_key("api_key")
                && let Some(placeholder) = default_api_key_placeholder(client.provider)
            {
                client
                    .options
                    .insert("api_key".to_string(), placeholder.to_string());
            }
        }
    }

    /// Deserialize from JSON Value (e.g. from persistent config store). Normalizes so
    /// OpenRouter/openai-generic clients have `base_url` in options.
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let mut config: Self = serde_json::from_value(v)?;
        config.normalize();
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_requirements::DEFAULT_OPENROUTER_BASE_URL;

    #[test]
    fn normalize_inserts_base_url_for_openrouter_client() {
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
        config.normalize();
        let client = config.clients.get("Default").unwrap();
        assert_eq!(
            client.options.get("base_url").map(String::as_str),
            Some(DEFAULT_OPENROUTER_BASE_URL)
        );
    }
}
