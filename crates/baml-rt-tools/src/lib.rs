//! Tool registry and mapping utilities.

pub mod bundles;
pub mod tool_fsm;
pub mod tool_schema;
pub mod tools;
pub mod ts_gen;
pub mod tool_catalog;
pub mod support;
mod spans;
mod metrics;

pub use bundles::{BundleType, Support};
pub use tool_fsm::{ToolFailure, ToolFailureKind, ToolSession, ToolSessionError, ToolSessionId, ToolStep};
pub use tool_schema::{json_schema_value, ts_decl, ts_name, ToolType};
pub use tool_catalog::{ToolCatalog, InventoryCatalog};
pub use tools::{
    BamlTool,
    BundleName,
    LocalToolName,
    ToolAccessPolicy,
    ToolBundle,
    ToolBundleMetadata,
    ToolCapability,
    ToolExecutor,
    ToolFunctionMetadataExport,
    ToolHandler,
    ToolMetadataBuilder,
    ToolName,
    ToolOrigin,
    ToolSessionAdvance,
    ToolSessionHandle,
    ToolRegistry,
    ToolSecretRequirement,
    ToolTypeSpec,
    TypeBasedMetadataBuilder,
    parse_tool_name_and_class,
};
