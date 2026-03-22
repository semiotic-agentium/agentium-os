//! Tool registry and mapping utilities.

pub mod access;
pub mod archive_read;
pub mod archive_refs;
pub mod bundles;
pub mod config_resolver;
pub mod host_registration;
mod metrics;
pub mod prompt_projection;
pub mod session_coordination;
mod spans;
pub mod tool_catalog;
pub mod tool_discovery;
pub mod tool_error_classify;
pub mod tool_fsm;
pub mod tool_schema;
pub mod tools;
pub mod ts_gen;

pub use access::{ToolAccessPolicy, enforce_tool_access, parse_access_allowlist};
/// Re-export the `#[baml_tool]` attribute macro so tool crates can use
/// `use baml_rt_tools::baml_tool;` as a single import path.
pub use baml_tool_derive::baml_tool;
pub use bundles::{BundleType, Support};
pub use config_resolver::ConfigResolver;
pub use host_registration::register_manifest_tools;
pub use session_coordination::get_session_coordination_baml_for_tools;
pub use tool_catalog::{InventoryCatalog, ManifestToolNames, ToolCatalog};
pub use tool_discovery::search_tools;
pub use tool_error_classify::{
    ClassifiedToolError, ToolExecutionClassifier, a2a_retryability, classify_for_session,
    should_host_retry, should_host_retry_baml_error,
};
pub use tool_fsm::{
    SessionPhase, ToolFailure, ToolFailureKind, ToolSession, ToolSessionError, ToolSessionId,
    ToolStep,
};
pub use tool_schema::{DescribeAction, ToolType, json_schema_value, ts_decl, ts_name};
pub use tools::{
    BamlTool, BundleName, FunctionPlanBinding, FunctionRole, LocalToolName, SecretRequest,
    SecretType, SessionPlanFunctionsMap, SessionPlanTypeName, SessionPolicy, SessionTypeNames,
    ToolAccess, ToolBundle, ToolBundleMetadata, ToolCapability, ToolConfigMetadata,
    ToolDiscoveryRecord, ToolExecutor, ToolFunctionMetadataExport, ToolHandler,
    ToolMetadataBuilder, ToolName, ToolOrigin, ToolRegistry, ToolSessionAdvance, ToolSessionHandle,
    ToolSlug, ToolTypeSpec, TypeBasedMetadataBuilder, create_multi_send_session_tool_from_async,
    parse_tool_name_and_class,
};
