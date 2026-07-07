// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Load stored semiotic gate config from the config service.

use baml_rt_tools::{BundleName, config_resolver::ConfigResolver};

use crate::config::{SEMIOTIC_CONFIG_BUNDLE_NAME, SemioticConfig};

/// Load semiotic config from the canonical `semiotic` bundle, with defaults on miss/parse failure.
pub async fn load_stored_config(resolver: &dyn ConfigResolver) -> SemioticConfig {
    let bundle = BundleName::new(SEMIOTIC_CONFIG_BUNDLE_NAME).expect("semiotic bundle name valid");
    match resolver.get_config(&bundle).await {
        Ok(Some(v)) => SemioticConfig::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "stored semiotic config parse failed; using default");
            SemioticConfig::default()
        }),
        Ok(None) => SemioticConfig::default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load semiotic config; using default");
            SemioticConfig::default()
        }
    }
}
