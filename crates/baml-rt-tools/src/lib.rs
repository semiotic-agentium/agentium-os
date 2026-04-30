//! Tool registry and mapping utilities.
//!
//! ## LLM → tool input JSON
//!
//! Send-phase payloads are deserialized from LLM output; enum casing often disagrees with
//! `serde(rename_all = "snake_case")`. See the design note **`docs/llm_json_boundary.md`**
//! in this crate for generalisation ideas (derive helpers, normalise layer, contract tests).

pub mod access;
pub mod archive_read;
pub mod archive_refs;
pub mod bundles;
pub mod citations;
pub mod config_resolver;
pub mod event_producer;
pub mod external_tools;
pub mod host_registration;
pub mod ingress_store;
pub mod llm_request_display;
mod metrics;
pub mod opaque_json;
pub mod open_input_schema;
pub mod phase_step_json;
pub mod prompt_projection;
pub mod session_coordination;
pub mod session_ctx_tags;
mod spans;
pub mod tool_catalog;
pub mod tool_discovery;
pub mod tool_error_classify;
pub mod tool_fsm;
pub mod tool_schema;
pub mod tools;
pub mod ts_gen;

pub use access::{
    ACCESS_ALLOWLIST_ENV, ToolAccessPolicy, enforce_tool_access, parse_access_allowlist,
    parse_access_allowlist_from,
};
/// Re-export the `#[baml_tool]` attribute macro so tool crates can use
/// `use baml_rt_tools::baml_tool;` as a single import path.
pub use baml_tool_derive::baml_tool;
pub use bundles::{BundleRegistrar, BundleType, Support};
pub use config_resolver::ConfigResolver;
pub use event_producer::{
    EventProducer, EventProducerBuildContext, EventProducerBuildFuture, EventProducerProvider,
    ProducerCheckpoint, ProducerPoll, ProducerRegistry, load_configured_event_producers,
    load_configured_event_producers_with_checkpoints,
};
pub use host_registration::{
    ExternalToolResolver, register_manifest_tools, register_manifest_tools_with_fallback,
};
pub use ingress_store::{
    IngressId, IngressItem, IngressStore, clear_ingress_store, ingress_store,
    install_ingress_store, require_ingress_store,
};
pub use llm_request_display::{
    flatten_chat_completion_request_for_display, flatten_message_content_value,
};
pub use opaque_json::{
    OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY, OpaqueJson, opaque_json_map_from_object,
};
pub use open_input_schema::schema_allows_empty_or_optional_open_input;
pub use session_coordination::get_session_coordination_baml_for_tools;
pub use session_ctx_tags::{
    CTX_TAG_SESSION_STEP_STABLE_PREFIX, SESSION_STEP_STABLE_PREFIX_BAML,
    SESSION_STEP_STABLE_PREFIX_VALUE,
};
pub use tool_catalog::{CompositeCatalog, InventoryCatalog, ManifestToolNames, ToolCatalog};
pub use tool_discovery::search_tools;
pub use tool_error_classify::{
    ClassifiedToolError, ToolExecutionClassifier, a2a_retryability, classify_for_session,
    should_host_retry, should_host_retry_baml_error,
};
pub use tool_fsm::{
    SessionPhase, ToolFailure, ToolFailureKind, ToolSession, ToolSessionError, ToolSessionId,
    ToolStep, tool_failure_to_baml_tool_execution_error,
};
pub use tool_schema::{DescribeAction, ToolType, json_schema_value, ts_decl, ts_name};
pub use tools::{
    BamlTool, BundleName, FunctionPlanBinding, FunctionRole, LocalToolName, SecretRequest,
    SecretType, SessionPlanFunctionsMap, SessionPlanTypeName, SessionPolicy, SessionTypeNames,
    ToolAccess, ToolBackend, ToolBundle, ToolBundleMetadata, ToolCapability, ToolConfigMetadata,
    ToolDiscoveryRecord, ToolExecutor, ToolFunctionMetadataExport, ToolHandler,
    ToolMetadataBuilder, ToolName, ToolOrigin, ToolRegistry, ToolSessionAdvance, ToolSessionHandle,
    ToolSlug, ToolTypeSpec, TypeBasedMetadataBuilder, create_multi_send_session_tool_from_async,
    create_one_shot_tool_from_async, create_one_shot_tool_from_async_with_context,
    parse_tool_name_and_class,
};
