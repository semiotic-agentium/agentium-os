//! Config resolution for tool sessions.
//!
//! Implemented by baml-rt-config; used by ToolRegistry when opening sessions.
//! Config is keyed by bundle name; tools in a bundle share the same config.

use baml_rt_core::Result;
use serde_json::Value;

use crate::BundleName;

/// Resolves config for tool bundles at session open.
/// Implemented by [baml_rt_config::ConfigReader] / [baml_rt_config::ConfigService].
pub trait ConfigResolver: Send + Sync {
    /// Get current config for a bundle, if any.
    fn get_config(&self, bundle_name: &BundleName) -> Result<Option<Value>>;

    /// Get current config with version, if any. Used for provenance linkage.
    /// Default returns (config, 0) when config exists; implementors with versioning should override.
    fn get_config_with_version(&self, bundle_name: &BundleName) -> Result<Option<(Value, u64)>> {
        Ok(self.get_config(bundle_name)?.map(|v| (v, 0)))
    }
}
