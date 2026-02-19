//! Tool bundle type system
//!
//! Bundles are represented as Rust types for compile-time type safety.
//! Each bundle implements `BundleType` to provide its metadata.

use baml_rt_core::Result;
use serde_json::Value;

use crate::tools::BundleName;

/// Trait for tool bundle types
///
/// Each bundle (e.g., "support") should be represented
/// as a Rust type that implements this trait.
///
/// # Example
/// ```rust,no_run
/// use baml_rt_tools::BundleType;
///
/// pub struct MyBundle;
///
/// impl BundleType for MyBundle {
///     const NAME: &'static str = "my_bundle";
///     fn description() -> &'static str {
///         "My bundle of tools"
///     }
/// }
/// ```
pub trait BundleType: Send + Sync + 'static {
    /// The bundle name (e.g., "support")
    const NAME: &'static str;

    /// Description of what this bundle provides
    fn description() -> &'static str;

    /// Optional JSON schema for bundle configuration
    fn config_schema() -> Option<Value> {
        None
    }

    /// Get the BundleName for this bundle type
    fn bundle_name() -> Result<BundleName> {
        BundleName::new(Self::NAME)
    }
}

/// Support bundle - basic support tools
pub struct Support;

impl BundleType for Support {
    const NAME: &'static str = "support";

    fn description() -> &'static str {
        "Support tools for basic operations (calculations, string manipulation, etc.)"
    }
}
