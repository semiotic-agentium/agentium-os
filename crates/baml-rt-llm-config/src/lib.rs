// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Centralised LLM client configuration for BAML runtime.
//!
//! Config defines clients, default, and overrides (agent, agent_function).
//! Resolution: overrides.agent_function["agent:fn"] → overrides.agent["agent"] → default.
//! BAML only has `client Default`; model selection is fully decoupled and host-controlled.

mod client_snippet;
mod compaction_policy;
mod config;
mod loader;
mod model_budget;
mod provider_requirements;
mod registry_builder;
mod resolver;
mod secret_resolver;
mod store_loader;
mod test_model;

pub use client_snippet::{CLIENT_DEFAULT_FALLBACK_BAML, client_default_baml_snippet};
pub use compaction_policy::{
    CompactionTriggerPolicy, resolve_compaction_trigger_policy, trigger_policy_from_budget,
};
pub use config::{
    ClientDef, LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, LlmOverrides, LlmProvider, RetryPolicyDef,
};
pub use model_budget::{
    BYTES_PER_TOKEN_ESTIMATE, BudgetFreshness, BudgetSource, CompactionBudgetDefaults,
    DEFAULT_EMERGENCY_RATIO, DEFAULT_OUTPUT_RESERVE_TOKENS, DEFAULT_TRIGGER_RATIO,
    FALLBACK_CONTEXT_WINDOW_TOKENS, LlmCompactionConfig, ModelBudgetOverride,
    ModelBudgetSourceConfig, ModelContextBudget, ResolvedClientBudgets, bytes_to_tokens,
    clear_online_budget_cache, refresh_online_budget_cache, resolve_all_client_budgets,
    resolve_client_budget, resolve_effective_budget, tokens_to_bytes,
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
pub use store_loader::load_stored_config;
pub use test_model::{FALLBACK_TEST_MODEL, test_model_default};
