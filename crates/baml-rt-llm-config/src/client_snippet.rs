//! Generate minimal BAML client declaration from LlmClientConfig for schema injection.
//!
//! Injected at schema load so the compiler sees a single `client Default` definition
//! whose properties come from llm_config; the host controls provider/options, not the .baml files.

use crate::config::LlmClientConfig;

/// Minimal client Default declaration used when no llm_config is provided (e.g. tests).
/// Host still overrides at runtime via ClientRegistry when config is present.
pub const CLIENT_DEFAULT_FALLBACK_BAML: &str = r#"// Placeholder when no llm_config; host overrides via ClientRegistry at runtime.
client Default {
  provider "openai"
}
"#;

/// Build the BAML source for `client Default { ... }` from the default client in config.
/// Returns None if the default client is not present in config.
///
/// Emits minimal declaration (provider "openai", no options); host controls
/// provider/options at runtime via ClientRegistry.
pub fn client_default_baml_snippet(config: &LlmClientConfig) -> Option<String> {
    let _ = config.get_client(&config.default)?;
    Some(
        r#"// Injected from llm_config; host controls provider/options at runtime.
client Default {
  provider "openai"
}
"#
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{ClientDef, LlmProvider};

    #[test]
    fn snippet_uses_default_client_from_config() {
        let mut options = HashMap::new();
        options.insert("model".to_string(), "openai/gpt-4o-mini".to_string());
        options.insert(
            "api_key".to_string(),
            "vault:OPENROUTER_API_KEY".to_string(),
        );
        let client = ClientDef {
            name: "Default".to_string(),
            provider: LlmProvider::Openrouter,
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
        let snippet = client_default_baml_snippet(&config).unwrap();
        assert!(snippet.contains("client Default"));
        assert!(snippet.contains("provider \"openai\""));
    }
}
