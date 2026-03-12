//! OpenAPI schema types and spec builder.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// Cut-down agent card (included in every GET /agents item).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentCardDto {
    pub name: String,
    pub version: String,
    pub agent_package: String,
    pub agent_instance_id: String,
    pub tools: Vec<String>,
    /// BAML function names registered in the agent's runtime.
    pub baml_functions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
}

/// Discovery entry for one running agent (GET /agents response item).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentDiscoveryEntryDto {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub name: String,
    pub version: String,
    /// Agent card (cut-down shape) for discovery.
    pub agent_card: AgentCardDto,
}

impl From<baml_rt_core::AgentCard> for AgentCardDto {
    fn from(c: baml_rt_core::AgentCard) -> Self {
        Self {
            name: c.name,
            version: c.version,
            agent_package: c.agent_package,
            agent_instance_id: c.agent_instance_id,
            tools: c.tools,
            baml_functions: c.baml_functions,
            description: c.description,
            capabilities: c.capabilities,
        }
    }
}

impl From<baml_rt_core::AgentDiscoveryEntry> for AgentDiscoveryEntryDto {
    fn from(e: baml_rt_core::AgentDiscoveryEntry) -> Self {
        Self {
            agent_package: e.agent_package,
            agent_instance_id: e.agent_instance_id,
            name: e.name,
            version: e.version,
            agent_card: AgentCardDto::from(e.agent_card),
        }
    }
}

// ---------------------------------------------------------------------------
// Config API DTOs (OpenAPI schema includes config type schemas)
// ---------------------------------------------------------------------------

/// Tool config schema and default (for GET /config and GET /config/{tool_name}).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ToolConfigSchemaDto {
    /// Tool name (bundle/local).
    pub tool_name: String,
    /// JSON Schema for the tool's config type (defines shape of PUT /config/{tool_name} body).
    #[schema(value_type = Object)]
    pub schema: Value,
    /// Default config (from schema or explicit).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub default: Option<Value>,
    /// Whether this tool has stored config.
    pub has_config: bool,
}

/// Tool config with version (GET /config/{tool_name} response).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ToolConfigDto {
    pub tool_name: String,
    /// Current config (with defaults merged). Shape defined by tool's config schema.
    #[schema(value_type = Object)]
    pub config: Value,
    pub version: u64,
}

/// Config at a specific version.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigVersionDto {
    pub version: u64,
    /// Config snapshot. Shape defined by tool's config schema.
    #[schema(value_type = Object)]
    pub config: Value,
    pub created_at_ms: u64,
}

/// Secret request (for GET /config/{tool_name}/secret-requests).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SecretRequestDto {
    pub name: String,
    pub secret_type: String,
    pub justification: String,
    pub descriptor: String,
}

/// One secret in the M:N overview: which tools and LLM clients require it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SecretOverviewEntryDto {
    /// Canonical secret name / key (e.g. NOTION_API_TOKEN).
    pub name: String,
    /// Type from tool secret_requests (e.g. "api_key"); absent if only referenced by LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_type: Option<String>,
    /// Why this secret is needed (from first tool that declares it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
    /// What the secret must provide (from first tool that declares it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<String>,
    /// Tool names (bundle/local) that declare this secret.
    pub tool_consumers: Vec<String>,
    /// LLM client names that reference this secret (e.g. api_key placeholder for OPENROUTER_API_KEY).
    pub llm_consumers: Vec<String>,
    /// True if the secret is provisioned (resolver returns a value for this key); false when missing or when resolver is unavailable. Always present (not optional).
    pub satisfied: bool,
    /// When satisfied and linked via the UI: the key in the secret store this secret is linked to (e.g. env.OPENROUTER_API_KEY). Omitted when not linked or loaded from defaults only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_to: Option<String>,
}

/// Request body for linking a secret by name (PUT /config/secrets/{name}).
/// Only link_from is accepted; secrets are stored in the secret store (fnox), never sent as raw values.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProvisionSecretDto {
    /// Key name in the secret store (fnox/env) to link from. The runner resolves this and uses its value for {name}.
    pub link_from: String,
}
