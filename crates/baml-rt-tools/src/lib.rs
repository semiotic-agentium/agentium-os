//! Tool registry and mapping utilities.

pub mod access;
pub mod bundles;
#[cfg(feature = "clickup")]
pub mod clickup;
pub mod host_registration;
mod metrics;
#[cfg(feature = "notion")]
pub mod notion;
mod spans;
pub mod support;
pub mod tool_catalog;
pub mod tool_discovery;
pub mod tool_fsm;
pub mod tool_schema;
pub mod tools;
pub mod ts_gen;

pub use access::{ToolAccessPolicy, enforce_tool_access, parse_access_allowlist};
pub use bundles::{BundleType, Support};
pub use host_registration::{is_system_host_tool, register_manifest_tools};
pub use tool_catalog::{InventoryCatalog, ManifestToolNames, ToolCatalog};
pub use tool_discovery::search_tools;
pub use tool_fsm::{
    SessionPhase, ToolFailure, ToolFailureKind, ToolSession, ToolSessionError, ToolSessionId,
    ToolStep,
};
pub use tool_schema::{ToolType, json_schema_value, ts_decl, ts_name};
pub use tools::{
    BamlTool, BundleName, LocalToolName, ToolAccess, ToolBundle, ToolBundleMetadata,
    ToolCapability, ToolDiscoveryRecord, ToolExecutor, ToolFunctionMetadataExport, ToolHandler,
    ToolMetadataBuilder, ToolName, ToolOrigin, ToolRegistry, ToolSecretRequirement,
    ToolSessionAdvance, ToolSessionHandle, ToolTypeSpec, TypeBasedMetadataBuilder,
    create_multi_send_session_tool_from_async, parse_tool_name_and_class,
};
