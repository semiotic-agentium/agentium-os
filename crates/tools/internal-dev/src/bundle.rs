//! Internal development bundle type.
//!
//! This bundle provides tools used only for testing and internal development.

use baml_rt_tools::bundles::BundleType;

/// Internal development bundle — tools for testing and dev only.
pub struct InternalDev;

impl BundleType for InternalDev {
    const NAME: &'static str = "internal-dev";

    fn description() -> &'static str {
        "Internal development tools for testing"
    }
}
