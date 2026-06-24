// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Load stored LLM client config from the config service / resolver.

use baml_rt_tools::{BundleName, config_resolver::ConfigResolver};

use crate::{LLM_CONFIG_BUNDLE_NAME, LlmClientConfig};

/// Load LLM client config from the canonical config bundle, with sensible defaults on miss/parse failure.
pub async fn load_stored_config(resolver: &dyn ConfigResolver) -> LlmClientConfig {
    let bundle = BundleName::new(LLM_CONFIG_BUNDLE_NAME).expect("llm bundle name valid");
    match resolver.get_config(&bundle).await {
        Ok(Some(v)) => LlmClientConfig::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "stored LLM config parse failed; using sensible default");
            LlmClientConfig::sensible_default()
        }),
        Ok(None) => LlmClientConfig::sensible_default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load LLM config; using sensible default");
            LlmClientConfig::sensible_default()
        }
    }
}
