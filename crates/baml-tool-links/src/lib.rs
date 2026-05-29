//! Centralized force-link crate for all BAML tool crates.
//!
//! This crate exists to break a dependency cycle: tool crates depend on
//! `baml-rt-tools` for the `BamlTool` trait, so `baml-rt-tools` cannot
//! depend back on tool crates. This leaf crate carries all tool crates
//! as dependencies and provides a single macro to force-link them.
//!
//! # Usage
//!
//! Call the macro once at the top level of any binary that needs tool
//! discovery (runner, builder, CLI):
//!
//! ```ignore
//! baml_tool_links::force_link_all_tools!();
//! ```
//!
//! # Feature Flags
//!
//! - `clickup` - ClickUp integration tool
//! - `memory` - Graph-based cognitive memory tool
//! - `notion` - Notion integration tool
//! - `security-eval` - CRM/email support tools for security-eval fixtures
//! - `slack` - Slack read integration tool
//! - `slack-notify` - Slack notification write tool
//! - `grafana-alerts` - Grafana alert webhook intake + event producer
//! - `internal-dev` - Test tool implementations (Calculator, Delay, etc.)
//! - `http-tools` - Enables `clickup`, `notion`, `slack`, `slack-notify`, `security-eval`, `grafana-alerts`
//! - `all-tools` - Enables `http-tools` and `memory`

// Re-export tool crates so binaries can access them through baml_tool_links
// without needing direct dependencies. This enables the force_link_all_tools!
// macro to work when binaries only depend on baml-tool-links.

// Unconditional re-exports (core platform tools - always linked)
pub use baml_rt_tools_claude;
pub use baml_tools_calculator;
// Feature-gated re-exports (integration tools)
#[cfg(feature = "clickup")]
pub use baml_tools_clickup;
#[cfg(feature = "grafana-alerts")]
pub use baml_tools_grafana_alerts;
// Test-only re-exports
#[cfg(feature = "internal-dev")]
pub use baml_tools_internal_dev;
#[cfg(feature = "memory")]
pub use baml_tools_memory;
#[cfg(feature = "notion")]
pub use baml_tools_notion;
#[cfg(feature = "security-eval")]
pub use baml_tools_security_eval;
#[cfg(feature = "slack")]
pub use baml_tools_slack;
#[cfg(feature = "slack-notify")]
pub use baml_tools_slack_notify;
pub use baml_tools_system;

/// Force-link all registered tool crates into the binary's inventory.
///
/// Call this macro once at the top level of any binary that needs tool
/// discovery (runner, builder, CLI). The `#[cfg(feature)]` gates match
/// the feature flags on this crate's Cargo.toml.
///
/// # Example
///
/// ```ignore
/// // At the top of main.rs or lib.rs
/// baml_tool_links::force_link_all_tools!();
///
/// fn main() {
///     // Tools are now discoverable via inventory
/// }
/// ```
#[macro_export]
macro_rules! force_link_all_tools {
    () => {
        // Unconditional (core platform tools - always linked)
        // Feature-gated (integration tools)
        #[cfg(feature = "clickup")]
        use $crate::baml_tools_clickup as _;
        #[cfg(feature = "grafana-alerts")]
        use $crate::baml_tools_grafana_alerts as _;
        // Test-only tools
        #[cfg(feature = "internal-dev")]
        use $crate::baml_tools_internal_dev as _;
        #[cfg(feature = "memory")]
        use $crate::baml_tools_memory as _;
        #[cfg(feature = "notion")]
        use $crate::baml_tools_notion as _;
        #[cfg(feature = "security-eval")]
        use $crate::baml_tools_security_eval as _;
        #[cfg(feature = "slack")]
        use $crate::baml_tools_slack as _;
        #[cfg(feature = "slack-notify")]
        use $crate::baml_tools_slack_notify as _;
        use $crate::{
            baml_rt_tools_claude as _, baml_tools_calculator as _, baml_tools_system as _,
        };
    };
}
