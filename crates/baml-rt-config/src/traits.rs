//! Config service traits: read-only and read-write.
//!
//! **Bundle config** is keyed by bundle name; tools in a bundle share the same config.
//! **Internal config** is key-value storage for runtime configuration (e.g. secret link state),
//! not tied to tool bundles.

use baml_rt_core::Result;
use baml_rt_tools::{BundleName, ConfigResolver};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Internal config (key-value; not bundle-scoped)
// ---------------------------------------------------------------------------

/// Read internal configuration by string key (e.g. secret link state). Not a tool bundle.
pub trait InternalConfigReader: Send + Sync {
    fn get_internal(&self, key: &str) -> Result<Option<Value>>;
}

/// Write internal configuration by string key.
pub trait InternalConfigWriter: Send + Sync {
    fn set_internal(&self, key: &str, value: Value) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Bundle config (tool bundles)
// ---------------------------------------------------------------------------

/// Monotonically increasing version for a bundle's config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigVersionNumber(pub u64);

impl From<ConfigVersionNumber> for u64 {
    fn from(v: ConfigVersionNumber) -> Self {
        v.0
    }
}

impl From<u64> for ConfigVersionNumber {
    fn from(v: u64) -> Self {
        ConfigVersionNumber(v)
    }
}

/// Unix timestamp in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixMs(pub u64);

impl From<UnixMs> for u64 {
    fn from(t: UnixMs) -> Self {
        t.0
    }
}

impl From<u64> for UnixMs {
    fn from(t: u64) -> Self {
        UnixMs(t)
    }
}

/// Current config plus version (returned by get_with_version).
#[derive(Debug, Clone)]
pub struct StoredConfig {
    pub config: Value,
    pub version: ConfigVersionNumber,
}

/// Versioned config snapshot for provenance linkage.
#[derive(Debug, Clone)]
pub struct ConfigVersion {
    pub bundle_name: BundleName,
    pub version: ConfigVersionNumber,
    pub config: Value,
    pub created_at_ms: UnixMs,
}

/// Read-only config access (used by registry, session open, provenance).
pub trait ConfigReader: Send + Sync {
    /// Get current config for a bundle, if any.
    fn get(&self, bundle_name: &BundleName) -> Result<Option<Value>>;

    /// Get current config with version, if any.
    fn get_with_version(&self, bundle_name: &BundleName) -> Result<Option<StoredConfig>>;

    /// List bundles that have stored config.
    fn list_with_config(&self) -> Result<Vec<BundleName>>;
}

/// Write config (used by provisioning/admin paths).
pub trait ConfigWriter: Send + Sync {
    /// Set config for a bundle; creates new version.
    fn set(&self, bundle_name: &BundleName, config: Value) -> Result<ConfigVersion>;

    /// Remove stored config for a bundle.
    fn delete(&self, bundle_name: &BundleName) -> Result<()>;

    /// Get config at a specific version.
    fn get_version(&self, bundle_name: &BundleName, version: u64) -> Result<Option<ConfigVersion>>;

    /// List version history for a bundle.
    fn list_versions(&self, bundle_name: &BundleName) -> Result<Vec<ConfigVersion>>;
}

/// Combined read-write config service (bundle config + internal config). Also implements ConfigResolver for registry use.
pub trait ConfigService:
    ConfigReader + ConfigWriter + ConfigResolver + InternalConfigReader + InternalConfigWriter
{
}
