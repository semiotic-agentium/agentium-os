//! Provider-level requirements enforced at configuration and registry-build time.
//!
//! The BAML runtime requires `base_url` in client options when using `openai-generic` (e.g.
//! OpenRouter). We ensure this at **config load/update** (normalize when config is read or
//! deserialized), not at registry-build runtime. IR-based builds validate before adding clients.

use std::collections::HashMap;

use anyhow::{Result, ensure};
use baml_types::{BamlMap, BamlValue};

/// Default base URL for OpenRouter. Use when provider is OpenRouter or openai-generic
/// and no `base_url` is specified in config.
pub const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Provider names that require `base_url` in client options (BAML runtime requirement).
const PROVIDERS_REQUIRING_BASE_URL: &[&str] = &["openrouter", "openai-generic", "openai_generic"];

/// Returns true if the provider (config string) requires `base_url` in options.
pub fn provider_requires_base_url(provider: &str) -> bool {
    let normalized = provider.trim().to_lowercase();
    PROVIDERS_REQUIRING_BASE_URL
        .iter()
        .any(|p| *p == normalized)
}

/// Ensures `base_url` is present in **config** options when the provider requires it.
/// Call from [`LlmClientConfig::normalize`] on load/update so we do not patch at registry-build time.
pub fn ensure_base_url_for_provider_config(options: &mut HashMap<String, String>, provider: &str) {
    if !provider_requires_base_url(provider) {
        return;
    }
    if options.contains_key("base_url") {
        return;
    }
    options.insert(
        "base_url".to_string(),
        DEFAULT_OPENROUTER_BASE_URL.to_string(),
    );
}

/// Fails if `requires_base_url` is true but `options` does not contain `base_url`.
/// Call before adding a client to the registry so invalid state is impossible.
pub fn require_base_url_if_required(
    options: &BamlMap<String, BamlValue>,
    requires_base_url: bool,
) -> Result<()> {
    ensure!(
        !requires_base_url || options.get("base_url").is_some(),
        "openai-generic / OpenRouter client must have base_url in options (BAML runtime requirement)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_requires_base_url_openrouter_and_generic() {
        assert!(provider_requires_base_url("openrouter"));
        assert!(provider_requires_base_url("openai-generic"));
        assert!(provider_requires_base_url("openai_generic"));
        assert!(!provider_requires_base_url("anthropic"));
        assert!(!provider_requires_base_url("openai"));
    }

    #[test]
    fn ensure_base_url_inserts_when_required_and_missing() {
        let mut options = HashMap::new();
        ensure_base_url_for_provider_config(&mut options, "openrouter");
        assert!(options.contains_key("base_url"));
        assert_eq!(
            options.get("base_url").map(String::as_str),
            Some(DEFAULT_OPENROUTER_BASE_URL)
        );
    }

    #[test]
    fn require_base_url_if_required_errors_when_required_but_missing() {
        let options = BamlMap::new();
        assert!(require_base_url_if_required(&options, true).is_err());
        assert!(require_base_url_if_required(&options, false).is_ok());
    }
}
