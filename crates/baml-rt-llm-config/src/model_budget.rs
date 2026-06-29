// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Model context-window budgets for compaction trigger policy resolution.

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::config::{ClientDef, LlmClientConfig, LlmProvider};

/// Conservative bytes-per-token estimate when exact tokenization is unavailable.
pub const BYTES_PER_TOKEN_ESTIMATE: u64 = 4;

/// Default fraction of context window at which post-turn compaction may trigger.
pub const DEFAULT_TRIGGER_RATIO: f64 = 0.70;

/// Default fraction of context window at which pre-model emergency may trigger.
pub const DEFAULT_EMERGENCY_RATIO: f64 = 0.90;

/// Tokens reserved for model output when computing safe prompt budgets.
pub const DEFAULT_OUTPUT_RESERVE_TOKENS: u64 = 4_096;

/// Conservative fallback context window for unknown models.
pub const FALLBACK_CONTEXT_WINDOW_TOKENS: u64 = 32_768;

/// Where a resolved budget came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetSource {
    Configured,
    KnownModel,
    Openrouter,
    ModelsDev,
    Litellm,
    Fallback,
}

impl BudgetSource {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::KnownModel => "known",
            Self::Openrouter => "openrouter",
            Self::ModelsDev => "models_dev",
            Self::Litellm => "litellm",
            Self::Fallback => "fallback",
        }
    }
}

/// Freshness of online metadata used for budget resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetFreshness {
    Fresh,
    Stale,
    Offline,
    NotApplicable,
}

impl BudgetFreshness {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Offline => "offline",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Per-model or per-client budget override stored in LLM config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelBudgetOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emergency_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_reserve_tokens: Option<u64>,
}

/// Global compaction budget defaults and overrides in the LLM config bundle.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LlmCompactionConfig {
    #[serde(default)]
    pub defaults: CompactionBudgetDefaults,
    #[serde(default)]
    pub model_overrides: HashMap<String, ModelBudgetOverride>,
    #[serde(default)]
    pub client_overrides: HashMap<String, ModelBudgetOverride>,
    #[serde(default)]
    pub online_sources: ModelBudgetSourceConfig,
}

/// Default ratios and reserves applied when overrides are absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionBudgetDefaults {
    #[serde(default = "default_trigger_ratio")]
    pub trigger_ratio: f64,
    #[serde(default = "default_emergency_ratio")]
    pub emergency_ratio: f64,
    #[serde(default = "default_output_reserve")]
    pub output_reserve_tokens: u64,
    #[serde(default = "default_recent_tail")]
    pub recent_tail_retention: usize,
    #[serde(default = "default_item_threshold")]
    pub item_threshold: usize,
    #[serde(default = "default_defer_in_flight")]
    pub defer_while_in_flight: bool,
    #[serde(default = "default_defer_awaiting")]
    pub defer_while_awaiting_input: bool,
}

fn default_trigger_ratio() -> f64 {
    DEFAULT_TRIGGER_RATIO
}
fn default_emergency_ratio() -> f64 {
    DEFAULT_EMERGENCY_RATIO
}
fn default_output_reserve() -> u64 {
    DEFAULT_OUTPUT_RESERVE_TOKENS
}
fn default_recent_tail() -> usize {
    12
}
fn default_item_threshold() -> usize {
    40
}
fn default_defer_in_flight() -> bool {
    true
}
fn default_defer_awaiting() -> bool {
    true
}

impl Default for CompactionBudgetDefaults {
    fn default() -> Self {
        Self {
            trigger_ratio: DEFAULT_TRIGGER_RATIO,
            emergency_ratio: DEFAULT_EMERGENCY_RATIO,
            output_reserve_tokens: DEFAULT_OUTPUT_RESERVE_TOKENS,
            recent_tail_retention: default_recent_tail(),
            item_threshold: default_item_threshold(),
            defer_while_in_flight: true,
            defer_while_awaiting_input: true,
        }
    }
}

/// Online metadata source toggles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelBudgetSourceConfig {
    #[serde(default = "default_true")]
    pub openrouter: bool,
    #[serde(default = "default_true")]
    pub models_dev: bool,
    #[serde(default = "default_true")]
    pub litellm: bool,
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_cache_ttl_secs() -> u64 {
    3600
}

impl Default for ModelBudgetSourceConfig {
    fn default() -> Self {
        Self {
            openrouter: true,
            models_dev: true,
            litellm: true,
            cache_ttl_secs: 3600,
        }
    }
}

/// Resolved context budget for a model/client pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelContextBudget {
    pub model_id: String,
    pub provider: String,
    pub client_name: String,
    pub context_window_tokens: u64,
    pub safe_prompt_tokens: u64,
    pub emergency_prompt_tokens: u64,
    pub output_reserve_tokens: u64,
    pub source: BudgetSource,
    pub freshness: BudgetFreshness,
    pub warning: Option<String>,
}

impl ModelContextBudget {
    /// Conservative byte threshold derived from emergency token budget.
    #[must_use]
    pub fn emergency_prompt_bytes(&self) -> u64 {
        tokens_to_bytes(self.emergency_prompt_tokens)
    }

    /// Conservative byte threshold derived from safe/trigger token budget.
    #[must_use]
    pub fn safe_prompt_bytes(&self) -> u64 {
        tokens_to_bytes(self.safe_prompt_tokens)
    }
}

/// Resolved budgets for all configured clients (API/UX view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedClientBudgets {
    pub clients: Vec<ModelContextBudget>,
    pub refreshed_at_ms: Option<u64>,
}

/// Convert token count to conservative byte estimate.
#[must_use]
pub fn tokens_to_bytes(tokens: u64) -> u64 {
    tokens.saturating_mul(BYTES_PER_TOKEN_ESTIMATE)
}

/// Convert byte count to conservative token estimate (round up).
#[must_use]
pub fn bytes_to_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN_ESTIMATE)
}

struct CacheEntry {
    context_window_tokens: u64,
    fetched_at: Instant,
}

static ONLINE_CACHE: OnceLock<RwLock<HashMap<String, CacheEntry>>> = OnceLock::new();

fn online_cache() -> &'static RwLock<HashMap<String, CacheEntry>> {
    ONLINE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Built-in context windows for commonly used models.
#[must_use]
pub fn known_model_context_window(model_id: &str) -> Option<u64> {
    let normalized = model_id.to_ascii_lowercase();
    let table: &[(&str, u64)] = &[
        ("openai/gpt-4o", 128_000),
        ("openai/gpt-4o-mini", 128_000),
        ("openai/gpt-4.1", 1_047_576),
        ("openai/gpt-4.1-mini", 1_047_576),
        ("anthropic/claude-3.5-sonnet", 200_000),
        ("anthropic/claude-3-7-sonnet", 200_000),
        ("anthropic/claude-sonnet-4", 200_000),
        ("google/gemini-2.0-flash", 1_048_576),
        ("google/gemini-2.5-pro", 1_048_576),
        ("x-ai/grok-3", 131_072),
        ("x-ai/grok-4", 131_072),
        ("meta-llama/llama-3.3-70b-instruct", 131_072),
    ];
    table
        .iter()
        .find(|(id, _)| normalized == *id)
        .map(|(_, tokens)| *tokens)
}

fn effective_override<'a>(
    config: &'a LlmCompactionConfig,
    client_name: &str,
    model_id: &str,
) -> (Option<&'a ModelBudgetOverride>, BudgetSource) {
    if let Some(o) = config.client_overrides.get(client_name) {
        return (Some(o), BudgetSource::Configured);
    }
    if let Some(o) = config.model_overrides.get(model_id) {
        return (Some(o), BudgetSource::Configured);
    }
    (None, BudgetSource::Fallback)
}

fn apply_ratios(
    context_window: u64,
    override_: Option<&ModelBudgetOverride>,
    defaults: &CompactionBudgetDefaults,
) -> (u64, u64, u64) {
    let output_reserve = override_
        .and_then(|o| o.output_reserve_tokens)
        .unwrap_or(defaults.output_reserve_tokens);
    let trigger_ratio = override_
        .and_then(|o| o.trigger_ratio)
        .unwrap_or(defaults.trigger_ratio)
        .clamp(0.1, 0.95);
    let emergency_ratio = override_
        .and_then(|o| o.emergency_ratio)
        .unwrap_or(defaults.emergency_ratio)
        .clamp(trigger_ratio, 0.99);
    let usable = context_window.saturating_sub(output_reserve).max(1024);
    let safe = ((usable as f64) * trigger_ratio) as u64;
    let emergency = ((usable as f64) * emergency_ratio) as u64;
    (safe.max(1024), emergency.max(safe), output_reserve)
}

fn parse_client_option_tokens(options: &HashMap<String, String>, key: &str) -> Option<u64> {
    options.get(key).and_then(|v| v.parse().ok())
}

fn resolve_context_window_tokens(
    config: &LlmCompactionConfig,
    client: &ClientDef,
    client_name: &str,
    model_id: &str,
    override_: Option<&ModelBudgetOverride>,
    override_source: BudgetSource,
) -> (u64, BudgetSource, BudgetFreshness, Option<String>) {
    if let Some(tokens) = override_
        .and_then(|o| o.context_window_tokens)
        .or_else(|| parse_client_option_tokens(&client.options, "context_window_tokens"))
    {
        return (
            tokens,
            if override_.and_then(|o| o.context_window_tokens).is_some() {
                override_source
            } else {
                BudgetSource::Configured
            },
            BudgetFreshness::NotApplicable,
            None,
        );
    }

    if let Some(tokens) = known_model_context_window(model_id) {
        return (
            tokens,
            BudgetSource::KnownModel,
            BudgetFreshness::NotApplicable,
            None,
        );
    }

    if let Some((tokens, source, freshness)) = lookup_online_cache(model_id, &config.online_sources)
    {
        return (tokens, source, freshness, None);
    }

    (
        FALLBACK_CONTEXT_WINDOW_TOKENS,
        BudgetSource::Fallback,
        BudgetFreshness::Offline,
        Some(format!(
            "unknown model {model_id} for client {client_name}; using conservative fallback"
        )),
    )
}

fn lookup_online_cache(
    model_id: &str,
    sources: &ModelBudgetSourceConfig,
) -> Option<(u64, BudgetSource, BudgetFreshness)> {
    let cache = online_cache().read().ok()?;
    let entry = cache.get(model_id)?;
    let ttl = Duration::from_secs(sources.cache_ttl_secs.max(60));
    let age = entry.fetched_at.elapsed();
    let freshness = if age <= ttl {
        BudgetFreshness::Fresh
    } else {
        BudgetFreshness::Stale
    };
    Some((
        entry.context_window_tokens,
        BudgetSource::Openrouter,
        freshness,
    ))
}

/// Resolve budget for one client without network I/O.
#[must_use]
pub fn resolve_client_budget(config: &LlmClientConfig, client_name: &str) -> ModelContextBudget {
    let compaction = config.compaction.clone();
    let client = config
        .clients
        .get(client_name)
        .cloned()
        .unwrap_or_else(|| ClientDef {
            name: client_name.to_string(),
            provider: LlmProvider::Openrouter,
            options: HashMap::new(),
            retry_policy: None,
        });
    let model_id = client
        .options
        .get("model")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let (override_, override_source) = effective_override(&compaction, client_name, &model_id);
    let (context_window, source, freshness, warning) = resolve_context_window_tokens(
        &compaction,
        &client,
        client_name,
        &model_id,
        override_,
        override_source,
    );
    let (safe, emergency, output_reserve) =
        apply_ratios(context_window, override_, &compaction.defaults);
    ModelContextBudget {
        model_id,
        provider: client.provider.as_str().to_string(),
        client_name: client_name.to_string(),
        context_window_tokens: context_window,
        safe_prompt_tokens: safe,
        emergency_prompt_tokens: emergency,
        output_reserve_tokens: output_reserve,
        source,
        freshness,
        warning,
    }
}

/// Resolve budgets for every configured client.
#[must_use]
pub fn resolve_all_client_budgets(config: &LlmClientConfig) -> ResolvedClientBudgets {
    let clients: Vec<_> = config
        .clients
        .keys()
        .map(|name| resolve_client_budget(config, name))
        .collect();
    ResolvedClientBudgets {
        clients,
        refreshed_at_ms: None,
    }
}

/// Resolve budget for the effective client given agent/function routing.
#[must_use]
pub fn resolve_effective_budget(
    config: &LlmClientConfig,
    agent_package: Option<&str>,
    function_name: &str,
) -> ModelContextBudget {
    let client_name = config.resolve(agent_package, function_name);
    resolve_client_budget(config, client_name)
}

/// Insert or update cached online metadata (called by refresh endpoint).
pub fn cache_online_context_window(model_id: &str, context_window_tokens: u64) {
    if let Ok(mut cache) = online_cache().write() {
        cache.insert(
            model_id.to_string(),
            CacheEntry {
                context_window_tokens,
                fetched_at: Instant::now(),
            },
        );
    }
}

/// Clear the online metadata cache.
pub fn clear_online_budget_cache() {
    if let Ok(mut cache) = online_cache().write() {
        cache.clear();
    }
}

/// Fetch model metadata from online sources and populate cache.
/// Does not block startup; failures are logged and ignored.
pub async fn refresh_online_budget_cache(config: &LlmClientConfig) -> usize {
    if !config.compaction.online_sources.openrouter {
        return 0;
    }
    let mut updated = 0usize;
    for client in config.clients.values() {
        if client.provider != LlmProvider::Openrouter {
            continue;
        }
        let Some(model_id) = client.options.get("model") else {
            continue;
        };
        if known_model_context_window(model_id).is_some() {
            continue;
        }
        if fetch_openrouter_context_length(model_id).await.is_some() {
            updated += 1;
        }
    }
    updated
}

async fn fetch_openrouter_context_length(model_id: &str) -> Option<u64> {
    let url = "https://openrouter.ai/api/v1/models";
    let response = reqwest::get(url).await.ok()?;
    let body: serde_json::Value = response.json().await.ok()?;
    let models = body.get("data")?.as_array()?;
    for model in models {
        let id = model.get("id")?.as_str()?;
        if id != model_id {
            continue;
        }
        let context_length = model
            .get("context_length")
            .and_then(|v| v.as_u64())
            .or_else(|| model.get("top_provider")?.get("context_length")?.as_u64())?;
        cache_online_context_window(model_id, context_length);
        tracing::info!(
            model_id,
            context_length,
            "cached OpenRouter model context window"
        );
        return Some(context_length);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClientDef, LlmClientConfig, LlmProvider};

    #[test]
    fn known_model_returns_known_source() {
        let mut config = LlmClientConfig::sensible_default();
        config.compaction = LlmCompactionConfig::default();
        let budget = resolve_client_budget(&config, "OpenRouter");
        assert_eq!(budget.source, BudgetSource::KnownModel);
        assert!(budget.context_window_tokens >= 128_000);
    }

    #[test]
    fn unknown_model_uses_fallback_with_warning() {
        let mut config = LlmClientConfig::sensible_default();
        config.compaction = LlmCompactionConfig::default();
        config.clients.insert(
            "Custom".to_string(),
            ClientDef {
                name: "Custom".to_string(),
                provider: LlmProvider::Ollama,
                options: [("model".to_string(), "local/unknown-model".to_string())]
                    .into_iter()
                    .collect(),
                retry_policy: None,
            },
        );
        let budget = resolve_client_budget(&config, "Custom");
        assert_eq!(budget.source, BudgetSource::Fallback);
        assert!(budget.warning.is_some());
        assert_eq!(budget.context_window_tokens, FALLBACK_CONTEXT_WINDOW_TOKENS);
    }

    #[test]
    fn client_override_takes_precedence() {
        let mut config = LlmClientConfig::sensible_default();
        config.compaction = LlmCompactionConfig {
            client_overrides: [(
                "OpenRouter".to_string(),
                ModelBudgetOverride {
                    context_window_tokens: Some(64_000),
                    trigger_ratio: Some(0.5),
                    emergency_ratio: Some(0.8),
                    output_reserve_tokens: Some(2048),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let budget = resolve_client_budget(&config, "OpenRouter");
        assert_eq!(budget.source, BudgetSource::Configured);
        assert_eq!(budget.context_window_tokens, 64_000);
        assert!(budget.safe_prompt_tokens < budget.emergency_prompt_tokens);
    }

    #[test]
    fn bytes_token_roundtrip_is_conservative() {
        assert_eq!(tokens_to_bytes(1000), 4000);
        assert_eq!(bytes_to_tokens(4000), 1000);
        assert_eq!(bytes_to_tokens(4001), 1001);
    }
}
