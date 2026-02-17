//! System bundle type.

use baml_rt_tools::bundles::BundleType;

/// System bundle — host tools for system operations.
pub struct System;

impl BundleType for System {
    const NAME: &'static str = "system";

    fn description() -> &'static str {
        "System tools (A2A conversation, etc.)."
    }
}
